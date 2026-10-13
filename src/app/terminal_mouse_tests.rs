use super::{Scroll, TerminalSession, TerminalStatus, TerminalView};
use alacritty_terminal::{tty, vte::ansi::Processor};
use gpui::{
    App, AppContext, Entity, Modifiers, MouseButton, Pixels, Point, ScrollDelta, ScrollWheelEvent,
    TestAppContext, point, px,
};
use std::{
    fs, thread,
    time::{Duration, Instant},
};

struct Fixture {
    _directory: tempfile::TempDir,
    capture: std::path::PathBuf,
    session: Entity<TerminalSession>,
}

fn wait(cx: &mut TestAppContext, condition: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        cx.run_until_parked();
        if cx.read(&condition) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("mouse fixture timed out");
}

fn fixture(cx: &mut TestAppContext, modes: &str, byte_count: usize) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let capture = directory.path().join("mouse.hex");
    let script = format!(
        "stty raw -echo; printf '{modes}ready'; dd bs=1 count={} 2>/dev/null | od -An -v -tx1 > '{}'",
        byte_count + 1,
        capture.display()
    );
    let session = cx.new(|cx| {
        TerminalSession::new_with_options(
            tty::Options {
                shell: Some(tty::Shell::new("/bin/sh".into(), vec!["-c".into(), script])),
                drain_on_exit: true,
                ..Default::default()
            },
            cx,
        )
    });
    wait(cx, |cx| {
        session
            .read(cx)
            .terminal()
            .unwrap()
            .lock()
            .renderable_content()
            .display_iter
            .map(|cell| cell.cell.c)
            .collect::<String>()
            .contains("ready")
    });
    Fixture {
        _directory: directory,
        capture,
        session,
    }
}

fn cell(view: &Entity<TerminalView>, cx: &App, column: usize, row: usize) -> Point<Pixels> {
    let view = view.read(cx);
    view.layout
        .unwrap()
        .origin(view.interaction_bounds, column, row)
        + point(px(0.1), px(0.1))
}

fn parse(fixture: &Fixture, bytes: &[u8], cx: &App) {
    let mut parser: Processor = Processor::new();
    parser.advance(
        &mut *fixture.session.read(cx).terminal().unwrap().lock(),
        bytes,
    );
}

fn finish(fixture: &Fixture, expected: &[u8], cx: &mut TestAppContext) {
    fixture
        .session
        .read_with(cx, |session, _| session.send(b"X".to_vec()));
    wait(cx, |cx| {
        matches!(fixture.session.read(cx).status(), TerminalStatus::Exited(_))
    });
    let bytes: Vec<u8> = fs::read_to_string(&fixture.capture)
        .unwrap()
        .split_whitespace()
        .map(|hex| u8::from_str_radix(hex, 16).unwrap())
        .collect();
    assert_eq!(bytes, [expected, b"X"].concat());
    fixture.session.update(cx, |session, cx| session.close(cx));
}

