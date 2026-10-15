//! Foreground ownership for extension tree and completion providers.

use std::collections::{HashMap, HashSet};

use gpui::{App, Entity, WeakEntity};

use crate::host::protocol::CompletionProviderRegistrationId;
use crate::host::protocol::{
    ExtensionId, ExtensionLifecycleId, RequestContext, TreeChildrenResponse,
    TreeProviderRegistrationId, ViewId,
};

use super::extension_host::handlers::{
    AccessError, CompletionArguments, CompletionRegistrationArguments, TreeArguments,
    TreeInvalidationArguments, TreeRegistrationArguments,
};
use crate::host::declarations::HostError;

use super::{
    completion::{CompletionProviderRegistration, CompletionProviderRegistry},
    tree_view::{TreeProviderIdentity, TreeView, TreeViewRegistrationError},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, HostError)]
pub(crate) enum SemanticError {
    #[error(transparent)]
    Access(AccessError),
    #[error(code = "TreeViewNotFound", message = "TreeViewNotFound")]
    TreeViewNotFound,
    #[error(code = "TreeProviderInUse", message = "TreeProviderInUse")]
    TreeProviderInUse,
    #[error(code = "TreeProviderNotFound", message = "TreeProviderNotFound")]
    TreeProviderNotFound,
    #[error(
        code = "CompletionProviderNotFound",
        message = "CompletionProviderNotFound"
    )]
    CompletionProviderNotFound,
}
impl From<AccessError> for SemanticError {
    fn from(error: AccessError) -> Self {
        Self::Access(error)
    }
}

type Lifecycle = (ExtensionId, ExtensionLifecycleId);

/// Foreground registries and native tree surfaces exposed to extension requests.
pub(crate) struct ExtensionSemanticBridge {
    lifecycles: HashSet<Lifecycle>,
    tree_kinds: HashSet<String>,
    tree_views: HashMap<String, Vec<WeakEntity<TreeView>>>,
    tree_registrations: HashMap<TreeProviderRegistrationId, TreeRegistration>,
    next_tree_registration: u64,
    completion_providers: CompletionProviderRegistry,
}

struct TreeRegistration {
    kind: String,
    identity: TreeProviderIdentity,
}

