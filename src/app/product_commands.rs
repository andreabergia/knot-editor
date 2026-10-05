//! Application-owned command discovery and dispatch for product windows.

use gpui::*;

use super::model::{CommandCatalog, CommandDefinition, SharedCommandCatalog};
use super::product::ProductShell;
use super::{CommandCompletion, CommandExecution};
use crate::host::protocol::{Command, CommandArgumentValue, CommandOutcome, ViewId};

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
pub(crate) const EDITOR_MOVE_PREFIX: &str = "editor.move-";
pub(crate) const EDITOR_SELECT_PREFIX: &str = "editor.select-";

pub(crate) const DELETE_BACKWARD_COMMAND: &str = "editor.delete-backward";
pub(crate) const DELETE_FORWARD_COMMAND: &str = "editor.delete-forward";
pub(crate) const INSERT_NEWLINE_COMMAND: &str = "editor.insert-newline";
pub(crate) const INSERT_TAB_COMMAND: &str = "editor.insert-tab";
pub(crate) const MOVE_DOCUMENT_END_COMMAND: &str = "editor.move-document-end";
pub(crate) const MOVE_DOCUMENT_START_COMMAND: &str = "editor.move-document-start";
pub(crate) const MOVE_DOWN_COMMAND: &str = "editor.move-down";
pub(crate) const MOVE_LEFT_COMMAND: &str = "editor.move-left";
pub(crate) const MOVE_LINE_END_COMMAND: &str = "editor.move-line-end";
pub(crate) const MOVE_LINE_START_COMMAND: &str = "editor.move-line-start";
pub(crate) const MOVE_PAGE_DOWN_COMMAND: &str = "editor.move-page-down";
pub(crate) const MOVE_PAGE_UP_COMMAND: &str = "editor.move-page-up";
pub(crate) const MOVE_RIGHT_COMMAND: &str = "editor.move-right";
pub(crate) const MOVE_UP_COMMAND: &str = "editor.move-up";
pub(crate) const MOVE_WORD_LEFT_COMMAND: &str = "editor.move-word-left";
pub(crate) const MOVE_WORD_RIGHT_COMMAND: &str = "editor.move-word-right";
pub(crate) const SELECT_DOCUMENT_END_COMMAND: &str = "editor.select-document-end";
pub(crate) const SELECT_DOCUMENT_START_COMMAND: &str = "editor.select-document-start";
pub(crate) const SELECT_DOWN_COMMAND: &str = "editor.select-down";
pub(crate) const SELECT_LEFT_COMMAND: &str = "editor.select-left";
pub(crate) const SELECT_LINE_END_COMMAND: &str = "editor.select-line-end";
pub(crate) const SELECT_LINE_START_COMMAND: &str = "editor.select-line-start";
pub(crate) const SELECT_PAGE_DOWN_COMMAND: &str = "editor.select-page-down";
pub(crate) const SELECT_PAGE_UP_COMMAND: &str = "editor.select-page-up";
pub(crate) const SELECT_RIGHT_COMMAND: &str = "editor.select-right";
pub(crate) const SELECT_UP_COMMAND: &str = "editor.select-up";
pub(crate) const SELECT_WORD_LEFT_COMMAND: &str = "editor.select-word-left";
pub(crate) const SELECT_WORD_RIGHT_COMMAND: &str = "editor.select-word-right";

