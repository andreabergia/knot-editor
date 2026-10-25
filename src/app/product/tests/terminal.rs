//! Terminal tabs, sessions, closure, and cross-window transfer.
use super::*;

#[cfg(unix)]
#[gpui::test]
async fn terminal_mouse_reporting_toggle_routes_to_the_focused_session_and_plain_drag_copies(
    cx: &mut TestAppContext,
) {
    use crate::app::{
        terminal_session::TerminalSession, terminal_view::TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND,
        terminal_view::TerminalView,
    };
    use alacritty_terminal::{tty, vte::ansi::Processor};
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(
            &shell,
            window_handle,
            TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND,
            cx
        )
        .completion
        .await
        .unwrap(),
        CommandOutcome::Unavailable
    );
    let session = cx.new(|cx| {
        TerminalSession::new_with_options(
            tty::Options {
                shell: Some(tty::Shell::new(
                    "/bin/sh".into(),
                    vec!["-c".into(), "read line".into()],
                )),
                drain_on_exit: true,
                ..Default::default()
            },
            cx,
        )
    });
    let view = cx
        .update_window(window_handle, |_, window, cx| {
            let id = cx
                .global::<ApplicationTerminalSessions>()
                .0
                .borrow_mut()
                .allocate_id();
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow_mut()
                .sessions
                .insert(id, session.clone());
            let view = cx.new(|cx| TerminalView::new(session.clone(), cx));
            shell.update(cx, |shell, cx| {
                let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
                let tab = shell.workbench.update(cx, |workbench, _| {
                    workbench.open_terminal_tab(pane, id, view.clone()).unwrap()
                });
                shell.activate_tab(pane, tab, window, cx);
            });
            view
        })
        .unwrap();
    cx.refresh().unwrap();
    assert_eq!(
        dispatch_product_command(
            &shell,
            window_handle,
            TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND,
            cx
        )
        .completion
        .await
        .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| assert!(!session.read(cx).mouse_reporting_enabled()));
    let (start, end) = cx
        .update_window(window_handle, |_, window, cx| {
            let mut parser: Processor = Processor::new();
            parser.advance(
                &mut *session.read(cx).terminal().unwrap().lock(),
                b"\x1b[?1002h\x1b[?1006h\x1b[2J\x1b[3J\x1b[Hcopy me",
            );
            let bounds = view.read(cx).interaction_bounds();
            let width = px(session.read(cx).size().cell_width as f32 / window.scale_factor());
            (
                bounds.origin + point(px(0.1), px(0.1)),
                bounds.origin + point(width * 3 + px(0.1), px(0.1)),
            )
        })
        .unwrap();
    let mut window_cx = VisualTestContext::from_window(window_handle, cx);
    window_cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    window_cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    window_cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) { "ctrl-c" } else { "cmd-c" },
    );
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("copy")
        );
        assert!(!session.read(cx).mouse_reporting_enabled());
    });
    assert_eq!(
        dispatch_product_command(
            &shell,
            window_handle,
            TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND,
            cx
        )
        .completion
        .await
        .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| assert!(session.read(cx).mouse_reporting_enabled()));
    session.update(cx, |session, cx| session.close(cx));
}

#[gpui::test]
async fn terminal_tab_has_a_captured_surface_and_document_commands_are_unavailable(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let (pane, tab) = shell.update(cx, |shell, cx| {
        let (session, view) = shell.create_terminal(cx);
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        let tab = shell.workbench.update(cx, |workbench, _| {
            workbench.open_terminal_tab(pane, session, view).unwrap()
        });
        (pane, tab)
    });
    let session = cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .pane(pane)
            .unwrap()
            .active_tab()
            .surface_id()
    });
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.activate_tab(pane, tab, window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    assert_eq!(target.surface, session);
    cx.read(|cx| {
        assert_eq!(shell.read(cx).workbench.read(cx).view_count(document), 1);
        assert_eq!(
            cx.global::<ApplicationWorkbenches>()
                .view_count(document, cx),
            1
        );
    });
    let (completion, receiver) = super::super::CommandCompletion::new();
    shell.update(cx, |shell, cx| {
        shell.start_save_command(false, target.clone(), completion, cx)
    });
    assert_eq!(receiver.await.unwrap(), CommandOutcome::Unavailable);
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    let result = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: "editor.move-left".into(),
                arguments: CommandArgumentValue::Null,
            },
            target.clone(),
            cx,
        )
    });
    assert_eq!(
        result.completion.await.unwrap(),
        CommandOutcome::Unavailable
    );
    let mut wrong_surface = target;
    wrong_surface.surface = TabSurfaceId::Document(document);
    let result = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: NEW_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            wrong_surface,
            cx,
        )
    });
    assert_eq!(
        result.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
}

