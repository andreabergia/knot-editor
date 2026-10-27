//! Settled view-state changes of one window.
//!
//! Editor cursor, selection, and scroll changes, focus, dock resizing, window
//! bounds, and shell changes restart a trailing-edge debounce. When it settles
//! the window captures its layout and, when it differs from the last one
//! published, reports `viewStateChanged` to extension observers.

use std::time::Duration;

use super::*;
use crate::app::window_layout::WindowLayout;

/// Trailing-edge debounce before a window reports a settled view state.
pub(super) const VIEW_STATE_DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(super) struct ViewStateTracking {
    pending: Option<Task<()>>,
    published: Option<WindowLayout>,
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
            let subscription = cx.observe(&editor, |this, _, cx| this.schedule_view_state(cx));
            self.view_state
                .editors
                .push((editor.entity_id(), subscription));
        }
    }

    /// Restart this window's view-state debounce.
    pub(super) fn schedule_view_state(&mut self, cx: &mut Context<Self>) {
        let Some(handle) = self.window else {
            return;
        };
        self.view_state.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(VIEW_STATE_DEBOUNCE).await;
            let changed = cx
                .update_window(handle, |_, window, cx| {
                    this.update(cx, |shell, cx| shell.settle_view_state(window, cx))
                        .ok()
                        .flatten()
                })
                .ok()
                .flatten();
            if let Some(target) = changed {
                let _ = cx.update(|cx| crate::app::extension_host::view_state_changed(target, cx));
            }
        }));
    }

    /// Record the settled layout, returning this window's target when it
    /// differs from the last one published.
    fn settle_view_state(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<crate::host::protocol::WindowTarget> {
        let layout = self.capture_layout(window, cx);
        if self.view_state.published.as_ref() == Some(&layout) {
            return None;
        }
        self.view_state.published = Some(layout);
        Some(crate::host::protocol::WindowTarget {
            window: window.window_handle().window_id().as_u64().to_string(),
        })
    }
}