const PRODUCT_COMMANDS: &[(&str, &str)] = &[
    (MOVE_LEFT_COMMAND, "Move left"),
    (MOVE_RIGHT_COMMAND, "Move right"),
    (MOVE_UP_COMMAND, "Move up"),
    (MOVE_DOWN_COMMAND, "Move down"),
    (MOVE_WORD_LEFT_COMMAND, "Move to previous word"),
    (MOVE_WORD_RIGHT_COMMAND, "Move to next word"),
    (MOVE_LINE_START_COMMAND, "Move to line start"),
    (MOVE_LINE_END_COMMAND, "Move to line end"),
    (MOVE_PAGE_UP_COMMAND, "Move up one page"),
    (MOVE_PAGE_DOWN_COMMAND, "Move down one page"),
    (MOVE_DOCUMENT_START_COMMAND, "Move to document start"),
    (MOVE_DOCUMENT_END_COMMAND, "Move to document end"),
    (SELECT_LEFT_COMMAND, "Select left"),
    (SELECT_RIGHT_COMMAND, "Select right"),
    (SELECT_UP_COMMAND, "Select up"),
    (SELECT_DOWN_COMMAND, "Select down"),
    (SELECT_WORD_LEFT_COMMAND, "Select to previous word"),
    (SELECT_WORD_RIGHT_COMMAND, "Select to next word"),
    (SELECT_LINE_START_COMMAND, "Select to line start"),
    (SELECT_LINE_END_COMMAND, "Select to line end"),
    (SELECT_PAGE_UP_COMMAND, "Select up one page"),
    (SELECT_PAGE_DOWN_COMMAND, "Select down one page"),
    (SELECT_DOCUMENT_START_COMMAND, "Select to document start"),
    (SELECT_DOCUMENT_END_COMMAND, "Select to document end"),
    (INSERT_NEWLINE_COMMAND, "Insert Newline"),
    (INSERT_TAB_COMMAND, "Insert Tab"),
    (DELETE_BACKWARD_COMMAND, "Delete Backward"),
    (DELETE_FORWARD_COMMAND, "Delete Forward"),
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
    pub(crate) view: Option<ViewId>,
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
        let Some(view) = self.view else { return Ok(()) };
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

pub(crate) fn validate_native_arguments(command: &Command) -> Result<(), CommandClaim> {
    if command.arguments == CommandArgumentValue::Null {
        Ok(())
    } else {
        Err(CommandClaim::Finished(CommandOutcome::InvalidArgument {
            message: "product commands do not accept arguments".into(),
        }))
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
            ("left", MOVE_LEFT_COMMAND),
            ("shift-left", SELECT_LEFT_COMMAND),
            ("right", MOVE_RIGHT_COMMAND),
            ("shift-right", SELECT_RIGHT_COMMAND),
            ("up", MOVE_UP_COMMAND),
            ("shift-up", SELECT_UP_COMMAND),
            ("down", MOVE_DOWN_COMMAND),
            ("shift-down", SELECT_DOWN_COMMAND),
            ("alt-left", MOVE_WORD_LEFT_COMMAND),
            ("shift-alt-left", SELECT_WORD_LEFT_COMMAND),
            ("alt-right", MOVE_WORD_RIGHT_COMMAND),
            ("shift-alt-right", SELECT_WORD_RIGHT_COMMAND),
            ("cmd-left", MOVE_LINE_START_COMMAND),
            ("shift-cmd-left", SELECT_LINE_START_COMMAND),
            ("cmd-right", MOVE_LINE_END_COMMAND),
            ("shift-cmd-right", SELECT_LINE_END_COMMAND),
            ("home", MOVE_LINE_START_COMMAND),
            ("shift-home", SELECT_LINE_START_COMMAND),
            ("end", MOVE_LINE_END_COMMAND),
            ("shift-end", SELECT_LINE_END_COMMAND),
            ("pageup", MOVE_PAGE_UP_COMMAND),
            ("shift-pageup", SELECT_PAGE_UP_COMMAND),
            ("pagedown", MOVE_PAGE_DOWN_COMMAND),
            ("shift-pagedown", SELECT_PAGE_DOWN_COMMAND),
            ("cmd-up", MOVE_DOCUMENT_START_COMMAND),
            ("shift-cmd-up", SELECT_DOCUMENT_START_COMMAND),
            ("cmd-down", MOVE_DOCUMENT_END_COMMAND),
            ("shift-cmd-down", SELECT_DOCUMENT_END_COMMAND),
            ("enter", INSERT_NEWLINE_COMMAND),
            ("tab", INSERT_TAB_COMMAND),
            ("backspace", DELETE_BACKWARD_COMMAND),
            ("delete", DELETE_FORWARD_COMMAND),
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
