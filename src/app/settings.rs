//! Application settings: one in-memory store of values that personal config
//! and extensions write and native views read.
//!
//! Every writer shares the same entries, so a later write to a key and scope
//! replaces an earlier one regardless of who made it, and lifecycle teardown
//! removes no values. A scope is a set of facts that must all match the
//! context of a read; resolution picks the matching entry with the most facts
//! and breaks ties by the latest write. When nothing matches, the definition's
//! default applies.
//!
//! A definition gives a key its type, default, and description. Native keys
//! are defined when the store is created; extension definitions belong to the
//! lifecycle that made them. Values written before their key is defined are
//! kept as raw JSON and validated when the definition arrives. An undefined key
//! is inert: its values are kept but resolve to nothing.

use std::collections::HashMap;

use gpui::{App, Global};
use serde_json::Value;

use crate::host::protocol::{ExtensionId, ExtensionLifecycleId};

/// Columns per tab stop in editor views.
pub(crate) const EDITOR_TAB_SIZE: &str = "editor.tabSize";

/// The value type a definition accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingType {
    Boolean,
    Integer,
    Number,
    String,
}

impl SettingType {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "boolean" => Self::Boolean,
            "integer" => Self::Integer,
            "number" => Self::Number,
            "string" => Self::String,
            _ => return None,
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Integer => "integer",
            Self::Number => "number",
            Self::String => "string",
        }
    }

    fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Boolean => value.is_boolean(),
            Self::Integer => value.is_i64(),
            Self::Number => value.is_number(),
            Self::String => value.is_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SettingDefinition {
    pub(crate) key: String,
    pub(crate) value_type: SettingType,
    pub(crate) default: Value,
    pub(crate) description: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingOwner {
    Native,
    Extension {
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    },
}

/// Facts about where a value applies, or about the context reading it. An
/// absent fact in a write's scope matches any context; an absent fact in a
/// read's context matches only scopes that leave it absent too.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SettingsScope {
    /// A document's effective language ID.
    pub(crate) language: Option<String>,
    /// A native surface kind (`editor`, `terminal`) or a contributed view ID.
    pub(crate) view: Option<String>,
}

impl SettingsScope {
    fn fact_count(&self) -> usize {
        usize::from(self.language.is_some()) + usize::from(self.view.is_some())
    }

