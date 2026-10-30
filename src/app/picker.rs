//! The native picker shared by the command palette, native pickers, and
//! extension pickers.
//!
//! A picker owns its query field, fuzzy matching, highlighting, selection, and
//! rows. Items are matched with `nucleo-matcher` in tiers: label matches rank
//! above description matches, which rank above matches in an item's hidden
//! match text. Within a tier a higher score ranks first, and ties keep the
//! caller's order, as does an empty query. Events name items by their index in
//! the list the picker was created with. The picker reports the highlighted
//! item whenever it changes, so a consumer can preview it.

use std::ops::Range;

use gpui::{prelude::*, *};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    text_input::{TextInput, TextInputEvent},
    theme::{color, theme},
};

const PANEL_WIDTH: f32 = 560.;
const PANEL_MAX_HEIGHT: f32 = 420.;
const LINE_HEIGHT: f32 = 22.;
const ROW_PADDING: f32 = 6.;
const PAGE_ROWS: isize = 10;

/// One row offered to a picker.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PickerEntry {
    pub(crate) label: String,
    /// Muted text beside the label.
    pub(crate) description: Option<String>,
    /// A muted second line.
    pub(crate) detail: Option<String>,
    /// Right-aligned text, such as a keybinding.
    pub(crate) hint: Option<String>,
    /// Matched after the label and description but never displayed.
    pub(crate) match_text: Option<String>,
}

impl PickerEntry {
    pub(crate) fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PickerEvent {
    /// The highlighted entry changed; `None` when nothing matches.
    Highlighted(Option<usize>),
    Confirmed(usize),
    Dismissed,
}

/// One visible row: the entry index and its highlighted byte ranges.
#[derive(Clone, Debug, PartialEq)]
struct PickerMatch {
    entry: usize,
    label: Vec<Range<usize>>,
    description: Vec<Range<usize>>,
}

pub(crate) struct Picker {
    title: Option<SharedString>,
    entries: Vec<PickerEntry>,
    matches: Vec<PickerMatch>,
    selected: usize,
    /// The highlighted entry last reported.
    highlighted: Option<usize>,
    input: Entity<TextInput>,
    scroll: UniformListScrollHandle,
    matcher: Matcher,
    key_context: &'static str,
    _input_subscription: Subscription,
}

impl Picker {
    pub(crate) fn new(
        title: Option<String>,
        entries: Vec<PickerEntry>,
        key_context: &'static str,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| TextInput::new("", cx));
        let subscription = cx.subscribe(
            &input,
            |this: &mut Self, _input, _event: &TextInputEvent, cx| {
                let query = this.input.read(cx).text().to_string();
                this.refilter(&query);
                this.report_highlight(cx);
                cx.notify();
            },
        );
        let mut this = Self {
            title: title.map(Into::into),
            entries,
            matches: Vec::new(),
            selected: 0,
            highlighted: None,
            input,
            scroll: UniformListScrollHandle::new(),
            matcher: Matcher::new(Config::DEFAULT),
            key_context,
            _input_subscription: subscription,
        };
        this.refilter("");
        this.highlighted = this.selected_entry();
        this
    }

    pub(crate) fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle()
    }

    /// The entry index of the selected row.
    pub(crate) fn selected_entry(&self) -> Option<usize> {
        self.matches.get(self.selected).map(|found| found.entry)
    }

    /// Entry indices of the visible rows, best first.
    #[cfg(test)]
    pub(crate) fn visible_entries(&self) -> Vec<usize> {
        self.matches.iter().map(|found| found.entry).collect()
    }

    #[cfg(test)]
    pub(crate) fn entry(&self, index: usize) -> &PickerEntry {
        &self.entries[index]
    }

