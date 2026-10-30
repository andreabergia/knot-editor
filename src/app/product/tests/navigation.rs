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
