use super::super::{
    ApplicationProtectedClosure, ApplicationTerminalSessions, ApplicationWorkbenches, ProductShell,
    create_untitled_document, open_tab_transfer_window,
};
use super::{TabRelocationDestination, TabRelocationSource};
use crate::app::{
    product::tests::{install_globals, product_window},
    workbench::{SplitEdge, TabMoveDestination, TabMovePlacement, TabSurfaceId},
};
use crate::host::protocol::CommandOutcome;
use gpui::{AnyWindowHandle, App, AppContext, Entity, Focusable, TestAppContext};
use std::sync::Arc;

fn capture(shell: &Entity<ProductShell>, cx: &App) -> TabRelocationSource {
    let shell = shell.read(cx);
    let pane = shell.workbench.read(cx).focused_pane().unwrap();
    shell
        .capture_tab_relocation(pane.id(), pane.active_tab_id(), cx)
        .unwrap()
}

fn target(
    shell: &Entity<ProductShell>,
    window: AnyWindowHandle,
    placement: TabMovePlacement,
    cx: &App,
) -> TabRelocationDestination {
    let source = capture(shell, cx);
    TabRelocationDestination {
        window,
        workbench: source.workbench,
        tab: TabMoveDestination::At {
            anchor: source.tab,
            placement,
        },
    }
}

#[gpui::test]
fn dirty_last_document_view_moves_to_new_window_without_closure(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let text = format!("first\nsecond\nthird\n{}", "line\n".repeat(200));
    model.update(cx, |model, _| model.replace(0..0, &text).unwrap());
    let (source, source_window) = product_window(document, model.clone(), cx);
    let (origin, command, editor) = cx
        .update_window(source_window, |_, window, cx| {
            source.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                let command = shell.capture_command_target(window, cx).unwrap();
                let editor = shell
                    .workbench
                    .read(cx)
                    .focused_pane()
                    .unwrap()
                    .active_tab()
                    .editor()
                    .unwrap()
                    .clone();
                editor.update(cx, |editor, _| {
                    editor.set_test_presentation_state(1, 2, Some((0, 1)), 12.)
                });
                (
                    shell
                        .capture_tab_relocation(command.pane, command.tab, cx)
                        .unwrap(),
                    command,
                    editor,
                )
            })
        })
        .unwrap();
    let outcome = cx
        .update_window(source_window, |_, window, cx| {
            source.update(cx, |shell, cx| {
                shell.relocate_tab_to_new_window_with(&origin, window, cx, open_tab_transfer_window)
            })
        })
        .unwrap();
    assert_eq!(outcome, CommandOutcome::Completed);
    let destination_window = *cx
        .windows()
        .iter()
        .find(|window| **window != source_window)
        .unwrap();
    let destination = cx
        .update_window(destination_window, |_, window, _| {
            window.root::<ProductShell>().flatten().unwrap()
        })
        .unwrap();
    cx.read(|cx| {
        let moved = destination
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab();
        assert_eq!(moved.document_id(), Some(document));
        assert_eq!(moved.editor(), Some(&editor));
        assert_eq!(
            editor.read(cx).test_presentation_state(),
            (1, 2, Some((0, 1)), 12.)
        );
        assert_eq!(
            cx.global::<ApplicationWorkbenches>()
                .view_count(document, cx),
            1
        );
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
        assert_eq!(model.read(cx).text(), text);
        assert!(!*cx.global::<ApplicationProtectedClosure>().0.borrow());
        assert_eq!(command.validate_tab(cx), Err(CommandOutcome::InvalidTarget));
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
    });
    cx.update_window(destination_window, |_, window, cx| {
        assert!(editor.focus_handle(cx).is_focused(window))
    })
    .unwrap();
}

#[gpui::test]
fn relocation_into_existing_document_pane_keeps_application_view_counts(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (source, source_window) = product_window(document, model.clone(), cx);
    let (destination, destination_window) = product_window(document, model, cx);
    let origin = cx.read(|cx| capture(&source, cx));
    let target = cx.read(|cx| {
        target(
            &destination,
            destination_window,
            TabMovePlacement::Before,
            cx,
        )
    });
    let original = cx.read(|cx| {
        source
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone()
    });
    let outcome = cx
        .update_window(source_window, |_, window, cx| {
            source.update(cx, |shell, cx| {
                shell.relocate_tab_to(&origin, &target, window, cx)
            })
        })
        .unwrap();
    assert_eq!(outcome, CommandOutcome::Completed);
    cx.read(|cx| {
        assert_eq!(
            cx.global::<ApplicationWorkbenches>()
                .view_count(document, cx),
            2
        );
        let pane = destination
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap();
        assert_eq!(pane.tabs()[0].editor(), Some(&original));
        assert_eq!(pane.active_tab_id(), pane.tabs()[0].id());
        assert_eq!(pane.tabs().len(), 2);
    });
}

