//! Application-owned keybinding slots and gpui recognition.

use std::collections::{HashMap, HashSet};

use gpui::{App, DummyKeyboardMapper, Global, KeyBinding, KeyBindingContextPredicate, Keystroke};

use crate::host::protocol::{
    Command, CommandArgumentValue, ExtensionId, ExtensionLifecycleId, KeybindingMutation,
};

use crate::app::commands::dispatch::{
    CLOSE_TAB_COMMAND, CLOSE_WINDOW_COMMAND, COPY_COMMAND, CUT_COMMAND, DELETE_BACKWARD_COMMAND,
    DELETE_FORWARD_COMMAND, INSERT_NEWLINE_COMMAND, INSERT_TAB_COMMAND, MOVE_DOCUMENT_END_COMMAND,
    MOVE_DOCUMENT_START_COMMAND, MOVE_DOWN_COMMAND, MOVE_LEFT_COMMAND, MOVE_LINE_END_COMMAND,
    MOVE_LINE_START_COMMAND, MOVE_PAGE_DOWN_COMMAND, MOVE_PAGE_UP_COMMAND, MOVE_RIGHT_COMMAND,
    MOVE_UP_COMMAND, MOVE_WORD_LEFT_COMMAND, MOVE_WORD_RIGHT_COMMAND, NEW_COMMAND,
    NEW_TERMINAL_COMMAND, NEW_WINDOW_COMMAND, OPEN_COMMAND, PASTE_COMMAND, QUIT_COMMAND,
    REDO_COMMAND, SAVE_AS_COMMAND, SAVE_COMMAND, SELECT_ALL_COMMAND, SELECT_DOCUMENT_END_COMMAND,
    SELECT_DOCUMENT_START_COMMAND, SELECT_DOWN_COMMAND, SELECT_LEFT_COMMAND,
    SELECT_LINE_END_COMMAND, SELECT_LINE_START_COMMAND, SELECT_PAGE_DOWN_COMMAND,
    SELECT_PAGE_UP_COMMAND, SELECT_RIGHT_COMMAND, SELECT_UP_COMMAND, SELECT_WORD_LEFT_COMMAND,
    SELECT_WORD_RIGHT_COMMAND, SHOW_COMMAND_PALETTE_COMMAND, SHOW_COMPLETIONS_COMMAND,
    SPLIT_HORIZONTAL_COMMAND, SPLIT_VERTICAL_COMMAND, UNDO_COMMAND,
};

/// Native bindings are the defaults beneath every extension binding.
/// Extension bindings, including personal config's, form one stack in which
/// the latest write wins, so removing a lifecycle reveals the previous write.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum BindingOwner {
    Native,
    Extension(ExtensionId, ExtensionLifecycleId),
}

