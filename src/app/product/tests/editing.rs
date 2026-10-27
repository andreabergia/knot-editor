//! Editor input, clipboard, motion, and undo routing through product commands.
use super::*;

#[gpui::test]
async fn editing_commands_keep_the_captured_view_and_native_clipboard(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    let (target, dispatcher) = cx
        .update_window(window_handle, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
            let target = shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            });
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                "fn café() {\n    👩‍💻\n}\n".into(),
            ));
            shell.update(cx, |shell, cx| shell.new_tab(window, cx));
            (target, cx.global::<ApplicationProductCommands>().0.clone())
        })
        .unwrap();
    for command in [
        super::super::PASTE_COMMAND,
        super::super::SELECT_ALL_COMMAND,
        super::super::COPY_COMMAND,
    ] {
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: command.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target.clone(),
                cx,
            )
        });
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed
        );
    }
    cx.read(|cx| {
        assert_eq!(model.read(cx).text(), "fn café() {\n    👩‍💻\n}\n");
        let active = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap();
        assert_eq!(active.read(cx).model().read(cx).text(), "");
    });
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: super::super::CUT_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target,
            cx,
        )
    });
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.update(|cx| {
        assert_eq!(model.read(cx).text(), "");
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "fn café() {\n    👩‍💻\n}\n"
        );
    });
}

#[gpui::test]
async fn copy_requires_focused_selection_and_declines_on_terminal(cx: &mut TestAppContext) {
    use alacritty_terminal::tty::{Options, Shell};
    use alacritty_terminal::{
        index::{Column, Line, Point as TermPoint, Side},
        selection::{Selection, SelectionType},
        vte::ansi::Handler,
    };
    use gpui::EntityInputHandler;

    #[cfg(windows)]
    let quiet_shell = Shell::new(
        "cmd.exe".into(),
        vec!["/C".into(), "ping -n 3 127.0.0.1 >nul".into()],
    );
    #[cfg(not(windows))]
    let quiet_shell = Shell::new("/bin/sh".into(), vec!["-c".into(), "sleep 2".into()]);

    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model.clone(), cx);
    cx.update_window(window, |_, window, cx| {
        let editor = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone();
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(None, "selected text", window, cx);
        });
    })
    .unwrap();
    let no_selection = dispatch_product_command(&shell, window, super::super::COPY_COMMAND, cx);
    assert_eq!(
        no_selection.completion.await.unwrap(),
        CommandOutcome::Unavailable
    );
    cx.update_window(window, |_, window, cx| {
        let editor = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone();
        editor.update(cx, |editor, cx| {
            editor.execute_editing_command("editor.select-all", window, cx);
        });
    })
    .unwrap();
    let editor_target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let editor_copy = dispatch_product_command(&shell, window, super::super::COPY_COMMAND, cx);
    assert_eq!(
        editor_copy.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.update(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "selected text"
        )
    });
    let legacy_copy = dispatch_product_command(&shell, window, "editor.copy", cx);
    assert_eq!(
        legacy_copy.completion.await.unwrap(),
        CommandOutcome::Unavailable
    );

    cx.update_window(window, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.open_command_palette(window, cx));
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(window, "c o p y enter");
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "selected text"
        );
        assert_eq!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .last_outcome(),
            Some(&CommandOutcome::Completed)
        );
    });

    let terminal_session = cx.update(|cx| {
        cx.new(|cx| {
            crate::app::terminal_session::TerminalSession::new_with_options(
                Options {
                    shell: Some(quiet_shell),
                    drain_on_exit: true,
                    ..Default::default()
                },
                cx,
            )
        })
    });
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                let session = cx
                    .global::<ApplicationTerminalSessions>()
                    .0
                    .borrow_mut()
                    .allocate_id();
                cx.global::<ApplicationTerminalSessions>()
                    .0
                    .borrow_mut()
                    .sessions
                    .insert(session, terminal_session.clone());
                let view = cx.new(|cx| {
                    crate::app::terminal_view::TerminalView::new(terminal_session.clone(), cx)
                });
                let pane = shell.workbench.read(cx).focused_pane_id().unwrap();
                let tab = shell.workbench.update(cx, |workbench, _| {
                    workbench.open_terminal_tab(pane, session, view).unwrap()
                });
                shell.activate_tab(pane, tab, window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    let captured_editor = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: super::super::COPY_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            editor_target,
            cx,
        )
    });
    assert_eq!(
        captured_editor.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "selected text"
        )
    });
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: super::super::COPY_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target.clone(),
            cx,
        )
    });
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Unavailable
    );
    cx.read(|cx| {
        let session_id = match target.surface {
            TabSurfaceId::Terminal(id) => id,
            _ => unreachable!(),
        };
        let session = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions[&session_id]
            .clone();
        let terminal = session.read(cx).terminal().unwrap().clone();
        let mut terminal = terminal.lock();
        terminal.goto(0, 0);
        for ch in "selected terminal text".chars() {
            terminal.input(ch);
        }
        let mut selection = Selection::new(
            SelectionType::Simple,
            TermPoint::new(Line(0), Column(0)),
            Side::Left,
        );
        selection.update(TermPoint::new(Line(0), Column(21)), Side::Right);
        terminal.selection = Some(selection);
    });
    let selected_text = cx.read(|cx| {
        let session_id = match target.surface {
            TabSurfaceId::Terminal(id) => id,
            _ => unreachable!(),
        };
        let session = cx
            .global::<ApplicationTerminalSessions>()
            .0
            .borrow()
            .sessions[&session_id]
            .clone();
        session
            .read(cx)
            .terminal()
            .unwrap()
            .lock()
            .selection_to_string()
            .unwrap()
    });
    assert!(!selected_text.is_empty());
    let selected_terminal =
        dispatch_product_command(&shell, window, super::super::COPY_COMMAND, cx);
    assert_eq!(
        selected_terminal.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            selected_text
        );
    });
    let application = dispatch_product_command(
        &shell,
        window,
        super::super::SHOW_EXTENSION_REPORT_COMMAND,
        cx,
    );
    assert_eq!(
        application.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| assert!(shell.read(cx).extension_report_open));

    let scripted = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: super::super::COPY_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target.clone(),
            cx,
        )
    });
    assert_eq!(
        scripted.completion.await.unwrap(),
        CommandOutcome::Completed
    );

    let unavailable = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: "missing.command".into(),
                arguments: CommandArgumentValue::Null,
            },
            target.clone(),
            cx,
        )
    });
    assert_eq!(
        unavailable.completion.await.unwrap(),
        CommandOutcome::Unavailable
    );

    let mut stale = target;
    stale.surface = TabSurfaceId::Document(document);
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: super::super::COPY_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            stale,
            cx,
        )
    });
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::InvalidTarget
    );
}

