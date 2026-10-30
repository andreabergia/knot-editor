//! Workbench and application command declarations.
use super::*;
use crate::app::commands::declarations::{CommandContext, command};
use crate::host::protocol::CommandOutcome;

#[command("file.new", title = "New Document", description = "Open an untitled document.", id = NEW_COMMAND, participant = Workbench)]
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

#[command("terminal.new", title = "New Terminal", description = "Open a terminal in a new tab.", id = NEW_TERMINAL_COMMAND, participant = Workbench)]
pub(crate) async fn new_terminal(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::commands::dispatch::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    context
        .shell(move |shell, target, window, cx| {
            let directory = shell.terminal_directory(cx);
            shell
                .new_terminal_in_pane(target.pane, directory, window, cx)
                .map(|_| ())
                .ok_or(CommandOutcome::InvalidTarget)
        })
        .await
}

#[command("workbench.close-tab", title = "Close Tab", description = "Close the focused tab.", id = CLOSE_TAB_COMMAND, participant = Workbench)]
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

#[command("workbench.split-horizontal", title = "Split Right", description = "Split the focused pane to the right.", id = SPLIT_HORIZONTAL_COMMAND, participant = Workbench)]
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
    split(&context, SplitDirection::Horizontal, SplitPlacement::After).await
}

#[command("workbench.split-left", title = "Split Left", description = "Split the focused pane to the left.", id = SPLIT_LEFT_COMMAND, participant = Workbench)]
pub(crate) async fn split_left(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::commands::dispatch::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    split(&context, SplitDirection::Horizontal, SplitPlacement::Before).await
}

#[command("workbench.split-up", title = "Split Up", description = "Split the focused pane upward.", id = SPLIT_UP_COMMAND, participant = Workbench)]
pub(crate) async fn split_up(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::commands::dispatch::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    split(&context, SplitDirection::Vertical, SplitPlacement::Before).await
}

#[command("workbench.split-vertical", title = "Split Down", description = "Split the focused pane downward.", id = SPLIT_VERTICAL_COMMAND, participant = Workbench)]
pub(crate) async fn split_vertical(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::commands::dispatch::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    split(&context, SplitDirection::Vertical, SplitPlacement::After).await
}

#[command("workbench.grow-pane", title = "Grow Pane", description = "Give the focused pane more room.", id = GROW_PANE_COMMAND, participant = Workbench)]
pub(crate) async fn grow_pane(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    resize(&context, true).await
}

#[command("workbench.shrink-pane", title = "Shrink Pane", description = "Give the focused pane less room.", id = SHRINK_PANE_COMMAND, participant = Workbench)]
pub(crate) async fn shrink_pane(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    resize(&context, false).await
}

#[command("workbench.equalize-panes", title = "Equalize Panes", description = "Give every split pane an even share.", id = EQUALIZE_PANES_COMMAND, participant = Workbench)]
pub(crate) async fn equalize_panes(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, _, _, cx| {
            shell
                .workbench
                .update(cx, |workbench, _| workbench.equalize_splits());
            cx.notify();
            Ok(())
        })
        .await
}

async fn resize(context: &CommandContext, grow: bool) -> Result<(), CommandOutcome> {
    context
        .shell(move |shell, target, _, cx| {
            let pane = target.pane;
            let dock = shell.workbench.read(cx).dock_location(pane);
            if !shell
                .workbench
                .update(cx, |workbench, _| workbench.resize_pane(pane, grow))
            {
                return Err(CommandOutcome::Unavailable);
            }
            if dock.is_some() {
                shell.publish_tool_layout(cx);
            }
            cx.notify();
            Ok(())
        })
        .await
}

#[command("file.open", title = "Open…", description = "Open a file or folder.", id = OPEN_COMMAND, participant = Workbench)]
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

#[command("file.save", title = "Save", description = "Save the current document.", id = SAVE_COMMAND, participant = Workbench)]
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

#[command("file.save-as", title = "Save As…", description = "Save the current document under a new name.", id = SAVE_AS_COMMAND, participant = Workbench)]
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

#[command("file.recover", title = "Recover Unsaved Documents…", description = "Restore or discard unsaved text kept from earlier sessions.", id = RECOVER_COMMAND, participant = Workbench)]
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

#[command("editor.set-language-mode", title = "Set Language Mode…", description = "Choose the language of the current document.", id = SET_LANGUAGE_MODE_COMMAND, participant = Workbench)]
pub(crate) async fn set_language_mode(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, target, window, cx| shell.start_language_mode(target, window, cx))
        .await
}

