
use super::*;
use crate::app::{
    documents::{DocumentCollection, DocumentId},
    model::BufferModel,
};
use gpui::{AppContext, TestAppContext};

fn fixture(cx: &mut TestAppContext) -> (Entity<Workbench>, DocumentId, Entity<BufferModel>) {
    let model = cx.new(|_| BufferModel::from_text("first\nsecond\nthird"));
    let mut documents = DocumentCollection::new();
    let document = cx.update(|cx| documents.create_untitled("Moved", model.clone(), cx));
    let workbench = cx.new(|cx| Workbench::new_for_document(document, model.clone(), cx));
    (workbench, document, model)
}

fn destination(anchor: TabMoveCapture, placement: TabMovePlacement) -> TabMoveDestination {
    TabMoveDestination::At { anchor, placement }
}

#[gpui::test]
fn every_same_pane_insertion_preserves_editor_and_order(cx: &mut TestAppContext) {
    for source_index in 0..4 {
        for slot in 0..=4 {
            let (entity, document, model) = fixture(cx);
            entity.update(cx, |workbench, cx| {
                let pane = workbench.focused_pane_id().unwrap();
                for _ in 1..4 {
                    workbench
                        .open_tab_for_document(pane, document, model.clone(), cx)
                        .unwrap();
                }
                let old = workbench.tab_snapshots();
                let source = workbench
                    .capture_tab_move(pane, old[source_index].tab_id)
                    .unwrap();
                let editor = workbench.pane(pane).unwrap().tabs[source_index]
                    .editor()
                    .unwrap()
                    .clone();
                editor.update(cx, |editor, _| {
                    editor.set_test_presentation_state(1, 2, Some((0, 1)), 12.)
                });
                let placement = if slot == 4 {
                    TabMovePlacement::End
                } else {
                    TabMovePlacement::Before
                };
                let anchor = workbench
                    .capture_tab_move(pane, old[slot.min(3)].tab_id)
                    .unwrap();
                let moved = workbench
                    .relocate_tab(source, destination(anchor, placement))
                    .unwrap();
                let mut expected = old.clone();
                let moving = expected.remove(source_index);
                expected.insert(slot - usize::from(source_index < slot), moving);
                assert_eq!(workbench.tab_snapshots(), expected);
                assert_eq!(moved.destination, source.tab);
                assert_eq!(workbench.focused_pane_id(), Some(pane));
                assert_eq!(
                    workbench.pane(pane).unwrap().active_tab_id(),
                    source.tab.tab_id
                );
                assert_eq!(
                    workbench.pane(pane).unwrap().active_tab().editor(),
                    Some(&editor)
                );
                assert_eq!(
                    editor.read(cx).test_presentation_state(),
                    (1, 2, Some((0, 1)), 12.)
                );
                assert_eq!(workbench.view_count(document), 4);
                workbench.debug_assert_invariants();
            });
        }
    }
}

#[gpui::test]
fn all_edges_move_the_payload_and_order_the_new_pane(cx: &mut TestAppContext) {
    for edge in [
        SplitEdge::Left,
        SplitEdge::Right,
        SplitEdge::Top,
        SplitEdge::Bottom,
    ] {
        let (entity, document, model) = fixture(cx);
        entity.update(cx, |workbench, cx| {
            let pane = workbench.focused_pane_id().unwrap();
            let source = workbench
                .capture_tab_move(pane, workbench.pane(pane).unwrap().active_tab_id())
                .unwrap();
            let original = workbench
                .pane(pane)
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone();
            let neighbor = workbench
                .open_tab_for_document(pane, document, model, cx)
                .unwrap();
            let anchor = workbench.capture_tab_move(pane, neighbor).unwrap();
            let moved = workbench
                .relocate_tab(source, destination(anchor, TabMovePlacement::Split(edge)))
                .unwrap();
            let new_pane = moved.destination.pane_id;
            let (direction, placement) = edge.split();
            let (first, second) = if placement == SplitPlacement::Before {
                (new_pane, pane)
            } else {
                (pane, new_pane)
            };
            assert_eq!(
                workbench.layout(),
                Some(&WorkbenchLayout::Split {
                    direction,
                    first: Box::new(WorkbenchLayout::Pane(first)),
                    second: Box::new(WorkbenchLayout::Pane(second)),
                })
            );
            assert_eq!(workbench.pane(pane).unwrap().active_tab_id(), neighbor);
            assert_eq!(workbench.focused_pane_id(), Some(new_pane));
            assert_eq!(
                workbench.pane(new_pane).unwrap().active_tab().editor(),
                Some(&original)
            );
            assert_eq!(workbench.view_count(document), 2);
            assert!(!workbench.contains_tab_move(source));
            assert!(!moved.source_is_empty);
        });
    }
}

