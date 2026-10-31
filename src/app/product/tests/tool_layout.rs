//! Automatic tool-layout memory across windows and restarts.
use super::operations::empty_product_window;
use super::*;
use crate::app::tool_layout::{
    ApplicationToolLayout, EXPLORER_TOOL_ID, StoredLocation, ToolLayoutSnapshot,
};
use crate::app::workbench::{
    DockLocation, SplitEdge, TabMoveDestination, TabMovePlacement, ToolFrameId, ToolPlacement,
    WorkbenchLayout,
};

fn stored(cx: &TestAppContext) -> ToolLayoutSnapshot {
    cx.read(|cx| cx.global::<ApplicationToolLayout>().0.current())
}

fn explorer_id(shell: &Entity<ProductShell>, cx: &gpui::App) -> ToolFrameId {
    shell
        .read(cx)
        .workbench
        .read(cx)
        .tool_by_stable_id(EXPLORER_TOOL_ID)
        .unwrap()
}

fn open_workspace(shell: &Entity<ProductShell>, cx: &mut TestAppContext) {
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    shell.update(cx, |shell, cx| {
        shell.install_workspace(ResourceUri::parse("mem://product/").unwrap(), cx);
    });
    cx.run_until_parked();
}

#[gpui::test]
fn user_customization_becomes_the_fresh_window_default(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    open_workspace(&shell, cx);
    // Automatic presentation on workspace open does not publish a new default.
    assert_eq!(stored(cx), ToolLayoutSnapshot::product_default());
    shell.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, _| {
            workbench.set_dock_size(DockLocation::Left, 311.);
            workbench.set_dock_size(DockLocation::Bottom, 300.);
        });
    });
    place_explorer(
        &shell,
        window,
        ToolPlacement::Dock(DockLocation::Bottom),
        cx,
    );
    let snapshot = stored(cx);
    let record = snapshot.tool(EXPLORER_TOOL_ID).unwrap();
    assert_eq!(record.location, StoredLocation::Bottom);
    assert!(record.visible);
    assert_eq!(snapshot.dock_sizes[DockLocation::Left.index()], 311.);
    assert_eq!(snapshot.dock_sizes[DockLocation::Bottom.index()], 300.);

    // A fresh window inherits the customized arrangement.
    let (fresh, fresh_window) = empty_product_window(&documents, cx);
    cx.read(|cx| {
        assert!(
            fresh
                .read(cx)
                .workbench
                .read(cx)
                .tool_by_stable_id(EXPLORER_TOOL_ID)
                .is_none()
        );
    });
    open_workspace(&fresh, cx);
    cx.read(|cx| {
        let workbench = fresh.read(cx).workbench.read(cx);
        let (pane, _) = workbench.tool_tab(explorer_id(&fresh, cx)).unwrap();
        assert_eq!(workbench.dock_location(pane), Some(DockLocation::Bottom));
        assert_eq!(workbench.dock_size(DockLocation::Bottom), 300.);
        assert_eq!(workbench.dock_size(DockLocation::Left), 311.);
    });
    let _ = fresh_window;
}

