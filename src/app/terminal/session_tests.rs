#![cfg(unix)]

use std::{
    fs,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use alacritty_terminal::{grid::Dimensions, tty};
use gpui::{AppContext, Entity, TestAppContext};

use crate::app::{
    terminal::session::{TerminalSession, TerminalSize, TerminalStatus},
    terminal::view::TerminalView,
};

fn shell(script: &str) -> tty::Options {
    tty::Options {
        shell: Some(tty::Shell::new(
            "/bin/sh".to_owned(),
            vec!["-c".to_owned(), script.to_owned()],
        )),
        drain_on_exit: true,
        ..Default::default()
    }
}

fn output(session: &TerminalSession) -> String {
    let Some(terminal) = session.terminal() else {
        return String::new();
    };
    terminal
        .lock()
        .renderable_content()
        .display_iter
        .map(|cell| cell.cell.c)
        .collect()
}

fn wait_for(
    cx: &mut TestAppContext,
    session: &Entity<TerminalSession>,
    condition: impl Fn(&TerminalSession) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        cx.run_until_parked();
        if session.read_with(cx, |session, _| condition(session)) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "timed out waiting for terminal; status: {:?}, output: {:?}",
        session.read_with(cx, |session, _| session.status()),
        session.read_with(cx, |session, _| output(session)),
    );
}

fn process_exists(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn wait_for_pid(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(contents) = fs::read_to_string(path)
            && let Ok(pid) = contents.parse()
        {
            return pid;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for child PID at {}", path.display());
}

#[gpui::test]
fn view_rebuild_preserves_process_and_grid(cx: &mut TestAppContext) {
    let session = cx.new(|cx| {
        TerminalSession::new_with_options(
            // The loop keeps the child alive so its output is never racing a process exit.
            shell(
                "printf 'before-rebuild\\n'; while IFS= read -r line; do printf 'after:%s\\n' \"$line\"; done",
            ),
            cx,
        )
    });
    let view = cx.new(|cx| TerminalView::new(session.clone(), cx));
    wait_for(cx, &session, |session| {
        output(session).contains("before-rebuild")
    });
    let terminal_before = session.read_with(cx, |session, _| session.terminal().unwrap().clone());

    view.update(cx, |view, cx| view.detach(cx));
    drop(view);
    let _rebuilt = cx.new(|cx| TerminalView::new(session.clone(), cx));
    assert!(session.read_with(cx, |session, _| {
        matches!(session.status(), TerminalStatus::Running)
            && output(session).contains("before-rebuild")
            && std::sync::Arc::ptr_eq(&terminal_before, session.terminal().unwrap())
    }));
    session.update(cx, |session, _| session.send(b"hello\r".to_vec()));
    wait_for(cx, &session, |session| {
        output(session).contains("after:hello")
    });
    assert!(session.read_with(cx, |session, _| {
        matches!(session.status(), TerminalStatus::Running)
            && std::sync::Arc::ptr_eq(&terminal_before, session.terminal().unwrap())
    }));
    session.update(cx, |session, cx| session.close(cx));
}

#[gpui::test]
fn dropping_view_does_not_close_session(cx: &mut TestAppContext) {
    let session = cx.new(|cx| {
        TerminalSession::new_with_options(shell("printf 'still-running\\n'; IFS= read -r line"), cx)
    });
    let view = cx.new(|cx| TerminalView::new(session.clone(), cx));
    wait_for(cx, &session, |session| {
        output(session).contains("still-running")
    });
    let terminal_before = session.read_with(cx, |session, _| session.terminal().unwrap().clone());

    drop(view);
    cx.update(|_| {});
    assert!(session.read_with(cx, |session, _| {
        matches!(session.status(), TerminalStatus::Running)
            && std::sync::Arc::ptr_eq(&terminal_before, session.terminal().unwrap())
    }));
    let _view = cx.new(|cx| TerminalView::new(session.clone(), cx));
    session.update(cx, |session, cx| session.close(cx));
}

#[gpui::test]
fn attachment_lease_allows_only_one_view(cx: &mut TestAppContext) {
    let session = cx.new(|cx| TerminalSession::new_with_options(shell("exec sleep 30"), cx));
    let first = session.update(cx, |session, _| session.attach()).unwrap();
    assert!(session.update(cx, |session, _| session.attach()).is_none());
    drop(first);
    assert!(session.update(cx, |session, _| session.attach()).is_some());
    session.update(cx, |session, cx| session.close(cx));
}

#[gpui::test]
fn startup_failure_has_visible_status_and_can_be_closed(cx: &mut TestAppContext) {
    let options = tty::Options {
        shell: Some(tty::Shell::new(
            "/does-not-exist/knot-test-shell".to_owned(),
            Vec::new(),
        )),
        ..Default::default()
    };
    let session = cx.new(|cx| TerminalSession::new_with_options(options, cx));
    assert!(session.read_with(cx, |session, _| {
        matches!(session.status(), TerminalStatus::Failed(_)) && session.terminal().is_none()
    }));
    session.update(cx, |session, cx| session.close(cx));
    assert!(session.read_with(cx, |session, _| {
        matches!(session.status(), TerminalStatus::Closed)
    }));
}

#[gpui::test]
fn natural_exit_retains_final_output_and_status(cx: &mut TestAppContext) {
    let session = cx.new(|cx| {
        TerminalSession::new_with_options(shell("printf 'final-output\\n'; exit 17"), cx)
    });
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Exited(_))
    });
    assert!(session.read_with(cx, |session, _| output(session).contains("final-output")));
    assert!(session.read_with(cx, |session, _| session.terminal().is_some()));
}

