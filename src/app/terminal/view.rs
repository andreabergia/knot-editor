use alacritty_terminal::{
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point as TermPoint, Side},
    selection::{Selection, SelectionType},
    term::{TermMode, cell::Flags, point_to_viewport},
    vte::ansi::CursorShape,
};
use gpui::*;
use std::rc::Rc;

use crate::app::{
    TERMINAL_KEY_CONTEXT,
    terminal::colors::{BACKGROUND, FOREGROUND, dim, resolve_color},
    terminal::input::terminal_key_bytes,
    terminal::layout::TerminalLayout,
    terminal::links::{LinkGrid, LinkTarget},
    terminal::mouse::{
        PointerOwner, PointerState, WheelRoute, button_code, interaction_modes, modifier_code,
        mouse_bytes, wheel_route,
    },
    terminal::session::{TerminalAttachment, TerminalSession, TerminalSize, TerminalStatus},
    typography::{TEXT_SIZE, text_font},
};

type LinkOpener = Rc<dyn Fn(&str, &mut App)>;

/// Native terminal presentation attached to one stable session.
pub(crate) struct TerminalView {
    session: Entity<TerminalSession>,
    attachment: Option<TerminalAttachment>,
    _session_subscription: Subscription,
    focus: FocusHandle,
    pointer: PointerState,
    pointer_epoch: Option<(u64, u64, TermMode)>,
    local_dragged: bool,
    pointer_position: Option<Point<Pixels>>,
    pointer_modifiers: Modifiers,
    reported_cell: Option<(usize, usize)>,
    interaction_bounds: Bounds<Pixels>,
    layout: Option<TerminalLayout>,
    focus_subscriptions: Vec<Subscription>,
    hovered_link: Option<LinkTarget>,
    armed_link: Option<ArmedLink>,
    open_link: LinkOpener,
}

struct ArmedLink {
    grid: LinkGrid,
    target: LinkTarget,
    generation: u64,
    bounds: Bounds<Pixels>,
    layout: TerminalLayout,
    position: Point<Pixels>,
}

