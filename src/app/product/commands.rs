//! Workbench and application command declarations.
use super::*;
use crate::app::command_declarations::{CommandContext, command};
use crate::host::protocol::CommandOutcome;

#[command("file.new", title = "New Document", id = NEW_COMMAND, participant = Workbench)]
pub(crate) async fn new(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    context
        .shell(move |shell, target, window, cx| {
            shell
                .new_document_in_pane(target.pane, window, cx)
                .map(|_| ())
                .ok_or(CommandOutcome::InvalidTarget)
        })
        .await
}

#[command("terminal.new", title = "New Terminal", id = NEW_TERMINAL_COMMAND, participant = Workbench)]
pub(crate) async fn new_terminal(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    context
        .shell(move |shell, target, window, cx| {
            if shell.new_terminal_in_pane(target.pane, window, cx) {
                Ok(())
            } else {
                Err(CommandOutcome::InvalidTarget)
            }
        })
        .await
}

#[command("terminal.move-to-new-window", title = "Move Terminal to New Window", id = MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND, participant = Workbench)]
pub(crate) async fn move_terminal_to_new_window(
    context: CommandContext,
    (): (),
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    context
        .shell(move |shell, target, window, cx| {
            Ok(
                shell.move_terminal_to_new_window_with(
                    target,
                    window,
                    cx,
                    open_tab_transfer_window,
                ),
            )
        })
        .await
}

#[command("workbench.close-tab", title = "Close Tab", id = CLOSE_TAB_COMMAND, participant = Workbench)]
pub(crate) async fn close_tab(
    context: CommandContext,
    (): (),
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    close_target(&context, ProtectedCloseKind::Tab).await
}

#[command("workbench.split-horizontal", title = "Split Right", id = SPLIT_HORIZONTAL_COMMAND, participant = Workbench)]
pub(crate) async fn split_horizontal(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    split(&context, SplitDirection::Horizontal).await
}

#[command("workbench.split-vertical", title = "Split Down", id = SPLIT_VERTICAL_COMMAND, participant = Workbench)]
pub(crate) async fn split_vertical(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    split(&context, SplitDirection::Vertical).await
}

#[command("file.open", title = "Open…", id = OPEN_COMMAND, participant = Workbench)]
pub(crate) async fn open(
    context: CommandContext,
    (): (),
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    context.pending(dispatch_open_to_captured_target).await
}

#[command("file.save", title = "Save", id = SAVE_COMMAND, participant = Workbench)]
pub(crate) async fn save(
    context: CommandContext,
    (): (),
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    save_to_target(&context, false).await
}

#[command("file.save-as", title = "Save As…", id = SAVE_AS_COMMAND, participant = Workbench)]
pub(crate) async fn save_as(
    context: CommandContext,
    (): (),
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    save_to_target(&context, true).await
}

#[command("file.recover", title = "Recover Unsaved Documents…", id = RECOVER_COMMAND, participant = Workbench)]
pub(crate) async fn recover(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    context
        .shell(|shell, target, _, cx| {
            shell.start_recovery_listing(target, cx);
            Ok(())
        })
        .await
}

#[command("editor.show-completions", title = "Show Completions", id = SHOW_COMPLETIONS_COMMAND, participant = Workbench)]
pub(crate) async fn show_completions(
    context: CommandContext,
    (): (),
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    context
        .foreground(move |target, _, cx| {
            Ok(super::super::extension_host::start_completion(
                target.clone(),
                cx,
            ))
        })
        .await
}

#[command("workbench.show-command-palette", title = "Show Command Palette", id = SHOW_COMMAND_PALETTE_COMMAND, participant = Workbench)]
pub(crate) async fn show_command_palette(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    context
        .shell(move |shell, target, window, cx| {
            shell.open_command_palette_from(target.clone(), window, cx);
            Ok(())
        })
        .await
}

#[command("extensions.show-startup-report", title = "Show Extension Startup Report", id = SHOW_EXTENSION_REPORT_COMMAND, participant = Application)]
pub(crate) async fn show_extension_report(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    context
        .shell(|shell, _, _, cx| {
            shell.toggle_extension_report(cx);
            Ok(())
        })
        .await
}

#[command("window.new", title = "New Window", id = NEW_WINDOW_COMMAND, participant = Application)]
pub(crate) async fn new_window(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    context
        .foreground(|_, _, cx| {
            open_product_window(None, cx);
            Ok(())
        })
        .await
}

#[command("window.close", title = "Close Window", id = CLOSE_WINDOW_COMMAND, participant = Application)]
pub(crate) async fn close_window(
    context: CommandContext,
    (): (),
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    close_target(&context, ProtectedCloseKind::Window).await
}

#[command("application.quit", title = "Quit Knot", id = QUIT_COMMAND, participant = Application)]
pub(crate) async fn quit(
    context: CommandContext,
    (): (),
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::product_commands::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    close_target(&context, ProtectedCloseKind::Quit).await
}

