//! Settled view-state changes of one window.
//!
//! Editor cursor, selection, and scroll changes, focus, dock resizing, window
//! bounds, and shell changes restart a trailing-edge debounce. When it settles
//! the window captures its layout and, when it differs from the last one
//! published, reports `viewStateChanged` to extension observers. A changed
//! layout or root set also re-records the native window session.

use std::time::Duration;

use super::*;
use crate::app::window_layout::WindowLayout;

/// Trailing-edge debounce before a window reports a settled view state.
pub(super) const VIEW_STATE_DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct ViewStateTracking {
    pending: Option<Task<()>>,
    published: Option<(Vec<String>, WindowLayout)>,
    editors: Vec<(EntityId, Subscription)>,
    bounds: Option<Subscription>,
}

impl ProductShell {
    pub(super) fn observe_window_bounds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view_state.bounds = Some(cx.observe_window_bounds(window, |this, _, cx| {
            this.schedule_view_state(cx);
        }));
    }

    /// Observe every editor view presented in this window.
    pub(super) fn observe_editor_view_state(&mut self, cx: &mut Context<Self>) {
        let editors = self
            .workbench
            .read(cx)
            .panes()
            .iter()
            .flat_map(|pane| pane.tabs())
            .filter_map(|tab| tab.editor().cloned())
            .collect::<Vec<_>>();
        self.view_state
            .editors
            .retain(|(id, _)| editors.iter().any(|editor| editor.entity_id() == *id));
        for editor in editors {
            if self
                .view_state
                .editors
                .iter()
                .any(|(id, _)| *id == editor.entity_id())
            {
                continue;
            }
            let subscription = cx.observe(&editor, |this, _, cx| {
                this.promote_edited_previews(cx);
                this.schedule_view_state(cx);
            });
            self.view_state
                .editors
                .push((editor.entity_id(), subscription));
        }
    }

    /// Make preview tabs whose documents have unsaved edits regular.
    fn promote_edited_previews(&mut self, cx: &mut Context<Self>) {
        let documents = Self::documents(cx);
        let promoted = self.workbench.update(cx, |workbench, cx| {
            workbench.promote_edited_previews(|document| {
                documents
                    .read(cx)
                    .get(document)
                    .is_some_and(|document| document.is_dirty(cx))
            })
        });
        if promoted {
            cx.notify();
        }
    }

    /// Restart this window's view-state debounce.
    pub(super) fn schedule_view_state(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.window else {
            return;
        };
        self.view_state.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(VIEW_STATE_DEBOUNCE).await;
            let Ok(Ok(settled)) = cx.update_window(handle, |_, window, cx| {
                this.update(cx, |shell, cx| shell.settle_view_state(window, cx))
            }) else {
                return;
            };
            let _ = cx.update(|cx| match settled {
                Settled::Unchanged => {}
                Settled::Roots => crate::app::window_sessions::record(cx),
                Settled::Layout(target) => {
                    crate::app::extensions::host::view_state_changed(target, cx);
                    crate::app::window_sessions::record(cx);
                }
            });
        }));
    }

    /// Record the settled layout and roots and report what changed.
    fn settle_view_state(&mut self, window: &Window, cx: &mut Context<Self>) -> Settled {
        let roots = self.root_uris().iter().map(ToString::to_string).collect();
        let settled = (roots, self.capture_layout(window, cx));
        let previous = self.view_state.published.replace(settled.clone());
        match previous {
            Some(previous) if previous == settled => Settled::Unchanged,
            Some((_, layout)) if layout == settled.1 => Settled::Roots,
            _ => Settled::Layout(crate::host::protocol::WindowTarget {
                window: window.window_handle().window_id().as_u64().to_string(),
            }),
        }
    }
}

enum Settled {
    Unchanged,
    /// Only the roots changed; workspace events already report them.
    Roots,
    Layout(crate::host::protocol::WindowTarget),
}
