use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

use alacritty_terminal::{
    event::{Event, EventListener, WindowSize},
    event_loop::{EventLoop, EventLoopSender, Msg, State},
    grid::{Dimensions, Scroll},
    sync::FairMutex,
    term::{
        Config, MIN_COLUMNS, MIN_SCREEN_LINES, Term, cell::Flags, color::Colors, point_to_viewport,
    },
    tty::{self, Options},
    vte::ansi::{Color, CursorShape, NamedColor, Rgb},
};
use gpui::*;

use super::{CommandAction, CommandSurfaceKind, DIAGNOSTIC_COMMAND, TERMINAL_KEY_CONTEXT};

const INITIAL_COLUMNS: usize = 80;
const INITIAL_LINES: usize = 11;
const CELL_WIDTH: f32 = 8.;
const CELL_HEIGHT: f32 = 16.;
const FONT_SIZE: f32 = 13.;
const SCROLL_PIXELS_PER_LINE: f32 = 4.;
const BACKGROUND: u32 = 0x181818;
const FOREGROUND: u32 = 0xd4d4d4;

#[derive(Clone, Copy, PartialEq, Eq)]
struct TerminalSize {
    columns: usize,
    lines: usize,
    cell_width: u16,
    cell_height: u16,
}

impl TerminalSize {
    fn initial() -> Self {
        Self {
            columns: INITIAL_COLUMNS,
            lines: INITIAL_LINES,
            cell_width: CELL_WIDTH as u16,
            cell_height: CELL_HEIGHT as u16,
        }
    }

