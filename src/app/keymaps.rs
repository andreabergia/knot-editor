//! Application-owned keybinding slots and gpui recognition.

use std::collections::{HashMap, HashSet};

use gpui::{App, DummyKeyboardMapper, Global, KeyBinding, KeyBindingContextPredicate, Keystroke};

use crate::host::protocol::{Command, CommandArgumentValue, ExtensionId, ExtensionLifecycleId};

use super::product_commands::{
    CLOSE_TAB_COMMAND, CLOSE_WINDOW_COMMAND, COPY_COMMAND, CUT_COMMAND, DELETE_BACKWARD_COMMAND,
    DELETE_FORWARD_COMMAND, FIND_COMMAND, FIND_NEXT_COMMAND, FIND_PREVIOUS_COMMAND,
    INSERT_NEWLINE_COMMAND, INSERT_TAB_COMMAND, MOVE_DOCUMENT_END_COMMAND,
    MOVE_DOCUMENT_START_COMMAND, MOVE_DOWN_COMMAND, MOVE_LEFT_COMMAND, MOVE_LINE_END_COMMAND,
    MOVE_LINE_START_COMMAND, MOVE_PAGE_DOWN_COMMAND, MOVE_PAGE_UP_COMMAND, MOVE_RIGHT_COMMAND,
    MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND, MOVE_UP_COMMAND, MOVE_WORD_LEFT_COMMAND,
    MOVE_WORD_RIGHT_COMMAND, NEW_COMMAND, NEW_TERMINAL_COMMAND, NEW_WINDOW_COMMAND, OPEN_COMMAND,
    PASTE_COMMAND, QUIT_COMMAND, REDO_COMMAND, SAVE_AS_COMMAND, SAVE_COMMAND, SELECT_ALL_COMMAND,
    SELECT_DOCUMENT_END_COMMAND, SELECT_DOCUMENT_START_COMMAND, SELECT_DOWN_COMMAND,
    SELECT_LEFT_COMMAND, SELECT_LINE_END_COMMAND, SELECT_LINE_START_COMMAND,
    SELECT_PAGE_DOWN_COMMAND, SELECT_PAGE_UP_COMMAND, SELECT_RIGHT_COMMAND, SELECT_UP_COMMAND,
    SELECT_WORD_LEFT_COMMAND, SELECT_WORD_RIGHT_COMMAND, SHOW_COMMAND_PALETTE_COMMAND,
    SHOW_COMPLETIONS_COMMAND, SPLIT_HORIZONTAL_COMMAND, SPLIT_VERTICAL_COMMAND, UNDO_COMMAND,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum BindingOwner {
    Native,
    Extension(ExtensionId, ExtensionLifecycleId),
    Personal(ExtensionId, ExtensionLifecycleId),
}

impl BindingOwner {
    fn priority(self) -> u8 {
        match self {
            Self::Native => 0,
            Self::Extension(..) => 1,
            Self::Personal(..) => 2,
        }
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

#[derive(Default)]
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
        let view = self.validate_view(view)?;
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
                if effective.global.is_some() {
                    return vec![(key, "product && !palette".to_owned())];
                }
                let mut views = effective.views.keys().collect::<Vec<_>>();
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
                        slot_key.owner.priority(),
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

pub(crate) fn install(cx: &mut App) {
    let mut registry = BindingRegistry::new();
    registry
        .declare_extension_kind("outline")
        .expect("product view kind is valid");
    for &(key, command, view) in DEFAULT_BINDINGS {
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
        "cmd-q",
        super::product_commands::ProductCommandSource::new(QUIT_COMMAND),
        Some("config-error"),
    )]);
}

const DEFAULT_BINDINGS: &[(&str, &str, Option<&str>)] = &[
    ("left", MOVE_LEFT_COMMAND, Some("editor")),
    ("shift-left", SELECT_LEFT_COMMAND, Some("editor")),
    ("right", MOVE_RIGHT_COMMAND, Some("editor")),
    ("shift-right", SELECT_RIGHT_COMMAND, Some("editor")),
    ("up", MOVE_UP_COMMAND, Some("editor")),
    ("shift-up", SELECT_UP_COMMAND, Some("editor")),
    ("down", MOVE_DOWN_COMMAND, Some("editor")),
    ("shift-down", SELECT_DOWN_COMMAND, Some("editor")),
    ("alt-left", MOVE_WORD_LEFT_COMMAND, Some("editor")),
    ("shift-alt-left", SELECT_WORD_LEFT_COMMAND, Some("editor")),
    ("alt-right", MOVE_WORD_RIGHT_COMMAND, Some("editor")),
    ("shift-alt-right", SELECT_WORD_RIGHT_COMMAND, Some("editor")),
    ("cmd-left", MOVE_LINE_START_COMMAND, Some("editor")),
    ("shift-cmd-left", SELECT_LINE_START_COMMAND, Some("editor")),
    ("cmd-right", MOVE_LINE_END_COMMAND, Some("editor")),
    ("shift-cmd-right", SELECT_LINE_END_COMMAND, Some("editor")),
    ("home", MOVE_LINE_START_COMMAND, Some("editor")),
    ("shift-home", SELECT_LINE_START_COMMAND, Some("editor")),
    ("end", MOVE_LINE_END_COMMAND, Some("editor")),
    ("shift-end", SELECT_LINE_END_COMMAND, Some("editor")),
    ("pageup", MOVE_PAGE_UP_COMMAND, Some("editor")),
    ("shift-pageup", SELECT_PAGE_UP_COMMAND, Some("editor")),
    ("pagedown", MOVE_PAGE_DOWN_COMMAND, Some("editor")),
    ("shift-pagedown", SELECT_PAGE_DOWN_COMMAND, Some("editor")),
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
    ("enter", INSERT_NEWLINE_COMMAND, Some("editor")),
    ("tab", INSERT_TAB_COMMAND, Some("editor")),
    ("backspace", DELETE_BACKWARD_COMMAND, Some("editor")),
    ("delete", DELETE_FORWARD_COMMAND, Some("editor")),
    ("ctrl-space", SHOW_COMPLETIONS_COMMAND, Some("editor")),
    ("cmd-n", NEW_COMMAND, None),
    ("cmd-t", NEW_COMMAND, None),
    ("cmd-shift-t", NEW_TERMINAL_COMMAND, None),
    (
        "cmd-ctrl-shift-n",
        MOVE_TERMINAL_TO_NEW_WINDOW_COMMAND,
        None,
    ),
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
    ("cmd-f", FIND_COMMAND, None),
    ("cmd-g", FIND_NEXT_COMMAND, None),
    ("cmd-shift-g", FIND_PREVIOUS_COMMAND, None),
    ("cmd-q", QUIT_COMMAND, None),
];

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{KeyContext, TestAppContext};

    fn command(name: &str) -> Command {
        Command {
            name: name.into(),
            arguments: CommandArgumentValue::Null,
        }
    }

    fn resolved(registry: &BindingRegistry, key: &str, view: Option<&str>) -> Option<String> {
        match registry.resolve(key, view) {
            Some(BindingResolution::Command(command)) => Some(command.name.to_string()),
            Some(BindingResolution::Unbound) => Some("<unbound>".into()),
            None => None,
        }
    }

    #[test]
    fn personal_global_beats_editor_default_and_editor_unbind_leaves_terminal_binding() {
        let mut registry = BindingRegistry::new();
        let personal = BindingOwner::Personal(ExtensionId::new(1), ExtensionLifecycleId::new(1));
        registry
            .set(
                BindingOwner::Native,
                "Cmd-C",
                Some("editor"),
                Some(command("copy")),
            )
            .unwrap();
        registry
            .set(
                BindingOwner::Native,
                "Cmd-C",
                Some("terminal"),
                Some(command("terminal.copy")),
            )
            .unwrap();
        registry
            .set(personal, "Cmd-C", None, Some(command("personal.copy")))
            .unwrap();
        assert_eq!(
            resolved(&registry, "cmd-c", Some("editor")).as_deref(),
            Some("personal.copy")
        );
        registry.remove(personal, "cmd-c", None).unwrap();
        registry
            .set(personal, "cmd-c", Some("editor"), None)
            .unwrap();
        assert_eq!(
            resolved(&registry, "cmd-c", Some("editor")).as_deref(),
            Some("<unbound>")
        );
        assert_eq!(
            resolved(&registry, "cmd-c", Some("terminal")).as_deref(),
            Some("terminal.copy")
        );
    }

    #[test]
    fn owner_replacement_removal_and_lifecycle_cleanup_reveal_lower_sources() {
        let mut registry = BindingRegistry::new();
        let extension = BindingOwner::Extension(ExtensionId::new(2), ExtensionLifecycleId::new(1));
        let personal = BindingOwner::Personal(ExtensionId::new(3), ExtensionLifecycleId::new(1));
        registry
            .set(BindingOwner::Native, "cmd-a", None, Some(command("native")))
            .unwrap();
        registry
            .set(extension, "cmd-a", None, Some(command("first")))
            .unwrap();
        registry
            .set(extension, "cmd-a", None, Some(command("second")))
            .unwrap();
        assert_eq!(
            resolved(&registry, "cmd-a", Some("editor")).as_deref(),
            Some("second")
        );
        registry.set(personal, "cmd-a", None, None).unwrap();
        assert_eq!(
            resolved(&registry, "cmd-a", Some("editor")).as_deref(),
            Some("<unbound>")
        );
        assert!(registry.remove_owner(personal));
        assert_eq!(
            resolved(&registry, "cmd-a", None).as_deref(),
            Some("second")
        );
        assert!(registry.remove_owner(extension));
        assert_eq!(
            resolved(&registry, "cmd-a", None).as_deref(),
            Some("native")
        );
        assert!(!registry.remove_owner(extension));
    }

    #[test]
    fn shadowed_source_changes_update_the_one_active_map_only_when_visible() {
        let mut registry = BindingRegistry::new();
        let extension = BindingOwner::Extension(ExtensionId::new(2), ExtensionLifecycleId::new(1));
        let personal = BindingOwner::Personal(ExtensionId::new(3), ExtensionLifecycleId::new(1));
        assert!(
            registry
                .set(extension, "cmd-a", None, Some(command("extension")))
                .unwrap()
        );
        assert!(
            registry
                .set(personal, "cmd-a", None, Some(command("personal")))
                .unwrap()
        );
        assert!(
            !registry
                .set(extension, "cmd-a", None, Some(command("updated")))
                .unwrap()
        );
        assert_eq!(registry.keys(), vec!["cmd-a"]);
        assert_eq!(
            resolved(&registry, "cmd-a", Some("editor")).as_deref(),
            Some("personal")
        );
        assert!(registry.remove_owner(personal));
        assert_eq!(
            resolved(&registry, "cmd-a", Some("editor")).as_deref(),
            Some("updated")
        );
        assert!(registry.remove_owner(extension));
        assert!(registry.keys().is_empty());
    }

    #[test]
    fn later_extension_and_same_source_conflicts_follow_registration_order() {
        let mut registry = BindingRegistry::new();
        let first = BindingOwner::Extension(ExtensionId::new(1), ExtensionLifecycleId::new(1));
        let second = BindingOwner::Extension(ExtensionId::new(2), ExtensionLifecycleId::new(1));
        registry
            .set(first, "cmd-z", None, Some(command("first")))
            .unwrap();
        registry
            .set(second, "cmd-z", None, Some(command("second")))
            .unwrap();
        assert_eq!(
            resolved(&registry, "cmd-z", Some("editor")).as_deref(),
            Some("second")
        );
        registry
            .set(first, "cmd-z", Some("editor"), Some(command("specific")))
            .unwrap();
        assert_eq!(
            resolved(&registry, "cmd-z", Some("editor")).as_deref(),
            Some("specific")
        );
        registry
            .set(first, "cmd-z", None, Some(command("latest")))
            .unwrap();
        assert_eq!(
            resolved(&registry, "cmd-z", Some("editor")).as_deref(),
            Some("specific")
        );
        assert_eq!(
            resolved(&registry, "cmd-z", Some("terminal")).as_deref(),
            Some("latest")
        );
    }

    #[test]
    fn declared_kinds_and_sequences_are_validated_and_normalized() {
        let mut registry = BindingRegistry::new();
        let owner = BindingOwner::Native;
        assert_eq!(
            registry.set(owner, "", None, Some(command("x"))),
            Err(BindingError::InvalidKey)
        );
        assert_eq!(
            registry.set(owner, "cmd-foo-bar", None, Some(command("x"))),
            Err(BindingError::InvalidKey)
        );
        assert_eq!(
            registry.set(owner, "cmd-a", Some("unknown"), Some(command("x"))),
            Err(BindingError::InvalidView)
        );
        assert_eq!(
            registry.declare_extension_kind("editor"),
            Err(BindingError::InvalidView)
        );
        registry.declare_extension_kind("outline").unwrap();
        registry
            .set(
                owner,
                "Cmd-K Right",
                Some("outline"),
                Some(command("split")),
            )
            .unwrap();
        assert_eq!(registry.keys(), vec!["cmd-k right"]);
        assert_eq!(
            resolved(&registry, "cmd-k right", Some("outline")).as_deref(),
            Some("split")
        );
        assert_eq!(resolved(&registry, "cmd-k right", Some("editor")), None);
    }

    #[test]
    fn bare_uppercase_key_preserves_implicit_shift() {
        let mut registry = BindingRegistry::new();
        registry
            .set(BindingOwner::Native, "A", None, Some(command("shifted")))
            .unwrap();
        assert_eq!(registry.keys(), vec!["shift-a"]);
        assert_eq!(
            resolved(&registry, "shift-a", Some("editor")).as_deref(),
            Some("shifted")
        );
        assert_eq!(resolved(&registry, "a", Some("editor")), None);
    }

    #[test]
    fn gpui_contexts_limit_scoped_prefixes_and_exclude_palette_input() {
        let mut registry = BindingRegistry::new();
        registry.declare_extension_kind("outline").unwrap();
        registry
            .set(
                BindingOwner::Native,
                "cmd-k right",
                Some("editor"),
                Some(command("split")),
            )
            .unwrap();
        registry
            .set(
                BindingOwner::Native,
                "cmd-j",
                Some("outline"),
                Some(command("copy")),
            )
            .unwrap();
        assert_eq!(
            registry.gpui_bindings(),
            vec![
                (
                    "cmd-j".into(),
                    format!("product > {}", view_context("outline"))
                ),
                ("cmd-k right".into(), "product > editor".into()),
            ]
        );
        let personal = BindingOwner::Personal(ExtensionId::new(0), ExtensionLifecycleId::new(1));
        registry
            .set(personal, "cmd-k right", None, Some(command("global")))
            .unwrap();
        assert_eq!(
            registry.gpui_bindings()[1],
            ("cmd-k right".into(), "product && !palette".into())
        );
    }

    #[gpui::test]
    fn rebuilt_map_retains_fixed_config_error_binding(cx: &mut TestAppContext) {
        cx.update(install);
        cx.update(|cx| {
            let owner = BindingOwner::Personal(ExtensionId::new(0), ExtensionLifecycleId::new(1));
            assert!(
                cx.global_mut::<ApplicationKeymaps>()
                    .0
                    .set(owner, "cmd-q", None, None)
                    .unwrap()
            );
            rebuild(cx);
        });
        cx.read(|cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let input = [Keystroke::parse("cmd-q").unwrap()];
            let contexts = [KeyContext::parse("config-error").unwrap()];
            let (bindings, _) = keymap.bindings_for_input(&input, &contexts);
            assert_eq!(bindings.len(), 1);
            assert!(
                bindings[0]
                    .action()
                    .as_any()
                    .is::<super::super::product_commands::ProductCommandSource>()
            );
        });
    }
}