    #[cfg(test)]
    pub(crate) fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_text(query, cx));
        self.refilter(query);
        self.report_highlight(cx);
        cx.notify();
    }

    fn report_highlight(&mut self, cx: &mut Context<Self>) {
        let highlighted = self.selected_entry();
        if highlighted != self.highlighted {
            self.highlighted = highlighted;
            cx.emit(PickerEvent::Highlighted(highlighted));
        }
    }

    fn refilter(&mut self, query: &str) {
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        if query.trim().is_empty() {
            self.matches = (0..self.entries.len())
                .map(|entry| PickerMatch {
                    entry,
                    label: Vec::new(),
                    description: Vec::new(),
                })
                .collect();
            return;
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut scored = Vec::new();
        let mut buffer = Vec::new();
        let mut indices = Vec::new();
        for (entry, item) in self.entries.iter().enumerate() {
            let fields = [
                Some(item.label.as_str()),
                item.description.as_deref(),
                item.match_text.as_deref(),
            ];
            let found = fields.iter().enumerate().find_map(|(tier, text)| {
                let text = (*text)?;
                indices.clear();
                let score = pattern.indices(
                    Utf32Str::new(text, &mut buffer),
                    &mut self.matcher,
                    &mut indices,
                )?;
                Some((tier, score, grapheme_ranges(text, &mut indices)))
            });
            if let Some((tier, score, ranges)) = found {
                let mut found = PickerMatch {
                    entry,
                    label: Vec::new(),
                    description: Vec::new(),
                };
                match tier {
                    0 => found.label = ranges,
                    1 => found.description = ranges,
                    _ => {}
                }
                scored.push((tier, std::cmp::Reverse(score), found));
            }
        }
        // A stable sort keeps the caller's order among equal scores.
        scored.sort_by_key(|(tier, score, _)| (*tier, *score));
        self.matches = scored.into_iter().map(|(_, _, found)| found).collect();
    }

    pub(crate) fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.matches.len();
        self.selected = if count == 0 {
            0
        } else {
            self.selected
                .saturating_add_signed(delta)
                .min(count.saturating_sub(1))
        };
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Center);
        self.report_highlight(cx);
        cx.notify();
    }

    pub(crate) fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.selected_entry() {
            cx.emit(PickerEvent::Confirmed(entry));
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.to_lowercase().as_str() {
            "escape" => cx.emit(PickerEvent::Dismissed),
            "enter" => self.confirm(cx),
            "up" => self.move_selection(-1, cx),
            "down" => self.move_selection(1, cx),
            "pageup" => self.move_selection(-PAGE_ROWS, cx),
            "pagedown" => self.move_selection(PAGE_ROWS, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    #[cfg(test)]
    fn highlights(&self, entry: usize) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
        self.matches
            .iter()
            .find(|found| found.entry == entry)
            .map(|found| (found.label.clone(), found.description.clone()))
            .unwrap_or_default()
    }
}

/// Convert sorted grapheme indices from the matcher into merged byte ranges.
fn grapheme_ranges(text: &str, indices: &mut Vec<u32>) -> Vec<Range<usize>> {
    indices.sort_unstable();
    indices.dedup();
    let mut wanted = indices.iter().copied().peekable();
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (index, (start, grapheme)) in text.grapheme_indices(true).enumerate() {
        if wanted.peek() != Some(&(index as u32)) {
            continue;
        }
        wanted.next();
        let end = start + grapheme.len();
        match ranges.last_mut() {
            Some(last) if last.end == start => last.end = end,
            _ => ranges.push(start..end),
        }
    }
    ranges
}

impl EventEmitter<PickerEvent> for Picker {}

impl Focusable for Picker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle()
    }
}

fn highlighted(text: String, ranges: &[Range<usize>], color: Hsla) -> StyledText {
    let style = HighlightStyle {
        color: Some(color),
        font_weight: Some(FontWeight::BOLD),
        ..HighlightStyle::default()
    };
    StyledText::new(text).with_highlights(ranges.iter().map(|range| (range.clone(), style)))
}