#[gpui::test]
fn product_editor_accepts_utf16_composition_and_grapheme_deletion(cx: &mut TestAppContext) {
    use gpui::EntityInputHandler;
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    cx.update_window(window_handle, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx);
        let editor = shell
            .read(cx)
            .workbench
            .read(cx)
            .focused_pane()
            .unwrap()
            .active_tab()
            .editor()
            .unwrap()
            .clone();
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(None, "# Notes\n👩‍💻 é ", window, cx);
            let start = "# Notes\n👩‍💻 é ".encode_utf16().count();
            editor.replace_and_mark_text_in_range(None, "に", Some(1..1), window, cx);
            assert_eq!(editor.marked_text_range(window, cx), Some(start..start + 1));
            editor.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
            assert_eq!(editor.marked_text_range(window, cx), Some(start..start + 2));
            editor.replace_text_in_range(None, "日本語", window, cx);
            assert_eq!(editor.marked_text_range(window, cx), None);
            assert!(editor.execute_editing_command("editor.move-document-start", window, cx));
            assert!(editor.execute_editing_command("editor.move-down", window, cx));
            assert!(editor.execute_editing_command("editor.delete-forward", window, cx));
        });
    })
    .unwrap();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "# Notes\n é 日本語"));
}

#[gpui::test]
fn editing_keybindings_use_semantic_commands(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    cx.update_window(window_handle, |_, window, cx| {
        shell.read(cx).focus_active_editor(window, cx)
    })
    .unwrap();
    cx.simulate_keystrokes(window_handle, "enter");
    cx.run_until_parked();
    cx.read(|cx| {
        assert_eq!(model.read(cx).text(), "\n");
        assert_eq!(
            cx.global::<ApplicationProductCommands>()
                .0
                .read(cx)
                .last_outcome(),
            Some(&CommandOutcome::Completed)
        );
    });
    cx.simulate_keystrokes(window_handle, "shift-up backspace tab");
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "\t"));
    cx.simulate_input(window_handle, "café👩‍💻");
    cx.simulate_keystrokes(window_handle, "enter");
    cx.simulate_input(window_handle, "tail");
    cx.simulate_keystrokes(window_handle, "home");
    cx.simulate_input(window_handle, ">");
    cx.simulate_keystrokes(window_handle, "end");
    cx.simulate_input(window_handle, "<");
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) {
            "shift-home ctrl-c"
        } else {
            "shift-home cmd-c"
        },
    );
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), ">tail<"));
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) {
            "ctrl-a ctrl-c"
        } else {
            "cmd-a cmd-c"
        },
    );
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "\tcafé👩‍💻\n>tail<"
        )
    });
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) {
            "ctrl-x ctrl-v"
        } else {
            "cmd-x cmd-v"
        },
    );
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "\tcafé👩‍💻\n>tail<"));
}

