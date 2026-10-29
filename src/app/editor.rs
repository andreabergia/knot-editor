//! Native editor presentation over an authoritative shared buffer model.
//!
//! Caret and selection use byte positions backed by model anchors. Movement and
//! pointer hit testing use grapheme boundaries and shaped visual coordinates.
//! Views independently own scrolling, selection, completion, and IME preedit.
//! A view keeps no copy of the text: it reads the lines it needs from the model
//! and converts positions through the model's line index.

mod completion;
mod editing;
mod input;
mod layout;
pub(super) mod native_commands;
mod paint;
mod text;

use layout::*;
use text::*;

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

use super::commands::dispatch as commands;
use super::typography::{TEXT_SIZE as FONT_SIZE, text_font as make_font};

use super::{
    CompletionAccept, CompletionDismiss, CompletionNext, CompletionPrevious, EDITOR_KEY_CONTEXT,
    completion::{CompletionController, CompletionProviderRegistration, CompletionSurface},
    history::{EditGroupKind, EditGrouping, HistoryViewChange, HistoryViewState},
    model::{BufferModel, ContributionSource, ResolvedEditorContribution},
    theme::{Highlight, Theme},
};

static NEXT_HISTORY_CONTEXT: AtomicU64 = AtomicU64::new(1);

/// One contributed decoration segment, styled through the decoration
/// highlight layers. `start` and `end` are byte columns within `line`.
#[derive(Clone, Copy)]
struct RenderedDecoration {
    line: usize,
    start: usize,
    end: usize,
    token: DecorationToken,
}

#[derive(Clone, Copy)]
struct RenderedGutterMarker {
    line: usize,
    highlight: Highlight,
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

/// Restorable editor presentation state. Offsets are absolute UTF-8 byte
/// positions in the shared buffer; `top` is the start of the first visible line.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EditorViewState {
    pub(crate) anchor: usize,
    pub(crate) caret: usize,
    pub(crate) top: usize,
    pub(crate) scroll_x: f32,
}

pub struct EditorView {
    element_id: usize,
    rendering: EditorRenderingOptions,
    /// Authoritative document state. Everything below is presentation state
    /// or derived from the model's current revision.
    model: Entity<BufferModel>,
    /// The model's line count as of the last refresh.
    line_count: usize,
    /// The model revision this view last refreshed from.
    seen_revision: u64,
    /// Widest line width shaped since the text last changed. Bounds horizontal
    /// scrolling without shaping lines that were never displayed.
    max_line_width: f32,
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
    preserve_scroll_on_layout: bool,
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
    /// IME preedit (marked) range as document-wide UTF-16 offsets. `None` =
    /// no active composition. Set by `replace_and_mark_text_in_range`,
    /// cleared by `replace_text_in_range` / `unmark_text`. The element paints
    /// an underline over this span.
    marked_range_utf16: Option<Range<usize>>,
    focus: FocusHandle,
    history_context: u64,
    history_was_focused: bool,
    last_history_restore_generation: u64,
    completion: Option<CompletionController>,
    completion_surface: Option<CompletionSurface>,
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
    pub(crate) fn interaction_bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// The part of `bounds` that shows text: everything right of the gutter,
    /// which exists only while the view renders gutter markers.
    pub(super) fn text_bounds(&self) -> Bounds<Pixels> {
        let gutter = if self.rendering.show_gutter_markers {
            px(GUTTER_WIDTH).min(self.bounds.size.width)
        } else {
            px(0.)
        };
        Bounds {
            origin: point(self.bounds.origin.x + gutter, self.bounds.origin.y),
            size: size(self.bounds.size.width - gutter, self.bounds.size.height),
        }
    }

    #[cfg(test)]
    fn text(&self, cx: &App) -> String {
        self.model.read(cx).text()
    }

    #[cfg(test)]
    pub(crate) fn completion_state(&self) -> Option<(usize, usize, usize, u64)> {
        let controller = self.completion.as_ref()?;
        let snapshot = controller.snapshot();
        Some((
            snapshot.items.len(),
            snapshot.pending_provider_count,
            snapshot.failures.len(),
            controller.generation(),
        ))
    }

    pub(crate) fn selected_byte_range(&self, cx: &App) -> Option<Range<usize>> {
        let model = self.model.read(cx);
        self.selection_range().map(|(start, end)| {
            to_flat_byte(model, start.0, start.1)..to_flat_byte(model, end.0, end.1)
        })
    }

    /// Retain deliberate scrolling when this view receives a relocated viewport.
    /// Pending caret reveals from editing still take precedence.
    pub(crate) fn preserve_scroll_for_relocation(&mut self) {
        self.preserve_scroll_on_layout = true;
    }

