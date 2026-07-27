use std::{borrow::Cow, sync::Arc, thread::JoinHandle};

use alacritty_terminal::{
    event::{VoidListener, WindowSize},
    event_loop::{EventLoop, EventLoopSender, Msg, State},
    grid::Dimensions,
    sync::FairMutex,
    term::{Config, Term},
    tty::{self, Options},
};
use gpui::*;

const INITIAL_COLUMNS: usize = 80;
const INITIAL_LINES: usize = 24;
const INITIAL_CELL_WIDTH: u16 = 8;
const INITIAL_CELL_HEIGHT: u16 = 16;

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

type PtyEventLoop = EventLoop<tty::Pty, VoidListener>;
type PtyThread = JoinHandle<(PtyEventLoop, State)>;

struct TerminalSession {
    terminal: Arc<FairMutex<Term<VoidListener>>>,
    sender: EventLoopSender,
    thread: Option<PtyThread>,
}

impl TerminalSession {
    fn start() -> std::io::Result<Self> {
        let dimensions = TerminalDimensions {
            columns: INITIAL_COLUMNS,
            lines: INITIAL_LINES,
        };
        let window_size = WindowSize {
            num_lines: INITIAL_LINES as u16,
            num_cols: INITIAL_COLUMNS as u16,
            cell_width: INITIAL_CELL_WIDTH,
            cell_height: INITIAL_CELL_HEIGHT,
        };
        let terminal = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &dimensions,
            VoidListener,
        )));
        let options = Options {
            working_directory: std::env::current_dir().ok(),
            drain_on_exit: true,
            env: [
                ("TERM".to_owned(), "xterm-256color".to_owned()),
                ("COLORTERM".to_owned(), "truecolor".to_owned()),
            ]
            .into(),
            ..Options::default()
        };
        let pty = tty::new(&options, window_size, 0)?;
        let event_loop = EventLoop::new(
            terminal.clone(),
            VoidListener,
            pty,
            options.drain_on_exit,
            false,
        )?;
        let sender = event_loop.channel();
        let thread = event_loop.spawn();

        Ok(Self {
            terminal,
            sender,
            thread: Some(thread),
        })
    }

    fn send(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.sender.send(Msg::Input(Cow::Owned(bytes)));
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Native terminal surface owning one authoritative PTY session and emulator grid.
pub(crate) struct TerminalView {
    session: Result<TerminalSession, String>,
    focus: FocusHandle,
}

impl TerminalView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            session: TerminalSession::start().map_err(|error| error.to_string()),
            focus: cx.focus_handle(),
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Ok(session) = &self.session else {
            return;
        };
        let keystroke = &event.keystroke;
        if keystroke.modifiers.platform {
            return;
        }

        let key = keystroke.key.to_lowercase();
        let mut bytes = match key.as_str() {
            "enter" => b"\r".to_vec(),
            "backspace" => vec![0x7f],
            "tab" => b"\t".to_vec(),
            "escape" => vec![0x1b],
            "up" => b"\x1b[A".to_vec(),
            "down" => b"\x1b[B".to_vec(),
            "right" => b"\x1b[C".to_vec(),
            "left" => b"\x1b[D".to_vec(),
            "home" => b"\x1b[H".to_vec(),
            "end" => b"\x1b[F".to_vec(),
            "delete" => b"\x1b[3~".to_vec(),
            "pageup" => b"\x1b[5~".to_vec(),
            "pagedown" => b"\x1b[6~".to_vec(),
            _ if keystroke.modifiers.control => control_byte(&key).into_iter().collect(),
            _ => keystroke
                .key_char
                .as_deref()
                .unwrap_or_default()
                .as_bytes()
                .to_vec(),
        };

        if keystroke.modifiers.alt && !bytes.is_empty() {
            bytes.insert(0, 0x1b);
        }
        if !bytes.is_empty() {
            session.send(bytes);
            cx.stop_propagation();
        }
    }
}

fn control_byte(key: &str) -> Option<u8> {
    let byte = key.as_bytes().first().copied()?;
    match byte {
        b'@'..=b'_' => Some(byte & 0x1f),
        b'a'..=b'z' => Some(byte - b'a' + 1),
        b'?' => Some(0x7f),
        _ => None,
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let status = match &self.session {
            Ok(session) => {
                let terminal = session.terminal.lock();
                format!(
                    "Terminal {}×{} · interactive shell running",
                    terminal.columns(),
                    terminal.screen_lines()
                )
            }
            Err(error) => format!("Terminal failed to start: {error}"),
        };

        div()
            .size_full()
            .px_2()
            .py_1()
            .bg(rgb(0x181818))
            .text_color(rgb(0x888888))
            .text_sm()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(status)
    }
}
