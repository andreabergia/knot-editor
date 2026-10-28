//! Foreground ownership for extension view and completion providers.
//!
//! A contributed view definition and its native instances share one
//! lifecycle-owned registration. Definitions exist independently of product
//! windows; each product window owns at most one native instance per definition.

use std::collections::{HashMap, HashSet};

use gpui::{App, Entity, WeakEntity};

use crate::host::protocol::CompletionProviderRegistrationId;
use crate::host::protocol::{
    ExtensionId, ExtensionLifecycleId, RequestContext, TreeAction, TreeChildrenResponse,
    TreeProviderRegistrationId, ViewContentKind, ViewId,
};

use crate::app::extensions::host::handlers::{
    AccessError, CompletionArguments, CompletionRegistrationArguments, ReplaceViewTextArguments,
    TreeArguments, TreeInvalidationArguments, TreeRegistrationArguments,
};
use crate::app::tool_layout::StoredLocation;
use crate::host::declarations::HostError;

use crate::app::{
    completion::{CompletionProviderRegistration, CompletionProviderRegistry},
    text_view::TextView,
    tree_view::{TreeProviderIdentity, TreeView, TreeViewRegistrationError},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, HostError)]
pub(crate) enum SemanticError {
    #[error(transparent)]
    Access(AccessError),
    #[error(code = "InvalidViewId", message = "InvalidViewId")]
    InvalidViewId,
    #[error(code = "ViewIdInUse", message = "ViewIdInUse")]
    ViewIdInUse,
    #[error(code = "ViewNotFound", message = "ViewNotFound")]
    ViewNotFound,
    #[error(
        code = "ViewRegistrationNotFound",
        message = "ViewRegistrationNotFound"
    )]
    ViewRegistrationNotFound,
    #[error(
        code = "CompletionProviderNotFound",
        message = "CompletionProviderNotFound"
    )]
    CompletionProviderNotFound,
    #[error(code = "ContentKindMismatch", message = "ContentKindMismatch")]
    ContentKindMismatch,
}
impl From<AccessError> for SemanticError {
    fn from(error: AccessError) -> Self {
        Self::Access(error)
    }
}

type Lifecycle = (ExtensionId, ExtensionLifecycleId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ViewPresentation {
    Show,
    Hide,
    Focus,
}

/// One lifecycle-owned contributed view definition visible to the product shell.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ViewDefinition {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) content: ViewContentKind,
    pub(crate) actions: Vec<TreeAction>,
    pub(crate) location: StoredLocation,
    pub(crate) order: usize,
    pub(crate) visible: bool,
}

struct TreeRegistration {
    definition: ViewDefinition,
    identity: TreeProviderIdentity,
    /// Last text pushed for a text frame, retained so windows created later
    /// still present current content without another JavaScript turn.
    text: Option<String>,
}

/// Foreground registries and native view surfaces exposed to extension requests.
pub(crate) struct ExtensionSemanticBridge {
    lifecycles: HashSet<Lifecycle>,
    tree_views: HashMap<String, Vec<(String, WeakEntity<TreeView>)>>,
    text_views: HashMap<String, Vec<(String, WeakEntity<TextView>)>>,
    tree_registrations: HashMap<TreeProviderRegistrationId, TreeRegistration>,
    next_tree_registration: u64,
    next_default_order: usize,
    completion_providers: CompletionProviderRegistry,
}

impl ExtensionSemanticBridge {
    pub(crate) fn new() -> Self {
        Self {
            lifecycles: HashSet::new(),
            tree_views: HashMap::new(),
            text_views: HashMap::new(),
            tree_registrations: HashMap::new(),
            next_tree_registration: 1,
            next_default_order: 1_000,
            completion_providers: CompletionProviderRegistry::new(),
        }
    }

