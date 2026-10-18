//! Native tool definitions outlive their visible tab attachments.

use gpui::{EntityId, FocusHandle};

use super::*;
use crate::app::tool_layout::{StoredLocation, StoredTool, ToolLayoutSnapshot};
use crate::app::workspace_tree::WorkspaceTree;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ToolFrameId(EntityId);

#[derive(Clone)]
pub(crate) enum ToolContent {
    Workspace(Entity<WorkspaceTree>),
}

impl ToolContent {
    pub(crate) fn presentation(&self) -> EntityId {
        match self {
            Self::Workspace(view) => view.entity_id(),
        }
    }

    pub(crate) fn command_view(&self) -> CommandView {
        match self {
            Self::Workspace(view) => CommandView::Workspace(view.clone()),
        }
    }

    pub(crate) fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Self::Workspace(view) => view.focus_handle(cx),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DockLocation {
    Left,
    Right,
    Bottom,
}

impl DockLocation {
    pub(crate) const ALL: [Self; 3] = [Self::Left, Self::Right, Self::Bottom];

    /// Usable dock extents in logical pixels. Dividers clamp to this range so
    /// a dragged dock stays visible without squeezing the editor shut.
    pub(crate) const MIN_SIZE: f32 = 80.;
    pub(crate) const MAX_SIZE: f32 = 480.;

    pub(crate) fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Bottom => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToolPlacement {
    Dock(DockLocation),
    Group(PaneId),
    DocumentGroup,
}

pub(crate) struct ToolFrame {
    pub(crate) id: ToolFrameId,
    pub(crate) stable_id: &'static str,
    pub(crate) title: &'static str,
    pub(crate) content: ToolContent,
    remembered: ToolPlacement,
    index: usize,
}

impl WorkbenchTab {
    pub(crate) fn tool_content(&self) -> Option<&ToolContent> {
        match &self.payload {
            WorkbenchTabPayload::Tool { content, .. } => Some(content),
            _ => None,
        }
    }

    pub(crate) fn presentation(&self) -> Option<EntityId> {
        self.editor()
            .map(Entity::entity_id)
            .or_else(|| self.terminal_view().map(Entity::entity_id))
            .or_else(|| self.tool_content().map(ToolContent::presentation))
    }
}

impl Workbench {
    pub(crate) fn register_tool(
        &mut self,
        stable_id: &'static str,
        title: &'static str,
        content: ToolContent,
        dock: DockLocation,
    ) -> ToolFrameId {
        let id = ToolFrameId(content.presentation());
        if !self.tools.iter().any(|tool| tool.id == id) {
            self.tools.push(ToolFrame {
                id,
                stable_id,
                title,
                content,
                remembered: ToolPlacement::Dock(dock),
                index: usize::MAX,
            });
        }
        id
    }

    pub(crate) fn tools(&self) -> &[ToolFrame] {
        &self.tools
    }

    pub(crate) fn tool(&self, id: ToolFrameId) -> Option<&ToolFrame> {
        self.tools.iter().find(|tool| tool.id == id)
    }

    pub(crate) fn tool_tab(&self, id: ToolFrameId) -> Option<(PaneId, TabId)> {
        self.panes.iter().find_map(|pane| {
            pane.tabs
                .iter()
                .find(|tab| tab.surface_id() == TabSurfaceId::Tool(id))
                .map(|tab| (pane.id, tab.id))
        })
    }

    pub(crate) fn dock_location(&self, pane: PaneId) -> Option<DockLocation> {
        self.docks
            .iter()
            .find(|(id, _)| *id == pane)
            .map(|(_, dock)| *dock)
    }

    pub(crate) fn dock_pane(&self, dock: DockLocation) -> Option<PaneId> {
        self.docks
            .iter()
            .find(|(_, location)| *location == dock)
            .map(|(pane, _)| *pane)
    }

    pub(crate) fn dock_size(&self, dock: DockLocation) -> f32 {
        self.dock_sizes[dock.index()]
    }

    pub(crate) fn set_dock_size(&mut self, dock: DockLocation, size: f32) {
        self.dock_sizes[dock.index()] = size.clamp(DockLocation::MIN_SIZE, DockLocation::MAX_SIZE);
        self.debug_assert_invariants();
    }

