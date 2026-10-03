//! Native editor presentation over an authoritative shared buffer model.
//!
//! Caret and selection use byte positions backed by model anchors. Movement and
//! pointer hit testing use grapheme boundaries and shaped visual coordinates.
//! Views independently own scrolling, selection, completion, and IME preedit.

use gpui::{prelude::*, *};
use std::{
    ops::Range,
    sync::atomic::{AtomicU64, Ordering},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::core::anchored_range::AnchoredRangeId;
use crate::host::protocol::{
    BufferHandle, ByteRange, CompletionProviderRegistrationId, CompletionRequest,
    CompletionResponse, DecorationToken, GutterToken, TextEdit,
};

use super::{
    CompletionAccept, CompletionDismiss, CompletionNext, CompletionPrevious, EDITOR_KEY_CONTEXT,
    completion::{
        CompactCompletionSurface, CompletionController, CompletionProviderRegistration,
        CompletionSurface, CompletionSurfaceKind, ListCompletionSurface,
    },
    history::{EditGroupKind, EditGrouping, HistoryViewChange, HistoryViewState},
    model::{BufferModel, ContributionSource, ResolvedEditorContribution},
};

static NEXT_HISTORY_CONTEXT: AtomicU64 = AtomicU64::new(1);

/// One styled segment of a projected line, with byte offsets into `lines`.
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
    /// Shaped x position retained across vertical movement, including short lines.
    preferred_x: Option<Pixels>,
    scroll_x: f32,
    drag_anchor: Option<(usize, usize)>,
    reveal_caret: bool,
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
    history_context: u64,
    history_was_focused: bool,
    last_history_restore_generation: u64,
    completion: Option<CompletionController>,
    completion_surface: Option<Box<dyn CompletionSurface>>,
    completion_tasks: Vec<(CompletionProviderRegistrationId, Task<()>)>,
    next_completion_generation: u64,
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
    pub(crate) fn completion_state(
        &self,
    ) -> Option<(usize, usize, usize, CompletionSurfaceKind, u64)> {
        let controller = self.completion.as_ref()?;
        let snapshot = controller.snapshot();
        Some((
            snapshot.items.len(),
            snapshot.pending_provider_count,
            snapshot.failures.len(),
            self.completion_surface.as_ref()?.kind(),
            controller.generation(),
        ))
    }

    #[cfg(test)]
    pub(crate) fn selected_byte_range(&self) -> Option<Range<usize>> {
        self.selection_range().map(|(start, end)| {
            self.to_flat_byte(start.0, start.1)..self.to_flat_byte(end.0, end.1)
        })
    }

    #[cfg(test)]
    pub(crate) fn set_test_presentation_state(
        &mut self,
        cursor_line: usize,
        cursor_col: usize,
        anchor: Option<(usize, usize)>,
        scroll: f32,
    ) {
        self.cursor_line = cursor_line;
        self.cursor_col = cursor_col;
        self.has_selection = anchor.is_some();
        if let Some((anchor_line, anchor_col)) = anchor {
            self.anchor_line = anchor_line;
            self.anchor_col = anchor_col;
        }
        self.scroll = scroll;
    }

    #[cfg(test)]
    pub(crate) fn test_presentation_state(&self) -> (usize, usize, Option<(usize, usize)>, f32) {
        (
            self.cursor_line,
            self.cursor_col,
            self.has_selection
                .then_some((self.anchor_line, self.anchor_col)),
            self.scroll,
        )
    }

    /// Select a source byte range, reveal its caret, and transfer focus here.
    pub(crate) fn select_reveal_and_focus(
        &mut self,
        range: ByteRange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.break_history_group(cx);
        self.marked_range_utf16 = None;
        self.preferred_x = None;
        let anchor = self.flat_byte_to_position(range.start_byte_offset);
        let caret = self.flat_byte_to_position(range.end_byte_offset);
        self.anchor_line = anchor.0;
        self.anchor_col = anchor.1;
        self.cursor_line = caret.0;
        self.cursor_col = caret.1;
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
            if this.completion.as_ref().is_some_and(|completion| {
                completion.revision() != model.revision() || !model.is_open()
            }) {
                this.completion = None;
                this.completion_surface = None;
                this.completion_tasks.clear();
            }
            if this.flat_doc() != model.text() {
                this.marked_range_utf16 = None;
                this.preferred_x = None;
            }
            this.rebuild_projection(
                &model.text(),
                model.resolved_contributions(),
                model.resolve_view_position(this.position_range),
            );
            if let Some(restore) = model.history_view_restore() {
                this.apply_history_view_restore(restore.generation, &restore.state);
            }
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
            preferred_x: None,
            scroll_x: 0.,
            drag_anchor: None,
            reveal_caret: true,
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
            history_context: NEXT_HISTORY_CONTEXT.fetch_add(1, Ordering::Relaxed),
            history_was_focused: false,
            last_history_restore_generation: 0,
            completion: None,
            completion_surface: None,
            completion_tasks: Vec::new(),
            next_completion_generation: 1,
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

    fn flat_byte_to_position(&self, mut offset: usize) -> (usize, usize) {
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
        self.dismiss_completion();
        self.reveal_caret = true;
        self.ensure_cursor_visible(self.viewport_h);
        self.clamp_scroll();
        self.sync_view_position(cx);
        cx.notify();
    }

    pub(crate) fn break_history_group(&self, cx: &mut Context<Self>) {
        self.model
            .update(cx, |model, _| model.break_history_group());
    }

    fn restore_view_position(&mut self, range: Range<usize>) {
        let start = self.flat_byte_to_position(range.start);
        let end = self.flat_byte_to_position(range.end);
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
        self.clamp_scroll();
    }

    fn history_view_state(&self) -> HistoryViewState {
        let caret = self.to_flat_byte(self.cursor_line, self.cursor_col);
        let anchor = if self.has_selection {
            self.to_flat_byte(self.anchor_line, self.anchor_col)
        } else {
            caret
        };
        HistoryViewState {
            context: self.history_context,
            anchor,
            caret,
        }
    }

    fn apply_history_view_restore(&mut self, generation: u64, state: &HistoryViewState) {
        if generation <= self.last_history_restore_generation {
            return;
        }
        self.last_history_restore_generation = generation;
        if state.context != self.history_context {
            return;
        }
        (self.anchor_line, self.anchor_col) = self.flat_byte_to_position(state.anchor);
        (self.cursor_line, self.cursor_col) = self.flat_byte_to_position(state.caret);
        self.has_selection = state.anchor != state.caret;
        self.selection_reversed = state.caret < state.anchor;
        self.reveal_caret = true;
        self.ensure_cursor_visible(self.viewport_h);
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
        self.lines
            .get(line)
            .map(|s| s.len() - usize::from(line + 1 < self.lines.len() && s.ends_with('\r')))
            .unwrap_or(0)
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
            .min(self.line_end(line))
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

    /// Fixture key fallback; product keybindings invoke the same semantic operations.
    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if window
            .root::<super::product::ProductShell>()
            .flatten()
            .is_some()
        {
            return;
        }
        let modifiers = ev.keystroke.modifiers;
        let movement = match ev.keystroke.key.as_str() {
            "left" if modifiers.platform => "line-start",
            "right" if modifiers.platform => "line-end",
            "up" if modifiers.platform => "document-start",
            "down" if modifiers.platform => "document-end",
            "left" if modifiers.alt => "word-left",
            "right" if modifiers.alt => "word-right",
            "left" => "left",
            "right" => "right",
            "up" => "up",
            "down" => "down",
            "home" => "line-start",
            "end" => "line-end",
            "pageup" => "page-up",
            "pagedown" => "page-down",
            key if !modifiers.platform && !modifiers.control && !modifiers.alt => {
                let command = match key {
                    "enter" => "editor.insert-newline",
                    "tab" => "editor.insert-tab",
                    "backspace" => "editor.delete-backward",
                    "delete" => "editor.delete-forward",
                    _ => return,
                };
                self.execute_editing_command(command, window, cx);
                cx.stop_propagation();
                return;
            }
            _ => return,
        };
        let prefix = if modifiers.shift { "select" } else { "move" };
        self.execute_editing_command(&format!("editor.{prefix}-{movement}"), window, cx);
        cx.stop_propagation();
    }

    pub(crate) fn execute_editing_command(
        &mut self,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if command == "editor.undo" || command == "editor.redo" {
            self.marked_range_utf16 = None;
            self.preferred_x = None;
            let replay = self.model.update(cx, |model, cx| {
                let changed = if command == "editor.undo" {
                    model.undo()
                } else {
                    model.redo()
                }
                .unwrap_or(false);
                if changed {
                    cx.notify();
                }
                changed.then(|| {
                    (
                        model.text(),
                        model.resolved_contributions(),
                        model.resolve_view_position(self.position_range),
                        model.history_view_restore().cloned(),
                    )
                })
            });
            if let Some((text, contributions, position, restore)) = replay {
                self.rebuild_projection(&text, contributions, position);
                if let Some(restore) = restore {
                    self.apply_history_view_restore(restore.generation, &restore.state);
                }
                self.sync_view_position(cx);
                cx.notify();
            }
            return true;
        }
        if matches!(command, "editor.copy" | "editor.cut") {
            self.marked_range_utf16 = None;
            if command == "editor.cut" && !self.model.read(cx).is_editable() {
                return true;
            }
            if let Some((start, end)) = self.selection_range() {
                let text = self.flat_doc()
                    [self.to_flat_byte(start.0, start.1)..self.to_flat_byte(end.0, end.1)]
                    .to_owned();
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                if command == "editor.cut" {
                    self.replace_text_with_grouping(None, "", None, window, cx);
                }
            }
            return true;
        }
        if command == "editor.paste" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.replace_text_with_grouping(None, &text, None, window, cx);
            }
            return true;
        }
        if command == "editor.select-all" {
            self.break_history_group(cx);
            self.marked_range_utf16 = None;
            self.anchor_line = 0;
            self.anchor_col = 0;
            self.cursor_line = self.lines.len() - 1;
            self.cursor_col = self.line_end(self.cursor_line);
            self.has_selection = self.cursor_line != 0 || self.cursor_col != 0;
            self.preferred_x = None;
            self.finish_position_change(cx);
            return true;
        }
        if matches!(command, "editor.insert-newline" | "editor.insert-tab") {
            self.replace_text_with_grouping(
                None,
                if command.ends_with("newline") {
                    "\n"
                } else {
                    "\t"
                },
                None,
                window,
                cx,
            );
            return true;
        }
        if matches!(command, "editor.delete-backward" | "editor.delete-forward") {
            if self.has_selection || self.marked_range_utf16.is_some() {
                self.replace_text_with_grouping(None, "", None, window, cx);
            } else {
                let doc = self.flat_doc();
                let caret = self.to_flat_byte(self.cursor_line, self.cursor_col);
                let (start, end) = if command.ends_with("backward") {
                    (
                        doc.grapheme_indices(true)
                            .map(|(i, _)| i)
                            .take_while(|i| *i < caret)
                            .last()
                            .unwrap_or(caret),
                        caret,
                    )
                } else {
                    (
                        caret,
                        doc.grapheme_indices(true)
                            .map(|(i, _)| i)
                            .find(|i| *i > caret)
                            .unwrap_or(doc.len()),
                    )
                };
                if start != end {
                    self.replace_text_with_grouping(
                        Some(
                            self.byte_col_to_utf16(&doc, start)..self.byte_col_to_utf16(&doc, end),
                        ),
                        "",
                        Some(if command.ends_with("backward") {
                            EditGroupKind::DeleteBackward
                        } else {
                            EditGroupKind::DeleteForward
                        }),
                        window,
                        cx,
                    );
                }
            }
            return true;
        }
        let (movement, extend) = if let Some(movement) = command.strip_prefix("editor.move-") {
            (movement, false)
        } else if let Some(movement) = command.strip_prefix("editor.select-") {
            (movement, true)
        } else {
            return false;
        };
        self.break_history_group(cx);
        self.marked_range_utf16 = None;
        let last = self.lines.len() - 1;
        let forward = matches!(
            movement,
            "right" | "down" | "word-right" | "line-end" | "page-down" | "document-end"
        );
        let vertical = matches!(movement, "up" | "down" | "page-up" | "page-down");
        let target = match movement {
            "left" if self.cursor_col > 0 => (
                self.cursor_line,
                self.prev_boundary(self.cursor_line, self.cursor_col),
            ),
            "left" if self.cursor_line > 0 => {
                (self.cursor_line - 1, self.line_end(self.cursor_line - 1))
            }
            "left" => (0, 0),
            "right" if self.cursor_col < self.line_end(self.cursor_line) => (
                self.cursor_line,
                self.next_boundary(self.cursor_line, self.cursor_col),
            ),
            "right" if self.cursor_line < last => (self.cursor_line + 1, 0),
            "right" => (last, self.line_end(last)),
            "line-start" => (self.cursor_line, 0),
            "line-end" => (self.cursor_line, self.line_end(self.cursor_line)),
            "document-start" => (0, 0),
            "document-end" => (last, self.line_end(last)),
            "word-left" | "word-right" => {
                let doc = self.flat_doc();
                let caret = self.to_flat_byte(self.cursor_line, self.cursor_col);
                let offset = if forward {
                    doc.unicode_word_indices()
                        .map(|(i, word)| i + word.len())
                        .find(|i| *i > caret)
                        .unwrap_or(doc.len())
                } else {
                    doc.unicode_word_indices()
                        .map(|(i, _)| i)
                        .take_while(|i| *i < caret)
                        .last()
                        .unwrap_or(0)
                };
                self.flat_byte_to_position(offset)
            }
            "up" | "down" | "page-up" | "page-down" => {
                let rows = if movement.starts_with("page-") {
                    (self.viewport_h / LINE_HEIGHT).floor().max(1.) as usize
                } else {
                    1
                };
                let line = if forward {
                    self.cursor_line.saturating_add(rows).min(last)
                } else {
                    self.cursor_line.saturating_sub(rows)
                };
                let x = self.preferred_x.unwrap_or_else(|| self.caret_x(window));
                self.preferred_x = Some(x);
                (line, self.column_for_x(line, x, window))
            }
            _ => return false,
        };
        if extend {
            if !self.has_selection {
                self.anchor_line = self.cursor_line;
                self.anchor_col = self.cursor_col;
            }
            (self.cursor_line, self.cursor_col) = target;
            self.has_selection = target != (self.anchor_line, self.anchor_col);
        } else if self.has_selection && matches!(movement, "left" | "right" | "up" | "down") {
            let (start, end) = self.selection_range().unwrap();
            (self.cursor_line, self.cursor_col) = if forward { end } else { start };
            self.has_selection = false;
            self.preferred_x = None;
        } else {
            (self.cursor_line, self.cursor_col) = target;
            self.has_selection = false;
        }
        if !vertical {
            self.preferred_x = None;
        }
        self.finish_position_change(cx);
        true
    }

    fn caret_x(&self, window: &Window) -> Pixels {
        let text = &self.lines[self.cursor_line];
        let shaped = shape_editor_line(
            window,
            text.clone().into(),
            px(FONT_SIZE),
            &runs_for(text, &self.segs[self.cursor_line]),
            None,
        );
        x_for_index_dir(&shaped, self.cursor_col, text)
            + self.line_x_offset(self.cursor_line, shaped.width, self.bounds.size.width)
            + px(self.scroll_x)
    }

    fn column_for_x(&self, line: usize, x: Pixels, window: &Window) -> usize {
        let text = &self.lines[line];
        let shaped = shape_editor_line(
            window,
            text.clone().into(),
            px(FONT_SIZE),
            &runs_for(text, &self.segs[line]),
            None,
        );
        let offset =
            self.line_x_offset(line, shaped.width, self.bounds.size.width) + px(self.scroll_x);
        text.grapheme_indices(true)
            .map(|(i, _)| i)
            .filter(|i| *i <= self.line_end(line))
            .chain(std::iter::once(self.line_end(line)))
            .min_by(|a, b| {
                f32::from((x_for_index_dir(&shaped, *a, text) + offset - x).abs()).total_cmp(
                    &f32::from((x_for_index_dir(&shaped, *b, text) + offset - x).abs()),
                )
            })
            .unwrap_or(0)
    }

    /// Mouse-down hit-tests the click onto a (line, byte_col) caret and moves
    /// it there. Uses `closest_index_for_x` on the shaped line — the public
    /// `LineLayout` API — so the caret lands on the nearest glyph boundary
    /// rather than a guessed byte offset. With shift held, the existing caret
    /// becomes the selection anchor and the click position becomes the new
    /// caret (extending the selection); without shift, any selection is
    /// cleared and the caret jumps to the click.
    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.break_history_group(cx);
        self.drag_anchor = None;
        self.marked_range_utf16 = None;
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
        self.drag_anchor = Some(if ev.modifiers.shift {
            if self.has_selection {
                (self.anchor_line, self.anchor_col)
            } else {
                (self.cursor_line, self.cursor_col)
            }
        } else {
            (line, col)
        });
        self.preferred_x = None;
        if ev.click_count >= 2 && !ev.modifiers.shift {
            let (start, end) = if ev.click_count >= 3 {
                (
                    (line, 0),
                    if line + 1 < self.lines.len() {
                        (line + 1, 0)
                    } else {
                        (line, self.line_end(line))
                    },
                )
            } else {
                let text = &self.lines[line];
                let (start, segment) = text
                    .split_word_bound_indices()
                    .find(|(start, segment)| col < start + segment.len())
                    .unwrap_or((text.len(), ""));
                ((line, start), (line, start + segment.len()))
            };
            (self.anchor_line, self.anchor_col) = start;
            self.drag_anchor = Some(start);
            (self.cursor_line, self.cursor_col) = end;
            self.has_selection = start != end;
            self.finish_position_change(cx);
            return;
        }
        if ev.modifiers.shift {
            if !self.has_selection {
                self.anchor_line = self.cursor_line;
                self.anchor_col = self.cursor_col;
            }
            self.cursor_line = line;
            self.cursor_col = col;
            self.has_selection =
                self.cursor_line != self.anchor_line || self.cursor_col != self.anchor_col;
        } else {
            self.cursor_line = line;
            self.cursor_col = col;
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
    /// Window-level pointer listeners keep the gesture active outside the pane.
    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag_anchor = None;
            self.suppress_drag_selection = false;
            return;
        }
        if self.suppress_drag_selection {
            return;
        }
        let Some(anchor) = self.drag_anchor else {
            return;
        };
        let (line, col) = self.hit_test(ev.position, window);
        (self.anchor_line, self.anchor_col) = anchor;
        self.cursor_line = line;
        self.cursor_col = col;
        self.preferred_x = None;
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
        (match self.lines.get(line) {
            Some(s) if is_rtl_line(s) => (pane_w - shaped_w).max(px(0.)),
            _ => px(0.),
        }) - px(self.scroll_x)
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
            let shaped = shape_editor_line(
                window,
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
            boundaries.retain(|column| *column <= self.line_end(line));
            boundaries.push(self.line_end(line));
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
    fn flat_utf16_to_position(&self, mut off: usize) -> (usize, usize) {
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
    fn splice(
        &mut self,
        byte_start: usize,
        byte_end: usize,
        text: &str,
        grouping: Option<EditGroupKind>,
        view: HistoryViewChange,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_completion();
        let model = self.model.clone();
        model.update(cx, |model, cx| {
            let changed = if let Some(kind) = grouping {
                model.replace_grouped_from_view(
                    byte_start..byte_end,
                    text,
                    EditGrouping {
                        context: self.history_context,
                        kind,
                    },
                    view,
                )
            } else {
                model.replace_from_view(byte_start..byte_end, text, view)
            }
            .unwrap_or(false);
            if changed {
                cx.notify();
            }
        });
        let model = model.read(cx);
        self.rebuild_projection(&model.text(), model.resolved_contributions(), None);
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

impl EditorView {
    fn replace_text_with_grouping(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        grouping: Option<EditGroupKind>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.model.read(cx).is_editable() {
            return;
        }
        self.preferred_x = None;
        let text: String = text.replace("\r\n", "\n").replace('\r', "\n");
        let (byte_start, byte_end) = self.resolve_replacement_range(range);
        let before = self.history_view_state();
        let after_offset = byte_start + text.len();
        let after = HistoryViewState {
            context: self.history_context,
            anchor: after_offset,
            caret: after_offset,
        };
        self.splice(
            byte_start,
            byte_end,
            &text,
            grouping,
            HistoryViewChange { before, after },
            cx,
        );
        self.marked_range_utf16 = None;

        let doc = self.flat_doc();
        let insert_end_byte = byte_start + text.len();
        let insert_end_utf16 = self.byte_col_to_utf16(&doc, insert_end_byte.min(doc.len()));
        let (line, col) = self.flat_utf16_to_position(insert_end_utf16);
        self.cursor_line = line;
        self.cursor_col = col;
        self.has_selection = false;
        self.finish_position_change(cx);
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<EditorContributionAction> for EditorView {}

impl EntityInputHandler for EditorView {
    /// Return the substring of the flat document at the given UTF-16 range.
    /// Report scalar-aligned offsets when a request splits a UTF-16 surrogate pair.
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let doc = self.flat_doc();
        let start = self.utf16_to_byte_col(&doc, range.start);
        let end = self.utf16_to_byte_col(&doc, range.end);
        if start > doc.len() || end > doc.len() || start > end {
            return None;
        }
        *adjusted = Some(self.byte_col_to_utf16(&doc, start)..self.byte_col_to_utf16(&doc, end));
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

    /// Commit the preedit marking without changing its text or selection.
    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.marked_range_utf16.take().is_some() {
            self.break_history_group(cx);
            cx.notify();
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
        let grouping = if self.marked_range_utf16.is_some() {
            EditGroupKind::Composition
        } else {
            EditGroupKind::Typing
        };
        self.replace_text_with_grouping(range, text, Some(grouping), _window, cx);
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
        if !self.model.read(cx).is_editable() {
            return;
        }
        self.preferred_x = None;
        let (byte_start, byte_end) = self.resolve_replacement_range(range);

        // Compute the UTF-16 offset where the marked text will start.
        let doc = self.flat_doc();
        let marked_start_utf16 = self.byte_col_to_utf16(&doc, byte_start);

        let marked_utf16_len: usize = new_text.chars().map(char::len_utf16).sum();
        let (sel_start_rel, sel_end_rel) = match new_selected_range.clone() {
            Some(r) => (r.start, r.end),
            None => (marked_utf16_len, marked_utf16_len),
        };
        let anchor = byte_start + self.utf16_to_byte_col(new_text, sel_start_rel);
        let caret = byte_start + self.utf16_to_byte_col(new_text, sel_end_rel);

        self.splice(
            byte_start,
            byte_end,
            new_text,
            Some(EditGroupKind::Composition),
            HistoryViewChange {
                before: self.history_view_state(),
                after: HistoryViewState {
                    context: self.history_context,
                    anchor,
                    caret,
                },
            },
            cx,
        );

        // Marked range = [marked_start, marked_start + utf16_len(new_text)).
        self.marked_range_utf16 = Some(marked_start_utf16..marked_start_utf16 + marked_utf16_len);

        // Caret + anchor from new_selected_range (relative to marked start).
        let anchor_utf16 = marked_start_utf16 + sel_start_rel.min(marked_utf16_len);
        let caret_utf16 = marked_start_utf16 + sel_end_rel.min(marked_utf16_len);
        let (al, ac) = self.flat_utf16_to_position(anchor_utf16);
        let (cl, cc) = self.flat_utf16_to_position(caret_utf16);
        self.anchor_line = al;
        self.anchor_col = ac;
        self.cursor_line = cl;
        self.cursor_col = cc;
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
        let (start_line, start_col) = self.flat_utf16_to_position(range_utf16.start);
        let (end_line, end_col) = self.flat_utf16_to_position(range_utf16.end);
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
        let shaped = shape_editor_line(
            window,
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        if focused != self.history_was_focused {
            self.break_history_group(cx);
            self.history_was_focused = focused;
        }
        let entity = cx.entity();
        let mut key_context = KeyContext::default();
        key_context.add(EDITOR_KEY_CONTEXT);
        if self.completion.is_some() {
            key_context.add("editor_completion");
        }
        let completion = self
            .completion_surface
            .as_ref()
            .map(|surface| surface.render(self.completion_anchor(window)));
        div()
            .id(("editor", self.element_id))
            .size_full()
            .bg(rgb(0x1e1e1e))
            // Track focus so clicking the pane focuses this view (gpui
            // auto-focuses a tracked element on mouse-down) and so on_key_down
            // listeners below actually receive keystrokes.
            .track_focus(&self.focus)
            .key_context(key_context)
            .on_action(cx.listener(Self::completion_previous))
            .on_action(cx.listener(Self::completion_next))
            .on_action(cx.listener(Self::completion_accept))
            .on_action(cx.listener(Self::completion_dismiss))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            // Mouse-move extends the selection while the left button is held
            // (drag-select). `MouseMoveEvent::dragging()` is true when the
            // platform reports the left button as currently pressed, so the
            // handler self-gates and we don't need a separate mouse-up.
            // Scroll wheel: accumulate pixel delta, clamp against the real
            // viewport height (written each paint by the element). macOS
            // "natural" scroll: trackpad two-finger gesture down moves the
            // content up, i.e. the viewport descends — scroll increases.
            // ScrollWheelEvent delta is positive for swipes upward (toward
            // content top), so we invert it here.
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, window, cx| {
                let (dx, dy) = match ev.delta {
                    ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
                    ScrollDelta::Lines(p) => (p.x * LINE_HEIGHT, p.y * LINE_HEIGHT),
                };
                this.scroll -= dy;
                let max_width = this
                    .lines
                    .iter()
                    .zip(&this.segs)
                    .map(|(line, segs)| {
                        f32::from(
                            shape_editor_line(
                                window,
                                line.clone().into(),
                                px(FONT_SIZE),
                                &runs_for(line, segs),
                                None,
                            )
                            .width,
                        )
                    })
                    .fold(0., f32::max);
                this.scroll_x = (this.scroll_x - dx).clamp(
                    0.,
                    (max_width + 2. - f32::from(this.bounds.size.width)).max(0.),
                );
                this.reveal_caret = false;
                this.clamp_scroll();
                cx.notify();
            }))
            .child(EditorElement { entity })
            .children(completion)
    }
}

impl EditorView {
    pub(crate) fn start_completion(
        &mut self,
        buffer: BufferHandle,
        providers: Vec<CompletionProviderRegistration>,
        cx: &mut Context<Self>,
    ) -> Vec<(CompletionProviderRegistration, CompletionRequest)> {
        self.dismiss_completion();
        let cursor_byte_offset = self.to_flat_byte(self.cursor_line, self.cursor_col);
        let Some((text, revision)) = self.model.read_with(cx, |model, _| {
            model
                .is_editable()
                .then(|| (model.text(), model.revision()))
        }) else {
            return Vec::new();
        };
        if cursor_byte_offset > text.len() || !text.is_char_boundary(cursor_byte_offset) {
            return Vec::new();
        }
        let mut prefix_start = cursor_byte_offset;
        while prefix_start > 0 {
            let byte = text.as_bytes()[prefix_start - 1];
            if !byte.is_ascii_alphanumeric() && byte != b'_' {
                break;
            }
            prefix_start -= 1;
        }
        let generation = self.next_completion_generation;
        self.next_completion_generation = self
            .next_completion_generation
            .checked_add(1)
            .expect("completion generation space exhausted");
        let controller = CompletionController::new(
            buffer,
            revision,
            cursor_byte_offset,
            prefix_start..cursor_byte_offset,
            text[prefix_start..cursor_byte_offset].into(),
            generation,
            providers,
        );
        let requests = controller.requests();
        self.completion_surface = Some(Box::new(ListCompletionSurface::attach(
            controller.snapshot().clone(),
        )));
        self.completion = Some(controller);
        cx.notify();
        requests
    }

    pub(crate) fn apply_completion_response(
        &mut self,
        buffer: BufferHandle,
        response: CompletionResponse,
        cx: &mut Context<Self>,
    ) -> bool {
        let revision = self.model.read(cx).revision();
        let Some(controller) = self.completion.as_mut() else {
            return false;
        };
        if controller.buffer() != buffer || controller.revision() != revision {
            return false;
        }
        if controller.apply_response(response) {
            if let Some(surface) = self.completion_surface.as_mut() {
                surface.update(controller.snapshot().clone());
            }
            cx.notify();
            true
        } else {
            false
        }
    }

    pub(crate) fn retain_completion_task(
        &mut self,
        generation: u64,
        registration: CompletionProviderRegistrationId,
        task: Task<()>,
    ) {
        if self
            .completion
            .as_ref()
            .is_some_and(|controller| controller.generation() == generation)
        {
            self.completion_tasks.push((registration, task));
        }
    }

    pub(crate) fn cancel_completion_provider(
        &mut self,
        registration: CompletionProviderRegistrationId,
        cx: &mut Context<Self>,
    ) {
        self.completion_tasks
            .retain(|(provider, _)| *provider != registration);
        if let Some(controller) = self.completion.as_mut()
            && controller.provider_unavailable(registration)
        {
            if let Some(surface) = self.completion_surface.as_mut() {
                surface.update(controller.snapshot().clone());
            }
            cx.notify();
        }
    }

    fn dismiss_completion(&mut self) {
        if let Some(surface) = self.completion_surface.as_mut() {
            surface.detach();
        }
        self.completion_surface = None;
        self.completion = None;
        self.completion_tasks.clear();
    }

    fn completion_anchor(&self, window: &mut Window) -> Point<Pixels> {
        let line = self.cursor_line.min(self.lines.len().saturating_sub(1));
        let text = self.lines.get(line).map(String::as_str).unwrap_or("");
        let x = if text.is_empty() {
            px(0.)
        } else {
            let shaped = shape_editor_line(
                window,
                SharedString::from(text.to_owned()),
                px(FONT_SIZE),
                &runs_for(text, &self.segs[line]),
                None,
            );
            self.line_x_offset(line, shaped.width, self.bounds.size.width)
                + x_for_index_dir(&shaped, self.cursor_col, text)
        };
        let y = px((line + 1) as f32 * LINE_HEIGHT - self.scroll);
        point(
            x.min((self.bounds.size.width - px(380.)).max(px(0.))),
            y.min((self.bounds.size.height - px(230.)).max(px(0.))),
        )
    }

    fn completion_previous(
        &mut self,
        _: &CompletionPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(surface) = self.completion_surface.as_mut() {
            surface.move_selection(-1);
            cx.notify();
        }
    }

    fn completion_next(&mut self, _: &CompletionNext, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(surface) = self.completion_surface.as_mut() {
            surface.move_selection(1);
            cx.notify();
        }
    }

    fn completion_accept(&mut self, _: &CompletionAccept, _: &mut Window, cx: &mut Context<Self>) {
        self.accept_selected_completion(cx);
    }

    fn accept_selected_completion(&mut self, cx: &mut Context<Self>) {
        let selected = self
            .completion_surface
            .as_ref()
            .and_then(|surface| surface.selected_item());
        let accepted = selected.and_then(|selected| {
            let controller = self.completion.as_ref()?;
            let item = controller.item(selected)?;
            Some((
                controller.revision(),
                controller.prefix_range(),
                item.insert_text.clone(),
            ))
        });
        self.dismiss_completion();
        let Some((revision, range, insert_text)) = accepted else {
            cx.notify();
            return;
        };
        let edit = TextEdit {
            range: ByteRange {
                start_byte_offset: range.start,
                end_byte_offset: range.end,
            },
            text: insert_text,
        };
        self.model.update(cx, |model, cx| {
            if model.apply_edits(&[edit], revision).unwrap_or(false) {
                cx.notify();
            }
        });
    }

    fn completion_dismiss(
        &mut self,
        _: &CompletionDismiss,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_completion();
        cx.notify();
    }

    fn swap_completion_surface(&mut self, cx: &mut Context<Self>) {
        let Some(controller) = self.completion.as_ref() else {
            return;
        };
        let next: Box<dyn CompletionSurface> = match self
            .completion_surface
            .as_ref()
            .map(|surface| surface.kind())
        {
            Some(CompletionSurfaceKind::List) => Box::new(CompactCompletionSurface::attach(
                controller.snapshot().clone(),
            )),
            _ => Box::new(ListCompletionSurface::attach(controller.snapshot().clone())),
        };
        if let Some(surface) = self.completion_surface.as_mut() {
            surface.detach();
        }
        self.completion_surface = Some(next);
        cx.notify();
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
        self.entity.update(cx, |view, _| {
            let resized = view.bounds.size != bounds.size;
            view.bounds = bounds;
            view.viewport_h = f32::from(bounds.size.height);
            if view.reveal_caret || resized {
                view.ensure_cursor_visible(view.viewport_h);
                let x = f32::from(view.caret_x(window));
                let width = f32::from(bounds.size.width);
                if x < view.scroll_x {
                    view.scroll_x = x.max(0.);
                }
                if x + 2. > view.scroll_x + width {
                    view.scroll_x = (x + 2. - width).max(0.);
                }
                view.reveal_caret = false;
            }
            view.clamp_scroll();
        });
        let drag_listener = window.listener_for(&self.entity, EditorView::on_mouse_move);
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                drag_listener(event, window, cx);
            }
        });
        let release_listener = window.listener_for(&self.entity, |view, _: &MouseUpEvent, _, _| {
            view.drag_anchor = None;
        });
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                release_listener(event, window, cx);
            }
        });
        let scroll_x = self.entity.read(cx).scroll_x;
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
            // underline painting. Done inside the borrow since flat_utf16_to_position
            // needs &self.
            let marked = view.marked_range_utf16.as_ref().map(|mr| {
                let s = view.flat_utf16_to_position(mr.start);
                let e = view.flat_utf16_to_position(mr.end);
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
                    let s = shape_editor_line(
                        window,
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
                    Some((*ix, s, x_off - px(scroll_x), line.as_str()))
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
                    window.paint_quad(fill(rect, sel_color));
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
                        window.paint_quad(fill(rect, sel_color));
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
                    window.paint_quad(fill(rect, mark_color));
                }
            }

            // Caret: a 2px vertical bar at the shaped line's
            // `x_for_index(cursor_col)`, painted only when the editor holds
            // window focus AND there is no active selection (macOS hides the
            // caret while a selection is drag-held). No blink yet —
            // IME/selection stages get a timer.
            if focused
                && selection.is_none()
                && let Some((line_ix, col)) = caret
            {
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
                window.paint_quad(fill(caret_bounds, hsla(0., 0., 0.9, 1.0)));
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
                window.paint_quad(fill(marker_bounds, rgb(marker.color)));
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
            window.paint_quad(fill(thumb_bounds, hsla(0., 0., 0.6, 0.4)));
        }
    }
}

