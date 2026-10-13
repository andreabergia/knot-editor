//! Terminal mouse wire formats and gesture ownership.

use alacritty_terminal::term::TermMode;
use gpui::{Modifiers, MouseButton, NavigationDirection, Pixels, Point, ScrollDelta};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerOwner {
    Local,
    Application,
    Link,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WheelRoute {
    Local,
    Application,
    Alternate,
}

pub(crate) fn interaction_modes(mode: TermMode) -> TermMode {
    mode & (TermMode::MOUSE_MODE
        | TermMode::SGR_MOUSE
        | TermMode::UTF8_MOUSE
        | TermMode::ALT_SCREEN
        | TermMode::ALTERNATE_SCROLL
        | TermMode::APP_CURSOR)
}

pub(crate) fn wheel_route(mode: TermMode, shift: bool) -> WheelRoute {
    if shift {
        WheelRoute::Local
    } else if mode.intersects(TermMode::MOUSE_MODE) {
        WheelRoute::Application
    } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
        WheelRoute::Alternate
    } else {
        WheelRoute::Local
    }
}

#[derive(Default)]
pub(crate) struct PointerState {
    pressed: Vec<(MouseButton, PointerOwner)>,
    last_motion: Option<(usize, usize, u8)>,
    wheel: Point<f32>,
    wheel_route: Option<WheelRoute>,
}

impl PointerState {
    pub(crate) fn clear_gestures(&mut self) {
        self.pressed.clear();
        self.last_motion = None;
    }
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
    pub(crate) fn press(&mut self, button: MouseButton, owner: PointerOwner) {
        self.pressed.retain(|(pressed, _)| *pressed != button);
        self.pressed.push((button, owner));
        self.last_motion = None;
    }
    pub(crate) fn application_buttons(&self) -> Vec<MouseButton> {
        self.pressed
            .iter()
            .filter_map(|(button, owner)| (*owner == PointerOwner::Application).then_some(*button))
            .collect()
    }
    pub(crate) fn owner(&self, button: MouseButton) -> Option<PointerOwner> {
        self.pressed
            .iter()
            .find(|(pressed, _)| *pressed == button)
            .map(|(_, owner)| *owner)
    }
    pub(crate) fn release(&mut self, button: MouseButton) -> Option<PointerOwner> {
        let owner = self.owner(button);
        self.pressed.retain(|(pressed, _)| *pressed != button);
        self.last_motion = None;
        owner
    }
    pub(crate) fn motion_changed(&mut self, column: usize, row: usize, code: u8) -> bool {
        let motion = (column, row, code);
        if self.last_motion == Some(motion) {
            return false;
        }
        self.last_motion = Some(motion);
        true
    }
    pub(crate) fn leave(&mut self) {
        self.last_motion = None;
    }
    pub(crate) fn wheel_steps(
        &mut self,
        delta: ScrollDelta,
        cell_width: Pixels,
        cell_height: Pixels,
        route: WheelRoute,
    ) -> (i32, i32) {
        if self.wheel_route != Some(route) {
            self.wheel = Point::default();
            self.wheel_route = Some(route);
        }
        let delta = match delta {
            ScrollDelta::Lines(delta) => delta,
            ScrollDelta::Pixels(delta) => {
                let divisor = if route == WheelRoute::Local {
                    gpui::px(4.)
                } else {
                    cell_height
                };
                gpui::point(delta.x / cell_width, delta.y / divisor)
            }
        };
        // Reversing direction starts a new accumulation instead of fighting old residue.
        if self.wheel.x * delta.x < 0. {
            self.wheel.x = 0.;
        }
        if self.wheel.y * delta.y < 0. {
            self.wheel.y = 0.;
        }
        self.wheel += delta;
        let horizontal = self.wheel.x.trunc() as i32;
        let vertical = self.wheel.y.trunc() as i32;
        self.wheel.x -= horizontal as f32;
        self.wheel.y -= vertical as f32;
        (horizontal, vertical)
    }
}

