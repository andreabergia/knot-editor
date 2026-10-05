//! Window-local editor layout and view ownership.
//!
//! A workbench is a binary split tree whose leaves are tabbed panes. Tabs own
//! presentation views, while documents and terminal sessions remain application-owned. This module models
//! layout and lifecycle transitions independently of native windows and
//! confirmation dialogs.

#[cfg(debug_assertions)]
use std::collections::HashSet;

use gpui::{App, AppContext, Context, Entity, Focusable, WeakFocusHandle, Window};

use crate::host::protocol::Command;

use super::{
    documents::{Document, DocumentId},
    editor::EditorView,
    product_commands::CommandClaim,
    terminal_view::TerminalView,
    tree_view::TreeView,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PaneId(u64);

impl PaneId {
    pub(crate) fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TabId(u64);

impl TabId {
    pub(crate) fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct WorkbenchTabSnapshot {
    pub(crate) pane_id: PaneId,
    pub(crate) tab_id: TabId,
    pub(crate) surface: TabSurfaceId,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TerminalSessionId(pub(crate) u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TabSurfaceId {
    Document(DocumentId),
    Terminal(TerminalSessionId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SplitDirection {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SplitPlacement {
    #[cfg_attr(not(test), allow(dead_code, reason = "supported layout placement"))]
    Before,
    After,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkbenchLayout {
    Pane(PaneId),
    Split {
        direction: SplitDirection,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl WorkbenchLayout {
    fn split_pane(
        &mut self,
        target: PaneId,
        new_pane: PaneId,
        direction: SplitDirection,
        placement: SplitPlacement,
    ) -> bool {
        match self {
            Self::Pane(pane_id) if *pane_id == target => {
                let (first, second) = match placement {
                    SplitPlacement::Before => (new_pane, target),
                    SplitPlacement::After => (target, new_pane),
                };
                *self = Self::Split {
                    direction,
                    first: Box::new(Self::Pane(first)),
                    second: Box::new(Self::Pane(second)),
                };
                true
            }
            Self::Pane(_) => false,
            Self::Split { first, second, .. } => {
                first.split_pane(target, new_pane, direction, placement)
                    || second.split_pane(target, new_pane, direction, placement)
            }
        }
    }

    fn remove_pane(self, target: PaneId) -> Option<Self> {
        match self {
            Self::Pane(pane_id) => (pane_id != target).then_some(Self::Pane(pane_id)),
            Self::Split {
                direction,
                first,
                second,
            } => match (first.remove_pane(target), second.remove_pane(target)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    direction,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            },
        }
    }

    fn first_pane(&self) -> PaneId {
        match self {
            Self::Pane(pane_id) => *pane_id,
            Self::Split { first, .. } => first.first_pane(),
        }
    }

    fn pane_ids(&self, result: &mut Vec<PaneId>) {
        match self {
            Self::Pane(pane_id) => result.push(*pane_id),
            Self::Split { first, second, .. } => {
                first.pane_ids(result);
                second.pane_ids(result);
            }
        }
    }
}

pub(crate) struct WorkbenchTab {
    id: TabId,
    payload: WorkbenchTabPayload,
}

pub(crate) enum WorkbenchTabPayload {
    Document {
        document_id: DocumentId,
        editor: Entity<EditorView>,
    },
    Terminal {
        session_id: TerminalSessionId,
        view: Option<Entity<TerminalView>>,
    },
}

#[derive(Clone)]
pub(crate) enum CommandView {
    Editor(Entity<EditorView>),
    Terminal(Entity<TerminalView>),
    Extension(Entity<TreeView>),
}

impl CommandView {
    pub(crate) fn matches_focus(&self, focus: &WeakFocusHandle, cx: &App) -> bool {
        let handle = match self {
            Self::Editor(view) => view.focus_handle(cx),
            Self::Terminal(view) => view.focus_handle(cx),
            Self::Extension(view) => view.focus_handle(cx),
        };
        focus.upgrade() == Some(handle)
    }

    pub(crate) fn handle_command(
        &self,
        command: &Command,
        window: &mut Window,
        cx: &mut App,
    ) -> CommandClaim {
        match self {
            Self::Editor(view) => {
                view.update(cx, |view, cx| view.handle_command(command, window, cx))
            }
            Self::Terminal(view) => view.update(cx, |view, cx| view.handle_command(command, cx)),
            Self::Extension(view) => view.update(cx, |view, cx| view.handle_command(command, cx)),
        }
    }
}

impl WorkbenchTab {
    pub(crate) fn command_view(&self) -> Option<CommandView> {
        match &self.payload {
            WorkbenchTabPayload::Document { editor, .. } => {
                Some(CommandView::Editor(editor.clone()))
            }
            WorkbenchTabPayload::Terminal { view, .. } => {
                view.as_ref().cloned().map(CommandView::Terminal)
            }
        }
    }

    pub(crate) fn id(&self) -> TabId {
        self.id
    }

    pub(crate) fn surface_id(&self) -> TabSurfaceId {
        match &self.payload {
            WorkbenchTabPayload::Document { document_id, .. } => {
                TabSurfaceId::Document(*document_id)
            }
            WorkbenchTabPayload::Terminal { session_id, .. } => TabSurfaceId::Terminal(*session_id),
        }
    }

    pub(crate) fn document_id(&self) -> Option<DocumentId> {
        match self.surface_id() {
            TabSurfaceId::Document(document_id) => Some(document_id),
            TabSurfaceId::Terminal(_) => None,
        }
    }

    pub(crate) fn editor(&self) -> Option<&Entity<EditorView>> {
        match &self.payload {
            WorkbenchTabPayload::Document { editor, .. } => Some(editor),
            WorkbenchTabPayload::Terminal { .. } => None,
        }
    }

    pub(crate) fn terminal_view(&self) -> Option<&Entity<TerminalView>> {
        match &self.payload {
            WorkbenchTabPayload::Terminal { view, .. } => view.as_ref(),
            WorkbenchTabPayload::Document { .. } => None,
        }
    }
}

pub(crate) struct Pane {
    id: PaneId,
    tabs: Vec<WorkbenchTab>,
    active_tab: TabId,
}

impl Pane {
    pub(crate) fn id(&self) -> PaneId {
        self.id
    }

    pub(crate) fn tabs(&self) -> &[WorkbenchTab] {
        &self.tabs
    }

    pub(crate) fn active_tab_id(&self) -> TabId {
        self.active_tab
    }

    pub(crate) fn active_tab(&self) -> &WorkbenchTab {
        self.tabs
            .iter()
            .find(|tab| tab.id == self.active_tab)
            .expect("pane active tab must belong to the pane")
    }
}

pub(crate) struct Workbench {
    next_pane_id: u64,
    next_tab_id: u64,
    layout: Option<WorkbenchLayout>,
    panes: Vec<Pane>,
    focused_pane: Option<PaneId>,
}

impl Workbench {
    pub(crate) fn empty() -> Self {
        Self {
            next_pane_id: 1,
            next_tab_id: 1,
            layout: None,
            panes: Vec::new(),
            focused_pane: None,
        }
    }

    pub(crate) fn new_for_terminal(
        session_id: TerminalSessionId,
        view: Entity<TerminalView>,
    ) -> Self {
        let pane_id = PaneId(1);
        let tab_id = TabId(1);
        let workbench = Self {
            next_pane_id: 2,
            next_tab_id: 2,
            layout: Some(WorkbenchLayout::Pane(pane_id)),
            panes: vec![Pane {
                id: pane_id,
                tabs: vec![WorkbenchTab {
                    id: tab_id,
                    payload: WorkbenchTabPayload::Terminal {
                        session_id,
                        view: Some(view),
                    },
                }],
                active_tab: tab_id,
            }],
            focused_pane: Some(pane_id),
        };
        workbench.debug_assert_invariants();
        workbench
    }

    #[cfg_attr(not(test), allow(dead_code, reason = "document convenience API"))]
    pub(crate) fn new(document: &Document, cx: &mut Context<Self>) -> Self {
        Self::new_for_document(document.id(), document.model().clone(), cx)
    }

    pub(crate) fn new_for_document(
        document_id: DocumentId,
        model: Entity<super::model::BufferModel>,
        cx: &mut Context<Self>,
    ) -> Self {
        let pane_id = PaneId(1);
        let tab_id = TabId(1);
        let workbench = Self {
            next_pane_id: 2,
            next_tab_id: 2,
            layout: Some(WorkbenchLayout::Pane(pane_id)),
            panes: vec![Pane {
                id: pane_id,
                tabs: vec![WorkbenchTab {
                    id: tab_id,
                    payload: WorkbenchTabPayload::Document {
                        document_id,
                        editor: cx.new(|cx| EditorView::new(model, cx)),
                    },
                }],
                active_tab: tab_id,
            }],
            focused_pane: Some(pane_id),
        };
        workbench.debug_assert_invariants();
        workbench
    }

    pub(crate) fn layout(&self) -> Option<&WorkbenchLayout> {
        self.layout.as_ref()
    }

    #[cfg_attr(not(test), allow(dead_code, reason = "layout inspection API"))]
    pub(crate) fn panes(&self) -> &[Pane] {
        &self.panes
    }

    pub(crate) fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.id == id)
    }

    #[cfg_attr(not(test), allow(dead_code, reason = "focused-pane convenience API"))]
    pub(crate) fn focused_pane_id(&self) -> Option<PaneId> {
        self.focused_pane
    }

    pub(crate) fn focused_pane(&self) -> Option<&Pane> {
        self.focused_pane.and_then(|id| self.pane(id))
    }

    /// Capture tab identities in layout pane order and then tab order.
    pub(crate) fn tab_snapshots(&self) -> Vec<WorkbenchTabSnapshot> {
        let mut pane_ids = Vec::new();
        if let Some(layout) = &self.layout {
            layout.pane_ids(&mut pane_ids);
        }
        pane_ids
            .into_iter()
            .flat_map(|pane_id| {
                self.pane(pane_id)
                    .expect("layout pane must belong to the workbench")
                    .tabs
                    .iter()
                    .map(move |tab| WorkbenchTabSnapshot {
                        pane_id,
                        tab_id: tab.id,
                        surface: tab.surface_id(),
                    })
            })
            .collect()
    }

    pub(crate) fn focus_pane(&mut self, pane_id: PaneId) -> bool {
        if self.pane(pane_id).is_none() {
            return false;
        }
        self.focused_pane = Some(pane_id);
        self.debug_assert_invariants();
        true
    }

    pub(crate) fn activate_tab(&mut self, pane_id: PaneId, tab_id: TabId) -> bool {
        let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == pane_id) else {
            return false;
        };
        if !pane.tabs.iter().any(|tab| tab.id == tab_id) {
            return false;
        }
        pane.active_tab = tab_id;
        self.focused_pane = Some(pane_id);
        self.debug_assert_invariants();
        true
    }

    #[cfg_attr(not(test), allow(dead_code, reason = "document convenience API"))]
    pub(crate) fn open_tab(
        &mut self,
        pane_id: PaneId,
        document: &Document,
        cx: &mut Context<Self>,
    ) -> Option<TabId> {
        self.open_tab_for_document(pane_id, document.id(), document.model().clone(), cx)
    }

    pub(crate) fn open_tab_for_document(
        &mut self,
        pane_id: PaneId,
        document_id: DocumentId,
        model: Entity<super::model::BufferModel>,
        cx: &mut Context<Self>,
    ) -> Option<TabId> {
        let pane_index = self.panes.iter().position(|pane| pane.id == pane_id)?;
        let tab_id = self.allocate_tab_id();
        let editor = cx.new(|cx| EditorView::new(model, cx));
        let pane = &mut self.panes[pane_index];
        pane.tabs.push(WorkbenchTab {
            id: tab_id,
            payload: WorkbenchTabPayload::Document {
                document_id,
                editor,
            },
        });
        pane.active_tab = tab_id;
        self.focused_pane = Some(pane_id);
        self.debug_assert_invariants();
        Some(tab_id)
    }

    pub(crate) fn open_terminal_tab(
        &mut self,
        pane_id: PaneId,
        session_id: TerminalSessionId,
        view: Entity<TerminalView>,
    ) -> Option<TabId> {
        self.insert_terminal_tab(pane_id, session_id, Some(view))
    }

    #[cfg(test)]
    pub(crate) fn open_terminal_tab_test_double(
        &mut self,
        pane_id: PaneId,
        session_id: TerminalSessionId,
    ) -> Option<TabId> {
        self.insert_terminal_tab(pane_id, session_id, None)
    }

    fn insert_terminal_tab(
        &mut self,
        pane_id: PaneId,
        session_id: TerminalSessionId,
        view: Option<Entity<TerminalView>>,
    ) -> Option<TabId> {
        if self.has_terminal_session(session_id) {
            return None;
        }
        let pane_index = self.panes.iter().position(|pane| pane.id == pane_id)?;
        let tab_id = self.allocate_tab_id();
        let pane = &mut self.panes[pane_index];
        pane.tabs.push(WorkbenchTab {
            id: tab_id,
            payload: WorkbenchTabPayload::Terminal { session_id, view },
        });
        pane.active_tab = tab_id;
        self.focused_pane = Some(pane_id);
        self.debug_assert_invariants();
        Some(tab_id)
    }

    pub(crate) fn replace_tab_document(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        document_id: DocumentId,
        model: Entity<super::model::BufferModel>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(pane) = self.panes.iter_mut().find(|pane| pane.id == pane_id) else {
            return false;
        };
        let Some(tab) = pane.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return false;
        };
        if !matches!(tab.payload, WorkbenchTabPayload::Document { .. }) {
            return false;
        }
        tab.payload = WorkbenchTabPayload::Document {
            document_id,
            editor: cx.new(|cx| EditorView::new(model, cx)),
        };
        pane.active_tab = tab_id;
        self.focused_pane = Some(pane_id);
        self.debug_assert_invariants();
        true
    }

    /// Split the focused pane and show its active document in a fresh view.
    #[cfg_attr(not(test), allow(dead_code, reason = "focused-pane convenience API"))]
    pub(crate) fn split_focused(
        &mut self,
        direction: SplitDirection,
        placement: SplitPlacement,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        let focused_id = self.focused_pane?;
        self.split_pane(focused_id, direction, placement, cx)
    }

    /// Split one captured pane and show its active document in a fresh view.
    pub(crate) fn split_pane(
        &mut self,
        pane_id: PaneId,
        direction: SplitDirection,
        placement: SplitPlacement,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        let active = self.pane(pane_id)?.active_tab();
        let document_id = active.document_id()?;
        let model = active.editor()?.read(cx).model().clone();
        let new_pane_id = self.allocate_pane_id();
        let tab_id = self.allocate_tab_id();
        let editor = cx.new(|cx| EditorView::new(model, cx));

        let layout = self.layout.as_mut()?;
        assert!(
            layout.split_pane(pane_id, new_pane_id, direction, placement),
            "captured pane must occur exactly once in the layout"
        );
        self.panes.push(Pane {
            id: new_pane_id,
            tabs: vec![WorkbenchTab {
                id: tab_id,
                payload: WorkbenchTabPayload::Document {
                    document_id,
                    editor,
                },
            }],
            active_tab: tab_id,
        });
        self.focused_pane = Some(new_pane_id);
        self.debug_assert_invariants();
        Some(new_pane_id)
    }

    /// Split a captured pane, allocating an independent session for a terminal.
    pub(crate) fn split_pane_with_terminal(
        &mut self,
        pane_id: PaneId,
        direction: SplitDirection,
        placement: SplitPlacement,
        new_session_id: TerminalSessionId,
        view: Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        self.split_pane_with_terminal_view(
            pane_id,
            direction,
            placement,
            new_session_id,
            Some(view),
            cx,
        )
    }

    #[cfg(test)]
    pub(crate) fn split_pane_with_terminal_test_double(
        &mut self,
        pane_id: PaneId,
        direction: SplitDirection,
        placement: SplitPlacement,
        new_session_id: TerminalSessionId,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        self.split_pane_with_terminal_view(pane_id, direction, placement, new_session_id, None, cx)
    }

    fn split_pane_with_terminal_view(
        &mut self,
        pane_id: PaneId,
        direction: SplitDirection,
        placement: SplitPlacement,
        new_session_id: TerminalSessionId,
        view: Option<Entity<TerminalView>>,
        cx: &mut Context<Self>,
    ) -> Option<PaneId> {
        let active = self.pane(pane_id)?.active_tab();
        if active.document_id().is_some() {
            return self.split_pane(pane_id, direction, placement, cx);
        }
        if self.has_terminal_session(new_session_id) {
            return None;
        }
        let new_pane_id = self.allocate_pane_id();
        let tab_id = self.allocate_tab_id();
        let layout = self.layout.as_mut()?;
        assert!(
            layout.split_pane(pane_id, new_pane_id, direction, placement),
            "captured pane must occur exactly once in the layout"
        );
        self.panes.push(Pane {
            id: new_pane_id,
            tabs: vec![WorkbenchTab {
                id: tab_id,
                payload: WorkbenchTabPayload::Terminal {
                    session_id: new_session_id,
                    view,
                },
            }],
            active_tab: tab_id,
        });
        self.focused_pane = Some(new_pane_id);
        self.debug_assert_invariants();
        Some(new_pane_id)
    }

    pub(crate) fn view_count(&self, document_id: DocumentId) -> usize {
        self.panes
            .iter()
            .flat_map(|pane| &pane.tabs)
            .filter(|tab| tab.document_id() == Some(document_id))
            .count()
    }

    fn has_terminal_session(&self, session_id: TerminalSessionId) -> bool {
        self.panes
            .iter()
            .flat_map(|pane| &pane.tabs)
            .any(|tab| tab.surface_id() == TabSurfaceId::Terminal(session_id))
    }

    fn allocate_pane_id(&mut self) -> PaneId {
        let id = PaneId(self.next_pane_id);
        self.next_pane_id = self
            .next_pane_id
            .checked_add(1)
            .expect("pane identity space exhausted");
        id
    }

    fn allocate_tab_id(&mut self) -> TabId {
        let id = TabId(self.next_tab_id);
        self.next_tab_id = self
            .next_tab_id
            .checked_add(1)
            .expect("tab identity space exhausted");
        id
    }

    fn remove_tab(&mut self, pane_id: PaneId, tab_id: TabId) -> Option<RemovedTab> {
        let pane_index = self.panes.iter().position(|pane| pane.id == pane_id)?;
        let tab_index = self.panes[pane_index]
            .tabs
            .iter()
            .position(|tab| tab.id == tab_id)?;
        let tab = self.panes[pane_index].tabs.remove(tab_index);

        if self.panes[pane_index].tabs.is_empty() {
            let previous_focus = self.focused_pane;
            self.panes.remove(pane_index);
            self.layout = self
                .layout
                .take()
                .and_then(|layout| layout.remove_pane(pane_id));
            self.focused_pane = previous_focus
                .filter(|focused| self.pane(*focused).is_some())
                .or_else(|| self.layout.as_ref().map(WorkbenchLayout::first_pane));
            let removed = RemovedTab {
                tab,
                removed_pane: Some(pane_id),
            };
            self.debug_assert_invariants();
            Some(removed)
        } else {
            let pane = &mut self.panes[pane_index];
            if pane.active_tab == tab_id {
                pane.active_tab = pane.tabs[tab_index.min(pane.tabs.len() - 1)].id;
            }
            let removed = RemovedTab {
                tab,
                removed_pane: None,
            };
            self.debug_assert_invariants();
            Some(removed)
        }
    }

    pub(crate) fn contains_tab(
        &self,
        pane_id: PaneId,
        tab_id: TabId,
        document_id: DocumentId,
    ) -> bool {
        self.pane(pane_id).is_some_and(|pane| {
            pane.tabs
                .iter()
                .any(|tab| tab.id == tab_id && tab.document_id() == Some(document_id))
        })
    }

    pub(crate) fn contains_surface(
        &self,
        pane_id: PaneId,
        tab_id: TabId,
        surface: TabSurfaceId,
    ) -> bool {
        self.pane(pane_id).is_some_and(|pane| {
            pane.tabs
                .iter()
                .any(|tab| tab.id == tab_id && tab.surface_id() == surface)
        })
    }

    fn debug_assert_invariants(&self) {
        #[cfg(debug_assertions)]
        {
            let mut layout_panes = Vec::new();
            if let Some(layout) = &self.layout {
                layout.pane_ids(&mut layout_panes);
            }
            let layout_pane_set = layout_panes.iter().copied().collect::<HashSet<_>>();
            let pane_set = self.panes.iter().map(Pane::id).collect::<HashSet<_>>();
            debug_assert_eq!(layout_panes.len(), layout_pane_set.len());
            debug_assert_eq!(layout_pane_set, pane_set);
            debug_assert_eq!(self.layout.is_none(), self.panes.is_empty());
            debug_assert_eq!(self.focused_pane.is_none(), self.panes.is_empty());
            debug_assert!(
                self.focused_pane
                    .is_none_or(|focused| pane_set.contains(&focused))
            );

            let mut tab_ids = HashSet::new();
            for pane in &self.panes {
                debug_assert!(!pane.tabs.is_empty());
                debug_assert!(pane.tabs.iter().any(|tab| tab.id == pane.active_tab));
                debug_assert!(pane.tabs.iter().all(|tab| tab_ids.insert(tab.id)));
            }
        }
    }
}

struct RemovedTab {
    tab: WorkbenchTab,
    removed_pane: Option<PaneId>,
}

mod close;

#[allow(unused_imports, reason = "these form the checkpoint 2 workbench API")]
pub(crate) use close::{
    CloseConfirmation, CloseRequestOutcome, CloseTransition, DocumentCloseDisposition, PendingClose,
};

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext};

    use super::*;
    use crate::app::{documents::DocumentCollection, model::BufferModel};

    #[gpui::test]
    fn mixed_tabs_keep_identity_activation_and_document_views(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("document"));
        let mut documents = DocumentCollection::new();
        let document = cx.update(|cx| documents.create_untitled("Document", model, cx));
        let workbench = cx.new(|cx| Workbench::new(documents.get(document).unwrap(), cx));

        workbench.update(cx, |workbench, _| {
            let pane = workbench.focused_pane_id().unwrap();
            let document_tab = workbench.focused_pane().unwrap().active_tab_id();
            let terminal = TerminalSessionId(41);
            let terminal_tab = workbench
                .open_terminal_tab_test_double(pane, terminal)
                .unwrap();
            assert_eq!(workbench.view_count(document), 1);
            assert_eq!(
                workbench.focused_pane().unwrap().active_tab_id(),
                terminal_tab
            );
            assert_eq!(
                workbench.focused_pane().unwrap().active_tab().editor(),
                None
            );
            assert_eq!(
                workbench.tab_snapshots(),
                vec![
                    WorkbenchTabSnapshot {
                        pane_id: pane,
                        tab_id: document_tab,
                        surface: TabSurfaceId::Document(document),
                    },
                    WorkbenchTabSnapshot {
                        pane_id: pane,
                        tab_id: terminal_tab,
                        surface: TabSurfaceId::Terminal(terminal),
                    },
                ]
            );
            assert!(!workbench.contains_surface(
                pane,
                document_tab,
                TabSurfaceId::Terminal(terminal)
            ));
            assert!(workbench.activate_tab(pane, document_tab));
            assert_eq!(
                workbench.focused_pane().unwrap().active_tab().document_id(),
                Some(document)
            );
        });
    }

    #[gpui::test]
    fn terminal_split_gets_independent_session_and_document_split_keeps_model(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(|_| BufferModel::from_text("document"));
        let mut documents = DocumentCollection::new();
        let document = cx.update(|cx| documents.create_untitled("Document", model, cx));
        let workbench = cx.new(|cx| Workbench::new(documents.get(document).unwrap(), cx));

        workbench.update(cx, |workbench, cx| {
            let document_pane = workbench.focused_pane_id().unwrap();
            let terminal_source = TerminalSessionId(41);
            workbench
                .open_terminal_tab_test_double(document_pane, terminal_source)
                .unwrap();
            assert!(
                workbench
                    .split_pane(
                        document_pane,
                        SplitDirection::Horizontal,
                        SplitPlacement::After,
                        cx
                    )
                    .is_none()
            );
            let terminal_pane = workbench
                .split_pane_with_terminal_test_double(
                    document_pane,
                    SplitDirection::Horizontal,
                    SplitPlacement::After,
                    TerminalSessionId(42),
                    cx,
                )
                .unwrap();
            assert_eq!(workbench.view_count(document), 1);
            assert_eq!(
                workbench
                    .pane(terminal_pane)
                    .unwrap()
                    .active_tab()
                    .surface_id(),
                TabSurfaceId::Terminal(TerminalSessionId(42))
            );
            assert_eq!(
                workbench
                    .pane(document_pane)
                    .unwrap()
                    .active_tab()
                    .surface_id(),
                TabSurfaceId::Terminal(terminal_source)
            );
            let document_tab = workbench.pane(document_pane).unwrap().tabs()[0].id();
            assert!(workbench.activate_tab(document_pane, document_tab));
            workbench
                .split_pane_with_terminal_test_double(
                    document_pane,
                    SplitDirection::Vertical,
                    SplitPlacement::After,
                    TerminalSessionId(43),
                    cx,
                )
                .unwrap();
            assert_eq!(workbench.view_count(document), 2);
        });
    }

    #[gpui::test]
    fn panes_keep_ordered_tabs_and_stable_focus(cx: &mut TestAppContext) {
        let first_model = cx.new(|_| BufferModel::from_text("first"));
        let second_model = cx.new(|_| BufferModel::from_text("second"));
        let mut documents = DocumentCollection::new();
        let first_document = cx.update(|cx| documents.create_untitled("First", first_model, cx));
        let second_document = cx.update(|cx| documents.create_untitled("Second", second_model, cx));
        let workbench = cx.new(|cx| Workbench::new(documents.get(first_document).unwrap(), cx));

        workbench.update(cx, |workbench, cx| {
            let pane_id = workbench.focused_pane_id().unwrap();
            let first_tab = workbench.focused_pane().unwrap().active_tab_id();
            let second_tab = workbench
                .open_tab(pane_id, documents.get(second_document).unwrap(), cx)
                .unwrap();

            let pane = workbench.focused_pane().unwrap();
            assert_eq!(
                pane.tabs().iter().map(WorkbenchTab::id).collect::<Vec<_>>(),
                vec![first_tab, second_tab]
            );
            assert_eq!(pane.active_tab_id(), second_tab);
            assert!(workbench.activate_tab(pane_id, first_tab));
            assert_eq!(workbench.focused_pane().unwrap().active_tab_id(), first_tab);
            assert!(!workbench.activate_tab(PaneId(u64::MAX), first_tab));
        });
    }

    #[gpui::test]
    fn tab_snapshots_follow_layout_and_tab_order(cx: &mut TestAppContext) {
        let first_model = cx.new(|_| BufferModel::from_text("first"));
        let second_model = cx.new(|_| BufferModel::from_text("second"));
        let third_model = cx.new(|_| BufferModel::from_text("third"));
        let mut documents = DocumentCollection::new();
        let first_document = cx.update(|cx| documents.create_untitled("First", first_model, cx));
        let second_document = cx.update(|cx| documents.create_untitled("Second", second_model, cx));
        let third_document = cx.update(|cx| documents.create_untitled("Third", third_model, cx));
        let workbench = cx.new(|cx| Workbench::new(documents.get(first_document).unwrap(), cx));

        workbench.update(cx, |workbench, cx| {
            let first_pane = workbench.focused_pane_id().unwrap();
            let first_tab = workbench.focused_pane().unwrap().active_tab_id();
            let second_tab = workbench
                .open_tab(first_pane, documents.get(second_document).unwrap(), cx)
                .unwrap();
            assert!(workbench.activate_tab(first_pane, first_tab));
            let preceding_pane = workbench
                .split_focused(SplitDirection::Vertical, SplitPlacement::Before, cx)
                .unwrap();
            let preceding_first_tab = workbench.focused_pane().unwrap().active_tab_id();
            let preceding_second_tab = workbench
                .open_tab(preceding_pane, documents.get(third_document).unwrap(), cx)
                .unwrap();

            assert_eq!(
                workbench.tab_snapshots(),
                vec![
                    WorkbenchTabSnapshot {
                        pane_id: preceding_pane,
                        tab_id: preceding_first_tab,
                        surface: TabSurfaceId::Document(first_document),
                    },
                    WorkbenchTabSnapshot {
                        pane_id: preceding_pane,
                        tab_id: preceding_second_tab,
                        surface: TabSurfaceId::Document(third_document),
                    },
                    WorkbenchTabSnapshot {
                        pane_id: first_pane,
                        tab_id: first_tab,
                        surface: TabSurfaceId::Document(first_document),
                    },
                    WorkbenchTabSnapshot {
                        pane_id: first_pane,
                        tab_id: second_tab,
                        surface: TabSurfaceId::Document(second_document),
                    },
                ]
            );
        });
    }

    #[gpui::test]
    fn nested_splits_preserve_tree_order_and_focus(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("shared"));
        let mut documents = DocumentCollection::new();
        let document = cx.update(|cx| documents.create_untitled("Shared", model, cx));
        let workbench = cx.new(|cx| Workbench::new(documents.get(document).unwrap(), cx));

        workbench.update(cx, |workbench, cx| {
            let first = workbench.focused_pane_id().unwrap();
            let second = workbench
                .split_focused(SplitDirection::Horizontal, SplitPlacement::After, cx)
                .unwrap();
            let third = workbench
                .split_focused(SplitDirection::Vertical, SplitPlacement::Before, cx)
                .unwrap();

            let mut pane_ids = Vec::new();
            workbench.layout().unwrap().pane_ids(&mut pane_ids);
            assert_eq!(pane_ids, vec![first, third, second]);
            assert_eq!(workbench.focused_pane_id(), Some(third));
            assert_eq!(workbench.panes().len(), 3);
            assert_eq!(workbench.view_count(document), 3);
            assert!(matches!(
                workbench.layout(),
                Some(WorkbenchLayout::Split {
                    direction: SplitDirection::Horizontal,
                    second,
                    ..
                }) if matches!(
                    second.as_ref(),
                    WorkbenchLayout::Split {
                        direction: SplitDirection::Vertical,
                        ..
                    }
                )
            ));
        });
    }

    #[gpui::test]
    fn split_placement_orders_the_new_pane_without_changing_its_focus(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("shared"));
        let mut documents = DocumentCollection::new();
        let document = cx.update(|cx| documents.create_untitled("Shared", model, cx));
        let workbench = cx.new(|cx| Workbench::new(documents.get(document).unwrap(), cx));

        workbench.update(cx, |workbench, cx| {
            let existing = workbench.focused_pane_id().unwrap();
            let new = workbench
                .split_focused(SplitDirection::Horizontal, SplitPlacement::Before, cx)
                .unwrap();
            let mut pane_ids = Vec::new();
            workbench.layout().unwrap().pane_ids(&mut pane_ids);

            assert_eq!(pane_ids, vec![new, existing]);
            assert_eq!(workbench.focused_pane_id(), Some(new));
        });
    }

    #[gpui::test]
    fn tabs_and_splits_own_distinct_views_of_one_model(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("one\ntwo\nthree"));
        let mut documents = DocumentCollection::new();
        let document = cx.update(|cx| documents.create_untitled("Shared", model.clone(), cx));
        let workbench = cx.new(|cx| Workbench::new(documents.get(document).unwrap(), cx));

        workbench.update(cx, |workbench, cx| {
            let first_pane = workbench.focused_pane_id().unwrap();
            let first_editor = workbench
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone();
            let second_tab = workbench
                .open_tab(first_pane, documents.get(document).unwrap(), cx)
                .unwrap();
            let second_editor = workbench
                .pane(first_pane)
                .unwrap()
                .tabs()
                .iter()
                .find(|tab| tab.id() == second_tab)
                .unwrap()
                .editor()
                .unwrap()
                .clone();
            let split_pane = workbench
                .split_focused(SplitDirection::Vertical, SplitPlacement::After, cx)
                .unwrap();
            let third_editor = workbench
                .pane(split_pane)
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone();

            assert_ne!(first_editor, second_editor);
            assert_ne!(second_editor, third_editor);
            assert_eq!(first_editor.read(cx).model(), &model);
            assert_eq!(second_editor.read(cx).model(), &model);
            assert_eq!(third_editor.read(cx).model(), &model);

            first_editor.update(cx, |editor, _| {
                editor.set_test_presentation_state(0, 1, None, 10.)
            });
            second_editor.update(cx, |editor, _| {
                editor.set_test_presentation_state(1, 2, Some((0, 0)), 20.)
            });
            third_editor.update(cx, |editor, _| {
                editor.set_test_presentation_state(2, 3, None, 30.)
            });

            assert_eq!(
                first_editor.read(cx).test_presentation_state(),
                (0, 1, None, 10.)
            );
            assert_eq!(
                second_editor.read(cx).test_presentation_state(),
                (1, 2, Some((0, 0)), 20.)
            );
            assert_eq!(
                third_editor.read(cx).test_presentation_state(),
                (2, 3, None, 30.)
            );
        });
    }
}
