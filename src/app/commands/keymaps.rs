//! Application-owned keybinding slots and gpui recognition.

use std::collections::{HashMap, HashSet};

use gpui::{App, DummyKeyboardMapper, Global, KeyBinding, KeyBindingContextPredicate, Keystroke};

use crate::host::protocol::{
    Command, CommandArgumentValue, CommandName, ExtensionId, ExtensionLifecycleId,
    KeybindingMutation,
};

use crate::app::commands::dispatch::{
    CLOSE_TAB_COMMAND, CLOSE_WINDOW_COMMAND, COPY_COMMAND, CUT_COMMAND, DELETE_BACKWARD_COMMAND,
    DELETE_FORWARD_COMMAND, DELETE_TO_LINE_END_COMMAND, INSERT_NEWLINE_COMMAND, INSERT_TAB_COMMAND,
    MOVE_DOCUMENT_END_COMMAND, MOVE_DOCUMENT_START_COMMAND, MOVE_DOWN_COMMAND, MOVE_LEFT_COMMAND,
    MOVE_LINE_END_COMMAND, MOVE_LINE_START_COMMAND, MOVE_PAGE_DOWN_COMMAND, MOVE_PAGE_UP_COMMAND,
    MOVE_RIGHT_COMMAND, MOVE_UP_COMMAND, MOVE_WORD_LEFT_COMMAND, MOVE_WORD_RIGHT_COMMAND,
    NEW_COMMAND, NEW_TERMINAL_COMMAND, NEW_WINDOW_COMMAND, OPEN_COMMAND, PASTE_COMMAND,
    QUIT_COMMAND, REDO_COMMAND, SAVE_AS_COMMAND, SAVE_COMMAND, SELECT_ALL_COMMAND,
    SELECT_DOCUMENT_END_COMMAND, SELECT_DOCUMENT_START_COMMAND, SELECT_DOWN_COMMAND,
    SELECT_LEFT_COMMAND, SELECT_LINE_END_COMMAND, SELECT_LINE_START_COMMAND,
    SELECT_PAGE_DOWN_COMMAND, SELECT_PAGE_UP_COMMAND, SELECT_RIGHT_COMMAND, SELECT_UP_COMMAND,
    SELECT_WORD_LEFT_COMMAND, SELECT_WORD_RIGHT_COMMAND, SHOW_COMMAND_PALETTE_COMMAND,
    SHOW_COMPLETIONS_COMMAND, SPLIT_HORIZONTAL_COMMAND, SPLIT_VERTICAL_COMMAND, UNDO_COMMAND,
};

/// The preset's bindings are the defaults beneath every extension binding.
/// Extension bindings, including personal config's, form one stack in which
/// the latest write wins, so removing a lifecycle reveals the previous write.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum BindingOwner {
    Preset,
    Extension(ExtensionId, ExtensionLifecycleId),
}