    pub(crate) fn admit_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) {
        self.lifecycles.insert((extension, lifecycle));
    }

    /// Attach a native view instance to its definition's provider. A definition
    /// can exist before any window; a window can therefore attach immediately.
    pub(crate) fn add_tree_view(
        &mut self,
        kind: impl Into<String>,
        window: impl Into<String>,
        view: Entity<TreeView>,
        cx: &mut App,
    ) -> Result<(), TreeViewRegistrationError> {
        let kind = kind.into();
        if let Some(registration) = self
            .tree_registrations
            .values()
            .find(|entry| entry.definition.id == kind)
        {
            let actions = registration.definition.actions.clone();
            view.update(cx, |tree, cx| {
                tree.register_provider(&kind, registration.identity, actions, cx)
            })?;
        }
        self.tree_views
            .entry(kind)
            .or_default()
            .push((window.into(), view.downgrade()));
        Ok(())
    }

    /// Attach a native text instance, seeding it with the last pushed content.
    pub(crate) fn add_text_view(
        &mut self,
        kind: impl Into<String>,
        window: impl Into<String>,
        view: Entity<TextView>,
        cx: &mut App,
    ) {
        let kind = kind.into();
        if let Some(text) = self
            .tree_registrations
            .values()
            .find(|entry| entry.definition.id == kind)
            .and_then(|entry| entry.text.clone())
        {
            view.update(cx, |view, cx| view.replace_text(&text, cx));
        }
        self.text_views
            .entry(kind)
            .or_default()
            .push((window.into(), view.downgrade()));
    }

    pub(crate) fn remove_tree_view(&mut self, view: ViewId, cx: &App) {
        for views in self.tree_views.values_mut() {
            views.retain(|(_, candidate)| {
                candidate
                    .upgrade()
                    .is_some_and(|entity| entity.read(cx).instance_id() != view)
            });
        }
        for views in self.text_views.values_mut() {
            views.retain(|(_, candidate)| {
                candidate
                    .upgrade()
                    .is_some_and(|entity| entity.read(cx).instance_id() != view)
            });
        }
    }

    fn validate(&self, request: &RequestContext) -> Result<(), SemanticError> {
        if self
            .lifecycles
            .contains(&(request.extension, request.lifecycle))
        {
            Ok(())
        } else {
            Err(AccessError::Cancelled.into())
        }
    }

    pub(crate) fn register_tree_provider(
        &mut self,
        request: &RequestContext,
        arguments: TreeArguments,
        cx: &mut App,
    ) -> Result<TreeProviderRegistrationId, SemanticError> {
        self.validate(request)?;
        let TreeArguments {
            id,
            title,
            content,
            actions,
            location,
            order,
            visible,
        } = arguments;

        if id.is_empty() || id.starts_with("knot.") {
            return Err(SemanticError::InvalidViewId);
        }
        if self
            .tree_registrations
            .values()
            .any(|entry| entry.definition.id == id)
        {
            return Err(SemanticError::ViewIdInUse);
        }
        let registration = TreeProviderRegistrationId::new(self.next_tree_registration);
        let identity = TreeProviderIdentity {
            extension: request.extension,
            lifecycle: request.lifecycle,
            registration,
        };
        let order = order.unwrap_or_else(|| {
            let order = self.next_default_order;
            self.next_default_order = self
                .next_default_order
                .checked_add(1)
                .expect("view default order exhausted");
            order as u64
        });
        let definition = ViewDefinition {
            id: id.clone(),
            title,
            content,
            actions: actions.clone(),
            location,
            order: order.try_into().expect("view order fits usize"),
            visible,
        };
        if content == ViewContentKind::Tree {
            for view in self.live_tree_views(&id, None) {
                view.update(cx, |tree, cx| {
                    tree.register_provider(&id, identity, actions.clone(), cx)
                })
                .map_err(map_tree_error)?;
            }
        }
        self.next_tree_registration = self
            .next_tree_registration
            .checked_add(1)
            .expect("tree registration space exhausted");
        self.tree_registrations.insert(
            registration,
            TreeRegistration {
                definition,
                identity,
                text: None,
            },
        );
        Ok(registration)
    }

    pub(crate) fn replace_view_text(
        &mut self,
        request: &RequestContext,
        arguments: ReplaceViewTextArguments,
        cx: &mut App,
    ) -> Result<(), SemanticError> {
        self.validate(request)?;
        let ReplaceViewTextArguments {
            registration,
            text,
            window,
        } = arguments;
        let kind = self.owned_tree_kind(registration, request)?;
        let entry = self
            .tree_registrations
            .get_mut(&registration)
            .expect("owned registration exists");
        if entry.definition.content != ViewContentKind::Text {
            return Err(SemanticError::ContentKindMismatch);
        }
        entry.text = Some(text.clone());
        let views = self.live_text_views(&kind, window.as_deref());
        if window.is_some() && views.is_empty() {
            return Err(SemanticError::ViewNotFound);
        }
        for view in views {
            view.update(cx, |view, cx| view.replace_text(&text, cx));
        }
        Ok(())
    }

    pub(crate) fn invalidate_tree_provider(
        &mut self,
        request: &RequestContext,
        arguments: TreeInvalidationArguments,
        cx: &mut App,
    ) -> Result<(), SemanticError> {
        self.validate(request)?;
        let TreeInvalidationArguments {
            registration,
            parent_id,
            window,
        } = &arguments;

        let kind = self.owned_tree_kind(*registration, request)?;
        if self
            .tree_registrations
            .get(registration)
            .is_some_and(|entry| entry.definition.content != ViewContentKind::Tree)
        {
            return Err(SemanticError::ContentKindMismatch);
        }
        let views = self.live_tree_views(&kind, window.as_deref());
        if window.is_some() && views.is_empty() {
            return Err(SemanticError::ViewNotFound);
        }
        for view in views {
            view.update(cx, |tree, cx| {
                tree.invalidate(*registration, parent_id.clone(), cx)
            })
            .map_err(map_tree_error)?;
        }
        Ok(())
    }

    pub(crate) fn unregister_tree_provider(
        &mut self,
        request: &RequestContext,
        arguments: TreeRegistrationArguments,
        cx: &mut App,
    ) -> Result<(), SemanticError> {
        self.validate(request)?;
        let TreeRegistrationArguments { registration } = &arguments;

        let kind = self.owned_tree_kind(*registration, request)?;
        for view in self.live_tree_views(&kind, None) {
            view.update(cx, |tree, cx| tree.unregister_provider(*registration, cx))
                .map_err(map_tree_error)?;
        }
        self.tree_registrations.remove(registration);
        Ok(())
    }

    pub(crate) fn definitions(&self) -> Vec<ViewDefinition> {
        let mut definitions = self
            .tree_registrations
            .values()
            .map(|entry| entry.definition.clone())
            .collect::<Vec<_>>();
        definitions.sort_by(|left, right| (left.order, &left.id).cmp(&(right.order, &right.id)));
        definitions
    }

    pub(crate) fn has_definition(&self, id: &str) -> bool {
        self.tree_registrations
            .values()
            .any(|entry| entry.definition.id == id)
    }

    pub(crate) fn owned_kind(
        &self,
        registration: TreeProviderRegistrationId,
        request: &RequestContext,
    ) -> Result<String, SemanticError> {
        let entry = self
            .tree_registrations
            .get(&registration)
            .ok_or(SemanticError::ViewRegistrationNotFound)?;
        if (entry.identity.extension, entry.identity.lifecycle)
            != (request.extension, request.lifecycle)
        {
            return Err(SemanticError::ViewRegistrationNotFound);
        }
        Ok(entry.definition.id.clone())
    }

    pub(crate) fn register_completion_provider(
        &mut self,
        request: &RequestContext,
        arguments: CompletionArguments,
        _cx: &mut App,
    ) -> Result<CompletionProviderRegistrationId, SemanticError> {
        self.validate(request)?;
        let CompletionArguments { label } = &arguments;

        let registration =
            self.completion_providers
                .register(request.extension, request.lifecycle, label.clone());
        Ok(registration)
    }
    pub(crate) fn unregister_completion_provider(
        &mut self,
        request: &RequestContext,
        arguments: CompletionRegistrationArguments,
        _cx: &mut App,
    ) -> Result<(), SemanticError> {
        self.validate(request)?;
        let CompletionRegistrationArguments { registration } = &arguments;

        if !self.completion_providers.unregister(
            *registration,
            request.extension,
            request.lifecycle,
        ) {
            return Err(SemanticError::CompletionProviderNotFound);
        }
        Ok(())
    }
    fn owned_tree_kind(
        &self,
        registration: TreeProviderRegistrationId,
        request: &RequestContext,
    ) -> Result<String, SemanticError> {
        self.owned_kind(registration, request)
    }

    pub(crate) fn apply_tree_response(&self, response: TreeChildrenResponse, cx: &mut App) -> bool {
        let Some(entry) = self.tree_registrations.get(&response.registration) else {
            return false;
        };
        let mut applied = false;
        for view in self.live_tree_views(&entry.definition.id, Some(&response.window)) {
            applied |= view.update(cx, |tree, cx| tree.apply_response(response.clone(), cx));
        }
        applied
    }

    fn live_tree_views(&self, kind: &str, window: Option<&str>) -> Vec<Entity<TreeView>> {
        self.tree_views
            .get(kind)
            .into_iter()
            .flatten()
            .filter(|(candidate, _)| window.is_none_or(|window| candidate == window))
            .filter_map(|(_, view)| view.upgrade())
            .collect()
    }

    fn live_text_views(&self, kind: &str, window: Option<&str>) -> Vec<Entity<TextView>> {
        self.text_views
            .get(kind)
            .into_iter()
            .flatten()
            .filter(|(candidate, _)| window.is_none_or(|window| candidate == window))
            .filter_map(|(_, view)| view.upgrade())
            .collect()
    }

    pub(crate) fn completion_snapshot(&self) -> Vec<CompletionProviderRegistration> {
        self.completion_providers.snapshot()
    }

    #[cfg(test)]
    pub(crate) fn owns_completion_provider(
        &self,
        registration: CompletionProviderRegistrationId,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> bool {
        self.completion_providers
            .owns(registration, extension, lifecycle)
    }

    pub(crate) fn remove_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        cx: &mut App,
    ) -> Vec<String> {
        self.lifecycles.remove(&(extension, lifecycle));
        let removed = self
            .tree_registrations
            .iter()
            .filter_map(|(registration, entry)| {
                ((entry.identity.extension, entry.identity.lifecycle) == (extension, lifecycle))
                    .then_some((*registration, entry.definition.id.clone()))
            })
            .collect::<Vec<_>>();
        let mut ids = Vec::new();
        for (registration, kind) in removed {
            for view in self.live_tree_views(&kind, None) {
                view.update(cx, |tree, cx| {
                    tree.remove_lifecycle(extension, lifecycle, cx)
                });
            }
            for view in self.live_text_views(&kind, None) {
                view.update(cx, |view, cx| view.replace_text("", cx));
            }
            self.tree_registrations.remove(&registration);
            ids.push(kind);
        }
        self.completion_providers
            .remove_lifecycle(extension, lifecycle);
        ids
    }
}

