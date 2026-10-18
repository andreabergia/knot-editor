//! Foreground tab drag coordination across native product windows.

use super::relocate::{TabRelocationDestination, TabRelocationSource};
use super::*;
use crate::app::workbench::{
    DockLocation, SplitEdge, TabMoveCapture, TabMoveDestination, TabMovePlacement,
};
use std::rc::Rc;

/// Distance from the workbench frame edge that starts a dock drop target for
/// tool frames. Pane-edge splits keep handling the interior; only tools use
/// these whole-workbench strips so documents and terminals keep their
/// pane-local split behavior.
const WORKBENCH_EDGE_WIDTH: f32 = 20.;
const DOCK_PREVIEW_SIDE: f32 = 240.;
const DOCK_PREVIEW_BOTTOM: f32 = 220.;

#[derive(Default)]
pub(super) struct TabDragState {
    panes: Vec<PaneGeometry>,
    workbench: Option<Bounds<Pixels>>,
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
    fn workbench_dock_target(&self, position: Point<Pixels>) -> Option<DropTarget> {
        let bounds = self.workbench?;
        if !bounds.contains(&position) {
            return None;
        }
        let edge = WORKBENCH_EDGE_WIDTH;
        let left = position.x - bounds.left() < px(edge);
        let right = bounds.right() - position.x < px(edge);
        let bottom = bounds.bottom() - position.y < px(edge);
        let (dock, preview) = if left {
            (
                DockLocation::Left,
                Bounds::new(
                    point(bounds.left(), bounds.top()),
                    size(
                        px(DOCK_PREVIEW_SIDE).min(bounds.size.width),
                        bounds.size.height,
                    ),
                ),
            )
        } else if right {
            let width = px(DOCK_PREVIEW_SIDE).min(bounds.size.width);
            (
                DockLocation::Right,
                Bounds::new(
                    point(bounds.right() - width, bounds.top()),
                    size(width, bounds.size.height),
                ),
            )
        } else if bottom {
            let height = px(DOCK_PREVIEW_BOTTOM).min(bounds.size.height);
            (
                DockLocation::Bottom,
                Bounds::new(
                    point(bounds.left(), bounds.bottom() - height),
                    size(bounds.size.width, height),
                ),
            )
        } else {
            return None;
        };
        Some(DropTarget {
            tab: TabMoveDestination::Dock(dock),
            bounds: preview,
        })
    }

    fn target_at(&self, position: Point<Pixels>, is_tool: bool) -> Option<DropTarget> {
        if is_tool && let Some(dock) = self.workbench_dock_target(position) {
            return Some(dock);
        }
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
        self.workbench = None;
    }
}

struct ApplicationTabDrag(Rc<RefCell<TabDragCoordinator>>);
impl Global for ApplicationTabDrag {}

#[derive(Default)]
pub(super) struct TabDragCoordinator {
    windows: Vec<DragWindow>,
    active: Option<ActiveTabDrag>,
    pointer: Option<Point<Pixels>>,
    pointer_window: Option<AnyWindowHandle>,
}

struct DragWindow {
    window: AnyWindowHandle,
    shell: WeakEntity<ProductShell>,
    workbench: WeakEntity<Workbench>,
    state: Rc<RefCell<TabDragState>>,
    accepts_drop: bool,
}

#[derive(Clone)]
struct ActiveTabDrag {
    shell: WeakEntity<ProductShell>,
    window: AnyWindowHandle,
    source: TabRelocationSource,
}

enum DropAction {
    Target(TabRelocationDestination),
    TearOut(Point<Pixels>),
    Invalid,
}

struct WindowGeometry {
    window: AnyWindowHandle,
    frame: Bounds<Pixels>,
    content: Bounds<Pixels>,
}

fn content_bounds(window: &Window) -> Bounds<Pixels> {
    let frame = window.bounds();
    let content = window.viewport_size();
    Bounds::new(
        frame.origin + point(px(0.), frame.size.height - content.height),
        content,
    )
}

