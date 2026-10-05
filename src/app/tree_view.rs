//! Native presentation and interaction state for a semantic extension tree.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{prelude::FluentBuilder, *};

use crate::host::protocol::{
    Command, ExtensionId, ExtensionLifecycleId, TreeChildrenRequest, TreeChildrenResponse,
    TreeCollapsibleState, TreeIcon, TreeItem, TreeProviderRegistrationId, ViewId,
};

use super::TREE_KEY_CONTEXT;

static NEXT_VIEW_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_TREE_GENERATION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TreeProviderIdentity {
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
    pub registration: TreeProviderRegistrationId,
}

#[derive(Clone)]
pub(crate) enum TreeViewEvent {
    RequestChildren {
        provider: TreeProviderIdentity,
        request: TreeChildrenRequest,
    },
    InvokeCommand {
        provider: TreeProviderIdentity,
        command: String,
        window: AnyWindowHandle,
        focus: WeakFocusHandle,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TreeViewRegistrationError {
    WrongView,
    ProviderInUse,
    ProviderNotFound,
}

#[derive(Clone, Debug, Default)]
struct ChildrenState {
    items: Vec<TreeItem>,
    generation: u64,
    loading: bool,
    error: Option<String>,
}

#[derive(Clone)]
enum VisibleRow {
    Item {
        item: TreeItem,
        parent_id: Option<String>,
        depth: usize,
    },
    Status {
        depth: usize,
        label: SharedString,
        error: bool,
    },
}

/// Foreground-owned cache and presentation state for one native tree surface.
pub(crate) struct TreeView {
    view_id: String,
    instance_id: ViewId,
    provider: Option<TreeProviderIdentity>,
    children: HashMap<Option<String>, ChildrenState>,
    expanded: HashSet<String>,
    selected: Option<String>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
}

impl TreeView {
    pub(crate) fn new(view_id: impl Into<String>, cx: &mut Context<Self>) -> Self {
        Self {
            view_id: view_id.into(),
            instance_id: ViewId::new(NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed)),
            provider: None,
            children: HashMap::new(),
            expanded: HashSet::new(),
            selected: None,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub(crate) fn instance_id(&self) -> ViewId {
        self.instance_id
    }

    pub(crate) fn has_provider(&self) -> bool {
        self.provider.is_some()
    }

    pub(crate) fn selected_text(&self) -> Option<String> {
        let selected = self.selected.as_deref()?;
        self.children
            .values()
            .flat_map(|children| &children.items)
            .find(|item| item.id == selected)
            .map(|item| item.label.clone())
    }

    #[cfg(test)]
    pub(crate) fn select_item(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        if !self
            .children
            .values()
            .flat_map(|children| &children.items)
            .any(|item| item.id == id)
        {
            return false;
        }
        self.selected = Some(id.to_owned());
        cx.notify();
        true
    }

    pub(crate) fn handle_command(
        &self,
        command: &Command,
        target: &super::product_commands::ProductCommandTarget,
        completion: super::CommandCompletion,
        cx: &mut Context<Self>,
    ) -> super::product_commands::CommandClaim {
        use super::product_commands::{ApplicationProductCommands, COPY_COMMAND, CommandClaim};

        if command.name.as_ref() == COPY_COMMAND && self.selected_text().is_none() {
            return CommandClaim::Declined;
        }
        let catalog = cx
            .global::<ApplicationProductCommands>()
            .0
            .read(cx)
            .catalog();
        let Some(handler) = catalog
            .borrow()
            .resolve_view(self.instance_id, command.name.as_ref())
        else {
            return CommandClaim::Declined;
        };
        let command = command.clone();
        let target = target.clone();
        CommandClaim::Pending(Box::new(move |cx| {
            super::extension_host::start_view_command(command, target, handler, completion, cx)
        }))
    }

    #[cfg(test)]
    pub(crate) fn owns_provider(
        &self,
        registration: TreeProviderRegistrationId,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> bool {
        self.provider.is_some_and(|provider| {
            provider.registration == registration
                && provider.extension == extension
                && provider.lifecycle == lifecycle
        })
    }

    pub(crate) fn register_provider(
        &mut self,
        view_id: &str,
        provider: TreeProviderIdentity,
        cx: &mut Context<Self>,
    ) -> Result<(), TreeViewRegistrationError> {
        if view_id != self.view_id {
            return Err(TreeViewRegistrationError::WrongView);
        }
        if self.provider.is_some() {
            return Err(TreeViewRegistrationError::ProviderInUse);
        }
        self.provider = Some(provider);
        self.invalidate(provider.registration, None, cx)?;
        Ok(())
    }

    pub(crate) fn invalidate(
        &mut self,
        registration: TreeProviderRegistrationId,
        parent_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Result<(), TreeViewRegistrationError> {
        let provider = self
            .provider
            .filter(|provider| provider.registration == registration)
            .ok_or(TreeViewRegistrationError::ProviderNotFound)?;
        let generation = NEXT_TREE_GENERATION.fetch_add(1, Ordering::Relaxed);
        let state = self.children.entry(parent_id.clone()).or_default();
        state.generation = generation;
        state.loading = true;
        state.error = None;
        cx.emit(TreeViewEvent::RequestChildren {
            provider,
            request: TreeChildrenRequest {
                registration,
                parent_id,
                generation,
            },
        });
        cx.notify();
        Ok(())
    }

    pub(crate) fn unregister_provider(
        &mut self,
        registration: TreeProviderRegistrationId,
        cx: &mut Context<Self>,
    ) -> Result<(), TreeViewRegistrationError> {
        if self
            .provider
            .is_none_or(|provider| provider.registration != registration)
        {
            return Err(TreeViewRegistrationError::ProviderNotFound);
        }
        self.clear_provider(cx);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn is_loading(&self) -> bool {
        self.children.values().any(|state| state.loading)
    }

    #[cfg(test)]
    pub(crate) fn root_generation(&self) -> u64 {
        self.children[&None].generation
    }

    #[cfg(test)]
    pub(crate) fn lifecycle_state(&self) -> (bool, usize, bool, usize) {
        (
            self.provider.is_some(),
            self.children.len(),
            self.is_loading(),
            self.children
                .values()
                .filter(|state| state.error.is_some())
                .count(),
        )
    }

    #[cfg(test)]
    pub(crate) fn root_labels(&self) -> Vec<String> {
        self.children
            .get(&None)
            .map(|state| state.items.iter().map(|item| item.label.clone()).collect())
            .unwrap_or_default()
    }

    pub(crate) fn remove_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        cx: &mut Context<Self>,
    ) {
        if self.provider.is_some_and(|provider| {
            provider.extension == extension && provider.lifecycle == lifecycle
        }) {
            self.clear_provider(cx);
        }
    }

    fn clear_provider(&mut self, cx: &mut Context<Self>) {
        self.provider = None;
        self.children.clear();
        self.expanded.clear();
        self.selected = None;
        cx.notify();
    }

    pub(crate) fn apply_response(
        &mut self,
        response: TreeChildrenResponse,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(provider) = self.provider else {
            return false;
        };
        if provider.registration != response.registration {
            return false;
        }
        let Some(state) = self.children.get_mut(&response.parent_id) else {
            return false;
        };
        if state.generation != response.generation {
            return false;
        }
        let mut expanded_to_load = Vec::new();
        state.loading = false;
        match response.result {
            Ok(items) => {
                for item in &items {
                    if item.collapsible_state == TreeCollapsibleState::Expanded {
                        self.expanded.insert(item.id.clone());
                        expanded_to_load.push(item.id.clone());
                    }
                }
                state.items = items;
                state.error = None;
            }
            Err(error) => state.error = Some(error.message),
        }
        for id in expanded_to_load {
            if !self.children.contains_key(&Some(id.clone())) {
                let _ = self.invalidate(provider.registration, Some(id), cx);
            }
        }
        cx.notify();
        true
    }

    fn visible_rows(&self) -> Vec<VisibleRow> {
        let mut rows = Vec::new();
        let mut visited = HashSet::new();
        self.append_children(None, 0, &mut visited, &mut rows);
        rows
    }

    fn append_children(
        &self,
        parent_id: Option<&str>,
        depth: usize,
        visited: &mut HashSet<String>,
        rows: &mut Vec<VisibleRow>,
    ) {
        let key = parent_id.map(str::to_owned);
        let Some(state) = self.children.get(&key) else {
            return;
        };
        for item in &state.items {
            if !visited.insert(item.id.clone()) {
                continue;
            }
            rows.push(VisibleRow::Item {
                item: item.clone(),
                parent_id: key.clone(),
                depth,
            });
            if self.expanded.contains(&item.id) {
                self.append_children(Some(&item.id), depth + 1, visited, rows);
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

    fn activate(&mut self, id: &str, window: AnyWindowHandle, cx: &mut Context<Self>) {
        self.selected = Some(id.to_owned());
        let item = self
            .children
            .values()
            .flat_map(|state| &state.items)
            .find(|item| item.id == id)
            .cloned();
        let Some(item) = item else {
            return;
        };
        match item.collapsible_state {
            TreeCollapsibleState::None => {
                if let (Some(provider), Some(command)) = (self.provider, item.command) {
                    cx.emit(TreeViewEvent::InvokeCommand {
                        provider,
                        command,
                        window,
                        focus: self.focus.downgrade(),
                    });
                }
            }
            _ if self.expanded.remove(id) => {}
            _ => {
                self.expanded.insert(id.to_owned());
                if !self.children.contains_key(&Some(id.to_owned()))
                    && let Some(provider) = self.provider
                {
                    let _ = self.invalidate(provider.registration, Some(id.to_owned()), cx);
                }
            }
        }
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.visible_rows();
        let item_rows = rows
            .iter()
            .filter_map(|row| match row {
                VisibleRow::Item {
                    item, parent_id, ..
                } => Some((item.id.as_str(), parent_id.as_deref())),
                VisibleRow::Status { .. } => None,
            })
            .collect::<Vec<_>>();
        if item_rows.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_deref()
            .and_then(|selected| item_rows.iter().position(|(id, _)| *id == selected));
        let target = match event.keystroke.key.to_lowercase().as_str() {
            "up" => Some(current.unwrap_or(1).saturating_sub(1)),
            "down" => Some((current.map_or(0, |index| index + 1)).min(item_rows.len() - 1)),
            "left" => {
                let Some(index) = current else {
                    return;
                };
                let (id, parent) = item_rows[index];
                if self.expanded.remove(id) {
                    cx.notify();
                } else if let Some(parent) = parent {
                    self.selected = Some(parent.to_owned());
                }
                cx.stop_propagation();
                return;
            }
            "right" | "enter" => {
                let Some(index) = current.or(Some(0)) else {
                    return;
                };
                self.activate(item_rows[index].0, window.window_handle(), cx);
                cx.stop_propagation();
                return;
            }
            _ => return,
        };
        if let Some(index) = target {
            self.selected = Some(item_rows[index].0.to_owned());
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn disclosure(item: &TreeItem, expanded: bool) -> &'static str {
        if item.collapsible_state == TreeCollapsibleState::None {
            " "
        } else if expanded {
            "▾"
        } else {
            "▸"
        }
    }

    fn icon(item: &TreeItem) -> &'static str {
        match item.icon {
            Some(TreeIcon::File) => "▧",
            Some(TreeIcon::Folder) => "□",
            Some(TreeIcon::Symbol) => "◇",
            None => "",
        }
    }
}

impl EventEmitter<TreeViewEvent> for TreeView {}

impl Focusable for TreeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TreeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.visible_rows();
        let selected = self.selected.clone();
        let expanded = self.expanded.clone();
        let entity = cx.entity();
        uniform_list("extension-tree", rows.len(), move |range, _, _| {
            range
                .map(|index| match &rows[index] {
                    VisibleRow::Item { item, depth, .. } => {
                        let id = item.id.clone();
                        let is_selected = selected.as_deref() == Some(&item.id);
                        div()
                            .id(("extension-tree-row", index))
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
                            .child(Self::disclosure(item, expanded.contains(&item.id)))
                            .child(Self::icon(item))
                            .child(item.label.clone())
                            .when_some(item.description.clone(), |row, description| {
                                row.child(div().text_color(rgb(0x888888)).child(description))
                            })
                            .on_click({
                                let entity = entity.clone();
                                move |_, window, cx| {
                                    let window = window.window_handle();
                                    entity.update(cx, |tree, cx| tree.activate(&id, window, cx));
                                }
                            })
                            .into_any_element()
                    }
                    VisibleRow::Status {
                        depth,
                        label,
                        error,
                    } => div()
                        .id(("extension-tree-status", index))
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
        .key_context(TREE_KEY_CONTEXT)
        .on_key_down(cx.listener(Self::on_key_down))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext};

    use super::{TreeProviderIdentity, TreeView, TreeViewRegistrationError};
    use crate::host::protocol::{
        ExtensionId, ExtensionLifecycleId, TreeChildrenResponse, TreeCollapsibleState, TreeItem,
        TreeProviderRegistrationId,
    };

    #[gpui::test]
    fn provider_registration_enforces_view_and_ownership(cx: &mut TestAppContext) {
        let registration = TreeProviderRegistrationId::new(3);
        let owner = TreeProviderIdentity {
            extension: ExtensionId::new(7),
            lifecycle: ExtensionLifecycleId::new(2),
            registration,
        };
        let tree = cx.new(|cx| TreeView::new("outline", cx));

        tree.update(cx, |tree, cx| {
            assert_eq!(
                tree.register_provider("wrong-view", owner, cx),
                Err(TreeViewRegistrationError::WrongView)
            );
            tree.register_provider("outline", owner, cx).unwrap();
            assert!(tree.owns_provider(registration, owner.extension, owner.lifecycle));
            assert!(!tree.owns_provider(
                registration,
                owner.extension,
                ExtensionLifecycleId::new(1)
            ));
            assert_eq!(
                tree.register_provider(
                    "outline",
                    TreeProviderIdentity {
                        extension: ExtensionId::new(8),
                        lifecycle: ExtensionLifecycleId::new(1),
                        registration: TreeProviderRegistrationId::new(4),
                    },
                    cx,
                ),
                Err(TreeViewRegistrationError::ProviderInUse)
            );
            assert_eq!(
                tree.unregister_provider(TreeProviderRegistrationId::new(4), cx),
                Err(TreeViewRegistrationError::ProviderNotFound)
            );
            assert!(tree.owns_provider(registration, owner.extension, owner.lifecycle));
        });
    }

    #[gpui::test]
    fn stale_and_disposed_tree_responses_are_ignored(cx: &mut TestAppContext) {
        let registration = TreeProviderRegistrationId::new(3);
        let tree = cx.new(|cx| TreeView::new("outline", cx));
        let stale_generation = tree.update(cx, |tree, cx| {
            tree.register_provider(
                "outline",
                TreeProviderIdentity {
                    extension: ExtensionId::new(7),
                    lifecycle: ExtensionLifecycleId::new(2),
                    registration,
                },
                cx,
            )
            .unwrap();
            let stale_generation = tree.children[&None].generation;
            tree.invalidate(registration, None, cx).unwrap();
            stale_generation
        });

        tree.update(cx, |tree, cx| {
            assert!(!tree.apply_response(
                TreeChildrenResponse {
                    registration,
                    parent_id: None,
                    generation: stale_generation,
                    result: Ok(Vec::new()),
                },
                cx,
            ));
            let current_generation = tree.children[&None].generation;
            assert!(tree.apply_response(
                TreeChildrenResponse {
                    registration,
                    parent_id: None,
                    generation: current_generation,
                    result: Ok(vec![TreeItem {
                        id: "root".into(),
                        label: "Root".into(),
                        description: None,
                        icon: None,
                        collapsible_state: TreeCollapsibleState::None,
                        command: None,
                    }]),
                },
                cx,
            ));
            assert_eq!(tree.children[&None].items[0].id, "root");
            tree.unregister_provider(registration, cx).unwrap();
            assert!(!tree.apply_response(
                TreeChildrenResponse {
                    registration,
                    parent_id: None,
                    generation: current_generation,
                    result: Ok(Vec::new()),
                },
                cx,
            ));
            tree.register_provider(
                "outline",
                TreeProviderIdentity {
                    extension: ExtensionId::new(7),
                    lifecycle: ExtensionLifecycleId::new(2),
                    registration,
                },
                cx,
            )
            .unwrap();
            let lifecycle_generation = tree.children[&None].generation;
            tree.remove_lifecycle(ExtensionId::new(7), ExtensionLifecycleId::new(2), cx);
            assert!(!tree.apply_response(
                TreeChildrenResponse {
                    registration,
                    parent_id: None,
                    generation: lifecycle_generation,
                    result: Ok(Vec::new()),
                },
                cx,
            ));
        });
    }
}