impl Default for ExtensionSemanticBridge {
    fn default() -> Self {
        Self::new()
    }
}

fn map_tree_error(error: TreeViewRegistrationError) -> SemanticError {
    match error {
        TreeViewRegistrationError::WrongView => SemanticError::ViewNotFound,
        TreeViewRegistrationError::ProviderInUse => SemanticError::ViewIdInUse,
        TreeViewRegistrationError::ProviderNotFound => SemanticError::ViewRegistrationNotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::text_view::TextView;
    use crate::host::protocol::{RequestId, TreeCollapsibleState, TreeItem, TreeProviderError};
    use gpui::{AppContext, TestAppContext};

    fn request(extension: ExtensionId, lifecycle: ExtensionLifecycleId, id: u64) -> RequestContext {
        RequestContext {
            extension,
            lifecycle,
            id: RequestId::new(id),
            invocation: None,
        }
    }

    fn arguments(id: &str) -> TreeArguments {
        TreeArguments {
            id: id.into(),
            title: "Outline".into(),
            content: ViewContentKind::Tree,
            actions: Vec::new(),
            location: StoredLocation::Left,
            order: None,
            visible: true,
        }
    }

    fn text_arguments(id: &str) -> TreeArguments {
        TreeArguments {
            id: id.into(),
            title: "Log".into(),
            content: ViewContentKind::Text,
            actions: Vec::new(),
            location: StoredLocation::Bottom,
            order: None,
            visible: true,
        }
    }

    fn replace(
        registration: TreeProviderRegistrationId,
        text: &str,
        window: Option<&str>,
    ) -> ReplaceViewTextArguments {
        ReplaceViewTextArguments {
            registration,
            text: text.into(),
            window: window.map(str::to_owned),
        }
    }

    #[gpui::test]
    fn registrations_route_to_native_owners_and_cleanup_with_the_lifecycle(
        cx: &mut TestAppContext,
    ) {
        let extension = ExtensionId::new(7);
        let lifecycle = ExtensionLifecycleId::new(3);
        let tree = cx.new(|cx| TreeView::new("example.outline", "1", cx));
        cx.update(|cx| {
            let mut bridge = ExtensionSemanticBridge::new();
            bridge
                .add_tree_view("example.outline", "1", tree.clone(), cx)
                .unwrap();
            bridge.admit_lifecycle(extension, lifecycle);

            let registration = bridge
                .register_tree_provider(
                    &request(extension, lifecycle, 1),
                    arguments("example.outline"),
                    cx,
                )
                .unwrap();
            assert!(
                tree.read(cx)
                    .owns_provider(registration, extension, lifecycle)
            );
            assert_eq!(bridge.definitions()[0].id, "example.outline");
            let generation = tree.read(cx).root_generation();
            assert!(bridge.apply_tree_response(
                TreeChildrenResponse {
                    registration,
                    window: "1".into(),
                    view: tree.read(cx).instance_id(),
                    parent_id: None,
                    generation,
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

            let completion = bridge.register_completion_provider(
                &request(extension, lifecycle, 2),
                CompletionArguments {
                    label: "fixture".into(),
                },
                cx,
            );
            let completion = completion.unwrap();
            assert!(bridge.owns_completion_provider(completion, extension, lifecycle));
            assert_eq!(bridge.completion_snapshot()[0].id, completion);

            bridge.remove_lifecycle(extension, lifecycle, cx);
            assert_eq!(tree.read(cx).lifecycle_state(), (false, 0, false, 0));
            assert!(!bridge.owns_completion_provider(completion, extension, lifecycle));
            assert!(bridge.definitions().is_empty());
            assert!(!bridge.apply_tree_response(
                TreeChildrenResponse {
                    registration,
                    window: "1".into(),
                    view: tree.read(cx).instance_id(),
                    parent_id: None,
                    generation,
                    result: Err(TreeProviderError {
                        message: "late".into(),
                    }),
                },
                cx,
            ));
        });
    }

    #[gpui::test]
    fn targeted_invalidation_touches_only_the_requested_window(cx: &mut TestAppContext) {
        let extension = ExtensionId::new(11);
        let lifecycle = ExtensionLifecycleId::new(5);
        cx.update(|cx| {
            let mut bridge = ExtensionSemanticBridge::new();
            bridge.admit_lifecycle(extension, lifecycle);
            let registration = bridge
                .register_tree_provider(
                    &request(extension, lifecycle, 1),
                    arguments("example.outline"),
                    cx,
                )
                .unwrap();
            let first = cx.new(|cx| TreeView::new("example.outline", "1", cx));
            let second = cx.new(|cx| TreeView::new("example.outline", "2", cx));
            bridge
                .add_tree_view("example.outline", "1", first.clone(), cx)
                .unwrap();
            bridge
                .add_tree_view("example.outline", "2", second.clone(), cx)
                .unwrap();

            let arguments = |window: Option<&str>| TreeInvalidationArguments {
                registration,
                parent_id: None,
                window: window.map(str::to_owned),
            };
            let first_before = first.read(cx).root_generation();
            let second_before = second.read(cx).root_generation();
            bridge
                .invalidate_tree_provider(
                    &request(extension, lifecycle, 2),
                    arguments(Some("1")),
                    cx,
                )
                .unwrap();
            assert!(first.read(cx).root_generation() > first_before);
            assert_eq!(second.read(cx).root_generation(), second_before);

            bridge
                .invalidate_tree_provider(&request(extension, lifecycle, 3), arguments(None), cx)
                .unwrap();
            assert!(second.read(cx).root_generation() > second_before);
            assert_eq!(
                bridge.invalidate_tree_provider(
                    &request(extension, lifecycle, 4),
                    arguments(Some("missing")),
                    cx
                ),
                Err(SemanticError::ViewNotFound)
            );
        });
    }

    #[gpui::test]
    fn text_frames_retain_last_text_and_target_windows(cx: &mut TestAppContext) {
        let extension = ExtensionId::new(12);
        let lifecycle = ExtensionLifecycleId::new(6);
        cx.update(|cx| {
            let mut bridge = ExtensionSemanticBridge::new();
            bridge.admit_lifecycle(extension, lifecycle);
            let registration = bridge
                .register_tree_provider(
                    &request(extension, lifecycle, 1),
                    text_arguments("example.log"),
                    cx,
                )
                .unwrap();
            let first = cx.new(|cx| TextView::new("example.log", "1", cx));
            bridge.add_text_view("example.log", "1", first.clone(), cx);
            bridge
                .replace_view_text(
                    &request(extension, lifecycle, 2),
                    replace(registration, "hello", None),
                    cx,
                )
                .unwrap();
            assert_eq!(first.read(cx).text(cx), "hello");

            // A window created after the push receives the retained text.
            let second = cx.new(|cx| TextView::new("example.log", "2", cx));
            bridge.add_text_view("example.log", "2", second.clone(), cx);
            assert_eq!(second.read(cx).text(cx), "hello");

            bridge
                .replace_view_text(
                    &request(extension, lifecycle, 3),
                    replace(registration, "only first", Some("1")),
                    cx,
                )
                .unwrap();
            assert_eq!(first.read(cx).text(cx), "only first");
            assert_eq!(second.read(cx).text(cx), "hello");
            assert_eq!(
                bridge.replace_view_text(
                    &request(extension, lifecycle, 4),
                    replace(registration, "missing", Some("missing")),
                    cx,
                ),
                Err(SemanticError::ViewNotFound)
            );

            let tree = bridge
                .register_tree_provider(
                    &request(extension, lifecycle, 5),
                    arguments("example.outline"),
                    cx,
                )
                .unwrap();
            assert_eq!(
                bridge.replace_view_text(
                    &request(extension, lifecycle, 6),
                    replace(tree, "wrong kind", None),
                    cx,
                ),
                Err(SemanticError::ContentKindMismatch)
            );
            assert_eq!(
                bridge.invalidate_tree_provider(
                    &request(extension, lifecycle, 7),
                    TreeInvalidationArguments {
                        registration,
                        parent_id: None,
                        window: None,
                    },
                    cx,
                ),
                Err(SemanticError::ContentKindMismatch)
            );

            bridge.remove_lifecycle(extension, lifecycle, cx);
            assert_eq!(first.read(cx).text(cx), "");
            assert_eq!(second.read(cx).text(cx), "");
        });
    }

    #[gpui::test]
    fn duplicate_ids_and_reserved_names_are_rejected(cx: &mut TestAppContext) {
        let extension = ExtensionId::new(9);
        let lifecycle = ExtensionLifecycleId::new(4);
        cx.update(|cx| {
            let mut bridge = ExtensionSemanticBridge::new();
            bridge.admit_lifecycle(extension, lifecycle);
            assert_eq!(
                bridge.register_tree_provider(
                    &request(extension, lifecycle, 1),
                    arguments("knot.explorer"),
                    cx
                ),
                Err(SemanticError::InvalidViewId)
            );
            bridge
                .register_tree_provider(
                    &request(extension, lifecycle, 2),
                    arguments("example.outline"),
                    cx,
                )
                .unwrap();
            assert_eq!(
                bridge.register_tree_provider(
                    &request(extension, lifecycle, 3),
                    arguments("example.outline"),
                    cx
                ),
                Err(SemanticError::ViewIdInUse)
            );
            assert_eq!(
                bridge.unregister_completion_provider(
                    &request(extension, lifecycle, 4),
                    CompletionRegistrationArguments {
                        registration: CompletionProviderRegistrationId::new(99),
                    },
                    cx
                ),
                Err(SemanticError::CompletionProviderNotFound)
            );
        });
    }
}
