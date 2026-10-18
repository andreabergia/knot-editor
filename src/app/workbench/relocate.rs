//! Atomic relocation of one tab without document or session closure.

use gpui::{Entity, EntityId};

use super::{
    DockLocation, Pane, PaneId, SplitDirection, SplitPlacement, TabId, TerminalView, Workbench,
    WorkbenchLayout, WorkbenchTab, WorkbenchTabPayload, WorkbenchTabSnapshot,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TabMoveCapture {
    pub(crate) tab: WorkbenchTabSnapshot,
    pub(crate) presentation: Option<EntityId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SplitEdge {
    Left,
    Right,
    Top,
    Bottom,
}

impl SplitEdge {
    fn split(self) -> (SplitDirection, SplitPlacement) {
        match self {
            Self::Left => (SplitDirection::Horizontal, SplitPlacement::Before),
            Self::Right => (SplitDirection::Horizontal, SplitPlacement::After),
            Self::Top => (SplitDirection::Vertical, SplitPlacement::Before),
            Self::Bottom => (SplitDirection::Vertical, SplitPlacement::After),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TabMovePlacement {
    /// Insert immediately before the captured destination tab.
    Before,
    /// Append to the captured tab's pane.
    End,
    Split(SplitEdge),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TabMoveDestination {
    At {
        anchor: TabMoveCapture,
        placement: TabMovePlacement,
    },
    /// Move to a sidebar or bottom dock, creating the dock group around the
    /// whole workbench when it is not visible. Produced by workbench-edge
    /// drops; pane-edge splits keep using `At` with `Split`.
    Dock(DockLocation),
    /// Install the first pane in an empty workbench.
    Empty,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TabMoveTransition {
    pub(crate) destination: WorkbenchTabSnapshot,
    pub(crate) removed_pane: Option<PaneId>,
    pub(crate) source_is_empty: bool,
}

struct PreparedDestination {
    pane: PaneId,
    tab: TabId,
    index: usize,
    new_pane: bool,
    new_tab: bool,
    dock: Option<DockLocation>,
    layout: WorkbenchLayout,
}

impl Workbench {
    pub(crate) fn capture_tab_move(&self, pane: PaneId, tab: TabId) -> Option<TabMoveCapture> {
        let tab = self
            .pane(pane)?
            .tabs
            .iter()
            .find(|candidate| candidate.id == tab)?;
        let presentation = tab.presentation();
        Some(TabMoveCapture {
            tab: WorkbenchTabSnapshot {
                pane_id: pane,
                tab_id: tab.id,
                surface: tab.surface_id(),
            },
            presentation,
        })
    }

    pub(crate) fn contains_tab_move(&self, captured: TabMoveCapture) -> bool {
        self.capture_tab_move(captured.tab.pane_id, captured.tab.tab_id) == Some(captured)
    }

    pub(crate) fn accepts_tab_move(&self, target: TabMoveDestination) -> bool {
        match target {
            TabMoveDestination::Empty => self.layout.is_none() && self.panes.is_empty(),
            TabMoveDestination::Dock(_) => true,
            TabMoveDestination::At { anchor, .. } => self.contains_tab_move(anchor),
        }
    }

    fn prepare_destination(
        &self,
        target: TabMoveDestination,
        reused_tab: Option<TabId>,
    ) -> Option<PreparedDestination> {
        if !self.accepts_tab_move(target) {
            return None;
        }
        let tab = reused_tab.unwrap_or(TabId(self.next_tab_id));
        let (pane, index, new_pane, dock, layout) = match target {
            TabMoveDestination::Empty => {
                let pane = PaneId(self.next_pane_id);
                (pane, 0, true, None, WorkbenchLayout::Pane(pane))
            }
            TabMoveDestination::Dock(dock) => {
                if let Some(pane) = self
                    .docks
                    .iter()
                    .find(|(_, location)| *location == dock)
                    .map(|(pane, _)| *pane)
                {
                    let index = self.pane(pane).map(|pane| pane.tabs.len()).unwrap_or(0);
                    let layout = self.layout.clone().unwrap_or(WorkbenchLayout::Pane(pane));
                    (pane, index, false, Some(dock), layout)
                } else {
                    let pane = PaneId(self.next_pane_id);
                    let leaf = WorkbenchLayout::Pane(pane);
                    let layout = match self.layout.clone() {
                        None => leaf,
                        Some(root) => {
                            let (direction, first, second) = match dock {
                                DockLocation::Left => (SplitDirection::Horizontal, leaf, root),
                                DockLocation::Right => (SplitDirection::Horizontal, root, leaf),
                                DockLocation::Bottom => (SplitDirection::Vertical, root, leaf),
                            };
                            WorkbenchLayout::Split {
                                direction,
                                first: Box::new(first),
                                second: Box::new(second),
                            }
                        }
                    };
                    (pane, 0, true, Some(dock), layout)
                }
            }
            TabMoveDestination::At { anchor, placement } => {
                let pane = self.pane(anchor.tab.pane_id)?;
                let mut layout = self.layout.clone()?;
                match placement {
                    TabMovePlacement::Before => (
                        pane.id,
                        pane.tabs
                            .iter()
                            .position(|tab| tab.id == anchor.tab.tab_id)?,
                        false,
                        None,
                        layout,
                    ),
                    TabMovePlacement::End => (pane.id, pane.tabs.len(), false, None, layout),
                    TabMovePlacement::Split(edge) => {
                        let new_pane = PaneId(self.next_pane_id);
                        let (direction, placement) = edge.split();
                        assert!(layout.split_pane(pane.id, new_pane, direction, placement));
                        (new_pane, 0, true, None, layout)
                    }
                }
            }
        };
        Some(PreparedDestination {
            pane,
            tab,
            index,
            new_pane,
            new_tab: reused_tab.is_none(),
            dock,
            layout,
        })
    }

    fn install_moved_tab(
        &mut self,
        mut tab: WorkbenchTab,
        prepared: PreparedDestination,
    ) -> WorkbenchTabSnapshot {
        tab.id = prepared.tab;
        let snapshot = WorkbenchTabSnapshot {
            pane_id: prepared.pane,
            tab_id: tab.id,
            surface: tab.surface_id(),
        };
        if prepared.new_tab {
            assert_eq!(self.allocate_tab_id(), tab.id);
        }
        if prepared.new_pane {
            assert_eq!(self.allocate_pane_id(), prepared.pane);
            if let Some(dock) = prepared.dock {
                self.docks.push((prepared.pane, dock));
            }
            self.panes.push(Pane {
                id: prepared.pane,
                tabs: vec![tab],
                active_tab: snapshot.tab_id,
            });
        } else {
            let pane = self
                .panes
                .iter_mut()
                .find(|pane| pane.id == prepared.pane)
                .expect("validated destination pane");
            pane.tabs.insert(prepared.index, tab);
            pane.active_tab = snapshot.tab_id;
        }
        self.layout = Some(prepared.layout);
        self.focused_pane = Some(prepared.pane);
        self.debug_assert_invariants();
        snapshot
    }

    pub(crate) fn relocate_tab(
        &mut self,
        source: TabMoveCapture,
        target: TabMoveDestination,
    ) -> Option<TabMoveTransition> {
        if !self.contains_tab_move(source) {
            return None;
        }
        let mut prepared = self.prepare_destination(target, Some(source.tab.tab_id))?;
        if prepared.pane == source.tab.pane_id {
            let pane = self
                .panes
                .iter_mut()
                .find(|pane| pane.id == prepared.pane)?;
            let index = pane
                .tabs
                .iter()
                .position(|tab| tab.id == source.tab.tab_id)?;
            let tab = pane.tabs.remove(index);
            if index < prepared.index {
                prepared.index -= 1;
            }
            let destination = self.install_moved_tab(tab, prepared);
            return Some(TabMoveTransition {
                destination,
                removed_pane: None,
                source_is_empty: false,
            });
        }
        if self.pane(source.tab.pane_id)?.tabs.len() == 1 {
            prepared.layout = prepared
                .layout
                .remove_pane(source.tab.pane_id)
                .expect("destination survives source pane collapse");
        }
        let removed = self
            .remove_tab(source.tab.pane_id, source.tab.tab_id)
            .expect("validated source tab");
        let destination = self.install_moved_tab(removed.tab, prepared);
        Some(TabMoveTransition {
            destination,
            removed_pane: removed.removed_pane,
            source_is_empty: false,
        })
    }

    /// Validate both layouts before removing a payload or invoking terminal handoff.
    pub(crate) fn transfer_tab_to(
        &mut self,
        destination: &mut Workbench,
        source: TabMoveCapture,
        target: TabMoveDestination,
        handoff: impl FnOnce(Entity<TerminalView>) -> Entity<TerminalView>,
    ) -> Option<TabMoveTransition> {
        if !self.contains_tab_move(source)
            || matches!(source.tab.surface, super::TabSurfaceId::Tool(_))
        {
            return None;
        }
        if let super::TabSurfaceId::Terminal(session) = source.tab.surface
            && destination.has_terminal_session(session)
        {
            return None;
        }
        let prepared = destination.prepare_destination(target, None)?;
        let mut removed = self
            .remove_tab(source.tab.pane_id, source.tab.tab_id)
            .expect("validated source tab");
        if let WorkbenchTabPayload::Terminal { view, .. } = &mut removed.tab.payload
            && let Some(source_view) = view.take()
        {
            *view = Some(handoff(source_view));
        }
        let snapshot = destination.install_moved_tab(removed.tab, prepared);
        self.debug_assert_invariants();
        Some(TabMoveTransition {
            destination: snapshot,
            removed_pane: removed.removed_pane,
            source_is_empty: self.panes.is_empty(),
        })
    }
}

#[cfg(test)]
mod tests;
