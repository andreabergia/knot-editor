//! Shared single-line native text entry.
//!
//! The field reserves one line, owns its caret and selection in UTF-16 offsets
//! at the gpui input boundary, and exposes the same editing path to transient
//! picker/input surfaces and the command palette. Typing, IME preedit, and
//! clipboard flow through gpui; the field supplies ordinary arrow/home/end
//! movement, shift selection, and pointer click/drag selection.

use gpui::{prelude::*, *};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

use super::typography::{TEXT_SIZE as FONT_SIZE, text_font};

const INPUT_LINE_HEIGHT: f32 = 22.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextInputEvent {
    Changed,
}

pub(crate) struct TextInput {
    text: String,
    /// Selection endpoints in UTF-8 byte offsets into `text`.
    anchor: usize,
    caret: usize,
    /// IME preedit range in UTF-16 offsets into `text`.
    marked: Option<Range<usize>>,
    focus: FocusHandle,
    /// Last painted text bounds, used for pointer hit testing.
    bounds: Option<Bounds<Pixels>>,
}

impl TextInput {
    pub(crate) fn new(initial: impl Into<String>, cx: &mut Context<Self>) -> Self {
        let text = initial.into();
        let caret = text.len();
        Self {
            text,
            anchor: caret,
            caret,
            marked: None,
            focus: cx.focus_handle(),
            bounds: None,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    #[cfg(test)]
    pub(crate) fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.text = text.into();
        self.caret = self.text.len();
        self.anchor = self.caret;
        self.marked = None;
        cx.notify();
        cx.emit(TextInputEvent::Changed);
    }

    fn selection(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    fn selected_text(&self) -> &str {
        &self.text[self.selection()]
    }

    fn set_caret(&mut self, byte: usize, extend: bool) {
        self.caret = byte.min(self.text.len());
        if !extend {
            self.anchor = self.caret;
        }
    }

    /// Grapheme boundaries followed by the exclusive end of the text.
    fn boundaries(&self) -> Vec<usize> {
        self.text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .chain(std::iter::once(self.text.len()))
            .collect()
    }

    fn move_by(&mut self, delta: isize, extend: bool) {
        let boundaries = self.boundaries();
        let position = boundaries
            .iter()
            .position(|boundary| *boundary == self.caret)
            .unwrap_or(boundaries.len().saturating_sub(1));
        let next = position
            .saturating_add_signed(delta)
            .min(boundaries.len().saturating_sub(1));
        self.set_caret(boundaries[next], extend);
    }

    fn move_to_start(&mut self, extend: bool) {
        self.set_caret(0, extend);
    }

    fn move_to_end(&mut self, extend: bool) {
        self.set_caret(self.text.len(), extend);
    }

    fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    fn replace_selection(&mut self, text: &str) {
        let selection = self.selection();
        self.text.replace_range(selection.clone(), text);
        self.caret = selection.start + text.len();
        self.anchor = self.caret;
        self.marked = None;
    }

    fn delete_backward(&mut self) -> bool {
        let selection = self.selection();
        if selection.start != selection.end {
            self.replace_selection("");
            return true;
        }
        let boundaries = self.boundaries();
        let Some(position) = boundaries.iter().position(|b| *b == self.caret) else {
            return false;
        };
        if position == 0 {
            return false;
        }
        let start = boundaries[position - 1];
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
        self.anchor = start;
        true
    }

    fn delete_forward(&mut self) -> bool {
        let selection = self.selection();
        if selection.start != selection.end {
            self.replace_selection("");
            return true;
        }
        let boundaries = self.boundaries();
        let Some(position) = boundaries.iter().position(|b| *b == self.caret) else {
            return false;
        };
        if position + 1 >= boundaries.len() {
            return false;
        }
        let end = boundaries[position + 1];
        self.text.replace_range(self.caret..end, "");
        true
    }

    fn copy(&self, cx: &mut Context<Self>) {
        let selected = self.selected_text();
        if !selected.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(selected.to_string()));
        }
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected_text();
        if selected.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(selected.to_string()));
        self.replace_selection("");
        cx.notify();
        cx.emit(TextInputEvent::Changed);
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .map(|text| sanitize(&text))
        else {
            return;
        };
        if text.is_empty() {
            return;
        }
        self.replace_selection(&text);
        cx.notify();
        cx.emit(TextInputEvent::Changed);
    }

    fn utf16_len(text: &str) -> usize {
        text.chars().map(char::len_utf16).sum()
    }

    fn byte_for_utf16(text: &str, offset: usize) -> usize {
        let mut utf16 = 0;
        for (byte, ch) in text.char_indices() {
            if utf16 >= offset {
                return byte;
            }
            utf16 += ch.len_utf16();
        }
        text.len()
    }

    fn utf16_for_byte(text: &str, byte: usize) -> usize {
        text[..byte.min(text.len())]
            .chars()
            .map(char::len_utf16)
            .sum()
    }

    fn shape(&self, window: &Window) -> ShapedLine {
        let run = TextRun {
            len: self.text.len(),
            font: text_font(false, false),
            color: rgb(0xffffff).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window.text_system().shape_line(
            SharedString::from(self.text.clone()),
            px(FONT_SIZE),
            &[run],
            None,
        )
    }

    fn index_for_position(&self, window: &Window, x: Pixels) -> usize {
        let Some(bounds) = self.bounds else {
            return Self::utf16_for_byte(&self.text, self.text.len());
        };
        let shaped = self.shape(window);
        let byte = shaped.closest_index_for_x(x - bounds.origin.x);
        Self::utf16_for_byte(&self.text, byte.min(self.text.len()))
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.to_lowercase();
        let command = event.keystroke.modifiers.platform || event.keystroke.modifiers.control;
        let extend = event.keystroke.modifiers.shift;
        if command {
            match key.as_str() {
                "a" => self.select_all(),
                "c" => self.copy(cx),
                "x" => self.cut(cx),
                "v" => self.paste(cx),
                "left" => self.move_to_start(extend),
                "right" => self.move_to_end(extend),
                _ => return,
            }
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let changed = match key.as_str() {
            "backspace" => self.delete_backward(),
            "delete" => self.delete_forward(),
            "left" => {
                self.move_by(-1, extend);
                false
            }
            "right" => {
                self.move_by(1, extend);
                false
            }
            "home" => {
                self.move_to_start(extend);
                false
            }
            "end" => {
                self.move_to_end(extend);
                false
            }
            _ => return,
        };
        cx.notify();
        if changed {
            cx.emit(TextInputEvent::Changed);
        }
        cx.stop_propagation();
    }

    fn begin_pointer_selection(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset = self.index_for_position(window, event.position.x);
        self.set_caret(offset, false);
        cx.notify();
        cx.stop_propagation();
    }

    fn extend_pointer_selection(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let offset = self.index_for_position(window, event.position.x);
        self.set_caret(offset, true);
        cx.notify();
    }
}

/// Single-line fields never accept newlines; drop them so a stray line break
/// cannot produce a second visual line.
fn sanitize(text: &str) -> String {
    text.replace(['\r', '\n'], "")
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let start = Self::byte_for_utf16(&self.text, range.start);
        let end = Self::byte_for_utf16(&self.text, range.end);
        if start > end {
            return None;
        }
        *adjusted =
            Some(Self::utf16_for_byte(&self.text, start)..Self::utf16_for_byte(&self.text, end));
        Some(self.text[start..end].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let reversed = self.anchor > self.caret;
        let range = Self::utf16_for_byte(&self.text, self.anchor.min(self.caret))
            ..Self::utf16_for_byte(&self.text, self.anchor.max(self.caret));
        Some(UTF16Selection { range, reversed })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked.clone()
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = sanitize(text);
        let range = range
            .map(|range| {
                Self::byte_for_utf16(&self.text, range.start)
                    ..Self::byte_for_utf16(&self.text, range.end)
            })
            .or_else(|| {
                self.marked.as_ref().map(|marked| {
                    Self::byte_for_utf16(&self.text, marked.start)
                        ..Self::byte_for_utf16(&self.text, marked.end)
                })
            })
            .unwrap_or_else(|| self.selection());
        self.text.replace_range(range.clone(), &text);
        self.caret = range.start + text.len();
        self.anchor = self.caret;
        self.marked = None;
        cx.notify();
        cx.emit(TextInputEvent::Changed);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = sanitize(new_text);
        let range = range
            .map(|range| {
                Self::byte_for_utf16(&self.text, range.start)
                    ..Self::byte_for_utf16(&self.text, range.end)
            })
            .unwrap_or_else(|| self.selection());
        let marked_start = Self::utf16_for_byte(&self.text, range.start);
        self.text.replace_range(range.clone(), &text);
        let marked_len = Self::utf16_len(&text);
        self.marked = Some(marked_start..marked_start + marked_len);
        let (select_start, select_end) = new_selected_range
            .map(|range| (range.start, range.end))
            .unwrap_or((marked_len, marked_len));
        self.anchor = range.start + Self::byte_for_utf16(&text, select_start);
        self.caret = range.start + Self::byte_for_utf16(&text, select_end);
        cx.notify();
        cx.emit(TextInputEvent::Changed);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let shaped = self.shape(window);
        let start = Self::byte_for_utf16(&self.text, range_utf16.start);
        let end = Self::byte_for_utf16(&self.text, range_utf16.end);
        let x0 = shaped.x_for_index(start);
        let x1 = shaped.x_for_index(end);
        let (x, width) = if x0 <= x1 {
            (x0, (x1 - x0).max(px(1.)))
        } else {
            (x1, (x0 - x1).max(px(1.)))
        };
        Some(Bounds {
            origin: point(element_bounds.origin.x + x, element_bounds.origin.y),
            size: size(width, px(INPUT_LINE_HEIGHT)),
        })
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.index_for_position(window, point.x))
    }
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let entity = cx.entity();
        let focus = self.focus.clone();
        let text = self.text.clone();
        let selection = self.selection();
        let caret = self.caret;
        let marked = self.marked.clone();
        let font = text_font(false, false);
        div()
            .id("text-input")
            .h(px(INPUT_LINE_HEIGHT))
            .w_full()
            .overflow_hidden()
            .cursor(CursorStyle::IBeam)
            .track_focus(&self.focus)
            .key_context(super::TEXT_INPUT_KEY_CONTEXT)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(Self::begin_pointer_selection),
            )
            .on_mouse_move(cx.listener(Self::extend_pointer_selection))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, window, cx| {
                        entity.update(cx, |input, _| input.bounds = Some(bounds));
                        let runs = [TextRun {
                            len: text.len(),
                            font: font.clone(),
                            color: rgb(0xffffff).into(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }];
                        let shaped = window.text_system().shape_line(
                            SharedString::from(text.clone()),
                            px(FONT_SIZE),
                            &runs,
                            None,
                        );
                        if selection.start != selection.end {
                            let origin = bounds.origin;
                            let x0 = shaped.x_for_index(selection.start);
                            let x1 = shaped.x_for_index(selection.end);
                            let left = x0.min(x1);
                            window.paint_quad(fill(
                                Bounds {
                                    origin: point(origin.x + left, origin.y),
                                    size: size((x0 - x1).abs(), px(INPUT_LINE_HEIGHT)),
                                },
                                hsla(0.6, 0.7, 0.55, 0.35),
                            ));
                        }
                        shaped
                            .paint(bounds.origin, px(INPUT_LINE_HEIGHT), window, cx)
                            .ok();
                        if let Some(marked) = marked {
                            let start = TextInput::byte_for_utf16(&text, marked.start);
                            let end = TextInput::byte_for_utf16(&text, marked.end);
                            let x0 = shaped.x_for_index(start);
                            let x1 = shaped.x_for_index(end);
                            let left = x0.min(x1);
                            window.paint_quad(fill(
                                Bounds {
                                    origin: point(
                                        bounds.origin.x + left,
                                        bounds.origin.y + px(INPUT_LINE_HEIGHT) - px(1.5),
                                    ),
                                    size: size((x0 - x1).abs().max(px(2.)), px(1.5)),
                                },
                                rgb(0xb0b0b0),
                            ));
                        }
                        if focused && selection.start == selection.end {
                            let x = shaped.x_for_index(caret);
                            window.paint_quad(fill(
                                Bounds {
                                    origin: point(bounds.origin.x + x, bounds.origin.y),
                                    size: size(px(1.5), px(INPUT_LINE_HEIGHT)),
                                },
                                rgb(0xffffff),
                            ));
                        }
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );
                    },
                )
                .size_full(),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui::{EntityInputHandler, TestAppContext};

    use super::{TextInput, TextInputEvent};

    #[gpui::test]
    fn typing_inserts_unicode_and_reports_changes(cx: &mut TestAppContext) {
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new("", cx));
        let changes = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = changes.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&input, move |_, event, _| {
                if *event == TextInputEvent::Changed {
                    observed.set(observed.get() + 1);
                }
            })
        });
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_text_in_range(None, "héλ", window, cx);
            });
        });
        input.read_with(cx, |input, _| assert_eq!(input.text(), "héλ"));
        assert_eq!(changes.get(), 1);
    }

    #[gpui::test]
    fn deletes_selection_and_moves_by_grapheme(cx: &mut TestAppContext) {
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new("ab👩‍🚀", cx));
        input.update(cx, |input, cx| {
            input.select_all();
            input.delete_backward();
            assert_eq!(input.text(), "");
            input.set_text("a👩‍🚀b", cx);
            input.move_to_end(false);
            input.delete_backward();
            assert_eq!(input.text(), "a👩‍🚀");
            input.delete_backward();
            assert_eq!(input.text(), "a");
        });
    }

    #[gpui::test]
    fn shift_selection_and_clipboard_round_trip(cx: &mut TestAppContext) {
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new("hello", cx));
        input.update(cx, |input, cx| {
            input.select_all();
            input.copy(cx);
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some("hello".to_string())
            );
            input.set_caret(0, false);
            input.move_by(3, true);
            assert_eq!(input.selected_text(), "hel");
            input.cut(cx);
            assert_eq!(input.text(), "lo");
            input.paste(cx);
            assert_eq!(input.text(), "hello");
        });
    }

    #[gpui::test]
    fn newlines_are_dropped_and_ime_marking_is_tracked(cx: &mut TestAppContext) {
        let (input, cx) = cx.add_window_view(|_, cx| TextInput::new("", cx));
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_text_in_range(None, "a\nb\r", window, cx);
                assert_eq!(input.text(), "ab");
                input.replace_and_mark_text_in_range(Some(0..0), "に", Some(0..1), window, cx);
                assert_eq!(input.text(), "にab");
                assert_eq!(input.marked, Some(0..1));
                assert_eq!(input.selected_text(), "に");
            });
        });
    }
}