#[gpui::test]
fn click_tracking_reports_buttons_and_wheels_without_selection_or_motion(cx: &mut TestAppContext) {
    let expected = b"\x1b[<0;3;2M\x1b[<0;4;2m\x1b[<1;3;2M\x1b[<1;3;2m\x1b[<26;3;2M\x1b[<26;3;2m\x1b[<64;3;2M\x1b[<65;3;2M\x1b[<66;3;2M\x1b[<67;3;2M";
    let fixture = fixture(cx, "\\033[?1000h\\033[?1006h", expected.len());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.refresh().unwrap();
    let start = cx.read(|cx| cell(&view, cx, 2, 1));
    let moved = cx.read(|cx| cell(&view, cx, 3, 1));
    cx.simulate_mouse_move(start, None, Modifiers::default());
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(moved, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(moved, MouseButton::Left, Modifiers::default());
    for button in [MouseButton::Middle, MouseButton::Right] {
        let modifiers = if button == MouseButton::Right {
            Modifiers {
                control: true,
                alt: true,
                ..Default::default()
            }
        } else {
            Modifiers::default()
        };
        cx.simulate_mouse_down(start, button, modifiers);
        cx.simulate_mouse_up(start, button, modifiers);
    }
    for delta in [point(0., 1.), point(0., -1.), point(1., 0.), point(-1., 0.)] {
        cx.simulate_event(ScrollWheelEvent {
            position: start,
            delta: ScrollDelta::Lines(delta),
            ..Default::default()
        });
    }
    cx.read(|cx| {
        assert!(
            fixture
                .session
                .read(cx)
                .terminal()
                .unwrap()
                .lock()
                .selection
                .is_none()
        )
    });
    finish(&fixture, expected, cx);
}

#[gpui::test]
fn drag_tracking_owns_release_outside_bounds_and_ignores_foreign_drags(cx: &mut TestAppContext) {
    let fixture = fixture(cx, "\\033[?1002h\\033[?1006h", 200);
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.refresh().unwrap();
    let (start, moved, outside, grid) = cx.read(|cx| {
        let bounds = view.read(cx).interaction_bounds;
        (
            cell(&view, cx, 1, 0),
            cell(&view, cx, 2, 1),
            bounds.origin + point(bounds.size.width + px(20.), bounds.size.height + px(20.)),
            fixture.session.read(cx).size(),
        )
    });
    let expected = format!(
        "\x1b[<0;2;1M\x1b[<32;3;2M\x1b[<32;{};{}M\x1b[<4;{};{}m",
        grid.columns, grid.lines, grid.columns, grid.lines
    );
    cx.simulate_mouse_move(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        start + point(px(0.2), px(0.2)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_move(moved, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(
        outside,
        MouseButton::Left,
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    // Complete the fixed byte-count capture with padding, then inspect only actual reports.
    fixture.session.read_with(cx, |session, _| {
        session.send(vec![b'X'; 201 - expected.len()])
    });
    wait(cx, |cx| {
        matches!(fixture.session.read(cx).status(), TerminalStatus::Exited(_))
    });
    let bytes: Vec<u8> = fs::read_to_string(&fixture.capture)
        .unwrap()
        .split_whitespace()
        .map(|hex| u8::from_str_radix(hex, 16).unwrap())
        .collect();
    assert_eq!(&bytes[..expected.len()], expected.as_bytes());
    assert!(bytes[expected.len()..].iter().all(|byte| *byte == b'X'));
    fixture.session.update(cx, |session, cx| session.close(cx));
}

#[gpui::test]
fn all_motion_legacy_reports_use_viewport_cells_and_deduplicate(cx: &mut TestAppContext) {
    let expected = b"\x1b[MC#\"\x1b[M #\"\x1b[M@%\"\x1b[M#%\"";
    let fixture = fixture(cx, "\\033[?1003h", expected.len());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.refresh().unwrap();
    let start = cx.read(|cx| cell(&view, cx, 2, 1));
    let moved = cx.read(|cx| cell(&view, cx, 4, 1));
    cx.simulate_mouse_move(start, None, Modifiers::default());
    cx.simulate_mouse_move(start + point(px(0.2), px(0.2)), None, Modifiers::default());
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(moved, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(moved, MouseButton::Left, Modifiers::default());
    finish(&fixture, expected, cx);
}

#[gpui::test]
fn shift_selection_and_scrolling_stay_local_in_tracking_scrollback(cx: &mut TestAppContext) {
    use super::super::product_commands::{COPY_COMMAND, CommandClaim};
    use crate::host::protocol::{Command, CommandArgumentValue, CommandOutcome};
    let expected = b"\x1b[<0;2;1M\x1b[<0;2;1m\x1b[<65;2;1M";
    let fixture = fixture(cx, "\\033[?1002h\\033[?1006h", expected.len());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.refresh().unwrap();
    cx.update(|_, cx| {
        parse(
            &fixture,
            format!(
                "\x1b[2J\x1b[3J\x1b[H{}",
                "history-A界e\u{301}\r\n".repeat(fixture.session.read(cx).size().lines + 30)
            )
            .as_bytes(),
            cx,
        );
        fixture
            .session
            .read(cx)
            .terminal()
            .unwrap()
            .lock()
            .scroll_display(Scroll::Top);
    });
    let start = cx.read(|cx| cell(&view, cx, 0, 0));
    let end = cx.read(|cx| cell(&view, cx, 3, 0));
    let reported = cx.read(|cx| cell(&view, cx, 1, 0));
    let shift = Modifiers {
        shift: true,
        ..Default::default()
    };
    cx.simulate_mouse_down(start, MouseButton::Left, shift);
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    cx.update(|_, cx| {
        assert_eq!(
            fixture
                .session
                .read(cx)
                .terminal()
                .unwrap()
                .lock()
                .selection_to_string()
                .as_deref(),
            Some("hist")
        );
        assert!(matches!(
            view.update(cx, |view, cx| view.handle_command(
                &Command {
                    name: COPY_COMMAND.into(),
                    arguments: CommandArgumentValue::Null
                },
                cx
            )),
            CommandClaim::Finished(CommandOutcome::Completed)
        ));
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("hist")
        );
    });
    let offset = cx.read(|cx| {
        fixture
            .session
            .read(cx)
            .terminal()
            .unwrap()
            .lock()
            .grid()
            .display_offset()
    });
    cx.simulate_event(ScrollWheelEvent {
        position: start,
        delta: ScrollDelta::Lines(point(0., -1.)),
        modifiers: shift,
        ..Default::default()
    });
    cx.read(|cx| {
        assert_eq!(
            fixture
                .session
                .read(cx)
                .terminal()
                .unwrap()
                .lock()
                .grid()
                .display_offset(),
            offset - 1
        )
    });
    cx.simulate_mouse_down(reported, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(reported, MouseButton::Left, Modifiers::default());
    cx.simulate_event(ScrollWheelEvent {
        position: reported,
        delta: ScrollDelta::Lines(point(0., -1.)),
        ..Default::default()
    });
    cx.read(|cx| {
        assert_eq!(
            fixture
                .session
                .read(cx)
                .terminal()
                .unwrap()
                .lock()
                .grid()
                .display_offset(),
            offset - 1
        )
    });
    finish(&fixture, expected, cx);
}

#[gpui::test]
fn wheel_accumulation_and_alternate_scroll_use_one_current_route(cx: &mut TestAppContext) {
    let expected = b"\x1b[<64;2;1M\x1bOA\x1bOA\x1bOD\x1b[A";
    let fixture = fixture(cx, "\\033[?1000h\\033[?1006h", expected.len());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.refresh().unwrap();
    let position = cx.read(|cx| cell(&view, cx, 1, 0));
    let height = cx.read(|cx| view.read(cx).layout.unwrap().cell_height);
    for _ in 0..4 {
        cx.simulate_event(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Pixels(point(px(0.), height / 4.)),
            ..Default::default()
        });
    }
    cx.update(|_, cx| parse(&fixture, b"\x1b[?1000l\x1b[?1049h\x1b[?1h", cx));
    for delta in [point(0., 2.), point(1., 0.)] {
        cx.simulate_event(ScrollWheelEvent {
            position,
            delta: ScrollDelta::Lines(delta),
            ..Default::default()
        });
    }
    // Shift and disabling alternate scroll never emit synthetic cursor input.
    cx.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Lines(point(0., 1.)),
        modifiers: Modifiers {
            shift: true,
            ..Default::default()
        },
        ..Default::default()
    });
    cx.update(|_, cx| parse(&fixture, b"\x1b[?1007l", cx));
    cx.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Lines(point(0., 1.)),
        ..Default::default()
    });
    cx.update(|_, cx| parse(&fixture, b"\x1b[?1007h\x1b[?1l", cx));
    cx.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Lines(point(0., 1.)),
        ..Default::default()
    });
    finish(&fixture, expected, cx);
}

