//! Typed messages exchanged between extension runtimes and the editor host.
//!
//! These types intentionally carry only Knot concepts. They form the transport
//! boundary that extension isolates use to ask the foreground editor to access
//! native state; neither side exposes V8, gpui, or Rust editor-model objects.

use std::{borrow::Borrow, collections::BTreeMap, fmt, sync::Arc};

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
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct CommandRegistrationId(u64);

impl CommandRegistrationId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn value(self) -> u64 {
        self.0
    }
}

/// A Knot-owned identity for one live view instance.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ViewId(u64);

impl ViewId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// An opaque identity for one command invocation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct CommandInvocationId(u64);

impl CommandInvocationId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn value(self) -> u64 {
        self.0
    }
}

/// An opaque identity for one buffer-change subscription.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BufferSubscriptionId(u64);

impl BufferSubscriptionId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn value(self) -> u64 {
        self.0
    }
}

/// An opaque identity for one tree data-provider registration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TreeProviderRegistrationId(u64);

impl TreeProviderRegistrationId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn value(self) -> u64 {
        self.0
    }
}

/// An opaque identity for one completion-provider registration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct CompletionProviderRegistrationId(u64);

impl CompletionProviderRegistrationId {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) const fn value(self) -> u64 {
        self.0
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

/// The stable, globally unique name of a command definition.
#[derive(
    Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct CommandName(String);

impl AsRef<str> for CommandName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for CommandName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CommandName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<String> for CommandName {
    fn from(name: String) -> Self {
        Self(name)
    }
}

impl From<&str> for CommandName {
    fn from(name: &str) -> Self {
        Self(name.into())
    }
}

/// One JSON-compatible value carried as an explicit command argument.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum CommandArgumentValue {
    Null,
    Boolean(bool),
    Number(f64),
    String(String),
    Array(Vec<Self>),
    Object(BTreeMap<String, Self>),
}

/// A semantic command independent of its invocation source and native adapter.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Command {
    pub name: CommandName,
    pub arguments: CommandArgumentValue,
}

/// The stable result of attempting to invoke a command.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CommandOutcome {
    Completed,
    Unavailable,
    InvalidTarget,
    InvalidArgument { message: String },
    Cancelled,
    HandlerFailure { message: String },
}

/// The private routing result for a command requested by JavaScript.
///
/// Same-runtime commands are returned to the requesting isolate for a nested
/// handler call. All other routes settle through the dispatcher before the
/// host responds.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CommandInvokeDispatch {
    Outcome {
        outcome: CommandOutcome,
    },
    Inline {
        invocation: CommandInvocationId,
        registration: CommandRegistrationId,
    },
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

/// Theme-aware range decoration tokens supported by native editor views.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DecorationToken {
    Info,
    Warning,
    Error,
}

/// Theme-aware gutter marker tokens supported by native editor views.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GutterToken {
    Info,
    Warning,
    Error,
}

/// One semantic editor contribution supplied by an extension.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorContribution {
    pub range: ByteRange,
    pub decoration: Option<DecorationToken>,
    pub gutter: Option<GutterToken>,
    pub command: Option<String>,
}

/// Theme-aware icon tokens supported by native tree views.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TreeIcon {
    File,
    Folder,
    Symbol,
}

/// Whether a tree item can be expanded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TreeCollapsibleState {
    None,
    Collapsed,
    Expanded,
}

/// Renderer-neutral semantics for one extension-provided tree item.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeItem {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub icon: Option<TreeIcon>,
    pub collapsible_state: TreeCollapsibleState,
    pub command: Option<String>,
}

/// One asynchronous child request issued by a native tree view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeChildrenRequest {
    pub registration: TreeProviderRegistrationId,
    pub parent_id: Option<String>,
    pub generation: u64,
}

/// A recoverable tree-provider callback failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeProviderError {
    pub message: String,
}