    pub(crate) fn set_dock_sizes(&mut self, sizes: [f32; 3]) {
        for dock in DockLocation::ALL {
            self.dock_sizes[dock.index()] =
                sizes[dock.index()].clamp(DockLocation::MIN_SIZE, DockLocation::MAX_SIZE);
        }
        self.debug_assert_invariants();
    }

    pub(crate) fn tool_by_stable_id(&self, stable_id: &str) -> Option<ToolFrameId> {
        self.tools
            .iter()
            .find(|tool| tool.stable_id == stable_id)
            .map(|tool| tool.id)
    }

    /// Remember a placement without showing the tool. Restore uses this for
    /// hidden tools so remembered locations survive without visible tabs.
    pub(crate) fn remember_tool_placement(
        &mut self,
        id: ToolFrameId,
        placement: ToolPlacement,
        index: usize,
    ) {
        if let Some(tool) = self.tools.iter_mut().find(|tool| tool.id == id) {
            tool.remembered = placement;
            tool.index = index;
        }
        self.debug_assert_invariants();
    }

    /// Capture the tool-only layout record. Visible tools in ordinary groups
    /// flatten to the document group; hidden tools resolve vanished panes to
    /// the document group. Documents, terminals, and live handles never enter
    /// the record: docks map to stable locations and selection maps to
    /// stable namespaced IDs.
    pub(crate) fn tool_layout_snapshot(&self) -> ToolLayoutSnapshot {
        let stable = |id: ToolFrameId| {
            self.tools
                .iter()
                .find(|tool| tool.id == id)
                .map(|tool| tool.stable_id.to_owned())
        };
        let tools = self
            .tools
            .iter()
            .map(|tool| {
                if let Some((pane, tab)) = self.tool_tab(tool.id) {
                    let index = self
                        .pane(pane)
                        .unwrap()
                        .tabs
                        .iter()
                        .position(|item| item.id == tab)
                        .unwrap();
                    let location = match self.dock_location(pane) {
                        Some(dock) => StoredLocation::from(dock),
                        None => StoredLocation::DocumentGroup,
                    };
                    StoredTool {
                        id: tool.stable_id.to_owned(),
                        location,
                        index,
                        visible: true,
                    }
                } else {
                    let location = match tool.remembered {
                        ToolPlacement::Dock(dock) => StoredLocation::from(dock),
                        ToolPlacement::DocumentGroup => StoredLocation::DocumentGroup,
                        ToolPlacement::Group(pane) => match self.dock_location(pane) {
                            Some(dock) => StoredLocation::from(dock),
                            None => StoredLocation::DocumentGroup,
                        },
                    };
                    StoredTool {
                        id: tool.stable_id.to_owned(),
                        location,
                        index: tool.index,
                        visible: false,
                    }
                }
            })
            .collect();
        ToolLayoutSnapshot {
            version: crate::app::tool_layout::TOOL_LAYOUT_VERSION,
            tools,
            dock_sizes: self.dock_sizes,
            dock_selected: DockLocation::ALL.map(|dock| {
                // A visible dock's selection is its active tab; a hidden dock
                // keeps its remembered selection so reopening restores it.
                if let Some((pane, _)) = self.docks.iter().find(|(_, location)| *location == dock)
                    && let Some(TabSurfaceId::Tool(id)) =
                        self.pane(*pane).map(|pane| pane.active_tab().surface_id())
                {
                    return stable(id);
                }
                self.dock_selected[dock.index()].and_then(stable)
            }),
        }
    }

    pub(crate) fn normal_pane(&self) -> Option<PaneId> {
        self.focused_pane
            .filter(|pane| self.dock_location(*pane).is_none())
            .or_else(|| {
                self.panes
                    .iter()
                    .find(|pane| self.dock_location(pane.id).is_none())
                    .map(Pane::id)
            })
    }