#[gpui::test]
fn abandoning_an_inactive_tab_capture_keeps_focus_and_document_ownership(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (source, source_window) = product_window(document, model.clone(), cx);
    let (destination, destination_window) = product_window(document, model.clone(), cx);
    let inactive = cx.read(|cx| capture(&source, cx));
    source.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, cx| {
            workbench
                .open_tab_for_document(inactive.tab.tab.pane_id, document, model, cx)
                .unwrap();
        })
    });
    let (before, active) = cx
        .update_window(source_window, |_, window, cx| {
            source.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                (
                    shell.workbench.read(cx).tab_snapshots(),
                    shell
                        .workbench
                        .read(cx)
                        .focused_pane()
                        .unwrap()
                        .active_tab_id(),
                )
            })
        })
        .unwrap();
    let attempt = cx.read(|cx| {
        (
            source
                .read(cx)
                .capture_tab_relocation(inactive.tab.tab.pane_id, inactive.tab.tab.tab_id, cx)
                .unwrap(),
            target(&destination, destination_window, TabMovePlacement::End, cx),
        )
    });
    drop(attempt);
    cx.run_until_parked();
    cx.read(|cx| {
        let workbench = source.read(cx).workbench.read(cx);
        assert_eq!(workbench.tab_snapshots(), before);
        assert_eq!(workbench.focused_pane().unwrap().active_tab_id(), active);
        assert_eq!(
            cx.global::<ApplicationWorkbenches>()
                .view_count(document, cx),
            3
        );
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
        assert!(!*cx.global::<ApplicationProtectedClosure>().0.borrow());
        assert_eq!(cx.windows().len(), 2);
    });
}

#[gpui::test]
fn failed_creation_and_stale_or_closed_destination_leave_source_intact(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (source, source_window) = product_window(document, model.clone(), cx);
    let (destination, destination_window) = product_window(document, model.clone(), cx);
    let origin = cx.read(|cx| capture(&source, cx));
    let target = cx.read(|cx| target(&destination, destination_window, TabMovePlacement::End, cx));
    let before = cx.read(|cx| source.read(cx).workbench.read(cx).tab_snapshots());
    let outcome = cx
        .update_window(source_window, |_, window, cx| {
            source.update(cx, |shell, cx| {
                shell.relocate_tab_to_new_window_with(&origin, window, cx, |_| {
                    anyhow::bail!("creation failed")
                })
            })
        })
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::HandlerFailure { .. }));
    destination.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, cx| {
            let pane = workbench.focused_pane().unwrap();
            workbench.replace_tab_document(pane.id(), pane.active_tab_id(), document, model, cx)
        })
    });
    let outcome = cx
        .update_window(source_window, |_, window, cx| {
            source.update(cx, |shell, cx| {
                shell.relocate_tab_to(&origin, &target, window, cx)
            })
        })
        .unwrap();
    assert_eq!(outcome, CommandOutcome::InvalidTarget);
    let target =
        cx.read(|cx| self::target(&destination, destination_window, TabMovePlacement::End, cx));
    cx.update_window(destination_window, |_, window, _| window.remove_window())
        .unwrap();
    let outcome = cx
        .update_window(source_window, |_, window, cx| {
            source.update(cx, |shell, cx| {
                shell.relocate_tab_to(&origin, &target, window, cx)
            })
        })
        .unwrap();
    assert_eq!(outcome, CommandOutcome::InvalidTarget);
    cx.read(|cx| {
        assert_eq!(source.read(cx).workbench.read(cx).tab_snapshots(), before);
        assert!(source.read(cx).relocation_is_live(&origin, cx));
    });
}