#[command("editor.find", title = "Find", id = FIND_COMMAND, participant = Application, claim = decline_find)]
#[command("editor.find-next", title = "Find Next", id = FIND_NEXT_COMMAND, participant = Application, claim = decline_find)]
#[command("editor.find-previous", title = "Find Previous", id = FIND_PREVIOUS_COMMAND, participant = Application, claim = decline_find)]
pub(crate) async fn find_unavailable(
    _context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    Err(CommandOutcome::Unavailable)
}
fn decline_find(_: &crate::host::protocol::Command, _: &ProductCommandTarget, _: &App) -> bool {
    false
}

async fn split(context: &CommandContext, direction: SplitDirection) -> Result<(), CommandOutcome> {
    context
        .shell(move |shell, target, window, cx| {
            if shell.split_pane(target.pane, direction, window, cx) {
                Ok(())
            } else {
                Err(CommandOutcome::InvalidTarget)
            }
        })
        .await
}
async fn save_to_target(
    context: &CommandContext,
    save_as: bool,
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .pending(move |target, completion, cx| {
            dispatch_save_to_captured_target(target, save_as, completion, cx)
        })
        .await
}
async fn close_target(
    context: &CommandContext,
    kind: ProtectedCloseKind,
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .pending(move |target, completion, cx| {
            dispatch_close_to_captured_target(target, kind, completion, cx)
        })
        .await
}

#[command("workbench.toggle-explorer", title = "Toggle Explorer", id = TOGGLE_EXPLORER_COMMAND, participant = Workbench)]
pub(crate) async fn toggle_explorer(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, _, window, cx| {
            shell.toggle_explorer(window, cx);
            Ok(())
        })
        .await
}

#[command("workbench.toggle-left-sidebar", title = "Toggle Left Sidebar", id = TOGGLE_LEFT_SIDEBAR_COMMAND, participant = Workbench)]
pub(crate) async fn toggle_left_sidebar(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    toggle_dock(context, DockLocation::Left).await
}

#[command("workbench.toggle-right-sidebar", title = "Toggle Right Sidebar", id = TOGGLE_RIGHT_SIDEBAR_COMMAND, participant = Workbench)]
pub(crate) async fn toggle_right_sidebar(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    toggle_dock(context, DockLocation::Right).await
}

#[command("workbench.toggle-bottom-dock", title = "Toggle Bottom Dock", id = TOGGLE_BOTTOM_DOCK_COMMAND, participant = Workbench)]
pub(crate) async fn toggle_bottom_dock(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    toggle_dock(context, DockLocation::Bottom).await
}

async fn toggle_dock(context: CommandContext, dock: DockLocation) -> Result<(), CommandOutcome> {
    context
        .shell(move |shell, _, window, cx| {
            shell
                .workbench
                .update(cx, |workbench, _| workbench.toggle_dock(dock));
            shell.publish_tool_layout(cx);
            shell.focus_active_surface(window, cx);
            cx.notify();
            Ok(())
        })
        .await
}

#[command("workbench.explorer-left", title = "Move Explorer to Left Sidebar", id = EXPLORER_LEFT_COMMAND, participant = Workbench)]
pub(crate) async fn explorer_left(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    place_explorer(context, ToolPlacement::Dock(DockLocation::Left)).await
}

#[command("workbench.explorer-right", title = "Move Explorer to Right Sidebar", id = EXPLORER_RIGHT_COMMAND, participant = Workbench)]
pub(crate) async fn explorer_right(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    place_explorer(context, ToolPlacement::Dock(DockLocation::Right)).await
}

#[command("workbench.explorer-bottom", title = "Move Explorer to Bottom Dock", id = EXPLORER_BOTTOM_COMMAND, participant = Workbench)]
pub(crate) async fn explorer_bottom(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    place_explorer(context, ToolPlacement::Dock(DockLocation::Bottom)).await
}

#[command("workbench.explorer-in-editor", title = "Move Explorer to Editor Tabs", id = EXPLORER_IN_EDITOR_COMMAND, participant = Workbench)]
pub(crate) async fn explorer_in_editor(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    place_explorer(context, ToolPlacement::DocumentGroup).await
}

async fn place_explorer(
    context: CommandContext,
    placement: ToolPlacement,
) -> Result<(), CommandOutcome> {
    context
        .shell(move |shell, _, window, cx| {
            if shell.workspace_tree.is_none() {
                shell.toggle_explorer(window, cx);
            }
            let id = shell.explorer_frame(cx).unwrap();
            shell.workbench.update(cx, |workbench, _| {
                workbench.place_tool(id, placement);
            });
            shell.publish_tool_layout(cx);
            shell.focus_active_surface(window, cx);
            cx.notify();
            Ok(())
        })
        .await
}

#[command("workbench.reset-layout", title = "Reset Layout", id = RESET_LAYOUT_COMMAND, participant = Workbench)]
pub(crate) async fn reset_layout(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(move |shell, _, window, cx| {
            shell.reset_tool_layout(window, cx);
            Ok(())
        })
        .await
}
