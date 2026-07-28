use std::{borrow::Cow, sync::Arc, thread::JoinHandle};

use alacritty_terminal::{
    event::{Event, EventListener, WindowSize},
    event_loop::{EventLoop, EventLoopSender, Msg, State},
    grid::{Dimensions, Scroll},
    sync::FairMutex,
    term::{Config, Term, cell::Flags, color::Colors, point_to_viewport},
    tty::{self, Options},
    vte::ansi::{Color, CursorShape, NamedColor, Rgb},
};
use gpui::*;

const INITIAL_COLUMNS: usize = 80;
const INITIAL_LINES: usize = 11;
const INITIAL_CELL_WIDTH: u16 = 8;
const INITIAL_CELL_HEIGHT: u16 = 16;
const CELL_WIDTH: f32 = 8.;
const CELL_HEIGHT: f32 = 16.;
const FONT_SIZE: f32 = 13.;
const SCROLL_PIXELS_PER_LINE: f32 = 4.;
const BACKGROUND: u32 = 0x181818;
const FOREGROUND: u32 = 0xd4d4d4;

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

#[derive(Clone)]
struct TerminalEventListener {
    wakeups: tokio::sync::mpsc::Sender<()>,
}

impl EventListener for TerminalEventListener {
    fn send_event(&self, event: Event) {
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
    fn start(listener: TerminalEventListener) -> std::io::Result<Self> {
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
        let pty = tty::new(&options, window_size, 0)?;
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
    scroll_delta_y: f32,
    _repaint_task: Task<()>,
}

impl TerminalView {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let (wakeup_tx, mut wakeup_rx) = tokio::sync::mpsc::channel(1);
        let session = TerminalSession::start(TerminalEventListener { wakeups: wakeup_tx })
            .map_err(|error| error.to_string());
        let repaint_task = cx.spawn(async move |this, cx| {
            while wakeup_rx.recv().await.is_some() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });

        Self {
            session,
            focus: cx.focus_handle(),
            scroll_delta_y: 0.,
            _repaint_task: repaint_task,
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
            session.terminal.lock().scroll_display(Scroll::Bottom);
            self.scroll_delta_y = 0.;
            session.send(bytes);
            cx.stop_propagation();
        }
    }

    fn scroll(&mut self, delta: ScrollDelta, cx: &mut Context<Self>) {
        let Ok(session) = &self.session else {
            return;
        };
        let delta = delta.pixel_delta(px(CELL_HEIGHT));
        self.scroll_delta_y += f32::from(delta.y);
        let rows = (self.scroll_delta_y / SCROLL_PIXELS_PER_LINE).trunc() as i32;
        if rows != 0 {
            self.scroll_delta_y -= rows as f32 * SCROLL_PIXELS_PER_LINE;
            session.terminal.lock().scroll_display(Scroll::Delta(rows));
            cx.notify();
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
        let entity = cx.entity();
        div()
            .size_full()
            .bg(rgb(BACKGROUND))
            .occlude()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(TerminalElement { entity })
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
        let Ok(session) = &view.session else {
            return Self {
                cells: Vec::new(),
                cursor: None,
            };
        };
        let terminal = session.terminal.lock();
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
        _: &mut App,
    ) -> Self::PrepaintState {
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
