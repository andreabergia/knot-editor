// Knot's gpui editor widget.
//
// A custom `Element` rendering fixture lines with multi-attribute styled
// text via `WindowTextSystem::shape_line` + `ShapedLine::paint`, clipped by
// a `with_content_mask` scroll viewport. Scroll-wheel mutates a pixel scroll
// offset on the view; the element recomputes the visible line range each
// paint from scroll + its allocated bounds.
//
// Cursor model: a single caret stored as `(line, byte_col)` into the owned
// `lines` buffer, plus a `preferred_col` used to keep a stable column when
// moving vertically across lines of differing widths. The caret is painted
// as a 2px bar at the shaped line's `x_for_index(col)` when the editor's
// focus handle is the window's focused handle AND no selection is active
// (macOS hides the caret while a selection is held). Keyboard movement
// clamps to utf-8 char boundaries and, after each move, re-scrolls so the
// caret stays inside the viewport.
//
// Selection model: an `anchor` (line, byte_col) plus the caret mark the two
// ends; `has_selection` is false when anchor == caret. Drag, shift-click,
// and shift-arrow all extend the selection by moving the caret while leaving
// the anchor fixed; a plain click or non-shift arrow collapses it.
//
// IME model: `EditorView` implements `EntityInputHandler`, the 8-method
// NSTextInputClient mapping. The element calls `Window::handle_input` during
// paint (self-gated on focus) to register an `ElementInputHandler` wrapper.
// `marked_range_utf16` tracks the active preedit span as flat UTF-16 offsets
// into `lines.join("\\n")`; the element paints a thin underline over it.
// Editing rebuilds `lines`+`segs` from the authoritative buffer; edited text
// uses default styling.

use gpui::{prelude::*, *};
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

use crate::core::anchored_range::AnchoredRangeId;
use crate::host::protocol::{ByteRange, DecorationToken, EditorContribution, GutterToken};

use super::{
    CommandAction, CommandSurfaceKind, DIAGNOSTIC_COMMAND, EDITOR_KEY_CONTEXT,
    model::{BufferModel, ContributionSource, ResolvedEditorContribution},
};

/// Owned, frame-stable copy of one styled segment of one line.
/// Mirrors `knot::view::fixture`'s borrowed `Segment`/`SegSpec` but holds
/// byte offsets into our owned `lines` so nothing crosses a frame boundary
/// with a lifetime.
#[derive(Clone, Copy)]
struct Seg {
    start: usize,
    end: usize,
    color: u32,
    bold: bool,
    italic: bool,
}

/// One diagnostic decoration to render as a wavy underline overlay. `start`
/// and `end` are byte columns within `line`; `color` is an RGB u32. This is
/// the minimal model needed by the current paint path. Provider metadata such
/// as severity, message, and source is not represented yet.
#[derive(Clone, Copy)]
struct RenderedDecoration {
    line: usize,
    start: usize,
    end: usize,
    color: u32,
}

#[derive(Clone, Copy)]
struct RenderedGutterMarker {
    line: usize,
    color: u32,
}

#[derive(Clone)]
struct RenderedContributionAction {
    line: usize,
    start: usize,
    end: usize,
    command: String,
    source: ContributionSource,
    range: ByteRange,
    gutter: bool,
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct EditorContributionAction {
    pub command: String,
    pub source: ContributionSource,
    pub range: ByteRange,
    pub model: Entity<BufferModel>,
    pub window: AnyWindowHandle,
    pub focus: WeakFocusHandle,
}

#[derive(Clone, Copy)]
pub(crate) struct EditorRenderingOptions {
    pub show_gutter_markers: bool,
}

impl Default for EditorRenderingOptions {
    fn default() -> Self {
        Self {
            show_gutter_markers: true,
        }
    }
}

const DEFAULT_COLOR: u32 = 0xC0C0C0;
const ERROR_COLOR: u32 = 0xF48771;
const WARNING_COLOR: u32 = 0xE2C08D;
const INFO_COLOR: u32 = 0x6CB6FF;

pub struct EditorView {
    element_id: usize,
    rendering: EditorRenderingOptions,
    /// Authoritative document state. Everything below is presentation state
    /// or a derived rendering projection.
    model: Entity<BufferModel>,
    lines: Vec<String>,
    segs: Vec<Vec<Seg>>,
    /// Renderer-local projection of shared, anchored buffer contributions.
    decorations: Vec<RenderedDecoration>,
    gutter_markers: Vec<RenderedGutterMarker>,
    contribution_actions: Vec<RenderedContributionAction>,
    /// Vertical scroll offset in pixels (0 = top of buffer).
    scroll: f32,
    /// Caret position: (line index, byte offset within that line).
    cursor_line: usize,
    cursor_col: usize,
    /// `preferred_col` tracks the byte column the user "aimed" at vertically;
    /// up/down resolve the actual col against the target line's shape but
    /// remember this so a subsequent down restores the same aim.
    preferred_col: usize,
    /// Last measured editor pane height in pixels; written by the element
    /// during paint and read by the key/scroll handlers so
    /// `ensure_cursor_visible` and `clamp_scroll` can clamp against the real
    /// viewport, not a guess from line count.
    viewport_h: f32,
    /// Last measured editor pane bounds; written by the element during paint
    /// and read by the mouse-down handler so it can map a click position to a
    /// (line, byte_col) caret.
    bounds: Bounds<Pixels>,
    /// Selection anchor (the "other" end of the selection, opposite the
    /// caret). Only meaningful while `has_selection` is true. Set when a
    /// drag/shift-extend begins from the caret's pre-existing position; the
    /// caret then tracks the moving end. Both anchor and caret use the same
    /// (line, byte_col) coordinate space as the cursor fields above.
    anchor_line: usize,
    anchor_col: usize,
    has_selection: bool,
    /// An action-region press owns the gesture through mouse-up; pointer
    /// jitter must not turn it into a text drag from the old caret.
    suppress_drag_selection: bool,
    position_range: Option<AnchoredRangeId>,
    selection_reversed: bool,
    /// IME preedit (marked) range as flat UTF-16 offsets into the
    /// `lines.join("\\n")` document. `None` = no active composition. Set by
    /// `replace_and_mark_text_in_range`, cleared by `replace_text_in_range` /
    /// `unmark_text`. The element paints an underline over this span.
    marked_range_utf16: Option<Range<usize>>,
    focus: FocusHandle,
    #[cfg(test)]
    paint_count: u64,
    _model_subscription: Subscription,
    _release_subscription: Subscription,
}

impl EditorView {
    pub(crate) fn model(&self) -> &Entity<BufferModel> {
        &self.model
    }

    #[cfg(test)]
    pub(crate) fn responsiveness_state(&self) -> (usize, f32, u64) {
        (self.cursor_line, self.scroll, self.paint_count)
    }

    #[cfg(test)]
    pub(crate) fn interaction_bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    #[cfg(test)]
    pub(crate) fn selected_byte_range(&self) -> Option<Range<usize>> {
        self.selection_range().map(|(start, end)| {
            self.to_flat_byte(start.0, start.1)..self.to_flat_byte(end.0, end.1)
        })
    }

    /// Select a source byte range, reveal its caret, and transfer focus here.
    pub(crate) fn select_reveal_and_focus(
        &mut self,
        range: ByteRange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let anchor = self.from_flat_byte(range.start_byte_offset);
        let caret = self.from_flat_byte(range.end_byte_offset);
        self.anchor_line = anchor.0;
        self.anchor_col = anchor.1;
        self.cursor_line = caret.0;
        self.cursor_col = caret.1;
        self.preferred_col = self.cursor_col;
        self.has_selection = range.start_byte_offset != range.end_byte_offset;
        self.focus.focus(window);
        self.finish_position_change(cx);
    }

    /// Build an editor using the default projection for arbitrary buffer text.
    pub fn new(model: Entity<BufferModel>, cx: &mut Context<Self>) -> Self {
        let (lines, segs) = default_projection(&model.read(cx).text());
        Self::from_projection(model, lines, segs, 0, EditorRenderingOptions::default(), cx)
    }

    pub(crate) fn new_with_options(
        model: Entity<BufferModel>,
        element_id: usize,
        rendering: EditorRenderingOptions,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut editor = Self::new(model, cx);
        editor.element_id = element_id;
        editor.rendering = rendering;
        editor
    }

    /// Build an editor preloaded with a styled fixture.
    pub fn from_fixture(
        fixture: &crate::view::fixture::Fixture,
        model: Entity<BufferModel>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::from_fixture_with_options(fixture, model, 0, EditorRenderingOptions::default(), cx)
    }

    pub(crate) fn from_fixture_with_options(
        fixture: &crate::view::fixture::Fixture,
        model: Entity<BufferModel>,
        element_id: usize,
        rendering: EditorRenderingOptions,
        cx: &mut Context<Self>,
    ) -> Self {
        let n = fixture.line_count();
        let mut lines = Vec::with_capacity(n);
        let mut segs = Vec::with_capacity(n);
        for i in 0..n {
            let line = fixture.lines.get(i).cloned().unwrap_or_default();
            lines.push(line);
            let specs = fixture.segments_of(i);
            let mut row = Vec::with_capacity(specs.len() + 1);
            // The public `Segment` only exposes the styled text slice, not
            // byte offsets, so we locate each seg's text in the real line
            // and fill any whitespace gaps with default-color segs. This
            // guarantees the run partition covers the full line byte range
            // [0, len) contiguously, so `shape_line`'s byte layout matches
            // the actual text and `x_for_index`/`closest_index_for_x` land
            // on the right glyph. Segments are non-overlapping and ordered,
            // so the first match at/after the running cursor is the seg's
            // true position.
            let full = lines[i].as_str();
            let mut cursor = 0usize;
            for seg in specs {
                let seg_text = seg.text;
                if seg_text.is_empty() {
                    continue;
                }
                let start = match full[cursor..].find(seg_text) {
                    Some(r) => cursor + r,
                    None => cursor,
                };
                if start > cursor {
                    row.push(Seg {
                        start: cursor,
                        end: start,
                        color: DEFAULT_COLOR,
                        bold: false,
                        italic: false,
                    });
                }
                let end = start + seg_text.len();
                row.push(Seg {
                    start,
                    end,
                    color: seg.color,
                    bold: seg.bold,
                    italic: seg.italic,
                });
                cursor = end;
            }
            if cursor < full.len() {
                row.push(Seg {
                    start: cursor,
                    end: full.len(),
                    color: DEFAULT_COLOR,
                    bold: false,
                    italic: false,
                });
            }
            if row.is_empty() {
                row.push(Seg {
                    start: 0,
                    end: full.len(),
                    color: DEFAULT_COLOR,
                    bold: false,
                    italic: false,
                });
            }
            segs.push(row);
        }

        Self::from_projection(model, lines, segs, element_id, rendering, cx)
    }

