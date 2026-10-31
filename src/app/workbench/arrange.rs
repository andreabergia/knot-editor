//! Installing a restored arrangement into a fresh workbench.

use super::*;
use crate::app::{editor::EditorViewState, model::BufferModel, window_layout::PlanNode};

/// One tab of a restored arrangement, materialized by its kind's step.
pub(crate) enum ArrangedTab {
    Document {
        document: DocumentId,
        model: Entity<BufferModel>,
        view: EditorViewState,
    },
    Tool(ToolFrameId),
    Terminal(TerminalSessionId, Option<Entity<TerminalView>>),
}

impl Workbench {
    fn arranged_payload(&self, tab: ArrangedTab, cx: &mut Context<Self>) -> WorkbenchTabPayload {
        match tab {
            ArrangedTab::Document {
                document,
                model,
                view,
            } => WorkbenchTabPayload::Document {
                document_id: document,
                editor: cx.new(|cx| {
                    let mut editor = EditorView::new(model, cx);
                    editor.restore_view_state(view, cx);
                    editor
                }),
            },
            ArrangedTab::Tool(id) => WorkbenchTabPayload::Tool {
                id,
                content: self
                    .tool(id)
                    .expect("arranged tools are registered")
                    .content
                    .clone(),
            },
            ArrangedTab::Terminal(session_id, view) => {
                WorkbenchTabPayload::Terminal { session_id, view }
            }
        }
    }

    /// Install one restored tab at `index` of `pane`, or at the end of the
    /// normal group when that pane is gone, and activate it.
    pub(crate) fn insert_arranged(
        &mut self,
        pane: PaneId,
        index: usize,
        tab: ArrangedTab,
        pinned: bool,
        cx: &mut Context<Self>,
    ) -> (PaneId, TabId) {
        let (pane, index) = if self.pane(pane).is_some() {
            (pane, index)
        } else {
            let pane = self
                .normal_pane()
                .unwrap_or_else(|| self.create_group(None));
            (pane, usize::MAX)
        };
        let tab = WorkbenchTab {
            id: self.allocate_tab_id(),
            pinned,
            payload: self.arranged_payload(tab, cx),
        };
        let id = tab.id;
        let group = self
            .panes
            .iter_mut()
            .find(|group| group.id == pane)
            .expect("destination pane is live");
        let index = group.section_index(pinned, index);
        group.tabs.insert(index, tab);
        group.select(id);
        self.focused_pane = Some(pane);
        self.debug_assert_invariants();
        (pane, id)
    }

    /// Replace every presentation with a restored arrangement. Visible tools
    /// are hidden first, keeping their remembered placement, so tools the
    /// arrangement does not name stay hidden. Each document tab gets its own
    /// view with its restored state. The focused pane is the marked one, else
    /// the first ordinary group, else the first pane.
    pub(crate) fn arrange(
        &mut self,
        root: Option<PlanNode<ArrangedTab>>,
        dock_sizes: [f32; 3],
        cx: &mut Context<Self>,
    ) {
        let visible = self
            .tools
            .iter()
            .map(|tool| tool.id)
            .filter(|id| self.tool_tab(*id).is_some())
            .collect::<Vec<_>>();
        for id in visible {
            self.hide_tool(id);
        }
        self.remove_document_tabs();
        debug_assert!(self.panes.is_empty(), "arranged workbenches start empty");
        self.panes.clear();
        self.docks.clear();
        let mut focused = None;
        self.layout = root.map(|root| self.install(root, &mut focused, cx));
        self.focused_pane = focused.or_else(|| {
            self.panes
                .iter()
                .find(|pane| self.dock_location(pane.id).is_none())
                .or(self.panes.first())
                .map(|pane| pane.id)
        });
        self.set_dock_sizes(dock_sizes);
    }

    fn install(
        &mut self,
        node: PlanNode<ArrangedTab>,
        focused: &mut Option<PaneId>,
        cx: &mut Context<Self>,
    ) -> WorkbenchLayout {
        match node {
            PlanNode::Pane {
                dock,
                selected,
                focused: is_focused,
                pinned,
                tabs,
            } => {
                let pane = self.allocate_pane_id();
                let tabs = tabs
                    .into_iter()
                    .enumerate()
                    .map(|(index, tab)| WorkbenchTab {
                        id: self.allocate_tab_id(),
                        pinned: index < pinned,
                        payload: self.arranged_payload(tab, cx),
                    })
                    .collect::<Vec<_>>();
                let active_tab = tabs[selected.min(tabs.len() - 1)].id;
                self.panes.push(Pane::new(pane, tabs, active_tab));
                if let Some(dock) = dock {
                    self.docks.push((pane, dock));
                }
                if is_focused {
                    *focused = Some(pane);
                }
                WorkbenchLayout::Pane(pane)
            }
            PlanNode::Split {
                direction,
                ratio,
                first,
                second,
            } => WorkbenchLayout::Split {
                direction,
                ratio: WorkbenchLayout::clamp_ratio(ratio),
                first: Box::new(self.install(*first, focused, cx)),
                second: Box::new(self.install(*second, focused, cx)),
            },
        }
    }
}
