use super::resource::ResourceUri;

/// One normalized root in a window's ordered root set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceRoot {
    pub(crate) id: uuid::Uuid,
    pub(crate) uri: ResourceUri,
}

/// Identity captured by asynchronous workspace operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceSnapshot {
    roots: Vec<WorkspaceRoot>,
    generation: u64,
}

impl WorkspaceSnapshot {
    #[cfg(test)]
    pub(crate) fn first_root(&self) -> Option<&ResourceUri> {
        self.roots().first().map(|root| &root.uri)
    }

    pub(crate) fn roots(&self) -> &[WorkspaceRoot] {
        &self.roots
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

/// Foreground-owned roots and generation for the entire lifetime of a window.
#[derive(Default)]
pub(crate) struct WorkspaceState {
    roots: Vec<WorkspaceRoot>,
    generation: u64,
}

impl WorkspaceState {
    #[cfg(test)]
    pub(crate) fn new(root: ResourceUri) -> Self {
        let mut state = Self::default();
        state.replace_roots(vec![root]);
        state
    }

    pub(crate) fn snapshot(&self) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            roots: self.roots.clone(),
            generation: self.generation,
        }
    }

    pub(crate) fn is_current(&self, snapshot: &WorkspaceSnapshot) -> bool {
        self.roots == snapshot.roots && self.generation == snapshot.generation()
    }

    /// Replaces the entire ordered root set atomically with already normalized
    /// roots, keeping the first of any duplicates. Retained roots keep their
    /// identity; every replacement invalidates captured work, and clearing
    /// preserves the generation.
    pub(crate) fn replace_roots(&mut self, roots: Vec<ResourceUri>) -> WorkspaceSnapshot {
        let mut unique = Vec::with_capacity(roots.len());
        for uri in roots {
            if !unique.contains(&uri) {
                unique.push(uri);
            }
        }
        self.roots = unique
            .into_iter()
            .map(|uri| {
                self.roots
                    .iter()
                    .find(|root| root.uri == uri)
                    .cloned()
                    .unwrap_or_else(|| WorkspaceRoot {
                        id: uuid::Uuid::new_v4(),
                        uri,
                    })
            })
            .collect();
        self.generation = self
            .generation
            .checked_add(1)
            .expect("workspace generation overflowed");
        self.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(value: &str) -> ResourceUri {
        ResourceUri::parse(value).unwrap()
    }

    #[test]
    fn replacing_a_root_invalidates_captured_workspace_identity() {
        let mut workspace = WorkspaceState::new(uri("mem://workspace/"));
        let captured = workspace.snapshot();
        let replacement = workspace.replace_roots(vec![uri("file:///tmp/knot-workspace/")]);
        assert!(!workspace.is_current(&captured));
        assert!(workspace.is_current(&replacement));
        assert_eq!(
            replacement.first_root(),
            Some(&uri("file:///tmp/knot-workspace/"))
        );
        assert_ne!(replacement.roots()[0].id, captured.roots()[0].id);
        assert_eq!(replacement.generation(), captured.generation() + 1);
    }

    #[test]
    fn retained_normalized_roots_keep_identity_but_invalidate_in_flight_work() {
        let mut workspace = WorkspaceState::new(uri("MEM://workspace/a/../"));
        let captured = workspace.snapshot();
        let replacement = workspace.replace_roots(vec![uri("mem://workspace/")]);
        assert_eq!(replacement.roots(), captured.roots());
        assert!(!workspace.is_current(&captured));
    }

    #[test]
    fn clear_and_reinstall_preserve_generation_and_allocate_a_new_root_identity() {
        let mut workspace = WorkspaceState::default();
        let empty = workspace.snapshot();
        assert!(empty.roots().is_empty());
        assert_eq!(empty.generation(), 0);
        let first = workspace.replace_roots(vec![uri("mem://workspace/")]);
        let cleared = workspace.replace_roots(Vec::new());
        assert!(cleared.roots().is_empty());
        assert!(cleared.first_root().is_none());
        let installed = workspace.replace_roots(vec![uri("mem://workspace/")]);
        assert_eq!(installed.generation(), first.generation() + 2);
        assert_ne!(installed.roots()[0].id, first.roots()[0].id);
        assert!(!workspace.is_current(&empty));
        assert!(!workspace.is_current(&first));
        assert!(!workspace.is_current(&cleared));
    }

    #[test]
    fn reordering_and_adding_keep_retained_identities_and_drop_duplicates() {
        let mut workspace = WorkspaceState::new(uri("mem://one/"));
        let first = workspace.snapshot();
        let replacement = workspace.replace_roots(vec![
            uri("mem://two/"),
            uri("mem://one/"),
            uri("MEM://two/x/../"),
        ]);
        let uris = replacement
            .roots()
            .iter()
            .map(|root| root.uri.to_string())
            .collect::<Vec<_>>();
        assert_eq!(uris, ["mem://two/", "mem://one/"]);
        assert_eq!(replacement.roots()[1].id, first.roots()[0].id);
        assert!(!workspace.is_current(&first));
        let removed = workspace.replace_roots(vec![uri("mem://one/")]);
        assert_eq!(removed.roots()[0].id, first.roots()[0].id);
        assert_eq!(removed.generation(), first.generation() + 2);
    }
}
