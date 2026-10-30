//! Split ratios and pane geometry: dividers, keyboard resizing, equalizing,
//! and directional neighbors.

use super::*;
use crate::app::window_layout::Rect;

/// The share of a split's extent that one grow or shrink moves.
const RESIZE_STEP: f32 = 0.05;
/// Logical pixels that one grow or shrink adds to or removes from a dock.
const DOCK_RESIZE_STEP: f32 = 40.;

/// The dock whose absolute size a split's divider controls: a direct child
/// that is a dock lying along the split's direction.
fn dock_of_split(
    docks: &[(PaneId, DockLocation)],
    layout: &WorkbenchLayout,
) -> Option<DockLocation> {
    let WorkbenchLayout::Split {
        direction,
        first,
        second,
        ..
    } = layout
    else {
        return None;
    };
    [first, second]
        .into_iter()
        .find_map(|child| match child.as_ref() {
            WorkbenchLayout::Pane(pane) => docks
                .iter()
                .find(|(id, _)| id == pane)
                .map(|(_, dock)| *dock)
                .filter(|dock| {
                    (*direction == SplitDirection::Vertical) == (*dock == DockLocation::Bottom)
                }),
            WorkbenchLayout::Split { .. } => None,
        })
}

fn find_split(layout: &WorkbenchLayout, boundary: PaneId) -> Option<&WorkbenchLayout> {
    if layout.boundary() == Some(boundary) {
        return Some(layout);
    }
    match layout {
        WorkbenchLayout::Pane(_) => None,
        WorkbenchLayout::Split { first, second, .. } => {
            find_split(first, boundary).or_else(|| find_split(second, boundary))
        }
    }
}

fn equalize(layout: &mut WorkbenchLayout, docks: &[(PaneId, DockLocation)]) {
    let ordinary = dock_of_split(docks, layout).is_none();
    if let WorkbenchLayout::Split {
        direction,
        ratio,
        first,
        second,
    } = layout
    {
        if ordinary {
            let first_weight = first.weight(*direction);
            *ratio = first_weight / (first_weight + second.weight(*direction));
        }
        equalize(first, docks);
        equalize(second, docks);
    }
}

/// Lay panes out in `area` from split ratios and dock sizes, ignoring
/// dividers.
fn place(
    layout: &WorkbenchLayout,
    area: Rect,
    docks: &[(PaneId, DockLocation)],
    sizes: [f32; 3],
    result: &mut Vec<(PaneId, Rect)>,
) {
    let WorkbenchLayout::Split {
        direction,
        ratio,
        first,
        second,
    } = layout
    else {
        if let WorkbenchLayout::Pane(pane) = layout {
            result.push((*pane, area));
        }
        return;
    };
    let horizontal = *direction == SplitDirection::Horizontal;
    let extent = if horizontal { area.width } else { area.height };
    let first_extent = match dock_of_split(docks, layout) {
        Some(dock) => {
            let size = sizes[dock.index()].min(extent);
            let first_is_dock = matches!(first.as_ref(), WorkbenchLayout::Pane(pane)
                if docks.contains(&(*pane, dock)));
            if first_is_dock { size } else { extent - size }
        }
        None => extent * ratio,
    };
    let (first_area, second_area) = if horizontal {
        (
            Rect {
                width: first_extent,
                ..area
            },
            Rect {
                x: area.x + first_extent,
                width: area.width - first_extent,
                ..area
            },
        )
    } else {
        (
            Rect {
                height: first_extent,
                ..area
            },
            Rect {
                y: area.y + first_extent,
                height: area.height - first_extent,
                ..area
            },
        )
    };
    place(first, first_area, docks, sizes, result);
    place(second, second_area, docks, sizes, result);
}