    pub(crate) fn hide_tool(&mut self, id: ToolFrameId) -> bool {
        let Some((pane, tab)) = self.tool_tab(id) else {
            return false;
        };
        let placement = self
            .dock_location(pane)
            .map(ToolPlacement::Dock)
            .unwrap_or(ToolPlacement::Group(pane));
        let index = self
            .pane(pane)
            .unwrap()
            .tabs
            .iter()
            .position(|item| item.id == tab)
            .unwrap();
        let tool = self.tools.iter_mut().find(|tool| tool.id == id).unwrap();
        tool.remembered = placement;
        tool.index = index;
        self.remove_tab(pane, tab);
        true
    }

    pub(crate) fn show_tool(&mut self, id: ToolFrameId) -> Option<(PaneId, TabId)> {
        if let Some((pane, tab)) = self.tool_tab(id) {
            self.activate_tab(pane, tab);
            return Some((pane, tab));
        }
        let tool = self.tool(id)?;
        let placement = tool.remembered;
        let index = tool.index;
        let content = tool.content.clone();
        let pane = match placement {
            ToolPlacement::Dock(dock) => self.ensure_dock(dock),
            ToolPlacement::Group(pane) if self.pane(pane).is_some() => pane,
            ToolPlacement::Group(_) | ToolPlacement::DocumentGroup => self
                .normal_pane()
                .unwrap_or_else(|| self.create_group(None)),
        };
        let tab = self.allocate_tab_id();
        let group = self
            .panes
            .iter_mut()
            .find(|group| group.id == pane)
            .unwrap();
        group.tabs.insert(
            index.min(group.tabs.len()),
            WorkbenchTab {
                id: tab,
                payload: WorkbenchTabPayload::Tool { id, content },
            },
        );
        group.active_tab = tab;
        self.focused_pane = Some(pane);
        self.debug_assert_invariants();
        Some((pane, tab))
    }

    pub(crate) fn place_tool(&mut self, id: ToolFrameId, placement: ToolPlacement) -> bool {
        if self.tool(id).is_none()
            || matches!(placement, ToolPlacement::Group(pane) if self.pane(pane).is_none() || self.dock_location(pane).is_some())
        {
            return false;
        }
        // A group that contains only this tool would disappear during detach.
        if matches!(placement, ToolPlacement::Group(pane) if self.tool_tab(id).is_some_and(|(source, _)| source == pane))
        {
            return true;
        }
        self.hide_tool(id);
        let tool = self.tools.iter_mut().find(|tool| tool.id == id).unwrap();
        tool.remembered = placement;
        tool.index = usize::MAX;
        self.show_tool(id).is_some()
    }