// Query current native bounds in platform stacking order. Geometry of the
// receiving window is read directly while its foreground update is borrowed.
fn window_geometries(receiver: &Window, cx: &mut App) -> Vec<WindowGeometry> {
    let stack = cx.window_stack().unwrap_or_else(|| {
        let mut windows = cx.windows();
        windows.retain(|handle| *handle != receiver.window_handle());
        windows.insert(0, receiver.window_handle());
        windows
    });
    stack
        .into_iter()
        .filter_map(|handle| {
            if handle == receiver.window_handle() {
                Some(WindowGeometry {
                    window: handle,
                    frame: receiver.bounds(),
                    content: content_bounds(receiver),
                })
            } else {
                cx.update_window(handle, |_, window, _| WindowGeometry {
                    window: handle,
                    frame: window.bounds(),
                    content: content_bounds(window),
                })
                .ok()
            }
        })
        .collect()
}

impl TabDragCoordinator {
    fn clear(&mut self) {
        self.active = None;
        self.pointer = None;
        self.pointer_window = None;
        for window in &self.windows {
            window.state.borrow_mut().preview = None;
        }
    }

    fn prune(&mut self, live: &[AnyWindowHandle]) -> bool {
        self.windows
            .retain(|entry| live.contains(&entry.window) && entry.shell.upgrade().is_some());
        if self.active.as_ref().is_some_and(|active| {
            !live.contains(&active.window) || active.shell.upgrade().is_none()
        }) {
            self.clear();
            return true;
        }
        if self
            .pointer_window
            .is_some_and(|window| !live.contains(&window))
        {
            self.pointer_window = None;
            for entry in &self.windows {
                entry.state.borrow_mut().preview = None;
            }
        }
        false
    }

    fn action_at(
        &self,
        pointer: Point<Pixels>,
        geometry: &[WindowGeometry],
        source: &TabRelocationSource,
    ) -> DropAction {
        let Some(front) = geometry
            .iter()
            .find(|window| window.frame.contains(&pointer))
        else {
            return DropAction::TearOut(pointer);
        };
        if !front.content.contains(&pointer) {
            return DropAction::Invalid;
        }
        let Some(entry) = self
            .windows
            .iter()
            .find(|entry| entry.window == front.window)
        else {
            return DropAction::Invalid;
        };
        if !entry.accepts_drop {
            return DropAction::Invalid;
        }
        let is_tool = matches!(
            source.tab.tab.surface,
            crate::app::workbench::TabSurfaceId::Tool(_)
        );
        let Some(target) = entry
            .state
            .borrow()
            .target_at(pointer - front.content.origin, is_tool)
        else {
            return DropAction::Invalid;
        };
        DropAction::Target(TabRelocationDestination {
            window: entry.window,
            workbench: entry.workbench.clone(),
            tab: target.tab,
        })
    }

    fn move_pointer(&mut self, pointer: Point<Pixels>, geometry: &[WindowGeometry]) {
        self.pointer = Some(pointer);
        self.pointer_window = geometry
            .iter()
            .find(|window| window.frame.contains(&pointer))
            .map(|window| window.window);
        let is_tool = self.active.as_ref().is_some_and(|active| {
            matches!(
                active.source.tab.tab.surface,
                crate::app::workbench::TabSurfaceId::Tool(_)
            )
        });
        for entry in &self.windows {
            let target = geometry
                .iter()
                .find(|window| {
                    Some(window.window) == self.pointer_window
                        && window.window == entry.window
                        && entry.accepts_drop
                        && window.content.contains(&pointer)
                })
                .and_then(|window| {
                    entry
                        .state
                        .borrow()
                        .target_at(pointer - window.content.origin, is_tool)
                });
            entry.state.borrow_mut().preview = target;
        }
    }
}

