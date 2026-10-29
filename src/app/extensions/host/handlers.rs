//! Declared application capabilities and their foreground access context.
//!
//! A `host_operation` attribute pairs the async function's owned context,
//! arguments, semantic success and error types, and automatically links its
//! descriptor. Implement `HostArguments` using recognized fields from
//! `ArgumentReader`; implement `HostSuccess` for the result representation. Reuse
//! shared payloads and compose local errors with `HostError`'s transparent form.
//! Every public failure variant declares an explicit stable code and diagnostic
//! message; named fields become structured details.
//!
//! Access models only through short foreground closures returning owned values.
//! Retain captured authority across native awaits and revalidate it before commit.
//! Keep V8 conversion in the host adapters and update the checked TypeScript
//! facade/declarations when exposing or changing a public capability.

use super::*;
use crate::app::extensions::storage::{OwnerStorage, StorageError};
use crate::host::declarations::{
    ArgumentReader, DecodeError, EncodedValue, HostArguments, HostError, HostSuccess, encode_json,
    host_operation,
};
use gpui::WeakEntity;

#[derive(Clone)]
pub(crate) struct HostContext {
    host: WeakEntity<ProductExtensionHost>,
    application: gpui::AsyncApp,
    request: crate::host::protocol::RequestContext,
}

impl HostContext {
    pub(crate) fn new(
        request: crate::host::protocol::RequestContext,
        cx: &mut Context<ProductExtensionHost>,
    ) -> Self {
        Self {
            host: cx.entity().downgrade(),
            application: cx.to_async(),
            request,
        }
    }

    fn foreground<R: 'static>(
        &self,
        access: impl FnOnce(&mut ProductExtensionHost, &mut Context<ProductExtensionHost>) -> R,
    ) -> Result<R, AccessError> {
        self.host
            .update(&mut self.application.clone(), |host, cx| {
                if !host.operation_is_live(
                    ExtensionKey::new(self.request.extension, self.request.lifecycle),
                    self.request.invocation,
                ) {
                    return Err(AccessError::Cancelled);
                }
                let result = access(host, cx);
                host.drain_command_events(cx);
                Ok(result)
            })
            .map_err(|_| AccessError::Cancelled)?
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, HostError)]
pub(crate) enum AccessError {
    #[error(code = "Cancelled", message = "Cancelled")]
    Cancelled,
    #[error(code = "InvalidTarget", message = "InvalidTarget")]
    InvalidTarget,
    #[error(code = "ContextUnavailable", message = "ContextUnavailable")]
    ContextUnavailable,
    #[error(code = "OwnershipDisposed", message = "OwnershipDisposed")]
    OwnershipDisposed,
}

/// Failures of an extension transient interaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq, HostError)]
pub(crate) enum TransientError {
    #[error(code = "OperationInProgress", message = "OperationInProgress")]
    OperationInProgress,
    #[error(code = "ContextUnavailable", message = "ContextUnavailable")]
    ContextUnavailable,
    #[error(code = "InvalidTarget", message = "InvalidTarget")]
    InvalidTarget,
    #[error(code = "Cancelled", message = "Cancelled")]
    Cancelled,
}

impl From<AccessError> for TransientError {
    fn from(error: AccessError) -> Self {
        match error {
            AccessError::Cancelled | AccessError::OwnershipDisposed => Self::Cancelled,
            AccessError::InvalidTarget => Self::InvalidTarget,
            AccessError::ContextUnavailable => Self::ContextUnavailable,
        }
    }
}

/// Failures of the `workspaces.pickFolder` interaction and its path validation.
#[derive(Debug, HostError)]
pub(crate) enum FolderPickerError {
    #[error(transparent)]
    Transient(TransientError),
    #[error(transparent)]
    Workspace(WorkspaceError),
    #[error(
        code = "ProviderFailure",
        message = "the folder dialog failed: {message}"
    )]
    Dialog { message: String },
}

impl From<TransientError> for FolderPickerError {
    fn from(error: TransientError) -> Self {
        Self::Transient(error)
    }
}

impl From<AccessError> for FolderPickerError {
    fn from(error: AccessError) -> Self {
        Self::Transient(error.into())
    }
}

impl From<WorkspaceError> for FolderPickerError {
    fn from(error: WorkspaceError) -> Self {
        Self::Workspace(error)
    }
}

impl From<OpenError> for FolderPickerError {
    fn from(error: OpenError) -> Self {
        Self::Workspace(error.into())
    }
}

#[derive(Debug, HostError)]
pub(crate) enum OpenError {
    #[error(transparent)]
    Access(AccessError),
    #[error(code = "InvalidUri", message = "InvalidUri")]
    InvalidUri,
    #[error(code = "InvalidUtf8", message = "InvalidUtf8")]
    InvalidUtf8,
    #[error(code = "ResourceNotFound", message = "ResourceNotFound")]
    ResourceNotFound,
    #[error(code = "ResourceWrongKind", message = "ResourceWrongKind")]
    ResourceWrongKind,
    #[error(code = "ProviderFailure", message = "ProviderFailure")]
    ProviderFailure,
    #[error(code = "BufferClosed", message = "BufferClosed")]
    BufferClosed,
    #[error(code = "DocumentNotFound", message = "DocumentNotFound")]
    DocumentNotFound,
}

impl From<AccessError> for OpenError {
    fn from(error: AccessError) -> Self {
        Self::Access(error)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClipboardArguments {
    text: String,
}

impl HostArguments for ClipboardArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            text: reader.text(2, "clipboard text must be text")?,
        })
    }
}

#[host_operation("writeClipboardText")]
async fn write_clipboard(
    context: HostContext,
    arguments: ClipboardArguments,
) -> Result<(), AccessError> {
    context
        .foreground(|_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(arguments.text)))
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenArguments {
    uri: String,
    target: Option<crate::host::protocol::DocumentDestination>,
}

impl HostArguments for OpenArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            uri: reader.text(2, "resource URI must be text")?,
            target: reader.destination(3)?,
        })
    }
}

impl HostSuccess for crate::host::protocol::DocumentPresentation {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

#[host_operation("openDocument")]
async fn open_document(
    context: HostContext,
    arguments: OpenArguments,
) -> Result<crate::host::protocol::DocumentPresentation, OpenError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let host = context.host.clone();
    let request = context.request;
    context.foreground(move |state, cx| {
        let destination = state.document_destination(&request, arguments.target, cx)?;
        product::operations::open_document(
            arguments.uri,
            destination,
            product::operations::OperationCaller {
                host,
                key: ExtensionKey::new(request.extension, request.lifecycle),
                invocation: request.invocation,
            },
            move |result, _| {
                let _ = sender.send(result);
            },
            cx,
        )
    })??;
    let (document, target) = receiver.await.map_err(|_| AccessError::Cancelled)??;
    context.foreground(move |host, cx| {
        product::operations::validate_presentation(&target, cx)?;
        host.document_presentation(document, target, cx)
    })?
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DocumentLookup {
    pub id: String,
}

impl HostArguments for DocumentLookup {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            id: reader.text(2, "document identity must be text")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WindowLookup {
    pub target: crate::host::protocol::WindowTarget,
}

impl HostArguments for WindowLookup {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            target: crate::host::protocol::WindowTarget {
                window: reader.text(2, "window target must be text")?,
            },
        })
    }
}

#[host_operation("listDocuments")]
pub(crate) async fn list_documents(
    context: HostContext,
    _: (),
) -> Result<Vec<crate::host::protocol::DocumentInfo>, AccessError> {
    context.foreground(|host, cx| host.application_snapshot(cx).documents)
}

#[host_operation("getDocument")]
pub(crate) async fn get_document(
    context: HostContext,
    arguments: DocumentLookup,
) -> Result<Option<crate::host::protocol::DocumentInfo>, AccessError> {
    context.foreground(|host, cx| {
        host.application_snapshot(cx)
            .documents
            .into_iter()
            .find(|document| document.id == arguments.id)
    })
}

#[host_operation("listWindows")]
pub(crate) async fn list_windows(
    context: HostContext,
    _: (),
) -> Result<Vec<crate::host::protocol::WindowInfo>, AccessError> {
    context.foreground(|host, cx| host.application_snapshot(cx).windows)
}

#[host_operation("getWindow")]
pub(crate) async fn get_window(
    context: HostContext,
    arguments: WindowLookup,
) -> Result<Option<crate::host::protocol::WindowInfo>, AccessError> {
    context.foreground(|host, cx| {
        host.application_snapshot(cx)
            .windows
            .into_iter()
            .find(|window| window.target == arguments.target)
    })
}