/// The pane beside `from` toward `edge`: of the panes lying entirely on that
/// side, the one overlapping `from` most along the other axis, then the
/// nearest.
pub(crate) fn neighbor(rects: &[(PaneId, Rect)], from: PaneId, edge: SplitEdge) -> Option<PaneId> {
    const EPSILON: f32 = 0.5;
    let (_, source) = rects.iter().find(|(pane, _)| *pane == from)?;
    let overlap = |a: f32, a_len: f32, b: f32, b_len: f32| (a + a_len).min(b + b_len) - a.max(b);
    rects
        .iter()
        .filter(|(pane, _)| *pane != from)
        .filter_map(|(pane, rect)| {
            let (distance, shared) = match edge {
                SplitEdge::Left => (
                    source.x - (rect.x + rect.width),
                    overlap(source.y, source.height, rect.y, rect.height),
                ),
                SplitEdge::Right => (
                    rect.x - (source.x + source.width),
                    overlap(source.y, source.height, rect.y, rect.height),
                ),
                SplitEdge::Top => (
                    source.y - (rect.y + rect.height),
                    overlap(source.x, source.width, rect.x, rect.width),
                ),
                SplitEdge::Bottom => (
                    rect.y - (source.y + source.height),
                    overlap(source.x, source.width, rect.x, rect.width),
                ),
            };
            (distance > -EPSILON && shared > EPSILON).then_some((*pane, shared, distance))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1).then(b.2.total_cmp(&a.2)))
        .map(|(pane, _, _)| pane)
}

