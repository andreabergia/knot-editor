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

    #[cfg(test)]
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

    pub(crate) const fn value(self) -> u64 {
        self.0
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
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize)]
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
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize)]
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
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize)]
pub struct BufferHandle(u64);

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

/// One ordered keymap change captured from a JavaScript turn.
#[derive(Clone, Debug, PartialEq)]
pub enum KeybindingMutation {
    Set {
        key: String,
        view: Option<String>,
        command: Option<Command>,
    },
    Remove {
        key: String,
        view: Option<String>,
    },
    Invalid(String),
}

/// Ordered native keymap mutations admitted after one JavaScript turn.
#[derive(Clone, Debug, PartialEq)]
pub struct KeybindingArguments {
    pub mutations: Vec<KeybindingMutation>,
}

impl super::declarations::HostArguments for KeybindingArguments {
    fn decode(
        _: &mut dyn super::declarations::ArgumentReader,
    ) -> Result<Self, super::declarations::DecodeError> {
        Err(super::declarations::DecodeError::Type(
            "keybinding batches require native turn admission",
        ))
    }
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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TreeCollapsibleState {
    #[default]
    None,
    Collapsed,
    Expanded,
}

/// Theme-aware severity tokens rendered as a tree row badge.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TreeBadge {
    Neutral,
    Info,
    Success,
    Warning,
    Error,
}

/// One semantic command-backed action attached to a row, group, or frame.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeAction {
    pub label: String,
    pub command: String,
    pub arguments: CommandArgumentValue,
}

/// Whether a tree row is ordinary selectable content or a group header.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TreeItemKind {
    #[default]
    Item,
    Group,
}

/// Renderer-neutral semantics for one extension-provided tree item.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeItem {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon: Option<TreeIcon>,
    #[serde(default)]
    pub collapsible_state: TreeCollapsibleState,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub kind: TreeItemKind,
    #[serde(default)]
    pub status_text: Option<String>,
    #[serde(default)]
    pub badge: Option<TreeBadge>,
    #[serde(default)]
    pub actions: Vec<TreeAction>,
}

/// Native content a contributed view definition provides in each window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewContentKind {
    Tree,
    Text,
}

/// One selectable row offered to an extension picker.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickerItem {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Settled outcome of a picker interaction.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PickerResult {
    Selected { id: String },
    Dismissed,
}

/// Settled outcome of a text input interaction.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum InputResult {
    Accepted { value: String },
    Dismissed,
}

/// Settled outcome of a confirmation interaction.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ConfirmationResult {
    Confirmed,
    Dismissed,
}

/// Settled outcome of a native folder picker.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FolderPickerResult {
    Selected { uri: String },
    Dismissed,
}

/// One asynchronous child request issued by a native view instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeChildrenRequest {
    pub registration: TreeProviderRegistrationId,
    pub window: String,
    pub view: ViewId,
    pub parent_id: Option<String>,
    pub generation: u64,
}

/// A recoverable tree-provider callback failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeProviderError {
    pub message: String,
}

/// One extension callback result returned to a native view instance.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeChildrenResponse {
    pub registration: TreeProviderRegistrationId,
    pub window: String,
    pub view: ViewId,
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

