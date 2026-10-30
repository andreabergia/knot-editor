//! Keymap presets: named sets of default bindings that occupy the preset
//! slot beneath every extension and personal-config binding.
//!
//! The Knot preset is native data, so editing works before packages start and
//! when they fail. Extensions and personal config register further presets by
//! name; the latest registration of a name defines it, and removing it reveals
//! the previous one. The `keymap.preset` setting selects the preset. It is
//! resolved when startup completes and again whenever the setting changes, and
//! an unregistered name selects Knot. Registering or removing a preset later
//! changes nothing until the next selection change.

use std::collections::BTreeMap;

use gpui::{App, Global, Keystroke};
use serde::{Deserialize, Serialize};

use super::{ApplicationKeymaps, canonical_key, rebuild};
use crate::app::commands::dispatch::*;
use crate::app::settings::{self, ApplicationSettings, KEYMAP_PRESET};
use crate::host::protocol::{Command, CommandArgumentValue, ExtensionId, ExtensionLifecycleId};

/// The name of the native preset, which registrations cannot replace.
pub(crate) const KNOT_PRESET: &str = "knot";

/// One preset binding; a missing command unbinds the key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PresetBinding {
    pub(crate) key: String,
    pub(crate) command: Option<Command>,
    #[serde(default)]
    pub(crate) view: Option<String>,
}