#[gpui::test]
fn current_directory_follows_the_shell(cx: &mut TestAppContext) {
    let temp = tempfile::tempdir().unwrap();
    let nested = temp.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let session = cx.new(|cx| {
        TerminalSession::new_with_options(
            tty::Options {
                working_directory: Some(temp.path().to_owned()),
                ..shell("printf 'started\\n'; while IFS= read -r dir; do cd \"$dir\" && printf 'moved\\n'; done")
            },
            cx,
        )
    });
    wait_for(cx, &session, |session| output(session).contains("started"));
    let canonical = |path: &Path| fs::canonicalize(path).unwrap();
    assert_eq!(
        session
            .read_with(cx, |session, _| session.current_directory())
            .map(|path| canonical(&path)),
        Some(canonical(temp.path()))
    );

    session.update(cx, |session, _| session.send(b"nested\r".to_vec()));
    wait_for(cx, &session, |session| output(session).contains("moved"));
    assert_eq!(
        session
            .read_with(cx, |session, _| session.current_directory())
            .map(|path| canonical(&path)),
        Some(canonical(&nested))
    );
    session.update(cx, |session, cx| session.close(cx));
}

#[gpui::test]
fn stopped_and_failed_sessions_report_their_start_directory(cx: &mut TestAppContext) {
    let temp = tempfile::tempdir().unwrap();
    let nested = temp.path().join("nested");
    fs::create_dir(&nested).unwrap();
    let exited = cx.new(|cx| {
        TerminalSession::new_with_options(
            tty::Options {
                working_directory: Some(temp.path().to_owned()),
                ..shell("cd nested; exit 0")
            },
            cx,
        )
    });
    wait_for(cx, &exited, |session| {
        matches!(session.status(), TerminalStatus::Exited(_))
    });
    assert_eq!(
        exited.read_with(cx, |session, _| session.current_directory()),
        Some(temp.path().to_owned())
    );

    let failed = cx.new(|cx| {
        TerminalSession::new_with_options(
            tty::Options {
                shell: Some(tty::Shell::new(
                    "/does-not-exist/knot-test-shell".to_owned(),
                    Vec::new(),
                )),
                working_directory: Some(temp.path().to_owned()),
                ..Default::default()
            },
            cx,
        )
    });
    assert_eq!(
        failed.read_with(cx, |session, _| session.current_directory()),
        Some(temp.path().to_owned())
    );
}