    fn from_projection(
        model: Entity<BufferModel>,
        lines: Vec<String>,
        segs: Vec<Vec<Seg>>,
        element_id: usize,
        rendering: EditorRenderingOptions,
        cx: &mut Context<Self>,
    ) -> Self {
        let (decorations, gutter_markers, contribution_actions) =
            project_contributions(&lines, model.read(cx).resolved_contributions());

        let position_range = model.update(cx, |model, _| model.add_view_position(0..0));
        let model_subscription = cx.observe(&model, |this, model, cx| {
            let model = model.read(cx);
            this.rebuild_projection(
                &model.text(),
                model.resolved_contributions(),
                model.resolve_view_position(this.position_range),
            );
            this.sync_view_position(cx);
            cx.notify();
        });
        let release_subscription = cx.on_release(|this, cx| {
            this.model.update(cx, |model, _| {
                model.remove_view_position(this.position_range);
            });
        });

        Self {
            element_id,
            rendering,
            model,
            lines,
            segs,
            decorations,
            gutter_markers,
            contribution_actions,
            scroll: 0.,
            cursor_line: 0,
            cursor_col: 0,
            preferred_col: 0,
            viewport_h: 0.,
            bounds: Bounds::default(),
            anchor_line: 0,
            anchor_col: 0,
            has_selection: false,
            suppress_drag_selection: false,
            position_range,
            selection_reversed: false,
            marked_range_utf16: None,
            focus: cx.focus_handle(),
            #[cfg(test)]
            paint_count: 0,
            _model_subscription: model_subscription,
            _release_subscription: release_subscription,
        }
    }

    fn rebuild_projection(
        &mut self,
        text: &str,
        contributions: Vec<ResolvedEditorContribution>,
        position: Option<Range<usize>>,
    ) {
        (self.lines, self.segs) = default_projection(text);
        (
            self.decorations,
            self.gutter_markers,
            self.contribution_actions,
        ) = project_contributions(&self.lines, contributions);
        if let Some(position) = position {
            self.restore_view_position(position);
        }
    }

    fn to_flat_byte(&self, line: usize, byte_col: usize) -> usize {
        self.lines
            .iter()
            .take(line)
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + byte_col
    }

    fn from_flat_byte(&self, mut offset: usize) -> (usize, usize) {
        for (line, text) in self.lines.iter().enumerate() {
            if offset <= text.len() {
                return (line, offset);
            }
            offset = offset.saturating_sub(text.len() + 1);
        }
        let line = self.lines.len().saturating_sub(1);
        (line, self.line_end(line))
    }

    fn sync_view_position(&mut self, cx: &mut Context<Self>) {
        let caret = self.to_flat_byte(self.cursor_line, self.cursor_col);
        let anchor = if self.has_selection {
            self.to_flat_byte(self.anchor_line, self.anchor_col)
        } else {
            caret
        };
        self.selection_reversed = self.has_selection && caret < anchor;
        let range = anchor.min(caret)..anchor.max(caret);
        self.position_range = self.model.update(cx, |model, _| {
            model.replace_view_position(self.position_range, range)
        });
    }

    fn finish_position_change(&mut self, cx: &mut Context<Self>) {
        self.ensure_cursor_visible(self.viewport_h);
        self.clamp_scroll();
        self.sync_view_position(cx);
        cx.notify();
    }

    fn restore_view_position(&mut self, range: Range<usize>) {
        let start = self.from_flat_byte(range.start);
        let end = self.from_flat_byte(range.end);
        if self.has_selection && !range.is_empty() {
            if self.selection_reversed {
                (self.cursor_line, self.cursor_col) = start;
                (self.anchor_line, self.anchor_col) = end;
            } else {
                (self.anchor_line, self.anchor_col) = start;
                (self.cursor_line, self.cursor_col) = end;
            }
        } else {
            (self.cursor_line, self.cursor_col) = end;
            self.has_selection = false;
        }
        self.preferred_col = self.cursor_col;
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        if self.scroll < 0. {
            self.scroll = 0.;
        }
        let max = ((self.lines.len() as f32) * LINE_HEIGHT - self.viewport_h).max(0.);
        if self.scroll > max {
            self.scroll = max;
        }
    }

    /// Last valid byte column on `line` (== `lines[line].len()`), with the
    /// empty-line case folded to 0.
    fn line_end(&self, line: usize) -> usize {
        self.lines.get(line).map(|s| s.len()).unwrap_or(0)
    }

    /// Previous grapheme-cluster boundary before `col` on `line`; 0 if
    /// already at the start. This keeps the caret out of emoji ZWJ and
    /// modifier sequences, which must be edited as a single user-perceived
    /// character.
    fn prev_boundary(&self, line: usize, col: usize) -> usize {
        let s = match self.lines.get(line) {
            Some(s) => s,
            None => return 0,
        };
        if col == 0 {
            return 0;
        }
        s.grapheme_indices(true)
            .take_while(|(start, _)| *start < col)
            .last()
            .map_or(0, |(start, _)| start)
    }

    /// Next grapheme-cluster boundary after `col` on `line`; line end if at
    /// the end.
    fn next_boundary(&self, line: usize, col: usize) -> usize {
        let s = match self.lines.get(line) {
            Some(s) => s,
            None => return 0,
        };
        if col >= s.len() {
            return s.len();
        }
        s.grapheme_indices(true)
            .map(|(start, _)| start)
            .find(|start| *start > col)
            .unwrap_or(s.len())
    }

    /// After any cursor move, scroll just enough to keep the caret inside
    /// the viewport. `viewport_h` is the editor pane height in px. We tweak
    /// `self.scroll` and rely on the caller's later `clamp_scroll`.
    fn ensure_cursor_visible(&mut self, viewport_h: f32) {
        if viewport_h <= 0. {
            return;
        }
        let line_top = self.cursor_line as f32 * LINE_HEIGHT;
        let line_bottom = line_top + LINE_HEIGHT;
        if line_top < self.scroll {
            self.scroll = line_top;
        } else if line_bottom > self.scroll + viewport_h {
            self.scroll = line_bottom - viewport_h;
        }
    }

    /// Key-down handler. Keystroke identities come lowercased in
    /// `keystroke.key` (e.g. "left", "right", "up", "down", "home", "end",
    /// "pageup", "pagedown"). cmd-left/right are treated as home/end per
    // macOS convention. `viewport_h` is unknown here (depends on layout),
    // so we fold ensure_cursor_visible's clamp into the paint-time scroll
    // clamp (see below) and additionally nudge scroll coarsely by one
    // viewport for pageup/pagedown.
    ///
    /// Selection semantics:
    /// - With **shift** held, every motion extends the selection: the anchor
    ///   is seeded from the pre-motion caret (if no selection yet) and the
    ///   caret moves to the target. If the target lands back on the anchor
    ///   the selection collapses (`has_selection = false`).
    /// - Without shift, an existing selection **collapses** to the end the
    ///   motion points toward (forward motions → selection end, backward →
    ///   selection start) without moving past it; a second press moves
    ///   normally. With no selection the caret simply moves to the target.
    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.to_lowercase();
        let cmd = ev.keystroke.modifiers.platform;
        let shift = ev.keystroke.modifiers.shift;
        let last_line = self.lines.len().saturating_sub(1);

