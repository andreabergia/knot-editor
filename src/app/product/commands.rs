//! Workbench and application command declarations.
use super::*;
use crate::app::commands::declarations::{CommandContext, command};
use crate::app::workbench::SplitEdge;
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

#[command("workbench.close-other-tabs", title = "Close Other Tabs", description = "Close every unpinned tab of the focused pane except the focused one.", id = CLOSE_OTHER_TABS_COMMAND, participant = Workbench)]
pub(crate) async fn close_other_tabs(
    context: CommandContext,
    (): (),
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::commands::dispatch::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    close_target(&context, ProtectedCloseKind::OtherTabs).await
}

#[command("workbench.close-pane", title = "Close Pane", description = "Close every tab of the focused pane.", id = CLOSE_PANE_COMMAND, participant = Workbench)]
pub(crate) async fn close_pane(
    context: CommandContext,
    (): (),
) -> Result<CommandOutcome, CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            super::super::commands::dispatch::break_captured_history_group(target, cx);
            Ok(())
        })
        .await?;
    close_target(&context, ProtectedCloseKind::Pane).await
}

#[command("workbench.toggle-pin-tab", title = "Toggle Pin Tab", description = "Pin the focused tab before the others, or unpin it.", id = TOGGLE_PIN_TAB_COMMAND, participant = Workbench)]
pub(crate) async fn toggle_pin_tab(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, target, _, cx| {
            shell
                .workbench
                .update(cx, |workbench, _| {
                    workbench.toggle_pin(target.pane, target.tab)
                })
                .ok_or(CommandOutcome::InvalidTarget)?;
            cx.notify();
            Ok(())
        })
        .await
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

#[command("workbench.next-tab", title = "Next Tab", description = "Show the next tab of the focused pane.", id = NEXT_TAB_COMMAND, participant = Workbench)]
pub(crate) async fn next_tab(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, target, window, cx| shell.cycle_tab(target.pane, true, window, cx))
        .await
}

#[command("workbench.previous-tab", title = "Previous Tab", description = "Show the previous tab of the focused pane.", id = PREVIOUS_TAB_COMMAND, participant = Workbench)]
pub(crate) async fn previous_tab(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, target, window, cx| shell.cycle_tab(target.pane, false, window, cx))
        .await
}

#[command("workbench.next-recent-tab", title = "Next Recently Used Tab", description = "Walk the focused pane's tabs from most to least recently used.", id = NEXT_RECENT_TAB_COMMAND, participant = Workbench)]
pub(crate) async fn next_recent_tab(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, target, window, cx| shell.walk_recent_tabs(target.pane, true, window, cx))
        .await
}

#[command("workbench.previous-recent-tab", title = "Previous Recently Used Tab", description = "Walk the focused pane's tabs from least to most recently used.", id = PREVIOUS_RECENT_TAB_COMMAND, participant = Workbench)]
pub(crate) async fn previous_recent_tab(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, target, window, cx| shell.walk_recent_tabs(target.pane, false, window, cx))
        .await
}

macro_rules! go_to_tab {
    ($($name:literal, $title:literal, $id:ident, $function:ident, $index:expr;)*) => {$(
        #[command($name, title = $title, palette = false, id = $id, participant = Workbench)]
        pub(crate) async fn $function(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
            context
                .shell(|shell, target, window, cx| shell.go_to_tab(target.pane, $index, window, cx))
                .await
        }
    )*};
}

go_to_tab! {
    "workbench.go-to-tab-1", "Go to Tab 1", GO_TO_TAB_1_COMMAND, go_to_tab_1, Some(0);
    "workbench.go-to-tab-2", "Go to Tab 2", GO_TO_TAB_2_COMMAND, go_to_tab_2, Some(1);
    "workbench.go-to-tab-3", "Go to Tab 3", GO_TO_TAB_3_COMMAND, go_to_tab_3, Some(2);
    "workbench.go-to-tab-4", "Go to Tab 4", GO_TO_TAB_4_COMMAND, go_to_tab_4, Some(3);
    "workbench.go-to-tab-5", "Go to Tab 5", GO_TO_TAB_5_COMMAND, go_to_tab_5, Some(4);
    "workbench.go-to-tab-6", "Go to Tab 6", GO_TO_TAB_6_COMMAND, go_to_tab_6, Some(5);
    "workbench.go-to-tab-7", "Go to Tab 7", GO_TO_TAB_7_COMMAND, go_to_tab_7, Some(6);
    "workbench.go-to-tab-8", "Go to Tab 8", GO_TO_TAB_8_COMMAND, go_to_tab_8, Some(7);
}