impl ExtensionSemanticBridge {
    pub(crate) fn new() -> Self {
        Self {
            lifecycles: HashSet::new(),
            tree_kinds: HashSet::new(),
            tree_views: HashMap::new(),
            tree_registrations: HashMap::new(),
            next_tree_registration: 1,
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

    pub(crate) fn declare_tree_kind(&mut self, kind: impl Into<String>) {
        self.tree_kinds.insert(kind.into());
    }

    pub(crate) fn add_tree_view(
        &mut self,
        kind: impl Into<String>,
        view: Entity<TreeView>,
        cx: &mut App,
    ) -> Result<(), TreeViewRegistrationError> {
        let kind = kind.into();
        self.tree_kinds.insert(kind.clone());
        if let Some(registration) = self
            .tree_registrations
            .values()
            .find(|entry| entry.kind == kind)
        {
            view.update(cx, |tree, cx| {
                tree.register_provider(&kind, registration.identity, cx)
            })?;
        }
        self.tree_views
            .entry(kind)
            .or_default()
            .push(view.downgrade());
        Ok(())
    }

    pub(crate) fn remove_tree_view(&mut self, view: ViewId, cx: &App) {
        for views in self.tree_views.values_mut() {
            views.retain(|candidate| {
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
        let TreeArguments { view_id } = &arguments;

        if !self.tree_kinds.contains(view_id) {
            return Err(SemanticError::TreeViewNotFound);
        }
        if self
            .tree_registrations
            .values()
            .any(|entry| entry.kind == *view_id)
        {
            return Err(SemanticError::TreeProviderInUse);
        }
        let registration = TreeProviderRegistrationId::new(self.next_tree_registration);
        let identity = TreeProviderIdentity {
            extension: request.extension,
            lifecycle: request.lifecycle,
            registration,
        };
        for view in self.live_tree_views(view_id) {
            view.update(cx, |tree, cx| tree.register_provider(view_id, identity, cx))
                .map_err(map_tree_error)?;
        }
        self.next_tree_registration = self
            .next_tree_registration
            .checked_add(1)
            .expect("tree registration space exhausted");
        self.tree_registrations.insert(
            registration,
            TreeRegistration {
                kind: view_id.clone(),
                identity,
            },
        );
        Ok(registration)
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
        } = &arguments;

        let kind = self.owned_tree_kind(*registration, request)?;
        for view in self.live_tree_views(&kind) {
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
        for view in self.live_tree_views(&kind) {
            view.update(cx, |tree, cx| tree.unregister_provider(*registration, cx))
                .map_err(map_tree_error)?;
        }
        self.tree_registrations.remove(registration);
        Ok(())
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
        let entry = self
            .tree_registrations
            .get(&registration)
            .ok_or(SemanticError::TreeProviderNotFound)?;
        if (entry.identity.extension, entry.identity.lifecycle)
            != (request.extension, request.lifecycle)
        {
            return Err(SemanticError::TreeProviderNotFound);
        }
        Ok(entry.kind.clone())
    }

    pub(crate) fn apply_tree_response(&self, response: TreeChildrenResponse, cx: &mut App) -> bool {
        let Some(entry) = self.tree_registrations.get(&response.registration) else {
            return false;
        };
        let mut applied = false;
        for view in self.live_tree_views(&entry.kind) {
            applied |= view.update(cx, |tree, cx| tree.apply_response(response.clone(), cx));
        }
        applied
    }

    fn live_tree_views(&self, kind: &str) -> Vec<Entity<TreeView>> {
        self.tree_views
            .get(kind)
            .into_iter()
            .flatten()
            .filter_map(WeakEntity::upgrade)
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
    ) {
        self.lifecycles.remove(&(extension, lifecycle));
        self.tree_registrations.retain(|_, entry| {
            (entry.identity.extension, entry.identity.lifecycle) != (extension, lifecycle)
        });
        for views in self.tree_views.values() {
            for view in views.iter().filter_map(WeakEntity::upgrade) {
                view.update(cx, |tree, cx| {
                    tree.remove_lifecycle(extension, lifecycle, cx)
                });
            }
        }
        self.completion_providers
            .remove_lifecycle(extension, lifecycle);
    }
}

impl Default for ExtensionSemanticBridge {
    fn default() -> Self {
        Self::new()
    }
}

fn map_tree_error(error: TreeViewRegistrationError) -> SemanticError {
    match error {
        TreeViewRegistrationError::WrongView => SemanticError::TreeViewNotFound,
        TreeViewRegistrationError::ProviderInUse => SemanticError::TreeProviderInUse,
        TreeViewRegistrationError::ProviderNotFound => SemanticError::TreeProviderNotFound,
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, TestAppContext};

    use crate::host::protocol::{
        CompletionProviderRegistrationId, RequestId, TreeCollapsibleState, TreeItem,
        TreeProviderError,
    };

    use super::*;

    fn request(extension: ExtensionId, lifecycle: ExtensionLifecycleId, id: u64) -> RequestContext {
        RequestContext {
            extension,
            lifecycle,
            id: RequestId::new(id),
            invocation: None,
        }
    }

    #[gpui::test]
    fn registrations_route_to_native_owners_and_cleanup_with_the_lifecycle(
        cx: &mut TestAppContext,
    ) {
        let extension = ExtensionId::new(7);
        let lifecycle = ExtensionLifecycleId::new(3);
        let tree = cx.new(|cx| TreeView::new("outline", cx));
        cx.update(|cx| {
            let mut bridge = ExtensionSemanticBridge::new();
            bridge.add_tree_view("outline", tree.clone(), cx).unwrap();
            bridge.admit_lifecycle(extension, lifecycle);

            let registered = bridge.register_tree_provider(
                &request(extension, lifecycle, 1),
                TreeArguments {
                    view_id: "outline".into(),
                },
                cx,
            );
            let registration = registered.unwrap();
            assert!(
                tree.read(cx)
                    .owns_provider(registration, extension, lifecycle)
            );
            let generation = tree.read(cx).root_generation();
            assert!(bridge.apply_tree_response(
                TreeChildrenResponse {
                    registration,
                    parent_id: None,
                    generation,
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
            assert!(!bridge.apply_tree_response(
                TreeChildrenResponse {
                    registration,
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
    fn wrong_lifecycle_and_registration_are_rejected(cx: &mut TestAppContext) {
        let extension = ExtensionId::new(9);
        let lifecycle = ExtensionLifecycleId::new(4);
        let tree = cx.new(|cx| TreeView::new("outline", cx));
        cx.update(|cx| {
            let mut bridge = ExtensionSemanticBridge::new();
            bridge.add_tree_view("outline", tree, cx).unwrap();
            assert_eq!(
                bridge.register_tree_provider(
                    &request(extension, lifecycle, 1),
                    TreeArguments {
                        view_id: "outline".into(),
                    },
                    cx
                ),
                Err(SemanticError::Access(AccessError::Cancelled))
            );
            bridge.admit_lifecycle(extension, lifecycle);
            assert_eq!(
                bridge.unregister_completion_provider(
                    &request(extension, lifecycle, 2),
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