#[gpui::test]
fn resize_updates_grid_and_child_pty(cx: &mut TestAppContext) {
    let session = cx.new(|cx| {
        TerminalSession::new_with_options(
            shell("IFS= read -r line; stty size; printf 'resize-done\\n'"),
            cx,
        )
    });
    let size = TerminalSize {
        columns: 93,
        lines: 19,
        cell_width: 8,
        cell_height: 16,
    };
    session.update(cx, |session, _| session.resize(size));
    session.update(cx, |session, _| session.send(b"go\r".to_vec()));
    wait_for(cx, &session, |session| {
        output(session).contains("resize-done")
    });
    session.read_with(cx, |session, _| {
        let terminal = session.terminal().unwrap().lock();
        assert_eq!(terminal.columns(), 93);
        assert_eq!(terminal.screen_lines(), 19);
    });
    assert!(session.read_with(cx, |session, _| output(session).contains("19 93")));
}

#[gpui::test]
fn repeated_close_reaps_child_without_blocking_foreground(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("child.pid");
    let mut options = shell("printf '%s' \"$$\" > \"$KNOT_TEST_PID_FILE\"; exec sleep 30");
    options.env.insert(
        "KNOT_TEST_PID_FILE".to_owned(),
        pid_file.to_string_lossy().into_owned(),
    );
    let session = cx.new(|cx| TerminalSession::new_with_options(options, cx));
    let pid = wait_for_pid(&pid_file);
    assert!(process_exists(pid));

    let started = Instant::now();
    session.update(cx, |session, cx| {
        session.close(cx);
        session.close(cx);
    });
    assert!(started.elapsed() < Duration::from_millis(200));
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Closed)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_exists(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process_exists(pid),
        "closed terminal child {pid} remains alive"
    );
}

#[gpui::test]
fn restart_ignores_prior_process_exit(cx: &mut TestAppContext) {
    let session = cx.new(|cx| {
        TerminalSession::new_with_options(shell("printf 'started\\n'; exec sleep 30"), cx)
    });
    wait_for(cx, &session, |session| output(session).contains("started"));
    let first_terminal = session.read_with(cx, |session, _| session.terminal().unwrap().clone());
    session.update(cx, |session, cx| session.restart(cx));
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Running)
            && output(session).contains("started")
            && !std::sync::Arc::ptr_eq(&first_terminal, session.terminal().unwrap())
    });
    thread::sleep(Duration::from_millis(100));
    cx.run_until_parked();
    assert!(session.read_with(cx, |session, _| {
        matches!(session.status(), TerminalStatus::Running)
    }));
    session.update(cx, |session, cx| session.close(cx));
}

#[gpui::test]
fn sustained_output_keeps_foreground_updates_responsive(cx: &mut TestAppContext) {
    let session = cx.new(|cx| {
        TerminalSession::new_with_options(
            shell(
                "IFS= read -r line; i=0; while [ \"$i\" -lt 20000 ]; do printf 'stream-%05d\\n' \"$i\"; i=$((i + 1)); if [ $((i % 100)) -eq 0 ]; then sleep 0.001; fi; done; printf 'stream-complete\\n'",
            ),
            cx,
        )
    });
    session.update(cx, |session, _| session.send(b"go\r".to_vec()));
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Running) && output(session).contains("stream-")
    });

    let started = Instant::now();
    for _ in 0..200 {
        session.update(cx, |session, _| {
            let _ = session.status();
        });
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "foreground updates stalled during PTY output"
    );
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Exited(_))
            && output(session).contains("stream-complete")
    });
}

