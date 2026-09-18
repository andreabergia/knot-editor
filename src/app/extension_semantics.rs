//! Foreground ownership for extension tree and completion providers.

use std::collections::{HashMap, HashSet};

use gpui::{App, Entity};

use crate::host::protocol::{
    CompletionProviderRegistrationId, ExtensionId, ExtensionLifecycleId, HostOperation,
    HostRequest, HostRequestError, HostResponse, HostResponseValue, TreeChildrenResponse,
    TreeProviderRegistrationId,
};

use super::{
    completion::{CompletionProviderRegistration, CompletionProviderRegistry},
    tree_view::{TreeProviderIdentity, TreeView, TreeViewRegistrationError},
};

type Lifecycle = (ExtensionId, ExtensionLifecycleId);

/// Foreground registries and native tree surfaces exposed to extension requests.
pub(crate) struct ExtensionSemanticBridge {
    lifecycles: HashSet<Lifecycle>,
    tree_views: HashMap<String, Entity<TreeView>>,
    tree_registrations: HashMap<TreeProviderRegistrationId, Entity<TreeView>>,
    next_tree_registration: u64,
    completion_providers: CompletionProviderRegistry,
}

impl ExtensionSemanticBridge {
    pub(crate) fn new() -> Self {
        Self {
            lifecycles: HashSet::new(),
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

    pub(crate) fn add_tree_view(&mut self, view_id: impl Into<String>, view: Entity<TreeView>) {
        self.tree_views.insert(view_id.into(), view);
    }

    pub(crate) fn dispatch(&mut self, request: HostRequest, cx: &mut App) -> HostResponse {
        let identity = (request.extension, request.lifecycle);
        let result = if self.lifecycles.contains(&identity) {
            self.dispatch_live(&request, cx)
        } else {
            Err(HostRequestError::Cancelled)
        };
        HostResponse {
            extension: request.extension,
            lifecycle: request.lifecycle,
            id: request.id,
            result,
        }
    }

    fn dispatch_live(
        &mut self,
        request: &HostRequest,
        cx: &mut App,
    ) -> Result<HostResponseValue, HostRequestError> {
        match &request.operation {
            HostOperation::RegisterTreeProvider { view_id } => {
                let view = self
                    .tree_views
                    .get(view_id)
                    .cloned()
                    .ok_or(HostRequestError::TreeViewNotFound)?;
                let registration = TreeProviderRegistrationId::new(self.next_tree_registration);
                view.update(cx, |tree, cx| {
                    tree.register_provider(
                        view_id,
                        TreeProviderIdentity {
                            extension: request.extension,
                            lifecycle: request.lifecycle,
                            registration,
                        },
                        cx,
                    )
                })
                .map_err(map_tree_error)?;
                self.next_tree_registration = self
                    .next_tree_registration
                    .checked_add(1)
                    .expect("tree registration space exhausted");
                self.tree_registrations.insert(registration, view);
                Ok(HostResponseValue::TreeProviderRegistered { registration })
            }
            HostOperation::InvalidateTreeProvider {
                registration,
                parent_id,
            } => {
                let view = self.owned_tree_view(*registration, request, cx)?;
                view.update(cx, |tree, cx| {
                    tree.invalidate(*registration, parent_id.clone(), cx)
                })
                .map_err(map_tree_error)?;
                Ok(HostResponseValue::TreeProviderInvalidated)
            }
            HostOperation::UnregisterTreeProvider { registration } => {
                let view = self.owned_tree_view(*registration, request, cx)?;
                view.update(cx, |tree, cx| tree.unregister_provider(*registration, cx))
                    .map_err(map_tree_error)?;
                self.tree_registrations.remove(registration);
                Ok(HostResponseValue::TreeProviderUnregistered {
                    registration: *registration,
                })
            }
            HostOperation::RegisterCompletionProvider { label } => {
                let registration = self.completion_providers.register(
                    request.extension,
                    request.lifecycle,
                    label.clone(),
                );
                Ok(HostResponseValue::CompletionProviderRegistered { registration })
            }
            HostOperation::UnregisterCompletionProvider { registration } => {
                if !self.completion_providers.unregister(
                    *registration,
                    request.extension,
                    request.lifecycle,
                ) {
                    return Err(HostRequestError::CompletionProviderNotFound);
                }
                Ok(HostResponseValue::CompletionProviderUnregistered {
                    registration: *registration,
                })
            }
            _ => Err(HostRequestError::UnsupportedOperation),
        }
    }

    fn owned_tree_view(
        &self,
        registration: TreeProviderRegistrationId,
        request: &HostRequest,
        cx: &App,
    ) -> Result<Entity<TreeView>, HostRequestError> {
        let view = self
            .tree_registrations
            .get(&registration)
            .cloned()
            .ok_or(HostRequestError::TreeProviderNotFound)?;
        if !view
            .read(cx)
            .owns_provider(registration, request.extension, request.lifecycle)
        {
            return Err(HostRequestError::TreeProviderNotFound);
        }
        Ok(view)
    }

    pub(crate) fn apply_tree_response(&self, response: TreeChildrenResponse, cx: &mut App) -> bool {
        self.tree_registrations
            .get(&response.registration)
            .is_some_and(|view| view.update(cx, |tree, cx| tree.apply_response(response, cx)))
    }

    pub(crate) fn completion_snapshot(&self) -> Vec<CompletionProviderRegistration> {
        self.completion_providers.snapshot()
    }

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
        self.tree_registrations.retain(|registration, view| {
            !view
                .read(cx)
                .owns_provider(*registration, extension, lifecycle)
        });
        for view in self.tree_views.values() {
            view.update(cx, |tree, cx| {
                tree.remove_lifecycle(extension, lifecycle, cx)
            });
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

fn map_tree_error(error: TreeViewRegistrationError) -> HostRequestError {
    match error {
        TreeViewRegistrationError::WrongView => HostRequestError::TreeViewNotFound,
        TreeViewRegistrationError::ProviderInUse => HostRequestError::TreeProviderInUse,
        TreeViewRegistrationError::ProviderNotFound => HostRequestError::TreeProviderNotFound,
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

    fn request(
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        id: u64,
        operation: HostOperation,
    ) -> HostRequest {
        HostRequest {
            extension,
            lifecycle,
            id: RequestId::new(id),
            invocation: None,
            operation,
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
            bridge.add_tree_view("outline", tree.clone());
            bridge.admit_lifecycle(extension, lifecycle);

            let registered = bridge.dispatch(
                request(
                    extension,
                    lifecycle,
                    1,
                    HostOperation::RegisterTreeProvider {
                        view_id: "outline".into(),
                    },
                ),
                cx,
            );
            let HostResponseValue::TreeProviderRegistered { registration } =
                registered.result.unwrap()
            else {
                panic!("expected tree registration");
            };
            assert!(
                tree.read(cx)
                    .owns_provider(registration, extension, lifecycle)
            );
            assert!(bridge.apply_tree_response(
                TreeChildrenResponse {
                    registration,
                    parent_id: None,
                    generation: 1,
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

            let completion = bridge.dispatch(
                request(
                    extension,
                    lifecycle,
                    2,
                    HostOperation::RegisterCompletionProvider {
                        label: "fixture".into(),
                    },
                ),
                cx,
            );
            let HostResponseValue::CompletionProviderRegistered {
                registration: completion,
            } = completion.result.unwrap()
            else {
                panic!("expected completion registration");
            };
            assert!(bridge.owns_completion_provider(completion, extension, lifecycle));
            assert_eq!(bridge.completion_snapshot()[0].id, completion);

            bridge.remove_lifecycle(extension, lifecycle, cx);
            assert_eq!(tree.read(cx).lifecycle_state(), (false, 0, false, 0));
            assert!(!bridge.owns_completion_provider(completion, extension, lifecycle));
            assert!(!bridge.apply_tree_response(
                TreeChildrenResponse {
                    registration,
                    parent_id: None,
                    generation: 1,
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
            bridge.add_tree_view("outline", tree);
            assert_eq!(
                bridge
                    .dispatch(
                        request(
                            extension,
                            lifecycle,
                            1,
                            HostOperation::RegisterTreeProvider {
                                view_id: "outline".into(),
                            },
                        ),
                        cx,
                    )
                    .result,
                Err(HostRequestError::Cancelled)
            );
            bridge.admit_lifecycle(extension, lifecycle);
            assert_eq!(
                bridge
                    .dispatch(
                        request(
                            extension,
                            lifecycle,
                            2,
                            HostOperation::UnregisterCompletionProvider {
                                registration: CompletionProviderRegistrationId::new(99),
                            },
                        ),
                        cx,
                    )
                    .result,
                Err(HostRequestError::CompletionProviderNotFound)
            );
        });
    }
}
