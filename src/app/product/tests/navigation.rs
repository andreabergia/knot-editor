//! Keyboard navigation between tabs and panes.
use super::*;
use crate::app::workbench::TabId;

/// A window whose one pane holds four views of one document, the last active.
fn four_tabs(cx: &mut TestAppContext) -> (Entity<ProductShell>, gpui::AnyWindowHandle) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    shell.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, cx| {
            let pane = workbench.focused_pane_id().unwrap();
            for _ in 0..3 {
                workbench
                    .open_tab_for_document(pane, document, model.clone(), cx)
                    .unwrap();
            }
        });
    });
    cx.run_until_parked();
    (shell, window)
}

fn tabs(shell: &Entity<ProductShell>, cx: &TestAppContext) -> Vec<TabId> {
    cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .tabs()
            .iter()
            .map(|tab| tab.id())
            .collect()
    })
}

fn active(shell: &Entity<ProductShell>, cx: &TestAppContext) -> TabId {
    cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab_id()
    })
}

fn recent(shell: &Entity<ProductShell>, cx: &TestAppContext) -> Vec<TabId> {
    cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .recent_tabs()
    })
}

fn run(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    name: &'static str,
    cx: &mut TestAppContext,
) -> CommandOutcome {
    let execution = dispatch_product_command(shell, window, name, cx);
    wait_for_command(execution, cx)
}

#[gpui::test]
fn ordered_and_positional_tab_commands_wrap_and_select_the_last(cx: &mut TestAppContext) {
    use crate::app::commands::dispatch::{
        GO_TO_LAST_TAB_COMMAND, GO_TO_TAB_2_COMMAND, GO_TO_TAB_8_COMMAND, NEXT_TAB_COMMAND,
        PREVIOUS_TAB_COMMAND,
    };
    let (shell, window) = four_tabs(cx);
    let order = tabs(&shell, cx);
    assert_eq!(
        run(&shell, window, NEXT_TAB_COMMAND, cx),
        CommandOutcome::Completed
    );
    assert_eq!(active(&shell, cx), order[0]);
    run(&shell, window, PREVIOUS_TAB_COMMAND, cx);
    assert_eq!(active(&shell, cx), order[3]);
    run(&shell, window, GO_TO_TAB_2_COMMAND, cx);
    assert_eq!(active(&shell, cx), order[1]);
    assert_eq!(
        run(&shell, window, GO_TO_TAB_8_COMMAND, cx),
        CommandOutcome::Unavailable
    );
    assert_eq!(active(&shell, cx), order[1]);
    run(&shell, window, GO_TO_LAST_TAB_COMMAND, cx);
    assert_eq!(active(&shell, cx), order[3]);
}

#[gpui::test]
fn a_recent_tab_walk_commits_one_activation_when_ctrl_is_released(cx: &mut TestAppContext) {
    use crate::app::commands::dispatch::{NEXT_RECENT_TAB_COMMAND, PREVIOUS_RECENT_TAB_COMMAND};
    let (shell, window) = four_tabs(cx);
    let order = tabs(&shell, cx);
    // Activate tabs so the recent order differs from tab order.
    for index in [1, 0, 3] {
        shell.update(cx, |shell, cx| {
            shell.workbench.update(cx, |workbench, _| {
                let pane = workbench.focused_pane_id().unwrap();
                workbench.activate_tab(pane, order[index]);
            })
        });
    }
    let before = recent(&shell, cx);
    assert_eq!(before, [order[3], order[0], order[1], order[2]]);

    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_modifiers_change(Modifiers::control());
    run(&shell, window, NEXT_RECENT_TAB_COMMAND, cx);
    run(&shell, window, NEXT_RECENT_TAB_COMMAND, cx);
    run(&shell, window, NEXT_RECENT_TAB_COMMAND, cx);
    run(&shell, window, PREVIOUS_RECENT_TAB_COMMAND, cx);
    // The walk shows tabs without reordering them.
    assert_eq!(active(&shell, cx), order[1]);
    assert_eq!(recent(&shell, cx), before);

    let mut visual = VisualTestContext::from_window(window, cx);
    visual.simulate_modifiers_change(Modifiers::none());
    visual.run_until_parked();
    assert_eq!(active(&shell, cx), order[1]);
    assert_eq!(recent(&shell, cx), [order[1], order[3], order[0], order[2]]);

    // Without ctrl held, as from the palette, a step commits at once and
    // switches back to the previous tab.
    run(&shell, window, NEXT_RECENT_TAB_COMMAND, cx);
    assert_eq!(active(&shell, cx), order[3]);
    assert_eq!(recent(&shell, cx)[0], order[3]);
}

fn pane_ids(
    shell: &Entity<ProductShell>,
    cx: &TestAppContext,
) -> Vec<crate::app::workbench::PaneId> {
    cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .panes()
            .iter()
            .map(|pane| pane.id())
            .collect()
    })
}

fn focused(shell: &Entity<ProductShell>, cx: &TestAppContext) -> crate::app::workbench::PaneId {
    cx.read(|cx| shell.read(cx).workbench.read(cx).focused_pane_id().unwrap())
}