#[gpui::test]
fn product_editor_captures_mouse_selection_outside_its_pane(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let text = "first line\nsecond line";
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    model.update(cx, |model, _| {
        model.replace(0..0, text).unwrap();
    });
    let (shell, window_handle) = product_window(document, model, cx);
    cx.refresh().unwrap();
    cx.run_until_parked();

    let editor = cx.read(|cx| {
        shell
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
    let bounds = cx.read(|cx| editor.read(cx).interaction_bounds());
    let start = point(bounds.origin.x + px(1.), bounds.origin.y + px(5.));
    let outside = point(
        bounds.origin.x + bounds.size.width + px(20.),
        bounds.origin.y + bounds.size.height + px(20.),
    );
    let mut window_cx = VisualTestContext::from_window(window_handle, cx);
    window_cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    window_cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::default());
    window_cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());

    window_cx.read(|cx| {
        assert_eq!(editor.read(cx).selected_byte_range(cx), Some(0..text.len()));
    });
}

#[gpui::test]
async fn undo_command_routes_to_the_captured_editor(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model, cx);
    cx.update(|cx| {
        let model = documents.read(cx).get(document).unwrap().model().clone();
        model.update(cx, |model, _| model.replace(0..0, "edit").unwrap());
    });
    let (target, dispatcher) = cx
        .update_window(window_handle, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
            let target = shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            });
            (target, cx.global::<ApplicationProductCommands>().0.clone())
        })
        .unwrap();
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: UNDO_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target.clone(),
            cx,
        )
    });

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            documents
                .read(cx)
                .get(document)
                .unwrap()
                .model()
                .read(cx)
                .text(),
            ""
        );
    });
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: REDO_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target,
            cx,
        )
    });
    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(
            documents
                .read(cx)
                .get(document)
                .unwrap()
                .model()
                .read(cx)
                .text(),
            "edit"
        );
    });
}

