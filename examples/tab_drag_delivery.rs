//! Two-window probe of gpui drag delivery. Events are printed to stderr.

use gpui::{
    App, Application, Bounds, Context, DispatchPhase, FocusHandle, MouseButton, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Window, WindowBounds, WindowOptions, canvas, div, point,
    prelude::*, px, rgb, size,
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

#[derive(Clone, Debug)]
struct TabDrag {
    source: usize,
    tab: usize,
    sequence: usize,
}

struct DragPreview {
    drag: TabDrag,
    trace: Rc<RefCell<Trace>>,
}

impl Render for DragPreview {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let trace = self.trace.borrow();
        let content = content_bounds(window);
        let pointer = trace
            .pointer
            .filter(|pointer| trace.active.is_some() && content.contains(pointer));
        div().when_some(pointer, |element, pointer| {
            // gpui places this root at its window-local mouse position minus
            // the grab offset. Correct that position with the delivered pointer.
            let correction = pointer - content.origin - window.mouse_position();
            element.child(
                div()
                    .debug_selector(|| "drag-preview".to_owned())
                    .relative()
                    .left(correction.x)
                    .top(correction.y)
                    .p_2()
                    .bg(rgb(0x405e90))
                    .text_color(rgb(0xffffff))
                    .child(format!(
                        "Window {} / tab {}",
                        self.drag.source, self.drag.tab
                    )),
            )
        })
    }
}