#[gpui::test]
async fn closing_terminal_tab_preserves_dirty_document_and_invalidates_its_target(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);
    let (pane, tab) = shell.update(cx, |shell, cx| {
        let (session, view) = shell.create_terminal(cx);
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        let tab = shell.workbench.update(cx, |workbench, _| {
            workbench.open_terminal_tab(pane, session, view).unwrap()
        });
        (pane, tab)
    });
    let stale = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.activate_tab(pane, tab, window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let execution = dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx);
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    assert!(!cx.has_pending_prompt());
    cx.read(|cx| {
        let workbench = shell.read(cx).workbench.clone();
        assert_eq!(workbench.read(cx).tab_snapshots().len(), 1);
        assert_eq!(workbench.read(cx).view_count(document), 1);
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
    });
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    let result = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: NEW_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            stale,
            cx,
        )
    });
    assert_eq!(
        result.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
}

#[gpui::test]
async fn mixed_window_and_quit_cancellation_preserve_every_surface(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    install_failing_recovery(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);
    let (pane, tab) = shell.update(cx, |shell, cx| {
        let (session, view) = shell.create_terminal(cx);
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        let tab = shell.workbench.update(cx, |workbench, _| {
            workbench.open_terminal_tab(pane, session, view).unwrap()
        });
        (pane, tab)
    });
    let session = cx.read(|cx| {
        shell
            .read(cx)
            .workbench
            .read(cx)
            .pane(pane)
            .unwrap()
            .active_tab()
            .surface_id()
    });
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.activate_tab(pane, tab, window, cx));
    })
    .unwrap();
    for command in [CLOSE_WINDOW_COMMAND, QUIT_COMMAND] {
        let execution = dispatch_product_command(&shell, window, command, cx);
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Cancelled
        );
        cx.read(|cx| {
            assert!(cx.windows().contains(&window));
            let snapshots = shell.read(cx).workbench.read(cx).tab_snapshots();
            assert_eq!(snapshots.len(), 2);
            assert!(
                snapshots
                    .iter()
                    .any(|tab| tab.surface == TabSurfaceId::Document(document))
            );
            assert!(snapshots.iter().any(|tab| tab.surface == session));
            let TabSurfaceId::Terminal(id) = session else {
                panic!()
            };
            let registry = cx.global::<ApplicationTerminalSessions>().0.borrow();
            assert_ne!(
                registry.sessions[&id].read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Closed
            );
            assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
        });
    }
}

#[gpui::test]
async fn terminal_command_focuses_a_live_view_and_split_creates_an_independent_session(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);

    let command = dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx);
    assert_eq!(command.completion.await.unwrap(), CommandOutcome::Completed);
    let (pane, first, document_tab) = cx.read(|cx| {
        let shell = shell.read(cx);
        let pane = shell.workbench.read(cx).focused_pane().unwrap();
        (
            pane.id(),
            pane.active_tab().surface_id(),
            pane.tabs()[0].id(),
        )
    });
    let TabSurfaceId::Terminal(first_id) = first else {
        panic!("terminal command opened a document")
    };
    cx.update_window(window, |_, window, cx| {
        let shell = shell.read(cx);
        let pane = shell.workbench.read(cx).pane(pane).unwrap();
        assert!(
            pane.active_tab()
                .terminal_view()
                .unwrap()
                .focus_handle(cx)
                .contains_focused(window, cx)
        );
    })
    .unwrap();
    cx.read(|cx| {
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&first_id)
        );
    });

    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.activate_tab(pane, document_tab, window, cx)
        });
    })
    .unwrap();
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            let terminal_tab = shell.workbench.read(cx).pane(pane).unwrap().tabs()[1].id();
            shell.activate_tab(pane, terminal_tab, window, cx);
        });
    })
    .unwrap();
    cx.read(|cx| {
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .pane(pane)
                .unwrap()
                .active_tab()
                .surface_id(),
            first
        );
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&first_id)
        );
    });

    let split = dispatch_product_command(&shell, window, SPLIT_HORIZONTAL_COMMAND, cx);
    assert_eq!(split.completion.await.unwrap(), CommandOutcome::Completed);
    let second_id = cx.read(|cx| {
        let shell = shell.read(cx);
        let workbench = shell.workbench.read(cx);
        assert_eq!(workbench.panes().len(), 2);
        match workbench.focused_pane().unwrap().active_tab().surface_id() {
            TabSurfaceId::Terminal(id) => id,
            TabSurfaceId::Document(_) | TabSurfaceId::Tool(_) => {
                panic!("split did not create terminal")
            }
        }
    });
    assert_ne!(first_id, second_id);
    cx.read(|cx| {
        let registry = cx.global::<ApplicationTerminalSessions>().0.borrow();
        assert_eq!(registry.sessions.len(), 2);
        assert!(registry.sessions.contains_key(&first_id));
        assert!(registry.sessions.contains_key(&second_id));
    });
}

