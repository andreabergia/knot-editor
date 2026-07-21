//! Typed messages exchanged between extension runtimes and the editor host.
//!
//! These types intentionally carry only Knot concepts. They form the transport
//! boundary that a future extension thread will use to ask the foreground
//! editor to access a buffer; neither side needs to expose a V8, Deno, gpui,
//! or Rust editor-model object to JavaScript.

/// An extension-owned identity, opaque outside the host boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExtensionId(u64);

impl ExtensionId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn value(self) -> u64 {
        self.0
    }
}

/// A request identity, unique within an extension's lifetime.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId(u64);

impl RequestId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// An identity for one loaded extension lifetime.
///
/// Reusing an [`ExtensionId`] for a later load does not reuse this value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExtensionLifecycleId(u64);

impl ExtensionLifecycleId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// An opaque identity for one extension command registration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CommandRegistrationId(u64);

impl CommandRegistrationId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// An opaque identity for one command invocation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CommandInvocationId(u64);

impl CommandInvocationId {
    #[allow(
        dead_code,
        reason = "invocation allocation lands with command execution in the next Step 7 slice"
    )]
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// An opaque reference to an editor buffer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BufferHandle(u64);

#[allow(
    dead_code,
    reason = "the foreground buffer owner will allocate opaque handles in a later Step 7 slice"
)]
impl BufferHandle {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn value(self) -> u64 {
        self.0
    }
}

/// A half-open range in UTF-8 byte offsets.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ByteRange {
    pub start_byte_offset: usize,
    pub end_byte_offset: usize,
}

/// One replacement expressed in pre-commit buffer coordinates.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEdit {
    pub range: ByteRange,
    pub text: String,
}

/// A request sent by one extension runtime to the editor foreground owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostRequest {
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
    pub id: RequestId,
    pub operation: HostOperation,
}

/// Editor operations available to the initial buffer API boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostOperation {
    ActiveBuffer,
    Snapshot {
        buffer: BufferHandle,
        range: Option<ByteRange>,
    },
    ApplyEdits {
        buffer: BufferHandle,
        edits: Vec<TextEdit>,
        if_revision: u64,
    },
    RegisterCommand {
        name: String,
    },
    UnregisterCommand {
        registration: CommandRegistrationId,
    },
}

/// A reply sent back to the extension runtime for one [`HostRequest`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostResponse {
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
    pub id: RequestId,
    pub result: Result<HostResponseValue, HostRequestError>,
}

/// Successful values returned by [`HostOperation`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostResponseValue {
    ActiveBuffer(Option<BufferHandle>),
    Snapshot(TextSnapshot),
    AppliedEdits { revision: u64 },
    CommandRegistered { registration: CommandRegistrationId },
    CommandUnregistered { registration: CommandRegistrationId },
}

/// A foreground-authorized command invocation routed to its extension owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandInvocation {
    pub id: CommandInvocationId,
    pub registration: CommandRegistrationId,
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
}

/// A snapshot of one requested UTF-8 byte range.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextSnapshot {
    pub text: String,
    pub range: ByteRange,
    pub revision: u64,
}

/// Stable failures at the host boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum HostRequestError {
    UnsupportedOperation,
    BufferClosed,
    InvalidRange,
    InvalidEditBatch,
    RevisionConflict,
    CommandNameInUse,
    CommandNotFound,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::{
        BufferHandle, ExtensionId, ExtensionLifecycleId, HostOperation, HostRequest, HostResponse,
        HostResponseValue, RequestId,
    };

    #[test]
    fn response_keeps_the_extension_and_request_identity() {
        let request = HostRequest {
            extension: ExtensionId::new(7),
            lifecycle: ExtensionLifecycleId::new(3),
            id: RequestId::new(11),
            operation: HostOperation::ActiveBuffer,
        };

        let response = HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result: Ok(HostResponseValue::ActiveBuffer(Some(BufferHandle::new(3)))),
        };

        assert_eq!(response.extension, request.extension);
        assert_eq!(response.lifecycle, request.lifecycle);
        assert_eq!(response.id, request.id);
    }
}
