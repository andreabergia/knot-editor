//! Application-owned command discovery and dispatch for product windows.

use gpui::*;

use super::model::{CommandCatalog, CommandDefinition, SharedCommandCatalog};
use super::product::ProductShell;
use super::workbench::CommandView;
use super::{CommandCompletion, CommandExecution};
use crate::host::protocol::{Command, CommandArgumentValue, CommandOutcome, ViewId};

pub(crate) use super::editor::native_commands::*;
pub(crate) use super::product::commands::*;
pub(crate) const EDITOR_MOVE_PREFIX: &str = "editor.move-";
pub(crate) const EDITOR_SELECT_PREFIX: &str = "editor.select-";

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

#[derive(Clone, PartialEq)]
pub(crate) struct ProductCommandTarget {
    pub(crate) window: AnyWindowHandle,
    pub(crate) shell: WeakEntity<ProductShell>,
    pub(crate) workbench: WeakEntity<super::workbench::Workbench>,
    pub(crate) pane: super::workbench::PaneId,
    pub(crate) tab: super::workbench::TabId,
    pub(crate) surface: super::workbench::TabSurfaceId,
    pub(crate) presentation: Option<EntityId>,
    pub(crate) focus: WeakFocusHandle,
    pub(crate) view: Option<ViewId>,
}

impl ProductCommandTarget {
    /// Validate the owning window/workbench and exact captured presentation.
    pub(crate) fn validate_tab(&self, cx: &App) -> Result<(), CommandOutcome> {
        let workbench = self
            .workbench
            .upgrade()
            .ok_or(CommandOutcome::InvalidTarget)?;
        if super::product::workbench_is_live(self.window, &workbench, cx)
            && workbench
                .read(cx)
                .capture_tab_move(self.pane, self.tab)
                .is_some_and(|capture| {
                    capture.tab.surface == self.surface && capture.presentation == self.presentation
                })
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
            super::workbench::TabSurfaceId::Terminal(_)
            | super::workbench::TabSurfaceId::Tool(_) => Err(CommandOutcome::Unavailable),
        }
    }

    pub(crate) fn command_view(&self, cx: &App) -> Option<super::workbench::CommandView> {
        self.validate_tab(cx).ok()?;
        if let Some(view) = self.view {
            return self
                .shell
                .upgrade()?
                .read(cx)
                .extension_command_view(view, cx)
                .map(super::workbench::CommandView::Extension);
        }
        self.workbench
            .upgrade()?
            .read(cx)
            .pane(self.pane)?
            .tabs()
            .iter()
            .find(|tab| tab.id() == self.tab && tab.surface_id() == self.surface)?
            .command_view()
    }

    pub(crate) fn validate_view(&self, cx: &App) -> Result<(), CommandOutcome> {
        let Some(view) = self.view else {
            if !cx.windows().contains(&self.window)
                || self.shell.upgrade().is_none()
                || self
                    .command_view(cx)
                    .is_none_or(|view| !view.matches_focus(&self.focus, cx))
            {
                return Err(CommandOutcome::InvalidTarget);
            }
            return Ok(());
        };
        if !cx.windows().contains(&self.window) {
            return Err(CommandOutcome::InvalidTarget);
        }
        let shell = self.shell.upgrade().ok_or(CommandOutcome::InvalidTarget)?;
        let tree = shell
            .read(cx)
            .extension_command_view(view, cx)
            .ok_or(CommandOutcome::InvalidTarget)?;
        if self.focus.upgrade() == Some(tree.focus_handle(cx)) {
            Ok(())
        } else {
            Err(CommandOutcome::InvalidTarget)
        }
    }
}

pub(crate) struct ApplicationProductCommands(pub(crate) Entity<ProductCommandDispatcher>);

impl Global for ApplicationProductCommands {}

pub(crate) struct ProductCommandDispatcher {
    catalog: SharedCommandCatalog,
    next_invocation: u64,
    last_outcome: Option<CommandOutcome>,
}

type PendingCommandStart = Box<dyn FnOnce(&mut App) -> Result<(), CommandOutcome>>;

pub(crate) enum CommandClaim {
    Declined,
    Finished(CommandOutcome),
    Pending(PendingCommandStart),
    Extension {
        handler: super::model::CommandTarget,
        view: Option<ViewId>,
    },
}