#[command("extensions.restart", title = "Developer: Restart Extension…", description = "Reload one extension package without restarting Knot.", id = RESTART_EXTENSION_COMMAND, participant = Workbench)]
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

#[command("keymaps.show", title = "Show Keybindings", description = "List every key binding, who made it, and what it overrides.", id = SHOW_KEYBINDINGS_COMMAND, participant = Workbench)]
pub(crate) async fn show_keybindings(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, target, window, cx| {
            shell.start_show_keybindings(target.clone(), window, cx);
            Ok(())
        })
        .await
}

#[command("editor.show-completions", title = "Show Completions", description = "Show completions at the cursor.", id = SHOW_COMPLETIONS_COMMAND, participant = Workbench)]
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

#[command("workbench.show-command-palette", title = "Show Command Palette", description = "Search and run commands.", id = SHOW_COMMAND_PALETTE_COMMAND, participant = Workbench)]
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

#[command("extensions.show-startup-report", title = "Developer: Show Extension Startup Report", description = "Show which packages started and why any failed.", id = SHOW_EXTENSION_REPORT_COMMAND, participant = Application)]
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

#[command("window.new", title = "New Blank Window", description = "Open a window without folders.", id = NEW_WINDOW_COMMAND, participant = Application)]
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

#[command("window.close", title = "Close Window", description = "Close the window and its tabs.", id = CLOSE_WINDOW_COMMAND, participant = Application)]
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

#[command("application.quit", title = "Quit Knot", description = "Quit Knot, keeping unsaved text for the next launch.", id = QUIT_COMMAND, participant = Application)]
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

async fn split(
    context: &CommandContext,
    direction: SplitDirection,
    placement: SplitPlacement,
) -> Result<(), CommandOutcome> {
    context
        .shell(move |shell, target, window, cx| {
            if shell.split_pane(target.pane, direction, placement, window, cx) {
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

#[command("workbench.toggle-explorer", title = "Toggle Explorer", description = "Show or hide the file explorer.", id = TOGGLE_EXPLORER_COMMAND, participant = Workbench)]
pub(crate) async fn toggle_explorer(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, _, window, cx| {
            shell.toggle_explorer(window, cx);
            Ok(())
        })
        .await
}

#[command("workspace.add-folder", title = "Add Folder to Workspace…", description = "Add a folder to this window's workspace.", id = ADD_FOLDER_COMMAND, participant = Workbench)]
pub(crate) async fn add_folder(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, target, window, cx| {
            shell.start_add_folder(target, window, cx);
            Ok(())
        })
        .await
}

#[command("workspace.remove-folder", title = "Remove Folder from Workspace", description = "Remove the selected root folder from the workspace.", id = REMOVE_FOLDER_COMMAND, participant = Workbench)]
pub(crate) async fn remove_folder(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, _, _, cx| shell.remove_selected_root(cx))
        .await
}

#[command("workspace.move-folder-up", title = "Move Folder Up", description = "Move the selected root folder up.", id = MOVE_FOLDER_UP_COMMAND, participant = Workbench)]
pub(crate) async fn move_folder_up(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, _, _, cx| shell.move_selected_root(true, cx))
        .await
}

#[command("workspace.move-folder-down", title = "Move Folder Down", description = "Move the selected root folder down.", id = MOVE_FOLDER_DOWN_COMMAND, participant = Workbench)]
pub(crate) async fn move_folder_down(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, _, _, cx| shell.move_selected_root(false, cx))
        .await
}

#[command("workbench.toggle-left-sidebar", title = "Toggle Left Sidebar", description = "Show or hide the left dock.", id = TOGGLE_LEFT_SIDEBAR_COMMAND, participant = Workbench)]
pub(crate) async fn toggle_left_sidebar(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    toggle_dock(context, DockLocation::Left).await
}

#[command("workbench.toggle-right-sidebar", title = "Toggle Right Sidebar", description = "Show or hide the right dock.", id = TOGGLE_RIGHT_SIDEBAR_COMMAND, participant = Workbench)]
pub(crate) async fn toggle_right_sidebar(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    toggle_dock(context, DockLocation::Right).await
}

#[command("workbench.toggle-bottom-dock", title = "Toggle Bottom Dock", description = "Show or hide the bottom dock.", id = TOGGLE_BOTTOM_DOCK_COMMAND, participant = Workbench)]
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

#[command("workbench.reset-layout", title = "Reset Layout", description = "Return docks and tools to their default arrangement.", id = RESET_LAYOUT_COMMAND, participant = Workbench)]
pub(crate) async fn reset_layout(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(move |shell, _, window, cx| {
            shell.reset_tool_layout(window, cx);
            Ok(())
        })
        .await
}