#[gpui::test]
fn paste_command_delivers_plain_and_bracketed_clipboard_bytes(cx: &mut TestAppContext) {
    use crate::app::commands::dispatch::CommandClaim;
    use crate::host::protocol::CommandOutcome;
    for bracketed in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("input.hex");
        let clipboard = "café\r\nnext\x1b";
        let expected = if bracketed {
            "\x1b[200~café\r\nnext\x1b[201~"
        } else {
            "café\rnext\x1b"
        };
        let enable = if bracketed {
            "printf '\\033[?2004h';"
        } else {
            ""
        };
        let script = format!(
            "stty raw -echo; {enable} printf ready; dd bs=1 count={} 2>/dev/null | od -An -tx1 > '{}'",
            expected.len(),
            destination.display()
        );
        let session = cx.new(|cx| TerminalSession::new_with_options(shell(&script), cx));
        let view = cx.new(|cx| TerminalView::new(session.clone(), cx));
        wait_for(cx, &session, |session| output(session).contains("ready"));
        session.read_with(cx, |session, _| {
            let mut terminal = session.terminal().unwrap().lock();
            let mut parser: alacritty_terminal::vte::ansi::Processor =
                alacritty_terminal::vte::ansi::Processor::new();
            parser.advance(&mut *terminal, "history\r\n".repeat(50).as_bytes());
            terminal.scroll_display(alacritty_terminal::grid::Scroll::Top);
            assert!(terminal.grid().display_offset() > 0);
        });
        cx.update(|cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(clipboard.into())));
        let claim = view.update(cx, |view, cx| {
            view.execute_terminal_command(super::view::TerminalCommand::Paste, cx)
        });
        assert!(matches!(
            claim,
            CommandClaim::Finished(CommandOutcome::Completed)
        ));
        assert_eq!(
            session.read_with(cx, |session, _| session
                .terminal()
                .unwrap()
                .lock()
                .grid()
                .display_offset()),
            0
        );
        wait_for(cx, &session, |session| {
            matches!(session.status(), TerminalStatus::Exited(_))
        });
        let bytes: Vec<u8> = fs::read_to_string(destination)
            .unwrap()
            .split_whitespace()
            .map(|hex| u8::from_str_radix(hex, 16).unwrap())
            .collect();
        assert_eq!(bytes, expected.as_bytes());
        view.update(cx, |view, cx| view.detach(cx));
        session.update(cx, |session, cx| session.close(cx));
    }
}

#[gpui::test]
fn emulator_queries_deliver_ordered_replies_to_the_child_pty(cx: &mut TestAppContext) {
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("replies.hex");
    let expected =
        b"\x1b[0n\x1b[1;1R\x1b[?6c\x1b[8;24;80t\x1b[4;432;720t\x1b]11;rgb:1818/1818/1818\x1b\\";
    let script = format!(
        "stty raw -echo; printf ready; dd bs=1 count=1 >/dev/null 2>&1; printf '\\033[H\\033[5n\\033[6n\\033[c\\033[18t\\033[14t\\033]11;?\\033\\\\'; dd bs=1 count={} 2>/dev/null | od -An -tx1 > '{}'",
        expected.len(),
        destination.display()
    );
    let session = cx.new(|cx| TerminalSession::new_with_options(shell(&script), cx));
    wait_for(cx, &session, |session| output(session).contains("ready"));
    session.update(cx, |session, _| {
        session.resize(TerminalSize {
            columns: 80,
            lines: 24,
            cell_width: 9,
            cell_height: 18,
        });
        session.send(b"!".to_vec());
    });
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Exited(_))
    });
    let bytes: Vec<u8> = fs::read_to_string(destination)
        .unwrap()
        .split_whitespace()
        .map(|hex| u8::from_str_radix(hex, 16).unwrap())
        .collect();
    assert_eq!(bytes, expected);
    session.update(cx, |session, cx| session.close(cx));
}