#[command("workbench.go-to-last-tab", title = "Go to Last Tab", description = "Show the last tab of the focused pane.", id = GO_TO_LAST_TAB_COMMAND, participant = Workbench)]
pub(crate) async fn go_to_last_tab(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, target, window, cx| shell.go_to_tab(target.pane, None, window, cx))
        .await
}

#[command("workbench.focus-pane-left", title = "Focus Pane Left", description = "Focus the pane to the left.", id = FOCUS_PANE_LEFT_COMMAND, participant = Workbench)]
pub(crate) async fn focus_pane_left(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    focus_toward(context, SplitEdge::Left).await
}

#[command("workbench.focus-pane-right", title = "Focus Pane Right", description = "Focus the pane to the right.", id = FOCUS_PANE_RIGHT_COMMAND, participant = Workbench)]
pub(crate) async fn focus_pane_right(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    focus_toward(context, SplitEdge::Right).await
}

#[command("workbench.focus-pane-up", title = "Focus Pane Up", description = "Focus the pane above.", id = FOCUS_PANE_UP_COMMAND, participant = Workbench)]
pub(crate) async fn focus_pane_up(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    focus_toward(context, SplitEdge::Top).await
}

#[command("workbench.focus-pane-down", title = "Focus Pane Down", description = "Focus the pane below.", id = FOCUS_PANE_DOWN_COMMAND, participant = Workbench)]
pub(crate) async fn focus_pane_down(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    focus_toward(context, SplitEdge::Bottom).await
}

#[command("workbench.move-tab-left", title = "Move Tab Left", description = "Move the focused tab to the pane on the left, splitting when there is none.", id = MOVE_TAB_LEFT_COMMAND, participant = Workbench)]
pub(crate) async fn move_tab_left(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    move_toward(context, SplitEdge::Left).await
}

#[command("workbench.move-tab-right", title = "Move Tab Right", description = "Move the focused tab to the pane on the right, splitting when there is none.", id = MOVE_TAB_RIGHT_COMMAND, participant = Workbench)]
pub(crate) async fn move_tab_right(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    move_toward(context, SplitEdge::Right).await
}

#[command("workbench.move-tab-up", title = "Move Tab Up", description = "Move the focused tab to the pane above, splitting when there is none.", id = MOVE_TAB_UP_COMMAND, participant = Workbench)]
pub(crate) async fn move_tab_up(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    move_toward(context, SplitEdge::Top).await
}

#[command("workbench.move-tab-down", title = "Move Tab Down", description = "Move the focused tab to the pane below, splitting when there is none.", id = MOVE_TAB_DOWN_COMMAND, participant = Workbench)]
pub(crate) async fn move_tab_down(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    move_toward(context, SplitEdge::Bottom).await
}

async fn focus_toward(context: CommandContext, edge: SplitEdge) -> Result<(), CommandOutcome> {
    context
        .shell(move |shell, target, window, cx| {
            shell.focus_pane_toward(target.pane, edge, window, cx)
        })
        .await
}

async fn move_toward(context: CommandContext, edge: SplitEdge) -> Result<(), CommandOutcome> {
    context
        .shell(move |shell, target, window, cx| {
            shell.move_tab_toward(target.pane, target.tab, edge, window, cx)
        })
        .await
}

#[command("workbench.toggle-maximize-pane", title = "Toggle Maximize Pane", description = "Show only the focused pane, or the whole layout again.", id = TOGGLE_MAXIMIZE_PANE_COMMAND, participant = Workbench)]
pub(crate) async fn toggle_maximize_pane(
    context: CommandContext,
    (): (),
) -> Result<(), CommandOutcome> {
    context
        .shell(|shell, target, _, cx| {
            shell.toggle_maximize(target.pane, cx);
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
