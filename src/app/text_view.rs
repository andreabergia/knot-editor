//! Read-only generated text presented inside a contributed tool frame.
//!
//! A text frame owns a private read-only buffer and renders it through the same
//! editor surface used for documents. The buffer is not registered with the
//! document collection, so it has no persistence or backing resource; the
//! extension replaces its whole text through `workbench.registerView`'s
//! `setText` operation.

use std::sync::atomic::Ordering;

use gpui::{prelude::*, *};

use crate::host::protocol::ViewId;

use super::editor::EditorView;
use super::model::BufferModel;
use super::tree_view::NEXT_VIEW_ID;

/// One contributed text frame instance in a product window.
pub(crate) struct TextView {
    view_id: String,
    instance_id: ViewId,
    model: Entity<BufferModel>,
    editor: Entity<EditorView>,
}

impl TextView {
    pub(crate) fn new(
        view_id: impl Into<String>,
        window: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let _ = window;
        let view_id = view_id.into();
        let model = cx.new(|_| BufferModel::from_read_only_text(""));
        let editor =
            cx.new(|cx| EditorView::for_contributed_view(model.clone(), view_id.clone(), cx));
        Self {
            view_id,
            instance_id: ViewId::new(NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed)),
            model,
            editor,
        }
    }

    pub(crate) fn kind(&self) -> &str {
        &self.view_id
    }

    pub(crate) fn instance_id(&self) -> ViewId {
        self.instance_id
    }

    pub(crate) fn model(&self) -> &Entity<BufferModel> {
        &self.model
    }

    /// Replace the complete frame text and keep the existing view state where
    /// the editor can preserve it.
    pub(crate) fn replace_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| {
            if model.replace_generated_text(text) {
                cx.notify();
            }
        });
        cx.notify();
    }

    pub(crate) fn selected_text(&self, cx: &App) -> Option<String> {
        let range = self.editor.read(cx).selected_byte_range(cx)?;
        if range.is_empty() {
            return None;
        }
        Some(self.model.read(cx).read(range))
    }

    pub(crate) fn handle_command(
        &self,
        command: &crate::host::protocol::Command,
        cx: &mut Context<Self>,
    ) -> super::commands::dispatch::CommandClaim {
        use super::commands::dispatch::{COPY_COMMAND, CommandClaim};

        if command.name.as_ref() == COPY_COMMAND
            && let Some(text) = self.selected_text(cx)
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            return CommandClaim::Finished(crate::host::protocol::CommandOutcome::Completed);
        }
        CommandClaim::Declined
    }

    #[cfg(test)]
    pub(crate) fn text(&self, cx: &App) -> String {
        self.model.read(cx).text()
    }
}

impl Focusable for TextView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl Render for TextView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.editor.clone()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext};

    use super::TextView;

    #[gpui::test]
    fn replacement_commits_and_repeated_text_is_a_no_op(cx: &mut TestAppContext) {
        let view = cx.new(|cx| TextView::new("example.log", "1", cx));
        view.update(cx, |view, cx| view.replace_text("first\n", cx));
        assert_eq!(cx.read(|cx| view.read(cx).text(cx)), "first\n");

        let revision = cx.read(|cx| view.read(cx).model.read(cx).revision());
        view.update(cx, |view, cx| view.replace_text("first\n", cx));
        assert_eq!(
            cx.read(|cx| view.read(cx).model.read(cx).revision()),
            revision
        );

        view.update(cx, |view, cx| view.replace_text("second\n", cx));
        assert_eq!(cx.read(|cx| view.read(cx).text(cx)), "second\n");
    }

    #[gpui::test]
    fn the_frame_editor_resolves_settings_for_its_view_id(cx: &mut TestAppContext) {
        use crate::app::{
            editor::EditorView,
            model::BufferModel,
            settings::{ApplicationSettings, EDITOR_TAB_SIZE, SettingsScope},
        };
        cx.update(|cx| {
            let mut settings = ApplicationSettings::default();
            let scope = SettingsScope {
                language: None,
                view: Some("example.log".into()),
            };
            settings.0.set(EDITOR_TAB_SIZE, 2.into(), scope).unwrap();
            cx.set_global(settings);
        });
        let frame = cx.new(|cx| TextView::new("example.log", "1", cx));
        let document = cx.new(|_| BufferModel::from_text(""));
        let document = cx.new(|cx| EditorView::new(document, cx));
        cx.read(|cx| {
            let frame = frame.read(cx);
            assert_eq!(frame.editor.read(cx).tab_size(frame.model.read(cx), cx), 2);
            let document = document.read(cx);
            assert_eq!(document.tab_size(document.model().read(cx), cx), 4);
        });
    }
}
