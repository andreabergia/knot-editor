use super::super::{ProductShell, create_untitled_document};
use crate::app::{
    product::tests::{install_globals, product_window},
    workbench::{SplitEdge, TabMoveDestination, TabMovePlacement, TabSurfaceId},
};
use gpui::{
    AnyWindowHandle, AppContext, Entity, Focusable, Modifiers, MouseButton, TestAppContext,
    VisualTestContext, point, px,
};

fn fixture(cx: &mut TestAppContext) -> (Entity<ProductShell>, AnyWindowHandle) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    shell.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, cx| {
            let pane = workbench.focused_pane_id().unwrap();
            for _ in 0..2 {
                workbench
                    .open_tab_for_document(pane, document, model.clone(), cx)
                    .unwrap();
            }
            cx.notify();
        });
        cx.notify();
    });
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.focus_active_surface(window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    (shell, window)
}

fn tab_selector(id: u64) -> &'static str {
    match id {
        1 => "tab-1",
        2 => "tab-2",
        3 => "tab-3",
        4 => "tab-4",
        _ => panic!("unexpected fixture tab"),
    }
}

fn start_drag(cx: &mut VisualTestContext, id: u64) {
    let from = cx.debug_bounds(tab_selector(id)).unwrap().center();
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        from + point(px(12.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.run_until_parked();
    cx.read(|cx| assert!(cx.has_active_drag()));
}

#[gpui::test]
fn tab_bar_reorder_and_release_position_drive_product_transition(cx: &mut TestAppContext) {
    let (shell, window) = fixture(cx);
    let before = cx.read(|cx| shell.read(cx).workbench.read(cx).tab_snapshots());
    let mut visual = VisualTestContext::from_window(window, cx);
    start_drag(&mut visual, before[0].tab_id.value());
    let tab = visual
        .debug_bounds(tab_selector(before[2].tab_id.value()))
        .unwrap();
    let hover = tab.origin + point(px(1.), px(5.));
    visual.simulate_mouse_move(hover, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    shell.read_with(&visual, |shell, _| {
        let state = shell.tab_drag.borrow();
        assert!(matches!(
            state.preview.unwrap().tab,
            TabMoveDestination::At {
                placement: TabMovePlacement::Before,
                ..
            }
        ));
        assert_eq!(state.preview.unwrap().bounds.size.width, px(2.));
    });
    // Release beyond the last midpoint appends, even without another move event.
    visual.simulate_mouse_up(
        point(tab.right() + px(20.), tab.center().y),
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    visual.read(|cx| {
        let shell = shell.read(cx);
        let workbench = shell.workbench.read(cx);
        assert_eq!(
            workbench.tab_snapshots(),
            vec![before[1], before[2], before[0]]
        );
        assert_eq!(
            workbench.focused_pane().unwrap().active_tab_id(),
            before[0].tab_id
        );
        assert!(shell.tab_drag.borrow().source.is_none());
        assert!(!cx.has_active_drag());
    });
}

#[gpui::test]
fn all_edges_split_then_center_move_collapses_source_and_focuses_editor(cx: &mut TestAppContext) {
    for edge in [
        SplitEdge::Left,
        SplitEdge::Right,
        SplitEdge::Top,
        SplitEdge::Bottom,
    ] {
        let (shell, window) = fixture(cx);
        let before = cx.read(|cx| shell.read(cx).workbench.read(cx).tab_snapshots());
        let mut visual = VisualTestContext::from_window(window, cx);
        start_drag(&mut visual, before[0].tab_id.value());
        let pane = visual.debug_bounds("pane-1").unwrap();
        let body_top = visual.debug_bounds("tab-bar-1").unwrap().bottom();
        let destination = match edge {
            SplitEdge::Left => point(pane.left() + px(5.), pane.center().y),
            SplitEdge::Right => point(pane.right() - px(5.), pane.center().y),
            SplitEdge::Top => point(pane.center().x, body_top + px(5.)),
            SplitEdge::Bottom => point(pane.center().x, pane.bottom() - px(5.)),
        };
        visual.simulate_mouse_move(destination, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        visual.read(|cx| {
            let shell = shell.read(cx);
            let preview = shell.tab_drag.borrow().preview.unwrap();
            assert!(matches!(preview.tab, TabMoveDestination::At { placement: TabMovePlacement::Split(actual), .. } if actual == edge));
        });
        visual.simulate_mouse_up(destination, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        let moved = visual.read(|cx| {
            let workbench = shell.read(cx).workbench.read(cx);
            assert_eq!(workbench.panes().len(), 2);
            assert_eq!(workbench.tab_snapshots().len(), 3);
            workbench.focused_pane_id().unwrap()
        });
        assert_ne!(moved, before[0].pane_id);
        start_drag(&mut visual, before[0].tab_id.value());
        let center = visual.debug_bounds("pane-1").unwrap().center();
        visual.simulate_mouse_move(center, MouseButton::Left, Modifiers::default());
        visual.simulate_mouse_up(center, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        let editor = visual.read(|cx| {
            let shell = shell.read(cx);
            let workbench = shell.workbench.read(cx);
            assert_eq!(
                workbench
                    .panes()
                    .iter()
                    .map(|pane| pane.id())
                    .collect::<Vec<_>>(),
                vec![before[0].pane_id]
            );
            assert_eq!(
                workbench.tab_snapshots(),
                vec![before[1], before[2], before[0]]
            );
            workbench
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone()
        });
        visual.update(|window, cx| assert!(editor.focus_handle(cx).is_focused(window)));
    }
}

#[gpui::test]
fn escape_outside_release_and_stale_source_leave_model_unchanged(cx: &mut TestAppContext) {
    let (shell, window) = fixture(cx);
    let before = cx.read(|cx| shell.read(cx).workbench.read(cx).tab_snapshots());
    let mut visual = VisualTestContext::from_window(window, cx);
    for scenario in 0..3 {
        let escape = scenario == 0;
        start_drag(&mut visual, before[0].tab_id.value());
        let pane = visual.debug_bounds("pane-1").unwrap();
        visual.simulate_mouse_move(pane.center(), MouseButton::Left, Modifiers::default());
        if escape {
            visual.simulate_keystrokes("escape");
        }
        let destination = match scenario {
            0 => pane.center(),
            1 => point(px(-30.), px(-30.)),
            _ => point(pane.center().x, pane.top() - px(5.)),
        };
        visual.simulate_mouse_up(destination, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        visual.read(|cx| {
            let shell = shell.read(cx);
            assert_eq!(shell.workbench.read(cx).tab_snapshots(), before);
            assert!(shell.tab_drag.borrow().source.is_none());
            assert!(shell.tab_drag.borrow().preview.is_none());
            assert!(!cx.has_active_drag());
        });
    }
    start_drag(&mut visual, before[0].tab_id.value());
    // Replacing the workbench invalidates the captured source despite matching local IDs.
    shell.update(&mut visual, |shell, cx| {
        let old = shell.workbench.read(cx);
        let document = old
            .focused_pane()
            .unwrap()
            .active_tab()
            .document_id()
            .unwrap();
        let model = ProductShell::documents(cx)
            .read(cx)
            .get(document)
            .unwrap()
            .model()
            .clone();
        shell.workbench =
            cx.new(|cx| crate::app::workbench::Workbench::new_for_document(document, model, cx));
        cx.notify();
    });
    visual.run_until_parked();
    let pane = visual.debug_bounds("pane-1").unwrap();
    visual.simulate_mouse_up(
        point(pane.left() + px(5.), pane.center().y),
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    visual.read(|cx| assert_eq!(shell.read(cx).workbench.read(cx).tab_snapshots().len(), 1));
}

#[gpui::test]
fn terminal_drag_preserves_live_view_session_and_attachment(cx: &mut TestAppContext) {
    let (shell, window) = fixture(cx);
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            assert!(shell.new_terminal_in_pane(pane, window, cx));
        });
    })
    .unwrap();
    cx.run_until_parked();
    let (tab, view, session) = cx.read(|cx| {
        let pane = shell.read(cx).workbench.read(cx).focused_pane().unwrap();
        let tab = pane.active_tab();
        let TabSurfaceId::Terminal(id) = tab.surface_id() else {
            panic!("expected terminal")
        };
        (
            tab.id(),
            tab.terminal_view().unwrap().clone(),
            cx.global::<super::super::ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&id]
                .clone(),
        )
    });
    let mut visual = VisualTestContext::from_window(window, cx);
    start_drag(&mut visual, tab.value());
    let pane = visual.debug_bounds("pane-1").unwrap();
    let destination = point(pane.right() - px(5.), pane.center().y);
    visual.simulate_mouse_move(destination, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(destination, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    visual.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        assert_eq!(workbench.panes().len(), 2);
        assert_eq!(
            workbench
                .focused_pane()
                .unwrap()
                .active_tab()
                .terminal_view(),
            Some(&view)
        );
        assert!(view.read(cx).is_attached_to(&session));
    });
    visual.update(|window, cx| assert!(view.focus_handle(cx).is_focused(window)));
}