#[gpui::test]
fn existing_windows_keep_live_state_and_activation_does_not_publish(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    open_workspace(&shell, cx);
    place_explorer(&shell, window, ToolPlacement::Dock(DockLocation::Right), cx);
    let published = stored(cx);

    let (fresh, _) = empty_product_window(&documents, cx);
    open_workspace(&fresh, cx);
    // The older window keeps its live arrangement and the fresh window copies it.
    for (candidate, dock) in [(&shell, DockLocation::Right), (&fresh, DockLocation::Right)] {
        cx.read(|cx| {
            let workbench = candidate.read(cx).workbench.read(cx);
            let (pane, _) = workbench.tool_tab(explorer_id(candidate, cx)).unwrap();
            assert_eq!(workbench.dock_location(pane), Some(dock));
        });
    }
    // Interacting with the older window without a layout change publishes nothing.
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.focus_active_surface(window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(stored(cx), published);
}

#[gpui::test]
fn hidden_tools_restore_placement_and_size_across_windows(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    open_workspace(&shell, cx);
    shell.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, _| {
            workbench.set_dock_size(DockLocation::Bottom, 300.);
        });
    });
    place_explorer(
        &shell,
        window,
        ToolPlacement::Dock(DockLocation::Bottom),
        cx,
    );
    let hidden = dispatch_product_command(&shell, window, "workbench.toggle-explorer", cx);
    assert_eq!(wait_for_command(hidden, cx), CommandOutcome::Completed);
    let record = stored(cx).tool(EXPLORER_TOOL_ID).unwrap().clone();
    assert!(!record.visible);
    assert_eq!(record.location, StoredLocation::Bottom);

    // A fresh window remembers the hidden placement without showing the tool.
    let (fresh, fresh_window) = empty_product_window(&documents, cx);
    open_workspace(&fresh, cx);
    cx.read(|cx| {
        let workbench = fresh.read(cx).workbench.read(cx);
        assert!(workbench.tool_tab(explorer_id(&fresh, cx)).is_none());
        assert_eq!(workbench.dock_size(DockLocation::Bottom), 300.);
    });
    // Explicitly reopening returns the tool to its remembered dock and size.
    let reopened = dispatch_product_command(&fresh, fresh_window, "workbench.toggle-explorer", cx);
    assert_eq!(wait_for_command(reopened, cx), CommandOutcome::Completed);
    cx.read(|cx| {
        let workbench = fresh.read(cx).workbench.read(cx);
        let (pane, _) = workbench.tool_tab(explorer_id(&fresh, cx)).unwrap();
        assert_eq!(workbench.dock_location(pane), Some(DockLocation::Bottom));
        assert_eq!(workbench.dock_size(DockLocation::Bottom), 300.);
    });
}

#[gpui::test]
fn mixed_document_groups_leave_no_splits_or_placeholders(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    open_workspace(&shell, cx);
    place_explorer(&shell, window, ToolPlacement::DocumentGroup, cx);
    assert_eq!(
        stored(cx).tool(EXPLORER_TOOL_ID).unwrap().location,
        StoredLocation::DocumentGroup
    );
    // Document splits are not layout changes and never publish.
    shell.update(cx, |shell, cx| {
        let (pane, tab) = shell
            .workbench
            .read(cx)
            .panes()
            .iter()
            .find_map(|pane| {
                pane.tabs()
                    .iter()
                    .find(|tab| tab.document_id().is_some())
                    .map(|tab| (pane.id(), tab.id()))
            })
            .unwrap();
        shell.workbench.update(cx, |workbench, _| {
            workbench.activate_tab(pane, tab);
        });
    });
    let split = dispatch_product_command(&shell, window, "workbench.split-horizontal", cx);
    assert_eq!(wait_for_command(split, cx), CommandOutcome::Completed);
    let published = stored(cx);

    let (fresh, _) = empty_product_window(&documents, cx);
    open_workspace(&fresh, cx);
    cx.read(|cx| {
        let workbench = fresh.read(cx).workbench.read(cx);
        assert!(matches!(workbench.layout(), Some(WorkbenchLayout::Pane(_))));
        let (pane, _) = workbench.tool_tab(explorer_id(&fresh, cx)).unwrap();
        assert_eq!(workbench.dock_location(pane), None);
        assert_eq!(pane, workbench.normal_pane().unwrap());
        assert!(
            workbench
                .pane(pane)
                .unwrap()
                .tabs()
                .iter()
                .any(|tab| tab.document_id().is_some())
        );
    });
    assert_eq!(stored(cx), published);
}

