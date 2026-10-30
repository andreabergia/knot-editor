use std::{
    borrow::Cow,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::JoinHandle,
};

use alacritty_terminal::{
    event::{Event, EventListener, WindowSize},
    event_loop::{EventLoop, EventLoopSender, Msg, State},
    grid::Dimensions,
    sync::FairMutex,
    term::{Config, MIN_COLUMNS, MIN_SCREEN_LINES, Term},
    tty::{self, Options},
};
use gpui::{Context, Task};

const INITIAL_COLUMNS: usize = 80;
const INITIAL_LINES: usize = 11;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalSize {
    pub(crate) columns: usize,
    pub(crate) lines: usize,
    pub(crate) cell_width: u16,
    pub(crate) cell_height: u16,
}

impl TerminalSize {
    pub(crate) fn initial() -> Self {
        Self {
            columns: INITIAL_COLUMNS,
            lines: INITIAL_LINES,
            // The attached view supplies measured pixel dimensions on first layout.
            cell_width: 1,
            cell_height: 1,
        }
    }

    pub(crate) fn clamped(mut self) -> Self {
        self.columns = self.columns.clamp(MIN_COLUMNS, u16::MAX as usize);
        self.lines = self.lines.clamp(MIN_SCREEN_LINES, u16::MAX as usize);
        self
    }

    fn window_size(self) -> WindowSize {
        WindowSize {
            num_lines: self.lines as u16,
            num_cols: self.columns as u16,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

impl Dimensions for TerminalSize {
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

#[derive(Clone)]
pub(crate) struct TerminalEventListener {
    wakeups: tokio::sync::mpsc::Sender<()>,
    events: Arc<Mutex<Vec<Event>>>,
    pointer_revision: Arc<AtomicU64>,
}

impl EventListener for TerminalEventListener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(_)
            | Event::ColorRequest(..)
            | Event::TextAreaSizeRequest(_)
            | Event::ChildExit(_) => {
                self.events
                    .lock()
                    .expect("terminal event queue poisoned")
                    .push(event);
            }
            Event::MouseCursorDirty => {
                self.pointer_revision.fetch_add(1, Ordering::AcqRel);
            }
            Event::Wakeup | Event::CursorBlinkingChange | Event::Exit => {}
            // OSC 52, title, and bell have no application behavior here.
            _ => return,
        }
        let _ = self.wakeups.try_send(());
    }
}

type PtyEventLoop = EventLoop<tty::Pty, TerminalEventListener>;
type PtyThread = JoinHandle<(PtyEventLoop, State)>;

struct RunningTerminal {
    terminal: Arc<FairMutex<Term<TerminalEventListener>>>,
    sender: EventLoopSender,
    thread: Option<PtyThread>,
    pid: Option<u32>,
}

impl RunningTerminal {
    fn start(
        listener: TerminalEventListener,
        size: TerminalSize,
        options: &Options,
    ) -> std::io::Result<Self> {
        let terminal = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            listener.clone(),
        )));
        let pty = tty::new(options, size.window_size(), 0)?;
        #[cfg(unix)]
        let pid = Some(pty.child().id());
        #[cfg(not(unix))]
        let pid = None;
        let event_loop = EventLoop::new(
            terminal.clone(),
            listener,
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
            pid,
        })
    }

    fn shutdown(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
        if let Some(thread) = self.thread.take() {
            std::thread::spawn(move || {
                let _ = thread.join();
            });
        }
    }
}