    /// The restorable presentation state: selection ends and the top visible
    /// line as byte offsets, and horizontal scroll in pixels.
    pub(crate) fn view_state(&self, cx: &App) -> EditorViewState {
        let model = self.model.read(cx);
        let state = self.history_view_state(model);
        let top_line = ((self.scroll / LINE_HEIGHT) as usize).min(self.line_count - 1);
        EditorViewState {
            anchor: state.anchor,
            caret: state.caret,
            top: model.line_range(top_line).start,
            scroll_x: self.scroll_x,
        }
    }

    /// Apply a restored presentation state. Offsets clamp to the current text
    /// and snap back to a character boundary, so changed content never yields
    /// an invalid position; the restored scroll is kept instead of revealing
    /// the caret.
    pub(crate) fn restore_view_state(&mut self, state: EditorViewState, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let snap = |offset: usize| {
            let mut offset = offset.min(model.len());
            while !model.is_char_boundary(offset) {
                offset -= 1;
            }
            offset
        };
        (self.anchor_line, self.anchor_col) = flat_byte_to_position(model, snap(state.anchor));
        (self.cursor_line, self.cursor_col) = flat_byte_to_position(model, snap(state.caret));
        self.has_selection = snap(state.anchor) != snap(state.caret);
        self.scroll = flat_byte_to_position(model, snap(state.top)).0 as f32 * LINE_HEIGHT;
        self.scroll_x = state.scroll_x.max(0.);
        self.preferred_x = None;
        self.reveal_caret = false;
        self.preserve_scroll_on_layout = true;
        self.sync_view_position(cx);
        cx.notify();
    }

    #[cfg(test)]
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
        let model = self.model.read(cx);
        (self.anchor_line, self.anchor_col) = flat_byte_to_position(model, range.start_byte_offset);
        (self.cursor_line, self.cursor_col) = flat_byte_to_position(model, range.end_byte_offset);
        self.has_selection = range.start_byte_offset != range.end_byte_offset;
        self.focus.focus(window);
        self.finish_position_change(cx);
    }

    /// Build an editor presenting `model`.
    pub fn new(model: Entity<BufferModel>, cx: &mut Context<Self>) -> Self {
        Self::with_rendering(model, 0, EditorRenderingOptions::default(), cx)
    }

    #[cfg(test)]
    pub(crate) fn new_with_options(
        model: Entity<BufferModel>,
        element_id: usize,
        rendering: EditorRenderingOptions,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_rendering(model, element_id, rendering, cx)
    }

    fn with_rendering(
        model: Entity<BufferModel>,
        element_id: usize,
        rendering: EditorRenderingOptions,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.default_global::<Theme>();
        let (line_count, seen_revision, (decorations, gutter_markers, contribution_actions)) = {
            let model = model.read(cx);
            (
                model.line_count(),
                model.revision(),
                project_contributions(model, model.resolved_contributions()),
            )
        };

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
            if this.seen_revision != model.revision() {
                this.text_changed(model);
                this.marked_range_utf16 = None;
                this.preferred_x = None;
            }
            this.refresh(model, model.resolve_view_position(this.position_range));
            if let Some(restore) = model.history_view_restore() {
                this.apply_history_view_restore(model, restore.generation, &restore.state);
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
            line_count,
            seen_revision,
            max_line_width: 0.,
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
            preserve_scroll_on_layout: false,
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

    /// Record that the model text changed: shaped widths no longer apply.
    fn text_changed(&mut self, model: &BufferModel) {
        self.seen_revision = model.revision();
        self.max_line_width = 0.;
    }

    /// Refresh state derived from the model and, when given, move the
    /// selection to the model's anchored view position.
    fn refresh(&mut self, model: &BufferModel, position: Option<Range<usize>>) {
        self.line_count = model.line_count();
        (
            self.decorations,
            self.gutter_markers,
            self.contribution_actions,
        ) = project_contributions(model, model.resolved_contributions());
        if let Some(position) = position {
            self.restore_view_position(model, position);
        }
    }

    fn sync_view_position(&mut self, cx: &mut Context<Self>) {
        let state = self.history_view_state(self.model.read(cx));
        let (anchor, caret) = (state.anchor, state.caret);
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

    fn restore_view_position(&mut self, model: &BufferModel, range: Range<usize>) {
        let start = flat_byte_to_position(model, range.start);
        let end = flat_byte_to_position(model, range.end);
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

    fn history_view_state(&self, model: &BufferModel) -> HistoryViewState {
        let caret = to_flat_byte(model, self.cursor_line, self.cursor_col);
        let anchor = if self.has_selection {
            to_flat_byte(model, self.anchor_line, self.anchor_col)
        } else {
            caret
        };
        HistoryViewState {
            context: self.history_context,
            anchor,
            caret,
        }
    }

    fn apply_history_view_restore(
        &mut self,
        model: &BufferModel,
        generation: u64,
        state: &HistoryViewState,
    ) {
        if generation <= self.last_history_restore_generation {
            return;
        }
        self.last_history_restore_generation = generation;
        if state.context != self.history_context {
            return;
        }
        (self.anchor_line, self.anchor_col) = flat_byte_to_position(model, state.anchor);
        (self.cursor_line, self.cursor_col) = flat_byte_to_position(model, state.caret);
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
        let max = ((self.line_count as f32) * LINE_HEIGHT - self.viewport_h).max(0.);
        if self.scroll > max {
            self.scroll = max;
        }
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

    /// Commit a replacement to the authoritative model and refresh derived
    /// state before resolving the resulting caret.
    fn splice(
        &mut self,
        range: Range<usize>,
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
                    range,
                    text,
                    EditGrouping {
                        context: self.history_context,
                        kind,
                    },
                    view,
                )
            } else {
                model.replace_from_view(range, text, view)
            }
            .unwrap_or(false);
            if changed {
                cx.notify();
            }
        });
        let model = model.read(cx);
        self.text_changed(model);
        self.refresh(model, None);
    }

    /// The byte range replaced by input that names no range: the marked text
    /// if there is one, otherwise the selection, otherwise the caret.
    fn implicit_replacement_range(&self, model: &BufferModel) -> Range<usize> {
        if let Some(marked) = &self.marked_range_utf16 {
            return model.offset_of_utf16(marked.start)..model.offset_of_utf16(marked.end);
        }
        let state = self.history_view_state(model);
        state.anchor.min(state.caret)..state.anchor.max(state.caret)
    }

    /// Replace a byte range, or the implicit replacement range, with `text`,
    /// leaving the caret after it. Newlines take the buffer's line ending.
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
        let model = self.model.read(cx);
        let text = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\n', model.line_ending());
        let range = range.unwrap_or_else(|| self.implicit_replacement_range(model));
        let before = self.history_view_state(model);
        let after_offset = range.start + text.len();
        let after = HistoryViewState {
            context: self.history_context,
            anchor: after_offset,
            caret: after_offset,
        };
        self.splice(
            range,
            &text,
            grouping,
            HistoryViewChange { before, after },
            cx,
        );
        self.marked_range_utf16 = None;
        (self.cursor_line, self.cursor_col) =
            flat_byte_to_position(self.model.read(cx), after_offset);
        self.has_selection = false;
        self.finish_position_change(cx);
    }

    /// Convert an optional platform UTF-16 range to bytes.
    fn bytes_for_utf16(&self, range: Option<Range<usize>>, cx: &App) -> Option<Range<usize>> {
        let model = self.model.read(cx);
        range.map(|range| model.offset_of_utf16(range.start)..model.offset_of_utf16(range.end))
    }
}