#[host_operation("getWorkspace")]
pub(crate) async fn get_workspace(
    context: HostContext,
    arguments: WindowLookup,
) -> Result<Option<crate::host::protocol::WorkspaceInfo>, AccessError> {
    context.foreground(|host, cx| {
        host.application_snapshot(cx)
            .windows
            .into_iter()
            .find(|window| window.target == arguments.target)
            .map(|window| window.workspace)
    })
}

#[host_operation("observeWorkbench")]
pub(crate) async fn observe_workbench(
    context: HostContext,
    _: (),
) -> Result<crate::host::protocol::WorkbenchObservation, AccessError> {
    let key = ExtensionKey::new(context.request.extension, context.request.lifecycle);
    context.foreground(|host, cx| {
        host.refresh_application_observation(cx);
        host.application_observation.subscribe(key)
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkbenchSubscription {
    pub subscription: crate::host::protocol::WorkbenchSubscriptionId,
}

impl HostArguments for WorkbenchSubscription {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            subscription: crate::host::protocol::WorkbenchSubscriptionId::new(reader.integer(2)?),
        })
    }
}

#[host_operation("unobserveWorkbench")]
pub(crate) async fn unobserve_workbench(
    context: HostContext,
    arguments: WorkbenchSubscription,
) -> Result<(), AccessError> {
    let key = ExtensionKey::new(context.request.extension, context.request.lifecycle);
    context.foreground(|host, _| {
        if host
            .application_observation
            .unsubscribe(arguments.subscription, key)
        {
            Ok(())
        } else {
            Err(AccessError::OwnershipDisposed)
        }
    })?
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DestinationArguments {
    pub target: Option<crate::host::protocol::DocumentDestination>,
}

impl HostArguments for DestinationArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            target: reader.destination(2)?,
        })
    }
}

#[host_operation("createUntitled")]
pub(crate) async fn create_untitled(
    context: HostContext,
    arguments: DestinationArguments,
) -> Result<crate::host::protocol::DocumentPresentation, OpenError> {
    let request = context.request;
    context.foreground(|host, cx| {
        let destination = host.document_destination(&request, arguments.target, cx)?;
        let (document, target) = product::operations::create_untitled(destination, cx)?;
        host.document_presentation(document, target, cx)
    })?
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PresentArguments {
    pub id: String,
    pub target: Option<crate::host::protocol::DocumentDestination>,
}

impl HostArguments for PresentArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            id: reader.text(2, "document identity must be text")?,
            target: reader.destination(3)?,
        })
    }
}

#[host_operation("presentDocument")]
pub(crate) async fn present_document(
    context: HostContext,
    arguments: PresentArguments,
) -> Result<crate::host::protocol::DocumentPresentation, OpenError> {
    let request = context.request;
    context.foreground(|host, cx| {
        let destination = host.document_destination(&request, arguments.target, cx)?;
        let (document, target) =
            product::operations::present_document(arguments.id, destination, cx)?;
        host.document_presentation(document, target, cx)
    })?
}

#[derive(Debug, HostError)]
pub(crate) enum WorkspaceError {
    #[error(transparent)]
    Resource(OpenError),
}

impl From<AccessError> for WorkspaceError {
    fn from(error: AccessError) -> Self {
        Self::Resource(error.into())
    }
}

impl From<OpenError> for WorkspaceError {
    fn from(error: OpenError) -> Self {
        Self::Resource(error)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct WorkspaceArguments {
    pub roots: Vec<String>,
    pub target: Option<crate::host::protocol::WindowTarget>,
}

fn read_window(
    reader: &mut dyn ArgumentReader,
    index: usize,
) -> Result<Option<crate::host::protocol::WindowTarget>, DecodeError> {
    Ok(reader
        .optional_text(index, "window target must be text")?
        .map(|window| crate::host::protocol::WindowTarget { window }))
}

impl HostArguments for WorkspaceArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            roots: reader.text_list(2)?,
            target: read_window(reader, 3)?,
        })
    }
}

#[host_operation("replaceWorkspaceRoots")]
pub(crate) async fn replace_workspace_roots(
    context: HostContext,
    arguments: WorkspaceArguments,
) -> Result<crate::host::protocol::WorkspaceInfo, WorkspaceError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let request = context.request;
    let caller = product::operations::OperationCaller {
        host: context.host.clone(),
        key: ExtensionKey::new(request.extension, request.lifecycle),
        invocation: request.invocation,
    };
    context.foreground(|host, cx| {
        let target = match arguments.target {
            Some(target) => target,
            None => host.operation_window(&request, cx)?,
        };
        product::operations::replace_roots(
            arguments.roots,
            target,
            caller,
            move |result| {
                let _ = sender.send(result);
            },
            cx,
        )
    })??;
    let result = receiver.await.map_err(|_| AccessError::Cancelled)??;
    context.foreground(|_, _| result).map_err(Into::into)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FolderArguments {
    pub uri: String,
    pub target: Option<crate::host::protocol::WindowTarget>,
}

impl HostArguments for FolderArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            uri: reader.text(2, "resource URI must be text")?,
            target: read_window(reader, 3)?,
        })
    }
}

#[host_operation("openFolder")]
pub(crate) async fn open_folder(
    context: HostContext,
    arguments: FolderArguments,
) -> Result<crate::host::protocol::WorkspaceInfo, WorkspaceError> {
    replace_workspace_roots(
        context,
        WorkspaceArguments {
            roots: vec![arguments.uri],
            target: arguments.target,
        },
    )
    .await
}

#[derive(Debug, HostError)]
pub(crate) enum CloseError {
    #[error(transparent)]
    Access(AccessError),
    #[error(code = "OperationInProgress", message = "OperationInProgress")]
    OperationInProgress,
    #[error(code = "PersistenceFailed", message = "PersistenceFailed")]
    PersistenceFailed,
}

impl From<AccessError> for CloseError {
    fn from(error: AccessError) -> Self {
        Self::Access(error)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CloseArguments {
    pub target: crate::host::protocol::PresentationTarget,
}

impl HostArguments for CloseArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        match reader.destination(2)? {
            Some(crate::host::protocol::DocumentDestination::View(target)) => Ok(Self { target }),
            _ => Err(DecodeError::Type("close requires an exact view target")),
        }
    }
}

#[host_operation("closePresentation")]
pub(crate) async fn close_presentation(
    context: HostContext,
    arguments: CloseArguments,
) -> Result<crate::host::protocol::PresentationCloseOutcome, CloseError> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let caller = product::operations::OperationCaller {
        host: context.host.clone(),
        key: ExtensionKey::new(context.request.extension, context.request.lifecycle),
        invocation: context.request.invocation,
    };
    context.foreground(|_, cx| {
        product::operations::close_presentation(
            arguments.target,
            caller,
            move |result| {
                let _ = sender.send(result);
            },
            cx,
        )
    })??;
    let result = receiver.await.map_err(|_| AccessError::Cancelled)??;
    context.foreground(|_, _| result).map_err(Into::into)
}

pub(crate) use crate::app::extensions::buffers::BufferError;
use crate::host::protocol::{
    BufferHandle, BufferSubscriptionId, ByteRange, EditorContribution, TextEdit, TextSnapshot,
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SnapshotArguments {
    pub buffer: BufferHandle,
    pub range: Option<ByteRange>,
}
impl HostArguments for SnapshotArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            buffer: BufferHandle::new(reader.integer(2)?),
            range: reader.range(3)?,
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EditArguments {
    pub buffer: BufferHandle,
    pub edits: Vec<TextEdit>,
    pub if_revision: u64,
}
impl HostArguments for EditArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            buffer: BufferHandle::new(reader.integer(2)?),
            edits: reader.edits(3)?,
            if_revision: reader.integer(4)?,
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BufferArguments {
    pub buffer: BufferHandle,
}
impl HostArguments for BufferArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            buffer: BufferHandle::new(reader.integer(2)?),
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SubscriptionArguments {
    pub subscription: BufferSubscriptionId,
}
impl HostArguments for SubscriptionArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            subscription: BufferSubscriptionId::new(reader.integer(2)?),
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ContributionArguments {
    pub buffer: BufferHandle,
    pub contributions: Vec<EditorContribution>,
    pub if_revision: u64,
}
impl HostArguments for ContributionArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            buffer: BufferHandle::new(reader.integer(2)?),
            contributions: reader.contributions(3)?,
            if_revision: reader.integer(4)?,
        })
    }
}

