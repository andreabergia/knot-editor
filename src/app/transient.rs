//! Foreground-owned transient extension interactions.
//!
//! A picker and a text prompt are window-local overlays that own a shared
//! native text field and settle exactly once; the picker is the shared
//! [`Picker`]. The product shell keeps at most
//! one live extension interaction per window; confirmations and messages use
//! the native platform prompt instead of an overlay.

use std::path::PathBuf;

use gpui::{prelude::*, *};

use crate::host::protocol::{CommandInvocationId, ExtensionId, ExtensionLifecycleId};

use super::picker::Picker;
use super::text_input::TextInput;
use super::theme::{color, theme};

const PANEL_WIDTH: f32 = 560.;

/// Identity of the extension operation that owns one active interaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TransientOwner {
    pub(crate) extension: ExtensionId,
    pub(crate) lifecycle: ExtensionLifecycleId,
    pub(crate) invocation: Option<CommandInvocationId>,
}

/// Title and optional body shared by native prompt interactions.
pub(crate) struct PromptText {
    pub(crate) title: String,
    pub(crate) message: Option<String>,
}

/// One rendered overlay surface owned by the window.
pub(crate) enum TransientSurface {
    Picker(Entity<Picker>),
    Input(Entity<InputView>),
}

/// One settled transient interaction result before it is mapped to a public
/// typed value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TransientOutcome {
    Picker(String),
    Input(String),
    Confirmed,
    Folder(PathBuf),
    Failed(String),
    Dismissed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum InputEvent {
    Accepted(String),
    Dismissed,
}

pub(crate) struct InputView {
    title: SharedString,
    input: Entity<TextInput>,
}

impl InputView {
    pub(crate) fn new(title: String, initial: String, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new(initial, cx));
        Self {
            title: title.into(),
            input,
        }
    }

    pub(crate) fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle()
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.to_lowercase().as_str() {
            "escape" => cx.emit(InputEvent::Dismissed),
            "enter" => {
                let value = self.input.read(cx).text().to_string();
                cx.emit(InputEvent::Accepted(value));
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}
impl EventEmitter<InputEvent> for InputView {}

impl Render for InputView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("extension-input")
            .w(px(PANEL_WIDTH))
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(theme(cx).color(color::POPOVER_BORDER))
            .bg(theme(cx).color(color::POPOVER_BACKGROUND))
            .shadow_lg()
            .key_context(super::TRANSIENT_KEY_CONTEXT)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(theme(cx).color(color::TEXT))
                    .child(self.title.clone()),
            )
            .child(div().px_3().pb_3().child(self.input.clone()))
    }
}