pub(crate) fn button_code(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::Navigate(NavigationDirection::Back) => 128,
        MouseButton::Navigate(NavigationDirection::Forward) => 129,
    }
}

pub(crate) fn modifier_code(modifiers: Modifiers) -> u8 {
    4 * u8::from(modifiers.shift) + 8 * u8::from(modifiers.alt) + 16 * u8::from(modifiers.control)
}

/// Coordinates are zero-based viewport cells. Unrepresentable legacy coordinates produce no report.
pub(crate) fn mouse_bytes(
    mode: TermMode,
    code: u8,
    column: usize,
    row: usize,
    release: bool,
) -> Vec<u8> {
    if mode.contains(TermMode::SGR_MOUSE) {
        return format!(
            "\x1b[<{code};{};{}{}",
            column + 1,
            row + 1,
            if release { 'm' } else { 'M' }
        )
        .into_bytes();
    }
    let limit = if mode.contains(TermMode::UTF8_MOUSE) {
        2015
    } else {
        223
    };
    if column >= limit || row >= limit {
        return Vec::new();
    }
    // Legacy releases use button 3 while retaining modifier bits.
    let code = if release { (code & 28) | 3 } else { code };
    let mut bytes = b"\x1b[M".to_vec();
    if mode.contains(TermMode::UTF8_MOUSE) {
        for value in [u32::from(code) + 32, column as u32 + 33, row as u32 + 33] {
            let mut buffer = [0; 4];
            bytes.extend_from_slice(
                char::from_u32(value)
                    .unwrap()
                    .encode_utf8(&mut buffer)
                    .as_bytes(),
            );
        }
    } else {
        bytes.extend_from_slice(&[code + 32, column as u8 + 33, row as u8 + 33]);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    #[test]
    fn button_motion_wheel_and_modifier_bytes_in_each_encoding() {
        for (button, base) in [
            (MouseButton::Left, 0),
            (MouseButton::Middle, 1),
            (MouseButton::Right, 2),
            (MouseButton::Navigate(NavigationDirection::Back), 128),
            (MouseButton::Navigate(NavigationDirection::Forward), 129),
        ] {
            assert_eq!(button_code(button), base);
        }
        for bits in 0..8 {
            let modifiers = Modifiers {
                shift: bits & 1 != 0,
                alt: bits & 2 != 0,
                control: bits & 4 != 0,
                ..Default::default()
            };
            let modifier = bits * 4;
            assert_eq!(modifier_code(modifiers), modifier);
            for base in [0, 1, 2, 32, 33, 34, 35, 64, 65, 66, 67, 128, 129] {
                let code = base + modifier;
                assert_eq!(
                    mouse_bytes(TermMode::SGR_MOUSE, code, 2, 4, false),
                    format!("\x1b[<{code};3;5M").as_bytes()
                );
                assert_eq!(
                    mouse_bytes(TermMode::NONE, code, 2, 4, false),
                    [27, 91, 77, code + 32, 35, 37]
                );
            }
            assert_eq!(
                mouse_bytes(TermMode::SGR_MOUSE, 2 + modifier, 2, 4, true),
                format!("\x1b[<{};3;5m", 2 + modifier).as_bytes()
            );
            assert_eq!(
                mouse_bytes(TermMode::NONE, 2 + modifier, 2, 4, true),
                [27, 91, 77, 35 + modifier, 35, 37]
            );
        }
    }

    #[test]
    fn coordinate_limits_do_not_wrap_or_alias_to_another_cell() {
        assert_eq!(
            mouse_bytes(TermMode::NONE, 0, 222, 222, false),
            [27, 91, 77, 32, 255, 255]
        );
        for (column, row) in [(223, 0), (0, 223), (65534, 65534)] {
            assert!(mouse_bytes(TermMode::NONE, 0, column, row, false).is_empty());
        }
        assert_eq!(
            mouse_bytes(TermMode::SGR_MOUSE, 0, 65534, 65534, false),
            b"\x1b[<0;65535;65535M"
        );
        assert_eq!(
            mouse_bytes(TermMode::UTF8_MOUSE, 128, 2, 4, false),
            "\x1b[M\u{a0}#%".as_bytes()
        );
        assert_eq!(
            mouse_bytes(TermMode::UTF8_MOUSE, 0, 222, 2014, false),
            "\x1b[M \u{ff}\u{7ff}".as_bytes()
        );
        assert!(mouse_bytes(TermMode::UTF8_MOUSE, 0, 2015, 0, false).is_empty());
        assert!(mouse_bytes(TermMode::UTF8_MOUSE, 0, 0, 2015, false).is_empty());
        assert_eq!(
            mouse_bytes(
                TermMode::UTF8_MOUSE | TermMode::SGR_MOUSE,
                0,
                3000,
                0,
                false
            ),
            b"\x1b[<0;3001;1M"
        );
    }

    #[test]
    fn gesture_ownership_motion_deduplication_and_reset() {
        let mut state = PointerState::default();
        assert_eq!(state.release(MouseButton::Left), None);
        state.press(MouseButton::Left, PointerOwner::Local);
        state.press(MouseButton::Right, PointerOwner::Application);
        assert_eq!(state.owner(MouseButton::Left), Some(PointerOwner::Local));
        assert_eq!(
            state.release(MouseButton::Right),
            Some(PointerOwner::Application)
        );
        assert!(state.motion_changed(1, 2, 35));
        assert!(!state.motion_changed(1, 2, 35));
        assert!(state.motion_changed(1, 2, 43));
        state.leave();
        assert!(state.motion_changed(1, 2, 43));
        state.clear_gestures();
        assert_eq!(state.owner(MouseButton::Left), None);
    }

    #[test]
    fn wheel_routes_fractional_deltas_direction_changes_and_route_changes() {
        let tracking =
            TermMode::MOUSE_REPORT_CLICK | TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL;
        assert_eq!(wheel_route(tracking, false), WheelRoute::Application);
        assert_eq!(wheel_route(tracking, true), WheelRoute::Local);
        assert_eq!(
            wheel_route(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL, false),
            WheelRoute::Alternate
        );
        assert_eq!(wheel_route(TermMode::ALT_SCREEN, false), WheelRoute::Local);
        let mut state = PointerState::default();
        let step = |state: &mut PointerState, delta, route| {
            state.wheel_steps(delta, px(8.), px(16.), route)
        };
        for _ in 0..3 {
            assert_eq!(
                step(
                    &mut state,
                    ScrollDelta::Pixels(point(px(2.), px(4.))),
                    WheelRoute::Application
                ),
                (0, 0)
            );
        }
        assert_eq!(
            step(
                &mut state,
                ScrollDelta::Pixels(point(px(2.), px(4.))),
                WheelRoute::Application
            ),
            (1, 1)
        );
        assert_eq!(
            step(
                &mut state,
                ScrollDelta::Lines(point(0., -2.5)),
                WheelRoute::Application
            ),
            (0, -2)
        );
        assert_eq!(
            step(
                &mut state,
                ScrollDelta::Lines(point(0., 0.5)),
                WheelRoute::Application
            ),
            (0, 0)
        );
        assert_eq!(
            step(
                &mut state,
                ScrollDelta::Lines(point(0., 0.5)),
                WheelRoute::Local
            ),
            (0, 0)
        );
        assert_eq!(
            step(
                &mut state,
                ScrollDelta::Lines(point(0., 0.5)),
                WheelRoute::Local
            ),
            (0, 1)
        );
        assert_eq!(
            step(
                &mut state,
                ScrollDelta::Pixels(point(px(0.), px(4.))),
                WheelRoute::Local
            ),
            (0, 1)
        );
        state.clear();
        assert_eq!(
            step(
                &mut state,
                ScrollDelta::Lines(point(0., 0.5)),
                WheelRoute::Alternate
            ),
            (0, 0)
        );
    }
}
