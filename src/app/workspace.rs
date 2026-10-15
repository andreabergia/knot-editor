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
    pub(crate) fn root(&self) -> Option<&ResourceUri> {
        self.roots().first().map(|root| &root.uri)
    }

    pub(crate) fn roots(&self) -> &[WorkspaceRoot] {
        &self.roots
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TooManyWorkspaceRoots;

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
        state.replace_root(root);
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

    /// Installs an already normalized root and invalidates all captured work.
    pub(crate) fn replace_root(&mut self, root: ResourceUri) -> WorkspaceSnapshot {
        self.replace_roots(vec![root])
            .expect("one root is supported")
    }

    /// Replaces the entire root set atomically. Clearing preserves the generation.
    pub(crate) fn replace_roots(
        &mut self,
        roots: Vec<ResourceUri>,
    ) -> Result<WorkspaceSnapshot, TooManyWorkspaceRoots> {
        if roots.len() > 1 {
            return Err(TooManyWorkspaceRoots);
        }
        self.roots = roots
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
        Ok(self.snapshot())
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
        let replacement = workspace.replace_root(uri("file:///tmp/knot-workspace/"));
        assert!(!workspace.is_current(&captured));
        assert!(workspace.is_current(&replacement));
        assert_eq!(
            replacement.root(),
            Some(&uri("file:///tmp/knot-workspace/"))
        );
        assert_ne!(replacement.roots()[0].id, captured.roots()[0].id);
        assert_eq!(replacement.generation(), captured.generation() + 1);
    }

    #[test]
    fn retained_normalized_roots_keep_identity_but_invalidate_in_flight_work() {
        let mut workspace = WorkspaceState::new(uri("MEM://workspace/a/../"));
        let captured = workspace.snapshot();
        let replacement = workspace.replace_root(uri("mem://workspace/"));
        assert_eq!(replacement.roots(), captured.roots());
        assert!(!workspace.is_current(&captured));
    }

    #[test]
    fn clear_and_reinstall_preserve_generation_and_allocate_a_new_root_identity() {
        let mut workspace = WorkspaceState::default();
        let empty = workspace.snapshot();
        assert!(empty.roots().is_empty());
        assert_eq!(empty.generation(), 0);
        let first = workspace.replace_root(uri("mem://workspace/"));
        let cleared = workspace.replace_roots(Vec::new()).unwrap();
        assert!(cleared.roots().is_empty());
        assert!(cleared.root().is_none());
        let installed = workspace.replace_root(uri("mem://workspace/"));
        assert_eq!(installed.generation(), first.generation() + 2);
        assert_ne!(installed.roots()[0].id, first.roots()[0].id);
        assert!(!workspace.is_current(&empty));
        assert!(!workspace.is_current(&first));
        assert!(!workspace.is_current(&cleared));
    }

    #[test]
    fn extra_roots_fail_without_mutating_the_current_workspace() {
        let mut workspace = WorkspaceState::new(uri("mem://workspace/"));
        let captured = workspace.snapshot();
        assert_eq!(
            workspace.replace_roots(vec![uri("mem://one/"), uri("mem://two/")]),
            Err(TooManyWorkspaceRoots)
        );
        assert_eq!(workspace.snapshot(), captured);
    }
}