#[gpui::test]
async fn terminal_close_and_reopen_settle_the_exact_session(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let (first_id, first_session) = cx.read(|cx| {
        let TabSurfaceId::Terminal(id) = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .surface_id()
        else {
            panic!()
        };
        (
            id,
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&id]
                .clone(),
        )
    });
    assert_eq!(
        dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            first_session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Closed
        );
        assert!(
            !cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&first_id)
        );
    });
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        let TabSurfaceId::Terminal(second_id) = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .surface_id()
        else {
            panic!()
        };
        assert_ne!(first_id, second_id);
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&second_id)
        );
    });
}

#[gpui::test]
async fn closing_the_final_terminal_tab_installs_an_untitled_workbench(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let (pane, document_tab, session) = cx.read(|cx| {
        let shell = shell.read(cx);
        let pane = shell.workbench.read(cx).focused_pane().unwrap();
        let TabSurfaceId::Terminal(id) = pane.active_tab().surface_id() else {
            panic!()
        };
        (
            pane.id(),
            pane.tabs()[0].id(),
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&id]
                .clone(),
        )
    });
    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.activate_tab(pane, document_tab, window, cx)
        });
    })
    .unwrap();
    assert_eq!(
        dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        let shell = shell.read(cx);
        let tabs = shell.workbench.read(cx).tab_snapshots();
        assert_eq!(tabs.len(), 1);
        assert!(matches!(tabs[0].surface, TabSurfaceId::Terminal(_)));
        assert!(documents.read(cx).get(document).is_none());
    });
    assert_eq!(
        dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        let shell = shell.read(cx);
        let tabs = shell.workbench.read(cx).tab_snapshots();
        assert_eq!(tabs.len(), 1);
        assert!(matches!(tabs[0].surface, TabSurfaceId::Document(_)));
        assert_eq!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Closed
        );
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .is_empty()
        );
    });
}

#[gpui::test]
async fn window_close_shuts_down_terminal_after_protected_decisions(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let session = cx.read(|cx| {
        let TabSurfaceId::Terminal(id) = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .surface_id()
        else {
            panic!()
        };
        cx.global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions[&id]
            .clone()
    });
    assert_eq!(
        dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert!(!cx.windows().contains(&window));
        assert_eq!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Closed
        );
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .is_empty()
        );
    });
}

#[gpui::test]
async fn mixed_window_save_failure_keeps_terminal_running(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.set_global(ApplicationFileSystems(memory_filesystems()));
    cx.set_global(ApplicationSaveDialog(Arc::new(FixedSaveDialog(
        SaveDialogOutcome::Selected(ResourceUri::parse("mem://product/src").unwrap()),
    ))));
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| model.replace(0..0, "unsaved").unwrap());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let (id, session) = cx.read(|cx| {
        let TabSurfaceId::Terminal(id) = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .surface_id()
        else {
            panic!()
        };
        (
            id,
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&id]
                .clone(),
        )
    });
    let execution = dispatch_product_command(&shell, window, CLOSE_WINDOW_COMMAND, cx);
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer("Save");
    assert!(matches!(
        execution.completion.await.unwrap(),
        CommandOutcome::HandlerFailure { .. }
    ));
    cx.read(|cx| {
        assert!(cx.windows().contains(&window));
        assert_eq!(shell.read(cx).workbench.read(cx).tab_snapshots().len(), 2);
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&id)
        );
        assert_ne!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Closed
        );
        assert!(documents.read(cx).get(document).unwrap().is_dirty(cx));
    });
}

