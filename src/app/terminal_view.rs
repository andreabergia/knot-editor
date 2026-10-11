use alacritty_terminal::{
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point as TermPoint, Side},
    selection::{Selection, SelectionType},
    term::{cell::Flags, point_to_viewport},
    vte::ansi::CursorShape,
};
use gpui::*;

use super::{
    TERMINAL_KEY_CONTEXT,
    terminal_colors::{BACKGROUND, FOREGROUND, dim, resolve_color},
    terminal_input::terminal_key_bytes,
    terminal_layout::TerminalLayout,
    terminal_session::{TerminalAttachment, TerminalSession, TerminalSize, TerminalStatus},
    typography::{TEXT_SIZE, text_font},
};

const SCROLL_PIXELS_PER_LINE: f32 = 4.;

/// Native terminal presentation attached to one stable session.
pub(crate) struct TerminalView {
    session: Entity<TerminalSession>,
    attachment: Option<TerminalAttachment>,
    _session_subscription: Subscription,
    focus: FocusHandle,
    scroll_delta_y: f32,
    selecting: bool,
    interaction_bounds: Bounds<Pixels>,
    layout: Option<TerminalLayout>,
    focus_subscriptions: Vec<Subscription>,
}

impl TerminalView {
    pub(crate) fn handle_command(
        &mut self,
        command: &crate::host::protocol::Command,
        cx: &mut Context<Self>,
    ) -> super::product_commands::CommandClaim {
        use super::product_commands::CommandClaim;
        use super::product_commands::{COPY_COMMAND, PASTE_COMMAND};
        if !matches!(command.name.as_ref(), COPY_COMMAND | PASTE_COMMAND)
            || self.attachment.is_none()
        {
            return CommandClaim::Declined;
        }
        if let Err(claim) = super::product_commands::validate_native_arguments(command) {
            return claim;
        }
        if command.name.as_ref() == PASTE_COMMAND {
            if self.session.read(cx).status() != TerminalStatus::Running {
                return CommandClaim::Declined;
            }
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                let bracketed = self.session.read(cx).terminal().is_some_and(|terminal| {
                    terminal
                        .lock()
                        .mode()
                        .contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE)
                });
                let text = if bracketed {
                    format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
                } else {
                    text.replace("\r\n", "\r").replace('\n', "\r")
                };
                self.send_user_input(text.into_bytes(), cx);
            }
            return CommandClaim::Finished(crate::host::protocol::CommandOutcome::Completed);
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
        cx.on_release(|view, cx| view.release_focus(cx)).detach();
        Self {
            session,
            attachment: Some(attachment),
            _session_subscription: subscription,
            focus: cx.focus_handle(),
            scroll_delta_y: 0.,
            selecting: false,
            interaction_bounds: Bounds::default(),
            layout: None,
            focus_subscriptions: Vec::new(),
        }
    }

    /// Release the sole presentation before replacing or moving this view.
    pub(crate) fn detach(&mut self, cx: &mut Context<Self>) {
        self.release_focus(cx);
        self.attachment = None;
        self.layout = None;
        self.selecting = false;
        cx.notify();
    }

    pub(crate) fn is_attached_to(&self, session: &Entity<TerminalSession>) -> bool {
        self.attachment.is_some() && &self.session == session
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
        let mode = self
            .session
            .read(cx)
            .terminal()
            .map(|terminal| *terminal.lock().mode())
            .unwrap_or_default();
        let bytes = terminal_key_bytes(&event.keystroke, mode);
        if !bytes.is_empty() {
            self.send_user_input(bytes, cx);
            cx.stop_propagation();
        }
    }

    fn release_focus(&mut self, cx: &mut App) {
        if let Some(attachment) = &self.attachment {
            self.session
                .update(cx, |session, _| session.set_focus(attachment, false));
        }
    }

    fn sync_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(attachment) = &self.attachment {
            let focused = self.focus.is_focused(window) && window.is_window_active();
            self.session
                .update(cx, |session, _| session.set_focus(attachment, focused));
            cx.notify();
        }
    }

    fn send_user_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let Some(attachment) = &self.attachment else {
            return;
        };
        if bytes.is_empty() || self.session.read(cx).status() != TerminalStatus::Running {
            return;
        }
        let session = self.session.read(cx);
        if let Some(terminal) = session.terminal() {
            terminal.lock().scroll_display(Scroll::Bottom);
        }
        session.send_input(attachment, session.generation(), bytes);
        self.scroll_delta_y = 0.;
        cx.notify();
    }

    fn scroll(&mut self, delta: ScrollDelta, cx: &mut Context<Self>) {
        if self.attachment.is_none() {
            return;
        }
        let Some(layout) = self.layout else {
            return;
        };
        let delta = delta.pixel_delta(layout.cell_height);
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
        let Some(layout) = self.layout else {
            return;
        };
        let (column, line) = layout.cell_at(
            position,
            bounds,
            terminal.columns(),
            terminal.screen_lines(),
        );
        let point = TermPoint::new(
            Line(line as i32 - terminal.grid().display_offset() as i32),
            Column(column),
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

#[cfg(test)]
mod tests {
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
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.focus_subscriptions.is_empty() {
            self.focus_subscriptions = vec![
                cx.on_focus(&self.focus.clone(), window, Self::sync_focus),
                cx.on_blur(&self.focus.clone(), window, Self::sync_focus),
                cx.observe_window_activation(window, Self::sync_focus),
            ];
            self.sync_focus(window, cx);
        }
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
        let layout = TerminalLayout::measure(window);
        let size = layout.grid_size(bounds);
        self.entity.update(cx, |terminal, cx| {
            terminal.interaction_bounds = bounds;
            terminal.layout = Some(layout);
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
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && bounds.contains(&event.position)
            {
                entity.update(cx, |terminal, cx| {
                    if terminal.attachment.is_some() {
                        terminal.focus.focus(window);
                    }
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
        let Some(layout) = self.entity.read(cx).layout else {
            return;
        };
        let cell_width = layout.cell_width;
        let cell_height = layout.cell_height;
        let mask = Some(ContentMask { bounds });
        window.with_content_mask(mask, |window| {
            for cell in &snapshot.cells {
                let origin = layout.origin(bounds, cell.column, cell.line);
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
                let origin = layout.origin(bounds, column, line);
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
                let font = text_font(
                    cell.flags.contains(Flags::BOLD),
                    cell.flags.contains(Flags::ITALIC),
                );
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
                    px(TEXT_SIZE),
                    &[run],
                    None,
                );
                let origin = layout.origin(bounds, cell.column, cell.line);
                let width = cell_width
                    * if cell.flags.contains(Flags::WIDE_CHAR) {
                        2
                    } else {
                        1
                    };
                window.with_content_mask(
                    Some(ContentMask {
                        bounds: Bounds::new(origin, size(width, cell_height)),
                    }),
                    |window| {
                        let origin = layout.text_origin(origin, shaped.ascent, shaped.descent);
                        let _ = shaped.paint(origin, cell_height, window, cx);
                    },
                );
            }
        });
    }
}