#[gpui::test]
fn focus_reports_follow_gpui_focus_activation_and_detachment(cx: &mut TestAppContext) {
    use gpui::Focusable;
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("focus.hex");
    let expected = b"\x1b[I\x1b[O\x1b[I\x1b[O\x1b[I\x1b[O";
    let script = format!(
        "stty raw -echo; printf '\\033[?1004hready'; dd bs=1 count={} 2>/dev/null | od -An -tx1 > '{}'",
        expected.len(),
        destination.display()
    );
    let session = cx.new(|cx| TerminalSession::new_with_options(shell(&script), cx));
    wait_for(cx, &session, |session| output(session).contains("ready"));
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(session.clone(), cx));
    cx.refresh().unwrap();
    cx.update(|window, cx| {
        window.activate_window();
        view.read(cx).focus_handle(cx).focus(window);
    });
    cx.run_until_parked();
    // Repaint and repeated focus do not describe a new focus transition.
    cx.refresh().unwrap();
    cx.update(|window, cx| view.read(cx).focus_handle(cx).focus(window));
    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    cx.update(|window, _| window.blur());
    cx.run_until_parked();
    cx.update(|window, cx| view.read(cx).focus_handle(cx).focus(window));
    cx.run_until_parked();
    cx.update(|_, cx| view.update(cx, |view, cx| view.detach(cx)));
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Exited(_))
    });
    let bytes: Vec<u8> = fs::read_to_string(destination)
        .unwrap()
        .split_whitespace()
        .map(|hex| u8::from_str_radix(hex, 16).unwrap())
        .collect();
    assert_eq!(bytes, expected);
    cx.update(|_, cx| session.update(cx, |session, cx| session.close(cx)));
}

#[gpui::test]
fn focus_handoff_reports_each_owner_once_and_detached_keys_are_ignored(cx: &mut TestAppContext) {
    use gpui::Focusable;
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("handoff.hex");
    let expected = b"\x1b[I\x1b[O\x1b[I\x1b[OX";
    let script = format!(
        "stty raw -echo; printf '\\033[?1004hready'; dd bs=1 count={} 2>/dev/null | od -An -tx1 > '{}'",
        expected.len(),
        destination.display()
    );
    let session = cx.new(|cx| TerminalSession::new_with_options(shell(&script), cx));
    wait_for(cx, &session, |session| output(session).contains("ready"));
    let (source, cx) = cx.add_window_view(|_, cx| TerminalView::new(session.clone(), cx));
    cx.refresh().unwrap();
    let source_window = cx.update(|window, cx| {
        window.activate_window();
        source.read(cx).focus_handle(cx).focus(window);
        window.window_handle()
    });
    cx.run_until_parked();
    cx.update(|_, cx| source.update(cx, |view, cx| view.detach(cx)));
    let (target, cx) = cx.add_window_view(|_, cx| TerminalView::new(session.clone(), cx));
    cx.refresh().unwrap();
    cx.update(|window, cx| {
        window.activate_window();
        target.read(cx).focus_handle(cx).focus(window);
    });
    cx.run_until_parked();
    cx.cx.simulate_keystrokes(source_window, "ctrl-z");
    cx.update(|_, cx| target.update(cx, |view, cx| view.detach(cx)));
    cx.update(|_, cx| session.read(cx).send(b"X".to_vec()));
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Exited(_))
    });
    let bytes: Vec<u8> = fs::read_to_string(destination)
        .unwrap()
        .split_whitespace()
        .map(|hex| u8::from_str_radix(hex, 16).unwrap())
        .collect();
    assert_eq!(bytes, expected);
    cx.update(|_, cx| session.update(cx, |session, cx| session.close(cx)));
}

