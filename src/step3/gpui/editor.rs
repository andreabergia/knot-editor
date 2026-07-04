// Step 4 — gpui editor widget (render stage).
//
// A custom `Element` rendering fixture lines with multi-attribute styled
// text via `WindowTextSystem::shape_line` + `ShapedLine::paint`, clipped by
// a `with_content_mask` scroll viewport. Scroll-wheel mutates a pixel scroll
// offset on the view; the element recomputes the visible line range each
// paint from scroll + its allocated bounds.
//
// This stage covers roadmap item 1 (styled text) + item 2 (keyboard-driven
// scroll + cursor movement) + item 3 (selection: mouse drag + shift-extend +
// shift-arrow). IME/annotation come later.
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

use gpui::{prelude::*, *};

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

const DEFAULT_COLOR: u32 = 0xC0C0C0;

pub struct EditorView {
    lines: Vec<String>,
    segs: Vec<Vec<Seg>>,
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
    focus: FocusHandle,
}

impl EditorView {
    /// Build an editor preloaded with a fixture, as step 2's bench backends do.
    pub fn from_fixture(fixture: &::knot::view::fixture::Fixture, cx: &mut App) -> Self {
        let n = fixture.line_count();
        let mut lines = Vec::with_capacity(n);
        let mut segs = Vec::with_capacity(n);
        for i in 0..n {
            let line = fixture
                .lines
                .get(i)
                .cloned()
                .unwrap_or_default();
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
        Self {
            lines,
            segs,
            scroll: 0.,
            cursor_line: 0,
            cursor_col: 0,
            preferred_col: 0,
            viewport_h: 0.,
            bounds: Bounds::default(),
            anchor_line: 0,
            anchor_col: 0,
            has_selection: false,
            focus: cx.focus_handle(),
        }
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

    /// Previous utf-8 char boundary before `col` on `line`; 0 if already 0.
    fn prev_boundary(&self, line: usize, col: usize) -> usize {
        let s = match self.lines.get(line) {
            Some(s) => s.as_bytes(),
            None => return 0,
        };
        if col == 0 || col > s.len() {
            return 0;
        }
        let mut i = col - 1;
        while i > 0 && (s[i] & 0xC0) == 0x80 {
            i -= 1;
        }
        i
    }

    /// Next utf-8 char boundary after `col` on `line`; line end if at end.
    fn next_boundary(&self, line: usize, col: usize) -> usize {
        let s = match self.lines.get(line) {
            Some(s) => s.as_bytes(),
            None => return 0,
        };
        let n = s.len();
        if col >= n {
            return n;
        }
        let mut i = col + 1;
        while i < n && (s[i] & 0xC0) == 0x80 {
            i += 1;
        }
        i
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

        // Compute the motion's target caret, its direction (forward = toward
        // end of buffer, used for collapse-tiebreak), whether it's a
        // horizontal motion (which resets `preferred_col` to the new col),
        // and a scroll delta (pageup/pagedown nudge the viewport too).
        let (target, forward, horizontal, scroll_delta): (
            (usize, usize),
            bool,
            bool,
            f32,
        ) = match (cmd, key.as_str()) {
            (true, "left") => ((self.cursor_line, 0), false, true, 0.),
            (true, "right") => {
                ((self.cursor_line, self.line_end(self.cursor_line)), true, true, 0.)
            }
            (false, "left") => {
                let (l, c) = if self.cursor_col > 0 {
                    (self.cursor_line, self.prev_boundary(self.cursor_line, self.cursor_col))
                } else if self.cursor_line > 0 {
                    (self.cursor_line - 1, self.line_end(self.cursor_line - 1))
                } else {
                    (self.cursor_line, 0)
                };
                ((l, c), false, true, 0.)
            }
            (false, "right") => {
                let (l, c) = if self.cursor_col < self.line_end(self.cursor_line) {
                    (self.cursor_line, self.next_boundary(self.cursor_line, self.cursor_col))
                } else if self.cursor_line < last_line {
                    (self.cursor_line + 1, 0)
                } else {
                    (self.cursor_line, self.cursor_col)
                };
                ((l, c), true, true, 0.)
            }
            (false, "up") => {
                let l = if self.cursor_line > 0 { self.cursor_line - 1 } else { self.cursor_line };
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
            (false, "end") => {
                ((self.cursor_line, self.line_end(self.cursor_line)), true, true, 0.)
            }
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

        self.ensure_cursor_visible(self.viewport_h);
        self.clamp_scroll();
        cx.stop_propagation();
        cx.notify();
    }

    /// Convert `preferred_col` to a valid byte column on `line`, never
    /// exceeding the line's byte length. No shaping here — vertical movement
    /// keeps the byte column stable; the next paint places the caret. (For
    /// proportional fonts this is approximate; correct per-char x would
    /// require shaping the target line, deferred to selection.)
    fn clamp_col_to_line(&self, line: usize, col: usize) -> usize {
        col.min(self.line_end(line))
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
        self.ensure_cursor_visible(self.viewport_h);
        self.clamp_scroll();
        cx.notify();
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
        self.ensure_cursor_visible(self.viewport_h);
        self.clamp_scroll();
        cx.notify();
    }

    /// Map a window-space point to a (line, byte_col) caret position using the
    /// last paint's `bounds`. Line is clamped to the last line; column comes
    /// from `closest_index_for_x` on the shaped line, falling back to 0 for
    /// empty lines.
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
            shaped.closest_index_for_x(p.x - origin.x)
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
        if a <= c {
            Some((a, c))
        } else {
            Some((c, a))
        }
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        div()
            .id("editor")
            .size_full()
            .bg(rgb(0x1e1e1e))
            // Track focus so clicking the pane focuses this view (gpui
            // auto-focuses a tracked element on mouse-down) and so on_key_down
            // listeners below actually receive keystrokes.
            .track_focus(&self.focus)
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
            focus_handle,
        ): (
            f32,
            f32,
            Vec<(usize, String, Vec<Seg>)>,
            Option<(usize, usize)>,
            Option<((usize, usize), (usize, usize))>,
            FocusHandle,
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
            (
                view.scroll,
                max,
                vis,
                caret,
                view.selection_range(),
                view.focus.clone(),
            )
        };

        // Write the measured viewport height back into the view so the key
        // and scroll handlers can clamp against the real viewport. No
        // notify: this must not trigger a re-render.
        let viewport_h = f32::from(bounds.size.height);
        self.entity.update(cx, |view, _cx| {
            view.viewport_h = viewport_h;
            view.bounds = bounds;
        });

        let font_size = px(FONT_SIZE);
        let line_height = px(LINE_HEIGHT);
        let focused = focus_handle.is_focused(window);

        // Clip to viewport so paint below the last line / outside horizontal
        // extent doesn't bleed into neighbouring panes.
        let mask = Some(ContentMask { bounds });
        window.with_content_mask(mask, |window| {
            // Shape each visible non-empty line once and reuse the
            // `ShapedLine` for both the selection highlight and the text
            // paint, so we don't pay for shaping twice per frame.
            let shaped: Vec<(usize, ShapedLine)> = vis
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
                    Some((*ix, s))
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
                for (ix, s) in &shaped {
                    if *ix < start.0 || *ix > end.0 {
                        continue;
                    }
                    let (rx, rw) = if start.0 == end.0 {
                        let x0 = s.x_for_index(start.1);
                        let x1 = s.x_for_index(end.1);
                        (x0, x1 - x0)
                    } else if *ix == start.0 {
                        let x0 = s.x_for_index(start.1);
                        (x0, pane_w - x0)
                    } else if *ix == end.0 {
                        let x1 = s.x_for_index(end.1);
                        (px(0.), x1)
                    } else {
                        (px(0.), pane_w)
                    };
                    let top = bounds.origin.y + px(*ix as f32 * LINE_HEIGHT) - px(scroll);
                    let rect = Bounds {
                        origin: point(bounds.origin.x + rx, top),
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
            for (ix, s) in &shaped {
                let origin = point(
                    bounds.origin.x,
                    bounds.origin.y + px(*ix as f32 * LINE_HEIGHT) - px(scroll),
                );
                let _ = s.paint(origin, line_height, window, cx);
            }

            // Caret: a 2px vertical bar at the shaped line's
            // `x_for_index(cursor_col)`, painted only when the editor holds
            // window focus AND there is no active selection (macOS hides the
            // caret while a selection is drag-held). No blink yet —
            // IME/selection stages get a timer.
            if focused && selection.is_none() {
                if let Some((line_ix, col)) = caret {
                    let caret_x = shaped
                        .iter()
                        .find(|(ix, _)| *ix == line_ix)
                        .map(|(_, s)| s.x_for_index(col))
                        .unwrap_or(px(0.));
                    let top = bounds.origin.y + px(line_ix as f32 * LINE_HEIGHT) - px(scroll);
                    let caret_bounds = Bounds {
                        origin: point(bounds.origin.x + caret_x, top),
                        size: size(px(2.), line_height),
                    };
                    let _ = window.paint_quad(fill(caret_bounds, hsla(0., 0., 0.9, 1.0)));
                }
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
                origin: point(
                    bounds.origin.x + bounds.size.width - px(track),
                    px(thumb_y),
                ),
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
    font
}

const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 20.0;