//! Destination admission and semantic product operations for extension requests.

use super::*;
use crate::host::{
    lifecycle::ExtensionKey,
    protocol::{
        CommandInvocationId, DocumentDestination, HostRequestError, PaneTarget, PresentationTarget,
        WindowTarget,
    },
};

#[derive(Clone)]
pub(crate) struct OperationCaller {
    pub(crate) host: WeakEntity<super::super::extension_host::ProductExtensionHost>,
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

fn capture_window(
    target: &WindowTarget,
    cx: &mut App,
) -> Result<(Entity<ProductShell>, AnyWindowHandle), HostRequestError> {
    let window = cx
        .windows()
        .into_iter()
        .find(|window| {
            window.downcast::<ProductShell>().is_some()
                && window.window_id().as_u64().to_string() == target.window
        })
        .ok_or(HostRequestError::InvalidTarget)?;
    let shell = cx
        .update_window(window, |root, _, _| root.downcast::<ProductShell>())
        .map_err(|_| HostRequestError::InvalidTarget)?
        .map_err(|_| HostRequestError::InvalidTarget)?;
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
) -> Result<(Entity<ProductShell>, OpenTarget), HostRequestError> {
    let (shell, window) = capture_window(window_target(destination), cx)?;
    cx.update_window(window, |_, _, cx| {
        let state = shell.read(cx);
        let workbench = state.workbench.read(cx);
        let pane = match destination {
            DocumentDestination::Window(_) => workbench.focused_pane(),
            DocumentDestination::Pane(target)
            | DocumentDestination::View(PresentationTarget { pane: target, .. }) => {
                if target.workbench != state.workbench.entity_id().as_u64().to_string() {
                    return Err(HostRequestError::InvalidTarget);
                }
                workbench
                    .panes()
                    .iter()
                    .find(|pane| pane.id().value().to_string() == target.pane)
            }
        }
        .ok_or(HostRequestError::InvalidTarget)?;
        let tab = match destination {
            DocumentDestination::View(target) => pane
                .tabs()
                .iter()
                .find(|tab| tab.id().value().to_string() == target.tab),
            _ => Some(pane.active_tab()),
        }
        .ok_or(HostRequestError::InvalidTarget)?;
        let presentation = tab
            .editor()
            .map(Entity::entity_id)
            .or_else(|| tab.terminal_view().map(Entity::entity_id));
        if let DocumentDestination::View(target) = destination
            && presentation
                .map(|id| id.as_u64().to_string())
                .unwrap_or_default()
                != target.presentation
        {
            return Err(HostRequestError::InvalidTarget);
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
    .map_err(|_| HostRequestError::InvalidTarget)?
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
    fn host_error(self) -> HostRequestError {
        use super::super::open::OpenResourceError;
        match self {
            Self::InvalidTarget => HostRequestError::InvalidTarget,
            Self::Cancelled => HostRequestError::Cancelled,
            Self::Resource(OpenResourceError::InvalidUtf8 { .. }) => HostRequestError::InvalidUtf8,
            Self::Resource(OpenResourceError::Provider(error)) => match error {
                ResourceError::InvalidUri { .. } => HostRequestError::InvalidUri,
                ResourceError::NotFound { .. } => HostRequestError::ResourceNotFound,
                ResourceError::WrongKind { .. } => HostRequestError::ResourceWrongKind,
                _ => HostRequestError::ProviderFailure,
            },
        }
    }
}

pub(crate) fn validate_presentation(
    target: &PresentationTarget,
    cx: &mut App,
) -> Result<(), HostRequestError> {
    capture_destination(&DocumentDestination::View(target.clone()), cx).map(|_| ())
}

pub(crate) fn open_document(
    uri: String,
    destination: DocumentDestination,
    caller: OperationCaller,
    completion: impl FnOnce(Result<(DocumentId, PresentationTarget), HostRequestError>, &mut App)
    + 'static,
    cx: &mut App,
) -> Result<(), HostRequestError> {
    let uri = super::super::resource::ResourceUri::parse(&uri)
        .map_err(|_| HostRequestError::InvalidUri)?;
    let (shell, target) = capture_destination(&destination, cx)?;
    shell.update(cx, |shell, cx| {
        shell.start_resource_operation(
            OpenRequest::from_uri_for_product(uri),
            target,
            OpenOptions {
                kind: Some(ResourceKind::File),
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
                            OpenCommit::Workspace(_) => Err(HostRequestError::ResourceWrongKind),
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
) -> Result<(DocumentId, PresentationTarget), HostRequestError> {
    let (shell, target) = capture_destination(&destination, cx)?;
    cx.update_window(target.window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            let (document, target) = shell
                .new_document_in_pane(target.pane, window, cx)
                .ok_or(HostRequestError::InvalidTarget)?;
            Ok((document, target.public_target()))
        })
    })
    .map_err(|_| HostRequestError::InvalidTarget)?
}

pub(crate) fn present_document(
    id: String,
    destination: DocumentDestination,
    cx: &mut App,
) -> Result<(DocumentId, PresentationTarget), HostRequestError> {
    let (shell, target) = capture_destination(&destination, cx)?;
    let document = ProductShell::documents(cx)
        .read(cx)
        .documents()
        .find(|document| document.identity().to_string() == id)
        .map(|document| document.id())
        .ok_or(HostRequestError::DocumentNotFound)?;
    let target = shell.update(cx, |shell, cx| {
        shell.next_open_generation();
        let target = shell.present_open_document(document, &target, false, cx);
        let _ = cx.update_window(target.window, |_, window, cx| {
            shell.focus_active_surface(window, cx)
        });
        cx.notify();
        target
    });
    Ok((document, target.public_target()))
}

pub(crate) fn replace_roots(
    roots: Vec<String>,
    window_target: WindowTarget,
    caller: OperationCaller,
    completion: impl FnOnce(Result<crate::host::protocol::WorkspaceInfo, HostRequestError>) + 'static,
    cx: &mut App,
) -> Result<(), HostRequestError> {
    if roots.len() > 1 {
        return Err(HostRequestError::TooManyWorkspaceRoots);
    }
    let roots = roots
        .into_iter()
        .map(|uri| {
            super::super::resource::ResourceUri::parse(&uri)
                .map_err(|_| HostRequestError::InvalidUri)
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
                        Ok(_) => return Err(HostRequestError::ResourceWrongKind),
                        Err(error) => return Err(OpenFailure::Resource(error).host_error()),
                    }
                }
                Ok(validated)
            }
            .await;
            let mut completion = Some(completion);
            if this
                .update(cx, |shell, cx| {
                    let result = if !caller.is_live(cx) {
                        Err(HostRequestError::Cancelled)
                    } else if shell.open_generation != generation
                        || !shell.workspace.is_current(&workspace)
                        || !cx
                            .update_window(window, |root, _, _| root.entity_id() == shell_id)
                            .unwrap_or(false)
                    {
                        Err(HostRequestError::InvalidTarget)
                    } else {
                        validated.and_then(|roots| {
                            shell
                                .replace_workspace_roots(roots, cx)
                                .map(|snapshot| workspace_info(window_target, &snapshot))
                                .map_err(|_| HostRequestError::TooManyWorkspaceRoots)
                        })
                    };
                    completion.take().unwrap()(result);
                })
                .is_err()
            {
                completion.take().unwrap()(Err(HostRequestError::InvalidTarget));
            }
        });
        shell.tasks.push(task);
    });
    Ok(())
}

