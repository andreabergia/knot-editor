//! Foreground-authoritative command definitions, ownership, and view-scoped
//! handlers.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use crate::host::protocol::{
    CommandName, CommandRegistrationId, ExtensionId, ExtensionLifecycleId, ViewId,
};

/// The application owner of a discoverable command definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandOwner {
    Native,
    Extension {
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    },
}

/// Foreground-owned metadata for one discoverable command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandDefinition {
    pub name: CommandName,
    pub title: String,
    pub owner: CommandOwner,
}

/// Foreground-authoritative command definitions and their ownership.
pub struct CommandCatalog {
    next_registration: u64,
    by_name: HashMap<CommandName, CommandCatalogEntry>,
    declared_view_kinds: HashSet<String>,
    view_kinds: HashMap<ViewId, String>,
    view_handlers: HashMap<(String, CommandName), CommandTarget>,
    by_view: HashMap<(ViewId, CommandName), CommandTarget>,
    by_id: HashMap<CommandRegistrationId, CommandRegistrationScope>,
}

enum CommandRegistrationScope {
    Global(CommandName),
    View(String, CommandName),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommandCatalogEntry {
    definition: CommandDefinition,
    extension_registration: Option<CommandRegistrationId>,
}

pub(crate) type SharedCommandCatalog = Rc<RefCell<CommandCatalog>>;

/// The extension lifetime authorized to receive a named command invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommandTarget {
    pub registration: CommandRegistrationId,
    pub extension: ExtensionId,
    pub lifecycle: ExtensionLifecycleId,
}

/// The authoritative dispatch target for one globally owned command name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandTargetKind {
    Native,
    Extension(CommandTarget),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandCatalogError {
    NameInUse,
    NotFound,
    ViewNotFound,
}

impl CommandCatalog {
    pub fn new() -> Self {
        Self {
            next_registration: 1,
            by_name: HashMap::new(),
            declared_view_kinds: HashSet::new(),
            view_kinds: HashMap::new(),
            view_handlers: HashMap::new(),
            by_view: HashMap::new(),
            by_id: HashMap::new(),
        }
    }

