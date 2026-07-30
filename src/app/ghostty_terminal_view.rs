use std::{
    cell::RefCell,
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use gpui::*;
use libghostty_vt::{
    RenderState, Terminal, TerminalOptions,
    key::{Action as KeyAction, Encoder as KeyEncoder, Event as KeyEvent, Key, Mods},
    render::{CellIterator, CursorVisualStyle, RowIterator},
    style::{RgbColor, Underline},
    terminal::ScrollViewport,
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

const CELL_WIDTH: f32 = 8.;
const CELL_HEIGHT: f32 = 16.;
const FONT_SIZE: f32 = 13.;
const BACKGROUND: u32 = 0x181818;
const FOREGROUND: u32 = 0xd4d4d4;
const MAX_OUTPUT_CHUNKS_PER_TICK: usize = 4;
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Clone, Copy, PartialEq, Eq)]
struct TerminalSize {
    columns: u16,
    lines: u16,
    cell_width: u16,
    cell_height: u16,
}

impl TerminalSize {
    fn initial() -> Self {
        Self {
            columns: 80,
            lines: 11,
            cell_width: CELL_WIDTH as u16,
            cell_height: CELL_HEIGHT as u16,
        }
    }

    fn from_viewport(viewport: Size<Pixels>, scale: f32) -> Self {
        Self {
            columns: ((viewport.width / px(CELL_WIDTH)).floor() as usize)
                .clamp(2, u16::MAX as usize) as u16,
            lines: ((viewport.height / px(CELL_HEIGHT)).floor() as usize)
                .clamp(1, u16::MAX as usize) as u16,
            cell_width: (CELL_WIDTH * scale).round().clamp(1., u16::MAX as f32) as u16,
            cell_height: (CELL_HEIGHT * scale).round().clamp(1., u16::MAX as f32) as u16,
        }
    }

    fn pty(self) -> PtySize {
        PtySize {
            rows: self.lines,
            cols: self.columns,
            pixel_width: self.cell_width,
            pixel_height: self.cell_height,
        }
    }
}

#[derive(Clone)]
struct RenderCell {
    line: usize,
    column: usize,
    text: String,
    foreground: u32,
    background: u32,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

#[derive(Clone, Copy)]
enum CursorShape {
    Bar,
    Block,
    Underline,
    HollowBlock,
}

#[derive(Clone, Copy)]
struct RenderCursor {
    line: usize,
    column: usize,
    shape: CursorShape,
    color: u32,
}

#[derive(Clone, Default)]
struct TerminalSnapshot {
    cells: Vec<RenderCell>,
    cursor: Option<RenderCursor>,
}

impl TerminalSnapshot {
    fn capture<'alloc: 'callbacks, 'callbacks>(
        terminal: &Terminal<'alloc, 'callbacks>,
        render_state: &mut RenderState<'alloc>,
    ) -> anyhow::Result<Self> {
        let mut rows = RowIterator::new()?;
        let mut cells = CellIterator::new()?;
        let snapshot = render_state.update(terminal)?;
        let colors = snapshot.colors()?;
        let mut rendered =
            Vec::with_capacity(usize::from(snapshot.cols()?) * usize::from(snapshot.rows()?));
        let mut row_iter = rows.update(&snapshot)?;
        let mut line = 0;
        while let Some(row) = row_iter.next() {
            let mut cell_iter = cells.update(row)?;
            let mut column = 0;
            while let Some(cell) = cell_iter.next() {
                let mut foreground = cell.fg_color()?.unwrap_or(colors.foreground);
                let mut background = cell.bg_color()?.unwrap_or(colors.background);
                let style = cell.style()?;
                if style.inverse {
                    std::mem::swap(&mut foreground, &mut background);
                }
                rendered.push(RenderCell {
                    line,
                    column,
                    text: cell.graphemes()?.into_iter().collect(),
                    foreground: pack(foreground),
                    background: pack(background),
                    bold: style.bold,
                    italic: style.italic,
                    underline: style.underline != Underline::None,
                    strikethrough: style.strikethrough,
                });
                column += 1;
            }
            line += 1;
        }
        let cursor = if snapshot.cursor_visible()? {
            snapshot
                .cursor_viewport()?
                .map(|cursor| -> anyhow::Result<_> {
                    let shape = match snapshot.cursor_visual_style()? {
                        CursorVisualStyle::Bar => CursorShape::Bar,
                        CursorVisualStyle::Underline => CursorShape::Underline,
                        CursorVisualStyle::BlockHollow => CursorShape::HollowBlock,
                        _ => CursorShape::Block,
                    };
                    let color = snapshot
                        .cursor_color()?
                        .or(colors.cursor)
                        .unwrap_or(colors.foreground);
                    Ok(RenderCursor {
                        line: cursor.y.into(),
                        column: cursor.x.into(),
                        shape,
                        color: pack(color),
                    })
                })
                .transpose()?
        } else {
            None
        };
        Ok(Self {
            cells: rendered,
            cursor,
        })
    }
}