fn link_modifier(modifiers: Modifiers) -> bool {
    !modifiers.shift
        && if cfg!(target_os = "macos") {
            modifiers.platform
        } else {
            modifiers.control
        }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum TerminalCommand {
    Copy,
    Paste,
    ToggleMouseReporting,
}

impl TerminalView {
    pub(crate) fn execute_terminal_command(
        &mut self,
        command: TerminalCommand,
        cx: &mut Context<Self>,
    ) -> crate::app::product_commands::CommandClaim {
        use crate::app::product_commands::CommandClaim;
        if self.attachment.is_none() {
            return CommandClaim::Declined;
        }
        if command == TerminalCommand::ToggleMouseReporting {
            self.cancel_pointer(cx);
            self.session
                .update(cx, |session, cx| session.toggle_mouse_reporting(cx));
            cx.notify();
            return CommandClaim::Finished(crate::host::protocol::CommandOutcome::Completed);
        }
        if command == TerminalCommand::Paste {
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
        let subscription = cx.observe(&session, |this, _, cx| {
            this.clear_links();
            this.sync_pointer(cx);
            cx.notify();
        });
        cx.on_release(|view, cx| {
            view.cancel_pointer(cx);
            view.release_focus(cx);
        })
        .detach();
        Self {
            session,
            attachment: Some(attachment),
            _session_subscription: subscription,
            focus: cx.focus_handle(),
            pointer: PointerState::default(),
            pointer_epoch: None,
            local_dragged: false,
            pointer_position: None,
            pointer_modifiers: Modifiers::default(),
            reported_cell: None,
            interaction_bounds: Bounds::default(),
            layout: None,
            focus_subscriptions: Vec::new(),
            hovered_link: None,
            armed_link: None,
            open_link: Rc::new(|uri, cx| cx.open_url(uri)),
        }
    }

    /// Release the sole presentation before replacing or moving this view.
    pub(crate) fn detach(&mut self, cx: &mut Context<Self>) {
        self.cancel_pointer(cx);
        self.release_focus(cx);
        self.attachment = None;
        self.layout = None;
        self.reset_pointer();
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
        self.cancel_pointer(cx);
        self.session.update(cx, |session, cx| session.restart(cx));
        self.reset_pointer();
    }

    pub(crate) fn resize(&mut self, size: TerminalSize, cx: &mut Context<Self>) {
        if self.attachment.is_none() {
            return;
        }
        if self.session.read(cx).size() != size {
            self.cancel_pointer(cx);
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
        if self.attachment.is_some() {
            let focused = self.focus.is_focused(window) && window.is_window_active();
            if !focused {
                self.cancel_pointer(cx);
            }
            let attachment = self.attachment.as_ref().unwrap();
            self.session
                .update(cx, |session, _| session.set_focus(attachment, focused));
            cx.notify();
        }
    }

    fn send_user_input(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if self.attachment.is_none() {
            return;
        }
        if bytes.is_empty() || self.session.read(cx).status() != TerminalStatus::Running {
            return;
        }
        self.cancel_pointer(cx);
        let attachment = self.attachment.as_ref().unwrap();
        let session = self.session.read(cx);
        if let Some(terminal) = session.terminal() {
            terminal.lock().scroll_display(Scroll::Bottom);
        }
        session.send_input(attachment, session.generation(), bytes);
        self.reset_pointer();
        cx.notify();
    }

    fn clear_links(&mut self) {
        self.hovered_link = None;
        self.armed_link = None;
    }

    fn link_grid(&self, cx: &App) -> Option<LinkGrid> {
        self.attachment.as_ref()?;
        let terminal = self.session.read(cx).terminal()?;
        Some(LinkGrid::capture(&terminal.lock()))
    }

    fn hover_link(&mut self, position: Point<Pixels>, hovered: bool, cx: &mut Context<Self>) {
        let target = if hovered && self.interaction_bounds.contains(&position) {
            self.viewport_cell(position, cx)
                .and_then(|(column, row)| self.link_grid(cx)?.at(column, row).cloned())
        } else {
            None
        };
        if target != self.hovered_link {
            self.hovered_link = target;
            cx.notify();
        }
    }

    fn reset_pointer(&mut self) {
        self.clear_links();
        self.pointer.clear();
        self.pointer_epoch = None;
        self.local_dragged = false;
        self.pointer_position = None;
        self.pointer_modifiers = Modifiers::default();
        self.reported_cell = None;
    }

    fn cancel_pointer(&mut self, cx: &App) {
        if let Some(position) = self.pointer_position {
            let session = self.session.read(cx);
            let mode = session
                .terminal()
                .map(|terminal| *terminal.lock().mode())
                .unwrap_or_default();
            if self.pointer_epoch.is_some_and(|epoch| {
                epoch.0 == session.generation()
                    && epoch.1 == session.pointer_revision()
                    && epoch.2 == interaction_modes(mode)
            }) && mode.intersects(TermMode::MOUSE_MODE)
                && let Some((column, row)) = self.viewport_cell(position, cx)
            {
                for button in self.pointer.application_buttons() {
                    self.send_pointer(
                        self.release_bytes(
                            mode,
                            button_code(button) + modifier_code(self.pointer_modifiers),
                            column,
                            row,
                        ),
                        cx,
                    );
                }
            }
        }
        self.reset_pointer();
    }

    fn sync_pointer(&mut self, cx: &App) -> Option<TermMode> {
        if self.attachment.is_none() {
            self.reset_pointer();
            return None;
        }
        let session = self.session.read(cx);
        let mode = if session.status() == TerminalStatus::Running {
            session
                .terminal()
                .map(|terminal| *terminal.lock().mode())
                .unwrap_or_default()
        } else {
            TermMode::NONE
        };
        let mode = if session.mouse_reporting_enabled() {
            mode
        } else {
            mode & !TermMode::MOUSE_MODE
        };
        let epoch = (
            session.generation(),
            session.pointer_revision(),
            interaction_modes(mode),
        );
        if let Some(previous) = self.pointer_epoch {
            if previous.0 != epoch.0 || previous.2 != epoch.2 {
                self.clear_links();
                self.pointer.clear();
                self.reported_cell = None;
                self.local_dragged = false;
            } else if previous.1 != epoch.1 {
                self.clear_links();
                self.pointer.clear_gestures();
                self.reported_cell = None;
                self.local_dragged = false;
            }
        }
        self.pointer_epoch = Some(epoch);
        Some(mode)
    }

    fn viewport_cell(&self, position: Point<Pixels>, cx: &App) -> Option<(usize, usize)> {
        let layout = self.layout?;
        let size = self.session.read(cx).size();
        Some(layout.cell_at(position, self.interaction_bounds, size.columns, size.lines))
    }

    fn release_bytes(&self, mode: TermMode, code: u8, column: usize, row: usize) -> Vec<u8> {
        let bytes = mouse_bytes(mode, code, column, row, true);
        if bytes.is_empty() {
            // Balance the press at its last representable cell when the pointer exceeds wire limits.
            self.reported_cell
                .map(|(column, row)| mouse_bytes(mode, code, column, row, true))
                .unwrap_or_default()
        } else {
            bytes
        }
    }

    fn send_pointer(&self, bytes: Vec<u8>, cx: &App) {
        if let Some(attachment) = &self.attachment {
            let session = self.session.read(cx);
            session.send_input(attachment, session.generation(), bytes);
        }
    }

    fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mode) = self.sync_pointer(cx) else {
            return false;
        };
        let Some((column, row)) = self.viewport_cell(event.position, cx) else {
            return false;
        };
        self.pointer_position = Some(event.position);
        self.pointer_modifiers = event.modifiers;
        self.focus.focus(window);
        if event.button == MouseButton::Left
            && link_modifier(event.modifiers)
            && let Some(grid) = self.link_grid(cx)
            && let Some(target) = grid.at(column, row).cloned()
        {
            self.armed_link = Some(ArmedLink {
                grid,
                target,
                generation: self.session.read(cx).generation(),
                bounds: self.interaction_bounds,
                layout: self.layout.unwrap(),
                position: event.position,
            });
            self.pointer.press(event.button, PointerOwner::Link);
            return true;
        }
        if mode.intersects(TermMode::MOUSE_MODE) && !event.modifiers.shift {
            let code = button_code(event.button) + modifier_code(event.modifiers);
            let bytes = mouse_bytes(mode, code, column, row, false);
            if !bytes.is_empty() {
                self.reported_cell = Some((column, row));
                self.pointer.press(event.button, PointerOwner::Application);
                self.pointer.motion_changed(column, row, code + 32);
                self.send_pointer(bytes, cx);
            }
            return true;
        }
        if event.button == MouseButton::Left {
            self.pointer.press(event.button, PointerOwner::Local);
            self.local_dragged = false;
            self.select_at(event.position, self.interaction_bounds, true, cx);
            return true;
        }
        false
    }

    fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        hovered: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(mode) = self.sync_pointer(cx) else {
            return false;
        };
        self.pointer_position = Some(event.position);
        self.pointer_modifiers = event.modifiers;
        self.hover_link(event.position, hovered, cx);
        if let Some(button) = event.pressed_button {
            match self.pointer.owner(button) {
                Some(PointerOwner::Link) => {
                    if !link_modifier(event.modifiers)
                        || self.armed_link.as_ref().is_some_and(|armed| {
                            (event.position.x - armed.position.x).abs() > px(4.)
                                || (event.position.y - armed.position.y).abs() > px(4.)
                        })
                    {
                        self.armed_link = None;
                    }
                    return true;
                }
                Some(PointerOwner::Local) if button == MouseButton::Left => {
                    self.local_dragged = true;
                    self.select_at(event.position, self.interaction_bounds, false, cx);
                    return true;
                }
                Some(PointerOwner::Application) => {
                    if mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION)
                        && let Some((column, row)) = self.viewport_cell(event.position, cx)
                    {
                        let code = button_code(button) + 32 + modifier_code(event.modifiers);
                        if self.pointer.motion_changed(column, row, code) {
                            let bytes = mouse_bytes(mode, code, column, row, false);
                            if !bytes.is_empty() {
                                self.reported_cell = Some((column, row));
                            }
                            self.send_pointer(bytes, cx);
                        }
                    }
                    return true;
                }
                _ => return false,
            }
        }
        if !hovered {
            self.pointer.leave();
            return false;
        }
        if mode.contains(TermMode::MOUSE_MOTION) && !event.modifiers.shift {
            if let Some((column, row)) = self.viewport_cell(event.position, cx) {
                let code = 35 + modifier_code(event.modifiers);
                if self.pointer.motion_changed(column, row, code) {
                    let bytes = mouse_bytes(mode, code, column, row, false);
                    if !bytes.is_empty() {
                        self.reported_cell = Some((column, row));
                    }
                    self.send_pointer(bytes, cx);
                }
            }
            return true;
        }
        false
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) -> bool {
        let Some(mode) = self.sync_pointer(cx) else {
            return false;
        };
        self.pointer_position = Some(event.position);
        self.pointer_modifiers = event.modifiers;
        match self.pointer.release(event.button) {
            Some(PointerOwner::Link) => {
                if let Some(armed) = self.armed_link.take() {
                    let current = self.link_grid(cx);
                    let cell = self.viewport_cell(event.position, cx);
                    if link_modifier(event.modifiers)
                        && self.interaction_bounds.contains(&event.position)
                        && (event.position.x - armed.position.x).abs() <= px(4.)
                        && (event.position.y - armed.position.y).abs() <= px(4.)
                        && armed.generation == self.session.read(cx).generation()
                        && armed.bounds == self.interaction_bounds
                        && Some(armed.layout) == self.layout
                        && current.as_ref() == Some(&armed.grid)
                        && cell.is_some_and(|(column, row)| {
                            current.as_ref().and_then(|grid| grid.at(column, row))
                                == Some(&armed.target)
                        })
                    {
                        (self.open_link)(&armed.target.uri, cx);
                    }
                }
                true
            }
            Some(PointerOwner::Application) => {
                if let Some((column, row)) = self.viewport_cell(event.position, cx) {
                    self.send_pointer(
                        self.release_bytes(
                            mode,
                            button_code(event.button) + modifier_code(event.modifiers),
                            column,
                            row,
                        ),
                        cx,
                    );
                }
                true
            }
            Some(PointerOwner::Local) => {
                if self.local_dragged {
                    self.select_at(event.position, self.interaction_bounds, false, cx);
                }
                self.local_dragged = false;
                true
            }
            None => false,
        }
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) -> bool {
        self.clear_links();
        let Some(mode) = self.sync_pointer(cx) else {
            return false;
        };
        let Some(layout) = self.layout else {
            return false;
        };
        let route = wheel_route(mode, event.modifiers.shift);
        let (horizontal, vertical) =
            self.pointer
                .wheel_steps(event.delta, layout.cell_width, layout.cell_height, route);
        match route {
            WheelRoute::Local => {
                if vertical != 0 {
                    if let Some(terminal) = self.session.read(cx).terminal() {
                        terminal.lock().scroll_display(Scroll::Delta(vertical));
                    }
                    cx.notify();
                }
            }
            WheelRoute::Application => {
                if let Some((column, row)) = self.viewport_cell(event.position, cx) {
                    let mut bytes = Vec::new();
                    for (steps, positive, negative) in [(vertical, 64, 65), (horizontal, 66, 67)] {
                        let code = if steps > 0 { positive } else { negative }
                            + modifier_code(event.modifiers);
                        bytes.extend(
                            mouse_bytes(mode, code, column, row, false)
                                .repeat(steps.unsigned_abs() as usize),
                        );
                    }
                    self.send_pointer(bytes, cx);
                }
            }
            WheelRoute::Alternate => {
                let mut bytes = Vec::new();
                for (steps, positive, negative) in
                    [(vertical, "up", "down"), (horizontal, "left", "right")]
                {
                    let key =
                        Keystroke::parse(if steps > 0 { positive } else { negative }).unwrap();
                    bytes.extend(
                        terminal_key_bytes(&key, mode).repeat(steps.unsigned_abs() as usize),
                    );
                }
                self.send_pointer(bytes, cx);
            }
        }
        true
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
                        "TERMINAL · {} · mouse: {}",
                        match self.session.read(cx).status() {
                            TerminalStatus::Running => "running".to_owned(),
                            TerminalStatus::Exited(status) => format!("exited: {status}"),
                            TerminalStatus::Failed(error) => format!("failed: {error}"),
                            TerminalStatus::Closed => "closed".to_owned(),
                        },
                        if self.session.read(cx).mouse_reporting_enabled() {
                            "on"
                        } else {
                            "off"
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
    links: Option<LinkGrid>,
}

impl TerminalSnapshot {
    fn capture(view: &TerminalView, cx: &App) -> Self {
        if view.attachment.is_none() {
            return Self {
                cells: Vec::new(),
                cursor: None,
                links: None,
            };
        }
        let Some(terminal) = view.session.read(cx).terminal() else {
            return Self {
                cells: Vec::new(),
                cursor: None,
                links: None,
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

        Self {
            cells,
            cursor,
            links: Some(LinkGrid::capture(&terminal)),
        }
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
            if terminal.interaction_bounds != bounds || terminal.layout != Some(layout) {
                terminal.cancel_pointer(cx);
            }
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
        let scroll_hitbox = hitbox.clone();
        let entity = self.entity.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && scroll_hitbox.should_handle_scroll(window)
                && entity.update(cx, |terminal, cx| terminal.scroll(event, cx))
            {
                cx.stop_propagation();
            }
        });
        let press_hitbox = hitbox.clone();
        let entity = self.entity.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && press_hitbox.is_hovered(window)
                && entity.update(cx, |terminal, cx| terminal.mouse_down(event, window, cx))
            {
                cx.stop_propagation();
            }
        });
        let motion_hitbox = hitbox.clone();
        let entity = self.entity.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = motion_hitbox.is_hovered(window);
                if entity.update(cx, |terminal, cx| terminal.mouse_move(event, hovered, cx)) {
                    cx.stop_propagation();
                }
            }
        });
        let entity = self.entity.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble
                && entity.update(cx, |terminal, cx| terminal.mouse_up(event, cx))
            {
                cx.stop_propagation();
            }
        });

        let modifier_entity = self.entity.clone();
        window.on_modifiers_changed(move |event, _, cx| {
            modifier_entity.update(cx, |terminal, cx| {
                terminal.pointer_modifiers = event.modifiers;
                if !link_modifier(event.modifiers) {
                    terminal.armed_link = None;
                }
                cx.notify();
            });
        });
        let snapshot = TerminalSnapshot::capture(self.entity.read(cx), cx);
        let hovered = self
            .entity
            .read(cx)
            .hovered_link
            .as_ref()
            .filter(|target| {
                snapshot
                    .links
                    .as_ref()
                    .is_some_and(|grid| grid.links_contain(target))
            })
            .cloned();
        if hovered.is_some() && link_modifier(window.modifiers()) {
            window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        }
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
                    underline: (cell.flags.intersects(Flags::ALL_UNDERLINES)
                        || hovered
                            .as_ref()
                            .is_some_and(|link| link.contains(cell.column, cell.line)))
                    .then(|| UnderlineStyle {
                        thickness: px(1.),
                        ..Default::default()
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

#[cfg(all(test, unix))]
#[path = "mouse_tests.rs"]
mod mouse_tests;

use crate::app::command_declarations::{CommandContext, command};
use crate::app::product_commands::ProductCommandTarget;
use crate::host::protocol::{Command, CommandArgumentValue, CommandOutcome};

fn claim_terminal(command: &Command, target: &ProductCommandTarget, cx: &App) -> bool {
    let Some(crate::app::workbench::CommandView::Terminal(view)) = target.command_view(cx) else {
        return false;
    };
    let view = view.read(cx);
    if view.attachment.is_none() {
        return false;
    }
    if command.arguments != CommandArgumentValue::Null {
        return true;
    }
    if command.name.as_ref() == crate::app::product_commands::PASTE_COMMAND {
        return view.session.read(cx).status() == TerminalStatus::Running;
    }
    if command.name.as_ref() == crate::app::product_commands::COPY_COMMAND {
        return view
            .session
            .read(cx)
            .terminal()
            .and_then(|terminal| terminal.lock().selection_to_string())
            .is_some_and(|text| !text.is_empty());
    }
    true
}

#[command("copy", title = "Copy", id = TERMINAL_COPY_COMMAND, participant = Terminal, claim = claim_terminal)]
pub(crate) async fn terminal_copy(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    execute_terminal(context, TerminalCommand::Copy).await
}
#[command("editor.paste", title = "Paste", id = TERMINAL_PASTE_COMMAND, participant = Terminal, claim = claim_terminal)]
pub(crate) async fn terminal_paste(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    execute_terminal(context, TerminalCommand::Paste).await
}
#[command("terminal.toggle-mouse-reporting", title = "Toggle Terminal Mouse Reporting", id = TOGGLE_TERMINAL_MOUSE_REPORTING_COMMAND, participant = Terminal, claim = claim_terminal)]
pub(crate) async fn terminal_toggle(context: CommandContext, (): ()) -> Result<(), CommandOutcome> {
    execute_terminal(context, TerminalCommand::ToggleMouseReporting).await
}
async fn execute_terminal(
    context: CommandContext,
    command: TerminalCommand,
) -> Result<(), CommandOutcome> {
    context
        .foreground(move |target, _, cx| {
            let Some(crate::app::workbench::CommandView::Terminal(view)) = target.command_view(cx)
            else {
                return Err(CommandOutcome::InvalidTarget);
            };
            match view.update(cx, |view, cx| view.execute_terminal_command(command, cx)) {
                crate::app::product_commands::CommandClaim::Finished(CommandOutcome::Completed) => {
                    Ok(())
                }
                crate::app::product_commands::CommandClaim::Declined => {
                    Err(CommandOutcome::Unavailable)
                }
                _ => unreachable!("terminal commands complete immediately"),
            }
        })
        .await
}