        // Text-editing keys (enter, backspace, forward-delete) are handled
        // before the motion match. The IME dispatch sends non-printing keys
        // (backspace) through `doCommandBySelector:` which re-dispatches to
        // this handler; if we don't handle them here they're lost. Enter
        // (`key_char = Some("\n")`) might go through `insertText:` or
        // `doCommandBySelector:` depending on the active keyboard layout,
        // so we handle it explicitly too. All three call
        // `replace_text_in_range` directly and stop propagation.
        if !cmd {
            match key.as_str() {
                "enter" => {
                    self.replace_text_in_range(None, "\n", window, cx);
                    cx.stop_propagation();
                    return;
                }
                "backspace" => {
                    let range = if self.has_selection {
                        let sel = self.selection_range().expect("has_selection");
                        let s = self.to_flat_utf16(sel.0.0, sel.0.1);
                        let e = self.to_flat_utf16(sel.1.0, sel.1.1);
                        Some(s..e)
                    } else {
                        let caret = self.to_flat_utf16(self.cursor_line, self.cursor_col);
                        if caret > 0 {
                            let doc = self.flat_doc();
                            let caret_byte = self.utf16_to_byte_col(&doc, caret);
                            doc.grapheme_indices(true)
                                .take_while(|(start, _)| *start < caret_byte)
                                .last()
                                .map(|(start, _)| self.byte_col_to_utf16(&doc, start)..caret)
                        } else {
                            None
                        }
                    };
                    if let Some(r) = range {
                        self.replace_text_in_range(Some(r), "", window, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                "delete" => {
                    // Forward delete (fn+delete on Mac keyboards).
                    let range = if self.has_selection {
                        let sel = self.selection_range().expect("has_selection");
                        let s = self.to_flat_utf16(sel.0.0, sel.0.1);
                        let e = self.to_flat_utf16(sel.1.0, sel.1.1);
                        Some(s..e)
                    } else {
                        let caret = self.to_flat_utf16(self.cursor_line, self.cursor_col);
                        let doc = self.flat_doc();
                        let caret_byte = self.utf16_to_byte_col(&doc, caret);
                        if caret_byte < doc.len() {
                            let end_byte = doc
                                .grapheme_indices(true)
                                .find(|(start, _)| *start > caret_byte)
                                .map_or(doc.len(), |(start, _)| start);
                            Some(caret..self.byte_col_to_utf16(&doc, end_byte))
                        } else {
                            None
                        }
                    };
                    if let Some(r) = range {
                        self.replace_text_in_range(Some(r), "", window, cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }

        // Compute the motion's target caret, its direction (forward = toward
        // end of buffer, used for collapse-tiebreak), whether it's a
        // horizontal motion (which resets `preferred_col` to the new col),
        // and a scroll delta (pageup/pagedown nudge the viewport too).
        let (target, forward, horizontal, scroll_delta): ((usize, usize), bool, bool, f32) =
            match (cmd, key.as_str()) {
                (true, "left") => ((self.cursor_line, 0), false, true, 0.),
                (true, "right") => (
                    (self.cursor_line, self.line_end(self.cursor_line)),
                    true,
                    true,
                    0.,
                ),
                (false, "left") => {
                    let (l, c) = if self.cursor_col > 0 {
                        (
                            self.cursor_line,
                            self.prev_boundary(self.cursor_line, self.cursor_col),
                        )
                    } else if self.cursor_line > 0 {
                        (self.cursor_line - 1, self.line_end(self.cursor_line - 1))
                    } else {
                        (self.cursor_line, 0)
                    };
                    ((l, c), false, true, 0.)
                }
                (false, "right") => {
                    let (l, c) = if self.cursor_col < self.line_end(self.cursor_line) {
                        (
                            self.cursor_line,
                            self.next_boundary(self.cursor_line, self.cursor_col),
                        )
                    } else if self.cursor_line < last_line {
                        (self.cursor_line + 1, 0)
                    } else {
                        (self.cursor_line, self.cursor_col)
                    };
                    ((l, c), true, true, 0.)
                }
                (false, "up") => {
                    let l = if self.cursor_line > 0 {
                        self.cursor_line - 1
                    } else {
                        self.cursor_line
                    };
                    let c = self.clamp_col_to_line(l, self.preferred_col);
                    ((l, c), false, false, 0.)
                }
                (false, "down") => {
                    let l = if self.cursor_line < last_line {
                        self.cursor_line + 1
                    } else {
                        self.cursor_line
                    };
                    let c = self.clamp_col_to_line(l, self.preferred_col);
                    ((l, c), true, false, 0.)
                }
                (false, "home") => ((self.cursor_line, 0), false, true, 0.),
                (false, "end") => (
                    (self.cursor_line, self.line_end(self.cursor_line)),
                    true,
                    true,
                    0.,
                ),
                (false, "pageup") => {
                    let rows = self.page_rows(window);
                    let l = self.cursor_line.saturating_sub(rows).min(last_line);
                    let c = self.clamp_col_to_line(l, self.preferred_col);
                    ((l, c), false, false, -(rows as f32 * LINE_HEIGHT))
                }
                (false, "pagedown") => {
                    let rows = self.page_rows(window);
                    let l = (self.cursor_line + rows).min(last_line);
                    let c = self.clamp_col_to_line(l, self.preferred_col);
                    ((l, c), true, false, rows as f32 * LINE_HEIGHT)
                }
                _ => return,
            };

        let did_page;
        if shift {
            if !self.has_selection {
                self.anchor_line = self.cursor_line;
                self.anchor_col = self.cursor_col;
            }
            self.cursor_line = target.0;
            self.cursor_col = target.1;
            self.has_selection =
                self.cursor_line != self.anchor_line || self.cursor_col != self.anchor_col;
            did_page = scroll_delta != 0.;
        } else if self.has_selection {
            // Collapse to the end the motion points toward; no further move.
            let (start, end) = self.selection_range().expect("has_selection was true");
            let end = if forward { end } else { start };
            self.cursor_line = end.0;
            self.cursor_col = end.1;
            self.has_selection = false;
            did_page = false;
        } else {
            self.cursor_line = target.0;
            self.cursor_col = target.1;
            did_page = scroll_delta != 0.;
        }

        if horizontal {
            self.preferred_col = self.cursor_col;
        }
        if did_page {
            self.scroll = (self.scroll + scroll_delta).max(0.);
        }

        cx.stop_propagation();
        self.finish_position_change(cx);
    }

    /// Convert `preferred_col` to a valid byte column on `line`, never
    /// exceeding the line's byte length. No shaping here — vertical movement
    /// keeps the byte column stable; the next paint places the caret. (For
    /// proportional fonts this is approximate; correct per-char x would
    /// require shaping the target line, deferred to selection.)
    fn clamp_col_to_line(&self, line: usize, col: usize) -> usize {
        let end = self.line_end(line);
        let col = col.min(end);
        // Snap to the nearest preceding UTF-8 char boundary. `col` often
        // comes from `preferred_col` which was a valid boundary on a
        // DIFFERENT line — e.g. ASCII col 6 is mid-character on a CJK line
        // where byte 6 is inside a 3-byte char. Without this snap, the
        // caret lands on a non-boundary and later `line_str[col..]` panics.
        self.prev_boundary(line, col)
    }

    fn page_rows(&self, window: &Window) -> usize {
        let h = f32::from(window.viewport_size().height);
        (h / LINE_HEIGHT).max(1.) as usize
    }

    /// Mouse-down hit-tests the click onto a (line, byte_col) caret and moves
    /// it there. Uses `closest_index_for_x` on the shaped line — the public
    /// `LineLayout` API — so the caret lands on the nearest glyph boundary
    /// rather than a guessed byte offset. With shift held, the existing caret
    /// becomes the selection anchor and the click position becomes the new
    /// caret (extending the selection); without shift, any selection is
    /// cleared and the caret jumps to the click.
    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (line, col) = self.hit_test(ev.position, window);
        let gutter = ev.position.x <= self.bounds.origin.x + px(14.);
        if let Some(action) = self.contribution_actions.iter().rev().find(|action| {
            action.line == line
                && if gutter {
                    action.gutter
                } else {
                    (action.start == action.end && col == action.start)
                        || (col >= action.start && col < action.end)
                }
        }) {
            self.suppress_drag_selection = true;
            cx.emit(EditorContributionAction {
                command: action.command.clone(),
                source: action.source,
                range: action.range,
                model: self.model.clone(),
                window: window.window_handle(),
                focus: self.focus.downgrade(),
            });
            cx.stop_propagation();
            return;
        }
        self.suppress_drag_selection = false;
        if ev.modifiers.shift {
            if !self.has_selection {
                self.anchor_line = self.cursor_line;
                self.anchor_col = self.cursor_col;
            }
            self.cursor_line = line;
            self.cursor_col = col;
            self.preferred_col = col;
            self.has_selection =
                self.cursor_line != self.anchor_line || self.cursor_col != self.anchor_col;
        } else {
            self.cursor_line = line;
            self.cursor_col = col;
            self.preferred_col = col;
            self.has_selection = false;
        }
        self.finish_position_change(cx);
    }

    /// Mouse-move while the left button is held extends the selection from
    /// the anchor (the position where the drag began) to the cursor's new
    /// position under the pointer. The first move event of a drag seeds the
    /// anchor from the pre-drag caret. `MouseMoveEvent::dragging()` is true
    /// when the platform reports the left button as currently pressed, so we
    /// don't need a separate mouse-up handler to know we're still dragging.
    /// (If the pointer leaves the pane we stop receiving moves — acceptable
    /// for the prototype; a drag-capture fix is deferred.)
    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !ev.dragging() {
            self.suppress_drag_selection = false;
            return;
        }
        if self.suppress_drag_selection {
            return;
        }
        let (line, col) = self.hit_test(ev.position, window);
        if !self.has_selection {
            self.anchor_line = self.cursor_line;
            self.anchor_col = self.cursor_col;
        }
        self.cursor_line = line;
        self.cursor_col = col;
        self.preferred_col = col;
        self.has_selection =
            self.cursor_line != self.anchor_line || self.cursor_col != self.anchor_col;
        self.finish_position_change(cx);
    }

    /// Per-line horizontal offset for RTL lines: right-align the line within
    /// the pane so a pure-Arabic line starts at the right edge instead of the
    /// left. Returns 0 for LTR lines, `(pane_w - shaped_w).max(0)` for RTL.
    /// The caret, selection, IME preedit, decoration, and hit-test all add
    /// this offset to their x computations so they stay consistent with the
    /// right-aligned text.
    fn line_x_offset(&self, line: usize, shaped_w: Pixels, pane_w: Pixels) -> Pixels {
        match self.lines.get(line) {
            Some(s) if is_rtl_line(s) => (pane_w - shaped_w).max(px(0.)),
            _ => px(0.),
        }
    }

    /// Map a window-space point to a (line, byte_col) caret position using the
    /// last paint's `bounds`. The candidate positions are grapheme boundaries,
    /// not glyph starts: a single emoji glyph may cover multiple UTF-8 code
    /// points, and its start alone is not enough to place a caret on either
    /// visible edge.
    fn hit_test(&self, p: Point<Pixels>, window: &Window) -> (usize, usize) {
        let origin = self.bounds.origin;
        let line = ((f32::from(p.y - origin.y) + self.scroll) / LINE_HEIGHT).floor() as usize;
        let line = line.min(self.lines.len().saturating_sub(1));
        let col = if self.lines.get(line).map(|s| s.is_empty()).unwrap_or(true) {
            0
        } else {
            let runs = runs_for(&self.lines[line], &self.segs[line]);
            let shaped = window.text_system().shape_line(
                SharedString::from(self.lines[line].clone()),
                px(FONT_SIZE),
                &runs,
                None,
            );
            let x_off = self.line_x_offset(line, shaped.width, self.bounds.size.width);
            let x = p.x - origin.x - x_off;
            let text = &self.lines[line];
            let mut boundaries: Vec<usize> = text
                .grapheme_indices(true)
                .map(|(start, _)| start)
                .collect();
            boundaries.push(text.len());
            boundaries
                .into_iter()
                .min_by(|a, b| {
                    let a_distance = f32::from((x_for_index_dir(&shaped, *a, text) - x).abs());
                    let b_distance = f32::from((x_for_index_dir(&shaped, *b, text) - x).abs());
                    a_distance.total_cmp(&b_distance)
                })
                .unwrap_or(0)
        };
        (line, col)
    }

    /// Ordered (start, end) of the active selection, where start <= end in
    /// (line, col) lexicographic order. Returns `None` when there is no
    /// selection (caret-only). The caller uses this to decide which lines get
    /// full-width highlight vs partial leading/trailing rects.
    fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        if !self.has_selection {
            return None;
        }
        let a = (self.anchor_line, self.anchor_col);
        let c = (self.cursor_line, self.cursor_col);
        if a <= c { Some((a, c)) } else { Some((c, a)) }
    }

    // ── Flat-UTF16 document model ───────────────────────────────────────
    //
    // The IME API speaks in flat UTF-16 offsets into the whole document
    // (lines joined by "\n"). These helpers convert between the (line,
    // byte_col) caret space and the flat UTF-16 offset space, and perform
    // text splices that rebuild the `lines`/`segs` storage. Editing resets
    // segments to a single default-color segment per line.

    /// Full document as a single String, lines joined by "\n".
    fn flat_doc(&self) -> String {
        self.lines.join("\n")
    }

    /// Convert a (line, byte_col) position to a flat UTF-16 offset.
    fn to_flat_utf16(&self, line: usize, byte_col: usize) -> usize {
        let mut off = 0usize;
        for (i, l) in self.lines.iter().enumerate() {
            if i == line {
                return off + self.byte_col_to_utf16(l, byte_col);
            }
            off += l.chars().map(char::len_utf16).sum::<usize>() + 1; // +1 for "\n"
        }
        off
    }

    /// Convert a flat UTF-16 offset to a (line, byte_col) position.
    fn from_flat_utf16(&self, mut off: usize) -> (usize, usize) {
        for (i, l) in self.lines.iter().enumerate() {
            let line_utf16_len = l.chars().map(char::len_utf16).sum::<usize>();
            if off <= line_utf16_len {
                return (i, self.utf16_to_byte_col(l, off));
            }
            off -= line_utf16_len + 1;
        }
        let last_line = self.lines.len().saturating_sub(1);
        (last_line, self.line_end(last_line))
    }

    /// Convert a UTF-16 offset within `s` to a UTF-8 byte offset.
    fn utf16_to_byte_col(&self, s: &str, utf16_off: usize) -> usize {
        let mut utf16_count = 0usize;
        for (byte_idx, ch) in s.char_indices() {
            if utf16_count >= utf16_off {
                return byte_idx;
            }
            utf16_count += ch.len_utf16();
        }
        s.len()
    }

    /// Convert a UTF-8 byte offset within `s` to a UTF-16 offset.
    fn byte_col_to_utf16(&self, s: &str, byte_off: usize) -> usize {
        let mut utf16_count = 0usize;
        for (byte_idx, ch) in s.char_indices() {
            if byte_idx >= byte_off {
                break;
            }
            utf16_count += ch.len_utf16();
        }
        utf16_count
    }

    /// Commit a replacement to the authoritative model. Its notification
    /// refreshes the derived line/segment projection.
    fn splice(&mut self, byte_start: usize, byte_end: usize, text: &str, cx: &mut Context<Self>) {
        let model = self.model.clone();
        model.update(cx, |model, cx| {
            if model.replace(byte_start..byte_end, text).unwrap_or(false) {
                cx.notify();
            }
        });
    }

    /// Determine the byte range to replace given an optional UTF-16 range.
    /// If `range` is `None`, replace the marked range if there is one,
    /// otherwise the current selection, otherwise the caret (zero-length).
    /// If `range` is `Some`, convert it from UTF-16 to byte offsets.
    fn resolve_replacement_range(&self, range: Option<Range<usize>>) -> (usize, usize) {
        let doc = self.flat_doc();
        match range {
            Some(r) => {
                let start = self.utf16_to_byte_col(&doc, r.start);
                let end = self.utf16_to_byte_col(&doc, r.end);
                (start, end)
            }
            None => {
                if let Some(mr) = &self.marked_range_utf16 {
                    let start = self.utf16_to_byte_col(&doc, mr.start);
                    let end = self.utf16_to_byte_col(&doc, mr.end);
                    (start, end)
                } else if self.has_selection {
                    let caret = self.to_flat_utf16(self.cursor_line, self.cursor_col);
                    let anchor = self.to_flat_utf16(self.anchor_line, self.anchor_col);
                    let (s, e) = if caret <= anchor {
                        (caret, anchor)
                    } else {
                        (anchor, caret)
                    };
                    let start = self.utf16_to_byte_col(&doc, s);
                    let end = self.utf16_to_byte_col(&doc, e);
                    (start, end)
                } else {
                    let caret = self.to_flat_utf16(self.cursor_line, self.cursor_col);
                    let byte = self.utf16_to_byte_col(&doc, caret);
                    (byte, byte)
                }
            }
        }
    }
}

pub(crate) fn seed_fixture_contributions(
    model: &mut BufferModel,
    overlap_source: ContributionSource,
) {
    let text = model.text();
    let mut primary = Vec::new();
    for (needle, decoration) in [
        ("LEAF_MAX", DecorationToken::Warning),
        ("INTERNAL_MIN", DecorationToken::Warning),
        ("unsafe", DecorationToken::Error),
    ] {
        primary.extend(
            text.match_indices(needle)
                .map(|(start, matched)| EditorContribution {
                    range: ByteRange {
                        start_byte_offset: start,
                        end_byte_offset: start + matched.len(),
                    },
                    decoration: Some(decoration),
                    gutter: None,
                    command: None,
                }),
        );
    }
    for (start, _) in text.match_indices("fn ") {
        let name_start = start + 3;
        let rest = &text[name_start..];
        let name_len = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        if name_len > 0 {
            primary.push(EditorContribution {
                range: ByteRange {
                    start_byte_offset: name_start,
                    end_byte_offset: name_start + name_len,
                },
                decoration: Some(DecorationToken::Info),
                gutter: None,
                command: None,
            });
        }
    }

    model
        .replace_contributions(ContributionSource::BuiltIn, &primary, model.revision())
        .expect("fixture contributions have valid ranges");

    // A second source deliberately overlaps LEAF_MAX. Info is painted before
    // warning, so the primary warning deterministically wins at the overlap.
    let overlap = text
        .match_indices("LEAF_MAX")
        .map(|(start, _)| {
            let line_start = text[..start].rfind('\n').map_or(0, |newline| newline + 1);
            let line_end = text[start..]
                .find('\n')
                .map_or(text.len(), |newline| start + newline);
            EditorContribution {
                range: ByteRange {
                    start_byte_offset: line_start,
                    end_byte_offset: line_end,
                },
                decoration: Some(DecorationToken::Info),
                gutter: None,
                command: None,
            }
        })
        .collect::<Vec<_>>();
    model
        .replace_contributions(overlap_source, &overlap, model.revision())
        .expect("fixture overlap contributions have valid ranges");
}

fn project_contributions(
    lines: &[String],
    contributions: Vec<ResolvedEditorContribution>,
) -> (
    Vec<RenderedDecoration>,
    Vec<RenderedGutterMarker>,
    Vec<RenderedContributionAction>,
) {
    let mut line_starts = Vec::with_capacity(lines.len());
    let mut next_start = 0;
    for line in lines {
        line_starts.push(next_start);
        next_start += line.len() + 1;
    }

    let line_at = |offset| {
        line_starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1)
            .min(lines.len().saturating_sub(1))
    };

    let mut decorations = Vec::new();
    let mut gutter_markers = Vec::new();
    let mut actions = Vec::new();
    for contribution in contributions {
        let start = contribution.range.start_byte_offset;
        let end = contribution.range.end_byte_offset;
        let first_line = line_at(start);
        let last_line = line_at(end);

        if let Some(token) = contribution.gutter {
            gutter_markers.push(RenderedGutterMarker {
                line: first_line,
                color: gutter_color(token),
            });
        }

        if contribution.decoration.is_none() && contribution.command.is_none() {
            continue;
        }

        for line in first_line..=last_line {
            let line_start = line_starts[line];
            let line_end = line_start + lines[line].len();
            let segment_start = start.max(line_start);
            let segment_end = end.min(line_end);

            if let Some(token) = contribution.decoration
                && segment_start < segment_end
            {
                decorations.push(RenderedDecoration {
                    line,
                    start: segment_start - line_start,
                    end: segment_end - line_start,
                    color: decoration_color(token),
                });
            }

            if let Some(command) = &contribution.command
                && segment_start <= segment_end
                && start <= line_end
                && end >= line_start
            {
                actions.push(RenderedContributionAction {
                    line,
                    start: segment_start - line_start,
                    end: segment_end - line_start,
                    command: command.clone(),
                    source: contribution.source,
                    range: contribution.range,
                    gutter: contribution.gutter.is_some() && line == first_line,
                });
            }
        }
    }
    (decorations, gutter_markers, actions)
}

fn decoration_color(token: DecorationToken) -> u32 {
    match token {
        DecorationToken::Info => INFO_COLOR,
        DecorationToken::Warning => WARNING_COLOR,
        DecorationToken::Error => ERROR_COLOR,
    }
}

fn gutter_color(token: GutterToken) -> u32 {
    match token {
        GutterToken::Info => INFO_COLOR,
        GutterToken::Warning => WARNING_COLOR,
        GutterToken::Error => ERROR_COLOR,
    }
}

fn default_projection(text: &str) -> (Vec<String>, Vec<Vec<Seg>>) {
    let lines: Vec<String> = text.split('\n').map(String::from).collect();
    let segs = lines
        .iter()
        .map(|line| {
            vec![Seg {
                start: 0,
                end: line.len(),
                color: DEFAULT_COLOR,
                bold: false,
                italic: false,
            }]
        })
        .collect();
    (lines, segs)
}

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<EditorContributionAction> for EditorView {}

impl EntityInputHandler for EditorView {
    /// Return the substring of the flat document at the given UTF-16 range.
    /// `adjusted_range` stays `None` — we don't need to adjust the range
    /// because our document is a plain string with no layout constraints.
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        _adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let doc = self.flat_doc();
        let start = self.utf16_to_byte_col(&doc, range.start);
        let end = self.utf16_to_byte_col(&doc, range.end);
        if start > doc.len() || end > doc.len() || start > end {
            return None;
        }
        Some(doc[start..end].to_string())
    }

    /// Return the current selection as a `UTF16Selection`. When there is no
    /// selection (caret only), return a zero-length range at the caret
    /// position. `reversed` is true when the caret (head) is before the
    /// anchor (tail) in document order.
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let caret = self.to_flat_utf16(self.cursor_line, self.cursor_col);
        if self.has_selection {
            let anchor = self.to_flat_utf16(self.anchor_line, self.anchor_col);
            let (start, end, reversed) = if anchor <= caret {
                (anchor, caret, false)
            } else {
                (caret, anchor, true)
            };
            Some(UTF16Selection {
                range: start..end,
                reversed,
            })
        } else {
            Some(UTF16Selection {
                range: caret..caret,
                reversed: false,
            })
        }
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range_utf16.clone()
    }

