use alacritty_terminal::{
    event::VoidListener,
    grid::Dimensions,
    term::{Config, Term},
};
use gpui::*;

const INITIAL_COLUMNS: usize = 80;
const INITIAL_LINES: usize = 24;

struct TerminalDimensions {
    columns: usize,
    lines: usize,
}

impl Dimensions for TerminalDimensions {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// Native terminal surface owning one authoritative emulator grid.
pub(crate) struct TerminalView {
    terminal: Term<VoidListener>,
    focus: FocusHandle,
}

impl TerminalView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let size = TerminalDimensions {
            columns: INITIAL_COLUMNS,
            lines: INITIAL_LINES,
        };
        Self {
            terminal: Term::new(Config::default(), &size, VoidListener),
            focus: cx.focus_handle(),
        }
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .px_2()
            .py_1()
            .bg(rgb(0x181818))
            .text_color(rgb(0x888888))
            .text_sm()
            .track_focus(&self.focus)
            .child(format!(
                "Terminal {}×{} · session not started",
                self.terminal.columns(),
                self.terminal.screen_lines()
            ))
    }
}