#[host_operation("snapshot")]
pub(crate) async fn snapshot(
    context: HostContext,
    arguments: SnapshotArguments,
) -> Result<TextSnapshot, BufferError> {
    let request = context.request;
    context.foreground(|host, cx| {
        let commands = &host.commands;
        host.buffers.snapshot(
            &request,
            arguments,
            || commands.is_cancelled(request.invocation),
            cx,
        )
    })?
}

#[host_operation("applyEdits")]
pub(crate) async fn apply_edits(
    context: HostContext,
    arguments: EditArguments,
) -> Result<u64, BufferError> {
    let request = context.request;
    context.foreground(|host, cx| {
        let commands = &host.commands;
        host.buffers.apply_edits(
            &request,
            arguments,
            || commands.is_cancelled(request.invocation),
            cx,
        )
    })?
}

#[host_operation("subscribeBufferChanges")]
pub(crate) async fn subscribe_buffer_changes(
    context: HostContext,
    arguments: BufferArguments,
) -> Result<BufferSubscriptionId, BufferError> {
    let request = context.request;
    context.foreground(|host, cx| {
        let commands = &host.commands;
        host.buffers.subscribe_buffer_changes(
            &request,
            arguments,
            || commands.is_cancelled(request.invocation),
            cx,
        )
    })?
}

#[host_operation("unsubscribeBufferChanges")]
pub(crate) async fn unsubscribe_buffer_changes(
    context: HostContext,
    arguments: SubscriptionArguments,
) -> Result<(), BufferError> {
    let request = context.request;
    context.foreground(|host, cx| {
        let commands = &host.commands;
        host.buffers.unsubscribe_buffer_changes(
            &request,
            arguments,
            || commands.is_cancelled(request.invocation),
            cx,
        )
    })?
}

#[host_operation("replaceEditorContributions")]
pub(crate) async fn replace_editor_contributions(
    context: HostContext,
    arguments: ContributionArguments,
) -> Result<(), BufferError> {
    let request = context.request;
    context.foreground(|host, cx| {
        let commands = &host.commands;
        host.buffers.replace_editor_contributions(
            &request,
            arguments,
            || commands.is_cancelled(request.invocation),
            cx,
        )
    })?
}

#[host_operation("disposeEditorContributions")]
pub(crate) async fn dispose_editor_contributions(
    context: HostContext,
    arguments: BufferArguments,
) -> Result<(), BufferError> {
    let request = context.request;
    context.foreground(|host, cx| {
        let commands = &host.commands;
        host.buffers.dispose_editor_contributions(
            &request,
            arguments,
            || commands.is_cancelled(request.invocation),
            cx,
        )
    })?
}

use crate::app::extensions::semantics::{SemanticError, ViewPresentation};
use crate::host::protocol::{
    CompletionProviderRegistrationId, ConfirmationResult, FolderPickerResult, InputResult,
    PickerItem, PickerResult, ProviderIdentity, TreeAction, TreeProviderRegistrationId,
    ViewContentKind,
};

use crate::app::transient::{PromptText, TransientOutcome, TransientOwner};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TreeArguments {
    pub id: String,
    pub title: String,
    pub content: ViewContentKind,
    pub actions: Vec<TreeAction>,
    pub location: crate::app::tool_layout::StoredLocation,
    pub order: Option<u64>,
    pub visible: bool,
    pub language: Option<String>,
}
impl HostArguments for TreeArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        let location = match reader.text(4, "view placement must be text")?.as_str() {
            "left" => crate::app::tool_layout::StoredLocation::Left,
            "right" => crate::app::tool_layout::StoredLocation::Right,
            "bottom" => crate::app::tool_layout::StoredLocation::Bottom,
            "document" => crate::app::tool_layout::StoredLocation::DocumentGroup,
            _ => return Err(DecodeError::Type("invalid view placement")),
        };
        let content = match reader.text(7, "view content kind must be text")?.as_str() {
            "tree" => ViewContentKind::Tree,
            "text" => ViewContentKind::Text,
            _ => return Err(DecodeError::Type("invalid view content kind")),
        };
        let actions = serde_json::to_value(reader.command_arguments(8)?)
            .ok()
            .and_then(|value| serde_json::from_value::<Vec<TreeAction>>(value).ok())
            .ok_or(DecodeError::Type("invalid view actions"))?;
        Ok(Self {
            id: reader.text(2, "view id must be text")?,
            title: reader.text(3, "view title must be text")?,
            content,
            actions,
            location,
            order: reader.optional_integer(5)?,
            visible: reader.integer(6)? != 0,
            language: reader.optional_text(9, "view language must be text or null")?,
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CompletionArguments {
    pub label: String,
}
impl HostArguments for CompletionArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            label: reader.text(2, "UnsupportedOperation")?,
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TreeRegistrationArguments {
    pub registration: TreeProviderRegistrationId,
}
impl HostArguments for TreeRegistrationArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            registration: TreeProviderRegistrationId::new(reader.integer(2)?),
        })
    }
    fn provider_mutation(&self) -> Option<ProviderIdentity> {
        Some(ProviderIdentity::Tree(self.registration))
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TreeInvalidationArguments {
    pub registration: TreeProviderRegistrationId,
    pub parent_id: Option<String>,
    pub window: Option<String>,
}
impl HostArguments for TreeInvalidationArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            registration: TreeProviderRegistrationId::new(reader.integer(2)?),
            parent_id: reader.optional_text(3, "UnsupportedOperation")?,
            window: reader.optional_text(4, "window target must be text")?,
        })
    }
    fn provider_mutation(&self) -> Option<ProviderIdentity> {
        Some(ProviderIdentity::Tree(self.registration))
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CompletionRegistrationArguments {
    pub registration: CompletionProviderRegistrationId,
}
impl HostArguments for CompletionRegistrationArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            registration: CompletionProviderRegistrationId::new(reader.integer(2)?),
        })
    }
    fn provider_mutation(&self) -> Option<ProviderIdentity> {
        Some(ProviderIdentity::Completion(self.registration))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReplaceViewTextArguments {
    pub registration: TreeProviderRegistrationId,
    pub text: String,
    pub window: Option<String>,
}
impl HostArguments for ReplaceViewTextArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            registration: TreeProviderRegistrationId::new(reader.integer(2)?),
            text: reader.text(3, "view text must be text")?,
            window: reader.optional_text(4, "window target must be text")?,
        })
    }
    fn provider_mutation(&self) -> Option<ProviderIdentity> {
        Some(ProviderIdentity::Tree(self.registration))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewOperationArguments {
    pub registration: TreeProviderRegistrationId,
    pub window: Option<String>,
}
impl HostArguments for ViewOperationArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            registration: TreeProviderRegistrationId::new(reader.integer(2)?),
            window: reader.optional_text(3, "window target must be text")?,
        })
    }
    fn provider_mutation(&self) -> Option<ProviderIdentity> {
        Some(ProviderIdentity::Tree(self.registration))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PickerArguments {
    pub title: String,
    pub items: Vec<PickerItem>,
    pub target: Option<String>,
}
impl HostArguments for PickerArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        let items = serde_json::to_value(reader.command_arguments(3)?)
            .ok()
            .and_then(|value| serde_json::from_value::<Vec<PickerItem>>(value).ok())
            .ok_or(DecodeError::Type("invalid picker items"))?;
        Ok(Self {
            title: reader.text(2, "picker title must be text")?,
            items,
            target: reader.optional_text(4, "window target must be text")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InputArguments {
    pub title: String,
    pub value: Option<String>,
    pub target: Option<String>,
}
impl HostArguments for InputArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            title: reader.text(2, "input title must be text")?,
            value: reader.optional_text(3, "input value must be text or null")?,
            target: reader.optional_text(4, "window target must be text")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConfirmationArguments {
    pub title: String,
    pub message: Option<String>,
    pub confirm_label: Option<String>,
    pub cancel_label: Option<String>,
    pub target: Option<String>,
}
impl HostArguments for ConfirmationArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            title: reader.text(2, "confirmation title must be text")?,
            message: reader.optional_text(3, "confirmation message must be text or null")?,
            confirm_label: reader.optional_text(4, "confirm label must be text or null")?,
            cancel_label: reader.optional_text(5, "cancel label must be text or null")?,
            target: reader.optional_text(6, "window target must be text")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MessageArguments {
    pub title: String,
    pub message: Option<String>,
    pub target: Option<String>,
}
impl HostArguments for MessageArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            title: reader.text(2, "message title must be text")?,
            message: reader.optional_text(3, "message body must be text or null")?,
            target: reader.optional_text(4, "window target must be text")?,
        })
    }
}