    /// Remove the IME composing state. The marked text stays in the document
    /// as regular text (per Apple's contract); we just clear the marking and
    /// move the caret to the end of the formerly marked span.
    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mr) = self.marked_range_utf16.take() {
            let (line, col) = self.from_flat_utf16(mr.end);
            self.cursor_line = line;
            self.cursor_col = col;
            self.preferred_col = col;
            self.has_selection = false;
            self.finish_position_change(cx);
        }
    }

    /// Replace text at the given UTF-16 range (or the current selection /
    /// marked range if `range` is `None`) with `text`. This is the
    /// `insertText:` callback — it commits text into the document. After
    /// replacement the marked range is cleared and the caret moves to the
    /// end of the inserted text.
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Normalize line endings: macOS may send \r for Enter in some
        // keyboard layouts; our line model uses \n.
        let text: String = text.replace("\r\n", "\n").replace('\r', "\n");
        let (byte_start, byte_end) = self.resolve_replacement_range(range);
        self.splice(byte_start, byte_end, &text, cx);
        self.marked_range_utf16 = None;

        // Caret → end of inserted text.
        let doc = self.flat_doc();
        let insert_end_byte = byte_start + text.len();
        let insert_end_utf16 = self.byte_col_to_utf16(&doc, insert_end_byte.min(doc.len()));
        let (line, col) = self.from_flat_utf16(insert_end_utf16);
        self.cursor_line = line;
        self.cursor_col = col;
        self.preferred_col = col;
        self.has_selection = false;
        self.finish_position_change(cx);
    }

    /// Replace text at the given range (or current selection / marked range
    /// if `None`) with `new_text`, and mark the result as IME composing text.
    /// `new_selected_range` is relative to the start of the marked text (per
    /// Apple's `setMarkedText:selectedRange:replacementRange:`). The caret
    /// moves to the end of `new_selected_range`, and the anchor to its start,
    /// giving a visible selection within the preedit string.
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (byte_start, byte_end) = self.resolve_replacement_range(range);

        // Compute the UTF-16 offset where the marked text will start.
        let doc = self.flat_doc();
        let marked_start_utf16 = self.byte_col_to_utf16(&doc, byte_start);

        self.splice(byte_start, byte_end, new_text, cx);

        // Marked range = [marked_start, marked_start + utf16_len(new_text)).
        let marked_utf16_len: usize = new_text.chars().map(|c| c.len_utf16()).sum();
        self.marked_range_utf16 = Some(marked_start_utf16..marked_start_utf16 + marked_utf16_len);

        // Caret + anchor from new_selected_range (relative to marked start).
        let (sel_start_rel, sel_end_rel) = match new_selected_range {
            Some(r) => (r.start, r.end),
            None => (marked_utf16_len, marked_utf16_len),
        };
        let anchor_utf16 = marked_start_utf16 + sel_start_rel.min(marked_utf16_len);
        let caret_utf16 = marked_start_utf16 + sel_end_rel.min(marked_utf16_len);
        let (al, ac) = self.from_flat_utf16(anchor_utf16);
        let (cl, cc) = self.from_flat_utf16(caret_utf16);
        self.anchor_line = al;
        self.anchor_col = ac;
        self.cursor_line = cl;
        self.cursor_col = cc;
        self.preferred_col = cc;
        self.has_selection = anchor_utf16 != caret_utf16;

        self.finish_position_change(cx);
    }

    /// Return the bounds (in window-local px) of the given UTF-16 range, used
    /// by macOS to position the IME candidate window. We shape the line
    /// containing the range start and return a rect spanning from the start
    /// column's x to the end column's x (or at least the caret x if they
    /// coincide), at the line's vertical position.
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (start_line, start_col) = self.from_flat_utf16(range_utf16.start);
        let (end_line, end_col) = self.from_flat_utf16(range_utf16.end);
        let line = start_line;
        let line_str = self.lines.get(line)?;
        let top = element_bounds.origin.y + px(line as f32 * LINE_HEIGHT) - px(self.scroll);

        if line_str.is_empty() {
            return Some(Bounds {
                origin: point(element_bounds.origin.x, top),
                size: size(px(1.), px(LINE_HEIGHT)),
            });
        }

        let runs = runs_for(line_str, &self.segs[line]);
        let shaped = window.text_system().shape_line(
            SharedString::from(line_str.clone()),
            px(FONT_SIZE),
            &runs,
            None,
        );
        let x0 = x_for_index_dir(&shaped, start_col, line_str);
        let x1 = if start_line == end_line {
            x_for_index_dir(&shaped, end_col, line_str)
        } else {
            element_bounds.size.width
        };
        let (origin_x, width) = if start_line == end_line && x0 > x1 {
            (x1, (x0 - x1).max(px(1.)))
        } else {
            (x0, (x1 - x0).max(px(1.)))
        };
        let x_off = self.line_x_offset(line, shaped.width, element_bounds.size.width);
        Some(Bounds {
            origin: point(element_bounds.origin.x + x_off + origin_x, top),
            size: size(width, px(LINE_HEIGHT)),
        })
    }

    /// Map a window-space point to a flat UTF-16 offset, for IME hit-testing.
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let (line, col) = self.hit_test(point, window);
        Some(self.to_flat_utf16(line, col))
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        div()
            .id(("editor", self.element_id))
            .size_full()
            .bg(rgb(0x1e1e1e))
            // Track focus so clicking the pane focuses this view (gpui
            // auto-focuses a tracked element on mouse-down) and so on_key_down
            // listeners below actually receive keystrokes.
            .track_focus(&self.focus)
            .key_context(EDITOR_KEY_CONTEXT)
            .on_action(cx.listener(Self::on_command_action))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            // Mouse-move extends the selection while the left button is held
            // (drag-select). `MouseMoveEvent::dragging()` is true when the
            // platform reports the left button as currently pressed, so the
            // handler self-gates and we don't need a separate mouse-up.
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            // Scroll wheel: accumulate pixel delta, clamp against the real
            // viewport height (written each paint by the element). macOS
            // "natural" scroll: trackpad two-finger gesture down moves the
            // content up, i.e. the viewport descends — scroll increases.
            // ScrollWheelEvent delta is positive for swipes upward (toward
            // content top), so we invert it here.
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                let dy = match ev.delta {
                    ScrollDelta::Pixels(p) => f32::from(p.y),
                    ScrollDelta::Lines(p) => p.y * LINE_HEIGHT,
                };
                this.scroll -= dy;
                this.clamp_scroll();
                cx.notify();
            }))
            .child(EditorElement { entity })
    }
}

