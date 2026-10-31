//! Destination admission and semantic product operations for extension requests.

use super::super::extensions::host::handlers::{
    AccessError, CloseError, OpenError, WorkspaceError,
};
use super::*;
use crate::host::{
    lifecycle::ExtensionKey,
    protocol::{
        CommandInvocationId, DocumentDestination, PaneTarget, PresentationTarget, WindowTarget,
    },
};

#[derive(Clone)]
pub(crate) struct OperationCaller {
    pub(crate) host: WeakEntity<super::super::extensions::host::ProductExtensionHost>,
    pub(crate) key: ExtensionKey,
    pub(crate) invocation: Option<CommandInvocationId>,
}

impl OperationCaller {
    pub(super) fn is_live(&self, cx: &App) -> bool {
        self.host
            .upgrade()
            .is_some_and(|host| host.read(cx).operation_is_live(self.key, self.invocation))
    }
}

#[derive(Default)]
pub(super) struct OpenOptions {
    pub(super) replace_target: bool,
    /// Present a newly shown document as the pane's preview tab.
    pub(super) preview: bool,
    pub(super) kind: Option<ResourceKind>,
    pub(super) caller: Option<OperationCaller>,
}

fn window_target(destination: &DocumentDestination) -> &WindowTarget {
    match destination {
        DocumentDestination::Window(window) => window,
        DocumentDestination::Pane(pane) => &pane.window,
        DocumentDestination::View(view) => &view.pane.window,
    }
}

pub(super) fn capture_window(
    target: &WindowTarget,
    cx: &mut App,
) -> Result<(Entity<ProductShell>, AnyWindowHandle), AccessError> {
    let window = cx
        .windows()
        .into_iter()
        .find(|window| {
            window.downcast::<ProductShell>().is_some()
                && window.window_id().as_u64().to_string() == target.window
        })
        .ok_or(AccessError::InvalidTarget)?;
    let shell = cx
        .update_window(window, |root, _, _| root.downcast::<ProductShell>())
        .map_err(|_| AccessError::InvalidTarget)?
        .map_err(|_| AccessError::InvalidTarget)?;
    Ok((shell, window))
}

pub(super) fn workspace_info(
    window: WindowTarget,
    snapshot: &super::super::workspace::WorkspaceSnapshot,
) -> crate::host::protocol::WorkspaceInfo {
    use crate::host::protocol::{WorkspaceInfo, WorkspaceRootInfo};
    WorkspaceInfo {
        window,
        generation: snapshot.generation(),
        roots: snapshot
            .roots()
            .iter()
            .map(|root| WorkspaceRootInfo {
                id: root.id.to_string(),
                uri: root.uri.to_string(),
            })
            .collect(),
    }
}

fn capture_destination(
    destination: &DocumentDestination,
    cx: &mut App,
) -> Result<(Entity<ProductShell>, OpenTarget), AccessError> {
    let (shell, window) = capture_window(window_target(destination), cx)?;
    cx.update_window(window, |_, _, cx| {
        let state = shell.read(cx);
        let workbench = state.workbench.read(cx);
        let pane = match destination {
            DocumentDestination::Window(_) => workbench.focused_pane(),
            DocumentDestination::Pane(target)
            | DocumentDestination::View(PresentationTarget { pane: target, .. }) => {
                if target.workbench != state.workbench.entity_id().as_u64().to_string() {
                    return Err(AccessError::InvalidTarget);
                }
                workbench
                    .panes()
                    .iter()
                    .find(|pane| pane.id().value().to_string() == target.pane)
            }
        }
        .ok_or(AccessError::InvalidTarget)?;
        let tab = match destination {
            DocumentDestination::View(target) => pane
                .tabs()
                .iter()
                .find(|tab| tab.id().value().to_string() == target.tab),
            _ => Some(pane.active_tab()),
        }
        .ok_or(AccessError::InvalidTarget)?;
        let presentation = tab.presentation();
        if let DocumentDestination::View(target) = destination
            && presentation
                .map(|id| id.as_u64().to_string())
                .unwrap_or_default()
                != target.presentation
        {
            return Err(AccessError::InvalidTarget);
        }
        let target = OpenTarget {
            window,
            workbench: state.workbench.downgrade(),
            pane: pane.id(),
            tab: tab.id(),
            surface: tab.surface_id(),
            presentation,
        };
        Ok((shell, target))
    })
    .map_err(|_| AccessError::InvalidTarget)?
}