/// Discoverable live product targets, separate from command invocation authority.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowTarget {
    pub window: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneTarget {
    pub window: WindowTarget,
    pub workbench: String,
    pub pane: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentationTarget {
    pub pane: PaneTarget,
    pub tab: String,
    pub presentation: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DocumentKind {
    Untitled,
    Destination,
    Persisted,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentInfo {
    pub id: String,
    pub title: String,
    pub kind: DocumentKind,
    pub resource_uri: Option<String>,
    pub dirty: bool,
    pub revision: u64,
    pub buffer: BufferHandle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SurfaceKind {
    Document,
    Terminal,
    Tool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentationInfo {
    pub target: PresentationTarget,
    pub kind: SurfaceKind,
    pub document: Option<String>,
    pub buffer: Option<BufferHandle>,
    /// Stable contribution ID for a contributed tool surface.
    pub tool: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneInfo {
    pub target: PaneTarget,
    pub tabs: Vec<PresentationInfo>,
    pub selected: PresentationTarget,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRootInfo {
    pub id: String,
    pub uri: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
    pub window: WindowTarget,
    pub roots: Vec<WorkspaceRootInfo>,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub target: WindowTarget,
    pub panes: Vec<PaneInfo>,
    pub current_target: Option<PresentationTarget>,
    pub workspace: WorkspaceInfo,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSnapshot {
    pub documents: Vec<DocumentInfo>,
    pub windows: Vec<WindowInfo>,
    pub active_window: Option<WindowTarget>,
    pub active_target: Option<PresentationTarget>,
    /// Whether startup has committed its restored and requested windows.
    pub launched: bool,
}

/// Destination captured when a resource operation is admitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentDestination {
    Window(WindowTarget),
    Pane(PaneTarget),
    View(PresentationTarget),
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentPresentation {
    pub document: DocumentInfo,
    pub presentation: PresentationInfo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PresentationCloseOutcome {
    Closed,
    Cancelled,
}

/// One lifecycle-owned application observer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize)]
pub struct WorkbenchSubscriptionId(u64);
impl WorkbenchSubscriptionId {
    pub(crate) fn new(value: u64) -> Self {
        Self(value)
    }
    pub(crate) fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationEvent {
    pub sequence: u64,
    #[serde(flatten)]
    pub change: ApplicationChange,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ApplicationChange {
    DocumentOpened {
        document: DocumentInfo,
    },
    DocumentChanged {
        document: DocumentInfo,
    },
    DocumentClosed {
        document: String,
    },
    WindowOpened {
        window: WindowInfo,
    },
    WindowClosed {
        window: WindowTarget,
    },
    PresentationsChanged {
        window: WindowInfo,
    },
    WorkspaceChanged {
        workspace: WorkspaceInfo,
    },
    ActiveTargetChanged {
        active_window: Option<WindowTarget>,
        active_target: Option<PresentationTarget>,
    },
    /// The window's captured layout changed after its view-state debounce.
    ViewStateChanged {
        window: WindowTarget,
    },
    /// Startup committed its restored and requested windows; sent once.
    Launched,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkbenchObservation {
    pub subscription: WorkbenchSubscriptionId,
    pub snapshot: ApplicationSnapshot,
    pub watermark: u64,
}

/// Owned identity of a provider whose queued mutations precede disposal acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderIdentity {
    Tree(TreeProviderRegistrationId),
    Completion(CompletionProviderRegistrationId),
}

/// Authority metadata shared by foreground capability implementations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestContext {
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
    pub id: RequestId,
    pub invocation: Option<CommandInvocationId>,
}

impl HostRequest {
    pub fn context(&self) -> RequestContext {
        RequestContext {
            extension: self.extension,
            lifecycle: self.lifecycle,
            id: self.id,
            invocation: self.invocation,
        }
    }
}

/// A request sent by one extension runtime to the editor foreground owner.
#[derive(Clone, Debug, PartialEq)]
pub struct HostRequest {
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
    pub id: RequestId,
    pub invocation: Option<CommandInvocationId>,
    pub operation: super::declarations::DeclaredRequest,
}

/// A reply sent back to the extension runtime for one [`HostRequest`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostResponse {
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
    pub id: RequestId,
    pub result: Result<super::declarations::DeclaredResponse, super::declarations::HostFailure>,
}

/// Opaque presentation authority shared by one admitted command tree.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct CommandCapture {
    pub authority: CommandInvocationId,
    pub window: String,
    pub view: Option<String>,
    /// Stable contribution ID for a captured contributed tool, when any.
    pub tool: Option<String>,
}

/// A foreground-authorized command invocation routed to its extension owner.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandInvocation {
    pub id: CommandInvocationId,
    pub registration: CommandRegistrationId,
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
    pub arguments: CommandArgumentValue,
    pub capture: Option<CommandCapture>,
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

#[cfg(test)]
mod tests {
    use super::{
        BufferHandle, ByteRange, Command, CommandArgumentValue, CommandInvocationId,
        CommandInvokeDispatch, CommandOutcome, CommandRegistrationId, ExtensionId,
        ExtensionLifecycleId, HostRequest, HostResponse, RequestId, TextEdit,
    };
    use crate::host::declarations::HostError;

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
        use crate::app::declared_host::{ContextArguments, active_buffer};
        use crate::host::declarations::{DeclaredRequest, Responder};
        let request = HostRequest {
            extension: ExtensionId::new(7),
            lifecycle: ExtensionLifecycleId::new(3),
            id: RequestId::new(11),
            invocation: None,
            operation: DeclaredRequest::new::<active_buffer>(ContextArguments { target: None }),
        };
        let response = HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result: Ok(Responder::<active_buffer>::new().respond(Ok(Some(BufferHandle::new(3))))),
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
            serde_json::to_value(
                crate::app::declared_host::BufferError::RevisionConflict
                    .into_failure()
                    .code
            )
            .unwrap(),
            serde_json::json!("RevisionConflict")
        );
    }
}