impl EditorView {
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
        action.record_diagnostic(CommandSurfaceKind::Editor, cx);
        true
    }
}

/// The custom `Element`. Owns no state itself; reads everything from the
/// `EditorView` entity each layout/paint. Implements `Element` directly
/// (not `RenderOnce`) per the `Element` module doc's recommendation for
/// code editors.
struct EditorElement {
    entity: Entity<EditorView>,
}

impl IntoElement for EditorElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = Bounds<Pixels>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        let layout_id = window.request_layout(style, [], cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        bounds
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Read the view state, compute the visible range, then clone just
        // the visible lines + segs out of the borrow before painting:
        // `ShapedLine::paint` takes `cx: &mut App`, but `entity.read(cx)`
        // borrows `cx: &App` for as long as the borrow lives. Cloning only
        // the on-screen rows keeps the per-frame cost to ~viewport-height
        // small String copies. We also pull the caret + focus handle so we
        // can paint the caret after the lines without re-borrowing the view.
        let (
            scroll,
            scroll_max,
            vis,
            caret,
            selection,
            marked,
            decorations,
            gutter_markers,
            focus_handle,
            entity,
        ): (
            f32,
            f32,
            Vec<(usize, String, Vec<Seg>)>,
            Option<(usize, usize)>,
            Option<((usize, usize), (usize, usize))>,
            Option<((usize, usize), (usize, usize))>,
            Vec<RenderedDecoration>,
            Vec<RenderedGutterMarker>,
            FocusHandle,
            Entity<EditorView>,
        ) = {
            let view = self.entity.read(cx);
            let max = (view.lines.len() as f32) * LINE_HEIGHT;
            let first = (view.scroll / LINE_HEIGHT).floor() as usize;
            let visible_rows = (f32::from(bounds.size.height) / LINE_HEIGHT).ceil() as usize + 1;
            let last = (first + visible_rows).min(view.lines.len());
            let vis = (first..last)
                .map(|ix| (ix, view.lines[ix].clone(), view.segs[ix].clone()))
                .collect();
            // Only paint the caret when it's on a visible line; otherwise it
            // is clipped anyway, so skip the shaping cost.
            let caret = if view.cursor_line >= first && view.cursor_line < last {
                Some((view.cursor_line, view.cursor_col))
            } else {
                None
            };
            // Convert marked UTF-16 range to (line, byte_col) start/end for
            // underline painting. Done inside the borrow since from_flat_utf16
            // needs &self.
            let marked = view.marked_range_utf16.as_ref().map(|mr| {
                let s = view.from_flat_utf16(mr.start);
                let e = view.from_flat_utf16(mr.end);
                (s, e)
            });
            // Only copy decorations that fall on visible lines.
            let decorations = view
                .decorations
                .iter()
                .filter(|a| a.line >= first && a.line < last)
                .copied()
                .collect();
            let gutter_markers = if view.rendering.show_gutter_markers {
                view.gutter_markers
                    .iter()
                    .filter(|marker| marker.line >= first && marker.line < last)
                    .copied()
                    .collect()
            } else {
                Vec::new()
            };
            (
                view.scroll,
                max,
                vis,
                caret,
                view.selection_range(),
                marked,
                decorations,
                gutter_markers,
                view.focus.clone(),
                self.entity.clone(),
            )
        };

        // Write the measured viewport height back into the view so the key
        // and scroll handlers can clamp against the real viewport. No
        // notify: this must not trigger a re-render.
        let viewport_h = f32::from(bounds.size.height);
        self.entity.update(cx, |view, _cx| {
            view.viewport_h = viewport_h;
            view.bounds = bounds;
            #[cfg(test)]
            {
                view.paint_count += 1;
            }
        });

        // Register the IME input handler. `handle_input` self-gates on
        // focus — it only registers if `focus_handle.is_focused(window)`,
        // so calling it unconditionally is safe. Must be called during
        // paint (debug_assert_paint). `ElementInputHandler` wraps our
        // `EntityInputHandler` impl and forwards all calls through
        // `entity.update`.
        if focus_handle.is_focused(window) {
            window.handle_input(&focus_handle, ElementInputHandler::new(bounds, entity), cx);
        }

        let font_size = px(FONT_SIZE);
        let line_height = px(LINE_HEIGHT);
        let focused = focus_handle.is_focused(window);

        // Clip to viewport so paint below the last line / outside horizontal
        // extent doesn't bleed into neighbouring panes.
        let mask = Some(ContentMask { bounds });
        let pane_w = bounds.size.width;
        window.with_content_mask(mask, |window| {
            // Shape each visible non-empty line once and reuse the
            // `ShapedLine` for both the selection highlight and the text
            // paint, so we don't pay for shaping twice per frame. The third
            // tuple element is the per-line x offset (0 for LTR, or
            // `pane_w - shaped_w` for RTL so they right-align); the fourth
            // is a `&str` reference to the line text, passed to
            // `x_for_index_dir` for per-character direction detection.
            let shaped: Vec<(usize, ShapedLine, Pixels, &str)> = vis
                .iter()
                .filter_map(|(ix, line, row)| {
                    if line.is_empty() {
                        return None;
                    }
                    let runs = runs_for(line, row);
                    let s = window.text_system().shape_line(
                        SharedString::from(line.clone()),
                        font_size,
                        &runs,
                        None,
                    );
                    let x_off = if is_rtl_line(line) {
                        (pane_w - s.width).max(px(0.))
                    } else {
                        px(0.)
                    };
                    Some((*ix, s, x_off, line.as_str()))
                })
                .collect();

            // Selection highlight, painted BEHIND the text. For each visible
            // line in the selection's line range we paint a rect covering the
            // selected byte span on that line: full pane width for interior
            // lines (including empty ones, which get a full-width bar so the
            // selection reads as contiguous), and partial spans for the start
            // and end lines. Empty boundary lines paint nothing (zero-width).
            if let Some((start, end)) = selection {
                let sel_color = hsla(0.6, 0.7, 0.55, 0.35);
                let pane_w = bounds.size.width;
                for (ix, s, x_off, line_str) in &shaped {
                    if *ix < start.0 || *ix > end.0 {
                        continue;
                    }
                    let rtl_base = is_rtl_line(line_str);
                    let (rx, rw) = if start.0 == end.0 {
                        let x0 = x_for_index_dir(s, start.1, line_str);
                        let x1 = x_for_index_dir(s, end.1, line_str);
                        let (lo, hi) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
                        (lo, hi - lo)
                    } else if *ix == start.0 {
                        let x0 = x_for_index_dir(s, start.1, line_str);
                        if rtl_base {
                            (px(0.), x0.max(px(0.)))
                        } else {
                            (x0, pane_w - x0)
                        }
                    } else if *ix == end.0 {
                        let x1 = x_for_index_dir(s, end.1, line_str);
                        if rtl_base {
                            (x1, (pane_w - x1).max(px(0.)))
                        } else {
                            (px(0.), x1)
                        }
                    } else {
                        (px(0.), pane_w)
                    };
                    let top = bounds.origin.y + px(*ix as f32 * LINE_HEIGHT) - px(scroll);
                    let rect = Bounds {
                        origin: point(bounds.origin.x + *x_off + rx, top),
                        size: size(rw, line_height),
                    };
                    let _ = window.paint_quad(fill(rect, sel_color));
                }
                // Empty interior lines: no shaped line above, so paint a
                // full-width highlight here so the selection looks unbroken
                // across blank lines.
                for (ix, line, _row) in &vis {
                    if line.is_empty() && *ix > start.0 && *ix < end.0 {
                        let top = bounds.origin.y + px(*ix as f32 * LINE_HEIGHT) - px(scroll);
                        let rect = Bounds {
                            origin: point(bounds.origin.x, top),
                            size: size(pane_w, line_height),
                        };
                        let _ = window.paint_quad(fill(rect, sel_color));
                    }
                }
            }

            // Text, reusing the shaped lines from above.
            for (ix, s, x_off, _line_str) in &shaped {
                let origin = point(
                    bounds.origin.x + *x_off,
                    bounds.origin.y + px(*ix as f32 * LINE_HEIGHT) - px(scroll),
                );
                let _ = s.paint(origin, line_height, window, cx);
            }

            // IME preedit (marked text) underline. Paint a 1px bar at the
            // bottom of each line's marked byte span. Single-line case:
            // x_for_index(start)..x_for_index(end). Multi-line: full width
            // for interior lines, partial for start/end (same partition as
            // selection but with underline styling instead of fill).
            if let Some((start, end)) = marked {
                let mark_color = hsla(0.0, 0.0, 0.7, 0.8);
                let underline_h = px(1.5);
                let pane_w = bounds.size.width;
                for (ix, s, x_off, line_str) in &shaped {
                    if *ix < start.0 || *ix > end.0 {
                        continue;
                    }
                    let rtl_base = is_rtl_line(line_str);
                    let (rx, rw) = if start.0 == end.0 {
                        let x0 = x_for_index_dir(s, start.1, line_str);
                        let x1 = x_for_index_dir(s, end.1, line_str);
                        let (lo, hi) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
                        (lo, (hi - lo).max(px(2.)))
                    } else if *ix == start.0 {
                        let x0 = x_for_index_dir(s, start.1, line_str);
                        if rtl_base {
                            (px(0.), x0.max(px(2.)))
                        } else {
                            (x0, pane_w - x0)
                        }
                    } else if *ix == end.0 {
                        let x1 = x_for_index_dir(s, end.1, line_str);
                        if rtl_base {
                            (x1, (pane_w - x1).max(px(2.)))
                        } else {
                            (px(0.), x1.max(px(2.)))
                        }
                    } else {
                        (px(0.), pane_w)
                    };
                    let top = bounds.origin.y + px(*ix as f32 * LINE_HEIGHT) - px(scroll)
                        + line_height
                        - underline_h;
                    let rect = Bounds {
                        origin: point(bounds.origin.x + *x_off + rx, top),
                        size: size(rw, underline_h),
                    };
                    let _ = window.paint_quad(fill(rect, mark_color));
                }
            }

            // Caret: a 2px vertical bar at the shaped line's
            // `x_for_index(cursor_col)`, painted only when the editor holds
            // window focus AND there is no active selection (macOS hides the
            // caret while a selection is drag-held). No blink yet —
            // IME/selection stages get a timer.
            if focused && selection.is_none() {
                if let Some((line_ix, col)) = caret {
                    let (caret_x, x_off) = shaped
                        .iter()
                        .find(|(ix, _, _, _)| *ix == line_ix)
                        .map(|(_, s, x_off, line_str)| (x_for_index_dir(s, col, line_str), *x_off))
                        .unwrap_or((px(0.), px(0.)));
                    let top = bounds.origin.y + px(line_ix as f32 * LINE_HEIGHT) - px(scroll);
                    let caret_bounds = Bounds {
                        origin: point(bounds.origin.x + x_off + caret_x, top),
                        size: size(px(2.), line_height),
                    };
                    let _ = window.paint_quad(fill(caret_bounds, hsla(0., 0., 0.9, 1.0)));
                }
            }

            // Decoration overlay: wavy underlines beneath contributed byte
            // spans, painted AFTER text + caret so they sit on top. Each
            // decoration is a (line, start_byte_col, end_byte_col, color)
            // tuple; we reuse the already-shaped lines to get the x
            // coordinates via `x_for_index`, then call `paint_underline`
            // with `wavy: true`. The underline y follows the same formula
            // the line painter uses internally:
            //   padding_top + ascent + descent * 0.618
            // below the line's top edge, so the squiggle sits just below the
            // text baseline. Decorations on empty lines (no shaped line) are
            // skipped — a zero-width wavy line would be invisible anyway.
            if !decorations.is_empty() {
                for ann in &decorations {
                    let (shaped_line, x_off, line_str) =
                        match shaped.iter().find(|(ix, _, _, _)| *ix == ann.line) {
                            Some((_, s, x_off, line_str)) => (s, *x_off, *line_str),
                            None => continue,
                        };
                    let x0 = x_for_index_dir(shaped_line, ann.start, line_str);
                    let x1 = x_for_index_dir(shaped_line, ann.end, line_str);
                    let (origin_x, width) = if x0 <= x1 {
                        (x0, (x1 - x0).max(px(0.)))
                    } else {
                        (x1, (x0 - x1).max(px(0.)))
                    };
                    if width <= px(0.) {
                        continue;
                    }
                    let ascent = shaped_line.ascent;
                    let descent = shaped_line.descent;
                    let padding_top = (line_height - ascent - descent) / 2.;
                    let underline_y = bounds.origin.y + px(ann.line as f32 * LINE_HEIGHT)
                        - px(scroll)
                        + padding_top
                        + ascent
                        + descent * 0.618;
                    window.paint_underline(
                        point(bounds.origin.x + x_off + origin_x, underline_y),
                        width,
                        &UnderlineStyle {
                            thickness: px(1.5),
                            color: Some(rgb(ann.color).into()),
                            wavy: true,
                        },
                    );
                }
            }

            for marker in &gutter_markers {
                let top = bounds.origin.y + px(marker.line as f32 * LINE_HEIGHT) - px(scroll);
                let marker_bounds = Bounds {
                    origin: point(bounds.origin.x + px(3.), top + px(6.)),
                    size: size(px(7.), px(7.)),
                };
                let _ = window.paint_quad(fill(marker_bounds, rgb(marker.color)));
            }
        });

        // Scrollbar overlay: paint a thin vertical thumb at the right edge
        // of the editor bounds indicating the current scroll position. Painted
        // OUTSIDE the content-mask so it always shows at the pane's right edge
        // regardless of how far the content has scrolled. Only drawn when the
        // content exceeds the viewport.
        let viewport_h = f32::from(bounds.size.height);
        let content_h = scroll_max;
        if content_h > viewport_h && viewport_h > 0. {
            let track = 6.0_f32;
            let thumb_h = (viewport_h * viewport_h / content_h).max(track);
            let origin_y = f32::from(bounds.origin.y);
            let thumb_y = origin_y + (scroll.max(0.) / content_h) * (viewport_h - thumb_h);
            let thumb_bounds = Bounds {
                origin: point(bounds.origin.x + bounds.size.width - px(track), px(thumb_y)),
                size: size(px(track), px(thumb_h)),
            };
            let _ = window.paint_quad(fill(thumb_bounds, hsla(0., 0., 0.6, 0.4)));
        }
    }
}

