//! Window-local tab drag geometry, feedback, and release dispatch.

use super::relocate::{TabRelocationDestination, TabRelocationSource};
use super::*;
use crate::app::workbench::{SplitEdge, TabMoveCapture, TabMoveDestination, TabMovePlacement};

#[derive(Default)]
pub(super) struct TabDragState {
    source: Option<TabRelocationSource>,
    panes: Vec<PaneGeometry>,
    preview: Option<DropTarget>,
}

struct PaneGeometry {
    pane: PaneId,
    bounds: Bounds<Pixels>,
    tabs: Vec<(TabMoveCapture, Bounds<Pixels>)>,
    bar: Option<Bounds<Pixels>>,
}

#[derive(Clone, Copy)]
struct DropTarget {
    tab: TabMoveDestination,
    bounds: Bounds<Pixels>,
}

impl TabDragState {
    fn target_at(&self, position: Point<Pixels>) -> Option<DropTarget> {
        let pane = self
            .panes
            .iter()
            .find(|pane| pane.bounds.contains(&position))?;
        let &(first, _) = pane.tabs.first()?;
        let bar = pane.bar?;
        let bounds = pane.bounds;
        if position.y < bar.bottom() {
            let (anchor, placement, x) = pane
                .tabs
                .iter()
                .find(|(_, tab)| position.x < tab.center().x)
                .map(|(anchor, tab)| (*anchor, TabMovePlacement::Before, tab.left()))
                .unwrap_or_else(|| {
                    let (anchor, tab) = pane.tabs.last().unwrap();
                    (
                        *anchor,
                        TabMovePlacement::End,
                        tab.right().min(bounds.right()),
                    )
                });
            return Some(DropTarget {
                tab: TabMoveDestination::At { anchor, placement },
                bounds: Bounds::new(point(x - px(1.), bar.top()), size(px(2.), bar.size.height)),
            });
        }
        let body = Bounds::new(
            point(bounds.left(), bar.bottom()),
            size(bounds.size.width, bounds.bottom() - bar.bottom()),
        );
        if body.size.width <= px(0.) || body.size.height <= px(0.) {
            return None;
        }
        let x = (position.x - body.left()) / body.size.width;
        let y = (position.y - body.top()) / body.size.height;
        let (distance, edge) = [
            (x, SplitEdge::Left),
            (1. - x, SplitEdge::Right),
            (y, SplitEdge::Top),
            (1. - y, SplitEdge::Bottom),
        ]
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))?;
        let mut preview = body;
        let placement = if distance < 0.22 {
            match edge {
                SplitEdge::Left => preview.size.width /= 2.,
                SplitEdge::Right => {
                    preview.size.width /= 2.;
                    preview.origin.x += preview.size.width;
                }
                SplitEdge::Top => preview.size.height /= 2.,
                SplitEdge::Bottom => {
                    preview.size.height /= 2.;
                    preview.origin.y += preview.size.height;
                }
            }
            TabMovePlacement::Split(edge)
        } else {
            TabMovePlacement::End
        };
        Some(DropTarget {
            tab: TabMoveDestination::At {
                anchor: first,
                placement,
            },
            bounds: preview,
        })
    }

    pub(super) fn begin_frame(&mut self) {
        self.panes.clear();
    }

    fn clear(&mut self) {
        self.source = None;
        self.preview = None;
    }
}

pub(super) struct TabDragLabel(SharedString);
impl Render for TabDragLabel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .text_sm()
            .bg(rgb(0x405e90))
            .text_color(rgb(0xffffff))
            .child(self.0.clone())
    }
}

impl ProductShell {
    pub(super) fn tab_drag_label(
        &self,
        title: SharedString,
    ) -> impl Fn(&TabRelocationSource, Point<Pixels>, &mut Window, &mut App) -> Entity<TabDragLabel>
    + use<> {
        let state = self.tab_drag.clone();
        move |source, _, _, cx| {
            let mut state = state.borrow_mut();
            state.source = Some(source.clone());
            state.preview = None;
            cx.new(|_| TabDragLabel(title.clone()))
        }
    }

    pub(super) fn pane_drag_geometry(&self, pane: PaneId) -> impl IntoElement + use<> {
        let state = self.tab_drag.clone();
        canvas(
            move |bounds, _, _| {
                state.borrow_mut().panes.push(PaneGeometry {
                    pane,
                    bounds,
                    tabs: Vec::new(),
                    bar: None,
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    pub(super) fn tab_bar_drag_geometry(&self, pane: PaneId) -> impl IntoElement + use<> {
        let state = self.tab_drag.clone();
        canvas(
            move |bounds, _, _| {
                if let Some(pane) = state
                    .borrow_mut()
                    .panes
                    .iter_mut()
                    .find(|geometry| geometry.pane == pane)
                {
                    pane.bar = Some(bounds);
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    pub(super) fn tab_drag_geometry(
        &self,
        pane: PaneId,
        tab: TabMoveCapture,
    ) -> impl IntoElement + use<> {
        let state = self.tab_drag.clone();
        canvas(
            move |bounds, _, _| {
                if let Some(pane) = state
                    .borrow_mut()
                    .panes
                    .iter_mut()
                    .find(|geometry| geometry.pane == pane)
                {
                    pane.tabs.push((tab, bounds));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    pub(super) fn cancel_tab_drag(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" && self.tab_drag.borrow().source.is_some() {
            self.tab_drag.borrow_mut().clear();
            cx.stop_active_drag(window);
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(super) fn drag_listener(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let state = self.tab_drag.clone();
        let shell = cx.entity().downgrade();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                if let Some(preview) = state.borrow().preview {
                    window.paint_quad(fill(preview.bounds, rgba(0x7fb5ff55)));
                }
                let moves = state.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture
                        && cx.has_active_drag()
                        && moves.borrow().source.is_some()
                    {
                        let next = moves.borrow().target_at(event.position);
                        moves.borrow_mut().preview = next;
                        cx.refresh_windows();
                    }
                });
                let releases = state.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                        return;
                    }
                    let source = releases.borrow_mut().source.take();
                    if let Some(source) = source {
                        let target = releases.borrow().target_at(event.position);
                        releases.borrow_mut().clear();
                        if cx.has_active_drag() {
                            cx.stop_active_drag(window);
                            if let (Some(target), Some(shell)) = (target, shell.upgrade()) {
                                shell.update(cx, |shell, cx| {
                                    let destination = TabRelocationDestination {
                                        window: window.window_handle(),
                                        workbench: shell.workbench.downgrade(),
                                        tab: target.tab,
                                    };
                                    shell.relocate_tab_to(&source, &destination, window, cx);
                                });
                            }
                            cx.stop_propagation();
                        }
                        cx.refresh_windows();
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

#[cfg(test)]
mod tests;