#[gpui::test]
fn old_parser_and_input_generation_cannot_write_to_restarted_pty(cx: &mut TestAppContext) {
    use alacritty_terminal::vte::ansi::Processor;
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("restart.hex");
    let script = format!(
        "stty raw -echo; printf ready; dd bs=1 count=1 2>/dev/null | od -An -tx1 > '{}'",
        destination.display()
    );
    let session = cx.new(|cx| TerminalSession::new_with_options(shell(&script), cx));
    wait_for(cx, &session, |session| output(session).contains("ready"));
    let attachment = session.update(cx, |session, _| session.attach().unwrap());
    let generation = session.read_with(cx, |session, _| session.generation());
    let old_terminal = session.read_with(cx, |session, _| session.terminal().unwrap().clone());
    session.update(cx, |session, cx| session.restart(cx));
    wait_for(cx, &session, |session| output(session).contains("ready"));
    let mut parser: Processor = Processor::new();
    parser.advance(&mut *old_terminal.lock(), b"\x1b[5n\x1b[14t\x1b]11;?\x1b\\");
    session.update(cx, |session, _| {
        session.send_input(&attachment, generation, b"stale".to_vec());
        session.send_input(&attachment, session.generation(), b"X".to_vec());
    });
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Exited(_))
    });
    assert_eq!(fs::read_to_string(destination).unwrap().trim(), "58");
    session.update(cx, |session, cx| session.close(cx));
}

gpui::actions!(terminal_compatibility, [BoundTerminalCommand]);

struct TerminalCommandHarness {
    view: Entity<TerminalView>,
    commands: usize,
}

impl gpui::Render for TerminalCommandHarness {
    fn render(
        &mut self,
        _: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{InteractiveElement, ParentElement, Styled};
        gpui::div()
            .size_full()
            .child(self.view.clone())
            .on_action(cx.listener(|this, _: &BoundTerminalCommand, _, _| this.commands += 1))
    }
}

#[gpui::test]
fn gpui_commands_precede_mode_aware_keyboard_delivery(cx: &mut TestAppContext) {
    use gpui::Focusable;
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("keyboard.hex");
    let expected = "\x1bOA\x1b[1;2D\x1b[15~\x1bb\0é".as_bytes();
    let script = format!(
        "stty raw -echo; printf '\\033[?1hready'; dd bs=1 count={} 2>/dev/null | od -An -tx1 > '{}'",
        expected.len(),
        destination.display()
    );
    let session = cx.new(|cx| TerminalSession::new_with_options(shell(&script), cx));
    wait_for(cx, &session, |session| output(session).contains("ready"));
    cx.update(|cx| {
        cx.bind_keys([gpui::KeyBinding::new(
            "ctrl-a",
            BoundTerminalCommand,
            Some("terminal"),
        )])
    });
    let (harness, cx) = cx.add_window_view(|_, cx| TerminalCommandHarness {
        view: cx.new(|cx| TerminalView::new(session.clone(), cx)),
        commands: 0,
    });
    cx.refresh().unwrap();
    cx.update(|window, cx| {
        harness
            .read(cx)
            .view
            .read(cx)
            .focus_handle(cx)
            .focus(window)
    });
    cx.simulate_keystrokes("ctrl-a up shift-left f5 alt-b ctrl-space");
    cx.simulate_event(gpui::KeyDownEvent {
        keystroke: gpui::Keystroke {
            key: "é".into(),
            key_char: Some("é".into()),
            ..Default::default()
        },
        is_held: false,
    });
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Exited(_))
    });
    let bytes: Vec<u8> = fs::read_to_string(destination)
        .unwrap()
        .split_whitespace()
        .map(|hex| u8::from_str_radix(hex, 16).unwrap())
        .collect();
    assert_eq!(bytes, expected);
    cx.read(|cx| assert_eq!(harness.read(cx).commands, 1));
    cx.update(|_, cx| session.update(cx, |session, cx| session.close(cx)));
}