#[gpui::test]
fn single_tab_edge_move_collapses_the_original_pane(cx: &mut TestAppContext) {
    let (entity, document, _) = fixture(cx);
    entity.update(cx, |workbench, _| {
        let old = workbench.tab_snapshots()[0];
        let source = workbench.capture_tab_move(old.pane_id, old.tab_id).unwrap();
        let moved = workbench
            .relocate_tab(
                source,
                destination(source, TabMovePlacement::Split(SplitEdge::Left)),
            )
            .unwrap();
        assert_eq!(moved.removed_pane, Some(old.pane_id));
        assert_eq!(
            workbench.layout(),
            Some(&WorkbenchLayout::Pane(moved.destination.pane_id))
        );
        assert_eq!(workbench.panes.len(), 1);
        assert_eq!(workbench.view_count(document), 1);
    });
}

#[gpui::test]
fn moving_between_panes_collapses_a_nested_branch(cx: &mut TestAppContext) {
    let (entity, document, _) = fixture(cx);
    entity.update(cx, |workbench, cx| {
        let first = workbench.focused_pane_id().unwrap();
        let second = workbench
            .split_pane(first, SplitDirection::Horizontal, SplitPlacement::After, cx)
            .unwrap();
        let third = workbench
            .split_pane(second, SplitDirection::Vertical, SplitPlacement::After, cx)
            .unwrap();
        let source = workbench
            .capture_tab_move(second, workbench.pane(second).unwrap().active_tab_id())
            .unwrap();
        let anchor = workbench
            .capture_tab_move(first, workbench.pane(first).unwrap().active_tab_id())
            .unwrap();
        let moved = workbench
            .relocate_tab(source, destination(anchor, TabMovePlacement::Before))
            .unwrap();
        assert_eq!(moved.removed_pane, Some(second));
        assert_eq!(
            workbench.layout(),
            Some(&WorkbenchLayout::Split {
                direction: SplitDirection::Horizontal,
                first: Box::new(WorkbenchLayout::Pane(first)),
                second: Box::new(WorkbenchLayout::Pane(third)),
            })
        );
        assert_eq!(workbench.pane(first).unwrap().tabs[0].id, source.tab.tab_id);
        assert_eq!(workbench.view_count(document), 3);
    });
}

#[gpui::test]
fn cross_workbench_transfer_allocates_destination_ids_and_preserves_editor(
    cx: &mut TestAppContext,
) {
    let (source_entity, document, model) = fixture(cx);
    let destination_entity = cx.new(|cx| Workbench::new_for_document(document, model, cx));
    source_entity.update(cx, |source, cx| {
        destination_entity.update(cx, |destination, _| {
            let captured = source.tab_snapshots()[0];
            let capture = source
                .capture_tab_move(captured.pane_id, captured.tab_id)
                .unwrap();
            let original = source
                .pane(captured.pane_id)
                .unwrap()
                .active_tab()
                .editor()
                .unwrap()
                .clone();
            let anchor = destination.tab_snapshots()[0];
            let anchor = destination
                .capture_tab_move(anchor.pane_id, anchor.tab_id)
                .unwrap();
            let moved = source
                .transfer_tab_to(
                    destination,
                    capture,
                    self::destination(anchor, TabMovePlacement::End),
                    |_| panic!("document transfer cannot hand off a terminal"),
                )
                .unwrap();
            assert_eq!(moved.destination.tab_id, TabId(2));
            assert!(moved.source_is_empty);
            assert!(source.layout().is_none());
            assert!(!source.contains_tab_move(capture));
            assert_eq!(
                destination.focused_pane().unwrap().active_tab().editor(),
                Some(&original)
            );
            assert_eq!(source.view_count(document), 0);
            assert_eq!(destination.view_count(document), 2);
        });
    });
}

