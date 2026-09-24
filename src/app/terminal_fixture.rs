//! Diagnostic window for exercising the terminal session and view lifecycle.

use gpui::*;

use super::{terminal_session::TerminalSession, terminal_view::TerminalView};

struct TerminalFixture {
    session: Entity<TerminalSession>,
    view: Entity<TerminalView>,
}

impl TerminalFixture {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let session = cx.new(TerminalSession::new);
        let view = cx.new(|cx| TerminalView::new(session.clone(), cx));
        window.focus(&view.focus_handle(cx));
        Self { session, view }
    }

    fn rebuild(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view.update(cx, |view, cx| view.detach(cx));
        let view = cx.new(|cx| TerminalView::new(self.session.clone(), cx));
        window.focus(&view.focus_handle(cx));
        self.view = view;
        cx.notify();
    }
}

impl Render for TerminalFixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x181818))
            .child(
                div()
                    .h(px(30.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px_2()
                    .gap_3()
                    .text_xs()
                    .text_color(rgb(0xd4d4d4))
                    .child("Terminal session fixture")
                    .child(
                        div()
                            .id("rebuild-terminal-view")
                            .cursor_pointer()
                            .text_color(rgb(0x80c0ff))
                            .child("rebuild view")
                            .on_click(cx.listener(|this, _, window, cx| this.rebuild(window, cx))),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.view.clone()))
    }
}

pub(crate) fn run() {
    Application::new().run(|cx| {
        let bounds = Bounds::centered(None, size(px(1000.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(480.), px(320.))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| TerminalFixture::new(window, cx)),
        )
        .expect("terminal fixture window must open");
    });
}
