//! Native presentation and interaction state for a semantic extension tree.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{prelude::FluentBuilder, *};

use crate::host::protocol::{
    Command, CommandArgumentValue, ExtensionId, ExtensionLifecycleId, TreeAction, TreeActionIcon,
    TreeBadge, TreeChildrenRequest, TreeChildrenResponse, TreeCollapsibleState, TreeIcon, TreeItem,
    TreeItemKind, TreeProviderRegistrationId, ViewId,
};

use super::TREE_KEY_CONTEXT;

pub(crate) static NEXT_VIEW_ID: AtomicU64 = AtomicU64::new(1);
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
        arguments: CommandArgumentValue,
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

/// Keyboard action mode: the row whose actions are being navigated, or frame
/// actions when no row qualifies.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ActionFocus {
    row: Option<String>,
    index: usize,
}

/// Foreground-owned cache and presentation state for one native tree surface.
pub(crate) struct TreeView {
    view_id: String,
    window: String,
    instance_id: ViewId,
    provider: Option<TreeProviderIdentity>,
    frame_actions: Vec<TreeAction>,
    children: HashMap<Option<String>, ChildrenState>,
    expanded: HashSet<String>,
    selected: Option<String>,
    action_focus: Option<ActionFocus>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
}

impl TreeView {
    pub(crate) fn kind(&self) -> &str {
        &self.view_id
    }

