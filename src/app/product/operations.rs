//! Destination admission and semantic product operations for extension requests.

use super::*;
use crate::host::{
    lifecycle::ExtensionKey,
    protocol::{
        CommandInvocationId, DocumentDestination, HostRequestError, PaneTarget, PresentationTarget,
        WindowTarget,
    },
};

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

fn capture_destination(
    destination: &DocumentDestination,
    cx: &mut App,
) -> Result<(Entity<ProductShell>, OpenTarget), HostRequestError> {
    let window = cx
        .windows()
        .into_iter()
        .find(|window| {
            window.downcast::<ProductShell>().is_some()
                && window.window_id().as_u64().to_string() == window_target(destination).window
        })
        .ok_or(HostRequestError::InvalidTarget)?;
    cx.update_window(window, |root, _, cx| {
        let shell = root
            .downcast::<ProductShell>()
            .map_err(|_| HostRequestError::InvalidTarget)?;
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
