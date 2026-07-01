// Step 3 — gpui API-reachability spike: simplest possible app.
// Goal: confirm gpui opens a window and renders text via the public API,
// from outside the Zed repo, with no privileged glue. This is the groundwork
// for the no-internal-path scorecard; it intentionally does nothing else.

use gpui::*;

struct HelloView;

impl Render for HelloView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().child("Hello, gpui!")
    }
}

fn main() {
    Application::new().run(|app: &mut App| {
        let bounds = Bounds::centered(None, size(px(800.), px(600.)), app);
        app.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(|_| HelloView),
        )
        .unwrap();
        app.set_menus(Vec::new());
    });
}