/// One extension callback result returned to a native tree view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeChildrenResponse {
    pub registration: TreeProviderRegistrationId,
    pub parent_id: Option<String>,
    pub generation: u64,
    pub result: Result<Vec<TreeItem>, TreeProviderError>,
}

/// One asynchronous completion request issued by an editor view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionRequest {
    pub registration: CompletionProviderRegistrationId,
    pub buffer: BufferHandle,
    pub revision: u64,
    pub cursor_byte_offset: usize,
    pub prefix: String,
    pub generation: u64,
}

/// One semantic completion candidate returned by an extension provider.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionResultItem {
    pub label: String,
    pub insert_text: String,
}

/// A recoverable completion-provider callback failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionProviderError {
    pub message: String,
}

/// One extension callback result returned to an editor completion session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionResponse {
    pub registration: CompletionProviderRegistrationId,
    pub revision: u64,
    pub generation: u64,
    pub result: Result<Vec<CompletionResultItem>, CompletionProviderError>,
}

/// A request sent by one extension runtime to the editor foreground owner.
#[derive(Clone, Debug, PartialEq)]
pub struct HostRequest {
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
    pub id: RequestId,
    pub invocation: Option<CommandInvocationId>,
    pub operation: HostOperation,
}

/// Editor operations available to the initial buffer API boundary.
#[derive(Clone, Debug, PartialEq)]
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
        name: CommandName,
        title: String,
    },
    RegisterViewCommand {
        view_kind: String,
        name: CommandName,
    },
    SelectedViewText,
    WriteClipboardText {
        text: String,
    },
    UnregisterCommand {
        registration: CommandRegistrationId,
    },
    InvokeCommand {
        command: Command,
    },
    CompleteInlineCommand {
        invocation: CommandInvocationId,
        outcome: CommandOutcome,
    },
    SubscribeBufferChanges {
        buffer: BufferHandle,
    },
    UnsubscribeBufferChanges {
        subscription: BufferSubscriptionId,
    },
    ReplaceEditorContributions {
        buffer: BufferHandle,
        contributions: Vec<EditorContribution>,
        if_revision: u64,
    },
    DisposeEditorContributions {
        buffer: BufferHandle,
    },
    RegisterTreeProvider {
        view_id: String,
    },
    InvalidateTreeProvider {
        registration: TreeProviderRegistrationId,
        parent_id: Option<String>,
    },
    UnregisterTreeProvider {
        registration: TreeProviderRegistrationId,
    },
    RegisterCompletionProvider {
        label: String,
    },
    UnregisterCompletionProvider {
        registration: CompletionProviderRegistrationId,
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
    AppliedEdits {
        revision: u64,
    },
    CommandRegistered {
        registration: CommandRegistrationId,
    },
    ViewCommandRegistered {
        registration: CommandRegistrationId,
    },
    SelectedViewText(Option<String>),
    ClipboardTextWritten,
    CommandUnregistered {
        registration: CommandRegistrationId,
    },
    CommandInvoked {
        dispatch: CommandInvokeDispatch,
    },
    InlineCommandCompleted {
        invocation: CommandInvocationId,
    },
    BufferChangesSubscribed {
        subscription: BufferSubscriptionId,
    },
    BufferChangesUnsubscribed {
        subscription: BufferSubscriptionId,
    },
    EditorContributionsReplaced,
    EditorContributionsDisposed,
    TreeProviderRegistered {
        registration: TreeProviderRegistrationId,
    },
    TreeProviderInvalidated,
    TreeProviderUnregistered {
        registration: TreeProviderRegistrationId,
    },
    CompletionProviderRegistered {
        registration: CompletionProviderRegistrationId,
    },
    CompletionProviderUnregistered {
        registration: CompletionProviderRegistrationId,
    },
}

/// A foreground-authorized command invocation routed to its extension owner.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandInvocation {
    pub id: CommandInvocationId,
    pub registration: CommandRegistrationId,
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
    pub arguments: CommandArgumentValue,
}

