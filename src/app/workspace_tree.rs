use std::collections::{HashMap, HashSet};

use gpui::{prelude::FluentBuilder, *};

use super::{
    filesystem::{ResourceEntry, ResourceKind},
    resource::ResourceUri,
    workspace::WorkspaceSnapshot,
};

const WORKSPACE_TREE_KEY_CONTEXT: &str = "workspace_tree";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceTreeRequest {
    pub workspace: WorkspaceSnapshot,
    pub parent: ResourceUri,
    pub generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspaceTreeResponse {
    pub workspace: WorkspaceSnapshot,
    pub parent: ResourceUri,
    pub generation: u64,
    pub result: Result<Vec<ResourceEntry>, SharedString>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceTreeEvent {
    RequestChildren(WorkspaceTreeRequest),
    OpenFile {
        workspace: WorkspaceSnapshot,
        uri: ResourceUri,
    },
}

#[derive(Clone, Debug, Default)]
struct ChildrenState {
    entries: Vec<ResourceEntry>,
    generation: u64,
    loading: bool,
    error: Option<SharedString>,
}

#[derive(Clone)]
enum VisibleRow {
    Entry {
        entry: ResourceEntry,
        parent: ResourceUri,
        depth: usize,
    },
    Status {
        depth: usize,
        label: SharedString,
        error: bool,
    },
}

/// Native cached presentation for the current URI-rooted workspace.
pub(crate) struct WorkspaceTree {
    workspace: WorkspaceSnapshot,
    children: HashMap<ResourceUri, ChildrenState>,
    expanded: HashSet<ResourceUri>,
    selected: Option<ResourceUri>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    next_generation: u64,
}

impl WorkspaceTree {
    pub(crate) fn new(workspace: WorkspaceSnapshot, cx: &mut Context<Self>) -> Self {
        Self {
            workspace,
            children: HashMap::new(),
            expanded: HashSet::new(),
            selected: None,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            next_generation: 1,
        }
    }

    #[cfg(test)]
    pub(crate) fn workspace(&self) -> &WorkspaceSnapshot {
        &self.workspace
    }

    pub(crate) fn selected(&self) -> Option<&ResourceUri> {
        self.selected.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn is_loading(&self) -> bool {
        self.children.values().any(|state| state.loading)
    }

    /// Starts initial enumeration after the shell has subscribed to events.
    pub(crate) fn load(&mut self, cx: &mut Context<Self>) {
        self.request_root(cx);
    }

    pub(crate) fn set_workspace(&mut self, workspace: WorkspaceSnapshot, cx: &mut Context<Self>) {
        self.workspace = workspace;
        self.children.clear();
        self.expanded.clear();
        self.selected = None;
        self.request_root(cx);
    }

    #[cfg(test)]
    pub(crate) fn reload(&mut self, uri: ResourceUri, cx: &mut Context<Self>) -> bool {
        if uri != *self.workspace.root() && !uri.is_descendant_of(self.workspace.root()) {
            return false;
        }
        self.request_children(uri, cx);
        true
    }

    pub(crate) fn apply_response(
        &mut self,
        response: WorkspaceTreeResponse,
        cx: &mut Context<Self>,
    ) -> bool {
        if response.workspace != self.workspace {
            return false;
        }
        let Some(state) = self.children.get_mut(&response.parent) else {
            return false;
        };
        if state.generation != response.generation {
            return false;
        }

        state.loading = false;
        match response.result {
            Ok(mut entries) => {
                sort_entries(&mut entries);
                state.entries = entries;
                state.error = None;
            }
            Err(error) => state.error = Some(error),
        }
        cx.notify();
        true
    }

    fn request_root(&mut self, cx: &mut Context<Self>) {
        self.request_children(self.workspace.root().clone(), cx);
    }

    fn request_children(&mut self, parent: ResourceUri, cx: &mut Context<Self>) {
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("workspace tree request generation overflowed");
        let state = self.children.entry(parent.clone()).or_default();
        state.generation = generation;
        state.loading = true;
        state.error = None;
        cx.emit(WorkspaceTreeEvent::RequestChildren(WorkspaceTreeRequest {
            workspace: self.workspace.clone(),
            parent,
            generation,
        }));
        cx.notify();
    }

    fn visible_rows(&self) -> Vec<VisibleRow> {
        let mut rows = Vec::new();
        let mut visited = HashSet::new();
        self.append_children(self.workspace.root(), 0, &mut visited, &mut rows);
        rows
    }

    fn append_children(
        &self,
        parent: &ResourceUri,
        depth: usize,
        visited: &mut HashSet<ResourceUri>,
        rows: &mut Vec<VisibleRow>,
    ) {
        let Some(state) = self.children.get(parent) else {
            return;
        };
        for entry in &state.entries {
            if !visited.insert(entry.uri.clone()) {
                continue;
            }
            rows.push(VisibleRow::Entry {
                entry: entry.clone(),
                parent: parent.clone(),
                depth,
            });
            if entry.kind == ResourceKind::Directory && self.expanded.contains(&entry.uri) {
                self.append_children(&entry.uri, depth + 1, visited, rows);
            }
        }
        if state.loading {
            rows.push(VisibleRow::Status {
                depth,
                label: "Loading…".into(),
                error: false,
            });
        } else if let Some(error) = &state.error {
            rows.push(VisibleRow::Status {
                depth,
                label: format!("Error: {error}").into(),
                error: true,
            });
        }
    }

    fn activate(&mut self, uri: &ResourceUri, cx: &mut Context<Self>) {
        let entry = self
            .children
            .values()
            .flat_map(|state| &state.entries)
            .find(|entry| &entry.uri == uri)
            .cloned();
        let Some(entry) = entry else {
            return;
        };
        self.selected = Some(entry.uri.clone());
        match entry.kind {
            ResourceKind::File => cx.emit(WorkspaceTreeEvent::OpenFile {
                workspace: self.workspace.clone(),
                uri: entry.uri,
            }),
            ResourceKind::Directory if self.expanded.remove(&entry.uri) => {}
            ResourceKind::Directory => {
                self.expanded.insert(entry.uri.clone());
                if !self.children.contains_key(&entry.uri) {
                    self.request_children(entry.uri, cx);
                }
            }
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn activate_for_test(&mut self, uri: &ResourceUri, cx: &mut Context<Self>) -> bool {
        let exists = self
            .children
            .values()
            .any(|state| state.entries.iter().any(|entry| &entry.uri == uri));
        if exists {
            self.activate(uri, cx);
        }
        exists
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let rows = self.visible_rows();
        let entries = rows
            .iter()
            .filter_map(|row| match row {
                VisibleRow::Entry { entry, parent, .. } => Some((&entry.uri, parent)),
                VisibleRow::Status { .. } => None,
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|selected| entries.iter().position(|(uri, _)| *uri == selected));
        let target = match event.keystroke.key.to_lowercase().as_str() {
            "up" => Some(current.unwrap_or(1).saturating_sub(1)),
            "down" => Some((current.map_or(0, |index| index + 1)).min(entries.len() - 1)),
            "left" => {
                let Some(index) = current else {
                    return;
                };
                let (uri, parent) = entries[index];
                if self.expanded.remove(uri) {
                    cx.notify();
                } else if parent != self.workspace.root() {
                    self.selected = Some(parent.clone());
                    cx.notify();
                }
                cx.stop_propagation();
                return;
            }
            "right" | "enter" => {
                let index = current.unwrap_or(0);
                let uri = entries[index].0.clone();
                self.activate(&uri, cx);
                cx.stop_propagation();
                return;
            }
            _ => return,
        };
        if let Some(index) = target {
            self.selected = Some(entries[index].0.clone());
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn disclosure(kind: &ResourceKind, expanded: bool) -> &'static str {
        match kind {
            ResourceKind::File => " ",
            ResourceKind::Directory if expanded => "▾",
            ResourceKind::Directory => "▸",
        }
    }

    fn icon(kind: &ResourceKind) -> &'static str {
        match kind {
            ResourceKind::File => "▧",
            ResourceKind::Directory => "□",
        }
    }
}

fn resource_kind_order(kind: &ResourceKind) -> u8 {
    match kind {
        ResourceKind::Directory => 0,
        ResourceKind::File => 1,
    }
}

fn sort_entries(entries: &mut [ResourceEntry]) {
    entries.sort_by(|left, right| {
        resource_kind_order(&left.kind)
            .cmp(&resource_kind_order(&right.kind))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.uri.as_url().as_str().cmp(right.uri.as_url().as_str()))
    });
}

impl EventEmitter<WorkspaceTreeEvent> for WorkspaceTree {}

impl Focusable for WorkspaceTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for WorkspaceTree {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.visible_rows();
        let selected = self.selected.clone();
        let expanded = self.expanded.clone();
        let entity = cx.entity();
        uniform_list("workspace-tree", rows.len(), move |range, _, _| {
            range
                .map(|index| match &rows[index] {
                    VisibleRow::Entry { entry, depth, .. } => {
                        let uri = entry.uri.clone();
                        let is_selected = selected.as_ref() == Some(&entry.uri);
                        div()
                            .id(("workspace-tree-row", index))
                            .w_full()
                            .pl(px(8. + *depth as f32 * 14.))
                            .pr_2()
                            .py_1()
                            .flex()
                            .flex_row()
                            .gap_1()
                            .text_sm()
                            .when(is_selected, |row| {
                                row.bg(rgb(0x2a4a7a)).text_color(rgb(0xffffff))
                            })
                            .hover(|style| style.bg(rgb(0x222222)))
                            .child(Self::disclosure(&entry.kind, expanded.contains(&entry.uri)))
                            .child(Self::icon(&entry.kind))
                            .child(entry.name.clone())
                            .on_click({
                                let entity = entity.clone();
                                move |_, window, cx| {
                                    window.focus(&entity.read(cx).focus);
                                    entity.update(cx, |tree, cx| tree.activate(&uri, cx));
                                }
                            })
                            .into_any_element()
                    }
                    VisibleRow::Status {
                        depth,
                        label,
                        error,
                    } => div()
                        .id(("workspace-tree-status", index))
                        .w_full()
                        .pl(px(22. + *depth as f32 * 14.))
                        .py_1()
                        .text_sm()
                        .text_color(if *error { rgb(0xf48771) } else { rgb(0x888888) })
                        .child(label.clone())
                        .into_any_element(),
                })
                .collect()
        })
        .h_full()
        .track_scroll(self.scroll.clone())
        .track_focus(&self.focus)
        .key_context(WORKSPACE_TREE_KEY_CONTEXT)
        .on_key_down(cx.listener(|this, event, _window, cx| {
            this.on_key_down(event, cx);
        }))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext};

    use super::{WorkspaceTree, WorkspaceTreeResponse, sort_entries};
    use crate::app::{
        filesystem::{ResourceEntry, ResourceKind},
        resource::ResourceUri,
        workspace::WorkspaceState,
    };

    fn uri(value: &str) -> ResourceUri {
        ResourceUri::parse(value).unwrap()
    }

    fn entry(value: &str, name: &str, kind: ResourceKind) -> ResourceEntry {
        ResourceEntry {
            uri: uri(value),
            name: name.into(),
            kind,
        }
    }

    #[test]
    fn responses_are_sorted_by_kind_name_and_uri() {
        let mut entries = vec![
            entry("mem://workspace/z.txt", "z.txt", ResourceKind::File),
            entry("mem://workspace/b/", "b", ResourceKind::Directory),
            entry("mem://workspace/a.txt", "a.txt", ResourceKind::File),
            entry("mem://workspace/a/", "a", ResourceKind::Directory),
        ];

        sort_entries(&mut entries);

        let names = entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["a", "b", "a.txt", "z.txt"]);
    }

    #[gpui::test]
    fn stale_responses_are_rejected_after_reload_and_workspace_change(cx: &mut TestAppContext) {
        let mut state = WorkspaceState::new(uri("mem://workspace/"));
        let first = state.snapshot();
        let replacement = state.replace_root(uri("file:///tmp/knot-workspace/"));
        let tree = cx.new(|cx| WorkspaceTree::new(first.clone(), cx));

        tree.update(cx, |tree, cx| {
            tree.load(cx);
            assert!(tree.reload(first.root().clone(), cx));
            assert!(!tree.apply_response(
                WorkspaceTreeResponse {
                    workspace: first.clone(),
                    parent: first.root().clone(),
                    generation: 1,
                    result: Ok(Vec::new()),
                },
                cx,
            ));

            tree.set_workspace(replacement.clone(), cx);
            assert!(!tree.apply_response(
                WorkspaceTreeResponse {
                    workspace: first,
                    parent: uri("mem://workspace/"),
                    generation: 2,
                    result: Ok(Vec::new()),
                },
                cx,
            ));
            assert_eq!(tree.workspace(), &replacement);
            assert!(tree.is_loading());
        });
    }

    #[gpui::test]
    fn errors_replace_loading_without_discarding_cached_children(cx: &mut TestAppContext) {
        let workspace = WorkspaceState::new(uri("mem://workspace/")).snapshot();
        let tree = cx.new(|cx| WorkspaceTree::new(workspace.clone(), cx));

        tree.update(cx, |tree, cx| {
            tree.load(cx);
            let root = workspace.root().clone();
            assert!(tree.apply_response(
                WorkspaceTreeResponse {
                    workspace: workspace.clone(),
                    parent: root.clone(),
                    generation: 1,
                    result: Ok(vec![entry(
                        "mem://workspace/file.txt",
                        "file.txt",
                        ResourceKind::File,
                    )]),
                },
                cx,
            ));
            assert!(tree.reload(root.clone(), cx));
            assert!(tree.apply_response(
                WorkspaceTreeResponse {
                    workspace,
                    parent: root.clone(),
                    generation: 2,
                    result: Err("expected failure".into()),
                },
                cx,
            ));
            let state = &tree.children[&root];
            assert!(!state.loading);
            assert_eq!(state.entries.len(), 1);
            assert_eq!(state.error.as_ref().unwrap().as_ref(), "expected failure");
        });
    }
}
