use super::resource::ResourceUri;

/// Identity captured by asynchronous workspace operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceSnapshot {
    root: ResourceUri,
    generation: u64,
}

impl WorkspaceSnapshot {
    pub(crate) fn root(&self) -> &ResourceUri {
        &self.root
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

/// Foreground-owned identity of the single workspace currently shown by a shell.
pub(crate) struct WorkspaceState {
    root: ResourceUri,
    generation: u64,
}

impl WorkspaceState {
    pub(crate) fn new(root: ResourceUri) -> Self {
        Self {
            root,
            generation: 1,
        }
    }

    pub(crate) fn root(&self) -> &ResourceUri {
        &self.root
    }

    pub(crate) fn snapshot(&self) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            root: self.root.clone(),
            generation: self.generation,
        }
    }

    pub(crate) fn is_current(&self, snapshot: &WorkspaceSnapshot) -> bool {
        self.root == snapshot.root && self.generation == snapshot.generation
    }

    /// Installs an already normalized root and invalidates all captured work.
    pub(crate) fn replace_root(&mut self, root: ResourceUri) -> WorkspaceSnapshot {
        self.root = root;
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

    #[test]
    fn replacing_a_root_invalidates_captured_workspace_identity() {
        let memory_root = ResourceUri::parse("mem://workspace/").unwrap();
        let local_root = ResourceUri::parse("file:///tmp/knot-workspace/").unwrap();
        let mut workspace = WorkspaceState::new(memory_root);
        let captured = workspace.snapshot();

        assert!(workspace.is_current(&captured));
        let replacement = workspace.replace_root(local_root.clone());

        assert!(!workspace.is_current(&captured));
        assert!(workspace.is_current(&replacement));
        assert_eq!(workspace.root(), &local_root);
        assert_eq!(replacement.generation(), captured.generation() + 1);
    }

    #[test]
    fn reinstalling_the_same_root_still_rejects_in_flight_work() {
        let root = ResourceUri::parse("mem://workspace/").unwrap();
        let mut workspace = WorkspaceState::new(root.clone());
        let captured = workspace.snapshot();

        workspace.replace_root(root);

        assert!(!workspace.is_current(&captured));
    }
}