impl OpenTarget {
    fn public_target(&self) -> PresentationTarget {
        PresentationTarget {
            pane: PaneTarget {
                window: WindowTarget {
                    window: self.window.window_id().as_u64().to_string(),
                },
                workbench: self.workbench.entity_id().as_u64().to_string(),
                pane: self.pane.value().to_string(),
            },
            tab: self.tab.value().to_string(),
            presentation: self
                .presentation
                .map(|id| id.as_u64().to_string())
                .unwrap_or_default(),
        }
    }
}

impl OpenFailure {
    fn host_error(self) -> OpenError {
        match self {
            Self::InvalidTarget => OpenError::Access(AccessError::InvalidTarget),
            Self::Cancelled => OpenError::Access(AccessError::Cancelled),
            Self::Resource(error) => resource_error(error),
        }
    }
}

pub(crate) fn resource_error(error: super::super::open::OpenResourceError) -> OpenError {
    use super::super::open::OpenResourceError;
    match error {
        OpenResourceError::InvalidUtf8 { .. } => OpenError::InvalidUtf8,
        OpenResourceError::Provider(error) => match error {
            ResourceError::InvalidUri { .. } => OpenError::InvalidUri,
            ResourceError::NotFound { .. } => OpenError::ResourceNotFound,
            ResourceError::WrongKind { .. } => OpenError::ResourceWrongKind,
            _ => OpenError::ProviderFailure,
        },
    }
}

/// Resolve a user-selected path to a normalized, existing directory URI.
pub(crate) fn resolve_directory(
    path: std::path::PathBuf,
    completion: impl FnOnce(Result<super::super::resource::ResourceUri, WorkspaceError>) + 'static,
    cx: &mut App,
) {
    let providers = ProductShell::filesystems(cx);
    cx.spawn(async move |_| {
        let result = async {
            let request = OpenRequest::from_path(&path, Path::new("/"))
                .map_err(|_| WorkspaceError::Resource(OpenError::InvalidUri))?;
            match super::super::open::load_resource_kind(
                providers,
                request.uri().clone(),
                Some(ResourceKind::Directory),
            )
            .await
            {
                Ok(OpenResource::Directory(uri)) => Ok(uri),
                Ok(_) => Err(WorkspaceError::Resource(OpenError::ResourceWrongKind)),
                Err(error) => Err(WorkspaceError::Resource(
                    OpenFailure::Resource(error).host_error(),
                )),
            }
        }
        .await;
        completion(result);
    })
    .detach();
}

/// Resolves an extension-supplied terminal directory: an existing local
/// directory, checked through the registered providers.
pub(crate) fn resolve_terminal_directory(
    uri: String,
    completion: impl FnOnce(Result<std::path::PathBuf, OpenError>) + 'static,
    cx: &mut App,
) -> Result<(), OpenError> {
    let uri =
        super::super::resource::ResourceUri::parse(&uri).map_err(|_| OpenError::InvalidUri)?;
    if uri.scheme() != "file" {
        return Err(OpenError::InvalidUri);
    }
    let providers = ProductShell::filesystems(cx);
    cx.spawn(async move |_| {
        let result = match super::super::open::load_resource_kind(
            providers,
            uri,
            Some(ResourceKind::Directory),
        )
        .await
        {
            Ok(OpenResource::Directory(uri)) => uri
                .as_url()
                .to_file_path()
                .map_err(|()| OpenError::InvalidUri),
            Ok(_) => Err(OpenError::ResourceWrongKind),
            Err(error) => Err(OpenFailure::Resource(error).host_error()),
        };
        completion(result);
    })
    .detach();
    Ok(())
}