fn pack(color: RgbColor) -> u32 {
    u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b)
}

fn unpack(color: u32) -> RgbColor {
    RgbColor {
        r: (color >> 16) as u8,
        g: (color >> 8) as u8,
        b: color as u8,
    }
}

struct EncodedKey {
    key: Key,
    mods: Mods,
    text: Option<String>,
    unshifted: Option<char>,
}

enum SessionCommand {
    Key(EncodedKey),
    Resize(TerminalSize),
    Scroll(isize),
    Shutdown,
}

struct Shared {
    snapshot: Mutex<Arc<TerminalSnapshot>>,
    status: Mutex<Option<String>>,
    wakeups: tokio::sync::mpsc::Sender<()>,
}

impl Shared {
    fn publish(&self, snapshot: TerminalSnapshot) {
        *self.snapshot.lock().expect("terminal snapshot poisoned") = Arc::new(snapshot);
        let _ = self.wakeups.try_send(());
    }

    fn status(&self, status: String) {
        *self.status.lock().expect("terminal status poisoned") = Some(status);
        let _ = self.wakeups.try_send(());
    }
}

struct TerminalSession {
    commands: Sender<SessionCommand>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl TerminalSession {
    fn start(size: TerminalSize, wakeups: tokio::sync::mpsc::Sender<()>) -> anyhow::Result<Self> {
        let pair = native_pty_system().openpty(size.pty())?;
        let mut command = CommandBuilder::new_default_prog();
        command.cwd(std::env::current_dir()?);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let (command_tx, command_rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            snapshot: Mutex::new(Arc::new(TerminalSnapshot::default())),
            status: Mutex::new(None),
            wakeups,
        });
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("Ghostty terminal session".into())
            .spawn(move || {
                run_session(
                    size,
                    pair.master,
                    child,
                    reader,
                    writer,
                    command_rx,
                    thread_shared,
                )
            })?;
        Ok(Self {
            commands: command_tx,
            thread: Some(thread),
            shared,
        })
    }

    fn send(&self, command: SessionCommand) {
        let _ = self.commands.send(command);
    }

    fn snapshot(&self) -> Arc<TerminalSnapshot> {
        self.shared
            .snapshot
            .lock()
            .expect("terminal snapshot poisoned")
            .clone()
    }

