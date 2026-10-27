//! Versioned window layouts shared by native window hot exit and extension
//! sessions.
//!
//! A layout records window geometry, the split tree with dock groups, ordered
//! tabs and the selected tab per pane, the focused pane, dock sizes, and each
//! editor tab's view state. It never contains pane, tab, view, or window
//! handles. Every tab records only what its kind needs to recreate a
//! presentation in the same position: a document tab its document identity and
//! resource URI, a tool tab its stable tool ID, and a terminal tab nothing.
//!
//! Restoring plans a pruned arrangement in one place for both restorers:
//! unresolved documents, unregistered or repeated tools, and repeated documents
//! within a pane are dropped, then empty panes and one-child splits collapse.

use gpui::App;
use serde::{Deserialize, Serialize};

use super::{
    documents::DocumentCollection,
    editor::EditorViewState,
    workbench::{DockLocation, SplitDirection, TabSurfaceId, Workbench, WorkbenchLayout},
};

pub(crate) const WINDOW_LAYOUT_VERSION: u32 = 1;

/// Maps a document tab's identity and URI to a live document.
type Resolve<'a, D> = dyn FnMut(Option<&str>, Option<&str>) -> Option<D> + 'a;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WindowLayout {
    pub(crate) version: u32,
    pub(crate) geometry: Option<WindowGeometry>,
    pub(crate) root: Option<LayoutNode>,
    /// Index of the focused pane in layout order.
    pub(crate) focused: Option<usize>,
    pub(crate) dock_sizes: DockSizes,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct DockSizes {
    pub(crate) left: f32,
    pub(crate) right: f32,
    pub(crate) bottom: f32,
}

impl DockSizes {
    fn from_array([left, right, bottom]: [f32; 3]) -> Self {
        Self {
            left,
            right,
            bottom,
        }
    }

    pub(crate) fn to_array(self) -> [f32; 3] {
        [self.left, self.right, self.bottom]
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum LayoutNode {
    Pane {
        dock: Option<DockLocation>,
        selected: usize,
        tabs: Vec<LayoutTab>,
    },
    Split {
        direction: SplitDirection,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum LayoutTab {
    Document {
        document: Option<String>,
        uri: Option<String>,
        view: EditorViewState,
    },
    Tool {
        tool: String,
    },
    Terminal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum WindowState {
    Windowed,
    Maximized,
    Fullscreen,
}

/// Logical window bounds, presentation state, and the display it was on.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(crate) struct WindowGeometry {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) state: WindowState,
    pub(crate) display: Option<String>,
}

impl WindowGeometry {
    pub(crate) fn rect(&self) -> Rect {
        Rect {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
        }
    }
}

/// Transported JSON numbers are doubles; offsets and indices decode from their
/// integral values.
fn integral(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Number(number) => match number.as_f64() {
            Some(float) if float.fract() == 0. && (0. ..=u64::MAX as f64).contains(&float) => {
                Value::from(float as u64)
            }
            _ => Value::Number(number),
        },
        Value::Array(items) => Value::Array(items.into_iter().map(integral).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, integral(value)))
                .collect(),
        ),
        other => other,
    }
}

impl WindowLayout {
    /// Decode a stored layout. An unsupported version or invalid shape yields
    /// `None`, so callers fall back to their layout-free arrangement.
    pub(crate) fn decode(value: &serde_json::Value) -> Option<Self> {
        let value = integral(value.clone());
        if value.get("version").and_then(serde_json::Value::as_u64)
            != Some(u64::from(WINDOW_LAYOUT_VERSION))
        {
            return None;
        }
        let layout: Self = serde_json::from_value(value).ok()?;
        let sizes = layout.dock_sizes.to_array();
        let geometry = layout.geometry.as_ref().is_none_or(|geometry| {
            [geometry.x, geometry.y, geometry.width, geometry.height]
                .iter()
                .all(|value| value.is_finite())
                && geometry.width > 0.
                && geometry.height > 0.
        });
        (sizes.iter().all(|size| size.is_finite()) && geometry).then_some(layout)
    }

    /// Capture a workbench with its window geometry.
    pub(crate) fn capture(
        workbench: &Workbench,
        geometry: Option<WindowGeometry>,
        documents: &DocumentCollection,
        cx: &App,
    ) -> Self {
        fn node(
            layout: &WorkbenchLayout,
            workbench: &Workbench,
            documents: &DocumentCollection,
            cx: &App,
        ) -> LayoutNode {
            match layout {
                WorkbenchLayout::Pane(id) => {
                    let pane = workbench.pane(*id).expect("layout panes are live");
                    let tabs = pane
                        .tabs()
                        .iter()
                        .map(|tab| match tab.surface_id() {
                            TabSurfaceId::Document(id) => {
                                let document = documents.get(id);
                                LayoutTab::Document {
                                    document: document
                                        .map(|document| document.identity().to_string()),
                                    uri: document
                                        .and_then(|document| document.resource_uri())
                                        .map(ToString::to_string),
                                    view: tab
                                        .editor()
                                        .map(|editor| editor.read(cx).view_state())
                                        .unwrap_or_default(),
                                }
                            }
                            TabSurfaceId::Tool(id) => LayoutTab::Tool {
                                tool: workbench
                                    .tool(id)
                                    .map(|tool| tool.stable_id.clone())
                                    .unwrap_or_default(),
                            },
                            TabSurfaceId::Terminal(_) => LayoutTab::Terminal,
                        })
                        .collect();
                    LayoutNode::Pane {
                        dock: workbench.dock_location(*id),
                        selected: pane
                            .tabs()
                            .iter()
                            .position(|tab| tab.id() == pane.active_tab_id())
                            .unwrap_or_default(),
                        tabs,
                    }
                }
                WorkbenchLayout::Split {
                    direction,
                    first,
                    second,
                } => LayoutNode::Split {
                    direction: *direction,
                    first: Box::new(node(first, workbench, documents, cx)),
                    second: Box::new(node(second, workbench, documents, cx)),
                },
            }
        }
        let order = workbench
            .tab_snapshots()
            .into_iter()
            .map(|tab| tab.pane_id)
            .fold(Vec::new(), |mut panes, pane| {
                if !panes.contains(&pane) {
                    panes.push(pane);
                }
                panes
            });
        Self {
            version: WINDOW_LAYOUT_VERSION,
            geometry,
            root: workbench
                .layout()
                .map(|layout| node(layout, workbench, documents, cx)),
            focused: workbench
                .focused_pane_id()
                .and_then(|focused| order.iter().position(|pane| *pane == focused)),
            dock_sizes: DockSizes::from_array(
                DockLocation::ALL.map(|dock| workbench.dock_size(dock)),
            ),
        }
    }

    /// Distinct document references of this layout's document tabs, as
    /// identity and URI, in layout order.
    pub(crate) fn documents(&self) -> Vec<(Option<String>, Option<String>)> {
        fn walk(node: &LayoutNode, documents: &mut Vec<(Option<String>, Option<String>)>) {
            match node {
                LayoutNode::Pane { tabs, .. } => {
                    for tab in tabs {
                        if let LayoutTab::Document { document, uri, .. } = tab {
                            let reference = (document.clone(), uri.clone());
                            if !documents.contains(&reference) {
                                documents.push(reference);
                            }
                        }
                    }
                }
                LayoutNode::Split { first, second, .. } => {
                    walk(first, documents);
                    walk(second, documents);
                }
            }
        }
        let mut documents = Vec::new();
        if let Some(root) = &self.root {
            walk(root, &mut documents);
        }
        documents
    }

    /// Whether any tab of this layout presents the given tool.
    pub(crate) fn names_tool(&self, tool: &str) -> bool {
        fn walk(node: &LayoutNode, tool: &str) -> bool {
            match node {
                LayoutNode::Pane { tabs, .. } => tabs
                    .iter()
                    .any(|tab| matches!(tab, LayoutTab::Tool { tool: named } if named == tool)),
                LayoutNode::Split { first, second, .. } => walk(first, tool) || walk(second, tool),
            }
        }
        self.root.as_ref().is_some_and(|root| walk(root, tool))
    }

    /// Plan the pruned arrangement of this layout. `resolve` maps a document
    /// tab's identity and URI to a live document; `has_tool` reports whether
    /// a stable tool ID is registered in the destination window.
    pub(crate) fn plan<D: Copy + PartialEq>(
        &self,
        mut resolve: impl FnMut(Option<&str>, Option<&str>) -> Option<D>,
        has_tool: impl Fn(&str) -> bool,
    ) -> Option<PlanNode<PlannedTab<D>>> {
        struct Pruning<'a, D> {
            resolve: &'a mut Resolve<'a, D>,
            has_tool: &'a dyn Fn(&str) -> bool,
            tools: Vec<String>,
            docks: Vec<DockLocation>,
            pane: usize,
            focused: Option<usize>,
        }
        fn prune<D: Copy + PartialEq>(
            node: &LayoutNode,
            state: &mut Pruning<'_, D>,
        ) -> Option<PlanNode<PlannedTab<D>>> {
            match node {
                LayoutNode::Pane {
                    dock,
                    selected,
                    tabs,
                } => {
                    let index = state.pane;
                    state.pane += 1;
                    let mut planned = Vec::new();
                    let mut selection = 0;
                    for (position, tab) in tabs.iter().enumerate() {
                        let kept = match tab {
                            LayoutTab::Document {
                                document,
                                uri,
                                view,
                            } => (state.resolve)(document.as_deref(), uri.as_deref())
                                .filter(|resolved| {
                                    !planned.iter().any(|tab| {
                                        matches!(tab, PlannedTab::Document { document, .. } if document == resolved)
                                    })
                                })
                                .map(|document| PlannedTab::Document {
                                    document,
                                    view: *view,
                                }),
                            LayoutTab::Tool { tool } => ((state.has_tool)(tool)
                                && !state.tools.contains(tool))
                            .then(|| {
                                state.tools.push(tool.clone());
                                PlannedTab::Tool(tool.clone())
                            }),
                            LayoutTab::Terminal => Some(PlannedTab::Terminal),
                        };
                        if let Some(tab) = kept {
                            if position == *selected {
                                selection = planned.len();
                            }
                            planned.push(tab);
                        }
                    }
                    if planned.is_empty() {
                        return None;
                    }
                    let dock = dock.filter(|dock| !state.docks.contains(dock));
                    state.docks.extend(dock);
                    Some(PlanNode::Pane {
                        dock,
                        selected: selection,
                        focused: state.focused == Some(index),
                        tabs: planned,
                    })
                }
                LayoutNode::Split {
                    direction,
                    first,
                    second,
                } => match (prune(first, state), prune(second, state)) {
                    (Some(first), Some(second)) => Some(PlanNode::Split {
                        direction: *direction,
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                    (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                    (None, None) => None,
                },
            }
        }
        let root = self.root.as_ref()?;
        prune(
            root,
            &mut Pruning {
                resolve: &mut resolve,
                has_tool: &has_tool,
                tools: Vec::new(),
                docks: Vec::new(),
                pane: 0,
                focused: self.focused,
            },
        )
    }
}

/// A pruned arrangement whose tabs are materialized per kind before the
/// workbench installs it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PlanNode<T> {
    Pane {
        dock: Option<DockLocation>,
        selected: usize,
        focused: bool,
        tabs: Vec<T>,
    },
    Split {
        direction: SplitDirection,
        first: Box<PlanNode<T>>,
        second: Box<PlanNode<T>>,
    },
}

impl<T> PlanNode<T> {
    pub(crate) fn map<U>(self, step: &mut impl FnMut(T) -> U) -> PlanNode<U> {
        match self {
            Self::Pane {
                dock,
                selected,
                focused,
                tabs,
            } => PlanNode::Pane {
                dock,
                selected,
                focused,
                tabs: tabs.into_iter().map(&mut *step).collect(),
            },
            Self::Split {
                direction,
                first,
                second,
            } => PlanNode::Split {
                direction,
                first: Box::new(first.map(step)),
                second: Box::new(second.map(step)),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PlannedTab<D> {
    Document { document: D, view: EditorViewState },
    Tool(String),
    Terminal,
}

/// A logical-pixel rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl Rect {
    fn intersection(&self, other: &Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right > x && bottom > y).then_some(Self {
            x,
            y,
            width: right - x,
            height: bottom - y,
        })
    }
}

/// A connected display: its persisted identity and logical bounds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DisplayArea {
    pub(crate) uuid: Option<String>,
    pub(crate) bounds: Rect,
}

/// Height of the strip a window must keep on a display so its title bar can
/// still be grabbed, and the minimum reachable width of that strip.
const TITLE_BAR_HEIGHT: f32 = 28.;
const TITLE_BAR_REACH: f32 = 80.;

/// Fit saved bounds to the connected displays. Bounds are kept when enough of
/// their title bar lies on some display; otherwise the window is sized to fit
/// and centered on its saved display, or else on the primary display. Returns
/// the bounds and the index of the display they were fitted to, if any.
pub(crate) fn fit_to_displays(
    saved: &WindowGeometry,
    displays: &[DisplayArea],
    primary: Option<usize>,
) -> (Rect, Option<usize>) {
    let bounds = saved.rect();
    let title = Rect {
        height: TITLE_BAR_HEIGHT.min(bounds.height),
        ..bounds
    };
    let reachable = displays.iter().position(|display| {
        title.intersection(&display.bounds).is_some_and(|visible| {
            visible.width >= TITLE_BAR_REACH.min(bounds.width)
                && visible.height >= title.height.min(TITLE_BAR_HEIGHT / 2.)
        })
    });
    if let Some(index) = reachable {
        return (bounds, Some(index));
    }
    let target = saved
        .display
        .as_ref()
        .and_then(|uuid| {
            displays
                .iter()
                .position(|display| display.uuid.as_ref() == Some(uuid))
        })
        .or(primary)
        .or((!displays.is_empty()).then_some(0));
    let Some(index) = target else {
        return (bounds, None);
    };
    let area = displays[index].bounds;
    let width = bounds.width.min(area.width);
    let height = bounds.height.min(area.height);
    (
        Rect {
            x: area.x + (area.width - width) / 2.,
            y: area.y + (area.height - height) / 2.,
            width,
            height,
        },
        Some(index),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view(caret: usize) -> EditorViewState {
        EditorViewState {
            anchor: caret,
            caret,
            top: 0,
            scroll_x: 0.,
        }
    }

    fn document(id: &str) -> serde_json::Value {
        json!({ "kind": "document", "document": id, "uri": null,
                "view": { "anchor": 1, "caret": 1, "top": 0, "scrollX": 0 } })
    }

    fn layout(root: serde_json::Value, focused: Option<usize>) -> WindowLayout {
        WindowLayout::decode(&json!({
            "version": 1,
            "geometry": null,
            "root": root,
            "focused": focused,
            "dockSizes": { "left": 240, "right": 240, "bottom": 220 },
        }))
        .unwrap()
    }

    fn resolve(document: Option<&str>, uri: Option<&str>) -> Option<u32> {
        match (document, uri) {
            (Some("a"), _) => Some(1),
            (Some("b"), _) => Some(2),
            (_, Some("mem://c")) => Some(3),
            _ => None,
        }
    }

    #[test]
    fn layouts_round_trip_through_json() {
        let original = layout(
            json!({ "kind": "split", "direction": "horizontal",
                    "first": { "kind": "pane", "dock": "left", "selected": 0,
                               "tabs": [{ "kind": "tool", "tool": "knot.explorer" }] },
                    "second": { "kind": "pane", "dock": null, "selected": 1,
                                "tabs": [document("a"), { "kind": "terminal" }] } }),
            Some(1),
        );
        let encoded = serde_json::to_value(&original).unwrap();
        assert_eq!(WindowLayout::decode(&encoded).unwrap(), original);
        // Extension transport delivers every number as a double.
        let doubles: serde_json::Value =
            serde_json::from_str(&encoded.to_string().replace(":1,", ":1.0,")).unwrap();
        assert_eq!(WindowLayout::decode(&doubles).unwrap(), original);
        let payload = encoded.to_string();
        for forbidden in ["pane\":", "tab\":", "window\":", "presentation"] {
            assert!(!payload.contains(forbidden), "layout leaks {forbidden}");
        }
    }

    #[test]
    fn unsupported_versions_and_invalid_shapes_are_ignored() {
        let valid = serde_json::to_value(layout(json!(null), None)).unwrap();
        for invalid in [
            json!({ "version": 2 }),
            json!("layout"),
            json!({ "version": 1, "root": { "kind": "window" }, "focused": null,
                    "geometry": null, "dockSizes": valid["dockSizes"] }),
            json!({ "version": 1, "root": null, "focused": null, "geometry": null }),
            json!({ "version": 1, "root": null, "focused": null, "dockSizes": valid["dockSizes"],
                    "geometry": { "x": 0, "y": 0, "width": 0, "height": 10,
                                  "state": "windowed", "display": null } }),
        ] {
            assert_eq!(WindowLayout::decode(&invalid), None, "{invalid}");
        }
        assert!(WindowLayout::decode(&valid).is_some());
    }

    #[test]
    fn pruning_drops_unknown_tools_unmatched_documents_and_duplicates() {
        let layout = layout(
            json!({ "kind": "split", "direction": "horizontal",
                    "first": { "kind": "pane", "dock": "left", "selected": 1, "tabs": [
                        { "kind": "tool", "tool": "knot.explorer" },
                        { "kind": "tool", "tool": "gone.tool" },
                        { "kind": "tool", "tool": "knot.explorer" },
                    ] },
                    "second": { "kind": "pane", "dock": "left", "selected": 3, "tabs": [
                        document("a"), document("missing"), document("a"),
                        { "kind": "document", "document": null, "uri": "mem://c",
                          "view": { "anchor": 0, "caret": 0, "top": 0, "scrollX": 0 } },
                        { "kind": "terminal" },
                    ] } }),
            Some(1),
        );
        let plan = layout
            .plan(resolve, |tool| tool == "knot.explorer")
            .unwrap();
        assert_eq!(
            plan,
            PlanNode::Split {
                direction: SplitDirection::Horizontal,
                first: Box::new(PlanNode::Pane {
                    dock: Some(DockLocation::Left),
                    // The selected tool was pruned, so the first tab is selected.
                    selected: 0,
                    focused: false,
                    tabs: vec![PlannedTab::Tool("knot.explorer".into())],
                }),
                second: Box::new(PlanNode::Pane {
                    // A second pane claiming the same dock becomes ordinary.
                    dock: None,
                    selected: 1,
                    focused: true,
                    tabs: vec![
                        PlannedTab::Document {
                            document: 1,
                            view: EditorViewState {
                                anchor: 1,
                                caret: 1,
                                top: 0,
                                scroll_x: 0.
                            },
                        },
                        PlannedTab::Document {
                            document: 3,
                            view: view(0)
                        },
                        PlannedTab::Terminal,
                    ],
                }),
            }
        );
    }

    #[test]
    fn pruning_collapses_empty_panes_and_nested_one_child_splits() {
        let layout = layout(
            json!({ "kind": "split", "direction": "vertical",
                    "first": { "kind": "split", "direction": "horizontal",
                               "first": { "kind": "pane", "dock": null, "selected": 0,
                                          "tabs": [document("missing")] },
                               "second": { "kind": "split", "direction": "vertical",
                                           "first": { "kind": "pane", "dock": null, "selected": 9,
                                                      "tabs": [document("b")] },
                                           "second": { "kind": "pane", "dock": "bottom", "selected": 0,
                                                       "tabs": [{ "kind": "tool", "tool": "gone" }] } } },
                    "second": { "kind": "pane", "dock": null, "selected": 0, "tabs": [] } }),
            Some(7),
        );
        assert_eq!(
            layout.plan(resolve, |_| false),
            Some(PlanNode::Pane {
                dock: None,
                selected: 0,
                // An out-of-range focus marks no pane; the installer falls back.
                focused: false,
                tabs: vec![PlannedTab::Document {
                    document: 2,
                    view: EditorViewState {
                        anchor: 1,
                        caret: 1,
                        top: 0,
                        scroll_x: 0.
                    },
                }],
            })
        );
        assert_eq!(
            super::super::window_layout::WindowLayout {
                root: Some(LayoutNode::Pane {
                    dock: None,
                    selected: 0,
                    tabs: vec![],
                }),
                ..layout
            }
            .plan(resolve, |_| true),
            None
        );
    }

    fn display(uuid: &str, x: f32, y: f32, width: f32, height: f32) -> DisplayArea {
        DisplayArea {
            uuid: Some(uuid.into()),
            bounds: Rect {
                x,
                y,
                width,
                height,
            },
        }
    }

    fn saved(x: f32, y: f32, width: f32, height: f32, display: Option<&str>) -> WindowGeometry {
        WindowGeometry {
            x,
            y,
            width,
            height,
            state: WindowState::Windowed,
            display: display.map(Into::into),
        }
    }

    #[test]
    fn saved_bounds_stay_while_their_title_bar_is_reachable() {
        let displays = [
            display("main", 0., 0., 1440., 900.),
            display("side", 1440., 0., 1920., 1080.),
        ];
        let partly = saved(1300., 40., 1000., 700., Some("side"));
        assert_eq!(
            fit_to_displays(&partly, &displays, Some(0)),
            (partly.rect(), Some(0))
        );
        // Only the bottom edge remains visible: the title bar is off screen.
        let hidden = saved(100., -680., 1000., 700., Some("main"));
        assert_eq!(
            fit_to_displays(&hidden, &displays, Some(1)),
            (
                Rect {
                    x: 220.,
                    y: 100.,
                    width: 1000.,
                    height: 700.
                },
                Some(0)
            )
        );
    }

    #[test]
    fn disconnected_displays_center_a_fitted_window_on_the_primary_display() {
        let displays = [display("laptop", 0., 0., 1280., 800.)];
        let geometry = saved(3000., 200., 1600., 1000., Some("external"));
        assert_eq!(
            fit_to_displays(&geometry, &displays, Some(0)),
            (
                Rect {
                    x: 0.,
                    y: 0.,
                    width: 1280.,
                    height: 800.
                },
                Some(0)
            )
        );
        let small = saved(3000., 200., 600., 400., None);
        assert_eq!(
            fit_to_displays(&small, &displays, None),
            (
                Rect {
                    x: 340.,
                    y: 200.,
                    width: 600.,
                    height: 400.
                },
                Some(0)
            )
        );
        assert_eq!(fit_to_displays(&small, &[], None), (small.rect(), None));
    }
}