/// Opens a focused terminal at `destination`, starting in `directory` or, when
/// it is omitted, in the window's default terminal directory.
pub(crate) fn open_terminal(
    destination: DocumentDestination,
    directory: Option<std::path::PathBuf>,
    cx: &mut App,
) -> Result<PresentationTarget, AccessError> {
    let (shell, target) = capture_destination(&destination, cx)?;
    cx.update_window(target.window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            let directory = directory.or_else(|| shell.terminal_directory(cx));
            shell
                .new_terminal_in_pane(target.pane, directory, window, cx)
                .map(|target| target.public_target())
                .ok_or(AccessError::InvalidTarget)
        })
    })
    .map_err(|_| AccessError::InvalidTarget)?
}

pub(crate) fn validate_presentation(
    target: &PresentationTarget,
    cx: &mut App,
) -> Result<(), AccessError> {
    capture_destination(&DocumentDestination::View(target.clone()), cx).map(|_| ())
}

pub(crate) fn open_document(
    uri: String,
    destination: DocumentDestination,
    preview: bool,
    caller: OperationCaller,
    completion: impl FnOnce(Result<(DocumentId, PresentationTarget), OpenError>, &mut App) + 'static,
    cx: &mut App,
) -> Result<(), OpenError> {
    let uri =
        super::super::resource::ResourceUri::parse(&uri).map_err(|_| OpenError::InvalidUri)?;
    let (shell, target) = capture_destination(&destination, cx)?;
    shell.update(cx, |shell, cx| {
        shell.start_resource_operation(
            OpenRequest::from_uri_for_product(uri),
            target,
            OpenOptions {
                kind: Some(ResourceKind::File),
                preview,
                caller: Some(caller),
                ..Default::default()
            },
            move |result, cx| {
                completion(
                    result
                        .map_err(OpenFailure::host_error)
                        .and_then(|commit| match commit {
                            OpenCommit::Document { document, target } => {
                                Ok((document, target.public_target()))
                            }
                            OpenCommit::Workspace(_) => Err(OpenError::ResourceWrongKind),
                        }),
                    cx,
                )
            },
            cx,
        )
    });
    Ok(())
}

pub(crate) fn create_untitled(
    destination: DocumentDestination,
    cx: &mut App,
) -> Result<(DocumentId, PresentationTarget), OpenError> {
    let (shell, target) = capture_destination(&destination, cx)?;
    cx.update_window(target.window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            let (document, target) = shell
                .new_document_in_pane(target.pane, window, cx)
                .ok_or(OpenError::Access(AccessError::InvalidTarget))?;
            Ok((document, target.public_target()))
        })
    })
    .map_err(|_| OpenError::Access(AccessError::InvalidTarget))?
}

pub(crate) fn present_document(
    id: String,
    destination: DocumentDestination,
    cx: &mut App,
) -> Result<(DocumentId, PresentationTarget), OpenError> {
    let (shell, target) = capture_destination(&destination, cx)?;
    let document = ProductShell::documents(cx)
        .read(cx)
        .documents()
        .find(|document| document.identity().to_string() == id)
        .map(|document| document.id())
        .ok_or(OpenError::DocumentNotFound)?;
    let target = shell.update(cx, |shell, cx| {
        shell.next_open_generation();
        let target = shell.present_open_document(document, &target, false, false, cx);
        let _ = cx.update_window(target.window, |_, window, cx| {
            shell.focus_active_surface(window, cx)
        });
        cx.notify();
        target
    });
    Ok((document, target.public_target()))
}

/// Restore one of `owner`'s retained recovery captures through the
/// application's registered providers.
pub(crate) fn restore_retained(
    owner: String,
    capture_key: String,
    cx: &mut App,
) -> Task<Result<super::super::recovery::RestoreOutcome, super::super::recovery::RestoreError>> {
    let providers = ProductShell::filesystems(cx);
    super::super::recovery::restore_retained(owner, capture_key, providers, cx)
}