#[gpui::test]
fn source_replaced_during_window_creation_rejects_move_and_cleans_destination(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (source, source_window) = product_window(document, model.clone(), cx);
    let origin = cx.read(|cx| capture(&source, cx));
    let workbench = cx.read(|cx| source.read(cx).workbench.clone());
    let outcome = cx
        .update_window(source_window, |_, window, cx| {
            source.update(cx, |shell, cx| {
                shell.relocate_tab_to_new_window_with(&origin, window, cx, |cx| {
                    let destination = open_tab_transfer_window(cx)?;
                    workbench.update(cx, |workbench, cx| {
                        workbench.replace_tab_document(
                            origin.tab.tab.pane_id,
                            origin.tab.tab.tab_id,
                            document,
                            model,
                            cx,
                        )
                    });
                    Ok(destination)
                })
            })
        })
        .unwrap();
    assert_eq!(outcome, CommandOutcome::InvalidTarget);
    assert!(cx.windows() == vec![source_window]);
    cx.read(|cx| assert_eq!(source.read(cx).workbench.read(cx).view_count(document), 1));
}

#[gpui::test]
fn terminal_moves_within_window_then_hands_off_into_existing_window(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (source, source_window) = product_window(document, model.clone(), cx);
    let (destination, destination_window) = product_window(document, model, cx);
    cx.update_window(source_window, |_, window, cx| {
        source.update(cx, |shell, cx| {
            let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
            assert!(shell.new_terminal_in_pane(pane, window, cx));
        })
    })
    .unwrap();
    source.update(cx, |shell, cx| {
        shell.workbench.update(cx, |workbench, _| {
            let pane = workbench.focused_pane().unwrap();
            let pane_id = pane.id();
            let document_tab = pane
                .tabs()
                .iter()
                .find(|tab| tab.document_id() == Some(document))
                .unwrap()
                .id();
            workbench
                .request_close_tab_with_state(pane_id, document_tab, document, false, 2)
                .unwrap();
        })
    });
    let origin = cx.read(|cx| capture(&source, cx));
    let same_window = cx.read(|cx| {
        target(
            &source,
            source_window,
            TabMovePlacement::Split(SplitEdge::Right),
            cx,
        )
    });
    let source_view = cx.read(|cx| {
        source
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .terminal_view()
            .unwrap()
            .clone()
    });
    let TabSurfaceId::Terminal(id) = origin.tab.tab.surface else {
        panic!("expected terminal");
    };
    let session = cx.read(|cx| {
        cx.global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions[&id]
            .clone()
    });
    let grid = cx.read(|cx| session.read(cx).terminal().unwrap().clone());
    assert_eq!(
        cx.update_window(source_window, |_, window, cx| source.update(
            cx,
            |shell, cx| shell.relocate_tab_to(&origin, &same_window, window, cx)
        ))
        .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            source
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .terminal_view(),
            Some(&source_view)
        );
        assert!(source_view.read(cx).is_attached_to(&session));
        assert!(!source.read(cx).relocation_is_live(&origin, cx));
    });
    let origin = cx.read(|cx| capture(&source, cx));
    let target = cx.read(|cx| target(&destination, destination_window, TabMovePlacement::End, cx));
    assert_eq!(
        cx.update_window(source_window, |_, window, cx| source
            .update(cx, |shell, cx| shell
                .relocate_tab_to(&origin, &target, window, cx)))
            .unwrap(),
        CommandOutcome::Completed
    );
    let moved_view = cx.read(|cx| {
        destination
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .terminal_view()
            .unwrap()
            .clone()
    });
    cx.read(|cx| {
        assert_ne!(source_view, moved_view);
        assert!(!source_view.read(cx).is_attached_to(&session));
        assert!(moved_view.read(cx).is_attached_to(&session));
        assert_eq!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Running
        );
        assert!(Arc::ptr_eq(session.read(cx).terminal().unwrap(), &grid));
        assert_eq!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&id],
            session
        );
        assert!(
            source
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .document_id()
                .is_some()
        );
        assert_eq!(
            cx.global::<ApplicationWorkbenches>()
                .view_count(document, cx),
            1
        );
    });
    let size = crate::app::terminal_session::TerminalSize {
        columns: 80,
        lines: 20,
        cell_width: 9,
        cell_height: 18,
    };
    moved_view.update(cx, |view, cx| view.resize(size, cx));
    let stale_size = crate::app::terminal_session::TerminalSize {
        columns: 40,
        lines: 10,
        cell_width: 9,
        cell_height: 18,
    };
    source_view.update(cx, |view, cx| view.resize(stale_size, cx));
    cx.read(|cx| assert_eq!(session.read(cx).size(), size));
    session.update(cx, |session, cx| session.close(cx));
}