#[gpui::test]
fn stopped_terminal_keeps_copy_and_declines_paste_and_detached_commands(cx: &mut TestAppContext) {
    use crate::app::commands::dispatch::CommandClaim;
    use crate::host::protocol::CommandOutcome;
    use alacritty_terminal::{
        index::{Column, Line, Point, Side},
        selection::{Selection, SelectionType},
    };
    let session = cx.new(|cx| TerminalSession::new_with_options(shell("printf 'copy me'"), cx));
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Exited(_))
    });
    let view = cx.new(|cx| TerminalView::new(session.clone(), cx));
    session.read_with(cx, |session, _| {
        let mut terminal = session.terminal().unwrap().lock();
        let mut selection = Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
            Side::Left,
        );
        selection.update(Point::new(Line(0), Column(6)), Side::Right);
        terminal.selection = Some(selection);
    });
    assert!(matches!(
        view.update(cx, |view, cx| view
            .execute_terminal_command(super::view::TerminalCommand::Copy, cx)),
        CommandClaim::Finished(CommandOutcome::Completed)
    ));
    cx.update(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("copy me")
        )
    });
    assert!(matches!(
        view.update(cx, |view, cx| view
            .execute_terminal_command(super::view::TerminalCommand::Paste, cx)),
        CommandClaim::Declined
    ));
    view.update(cx, |view, cx| view.detach(cx));
    assert!(matches!(
        view.update(cx, |view, cx| view
            .execute_terminal_command(super::view::TerminalCommand::Copy, cx)),
        CommandClaim::Declined
    ));
    session.update(cx, |session, cx| session.close(cx));
}

#[gpui::test]
fn vim_fixture_enters_alternate_screen_edits_and_exits(cx: &mut TestAppContext) {
    use alacritty_terminal::term::TermMode;
    if Command::new("vim").arg("--version").output().is_err() {
        eprintln!("Vim fixture unavailable on this host");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("fixture.txt");
    fs::write(&file, "before-vim\n").unwrap();
    let script = format!(
        "TERM=xterm-256color vim -Nu NONE -n -i NONE '{}'",
        file.display()
    );
    let session = cx.new(|cx| TerminalSession::new_with_options(shell(&script), cx));
    wait_for(cx, &session, |session| {
        output(session).contains("before-vim")
    });
    assert!(session.read_with(cx, |session, _| {
        session
            .terminal()
            .unwrap()
            .lock()
            .mode()
            .contains(TermMode::ALT_SCREEN)
    }));
    session.update(cx, |session, _| {
        session.send(b"gg0iinserted-\x1b:wq\r".to_vec())
    });
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Exited(_))
    });
    assert_eq!(fs::read_to_string(file).unwrap(), "inserted-before-vim\n");
    assert!(!session.read_with(cx, |session, _| {
        session
            .terminal()
            .unwrap()
            .lock()
            .mode()
            .contains(TermMode::ALT_SCREEN)
    }));
    session.update(cx, |session, cx| session.close(cx));
}

#[gpui::test]
fn less_fixture_pages_to_end_and_restores_the_primary_screen(cx: &mut TestAppContext) {
    use alacritty_terminal::term::TermMode;
    if Command::new("less").arg("--version").output().is_err() {
        eprintln!("less fixture unavailable on this host");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("fixture.txt");
    fs::write(
        &file,
        (1..=100)
            .map(|line| format!("fixture-line-{line:03}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let script = format!("TERM=xterm-256color LESS= less '{}'", file.display());
    let session = cx.new(|cx| TerminalSession::new_with_options(shell(&script), cx));
    wait_for(cx, &session, |session| {
        output(session).contains("fixture-line-001")
    });
    assert!(session.read_with(cx, |session, _| {
        session
            .terminal()
            .unwrap()
            .lock()
            .mode()
            .contains(TermMode::ALT_SCREEN)
    }));
    session.update(cx, |session, _| session.send(b" G".to_vec()));
    wait_for(cx, &session, |session| {
        output(session).contains("fixture-line-100")
    });
    session.update(cx, |session, _| session.send(b"q".to_vec()));
    wait_for(cx, &session, |session| {
        matches!(session.status(), TerminalStatus::Exited(_))
    });
    assert!(!session.read_with(cx, |session, _| {
        session
            .terminal()
            .unwrap()
            .lock()
            .mode()
            .contains(TermMode::ALT_SCREEN)
    }));
    session.update(cx, |session, cx| session.close(cx));
}