impl Render for Picker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let has_detail = self.entries.iter().any(|entry| entry.detail.is_some());
        let row_height = LINE_HEIGHT * if has_detail { 2. } else { 1. } + ROW_PADDING;
        let rows = self.matches.len();
        let list_height = (rows as f32 * row_height).min(PANEL_MAX_HEIGHT);
        let list = uniform_list("picker-rows", rows, move |range, _, cx| {
            let picker = entity.read(cx);
            let theme = theme(cx);
            let muted = theme.color(color::TEXT_MUTED);
            let matched = theme.color(color::LIST_MATCH_FOREGROUND);
            range
                .map(|index| {
                    let found = &picker.matches[index];
                    let entry = &picker.entries[found.entry];
                    let is_selected = index == picker.selected;
                    let confirmed = found.entry;
                    let entity = entity.clone();
                    let line =
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .child(div().flex_shrink_0().child(highlighted(
                                entry.label.clone(),
                                &found.label,
                                matched,
                            )))
                            .when_some(entry.description.clone(), |line, description| {
                                line.child(
                                    div().flex_1().truncate().text_sm().text_color(muted).child(
                                        highlighted(description, &found.description, matched),
                                    ),
                                )
                            })
                            .when(entry.description.is_none(), |line| {
                                line.child(div().flex_1())
                            })
                            .when_some(entry.hint.clone(), |line, hint| {
                                line.child(
                                    div()
                                        .flex_shrink_0()
                                        .text_sm()
                                        .text_color(muted)
                                        .child(hint),
                                )
                            });
                    div()
                        .id(("picker-row", index))
                        .h(px(row_height))
                        .px_3()
                        .flex()
                        .flex_col()
                        .justify_center()
                        .overflow_hidden()
                        .when(is_selected, |row| {
                            row.bg(theme.color(color::LIST_SELECTED_BACKGROUND))
                                .text_color(theme.color(color::LIST_SELECTED_FOREGROUND))
                        })
                        .when(!is_selected, |row| {
                            row.hover(|style| style.bg(theme.color(color::LIST_HOVER_BACKGROUND)))
                        })
                        .child(line)
                        .when_some(entry.detail.clone(), |row, detail| {
                            row.child(div().truncate().text_sm().text_color(muted).child(detail))
                        })
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |_, cx| cx.emit(PickerEvent::Confirmed(confirmed)));
                        })
                })
                .collect()
        })
        .track_scroll(self.scroll.clone())
        .h(px(list_height));

        div()
            .id("picker")
            .w(px(PANEL_WIDTH))
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(theme(cx).color(color::POPOVER_BORDER))
            .bg(theme(cx).color(color::POPOVER_BACKGROUND))
            .text_color(theme(cx).color(color::TEXT))
            .shadow_lg()
            .key_context(self.key_context)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(Self::on_key_down))
            .when_some(self.title.clone(), |panel, title| {
                panel.child(div().px_3().pt_2().text_sm().child(title))
            })
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme(cx).color(color::BORDER))
                    .child(self.input.clone()),
            )
            .when(rows == 0, |panel| {
                panel.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_color(theme(cx).color(color::TEXT_MUTED))
                        .child("No matching items"),
                )
            })
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use gpui::{Entity, TestAppContext, VisualTestContext};

    use super::{Picker, PickerEntry, PickerEvent};

    fn entry(label: &str, description: Option<&str>) -> PickerEntry {
        PickerEntry {
            description: description.map(str::to_owned),
            ..PickerEntry::new(label)
        }
    }

    fn picker(
        entries: Vec<PickerEntry>,
        cx: &mut TestAppContext,
    ) -> (Entity<Picker>, &mut VisualTestContext) {
        cx.add_window_view(|_, cx| Picker::new(Some("Pick".into()), entries, "picker", cx))
    }

    fn labels(picker: &Picker) -> Vec<&str> {
        picker
            .visible_entries()
            .into_iter()
            .map(|entry| picker.entries[entry].label.as_str())
            .collect()
    }

    const COMMANDS: &[&str] = &[
        "Close Tab",
        "Close Window",
        "Copy",
        "Cut",
        "New Document",
        "New Terminal",
        "New Window",
        "Open…",
        "Paste",
        "Recover Unsaved Documents…",
        "Restart Extension…",
        "Save",
        "Save As…",
        "Set Language Mode…",
        "Show Command Palette",
        "Split Down",
        "Split Right",
        "Toggle Terminal Mouse Reporting",
    ];

    #[gpui::test]
    fn ranks_the_expected_command_first(cx: &mut TestAppContext) {
        let (picker, cx) = picker(
            COMMANDS.iter().map(|label| entry(label, None)).collect(),
            cx,
        );
        for (query, expected) in [
            ("save", "Save"),
            ("saveas", "Save As…"),
            ("sa a", "Save As…"),
            ("nt", "New Terminal"),
            ("new term", "New Terminal"),
            ("nw", "New Window"),
            ("copy", "Copy"),
            ("spr", "Split Right"),
            ("sd", "Split Down"),
            ("lang", "Set Language Mode…"),
            ("palette", "Show Command Palette"),
            ("rest", "Restart Extension…"),
            ("tmr", "Toggle Terminal Mouse Reporting"),
        ] {
            picker.update(cx, |picker, cx| {
                picker.set_query(query, cx);
                assert_eq!(labels(picker).first(), Some(&expected), "query {query:?}");
            });
        }
    }

    #[gpui::test]
    fn ranks_paths_by_their_file_name(cx: &mut TestAppContext) {
        let paths = [
            "src/app/product/commands.rs",
            "src/app/commands/catalog.rs",
            "src/app/command_palette.rs",
            "docs/architecture/commands.md",
        ];
        let (picker, cx) = picker(paths.iter().map(|path| entry(path, None)).collect(), cx);
        picker.update(cx, |picker, cx| {
            picker.set_query("catalog", cx);
            assert_eq!(labels(picker), ["src/app/commands/catalog.rs"]);
            picker.set_query("cmdpal", cx);
            assert_eq!(labels(picker).first(), Some(&"src/app/command_palette.rs"));
        });
    }

    #[gpui::test]
    fn empty_query_and_ties_keep_the_callers_order(cx: &mut TestAppContext) {
        let (picker, cx) = picker(
            vec![
                entry("beta", None),
                entry("alpha", None),
                entry("alpha", None),
            ],
            cx,
        );
        picker.update(cx, |picker, cx| {
            assert_eq!(picker.visible_entries(), [0, 1, 2]);
            picker.set_query("alpha", cx);
            assert_eq!(picker.visible_entries(), [1, 2]);
            picker.set_query("   ", cx);
            assert_eq!(picker.visible_entries(), [0, 1, 2]);
        });
    }

    #[gpui::test]
    fn label_matches_rank_above_description_and_hidden_text(cx: &mut TestAppContext) {
        let hidden = PickerEntry {
            match_text: Some("files.recent".into()),
            ..PickerEntry::new("Open Recent")
        };
        let (picker, cx) = picker(
            vec![
                hidden,
                entry("Projects", Some("recent folders")),
                entry("Recent Files", None),
            ],
            cx,
        );
        picker.update(cx, |picker, cx| {
            picker.set_query("recent", cx);
            // Equal label scores keep the caller's order, above the
            // description match.
            assert_eq!(labels(picker), ["Open Recent", "Recent Files", "Projects"]);
            picker.set_query("files.rec", cx);
            assert_eq!(labels(picker), ["Open Recent"]);
            assert_eq!(picker.highlights(0), (Vec::new(), Vec::new()));
        });
    }

    #[gpui::test]
    #[allow(clippy::single_range_in_vec_init)]
    fn highlights_cover_matched_bytes_in_multibyte_text(cx: &mut TestAppContext) {
        let (picker, cx) = picker(vec![entry("Café crème", Some("naïve 日本語"))], cx);
        picker.update(cx, |picker, cx| {
            picker.set_query("cafe", cx);
            assert_eq!(picker.highlights(0).0, [0..5]);
            picker.set_query("cre", cx);
            assert_eq!(picker.highlights(0).0, [6..10]);
            picker.set_query("日本", cx);
            let (label, description) = picker.highlights(0);
            assert!(label.is_empty());
            assert_eq!(description, [7..13]);
        });
    }

    #[gpui::test]
    fn confirms_the_selection_and_blocks_without_matches(cx: &mut TestAppContext) {
        let (picker, cx) = picker(
            vec![entry("Alpha", Some("first")), entry("Beta", Some("second"))],
            cx,
        );
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&picker, move |_, event, _| {
                observed.borrow_mut().push(event.clone())
            })
        });
        picker.update(cx, |picker, cx| {
            picker.move_selection(10, cx);
            assert_eq!(picker.selected_entry(), Some(1));
            picker.set_query("second", cx);
            assert_eq!(picker.selected_entry(), Some(1));
            picker.confirm(cx);
            picker.set_query("zzz", cx);
            assert_eq!(picker.selected_entry(), None);
            picker.confirm(cx);
            picker.move_selection(1, cx);
            assert_eq!(picker.selected, 0);
        });
        assert_eq!(
            *events.borrow(),
            [
                PickerEvent::Highlighted(Some(1)),
                PickerEvent::Confirmed(1),
                PickerEvent::Highlighted(None),
            ]
        );
    }
}
