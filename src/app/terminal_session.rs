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
const CELL_WIDTH: f32 = 8.;
const CELL_HEIGHT: f32 = 16.;

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
            cell_width: CELL_WIDTH as u16,
            cell_height: CELL_HEIGHT as u16,
        }
    }

    pub(crate) fn from_pixels(width: f32, height: f32, scale_factor: f32) -> Self {
        Self {
            columns: (width / CELL_WIDTH).floor() as usize,
            lines: (height / CELL_HEIGHT).floor() as usize,
            cell_width: (CELL_WIDTH * scale_factor)
                .round()
                .clamp(1., u16::MAX as f32) as u16,
            cell_height: (CELL_HEIGHT * scale_factor)
                .round()
                .clamp(1., u16::MAX as f32) as u16,
        }
        .clamped()
    }

    fn clamped(mut self) -> Self {
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
    child_exit: Arc<Mutex<Option<String>>>,
}

impl EventListener for TerminalEventListener {
    fn send_event(&self, event: Event) {
        if let Event::ChildExit(status) = &event {
            *self
                .child_exit
                .lock()
                .expect("terminal exit state poisoned") = Some(status.to_string());
        }
        if matches!(
            event,
            Event::Wakeup | Event::CursorBlinkingChange | Event::Exit | Event::ChildExit(_)
        ) {
            let _ = self.wakeups.try_send(());
        }
    }
}

type PtyEventLoop = EventLoop<tty::Pty, TerminalEventListener>;
type PtyThread = JoinHandle<(PtyEventLoop, State)>;

struct RunningTerminal {
    terminal: Arc<FairMutex<Term<TerminalEventListener>>>,
    sender: EventLoopSender,
    thread: Option<PtyThread>,
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
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
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
        };
        this.start(cx);
        this
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let (wakeup_tx, mut wakeup_rx) = tokio::sync::mpsc::channel(1);
        let child_exit = Arc::new(Mutex::new(None));
        let listener = TerminalEventListener {
            wakeups: wakeup_tx,
            child_exit: child_exit.clone(),
        };
        self.state = match RunningTerminal::start(listener, self.size, &self.options) {
            Ok(running) => TerminalState::Running(running),
            Err(error) => TerminalState::Failed(error.to_string()),
        };
        self._wakeup_task = Some(cx.spawn(async move |this, cx| {
            while wakeup_rx.recv().await.is_some() {
                let status = child_exit
                    .lock()
                    .expect("terminal exit state poisoned")
                    .take();
                if this
                    .update(cx, |session, cx| {
                        if session.generation != generation {
                            return;
                        }
                        if let Some(status) = status {
                            session.finish_exit(status);
                        }
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

    pub(crate) fn restart(&mut self, cx: &mut Context<Self>) {
        self.close(cx);
        self.start(cx);
    }
}