#[gpui::test]
async fn quit_shuts_down_terminal_sessions_in_every_window(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let first = cx.update(|cx| create_untitled_document(&documents, cx));
    let first_model = cx.read(|cx| documents.read(cx).get(first).unwrap().model().clone());
    let (first_shell, first_window) = product_window(first, first_model, cx);
    let second = cx.update(|cx| create_untitled_document(&documents, cx));
    let second_model = cx.read(|cx| documents.read(cx).get(second).unwrap().model().clone());
    let (second_shell, second_window) = product_window(second, second_model, cx);
    for (shell, window) in [(&first_shell, first_window), (&second_shell, second_window)] {
        assert_eq!(
            dispatch_product_command(shell, window, NEW_TERMINAL_COMMAND, cx)
                .completion
                .await
                .unwrap(),
            CommandOutcome::Completed
        );
    }
    let sessions = cx.read(|cx| {
        cx.global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions
            .values()
            .cloned()
            .collect::<Vec<_>>()
    });
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        dispatch_product_command(&first_shell, first_window, QUIT_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .is_empty()
        );
        for session in &sessions {
            assert_eq!(
                session.read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Closed
            );
        }
    });
}

#[gpui::test]
async fn moving_terminal_keeps_its_session_and_source_window_close_does_not_stop_it(
    cx: &mut TestAppContext,
) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (source, source_window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&source, source_window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let (id, session, grid, source_view) = cx.read(|cx| {
        let shell = source.read(cx);
        let tab = shell
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab();
        let TabSurfaceId::Terminal(id) = tab.surface_id() else {
            panic!()
        };
        let session = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions[&id]
            .clone();
        let grid = session.read(cx).terminal().unwrap().clone();
        (id, session, grid, tab.terminal_view().unwrap().clone())
    });
    assert_eq!(
        dispatch_product_command(
            &source,
            source_window,
            MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND,
            cx
        )
        .completion
        .await
        .unwrap(),
        CommandOutcome::Completed
    );
    let destination_window = cx.read(|cx| {
        *cx.windows()
            .iter()
            .find(|handle| **handle != source_window)
            .unwrap()
    });
    let destination = cx
        .update_window(destination_window, |_, window, _| {
            window.root::<ProductShell>().flatten().unwrap()
        })
        .unwrap();
    let destination_view = cx.read(|cx| {
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
    cx.update_window(destination_window, |_, window, cx| {
        assert!(
            destination_view
                .focus_handle(cx)
                .contains_focused(window, cx)
        );
    })
    .unwrap();
    let destination_size = crate::app::terminal_session::TerminalSize {
        columns: 80,
        lines: 20,
        cell_width: 9,
        cell_height: 18,
    };
    destination_view.update(cx, |view, cx| view.resize(destination_size, cx));
    cx.read(|cx| assert_eq!(session.read(cx).size(), destination_size));
    let stale_size = crate::app::terminal_session::TerminalSize {
        columns: 40,
        lines: 10,
        cell_width: 9,
        cell_height: 18,
    };
    source_view.update(cx, |view, cx| view.resize(stale_size, cx));
    cx.read(|cx| assert_eq!(session.read(cx).size(), destination_size));
    cx.read(|cx| {
        assert!(
            !source
                .read(cx)
                .workbench
                .read(cx)
                .tab_snapshots()
                .iter()
                .any(|tab| tab.surface == TabSurfaceId::Terminal(id))
        );
        let destination_shell = destination.read(cx);
        let tab = destination_shell
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab();
        assert_eq!(tab.surface_id(), TabSurfaceId::Terminal(id));
        assert_ne!(tab.terminal_view(), Some(&source_view));
        assert!(std::sync::Arc::ptr_eq(
            session.read(cx).terminal().unwrap(),
            &grid
        ));
        assert_eq!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions[&id],
            session
        );
    });
    assert_eq!(
        dispatch_product_command(&source, source_window, CLOSE_WINDOW_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Running
        );
        assert!(cx.windows().contains(&destination_window));
    });
    assert_eq!(
        dispatch_product_command(&destination, destination_window, CLOSE_TAB_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Closed
        );
        assert!(
            !cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&id)
        );
    });
}

#[gpui::test]
async fn failed_terminal_destination_leaves_source_attached(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let outcome = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                let target = shell.capture_command_target(window, cx).unwrap();
                shell.move_terminal_to_new_window_with(&target, window, cx, |_| {
                    anyhow::bail!("simulated window creation failure")
                })
            })
        })
        .unwrap();
    assert!(matches!(outcome, CommandOutcome::HandlerFailure { .. }));
    cx.read(|cx| {
        let shell = shell.read(cx);
        let tab = shell
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab();
        let TabSurfaceId::Terminal(id) = tab.surface_id() else {
            panic!()
        };
        assert!(tab.terminal_view().is_some());
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&id)
        );
        assert_eq!(cx.windows().len(), 1);
    });
}

