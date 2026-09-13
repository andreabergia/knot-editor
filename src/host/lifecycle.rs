//! Extension lifecycle identity and scheduler-visible state.

use std::sync::Arc;

use super::protocol::{ExtensionId, ExtensionLifecycleId};

/// Identifies one load of an extension.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExtensionKey {
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
}

impl ExtensionKey {
    pub const fn new(extension: ExtensionId, lifecycle: ExtensionLifecycleId) -> Self {
        Self {
            extension,
            lifecycle,
        }
    }
}

/// The scheduler-visible state of one extension lifetime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtensionState {
    Loading,
    Idle,
    Queued,
    Running,
    AwaitingHostWork,
    Stopping,
    Failed(Failure),
}

/// A terminal extension-local failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Failure {
    message: Arc<str>,
}

impl Failure {
    pub fn new(message: impl Into<Arc<str>>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}