pub(crate) fn replace_roots(
    roots: Vec<String>,
    window_target: WindowTarget,
    caller: OperationCaller,
    completion: impl FnOnce(Result<crate::host::protocol::WorkspaceInfo, WorkspaceError>) + 'static,
    cx: &mut App,
) -> Result<(), WorkspaceError> {
    let roots = roots
        .into_iter()
        .map(|uri| {
            super::super::resource::ResourceUri::parse(&uri)
                .map_err(|_| WorkspaceError::Resource(OpenError::InvalidUri))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (shell, window) = capture_window(&window_target, cx)?;
    let shell_id = shell.entity_id();
    shell.update(cx, |shell, cx| {
        let generation = shell.next_open_generation();
        let workspace = shell.workspace.snapshot();
        let providers = ProductShell::filesystems(cx);
        let task = cx.spawn(async move |this, cx| {
            let validated = async {
                let mut validated = Vec::new();
                for uri in roots {
                    match super::super::open::load_resource_kind(
                        providers.clone(),
                        uri,
                        Some(ResourceKind::Directory),
                    )
                    .await
                    {
                        Ok(OpenResource::Directory(uri)) => validated.push(uri),
                        Ok(_) => {
                            return Err(WorkspaceError::Resource(OpenError::ResourceWrongKind));
                        }
                        Err(error) => {
                            return Err(WorkspaceError::Resource(
                                OpenFailure::Resource(error).host_error(),
                            ));
                        }
                    }
                }
                Ok(validated)
            }
            .await;
            let mut completion = Some(completion);
            if this
                .update(cx, |shell, cx| {
                    let result = if !caller.is_live(cx) {
                        Err(WorkspaceError::Resource(OpenError::Access(
                            AccessError::Cancelled,
                        )))
                    } else if shell.open_generation != generation
                        || !shell.workspace.is_current(&workspace)
                        || !cx
                            .update_window(window, |root, _, _| root.entity_id() == shell_id)
                            .unwrap_or(false)
                    {
                        Err(WorkspaceError::Resource(OpenError::Access(
                            AccessError::InvalidTarget,
                        )))
                    } else {
                        validated.map(|roots| {
                            let snapshot = shell.replace_workspace_roots(roots, cx);
                            workspace_info(window_target, &snapshot)
                        })
                    };
                    completion.take().unwrap()(result);
                })
                .is_err()
            {
                completion.take().unwrap()(Err(WorkspaceError::Resource(OpenError::Access(
                    AccessError::InvalidTarget,
                ))));
            }
        });
        shell.tasks.push(task);
    });
    Ok(())
}

pub(crate) fn close_presentation(
    presentation: PresentationTarget,
    caller: OperationCaller,
    completion: impl FnOnce(Result<crate::host::protocol::PresentationCloseOutcome, CloseError>)
    + 'static,
    cx: &mut App,
) -> Result<(), CloseError> {
    let (shell, target) = capture_destination(&DocumentDestination::View(presentation), cx)?;
    let workbench = target
        .workbench
        .upgrade()
        .ok_or(CloseError::Access(AccessError::InvalidTarget))?;
    let focus = workbench
        .read(cx)
        .pane(target.pane)
        .and_then(|pane| pane.tabs().iter().find(|tab| tab.id() == target.tab))
        .and_then(|tab| tab.command_view().map(|view| view.focus_handle(cx)))
        .ok_or(CloseError::Access(AccessError::InvalidTarget))?;
    let origin = ProductCommandTarget {
        window: target.window,
        shell: shell.downgrade(),
        workbench: target.workbench,
        pane: target.pane,
        tab: target.tab,
        surface: target.surface,
        presentation: target.presentation,
        focus: focus.downgrade(),
        view: None,
    };
    super::super::commands::dispatch::break_captured_history_group(&origin, cx);
    ProductShell::begin_protected_operation(
        ProtectedCloseKind::Tab,
        origin,
        Some(caller),
        Box::new(move |result, _| {
            completion(result.map_err(|error| match error {
                ProtectedCloseFailure::InvalidTarget => {
                    CloseError::Access(AccessError::InvalidTarget)
                }
                ProtectedCloseFailure::InProgress => CloseError::OperationInProgress,
                ProtectedCloseFailure::Cancelled => CloseError::Access(AccessError::Cancelled),
                ProtectedCloseFailure::PersistenceFailed { .. } => CloseError::PersistenceFailed,
            }));
        }),
        cx,
    );
    Ok(())
}