fn transient_owner(request: &crate::host::protocol::RequestContext) -> TransientOwner {
    TransientOwner {
        extension: request.extension,
        lifecycle: request.lifecycle,
        invocation: request.invocation,
    }
}

#[host_operation("showPicker")]
pub(crate) async fn show_picker(
    context: HostContext,
    arguments: PickerArguments,
) -> Result<PickerResult, TransientError> {
    let request = context.request;
    let owner = transient_owner(&request);
    let PickerArguments {
        title,
        items,
        target,
    } = arguments;
    let receiver = context.foreground(|host, cx| {
        host.with_transient_window(
            &request,
            target.as_deref(),
            move |shell, window, cx| shell.begin_picker(owner, title, items, window, cx),
            cx,
        )
    })??;
    match receiver.await {
        Ok(TransientOutcome::Picker(id)) => Ok(PickerResult::Selected { id }),
        Ok(TransientOutcome::Dismissed) => Ok(PickerResult::Dismissed),
        _ => Err(TransientError::Cancelled),
    }
}

#[host_operation("showInput")]
pub(crate) async fn show_input(
    context: HostContext,
    arguments: InputArguments,
) -> Result<InputResult, TransientError> {
    let request = context.request;
    let owner = transient_owner(&request);
    let InputArguments {
        title,
        value,
        target,
    } = arguments;
    let receiver = context.foreground(|host, cx| {
        host.with_transient_window(
            &request,
            target.as_deref(),
            move |shell, window, cx| {
                shell.begin_input(owner, title, value.unwrap_or_default(), window, cx)
            },
            cx,
        )
    })??;
    match receiver.await {
        Ok(TransientOutcome::Input(value)) => Ok(InputResult::Accepted { value }),
        Ok(TransientOutcome::Dismissed) => Ok(InputResult::Dismissed),
        _ => Err(TransientError::Cancelled),
    }
}

#[host_operation("showConfirmation")]
pub(crate) async fn show_confirmation(
    context: HostContext,
    arguments: ConfirmationArguments,
) -> Result<ConfirmationResult, TransientError> {
    let request = context.request;
    let owner = transient_owner(&request);
    let ConfirmationArguments {
        title,
        message,
        confirm_label,
        cancel_label,
        target,
    } = arguments;
    let receiver = context.foreground(|host, cx| {
        host.with_transient_window(
            &request,
            target.as_deref(),
            move |shell, window, cx| {
                shell.begin_confirmation(
                    owner,
                    PromptText { title, message },
                    confirm_label.unwrap_or_else(|| "OK".to_owned()),
                    cancel_label.unwrap_or_else(|| "Cancel".to_owned()),
                    window,
                    cx,
                )
            },
            cx,
        )
    })??;
    match receiver.await {
        Ok(TransientOutcome::Confirmed) => Ok(ConfirmationResult::Confirmed),
        Ok(TransientOutcome::Dismissed) => Ok(ConfirmationResult::Dismissed),
        _ => Err(TransientError::Cancelled),
    }
}

#[host_operation("showMessage")]
pub(crate) async fn show_message(
    context: HostContext,
    arguments: MessageArguments,
) -> Result<(), TransientError> {
    let request = context.request;
    let owner = transient_owner(&request);
    let MessageArguments {
        title,
        message,
        target,
    } = arguments;
    let receiver = context.foreground(|host, cx| {
        host.with_transient_window(
            &request,
            target.as_deref(),
            move |shell, window, cx| {
                shell.begin_message(owner, PromptText { title, message }, window, cx)
            },
            cx,
        )
    })??;
    match receiver.await {
        Ok(_) => Ok(()),
        Err(_) => Err(TransientError::Cancelled),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FolderPickerArguments {
    pub target: Option<String>,
}

impl HostArguments for FolderPickerArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            target: reader.optional_text(2, "window target must be text")?,
        })
    }
}

#[host_operation("pickFolder")]
pub(crate) async fn pick_folder(
    context: HostContext,
    arguments: FolderPickerArguments,
) -> Result<FolderPickerResult, FolderPickerError> {
    let request = context.request;
    let owner = transient_owner(&request);
    let receiver = context.foreground(|host, cx| {
        host.with_transient_window(
            &request,
            arguments.target.as_deref(),
            move |shell, window, cx| shell.begin_folder_picker(Some(owner), window, cx),
            cx,
        )
    })??;
    let path = match receiver.await {
        Ok(TransientOutcome::Folder(path)) => path,
        Ok(TransientOutcome::Dismissed) => return Ok(FolderPickerResult::Dismissed),
        Ok(TransientOutcome::Failed(message)) => return Err(FolderPickerError::Dialog { message }),
        _ => return Err(TransientError::Cancelled.into()),
    };
    let (sender, response) = tokio::sync::oneshot::channel();
    context.foreground(|_, cx| {
        product::operations::resolve_directory(
            path,
            move |result| {
                let _ = sender.send(result);
            },
            cx,
        )
    })?;
    let result = response.await.map_err(|_| TransientError::Cancelled)?;
    let uri = context.foreground(|_, _| result)??;
    Ok(FolderPickerResult::Selected {
        uri: uri.to_string(),
    })
}

#[host_operation("registerView")]
pub(crate) async fn register_tree_provider(
    context: HostContext,
    arguments: TreeArguments,
) -> Result<TreeProviderRegistrationId, SemanticError> {
    let request = context.request;
    context.foreground(|host, cx| host.register_view(&request, arguments, cx))?
}

#[host_operation("replaceViewText")]
pub(crate) async fn replace_view_text(
    context: HostContext,
    arguments: ReplaceViewTextArguments,
) -> Result<(), SemanticError> {
    let request = context.request;
    context.foreground(|host, cx| host.semantics.replace_view_text(&request, arguments, cx))?
}

#[host_operation("invalidateView")]
pub(crate) async fn invalidate_tree_provider(
    context: HostContext,
    arguments: TreeInvalidationArguments,
) -> Result<(), SemanticError> {
    let request = context.request;
    context.foreground(|host, cx| {
        host.semantics
            .invalidate_tree_provider(&request, arguments, cx)
    })?
}

#[host_operation("unregisterView")]
pub(crate) async fn unregister_tree_provider(
    context: HostContext,
    arguments: TreeRegistrationArguments,
) -> Result<(), SemanticError> {
    let request = context.request;
    context.foreground(|host, cx| host.unregister_view(&request, arguments, cx))?
}

#[host_operation("showView")]
pub(crate) async fn show_view(
    context: HostContext,
    arguments: ViewOperationArguments,
) -> Result<(), SemanticError> {
    let request = context.request;
    context.foreground(|host, cx| {
        host.present_view(
            &request,
            arguments.registration,
            arguments.window.as_deref(),
            ViewPresentation::Show,
            cx,
        )
    })?
}

#[host_operation("hideView")]
pub(crate) async fn hide_view(
    context: HostContext,
    arguments: ViewOperationArguments,
) -> Result<(), SemanticError> {
    let request = context.request;
    context.foreground(|host, cx| {
        host.present_view(
            &request,
            arguments.registration,
            arguments.window.as_deref(),
            ViewPresentation::Hide,
            cx,
        )
    })?
}

#[host_operation("focusView")]
pub(crate) async fn focus_view(
    context: HostContext,
    arguments: ViewOperationArguments,
) -> Result<(), SemanticError> {
    let request = context.request;
    context.foreground(|host, cx| {
        host.present_view(
            &request,
            arguments.registration,
            arguments.window.as_deref(),
            ViewPresentation::Focus,
            cx,
        )
    })?
}

#[host_operation("registerCompletionProvider")]
pub(crate) async fn register_completion_provider(
    context: HostContext,
    arguments: CompletionArguments,
) -> Result<CompletionProviderRegistrationId, SemanticError> {
    let request = context.request;
    context.foreground(|host, cx| {
        host.semantics
            .register_completion_provider(&request, arguments, cx)
    })?
}

#[host_operation("unregisterCompletionProvider")]
pub(crate) async fn unregister_completion_provider(
    context: HostContext,
    arguments: CompletionRegistrationArguments,
) -> Result<(), SemanticError> {
    let request = context.request;
    context.foreground(|host, cx| {
        let registration = arguments.registration;
        host.semantics
            .unregister_completion_provider(&request, arguments, cx)?;
        host.cancel_completion_provider(registration, cx);
        Ok(())
    })?
}