impl Drop for RunningTerminal {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalStatus {
    Running,
    Exited(String),
    Failed(String),
    Closed,
}

enum TerminalState {
    Running(RunningTerminal),
    Stopped {
        terminal: Arc<FairMutex<Term<TerminalEventListener>>>,
        status: TerminalStatus,
    },
    Failed(String),
    Closed,
}

/// Owns one PTY, emulator grid, and process independently of any presentation.
pub(crate) struct TerminalSession {
    state: TerminalState,
    size: TerminalSize,
    options: Options,
    generation: u64,
    _wakeup_task: Option<Task<()>>,
    attachment: Arc<AtomicU64>,
    next_attachment: u64,
    focused: bool,
    mouse_reporting_enabled: bool,
    pointer_revision: Arc<AtomicU64>,
}

pub(crate) struct TerminalAttachment {
    active: Arc<AtomicU64>,
    id: u64,
}

impl Drop for TerminalAttachment {
    fn drop(&mut self) {
        let _ = self
            .active
            .compare_exchange(self.id, 0, Ordering::AcqRel, Ordering::Acquire);
    }
}

impl TerminalSession {
    pub(crate) fn new(
        working_directory: Option<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let options = Options {
            working_directory,
            drain_on_exit: true,
            env: [
                ("TERM".to_owned(), "xterm-256color".to_owned()),
                ("COLORTERM".to_owned(), "truecolor".to_owned()),
            ]
            .into(),
            ..Options::default()
        };
        Self::new_with_options(options, cx)
    }