#[gpui::test]
async fn every_editor_motion_command_reaches_the_focused_view(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window) = product_window(document, model, cx);
    let target = cx
        .update_window(window, |_, window, cx| {
            shell.read(cx).focus_active_editor(window, cx);
            shell.update(cx, |shell, cx| {
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    for name in super::super::super::product_commands::product_command_names().filter(|name| {
        name.starts_with(super::super::super::product_commands::EDITOR_MOVE_PREFIX)
            || name.starts_with(super::super::super::product_commands::EDITOR_SELECT_PREFIX)
    }) {
        let execution = dispatcher.update(cx, |dispatcher, cx| {
            dispatcher.dispatch(
                Command {
                    name: name.into(),
                    arguments: CommandArgumentValue::Null,
                },
                target.clone(),
                cx,
            )
        });
        assert_eq!(
            execution.completion.await.unwrap(),
            CommandOutcome::Completed,
            "{name}"
        );
    }
}

#[gpui::test]
async fn undo_keeps_the_document_captured_before_focus_changes(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    let first_document = cx.update(|cx| create_untitled_document(&documents, cx));
    let first_model = cx.read(|cx| {
        documents
            .read(cx)
            .get(first_document)
            .unwrap()
            .model()
            .clone()
    });
    let (shell, window_handle) = product_window(first_document, first_model.clone(), cx);
    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| shell.new_tab(window, cx));
    })
    .unwrap();

    let (pane, first_tab, second_tab, second_document, second_model) = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        let pane = workbench.focused_pane().unwrap();
        let first = &pane.tabs()[0];
        let second = &pane.tabs()[1];
        let second_model = documents
            .read(cx)
            .get(second.document_id().unwrap())
            .unwrap()
            .model()
            .clone();
        (
            pane.id(),
            first.id(),
            second.id(),
            second.document_id().unwrap(),
            second_model,
        )
    });
    first_model.update(cx, |model, _| model.replace(0..0, "first").unwrap());
    second_model.update(cx, |model, _| model.replace(0..0, "second").unwrap());

    let target = cx
        .update_window(window_handle, |_, window, cx| {
            shell.update(cx, |shell, cx| {
                shell.activate_tab(pane, first_tab, window, cx);
                shell.capture_command_target(window, cx).unwrap()
            })
        })
        .unwrap();
    let dispatcher = cx.read(|cx| cx.global::<ApplicationProductCommands>().0.clone());
    let execution = dispatcher.update(cx, |dispatcher, cx| {
        dispatcher.dispatch(
            Command {
                name: UNDO_COMMAND.into(),
                arguments: CommandArgumentValue::Null,
            },
            target,
            cx,
        )
    });
    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.activate_tab(pane, second_tab, window, cx)
        });
    })
    .unwrap();

    assert_eq!(
        execution.completion.await.unwrap(),
        CommandOutcome::Completed
    );
    cx.read(|cx| {
        assert_eq!(first_model.read(cx).text(), "");
        assert_eq!(second_model.read(cx).text(), "second");
        assert_eq!(
            shell
                .read(cx)
                .workbench
                .read(cx)
                .focused_pane()
                .unwrap()
                .active_tab()
                .document_id()
                .unwrap(),
            second_document
        );
    });
}

#[gpui::test]
fn focus_transfer_and_distinct_views_break_typing_groups(cx: &mut TestAppContext) {
    let documents = install_globals(cx);
    cx.update(super::super::bind_product_keys);
    let document = cx.update(|cx| create_untitled_document(&documents, cx));
    let model = cx.read(|cx| documents.read(cx).get(document).unwrap().model().clone());
    let (shell, window_handle) = product_window(document, model.clone(), cx);
    cx.update_window(window_handle, |_, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.split(SplitDirection::Horizontal, window, cx)
        });
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.run_until_parked();
    let (first, second) = cx.read(|cx| {
        let workbench = shell.read(cx).workbench.read(cx);
        (
            workbench.panes()[0].active_tab().editor().unwrap().clone(),
            workbench.panes()[1].active_tab().editor().unwrap().clone(),
        )
    });
    cx.update_window(window_handle, |_, window, cx| {
        first.focus_handle(cx).focus(window)
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_input(window_handle, "a");
    cx.update_window(window_handle, |_, window, cx| {
        second.focus_handle(cx).focus(window);
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.run_until_parked();
    cx.update_window(window_handle, |_, window, cx| {
        first.focus_handle(cx).focus(window);
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.run_until_parked();
    cx.simulate_input(window_handle, "b");
    cx.refresh().unwrap();
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) { "ctrl-z" } else { "cmd-z" },
    );
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "a"));

    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) {
            "ctrl-shift-z"
        } else {
            "cmd-shift-z"
        },
    );
    cx.run_until_parked();
    cx.update_window(window_handle, |_, window, cx| {
        second.focus_handle(cx).focus(window)
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.run_until_parked();
    cx.simulate_input(window_handle, "X");
    cx.update_window(window_handle, |_, window, cx| {
        first.focus_handle(cx).focus(window)
    })
    .unwrap();
    cx.refresh().unwrap();
    cx.simulate_keystrokes(
        window_handle,
        if cfg!(windows) { "ctrl-z" } else { "cmd-z" },
    );
    cx.run_until_parked();
    cx.read(|cx| assert_eq!(model.read(cx).text(), "ab"));
}
