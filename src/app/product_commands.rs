//! Application-owned command discovery and dispatch for product windows.

use gpui::*;

use crate::host::protocol::{
    Command, CommandArgumentValue, CommandOutcome, CommandRegistrationId, ExtensionId,
    ExtensionLifecycleId,
};
#[cfg(test)]
use crate::host::protocol::{
    CommandInvokeDispatch, HostOperation, HostRequest, HostRequestError, HostResponse,
    HostResponseValue, RequestId,
};

use super::model::{CommandCatalog, CommandCatalogError, CommandDefinition, CommandTargetKind};
use super::product::ProductShell;
use super::{CommandCompletion, CommandExecution};

pub(crate) const NEW_COMMAND: &str = "file.new";
pub(crate) const NEW_TERMINAL_COMMAND: &str = "terminal.new";
pub(crate) const MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND: &str = "terminal.move-to-new-window";
pub(crate) const OPEN_COMMAND: &str = "file.open";
pub(crate) const SAVE_COMMAND: &str = "file.save";
pub(crate) const SAVE_AS_COMMAND: &str = "file.save-as";
pub(crate) const CLOSE_TAB_COMMAND: &str = "workbench.close-tab";
pub(crate) const CLOSE_WINDOW_COMMAND: &str = "window.close";
pub(crate) const NEW_WINDOW_COMMAND: &str = "window.new";
pub(crate) const QUIT_COMMAND: &str = "application.quit";
pub(crate) const SPLIT_HORIZONTAL_COMMAND: &str = "workbench.split-horizontal";
pub(crate) const SPLIT_VERTICAL_COMMAND: &str = "workbench.split-vertical";
pub(crate) const UNDO_COMMAND: &str = "editor.undo";
pub(crate) const REDO_COMMAND: &str = "editor.redo";
pub(crate) const CUT_COMMAND: &str = "editor.cut";
pub(crate) const COPY_COMMAND: &str = "copy";
pub(crate) const PASTE_COMMAND: &str = "editor.paste";
pub(crate) const SELECT_ALL_COMMAND: &str = "editor.select-all";
pub(crate) const FIND_COMMAND: &str = "editor.find";
pub(crate) const FIND_NEXT_COMMAND: &str = "editor.find-next";
pub(crate) const FIND_PREVIOUS_COMMAND: &str = "editor.find-previous";
pub(crate) const SHOW_COMPLETIONS_COMMAND: &str = "editor.show-completions";
pub(crate) const SHOW_EXTENSION_REPORT_COMMAND: &str = "extensions.show-startup-report";

const PRODUCT_COMMANDS: &[(&str, &str)] = &[
    ("editor.move-left", "Move left"),
    ("editor.move-right", "Move right"),
    ("editor.move-up", "Move up"),
    ("editor.move-down", "Move down"),
    ("editor.move-word-left", "Move to previous word"),
    ("editor.move-word-right", "Move to next word"),
    ("editor.move-line-start", "Move to line start"),
    ("editor.move-line-end", "Move to line end"),
    ("editor.move-page-up", "Move up one page"),
    ("editor.move-page-down", "Move down one page"),
    ("editor.move-document-start", "Move to document start"),
    ("editor.move-document-end", "Move to document end"),
    ("editor.select-left", "Select left"),
    ("editor.select-right", "Select right"),
    ("editor.select-up", "Select up"),
    ("editor.select-down", "Select down"),
    ("editor.select-word-left", "Select to previous word"),
    ("editor.select-word-right", "Select to next word"),
    ("editor.select-line-start", "Select to line start"),
    ("editor.select-line-end", "Select to line end"),
    ("editor.select-page-up", "Select up one page"),
    ("editor.select-page-down", "Select down one page"),
    ("editor.select-document-start", "Select to document start"),
    ("editor.select-document-end", "Select to document end"),
    ("editor.insert-newline", "Insert Newline"),
    ("editor.insert-tab", "Insert Tab"),
    ("editor.delete-backward", "Delete Backward"),
    ("editor.delete-forward", "Delete Forward"),
    (NEW_COMMAND, "New Document"),
    (NEW_TERMINAL_COMMAND, "New Terminal"),
    (
        MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND,
        "Move Terminal to New Window",
    ),
    (OPEN_COMMAND, "Open…"),
    (SAVE_COMMAND, "Save"),
    (SAVE_AS_COMMAND, "Save As…"),
    (CLOSE_TAB_COMMAND, "Close Tab"),
    (CLOSE_WINDOW_COMMAND, "Close Window"),
    (NEW_WINDOW_COMMAND, "New Window"),
    (QUIT_COMMAND, "Quit Knot"),
    (SPLIT_HORIZONTAL_COMMAND, "Split Right"),
    (SPLIT_VERTICAL_COMMAND, "Split Down"),
    (UNDO_COMMAND, "Undo"),
    (REDO_COMMAND, "Redo"),
    (CUT_COMMAND, "Cut"),
    (COPY_COMMAND, "Copy"),
    (PASTE_COMMAND, "Paste"),
    (SELECT_ALL_COMMAND, "Select All"),
    (FIND_COMMAND, "Find"),
    (FIND_NEXT_COMMAND, "Find Next"),
    (FIND_PREVIOUS_COMMAND, "Find Previous"),
    (SHOW_COMPLETIONS_COMMAND, "Show Completions"),
    (
        SHOW_EXTENSION_REPORT_COMMAND,
        "Show Extension Startup Report",
    ),
];