    pub(crate) fn new_with_options(options: Options, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            state: TerminalState::Closed,
            size: TerminalSize::initial(),
            options,
            generation: 0,
            _wakeup_task: None,
            attachment: Arc::new(AtomicU64::new(0)),
            next_attachment: 0,
            focused: false,
            mouse_reporting_enabled: true,
            pointer_revision: Arc::new(AtomicU64::new(0)),
        };
        this.start(cx);
        this
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let (wakeup_tx, mut wakeup_rx) = tokio::sync::mpsc::channel(1);
        let events = Arc::new(Mutex::new(Vec::new()));
        let pointer_revision = Arc::new(AtomicU64::new(0));
        let listener = TerminalEventListener {
            wakeups: wakeup_tx,
            events: events.clone(),
            pointer_revision: pointer_revision.clone(),
        };
        self.pointer_revision = pointer_revision;
        self.state = match RunningTerminal::start(listener, self.size, &self.options) {
            Ok(running) => TerminalState::Running(running),
            Err(error) => TerminalState::Failed(error.to_string()),
        };
        if let Some(terminal) = self.terminal() {
            terminal.lock().is_focused = self.focused;
        }
        self._wakeup_task = Some(cx.spawn(async move |this, cx| {
            while wakeup_rx.recv().await.is_some() {
                let pending =
                    std::mem::take(&mut *events.lock().expect("terminal event queue poisoned"));
                if this
                    .update(cx, |session, cx| {
                        session.apply_events(generation, pending);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn apply_events(&mut self, generation: u64, events: Vec<Event>) {
        if self.generation != generation {
            return;
        }
        for event in events {
            match event {
                Event::PtyWrite(text) => self.send(text.into_bytes()),
                Event::ColorRequest(index, format) => {
                    if let Some(terminal) = self.terminal() {
                        let color = super::colors::query_color(index, terminal.lock().colors());
                        self.send(format(color).into_bytes());
                    }
                }
                Event::TextAreaSizeRequest(format) => {
                    self.send(format(self.size.window_size()).into_bytes())
                }
                Event::ChildExit(status) => self.finish_exit(status.to_string()),
                _ => {}
            }
        }
    }

    pub(crate) fn mouse_reporting_enabled(&self) -> bool {
        self.mouse_reporting_enabled
    }

    pub(crate) fn toggle_mouse_reporting(&mut self, cx: &mut Context<Self>) {
        self.mouse_reporting_enabled = !self.mouse_reporting_enabled;
        cx.notify();
    }

    pub(crate) fn pointer_revision(&self) -> u64 {
        self.pointer_revision.load(Ordering::Acquire)
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    fn owns_attachment(&self, attachment: &TerminalAttachment) -> bool {
        Arc::ptr_eq(&self.attachment, &attachment.active)
            && self.attachment.load(Ordering::Acquire) == attachment.id
    }

    pub(crate) fn send_input(
        &self,
        attachment: &TerminalAttachment,
        generation: u64,
        bytes: Vec<u8>,
    ) {
        if self.generation == generation && self.owns_attachment(attachment) {
            self.send(bytes);
        }
    }

    pub(crate) fn set_focus(&mut self, attachment: &TerminalAttachment, focused: bool) {
        if !self.owns_attachment(attachment) || self.focused == focused {
            return;
        }
        self.focused = focused;
        let report = self.terminal().is_some_and(|terminal| {
            let mut terminal = terminal.lock();
            terminal.is_focused = focused;
            terminal
                .mode()
                .contains(alacritty_terminal::term::TermMode::FOCUS_IN_OUT)
        });
        if report {
            self.send(if focused { b"\x1b[I" } else { b"\x1b[O" }.to_vec());
        }
    }

    fn finish_exit(&mut self, status: String) {
        let old = std::mem::replace(&mut self.state, TerminalState::Closed);
        self.state = match old {
            TerminalState::Running(mut running) => {
                let terminal = running.terminal.clone();
                running.shutdown();
                TerminalState::Stopped {
                    terminal,
                    status: TerminalStatus::Exited(status),
                }
            }
            state => state,
        };
    }

    pub(crate) fn attach(&mut self) -> Option<TerminalAttachment> {
        if self.attachment.load(Ordering::Acquire) != 0 {
            return None;
        }
        self.next_attachment += 1;
        let id = self.next_attachment;
        self.attachment.store(id, Ordering::Release);
        Some(TerminalAttachment {
            active: self.attachment.clone(),
            id,
        })
    }

    pub(crate) fn terminal(&self) -> Option<&Arc<FairMutex<Term<TerminalEventListener>>>> {
        match &self.state {
            TerminalState::Running(running) => Some(&running.terminal),
            TerminalState::Stopped { terminal, .. } => Some(terminal),
            TerminalState::Failed(_) | TerminalState::Closed => None,
        }
    }

    pub(crate) fn status(&self) -> TerminalStatus {
        match &self.state {
            TerminalState::Running(_) => TerminalStatus::Running,
            TerminalState::Stopped { status, .. } => status.clone(),
            TerminalState::Failed(error) => TerminalStatus::Failed(error.clone()),
            TerminalState::Closed => TerminalStatus::Closed,
        }
    }

    pub(crate) fn size(&self) -> TerminalSize {
        self.size
    }

    pub(crate) fn send(&self, bytes: Vec<u8>) {
        if let TerminalState::Running(running) = &self.state
            && !bytes.is_empty()
        {
            let _ = running.sender.send(Msg::Input(Cow::Owned(bytes)));
        }
    }

    pub(crate) fn resize(&mut self, size: TerminalSize) {
        if self.size == size {
            return;
        }
        self.size = size;
        if let TerminalState::Running(running) = &self.state {
            running.terminal.lock().resize(size);
            let _ = running.sender.send(Msg::Resize(size.window_size()));
        }
    }

    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self._wakeup_task = None;
        self.state = TerminalState::Closed;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn working_directory(&self) -> Option<&std::path::Path> {
        self.options.working_directory.as_deref()
    }

    /// The directory the shell is in now where the platform can report it,
    /// otherwise the directory the session started in.
    pub(crate) fn current_directory(&self) -> Option<std::path::PathBuf> {
        match &self.state {
            TerminalState::Running(RunningTerminal { pid: Some(pid), .. }) => {
                super::process::working_directory(*pid)
            }
            _ => None,
        }
        .or_else(|| self.options.working_directory.clone())
    }

    pub(crate) fn restart(&mut self, cx: &mut Context<Self>) {
        self.close(cx);
        self.start(cx);
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    use alacritty_terminal::{term::TermMode, vte::ansi::Processor};

    #[test]
    fn parser_replies_remain_ordered_when_repaint_wakeup_is_full() {
        let (wakeups, mut receiver) = tokio::sync::mpsc::channel(1);
        let events = Arc::new(Mutex::new(Vec::new()));
        let pointer_revision = Arc::new(AtomicU64::new(0));
        let listener = TerminalEventListener {
            wakeups,
            events: events.clone(),
            pointer_revision: pointer_revision.clone(),
        };
        let mut terminal = Term::new(
            Config::default(),
            &TerminalSize::initial(),
            listener.clone(),
        );
        let mut parser: Processor = Processor::new();
        for _ in 0..100 {
            listener.send_event(Event::Wakeup);
            parser.advance(&mut terminal, b"\x1b[5n\x1b[6n\x1b[c\x1b[18t");
        }
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
        let replies: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .map(|event| match event {
                Event::PtyWrite(text) => text.clone(),
                _ => panic!("unexpected event: {event:?}"),
            })
            .collect();
        assert_eq!(
            replies,
            ["\x1b[0n", "\x1b[1;1R", "\x1b[?6c", "\x1b[8;11;80t"].repeat(100)
        );
        parser.advance(
            &mut terminal,
            b"\x1b[?1h\x1b=\x1b[?1004h\x1b[?2004h\x1b[?1049h",
        );
        assert!(terminal.mode().contains(
            TermMode::APP_CURSOR
                | TermMode::APP_KEYPAD
                | TermMode::FOCUS_IN_OUT
                | TermMode::BRACKETED_PASTE
                | TermMode::ALT_SCREEN
        ));
        parser.advance(
            &mut terminal,
            b"\x1b[?1049l\x1b[?1l\x1b>\x1b[?1004l\x1b[?2004l",
        );
        assert!(!terminal.mode().intersects(
            TermMode::APP_CURSOR
                | TermMode::APP_KEYPAD
                | TermMode::FOCUS_IN_OUT
                | TermMode::BRACKETED_PASTE
                | TermMode::ALT_SCREEN
        ));
    }

    #[test]
    fn parser_color_queries_use_display_palette_without_clipboard_or_keyboard_protocols() {
        let (wakeups, _) = tokio::sync::mpsc::channel(1);
        let events = Arc::new(Mutex::new(Vec::new()));
        let pointer_revision = Arc::new(AtomicU64::new(0));
        let listener = TerminalEventListener {
            wakeups,
            events: events.clone(),
            pointer_revision: pointer_revision.clone(),
        };
        let mut terminal = Term::new(Config::default(), &TerminalSize::initial(), listener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut terminal, b"\x1b]4;1;#112233\x1b\\\x1b]4;1;?\x1b\\\x1b]10;?\x1b\\\x1b]52;c;?\x1b\\\x1b]52;c;YQ==\x1b\\\x1b[?u");
        let replies: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .map(|event| match event {
                Event::ColorRequest(index, format) => {
                    format(super::super::colors::query_color(*index, terminal.colors()))
                }
                _ => panic!("unexpected event: {event:?}"),
            })
            .collect();
        assert_eq!(
            replies,
            [
                "\x1b]4;1;rgb:1111/2222/3333\x1b\\",
                "\x1b]10;rgb:d4d4/d4d4/d4d4\x1b\\"
            ]
        );
    }

    #[gpui::test]
    fn old_generation_events_cannot_format_replies_for_the_replacement(
        cx: &mut gpui::TestAppContext,
    ) {
        use gpui::AppContext;
        let session = cx.new(|cx| {
            TerminalSession::new_with_options(
                Options {
                    shell: Some(tty::Shell::new("/missing/knot-shell".into(), vec![])),
                    ..Default::default()
                },
                cx,
            )
        });
        session.update(cx, |session, cx| {
            let generation = session.generation;
            session.restart(cx);
            let status = session.status();
            session.apply_events(
                generation,
                vec![
                    Event::PtyWrite("stale".into()),
                    Event::TextAreaSizeRequest(Arc::new(|_| panic!("stale size formatter ran"))),
                    Event::ColorRequest(0, Arc::new(|_| panic!("stale color formatter ran"))),
                ],
            );
            assert_eq!(session.status(), status);
            session.close(cx);
        });
    }
}
