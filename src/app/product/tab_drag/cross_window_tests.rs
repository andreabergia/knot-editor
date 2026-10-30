use super::super::{
    ApplicationProtectedClosure, ApplicationTerminalSessions, ApplicationWorkbenches, ProductShell,
    create_untitled_document, open_tab_tear_out_window,
};
use super::tests::start_drag;
use super::{ApplicationTabDrag, DropAction, WindowGeometry, commit_drop_with, content_bounds};
use crate::app::{
    product::tests::install_globals,
    workbench::{SplitEdge, TabMoveDestination, TabMovePlacement, TabSurfaceId, Workbench},
};
use gpui::{
    AnyWindowHandle, AppContext, Bounds, Entity, Focusable, Modifiers, MouseButton, Pixels, Point,
    TestAppContext, VisualTestContext, WindowBounds, WindowOptions, point, px, size,
};

fn pair(
    cx: &mut TestAppContext,
    source_tabs: usize,
) -> (
    Entity<ProductShell>,
    AnyWindowHandle,
    Entity<ProductShell>,
    AnyWindowHandle,
) {
    let documents = install_globals(cx);
    let shells = cx.update(|cx| {
        [(-700., 100., source_tabs), (40., 160., 1)]
            .into_iter()
            .map(|(x, y, count)| {
                let document = create_untitled_document(&documents, cx);
                let model = documents.read(cx).get(document).unwrap().model().clone();
                model.update(cx, |model, _| {
                    model
                        .replace(0..0, &"dirty document\n".repeat(200))
                        .unwrap()
                });
                let handle = cx
                    .open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(x), px(y)),
                                size(px(640.), px(520.)),
                            ))),
                            ..Default::default()
                        },
                        |window, cx| {
                            let workbench = cx.new(|cx| {
                                let mut workbench =
                                    Workbench::new_for_document(document, model.clone(), cx);
                                let pane = workbench.focused_pane_id().unwrap();
                                for _ in 1..count {
                                    workbench
                                        .open_tab_for_document(pane, document, model.clone(), cx)
                                        .unwrap();
                                }
                                workbench
                            });
                            cx.global::<ApplicationWorkbenches>()
                                .register(&workbench, window.window_handle());
                            let shell = cx.new(|_| ProductShell::new(workbench));
                            shell.read(cx).focus_active_surface(window, cx);
                            shell
                        },
                    )
                    .unwrap();
                (
                    cx.update_window(handle.into(), |_, window, _| {
                        window.root::<ProductShell>().flatten().unwrap()
                    })
                    .unwrap(),
                    handle.into(),
                )
            })
            .collect::<Vec<_>>()
    });
    cx.run_until_parked();
    (
        shells[0].0.clone(),
        shells[0].1,
        shells[1].0.clone(),
        shells[1].1,
    )
}

fn offset(
    cx: &mut TestAppContext,
    source: AnyWindowHandle,
    destination: AnyWindowHandle,
) -> Point<Pixels> {
    let from = cx
        .update_window(source, |_, window, _| content_bounds(window).origin)
        .unwrap();
    let to = cx
        .update_window(destination, |_, window, _| content_bounds(window).origin)
        .unwrap();
    to - from
}

fn assert_cleared(cx: &TestAppContext) {
    cx.read(|cx| {
        let coordinator = cx.global::<ApplicationTabDrag>().0.borrow();
        assert!(coordinator.active.is_none());
        assert!(coordinator.pointer.is_none());
        assert!(coordinator.pointer_window.is_none());
        assert!(
            coordinator
                .windows
                .iter()
                .all(|window| window.state.borrow().preview.is_none())
        );
        assert!(!cx.has_active_drag());
    });
}

