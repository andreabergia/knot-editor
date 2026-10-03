//! Native fatal personal configuration diagnostic window.

use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Bounds, ClipboardItem, Context, FocusHandle, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window, WindowBounds, WindowOptions,
    div, px, rgb, size,
};

use super::personal_config::ConfigDiagnostic;
use super::product_commands::{ProductCommandSource, QUIT_COMMAND};

pub(crate) struct PersonalConfigErrorView {
    diagnostic: ConfigDiagnostic,
    focus: FocusHandle,
    #[cfg(test)]
    quit_requested: bool,
}

impl PersonalConfigErrorView {
    fn new(diagnostic: ConfigDiagnostic, cx: &mut Context<Self>) -> Self {
        Self {
            diagnostic,
            focus: cx.focus_handle(),
            #[cfg(test)]
            quit_requested: false,
        }
    }

    fn copy(&self, cx: &mut App) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.diagnostic.clipboard_text()));
    }
}

impl Render for PersonalConfigErrorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title("Knot configuration error");
        let phase = self
            .diagnostic
            .phase
            .map(|phase| phase.label())
            .unwrap_or("Discovery");
        let location = match (self.diagnostic.line, self.diagnostic.column) {
            (Some(line), Some(column)) => {
                format!("{}:{line}:{column}", self.diagnostic.path.display())
            }
            (Some(line), None) => format!("{}:{line}", self.diagnostic.path.display()),
            _ => self.diagnostic.path.display().to_string(),
        };
        div()
            .track_focus(&self.focus)
            .key_context("config-error")
            .on_action(cx.listener(|_this, source: &ProductCommandSource, _, cx| {
                if source.name.as_ref() == QUIT_COMMAND {
                    #[cfg(test)]
                    {
                        _this.quit_requested = true;
                    }
                    cx.quit();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_5()
            .bg(rgb(0x202024))
            .text_color(rgb(0xe6e6e6))
            .child(div().text_lg().child("Personal configuration failed"))
            .child(div().text_color(rgb(0xf19a8e)).child(phase))
            .child(div().text_sm().child(location))
            .child(
                div()
                    .id("config-error-message")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.diagnostic.cause.clone())
                    .when_some(self.diagnostic.stack.clone(), |message, stack| {
                        message.child(div().pt_3().text_sm().child(stack))
                    }),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_4()
                    .child(
                        div()
                            .id("copy-config-error")
                            .cursor_pointer()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(0x5c5c65))
                            .px_3()
                            .py_1()
                            .text_color(rgb(0x80c0ff))
                            .child("Copy")
                            .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
                    )
                    .child(
                        div()
                            .id("quit-config-error")
                            .cursor_pointer()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(0x5c5c65))
                            .px_3()
                            .py_1()
                            .text_color(rgb(0x80c0ff))
                            .child("Quit")
                            .on_click(|_, _, cx| cx.quit()),
                    ),
            )
    }
}

pub(crate) fn open(diagnostic: ConfigDiagnostic, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(760.), px(420.)), cx);
    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(560.), px(300.))),
            ..Default::default()
        },
        |window, cx| {
            window.on_window_should_close(cx, |_, _| false);
            let view = cx.new(|cx| PersonalConfigErrorView::new(diagnostic, cx));
            view.read(cx).focus.focus(window);
            view
        },
    )
    .expect("personal configuration error window must open");
    cx.activate(true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::personal_config::ConfigPhase;
    use crate::app::product::bind_product_keys;
    use gpui::TestAppContext;

    fn diagnostic() -> ConfigDiagnostic {
        ConfigDiagnostic {
            phase: Some(ConfigPhase::PostInit),
            path: std::path::PathBuf::from("config").join("knot/post-init.js"),
            line: Some(7),
            column: Some(3),
            cause: "Error: bad configuration".into(),
            stack: Some("at post-init.js:7:3".into()),
        }
    }

    #[gpui::test]
    fn quit_command_is_bound_in_the_error_window(cx: &mut TestAppContext) {
        cx.update(bind_product_keys);
        cx.update(|cx| open(diagnostic(), cx));
        let window = cx.windows()[0]
            .downcast::<PersonalConfigErrorView>()
            .unwrap();
        cx.simulate_keystrokes(*window, "cmd-q");
        cx.read(|cx| assert!(window.read(cx).unwrap().quit_requested));
    }

    #[gpui::test]
    fn copy_places_the_complete_diagnostic_on_the_clipboard(cx: &mut TestAppContext) {
        let diagnostic = diagnostic();
        cx.update(|cx| {
            let view = cx.new(|cx| PersonalConfigErrorView::new(diagnostic.clone(), cx));
            view.update(cx, |view, cx| view.copy(cx));
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                format!(
                    "Knot personal configuration failed during Post-init\n{}:7:3\nError: bad configuration\nat post-init.js:7:3",
                    diagnostic.path.display()
                )
            );
        });
    }
}
