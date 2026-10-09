//! Two-window probe of gpui drag delivery. Events are printed to stderr.

use gpui::{
    App, Application, Bounds, Context, DispatchPhase, FocusHandle, MouseMoveEvent, MouseUpEvent,
    Window, WindowBounds, WindowOptions, canvas, div, point, prelude::*, px, rgb, size,
};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Debug)]
struct TabDrag {
    source: usize,
    tab: usize,
    sequence: usize,
}

impl Render for TabDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_2()
            .bg(rgb(0x405e90))
            .text_color(rgb(0xffffff))
            .child(format!("Window {} / tab {}", self.source, self.tab))
    }
}

#[derive(Default)]
struct Trace {
    sequence: usize,
    active: Option<TabDrag>,
    drops: Vec<(TabDrag, usize, &'static str)>,
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
        Self { id, focus, trace }
    }

    fn target(&self, label: &'static str) -> impl IntoElement {
        let trace = self.trace.clone();
        let destination = self.id;
        div()
            .id(label)
            .debug_selector(move || label.to_owned())
            .flex_1()
            .p_3()
            .border_2()
            .border_color(rgb(0x657080))
            .drag_over::<TabDrag>(|style, _, _, _| {
                style.bg(rgb(0x405e90)).border_color(rgb(0x7fb5ff))
            })
            .child(label)
            .on_drop(move |drag: &TabDrag, _, _| {
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
                let moves = trace.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture && cx.has_active_drag() {
                        let trace = moves.borrow();
                        let bounds = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
                        eprintln!("MOVE seq={:?} receiver={id} position={:?} inside={} pressed={:?} window={:?}", trace.active.as_ref().map(|drag| drag.sequence), event.position, bounds.contains(&event.position), event.pressed_button, window.inner_window_bounds());
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase == DispatchPhase::Capture {
                        if let Some(drag) = trace.borrow().active.clone() {
                            let bounds = Bounds::new(point(px(0.), px(0.)), window.viewport_size());
                            eprintln!("UP seq={} receiver={id} position={:?} inside={} button={:?} gpui_active={}", drag.sequence, event.position, bounds.contains(&event.position), event.button, cx.has_active_drag());
                            let trace = trace.clone();
                            cx.defer(move |cx| {
                                let mut trace = trace.borrow_mut();
                                if trace.active.as_ref().is_some_and(|active| active.sequence == drag.sequence) {
                                    trace.active = None;
                                    eprintln!("NO_TARGET seq={} gpui_active={}", drag.sequence, cx.has_active_drag());
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
                    .on_drag(TabDrag { source: id, tab, sequence: 0 }, move |drag, _, _, cx| {
                        let mut trace = trace.borrow_mut();
                        trace.sequence += 1;
                        let drag = TabDrag { sequence: trace.sequence, ..drag.clone() };
                        eprintln!("START seq={} source={id} tab={tab}", drag.sequence);
                        trace.active = Some(drag.clone());
                        cx.new(|_| drag)
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