    fn matches(&self, context: &SettingsScope) -> bool {
        fn fact(scope: &Option<String>, context: &Option<String>) -> bool {
            scope.is_none() || scope == context
        }
        fact(&self.language, &context.language) && fact(&self.view, &context.view)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SettingsError {
    /// The key is already defined by native code or a live lifecycle.
    DuplicateSetting,
    /// A definition's default, or a write to a defined key, has the wrong type.
    TypeMismatch { expected: SettingType },
}

struct Definition {
    definition: SettingDefinition,
    owner: SettingOwner,
}

struct Entry {
    scope: SettingsScope,
    value: Value,
    sequence: u64,
}

pub(crate) struct SettingsStore {
    definitions: HashMap<String, Definition>,
    entries: HashMap<String, Vec<Entry>>,
    next_sequence: u64,
}

impl Default for SettingsStore {
    fn default() -> Self {
        let mut store = Self {
            definitions: HashMap::new(),
            entries: HashMap::new(),
            next_sequence: 0,
        };
        for definition in native_definitions() {
            store
                .define(SettingOwner::Native, definition)
                .expect("native setting definitions are valid and distinct");
        }
        store
    }
}

fn native_definitions() -> Vec<SettingDefinition> {
    vec![SettingDefinition {
        key: EDITOR_TAB_SIZE.into(),
        value_type: SettingType::Integer,
        default: 4.into(),
        description: "Columns per tab stop in editor views".into(),
    }]
}

impl SettingsStore {
    /// Define a key and validate the values already stored for it. Returns the
    /// scopes of values dropped because they do not match the type.
    pub(crate) fn define(
        &mut self,
        owner: SettingOwner,
        definition: SettingDefinition,
    ) -> Result<Vec<SettingsScope>, SettingsError> {
        if self.definitions.contains_key(&definition.key) {
            return Err(SettingsError::DuplicateSetting);
        }
        let value_type = definition.value_type;
        if !value_type.accepts(&definition.default) {
            return Err(SettingsError::TypeMismatch {
                expected: value_type,
            });
        }
        let mut dropped = Vec::new();
        if let Some(entries) = self.entries.get_mut(&definition.key) {
            entries.retain(|entry| {
                let keep = value_type.accepts(&entry.value);
                if !keep {
                    dropped.push(entry.scope.clone());
                }
                keep
            });
        }
        self.definitions
            .insert(definition.key.clone(), Definition { definition, owner });
        Ok(dropped)
    }

    /// Remove every definition one lifecycle made, keeping their values.
    pub(crate) fn remove_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) -> bool {
        let before = self.definitions.len();
        let owner = SettingOwner::Extension {
            extension,
            lifecycle,
        };
        self.definitions
            .retain(|_, definition| definition.owner != owner);
        self.definitions.len() != before
    }

    #[cfg(test)]
    pub(crate) fn definition(&self, key: &str) -> Option<&SettingDefinition> {
        self.definitions
            .get(key)
            .map(|definition| &definition.definition)
    }

    /// Check that `value` fits `key`'s definition, if it has one.
    pub(crate) fn check(&self, key: &str, value: &Value) -> Result<(), SettingsError> {
        match self.definitions.get(key) {
            Some(definition) if !definition.definition.value_type.accepts(value) => {
                Err(SettingsError::TypeMismatch {
                    expected: definition.definition.value_type,
                })
            }
            _ => Ok(()),
        }
    }

    /// Write the value for exactly `key` and `scope`, as the latest write.
    pub(crate) fn set(
        &mut self,
        key: &str,
        value: Value,
        scope: SettingsScope,
    ) -> Result<(), SettingsError> {
        self.check(key, &value)?;
        self.next_sequence += 1;
        let entries = self.entries.entry(key.to_owned()).or_default();
        entries.retain(|entry| entry.scope != scope);
        entries.push(Entry {
            scope,
            value,
            sequence: self.next_sequence,
        });
        Ok(())
    }

    /// Remove the value for exactly `key` and `scope`. Returns false when
    /// there was none.
    pub(crate) fn reset(&mut self, key: &str, scope: &SettingsScope) -> bool {
        let Some(entries) = self.entries.get_mut(key) else {
            return false;
        };
        let before = entries.len();
        entries.retain(|entry| &entry.scope != scope);
        entries.len() != before
    }

    /// The value of a defined `key` for `context`, or its default when no
    /// entry matches. `None` for an undefined key.
    pub(crate) fn get(&self, key: &str, context: &SettingsScope) -> Option<&Value> {
        let definition = &self.definitions.get(key)?.definition;
        let resolved = self
            .entries
            .get(key)
            .into_iter()
            .flatten()
            .filter(|entry| entry.scope.matches(context))
            .max_by_key(|entry| (entry.scope.fact_count(), entry.sequence));
        Some(resolved.map_or(&definition.default, |entry| &entry.value))
    }
}

/// The application's settings store.
#[derive(Default)]
pub(crate) struct ApplicationSettings(pub(crate) SettingsStore);

impl Global for ApplicationSettings {}

pub(crate) fn remove_lifecycle(
    extension: ExtensionId,
    lifecycle: ExtensionLifecycleId,
    cx: &mut App,
) {
    if cx.has_global::<ApplicationSettings>()
        && cx
            .global::<ApplicationSettings>()
            .0
            .definitions
            .values()
            .any(|definition| {
                definition.owner
                    == SettingOwner::Extension {
                        extension,
                        lifecycle,
                    }
            })
    {
        cx.global_mut::<ApplicationSettings>()
            .0
            .remove_lifecycle(extension, lifecycle);
    }
}

/// Columns per tab stop for an editor in `context`, within a sane range.
pub(crate) fn editor_tab_size(context: &SettingsScope, cx: &App) -> usize {
    let size = match cx.try_global::<ApplicationSettings>() {
        Some(settings) => settings
            .0
            .get(EDITOR_TAB_SIZE, context)
            .and_then(Value::as_i64),
        None => SettingsStore::default()
            .get(EDITOR_TAB_SIZE, context)
            .and_then(Value::as_i64),
    };
    size.expect("editor.tabSize is a native integer setting")
        .clamp(1, 32) as usize
}

#[cfg(test)]
mod tests;