#[derive(Clone, PartialEq, Action)]
#[action(namespace = knot, no_json)]
pub(crate) struct ProductCommandSource {
    pub(crate) name: SharedString,
}

impl ProductCommandSource {
    pub(crate) fn new(name: &'static str) -> Self {
        Self { name: name.into() }
    }

    pub(crate) fn command(&self) -> Command {
        Command {
            name: self.name.as_ref().into(),
            arguments: CommandArgumentValue::Null,
        }
    }
}

#[derive(Clone, PartialEq, Action)]
#[action(namespace = knot, no_json)]
pub(crate) struct ShowProductCommandPalette;

#[derive(Clone, PartialEq)]
pub(crate) struct ProductCommandTarget {
    pub(crate) window: AnyWindowHandle,
    pub(crate) shell: WeakEntity<ProductShell>,
    pub(crate) workbench: WeakEntity<super::workbench::Workbench>,
    pub(crate) pane: super::workbench::PaneId,
    pub(crate) tab: super::workbench::TabId,
    pub(crate) surface: super::workbench::TabSurfaceId,
    pub(crate) focus: WeakFocusHandle,
}

impl ProductCommandTarget {
    /// Validate the captured tab and its surface before using its contents.
    pub(crate) fn validate_tab(&self, cx: &App) -> Result<(), CommandOutcome> {
        let workbench = self
            .workbench
            .upgrade()
            .ok_or(CommandOutcome::InvalidTarget)?;
        if workbench
            .read(cx)
            .contains_surface(self.pane, self.tab, self.surface)
        {
            Ok(())
        } else {
            Err(CommandOutcome::InvalidTarget)
        }
    }

    pub(crate) fn document_id(
        &self,
        cx: &App,
    ) -> Result<super::documents::DocumentId, CommandOutcome> {
        self.validate_tab(cx)?;
        match self.surface {
            super::workbench::TabSurfaceId::Document(document) => Ok(document),
            super::workbench::TabSurfaceId::Terminal(_) => Err(CommandOutcome::Unavailable),
        }
    }

    pub(crate) fn command_view(&self, cx: &App) -> Option<super::workbench::CommandView> {
        self.workbench
            .upgrade()?
            .read(cx)
            .pane(self.pane)?
            .tabs()
            .iter()
            .find(|tab| tab.id() == self.tab && tab.surface_id() == self.surface)?
            .command_view()
    }
}

pub(crate) struct ApplicationProductCommands(pub(crate) Entity<ProductCommandDispatcher>);

impl Global for ApplicationProductCommands {}

pub(crate) struct ProductCommandDispatcher {
    catalog: CommandCatalog,
    next_invocation: u64,
    last_outcome: Option<CommandOutcome>,
}

type PendingCommandStart = Box<dyn FnOnce(&mut App) -> Result<(), CommandOutcome>>;

pub(crate) enum CommandClaim {
    Declined,
    Finished(CommandOutcome),
    Pending(PendingCommandStart),
}

#[cfg(test)]
pub(crate) enum ProductCommandHostResponse {
    Ready(HostResponse),
    Awaiting {
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        request: RequestId,
        completion: tokio::sync::oneshot::Receiver<CommandOutcome>,
    },
}