pub(crate) fn close_presentation(
    presentation: PresentationTarget,
    caller: OperationCaller,
    completion: impl FnOnce(Result<crate::host::protocol::PresentationCloseOutcome, HostRequestError>)
    + 'static,
    cx: &mut App,
) -> Result<(), HostRequestError> {
    let (shell, target) = capture_destination(&DocumentDestination::View(presentation), cx)?;
    let workbench = target
        .workbench
        .upgrade()
        .ok_or(HostRequestError::InvalidTarget)?;
    let focus = workbench
        .read(cx)
        .pane(target.pane)
        .and_then(|pane| pane.tabs().iter().find(|tab| tab.id() == target.tab))
        .and_then(|tab| {
            tab.editor()
                .map(|editor| editor.focus_handle(cx))
                .or_else(|| tab.terminal_view().map(|view| view.focus_handle(cx)))
        })
        .ok_or(HostRequestError::InvalidTarget)?;
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
    super::super::product_commands::break_captured_history_group(&origin, cx);
    ProductShell::begin_protected_operation(
        ProtectedCloseKind::Tab,
        origin,
        Some(caller),
        Box::new(move |result, _| {
            completion(result.map_err(|error| match error {
                ProtectedCloseFailure::InvalidTarget => HostRequestError::InvalidTarget,
                ProtectedCloseFailure::InProgress => HostRequestError::OperationInProgress,
                ProtectedCloseFailure::Cancelled => HostRequestError::Cancelled,
                ProtectedCloseFailure::PersistenceFailed { .. } => {
                    HostRequestError::PersistenceFailed
                }
            }));
        }),
        cx,
    );
    Ok(())
}
