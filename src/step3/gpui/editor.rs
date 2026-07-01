// Step 4 — gpui editor widget (render stage).
//
// A custom `Element` rendering fixture lines with multi-attribute styled
// text via `WindowTextSystem::shape_line` + `ShapedLine::paint`, clipped by
// a `with_content_mask` scroll viewport. Scroll-wheel mutates a pixel scroll
// offset on the view; the element recomputes the visible line range each
// paint from scroll + its allocated bounds.
//
// This stage covers roadmap item 1 (styled text) + the scroll half of
// item 2. Cursor/selection/keyboard/IME/annotation come later.

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
            let mut row = Vec::with_capacity(specs.len());
            let mut cursor = 0usize;
            for seg in specs {
                let start = cursor;
                let end = start + seg.text.len();
                cursor = end;
                row.push(Seg {
                    start,
                    end,
                    color: seg.color,
                    bold: seg.bold,
                    italic: seg.italic,
                });
            }
            if row.is_empty() {
                let len = lines[i].len();
                row.push(Seg {
                    start: 0,
                    end: len,
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
            focus: cx.focus_handle(),
        }
    }

    fn clamp_scroll(&mut self, max: f32) {
        if self.scroll < 0. {
            self.scroll = 0.;
        }
        if self.scroll > max {
            self.scroll = max;
        }
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        div()
            .id("editor")
            .size_full()
            .bg(rgb(0x1e1e1e))
            // Scroll wheel: accumulate pixel delta, clamp on notify. We don't
            // know the max scroll yet (depends on allocated height); the clamp
            // is applied inside the element paint by re-reading state — but
            // paint can't mutate. Instead clamp loosely here against a large
            // bound derived from line count; the element additionally stops
            // painting past the last line, which is what the user sees.
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _window, cx| {
                let dy = match ev.delta {
                    ScrollDelta::Pixels(p) => f32::from(p.y),
                    ScrollDelta::Lines(p) => p.y * LINE_HEIGHT,
                };
                // macOS "natural" scroll: trackpad two-finger gesture down
                // moves the content up, i.e. the viewport descends — scroll
                // increases. ScrollWheelEvent delta is positive for swipes
                // upward (toward content top), so we invert it here.
                this.scroll -= dy;
                let max = (this.lines.len() as f32) * LINE_HEIGHT;
                this.clamp_scroll(max);
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
        // small String copies.
        let (scroll, scroll_max, vis): (f32, f32, Vec<(usize, String, Vec<Seg>)>) = {
            let view = self.entity.read(cx);
            let max = (view.lines.len() as f32) * LINE_HEIGHT;
            let first = (view.scroll / LINE_HEIGHT).floor() as usize;
            let visible_rows = (f32::from(bounds.size.height) / LINE_HEIGHT).ceil() as usize + 1;
            let last = (first + visible_rows).min(view.lines.len());
            let vis = (first..last)
                .map(|ix| (ix, view.lines[ix].clone(), view.segs[ix].clone()))
                .collect();
            (view.scroll, max, vis)
        };

        let font_size = px(FONT_SIZE);
        let line_height = px(LINE_HEIGHT);
        let ascent = line_height; // approximate baseline; exact ascent available via LineLayout for later stages

        // Clip to viewport so paint below the last line / outside horizontal
        // extent doesn't bleed into neighbouring panes.
        let mask = Some(ContentMask { bounds });
        window.with_content_mask(mask, |window| {
            for (ix, line, row) in vis {
                if line.is_empty() {
                    continue;
                }
                let runs = runs_for(&line, &row);
                let shaped = window
                    .text_system()
                    .shape_line(SharedString::from(line), font_size, &runs, None);
                let origin = point(
                    bounds.origin.x,
                    bounds.origin.y + px(ix as f32 * LINE_HEIGHT) - px(scroll) + ascent,
                );
                let _ = shaped.paint(origin, line_height, window, cx);
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