    fn from_viewport(viewport: Size<Pixels>, scale_factor: f32) -> Self {
        let columns = (viewport.width / px(CELL_WIDTH)).floor() as usize;
        let lines = (viewport.height / px(CELL_HEIGHT)).floor() as usize;
        let scaled_cell_width = (CELL_WIDTH * scale_factor).round();
        let scaled_cell_height = (CELL_HEIGHT * scale_factor).round();

        Self {
            columns: columns.clamp(MIN_COLUMNS, u16::MAX as usize),
            lines: lines.clamp(MIN_SCREEN_LINES, u16::MAX as usize),
            cell_width: scaled_cell_width.clamp(1., u16::MAX as f32) as u16,
            cell_height: scaled_cell_height.clamp(1., u16::MAX as f32) as u16,
        }
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
struct TerminalEventListener {
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

struct TerminalSession {
    terminal: Arc<FairMutex<Term<TerminalEventListener>>>,
    sender: EventLoopSender,
    thread: Option<PtyThread>,
}

impl TerminalSession {
    fn start(listener: TerminalEventListener, size: TerminalSize) -> std::io::Result<Self> {
        let terminal = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            listener.clone(),
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
        let pty = tty::new(&options, size.window_size(), 0)?;
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

    fn send(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.sender.send(Msg::Input(Cow::Owned(bytes)));
        }
    }

    fn resize(&self, size: TerminalSize) {
        self.terminal.lock().resize(size);
        let _ = self.sender.send(Msg::Resize(size.window_size()));
    }

    fn shutdown(mut self) {
        let _ = self.sender.send(Msg::Shutdown);
        if let Some(thread) = self.thread.take() {
            std::thread::spawn(move || {
                let _ = thread.join();
            });
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

enum TerminalState {
    Running(TerminalSession),
    Stopped {
        terminal: Arc<FairMutex<Term<TerminalEventListener>>>,
        status: String,
    },
    Failed(String),
}

impl TerminalState {
    fn terminal(&self) -> Option<&Arc<FairMutex<Term<TerminalEventListener>>>> {
        match self {
            Self::Running(session) => Some(&session.terminal),
            Self::Stopped { terminal, .. } => Some(terminal),
            Self::Failed(_) => None,
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Running(_) => "running".to_owned(),
            Self::Stopped { status, .. } => status.clone(),
            Self::Failed(error) => format!("failed: {error}"),
        }
    }
}

/// Native terminal surface owning one authoritative PTY session and emulator grid.
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
        let child_exit = Arc::new(Mutex::new(None));
        let listener = TerminalEventListener {
            wakeups: wakeup_tx,
            child_exit: child_exit.clone(),
        };
        let state = match TerminalSession::start(listener, size) {
            Ok(session) => TerminalState::Running(session),
            Err(error) => TerminalState::Failed(error.to_string()),
        };
        let repaint_task = cx.spawn(async move |this, cx| {
            while wakeup_rx.recv().await.is_some() {
                let status = child_exit
                    .lock()
                    .expect("terminal exit state poisoned")
                    .take();
                if this
                    .update(cx, |this, cx| {
                        if let Some(status) = status {
                            this.stop(format!("exited: {status}"));
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
        let previous = std::mem::replace(
            &mut self.state,
            TerminalState::Failed("terminal state unavailable".to_owned()),
        );
        self.state = match previous {
            TerminalState::Running(session) => {
                let terminal = session.terminal.clone();
                session.shutdown();
                TerminalState::Stopped { terminal, status }
            }
            state => state,
        };
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.stop("closed".to_owned());
        cx.notify();
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        self.stop("restarting".to_owned());
        let (state, repaint_task) = Self::start(self.size, cx);
        self.state = state;
        self.scroll_delta_y = 0.;
        self._repaint_task = repaint_task;
        cx.notify();
    }

    fn resize(&mut self, size: TerminalSize) {
        if self.size == size {
            return;
        }
        self.size = size;
        if let TerminalState::Running(session) = &self.state {
            session.resize(size);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let TerminalState::Running(session) = &self.state else {
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
            session.terminal.lock().scroll_display(Scroll::Bottom);
            self.scroll_delta_y = 0.;
            session.send(bytes);
            cx.stop_propagation();
        }
    }

    fn scroll(&mut self, delta: ScrollDelta, cx: &mut Context<Self>) {
        let Some(terminal) = self.state.terminal() else {
            return;
        };
        let delta = delta.pixel_delta(px(CELL_HEIGHT));
        self.scroll_delta_y += f32::from(delta.y);
        let rows = (self.scroll_delta_y / SCROLL_PIXELS_PER_LINE).trunc() as i32;
        if rows != 0 {
            self.scroll_delta_y -= rows as f32 * SCROLL_PIXELS_PER_LINE;
            terminal.lock().scroll_display(Scroll::Delta(rows));
            cx.notify();
        }
    }

    fn on_command_action(
        &mut self,
        action: &CommandAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.handle_native_command(action, window, cx) {
            cx.propagate();
        }
    }

    fn handle_native_command(
        &mut self,
        action: &CommandAction,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if action.command.name.as_ref() != DIAGNOSTIC_COMMAND {
            return false;
        }
        action.record_diagnostic(CommandSurfaceKind::Terminal, cx);
        true
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        self.stop("closed".to_owned());
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
                    .key_context(TERMINAL_KEY_CONTEXT)
                    .on_action(cx.listener(Self::on_command_action))
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

#[derive(Clone)]
struct RenderCell {
    line: usize,
    column: usize,
    text: String,
    foreground: u32,
    background: u32,
    flags: Flags,
}

struct TerminalSnapshot {
    cells: Vec<RenderCell>,
    cursor: Option<(usize, usize, CursorShape)>,
}

impl TerminalSnapshot {
    fn capture(view: &TerminalView) -> Self {
        let Some(terminal) = view.state.terminal() else {
            return Self {
                cells: Vec::new(),
                cursor: None,
            };
        };
        let terminal = terminal.lock();
        let content = terminal.renderable_content();
        let display_offset = content.display_offset;
        let colors = *content.colors;
        let cursor = point_to_viewport(display_offset, content.cursor.point)
            .map(|point| (point.line, point.column.0, content.cursor.shape));
        let cells = content
            .display_iter
            .filter_map(|indexed| {
                let point = point_to_viewport(display_offset, indexed.point)?;
                let cell = indexed.cell;
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    return None;
                }
                let mut foreground = resolve_color(cell.fg, &colors, FOREGROUND);
                let mut background = resolve_color(cell.bg, &colors, BACKGROUND);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut foreground, &mut background);
                }
                if cell.flags.contains(Flags::DIM) {
                    foreground = dim(foreground);
                }
                let mut text = cell.c.to_string();
                if let Some(zerowidth) = cell.zerowidth() {
                    text.extend(zerowidth);
                }
                Some(RenderCell {
                    line: point.line,
                    column: point.column.0,
                    text,
                    foreground,
                    background,
                    flags: cell.flags,
                })
            })
            .collect();

        Self { cells, cursor }
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
    ) -> (LayoutId, Self::RequestLayoutState) {
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
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let size = TerminalSize::from_viewport(bounds.size, window.scale_factor());
        let _ = self.entity.update(cx, |terminal, _| terminal.resize(size));
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let hitbox = hitbox.clone();
        let entity = self.entity.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                let _ = entity.update(cx, |terminal, cx| {
                    terminal.scroll(event.delta, cx);
                });
                cx.stop_propagation();
            }
        });

        let snapshot = TerminalSnapshot::capture(self.entity.read(cx));
        let cell_width = px(CELL_WIDTH);
        let cell_height = px(CELL_HEIGHT);
        let mask = Some(ContentMask { bounds });
        window.with_content_mask(mask, |window| {
            for cell in &snapshot.cells {
                let origin = point(
                    bounds.origin.x + cell_width * cell.column,
                    bounds.origin.y + cell_height * cell.line,
                );
                if cell.background != BACKGROUND {
                    let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                        cell_width * 2
                    } else {
                        cell_width
                    };
                    let _ = window.paint_quad(fill(
                        Bounds {
                            origin,
                            size: size(width, cell_height),
                        },
                        rgb(cell.background),
                    ));
                }
            }

            if let Some((line, column, shape)) = snapshot.cursor
                && shape != CursorShape::Hidden
            {
                let origin = point(
                    bounds.origin.x + cell_width * column,
                    bounds.origin.y + cell_height * line,
                );
                let cursor_bounds = match shape {
                    CursorShape::Beam => Bounds {
                        origin,
                        size: size(px(2.), cell_height),
                    },
                    CursorShape::Underline => Bounds {
                        origin: point(origin.x, origin.y + cell_height - px(2.)),
                        size: size(cell_width, px(2.)),
                    },
                    CursorShape::HollowBlock => {
                        let _ = window.paint_quad(quad(
                            Bounds {
                                origin,
                                size: size(cell_width, cell_height),
                            },
                            0.,
                            transparent_black(),
                            px(1.),
                            rgb(0xd4d4d4),
                            BorderStyle::Solid,
                        ));
                        Bounds {
                            origin,
                            size: size(px(0.), px(0.)),
                        }
                    }
                    CursorShape::Block => Bounds {
                        origin,
                        size: size(cell_width, cell_height),
                    },
                    CursorShape::Hidden => unreachable!(),
                };
                if cursor_bounds.size.width > px(0.) {
                    let _ = window.paint_quad(fill(cursor_bounds, rgba(0xd4d4d480)));
                }
            }

            for cell in &snapshot.cells {
                if cell.flags.contains(Flags::HIDDEN) || cell.text == " " {
                    continue;
                }
                let mut font = gpui::font("Menlo");
                if cell.flags.contains(Flags::BOLD) {
                    font = font.bold();
                }
                if cell.flags.contains(Flags::ITALIC) {
                    font = font.italic();
                }
                let run = TextRun {
                    len: cell.text.len(),
                    font,
                    color: rgb(cell.foreground).into(),
                    background_color: None,
                    underline: cell.flags.intersects(Flags::ALL_UNDERLINES).then(|| {
                        UnderlineStyle {
                            thickness: px(1.),
                            ..Default::default()
                        }
                    }),
                    strikethrough: cell.flags.contains(Flags::STRIKEOUT).then(|| {
                        StrikethroughStyle {
                            thickness: px(1.),
                            ..Default::default()
                        }
                    }),
                };
                let shaped = window.text_system().shape_line(
                    SharedString::from(cell.text.clone()),
                    px(FONT_SIZE),
                    &[run],
                    None,
                );
                let origin = point(
                    bounds.origin.x + cell_width * cell.column,
                    bounds.origin.y + cell_height * cell.line,
                );
                let _ = shaped.paint(origin, cell_height, window, cx);
            }
        });
    }
}

fn resolve_color(color: Color, overrides: &Colors, default: u32) -> u32 {
    let rgb = match color {
        Color::Spec(rgb) => rgb,
        Color::Named(named) => overrides[named].unwrap_or_else(|| named_color(named, default)),
        Color::Indexed(index) => overrides[index as usize].unwrap_or_else(|| indexed_color(index)),
    };
    (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b)
}

fn named_color(color: NamedColor, default: u32) -> Rgb {
    if color == NamedColor::Foreground || color == NamedColor::BrightForeground {
        return hex_rgb(default);
    }
    if color == NamedColor::Background {
        return hex_rgb(BACKGROUND);
    }
    let index = color as usize;
    if index < 16 {
        indexed_color(index as u8)
    } else if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&index) {
        indexed_color((index - NamedColor::DimBlack as usize) as u8)
    } else {
        hex_rgb(default)
    }
}

fn indexed_color(index: u8) -> Rgb {
    const ANSI: [u32; 16] = [
        0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666,
        0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
    ];
    match index {
        0..=15 => hex_rgb(ANSI[index as usize]),
        16..=231 => {
            let value = index - 16;
            let component = |part: u8| if part == 0 { 0 } else { 55 + part * 40 };
            Rgb {
                r: component(value / 36),
                g: component((value % 36) / 6),
                b: component(value % 6),
            }
        }
        232..=255 => {
            let level = 8 + (index - 232) * 10;
            Rgb {
                r: level,
                g: level,
                b: level,
            }
        }
    }
}

fn hex_rgb(color: u32) -> Rgb {
    Rgb {
        r: (color >> 16) as u8,
        g: (color >> 8) as u8,
        b: color as u8,
    }
}

fn dim(color: u32) -> u32 {
    let rgb = hex_rgb(color);
    (u32::from(rgb.r) * 2 / 3 << 16) | (u32::from(rgb.g) * 2 / 3 << 8) | u32::from(rgb.b) * 2 / 3
}
