//! Application entry point: window and event loop.
//!
//! This is the prototype skeleton from roadmap step 1. It stands up a
//! winit window and runs the event loop. No GPU, no rendering — that
//! arrives in step 2. The goal is to validate that the project can open
//! a window with the desired module layering in place.

use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

pub fn run() {
    let event_loop = EventLoop::new().expect("event loop construction failed");

    let mut app = App { window: None };

    event_loop
        .run_app(&mut app)
        .expect("event loop exited with an error");
}

struct App {
    window: Option<Window>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let window = event_loop
                .create_window(Window::default_attributes().with_title("Knot"))
                .expect("window creation failed");
            self.window = Some(window);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            _ => {}
        }
    }
}