use crate::app::commands::extension::CommandError;
use crate::host::protocol::{
    Command, CommandInvokeDispatch, CommandName, CommandOutcome, CommandRegistrationId,
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RegisterCommandArguments {
    pub name: CommandName,
    pub title: String,
}
impl HostArguments for RegisterCommandArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            name: reader.text(2, "UnsupportedOperation")?.into(),
            title: reader.text(3, "UnsupportedOperation")?,
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RegisterViewArguments {
    pub view_kind: String,
    pub name: CommandName,
}
impl HostArguments for RegisterViewArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            view_kind: reader.text(2, "UnsupportedOperation")?,
            name: reader.text(3, "UnsupportedOperation")?.into(),
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CommandRegistrationArguments {
    pub registration: CommandRegistrationId,
}
impl HostArguments for CommandRegistrationArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            registration: CommandRegistrationId::new(reader.integer(2)?),
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InlineArguments {
    pub invocation: CommandInvocationId,
    pub outcome: CommandOutcome,
}
impl HostArguments for InlineArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            invocation: CommandInvocationId::new(reader.integer(2)?),
            outcome: reader.outcome(3)?,
        })
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InvokeArguments {
    pub command: Command,
}
impl HostArguments for InvokeArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            command: Command {
                name: reader.text(2, "UnsupportedOperation")?.into(),
                arguments: reader.command_arguments(3)?,
            },
        })
    }
}

#[host_operation("registerCommand")]
pub(crate) async fn register_command(
    context: HostContext,
    arguments: RegisterCommandArguments,
) -> Result<CommandRegistrationId, CommandError> {
    let request = context.request;
    context.foreground(|host, _| {
        host.commands
            .register_command(&request, arguments.name, arguments.title)
    })?
}
#[host_operation("registerViewCommand")]
pub(crate) async fn register_view_command(
    context: HostContext,
    arguments: RegisterViewArguments,
) -> Result<CommandRegistrationId, CommandError> {
    let request = context.request;
    context.foreground(|host, _| {
        host.commands
            .register_view_command(&request, arguments.view_kind, arguments.name)
    })?
}
#[host_operation("unregisterCommand")]
pub(crate) async fn unregister_command(
    context: HostContext,
    arguments: CommandRegistrationArguments,
) -> Result<(), CommandError> {
    let request = context.request;
    context.foreground(|host, _| {
        host.commands
            .unregister_command(&request, arguments.registration)
    })?
}
#[host_operation("completeInlineCommand")]
pub(crate) async fn complete_inline_command(
    context: HostContext,
    arguments: InlineArguments,
) -> Result<(), AccessError> {
    let request = context.request;
    context.foreground(|host, _| {
        host.commands
            .complete_inline(&request, arguments.invocation, arguments.outcome)
    })?
}
#[host_operation("invokeCommand")]
pub(crate) async fn invoke_command(
    context: HostContext,
    arguments: InvokeArguments,
) -> Result<CommandInvokeDispatch, AccessError> {
    let request = context.request;
    context.foreground(|_, _| ())?;
    if request.invocation.is_none() {
        return Err(AccessError::ContextUnavailable);
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    context.foreground(|_, cx| {
        cx.defer(move |cx| route_nested_command(request, arguments.command, sender, cx))
    })?;
    receiver.await.map_err(|_| AccessError::Cancelled)?
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ContextArguments {
    pub target: Option<CommandInvocationId>,
}
impl HostArguments for ContextArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            target: reader.optional_integer(2)?.map(CommandInvocationId::new),
        })
    }
}
#[derive(Debug, HostError)]
pub(crate) enum ContextError {
    #[error(transparent)]
    Access(AccessError),
    #[error(code = "UnsupportedOperation", message = "UnsupportedOperation")]
    UnsupportedOperation,
}
impl From<AccessError> for ContextError {
    fn from(error: AccessError) -> Self {
        Self::Access(error)
    }
}

#[host_operation("activeBuffer")]
pub(crate) async fn active_buffer(
    context: HostContext,
    arguments: ContextArguments,
) -> Result<Option<BufferHandle>, AccessError> {
    let request = context.request;
    context.foreground(|host, cx| host.contextual_buffer(&request, arguments.target, cx))?
}
#[host_operation("selectedViewText")]
pub(crate) async fn selected_view_text(
    context: HostContext,
    arguments: ContextArguments,
) -> Result<Option<String>, ContextError> {
    let request = context.request;
    context.foreground(|host, cx| host.contextual_selected_text(&request, arguments.target, cx))?
}

pub(crate) use crate::host::protocol::KeybindingArguments;

#[derive(Debug, HostError)]
pub(crate) enum KeymapError {
    #[error(transparent)]
    Access(AccessError),
    #[error(code = "UnsupportedOperation", message = "{message}")]
    Invalid { message: String },
}
impl KeymapError {
    pub(crate) fn invalid(message: String) -> Self {
        Self::Invalid { message }
    }
}
impl From<AccessError> for KeymapError {
    fn from(error: AccessError) -> Self {
        Self::Access(error)
    }
}

#[host_operation("mutateKeybindings")]
pub(crate) async fn mutate_keybindings(
    context: HostContext,
    arguments: KeybindingArguments,
) -> Result<(), KeymapError> {
    let key = ExtensionKey::new(context.request.extension, context.request.lifecycle);
    context.foreground(|host, cx| {
        if !cx.has_global::<crate::app::commands::keymaps::ApplicationKeymaps>() {
            return Err(KeymapError::invalid(
                "application keymap is unavailable".into(),
            ));
        }
        let owner = if host.personal_config_key == Some(key) {
            crate::app::commands::keymaps::BindingOwner::Personal(key.extension, key.lifecycle)
        } else {
            crate::app::commands::keymaps::BindingOwner::Extension(key.extension, key.lifecycle)
        };
        let result = cx
            .global_mut::<crate::app::commands::keymaps::ApplicationKeymaps>()
            .0
            .apply_batch(owner, &arguments.mutations);
        if matches!(result, Ok(true)) {
            crate::app::commands::keymaps::rebuild(cx);
        }
        result.map(|_| ()).map_err(KeymapError::invalid)
    })?
}

#[derive(Debug, HostError)]
pub(crate) enum RecordError {
    #[error(transparent)]
    Access(AccessError),
    #[error(code = "StorageUnavailable", message = "Storage {database}: {cause}")]
    Unavailable { database: String, cause: String },
}

impl From<AccessError> for RecordError {
    fn from(error: AccessError) -> Self {
        Self::Access(error)
    }
}

impl From<StorageError> for RecordError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::Cancelled => Self::Access(AccessError::Cancelled),
            StorageError::Unavailable { database, cause } => Self::Unavailable {
                database: database.display().to_string(),
                cause,
            },
        }
    }
}

impl HostContext {
    fn records(&self) -> Result<OwnerStorage, RecordError> {
        self.foreground(|host, _| {
            if host.storage_stopping {
                return Err(AccessError::Cancelled.into());
            }
            let key = ExtensionKey::new(self.request.extension, self.request.lifecycle);
            let owner = host
                .storage_owners
                .get(&key)
                .ok_or(AccessError::ContextUnavailable)?;
            let storage = host
                .storage
                .as_ref()
                .map_err(|cause| RecordError::Unavailable {
                    database: "local application-data directory".into(),
                    cause: cause.clone(),
                })?;
            let records = storage.for_owner(owner.clone());
            let caller = Arc::new(std::sync::atomic::AtomicBool::new(true));
            host.storage_callers
                .push((self.request, Arc::downgrade(&caller)));
            Ok(records.with_caller(caller))
        })?
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordKey {
    key: String,
}
impl HostArguments for RecordKey {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            key: reader.text(2, "storage key must be text")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RecordWrite {
    key: String,
    value: serde_json::Value,
}
impl HostArguments for RecordWrite {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            key: reader.text(2, "storage key must be text")?,
            value: serde_json::to_value(reader.command_arguments(3)?).expect("validated JSON"),
        })
    }
}

pub(crate) struct RecordResult(Option<serde_json::Value>);
impl HostSuccess for RecordResult {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        match self.0 {
            Some(value) => encode_json(serde_json::json!({ "found": true, "value": value })),
            None => encode_json(serde_json::json!({ "found": false })),
        }
    }
}

#[host_operation("storageRead")]
async fn storage_read(
    context: HostContext,
    arguments: RecordKey,
) -> Result<RecordResult, RecordError> {
    Ok(RecordResult(context.records()?.read(arguments.key).await?))
}

