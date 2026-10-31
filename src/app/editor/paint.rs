//! The editor element: layout, viewport-culled painting, and IME registration.

use super::*;

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        if focused != self.history_was_focused {
            self.break_history_group(cx);
            self.history_was_focused = focused;
        }
        self.update_caret_blink(focused, cx);
        let entity = cx.entity();
        let mut key_context = KeyContext::default();
        key_context.add(EDITOR_KEY_CONTEXT);
        if self.completion.is_some() {
            key_context.add("editor_completion");
        }
        let completion = self.completion_surface.as_ref().map(|surface| {
            let anchor = self.completion_anchor(self.model.read(cx), window, cx);
            surface.render(anchor, theme(cx))
        });
        div()
            .id(("editor", self.element_id))
            .size_full()
            .bg(theme(cx).color(color::EDITOR_BACKGROUND))
            // Track focus so clicking the pane focuses this view (gpui
            // auto-focuses a tracked element on mouse-down) and so on_key_down
            // listeners below actually receive keystrokes.
            .track_focus(&self.focus)
            .key_context(key_context)
            .on_action(cx.listener(Self::completion_previous))
            .on_action(cx.listener(Self::completion_next))
            .on_action(cx.listener(Self::completion_accept))
            .on_action(cx.listener(Self::completion_dismiss))
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
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _, cx| {
                let (dx, dy) = match ev.delta {
                    ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
                    ScrollDelta::Lines(p) => (p.x * LINE_HEIGHT, p.y * LINE_HEIGHT),
                };
                this.scroll -= dy;
                this.scroll_x = (this.scroll_x - dx).clamp(
                    0.,
                    (this.max_line_width + 2. - f32::from(this.text_bounds().size.width)).max(0.),
                );
                this.reveal_caret = false;
                this.clamp_scroll();
                cx.notify();
            }))
            .child(EditorElement { entity })
            .children(completion)
    }
}
/// The custom `Element`. Owns no state itself; reads everything from the
/// `EditorView` entity each layout/paint. Implements `Element` directly
/// (not `RenderOnce`) per the `Element` module doc's recommendation for
/// code editors.
pub(super) struct EditorElement {
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
        self.entity.update(cx, |view, cx| {
            let resized = view.bounds.size != bounds.size;
            view.bounds = bounds;
            view.viewport_h = f32::from(bounds.size.height);
            if view.reveal_caret || (resized && !view.preserve_scroll_on_layout) {
                view.ensure_cursor_visible(view.viewport_h);
                let x = f32::from(view.caret_x(view.model.read(cx), window, cx));
                let width = f32::from(view.text_bounds().size.width);
                if x < view.scroll_x {
                    view.scroll_x = x.max(0.);
                }
                if x + 2. > view.scroll_x + width {
                    view.scroll_x = (x + 2. - width).max(0.);
                }
                view.reveal_caret = false;
            }
            view.preserve_scroll_on_layout = false;
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
        // Read the view state, compute the visible range, then read just the
        // visible lines and their runs out of the model before painting:
        // `ShapedLine::paint` takes `cx: &mut App`, but `entity.read(cx)`
        // borrows `cx: &App` for as long as the borrow lives. We also pull the
        // caret, focus handle, and theme colors so we can paint without
        // re-borrowing.
        let (
            scroll,
            scroll_max,
            vis,
            tab_size,
            caret,
            selection,
            marked,
            gutter_markers,
            colors,
            focus_handle,
            entity,
        ): EditorPaintState = {
            let view = self.entity.read(cx);
            let model = view.model.read(cx);
            let theme = theme(cx);
            let max = (view.line_count as f32) * LINE_HEIGHT;
            let Range {
                start: first,
                end: last,
            } = view.visible_lines();
            view.cache_syntax(model, first..last);
            let vis = (first..last)
                .map(|ix| {
                    let text = line_text(model, ix);
                    let runs = view.line_runs(model, ix, &text, cx);
                    (ix, text, runs)
                })
                .collect();
            // Only paint the caret when it's shown in its blink cycle and on a
            // visible line; otherwise it is clipped anyway, so skip the
            // shaping cost.
            let caret =
                if view.caret_blink.visible && view.cursor_line >= first && view.cursor_line < last
                {
                    Some((view.cursor_line, view.cursor_col))
                } else {
                    None
                };
            // Convert marked UTF-16 range to (line, byte_col) start/end for
            // underline painting.
            let marked = view.marked_range_utf16.as_ref().map(|mr| {
                let s = flat_utf16_to_position(model, mr.start);
                let e = flat_utf16_to_position(model, mr.end);
                (s, e)
            });
            let gutter_markers = if view.rendering.show_gutter_markers {
                view.gutter_markers
                    .iter()
                    .filter(|marker| marker.line >= first && marker.line < last)
                    .map(|marker| (marker.line, theme.color(&marker.key)))
                    .collect()
            } else {
                Vec::new()
            };
            (
                view.scroll,
                max,
                vis,
                view.tab_size(model, cx),
                caret,
                view.selection_range(),
                marked,
                gutter_markers,
                PaintColors {
                    selection: theme.color(color::EDITOR_SELECTION),
                    caret: theme.color(color::EDITOR_CURSOR),
                    marked_text: theme.color(color::EDITOR_MARKED_TEXT),
                    scrollbar: theme.color(color::SCROLLBAR_THUMB),
                },
                view.focus.clone(),
                self.entity.clone(),
            )
        };

        // Write the measured viewport height back into the view so the key
        // and scroll handlers can clamp against the real viewport. No
        // notify: this must not trigger a re-render.
        let viewport_h = f32::from(bounds.size.height);
        let gutter = bounds;
        let bounds = self.entity.update(cx, |view, _cx| {
            view.viewport_h = viewport_h;
            view.bounds = gutter;
            #[cfg(test)]
            {
                view.paint_count += 1;
            }
            view.text_bounds()
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
        let widest = window.with_content_mask(mask, |window| {
            // Shape each visible non-empty line once and reuse the
            // `ShapedLine` for both the selection highlight and the text
            // paint, so we don't pay for shaping twice per frame. The third
            // tuple element is the per-line x offset (0 for LTR, or
            // `pane_w - shaped_w` for RTL so they right-align); the fourth
            // is a `&str` reference to the line text, passed to
            // `x_for_index_dir` for per-character direction detection.
            let shaped: Vec<(usize, ShapedLine, Pixels, &str)> = vis
                .iter()
                .filter_map(|(ix, line, runs)| {
                    if line.is_empty() {
                        return None;
                    }
                    let s = shape_editor_line(
                        window,
                        SharedString::from(line.clone()),
                        font_size,
                        runs,
                        tab_size,
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
                let sel_color = colors.selection;
                let pane_w = bounds.size.width;
                for (ix, s, x_off, line_str) in &shaped {
                    if *ix < start.0 || *ix > end.0 {
                        continue;
                    }
                    let rtl_base = is_rtl_line(line_str);
                    let (rx, rw) = if start.0 == end.0 {
                        let x0 = x_for_index_dir(s, start.1, line_str, tab_size);
                        let x1 = x_for_index_dir(s, end.1, line_str, tab_size);
                        let (lo, hi) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
                        (lo, hi - lo)
                    } else if *ix == start.0 {
                        let x0 = x_for_index_dir(s, start.1, line_str, tab_size);
                        if rtl_base {
                            (px(0.), x0.max(px(0.)))
                        } else {
                            (x0, pane_w - x0)
                        }
                    } else if *ix == end.0 {
                        let x1 = x_for_index_dir(s, end.1, line_str, tab_size);
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
                for (ix, line, _) in &vis {
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
                let mark_color = colors.marked_text;
                let underline_h = px(1.5);
                let pane_w = bounds.size.width;
                for (ix, s, x_off, line_str) in &shaped {
                    if *ix < start.0 || *ix > end.0 {
                        continue;
                    }
                    let rtl_base = is_rtl_line(line_str);
                    let (rx, rw) = if start.0 == end.0 {
                        let x0 = x_for_index_dir(s, start.1, line_str, tab_size);
                        let x1 = x_for_index_dir(s, end.1, line_str, tab_size);
                        let (lo, hi) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
                        (lo, (hi - lo).max(px(2.)))
                    } else if *ix == start.0 {
                        let x0 = x_for_index_dir(s, start.1, line_str, tab_size);
                        if rtl_base {
                            (px(0.), x0.max(px(2.)))
                        } else {
                            (x0, pane_w - x0)
                        }
                    } else if *ix == end.0 {
                        let x1 = x_for_index_dir(s, end.1, line_str, tab_size);
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
            // caret while a selection is drag-held). `caret` is `None`
            // during the hidden half of a blink.
            if focused
                && selection.is_none()
                && let Some((line_ix, col)) = caret
            {
                let (caret_x, x_off) = shaped
                    .iter()
                    .find(|(ix, _, _, _)| *ix == line_ix)
                    .map(|(_, s, x_off, line_str)| {
                        (x_for_index_dir(s, col, line_str, tab_size), *x_off)
                    })
                    .unwrap_or((px(0.), px(0.)));
                let top = bounds.origin.y + px(line_ix as f32 * LINE_HEIGHT) - px(scroll);
                let caret_bounds = Bounds {
                    origin: point(bounds.origin.x + x_off + caret_x, top),
                    size: size(px(2.), line_height),
                };
                window.paint_quad(fill(caret_bounds, colors.caret));
            }

            shaped
                .iter()
                .map(|(_, s, _, _)| f32::from(s.width))
                .fold(0., f32::max)
        });

        // Gutter markers, beside the text area rather than over it.
        window.with_content_mask(Some(ContentMask { bounds: gutter }), |window| {
            for (line, color) in &gutter_markers {
                let top = gutter.origin.y + px(*line as f32 * LINE_HEIGHT) - px(scroll);
                let marker_bounds = Bounds {
                    origin: point(gutter.origin.x + px(4.), top + px(6.)),
                    size: size(px(7.), px(7.)),
                };
                window.paint_quad(fill(marker_bounds, *color));
            }
        });
        self.entity.update(cx, |view, _| {
            view.max_line_width = view.max_line_width.max(widest);
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
            window.paint_quad(fill(thumb_bounds, colors.scrollbar));
        }
    }
}
pub(super) type EditorPaintState = (
    f32,
    f32,
    Vec<(usize, String, Vec<TextRun>)>,
    usize,
    Option<(usize, usize)>,
    Option<((usize, usize), (usize, usize))>,
    Option<((usize, usize), (usize, usize))>,
    Vec<(usize, Hsla)>,
    PaintColors,
    FocusHandle,
    Entity<EditorView>,
);

/// Theme colors copied out for one paint.
pub(super) struct PaintColors {
    selection: Hsla,
    caret: Hsla,
    marked_text: Hsla,
    scrollbar: Hsla,
}