#[gpui::test]
fn reset_layout_restores_product_defaults(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    open_workspace(&shell, cx);
    place_explorer(
        &shell,
        window,
        ToolPlacement::Dock(DockLocation::Bottom),
        cx,
    );
    let hidden = dispatch_product_command(&shell, window, "workbench.toggle-explorer", cx);
    assert_eq!(wait_for_command(hidden, cx), CommandOutcome::Completed);
    assert_ne!(stored(cx), ToolLayoutSnapshot::product_default());

    let reset = dispatch_product_command(&shell, window, "workbench.reset-layout", cx);
    assert_eq!(wait_for_command(reset, cx), CommandOutcome::Completed);
    // Reset publishes product defaults as the confirmed customization.
    let mut expected = ToolLayoutSnapshot::product_default();
    expected.customized = true;
    assert_eq!(stored(cx), expected);
    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let (pane, _) = workbench.tool_tab(explorer_id(&shell, cx)).unwrap();
        assert_eq!(workbench.dock_location(pane), Some(DockLocation::Left));
        for dock in DockLocation::ALL {
            assert_eq!(
                workbench.dock_size(dock),
                ToolLayoutSnapshot::product_default().dock_sizes[dock.index()]
            );
        }
    });
}

#[gpui::test]
fn restore_skips_unknown_tools_without_blank_frames(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, _) = empty_product_window(&documents, cx);
    open_workspace(&shell, cx);
    let before = stored(cx);
    let mut snapshot = ToolLayoutSnapshot::product_default();
    snapshot.tools.push(crate::app::tool_layout::StoredTool {
        id: "knot.missing".into(),
        location: StoredLocation::Right,
        index: 0,
        visible: true,
    });
    snapshot.tools[0].location = StoredLocation::DocumentGroup;
    shell.update(cx, |shell, cx| {
        shell.restore_tool_layout(&snapshot, cx);
    });
    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        assert_eq!(workbench.tools().len(), 1);
        let (pane, _) = workbench.tool_tab(explorer_id(&shell, cx)).unwrap();
        assert_eq!(workbench.dock_location(pane), None);
        assert_eq!(workbench.panes().len(), 1);
    });
    // Restore is never a customization: the stored default is untouched.
    assert_eq!(stored(cx), before);
}

#[gpui::test]
fn fresh_window_restores_visible_tools_without_folder(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    open_workspace(&shell, cx);
    place_explorer(&shell, window, ToolPlacement::Dock(DockLocation::Right), cx);
    // A fresh window with no folder inherits the visible right dock.
    let (fresh, _) = empty_product_window(&documents, cx);
    fresh.update(cx, |fresh, cx| fresh.restore_fresh_window_layout(cx));
    cx.read(|cx| {
        let workbench = fresh.read(cx).workbench.read(cx);
        let (pane, _) = workbench.tool_tab(explorer_id(&fresh, cx)).unwrap();
        assert_eq!(workbench.dock_location(pane), Some(DockLocation::Right));
    });
}

#[gpui::test]
fn fresh_window_without_customization_keeps_first_run(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (fresh, _) = empty_product_window(&documents, cx);
    fresh.update(cx, |fresh, cx| fresh.restore_fresh_window_layout(cx));
    cx.read(|cx| {
        assert!(
            fresh
                .read(cx)
                .workbench
                .read(cx)
                .tool_by_stable_id(EXPLORER_TOOL_ID)
                .is_none()
        );
    });
}

#[gpui::test]
fn fresh_window_with_hidden_tool_restores_nothing(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    open_workspace(&shell, cx);
    place_explorer(&shell, window, ToolPlacement::Dock(DockLocation::Right), cx);
    let hidden = dispatch_product_command(&shell, window, "workbench.toggle-explorer", cx);
    assert_eq!(wait_for_command(hidden, cx), CommandOutcome::Completed);
    let (fresh, _) = empty_product_window(&documents, cx);
    fresh.update(cx, |fresh, cx| fresh.restore_fresh_window_layout(cx));
    cx.read(|cx| {
        assert!(
            fresh
                .read(cx)
                .workbench
                .read(cx)
                .tool_by_stable_id(EXPLORER_TOOL_ID)
                .is_none()
        );
    });
}