#[host_operation("storageWrite")]
async fn storage_write(context: HostContext, arguments: RecordWrite) -> Result<(), RecordError> {
    Ok(context
        .records()?
        .write(arguments.key, arguments.value)
        .await?)
}

#[host_operation("storageDelete")]
async fn storage_delete(context: HostContext, arguments: RecordKey) -> Result<(), RecordError> {
    Ok(context.records()?.delete(arguments.key).await?)
}

#[host_operation("storageList")]
async fn storage_list(context: HostContext, _: ()) -> Result<Vec<String>, RecordError> {
    Ok(context.records()?.list().await?)
}

#[derive(Debug, HostError)]
pub(crate) enum RecoveryError {
    #[error(transparent)]
    Open(OpenError),
    #[error(code = "CaptureNotFound", message = "CaptureNotFound")]
    CaptureNotFound,
    #[error(code = "RecoveryUnavailable", message = "Recovery: {cause}")]
    Unavailable { cause: String },
}

impl From<AccessError> for RecoveryError {
    fn from(error: AccessError) -> Self {
        Self::Open(error.into())
    }
}

impl From<OpenError> for RecoveryError {
    fn from(error: OpenError) -> Self {
        Self::Open(error)
    }
}

impl From<String> for RecoveryError {
    fn from(cause: String) -> Self {
        Self::Unavailable { cause }
    }
}

impl From<crate::app::recovery::RestoreError> for RecoveryError {
    fn from(error: crate::app::recovery::RestoreError) -> Self {
        use crate::app::recovery::RestoreError;
        match error {
            RestoreError::Unavailable => Self::CaptureNotFound,
            RestoreError::Resource(error) => Self::Open(product::operations::resource_error(error)),
            RestoreError::Store(cause) => Self::Unavailable { cause },
        }
    }
}

impl HostContext {
    /// The calling lifecycle's canonical owner, stamped at admission. Recovery
    /// captures are never addressed by an author-supplied owner.
    fn recovery_owner(&self) -> Result<String, RecoveryError> {
        self.foreground(|host, _| {
            let key = ExtensionKey::new(self.request.extension, self.request.lifecycle);
            host.storage_owners
                .get(&key)
                .map(|owner| owner.identity())
                .ok_or(AccessError::ContextUnavailable)
        })?
        .map_err(RecoveryError::from)
    }
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetainedCapture {
    id: String,
    document: String,
    title: String,
    kind: &'static str,
    resource_uri: Option<String>,
    captured_at: i64,
}

impl From<crate::app::recovery::StoredSnapshot> for RetainedCapture {
    fn from(snapshot: crate::app::recovery::StoredSnapshot) -> Self {
        use crate::app::recovery::SnapshotState;
        Self {
            id: snapshot.capture_key,
            document: snapshot.document_id.to_string(),
            title: snapshot.title,
            kind: match snapshot.state {
                SnapshotState::Untitled => "untitled",
                SnapshotState::Destination => "destination",
                SnapshotState::Persisted => "persisted",
            },
            resource_uri: snapshot.resource_uri,
            captured_at: snapshot.captured_at_ms,
        }
    }
}

impl HostSuccess for RetainedCapture {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CaptureLookup {
    id: String,
}

impl HostArguments for CaptureLookup {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            id: reader.text(2, "capture identity must be text")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CaptureRestore {
    id: String,
    target: Option<crate::host::protocol::DocumentDestination>,
}

impl HostArguments for CaptureRestore {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            id: reader.text(2, "capture identity must be text")?,
            target: reader.destination(3)?,
        })
    }
}

pub(crate) struct RecoveryRestore {
    outcome: serde_json::Value,
    presentation: crate::host::protocol::DocumentPresentation,
}

impl HostSuccess for RecoveryRestore {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        let mut value = self.outcome;
        value["presentation"] = serde_json::to_value(self.presentation).expect("serializable");
        encode_json(value)
    }
}

#[host_operation("recoveryList")]
async fn recovery_list(context: HostContext, _: ()) -> Result<Vec<RetainedCapture>, RecoveryError> {
    let owner = context.recovery_owner()?;
    let listing = context.foreground(|_, cx| crate::app::recovery::retained(owner, cx))?;
    Ok(listing.await?.into_iter().map(Into::into).collect())
}

#[host_operation("recoveryRetain")]
async fn recovery_retain(
    context: HostContext,
    arguments: DocumentLookup,
) -> Result<RetainedCapture, RecoveryError> {
    let owner = context.recovery_owner()?;
    let document = uuid::Uuid::parse_str(&arguments.id).map_err(|_| OpenError::DocumentNotFound)?;
    let retain = context
        .foreground(|_, cx| crate::app::recovery::retain(owner, document, cx))?
        .ok_or(OpenError::DocumentNotFound)?;
    Ok(retain.await?.into())
}

#[host_operation("recoveryRelease")]
async fn recovery_release(
    context: HostContext,
    arguments: CaptureLookup,
) -> Result<bool, RecoveryError> {
    let owner = context.recovery_owner()?;
    let release =
        context.foreground(|_, cx| crate::app::recovery::release(owner, arguments.id, cx))?;
    Ok(release.await?)
}

#[host_operation("recoveryRestore")]
async fn recovery_restore(
    context: HostContext,
    arguments: CaptureRestore,
) -> Result<RecoveryRestore, RecoveryError> {
    use crate::app::recovery::{RestoreOutcome, RestoredResource};
    let owner = context.recovery_owner()?;
    let request = context.request;
    let (destination, restore) = context.foreground(|host, cx| {
        let destination = host.document_destination(&request, arguments.target, cx)?;
        let restore = product::operations::restore_retained(owner, arguments.id, cx);
        Ok::<_, AccessError>((destination, restore))
    })??;
    let outcome = restore.await?;
    context.foreground(move |host, cx| {
        let (document, outcome) = match outcome {
            RestoreOutcome::Restored { document, resource } => (
                document,
                serde_json::json!({
                    "outcome": "restored",
                    "resource": match resource {
                        RestoredResource::Untitled => "untitled",
                        RestoredResource::Unchanged => "unchanged",
                        RestoredResource::Missing => "missing",
                        RestoredResource::Changed => "changed",
                    },
                }),
            ),
            RestoreOutcome::Preserved { owner, document } => {
                let documents = cx.global::<crate::app::documents::ApplicationDocuments>();
                let owner = documents
                    .0
                    .read(cx)
                    .get(owner)
                    .map(|owner| owner.identity());
                (
                    document,
                    serde_json::json!({
                        "outcome": "preserved",
                        "owner": owner.map(|owner| owner.to_string()),
                    }),
                )
            }
            RestoreOutcome::AlreadyOpen { .. } => {
                unreachable!("only automatic snapshots restore as already open")
            }
        };
        let identity = cx
            .global::<crate::app::documents::ApplicationDocuments>()
            .0
            .read(cx)
            .get(document)
            .map(|document| document.identity().to_string())
            .ok_or(OpenError::DocumentNotFound)?;
        let (document, target) = product::operations::present_document(identity, destination, cx)?;
        Ok(RecoveryRestore {
            outcome,
            presentation: host.document_presentation(document, target, cx)?,
        })
    })?
}

/// Failures of `workbench.replaceWindows`. A rejection leaves every window,
/// document, and capture as it was.
#[derive(Debug, HostError)]
pub(crate) enum ReplaceError {
    #[error(transparent)]
    Resource(OpenError),
    #[error(code = "OperationInProgress", message = "OperationInProgress")]
    OperationInProgress,
    #[error(code = "SessionConflict", message = "SessionConflict: {reason}")]
    Conflict { reason: String },
}

impl From<AccessError> for ReplaceError {
    fn from(error: AccessError) -> Self {
        Self::Resource(error.into())
    }
}

impl From<OpenError> for ReplaceError {
    fn from(error: OpenError) -> Self {
        Self::Resource(error)
    }
}