    pub(crate) fn register_extension(
        &mut self,
        name: CommandName,
        title: String,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> Result<CommandRegistrationId, CommandCatalogError> {
        if self.by_name.contains_key(&name) {
            return Err(CommandCatalogError::NameInUse);
        }
        let id = CommandRegistrationId::new(self.next_registration);
        self.next_registration = self
            .next_registration
            .checked_add(1)
            .expect("command registration space exhausted");
        self.by_name.insert(
            name.clone(),
            CommandCatalogEntry {
                definition: CommandDefinition {
                    name: name.clone(),
                    title,
                    owner: CommandOwner::Extension {
                        extension,
                        lifecycle,
                    },
                },
                extension_registration: Some(id),
            },
        );
        self.by_id
            .insert(id, CommandRegistrationScope::Global(name));
        Ok(id)
    }

    pub(crate) fn declare_view_kind(&mut self, kind: impl Into<String>) {
        self.declared_view_kinds.insert(kind.into());
    }

    /// Remove a contributed view kind while retaining view-command registrations.
    /// A later definition with the same ID rebinds those handlers when its
    /// instances attach.
    pub(crate) fn remove_view_kind(&mut self, kind: &str) {
        self.declared_view_kinds.remove(kind);
        let views = self
            .view_kinds
            .iter()
            .filter_map(|(view, registered)| (registered == kind).then_some(*view))
            .collect::<Vec<_>>();
        for view in views {
            self.view_kinds.remove(&view);
            self.by_view.retain(|(owner, _), _| *owner != view);
        }
    }

    pub(crate) fn bind_view(
        &mut self,
        kind: &str,
        view: ViewId,
    ) -> Result<(), CommandCatalogError> {
        if !self.declared_view_kinds.contains(kind) {
            return Err(CommandCatalogError::ViewNotFound);
        }
        if self.view_kinds.contains_key(&view) {
            return Err(CommandCatalogError::NameInUse);
        }
        self.view_kinds.insert(view, kind.to_owned());
        for ((handler_kind, name), target) in &self.view_handlers {
            if handler_kind == kind {
                self.by_view.insert((view, name.clone()), *target);
            }
        }
        Ok(())
    }

    pub(crate) fn register_view_handler(
        &mut self,
        kind: String,
        name: CommandName,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> Result<CommandRegistrationId, CommandCatalogError> {
        if !self.declared_view_kinds.contains(&kind) {
            return Err(CommandCatalogError::ViewNotFound);
        }
        if !self.by_name.contains_key(&name) {
            return Err(CommandCatalogError::NotFound);
        }
        let key = (kind, name);
        if self.view_handlers.contains_key(&key) {
            return Err(CommandCatalogError::NameInUse);
        }
        let registration = CommandRegistrationId::new(self.next_registration);
        self.next_registration = self
            .next_registration
            .checked_add(1)
            .expect("command registration space exhausted");
        let target = CommandTarget {
            registration,
            extension,
            lifecycle,
        };
        for (view, kind) in &self.view_kinds {
            if kind == &key.0 {
                self.by_view.insert((*view, key.1.clone()), target);
            }
        }
        self.view_handlers.insert(key.clone(), target);
        self.by_id
            .insert(registration, CommandRegistrationScope::View(key.0, key.1));
        Ok(registration)
    }

    pub(crate) fn resolve_view(&self, view: ViewId, name: &str) -> Option<CommandTarget> {
        self.by_view.get(&(view, name.into())).copied()
    }

    pub(crate) fn remove_view(&mut self, view: ViewId) {
        self.view_kinds.remove(&view);
        self.by_view.retain(|(owner, _), _| *owner != view);
    }

    pub(crate) fn register_native(
        &mut self,
        name: CommandName,
        title: String,
    ) -> Result<(), CommandCatalogError> {
        if self.by_name.contains_key(&name) {
            return Err(CommandCatalogError::NameInUse);
        }
        self.by_name.insert(
            name.clone(),
            CommandCatalogEntry {
                definition: CommandDefinition {
                    name,
                    title,
                    owner: CommandOwner::Native,
                },
                extension_registration: None,
            },
        );
        Ok(())
    }

    pub fn definitions(&self) -> impl Iterator<Item = &CommandDefinition> {
        self.by_name.values().map(|entry| &entry.definition)
    }

    pub(crate) fn resolve(&self, name: &str) -> Result<CommandTargetKind, CommandCatalogError> {
        let entry = self
            .by_name
            .get(name)
            .ok_or(CommandCatalogError::NotFound)?;
        match entry.definition.owner {
            CommandOwner::Native => Ok(CommandTargetKind::Native),
            CommandOwner::Extension {
                extension,
                lifecycle,
            } => Ok(CommandTargetKind::Extension(CommandTarget {
                registration: entry
                    .extension_registration
                    .expect("extension definitions have registrations"),
                extension,
                lifecycle,
            })),
        }
    }

    #[cfg(test)]
    pub(crate) fn resolve_extension(
        &self,
        name: &str,
    ) -> Result<CommandTarget, CommandCatalogError> {
        match self.resolve(name)? {
            CommandTargetKind::Native => Err(CommandCatalogError::NotFound),
            CommandTargetKind::Extension(target) => Ok(target),
        }
    }

    pub(crate) fn unregister(
        &mut self,
        id: CommandRegistrationId,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> Result<(), CommandCatalogError> {
        match self.by_id.get(&id).ok_or(CommandCatalogError::NotFound)? {
            CommandRegistrationScope::Global(name) => {
                let entry = self.by_name.get(name).expect("command indexes agree");
                if entry.definition.owner
                    != (CommandOwner::Extension {
                        extension,
                        lifecycle,
                    })
                {
                    return Err(CommandCatalogError::NotFound);
                }
                let name = name.clone();
                self.by_id.remove(&id);
                self.by_name.remove(&name);
                self.remove_view_handlers_for_command(&name);
            }
            CommandRegistrationScope::View(kind, name) => {
                let key = (kind.clone(), name.clone());
                if self.view_handlers.get(&key).is_none_or(|target| {
                    (target.extension, target.lifecycle) != (extension, lifecycle)
                }) {
                    return Err(CommandCatalogError::NotFound);
                }
                self.by_id.remove(&id);
                self.view_handlers.remove(&key);
                self.by_view.retain(|_, target| target.registration != id);
            }
        }
        Ok(())
    }

    pub(crate) fn remove_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) {
        let registrations: Vec<_> = self
            .by_name
            .iter()
            .filter_map(|(name, entry)| {
                if entry.definition.owner
                    != (CommandOwner::Extension {
                        extension,
                        lifecycle,
                    })
                {
                    return None;
                }
                Some((
                    entry
                        .extension_registration
                        .expect("extension definitions have registrations"),
                    name.clone(),
                ))
            })
            .collect();
        for (id, name) in registrations {
            self.by_id.remove(&id);
            self.by_name.remove(&name);
            self.remove_view_handlers_for_command(&name);
        }
        self.view_handlers.retain(|_, target| {
            if (target.extension, target.lifecycle) == (extension, lifecycle) {
                self.by_id.remove(&target.registration);
                false
            } else {
                true
            }
        });
        self.by_view
            .retain(|_, target| (target.extension, target.lifecycle) != (extension, lifecycle));
    }

    fn remove_view_handlers_for_command(&mut self, name: &CommandName) {
        self.view_handlers.retain(|(_, registered_name), target| {
            if registered_name == name {
                self.by_id.remove(&target.registration);
                false
            } else {
                true
            }
        });
        self.by_view
            .retain(|(_, registered_name), _| registered_name != name);
    }
}

impl Default for CommandCatalog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_names_belong_to_one_extension_lifetime() {
        let mut catalog = CommandCatalog::new();
        let extension = ExtensionId::new(7);
        let lifecycle = ExtensionLifecycleId::new(3);
        assert_eq!(
            catalog.resolve_extension("knot.fixture.missing"),
            Err(CommandCatalogError::NotFound)
        );
        let registration = catalog
            .register_extension(
                "knot.fixture.edit".into(),
                "Edit fixture".into(),
                extension,
                lifecycle,
            )
            .unwrap();

        assert_eq!(
            catalog.definitions().cloned().collect::<Vec<_>>(),
            vec![CommandDefinition {
                name: "knot.fixture.edit".into(),
                title: "Edit fixture".into(),
                owner: CommandOwner::Extension {
                    extension,
                    lifecycle,
                },
            }]
        );
        assert_eq!(
            catalog.resolve_extension("knot.fixture.edit").unwrap(),
            CommandTarget {
                registration,
                extension,
                lifecycle,
            }
        );
        assert_eq!(
            catalog.resolve("knot.fixture.edit"),
            Ok(CommandTargetKind::Extension(CommandTarget {
                registration,
                extension,
                lifecycle,
            }))
        );
        assert_eq!(
            catalog.register_extension(
                "knot.fixture.edit".into(),
                "Other edit".into(),
                ExtensionId::new(8),
                ExtensionLifecycleId::new(4),
            ),
            Err(CommandCatalogError::NameInUse)
        );
        assert_eq!(
            catalog.unregister(registration, extension, ExtensionLifecycleId::new(4)),
            Err(CommandCatalogError::NotFound)
        );
        catalog
            .unregister(registration, extension, lifecycle)
            .unwrap();
        assert_eq!(
            catalog.resolve_extension("knot.fixture.edit"),
            Err(CommandCatalogError::NotFound)
        );

        let replacement = catalog
            .register_extension(
                "knot.fixture.edit".into(),
                "Edit fixture".into(),
                extension,
                lifecycle,
            )
            .unwrap();
        catalog.remove_lifecycle(extension, lifecycle);
        assert_eq!(
            catalog.unregister(replacement, extension, lifecycle),
            Err(CommandCatalogError::NotFound)
        );
    }

    #[test]
    fn native_command_names_remain_reserved_across_extension_cleanup() {
        let mut catalog = CommandCatalog::new();
        let extension = ExtensionId::new(7);
        let lifecycle = ExtensionLifecycleId::new(3);

        catalog
            .register_native("fixture.copy".into(), "Copy".into())
            .unwrap();
        assert_eq!(
            catalog.resolve("fixture.copy"),
            Ok(CommandTargetKind::Native)
        );
        assert_eq!(
            catalog.resolve_extension("fixture.copy"),
            Err(CommandCatalogError::NotFound)
        );
        assert_eq!(
            catalog.register_native("fixture.copy".into(), "Other copy".into()),
            Err(CommandCatalogError::NameInUse)
        );
        assert_eq!(
            catalog.register_extension(
                "fixture.copy".into(),
                "Replacement copy".into(),
                extension,
                lifecycle,
            ),
            Err(CommandCatalogError::NameInUse)
        );

        catalog
            .register_extension(
                "example.transform".into(),
                "Transform Selection".into(),
                extension,
                lifecycle,
            )
            .unwrap();
        assert_eq!(
            catalog.register_native("example.transform".into(), "Native transform".into()),
            Err(CommandCatalogError::NameInUse)
        );
        catalog.remove_lifecycle(extension, lifecycle);

        assert_eq!(
            catalog.definitions().cloned().collect::<Vec<_>>(),
            vec![CommandDefinition {
                name: "fixture.copy".into(),
                title: "Copy".into(),
                owner: CommandOwner::Native,
            }]
        );
        assert_eq!(
            catalog.register_extension(
                "fixture.copy".into(),
                "Replacement copy".into(),
                extension,
                ExtensionLifecycleId::new(4),
            ),
            Err(CommandCatalogError::NameInUse)
        );
    }

    #[test]
    fn view_handlers_share_public_names_without_owning_definitions() {
        let mut catalog = CommandCatalog::new();
        let first_view = ViewId::new(11);
        let second_view = ViewId::new(12);
        let extension = ExtensionId::new(7);
        let lifecycle = ExtensionLifecycleId::new(3);
        catalog
            .register_native("copy".into(), "Copy".into())
            .unwrap();
        catalog.declare_view_kind("outline");
        catalog.bind_view("outline", first_view).unwrap();

        assert_eq!(
            catalog.register_view_handler("outline".into(), "missing".into(), extension, lifecycle),
            Err(CommandCatalogError::NotFound)
        );
        let first = catalog
            .register_view_handler("outline".into(), "copy".into(), extension, lifecycle)
            .unwrap();
        catalog.bind_view("outline", second_view).unwrap();
        assert_eq!(
            catalog.register_view_handler("outline".into(), "copy".into(), extension, lifecycle),
            Err(CommandCatalogError::NameInUse)
        );
        assert_eq!(catalog.definitions().count(), 1);
        assert_eq!(catalog.resolve("copy"), Ok(CommandTargetKind::Native));
        assert_eq!(
            catalog
                .resolve_view(first_view, "copy")
                .unwrap()
                .registration,
            first
        );
        assert_eq!(
            catalog
                .resolve_view(second_view, "copy")
                .unwrap()
                .registration,
            first
        );
        assert_eq!(
            catalog.unregister(first, extension, ExtensionLifecycleId::new(4)),
            Err(CommandCatalogError::NotFound)
        );
        catalog.remove_view(first_view);
        assert_eq!(catalog.resolve_view(first_view, "copy"), None);
        assert_eq!(
            catalog
                .resolve_view(second_view, "copy")
                .unwrap()
                .registration,
            first
        );
        assert_eq!(catalog.unregister(first, extension, lifecycle), Ok(()));
        assert_eq!(catalog.resolve_view(second_view, "copy"), None);
        let replacement = catalog
            .register_view_handler("outline".into(), "copy".into(), extension, lifecycle)
            .unwrap();
        catalog.remove_lifecycle(extension, lifecycle);
        assert_eq!(catalog.resolve_view(second_view, "copy"), None);
        assert_eq!(
            catalog.unregister(replacement, extension, lifecycle),
            Err(CommandCatalogError::NotFound)
        );
        assert_eq!(catalog.resolve("copy"), Ok(CommandTargetKind::Native));
    }

    #[test]
    fn command_definitions_describe_native_and_extension_ownership() {
        let native = CommandDefinition {
            name: "fixture.copy".into(),
            title: "Copy".into(),
            owner: CommandOwner::Native,
        };
        let extension = CommandDefinition {
            name: "example.transform".into(),
            title: "Transform Selection".into(),
            owner: CommandOwner::Extension {
                extension: ExtensionId::new(7),
                lifecycle: ExtensionLifecycleId::new(3),
            },
        };

        assert_eq!(native.name.as_ref(), "fixture.copy");
        assert_eq!(native.title, "Copy");
        assert_eq!(native.owner, CommandOwner::Native);
        assert_eq!(extension.name.as_ref(), "example.transform");
        assert_eq!(
            extension.owner,
            CommandOwner::Extension {
                extension: ExtensionId::new(7),
                lifecycle: ExtensionLifecycleId::new(3),
            }
        );
    }
}