#[gpui::test]
fn mode_changes_clear_old_drags_even_when_tracking_returns_to_the_same_mode(
    cx: &mut TestAppContext,
) {
    let expected = b"\x1b[<0;2;1M\x1b[<2;2;1M\x1b[<2;2;1m";
    let fixture = fixture(cx, "\\033[?1002h\\033[?1006h", expected.len());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.refresh().unwrap();
    let position = cx.read(|cx| cell(&view, cx, 1, 0));
    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
    cx.update(|_, cx| parse(&fixture, b"\x1b[?1002l\x1b[?1002h", cx));
    cx.simulate_mouse_move(
        position + point(px(30.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    finish(&fixture, expected, cx);
}

#[gpui::test]
fn detach_balances_reported_buttons_before_new_attachment_and_ignores_stale_events(
    cx: &mut TestAppContext,
) {
    let expected = b"\x1b[<0;2;1M\x1b[<0;2;1m\x1b[<2;2;1M\x1b[<2;2;1m";
    let fixture = fixture(cx, "\\033[?1002h\\033[?1006h", expected.len());
    let (source, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.refresh().unwrap();
    let position = cx.read(|cx| cell(&source, cx, 1, 0));
    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
    cx.update(|_, cx| source.update(cx, |view, cx| view.detach(cx)));
    cx.simulate_mouse_move(
        position + point(px(30.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
    let (target, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.refresh().unwrap();
    let position = cx.read(|cx| cell(&target, cx, 1, 0));
    cx.simulate_mouse_move(position, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    finish(&fixture, expected, cx);
}

#[gpui::test]
fn restart_cannot_deliver_old_mouse_release_to_the_new_pty(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("first-process");
    let capture = directory.path().join("restart.hex");
    let script = format!(
        "stty raw -echo; if [ -e '{}' ]; then printf '\\033[?1002h\\033[?1006hready'; dd bs=1 count=1 2>/dev/null | od -An -v -tx1 > '{}'; else touch '{}'; printf '\\033[?1002h\\033[?1006hready'; exec sleep 30; fi",
        marker.display(),
        capture.display(),
        marker.display()
    );
    let session = cx.new(|cx| {
        TerminalSession::new_with_options(
            tty::Options {
                shell: Some(tty::Shell::new("/bin/sh".into(), vec!["-c".into(), script])),
                drain_on_exit: true,
                ..Default::default()
            },
            cx,
        )
    });
    let fixture = Fixture {
        _directory: directory,
        capture,
        session,
    };
    wait(cx, |cx| {
        fixture
            .session
            .read(cx)
            .terminal()
            .unwrap()
            .lock()
            .renderable_content()
            .display_iter
            .map(|cell| cell.cell.c)
            .collect::<String>()
            .contains("ready")
    });
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.refresh().unwrap();
    let position = cx.read(|cx| cell(&view, cx, 1, 0));
    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
    cx.update(|_, cx| view.update(cx, |view, cx| view.restart(cx)));
    wait(cx, |cx| {
        fixture
            .session
            .read(cx)
            .terminal()
            .unwrap()
            .lock()
            .renderable_content()
            .display_iter
            .map(|cell| cell.cell.c)
            .collect::<String>()
            .contains("ready")
    });
    cx.simulate_mouse_move(
        position + point(px(30.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
    finish(&fixture, b"", cx);
}

#[gpui::test]
fn native_mouse_fixture_decodes_gpui_reports_and_restores_modes_on_quit(cx: &mut TestAppContext) {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("Python mouse fixture unavailable on this host");
        return;
    }
    for encoding in ["sgr", "legacy", "utf8"] {
        let script = format!(
            "python3 '{}/scripts/terminal-mouse-fixture.py' --encoding {encoding}",
            env!("CARGO_MANIFEST_DIR")
        );
        let session = cx.new(|cx| {
            TerminalSession::new_with_options(
                tty::Options {
                    shell: Some(tty::Shell::new("/bin/sh".into(), vec!["-c".into(), script])),
                    drain_on_exit: true,
                    ..Default::default()
                },
                cx,
            )
        });
        let text = |cx: &App| {
            session
                .read(cx)
                .terminal()
                .unwrap()
                .lock()
                .renderable_content()
                .display_iter
                .map(|cell| cell.cell.c)
                .collect::<String>()
        };
        wait(cx, |cx| text(cx).contains("Mouse fixture:"));
        let (view, window_cx) = cx.add_window_view(|_, cx| TerminalView::new(session.clone(), cx));
        window_cx.simulate_resize(gpui::size(px(4000.), px(300.)));
        window_cx.refresh().unwrap();
        let position = window_cx.read(|cx| cell(&view, cx, 100, 0));
        for button in [
            MouseButton::Left,
            MouseButton::Navigate(gpui::NavigationDirection::Back),
        ] {
            window_cx.simulate_mouse_down(position, button, Modifiers::default());
            window_cx.simulate_mouse_up(position, button, Modifiers::default());
        }
        let release_code = if encoding == "sgr" { 128 } else { 3 };
        wait(window_cx, |cx| {
            text(cx).contains(&format!("4: release code={release_code} cell=101,1"))
        });
        window_cx.update(|_, cx| session.read(cx).send(b"q".to_vec()));
        wait(window_cx, |cx| {
            matches!(session.read(cx).status(), TerminalStatus::Exited(_))
        });
        window_cx.read(|cx| {
            assert!(
                !session
                    .read(cx)
                    .terminal()
                    .unwrap()
                    .lock()
                    .mode()
                    .intersects(alacritty_terminal::term::TermMode::MOUSE_MODE)
            )
        });
        window_cx.update(|_, cx| session.update(cx, |session, cx| session.close(cx)));
    }
}

#[gpui::test]
fn wide_spacer_mouse_coordinates_stay_distinct_and_shift_selection_expands_the_glyph(
    cx: &mut TestAppContext,
) {
    let expected = b"\x1b[<0;3;1M\x1b[<0;3;1m";
    let fixture = fixture(cx, "\\033[?1000h\\033[?1006h", expected.len());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.refresh().unwrap();
    cx.update(|_, cx| parse(&fixture, "\x1b[2J\x1b[3J\x1b[HA界e\u{301}Z".as_bytes(), cx));
    let spacer = cx.read(|cx| cell(&view, cx, 2, 0));
    cx.simulate_mouse_down(spacer, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(spacer, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_down(
        spacer,
        MouseButton::Left,
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    cx.simulate_mouse_move(
        spacer + point(px(1.), px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(spacer, MouseButton::Left, Modifiers::default());
    cx.read(|cx| {
        assert_eq!(
            fixture
                .session
                .read(cx)
                .terminal()
                .unwrap()
                .lock()
                .selection_to_string()
                .as_deref(),
            Some("界")
        )
    });
    finish(&fixture, expected, cx);
}

#[gpui::test]
fn utf8_reporting_encodes_large_coordinates_and_extended_button_codes(cx: &mut TestAppContext) {
    let expected = "\x1b[M \u{100}!\x1b[M#\u{100}!\x1b[M\u{a0}\u{100}!\x1b[M#\u{100}!".as_bytes();
    let fixture = fixture(cx, "\\033[?1000h\\033[?1005h", expected.len());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.simulate_resize(gpui::size(px(4000.), px(300.)));
    cx.refresh().unwrap();
    let position = cx.read(|cx| cell(&view, cx, 223, 0));
    for button in [
        MouseButton::Left,
        MouseButton::Navigate(gpui::NavigationDirection::Back),
    ] {
        cx.simulate_mouse_down(position, button, Modifiers::default());
        cx.simulate_mouse_up(position, button, Modifiers::default());
    }
    finish(&fixture, expected, cx);
}

#[gpui::test]
fn geometry_and_focus_changes_cancel_owned_reports_and_reset_releases(cx: &mut TestAppContext) {
    let expected = b"\x1b[<0;2;1M\x1b[<0;2;1m\x1b[<2;2;1M\x1b[<2;2;1m";
    let fixture = fixture(cx, "\\033[?1002h\\033[?1006h", expected.len());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.refresh().unwrap();
    let position = cx.read(|cx| cell(&view, cx, 1, 0));
    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
    cx.simulate_resize(gpui::size(px(403.5), px(257.25)));
    cx.refresh().unwrap();
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
    let position = cx.read(|cx| cell(&view, cx, 1, 0));
    cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    cx.update(|window, _| window.blur());
    cx.run_until_parked();
    cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    finish(&fixture, expected, cx);
}

#[gpui::test]
fn legacy_wire_limit_ignores_unrepresentable_presses_and_balances_owned_releases(
    cx: &mut TestAppContext,
) {
    let expected = &[27, 91, 77, 32, 255, 33, 27, 91, 77, 35, 255, 33];
    let fixture = fixture(cx, "\\033[?1002h", expected.len());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.simulate_resize(gpui::size(px(4000.), px(300.)));
    cx.refresh().unwrap();
    let valid = cx.read(|cx| cell(&view, cx, 222, 0));
    let invalid = cx.read(|cx| cell(&view, cx, 223, 0));
    cx.simulate_mouse_down(invalid, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(valid, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(valid, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_down(valid, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(invalid, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(invalid, MouseButton::Left, Modifiers::default());
    finish(&fixture, expected, cx);
}

#[gpui::test]
fn mouse_reporting_toggle_balances_drag_then_supports_ordinary_selection_and_copy(
    cx: &mut TestAppContext,
) {
    use super::super::product_commands::{
        COPY_COMMAND, CommandClaim, TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND,
    };
    use crate::host::protocol::{Command, CommandArgumentValue, CommandOutcome};
    let expected = b"\x1b[<0;2;1M\x1b[<0;2;1m\x1b[<2;2;1M\x1b[<2;2;1m";
    let fixture = fixture(cx, "\\033[?1002h\\033[?1006h", expected.len());
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(fixture.session.clone(), cx));
    cx.refresh().unwrap();
    let toggle = Command {
        name: TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND.into(),
        arguments: CommandArgumentValue::Null,
    };
    let position = cx.read(|cx| cell(&view, cx, 1, 0));
    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
    cx.update(|_, cx| {
        assert!(fixture.session.read(cx).mouse_reporting_enabled());
        assert!(matches!(
            view.update(cx, |view, cx| view.handle_command(&toggle, cx)),
            CommandClaim::Finished(CommandOutcome::Completed)
        ));
        assert!(!fixture.session.read(cx).mouse_reporting_enabled());
        parse(&fixture, b"\x1b[2J\x1b[3J\x1b[Hcopy me", cx);
    });
    cx.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
    let (start, end) = cx.read(|cx| (cell(&view, cx, 0, 0), cell(&view, cx, 3, 0)));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    cx.update(|_, cx| {
        assert!(matches!(
            view.update(cx, |view, cx| view.handle_command(
                &Command {
                    name: COPY_COMMAND.into(),
                    arguments: CommandArgumentValue::Null
                },
                cx
            )),
            CommandClaim::Finished(CommandOutcome::Completed)
        ));
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("copy")
        );
        view.update(cx, |view, cx| {
            view.handle_command(&toggle, cx);
        });
        assert!(fixture.session.read(cx).mouse_reporting_enabled());
    });
    cx.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    finish(&fixture, expected, cx);
}

#[gpui::test]
fn mouse_reporting_preference_survives_view_handoff_and_restart_and_rejects_detached_toggle(
    cx: &mut TestAppContext,
) {
    use super::super::product_commands::{CommandClaim, TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND};
    use crate::host::protocol::{Command, CommandArgumentValue};
    let fixture = fixture(cx, "\\033[?1002h\\033[?1006h", 0);
    let view = cx.new(|cx| TerminalView::new(fixture.session.clone(), cx));
    let toggle = Command {
        name: TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND.into(),
        arguments: CommandArgumentValue::Null,
    };
    view.update(cx, |view, cx| {
        view.handle_command(&toggle, cx);
        view.detach(cx);
    });
    assert!(matches!(
        view.update(cx, |view, cx| view.handle_command(&toggle, cx)),
        CommandClaim::Declined
    ));
    let rebuilt = cx.new(|cx| TerminalView::new(fixture.session.clone(), cx));
    cx.read(|cx| assert!(!fixture.session.read(cx).mouse_reporting_enabled()));
    rebuilt.update(cx, |view, cx| view.restart(cx));
    cx.read(|cx| assert!(!fixture.session.read(cx).mouse_reporting_enabled()));
    fixture.session.update(cx, |session, cx| session.close(cx));
}