impl From<RecoveryError> for ReplaceError {
    fn from(error: RecoveryError) -> Self {
        match error {
            RecoveryError::Open(error) => Self::Resource(error),
            RecoveryError::CaptureNotFound | RecoveryError::Unavailable { .. } => {
                Self::Resource(OpenError::Access(AccessError::ContextUnavailable))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ReplaceWindowsArguments {
    request: product::sessions::ReplaceWindowsRequest,
}

impl HostArguments for ReplaceWindowsArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        let request = serde_json::to_value(reader.command_arguments(2)?)
            .ok()
            .and_then(|value| serde_json::from_value(value).ok())
            .ok_or(DecodeError::Type("invalid window replacement"))?;
        Ok(Self { request })
    }
}

impl HostSuccess for product::sessions::ReplaceWindowsResult {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

#[host_operation("replaceWindows")]
async fn replace_windows(
    context: HostContext,
    arguments: ReplaceWindowsArguments,
) -> Result<product::sessions::ReplaceWindowsResult, ReplaceError> {
    let owner = context.recovery_owner()?;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let request = context.request;
    let caller = product::operations::OperationCaller {
        host: context.host.clone(),
        key: ExtensionKey::new(request.extension, request.lifecycle),
        invocation: request.invocation,
    };
    context.foreground(|_, cx| {
        product::sessions::replace_windows(
            arguments.request,
            owner,
            Some(caller),
            move |result| {
                let _ = sender.send(result);
            },
            cx,
        )
    })??;
    receiver.await.map_err(|_| AccessError::Cancelled)?
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenWindowArguments {
    pub roots: Vec<String>,
}

impl HostArguments for OpenWindowArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            roots: reader.text_list(2)?,
        })
    }
}

#[host_operation("openWindow")]
async fn open_window(
    context: HostContext,
    arguments: OpenWindowArguments,
) -> Result<crate::host::protocol::WindowInfo, WorkspaceError> {
    // The window's shell observes the host, so it opens after this host update.
    let (sender, receiver) = tokio::sync::oneshot::channel();
    context.foreground(|_, cx| {
        cx.defer(move |cx| {
            let _ = sender.send(product::sessions::open_window(cx));
        })
    })?;
    let target = receiver.await.map_err(|_| AccessError::Cancelled)?;
    if !arguments.roots.is_empty() {
        let replaced = replace_workspace_roots(
            context.clone(),
            WorkspaceArguments {
                roots: arguments.roots,
                target: Some(target.clone()),
            },
        )
        .await;
        if let Err(error) = replaced {
            let _ = context.foreground(|_, cx| product::sessions::discard_window(&target, cx));
            return Err(error);
        }
    }
    let window = context.foreground(|host, cx| {
        host.application_snapshot(cx)
            .windows
            .into_iter()
            .find(|window| window.target == target)
    })?;
    Ok(window.ok_or(AccessError::InvalidTarget)?)
}

impl HostSuccess for crate::app::window_layout::WindowLayout {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

#[host_operation("captureLayout")]
async fn capture_layout(
    context: HostContext,
    arguments: WindowLookup,
) -> Result<crate::app::window_layout::WindowLayout, AccessError> {
    context.foreground(|_, cx| product::sessions::capture_layout(&arguments.target, cx))?
}

#[host_operation("focusWindow")]
async fn focus_window(context: HostContext, arguments: WindowLookup) -> Result<(), AccessError> {
    context.foreground(|_, cx| product::sessions::focus_window(&arguments.target, cx))?
}

use crate::app::languages::LanguageDefinition;
use crate::app::syntax::{SyntaxLanguageError, SyntaxQuery};
use crate::host::protocol::LanguageInfo;

/// Failures of a language registration.
#[derive(Debug, HostError)]
pub(crate) enum LanguageError {
    #[error(transparent)]
    Access(AccessError),
    #[error(
        code = "UnknownGrammar",
        message = "no compiled-in grammar is named {grammar}"
    )]
    UnknownGrammar { grammar: String },
    #[error(
        code = "InvalidQuery",
        message = "invalid {query} query at {row}:{column}: {message}"
    )]
    InvalidQuery {
        query: String,
        message: String,
        row: usize,
        column: usize,
    },
    #[error(
        code = "LanguageRegistrationNotFound",
        message = "LanguageRegistrationNotFound"
    )]
    RegistrationNotFound,
}

impl From<AccessError> for LanguageError {
    fn from(error: AccessError) -> Self {
        Self::Access(error)
    }
}

impl LanguageError {
    fn from_syntax(error: SyntaxLanguageError, grammar: String) -> Self {
        match error {
            SyntaxLanguageError::UnknownGrammar => Self::UnknownGrammar { grammar },
            SyntaxLanguageError::InvalidQuery {
                query,
                message,
                row,
                column,
            } => Self::InvalidQuery {
                query: match query {
                    SyntaxQuery::Highlights => "highlights",
                    SyntaxQuery::Injections => "injections",
                }
                .into(),
                message,
                row,
                column,
            },
        }
    }
}

impl HostArguments for LanguageDefinition {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            info: LanguageInfo {
                id: reader.text(2, "language id must be text")?,
                name: reader.text(3, "language name must be text")?,
                grammar: reader.text(4, "language grammar must be text")?,
                file_extensions: reader.text_list(5)?,
                file_names: reader.text_list(6)?,
                interpreters: reader.text_list(7)?,
            },
            highlights: reader.text(8, "highlights query must be text")?,
            injections: reader.text(9, "injections query must be text")?,
        })
    }
}

impl HostSuccess for LanguageInfo {
    fn encode(self) -> Result<EncodedValue, DecodeError> {
        encode_json(self)
    }
}

#[host_operation("registerLanguage")]
async fn register_language(
    context: HostContext,
    arguments: LanguageDefinition,
) -> Result<u64, LanguageError> {
    let request = context.request;
    context.foreground(|host, cx| {
        let grammar = arguments.info.grammar.clone();
        crate::app::languages::register(host.language_owner(&request), arguments, cx)
            .map_err(|error| LanguageError::from_syntax(error, grammar))
    })?
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LanguageRegistrationArguments {
    registration: u64,
}

impl HostArguments for LanguageRegistrationArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            registration: reader.integer(2)?,
        })
    }
}

#[host_operation("unregisterLanguage")]
async fn unregister_language(
    context: HostContext,
    arguments: LanguageRegistrationArguments,
) -> Result<(), LanguageError> {
    let request = context.request;
    context.foreground(|host, cx| {
        if crate::app::languages::unregister(
            host.language_owner(&request),
            arguments.registration,
            cx,
        ) {
            Ok(())
        } else {
            Err(LanguageError::RegistrationNotFound)
        }
    })?
}

#[host_operation("listLanguages")]
async fn list_languages(context: HostContext, _: ()) -> Result<Vec<LanguageInfo>, AccessError> {
    context.foreground(|_, cx| crate::app::languages::list(cx))
}