/// Map a line's owned `Seg` rows to gpui `TextRun`s covering the whole line.
fn runs_for(line: &str, segs: &[Seg]) -> Vec<TextRun> {
    let mut out = Vec::with_capacity(segs.len());
    let total = line.len();
    for s in segs {
        let len = s.end.min(total) - s.start.min(total);
        if len == 0 {
            continue;
        }
        out.push(TextRun {
            len,
            font: make_font(s.bold, s.italic),
            color: rgb(s.color).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    out
}

fn make_font(bold: bool, italic: bool) -> Font {
    let mut font = gpui::font("Menlo");
    if bold {
        font = font.bold();
    }
    if italic {
        font = font.italic();
    }
    // Menlo has no Arabic/CJK/emoji glyphs; add fallbacks so Core Text
    // substitutes the system fonts for those ranges. The cascade order
    // matches the fixture set: Geeza Pro (Arabic), PingFang SC (CJK),
    // Apple Color Emoji.
    font.fallbacks = Some(FontFallbacks::from_fonts(vec![
        "Geeza Pro".into(),
        "PingFang SC".into(),
        "Apple Color Emoji".into(),
    ]));
    font
}

/// Heuristic: is this line's base direction right-to-left? Scans for the
/// first strong directional character (a letter) and checks whether it
/// falls in an RTL Unicode block (Arabic or Hebrew). Lines starting with
/// LTR content (code, comments, markdown) return false even if they
/// contain embedded RTL runs — the base direction stays LTR.
fn is_rtl_line(s: &str) -> bool {
    for ch in s.chars() {
        if !ch.is_alphabetic() {
            continue;
        }
        return char_is_strong_rtl(ch);
    }
    false
}

/// Is this character a strong RTL character (Arabic/Hebrew block)?
fn char_is_strong_rtl(ch: char) -> bool {
    let c = ch as u32;
    (0x0590..=0x05FF).contains(&c)   // Hebrew
        || (0x0600..=0x06FF).contains(&c)      // Arabic
        || (0x0700..=0x074F).contains(&c)      // Syriac
        || (0x0750..=0x077F).contains(&c)      // Arabic Supplement
        || (0x08A0..=0x08FF).contains(&c)      // Arabic Extended-A
        || (0xFB1D..=0xFB4F).contains(&c)      // Hebrew presentation forms
        || (0xFB50..=0xFDFF).contains(&c)      // Arabic presentation forms-A
        || (0xFE70..=0xFEFF).contains(&c) // Arabic presentation forms-B
}

/// Direction-aware `x_for_index`. gpui's `ShapedLine::x_for_index` walks
/// glyphs in visual order assuming increasing logical indices — correct for
/// pure LTR, broken for RTL where Core Text reorders glyphs so visual order
/// has DECREASING logical indices.
///
/// This function detects the direction of the character AT `index` (the char
/// after the boundary) rather than relying on per-run heuristics, because
/// gpui merges CTRuns by font — a single `ShapedRun` can contain both RTL
/// and LTR glyphs (e.g. an Arabic line with embedded English, where the
/// surrounding spaces share the same font as the English text).
///
/// For an LTR character at `index`: the caret is at the LEFT edge of that
/// character's glyph (find the glyph with `index == target`).
/// For an RTL character at `index`: the caret is at the LEFT edge of the
/// glyph with the LARGEST index < `index` (the char before the boundary
/// in logical order, which sits to the RIGHT in visual order).
///
/// Special cases: `index == 0` returns `s.width` for RTL-base lines (right
/// edge) or `px(0.)` for LTR; `index >= s.len` is the mirror.
///
/// Uses only public fields: `LineLayout.runs`, `ShapedRun.glyphs`,
/// `ShapedGlyph.index`, `ShapedGlyph.position`, `LineLayout.width`,
/// `LineLayout.len`.
fn x_for_index_dir(s: &ShapedLine, index: usize, line_str: &str) -> Pixels {
    let rtl_base = is_rtl_line(line_str);
    if index == 0 {
        return if rtl_base { s.width } else { px(0.) };
    }
    if index >= s.len {
        return if rtl_base { px(0.) } else { s.width };
    }

    let rtl = line_str[index..]
        .chars()
        .next()
        .map(char_is_strong_rtl)
        .unwrap_or(rtl_base);

    let all_glyphs: Vec<&ShapedGlyph> = s.runs.iter().flat_map(|r| r.glyphs.iter()).collect();

    if !rtl {
        // A grapheme boundary can fall after a single glyph representing an
        // entire emoji/ZWJ cluster. In that case there is no glyph *at* the
        // boundary: use the next glyph's left edge, or the line's right edge
        // at end-of-line. Falling back to the preceding glyph's position
        // would paint the caret on the emoji's left edge.
        return all_glyphs
            .iter()
            .filter(|g| g.index >= index)
            .min_by_key(|g| g.index)
            .map_or(s.width, |g| g.position.x);
    }

    let mut best: Option<Pixels> = None;
    let mut best_idx: i64 = -1;
    for g in &all_glyphs {
        if (g.index as i64) < (index as i64) && (g.index as i64) > best_idx {
            best_idx = g.index as i64;
            best = Some(g.position.x);
        }
    }
    best.unwrap_or(if rtl_base { px(0.) } else { s.width })
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext, EntityInputHandler, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
        ScrollDelta, ScrollWheelEvent, TestAppContext, point, px,
    };

    use super::{
        BufferModel, ContributionSource, EditorRenderingOptions, EditorView,
        ResolvedEditorContribution, default_projection, project_contributions,
    };
    use crate::host::protocol::{ByteRange, DecorationToken, EditorContribution, GutterToken};
    use crate::view::fixture::Fixture;

    #[test]
    fn projection_refreshes_from_authoritative_buffer_text() {
        let mut model = BufferModel::from_text("one\ntwo");
        model.replace(0..3, "three").unwrap();
        let (lines, segs) = default_projection(&model.text());

        assert_eq!(lines, ["three", "two"]);
        assert_eq!(segs[0][0].end, "three".len());
        assert_eq!(segs[1][0].end, "two".len());
    }

    #[gpui::test]
    fn constructs_from_arbitrary_buffer_text(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("generated\ntext"));
        let editor = cx.new(|cx| EditorView::new(model.clone(), cx));

        cx.read(|cx| {
            let editor = editor.read(cx);
            assert_eq!(editor.model(), &model);
            assert_eq!(editor.lines, ["generated", "text"]);
            assert!(editor.rendering.show_gutter_markers);
        });
    }

    #[gpui::test]
    fn action_click_pointer_jitter_does_not_start_text_selection(cx: &mut TestAppContext) {
        let model = cx.new(|_| {
            let mut model = BufferModel::from_read_only_text("result");
            model
                .replace_contributions(
                    ContributionSource::BuiltIn,
                    &[EditorContribution {
                        range: ByteRange {
                            start_byte_offset: 0,
                            end_byte_offset: 6,
                        },
                        decoration: None,
                        gutter: None,
                        command: Some("fixture.action".into()),
                    }],
                    0,
                )
                .unwrap();
            model
        });
        let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model, cx));
        cx.refresh().unwrap();
        let click = cx.read(|cx| {
            let bounds = editor.read(cx).interaction_bounds();
            point(bounds.origin.x + px(20.), bounds.origin.y + px(10.))
        });

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.on_mouse_down(
                    &MouseDownEvent {
                        position: click,
                        modifiers: Modifiers::default(),
                        button: MouseButton::Left,
                        click_count: 1,
                        first_mouse: false,
                    },
                    window,
                    cx,
                );
                editor.on_mouse_move(
                    &MouseMoveEvent {
                        position: point(click.x + px(3.), click.y),
                        modifiers: Modifiers::default(),
                        pressed_button: Some(MouseButton::Left),
                    },
                    window,
                    cx,
                );
                assert_eq!(editor.selected_byte_range(), None);
            });
        });
    }

    #[gpui::test]
    fn read_only_text_keeps_normal_selection_copy_and_scroll_behavior(cx: &mut TestAppContext) {
        let text = (0..100)
            .map(|line| format!("result {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let model = cx.new(|_| BufferModel::from_read_only_text(text.clone()));
        let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
        cx.refresh().unwrap();

        cx.update(|window, cx| {
            editor.update(cx, |editor, cx| {
                editor.select_reveal_and_focus(
                    ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 8,
                    },
                    window,
                    cx,
                );
                let selection = editor.selected_text_range(false, window, cx).unwrap();
                assert_eq!(selection.range, 0..8);
                assert_eq!(
                    editor.text_for_range(selection.range, &mut None, window, cx),
                    Some("result 0".into())
                );

                editor.replace_text_in_range(None, "changed", window, cx);
            });
        });
        cx.simulate_event(ScrollWheelEvent {
            position: cx.read(|cx| editor.read(cx).interaction_bounds().center()),
            delta: ScrollDelta::Lines(point(0., -5.)),
            ..Default::default()
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let editor = editor.read(cx);
            assert_eq!(editor.model().read(cx).text(), text);
            assert!(editor.scroll > 0.);
        });
    }

    #[test]
    fn contribution_projection_indexes_multiline_ranges() {
        let lines = vec!["aa".into(), "bbbb".into(), "cc".into()];
        let (decorations, gutters, actions) = project_contributions(
            &lines,
            vec![ResolvedEditorContribution {
                range: ByteRange {
                    start_byte_offset: 1,
                    end_byte_offset: 9,
                },
                source: ContributionSource::BuiltIn,
                decoration: Some(DecorationToken::Warning),
                gutter: Some(GutterToken::Warning),
                command: Some("fixture.action".into()),
            }],
        );

        assert_eq!(
            decorations
                .iter()
                .map(|decoration| (decoration.line, decoration.start, decoration.end))
                .collect::<Vec<_>>(),
            [(0, 1, 2), (1, 0, 4), (2, 0, 1)]
        );
        assert_eq!(gutters.len(), 1);
        assert_eq!(gutters[0].line, 0);
        assert!(actions.iter().all(|action| {
            action.range
                == ByteRange {
                    start_byte_offset: 1,
                    end_byte_offset: 9,
                }
        }));
        assert_eq!(
            actions
                .iter()
                .map(|action| (action.line, action.start, action.end, action.gutter))
                .collect::<Vec<_>>(),
            [(0, 1, 2, true), (1, 0, 4, false), (2, 0, 1, false)]
        );
    }

    #[gpui::test]
    async fn flat_utf16_offsets_round_trip_across_emoji_lines(cx: &mut TestAppContext) {
        let family = "👨‍👩‍👧‍👦";
        let first_line = "a😀b";
        let second_line = format!("{family}z");
        let fixture = Fixture::from_lines(vec![first_line.into(), second_line.clone()]);
        let model = cx.new(|_| BufferModel::from_text(fixture.lines.join("\n")));
        let editor = cx.new(|cx| EditorView::from_fixture(&fixture, model, cx));

        cx.read(|cx| {
            let editor = editor.read(cx);
            let second_line_start = 5;
            let family_utf16_len = family.chars().map(char::len_utf16).sum::<usize>();

            assert_eq!(editor.from_flat_utf16(1), (0, 1));
            assert_eq!(editor.from_flat_utf16(3), (0, "a😀".len()));
            assert_eq!(editor.from_flat_utf16(second_line_start), (1, 0));
            assert_eq!(
                editor.from_flat_utf16(second_line_start + family_utf16_len),
                (1, family.len())
            );

            for (line, byte_col) in [
                (0, 0),
                (0, 1),
                (0, "a😀".len()),
                (0, first_line.len()),
                (1, 0),
                (1, family.len()),
                (1, second_line.len()),
            ] {
                let utf16 = editor.to_flat_utf16(line, byte_col);
                assert_eq!(editor.from_flat_utf16(utf16), (line, byte_col));
            }
        });
    }

    #[gpui::test]
    fn views_share_model_state_and_keep_presentation_state_independent(cx: &mut TestAppContext) {
        let fixture = Fixture::from_lines(vec!["one".into(), "two".into(), "three".into()]);
        let model = cx.new(|_| BufferModel::from_text(fixture.lines.join("\n")));
        let first = cx.new(|cx| EditorView::from_fixture(&fixture, model.clone(), cx));
        let second = cx.new(|cx| {
            EditorView::from_fixture_with_options(
                &fixture,
                model.clone(),
                1,
                EditorRenderingOptions {
                    show_gutter_markers: false,
                },
                cx,
            )
        });

        first.update(cx, |view, cx| {
            view.cursor_line = 1;
            view.cursor_col = 2;
            view.anchor_line = 0;
            view.anchor_col = 1;
            view.has_selection = true;
            view.scroll = 20.;
            view.sync_view_position(cx);
        });
        second.update(cx, |view, cx| {
            view.cursor_line = 2;
            view.cursor_col = 3;
            view.scroll = 40.;
            view.sync_view_position(cx);
        });

        model.update(cx, |model, cx| {
            assert!(model.replace(0..0, "shared\n").unwrap());
            model
                .replace_contributions(
                    ContributionSource::BuiltIn,
                    &[EditorContribution {
                        range: ByteRange {
                            start_byte_offset: 0,
                            end_byte_offset: 6,
                        },
                        decoration: Some(DecorationToken::Warning),
                        gutter: Some(GutterToken::Warning),
                        command: None,
                    }],
                    model.revision(),
                )
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let first = first.read(cx);
            let second = second.read(cx);

            assert_eq!(first.model, second.model);
            assert_eq!(first.lines, second.lines);
            assert_eq!(first.lines[0], "shared");
            assert_eq!(first.decorations.len(), 1);
            assert_eq!(first.decorations.len(), second.decorations.len());
            assert_eq!(first.gutter_markers.len(), second.gutter_markers.len());

            assert_eq!((first.cursor_line, first.cursor_col), (2, 2));
            assert_eq!((second.cursor_line, second.cursor_col), (3, 3));
            assert!(first.has_selection);
            assert!(!second.has_selection);
            assert_eq!(first.scroll, 20.);
            assert_eq!(second.scroll, 40.);
            assert!(first.rendering.show_gutter_markers);
            assert!(!second.rendering.show_gutter_markers);
            assert_ne!(first.focus, second.focus);
        });
    }

    #[gpui::test]
    fn selection_tracks_a_deletion_from_another_view(cx: &mut TestAppContext) {
        let fixture = Fixture::from_lines(vec!["0123456789".into()]);
        let model = cx.new(|_| BufferModel::from_text("0123456789"));
        let first = cx.new(|cx| EditorView::from_fixture(&fixture, model.clone(), cx));
        let second = cx.new(|cx| EditorView::from_fixture(&fixture, model.clone(), cx));

        second.update(cx, |view, cx| {
            view.anchor_col = 4;
            view.cursor_col = 7;
            view.has_selection = true;
            view.sync_view_position(cx);
        });
        model.update(cx, |model, cx| {
            assert!(model.replace(1..2, "").unwrap());
            cx.notify();
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let first = first.read(cx);
            let second = second.read(cx);
            assert_eq!(first.lines[0], "023456789");
            assert_eq!(second.lines, first.lines);
            assert_eq!((second.anchor_line, second.anchor_col), (0, 3));
            assert_eq!((second.cursor_line, second.cursor_col), (0, 6));
            assert!(second.has_selection);
        });
    }
}

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 20.0;