#[gpui::test]
fn failed_or_stale_transfers_leave_both_workbenches_intact(cx: &mut TestAppContext) {
    let (source_entity, document, model) = fixture(cx);
    let destination_entity = cx.new(|cx| Workbench::new_for_document(document, model.clone(), cx));
    source_entity.update(cx, |source, cx| {
        destination_entity.update(cx, |destination, cx| {
            let source_snapshot = source.tab_snapshots()[0];
            let capture = source
                .capture_tab_move(source_snapshot.pane_id, source_snapshot.tab_id)
                .unwrap();
            let anchor_snapshot = destination.tab_snapshots()[0];
            let anchor = destination
                .capture_tab_move(anchor_snapshot.pane_id, anchor_snapshot.tab_id)
                .unwrap();
            let before = (source.tab_snapshots(), destination.tab_snapshots());
            assert!(
                source
                    .transfer_tab_to(destination, capture, TabMoveDestination::Empty, |_| panic!(
                        "failed transfer cannot hand off"
                    ))
                    .is_none()
            );
            destination.replace_tab_document(
                anchor_snapshot.pane_id,
                anchor_snapshot.tab_id,
                document,
                model.clone(),
                cx,
            );
            assert!(
                source
                    .transfer_tab_to(
                        destination,
                        capture,
                        self::destination(anchor, TabMovePlacement::End),
                        |_| panic!("stale destination cannot hand off")
                    )
                    .is_none()
            );
            let anchor = destination
                .capture_tab_move(anchor_snapshot.pane_id, anchor_snapshot.tab_id)
                .unwrap();
            source.replace_tab_document(
                source_snapshot.pane_id,
                source_snapshot.tab_id,
                document,
                model,
                cx,
            );
            assert!(
                source
                    .transfer_tab_to(
                        destination,
                        capture,
                        self::destination(anchor, TabMovePlacement::End),
                        |_| panic!("stale source cannot hand off")
                    )
                    .is_none()
            );
            assert_eq!(
                (source.tab_snapshots(), destination.tab_snapshots()),
                before
            );
            assert_eq!(source.next_tab_id, 2);
            assert_eq!(destination.next_tab_id, 2);
        });
    });
}

#[gpui::test]
fn terminal_transfer_keeps_session_and_rejects_duplicate_destination(cx: &mut TestAppContext) {
    let (entity, document, _) = fixture(cx);
    entity.update(cx, |source, _| {
        let pane = source.focused_pane_id().unwrap();
        let session = super::super::TerminalSessionId(41);
        let tab = source.open_terminal_tab_test_double(pane, session).unwrap();
        let capture = source.capture_tab_move(pane, tab).unwrap();
        let mut destination = Workbench::empty();
        let moved = source
            .transfer_tab_to(&mut destination, capture, TabMoveDestination::Empty, |_| {
                panic!("test double has no attachment")
            })
            .unwrap();
        assert_eq!(
            moved.destination.surface,
            super::super::TabSurfaceId::Terminal(session)
        );
        assert_eq!(source.view_count(document), 1);
        assert_eq!(destination.view_count(document), 0);
        let tab = source.open_terminal_tab_test_double(pane, session).unwrap();
        let capture = source.capture_tab_move(pane, tab).unwrap();
        let anchor = destination.tab_snapshots()[0];
        let anchor = destination
            .capture_tab_move(anchor.pane_id, anchor.tab_id)
            .unwrap();
        let before = source.tab_snapshots();
        assert!(
            source
                .transfer_tab_to(
                    &mut destination,
                    capture,
                    self::destination(anchor, TabMovePlacement::End),
                    |_| panic!("duplicate session cannot hand off")
                )
                .is_none()
        );
        assert_eq!(source.tab_snapshots(), before);
    });
}
