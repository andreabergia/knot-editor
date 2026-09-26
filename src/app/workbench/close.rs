use gpui::Context;

use super::{PaneId, TabId, TabSurfaceId, TerminalSessionId, Workbench};
use crate::app::documents::Document;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DocumentCloseDisposition {
    Retained,
    CloseRequested,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CloseTransition {
    pub(crate) closed_tab: TabId,
    pub(crate) removed_pane: Option<PaneId>,
    pub(crate) focused_pane: Option<PaneId>,
    pub(crate) document: Option<DocumentCloseDisposition>,
}

impl CloseTransition {
    pub(crate) fn workbench_is_empty(self) -> bool {
        self.focused_pane.is_none()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PendingClose {
    pane_id: PaneId,
    tab_id: TabId,
    document_id: super::DocumentId,
}

impl PendingClose {
    #[cfg_attr(not(test), allow(dead_code, reason = "used by protected closure"))]
    pub(crate) fn document_id(&self) -> super::DocumentId {
        self.document_id
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CloseRequestOutcome {
    Closed(CloseTransition),
    Pending(PendingClose),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code, reason = "used by protected closure"))]
pub(crate) enum CloseConfirmation {
    Close,
    Cancel,
}

impl Workbench {
    /// Request closure without presenting UI or performing persistence.
    ///
    /// `open_view_count` is application-wide because another native window may
    /// show the same document. A dirty document needs confirmation only when
    /// this tab is its final view.
    #[cfg_attr(not(test), allow(dead_code, reason = "document convenience API"))]
    pub(crate) fn request_close_tab(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        document: &Document,
        open_view_count: usize,
        cx: &mut Context<Self>,
    ) -> Option<CloseRequestOutcome> {
        self.request_close_tab_with_state(
            pane_id,
            tab_id,
            document.id(),
            document.is_dirty(cx),
            open_view_count,
        )
    }

    pub(crate) fn request_close_tab_with_state(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        document_id: super::DocumentId,
        document_is_dirty: bool,
        open_view_count: usize,
    ) -> Option<CloseRequestOutcome> {
        assert!(open_view_count > 0, "the closing tab is an open view");
        if !self.contains_tab(pane_id, tab_id, document_id) {
            return None;
        }
        assert!(
            open_view_count >= self.view_count(document_id),
            "application view count must include every view in this workbench"
        );
        if open_view_count == 1 && document_is_dirty {
            return Some(CloseRequestOutcome::Pending(PendingClose {
                pane_id,
                tab_id,
                document_id,
            }));
        }
        Some(CloseRequestOutcome::Closed(self.close_tab_now(
            pane_id,
            tab_id,
            Some(open_view_count),
        )))
    }

    pub(crate) fn request_close_terminal_tab(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        session_id: TerminalSessionId,
    ) -> Option<CloseTransition> {
        self.contains_surface(pane_id, tab_id, TabSurfaceId::Terminal(session_id))
            .then(|| self.close_tab_now(pane_id, tab_id, None))
    }

    /// Complete or cancel a previously pending final-view close.
    ///
    /// The caller resolves Save/Don't Save into `Close` only after persistence
    /// or discard has succeeded. Stale requests are rejected rather than
    /// closing a replacement tab.
    #[cfg_attr(not(test), allow(dead_code, reason = "used by protected closure"))]
    pub(crate) fn resolve_pending_close(
        &mut self,
        pending: PendingClose,
        confirmation: CloseConfirmation,
        open_view_count: usize,
    ) -> Option<CloseTransition> {
        if confirmation == CloseConfirmation::Cancel {
            return None;
        }
        assert!(open_view_count > 0, "the closing tab is an open view");
        if !self.contains_tab(pending.pane_id, pending.tab_id, pending.document_id) {
            return None;
        }
        assert!(
            open_view_count >= self.view_count(pending.document_id),
            "application view count must include every view in this workbench"
        );
        Some(self.close_tab_now(pending.pane_id, pending.tab_id, Some(open_view_count)))
    }

    fn close_tab_now(
        &mut self,
        pane_id: PaneId,
        tab_id: TabId,
        open_view_count: Option<usize>,
    ) -> CloseTransition {
        let removed = self
            .remove_tab(pane_id, tab_id)
            .expect("validated tab must remain present through synchronous close");
        debug_assert_eq!(removed.tab.id, tab_id);
        CloseTransition {
            closed_tab: tab_id,
            removed_pane: removed.removed_pane,
            focused_pane: self.focused_pane,
            document: open_view_count.map(|open_view_count| {
                if open_view_count == 1 {
                    DocumentCloseDisposition::CloseRequested
                } else {
                    DocumentCloseDisposition::Retained
                }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext};

    use super::*;
    use crate::app::{
        documents::DocumentCollection,
        model::BufferModel,
        workbench::{SplitDirection, SplitPlacement, TerminalSessionId},
    };

    #[gpui::test]
    fn terminal_close_never_requests_document_disposition(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("dirty"));
        let mut documents = DocumentCollection::new();
        let document = cx.update(|cx| documents.create_untitled("Dirty", model.clone(), cx));
        model.update(cx, |model, _| model.replace(0..0, "edited ").unwrap());
        let workbench = cx.new(|cx| Workbench::new(documents.get(document).unwrap(), cx));

        workbench.update(cx, |workbench, _| {
            let pane = workbench.focused_pane_id().unwrap();
            let document_tab = workbench.focused_pane().unwrap().active_tab_id();
            let session = TerminalSessionId(7);
            let terminal_tab = workbench
                .open_terminal_tab_test_double(pane, session)
                .unwrap();
            assert!(
                workbench
                    .request_close_tab_with_state(pane, terminal_tab, document, true, 1)
                    .is_none()
            );
            assert!(
                workbench
                    .request_close_terminal_tab(pane, terminal_tab, TerminalSessionId(8))
                    .is_none()
            );
            assert_eq!(workbench.view_count(document), 1);
            let transition = workbench
                .request_close_terminal_tab(pane, terminal_tab, session)
                .unwrap();
            assert_eq!(transition.document, None);
            assert_eq!(
                workbench.focused_pane().unwrap().active_tab_id(),
                document_tab
            );
            assert_eq!(workbench.view_count(document), 1);
            assert!(matches!(
                workbench.request_close_tab_with_state(pane, document_tab, document, true, 1),
                Some(CloseRequestOutcome::Pending(_))
            ));
        });
    }

    #[gpui::test]
    fn closing_tabs_disposes_views_and_selects_an_ordered_neighbor(cx: &mut TestAppContext) {
        let first_model = cx.new(|_| BufferModel::from_text("first"));
        let second_model = cx.new(|_| BufferModel::from_text("second"));
        let second_model_weak = second_model.downgrade();
        let mut documents = DocumentCollection::new();
        let first_document =
            cx.update(|cx| documents.create_untitled("First", first_model.clone(), cx));
        let second_document =
            cx.update(|cx| documents.create_untitled("Second", second_model.clone(), cx));
        drop(second_model);
        let workbench = cx.new(|cx| Workbench::new(documents.get(first_document).unwrap(), cx));
        let mut closed_view = None;
        let mut retained_view = None;

        workbench.update(cx, |workbench, cx| {
            let pane = workbench.focused_pane_id().unwrap();
            let first_tab = workbench.focused_pane().unwrap().active_tab_id();
            let first_view = workbench
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .downgrade();
            let second_tab = workbench
                .open_tab(pane, documents.get(second_document).unwrap(), cx)
                .unwrap();
            let second_view = workbench
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .downgrade();

            let transition = match workbench
                .request_close_tab(
                    pane,
                    second_tab,
                    documents.get(second_document).unwrap(),
                    1,
                    cx,
                )
                .unwrap()
            {
                CloseRequestOutcome::Closed(transition) => transition,
                CloseRequestOutcome::Pending(_) => panic!("clean document should close"),
            };
            assert_eq!(
                transition.document,
                Some(DocumentCloseDisposition::CloseRequested)
            );
            assert_eq!(transition.removed_pane, None);
            assert_eq!(workbench.focused_pane().unwrap().active_tab_id(), first_tab);
            closed_view = Some(second_view);
            retained_view = Some(first_view);
        });
        cx.run_until_parked();
        assert!(closed_view.unwrap().upgrade().is_none());
        assert!(retained_view.unwrap().upgrade().is_some());
        assert!(documents.get(second_document).is_some());
        assert!(second_model_weak.upgrade().is_some());
        assert!(documents.remove(second_document));
        assert!(second_model_weak.upgrade().is_none());
    }

    #[gpui::test]
    fn closing_a_panes_last_tab_collapses_its_split(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("shared"));
        let mut documents = DocumentCollection::new();
        let document = cx.update(|cx| documents.create_untitled("Shared", model.clone(), cx));
        let workbench = cx.new(|cx| Workbench::new(documents.get(document).unwrap(), cx));

        workbench.update(cx, |workbench, cx| {
            let remaining_pane = workbench.focused_pane_id().unwrap();
            let closing_pane = workbench
                .split_focused(SplitDirection::Horizontal, SplitPlacement::After, cx)
                .unwrap();
            let closing_tab = workbench.focused_pane().unwrap().active_tab_id();
            let transition = match workbench
                .request_close_tab(
                    closing_pane,
                    closing_tab,
                    documents.get(document).unwrap(),
                    2,
                    cx,
                )
                .unwrap()
            {
                CloseRequestOutcome::Closed(transition) => transition,
                CloseRequestOutcome::Pending(_) => panic!("another view remains"),
            };

            assert_eq!(transition.removed_pane, Some(closing_pane));
            assert_eq!(transition.focused_pane, Some(remaining_pane));
            assert_eq!(
                transition.document,
                Some(DocumentCloseDisposition::Retained)
            );
            assert_eq!(
                workbench.layout(),
                Some(&super::super::WorkbenchLayout::Pane(remaining_pane))
            );
            assert_eq!(workbench.panes().len(), 1);
        });
    }

    #[gpui::test]
    fn dirty_final_view_waits_for_confirmation_and_last_tab_can_empty_workbench(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(|_| BufferModel::from_text("dirty"));
        let mut documents = DocumentCollection::new();
        let document = cx.update(|cx| documents.create_untitled("Dirty", model.clone(), cx));
        model.update(cx, |model, _| model.replace(0..0, "edited ").unwrap());
        let workbench = cx.new(|cx| Workbench::new(documents.get(document).unwrap(), cx));

        workbench.update(cx, |workbench, cx| {
            let pane = workbench.focused_pane_id().unwrap();
            let tab = workbench.focused_pane().unwrap().active_tab_id();
            let view = workbench
                .focused_pane()
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .downgrade();
            let pending = match workbench
                .request_close_tab(pane, tab, documents.get(document).unwrap(), 1, cx)
                .unwrap()
            {
                CloseRequestOutcome::Pending(pending) => pending,
                CloseRequestOutcome::Closed(_) => panic!("dirty final view must wait"),
            };

            assert_eq!(pending.document_id(), document);
            assert!(view.upgrade().is_some());
            assert_eq!(
                workbench.resolve_pending_close(pending, CloseConfirmation::Cancel, 1),
                None
            );
            assert!(view.upgrade().is_some());

            let pending = match workbench
                .request_close_tab(pane, tab, documents.get(document).unwrap(), 1, cx)
                .unwrap()
            {
                CloseRequestOutcome::Pending(pending) => pending,
                CloseRequestOutcome::Closed(_) => unreachable!(),
            };
            let transition = workbench
                .resolve_pending_close(pending, CloseConfirmation::Close, 1)
                .unwrap();
            assert!(transition.workbench_is_empty());
            assert_eq!(transition.removed_pane, Some(pane));
            assert_eq!(
                transition.document,
                Some(DocumentCloseDisposition::CloseRequested)
            );
            assert!(workbench.layout().is_none());
            assert!(workbench.panes().is_empty());
            assert!(view.upgrade().is_none());
        });
    }

    #[gpui::test]
    fn dirty_document_closes_without_confirmation_when_another_view_remains(
        cx: &mut TestAppContext,
    ) {
        let model = cx.new(|_| BufferModel::from_text("dirty"));
        let mut documents = DocumentCollection::new();
        let document = cx.update(|cx| documents.create_untitled("Dirty", model.clone(), cx));
        model.update(cx, |model, _| model.replace(0..0, "edited ").unwrap());
        let workbench = cx.new(|cx| Workbench::new(documents.get(document).unwrap(), cx));

        workbench.update(cx, |workbench, cx| {
            let first_pane = workbench.focused_pane_id().unwrap();
            let first_tab = workbench.focused_pane().unwrap().active_tab_id();
            workbench
                .split_focused(SplitDirection::Vertical, SplitPlacement::After, cx)
                .unwrap();

            let outcome = workbench
                .request_close_tab(
                    first_pane,
                    first_tab,
                    documents.get(document).unwrap(),
                    2,
                    cx,
                )
                .unwrap();
            assert!(matches!(
                outcome,
                CloseRequestOutcome::Closed(CloseTransition {
                    document: Some(DocumentCloseDisposition::Retained),
                    ..
                })
            ));
            assert_eq!(workbench.view_count(document), 1);
        });
    }

    #[gpui::test]
    fn stale_pending_close_cannot_close_a_different_tab(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("dirty"));
        let mut documents = DocumentCollection::new();
        let document = cx.update(|cx| documents.create_untitled("Dirty", model.clone(), cx));
        model.update(cx, |model, _| model.replace(0..0, "edited ").unwrap());
        let workbench = cx.new(|cx| Workbench::new(documents.get(document).unwrap(), cx));

        workbench.update(cx, |workbench, cx| {
            let pane = workbench.focused_pane_id().unwrap();
            let tab = workbench.focused_pane().unwrap().active_tab_id();
            let pending = match workbench
                .request_close_tab(pane, tab, documents.get(document).unwrap(), 1, cx)
                .unwrap()
            {
                CloseRequestOutcome::Pending(pending) => pending,
                CloseRequestOutcome::Closed(_) => unreachable!(),
            };
            assert!(workbench.remove_tab(pane, tab).is_some());
            assert_eq!(
                workbench.resolve_pending_close(pending, CloseConfirmation::Close, 1),
                None
            );
        });
    }

    #[gpui::test]
    fn pending_close_rechecks_application_view_count(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("dirty"));
        let mut documents = DocumentCollection::new();
        let document = cx.update(|cx| documents.create_untitled("Dirty", model.clone(), cx));
        model.update(cx, |model, _| model.replace(0..0, "edited ").unwrap());
        let workbench = cx.new(|cx| Workbench::new(documents.get(document).unwrap(), cx));

        workbench.update(cx, |workbench, cx| {
            let pane = workbench.focused_pane_id().unwrap();
            let tab = workbench.focused_pane().unwrap().active_tab_id();
            let pending = match workbench
                .request_close_tab(pane, tab, documents.get(document).unwrap(), 1, cx)
                .unwrap()
            {
                CloseRequestOutcome::Pending(pending) => pending,
                CloseRequestOutcome::Closed(_) => unreachable!(),
            };
            workbench
                .split_focused(SplitDirection::Horizontal, SplitPlacement::After, cx)
                .unwrap();

            let transition = workbench
                .resolve_pending_close(pending, CloseConfirmation::Close, 2)
                .unwrap();
            assert_eq!(
                transition.document,
                Some(DocumentCloseDisposition::Retained)
            );
            assert_eq!(workbench.view_count(document), 1);
        });
    }
}
