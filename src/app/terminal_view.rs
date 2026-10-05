use alacritty_terminal::{
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point as TermPoint, Side},
    selection::{Selection, SelectionType},
    term::{cell::Flags, color::Colors, point_to_viewport},
    vte::ansi::{Color, CursorShape, NamedColor, Rgb},
};
use gpui::*;

use super::{
    TERMINAL_KEY_CONTEXT,
    terminal_session::{TerminalAttachment, TerminalSession, TerminalSize, TerminalStatus},
};

const CELL_WIDTH: f32 = 8.;
const CELL_HEIGHT: f32 = 16.;
const FONT_SIZE: f32 = 13.;
const SCROLL_PIXELS_PER_LINE: f32 = 4.;
const BACKGROUND: u32 = 0x181818;
const FOREGROUND: u32 = 0xd4d4d4;

/// Native terminal presentation attached to one stable session.
pub(crate) struct TerminalView {
    session: Entity<TerminalSession>,
    attachment: Option<TerminalAttachment>,
    _session_subscription: Subscription,
    focus: FocusHandle,
    scroll_delta_y: f32,
    selecting: bool,
    interaction_bounds: Bounds<Pixels>,
}

impl TerminalView {
    pub(crate) fn handle_command(
        &mut self,
        command: &crate::host::protocol::Command,
        cx: &mut Context<Self>,
    ) -> super::product_commands::CommandClaim {
        use super::product_commands::CommandClaim;
        if command.name.as_ref() != super::product_commands::COPY_COMMAND
            || self.attachment.is_none()
        {
            return CommandClaim::Declined;
        }
        if let Err(claim) = super::product_commands::validate_native_arguments(command) {
            return claim;
        }
        let selected = self
            .session
            .read(cx)
            .terminal()
            .and_then(|terminal| terminal.lock().selection_to_string())
            .filter(|text| !text.is_empty());
        if let Some(text) = selected {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            CommandClaim::Finished(crate::host::protocol::CommandOutcome::Completed)
        } else {
            CommandClaim::Declined
        }
    }

    pub(crate) fn new(session: Entity<TerminalSession>, cx: &mut Context<Self>) -> Self {
        let attachment = session
            .update(cx, |session, _| session.attach())
            .expect("terminal session already has a view");
        let subscription = cx.observe(&session, |_, _, cx| cx.notify());
        Self {
            session,
            attachment: Some(attachment),
            _session_subscription: subscription,
            focus: cx.focus_handle(),
            scroll_delta_y: 0.,
            selecting: false,
            interaction_bounds: Bounds::default(),
        }
    }

    /// Release the sole presentation before replacing or moving this view.
    pub(crate) fn detach(&mut self, cx: &mut Context<Self>) {
        self.attachment = None;
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.attachment.is_none() {
            return;
        }
        self.session.update(cx, |session, cx| session.close(cx));
    }

    fn restart(&mut self, cx: &mut Context<Self>) {
        if self.attachment.is_none() {
            return;
        }
        self.session.update(cx, |session, cx| session.restart(cx));
        self.scroll_delta_y = 0.;
    }