pub(super) fn coordinator(cx: &mut App) -> Rc<RefCell<TabDragCoordinator>> {
    if !cx.has_global::<ApplicationTabDrag>() {
        let state = Rc::new(RefCell::new(TabDragCoordinator::default()));
        cx.set_global(ApplicationTabDrag(state.clone()));
        let closed = state.clone();
        cx.on_window_closed(move |cx| {
            let live = cx.windows();
            let cancelled = closed.borrow_mut().prune(&live);
            if cancelled && let Some(window) = live.first() {
                let _ = cx.update_window(*window, |_, window, cx| {
                    cx.stop_active_drag(window);
                });
            }
            cx.refresh_windows();
        })
        .detach();
    }
    cx.global::<ApplicationTabDrag>().0.clone()
}

pub(super) fn cancel_source_window(window: &mut Window, cx: &mut App) {
    let Some(shared) = cx
        .try_global::<ApplicationTabDrag>()
        .map(|global| global.0.clone())
    else {
        return;
    };
    let is_source = shared
        .borrow()
        .active
        .as_ref()
        .is_some_and(|active| active.window == window.window_handle());
    if is_source {
        shared.borrow_mut().clear();
        cx.stop_active_drag(window);
        cx.refresh_windows();
    }
}

fn commit_drop_with(
    active: ActiveTabDrag,
    action: DropAction,
    cx: &mut App,
    create: impl FnOnce(Point<Pixels>, &mut App) -> anyhow::Result<WindowHandle<ProductShell>>,
) {
    let _ = cx.update_window(active.window, |_, window, cx| {
        if window
            .root::<ProductShell>()
            .flatten()
            .map(|shell| shell.downgrade())
            != Some(active.shell.clone())
        {
            return;
        }
        let Some(shell) = active.shell.upgrade() else {
            return;
        };
        shell.update(cx, |shell, cx| match action {
            DropAction::Target(destination) => {
                shell.relocate_tab_to(&active.source, &destination, window, cx);
            }
            DropAction::TearOut(pointer) => {
                shell.relocate_tab_to_new_window_with(&active.source, window, cx, |cx| {
                    create(pointer, cx)
                });
            }
            DropAction::Invalid => {}
        });
    });
}

pub(super) struct TabDragLabel {
    title: SharedString,
    coordinator: Rc<RefCell<TabDragCoordinator>>,
}
impl Render for TabDragLabel {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let state = self.coordinator.borrow();
        let content = content_bounds(window);
        let pointer = state.pointer.filter(|pointer| {
            state.active.is_some()
                && state.pointer_window == Some(window.window_handle())
                && content.contains(pointer)
        });
        div().when_some(pointer, |element, pointer| {
            // gpui positions the shared preview using each window's local
            // mouse position. Only the event receiver has a current position.
            let correction = pointer - content.origin - window.mouse_position();
            element.child(
                div()
                    .relative()
                    .left(correction.x)
                    .top(correction.y)
                    .debug_selector(|| "tab-drag-label".to_owned())
                    .px_3()
                    .py_1()
                    .text_sm()
                    .bg(rgb(0x405e90))
                    .text_color(rgb(0xffffff))
                    .child(self.title.clone()),
            )
        })
    }
}

impl ProductShell {
    pub(super) fn tab_drag_label(
        &self,
        title: SharedString,
        cx: &Context<Self>,
    ) -> impl Fn(&TabRelocationSource, Point<Pixels>, &mut Window, &mut App) -> Entity<TabDragLabel>
    + use<> {
        let coordinator = cx.global::<ApplicationTabDrag>().0.clone();
        let shell = cx.entity().downgrade();
        move |source, _, window, cx| {
            let geometry = window_geometries(window, cx);
            let mut state = coordinator.borrow_mut();
            state.clear();
            state.active = Some(ActiveTabDrag {
                shell: shell.clone(),
                window: window.window_handle(),
                source: source.clone(),
            });
            state.move_pointer(
                content_bounds(window).origin + window.mouse_position(),
                &geometry,
            );
            drop(state);
            cx.refresh_windows();
            cx.new(|_| TabDragLabel {
                title: title.clone(),
                coordinator: coordinator.clone(),
            })
        }
    }