#[derive(Default)]
struct Trace {
    sequence: usize,
    active: Option<TabDrag>,
    drops: Vec<(TabDrag, usize, &'static str)>,
    windows: BTreeMap<usize, Bounds<Pixels>>,
    frames: BTreeMap<usize, Bounds<Pixels>>,
    targets: BTreeMap<(usize, &'static str), Bounds<Pixels>>,
    highlighted: Option<(usize, &'static str)>,
    pointer: Option<Point<Pixels>>,
}

fn content_bounds(window: &Window) -> Bounds<Pixels> {
    let frame = window.bounds();
    let content = window.viewport_size();
    Bounds::new(
        frame.origin + point(px(0.), frame.size.height - content.height),
        content,
    )
}

impl Trace {
    fn target_at(&self, receiver: usize, local: Point<Pixels>) -> Option<(usize, &'static str)> {
        let screen = self.windows.get(&receiver)?.origin + local;
        // Native drop handlers retain authority while the pointer is inside the receiver.
        let destination = if self.windows.get(&receiver)?.contains(&screen) {
            receiver
        } else {
            *self
                .windows
                .iter()
                .find(|(_, bounds)| bounds.contains(&screen))?
                .0
        };
        let local = screen - self.windows.get(&destination)?.origin;
        self.targets.iter().find_map(|(target, bounds)| {
            (target.0 == destination && bounds.contains(&local)).then_some(*target)
        })
    }
}

struct Probe {
    id: usize,
    focus: FocusHandle,
    trace: Rc<RefCell<Trace>>,
}

impl Probe {
    fn new(
        id: usize,
        trace: Rc<RefCell<Trace>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        cx.observe_window_bounds(window, |_, _, cx| cx.notify())
            .detach();
        let released_trace = trace.clone();
        cx.on_release(move |_, cx| {
            let mut trace = released_trace.borrow_mut();
            trace.windows.remove(&id);
            trace.frames.remove(&id);
            trace.targets.retain(|(window, _), _| *window != id);
            if trace.highlighted.is_some_and(|target| target.0 == id) {
                trace.highlighted = None;
            }
            cx.refresh_windows();
        })
        .detach();
        Self { id, focus, trace }
    }

    fn target(&self, label: &'static str) -> impl IntoElement {
        let trace = self.trace.clone();
        let destination = self.id;
        let bounds_trace = trace.clone();
        let highlighted = trace.borrow().highlighted == Some((destination, label));
        div()
            .id(label)
            .relative()
            .debug_selector(move || label.to_owned())
            .flex_1()
            .p_3()
            .border_2()
            .border_color(rgb(0x657080))
            .when(highlighted, |style| {
                style.bg(rgb(0x405e90)).border_color(rgb(0x7fb5ff))
            })
            .drag_over::<TabDrag>(|style, _, _, _| {
                style.bg(rgb(0x405e90)).border_color(rgb(0x7fb5ff))
            })
            .child(label)
            .child(
                canvas(
                    move |bounds, _, _| {
                        bounds_trace
                            .borrow_mut()
                            .targets
                            .insert((destination, label), bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .on_drop(move |drag: &TabDrag, _, cx| {
                let mut trace = trace.borrow_mut();
                let sequence = trace.active.as_ref().map(|drag| drag.sequence).unwrap_or(0);
                eprintln!(
                    "DROP seq={sequence} source={} tab={} destination={} target={label}",
                    drag.source, drag.tab, destination
                );
                trace.drops.push((
                    TabDrag {
                        sequence,
                        ..drag.clone()
                    },
                    destination,
                    label,
                ));
                trace.active = None;
                trace.highlighted = None;
                trace.pointer = None;
                cx.refresh_windows();
            })
    }
}

impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let id = self.id;
        let trace = self.trace.clone();
        let logger = canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                trace
                    .borrow_mut()
                    .windows
                    .insert(id, content_bounds(window));
                trace.borrow_mut().frames.insert(id, window.bounds());
                let moves = trace.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture && cx.has_active_drag() {
                        let mut trace = moves.borrow_mut();
                        let pointer = trace.windows.get(&id).map(|bounds| bounds.origin + event.position);
                        let pointer_changed = trace.pointer != pointer;
                        trace.pointer = pointer;
                        let next = trace.target_at(id, event.position);
                        if trace.highlighted != next {
                            trace.highlighted = next;
                            eprintln!("PREVIEW seq={:?} receiver={id} destination={next:?}", trace.active.as_ref().map(|drag| drag.sequence));
                            cx.refresh_windows();
                        }
                        if pointer_changed {
                            cx.refresh_windows();
                        }
                        let bounds = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
                        eprintln!("MOVE seq={:?} receiver={id} position={:?} inside={} pressed={:?} window={:?}", trace.active.as_ref().map(|drag| drag.sequence), event.position, bounds.contains(&event.position), event.pressed_button, window.inner_window_bounds());
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        let active = trace.borrow().active.clone();
                        if let Some(drag) = active {
                            let bounds = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
                            eprintln!("UP seq={} receiver={id} position={:?} inside={} button={:?} gpui_active={}", drag.sequence, event.position, bounds.contains(&event.position), event.button, cx.has_active_drag());
                            let destination = trace.borrow().target_at(id, event.position);
                            if event.button == MouseButton::Left && cx.has_active_drag() {
                                if let Some((destination, label)) = destination.filter(|target| target.0 != id) {
                                    eprintln!("COORDINATED_DROP seq={} source={} tab={} destination={destination} target={label}", drag.sequence, drag.source, drag.tab);
                                    let mut trace = trace.borrow_mut();
                                    trace.drops.push((drag, destination, label));
                                    trace.active = None;
                                    trace.highlighted = None;
                                    trace.pointer = None;
                                    cx.stop_active_drag(window);
                                    cx.stop_propagation();
                                    cx.refresh_windows();
                                    return;
                                }
                                let trace = trace.borrow();
                                let screen = trace.windows[&id].origin + event.position;
                                if !trace.frames.values().any(|bounds| bounds.contains(&screen)) {
                                    eprintln!("TEAR_OUT_READY seq={} screen={screen:?}", drag.sequence);
                                }
                            }
                            let trace = trace.clone();
                            cx.defer(move |cx| {
                                let mut trace = trace.borrow_mut();
                                if trace.active.as_ref().is_some_and(|active| active.sequence == drag.sequence) {
                                    trace.active = None;
                                    trace.highlighted = None;
                                    trace.pointer = None;
                                    eprintln!("NO_TARGET seq={} gpui_active={}", drag.sequence, cx.has_active_drag());
                                    cx.refresh_windows();
                                }
                            });
                        }
                    }
                });
            },
        );
        let cancel_trace = self.trace.clone();
        div()
            .id("probe")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .bg(rgb(0x20252d))
            .text_color(rgb(0xffffff))
            .on_key_down(move |event, window, cx| {
                if event.keystroke.key == "escape" {
                    let stopped = cx.stop_active_drag(window);
                    let active = cancel_trace.borrow_mut().active.take();
                    cancel_trace.borrow_mut().highlighted = None;
                    cancel_trace.borrow_mut().pointer = None;
                    cx.refresh_windows();
                    eprintln!("CANCEL receiver={id} seq={:?} stopped={stopped}", active.map(|drag| drag.sequence));
                    cx.stop_propagation();
                }
            })
            .child(format!("Drag delivery probe — Window {id}"))
            .child("Drag tabs to blue targets. Escape cancels. Events go to stderr.")
            .child(logger)
            .child(div().flex().gap_2().children((0..3).map(|tab| {
                let trace = self.trace.clone();
                div()
                    .id(("tab", tab))
                    .debug_selector(move || ["tab0", "tab1", "tab2"][tab].to_owned())
                    .w(px(110.))
                    .h(px(40.))
                    .p_2()
                    .bg(rgb(0x384252))
                    .cursor_move()
                    .child(format!("Tab {tab}"))
                    .on_drag(TabDrag { source: id, tab, sequence: 0 }, move |drag, _, window, cx| {
                        let drag = {
                            let mut trace = trace.borrow_mut();
                            trace.sequence += 1;
                            let drag = TabDrag { sequence: trace.sequence, ..drag.clone() };
                            eprintln!("START seq={} source={id} tab={tab}", drag.sequence);
                            trace.active = Some(drag.clone());
                            trace.pointer = Some(content_bounds(window).origin + window.mouse_position());
                            drag
                        };
                        cx.new(|_| DragPreview { drag, trace: trace.clone() })
                    })
            })))
            .child(div().flex().gap_2().children([
                self.target("insert 0"), self.target("insert 1"), self.target("append"),
            ]))
            .child(div().flex().flex_col().flex_1().gap_2()
                .child(self.target("top"))
                .child(div().flex().flex_1().gap_2().children([
                    self.target("left"), self.target("center"), self.target("right"),
                ]))
                .child(self.target("bottom")))
            .child("Targets report delivery only; tabs stay in place. Release on empty space or desktop to compare UP and DROP.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext};

    fn start_drag(cx: &mut VisualTestContext) {
        let source = cx.debug_bounds("tab0").unwrap().center();
        cx.simulate_mouse_down(source, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            source + point(px(12.), px(0.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.run_until_parked();
    }

    #[gpui::test]
    fn targets_receive_payload_and_escape_prevents_drop(cx: &mut TestAppContext) {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let (_, cx) = cx.add_window_view(|window, cx| Probe::new(1, trace.clone(), window, cx));
        cx.run_until_parked();
        for label in [
            "insert 0", "insert 1", "append", "top", "left", "center", "right", "bottom",
        ] {
            start_drag(cx);
            let target = cx.debug_bounds(label).unwrap().center();
            cx.simulate_mouse_move(target, MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
            cx.run_until_parked();
            let trace = trace.borrow();
            let (drag, destination, actual) = trace.drops.last().unwrap();
            assert_eq!(
                (drag.source, drag.tab, *destination, *actual),
                (1, 0, 1, label)
            );
            assert_eq!(drag.sequence, trace.sequence);
            assert!(trace.active.is_none());
        }
        start_drag(cx);
        cx.simulate_keystrokes("escape");
        let target = cx.debug_bounds("center").unwrap().center();
        cx.simulate_mouse_move(target, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(trace.borrow().drops.len(), 8);
        assert!(trace.borrow().active.is_none());
        cx.read(|cx| assert!(!cx.has_active_drag()));
    }

    #[gpui::test]
    fn delivered_outside_release_clears_drag_without_drop(cx: &mut TestAppContext) {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let (_, cx) = cx.add_window_view(|window, cx| Probe::new(1, trace.clone(), window, cx));
        cx.run_until_parked();
        start_drag(cx);
        cx.simulate_mouse_up(
            point(px(-100.), px(-100.)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.run_until_parked();
        assert!(trace.borrow().active.is_none());
        assert!(trace.borrow().drops.is_empty());
        cx.read(|cx| assert!(!cx.has_active_drag()));
    }

    #[gpui::test]
    fn payload_reaches_another_window_when_events_are_delivered_there(cx: &mut TestAppContext) {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let source = cx.add_window(|window, cx| Probe::new(1, trace.clone(), window, cx));
        let destination = cx.add_window(|window, cx| Probe::new(2, trace.clone(), window, cx));
        cx.run_until_parked();
        start_drag(&mut VisualTestContext::from_window(*source, cx));
        let mut destination = VisualTestContext::from_window(*destination, cx);
        let target = destination.debug_bounds("center").unwrap().center();
        destination.simulate_mouse_move(target, MouseButton::Left, Modifiers::default());
        destination.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
        destination.run_until_parked();
        let trace = trace.borrow();
        assert_eq!(trace.drops.len(), 1);
        let (drag, destination, label) = &trace.drops[0];
        assert_eq!(
            (drag.source, drag.tab, *destination, *label),
            (1, 0, 2, "center")
        );
        assert!(trace.active.is_none());
    }

    #[gpui::test]
    fn source_window_delivery_coordinates_destination_and_cancellation(cx: &mut TestAppContext) {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let windows = cx.update(|cx| {
            (1..=2)
                .map(|id| {
                    let trace = trace.clone();
                    cx.open_window(
                        WindowOptions {
                            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                                point(px(40. + (id - 1) as f32 * 660.), px(80.)),
                                size(px(640.), px(520.)),
                            ))),
                            ..Default::default()
                        },
                        |window, cx| cx.new(|cx| Probe::new(id, trace, window, cx)),
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>()
        });
        cx.run_until_parked();
        let destination_center = VisualTestContext::from_window(*windows[1], cx)
            .debug_bounds("center")
            .unwrap()
            .center();
        let destination_right = VisualTestContext::from_window(*windows[1], cx)
            .debug_bounds("right")
            .unwrap()
            .center();
        let offset = {
            let trace = trace.borrow();
            trace.windows[&2].origin - trace.windows[&1].origin
        };
        let mut source = VisualTestContext::from_window(*windows[0], cx);
        let tab_bounds = source.debug_bounds("tab0").unwrap();
        let grab_offset = tab_bounds.center() - tab_bounds.origin + point(px(12.), px(0.));
        start_drag(&mut source);
        source.simulate_mouse_move(
            destination_center + offset,
            MouseButton::Left,
            Modifiers::default(),
        );
        source.run_until_parked();
        assert_eq!(trace.borrow().highlighted, Some((2, "center")));
        assert!(source.debug_bounds("drag-preview").is_none());
        let first_preview = VisualTestContext::from_window(*windows[1], cx)
            .debug_bounds("drag-preview")
            .unwrap();
        assert_eq!(first_preview.origin, destination_center - grab_offset);
        let delta = point(px(10.), px(8.));
        source.simulate_mouse_move(
            destination_center + offset + delta,
            MouseButton::Left,
            Modifiers::default(),
        );
        source.run_until_parked();
        assert_eq!(trace.borrow().highlighted, Some((2, "center")));
        let second_preview = VisualTestContext::from_window(*windows[1], cx)
            .debug_bounds("drag-preview")
            .unwrap();
        assert_eq!(second_preview.origin - first_preview.origin, delta);
        // Commit uses the release position rather than the last preview.
        source.simulate_mouse_up(
            destination_right + offset,
            MouseButton::Left,
            Modifiers::default(),
        );
        source.run_until_parked();
        assert_eq!(trace.borrow().drops.len(), 1);
        assert_eq!(
            (trace.borrow().drops[0].1, trace.borrow().drops[0].2),
            (2, "right")
        );
        assert!(trace.borrow().highlighted.is_none());
        assert!(trace.borrow().pointer.is_none());
        source.read(|cx| assert!(!cx.has_active_drag()));

        start_drag(&mut source);
        source.simulate_mouse_move(
            destination_center + offset,
            MouseButton::Left,
            Modifiers::default(),
        );
        source.simulate_keystrokes("escape");
        source.simulate_mouse_up(
            destination_center + offset,
            MouseButton::Left,
            Modifiers::default(),
        );
        source.run_until_parked();
        assert_eq!(trace.borrow().drops.len(), 1);
        assert!(trace.borrow().highlighted.is_none());
        assert!(trace.borrow().active.is_none());
        assert!(trace.borrow().pointer.is_none());
    }

    #[test]
    fn coordinate_mapping_accounts_for_distinct_content_origins_and_invalid_space() {
        let mut trace = Trace::default();
        trace.windows.insert(
            1,
            Bounds::new(point(px(40.), px(112.)), size(px(640.), px(520.))),
        );
        trace.windows.insert(
            2,
            Bounds::new(point(px(700.), px(152.)), size(px(640.), px(520.))),
        );
        trace.targets.insert(
            (2, "center"),
            Bounds::new(point(px(200.), px(200.)), size(px(200.), px(100.))),
        );
        assert_eq!(
            trace.target_at(1, point(px(960.), px(290.))),
            Some((2, "center"))
        );
        assert_eq!(trace.target_at(1, point(px(660.), px(40.))), None);
        assert_eq!(trace.target_at(1, point(px(-100.), px(-100.))), None);
        trace.windows.remove(&2);
        assert_eq!(trace.target_at(1, point(px(960.), px(290.))), None);
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let trace = Rc::new(RefCell::new(Trace::default()));
        for id in 1..=2 {
            let trace = trace.clone();
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(40. + (id - 1) as f32 * 660.), px(80.)),
                        size(px(640.), px(520.)),
                    ))),
                    ..Default::default()
                },
                |window, cx| cx.new(|cx| Probe::new(id, trace, window, cx)),
            )
            .unwrap();
        }
        cx.activate(true);
    });
}