/// Immutable UTF-16 snapshot storage shared across isolates and externalized by V8.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotText(pub Arc<[u16]>);

impl SnapshotText {
    pub(crate) fn from_utf8(text: &str) -> Self {
        Self(text.encode_utf16().collect::<Vec<_>>().into())
    }

    #[cfg(test)]
    pub(crate) fn to_utf8(&self) -> String {
        String::from_utf16(&self.0).expect("snapshot UTF-16 is well formed")
    }
}

/// A snapshot of one requested UTF-8 byte range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextSnapshot {
    pub text: SnapshotText,
    pub range: ByteRange,
    pub revision: u64,
}

/// One committed editor-visible buffer change in pre-commit coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BufferChange {
    pub buffer: BufferHandle,
    pub before_revision: u64,
    pub revision: u64,
    pub edits: Vec<TextEdit>,
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
    ContributionSetNotFound,
    TreeViewNotFound,
    ViewNotFound,
    TreeProviderInUse,
    TreeProviderNotFound,
    CompletionProviderNotFound,
    CommandNameInUse,
    CommandNotFound,
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::{
        BufferHandle, ByteRange, Command, CommandArgumentValue, CommandInvocationId,
        CommandInvokeDispatch, CommandOutcome, CommandRegistrationId, ExtensionId,
        ExtensionLifecycleId, HostOperation, HostRequest, HostRequestError, HostResponse,
        HostResponseValue, RequestId, TextEdit,
    };

    #[test]
    fn command_round_trips_json_compatible_arguments() {
        let command = Command {
            name: "editor.insert".into(),
            arguments: CommandArgumentValue::Object(
                [
                    ("enabled".into(), CommandArgumentValue::Boolean(true)),
                    (
                        "options".into(),
                        CommandArgumentValue::Array(vec![
                            CommandArgumentValue::Null,
                            CommandArgumentValue::Number(3.5),
                        ]),
                    ),
                    ("text".into(), CommandArgumentValue::String("λ".into())),
                ]
                .into(),
            ),
        };

        let json = serde_json::to_string(&command).unwrap();
        assert_eq!(serde_json::from_str::<Command>(&json).unwrap(), command);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap(),
            serde_json::json!({
                "name": "editor.insert",
                "arguments": {
                    "enabled": true,
                    "options": [null, 3.5],
                    "text": "λ"
                }
            })
        );
    }

    #[test]
    fn response_keeps_the_extension_and_request_identity() {
        let request = HostRequest {
            extension: ExtensionId::new(7),
            lifecycle: ExtensionLifecycleId::new(3),
            id: RequestId::new(11),
            invocation: None,
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

    #[test]
    fn command_routing_and_outcomes_keep_their_wire_contract() {
        let dispatch = CommandInvokeDispatch::Inline {
            invocation: CommandInvocationId::new(13),
            registration: CommandRegistrationId::new(21),
        };
        assert_eq!(
            serde_json::to_value(dispatch).unwrap(),
            serde_json::json!({
                "kind": "inline",
                "invocation": 13,
                "registration": 21,
            })
        );
        assert_eq!(
            serde_json::to_value(CommandOutcome::InvalidArgument {
                message: "expected string".into(),
            })
            .unwrap(),
            serde_json::json!({
                "kind": "invalidArgument",
                "message": "expected string",
            })
        );
    }

    #[test]
    fn edits_and_host_errors_keep_their_wire_contract() {
        let edit = TextEdit {
            range: ByteRange {
                start_byte_offset: 2,
                end_byte_offset: 5,
            },
            text: "λ".into(),
        };
        assert_eq!(
            serde_json::to_value(edit).unwrap(),
            serde_json::json!({
                "range": { "startByteOffset": 2, "endByteOffset": 5 },
                "text": "λ",
            })
        );
        assert_eq!(
            serde_json::to_value(HostRequestError::RevisionConflict).unwrap(),
            serde_json::json!("RevisionConflict")
        );
    }
}
