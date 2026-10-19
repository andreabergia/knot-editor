//! Foreground-owned transient extension interactions.
//!
//! A picker and a text prompt are window-local overlays that own a shared
//! native text field and settle exactly once. The product shell keeps at most
//! one live extension interaction per window; confirmations and messages use
//! the native platform prompt instead of an overlay.

use gpui::{prelude::*, *};

use crate::host::protocol::{CommandInvocationId, ExtensionId, ExtensionLifecycleId, PickerItem};

use super::text_input::{TextInput, TextInputEvent};

const PANEL_WIDTH: f32 = 560.;
const ROW_HEIGHT: f32 = 28.;

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
    Picker(Entity<PickerView>),
    Input(Entity<InputView>),
}

/// One settled transient interaction result before it is mapped to a public
/// typed value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TransientOutcome {
    Picker(String),
    Input(String),
    Confirmed,
    Dismissed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PickerEvent {
    Selected(String),
    Dismissed,
}

pub(crate) struct PickerView {
    title: SharedString,
    items: Vec<PickerItem>,
    query: String,
    selected: usize,
    input: Entity<TextInput>,
    _input_subscription: Subscription,
}

impl PickerView {
    pub(crate) fn new(title: String, items: Vec<PickerItem>, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("", cx));
        let subscription = cx.subscribe(
            &input,
            |this: &mut Self, _input, _event: &TextInputEvent, cx| {
                this.query = this.input.read(cx).text().to_string();
                this.selected = 0;
                cx.notify();
            },
        );
        Self {
            title: title.into(),
            items,
            query: String::new(),
            selected: 0,
            input,
            _input_subscription: subscription,
        }
    }

    pub(crate) fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle()
    }

    #[cfg(test)]
    pub(crate) fn selected_item_id(&self) -> Option<&str> {
        self.visible_items()
            .get(self.selected)
            .map(|item| item.id.as_str())
    }

    #[cfg(test)]
    fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_text(query, cx));
        self.query = self.input.read(cx).text().to_string();
        self.selected = 0;
        cx.notify();
    }

    fn visible_items(&self) -> Vec<&PickerItem> {
        let query = self.query.to_lowercase();
        self.items
            .iter()
            .filter(|item| {
                query.is_empty()
                    || item.label.to_lowercase().contains(&query)
                    || item
                        .description
                        .as_deref()
                        .is_some_and(|description| description.to_lowercase().contains(&query))
            })
            .collect()
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.visible_items().len();
        if count == 0 {
            self.selected = 0;
        } else {
            self.selected = self
                .selected
                .saturating_add_signed(delta)
                .min(count.saturating_sub(1));
        }
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(item) = self.visible_items().get(self.selected) {
            cx.emit(PickerEvent::Selected(item.id.clone()));
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.to_lowercase().as_str() {
            "escape" => cx.emit(PickerEvent::Dismissed),
            "enter" => self.confirm(cx),
            "up" => self.move_selection(-1, cx),
            "down" => self.move_selection(1, cx),
            "pageup" => self.move_selection(-10, cx),
            "pagedown" => self.move_selection(10, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl EventEmitter<PickerEvent> for PickerView {}

impl Render for PickerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let selected = self.selected;
        let rows = self
            .visible_items()
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                let entity = entity.clone();
                let id = item.id.clone();
                let description = item.description.clone();
                div()
                    .id(("picker-row", index))
                    .h(px(ROW_HEIGHT))
                    .px_3()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .when(index == selected, |row| row.bg(rgb(0x2a4a7a)))
                    .hover(|row| row.bg(rgb(0x333333)))
                    .child(item.label.clone())
                    .children(description.map(|description| {
                        div().text_xs().text_color(rgb(0x999999)).child(description)
                    }))
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |_, cx| {
                            cx.emit(PickerEvent::Selected(id.clone()));
                        });
                    })
            })
            .collect::<Vec<_>>();
        let empty = self.visible_items().is_empty();
        div()
            .id("extension-picker")
            .w(px(PANEL_WIDTH))
            .max_h(px(420.))
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x555555))
            .bg(rgb(0x252526))
            .shadow_lg()
            .key_context(super::TRANSIENT_KEY_CONTEXT)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(rgb(0xcccccc))
                    .child(self.title.clone()),
            )
            .child(div().px_3().pb_2().child(self.input.clone()))
            .child(
                div()
                    .id("extension-picker-results")
                    .flex_1()
                    .overflow_scroll()
                    .when(empty, |results| {
                        results.child(
                            div()
                                .px_3()
                                .py_2()
                                .text_color(rgb(0x999999))
                                .child("No matching items"),
                        )
                    })
                    .children(rows),
            )
    }
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
            .border_color(rgb(0x555555))
            .bg(rgb(0x252526))
            .shadow_lg()
            .key_context(super::TRANSIENT_KEY_CONTEXT)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_color(rgb(0xcccccc))
                    .child(self.title.clone()),
            )
            .child(div().px_3().pb_3().child(self.input.clone()))
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::TestAppContext;

    use super::{PickerEvent, PickerView};
    use crate::host::protocol::PickerItem;

    fn item(id: &str, label: &str, description: Option<&str>) -> PickerItem {
        PickerItem {
            id: id.into(),
            label: label.into(),
            description: description.map(str::to_owned),
        }
    }

    #[gpui::test]
    fn filters_by_label_and_description_and_confirms(cx: &mut TestAppContext) {
        let (picker, cx) = cx.add_window_view(|_, cx| {
            PickerView::new(
                "Pick a project".into(),
                vec![
                    item("alpha", "Alpha", Some("first")),
                    item("beta", "Beta", Some("second")),
                ],
                cx,
            )
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&picker, move |_, event, _| {
                observed.borrow_mut().push(event.clone());
            })
        });
        picker.update(cx, |picker, cx| {
            picker.set_query("second", cx);
            assert_eq!(picker.selected_item_id(), Some("beta"));
            picker.confirm(cx);
        });
        assert!(events.borrow().as_slice() == [PickerEvent::Selected("beta".into())].as_slice());
    }

    #[gpui::test]
    fn no_matches_blocks_confirmation(cx: &mut TestAppContext) {
        let (picker, cx) = cx.add_window_view(|_, cx| {
            PickerView::new("Pick".into(), vec![item("a", "Alpha", None)], cx)
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&picker, move |_, event, _| {
                observed.borrow_mut().push(event.clone());
            })
        });
        picker.update(cx, |picker, cx| {
            picker.set_query("zzz", cx);
            assert_eq!(picker.selected_item_id(), None);
            picker.confirm(cx);
            picker.move_selection(1, cx);
            assert_eq!(picker.selected, 0);
        });
        assert!(events.borrow().is_empty());
    }
}
