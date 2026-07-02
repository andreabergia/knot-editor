// Step 4 — gpui editor widget (render stage).
//
// A custom `Element` rendering fixture lines with multi-attribute styled
// text via `WindowTextSystem::shape_line` + `ShapedLine::paint`, clipped by
// a `with_content_mask` scroll viewport. Scroll-wheel mutates a pixel scroll
// offset on the view; the element recomputes the visible line range each
// paint from scroll + its allocated bounds.
//
// This stage covers roadmap item 1 (styled text) + item 2 (keyboard-driven
// scroll + cursor movement). Selection/IME/annotation come later.
//
// Cursor model: a single caret stored as `(line, byte_col)` into the owned
// `lines` buffer, plus a `preferred_col` used to keep a stable column when
// moving vertically across lines of differing widths. The caret is painted
// as a 2px bar at the shaped line's `x_for_index(col)` when the editor's
// focus handle is the window's focused handle. Keyboard movement clamps to
// utf-8 char boundaries and, after each move, re-scrolls so the caret stays
// inside the viewport.

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
    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.to_lowercase();
        let cmd = ev.keystroke.modifiers.platform;
        let last_line = self.lines.len().saturating_sub(1);
        let handled = match (cmd, key.as_str()) {
            (true, "left") => {
                self.cursor_col = 0;
                self.preferred_col = 0;
                true
            }
            (true, "right") => {
                self.cursor_col = self.line_end(self.cursor_line);
                self.preferred_col = self.cursor_col;
                true
            }
            (false, "left") => {
                if self.cursor_col > 0 {
                    self.cursor_col = self.prev_boundary(self.cursor_line, self.cursor_col);
                } else if self.cursor_line > 0 {
                    self.cursor_line -= 1;
                    self.cursor_col = self.line_end(self.cursor_line);
                }
                self.preferred_col = self.cursor_col;
                true
            }
            (false, "right") => {
                if self.cursor_col < self.line_end(self.cursor_line) {
                    self.cursor_col = self.next_boundary(self.cursor_line, self.cursor_col);
                } else if self.cursor_line < last_line {
                    self.cursor_line += 1;
                    self.cursor_col = 0;
                }
                self.preferred_col = self.cursor_col;
                true
            }
            (false, "up") => {
                if self.cursor_line > 0 {
                    self.cursor_line -= 1;
                    self.cursor_col = self.clamp_col_to_line(self.cursor_line, self.preferred_col);
                }
                true
            }
            (false, "down") => {
                if self.cursor_line < last_line {
                    self.cursor_line += 1;
                    self.cursor_col = self.clamp_col_to_line(self.cursor_line, self.preferred_col);
                }
                true
            }
            (false, "home") => {
                self.cursor_col = 0;
                self.preferred_col = 0;
                true
            }
            (false, "end") => {
                self.cursor_col = self.line_end(self.cursor_line);
                self.preferred_col = self.cursor_col;
                true
            }
            (false, "pageup") => {
                let rows = self.page_rows(window);
                self.cursor_line = self.cursor_line.saturating_sub(rows).min(last_line);
                self.cursor_col = self.clamp_col_to_line(self.cursor_line, self.preferred_col);
                self.scroll = (self.scroll - rows as f32 * LINE_HEIGHT).max(0.);
                true
            }
            (false, "pagedown") => {
                let rows = self.page_rows(window);
                self.cursor_line = (self.cursor_line + rows).min(last_line);
                self.cursor_col = self.clamp_col_to_line(self.cursor_line, self.preferred_col);
                self.scroll += rows as f32 * LINE_HEIGHT;
                true
            }
            _ => false,
        };
        if handled {
            self.ensure_cursor_visible(self.viewport_h);
            self.clamp_scroll();
            cx.stop_propagation();
            cx.notify();
        }
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
    /// rather than a guessed byte offset.
    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let p = ev.position;
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
        self.cursor_line = line;
        self.cursor_col = col;
        self.preferred_col = col;
        self.ensure_cursor_visible(self.viewport_h);
        self.clamp_scroll();
        cx.notify();
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
            focus_handle,
        ): (
            f32,
            f32,
            Vec<(usize, String, Vec<Seg>)>,
            Option<(usize, usize)>,
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
            (view.scroll, max, vis, caret, view.focus.clone())
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
            for (ix, line, row) in &vis {
                if line.is_empty() {
                    continue;
                }
                let runs = runs_for(line, row);
                let shaped = window
                    .text_system()
                    .shape_line(SharedString::from(line.clone()), font_size, &runs, None);
                let origin = point(
                    bounds.origin.x,
                    bounds.origin.y + px(*ix as f32 * LINE_HEIGHT) - px(scroll),
                );
                let _ = shaped.paint(origin, line_height, window, cx);
            }

            // Caret: a 2px vertical bar at the shaped line's
            // `x_for_index(cursor_col)`, painted only when the editor holds
            // window focus. (No blink yet — IME/selection stages get a timer.)
            if focused {
                if let Some((line_ix, col)) = caret {
                    let caret_x = if let Some((_, line, row)) =
                        vis.iter().find(|(ix, _, _)| *ix == line_ix)
                    {
                        if line.is_empty() {
                            px(0.)
                        } else {
                            let runs = runs_for(line, row);
                            let shaped = window.text_system().shape_line(
                                SharedString::from(line.clone()),
                                font_size,
                                &runs,
                                None,
                            );
                            shaped.x_for_index(col)
                        }
                    } else {
                        px(0.)
                    };
                    // Re-shape the cursor line is cheap; fall back to 0 for
                    // empty line. Caret rect spans the full line height.
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