fn project_contributions(
    model: &BufferModel,
    contributions: Vec<ResolvedEditorContribution>,
) -> (
    Vec<RenderedDecoration>,
    Vec<RenderedGutterMarker>,
    Vec<RenderedContributionAction>,
) {
    let mut decorations = Vec::new();
    let mut gutter_markers = Vec::new();
    let mut actions = Vec::new();
    for contribution in contributions {
        let start = contribution.range.start_byte_offset;
        let end = contribution.range.end_byte_offset;
        let first_line = model.line_of_offset(start);
        let last_line = model.line_of_offset(end);

        if let Some(token) = contribution.gutter {
            gutter_markers.push(RenderedGutterMarker {
                line: first_line,
                highlight: gutter_highlight(token),
            });
        }

        if contribution.decoration.is_none() && contribution.command.is_none() {
            continue;
        }

        for line in first_line..=last_line {
            let Range {
                start: line_start,
                end: line_end,
            } = model.line_range(line);
            let segment_start = start.max(line_start);
            let segment_end = end.min(line_end);

            if let Some(token) = contribution.decoration
                && segment_start < segment_end
            {
                decorations.push(RenderedDecoration {
                    line,
                    start: segment_start - line_start,
                    end: segment_end - line_start,
                    token,
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

fn decoration_highlight(token: DecorationToken) -> Highlight {
    Highlight::named(match token {
        DecorationToken::Info => "diagnostic.info",
        DecorationToken::Warning => "diagnostic.warning",
        DecorationToken::Error => "diagnostic.error",
    })
}

fn gutter_highlight(token: GutterToken) -> Highlight {
    Highlight::named(match token {
        GutterToken::Info => "diagnostic.info",
        GutterToken::Warning => "diagnostic.warning",
        GutterToken::Error => "diagnostic.error",
    })
}

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<EditorContributionAction> for EditorView {}

const LINE_HEIGHT: f32 = 20.0;
const GUTTER_WIDTH: f32 = 16.0;

#[cfg(test)]
mod tests;
