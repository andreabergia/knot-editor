//! Keyboard navigation between tabs and panes, and the maximized pane.
//!
//! A recent-tab walk shows tabs in the pane's recently used order without
//! changing that order. Releasing ctrl commits the walk as one activation of
//! the tab it ended on; a walk started without ctrl held commits at once.
//!
//! Directional commands find a neighbor from pane rectangles laid out by
//! split ratios and dock sizes over the painted workbench, so they work while
//! a pane is maximized. Maximize is window-local render state that lasts only
//! while its pane stays focused.

use super::relocate::TabRelocationDestination;
use super::*;
use crate::app::window_layout::Rect;
use crate::app::workbench::{SplitEdge, TabMoveDestination, TabMovePlacement};
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

impl ProductShell {
    /// The pane beside `pane` toward `edge`.
    fn neighbor(&self, pane: PaneId, edge: SplitEdge, cx: &App) -> Option<PaneId> {
        let area = self
            .tab_drag
            .borrow()
            .workbench_bounds()
            .map(|bounds| Rect {
                x: 0.,
                y: 0.,
                width: f32::from(bounds.size.width),
                height: f32::from(bounds.size.height),
            })
            .unwrap_or(Rect {
                x: 0.,
                y: 0.,
                width: 1200.,
                height: 800.,
            });
        let rects = self.workbench.read(cx).pane_rects(area);
        crate::app::workbench::neighbor(&rects, pane, edge)
    }

    /// Focus the pane beside `pane` toward `edge`, restoring the layout.
    pub(super) fn focus_pane_toward(
        &mut self,
        pane: PaneId,
        edge: SplitEdge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), CommandOutcome> {
        self.maximized = None;
        cx.notify();
        let neighbor = self
            .neighbor(pane, edge, cx)
            .ok_or(CommandOutcome::Unavailable)?;
        self.workbench
            .update(cx, |workbench, _| workbench.focus_pane(neighbor));
        self.focus_active_surface(window, cx);
        Ok(())
    }

    /// Move a tab into the pane beside its own toward `edge`, or split its
    /// pane on that side when no pane lies there.
    pub(super) fn move_tab_toward(
        &mut self,
        pane: PaneId,
        tab: TabId,
        edge: SplitEdge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), CommandOutcome> {
        self.maximized = None;
        cx.notify();
        let source = self
            .capture_tab_relocation(pane, tab, cx)
            .ok_or(CommandOutcome::InvalidTarget)?;
        let workbench = self.workbench.read(cx);
        let target = match self.neighbor(pane, edge, cx) {
            Some(neighbor) => TabMoveDestination::At {
                anchor: workbench
                    .capture_tab_move(neighbor, workbench.pane(neighbor).unwrap().active_tab_id())
                    .ok_or(CommandOutcome::InvalidTarget)?,
                placement: TabMovePlacement::End,
            },
            None if workbench
                .pane(pane)
                .is_some_and(|pane| pane.tabs().len() > 1) =>
            {
                TabMoveDestination::At {
                    anchor: source.tab,
                    placement: TabMovePlacement::Split(edge),
                }
            }
            None => return Err(CommandOutcome::Unavailable),
        };
        let destination = TabRelocationDestination {
            window: window.window_handle(),
            workbench: self.workbench.downgrade(),
            tab: target,
        };
        match self.relocate_tab_to(&source, &destination, window, cx) {
            CommandOutcome::Completed => Ok(()),
            outcome => Err(outcome),
        }
    }

    /// Render only `pane`, or the whole layout again.
    pub(super) fn toggle_maximize(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        self.maximized = if self.maximized == Some(pane) {
            None
        } else {
            Some(pane)
        };
        cx.notify();
    }

    /// The maximized pane while it is still focused; anything else ends it.
    pub(super) fn maximized_pane(&mut self, cx: &App) -> Option<PaneId> {
        let focused = self.workbench.read(cx).focused_pane_id();
        self.maximized = self.maximized.filter(|pane| focused == Some(*pane));
        self.maximized
    }
}