impl Workbench {
    /// Every pane's rectangle when the workbench fills `area`.
    pub(crate) fn pane_rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut result = Vec::new();
        if let Some(layout) = &self.layout {
            place(layout, area, &self.docks, self.dock_sizes, &mut result);
        }
        result
    }

    /// The dock a split's divider resizes, or `None` for an ordinary split
    /// that divides its extent by ratio.
    pub(crate) fn split_dock(&self, layout: &WorkbenchLayout) -> Option<DockLocation> {
        dock_of_split(&self.docks, layout)
    }

    pub(crate) fn split_ratio(&self, boundary: PaneId) -> Option<f32> {
        match find_split(self.layout.as_ref()?, boundary)? {
            WorkbenchLayout::Split { ratio, .. } => Some(*ratio),
            WorkbenchLayout::Pane(_) => None,
        }
    }

    /// Set the ratio of the split identified by `boundary`, clamped so
    /// neither side drops below the minimum share.
    pub(crate) fn set_split_ratio(&mut self, boundary: PaneId, value: f32) -> bool {
        let Some(WorkbenchLayout::Split { ratio, .. }) = self
            .layout
            .as_mut()
            .and_then(|layout| layout.split_with_boundary_mut(boundary))
        else {
            return false;
        };
        *ratio = WorkbenchLayout::clamp_ratio(value);
        true
    }

    /// Grow or shrink a pane by one step: a dock changes its size, any other
    /// pane the ratio of its nearest enclosing ordinary split. Returns whether
    /// anything changed.
    pub(crate) fn resize_pane(&mut self, pane: PaneId, grow: bool) -> bool {
        let sign = if grow { 1. } else { -1. };
        if let Some(dock) = self.dock_location(pane) {
            let before = self.dock_size(dock);
            self.set_dock_size(dock, before + sign * DOCK_RESIZE_STEP);
            return self.dock_size(dock) != before;
        }
        let Some((boundary, in_second, ratio)) = self.layout.as_ref().and_then(|layout| {
            layout
                .ancestors(pane)
                .into_iter()
                .rev()
                .find(|(split, _)| self.split_dock(split).is_none())
                .and_then(|(split, in_second)| match split {
                    WorkbenchLayout::Split { ratio, .. } => {
                        Some((split.boundary()?, in_second, *ratio))
                    }
                    WorkbenchLayout::Pane(_) => None,
                })
        }) else {
            return false;
        };
        let delta = if in_second { -sign } else { sign } * RESIZE_STEP;
        self.set_split_ratio(boundary, ratio + delta);
        self.split_ratio(boundary) != Some(ratio)
    }

    /// Give every pane of each ordinary split chain an even share.
    pub(crate) fn equalize_splits(&mut self) {
        if let Some(layout) = self.layout.as_mut() {
            equalize(layout, &self.docks);
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext};

    use super::*;
    use crate::app::{documents::DocumentCollection, model::BufferModel};

    fn ratios(layout: &WorkbenchLayout) -> Vec<f32> {
        match layout {
            WorkbenchLayout::Pane(_) => Vec::new(),
            WorkbenchLayout::Split {
                ratio,
                first,
                second,
                ..
            } => {
                let mut result = vec![*ratio];
                result.extend(ratios(first));
                result.extend(ratios(second));
                result
            }
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[gpui::test]
    fn grow_and_shrink_move_the_nearest_split_and_clamp(cx: &mut TestAppContext) {
        let model = cx.new(|_| BufferModel::from_text("shared"));
        let mut documents = DocumentCollection::new();
        let document = cx.update(|cx| documents.create_untitled("Shared", model, cx));
        let workbench = cx.new(|cx| Workbench::new(documents.get(document).unwrap(), cx));

        workbench.update(cx, |workbench, cx| {
            let left = workbench.focused_pane_id().unwrap();
            let right = workbench
                .split_focused(SplitDirection::Horizontal, SplitPlacement::After, cx)
                .unwrap();
            let bottom = workbench
                .split_focused(SplitDirection::Vertical, SplitPlacement::After, cx)
                .unwrap();
            assert_eq!(workbench.split_ratio(right), Some(0.5));

            // The bottom pane's nearest split is the vertical one.
            assert!(workbench.resize_pane(bottom, true));
            assert!(close(workbench.split_ratio(bottom).unwrap(), 0.45));
            assert_eq!(workbench.split_ratio(right), Some(0.5));
            // The left pane grows its horizontal split.
            assert!(workbench.resize_pane(left, true));
            assert!(close(workbench.split_ratio(right).unwrap(), 0.55));
            // The right pane's nearest split is the vertical one.
            assert!(workbench.resize_pane(right, true));
            assert!(close(workbench.split_ratio(bottom).unwrap(), 0.5));
            assert!(workbench.resize_pane(left, false));
            assert!(close(workbench.split_ratio(right).unwrap(), 0.5));

            for _ in 0..40 {
                workbench.resize_pane(left, true);
            }
            assert!(close(
                workbench.split_ratio(right).unwrap(),
                1. - WorkbenchLayout::MIN_RATIO
            ));
            assert!(!workbench.resize_pane(left, true));
            assert!(workbench.set_split_ratio(right, -3.));
            assert!(close(
                workbench.split_ratio(right).unwrap(),
                WorkbenchLayout::MIN_RATIO
            ));
            assert!(!workbench.set_split_ratio(left, 0.3));
        });
    }

    #[gpui::test]
    fn equalize_gives_each_pane_of_a_chain_an_even_share(cx: &mut TestAppContext) {
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
                .split_focused(SplitDirection::Horizontal, SplitPlacement::After, cx)
                .unwrap();
            workbench
                .split_focused(SplitDirection::Vertical, SplitPlacement::After, cx)
                .unwrap();
            workbench.set_split_ratio(second, 0.8);
            workbench.set_split_ratio(third, 0.2);
            workbench.equalize_splits();
            let layout = workbench.layout().unwrap();
            // first | (second | (third / fourth)): thirds across, halves down.
            let found = ratios(layout);
            assert!(close(found[0], 1. / 3.), "{found:?}");
            assert!(close(found[1], 0.5), "{found:?}");
            assert!(close(found[2], 0.5), "{found:?}");
            assert!(workbench.pane(first).is_some());
        });
    }

    #[gpui::test]
    fn collapsing_a_pane_keeps_its_siblings_ratios(cx: &mut TestAppContext) {
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
                .split_focused(SplitDirection::Vertical, SplitPlacement::After, cx)
                .unwrap();
            workbench.set_split_ratio(third, 0.3);
            workbench.set_split_ratio(second, 0.7);
            let tab = workbench.pane(first).unwrap().active_tab_id();
            workbench.remove_tab(first, tab).unwrap();
            assert_eq!(workbench.split_ratio(third), Some(0.3));
            assert_eq!(ratios(workbench.layout().unwrap()), vec![0.3]);
        });
    }

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn neighbors_lie_on_the_side_and_overlap_most_along_the_other_axis() {
        // A left dock, a large pane split right into a top and bottom pair,
        // and a bottom dock across everything but the left dock.
        let [dock, main, top, bottom, panel] = [1, 2, 3, 4, 5].map(PaneId);
        let rects = [
            (dock, rect(0., 0., 200., 800.)),
            (main, rect(200., 0., 500., 600.)),
            (top, rect(700., 0., 300., 200.)),
            (bottom, rect(700., 200., 300., 400.)),
            (panel, rect(200., 600., 800., 200.)),
        ];
        assert_eq!(neighbor(&rects, main, SplitEdge::Right), Some(bottom));
        assert_eq!(neighbor(&rects, main, SplitEdge::Left), Some(dock));
        assert_eq!(neighbor(&rects, main, SplitEdge::Bottom), Some(panel));
        assert_eq!(neighbor(&rects, main, SplitEdge::Top), None);
        assert_eq!(neighbor(&rects, top, SplitEdge::Bottom), Some(bottom));
        assert_eq!(neighbor(&rects, top, SplitEdge::Left), Some(main));
        assert_eq!(neighbor(&rects, top, SplitEdge::Right), None);
        assert_eq!(neighbor(&rects, panel, SplitEdge::Top), Some(main));
        assert_eq!(neighbor(&rects, panel, SplitEdge::Left), Some(dock));
        assert_eq!(neighbor(&rects, dock, SplitEdge::Right), Some(main));
        assert_eq!(neighbor(&rects, PaneId(9), SplitEdge::Right), None);
    }

    #[gpui::test]
    fn pane_rects_follow_ratios_and_dock_sizes(cx: &mut TestAppContext) {
        let workbench = cx.new(|_| Workbench::empty());
        workbench.update(cx, |workbench, cx| {
            let model = cx.new(|_| BufferModel::from_text(""));
            let mut documents = DocumentCollection::new();
            let document = documents.create_untitled("doc", model.clone(), cx);
            let (left, _) = workbench.open_document_in_normal_group(document, model, cx);
            let right = workbench
                .split_pane(left, SplitDirection::Horizontal, SplitPlacement::After, cx)
                .unwrap();
            workbench.set_split_ratio(right, 0.25);
            let tree = cx.new(|cx| {
                crate::app::workspace_tree::WorkspaceTree::new(
                    crate::app::workspace::WorkspaceState::default().snapshot(),
                    cx,
                )
            });
            let id = workbench.register_tool(
                crate::app::tool_layout::EXPLORER_TOOL_ID,
                "Explorer",
                ToolContent::Workspace(tree),
                DockLocation::Left,
            );
            let (dock, _) = workbench.show_tool(id).unwrap();
            workbench.set_dock_size(DockLocation::Left, 200.);
            let rects = workbench.pane_rects(rect(0., 0., 1000., 500.));
            assert_eq!(
                rects,
                [
                    (dock, rect(0., 0., 200., 500.)),
                    (left, rect(200., 0., 200., 500.)),
                    (right, rect(400., 0., 600., 500.)),
                ]
            );
        });
    }

    #[gpui::test]
    fn docks_resize_by_size_and_keep_their_split_out_of_equalize(cx: &mut TestAppContext) {
        let workbench = cx.new(|_| Workbench::empty());
        workbench.update(cx, |workbench, cx| {
            let model = cx.new(|_| BufferModel::from_text(""));
            let mut documents = DocumentCollection::new();
            let document = documents.create_untitled("doc", model.clone(), cx);
            let (pane, _) = workbench.open_document_in_normal_group(document, model, cx);
            let tree = cx.new(|cx| {
                crate::app::workspace_tree::WorkspaceTree::new(
                    crate::app::workspace::WorkspaceState::default().snapshot(),
                    cx,
                )
            });
            let id = workbench.register_tool(
                crate::app::tool_layout::EXPLORER_TOOL_ID,
                "Explorer",
                ToolContent::Workspace(tree),
                DockLocation::Left,
            );
            let (dock, _) = workbench.show_tool(id).unwrap();
            let before = workbench.dock_size(DockLocation::Left);
            assert!(workbench.resize_pane(dock, true));
            assert_eq!(workbench.dock_size(DockLocation::Left), before + 40.);
            // The document pane's only split is the dock's, so nothing moves.
            assert!(!workbench.resize_pane(pane, true));
            assert_eq!(
                workbench.split_dock(workbench.layout().unwrap()),
                Some(DockLocation::Left)
            );
            workbench.set_split_ratio(pane, 0.3);
            workbench.equalize_splits();
            assert_eq!(workbench.split_ratio(pane), Some(0.3));
        });
    }
}