#[gpui::test]
fn split_right_tool_restores_as_right_dock(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let (shell, _) = empty_product_window(&documents, cx);
    open_workspace(&shell, cx);
    // Drag the explorer to the pane's right edge: an ordinary tool-only split.
    shell.update(cx, |shell, cx| {
        let id = shell
            .workbench
            .read(cx)
            .tool_by_stable_id(EXPLORER_TOOL_ID)
            .unwrap();
        shell.workbench.update(cx, |workbench, _| {
            let (dock, tab) = workbench.tool_tab(id).unwrap();
            let source = workbench.capture_tab_move(dock, tab).unwrap();
            let (doc_pane, doc_tab) = workbench
                .panes()
                .iter()
                .find_map(|pane| {
                    pane.tabs()
                        .iter()
                        .find(|tab| tab.document_id().is_some())
                        .map(|tab| (pane.id(), tab.id()))
                })
                .unwrap();
            let anchor = workbench.capture_tab_move(doc_pane, doc_tab).unwrap();
            workbench
                .relocate_tab(
                    source,
                    TabMoveDestination::At {
                        anchor,
                        placement: TabMovePlacement::Split(SplitEdge::Right),
                    },
                )
                .unwrap();
        });
        shell.publish_tool_layout(cx);
    });
    // A tool alone in a right split is remembered as the right dock.
    assert_eq!(
        stored(cx).tool(EXPLORER_TOOL_ID).unwrap().location,
        StoredLocation::Right
    );
    let (fresh, _) = empty_product_window(&documents, cx);
    fresh.update(cx, |fresh, cx| fresh.restore_fresh_window_layout(cx));
    cx.read(|cx| {
        let workbench = fresh.read(cx).workbench.read(cx);
        let (pane, _) = workbench.tool_tab(explorer_id(&fresh, cx)).unwrap();
        assert_eq!(workbench.dock_location(pane), Some(DockLocation::Right));
    });
}

#[gpui::test]
fn move_tool_to_dock_places_the_focused_tool_and_publishes_the_default(cx: &mut TestAppContext) {
    use crate::app::commands::dispatch::MOVE_TOOL_TO_DOCK_COMMAND;
    let documents = install_globals(cx);
    let (shell, window) = empty_product_window(&documents, cx);
    // A document has focus, so there is no tool to move.
    assert_eq!(
        wait_for_command(
            dispatch_product_command(&shell, window, MOVE_TOOL_TO_DOCK_COMMAND, cx),
            cx
        ),
        CommandOutcome::Unavailable
    );
    open_workspace(&shell, cx);
    for (label, location) in [
        ("Right", StoredLocation::Right),
        ("Bottom", StoredLocation::Bottom),
        ("Document Area", StoredLocation::DocumentGroup),
        ("Left", StoredLocation::Left),
    ] {
        let id = cx.read(|cx| explorer_id(&shell, cx));
        shell.update(cx, |shell, cx| {
            shell.workbench.update(cx, |workbench, _| {
                let (pane, tab) = workbench.tool_tab(id).unwrap();
                workbench.activate_tab(pane, tab);
            });
        });
        let execution = dispatch_product_command(&shell, window, MOVE_TOOL_TO_DOCK_COMMAND, cx);
        assert_eq!(wait_for_command(execution, cx), CommandOutcome::Completed);
        wait_for_api_condition(cx, |cx| shell.read(cx).transient.is_some());
        cx.simulate_input(window, label);
        cx.simulate_keystrokes(window, "enter");
        cx.run_until_parked();
        let record = stored(cx).tool(EXPLORER_TOOL_ID).unwrap().clone();
        assert_eq!(record.location, location, "{label}");
        assert!(record.visible);
        cx.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            let (pane, _) = workbench.tool_tab(explorer_id(&shell, cx)).unwrap();
            assert_eq!(workbench.focused_pane_id(), Some(pane));
        });
    }
}