#[gpui::test]
fn source_delivery_moves_dirty_documents_to_every_destination_target(cx: &mut TestAppContext) {
    for placement in [
        TabMovePlacement::Before,
        TabMovePlacement::End,
        TabMovePlacement::Split(SplitEdge::Left),
        TabMovePlacement::Split(SplitEdge::Right),
        TabMovePlacement::Split(SplitEdge::Top),
        TabMovePlacement::Split(SplitEdge::Bottom),
    ] {
        let (source, source_window, destination, destination_window) = pair(cx, 3);
        let (old, editor, document) = cx.read(|cx| {
            let workbench = source.read(cx).workbench.read(cx);
            let tab = &workbench.focused_pane().unwrap().tabs()[0];
            (
                workbench.tab_snapshots(),
                tab.editor().unwrap().clone(),
                tab.document_id().unwrap(),
            )
        });
        let target = {
            let mut destination_cx = VisualTestContext::from_window(destination_window, cx);
            let pane = destination_cx.debug_bounds("pane-1").unwrap();
            let bar = destination_cx.debug_bounds("tab-bar-1").unwrap();
            let tab = destination_cx.debug_bounds("tab-1").unwrap();
            match placement {
                TabMovePlacement::Before => point(tab.left() + px(1.), bar.center().y),
                TabMovePlacement::End => pane.center(),
                TabMovePlacement::Split(SplitEdge::Left) => {
                    point(pane.left() + px(4.), pane.center().y)
                }
                TabMovePlacement::Split(SplitEdge::Right) => {
                    point(pane.right() - px(4.), pane.center().y)
                }
                TabMovePlacement::Split(SplitEdge::Top) => {
                    point(pane.center().x, bar.bottom() + px(4.))
                }
                TabMovePlacement::Split(SplitEdge::Bottom) => {
                    point(pane.center().x, pane.bottom() - px(4.))
                }
            }
        };
        let offset = offset(cx, source_window, destination_window);
        let mut visual = VisualTestContext::from_window(source_window, cx);
        start_drag(&mut visual, old[0].tab_id.value());
        visual.simulate_mouse_move(target + offset, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        visual.read(|cx| {
            let preview = destination.read(cx).tab_drag.borrow().preview.unwrap();
            assert!(matches!(preview.tab, TabMoveDestination::At { placement: actual, .. } if actual == placement));
            assert_eq!(source.read(cx).workbench.read(cx).tab_snapshots(), old);
        });
        visual.simulate_mouse_up(target + offset, MouseButton::Left, Modifiers::default());
        visual.run_until_parked();
        visual.read(|cx| {
            assert_eq!(source.read(cx).workbench.read(cx).tab_snapshots(), old[1..]);
            let workbench = destination.read(cx).workbench.read(cx);
            let active = workbench.focused_pane().unwrap().active_tab();
            assert_eq!(active.editor(), Some(&editor));
            assert_eq!(active.document_id(), Some(document));
            assert_eq!(workbench.tab_snapshots().len(), 2);
            assert_eq!(
                workbench.panes().len(),
                if matches!(placement, TabMovePlacement::Split(_)) {
                    2
                } else {
                    1
                }
            );
            if placement == TabMovePlacement::Before {
                assert_eq!(
                    workbench.focused_pane().unwrap().tabs()[0].editor(),
                    Some(&editor)
                );
            }
            assert_eq!(
                cx.global::<ApplicationWorkbenches>()
                    .view_count(document, cx),
                3
            );
            assert!(
                ProductShell::documents(cx)
                    .read(cx)
                    .get(document)
                    .unwrap()
                    .is_dirty(cx)
            );
            assert!(!*cx.global::<ApplicationProtectedClosure>().0.borrow());
        });
        visual
            .update_window(destination_window, |_, window, cx| {
                assert!(editor.focus_handle(cx).is_focused(window))
            })
            .unwrap();
        drop(visual);
        assert_cleared(cx);
        for window in [source_window, destination_window] {
            cx.update_window(window, |_, window, _| window.remove_window())
                .unwrap();
        }
        cx.run_until_parked();
    }
}

#[gpui::test]
fn destination_preview_tracks_source_pointer_and_either_receiver_can_drop_or_cancel(
    cx: &mut TestAppContext,
) {
    let (source, source_window, destination, destination_window) = pair(cx, 3);
    let offset = offset(cx, source_window, destination_window);
    let target = VisualTestContext::from_window(destination_window, cx)
        .debug_bounds("pane-1")
        .unwrap()
        .center();
    let mut visual = VisualTestContext::from_window(source_window, cx);
    let bounds = visual.debug_bounds("tab-1").unwrap();
    let grab_offset = bounds.center() - bounds.origin + point(px(12.), px(0.));
    start_drag(&mut visual, 1);
    visual.simulate_mouse_move(target + offset, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    let first = VisualTestContext::from_window(destination_window, &visual)
        .debug_bounds("tab-drag-label")
        .unwrap();
    let error = first.origin - (target - grab_offset);
    assert!(error.x.abs() <= px(0.5) && error.y.abs() <= px(0.5));
    let delta = point(px(10.), px(8.));
    visual.simulate_mouse_move(
        target + offset + delta,
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    let second = VisualTestContext::from_window(destination_window, &visual)
        .debug_bounds("tab-drag-label")
        .unwrap();
    assert_eq!(second.origin - first.origin, delta);
    // Escape is delivered through destination focus while movement stays in the source.
    let mut destination_visual = VisualTestContext::from_window(destination_window, &visual);
    destination_visual.simulate_keystrokes("escape");
    destination_visual.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
    destination_visual.run_until_parked();
    assert_cleared(&destination_visual);
    destination_visual.read(|cx| {
        assert_eq!(source.read(cx).workbench.read(cx).tab_snapshots().len(), 3);
        assert_eq!(
            destination
                .read(cx)
                .workbench
                .read(cx)
                .tab_snapshots()
                .len(),
            1
        );
    });
    drop(destination_visual);
    start_drag(&mut visual, 1);
    let mut destination_visual = VisualTestContext::from_window(destination_window, &visual);
    destination_visual.simulate_mouse_move(target, MouseButton::Left, Modifiers::default());
    destination_visual.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
    destination_visual.run_until_parked();
    destination_visual.read(|cx| {
        assert_eq!(source.read(cx).workbench.read(cx).tab_snapshots().len(), 2);
        assert_eq!(
            destination
                .read(cx)
                .workbench
                .read(cx)
                .tab_snapshots()
                .len(),
            2
        );
    });
    drop(destination_visual);
    drop(visual);
    assert_cleared(cx);
    // Reverse direction still uses source-delivered coordinates.
    let moved_id = cx.read(|cx| {
        destination
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab_id()
            .value()
    });
    let target = VisualTestContext::from_window(source_window, cx)
        .debug_bounds("pane-1")
        .unwrap()
        .center();
    let mut visual = VisualTestContext::from_window(destination_window, cx);
    start_drag(&mut visual, moved_id);
    visual.simulate_mouse_up(target - offset, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    visual.read(|cx| {
        assert_eq!(source.read(cx).workbench.read(cx).tab_snapshots().len(), 3);
        assert_eq!(
            destination
                .read(cx)
                .workbench
                .read(cx)
                .tab_snapshots()
                .len(),
            1
        );
    });
}

#[gpui::test]
fn desktop_release_moves_dirty_last_view_and_replaces_source(cx: &mut TestAppContext) {
    let (source, source_window, _, _) = pair(cx, 1);
    let (document, editor, command) = cx
        .update_window(source_window, |_, window, cx| {
            source.update(cx, |shell, cx| {
                let tab = shell
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .active_tab();
                let editor = tab.editor().unwrap().clone();
                let document = tab.document_id().unwrap();
                editor.update(cx, |editor, cx| {
                    editor.restore_view_state(
                        crate::app::editor::EditorViewState {
                            anchor: 0,
                            caret: 0,
                            top: 0,
                            scroll_x: 12.,
                        },
                        cx,
                    )
                });
                (
                    document,
                    editor,
                    shell.capture_command_target(window, cx).unwrap(),
                )
            })
        })
        .unwrap();
    let old_windows = cx.windows();
    let mut visual = VisualTestContext::from_window(source_window, cx);
    start_drag(&mut visual, 1);
    visual.simulate_mouse_up(
        point(px(1600.), px(700.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    let new_window = *visual
        .windows()
        .iter()
        .find(|window| !old_windows.contains(window))
        .unwrap();
    visual
        .update_window(new_window, |_, window, cx| {
            let shell = window.root::<ProductShell>().flatten().unwrap();
            let workbench = shell.read(cx).workbench.read(cx);
            assert_eq!(
                workbench.focused_pane().unwrap().active_tab().editor(),
                Some(&editor)
            );
            assert!(editor.focus_handle(cx).is_focused(window));
            assert_eq!(
                editor.read(cx).view_state(cx),
                crate::app::editor::EditorViewState {
                    anchor: 0,
                    caret: 0,
                    top: 0,
                    scroll_x: 12.
                }
            );
            assert_eq!(
                cx.global::<ApplicationWorkbenches>()
                    .view_count(document, cx),
                1
            );
            assert!(
                ProductShell::documents(cx)
                    .read(cx)
                    .get(document)
                    .unwrap()
                    .is_dirty(cx)
            );
            assert_ne!(
                source
                    .read(cx)
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .active_tab()
                    .document_id(),
                Some(document)
            );
            assert!(command.validate_tab(cx).is_err());
        })
        .unwrap();
    drop(visual);
    assert_cleared(cx);
}

#[gpui::test]
fn failed_tear_out_and_stale_or_closed_drop_preserve_source(cx: &mut TestAppContext) {
    let (source, source_window, destination, destination_window) = pair(cx, 1);
    let original = cx.read(|cx| source.read(cx).workbench.read(cx).tab_snapshots());
    let mut visual = VisualTestContext::from_window(source_window, cx);
    start_drag(&mut visual, 1);
    visual.update(|_, cx| {
        let shared = cx.global::<ApplicationTabDrag>().0.clone();
        let active = shared.borrow().active.clone().unwrap();
        shared.borrow_mut().clear();
        commit_drop_with(
            active,
            DropAction::TearOut(point(px(1200.), px(900.))),
            cx,
            |_, _| anyhow::bail!("test window creation failure"),
        );
    });
    visual.read(|cx| {
        assert_eq!(cx.windows().len(), 2);
        assert_eq!(source.read(cx).workbench.read(cx).tab_snapshots(), original);
    });
    // Start a fresh gpui drag after consuming the failed dispatch.
    visual.update(|window, cx| {
        cx.stop_active_drag(window);
    });
    for close in [false, true] {
        start_drag(&mut visual, 1);
        visual.update(|window, cx| {
            let shared = cx.global::<ApplicationTabDrag>().0.clone();
            let active = shared.borrow().active.clone().unwrap();
            let pane = destination
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap();
            let destination_capture = destination
                .read(cx)
                .capture_tab_relocation(pane.id(), pane.active_tab_id(), cx)
                .unwrap();
            let action = DropAction::Target(super::TabRelocationDestination {
                window: destination_window,
                workbench: destination_capture.workbench,
                tab: TabMoveDestination::At {
                    anchor: destination_capture.tab,
                    placement: TabMovePlacement::End,
                },
            });
            shared.borrow_mut().clear();
            if close {
                cx.update_window(destination_window, |_, window, _| window.remove_window())
                    .unwrap();
            } else {
                destination.update(cx, |shell, cx| {
                    let tab = shell
                        .workbench
                        .read(cx)
                        .focused_pane()
                        .unwrap()
                        .active_tab();
                    let document = tab.document_id().unwrap();
                    let tab_id = tab.id();
                    let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
                    let model = ProductShell::documents(cx)
                        .read(cx)
                        .get(document)
                        .unwrap()
                        .model()
                        .clone();
                    shell.workbench.update(cx, |workbench, cx| {
                        assert!(workbench.replace_tab_document(pane, tab_id, document, model, cx));
                    });
                });
            }
            // Test the same deferred commit used after native release.
            cx.stop_active_drag(window);
            cx.defer(move |cx| commit_drop_with(active, action, cx, open_tab_tear_out_window));
        });
        visual.run_until_parked();
        visual.read(|cx| assert_eq!(source.read(cx).workbench.read(cx).tab_snapshots(), original));
    }
}

#[gpui::test]
fn live_terminal_cross_window_and_desktop_drops_keep_session_and_sole_attachment(
    cx: &mut TestAppContext,
) {
    let (source, source_window, destination, destination_window) = pair(cx, 1);
    cx.update_window(source_window, |_, window, cx| {
        source.update(cx, |shell, cx| {
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            assert!(shell.new_terminal_in_pane(pane, None, window, cx).is_some());
        })
    })
    .unwrap();
    cx.run_until_parked();
    let (id, original_view, session, grid) = cx.read(|cx| {
        let tab = source
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab();
        let TabSurfaceId::Terminal(id) = tab.surface_id() else {
            panic!("expected terminal")
        };
        let session = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions[&id]
            .clone();
        (
            id,
            tab.terminal_view().unwrap().clone(),
            session.clone(),
            session.read(cx).terminal().unwrap().clone(),
        )
    });
    let target = VisualTestContext::from_window(destination_window, cx)
        .debug_bounds("pane-1")
        .unwrap()
        .center();
    let offset = offset(cx, source_window, destination_window);
    let mut visual = VisualTestContext::from_window(source_window, cx);
    start_drag(&mut visual, 2);
    visual.simulate_mouse_up(target + offset, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    let moved_view = visual.read(|cx| {
        let view = destination
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .terminal_view()
            .unwrap()
            .clone();
        assert_ne!(view, original_view);
        assert!(!original_view.read(cx).is_attached_to(&session));
        assert!(view.read(cx).is_attached_to(&session));
        assert_eq!(
            session.read(cx).status(),
            crate::app::terminal::session::TerminalStatus::Running
        );
        assert!(std::sync::Arc::ptr_eq(
            session.read(cx).terminal().unwrap(),
            &grid
        ));
        view
    });
    drop(visual);
    let old_windows = cx.windows();
    let mut visual = VisualTestContext::from_window(destination_window, cx);
    start_drag(&mut visual, 2);
    visual.simulate_mouse_up(
        point(px(1000.), px(700.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    visual.run_until_parked();
    let new_window = *visual
        .windows()
        .iter()
        .find(|window| !old_windows.contains(window))
        .unwrap();
    visual
        .update_window(new_window, |_, window, cx| {
            let shell = window.root::<ProductShell>().flatten().unwrap();
            let view = shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .terminal_view()
                .unwrap()
                .clone();
            assert_ne!(view, moved_view);
            assert!(!moved_view.read(cx).is_attached_to(&session));
            assert!(view.read(cx).is_attached_to(&session));
            assert!(view.focus_handle(cx).is_focused(window));
            assert_eq!(
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow()
                    .sessions[&id],
                session
            );
            assert_eq!(
                session.read(cx).status(),
                crate::app::terminal::session::TerminalStatus::Running
            );
            assert!(std::sync::Arc::ptr_eq(
                session.read(cx).terminal().unwrap(),
                &grid
            ));
            session.update(cx, |session, _| assert!(session.attach().is_none()));
        })
        .unwrap();
    drop(visual);
    assert_cleared(cx);
}

#[gpui::test]
fn source_window_closure_cancels_shared_drag_and_releases_geometry(cx: &mut TestAppContext) {
    let (_, source_window, destination, destination_window) = pair(cx, 1);
    let mut visual = VisualTestContext::from_window(source_window, cx);
    start_drag(&mut visual, 1);
    drop(visual);
    cx.update_window(source_window, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert_cleared(cx);
    cx.read(|cx| {
        let coordinator = cx.global::<ApplicationTabDrag>().0.borrow();
        assert!(
            coordinator
                .windows
                .iter()
                .all(|entry| entry.window != source_window)
        );
        assert_eq!(
            destination
                .read(cx)
                .workbench
                .read(cx)
                .tab_snapshots()
                .len(),
            1
        );
    });
    let mut visual = VisualTestContext::from_window(destination_window, cx);
    start_drag(&mut visual, 1);
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    drop(visual);
    assert_cleared(cx);
}

#[gpui::test]
fn hit_testing_respects_title_bar_occlusion_and_global_content_offsets(cx: &mut TestAppContext) {
    let (source, source_window, _, destination_window) = pair(cx, 1);
    let local = VisualTestContext::from_window(destination_window, cx)
        .debug_bounds("pane-1")
        .unwrap()
        .center();
    // Synthetic native frames include differing title bars and a negative display origin.
    let content = Bounds::new(point(px(-1500.), px(-600.)), size(px(640.), px(520.)));
    let frame = Bounds::new(
        content.origin - point(px(0.), px(32.)),
        size(px(640.), px(552.)),
    );
    cx.read(|cx| {
        let shell = source.read(cx);
        let pane = shell.workbench.read(cx).focused_pane().unwrap();
        let document_source = shell
            .capture_tab_relocation(pane.id(), pane.active_tab_id(), cx)
            .unwrap();
        let state = cx.global::<ApplicationTabDrag>().0.borrow();
        let pointer = content.origin + local;
        let geometry = vec![WindowGeometry { window: destination_window, frame, content }];
        assert!(matches!(state.action_at(pointer, &geometry, &document_source), DropAction::Target(destination) if destination.window == destination_window));
        let title = frame.origin + point(px(20.), px(10.));
        assert!(matches!(state.action_at(title, &geometry, &document_source), DropAction::Invalid));
        // A front window's non-content frame prevents dropping into the pane behind it.
        let mut overlapping = vec![WindowGeometry { window: source_window, frame: Bounds::new(pointer - point(px(5.), px(5.)), size(px(100.), px(100.))), content: Bounds::new(pointer + point(px(0.), px(40.)), size(px(100.), px(60.))) }];
        overlapping.extend(geometry);
        assert!(matches!(state.action_at(pointer, &overlapping, &document_source), DropAction::Invalid));
        assert!(matches!(state.action_at(point(px(-3000.), px(-3000.)), &overlapping, &document_source), DropAction::TearOut(_)));
    });
}

#[gpui::test]
fn destination_overlay_and_desktop_escape_do_not_move_or_create_windows(cx: &mut TestAppContext) {
    let (source, source_window, destination, destination_window) = pair(cx, 1);
    let target = VisualTestContext::from_window(destination_window, cx)
        .debug_bounds("pane-1")
        .unwrap()
        .center();
    let offset = offset(cx, source_window, destination_window);
    destination.update(cx, |shell, cx| {
        shell.extension_report_open = true;
        cx.notify();
    });
    cx.run_until_parked();
    let mut visual = VisualTestContext::from_window(source_window, cx);
    start_drag(&mut visual, 1);
    visual.simulate_mouse_move(target + offset, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    visual.read(|cx| assert!(destination.read(cx).tab_drag.borrow().preview.is_none()));
    visual.simulate_mouse_up(target + offset, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    visual.read(|cx| {
        assert_eq!(source.read(cx).workbench.read(cx).tab_snapshots().len(), 1);
        assert_eq!(
            destination
                .read(cx)
                .workbench
                .read(cx)
                .tab_snapshots()
                .len(),
            1
        );
    });
    start_drag(&mut visual, 1);
    let desktop = point(px(1800.), px(700.));
    visual.simulate_mouse_move(desktop, MouseButton::Left, Modifiers::default());
    visual.simulate_keystrokes("escape");
    visual.simulate_mouse_up(desktop, MouseButton::Left, Modifiers::default());
    visual.run_until_parked();
    visual.read(|cx| assert_eq!(cx.windows().len(), 2));
    drop(visual);
    assert_cleared(cx);
}

#[gpui::test]
fn closing_final_source_window_clears_framework_drag_before_reopening(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = crate::app::product::tests::product_window(document, model, cx);
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    cx.run_until_parked();
    let mut visual = VisualTestContext::from_window(window, cx);
    start_drag(&mut visual, 1);
    drop(visual);
    cx.update(|cx| {
        ProductShell::begin_protected_close(
            super::super::ProtectedCloseKind::Window,
            target,
            None,
            cx,
        );
    });
    cx.run_until_parked();
    assert!(cx.windows().is_empty());
    assert_cleared(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = crate::app::product::tests::product_window(document, model, cx);
    cx.update_window(window, |_, window, cx| {
        shell.read(cx).focus_active_surface(window, cx)
    })
    .unwrap();
    cx.run_until_parked();
    let mut visual = VisualTestContext::from_window(window, cx);
    start_drag(&mut visual, 1);
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    drop(visual);
    assert_cleared(cx);
}
