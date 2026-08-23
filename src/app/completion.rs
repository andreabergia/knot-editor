use std::collections::HashMap;

use crate::host::protocol::{
    CompletionProviderRegistrationId, ExtensionId, ExtensionLifecycleId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompletionProviderRegistration {
    pub(crate) id: CompletionProviderRegistrationId,
    pub(crate) extension: ExtensionId,
    pub(crate) lifecycle: ExtensionLifecycleId,
    pub(crate) label: String,
    pub(crate) order: u64,
}

pub(crate) struct CompletionProviderRegistry {
    next_registration: u64,
    registrations: HashMap<CompletionProviderRegistrationId, CompletionProviderRegistration>,
}

impl CompletionProviderRegistry {
    pub(crate) fn new() -> Self {
        Self {
            next_registration: 1,
            registrations: HashMap::new(),
        }
    }

    pub(crate) fn register(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
        label: String,
    ) -> CompletionProviderRegistrationId {
        let order = self.next_registration;
        self.next_registration = self
            .next_registration
            .checked_add(1)
            .expect("completion registration space exhausted");
        let id = CompletionProviderRegistrationId::new(order);
        self.registrations.insert(
            id,
            CompletionProviderRegistration {
                id,
                extension,
                lifecycle,
                label,
                order,
            },
        );
        id
    }

    pub(crate) fn unregister(
        &mut self,
        id: CompletionProviderRegistrationId,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> bool {
        if !self.owns(id, extension, lifecycle) {
            return false;
        }
        self.registrations.remove(&id);
        true
    }

    pub(crate) fn owns(
        &self,
        id: CompletionProviderRegistrationId,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> bool {
        self.registrations.get(&id).is_some_and(|registration| {
            registration.extension == extension && registration.lifecycle == lifecycle
        })
    }

    pub(crate) fn snapshot(&self) -> Vec<CompletionProviderRegistration> {
        let mut registrations = self.registrations.values().cloned().collect::<Vec<_>>();
        registrations.sort_by_key(|registration| registration.order);
        registrations
    }

    pub(crate) fn remove_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) {
        self.registrations.retain(|_, registration| {
            registration.extension != extension || registration.lifecycle != lifecycle
        });
    }
}

#[cfg(test)]
mod tests {
    use super::CompletionProviderRegistry;
    use crate::host::protocol::{ExtensionId, ExtensionLifecycleId};

    #[test]
    fn registration_snapshot_preserves_registration_order_and_lifecycle_ownership() {
        let mut registry = CompletionProviderRegistry::new();
        let first = registry.register(
            ExtensionId::new(2),
            ExtensionLifecycleId::new(1),
            "first".into(),
        );
        let second = registry.register(
            ExtensionId::new(1),
            ExtensionLifecycleId::new(3),
            "second".into(),
        );

        assert_eq!(
            registry
                .snapshot()
                .into_iter()
                .map(|registration| registration.id)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert!(!registry.unregister(first, ExtensionId::new(1), ExtensionLifecycleId::new(3)));
        assert!(registry.owns(first, ExtensionId::new(2), ExtensionLifecycleId::new(1)));

        registry.remove_lifecycle(ExtensionId::new(2), ExtensionLifecycleId::new(1));
        assert!(!registry.owns(first, ExtensionId::new(2), ExtensionLifecycleId::new(1)));
    }
}