    pub(super) fn workbench_drag_geometry(&self) -> impl IntoElement + use<> {
        let state = self.tab_drag.clone();
        canvas(
            move |bounds, _, _| {
                state.borrow_mut().workbench = Some(bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
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
        if event.keystroke.key == "escape"
            && let Some(resize) = self.dock_resize.take()
        {
            self.workbench.update(cx, |workbench, _| {
                workbench.set_dock_size(resize.dock, resize.start_size);
            });
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let coordinator = cx.global::<ApplicationTabDrag>().0.clone();
        if event.keystroke.key == "escape" && coordinator.borrow().active.is_some() {
            coordinator.borrow_mut().clear();
            cx.stop_active_drag(window);
            cx.stop_propagation();
            cx.refresh_windows();
        }
    }

    pub(super) fn drag_listener(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let state = self.tab_drag.clone();
        let coordinator = cx.global::<ApplicationTabDrag>().0.clone();
        let shell = cx.entity().downgrade();
        let workbench = self.workbench.downgrade();
        let accepts_drop = self.command_palette.is_none() && !self.extension_report_open;
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let handle = window.window_handle();
                let mut shared = coordinator.borrow_mut();
                shared.windows.retain(|entry| entry.window != handle);
                shared.windows.push(DragWindow {
                    window: handle,
                    shell: shell.clone(),
                    workbench: workbench.clone(),
                    state: state.clone(),
                    accepts_drop,
                });
                drop(shared);
                if let Some(preview) = state.borrow().preview {
                    window.paint_quad(fill(preview.bounds, rgba(0x7fb5ff55)));
                }
                let moves = coordinator.clone();
                let resize_shell = shell.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture
                        && let Some(shell) = resize_shell.upgrade()
                    {
                        let resize = shell.read(cx).dock_resize;
                        if let Some(resize) = resize
                            && event.dragging()
                        {
                            let delta = event.position - resize.start_pointer;
                            let size = match resize.dock {
                                DockLocation::Left => resize.start_size + f32::from(delta.x),
                                DockLocation::Right => resize.start_size - f32::from(delta.x),
                                DockLocation::Bottom => resize.start_size - f32::from(delta.y),
                            };
                            shell.update(cx, |shell, cx| {
                                shell.workbench.update(cx, |workbench, _| {
                                    workbench.set_dock_size(resize.dock, size);
                                });
                                cx.notify();
                            });
                            return;
                        }
                    }
                    if phase == DispatchPhase::Capture
                        && cx.has_active_drag()
                        && moves.borrow().active.is_some()
                    {
                        let geometry = window_geometries(window, cx);
                        moves.borrow_mut().move_pointer(
                            content_bounds(window).origin + event.position,
                            &geometry,
                        );
                        cx.refresh_windows();
                    }
                });
                let releases = coordinator.clone();
                let resize_release = shell.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                        return;
                    }
                    if let Some(shell) = resize_release.upgrade()
                        && shell.read(cx).dock_resize.is_some()
                    {
                        shell.update(cx, |shell, cx| {
                            shell.dock_resize = None;
                            cx.notify();
                        });
                    }
                    let active = releases.borrow_mut().active.take();
                    if let Some(active) = active {
                        let geometry = window_geometries(window, cx);
                        let action = releases.borrow().action_at(
                            content_bounds(window).origin + event.position,
                            &geometry,
                            &active.source,
                        );
                        releases.borrow_mut().clear();
                        if cx.has_active_drag() {
                            cx.stop_active_drag(window);
                            cx.stop_propagation();
                            // Release can arrive in the destination window. Commit
                            // after dispatch so neither endpoint is borrowed here.
                            cx.defer(move |cx| {
                                commit_drop_with(active, action, cx, open_tab_tear_out_window)
                            });
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

#[cfg(test)]
mod cross_window_tests;