impl PresetBinding {
    fn new(key: String, command: Option<&str>, view: Option<&str>) -> Self {
        Self {
            key,
            command: command.map(|name| Command {
                name: name.into(),
                arguments: CommandArgumentValue::Null,
            }),
            view: view.map(ToOwned::to_owned),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct KeymapPreset {
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) bindings: Vec<PresetBinding>,
}

impl KeymapPreset {
    /// Why the definition is invalid, if it is.
    pub(crate) fn invalid_value(&self) -> Option<&'static str> {
        if self.name.is_empty() {
            return Some("keymap preset name must be nonempty");
        }
        if self.name == KNOT_PRESET {
            return Some("the knot keymap preset cannot be replaced");
        }
        if self
            .bindings
            .iter()
            .any(|binding| canonical_key(&binding.key).is_err())
        {
            return Some("keymap preset binding has an invalid key sequence");
        }
        if self.bindings.iter().any(|binding| {
            binding
                .command
                .as_ref()
                .is_some_and(|command| command.name.as_ref().is_empty())
        }) {
            return Some("keymap preset binding command name is empty");
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KeymapPresetInfo {
    pub(crate) name: String,
    pub(crate) title: String,
}

/// The extension lifecycle that owns a registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PresetOwner {
    pub(crate) extension: ExtensionId,
    pub(crate) lifecycle: ExtensionLifecycleId,
}

struct Registration {
    id: u64,
    owner: PresetOwner,
    preset: KeymapPreset,
}

#[derive(Default)]
pub(crate) struct PresetRegistry {
    /// In registration order, so a later index wins.
    registrations: Vec<Registration>,
    next_id: u64,
    /// The selection the installed preset was resolved from.
    applied: Option<String>,
}

impl PresetRegistry {
    /// Add a validated registration, returning its ID.
    pub(crate) fn register(&mut self, owner: PresetOwner, preset: KeymapPreset) -> u64 {
        self.next_id += 1;
        self.registrations.push(Registration {
            id: self.next_id,
            owner,
            preset,
        });
        self.next_id
    }

    /// Remove an owned registration; false when it is not live.
    pub(crate) fn unregister(&mut self, owner: PresetOwner, id: u64) -> bool {
        let before = self.registrations.len();
        self.registrations
            .retain(|registration| registration.id != id || registration.owner != owner);
        self.registrations.len() != before
    }

    pub(crate) fn remove_lifecycle(&mut self, owner: PresetOwner) {
        self.registrations
            .retain(|registration| registration.owner != owner);
    }

    /// The preset `name` selects: the native Knot preset or the latest
    /// registration of the name.
    pub(crate) fn get(&self, name: &str) -> Option<KeymapPreset> {
        if name == KNOT_PRESET {
            return Some(KeymapPreset {
                name: KNOT_PRESET.into(),
                title: "Knot".into(),
                bindings: knot_preset(Platform::current()),
            });
        }
        self.registrations
            .iter()
            .rev()
            .map(|registration| &registration.preset)
            .find(|preset| preset.name == name)
            .cloned()
    }

    /// Knot and each registered name's effective preset, sorted by name.
    pub(crate) fn list(&self) -> Vec<KeymapPresetInfo> {
        let mut presets = BTreeMap::from([(KNOT_PRESET.to_owned(), "Knot".to_owned())]);
        for registration in &self.registrations {
            presets.insert(
                registration.preset.name.clone(),
                registration.preset.title.clone(),
            );
        }
        presets
            .into_iter()
            .map(|(name, title)| KeymapPresetInfo { name, title })
            .collect()
    }

    /// The title of the installed preset.
    pub(crate) fn applied_title(&self) -> String {
        self.applied
            .as_deref()
            .and_then(|name| self.get(name))
            .or_else(|| self.get(KNOT_PRESET))
            .map(|preset| preset.title)
            .unwrap_or_default()
    }

    /// The bindings `selection` names, falling back to Knot.
    pub(super) fn bindings(&self, selection: &str) -> Vec<PresetBinding> {
        self.get(selection)
            .or_else(|| self.get(KNOT_PRESET))
            .map(|preset| preset.bindings)
            .unwrap_or_default()
    }
}

#[derive(Default)]
pub(crate) struct ApplicationKeymapPresets(pub(crate) PresetRegistry);

impl Global for ApplicationKeymapPresets {}

/// Resolve and install the selected preset. Startup calls this once every
/// preset is registered, and it starts following the selection setting.
pub(crate) fn apply_at_startup(cx: &mut App) {
    apply(cx, true);
    cx.observe_global::<ApplicationSettings>(|cx| apply(cx, false))
        .detach();
}

fn apply(cx: &mut App, force: bool) {
    let selection = settings::text(KEYMAP_PRESET, cx);
    let presets = &mut cx.default_global::<ApplicationKeymapPresets>().0;
    if !force && presets.applied.as_ref() == Some(&selection) {
        return;
    }
    let bindings = presets.bindings(&selection);
    presets.applied = Some(selection);
    if cx.has_global::<ApplicationKeymaps>()
        && cx
            .global_mut::<ApplicationKeymaps>()
            .0
            .replace_preset(&bindings)
    {
        rebuild(cx);
    }
}

pub(crate) fn remove_lifecycle(
    extension: ExtensionId,
    lifecycle: ExtensionLifecycleId,
    cx: &mut App,
) {
    if cx.has_global::<ApplicationKeymapPresets>() {
        cx.global_mut::<ApplicationKeymapPresets>()
            .0
            .remove_lifecycle(PresetOwner {
                extension,
                lifecycle,
            });
    }
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

/// The Knot preset for a platform. On Windows and Linux, a global binding
/// whose first keystroke is ctrl with a printable character other than
/// backtick is unbound in terminals, so the shell receives readline and
/// job-control keys, and the terminal copies and pastes with ctrl-shift-c and
/// ctrl-shift-v instead. Named keys such as tab and pageup, and ctrl-`, stay
/// global because shells do not use them.
pub(crate) fn knot_preset(platform: Platform) -> Vec<PresetBinding> {
    let (secondary, platform_entries) = match platform {
        Platform::MacOs => ("cmd-", MACOS_BINDINGS),
        Platform::Windows | Platform::Linux => ("ctrl-", PC_BINDINGS),
    };
    let mut preset = SHARED_BINDINGS
        .iter()
        .chain(platform_entries)
        .map(|&(key, command, view)| {
            PresetBinding::new(key.replace("secondary-", secondary), Some(command), view)
        })
        .collect::<Vec<_>>();
    if platform != Platform::MacOs {
        let mut pass_through = preset
            .iter()
            .filter(|binding| binding.view.is_none() && reaches_the_shell(&binding.key))
            .map(|binding| binding.key.clone())
            .collect::<Vec<_>>();
        pass_through.sort();
        pass_through.dedup();
        preset.extend(
            pass_through
                .into_iter()
                .map(|key| PresetBinding::new(key, None, Some("terminal"))),
        );
        preset.extend(
            PC_TERMINAL_BINDINGS
                .iter()
                .map(|&(key, command, view)| PresetBinding::new(key.into(), Some(command), view)),
        );
    }
    preset
}

/// Whether a binding's first keystroke is one a shell reads: plain ctrl with
/// a printable character other than backtick.
fn reaches_the_shell(key: &str) -> bool {
    key.split_whitespace()
        .next()
        .and_then(|stroke| Keystroke::parse(stroke).ok())
        .is_some_and(|stroke| {
            let mut characters = stroke.key.chars();
            stroke.modifiers
                == gpui::Modifiers {
                    control: true,
                    ..Default::default()
                }
                && characters.next().is_some_and(|key| key != '`')
                && characters.next().is_none()
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
    ("ctrl-`", NEW_TERMINAL_COMMAND, None),
    ("ctrl-tab", NEXT_RECENT_TAB_COMMAND, None),
    ("ctrl-shift-tab", PREVIOUS_RECENT_TAB_COMMAND, None),
    ("secondary-shift-n", NEW_WINDOW_COMMAND, None),
    ("secondary-o", OPEN_COMMAND, None),
    ("secondary-s", SAVE_COMMAND, None),
    ("secondary-shift-s", SAVE_AS_COMMAND, None),
    ("secondary-w", CLOSE_TAB_COMMAND, None),
    ("secondary-shift-w", CLOSE_WINDOW_COMMAND, None),
    ("secondary-k right", SPLIT_HORIZONTAL_COMMAND, None),
    ("secondary-k down", SPLIT_VERTICAL_COMMAND, None),
    ("secondary-k left", SPLIT_LEFT_COMMAND, None),
    ("secondary-k up", SPLIT_UP_COMMAND, None),
    ("secondary-k =", GROW_PANE_COMMAND, None),
    ("secondary-k -", SHRINK_PANE_COMMAND, None),
    ("secondary-k 0", EQUALIZE_PANES_COMMAND, None),
    ("secondary-k secondary-left", FOCUS_PANE_LEFT_COMMAND, None),
    (
        "secondary-k secondary-right",
        FOCUS_PANE_RIGHT_COMMAND,
        None,
    ),
    ("secondary-k secondary-up", FOCUS_PANE_UP_COMMAND, None),
    ("secondary-k secondary-down", FOCUS_PANE_DOWN_COMMAND, None),
    ("secondary-k shift-left", MOVE_TAB_LEFT_COMMAND, None),
    ("secondary-k shift-right", MOVE_TAB_RIGHT_COMMAND, None),
    ("secondary-k shift-up", MOVE_TAB_UP_COMMAND, None),
    ("secondary-k shift-down", MOVE_TAB_DOWN_COMMAND, None),
    (
        "secondary-k secondary-m",
        TOGGLE_MAXIMIZE_PANE_COMMAND,
        None,
    ),
    ("secondary-k shift-enter", TOGGLE_PIN_TAB_COMMAND, None),
    ("secondary-k o", CLOSE_OTHER_TABS_COMMAND, None),
    ("secondary-k w", CLOSE_PANE_COMMAND, None),
    ("secondary-shift-p", SHOW_COMMAND_PALETTE_COMMAND, None),
    ("secondary-z", UNDO_COMMAND, None),
    ("secondary-shift-z", REDO_COMMAND, None),
    ("secondary-x", CUT_COMMAND, None),
    ("secondary-c", COPY_COMMAND, None),
    ("secondary-v", PASTE_COMMAND, None),
    ("secondary-a", SELECT_ALL_COMMAND, None),
    ("secondary-q", QUIT_COMMAND, None),
];

/// macOS word, line, and document movement, the Emacs-style keys that every
/// Cocoa text view accepts, and tab navigation.
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
    // macOS reports cmd-shift-] as cmd-}.
    ("cmd-}", NEXT_TAB_COMMAND, None),
    ("cmd-{", PREVIOUS_TAB_COMMAND, None),
    ("cmd-1", GO_TO_TAB_1_COMMAND, None),
    ("cmd-2", GO_TO_TAB_2_COMMAND, None),
    ("cmd-3", GO_TO_TAB_3_COMMAND, None),
    ("cmd-4", GO_TO_TAB_4_COMMAND, None),
    ("cmd-5", GO_TO_TAB_5_COMMAND, None),
    ("cmd-6", GO_TO_TAB_6_COMMAND, None),
    ("cmd-7", GO_TO_TAB_7_COMMAND, None),
    ("cmd-8", GO_TO_TAB_8_COMMAND, None),
    ("cmd-9", GO_TO_LAST_TAB_COMMAND, None),
];

/// Windows and Linux word and document movement, the extra Redo key, and tab
/// navigation keys that shells leave alone.
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
    ("ctrl-pagedown", NEXT_TAB_COMMAND, None),
    ("ctrl-pageup", PREVIOUS_TAB_COMMAND, None),
    ("alt-1", GO_TO_TAB_1_COMMAND, None),
    ("alt-2", GO_TO_TAB_2_COMMAND, None),
    ("alt-3", GO_TO_TAB_3_COMMAND, None),
    ("alt-4", GO_TO_TAB_4_COMMAND, None),
    ("alt-5", GO_TO_TAB_5_COMMAND, None),
    ("alt-6", GO_TO_TAB_6_COMMAND, None),
    ("alt-7", GO_TO_TAB_7_COMMAND, None),
    ("alt-8", GO_TO_TAB_8_COMMAND, None),
    ("alt-9", GO_TO_LAST_TAB_COMMAND, None),
];

const PC_TERMINAL_BINDINGS: &[PresetEntry] = &[
    ("ctrl-shift-c", COPY_COMMAND, Some("terminal")),
    ("ctrl-shift-v", PASTE_COMMAND, Some("terminal")),
];