impl BindingOwner {
    fn is_default(self) -> bool {
        self == Self::Native
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SlotKey {
    owner: BindingOwner,
    key: String,
    view: Option<String>,
}

#[derive(Clone, Debug)]
struct Slot {
    command: Option<Command>,
    order: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BindingResolution {
    Command(Command),
    Unbound,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct EffectiveKey {
    global: Option<BindingResolution>,
    views: HashMap<String, BindingResolution>,
}

impl EffectiveKey {
    fn resolve(&self, view: Option<&str>) -> Option<&BindingResolution> {
        view.and_then(|view| self.views.get(view))
            .or(self.global.as_ref())
    }

    fn is_empty(&self) -> bool {
        self.global.is_none() && self.views.is_empty()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum BindingError {
    InvalidKey,
    InvalidView,
}

#[derive(Clone, Default)]
pub(crate) struct BindingRegistry {
    slots: HashMap<SlotKey, Slot>,
    active: HashMap<String, EffectiveKey>,
    kinds: HashSet<String>,
    next_order: u64,
}

impl BindingRegistry {
    pub(crate) fn new() -> Self {
        let mut this = Self::default();
        this.kinds.extend(
            ["editor", "terminal", "workspace-tree"]
                .into_iter()
                .map(str::to_owned),
        );
        this
    }

    pub(crate) fn declare_extension_kind(&mut self, kind: &str) -> Result<(), BindingError> {
        if kind.is_empty()
            || matches!(kind, "editor" | "terminal" | "workspace-tree")
            || kind.chars().any(char::is_whitespace)
        {
            return Err(BindingError::InvalidView);
        }
        self.kinds.insert(kind.to_owned());
        Ok(())
    }

    /// Remove a contributed view kind together with its scoped bindings.
    pub(crate) fn remove_extension_kind(&mut self, kind: &str) -> bool {
        if matches!(kind, "editor" | "terminal" | "workspace-tree") || !self.kinds.remove(kind) {
            return false;
        }
        let keys = self
            .slots
            .keys()
            .filter(|key| key.view.as_deref() == Some(kind))
            .map(|key| key.key.clone())
            .collect::<HashSet<_>>();
        self.slots
            .retain(|key, _| key.view.as_deref() != Some(kind));
        let mut changed = false;
        for key in keys {
            let before = self.active.get(&key).cloned();
            self.refresh_key(&key);
            changed |= before != self.active.get(&key).cloned();
        }
        changed
    }

    pub(crate) fn set(
        &mut self,
        owner: BindingOwner,
        key: &str,
        view: Option<&str>,
        command: Option<Command>,
    ) -> Result<bool, BindingError> {
        let key = canonical_key(key)?;
        let view = self.validate_view(view)?;
        let before = self.active.get(&key).cloned();
        self.next_order += 1;
        self.slots.insert(
            SlotKey {
                owner,
                key: key.clone(),
                view,
            },
            Slot {
                command,
                order: self.next_order,
            },
        );
        self.refresh_key(&key);
        Ok(before != self.active.get(&key).cloned())
    }

    pub(crate) fn remove(
        &mut self,
        owner: BindingOwner,
        key: &str,
        view: Option<&str>,
    ) -> Result<bool, BindingError> {
        let key = canonical_key(key)?;
        // Removal is idempotent: the scoped kind may already be gone when an
        // unload or view disposal commits before the keybinding batch.
        let view = view.map(ToOwned::to_owned);
        let before = self.active.get(&key).cloned();
        self.slots.remove(&SlotKey {
            owner,
            key: key.clone(),
            view,
        });
        self.refresh_key(&key);
        Ok(before != self.active.get(&key).cloned())
    }

    pub(crate) fn remove_owner(&mut self, owner: BindingOwner) -> bool {
        let keys = self
            .slots
            .keys()
            .filter(|key| key.owner == owner)
            .map(|key| key.key.clone())
            .collect::<HashSet<_>>();
        self.slots.retain(|key, _| key.owner != owner);
        let mut changed = false;
        for key in keys {
            let before = self.active.get(&key).cloned();
            self.refresh_key(&key);
            changed |= before != self.active.get(&key).cloned();
        }
        changed
    }

    pub(crate) fn apply_batch(
        &mut self,
        owner: BindingOwner,
        mutations: &[KeybindingMutation],
    ) -> Result<bool, String> {
        let mut candidate = self.clone();
        for (index, mutation) in mutations.iter().enumerate() {
            let result = match mutation {
                KeybindingMutation::Set { key, view, command } => {
                    if command
                        .as_ref()
                        .is_some_and(|command| command.name.as_ref().is_empty())
                    {
                        return Err(format!(
                            "keybinding operation {}: command name is empty",
                            index + 1
                        ));
                    }
                    candidate.set(owner, key, view.as_deref(), command.clone())
                }
                KeybindingMutation::Remove { key, view } => {
                    candidate.remove(owner, key, view.as_deref())
                }
                KeybindingMutation::Invalid(message) => {
                    return Err(format!("keybinding operation {}: {message}", index + 1));
                }
            };
            if let Err(error) = result {
                let message = match error {
                    BindingError::InvalidKey => "invalid key sequence",
                    BindingError::InvalidView => "unknown view kind",
                };
                return Err(format!("keybinding operation {}: {message}", index + 1));
            }
        }
        let changed = self.active != candidate.active;
        *self = candidate;
        Ok(changed)
    }

    /// Reads a canonical key sequence from the merged map.
    pub(crate) fn resolve(&self, key: &str, view: Option<&str>) -> Option<BindingResolution> {
        self.active.get(key)?.resolve(view).cloned()
    }

    pub(crate) fn keys(&self) -> Vec<String> {
        let mut keys = self.active.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        keys
    }

    fn gpui_bindings(&self) -> Vec<(String, String)> {
        self.keys()
            .into_iter()
            .flat_map(|key| {
                let effective = &self.active[&key];
                if matches!(effective.global, Some(BindingResolution::Command(_))) {
                    let mut context = "product && !palette && !transient".to_owned();
                    let mut unbound_views = effective
                        .views
                        .iter()
                        .filter_map(|(view, resolution)| {
                            matches!(resolution, BindingResolution::Unbound).then_some(view)
                        })
                        .collect::<Vec<_>>();
                    unbound_views.sort();
                    for view in unbound_views {
                        context.push_str(&format!(" && !{}", view_context(view)));
                    }
                    return vec![(key, context)];
                }
                let mut views = effective
                    .views
                    .iter()
                    .filter_map(|(view, resolution)| {
                        matches!(resolution, BindingResolution::Command(_)).then_some(view)
                    })
                    .collect::<Vec<_>>();
                views.sort();
                views
                    .into_iter()
                    .map(|view| (key.clone(), format!("product > {}", view_context(view))))
                    .collect()
            })
            .collect()
    }

    fn validate_view(&self, view: Option<&str>) -> Result<Option<String>, BindingError> {
        match view {
            Some(kind) if self.kinds.contains(kind) => Ok(Some(kind.to_owned())),
            Some(_) => Err(BindingError::InvalidView),
            None => Ok(None),
        }
    }

    fn refresh_key(&mut self, key: &str) {
        let candidates = self
            .slots
            .iter()
            .filter(|(slot_key, _)| slot_key.key == key)
            .collect::<Vec<_>>();
        let winner = |view: Option<&str>| {
            candidates
                .iter()
                .filter(|(slot_key, _)| slot_key.view.is_none() || slot_key.view.as_deref() == view)
                .max_by_key(|(slot_key, slot)| {
                    (
                        !slot_key.owner.is_default(),
                        slot_key.view.is_some(),
                        slot.order,
                    )
                })
                .map(|(_, slot)| match &slot.command {
                    Some(command) => BindingResolution::Command(command.clone()),
                    None => BindingResolution::Unbound,
                })
        };
        let global = winner(None);
        let views = self
            .kinds
            .iter()
            .filter_map(|kind| {
                let resolution = winner(Some(kind))?;
                (Some(&resolution) != global.as_ref()).then(|| (kind.clone(), resolution))
            })
            .collect();
        let effective = EffectiveKey { global, views };
        if effective.is_empty() {
            self.active.remove(key);
        } else {
            self.active.insert(key.to_owned(), effective);
        }
    }
}

pub(crate) fn view_context(kind: &str) -> String {
    match kind {
        "editor" => "editor".to_owned(),
        "terminal" => "terminal".to_owned(),
        "workspace-tree" => "workspace_tree".to_owned(),
        _ => {
            let mut context = "knot_view_".to_owned();
            for byte in kind.as_bytes() {
                context.push_str(&format!("{byte:02x}"));
            }
            context
        }
    }
}

fn canonical_key(source: &str) -> Result<String, BindingError> {
    if source.trim().is_empty() {
        return Err(BindingError::InvalidKey);
    }
    let canonical = source
        .split_whitespace()
        .map(|stroke| {
            let stroke = if stroke.contains('-') {
                stroke.to_ascii_lowercase()
            } else {
                stroke.to_owned()
            };
            Keystroke::parse(&stroke)
                .map(|stroke| stroke.unparse())
                .map_err(|_| BindingError::InvalidKey)
        })
        .collect::<Result<Vec<_>, _>>()?
        .join(" ");
    KeyBinding::load(
        &canonical,
        Box::new(KeymapAction {
            key: canonical.clone().into(),
        }),
        Some(
            KeyBindingContextPredicate::parse("product")
                .map_err(|_| BindingError::InvalidKey)?
                .into(),
        ),
        false,
        None,
        &DummyKeyboardMapper,
    )
    .map_err(|_| BindingError::InvalidKey)?;
    Ok(canonical)
}

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = knot, no_json)]
pub(crate) struct KeymapAction {
    pub(crate) key: gpui::SharedString,
}

pub(crate) struct ApplicationKeymaps(pub(crate) BindingRegistry);

impl Global for ApplicationKeymaps {}

fn native_registry() -> BindingRegistry {
    let mut registry = BindingRegistry::new();
    for &(key, command, view) in COMMON_BINDINGS.iter().chain(PLATFORM_BINDINGS) {
        registry
            .set(
                BindingOwner::Native,
                key,
                view,
                Some(Command {
                    name: command.into(),
                    arguments: CommandArgumentValue::Null,
                }),
            )
            .expect("native keybindings are valid");
    }
    registry
}

pub(crate) fn install(cx: &mut App) {
    let registry = native_registry();
    cx.set_global(ApplicationKeymaps(registry));
    rebuild(cx);
}

pub(crate) fn rebuild(cx: &mut App) {
    let bindings = cx.global::<ApplicationKeymaps>().0.gpui_bindings();
    cx.clear_key_bindings();
    cx.bind_keys(bindings.into_iter().map(|(key, context)| {
        KeyBinding::new(
            &key,
            KeymapAction {
                key: key.clone().into(),
            },
            Some(&context),
        )
    }));
    cx.bind_keys([KeyBinding::new(
        if cfg!(windows) { "ctrl-q" } else { "cmd-q" },
        crate::app::commands::dispatch::ProductCommandSource::new(QUIT_COMMAND),
        Some("config-error"),
    )]);
}

const COMMON_BINDINGS: &[(&str, &str, Option<&str>)] = &[
    ("left", MOVE_LEFT_COMMAND, Some("editor")),
    ("shift-left", SELECT_LEFT_COMMAND, Some("editor")),
    ("right", MOVE_RIGHT_COMMAND, Some("editor")),
    ("shift-right", SELECT_RIGHT_COMMAND, Some("editor")),
    ("up", MOVE_UP_COMMAND, Some("editor")),
    ("shift-up", SELECT_UP_COMMAND, Some("editor")),
    ("down", MOVE_DOWN_COMMAND, Some("editor")),
    ("shift-down", SELECT_DOWN_COMMAND, Some("editor")),
    ("home", MOVE_LINE_START_COMMAND, Some("editor")),
    ("shift-home", SELECT_LINE_START_COMMAND, Some("editor")),
    ("end", MOVE_LINE_END_COMMAND, Some("editor")),
    ("shift-end", SELECT_LINE_END_COMMAND, Some("editor")),
    ("pageup", MOVE_PAGE_UP_COMMAND, Some("editor")),
    ("shift-pageup", SELECT_PAGE_UP_COMMAND, Some("editor")),
    ("pagedown", MOVE_PAGE_DOWN_COMMAND, Some("editor")),
    ("shift-pagedown", SELECT_PAGE_DOWN_COMMAND, Some("editor")),
    ("enter", INSERT_NEWLINE_COMMAND, Some("editor")),
    ("tab", INSERT_TAB_COMMAND, Some("editor")),
    ("backspace", DELETE_BACKWARD_COMMAND, Some("editor")),
    ("delete", DELETE_FORWARD_COMMAND, Some("editor")),
    ("ctrl-space", SHOW_COMPLETIONS_COMMAND, Some("editor")),
];

#[cfg(not(windows))]
const PLATFORM_BINDINGS: &[(&str, &str, Option<&str>)] = &[
    ("alt-left", MOVE_WORD_LEFT_COMMAND, Some("editor")),
    ("shift-alt-left", SELECT_WORD_LEFT_COMMAND, Some("editor")),
    ("alt-right", MOVE_WORD_RIGHT_COMMAND, Some("editor")),
    ("shift-alt-right", SELECT_WORD_RIGHT_COMMAND, Some("editor")),
    ("cmd-left", MOVE_LINE_START_COMMAND, Some("editor")),
    ("shift-cmd-left", SELECT_LINE_START_COMMAND, Some("editor")),
    ("cmd-right", MOVE_LINE_END_COMMAND, Some("editor")),
    ("shift-cmd-right", SELECT_LINE_END_COMMAND, Some("editor")),
    ("cmd-up", MOVE_DOCUMENT_START_COMMAND, Some("editor")),
    (
        "shift-cmd-up",
        SELECT_DOCUMENT_START_COMMAND,
        Some("editor"),
    ),
    ("cmd-down", MOVE_DOCUMENT_END_COMMAND, Some("editor")),
    (
        "shift-cmd-down",
        SELECT_DOCUMENT_END_COMMAND,
        Some("editor"),
    ),
    ("cmd-n", NEW_COMMAND, None),
    ("cmd-t", NEW_COMMAND, None),
    ("cmd-shift-t", NEW_TERMINAL_COMMAND, None),
    ("cmd-shift-n", NEW_WINDOW_COMMAND, None),
    ("cmd-o", OPEN_COMMAND, None),
    ("cmd-s", SAVE_COMMAND, None),
    ("cmd-shift-s", SAVE_AS_COMMAND, None),
    ("cmd-w", CLOSE_TAB_COMMAND, None),
    ("cmd-shift-w", CLOSE_WINDOW_COMMAND, None),
    ("cmd-k right", SPLIT_HORIZONTAL_COMMAND, None),
    ("cmd-k down", SPLIT_VERTICAL_COMMAND, None),
    ("cmd-shift-p", SHOW_COMMAND_PALETTE_COMMAND, None),
    ("cmd-z", UNDO_COMMAND, None),
    ("cmd-shift-z", REDO_COMMAND, None),
    ("cmd-x", CUT_COMMAND, None),
    ("cmd-c", COPY_COMMAND, None),
    ("cmd-v", PASTE_COMMAND, None),
    ("cmd-a", SELECT_ALL_COMMAND, None),
    ("cmd-q", QUIT_COMMAND, None),
];

#[cfg(windows)]
const PLATFORM_BINDINGS: &[(&str, &str, Option<&str>)] = &[
    ("ctrl-left", MOVE_WORD_LEFT_COMMAND, Some("editor")),
    ("shift-ctrl-left", SELECT_WORD_LEFT_COMMAND, Some("editor")),
    ("ctrl-right", MOVE_WORD_RIGHT_COMMAND, Some("editor")),
    (
        "shift-ctrl-right",
        SELECT_WORD_RIGHT_COMMAND,
        Some("editor"),
    ),
    ("ctrl-home", MOVE_DOCUMENT_START_COMMAND, Some("editor")),
    (
        "shift-ctrl-home",
        SELECT_DOCUMENT_START_COMMAND,
        Some("editor"),
    ),
    ("ctrl-end", MOVE_DOCUMENT_END_COMMAND, Some("editor")),
    (
        "shift-ctrl-end",
        SELECT_DOCUMENT_END_COMMAND,
        Some("editor"),
    ),
    ("ctrl-n", NEW_COMMAND, None),
    ("ctrl-t", NEW_COMMAND, None),
    ("ctrl-shift-t", NEW_TERMINAL_COMMAND, None),
    ("ctrl-shift-n", NEW_WINDOW_COMMAND, None),
    ("ctrl-o", OPEN_COMMAND, None),
    ("ctrl-s", SAVE_COMMAND, None),
    ("ctrl-shift-s", SAVE_AS_COMMAND, None),
    ("ctrl-w", CLOSE_TAB_COMMAND, None),
    ("ctrl-shift-w", CLOSE_WINDOW_COMMAND, None),
    ("ctrl-k right", SPLIT_HORIZONTAL_COMMAND, None),
    ("ctrl-k down", SPLIT_VERTICAL_COMMAND, None),
    ("ctrl-shift-p", SHOW_COMMAND_PALETTE_COMMAND, None),
    ("ctrl-z", UNDO_COMMAND, None),
    ("ctrl-y", REDO_COMMAND, None),
    ("ctrl-shift-z", REDO_COMMAND, None),
    ("ctrl-x", CUT_COMMAND, None),
    ("ctrl-c", COPY_COMMAND, None),
    ("ctrl-v", PASTE_COMMAND, None),
    ("ctrl-a", SELECT_ALL_COMMAND, None),
    ("ctrl-q", QUIT_COMMAND, None),
];

#[cfg(test)]
mod tests;