impl BindingOwner {
    fn is_default(self) -> bool {
        self == Self::Preset
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

    /// The key sequence to show for each argument-less command in a view:
    /// the binding with the fewest keystrokes, then the shortest text, with
    /// remaining ties going to the latest write.
    pub(crate) fn shown_bindings(&self, view: Option<&str>) -> HashMap<CommandName, String> {
        let mut shown: HashMap<CommandName, (usize, usize, std::cmp::Reverse<u64>, &str)> =
            HashMap::new();
        for (key, effective) in &self.active {
            let Some(BindingResolution::Command(command)) = effective.resolve(view) else {
                continue;
            };
            if command.arguments != CommandArgumentValue::Null {
                continue;
            }
            let order = self
                .slots
                .iter()
                .filter(|(slot_key, slot)| {
                    slot_key.key == *key && slot.command.as_ref() == Some(command)
                })
                .map(|(_, slot)| slot.order)
                .max()
                .unwrap_or_default();
            let rank = (
                key.split_whitespace().count(),
                key.len(),
                std::cmp::Reverse(order),
                key.as_str(),
            );
            shown
                .entry(command.name.clone())
                .and_modify(|best| *best = (*best).min(rank))
                .or_insert(rank);
        }
        shown
            .into_iter()
            .map(|(name, (_, _, _, key))| (name, key.to_owned()))
            .collect()
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

/// Render a canonical key sequence for display: `⌘K ⌘S` on macOS and
/// `Ctrl+K Ctrl+S` elsewhere.
pub(crate) fn display_key(key: &str) -> String {
    display_key_for(key, cfg!(target_os = "macos"))
}

fn display_key_for(key: &str, mac: bool) -> String {
    key.split_whitespace()
        .map(|stroke| {
            let Ok(stroke) = Keystroke::parse(stroke) else {
                return stroke.to_owned();
            };
            let modifiers = stroke.modifiers;
            let name = display_key_name(&stroke.key, mac);
            if mac {
                let mut text = String::new();
                for (held, symbol) in [
                    (modifiers.control, "⌃"),
                    (modifiers.alt, "⌥"),
                    (modifiers.shift, "⇧"),
                    (modifiers.platform, "⌘"),
                    (modifiers.function, "fn "),
                ] {
                    if held {
                        text.push_str(symbol);
                    }
                }
                text + &name
            } else {
                let platform = if cfg!(windows) { "Win" } else { "Super" };
                [
                    (modifiers.control, "Ctrl"),
                    (modifiers.alt, "Alt"),
                    (modifiers.shift, "Shift"),
                    (modifiers.platform, platform),
                    (modifiers.function, "Fn"),
                ]
                .into_iter()
                .filter_map(|(held, label)| held.then_some(label))
                .chain([name.as_str()])
                .collect::<Vec<_>>()
                .join("+")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn display_key_name(key: &str, mac: bool) -> String {
    let named = match (key, mac) {
        ("left", true) => "←",
        ("right", true) => "→",
        ("up", true) => "↑",
        ("down", true) => "↓",
        ("enter", true) => "↩",
        ("escape", true) => "⎋",
        ("tab", true) => "⇥",
        ("backspace", true) => "⌫",
        ("delete", true) => "⌦",
        ("left", false) => "Left",
        ("right", false) => "Right",
        ("up", false) => "Up",
        ("down", false) => "Down",
        ("enter", false) => "Enter",
        ("escape", false) => "Esc",
        ("tab", false) => "Tab",
        ("backspace", false) => "Backspace",
        ("delete", false) => "Delete",
        ("space", _) => "Space",
        ("pageup", _) => "PgUp",
        ("pagedown", _) => "PgDn",
        ("home", _) => "Home",
        ("end", _) => "End",
        _ => {
            let mut chars = key.chars();
            return match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            };
        }
    };
    named.to_owned()
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

pub(crate) fn preset_registry(platform: Platform) -> BindingRegistry {
    let mut registry = BindingRegistry::new();
    for (key, command, view) in knot_preset(platform) {
        registry
            .set(
                BindingOwner::Preset,
                &key,
                view,
                command.map(|name| Command {
                    name: name.into(),
                    arguments: CommandArgumentValue::Null,
                }),
            )
            .expect("Knot preset bindings are valid");
    }
    registry
}

pub(crate) fn install(cx: &mut App) {
    let registry = preset_registry(Platform::current());
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
        "secondary-q",
        crate::app::commands::dispatch::ProductCommandSource::new(QUIT_COMMAND),
        Some("config-error"),
    )]);
}

/// The operating system whose conventions a preset follows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Platform {
    MacOs,
    Windows,
    Linux,
}

impl Platform {
    pub(crate) fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// A preset table entry: key, command, and view kind. `secondary-` stands for
/// cmd on macOS and ctrl elsewhere.
type PresetEntry = (&'static str, &'static str, Option<&'static str>);

/// The Knot preset for a platform as key, command, and view kind, where a
/// missing command unbinds the key. On Windows and Linux, a global binding
/// whose first keystroke is a plain ctrl chord is unbound in terminals, so
/// the shell receives readline and job-control keys, and the terminal copies
/// and pastes with ctrl-shift-c and ctrl-shift-v instead.
pub(crate) fn knot_preset(
    platform: Platform,
) -> Vec<(String, Option<&'static str>, Option<&'static str>)> {
    let (secondary, platform_entries) = match platform {
        Platform::MacOs => ("cmd-", MACOS_BINDINGS),
        Platform::Windows | Platform::Linux => ("ctrl-", PC_BINDINGS),
    };
    let mut preset = SHARED_BINDINGS
        .iter()
        .chain(platform_entries)
        .map(|&(key, command, view)| (key.replace("secondary-", secondary), Some(command), view))
        .collect::<Vec<_>>();
    if platform != Platform::MacOs {
        let mut pass_through = preset
            .iter()
            .filter(|(key, _, view)| view.is_none() && starts_with_plain_control(key))
            .map(|(key, _, _)| key.clone())
            .collect::<Vec<_>>();
        pass_through.sort();
        pass_through.dedup();
        preset.extend(
            pass_through
                .into_iter()
                .map(|key| (key, None, Some("terminal"))),
        );
        preset.extend(
            PC_TERMINAL_BINDINGS
                .iter()
                .map(|&(key, command, view)| (key.to_owned(), Some(command), view)),
        );
    }
    preset
}

fn starts_with_plain_control(key: &str) -> bool {
    key.split_whitespace()
        .next()
        .and_then(|stroke| Keystroke::parse(stroke).ok())
        .is_some_and(|stroke| {
            stroke.modifiers
                == gpui::Modifiers {
                    control: true,
                    ..Default::default()
                }
        })
}

const SHARED_BINDINGS: &[PresetEntry] = &[
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
    ("secondary-n", NEW_COMMAND, None),
    ("secondary-shift-t", NEW_TERMINAL_COMMAND, None),
    ("secondary-shift-n", NEW_WINDOW_COMMAND, None),
    ("secondary-o", OPEN_COMMAND, None),
    ("secondary-s", SAVE_COMMAND, None),
    ("secondary-shift-s", SAVE_AS_COMMAND, None),
    ("secondary-w", CLOSE_TAB_COMMAND, None),
    ("secondary-shift-w", CLOSE_WINDOW_COMMAND, None),
    ("secondary-k right", SPLIT_HORIZONTAL_COMMAND, None),
    ("secondary-k down", SPLIT_VERTICAL_COMMAND, None),
    ("secondary-shift-p", SHOW_COMMAND_PALETTE_COMMAND, None),
    ("secondary-z", UNDO_COMMAND, None),
    ("secondary-shift-z", REDO_COMMAND, None),
    ("secondary-x", CUT_COMMAND, None),
    ("secondary-c", COPY_COMMAND, None),
    ("secondary-v", PASTE_COMMAND, None),
    ("secondary-a", SELECT_ALL_COMMAND, None),
    ("secondary-q", QUIT_COMMAND, None),
];

/// macOS word, line, and document movement, plus the Emacs-style keys that
/// every Cocoa text view accepts.
const MACOS_BINDINGS: &[PresetEntry] = &[
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
    ("ctrl-a", MOVE_LINE_START_COMMAND, Some("editor")),
    ("ctrl-e", MOVE_LINE_END_COMMAND, Some("editor")),
    ("ctrl-f", MOVE_RIGHT_COMMAND, Some("editor")),
    ("ctrl-b", MOVE_LEFT_COMMAND, Some("editor")),
    ("ctrl-n", MOVE_DOWN_COMMAND, Some("editor")),
    ("ctrl-p", MOVE_UP_COMMAND, Some("editor")),
    ("ctrl-d", DELETE_FORWARD_COMMAND, Some("editor")),
    ("ctrl-h", DELETE_BACKWARD_COMMAND, Some("editor")),
    ("ctrl-k", DELETE_TO_LINE_END_COMMAND, Some("editor")),
];

/// Windows and Linux word and document movement and the extra Redo key.
const PC_BINDINGS: &[PresetEntry] = &[
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
    ("ctrl-y", REDO_COMMAND, None),
];

const PC_TERMINAL_BINDINGS: &[PresetEntry] = &[
    ("ctrl-shift-c", COPY_COMMAND, Some("terminal")),
    ("ctrl-shift-v", PASTE_COMMAND, Some("terminal")),
];

#[cfg(test)]
mod tests;
