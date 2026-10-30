//! Keyboard navigation between the tabs of a pane.
//!
//! A recent-tab walk shows tabs in the pane's recently used order without
//! changing that order. Releasing ctrl commits the walk as one activation of
//! the tab it ended on; a walk started without ctrl held commits at once.

use super::*;
use crate::host::protocol::CommandOutcome;

/// An uncommitted walk through one pane's recently used tabs.
pub(super) struct RecentWalk {
    pane: PaneId,
    order: Vec<TabId>,
    index: usize,
}

impl ProductShell {
    /// Activate the next or previous tab of a pane in tab order, wrapping.
    pub(super) fn cycle_tab(
        &mut self,
        pane: PaneId,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), CommandOutcome> {
        let tab = self
            .workbench
            .read(cx)
            .adjacent_tab(pane, forward)
            .ok_or(CommandOutcome::InvalidTarget)?;
        self.activate_tab(pane, tab, window, cx);
        Ok(())
    }

    /// Activate a pane's tab by position, or its last tab for `None`.
    pub(super) fn go_to_tab(
        &mut self,
        pane: PaneId,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), CommandOutcome> {
        let tabs = self
            .workbench
            .read(cx)
            .pane(pane)
            .ok_or(CommandOutcome::InvalidTarget)?
            .tabs();
        let tab = match index {
            Some(index) => tabs.get(index),
            None => tabs.last(),
        }
        .map(|tab| tab.id())
        .ok_or(CommandOutcome::Unavailable)?;
        self.activate_tab(pane, tab, window, cx);
        Ok(())
    }

    /// Step a recent-tab walk of the pane forward or back, starting one.
    pub(super) fn walk_recent_tabs(
        &mut self,
        pane: PaneId,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), CommandOutcome> {
        if self
            .recent_walk
            .as_ref()
            .is_some_and(|walk| walk.pane != pane)
        {
            self.commit_recent_walk(cx);
        }
        let order = self
            .workbench
            .read(cx)
            .pane(pane)
            .ok_or(CommandOutcome::InvalidTarget)?
            .recent_tabs();
        if order.len() < 2 {
            return Err(CommandOutcome::Unavailable);
        }
        let walk = self.recent_walk.get_or_insert_with(|| RecentWalk {
            pane,
            order,
            index: 0,
        });
        let count = walk.order.len();
        walk.index = if forward {
            (walk.index + 1) % count
        } else {
            (walk.index + count - 1) % count
        };
        let tab = walk.order[walk.index];
        let shown = self
            .workbench
            .update(cx, |workbench, _| workbench.show_tab(pane, tab, false));
        if !shown {
            self.recent_walk = None;
            return Err(CommandOutcome::InvalidTarget);
        }
        self.focus_active_surface(window, cx);
        if !window.modifiers().control {
            self.commit_recent_walk(cx);
        }
        cx.notify();
        Ok(())
    }

    /// End a recent-tab walk, recording the tab it ended on as activated.
    pub(super) fn commit_recent_walk(&mut self, cx: &mut Context<Self>) {
        let Some(walk) = self.recent_walk.take() else {
            return;
        };
        let tab = walk.order[walk.index];
        self.workbench.update(cx, |workbench, _| {
            let pane = workbench.pane(walk.pane);
            if pane.is_some_and(|pane| pane.active_tab_id() == tab) {
                workbench.activate_tab(walk.pane, tab);
            }
        });
    }

    pub(super) fn modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.modifiers.control {
            self.commit_recent_walk(cx);
        }
    }
}