#[host_operation("listGrammars")]
async fn list_grammars(context: HostContext, _: ()) -> Result<Vec<String>, AccessError> {
    context.foreground(|_, _| {
        crate::app::syntax::grammars::grammar_ids()
            .map(str::to_owned)
            .collect()
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DocumentLanguageArguments {
    id: String,
    language: Option<String>,
}

impl HostArguments for DocumentLanguageArguments {
    fn decode(reader: &mut dyn ArgumentReader) -> Result<Self, DecodeError> {
        Ok(Self {
            id: reader.text(2, "document identity must be text")?,
            language: reader.optional_text(3, "language must be text or null")?,
        })
    }
}

#[host_operation("setDocumentLanguage")]
async fn set_document_language(
    context: HostContext,
    arguments: DocumentLanguageArguments,
) -> Result<(), OpenError> {
    context.foreground(|_, cx| {
        let documents = cx
            .try_global::<crate::app::documents::ApplicationDocuments>()
            .ok_or(OpenError::DocumentNotFound)?
            .0
            .clone();
        let (model, uri) = uuid::Uuid::parse_str(&arguments.id)
            .ok()
            .and_then(|identity| {
                let documents = documents.read(cx);
                let document = documents.document_by_identity(identity)?;
                Some((document.model().clone(), document.resource_uri().cloned()))
            })
            .ok_or(OpenError::DocumentNotFound)?;
        crate::app::languages::set_document_language(&model, uri.as_ref(), arguments.language, cx);
        crate::app::application_observation::changed(cx);
        Ok(())
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::protocol::RequestId;
    use gpui::TestAppContext;

    #[gpui::test]
    async fn immediate_handler_commits_and_rejects_a_disposed_context(cx: &mut TestAppContext) {
        let key = ExtensionKey::new(ExtensionId::new(702), ExtensionLifecycleId::new(1));
        let (host, context) = cx.update(|cx| {
            let commands = cx.new(crate::app::commands::dispatch::ProductCommandDispatcher::new);
            cx.set_global(ApplicationProductCommands(commands));
            let host = install(cx);
            let context = host.update(cx, |host, cx| {
                host.load(key, ExtensionConfig::default()).unwrap();
                HostContext::new(
                    crate::host::protocol::RequestContext {
                        extension: key.extension,
                        lifecycle: key.lifecycle,
                        id: RequestId::new(1),
                        invocation: None,
                    },
                    cx,
                )
            });
            (host, context)
        });
        write_clipboard(
            context.clone(),
            ClipboardArguments {
                text: "application".into(),
            },
        )
        .await
        .unwrap();
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "application"
            );
            host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        });
        let result = write_clipboard(
            context,
            ClipboardArguments {
                text: "late".into(),
            },
        )
        .await;
        assert!(matches!(result, Err(AccessError::Cancelled)));
        cx.update(|cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "application"
            )
        });
    }
    fn storage_context(
        root: &std::path::Path,
        cx: &mut TestAppContext,
    ) -> (Entity<ProductExtensionHost>, HostContext, ExtensionKey) {
        let key = ExtensionKey::new(ExtensionId::new(703), ExtensionLifecycleId::new(1));
        cx.update(|cx| {
            let commands = cx.new(crate::app::commands::dispatch::ProductCommandDispatcher::new);
            cx.set_global(ApplicationProductCommands(commands));
            let host = install(cx);
            let context = host.update(cx, |host, cx| {
                host.load(key, ExtensionConfig::default()).unwrap();
                host.set_test_storage_root(root.to_owned(), cx);
                host.storage_owners.insert(
                    key,
                    crate::app::extensions::storage::StorageOwner::personal_config(),
                );
                HostContext::new(
                    crate::host::protocol::RequestContext {
                        extension: key.extension,
                        lifecycle: key.lifecycle,
                        id: RequestId::new(1),
                        invocation: None,
                    },
                    cx,
                )
            });
            (host, context, key)
        })
    }

    #[gpui::test]
    async fn retained_captures_survive_unload_and_the_unloaded_lifecycle_loses_access(
        cx: &mut TestAppContext,
    ) {
        use crate::app::{
            documents::{ApplicationDocuments, DocumentCollection},
            model::BufferModel,
            recovery::{ApplicationRecovery, RecoveryService, RecoveryStore, SnapshotKind},
        };
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("recovery.sqlite3");
        let (host, context, key) = storage_context(root.path(), cx);
        let identity = cx.update(|cx| {
            let documents = cx.new(|_| DocumentCollection::new());
            cx.set_global(ApplicationDocuments(documents.clone()));
            cx.set_global(ApplicationRecovery(RecoveryService::new(
                RecoveryStore::open(path.clone(), cx.background_executor().clone()),
            )));
            let model = cx.new(|_| BufferModel::from_text("kept"));
            documents.update(cx, |documents, cx| {
                let document = documents.create_untitled("Untitled", model, cx);
                documents.get(document).unwrap().identity().to_string()
            })
        });
        let lookup = DocumentLookup { id: identity };
        let capture = recovery_retain(context.clone(), lookup.clone())
            .await
            .unwrap();
        assert!(matches!(
            recovery_retain(
                context.clone(),
                DocumentLookup {
                    id: "missing".into()
                }
            )
            .await,
            Err(RecoveryError::Open(OpenError::DocumentNotFound))
        ));

        host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        let cancelled = |result: Result<_, RecoveryError>| {
            matches!(
                result,
                Err(RecoveryError::Open(OpenError::Access(
                    AccessError::Cancelled
                )))
            )
        };
        assert!(cancelled(
            recovery_list(context.clone(), ()).await.map(|_| ())
        ));
        assert!(cancelled(
            recovery_retain(context.clone(), lookup).await.map(|_| ())
        ));
        assert!(cancelled(
            recovery_release(
                context,
                CaptureLookup {
                    id: capture.id.clone()
                }
            )
            .await
            .map(|_| ())
        ));

        // The capture is durable and owned by the canonical identity, not the lifecycle.
        let reopened = RecoveryStore::open(path, cx.background_executor.clone());
        let stored = reopened
            .snapshot(SnapshotKind::Retained, capture.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.owner.as_deref(), Some("personal-config"));
        assert_eq!(stored.text, "kept");
    }

    #[gpui::test]
    async fn unload_before_storage_starts_cancels_without_creating_a_database(
        cx: &mut TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let (host, context, key) = storage_context(root.path(), cx);
        let pending = context
            .records()
            .unwrap()
            .write("key".into(), serde_json::json!(1));
        host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        assert!(matches!(pending.await, Err(StorageError::Cancelled)));
        assert!(!root.path().join("personal-config.sqlite3").exists());
    }

    #[gpui::test]
    async fn command_cancellation_invalidates_queued_storage(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (host, mut context, key) = storage_context(root.path(), cx);
        let invocation = host.update(cx, |host, _| {
            let registration = host
                .commands
                .register_command(
                    &context.request,
                    "storage-test".into(),
                    "Storage test".into(),
                )
                .unwrap();
            host.commands
                .enqueue_extension_root(
                    crate::host::protocol::Command {
                        name: "storage-test".into(),
                        arguments: crate::host::protocol::CommandArgumentValue::Null,
                    },
                    CommandTarget {
                        registration,
                        extension: key.extension,
                        lifecycle: key.lifecycle,
                    },
                    None,
                )
                .id
        });
        context.request.invocation = Some(invocation);
        let pending = context
            .records()
            .unwrap()
            .write("key".into(), serde_json::json!(1));
        host.update(cx, |host, cx| {
            host.commands.cancel(invocation);
            host.drain_command_events(cx);
        });
        assert!(matches!(pending.await, Err(StorageError::Cancelled)));
        assert!(!root.path().join("personal-config.sqlite3").exists());
    }

    #[gpui::test]
    fn unload_during_storage_transaction_preserves_commit(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (host, context, key) = storage_context(root.path(), cx);
        host.update(cx, |host, _| {
            host.storage = Ok(crate::app::extensions::storage::ExtensionStorage::new(
                root.path().to_owned(),
                crate::app::extensions::storage::test_executor(),
            ));
        });
        let (started, ready) = std::sync::mpsc::channel();
        let (resume, gate) = std::sync::mpsc::channel();
        let pending = context.records().unwrap().write_with_gate(started, gate);
        ready
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        host.update(cx, |host, cx| host.unload(key, cx).unwrap());
        resume.send(()).unwrap();
        pollster::block_on(pending).unwrap();
        let drain = host.update(cx, |host, _| host.shutdown_storage().unwrap());
        pollster::block_on(drain);
        let reopened = crate::app::extensions::storage::ExtensionStorage::new(
            root.path().to_owned(),
            crate::app::extensions::storage::test_executor(),
        );
        assert_eq!(
            pollster::block_on(
                reopened
                    .for_owner(crate::app::extensions::storage::StorageOwner::personal_config())
                    .read("key".into())
            )
            .unwrap(),
            Some(serde_json::json!(42))
        );
        pollster::block_on(reopened.shutdown());
    }

    #[gpui::test]
    async fn shutdown_rejects_new_host_storage_requests(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (host, context, _) = storage_context(root.path(), cx);
        let pending = context
            .records()
            .unwrap()
            .write("key".into(), serde_json::json!(1));
        let drain = host.update(cx, |host, _| host.shutdown_storage().unwrap());
        assert!(matches!(
            storage_write(
                context,
                RecordWrite {
                    key: "late".into(),
                    value: serde_json::json!(2)
                }
            )
            .await,
            Err(RecordError::Access(AccessError::Cancelled))
        ));
        pending.await.unwrap();
        drain.await;
        assert!(host.update(cx, |host, _| host.shutdown_storage()).is_none());
    }

    #[gpui::test]
    fn product_quit_drains_storage_without_blocking_foreground(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let (host, context, _) = storage_context(root.path(), cx);
        host.update(cx, |host, _| {
            host.storage = Ok(crate::app::extensions::storage::ExtensionStorage::new(
                root.path().to_owned(),
                crate::app::extensions::storage::test_executor(),
            ));
        });
        let (started, ready) = std::sync::mpsc::channel();
        let (resume, gate) = std::sync::mpsc::channel();
        let records = context.records().unwrap();
        let pending = records.write_with_gate(started, gate);
        ready
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        cx.update(quit_after_storage);
        cx.update(quit_after_storage);
        cx.run_until_parked();
        cx.update(|cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                "foreground remains responsive".into(),
            ))
        });
        assert!(cx.read(|cx| host.read(cx).storage_stopping));
        assert!(matches!(
            pollster::block_on(records.write("late".into(), serde_json::json!(1))),
            Err(StorageError::Cancelled)
        ));
        resume.send(()).unwrap();
        pollster::block_on(pending).unwrap();
        cx.run_until_parked();
    }
}