    pub(crate) fn resize(&mut self, size: TerminalSize, cx: &mut Context<Self>) {
        if self.attachment.is_none() {
            return;
        }
        self.session.update(cx, |session, _| session.resize(size));
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.attachment.is_none() {
            return;
        }
        if self.session.read(cx).status() != TerminalStatus::Running {
            return;
        }
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
            if let Some(terminal) = self.session.read(cx).terminal() {
                terminal.lock().scroll_display(Scroll::Bottom);
            }
            self.scroll_delta_y = 0.;
            self.session.read(cx).send(bytes);
            cx.stop_propagation();
        }
    }

    fn scroll(&mut self, delta: ScrollDelta, cx: &mut Context<Self>) {
        if self.attachment.is_none() {
            return;
        }
        let delta = delta.pixel_delta(px(CELL_HEIGHT));
        self.scroll_delta_y += f32::from(delta.y);
        let rows = (self.scroll_delta_y / SCROLL_PIXELS_PER_LINE).trunc() as i32;
        if rows != 0 {
            self.scroll_delta_y -= rows as f32 * SCROLL_PIXELS_PER_LINE;
            if let Some(terminal) = self.session.read(cx).terminal() {
                terminal.lock().scroll_display(Scroll::Delta(rows));
            }
            cx.notify();
        }
    }

    fn select_at(
        &mut self,
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        start: bool,
        cx: &mut Context<Self>,
    ) {
        if self.attachment.is_none() {
            return;
        }
        let Some(terminal) = self.session.read(cx).terminal().cloned() else {
            return;
        };
        let mut terminal = terminal.lock();
        let column = (f32::from(position.x - bounds.origin.x) / CELL_WIDTH)
            .floor()
            .max(0.) as usize;
        let line = (f32::from(position.y - bounds.origin.y) / CELL_HEIGHT)
            .floor()
            .max(0.) as usize;
        let point = TermPoint::new(
            Line(
                line.min(terminal.screen_lines().saturating_sub(1)) as i32
                    - terminal.grid().display_offset() as i32,
            ),
            Column(column.min(terminal.columns().saturating_sub(1))),
        );
        if start {
            terminal.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
            self.selecting = true;
        } else if let Some(selection) = terminal.selection.as_mut() {
            selection.update(point, Side::Right);
        }
        drop(terminal);
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn interaction_bounds(&self) -> Bounds<Pixels> {
        self.interaction_bounds
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
                    .child(format!(
                        "TERMINAL · {}",
                        match self.session.read(cx).status() {
                            TerminalStatus::Running => "running".to_owned(),
                            TerminalStatus::Exited(status) => format!("exited: {status}"),
                            TerminalStatus::Failed(error) => format!("failed: {error}"),
                            TerminalStatus::Closed => "closed".to_owned(),
                        }
                    ))
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
    selected: bool,
}

struct TerminalSnapshot {
    cells: Vec<RenderCell>,
    cursor: Option<(usize, usize, CursorShape)>,
}

impl TerminalSnapshot {
    fn capture(view: &TerminalView, cx: &App) -> Self {
        if view.attachment.is_none() {
            return Self {
                cells: Vec::new(),
                cursor: None,
            };
        }
        let Some(terminal) = view.session.read(cx).terminal() else {
            return Self {
                cells: Vec::new(),
                cursor: None,
            };
        };
        let terminal = terminal.lock();
        let content = terminal.renderable_content();
        let display_offset = content.display_offset;
        let selection = content.selection;
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
                    selected: selection.is_some_and(|range| range.contains(indexed.point)),
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
        let size = TerminalSize::from_pixels(
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
            window.scale_factor(),
        );
        self.entity.update(cx, |terminal, cx| {
            terminal.interaction_bounds = bounds;
            terminal.resize(size, cx);
        });
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
                entity.update(cx, |terminal, cx| {
                    terminal.scroll(event.delta, cx);
                });
                cx.stop_propagation();
            }
        });

        let entity = self.entity.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && bounds.contains(&event.position)
            {
                entity.update(cx, |terminal, cx| {
                    terminal.select_at(event.position, bounds, true, cx);
                });
            }
        });
        let entity = self.entity.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && event.dragging() {
                entity.update(cx, |terminal, cx| {
                    if terminal.selecting {
                        terminal.select_at(event.position, bounds, false, cx);
                    }
                });
            }
        });
        let entity = self.entity.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                entity.update(cx, |terminal, _| terminal.selecting = false);
            }
        });

        let snapshot = TerminalSnapshot::capture(self.entity.read(cx), cx);
        let cell_width = px(CELL_WIDTH);
        let cell_height = px(CELL_HEIGHT);
        let mask = Some(ContentMask { bounds });
        window.with_content_mask(mask, |window| {
            for cell in &snapshot.cells {
                let origin = point(
                    bounds.origin.x + cell_width * cell.column,
                    bounds.origin.y + cell_height * cell.line,
                );
                if cell.background != BACKGROUND || cell.selected {
                    let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                        cell_width * 2
                    } else {
                        cell_width
                    };
                    window.paint_quad(fill(
                        Bounds {
                            origin,
                            size: size(width, cell_height),
                        },
                        rgb(if cell.selected {
                            0x355a88
                        } else {
                            cell.background
                        }),
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
                        window.paint_quad(quad(
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
                    window.paint_quad(fill(cursor_bounds, rgba(0xd4d4d480)));
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
    ((u32::from(rgb.r) * 2 / 3) << 16)
        | ((u32::from(rgb.g) * 2 / 3) << 8)
        | (u32::from(rgb.b) * 2 / 3)
}