#[gpui::test]
async fn vanished_terminal_destination_leaves_source_attached(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Unavailable
    );
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let outcome = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                let target = shell.capture_command_target(window, cx).unwrap();
                shell.move_terminal_to_new_window_with(&target, window, cx, |cx| {
                    let destination = open_tab_transfer_window(cx)?;
                    cx.update_window(destination.into(), |_, window, _| window.remove_window())?;
                    Ok(destination)
                })
            })
        })
        .unwrap();
    assert_eq!(outcome, CommandOutcome::InvalidTarget);
    cx.read(|cx| {
        let shell = shell.read(cx);
        let tab = shell
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab();
        let TabSurfaceId::Terminal(id) = tab.surface_id() else {
            panic!()
        };
        assert!(tab.terminal_view().is_some());
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&id)
        );
        assert_eq!(cx.windows().len(), 1);
    });
}

#[gpui::test]
async fn stale_terminal_move_target_is_rejected(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    assert_eq!(
        dispatch_product_command(&shell, window, NEW_TERMINAL_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    assert_eq!(
        dispatch_product_command(&shell, window, CLOSE_TAB_COMMAND, cx)
            .completion
            .await
            .unwrap(),
        CommandOutcome::Completed
    );
    let command = crate::host::protocol::Command {
        name: MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND.into(),
        arguments: crate::host::protocol::CommandArgumentValue::Null,
    };
    let execution = cx.update(|cx| {
        let dispatcher = cx.global::<ApplicationProductCommands>().0.clone();
        dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(command, target, cx)
        })
    });
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
    cx.read(|cx| assert_eq!(cx.windows().len(), 1));
}

#[gpui::test]
async fn process_exit_during_terminal_handoff_keeps_final_grid(cx: &mut TestAppContext) {
    use alacritty_terminal::tty::{Options, Shell};
    use std::{
        thread,
        time::{Duration, Instant},
    };

    #[cfg(windows)]
    let shell = Shell::new(
        "cmd.exe".into(),
        vec!["/C".into(), "echo handoff-output & exit /b 7".into()],
    );
    #[cfg(not(windows))]
    let shell = Shell::new(
        "/bin/sh".into(),
        vec![
            "-c".into(),
            "printf 'handoff-output\\n'; sleep 0.05; exit 7".into(),
        ],
    );

    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (source, source_window) = product_window(document, model, cx);
    let session = cx.update(|cx| {
        cx.new(|cx| {
            crate::app::terminal_session::TerminalSession::new_with_options(
                Options {
                    shell: Some(shell),
                    drain_on_exit: true,
                    ..Default::default()
                },
                cx,
            )
        })
    });
    let id = source.update(cx, |shell, cx| {
        let id = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow_mut()
            .allocate_id();
        cx.global::<ApplicationTerminalSessions>()
            .0
            .borrow_mut()
            .sessions
            .insert(id, session.clone());
        let view = cx.new(|cx| crate::app::terminal_view::TerminalView::new(session.clone(), cx));
        let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
        shell.workbench.update(cx, |workbench, _| {
            workbench.open_terminal_tab(pane, id, view).unwrap()
        });
        id
    });
    let outcome = cx
        .update_window(source_window, |_, window, cx| {
            source.update(cx, |shell, cx| {
                shell.focus_active_surface(window, cx);
                let target = shell.capture_command_target(window, cx).unwrap();
                shell.move_terminal_to_new_window_with(&target, window, cx, |cx| {
                    thread::sleep(Duration::from_millis(100));
                    open_tab_transfer_window(cx)
                })
            })
        })
        .unwrap();
    assert_eq!(outcome, CommandOutcome::Completed);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if cx.read(|cx| {
            matches!(
                session.read(cx).status(),
                crate::app::terminal_session::TerminalStatus::Exited(_)
            )
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "terminal did not exit after handoff"
        );
        thread::sleep(Duration::from_millis(5));
    }
    cx.read(|cx| {
        assert!(matches!(
            session.read(cx).status(),
            crate::app::terminal_session::TerminalStatus::Exited(_)
        ));
        let terminal = session.read(cx).terminal().unwrap().lock();
        let output: String = terminal
            .renderable_content()
            .display_iter
            .map(|cell| cell.cell.c)
            .collect();
        assert!(output.contains("handoff-output"));
        assert!(
            cx.global::<ApplicationTerminalSessions>()
                .0
                .borrow()
                .sessions
                .contains_key(&id)
        );
    });
}