fn focus(
    shell: &Entity<ProductShell>,
    window: gpui::AnyWindowHandle,
    pane: crate::app::workbench::PaneId,
    cx: &mut TestAppContext,
) {
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell
                .workbench
                .update(cx, |workbench, _| workbench.focus_pane(pane));
            shell.focus_active_surface(window, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
}

/// Whether the last frame painted `pane`.
fn painted(
    shell: &Entity<ProductShell>,
    pane: crate::app::workbench::PaneId,
    cx: &mut TestAppContext,
) -> bool {
    cx.run_until_parked();
    cx.read(|cx| {
        shell
            .read(cx)
            .tab_drag
            .borrow()
            .painted_panes()
            .contains(&pane)
    })
}

/// A document pane with the explorer docked on its left and two tabs.
fn docked_window(cx: &mut TestAppContext) -> (Entity<ProductShell>, gpui::AnyWindowHandle) {
    let (shell, window) = four_tabs(cx);
    shell.update(cx, |shell, cx| {
        shell.ensure_explorer(true, cx);
        let pane = shell.workbench.read(cx).normal_pane().unwrap();
        shell
            .workbench
            .update(cx, |workbench, _| workbench.focus_pane(pane));
        cx.notify();
    });
    cx.run_until_parked();
    (shell, window)
}

fn dock_and_main(
    shell: &Entity<ProductShell>,
    cx: &TestAppContext,
) -> (crate::app::workbench::PaneId, crate::app::workbench::PaneId) {
    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let dock = workbench
            .dock_pane(crate::app::workbench::DockLocation::Left)
            .unwrap();
        (dock, workbench.normal_pane().unwrap())
    })
}

#[gpui::test]
fn moving_tabs_by_direction_splits_at_edges_enters_docks_and_collapses_emptied_panes(
    cx: &mut TestAppContext,
) {
    use crate::app::commands::dispatch::{
        FOCUS_PANE_LEFT_COMMAND, FOCUS_PANE_RIGHT_COMMAND, MOVE_TAB_LEFT_COMMAND,
        MOVE_TAB_RIGHT_COMMAND,
    };
    let (shell, window) = docked_window(cx);
    let (dock, main) = dock_and_main(&shell, cx);
    focus(&shell, window, main, cx);

    // No pane lies to the right, so the tab splits its pane on that side.
    let moved = active(&shell, cx);
    assert_eq!(
        run(&shell, window, MOVE_TAB_RIGHT_COMMAND, cx),
        CommandOutcome::Completed
    );
    let right = focused(&shell, cx);
    assert_ne!(right, main);
    assert_eq!(tabs(&shell, cx), [moved]);
    assert_eq!(pane_ids(&shell, cx).len(), 3);

    // Moving the only tab back collapses the emptied pane.
    assert_eq!(
        run(&shell, window, MOVE_TAB_LEFT_COMMAND, cx),
        CommandOutcome::Completed
    );
    assert_eq!(focused(&shell, cx), main);
    assert_eq!(dock_and_main(&shell, cx), (dock, main));
    assert_eq!(pane_ids(&shell, cx).len(), 2);
    assert_eq!(tabs(&shell, cx).last(), Some(&moved));

    // Focus and tabs move into the dock on the left.
    assert_eq!(
        run(&shell, window, FOCUS_PANE_LEFT_COMMAND, cx),
        CommandOutcome::Completed
    );
    assert_eq!(focused(&shell, cx), dock);
    assert_eq!(
        run(&shell, window, FOCUS_PANE_LEFT_COMMAND, cx),
        CommandOutcome::Unavailable
    );
    run(&shell, window, FOCUS_PANE_RIGHT_COMMAND, cx);
    assert_eq!(focused(&shell, cx), main);
    run(&shell, window, MOVE_TAB_LEFT_COMMAND, cx);
    assert_eq!(focused(&shell, cx), dock);
    assert_eq!(active(&shell, cx), moved);
    assert_eq!(tabs(&shell, cx).len(), 2);
}

#[gpui::test]
fn maximize_ends_on_toggle_split_close_and_directional_focus(cx: &mut TestAppContext) {
    use crate::app::commands::dispatch::{
        CLOSE_TAB_COMMAND, FOCUS_PANE_LEFT_COMMAND, SPLIT_HORIZONTAL_COMMAND,
        TOGGLE_MAXIMIZE_PANE_COMMAND,
    };
    let (shell, window) = docked_window(cx);
    let (dock, main) = dock_and_main(&shell, cx);
    focus(&shell, window, main, cx);
    let maximize = |cx: &mut TestAppContext| {
        assert_eq!(
            run(&shell, window, TOGGLE_MAXIMIZE_PANE_COMMAND, cx),
            CommandOutcome::Completed
        );
    };

    maximize(cx);
    assert!(painted(&shell, main, cx));
    assert!(!painted(&shell, dock, cx));
    maximize(cx);
    assert!(painted(&shell, dock, cx));

    // Splitting focuses the new pane, which ends the maximized state.
    maximize(cx);
    run(&shell, window, SPLIT_HORIZONTAL_COMMAND, cx);
    let split = focused(&shell, cx);
    assert!(painted(&shell, dock, cx));
    assert!(painted(&shell, main, cx));

    // Closing the maximized pane restores the rest.
    maximize(cx);
    assert!(!painted(&shell, main, cx));
    run(&shell, window, CLOSE_TAB_COMMAND, cx);
    assert!(!pane_ids(&shell, cx).contains(&split));
    assert!(painted(&shell, main, cx));
    assert!(painted(&shell, dock, cx));

    // Moving focus by direction restores the layout too.
    focus(&shell, window, main, cx);
    maximize(cx);
    assert!(!painted(&shell, dock, cx));
    run(&shell, window, FOCUS_PANE_LEFT_COMMAND, cx);
    assert_eq!(focused(&shell, cx), dock);
    assert!(painted(&shell, main, cx));
    assert!(painted(&shell, dock, cx));
}