#[cfg(test)]
impl ProductCommandHostResponse {
    pub(crate) async fn resolve(self) -> HostResponse {
        match self {
            Self::Ready(response) => response,
            Self::Awaiting {
                extension,
                lifecycle,
                request,
                completion,
            } => command_host_response(
                extension,
                lifecycle,
                request,
                Ok(CommandInvokeDispatch::Outcome {
                    outcome: completion.await.unwrap_or(CommandOutcome::Cancelled),
                }),
            ),
        }
    }
}

impl ProductCommandDispatcher {
    pub(crate) fn new(_cx: &mut Context<Self>) -> Self {
        let mut catalog = CommandCatalog::new();
        for &(name, title) in PRODUCT_COMMANDS {
            catalog
                .register_native(name.into(), title.into())
                .expect("product command names are unique");
        }

        Self {
            catalog,
            next_invocation: 1,
            last_outcome: None,
        }
    }

    pub(crate) fn definitions(&self) -> impl Iterator<Item = &CommandDefinition> {
        self.catalog.definitions()
    }

    pub(crate) fn register_extension(
        &mut self,
        name: crate::host::protocol::CommandName,
        title: String,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> Result<CommandRegistrationId, CommandCatalogError> {
        self.catalog
            .register_extension(name, title, extension, lifecycle)
    }

    pub(crate) fn unregister_extension(
        &mut self,
        registration: CommandRegistrationId,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> Result<(), CommandCatalogError> {
        self.catalog.unregister(registration, extension, lifecycle)
    }

    pub(crate) fn remove_extension_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) {
        self.catalog.remove_lifecycle(extension, lifecycle);
    }

    pub(crate) fn dispatch(
        &mut self,
        command: Command,
        target: ProductCommandTarget,
        cx: &mut Context<Self>,
    ) -> CommandExecution {
        if matches!(
            self.catalog.resolve(command.name.as_ref()),
            Ok(CommandTargetKind::Extension(_))
        ) {
            return super::extension_host::dispatch_product_command(command, target, cx);
        }
        let id = crate::host::protocol::CommandInvocationId::new(self.next_invocation);
        self.next_invocation = self
            .next_invocation
            .checked_add(1)
            .expect("product command invocation space exhausted");
        let (completion, receiver) = CommandCompletion::new();
        let execution = CommandExecution {
            id,
            completion: receiver,
        };

        let admitted = self.catalog.resolve(command.name.as_ref()) == Ok(CommandTargetKind::Native);
        let dispatcher = cx.entity();
        cx.defer(move |cx| {
            let claim = if !admitted {
                CommandClaim::Declined
            } else if command.arguments != CommandArgumentValue::Null {
                CommandClaim::Finished(CommandOutcome::InvalidArgument {
                    message: "product commands do not accept arguments".into(),
                })
            } else {
                dispatch_to_captured_target(&command, &target, completion.clone(), cx)
            };
            let outcome = match claim {
                CommandClaim::Declined => CommandOutcome::Unavailable,
                CommandClaim::Finished(outcome) => outcome,
                CommandClaim::Pending(start) => match start(cx) {
                    Ok(()) => return,
                    Err(outcome) => outcome,
                },
            };
            completion.complete(outcome.clone());
            dispatcher.update(cx, |dispatcher, cx| {
                dispatcher.last_outcome = Some(outcome);
                cx.notify();
            });
        });
        execution
    }

    #[cfg(test)]
    pub(crate) fn dispatch_host_request(
        &mut self,
        request: HostRequest,
        target: Option<ProductCommandTarget>,
        cx: &mut Context<Self>,
    ) -> ProductCommandHostResponse {
        let extension = request.extension;
        let lifecycle = request.lifecycle;
        let id = request.id;
        match request.operation {
            HostOperation::InvokeCommand { command } if request.invocation.is_none() => {
                let Some(target) = target else {
                    return ProductCommandHostResponse::Ready(command_host_response(
                        extension,
                        lifecycle,
                        id,
                        Ok(CommandInvokeDispatch::Outcome {
                            outcome: CommandOutcome::InvalidTarget,
                        }),
                    ));
                };
                let execution = self.dispatch(command, target, cx);
                ProductCommandHostResponse::Awaiting {
                    extension,
                    lifecycle,
                    request: id,
                    completion: execution.completion,
                }
            }
            HostOperation::InvokeCommand { .. } => {
                ProductCommandHostResponse::Ready(command_host_response(
                    extension,
                    lifecycle,
                    id,
                    Ok(CommandInvokeDispatch::Outcome {
                        outcome: CommandOutcome::Unavailable,
                    }),
                ))
            }
            _ => ProductCommandHostResponse::Ready(HostResponse {
                extension,
                lifecycle,
                id,
                result: Err(HostRequestError::UnsupportedOperation),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn last_outcome(&self) -> Option<&CommandOutcome> {
        self.last_outcome.as_ref()
    }

    pub(crate) fn record_outcome(&mut self, outcome: CommandOutcome, cx: &mut Context<Self>) {
        self.last_outcome = Some(outcome);
        cx.notify();
    }
}

#[cfg(test)]
fn command_host_response(
    extension: ExtensionId,
    lifecycle: ExtensionLifecycleId,
    id: RequestId,
    dispatch: Result<CommandInvokeDispatch, HostRequestError>,
) -> HostResponse {
    HostResponse {
        extension,
        lifecycle,
        id,
        result: dispatch.map(|dispatch| HostResponseValue::CommandInvoked { dispatch }),
    }
}

pub(crate) fn break_captured_history_group(target: &ProductCommandTarget, cx: &mut App) {
    let Some(workbench) = target.workbench.upgrade() else {
        return;
    };
    let model = workbench
        .read(cx)
        .pane(target.pane)
        .and_then(|pane| pane.tabs().iter().find(|tab| tab.id() == target.tab))
        .filter(|tab| tab.surface_id() == target.surface)
        .and_then(|tab| tab.editor())
        .map(|editor| editor.read(cx).model().clone());
    if let Some(model) = model {
        model.update(cx, |model, _| model.break_history_group());
    }
}

pub(crate) fn dispatch_open_to_captured_target(
    target: &ProductCommandTarget,
    completion: CommandCompletion,
    cx: &mut App,
) -> Result<(), CommandOutcome> {
    if target.shell.upgrade().is_none()
        || target.workbench.upgrade().is_none()
        || target.focus.upgrade().is_none()
        || !cx.windows().contains(&target.window)
    {
        return Err(CommandOutcome::InvalidTarget);
    }
    let shell = target.shell.clone();
    let target = target.clone();
    cx.update_window(target.window, move |_, window, cx| {
        let Some(shell) = shell.upgrade() else {
            return Err(CommandOutcome::InvalidTarget);
        };
        if window.root::<ProductShell>().flatten().as_ref() != Some(&shell) {
            return Err(CommandOutcome::InvalidTarget);
        }
        target.document_id(cx)?;
        break_captured_history_group(&target, cx);
        shell.update(cx, |shell, cx| {
            shell.start_open_dialog(target, completion, cx);
        });
        Ok(())
    })
    .unwrap_or(Err(CommandOutcome::InvalidTarget))
}

pub(crate) fn dispatch_save_to_captured_target(
    target: &ProductCommandTarget,
    save_as: bool,
    completion: CommandCompletion,
    cx: &mut App,
) -> Result<(), CommandOutcome> {
    if target.shell.upgrade().is_none()
        || target.workbench.upgrade().is_none()
        || target.focus.upgrade().is_none()
        || !cx.windows().contains(&target.window)
    {
        return Err(CommandOutcome::InvalidTarget);
    }
    let shell = target.shell.clone();
    let target = target.clone();
    cx.update_window(target.window, move |_, window, cx| {
        let Some(shell) = shell.upgrade() else {
            return Err(CommandOutcome::InvalidTarget);
        };
        if window.root::<ProductShell>().flatten().as_ref() != Some(&shell) {
            return Err(CommandOutcome::InvalidTarget);
        }
        target.document_id(cx)?;
        break_captured_history_group(&target, cx);
        shell.update(cx, |shell, cx| {
            shell.start_save_command(save_as, target, completion, cx);
        });
        Ok(())
    })
    .unwrap_or(Err(CommandOutcome::InvalidTarget))
}

pub(crate) fn dispatch_close_to_captured_target(
    target: &ProductCommandTarget,
    kind: super::product::ProtectedCloseKind,
    completion: CommandCompletion,
    cx: &mut App,
) -> Result<(), CommandOutcome> {
    if target.shell.upgrade().is_none()
        || target.workbench.upgrade().is_none()
        || target.focus.upgrade().is_none()
        || !cx.windows().contains(&target.window)
    {
        return Err(CommandOutcome::InvalidTarget);
    }
    let Some(shell) = target.shell.upgrade() else {
        return Err(CommandOutcome::InvalidTarget);
    };
    let target = target.clone();
    let valid = cx
        .update_window(target.window, |_, window, _| {
            window.root::<ProductShell>().flatten().as_ref() == Some(&shell)
        })
        .unwrap_or(false);
    if !valid {
        return Err(CommandOutcome::InvalidTarget);
    }
    target.validate_tab(cx)?;
    break_captured_history_group(&target, cx);
    ProductShell::begin_protected_close(kind, target, Some(completion), cx);
    Ok(())
}

fn dispatch_to_captured_target(
    command: &Command,
    target: &ProductCommandTarget,
    completion: CommandCompletion,
    cx: &mut App,
) -> CommandClaim {
    if target.shell.upgrade().is_none()
        || target.workbench.upgrade().is_none()
        || target.focus.upgrade().is_none()
        || !cx.windows().contains(&target.window)
    {
        return CommandClaim::Finished(CommandOutcome::InvalidTarget);
    }
    let shell = target.shell.clone();
    let target = target.clone();
    cx.update_window(target.window, move |_, window, cx| {
        let Some(shell) = shell.upgrade() else {
            return CommandClaim::Finished(CommandOutcome::InvalidTarget);
        };
        if window.root::<ProductShell>().flatten().as_ref() != Some(&shell) {
            return CommandClaim::Finished(CommandOutcome::InvalidTarget);
        }
        if let Err(outcome) = target.validate_tab(cx) {
            return CommandClaim::Finished(outcome);
        }
        if let Some(view) = target.command_view(cx)
            && view.matches_focus(&target.focus, cx)
        {
            let result = view.handle_command(command, window, cx);
            if !matches!(result, CommandClaim::Declined) {
                return result;
            }
        }
        let result = shell.update(cx, |shell, cx| {
            shell.handle_workbench_command(command, &target, completion.clone(), window, cx)
        });
        if !matches!(result, CommandClaim::Declined) {
            return result;
        }
        shell.update(cx, |shell, cx| {
            shell.handle_application_command(command, &target, completion, cx)
        })
    })
    .unwrap_or(CommandClaim::Finished(CommandOutcome::InvalidTarget))
}

#[cfg(test)]
pub(crate) fn product_command_names() -> impl Iterator<Item = &'static str> {
    PRODUCT_COMMANDS.iter().map(|(name, _)| *name)
}

/// Editing bindings share the captured product command path.
pub(crate) fn bind_editing_keys(cx: &mut App) {
    cx.bind_keys(
        [
            ("left", "editor.move-left"),
            ("shift-left", "editor.select-left"),
            ("right", "editor.move-right"),
            ("shift-right", "editor.select-right"),
            ("up", "editor.move-up"),
            ("shift-up", "editor.select-up"),
            ("down", "editor.move-down"),
            ("shift-down", "editor.select-down"),
            ("alt-left", "editor.move-word-left"),
            ("shift-alt-left", "editor.select-word-left"),
            ("alt-right", "editor.move-word-right"),
            ("shift-alt-right", "editor.select-word-right"),
            ("cmd-left", "editor.move-line-start"),
            ("shift-cmd-left", "editor.select-line-start"),
            ("cmd-right", "editor.move-line-end"),
            ("shift-cmd-right", "editor.select-line-end"),
            ("home", "editor.move-line-start"),
            ("shift-home", "editor.select-line-start"),
            ("end", "editor.move-line-end"),
            ("shift-end", "editor.select-line-end"),
            ("pageup", "editor.move-page-up"),
            ("shift-pageup", "editor.select-page-up"),
            ("pagedown", "editor.move-page-down"),
            ("shift-pagedown", "editor.select-page-down"),
            ("cmd-up", "editor.move-document-start"),
            ("shift-cmd-up", "editor.select-document-start"),
            ("cmd-down", "editor.move-document-end"),
            ("shift-cmd-down", "editor.select-document-end"),
            ("enter", "editor.insert-newline"),
            ("tab", "editor.insert-tab"),
            ("backspace", "editor.delete-backward"),
            ("delete", "editor.delete-forward"),
        ]
        .into_iter()
        .map(|(key, command)| {
            KeyBinding::new(
                key,
                ProductCommandSource::new(command),
                Some("product > editor"),
            )
        }),
    );
    cx.bind_keys([KeyBinding::new(
        "ctrl-space",
        ProductCommandSource::new(SHOW_COMPLETIONS_COMMAND),
        Some("product > editor"),
    )]);
}
