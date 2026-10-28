use gpui::{Keystroke, Modifiers, TestAppContext};

use super::terminal_key_bytes;

#[gpui::test]
fn windows_space_key_without_key_char_reaches_terminal(_cx: &mut TestAppContext) {
    let space = Keystroke {
        key: "space".into(),
        key_char: None,
        modifiers: Modifiers::default(),
    };
    assert_eq!(
        terminal_key_bytes(&space, alacritty_terminal::term::TermMode::NONE),
        b" "
    );
}

#[cfg(unix)]
#[gpui::test]
fn measured_layout_drives_grid_and_unicode_selection(cx: &mut TestAppContext) {
    use super::*;
    use alacritty_terminal::{tty, vte::ansi::Processor};

    let session = cx.new(|cx| {
        TerminalSession::new_with_options(
            tty::Options {
                shell: Some(tty::Shell::new(
                    "/bin/sh".into(),
                    vec!["-c".into(), "read line".into()],
                )),
                ..Default::default()
            },
            cx,
        )
    });
    let (view, cx) = cx.add_window_view(|_, cx| TerminalView::new(session.clone(), cx));
    cx.refresh().unwrap();
    let initial = cx.read(|cx| session.read(cx).size());
    cx.simulate_resize(size(px(403.5), px(257.25)));
    cx.refresh().unwrap();
    cx.read(|cx| assert_ne!(session.read(cx).size(), initial));
    let (layout, bounds) = cx.update(|window, cx| {
        let view = view.read(cx);
        let layout = view.layout.unwrap();
        let measured = TerminalLayout::measure(window);
        assert_eq!(layout.cell_width, measured.cell_width);
        assert_eq!(layout.cell_height, measured.cell_height);
        assert_eq!(
            session.read(cx).size(),
            layout.grid_size(view.interaction_bounds)
        );
        (layout, view.interaction_bounds)
    });
    cx.update(|_, cx| {
        let terminal = session.read(cx).terminal().unwrap();
        let mut parser: Processor = Processor::new();
        parser.advance(
            &mut *terminal.lock(),
            "\x1b[2J\x1b[HA界e\u{301}Z".as_bytes(),
        );
        let snapshot = TerminalSnapshot::capture(view.read(cx), cx);
        let wide = snapshot
            .cells
            .iter()
            .find(|cell| cell.text == "界")
            .unwrap();
        assert_eq!(wide.column, 1);
        assert!(wide.flags.contains(Flags::WIDE_CHAR));
        assert!(
            !snapshot
                .cells
                .iter()
                .any(|cell| cell.line == 0 && cell.column == 2)
        );
        assert!(
            snapshot
                .cells
                .iter()
                .any(|cell| cell.column == 3 && cell.text == "e\u{301}")
        );
    });
    let start = layout.origin(bounds, 3, 0) + point(px(0.1), px(0.1));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(
        start + point(layout.cell_width / 2., px(0.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::default());
    cx.read(|cx| {
        assert_eq!(
            session
                .read(cx)
                .terminal()
                .unwrap()
                .lock()
                .selection_to_string()
                .as_deref(),
            Some("e\u{301}")
        );
    });
    cx.update(|_, cx| session.update(cx, |session, cx| session.close(cx)));
}
