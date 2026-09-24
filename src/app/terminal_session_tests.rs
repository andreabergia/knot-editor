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

use super::{
    terminal_session::{TerminalSession, TerminalSize, TerminalStatus},
    terminal_view::TerminalView,
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
            shell("printf 'before-rebuild\\n'; IFS= read -r line; printf 'after:%s\\n' \"$line\""),
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
        std::sync::Arc::ptr_eq(&terminal_before, session.terminal().unwrap())
    }));
    session.update(cx, |session, _| session.send(b"hello\r".to_vec()));
    wait_for(cx, &session, |session| {
        output(session).contains("after:hello")
    });
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