    pub(crate) fn new(
        view_id: impl Into<String>,
        window: impl Into<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            view_id: view_id.into(),
            window: window.into(),
            instance_id: ViewId::new(NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed)),
            provider: None,
            frame_actions: Vec::new(),
            children: HashMap::new(),
            expanded: HashSet::new(),
            selected: None,
            action_focus: None,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub(crate) fn instance_id(&self) -> ViewId {
        self.instance_id
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
        CommandClaim::Extension {
            handler,
            view: Some(self.instance_id),
        }
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
        frame_actions: Vec<TreeAction>,
        cx: &mut Context<Self>,
    ) -> Result<(), TreeViewRegistrationError> {
        if view_id != self.view_id {
            return Err(TreeViewRegistrationError::WrongView);
        }
        if self.provider.is_some() {
            return Err(TreeViewRegistrationError::ProviderInUse);
        }
        self.provider = Some(provider);
        self.frame_actions = frame_actions;
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
        if parent_id.is_none() {
            // A root refresh replaces the whole cache so nested groups and
            // expanded children refetch current data.
            self.children.clear();
            self.action_focus = None;
        }
        let state = self.children.entry(parent_id.clone()).or_default();
        state.generation = generation;
        state.loading = true;
        state.error = None;
        cx.emit(TreeViewEvent::RequestChildren {
            provider,
            request: TreeChildrenRequest {
                registration,
                window: self.window.clone(),
                view: self.instance_id,
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

    /// Reload root children for a still-registered provider.
    pub(crate) fn refresh_root(&mut self, cx: &mut Context<Self>) {
        if let Some(provider) = self.provider {
            let _ = self.invalidate(provider.registration, None, cx);
        }
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
        self.frame_actions.clear();
        self.children.clear();
        self.expanded.clear();
        self.selected = None;
        self.action_focus = None;
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
        if provider.registration != response.registration
            || response.window != self.window
            || response.view != self.instance_id
        {
            return false;
        }
        // Resolve which nested rows need their own children before borrowing
        // the parent state mutably.
        let children_to_load = match &response.result {
            Ok(items) => items
                .iter()
                .filter(|item| {
                    (item.kind == TreeItemKind::Group
                        || item.collapsible_state == TreeCollapsibleState::Expanded)
                        && !self.children.contains_key(&Some(item.id.clone()))
                })
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        let Some(state) = self.children.get_mut(&response.parent_id) else {
            return false;
        };
        if state.generation != response.generation {
            return false;
        }
        state.loading = false;
        match response.result {
            Ok(items) => {
                for item in &items {
                    if item.kind != TreeItemKind::Group
                        && item.collapsible_state == TreeCollapsibleState::Expanded
                    {
                        self.expanded.insert(item.id.clone());
                    }
                }
                state.items = items;
                state.error = None;
            }
            Err(error) => state.error = Some(error.message),
        }
        for id in children_to_load {
            let _ = self.invalidate(provider.registration, Some(id), cx);
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
            if item.kind == TreeItemKind::Group || self.expanded.contains(&item.id) {
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

    fn item(&self, id: &str) -> Option<&TreeItem> {
        self.children
            .values()
            .flat_map(|state| &state.items)
            .find(|item| item.id == id)
    }

    fn activate(&mut self, id: &str, window: AnyWindowHandle, cx: &mut Context<Self>) {
        self.selected = Some(id.to_owned());
        let Some(item) = self.item(id).cloned() else {
            return;
        };
        // Group headers organize rows and expose actions; they are not
        // activation targets themselves.
        if item.kind == TreeItemKind::Group {
            cx.notify();
            return;
        }
        match item.collapsible_state {
            TreeCollapsibleState::None => {
                if let (Some(provider), Some(command)) = (self.provider, item.command) {
                    cx.emit(TreeViewEvent::InvokeCommand {
                        provider,
                        command,
                        arguments: CommandArgumentValue::Null,
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

    fn actions_for_row(&self, id: &str) -> Vec<TreeAction> {
        self.item(id)
            .map(|item| item.actions.clone())
            .unwrap_or_default()
    }

    fn focused_actions(&self) -> Vec<TreeAction> {
        let Some(focus) = &self.action_focus else {
            return Vec::new();
        };
        match &focus.row {
            Some(row) => self.actions_for_row(row),
            None => self.frame_actions.clone(),
        }
    }

    /// Enter keyboard action mode for the current row, falling back to the
    /// frame's toolbar when that row exposes no actions.
    fn enter_action_focus(&mut self, cx: &mut Context<Self>) {
        if let Some(selected) = self.selected.clone()
            && !self.actions_for_row(&selected).is_empty()
        {
            self.action_focus = Some(ActionFocus {
                row: Some(selected),
                index: 0,
            });
            cx.notify();
            return;
        }
        if !self.frame_actions.is_empty() {
            self.action_focus = Some(ActionFocus {
                row: None,
                index: 0,
            });
            cx.notify();
        }
    }

    fn invoke_action(
        &mut self,
        action: TreeAction,
        window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self.provider else {
            return;
        };
        cx.emit(TreeViewEvent::InvokeCommand {
            provider,
            command: action.command,
            arguments: action.arguments,
            window,
            focus: self.focus.downgrade(),
        });
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.to_lowercase();
        if let Some(focus) = self.action_focus.clone() {
            let actions = self.focused_actions();
            if actions.is_empty() {
                self.action_focus = None;
                return;
            }
            match key.as_str() {
                "up" => {
                    self.action_focus = Some(ActionFocus {
                        row: focus.row,
                        index: focus.index.saturating_sub(1),
                    });
                    cx.notify();
                    cx.stop_propagation();
                }
                "down" => {
                    self.action_focus = Some(ActionFocus {
                        row: focus.row,
                        index: (focus.index + 1).min(actions.len() - 1),
                    });
                    cx.notify();
                    cx.stop_propagation();
                }
                "enter" | "space" => {
                    let action = actions[focus.index.min(actions.len() - 1)].clone();
                    self.invoke_action(action, window.window_handle(), cx);
                    cx.stop_propagation();
                }
                "escape" | "left" => {
                    self.action_focus = None;
                    cx.notify();
                    cx.stop_propagation();
                }
                _ => {}
            }
            return;
        }

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
        let target = match key.as_str() {
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
            "right" => {
                let Some(index) = current else {
                    return;
                };
                let id = item_rows[index].0;
                let collapsible = self
                    .item(id)
                    .is_some_and(|item| item.collapsible_state != TreeCollapsibleState::None);
                if collapsible {
                    self.activate(id, window.window_handle(), cx);
                } else {
                    self.selected = Some(id.to_owned());
                    self.enter_action_focus(cx);
                }
                cx.stop_propagation();
                return;
            }
            "enter" => {
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
            self.action_focus = None;
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn disclosure(item: &TreeItem, expanded: bool) -> &'static str {
        if item.kind == TreeItemKind::Group || item.collapsible_state == TreeCollapsibleState::None
        {
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

    fn action_glyph(icon: Option<TreeActionIcon>) -> &'static str {
        match icon {
            Some(TreeActionIcon::Stage) => "+",
            Some(TreeActionIcon::Unstage) => "−",
            Some(TreeActionIcon::Discard) => "×",
            None => "",
        }
    }

    fn badge_letter(badge: TreeBadge) -> &'static str {
        match badge {
            TreeBadge::Added => "A",
            TreeBadge::Modified => "M",
            TreeBadge::Deleted => "D",
            TreeBadge::Renamed => "R",
            TreeBadge::Untracked => "U",
            TreeBadge::Conflicted => "!",
        }
    }

    fn badge_color(badge: TreeBadge) -> u32 {
        match badge {
            TreeBadge::Added => 0x5fb85f,
            TreeBadge::Modified => 0xe2c08d,
            TreeBadge::Deleted => 0xf48771,
            TreeBadge::Renamed => 0x6cb6ff,
            TreeBadge::Untracked => 0x9a9a9a,
            TreeBadge::Conflicted => 0xe08a3c,
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
        let action_focus = self.action_focus.clone();
        let frame_actions = self.frame_actions.clone();
        let entity = cx.entity();
        let mut key_context = KeyContext::default();
        key_context.add(TREE_KEY_CONTEXT);
        key_context.add(super::keymaps::view_context(&self.view_id));

        let toolbar = if frame_actions.is_empty() {
            None
        } else {
            let frame_focused = action_focus
                .as_ref()
                .is_some_and(|focus| focus.row.is_none());
            let mut bar = div()
                .id("extension-tree-frame-actions")
                .w_full()
                .px_2()
                .py_1()
                .flex()
                .flex_row()
                .gap_2()
                .border_b_1()
                .border_color(rgb(0x333333));
            for (action_index, action) in frame_actions.into_iter().enumerate() {
                let focused = frame_focused
                    && action_focus
                        .as_ref()
                        .is_some_and(|focus| focus.index == action_index);
                let glyph = Self::action_glyph(action.icon);
                let target = action.clone();
                let entity = entity.clone();
                bar = bar.child(
                    div()
                        .id(("extension-tree-frame-action", action_index))
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .text_xs()
                        .text_color(rgb(0xd0d0d0))
                        .bg(if focused {
                            rgb(0x2a4a7a)
                        } else {
                            rgb(0x2d2d2d)
                        })
                        .hover(|style| style.bg(rgb(0x3a3a3a)))
                        .flex()
                        .flex_row()
                        .gap_1()
                        .when(!glyph.is_empty(), |button| button.child(glyph))
                        .child(action.label)
                        .on_click(move |_, window, cx| {
                            let handle = window.window_handle();
                            entity.update(cx, |tree, cx| {
                                tree.focus_handle(cx).focus(window);
                                tree.invoke_action(target.clone(), handle, cx);
                            });
                            cx.stop_propagation();
                        }),
                );
            }
            Some(bar)
        };

        let list = uniform_list("extension-tree", rows.len(), move |range, _, _| {
            range
                .map(|index| match &rows[index] {
                    VisibleRow::Item { item, depth, .. } => {
                        let id = item.id.clone();
                        let is_selected = selected.as_deref() == Some(&item.id);
                        let is_group = item.kind == TreeItemKind::Group;
                        let row_action_focus = action_focus.as_ref().and_then(|focus| {
                            (focus.row.as_deref() == Some(item.id.as_str())).then_some(focus.index)
                        });
                        let mut row = div()
                            .id(("extension-tree-row", index))
                            .debug_selector(move || format!("extension-tree-row-{index}"))
                            .w_full()
                            .pl(px(8. + *depth as f32 * 14.))
                            .pr_2()
                            .py_1()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .text_sm()
                            .when(is_group, |row| {
                                row.bg(rgb(0x262626)).text_color(rgb(0x9a9a9a))
                            })
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
                            .child(div().flex_1());
                        if let Some(status) = item.status_text.clone() {
                            row = row.child(div().text_color(rgb(0xb0b0b0)).child(status));
                        }
                        if let Some(badge) = item.badge {
                            row = row.child(
                                div()
                                    .px_1()
                                    .rounded_sm()
                                    .text_xs()
                                    .text_color(rgb(Self::badge_color(badge)))
                                    .child(Self::badge_letter(badge)),
                            );
                        }
                        for (action_index, action) in item.actions.iter().enumerate() {
                            let focused = row_action_focus == Some(action_index);
                            let glyph = Self::action_glyph(action.icon);
                            let target = action.clone();
                            let entity = entity.clone();
                            row = row.child(
                                div()
                                    .id(("extension-tree-action", index * 1024 + action_index))
                                    .px_1()
                                    .rounded_sm()
                                    .text_xs()
                                    .text_color(rgb(0x6cb6ff))
                                    .bg(if focused {
                                        rgb(0x2a4a7a)
                                    } else {
                                        rgb(0x2d2d2d)
                                    })
                                    .hover(|style| style.bg(rgb(0x3a3a3a)))
                                    .flex()
                                    .flex_row()
                                    .gap_1()
                                    .when(!glyph.is_empty(), |button| button.child(glyph))
                                    .child(action.label.clone())
                                    .on_click(move |_, window, cx| {
                                        let handle = window.window_handle();
                                        entity.update(cx, |tree, cx| {
                                            tree.focus_handle(cx).focus(window);
                                            tree.invoke_action(target.clone(), handle, cx);
                                        });
                                        cx.stop_propagation();
                                    }),
                            );
                        }
                        row.on_click({
                            let entity = entity.clone();
                            move |_, window, cx| {
                                let handle = window.window_handle();
                                entity.update(cx, |tree, cx| {
                                    tree.focus_handle(cx).focus(window);
                                    tree.activate(&id, handle, cx);
                                });
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
        .flex_1()
        .track_scroll(self.scroll.clone())
        .track_focus(&self.focus)
        .key_context(key_context)
        .on_key_down(cx.listener(Self::on_key_down));

        div()
            .size_full()
            .flex()
            .flex_col()
            .when_some(toolbar, |container, toolbar| container.child(toolbar))
            .child(list)
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
        let tree = cx.new(|cx| TreeView::new("outline", "1", cx));

        tree.update(cx, |tree, cx| {
            assert_eq!(
                tree.register_provider("wrong-view", owner, Vec::new(), cx),
                Err(TreeViewRegistrationError::WrongView)
            );
            tree.register_provider("outline", owner, Vec::new(), cx)
                .unwrap();
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
                    Vec::new(),
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
        let tree = cx.new(|cx| TreeView::new("outline", "1", cx));
        let stale_generation = tree.update(cx, |tree, cx| {
            tree.register_provider(
                "outline",
                TreeProviderIdentity {
                    extension: ExtensionId::new(7),
                    lifecycle: ExtensionLifecycleId::new(2),
                    registration,
                },
                Vec::new(),
                cx,
            )
            .unwrap();
            let stale_generation = tree.children[&None].generation;
            tree.invalidate(registration, None, cx).unwrap();
            stale_generation
        });

        let view = cx.read(|cx| tree.read(cx).instance_id());
        tree.update(cx, |tree, cx| {
            assert!(!tree.apply_response(
                TreeChildrenResponse {
                    registration,
                    window: "1".into(),
                    view,
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
                    window: "1".into(),
                    view,
                    parent_id: None,
                    generation: current_generation,
                    result: Ok(vec![TreeItem {
                        id: "root".into(),
                        label: "Root".into(),
                        description: None,
                        icon: None,
                        collapsible_state: TreeCollapsibleState::None,
                        command: None,
                        kind: crate::host::protocol::TreeItemKind::Item,
                        status_text: None,
                        badge: None,
                        actions: Vec::new(),
                    }]),
                },
                cx,
            ));
            assert_eq!(tree.children[&None].items[0].id, "root");
            tree.unregister_provider(registration, cx).unwrap();
            assert!(!tree.apply_response(
                TreeChildrenResponse {
                    registration,
                    window: "1".into(),
                    view,
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
                Vec::new(),
                cx,
            )
            .unwrap();
            let lifecycle_generation = tree.children[&None].generation;
            tree.remove_lifecycle(ExtensionId::new(7), ExtensionLifecycleId::new(2), cx);
            assert!(!tree.apply_response(
                TreeChildrenResponse {
                    registration,
                    window: "1".into(),
                    view,
                    parent_id: None,
                    generation: lifecycle_generation,
                    result: Ok(Vec::new()),
                },
                cx,
            ));
        });
    }
}
