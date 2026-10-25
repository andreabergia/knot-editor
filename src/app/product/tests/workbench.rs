//! Tabs, splits, blank replacements, and view lifetime across windows.
use super::*;

#[gpui::test]
fn shell_actions_render_tabs_and_both_split_directions(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
        cx.global::<ApplicationWorkbenches>()
            .register(&workbench, window.window_handle());
        ProductShell::new(workbench)
    });

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.new_tab(window, cx));
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Vertical, window, cx)
        });
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        assert_eq!(workbench.panes().len(), 3);
        assert!(matches!(
            workbench.layout(),
            Some(WorkbenchLayout::Split {
                direction: SplitDirection::Horizontal,
                second,
                ..
            }) if matches!(
                second.as_ref(),
                WorkbenchLayout::Split {
                    direction: SplitDirection::Vertical,
                    ..
                }
            )
        ));
        assert_eq!(
            workbench
                .panes()
                .iter()
                .map(|pane| pane.tabs().len())
                .sum::<usize>(),
            4
        );
    });
}

#[gpui::test]
fn new_tab_targets_the_pane_whose_editor_has_focus(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| Workbench::new_for_document(document, model, cx));
        cx.global::<ApplicationWorkbenches>()
            .register(&workbench, window.window_handle());
        ProductShell::new(workbench)
    });

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
    });
    cx.run_until_parked();
    cx.refresh().unwrap();

    let (first_pane, first_editor, second_editor) = cx.read(|cx| {
        let shell = shell.read(cx);
        let workbench = shell.workbench.read(cx);
        let first_pane = workbench.panes()[0].id();
        (
            first_pane,
            workbench
                .pane(first_pane)
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone(),
            workbench.panes()[1].active_tab().editor().unwrap().clone(),
        )
    });
    cx.update(|window, cx| second_editor.focus_handle(cx).focus(window));
    cx.run_until_parked();
    cx.refresh().unwrap();
    assert!(cx.update(|window, cx| second_editor.focus_handle(cx).is_focused(window)));
    cx.update(|window, cx| first_editor.focus_handle(cx).focus(window));
    cx.run_until_parked();
    cx.refresh().unwrap();
    assert!(cx.update(|window, cx| first_editor.focus_handle(cx).is_focused(window)));

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.new_tab(window, cx));
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        assert_eq!(workbench.pane(first_pane).unwrap().tabs().len(), 2);
        assert_eq!(
            workbench
                .panes()
                .iter()
                .find(|pane| pane.id() != first_pane)
                .unwrap()
                .tabs()
                .len(),
            1
        );
    });
}

#[gpui::test]
fn closing_the_final_clean_tab_creates_a_replacement(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let original = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(original).unwrap().model().clone());
    let (shell, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| Workbench::new_for_document(original, model, cx));
        cx.global::<ApplicationWorkbenches>()
            .register(&workbench, window.window_handle());
        ProductShell::new(workbench)
    });

    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| shell.close_active_tab(window, cx));
    });
    cx.run_until_parked();

    cx.read(|cx| {
        let replacement = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .document_id()
            .unwrap();
        assert_ne!(replacement, original);
        assert!(documents.read(cx).get(original).is_none());
        assert!(documents.read(cx).get(replacement).is_some());
        assert_eq!(
            shell.read(cx).status,
            "created replacement untitled document"
        );
    });
}

#[gpui::test]
fn replacing_a_shared_blank_tab_preserves_the_other_windows_document(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let blank = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(blank).unwrap().model().clone());
    let (first, first_window) = product_window(blank, model.clone(), cx);
    let (second, _) = product_window(blank, model.clone(), cx);
    let replacement = cx.update(|cx| create_untitled_document(&documents, cx));
    cx.update_window(first_window, |_, _window, cx| {
        first.update(cx, |shell, cx| {
            let pane = shell.workbench.read(cx).focused_pane().unwrap();
            let target = super::super::OpenTarget {
                window: _window.window_handle(),
                workbench: shell.workbench.downgrade(),
                pane: pane.id(),
                tab: pane.active_tab_id(),
                surface: pane.active_tab().surface_id(),
                presentation: pane
                    .active_tab()
                    .editor()
                    .map(Entity::entity_id)
                    .or_else(|| pane.active_tab().terminal_view().map(Entity::entity_id)),
            };
            shell.present_open_document(replacement, &target, true, cx);
        });
    })
    .unwrap();
    cx.read(|cx| {
        assert!(documents.read(cx).get(blank).is_some());
        assert!(model.read(cx).is_open());
        assert_eq!(first.read(cx).workbench.read(cx).view_count(blank), 0);
        assert_eq!(second.read(cx).workbench.read(cx).view_count(blank), 1);
    });
}

#[gpui::test]
fn workbench_registry_counts_views_across_windows(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (_first, _) = product_window(document, model.clone(), cx);
    let (_second, _) = product_window(document, model, cx);
    cx.read(|cx| {
        assert_eq!(
            cx.global::<ApplicationWorkbenches>()
                .view_count(document, cx),
            2
        )
    });
}

#[gpui::test]
async fn retained_closed_shell_does_not_keep_the_last_live_document_open(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (closed_shell, closed_window) = product_window(document, model.clone(), cx);
    let (live_shell, live_window) = product_window(document, model.clone(), cx);
    assert_eq!(
        dispatch_product_command(
            &closed_shell,
            closed_window,
            super::super::CLOSE_WINDOW_COMMAND,
            cx
        )
        .completion
        .await
        .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            closed_shell
                .read(cx)
                .workbench
                .read(cx)
                .view_count(document),
            1
        );
        assert!(documents.read(cx).get(document).is_some());
    });
    assert_eq!(
        dispatch_product_command(&live_shell, live_window, CLOSE_TAB_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert!(documents.read(cx).get(document).is_none());
        assert!(!model.read(cx).is_open());
    });
}