impl ProductCommandDispatcher {
    pub(crate) fn new(_cx: &mut Context<Self>) -> Self {
        let mut catalog = CommandCatalog::new();
        for (name, title) in super::command_declarations::registry().definitions() {
            catalog
                .register_native(name.into(), title.into())
                .expect("product command names are unique");
        }

        Self {
            catalog: std::rc::Rc::new(std::cell::RefCell::new(catalog)),
            next_invocation: 1,
            last_outcome: None,
        }
    }

    pub(crate) fn definitions(&self) -> impl Iterator<Item = CommandDefinition> {
        self.catalog
            .borrow()
            .definitions()
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub(crate) fn catalog(&self) -> SharedCommandCatalog {
        self.catalog.clone()
    }

    pub(crate) fn dispatch(
        &mut self,
        command: Command,
        target: ProductCommandTarget,
        cx: &mut Context<Self>,
    ) -> CommandExecution {
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

        let admitted = self.catalog.borrow().resolve(command.name.as_ref()).is_ok();
        let dispatcher = cx.entity();
        // Release any window on the update stack before reentering it in this turn's effect cycle.
        cx.defer(move |cx| {
            let claim = if !admitted {
                CommandClaim::Declined
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
                CommandClaim::Extension { handler, view } => {
                    let result = if view.is_some() {
                        super::extension_host::start_view_command(
                            command.clone(),
                            target.clone(),
                            handler,
                            completion.clone(),
                            cx,
                        )
                    } else {
                        super::extension_host::start_global_command(
                            command.clone(),
                            target.clone(),
                            handler,
                            completion.clone(),
                            cx,
                        )
                    };
                    match result {
                        Ok(()) => return,
                        Err(outcome) => outcome,
                    }
                }
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
    pub(crate) fn last_outcome(&self) -> Option<&CommandOutcome> {
        self.last_outcome.as_ref()
    }

    pub(crate) fn record_outcome(&mut self, outcome: CommandOutcome, cx: &mut Context<Self>) {
        self.last_outcome = Some(outcome);
        cx.notify();
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
        target.validate_tab(cx)?;
        break_captured_history_group(&target, cx);
        shell.update(cx, |shell, cx| {
            shell.start_open_dialog(target, completion, window, cx);
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

pub(crate) fn dispatch_to_captured_target(
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
        if let Err(outcome) = target.validate_view(cx) {
            return CommandClaim::Finished(outcome);
        }
        if let Some(view) = target.command_view(cx)
            && view.matches_focus(&target.focus, cx)
        {
            let result = match view {
                CommandView::Workspace(_) => super::command_declarations::registry().dispatch(
                    super::command_declarations::Participant::WorkspaceTree,
                    command,
                    &target,
                    completion.clone(),
                    cx,
                ),
                CommandView::Extension(view) => {
                    view.update(cx, |view, cx| view.handle_command(command, cx))
                }
                CommandView::Editor(_) => super::command_declarations::registry().dispatch(
                    super::command_declarations::Participant::Editor,
                    command,
                    &target,
                    completion.clone(),
                    cx,
                ),
                CommandView::Terminal(_) => super::command_declarations::registry().dispatch(
                    super::command_declarations::Participant::Terminal,
                    command,
                    &target,
                    completion.clone(),
                    cx,
                ),
            };
            if !matches!(result, CommandClaim::Declined) {
                return result;
            }
        }
        let result = super::command_declarations::registry().dispatch(
            super::command_declarations::Participant::Workbench,
            command,
            &target,
            completion.clone(),
            cx,
        );
        if !matches!(result, CommandClaim::Declined) {
            return result;
        }
        let result = super::command_declarations::registry().dispatch(
            super::command_declarations::Participant::Application,
            command,
            &target,
            completion,
            cx,
        );
        if !matches!(result, CommandClaim::Declined) {
            return result;
        }
        let catalog = cx
            .global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog();
        let handler = match catalog.borrow().resolve(command.name.as_ref()) {
            Ok(super::model::CommandTargetKind::Extension(handler)) => handler,
            _ => return CommandClaim::Declined,
        };
        CommandClaim::Extension {
            handler,
            view: None,
        }
    })
    .unwrap_or(CommandClaim::Finished(CommandOutcome::InvalidTarget))
}

#[cfg(test)]
pub(crate) fn product_command_names() -> impl Iterator<Item = &'static str> {
    super::command_declarations::registry()
        .definitions()
        .map(|(name, _)| name)
}
