use std::collections::{HashMap, HashSet};

use gpui::{prelude::FluentBuilder, *};

use super::{
    filesystem::{ResourceEntry, ResourceKind},
    resource::ResourceUri,
    workspace::{WorkspaceRoot, WorkspaceSnapshot},
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

/// A row's identity: the root it is shown under and its URI. Overlapping roots
/// can show one URI under several roots, each with independent state.
type RowKey = (uuid::Uuid, ResourceUri);

#[derive(Clone)]
enum VisibleRow {
    Root {
        root: WorkspaceRoot,
        label: SharedString,
    },
    Entry {
        root: uuid::Uuid,
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

impl VisibleRow {
    fn key(&self) -> Option<RowKey> {
        match self {
            Self::Root { root, .. } => Some((root.id, root.uri.clone())),
            Self::Entry { root, entry, .. } => Some((*root, entry.uri.clone())),
            Self::Status { .. } => None,
        }
    }
}

/// Native cached presentation for the window's ordered, URI-rooted workspace.
/// Directory listings are cached by URI and shared between overlapping roots;
/// expansion and selection belong to rows.
pub(crate) struct WorkspaceTree {
    workspace: WorkspaceSnapshot,
    children: HashMap<ResourceUri, ChildrenState>,
    expanded: HashSet<RowKey>,
    selected: Option<RowKey>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    next_generation: u64,
}

impl WorkspaceTree {
    pub(crate) fn new(workspace: WorkspaceSnapshot, cx: &mut Context<Self>) -> Self {
        let expanded = workspace
            .roots()
            .iter()
            .map(|root| (root.id, root.uri.clone()))
            .collect();
        Self {
            workspace,
            children: HashMap::new(),
            expanded,
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
        self.selected.as_ref().map(|(_, uri)| uri)
    }

    /// The root shown at or above the selected row.
    pub(crate) fn selected_root(&self) -> Option<&WorkspaceRoot> {
        let (id, _) = self.selected.as_ref()?;
        self.workspace.roots().iter().find(|root| root.id == *id)
    }

    #[cfg(test)]
    pub(crate) fn is_loading(&self) -> bool {
        self.children.values().any(|state| state.loading)
    }

    /// Starts initial enumeration after the shell has subscribed to events.
    pub(crate) fn load(&mut self, cx: &mut Context<Self>) {
        for root in self.workspace.roots().to_vec() {
            self.request_children(root.uri, cx);
        }
        cx.notify();
    }

    /// Installs a new root set. Retained roots keep their expansion, selection,
    /// and cached listings; requests issued under the previous snapshot are
    /// stale, so directories still loading are requested again.
    pub(crate) fn set_workspace(&mut self, workspace: WorkspaceSnapshot, cx: &mut Context<Self>) {
        let previous = std::mem::replace(&mut self.workspace, workspace);
        let roots = self.workspace.roots().to_vec();
        let retained = |id: &uuid::Uuid| roots.iter().any(|root| root.id == *id);
        self.expanded.retain(|(id, _)| retained(id));
        if self.selected.as_ref().is_some_and(|(id, _)| !retained(id)) {
            self.selected = None;
        }
        for root in &roots {
            if !previous.roots().iter().any(|old| old.id == root.id) {
                self.expanded.insert((root.id, root.uri.clone()));
            }
        }
        self.children
            .retain(|uri, _| roots.iter().any(|root| uri.is_within(&root.uri)));
        let mut pending = self
            .children
            .iter()
            .filter(|(_, state)| state.loading)
            .map(|(uri, _)| uri.clone())
            .collect::<Vec<_>>();
        for root in &roots {
            if !self.children.contains_key(&root.uri) && !pending.contains(&root.uri) {
                pending.push(root.uri.clone());
            }
        }
        for uri in pending {
            self.request_children(uri, cx);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn reload(&mut self, uri: ResourceUri, cx: &mut Context<Self>) -> bool {
        if !self
            .workspace
            .roots()
            .iter()
            .any(|root| uri.is_within(&root.uri))
        {
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
        let roots = self.workspace.roots();
        for (root, label) in roots.iter().zip(root_labels(roots)) {
            rows.push(VisibleRow::Root {
                root: root.clone(),
                label: label.into(),
            });
            if self.expanded.contains(&(root.id, root.uri.clone())) {
                let mut visited = HashSet::new();
                self.append_children(root.id, &root.uri, 1, &mut visited, &mut rows);
            }
        }
        rows
    }

    fn append_children(
        &self,
        root: uuid::Uuid,
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
                root,
                entry: entry.clone(),
                parent: parent.clone(),
                depth,
            });
            if entry.kind == ResourceKind::Directory
                && self.expanded.contains(&(root, entry.uri.clone()))
            {
                self.append_children(root, &entry.uri, depth + 1, visited, rows);
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

    fn activate(&mut self, key: &RowKey, cx: &mut Context<Self>) {
        let (root, uri) = key;
        let kind = if self
            .workspace
            .roots()
            .iter()
            .any(|entry| entry.id == *root && entry.uri == *uri)
        {
            ResourceKind::Directory
        } else {
            let Some(entry) = self
                .children
                .values()
                .flat_map(|state| &state.entries)
                .find(|entry| &entry.uri == uri)
            else {
                return;
            };
            entry.kind
        };
        self.selected = Some(key.clone());
        match kind {
            ResourceKind::File => cx.emit(WorkspaceTreeEvent::OpenFile {
                workspace: self.workspace.clone(),
                uri: uri.clone(),
            }),
            ResourceKind::Directory if self.expanded.remove(key) => {}
            ResourceKind::Directory => {
                self.expanded.insert(key.clone());
                if !self.children.contains_key(uri) {
                    self.request_children(uri.clone(), cx);
                }
            }
        }
        cx.notify();
    }

    /// Activates the first visible row showing `uri`.
    #[cfg(test)]
    pub(crate) fn activate_for_test(&mut self, uri: &ResourceUri, cx: &mut Context<Self>) -> bool {
        let key = self
            .visible_rows()
            .iter()
            .filter_map(VisibleRow::key)
            .find(|(_, row)| row == uri);
        if let Some(key) = &key {
            self.activate(key, cx);
        }
        key.is_some()
    }

    #[cfg(test)]
    pub(crate) fn select_for_test(&mut self, index: usize) -> bool {
        let key = self.visible_rows().get(index).and_then(VisibleRow::key);
        self.selected = key;
        self.selected.is_some()
    }

    #[cfg(test)]
    pub(crate) fn row_labels(&self) -> Vec<String> {
        self.visible_rows()
            .iter()
            .map(|row| match row {
                VisibleRow::Root { label, .. } => label.to_string(),
                VisibleRow::Entry { entry, depth, .. } => {
                    format!("{}{}", "  ".repeat(*depth), entry.name)
                }
                VisibleRow::Status { depth, label, .. } => {
                    format!("{}{label}", "  ".repeat(*depth))
                }
            })
            .collect()
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let rows = self.visible_rows();
        let entries = rows
            .iter()
            .filter_map(|row| match row {
                VisibleRow::Root { root, .. } => Some(((root.id, root.uri.clone()), None)),
                VisibleRow::Entry {
                    root,
                    entry,
                    parent,
                    ..
                } => Some(((*root, entry.uri.clone()), Some((*root, parent.clone())))),
                VisibleRow::Status { .. } => None,
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|selected| entries.iter().position(|(key, _)| key == selected));
        let target = match event.keystroke.key.to_lowercase().as_str() {
            "up" => Some(current.unwrap_or(1).saturating_sub(1)),
            "down" => Some((current.map_or(0, |index| index + 1)).min(entries.len() - 1)),
            "left" => {
                let Some(index) = current else {
                    return;
                };
                let (key, parent) = &entries[index];
                if self.expanded.remove(key) {
                    cx.notify();
                } else if let Some(parent) = parent {
                    self.selected = Some(parent.clone());
                    cx.notify();
                }
                cx.stop_propagation();
                return;
            }
            "right" | "enter" => {
                let index = current.unwrap_or(0);
                let key = entries[index].0.clone();
                self.activate(&key, cx);
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

/// Decoded non-empty path segments of a URI.
fn decoded_segments(uri: &ResourceUri) -> Vec<String> {
    uri.as_url()
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            percent_encoding::percent_decode_str(segment)
                .decode_utf8_lossy()
                .into_owned()
        })
        .collect()
}

/// Root row labels: the last path segment (or the host for a bare authority),
/// plus the parent segment when several roots share a last segment.
fn root_labels(roots: &[WorkspaceRoot]) -> Vec<String> {
    let segments = roots
        .iter()
        .map(|root| decoded_segments(&root.uri))
        .collect::<Vec<_>>();
    let names = segments
        .iter()
        .zip(roots)
        .map(|(segments, root)| {
            segments
                .last()
                .cloned()
                .or_else(|| root.uri.as_url().host_str().map(str::to_owned))
                .unwrap_or_else(|| root.uri.to_string())
        })
        .collect::<Vec<_>>();
    names
        .iter()
        .zip(&segments)
        .map(|(name, segments)| {
            let shared = names.iter().filter(|other| *other == name).count() > 1;
            match segments.len().checked_sub(2).map(|index| &segments[index]) {
                Some(parent) if shared => format!("{name} — {parent}"),
                _ => name.clone(),
            }
        })
        .collect()
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
                .map(|index| {
                    let row = &rows[index];
                    let (depth, kind, label, is_root) = match row {
                        VisibleRow::Root { label, .. } => {
                            (0, ResourceKind::Directory, label.clone(), true)
                        }
                        VisibleRow::Entry { entry, depth, .. } => {
                            (*depth, entry.kind, entry.name.clone().into(), false)
                        }
                        VisibleRow::Status {
                            depth,
                            label,
                            error,
                        } => {
                            return div()
                                .id(("workspace-tree-status", index))
                                .w_full()
                                .pl(px(22. + *depth as f32 * 14.))
                                .py_1()
                                .text_sm()
                                .text_color(if *error { rgb(0xf48771) } else { rgb(0x888888) })
                                .child(label.clone())
                                .into_any_element();
                        }
                    };
                    let key = row.key().unwrap();
                    let is_selected = selected.as_ref() == Some(&key);
                    let is_expanded = expanded.contains(&key);
                    div()
                        .id(("workspace-tree-row", index))
                        .debug_selector(move || format!("workspace-tree-row-{index}"))
                        .w_full()
                        .pl(px(8. + depth as f32 * 14.))
                        .pr_2()
                        .py_1()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .text_sm()
                        .when(is_root, |row| row.font_weight(FontWeight::SEMIBOLD))
                        .when(is_selected, |row| {
                            row.bg(rgb(0x2a4a7a)).text_color(rgb(0xffffff))
                        })
                        .hover(|style| style.bg(rgb(0x222222)))
                        .child(Self::disclosure(&kind, is_expanded))
                        .when(!is_root, |row| row.child(Self::icon(&kind)))
                        .child(label)
                        .on_click({
                            let entity = entity.clone();
                            move |_, window, cx| {
                                window.focus(&entity.read(cx).focus);
                                entity.update(cx, |tree, cx| tree.activate(&key, cx));
                            }
                        })
                        .into_any_element()
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

    use super::{WorkspaceTree, WorkspaceTreeEvent, WorkspaceTreeResponse, sort_entries};
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

    fn response(
        workspace: &crate::app::workspace::WorkspaceSnapshot,
        parent: &str,
        generation: u64,
        entries: Vec<ResourceEntry>,
    ) -> WorkspaceTreeResponse {
        WorkspaceTreeResponse {
            workspace: workspace.clone(),
            parent: uri(parent),
            generation,
            result: Ok(entries),
        }
    }

    #[test]
    fn root_labels_add_the_parent_segment_only_when_names_collide() {
        let mut state = WorkspaceState::default();
        let workspace = state.replace_roots(vec![
            uri("mem://host/knot/src/"),
            uri("mem://host/other/src/"),
            uri("mem://host/My%20Notes/"),
            uri("file:///"),
        ]);
        assert_eq!(
            super::root_labels(workspace.roots()),
            ["src — knot", "src — other", "My Notes", "file:///"]
        );
    }

    #[gpui::test]
    fn overlapping_roots_render_independently_in_root_order(cx: &mut TestAppContext) {
        let mut state = WorkspaceState::default();
        let workspace = state.replace_roots(vec![uri("mem://host/a/"), uri("mem://host/a/b/")]);
        let tree = cx.new(|cx| WorkspaceTree::new(workspace.clone(), cx));
        tree.update(cx, |tree, cx| {
            tree.load(cx);
            assert_eq!(tree.row_labels(), ["a", "  Loading…", "b", "  Loading…"]);
            assert!(tree.apply_response(
                response(
                    &workspace,
                    "mem://host/a/",
                    1,
                    vec![entry("mem://host/a/b/", "b", ResourceKind::Directory)],
                ),
                cx,
            ));
            assert!(tree.apply_response(
                response(
                    &workspace,
                    "mem://host/a/b/",
                    2,
                    vec![entry("mem://host/a/b/x.txt", "x.txt", ResourceKind::File)],
                ),
                cx,
            ));
            assert_eq!(tree.row_labels(), ["a", "  b", "b", "  x.txt"]);

            // Expanding b under a reuses the cached listing and leaves the
            // second root's row state alone.
            assert!(tree.activate_for_test(&uri("mem://host/a/b/"), cx));
            assert_eq!(tree.row_labels(), ["a", "  b", "    x.txt", "b", "  x.txt"]);
            assert!(!tree.is_loading());
            assert_eq!(tree.selected_root(), Some(&workspace.roots()[0]));
            assert!(tree.select_for_test(4));
            assert_eq!(tree.selected(), Some(&uri("mem://host/a/b/x.txt")));
            assert_eq!(tree.selected_root(), Some(&workspace.roots()[1]));
        });
    }

    #[gpui::test]
    fn root_changes_keep_retained_state_and_rerequest_loading_directories(cx: &mut TestAppContext) {
        let mut state = WorkspaceState::new(uri("mem://host/one/"));
        let first = state.snapshot();
        let tree = cx.new(|cx| WorkspaceTree::new(first.clone(), cx));
        let requests = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        cx.update(|cx| {
            let requests = requests.clone();
            cx.subscribe(&tree, move |_, event: &WorkspaceTreeEvent, _| {
                if let WorkspaceTreeEvent::RequestChildren(request) = event {
                    requests
                        .borrow_mut()
                        .push((request.parent.to_string(), request.generation));
                }
            })
            .detach();
        });
        tree.update(cx, |tree, cx| {
            tree.load(cx);
            assert!(tree.apply_response(
                response(
                    &first,
                    "mem://host/one/",
                    1,
                    vec![entry("mem://host/one/dir/", "dir", ResourceKind::Directory)],
                ),
                cx,
            ));
            // dir is still loading when a second root arrives.
            assert!(tree.activate_for_test(&uri("mem://host/one/dir/"), cx));
        });
        let added = state.replace_roots(vec![uri("mem://host/one/"), uri("mem://host/two/")]);
        tree.update(cx, |tree, cx| {
            tree.set_workspace(added.clone(), cx);
            // The response issued under the old snapshot is stale.
            assert!(!tree.apply_response(response(&first, "mem://host/one/dir/", 2, vec![]), cx));
            assert_eq!(tree.selected(), Some(&uri("mem://host/one/dir/")));
            assert_eq!(
                tree.row_labels(),
                ["one", "  dir", "    Loading…", "two", "  Loading…"]
            );
        });
        let mut issued = requests.borrow().clone();
        issued.sort();
        assert_eq!(
            issued,
            [
                ("mem://host/one/".to_owned(), 1),
                ("mem://host/one/dir/".to_owned(), 2),
                ("mem://host/one/dir/".to_owned(), 3),
                ("mem://host/two/".to_owned(), 4),
            ]
        );
        let removed = state.replace_roots(vec![uri("mem://host/two/")]);
        tree.update(cx, |tree, cx| {
            tree.set_workspace(removed, cx);
            assert_eq!(tree.selected(), None);
            assert_eq!(tree.row_labels(), ["two", "  Loading…"]);
            assert!(!tree.children.contains_key(&uri("mem://host/one/dir/")));
        });
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
        let replacement = state.replace_roots(vec![uri("file:///tmp/knot-workspace/")]);
        let tree = cx.new(|cx| WorkspaceTree::new(first.clone(), cx));

        tree.update(cx, |tree, cx| {
            tree.load(cx);
            assert!(tree.reload(first.first_root().unwrap().clone(), cx));
            assert!(!tree.apply_response(
                WorkspaceTreeResponse {
                    workspace: first.clone(),
                    parent: first.first_root().unwrap().clone(),
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
    fn clearing_and_reinstalling_roots_rejects_prior_directory_responses(cx: &mut TestAppContext) {
        let root = uri("mem://workspace/");
        let mut state = WorkspaceState::new(root.clone());
        let first = state.snapshot();
        let tree = cx.new(|cx| WorkspaceTree::new(first.clone(), cx));
        let cleared = state.replace_roots(Vec::new());
        let reinstalled = state.replace_roots(vec![root.clone()]);
        tree.update(cx, |tree, cx| {
            tree.load(cx);
            assert!(tree.apply_response(
                WorkspaceTreeResponse {
                    workspace: first.clone(),
                    parent: root.clone(),
                    generation: 1,
                    result: Ok(vec![entry(
                        "mem://workspace/old.txt",
                        "old.txt",
                        ResourceKind::File
                    )]),
                },
                cx
            ));
            assert_eq!(tree.row_labels(), ["workspace", "  old.txt"]);
            tree.set_workspace(cleared, cx);
            assert!(tree.visible_rows().is_empty());
            assert!(!tree.is_loading());
            assert!(!tree.reload(root.clone(), cx));
            tree.set_workspace(reinstalled.clone(), cx);
            assert!(!tree.apply_response(
                WorkspaceTreeResponse {
                    workspace: first,
                    parent: root.clone(),
                    generation: 1,
                    result: Ok(Vec::new()),
                },
                cx
            ));
            assert!(tree.is_loading());
            assert!(tree.apply_response(
                WorkspaceTreeResponse {
                    workspace: reinstalled,
                    parent: root,
                    generation: 2,
                    result: Ok(vec![entry(
                        "mem://workspace/new.txt",
                        "new.txt",
                        ResourceKind::File
                    )]),
                },
                cx
            ));
            assert_eq!(tree.row_labels(), ["workspace", "  new.txt"]);
            assert!(!tree.is_loading());
        });
    }

    #[gpui::test]
    fn errors_replace_loading_without_discarding_cached_children(cx: &mut TestAppContext) {
        let workspace = WorkspaceState::new(uri("mem://workspace/")).snapshot();
        let tree = cx.new(|cx| WorkspaceTree::new(workspace.clone(), cx));

        tree.update(cx, |tree, cx| {
            tree.load(cx);
            let root = workspace.first_root().unwrap().clone();
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

#[crate::app::commands::declarations::command("copy", title = "Copy", id = WORKSPACE_COPY_COMMAND, participant = WorkspaceTree)]
pub(crate) async fn copy_selected_uri(
    context: crate::app::commands::declarations::CommandContext,
    (): (),
) -> Result<(), crate::host::protocol::CommandOutcome> {
    use crate::host::protocol::CommandOutcome;
    context
        .foreground(|target, _, cx| {
            let Some(crate::app::workbench::CommandView::Workspace(view)) = target.command_view(cx)
            else {
                return Err(CommandOutcome::InvalidTarget);
            };
            let uri = view
                .read(cx)
                .selected()
                .ok_or(CommandOutcome::Unavailable)?
                .to_string();
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(uri));
            Ok(())
        })
        .await
}