    pub(crate) fn toggle_dock(&mut self, dock: DockLocation) {
        let visible = self
            .docks
            .iter()
            .find(|(_, location)| *location == dock)
            .map(|(pane, _)| *pane);
        if let Some(pane) = visible {
            let ids = self
                .pane(pane)
                .unwrap()
                .tabs
                .iter()
                .filter_map(|tab| match tab.surface_id() {
                    TabSurfaceId::Tool(id) => Some(id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            self.dock_selected[dock.index()] =
                match self.pane(pane).unwrap().active_tab().surface_id() {
                    TabSurfaceId::Tool(id) => Some(id),
                    _ => None,
                };
            for id in ids.into_iter().rev() {
                self.hide_tool(id);
            }
        } else {
            let mut ids = self
                .tools
                .iter()
                .filter(|tool| tool.remembered == ToolPlacement::Dock(dock))
                .map(|tool| (tool.index, tool.id))
                .collect::<Vec<_>>();
            ids.sort_by_key(|(index, _)| *index);
            for (_, id) in ids {
                self.show_tool(id);
            }
            if let Some(id) = self.dock_selected[dock.index()] {
                self.show_tool(id);
            }
        }
    }

    fn ensure_dock(&mut self, dock: DockLocation) -> PaneId {
        self.docks
            .iter()
            .find(|(_, location)| *location == dock)
            .map(|(pane, _)| *pane)
            .unwrap_or_else(|| self.create_group(Some(dock)))
    }

    fn create_group(&mut self, dock: Option<DockLocation>) -> PaneId {
        let pane = self.allocate_pane_id();
        let leaf = WorkbenchLayout::Pane(pane);
        self.layout = Some(match self.layout.take() {
            None => leaf,
            Some(mut layout) if dock.is_none() => {
                let target = layout.first_pane();
                let (direction, placement) = match self.dock_location(target) {
                    Some(DockLocation::Left) => (SplitDirection::Horizontal, SplitPlacement::After),
                    Some(DockLocation::Right) => {
                        (SplitDirection::Horizontal, SplitPlacement::Before)
                    }
                    Some(DockLocation::Bottom) => {
                        (SplitDirection::Vertical, SplitPlacement::Before)
                    }
                    None => (SplitDirection::Horizontal, SplitPlacement::After),
                };
                assert!(layout.split_pane(target, pane, direction, placement));
                layout
            }
            Some(layout) => {
                let (direction, first, second) = match dock {
                    Some(DockLocation::Left) => (SplitDirection::Horizontal, leaf, layout),
                    Some(DockLocation::Right) | None => (SplitDirection::Horizontal, layout, leaf),
                    Some(DockLocation::Bottom) => (SplitDirection::Vertical, layout, leaf),
                };
                WorkbenchLayout::Split {
                    direction,
                    first: Box::new(first),
                    second: Box::new(second),
                }
            }
        });
        if let Some(dock) = dock {
            self.docks.push((pane, dock));
        }
        // The caller installs the first tab before publishing this transition.
        self.panes.push(Pane {
            id: pane,
            tabs: Vec::new(),
            active_tab: TabId(0),
        });
        pane
    }

    pub(crate) fn open_terminal_in_normal_group(
        &mut self,
        session: TerminalSessionId,
        view: Entity<TerminalView>,
    ) -> Option<TabId> {
        if self.has_terminal_session(session) {
            return None;
        }
        let pane = self
            .normal_pane()
            .unwrap_or_else(|| self.create_group(None));
        self.open_terminal_tab(pane, session, view)
    }

    pub(crate) fn open_document_in_normal_group(
        &mut self,
        document: DocumentId,
        model: Entity<super::super::model::BufferModel>,
        cx: &mut Context<Self>,
    ) -> (PaneId, TabId) {
        let pane = self
            .normal_pane()
            .unwrap_or_else(|| self.create_group(None));
        let tab = self
            .open_tab_for_document(pane, document, model, cx)
            .unwrap();
        (pane, tab)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{model::BufferModel, workspace::WorkspaceState};
    use gpui::TestAppContext;

    fn explorer(workbench: &mut Workbench, cx: &mut Context<Workbench>) -> ToolFrameId {
        let tree = cx.new(|cx| WorkspaceTree::new(WorkspaceState::default().snapshot(), cx));
        workbench.register_tool(
            crate::app::tool_layout::EXPLORER_TOOL_ID,
            "Explorer",
            ToolContent::Workspace(tree),
            DockLocation::Left,
        )
    }

    #[gpui::test]
    fn tools_preserve_native_identity_group_order_and_dock_size(cx: &mut TestAppContext) {
        let workbench = cx.new(|_| Workbench::empty());
        workbench.update(cx, |workbench, cx| {
            let model = cx.new(|_| BufferModel::from_text(""));
            let mut documents = crate::app::documents::DocumentCollection::new();
            let document = documents.create_untitled("doc", model.clone(), cx);
            let (pane, doc_tab) = workbench.open_document_in_normal_group(document, model, cx);
            let id = explorer(workbench, cx);
            let presentation = workbench.tool(id).unwrap().content.presentation();
            let (dock, _) = workbench.show_tool(id).unwrap();
            assert_eq!(workbench.dock_location(dock), Some(DockLocation::Left));
            workbench.set_dock_size(DockLocation::Left, 315.);
            assert!(workbench.hide_tool(id));
            assert!(workbench.pane(dock).is_none());
            assert!(matches!(workbench.layout(), Some(WorkbenchLayout::Pane(id)) if *id == pane));
            let (reopened, _) = workbench.show_tool(id).unwrap();
            assert_ne!(reopened, dock);
            assert_eq!(workbench.dock_size(DockLocation::Left), 315.);
            assert_eq!(
                workbench.tool(id).unwrap().content.presentation(),
                presentation
            );
            assert!(workbench.place_tool(id, ToolPlacement::Group(pane)));
            let (_, tool_tab) = workbench.tool_tab(id).unwrap();
            let capture = workbench.capture_tab_move(pane, tool_tab).unwrap();
            assert_eq!(
                workbench
                    .pane(pane)
                    .unwrap()
                    .tabs()
                    .iter()
                    .map(WorkbenchTab::id)
                    .collect::<Vec<_>>(),
                [doc_tab, tool_tab]
            );
            assert_eq!(workbench.view_count(document), 1);
            workbench.hide_tool(id);
            assert!(!workbench.contains_tab_move(capture));
            let (same_pane, new_tab) = workbench.show_tool(id).unwrap();
            assert_eq!(same_pane, pane);
            assert_ne!(new_tab, tool_tab);
            assert_eq!(
                workbench.pane(pane).unwrap().tabs()[1].surface_id(),
                TabSurfaceId::Tool(id)
            );
        });
    }

    #[gpui::test]
    fn vanished_mixed_group_reopens_without_historical_document_layout(cx: &mut TestAppContext) {
        let workbench = cx.new(|_| Workbench::empty());
        workbench.update(cx, |workbench, cx| {
            let id = explorer(workbench, cx);
            workbench.place_tool(id, ToolPlacement::DocumentGroup);
            let old = workbench.tool_tab(id).unwrap().0;
            workbench.hide_tool(id);
            assert!(workbench.panes().is_empty());
            let (new, _) = workbench.show_tool(id).unwrap();
            assert_ne!(new, old);
            assert_eq!(workbench.panes().len(), 1);
            assert_eq!(workbench.panes()[0].tabs().len(), 1);
            assert_eq!(workbench.dock_location(new), None);
            workbench.place_tool(id, ToolPlacement::Dock(DockLocation::Bottom));
            let bottom = workbench.tool_tab(id).unwrap().0;
            assert_eq!(workbench.dock_location(bottom), Some(DockLocation::Bottom));
            workbench.toggle_dock(DockLocation::Bottom);
            assert!(workbench.panes().is_empty());
            workbench.toggle_dock(DockLocation::Bottom);
            assert_eq!(workbench.panes().len(), 1);
            let model = cx.new(|_| BufferModel::from_text(""));
            let mut documents = crate::app::documents::DocumentCollection::new();
            let document = documents.create_untitled("doc", model.clone(), cx);
            workbench.open_document_in_normal_group(document, model, cx);
            assert!(matches!(workbench.layout(), Some(WorkbenchLayout::Split { direction: SplitDirection::Vertical, second, .. }) if **second == WorkbenchLayout::Pane(workbench.tool_tab(id).unwrap().0)));
            let capture = workbench.capture_tab_move(workbench.tool_tab(id).unwrap().0, workbench.tool_tab(id).unwrap().1).unwrap();
            let mut destination = Workbench::empty();
            assert!(workbench.transfer_tab_to(&mut destination, capture, TabMoveDestination::Empty, |view| view).is_none());
            assert!(destination.panes().is_empty());
            assert!(workbench.contains_tab_move(capture));
        });
    }
    #[gpui::test]
    fn dock_toggle_restores_selector_order_and_selection(cx: &mut gpui::TestAppContext) {
        let workbench = cx.new(|_| Workbench::empty());
        workbench.update(cx, |workbench, cx| {
            let ids = [
                ("knot.first", "First"),
                ("knot.second", "Second"),
                ("knot.third", "Third"),
            ]
            .map(|(stable_id, title)| {
                let tree = cx.new(|cx| {
                    WorkspaceTree::new(
                        crate::app::workspace::WorkspaceState::default().snapshot(),
                        cx,
                    )
                });
                let id = workbench.register_tool(
                    stable_id,
                    title,
                    ToolContent::Workspace(tree),
                    DockLocation::Right,
                );
                workbench.show_tool(id);
                id
            });
            workbench.show_tool(ids[1]);
            workbench.toggle_dock(DockLocation::Right);
            assert!(workbench.layout().is_none());
            workbench.toggle_dock(DockLocation::Right);
            let pane = workbench.focused_pane().unwrap();
            assert_eq!(
                pane.tabs()
                    .iter()
                    .map(WorkbenchTab::surface_id)
                    .collect::<Vec<_>>(),
                ids.map(TabSurfaceId::Tool)
            );
            assert_eq!(pane.active_tab().surface_id(), TabSurfaceId::Tool(ids[1]));
        });
    }

    #[gpui::test]
    fn tool_moves_between_groups_edges_and_docks_preserve_frame(cx: &mut gpui::TestAppContext) {
        let workbench = cx.new(|_| Workbench::empty());
        workbench.update(cx, |workbench, cx| {
            let model = cx.new(|_| BufferModel::from_text(""));
            let mut documents = crate::app::documents::DocumentCollection::new();
            let document = documents.create_untitled("doc", model.clone(), cx);
            let (doc_pane, _) = workbench.open_document_in_normal_group(document, model, cx);
            let id = explorer(workbench, cx);
            let presentation = workbench.tool(id).unwrap().content.presentation();
            let (dock, _) = workbench.show_tool(id).unwrap();
            assert_eq!(workbench.dock_location(dock), Some(DockLocation::Left));

            // Pane-edge split moves the frame next to the document group and
            // collapses the emptied dock.
            let source = workbench
                .capture_tab_move(dock, workbench.tool_tab(id).unwrap().1)
                .unwrap();
            let anchor = workbench
                .capture_tab_move(doc_pane, workbench.pane(doc_pane).unwrap().tabs()[0].id())
                .unwrap();
            let moved = workbench
                .relocate_tab(
                    source,
                    super::TabMoveDestination::At {
                        anchor,
                        placement: super::TabMovePlacement::Split(super::SplitEdge::Right),
                    },
                )
                .unwrap();
            assert_eq!(
                workbench.tool(id).unwrap().content.presentation(),
                presentation
            );
            assert_eq!(moved.destination.tab_id, source.tab.tab_id);
            assert!(!workbench.contains_tab_move(source));
            assert!(workbench.pane(dock).is_none());
            assert_eq!(workbench.focused_pane_id(), Some(moved.destination.pane_id));

            // Workbench-edge drop recreates the dock around the whole layout.
            let source = workbench
                .capture_tab_move(moved.destination.pane_id, moved.destination.tab_id)
                .unwrap();
            let docked = workbench
                .relocate_tab(
                    source,
                    super::TabMoveDestination::Dock(DockLocation::Bottom),
                )
                .unwrap();
            assert_eq!(
                workbench.tool(id).unwrap().content.presentation(),
                presentation
            );
            assert_eq!(
                workbench.dock_location(docked.destination.pane_id),
                Some(DockLocation::Bottom)
            );
            assert_eq!(
                workbench.focused_pane_id(),
                Some(docked.destination.pane_id)
            );

            // Joining the document group keeps the document view count at one.
            let anchor = workbench
                .capture_tab_move(doc_pane, workbench.pane(doc_pane).unwrap().tabs()[0].id())
                .unwrap();
            let joined = workbench
                .relocate_tab(
                    workbench
                        .capture_tab_move(docked.destination.pane_id, docked.destination.tab_id)
                        .unwrap(),
                    super::TabMoveDestination::At {
                        anchor,
                        placement: super::TabMovePlacement::End,
                    },
                )
                .unwrap();
            assert_eq!(joined.destination.pane_id, doc_pane);
            assert_eq!(workbench.view_count(document), 1);
            assert!(workbench.pane(docked.destination.pane_id).is_none());
            assert_eq!(
                workbench.tool(id).unwrap().content.presentation(),
                presentation
            );
        });
    }

    #[gpui::test]
    fn dock_sizes_clamp_and_survive_hide_reopen(cx: &mut gpui::TestAppContext) {
        let workbench = cx.new(|_| Workbench::empty());
        workbench.update(cx, |workbench, cx| {
            let id = explorer(workbench, cx);
            workbench.show_tool(id).unwrap();
            workbench.set_dock_size(DockLocation::Left, 1.);
            assert_eq!(
                workbench.dock_size(DockLocation::Left),
                DockLocation::MIN_SIZE
            );
            workbench.set_dock_size(DockLocation::Left, 10_000.);
            assert_eq!(
                workbench.dock_size(DockLocation::Left),
                DockLocation::MAX_SIZE
            );
            workbench.set_dock_size(DockLocation::Bottom, 133.);
            workbench.place_tool(id, ToolPlacement::Dock(DockLocation::Bottom));
            assert_eq!(workbench.dock_size(DockLocation::Bottom), 133.);
            assert!(workbench.hide_tool(id));
            workbench.set_dock_size(DockLocation::Bottom, 200.);
            let (pane, _) = workbench.show_tool(id).unwrap();
            assert_eq!(workbench.dock_location(pane), Some(DockLocation::Bottom));
            assert_eq!(workbench.dock_size(DockLocation::Bottom), 200.);
        });
    }

    #[gpui::test]
    fn tool_snapshots_record_only_tools_with_stable_ids(cx: &mut gpui::TestAppContext) {
        let workbench = cx.new(|_| Workbench::empty());
        workbench.update(cx, |workbench, cx| {
            let model = cx.new(|_| BufferModel::from_text(""));
            let mut documents = crate::app::documents::DocumentCollection::new();
            let document = documents.create_untitled("doc", model.clone(), cx);
            let (doc_pane, _) = workbench.open_document_in_normal_group(document, model, cx);
            workbench.open_terminal_tab_test_double(doc_pane, super::TerminalSessionId(7));
            let id = explorer(workbench, cx);
            workbench.show_tool(id).unwrap();
            workbench.set_dock_size(DockLocation::Left, 311.);
            // A visible tool in an ordinary group flattens to the document group.
            workbench.place_tool(id, ToolPlacement::DocumentGroup);
            let snapshot = workbench.tool_layout_snapshot();
            assert_eq!(
                snapshot.tools,
                [crate::app::tool_layout::StoredTool {
                    id: crate::app::tool_layout::EXPLORER_TOOL_ID.to_owned(),
                    location: crate::app::tool_layout::StoredLocation::DocumentGroup,
                    index: 2,
                    visible: true,
                }]
            );
            assert_eq!(snapshot.dock_sizes[DockLocation::Left.index()], 311.);
            let payload = serde_json::to_string(&snapshot).unwrap();
            for forbidden in ["Document", "Terminal", "Pane(", "Tab(", "presentation"] {
                assert!(
                    !payload.contains(forbidden),
                    "tool record leaks {forbidden}"
                );
            }
            let decoded: crate::app::tool_layout::ToolLayoutSnapshot =
                serde_json::from_str(&payload).unwrap();
            assert_eq!(decoded, snapshot);
            // Hidden tools keep their remembered dock while staying invisible.
            workbench.place_tool(id, ToolPlacement::Dock(DockLocation::Bottom));
            workbench.hide_tool(id);
            let hidden = workbench.tool_layout_snapshot();
            assert_eq!(
                hidden.tools[0].location,
                crate::app::tool_layout::StoredLocation::Bottom
            );
            assert!(!hidden.tools[0].visible);
            assert_eq!(doc_pane, workbench.normal_pane().unwrap());
        });
    }

    #[gpui::test]
    fn tool_snapshots_flatten_vanished_groups(cx: &mut gpui::TestAppContext) {
        let workbench = cx.new(|_| Workbench::empty());
        workbench.update(cx, |workbench, cx| {
            let id = explorer(workbench, cx);
            workbench.place_tool(id, ToolPlacement::DocumentGroup);
            workbench.hide_tool(id);
            // The remembered pane still exists as an ordinary group.
            let remembered = workbench.tool_layout_snapshot();
            assert_eq!(
                remembered.tools[0].location,
                crate::app::tool_layout::StoredLocation::DocumentGroup
            );
            // A vanished pane flattens the same way instead of naming a dead group.
            workbench.remember_tool_placement(id, ToolPlacement::Group(PaneId(999)), 3);
            let vanished = workbench.tool_layout_snapshot();
            assert_eq!(
                vanished.tools[0].location,
                crate::app::tool_layout::StoredLocation::DocumentGroup
            );
            assert_eq!(vanished.tools[0].index, 3);
            assert!(!vanished.tools[0].visible);
        });
    }
}
