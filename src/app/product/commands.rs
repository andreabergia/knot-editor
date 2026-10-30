//! Workbench and application command declarations.
use super::*;
use crate::app::commands::declarations::{CommandContext, command};
use crate::host::protocol::CommandOutcome;

#[command("file.new", title = "New Document", id = NEW_COMMAND, participant = Workbench)]
pub(crate) async fn new(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::commands::dispatch::break_captured_history_group(target, cx);
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
            super::super::commands::dispatch::break_captured_history_group(target, cx);
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

#[command("workbench.close-tab", title = "Close Tab", id = CLOSE_TAB_COMMAND, participant = Workbench)]
pub(crate) async fn close_tab(
    context: CommandContext,
    (): (),
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::commands::dispatch::break_captured_history_group(target, cx);
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
            super::super::commands::dispatch::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    split(&context, SplitDirection::Horizontal).await
}

#[command("workbench.split-vertical", title = "Split Down", id = SPLIT_VERTICAL_COMMAND, participant = Workbench)]
pub(crate) async fn split_vertical(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::commands::dispatch::break_captured_history_group(target, cx);
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
            super::super::commands::dispatch::break_captured_history_group(target, cx);
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
            super::super::commands::dispatch::break_captured_history_group(target, cx);
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
            super::super::commands::dispatch::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    save_to_target(&context, true).await
}

#[command("file.recover", title = "Recover Unsaved Documents…", id = RECOVER_COMMAND, participant = Workbench)]
pub(crate) async fn recover(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::commands::dispatch::break_captured_history_group(target, cx);
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

#[command("editor.set-language-mode", title = "Set Language Mode…", id = SET_LANGUAGE_MODE_COMMAND, participant = Workbench)]
pub(crate) async fn set_language_mode(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, target, window, cx| shell.start_language_mode(target, window, cx))
        .await
}

#[command("extensions.restart", title = "Developer: Restart Extension…", id = RESTART_EXTENSION_COMMAND, participant = Workbench)]
pub(crate) async fn restart_extension(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, _, window, cx| {
            shell.start_extension_restart(window, cx);
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
            super::super::commands::dispatch::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    context
        .foreground(move |target, _, cx| {
            Ok(super::super::extensions::host::start_completion(
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
            super::super::commands::dispatch::break_captured_history_group(target, cx);
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

#[command("extensions.show-startup-report", title = "Developer: Show Extension Startup Report", id = SHOW_EXTENSION_REPORT_COMMAND, participant = Application)]
pub(crate) async fn show_extension_report(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::commands::dispatch::break_captured_history_group(target, cx);
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

#[command("window.new", title = "New Blank Window", id = NEW_WINDOW_COMMAND, participant = Application)]
pub(crate) async fn new_window(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::commands::dispatch::break_captured_history_group(target, cx);
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
            super::super::commands::dispatch::break_captured_history_group(target, cx);
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
            super::super::commands::dispatch::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    close_target(&context, ProtectedCloseKind::Quit).await
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

#[command("workspace.add-folder", title = "Add Folder to Workspace…", id = ADD_FOLDER_COMMAND, participant = Workbench)]
pub(crate) async fn add_folder(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, target, window, cx| {
            shell.start_add_folder(target, window, cx);
            Ok(())
        })
        .await
}

#[command("workspace.remove-folder", title = "Remove Folder from Workspace", id = REMOVE_FOLDER_COMMAND, participant = Workbench)]
pub(crate) async fn remove_folder(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, _, _, cx| shell.remove_selected_root(cx))
        .await
}

#[command("workspace.move-folder-up", title = "Move Folder Up", id = MOVE_FOLDER_UP_COMMAND, participant = Workbench)]
pub(crate) async fn move_folder_up(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, _, _, cx| shell.move_selected_root(true, cx))
        .await
}

#[command("workspace.move-folder-down", title = "Move Folder Down", id = MOVE_FOLDER_DOWN_COMMAND, participant = Workbench)]
pub(crate) async fn move_folder_down(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, _, _, cx| shell.move_selected_root(false, cx))
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

#[command("workbench.reset-layout", title = "Reset Layout", id = RESET_LAYOUT_COMMAND, participant = Workbench)]
pub(crate) async fn reset_layout(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(move |shell, _, window, cx| {
            shell.reset_tool_layout(window, cx);
            Ok(())
        })
        .await
}