    fn shutdown(mut self) -> Arc<TerminalSnapshot> {
        self.send(SessionCommand::Shutdown);
        let snapshot = self.snapshot();
        if let Some(thread) = self.thread.take() {
            std::thread::spawn(move || {
                let _ = thread.join();
            });
        }
        snapshot
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.send(SessionCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_session(
    size: TerminalSize,
    master: Box<dyn MasterPty + Send>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Box<dyn Read + Send>,
    mut writer: Box<dyn Write + Send>,
    commands: Receiver<SessionCommand>,
    shared: Arc<Shared>,
) {
    let (output_tx, output_rx) = mpsc::sync_channel(16);
    let reader_thread = spawn_reader(reader, output_tx);
    let replies = RefCell::new(Vec::new());
    let result = (|| -> anyhow::Result<()> {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: size.columns,
            rows: size.lines,
            max_scrollback: 10_000,
        })?;
        terminal
            .set_default_fg_color(Some(unpack(FOREGROUND)))?
            .set_default_bg_color(Some(unpack(BACKGROUND)))?
            .set_default_cursor_color(Some(unpack(FOREGROUND)))?
            .on_pty_write(|_, bytes| replies.borrow_mut().extend_from_slice(bytes))?;
        let mut encoder = KeyEncoder::new()?;
        let mut render_state = RenderState::new()?;
        shared.publish(TerminalSnapshot::capture(&terminal, &mut render_state)?);
        let mut last_snapshot = Instant::now();
        let mut snapshot_dirty = false;
        let mut running = true;
        while running {
            while let Ok(command) = commands.try_recv() {
                running =
                    handle_command(command, &mut terminal, &mut encoder, &master, &mut writer)?;
                if !running {
                    break;
                }
                shared.publish(TerminalSnapshot::capture(&terminal, &mut render_state)?);
                last_snapshot = Instant::now();
                snapshot_dirty = false;
            }
            if !running {
                break;
            }
            let mut changed = false;
            match output_rx.recv_timeout(Duration::from_millis(16)) {
                Ok(bytes) => {
                    terminal.vt_write(&bytes);
                    changed = true;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => running = false,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if changed {
                for _ in 1..MAX_OUTPUT_CHUNKS_PER_TICK {
                    let Ok(bytes) = output_rx.try_recv() else {
                        break;
                    };
                    terminal.vt_write(&bytes);
                }
                snapshot_dirty = true;
            }
            let replies = replies.take();
            if !replies.is_empty() {
                writer.write_all(&replies)?;
                writer.flush()?;
            }
            if snapshot_dirty && last_snapshot.elapsed() >= SNAPSHOT_INTERVAL {
                shared.publish(TerminalSnapshot::capture(&terminal, &mut render_state)?);
                last_snapshot = Instant::now();
                snapshot_dirty = false;
            }
            if let Some(status) = child.try_wait()? {
                shared.status(format!("exited: {status}"));
                running = false;
            }
        }
        if snapshot_dirty {
            shared.publish(TerminalSnapshot::capture(&terminal, &mut render_state)?);
        }
        Ok(())
    })();
    let _ = child.kill();
    let _ = child.wait();
    drop(master);
    drop(writer);
    let _ = reader_thread.join();
    if let Err(error) = result {
        shared.status(format!("failed: {error:#}"));
    }
}

fn spawn_reader(mut reader: Box<dyn Read + Send>, output: SyncSender<Vec<u8>>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("Ghostty PTY reader".into())
        .spawn(move || {
            loop {
                let mut bytes = vec![0; 64 * 1024];
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(length) => {
                        bytes.truncate(length);
                        if output.send(bytes).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        })
        .expect("spawn Ghostty PTY reader")
}

fn handle_command(
    command: SessionCommand,
    terminal: &mut Terminal<'_, '_>,
    encoder: &mut KeyEncoder<'_>,
    master: &Box<dyn MasterPty + Send>,
    writer: &mut Box<dyn Write + Send>,
) -> anyhow::Result<bool> {
    match command {
        SessionCommand::Key(input) => {
            terminal.scroll_viewport(ScrollViewport::Bottom);
            encoder.set_options_from_terminal(terminal);
            let mut event = KeyEvent::new()?;
            event
                .set_action(KeyAction::Press)
                .set_key(input.key)
                .set_mods(input.mods)
                .set_utf8(input.text);
            if let Some(character) = input.unshifted {
                event.set_unshifted_codepoint(character);
            }
            let mut bytes = Vec::with_capacity(16);
            encoder.encode_to_vec(&event, &mut bytes)?;
            if !bytes.is_empty() {
                writer.write_all(&bytes)?;
                writer.flush()?;
            }
        }
        SessionCommand::Resize(size) => {
            terminal.resize(
                size.columns,
                size.lines,
                size.cell_width.into(),
                size.cell_height.into(),
            )?;
            master.resize(size.pty())?;
        }
        SessionCommand::Scroll(rows) => terminal.scroll_viewport(ScrollViewport::Delta(rows)),
        SessionCommand::Shutdown => return Ok(false),
    }
    Ok(true)
}

enum TerminalState {
    Running(TerminalSession),
    Stopped {
        snapshot: Arc<TerminalSnapshot>,
        status: String,
    },
    Failed(String),
}

impl TerminalState {
    fn label(&self) -> String {
        match self {
            Self::Running(_) => "running · Ghostty VT".into(),
            Self::Stopped { status, .. } => format!("{status} · Ghostty VT"),
            Self::Failed(error) => format!("failed: {error} · Ghostty VT"),
        }
    }

    fn snapshot(&self) -> Arc<TerminalSnapshot> {
        match self {
            Self::Running(session) => session.snapshot(),
            Self::Stopped { snapshot, .. } => snapshot.clone(),
            Self::Failed(_) => Arc::new(TerminalSnapshot::default()),
        }
    }
}

/// Native terminal surface owning one Ghostty VT session and local PTY.
pub(crate) struct TerminalView {
    state: TerminalState,
    focus: FocusHandle,
    scroll_delta_y: f32,
    size: TerminalSize,
    _repaint_task: Task<()>,
}

impl TerminalView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let size = TerminalSize::initial();
        let (state, repaint_task) = Self::start(size, cx);
        Self {
            state,
            focus: cx.focus_handle(),
            scroll_delta_y: 0.,
            size,
            _repaint_task: repaint_task,
        }
    }

    fn start(size: TerminalSize, cx: &mut Context<Self>) -> (TerminalState, Task<()>) {
        let (wakeup_tx, mut wakeup_rx) = tokio::sync::mpsc::channel(1);
        let state = match TerminalSession::start(size, wakeup_tx) {
            Ok(session) => TerminalState::Running(session),
            Err(error) => TerminalState::Failed(format!("{error:#}")),
        };
        let repaint_task = cx.spawn(async move |this, cx| {
            while wakeup_rx.recv().await.is_some() {
                if this
                    .update(cx, |this, cx| {
                        let status = match &this.state {
                            TerminalState::Running(session) => session
                                .shared
                                .status
                                .lock()
                                .expect("terminal status poisoned")
                                .take(),
                            _ => None,
                        };
                        if let Some(status) = status {
                            this.stop(status);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        (state, repaint_task)
    }

    fn stop(&mut self, status: String) {
        let old = std::mem::replace(
            &mut self.state,
            TerminalState::Failed("terminal state unavailable".into()),
        );
        self.state = match old {
            TerminalState::Running(session) => TerminalState::Stopped {
                snapshot: session.shutdown(),
                status,
            },
            state => state,
        };
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.stop("closed".into());
        cx.notify();
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        self.stop("restarting".into());
        let (state, task) = Self::start(self.size, cx);
        self.state = state;
        self.scroll_delta_y = 0.;
        self._repaint_task = task;
        cx.notify();
    }

    fn resize(&mut self, size: TerminalSize) {
        if self.size == size {
            return;
        }
        self.size = size;
        if let TerminalState::Running(session) = &self.state {
            session.send(SessionCommand::Resize(size));
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let TerminalState::Running(session) = &self.state else {
            return;
        };
        if event.keystroke.modifiers.platform {
            return;
        }
        let name = event.keystroke.key.to_lowercase();
        let key = ghostty_key(&name);
        let text = event
            .keystroke
            .key_char
            .clone()
            .filter(|text| text.chars().all(|ch| ch >= ' ' && ch != '\u{7f}'));
        if key == Key::Unidentified && text.is_none() {
            return;
        }
        let unshifted = name.chars().next().filter(|_| name.chars().count() == 1);
        session.send(SessionCommand::Key(EncodedKey {
            key,
            mods: ghostty_mods(event.keystroke.modifiers),
            text,
            unshifted,
        }));
        self.scroll_delta_y = 0.;
        cx.stop_propagation();
    }

    fn scroll(&mut self, delta: ScrollDelta, cx: &mut Context<Self>) {
        let TerminalState::Running(session) = &self.state else {
            return;
        };
        let delta = delta.pixel_delta(px(CELL_HEIGHT));
        self.scroll_delta_y += f32::from(delta.y);
        let rows = (self.scroll_delta_y / 4.).trunc() as isize;
        if rows != 0 {
            self.scroll_delta_y -= rows as f32 * 4.;
            session.send(SessionCommand::Scroll(rows));
            cx.notify();
        }
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        self.stop("closed".into());
    }
}

fn ghostty_mods(modifiers: Modifiers) -> Mods {
    let mut result = Mods::empty();
    result.set(Mods::SHIFT, modifiers.shift);
    result.set(Mods::ALT, modifiers.alt);
    result.set(Mods::CTRL, modifiers.control);
    result
}

fn ghostty_key(key: &str) -> Key {
    match key {
        "enter" => Key::Enter,
        "backspace" => Key::Backspace,
        "tab" => Key::Tab,
        "escape" => Key::Escape,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "right" => Key::ArrowRight,
        "left" => Key::ArrowLeft,
        "home" => Key::Home,
        "end" => Key::End,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "space" => Key::Space,
        "a" => Key::A,
        "b" => Key::B,
        "c" => Key::C,
        "d" => Key::D,
        "e" => Key::E,
        "f" => Key::F,
        "g" => Key::G,
        "h" => Key::H,
        "i" => Key::I,
        "j" => Key::J,
        "k" => Key::K,
        "l" => Key::L,
        "m" => Key::M,
        "n" => Key::N,
        "o" => Key::O,
        "p" => Key::P,
        "q" => Key::Q,
        "r" => Key::R,
        "s" => Key::S,
        "t" => Key::T,
        "u" => Key::U,
        "v" => Key::V,
        "w" => Key::W,
        "x" => Key::X,
        "y" => Key::Y,
        "z" => Key::Z,
        "0" => Key::Digit0,
        "1" => Key::Digit1,
        "2" => Key::Digit2,
        "3" => Key::Digit3,
        "4" => Key::Digit4,
        "5" => Key::Digit5,
        "6" => Key::Digit6,
        "7" => Key::Digit7,
        "8" => Key::Digit8,
        "9" => Key::Digit9,
        "-" => Key::Minus,
        "=" => Key::Equal,
        "[" => Key::BracketLeft,
        "]" => Key::BracketRight,
        "\\" => Key::Backslash,
        ";" => Key::Semicolon,
        "'" => Key::Quote,
        "," => Key::Comma,
        "." => Key::Period,
        "/" => Key::Slash,
        "`" => Key::Backquote,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        _ => Key::Unidentified,
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .occlude()
            .child(
                div()
                    .h(px(22.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_2()
                    .text_xs()
                    .text_color(rgb(0x8f8f8f))
                    .bg(rgb(0x202020))
                    .child(format!("TERMINAL · {}", self.state.label()))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .id("close-terminal")
                                    .cursor_pointer()
                                    .text_color(rgb(0xffb080))
                                    .child("close")
                                    .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                            )
                            .child(
                                div()
                                    .id("restart-terminal")
                                    .cursor_pointer()
                                    .text_color(rgb(0x80c0ff))
                                    .child("restart")
                                    .on_click(cx.listener(|this, _, _, cx| this.restart(cx))),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(Self::on_key_down))
                    .child(TerminalElement { entity }),
            )
    }
}

struct TerminalElement {
    entity: Entity<TerminalView>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        let size = TerminalSize::from_viewport(bounds.size, window.scale_factor());
        let _ = self.entity.update(cx, |terminal, _| terminal.resize(size));
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let hitbox = hitbox.clone();
        let entity = self.entity.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                let _ = entity.update(cx, |terminal, cx| terminal.scroll(event.delta, cx));
                cx.stop_propagation();
            }
        });

        let snapshot = self.entity.read(cx).state.snapshot();
        let cell_width = px(CELL_WIDTH);
        let cell_height = px(CELL_HEIGHT);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for cell in &snapshot.cells {
                if cell.background != BACKGROUND {
                    let origin = cell_origin(bounds, cell.line, cell.column);
                    let _ = window.paint_quad(fill(
                        Bounds {
                            origin,
                            size: size(cell_width, cell_height),
                        },
                        rgb(cell.background),
                    ));
                }
            }
            if let Some(cursor) = snapshot.cursor {
                paint_cursor(cursor, bounds, window);
            }
            for cell in &snapshot.cells {
                if cell.text.is_empty() || cell.text == " " {
                    continue;
                }
                let mut font = gpui::font("Menlo");
                if cell.bold {
                    font = font.bold();
                }
                if cell.italic {
                    font = font.italic();
                }
                let run = TextRun {
                    len: cell.text.len(),
                    font,
                    color: rgb(cell.foreground).into(),
                    background_color: None,
                    underline: cell.underline.then(|| UnderlineStyle {
                        thickness: px(1.),
                        ..Default::default()
                    }),
                    strikethrough: cell.strikethrough.then(|| StrikethroughStyle {
                        thickness: px(1.),
                        ..Default::default()
                    }),
                };
                let shaped = window.text_system().shape_line(
                    SharedString::from(cell.text.clone()),
                    px(FONT_SIZE),
                    &[run],
                    None,
                );
                let _ = shaped.paint(
                    cell_origin(bounds, cell.line, cell.column),
                    cell_height,
                    window,
                    cx,
                );
            }
        });
    }
}

fn cell_origin(bounds: Bounds<Pixels>, line: usize, column: usize) -> Point<Pixels> {
    point(
        bounds.origin.x + px(CELL_WIDTH) * column,
        bounds.origin.y + px(CELL_HEIGHT) * line,
    )
}

fn paint_cursor(cursor: RenderCursor, bounds: Bounds<Pixels>, window: &mut Window) {
    let origin = cell_origin(bounds, cursor.line, cursor.column);
    let cell_width = px(CELL_WIDTH);
    let cell_height = px(CELL_HEIGHT);
    let color = rgb(cursor.color);
    match cursor.shape {
        CursorShape::Bar => {
            let _ = window.paint_quad(fill(
                Bounds {
                    origin,
                    size: size(px(2.), cell_height),
                },
                color,
            ));
        }
        CursorShape::Underline => {
            let _ = window.paint_quad(fill(
                Bounds {
                    origin: point(origin.x, origin.y + cell_height - px(2.)),
                    size: size(cell_width, px(2.)),
                },
                color,
            ));
        }
        CursorShape::HollowBlock => {
            let _ = window.paint_quad(quad(
                Bounds {
                    origin,
                    size: size(cell_width, cell_height),
                },
                0.,
                transparent_black(),
                px(1.),
                color,
                BorderStyle::Solid,
            ));
        }
        CursorShape::Block => {
            let _ = window.paint_quad(fill(
                Bounds {
                    origin,
                    size: size(cell_width, cell_height),
                },
                rgba((cursor.color << 8) | 0x80),
            ));
        }
    }
}