/// Map a line's owned `Seg` rows to gpui `TextRun`s covering the whole line.
/// Four-column tab stops are a rendering projection; source byte offsets stay intact.
fn expand_tabs(text: &str) -> String {
    let mut out = String::new();
    let mut column = 0;
    for grapheme in text.graphemes(true) {
        if grapheme == "\t" {
            let count = 4 - column % 4;
            out.extend(std::iter::repeat_n(' ', count));
            column += count;
        } else {
            out.push_str(grapheme);
            column += 1;
        }
    }
    out
}

fn shape_editor_line(
    window: &Window,
    text: SharedString,
    font_size: Pixels,
    runs: &[TextRun],
    _: Option<Pixels>,
) -> ShapedLine {
    if !text.contains('\t') {
        return window.text_system().shape_line(text, font_size, runs, None);
    }
    let mut offset = 0;
    let projected_runs = runs
        .iter()
        .map(|run| {
            let end = offset + run.len;
            let mut projected = run.clone();
            projected.len = expand_tabs(&text[..end]).len() - expand_tabs(&text[..offset]).len();
            offset = end;
            projected
        })
        .collect::<Vec<_>>();
    window
        .text_system()
        .shape_line(expand_tabs(&text).into(), font_size, &projected_runs, None)
}

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
    // substitutes the system fonts for those ranges. The cascade order is
    // Geeza Pro (Arabic), PingFang SC (CJK), then Apple Color Emoji.
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
    let expanded;
    let (index, line_str) = if line_str.contains('\t') {
        let mapped = expand_tabs(&line_str[..index]).len();
        expanded = expand_tabs(line_str);
        (mapped, expanded.as_str())
    } else {
        (index, line_str)
    };
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

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 20.0;

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
    use crate::{
        app::completion::CompletionProviderRegistry,
        host::protocol::{
            BufferHandle, ByteRange, CompletionResponse, CompletionResultItem, DecorationToken,
            EditorContribution, ExtensionId, ExtensionLifecycleId, GutterToken,
        },
    };

    #[gpui::test]
    fn ordinary_editing_keeps_projection_clipboard_and_composition_synchronous(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(|_| BufferModel::from_text(""));
        let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
        cx.update(|window, cx| {
            editor.update(cx, |view, cx| {
                view.replace_text_in_range(None, "a", window, cx);
                view.replace_text_in_range(None, "👩‍💻", window, cx);
                view.replace_text_in_range(None, "é", window, cx);
                assert_eq!(view.flat_doc(), "a👩‍💻é");
                view.execute_editing_command("editor.delete-backward", window, cx);
                assert_eq!(view.flat_doc(), "a👩‍💻");
                view.execute_editing_command("editor.select-all", window, cx);
                view.execute_editing_command("editor.cut", window, cx);
                assert_eq!(view.flat_doc(), "");
                view.execute_editing_command("editor.paste", window, cx);
                assert_eq!(view.flat_doc(), "a👩‍💻");
                view.replace_and_mark_text_in_range(None, "に", Some(0..1), window, cx);
                assert_eq!(view.marked_range_utf16, Some(6..7));
                view.replace_and_mark_text_in_range(None, "日本", Some(2..2), window, cx);
                assert_eq!(view.flat_doc(), "a👩‍💻日本");
                view.replace_text_in_range(None, "日本語", window, cx);
                assert_eq!(view.flat_doc(), "a👩‍💻日本語");
                assert_eq!(view.marked_range_utf16, None);
                view.execute_editing_command("editor.undo", window, cx);
                assert_eq!(view.flat_doc(), "a👩‍💻");
                view.execute_editing_command("editor.redo", window, cx);
                assert_eq!(view.flat_doc(), "a👩‍💻日本語");
                view.execute_editing_command("editor.select-all", window, cx);
                view.replace_text_in_range(None, "one\r\ntwo\rthree", window, cx);
                assert_eq!(view.flat_doc(), "one\ntwo\nthree");
            })
        });
    }

    #[gpui::test]
    fn typing_deletion_and_interaction_boundaries_form_practical_undo_steps(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(|_| BufferModel::from_text(""));
        let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
        cx.update(|window, cx| {
            editor.update(cx, |view, cx| {
                view.replace_text_in_range(None, "a", window, cx);
                view.replace_text_in_range(None, "β", window, cx);
                view.replace_text_in_range(None, "c", window, cx);
                view.execute_editing_command("editor.undo", window, cx);
                assert_eq!(view.flat_doc(), "");
                view.execute_editing_command("editor.redo", window, cx);
                assert_eq!(view.flat_doc(), "aβc");

                view.execute_editing_command("editor.move-document-end", window, cx);
                view.execute_editing_command("editor.delete-backward", window, cx);
                view.execute_editing_command("editor.delete-backward", window, cx);
                assert_eq!(view.flat_doc(), "a");
                view.execute_editing_command("editor.undo", window, cx);
                assert_eq!(view.flat_doc(), "aβc");

                view.execute_editing_command("editor.move-document-end", window, cx);
                view.execute_editing_command("editor.move-left", window, cx);
                view.replace_text_in_range(None, "X", window, cx);
                assert_eq!(view.flat_doc(), "aβXc");
                view.execute_editing_command("editor.undo", window, cx);
                assert_eq!(view.flat_doc(), "aβc");

                cx.write_to_clipboard(gpui::ClipboardItem::new_string(" pasted".into()));
                view.execute_editing_command("editor.paste", window, cx);
                assert_eq!(view.flat_doc(), "aβ pastedc");
                view.execute_editing_command("editor.undo", window, cx);
                assert_eq!(view.flat_doc(), "aβc");
            });
        });
    }

    #[gpui::test]
    fn undo_and_redo_restore_the_originating_views_grouped_selection(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("abcdef"));
        let (other, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
        let origin = cx.new(|cx| EditorView::new(model.clone(), cx));

        cx.update(|window, cx| {
            origin.update(cx, |view, cx| {
                view.anchor_col = 4;
                view.cursor_col = 1;
                view.has_selection = true;
                view.sync_view_position(cx);
                view.replace_text_in_range(None, "X", window, cx);
                view.replace_text_in_range(None, "Y", window, cx);
            });
            other.update(cx, |view, cx| {
                view.cursor_col = 5;
                view.sync_view_position(cx);
                view.execute_editing_command("editor.undo", window, cx);
            });
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let origin = origin.read(cx);
            let other = other.read(cx);
            assert_eq!(origin.flat_doc(), "abcdef");
            assert_eq!((origin.anchor_col, origin.cursor_col), (4, 1));
            assert!(origin.has_selection);
            assert!(origin.selection_reversed);
            assert!(!other.has_selection);
            assert_ne!((other.cursor_line, other.cursor_col), (0, 1));
        });

        cx.update(|window, cx| {
            other.update(cx, |view, cx| {
                view.execute_editing_command("editor.redo", window, cx);
            });
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let origin = origin.read(cx);
            assert_eq!(origin.flat_doc(), "aXYef");
            assert_eq!((origin.cursor_line, origin.cursor_col), (0, 3));
            assert!(!origin.has_selection);
        });
    }

    #[gpui::test]
    fn replay_ignores_view_state_for_an_originating_view_that_was_dropped(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("text"));
        let (survivor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
        let origin = cx.new(|cx| EditorView::new(model.clone(), cx));

        cx.update(|window, cx| {
            origin.update(cx, |view, cx| {
                view.execute_editing_command("editor.move-document-end", window, cx);
                view.replace_text_in_range(None, "!", window, cx);
            });
        });
        drop(origin);
        cx.run_until_parked();

        cx.update(|window, cx| {
            survivor.update(cx, |view, cx| {
                view.execute_editing_command("editor.undo", window, cx);
            });
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let survivor = survivor.read(cx);
            assert_eq!(survivor.flat_doc(), "text");
            assert_eq!((survivor.cursor_line, survivor.cursor_col), (0, 0));
        });
    }

    #[gpui::test]
    fn a_newer_edit_invalidates_an_unobserved_history_view_restore(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("abcdef"));
        let (other, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
        let origin = cx.new(|cx| EditorView::new(model.clone(), cx));

        cx.update(|window, cx| {
            origin.update(cx, |view, cx| {
                view.anchor_col = 4;
                view.cursor_col = 1;
                view.has_selection = true;
                view.sync_view_position(cx);
                view.replace_text_in_range(None, "X", window, cx);
            });
            other.update(cx, |view, cx| {
                view.execute_editing_command("editor.undo", window, cx);
            });
            model.update(cx, |model, cx| {
                assert!(model.replace(0..0, "z").unwrap());
                cx.notify();
            });
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let origin = origin.read(cx);
            assert_eq!(origin.flat_doc(), "zabcdef");
            assert_eq!((origin.cursor_line, origin.cursor_col), (0, 5));
            assert!(!origin.has_selection);
        });
    }

    #[gpui::test]
    fn movement_uses_visual_columns_graphemes_and_pane_height(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("éééx\na\nabcde\nword next\nlast"));
        let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model, cx));
        cx.refresh().unwrap();
        cx.update(|window, cx| {
            editor.update(cx, |view, cx| {
                view.cursor_col = "ééé".len();
                view.execute_editing_command("editor.move-down", window, cx);
                assert_eq!((view.cursor_line, view.cursor_col), (1, 1));
                view.execute_editing_command("editor.move-down", window, cx);
                assert_eq!((view.cursor_line, view.cursor_col), (2, 3));
                view.execute_editing_command("editor.move-document-start", window, cx);
                view.viewport_h = 2. * super::LINE_HEIGHT;
                view.execute_editing_command("editor.select-page-down", window, cx);
                assert_eq!(view.selected_byte_range(), Some(0..10));
                view.execute_editing_command("editor.move-document-end", window, cx);
                assert_eq!((view.cursor_line, view.cursor_col), (4, 4));
                view.execute_editing_command("editor.move-word-left", window, cx);
                assert_eq!((view.cursor_line, view.cursor_col), (4, 0));
                view.execute_editing_command("editor.select-word-left", window, cx);
                assert_eq!((view.cursor_line, view.cursor_col), (3, 5));
            })
        });
    }

    #[gpui::test]
    fn tabs_hit_testing_and_horizontal_reveal_share_geometry(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text(format!("\tx{}", "z".repeat(300))));
        let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model, cx));
        cx.refresh().unwrap();
        cx.update(|window, cx| {
            editor.update(cx, |view, cx| {
                view.cursor_col = 1;
                let tab_x = view.caret_x(window);
                assert!(f32::from(tab_x) > 20.);
                assert_eq!(
                    view.hit_test(
                        point(view.bounds.origin.x + tab_x, view.bounds.origin.y + px(5.)),
                        window
                    ),
                    (0, 1)
                );
                view.execute_editing_command("editor.move-line-end", window, cx);
            })
        });
        cx.refresh().unwrap();
        cx.read(|cx| assert!(editor.read(cx).scroll_x > 0.));
        cx.update(|window, cx| {
            editor.update(cx, |view, cx| {
                view.execute_editing_command("editor.move-line-start", window, cx);
            })
        });
        cx.refresh().unwrap();
        cx.read(|cx| assert_eq!(editor.read(cx).scroll_x, 0.));
    }

    #[gpui::test]
    fn mouse_selection_keeps_press_anchor_across_reversal_and_ignores_foreign_drags(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(|_| BufferModel::from_text("alpha beta\nnext"));
        let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model, cx));
        cx.refresh().unwrap();
        cx.update(|window, cx| {
            editor.update(cx, |view, cx| {
                let position = |view: &mut EditorView, col| {
                    view.cursor_col = col;
                    point(
                        view.bounds.origin.x + view.caret_x(window),
                        view.bounds.origin.y + px(5.),
                    )
                };
                let start = position(view, 3);
                let end = position(view, 8);
                let before = position(view, 1);
                view.on_mouse_move(
                    &MouseMoveEvent {
                        position: end,
                        modifiers: Modifiers::default(),
                        pressed_button: Some(MouseButton::Left),
                    },
                    window,
                    cx,
                );
                assert!(!view.has_selection);
                view.on_mouse_down(
                    &MouseDownEvent {
                        position: start,
                        modifiers: Modifiers::default(),
                        button: MouseButton::Left,
                        click_count: 1,
                        first_mouse: false,
                    },
                    window,
                    cx,
                );
                for p in [end, start, before] {
                    view.on_mouse_move(
                        &MouseMoveEvent {
                            position: p,
                            modifiers: Modifiers::default(),
                            pressed_button: Some(MouseButton::Left),
                        },
                        window,
                        cx,
                    );
                }
                assert_eq!(view.selected_byte_range(), Some(1..3));
                view.on_mouse_down(
                    &MouseDownEvent {
                        position: start,
                        modifiers: Modifiers::default(),
                        button: MouseButton::Left,
                        click_count: 2,
                        first_mouse: false,
                    },
                    window,
                    cx,
                );
                assert_eq!(view.selected_byte_range(), Some(0..5));
                view.on_mouse_down(
                    &MouseDownEvent {
                        position: start,
                        modifiers: Modifiers::default(),
                        button: MouseButton::Left,
                        click_count: 3,
                        first_mouse: false,
                    },
                    window,
                    cx,
                );
                assert_eq!(view.selected_byte_range(), Some(0..11));
            })
        });
    }

    #[gpui::test]
    fn crlf_is_one_navigation_and_deletion_boundary(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("a\r\nb"));
        let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model, cx));
        cx.update(|window, cx| {
            editor.update(cx, |view, cx| {
                view.execute_editing_command("editor.move-line-end", window, cx);
                assert_eq!((view.cursor_line, view.cursor_col), (0, 1));
                view.execute_editing_command("editor.move-right", window, cx);
                assert_eq!((view.cursor_line, view.cursor_col), (1, 0));
                view.execute_editing_command("editor.delete-backward", window, cx);
                assert_eq!(view.flat_doc(), "ab");
            })
        });
    }

    #[gpui::test]
    fn composition_is_cleared_by_navigation_and_external_edits(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("abc"));
        let (editor, cx) = cx.add_window_view(|_, cx| EditorView::new(model.clone(), cx));
        cx.update(|window, cx| {
            editor.update(cx, |view, cx| {
                view.replace_and_mark_text_in_range(None, "に", Some(1..1), window, cx);
                view.execute_editing_command("editor.move-right", window, cx);
                assert_eq!(view.marked_range_utf16, None);
                view.replace_and_mark_text_in_range(None, "本", Some(1..1), window, cx);
                view.execute_editing_command("editor.select-all", window, cx);
                assert_eq!(view.marked_range_utf16, None);
                view.replace_and_mark_text_in_range(None, "日", Some(1..1), window, cx);
                view.select_reveal_and_focus(
                    ByteRange {
                        start_byte_offset: 0,
                        end_byte_offset: 1,
                    },
                    window,
                    cx,
                );
                assert_eq!(view.marked_range_utf16, None);
                view.replace_and_mark_text_in_range(None, "語", Some(1..1), window, cx);
                assert!(view.marked_range_utf16.is_some());
            })
        });
        model.update(cx, |model, cx| {
            model.replace(0..0, "prefix").unwrap();
            cx.notify();
        });
        cx.run_until_parked();
        cx.read(|cx| assert_eq!(editor.read(cx).marked_range_utf16, None));
    }

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
    fn completion_acceptance_rechecks_the_captured_revision(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("fo"));
        let editor = cx.new(|cx| EditorView::new(model.clone(), cx));
        let mut registry = CompletionProviderRegistry::new();
        registry.register(
            ExtensionId::new(1),
            ExtensionLifecycleId::new(1),
            "fixture".into(),
        );

        let request = editor.update(cx, |editor, cx| {
            editor.cursor_col = 2;
            editor
                .start_completion(BufferHandle::new(1), registry.snapshot(), cx)
                .remove(0)
                .1
        });
        editor.update(cx, |editor, cx| {
            assert!(editor.apply_completion_response(
                request.buffer,
                CompletionResponse {
                    registration: request.registration,
                    revision: request.revision,
                    generation: request.generation,
                    result: Ok(vec![CompletionResultItem {
                        label: "foo".into(),
                        insert_text: "foo".into(),
                    }]),
                },
                cx,
            ));
        });
        model.update(cx, |model, _| {
            assert!(model.replace(0..0, "x").unwrap());
        });
        editor.update(cx, |editor, cx| editor.accept_selected_completion(cx));

        cx.read(|cx| assert_eq!(model.read(cx).text(), "xfo"));

        let model = cx.new(|_| BufferModel::from_text("fo"));
        let editor = cx.new(|cx| EditorView::new(model.clone(), cx));
        let request = editor.update(cx, |editor, cx| {
            editor.cursor_col = 2;
            editor
                .start_completion(BufferHandle::new(2), registry.snapshot(), cx)
                .remove(0)
                .1
        });
        editor.update(cx, |editor, cx| {
            assert!(editor.apply_completion_response(
                request.buffer,
                CompletionResponse {
                    registration: request.registration,
                    revision: request.revision,
                    generation: request.generation,
                    result: Ok(vec![CompletionResultItem {
                        label: "foo".into(),
                        insert_text: "foo".into(),
                    }]),
                },
                cx,
            ));
            editor.accept_selected_completion(cx);
        });
        cx.read(|cx| assert_eq!(model.read(cx).text(), "foo"));
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
        let model = cx.new(|_| BufferModel::from_text(format!("{first_line}\n{second_line}")));
        let editor = cx.new(|cx| EditorView::new(model, cx));

        cx.read(|cx| {
            let editor = editor.read(cx);
            let second_line_start = 5;
            let family_utf16_len = family.chars().map(char::len_utf16).sum::<usize>();

            assert_eq!(editor.flat_utf16_to_position(1), (0, 1));
            assert_eq!(editor.flat_utf16_to_position(3), (0, "a😀".len()));
            assert_eq!(editor.flat_utf16_to_position(second_line_start), (1, 0));
            assert_eq!(
                editor.flat_utf16_to_position(second_line_start + family_utf16_len),
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
                assert_eq!(editor.flat_utf16_to_position(utf16), (line, byte_col));
            }
        });
    }

    #[gpui::test]
    fn views_share_model_state_and_keep_presentation_state_independent(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("one\ntwo\nthree"));
        let first = cx.new(|cx| EditorView::new(model.clone(), cx));
        let second = cx.new(|cx| {
            EditorView::new_with_options(
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
        let model = cx.new(|_| BufferModel::from_text("0123456789"));
        let first = cx.new(|cx| EditorView::new(model.clone(), cx));
        let second = cx.new(|cx| EditorView::new(model.clone(), cx));

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
