use super::*;
use crate::app::commands::dispatch::*;
use gpui::{KeyContext, TestAppContext};

const PLATFORMS: [Platform; 3] = [Platform::MacOs, Platform::Windows, Platform::Linux];

fn preset_registry(platform: Platform) -> BindingRegistry {
    let mut registry = BindingRegistry::new();
    registry.replace_preset(&knot_preset(platform));
    registry
}

#[test]
fn every_preset_binding_is_unique_and_names_a_native_command() {
    let native = inventory::iter::<crate::app::commands::declarations::CommandDescriptor>
        .into_iter()
        .map(|descriptor| descriptor.name)
        .collect::<HashSet<_>>();
    for platform in PLATFORMS {
        let mut seen = HashSet::new();
        for binding in knot_preset(platform) {
            let key = canonical_key(&binding.key)
                .unwrap_or_else(|_| panic!("{platform:?}: invalid key {}", binding.key));
            let view = binding.view;
            assert!(
                seen.insert((key.clone(), view.clone())),
                "{platform:?}: {key} bound twice in {view:?}"
            );
            if let Some(command) = binding.command {
                assert!(
                    native.contains(command.name.as_ref()),
                    "{platform:?}: {key} names unknown command {}",
                    command.name
                );
            }
        }
    }
}

#[test]
fn every_platform_binds_the_pane_layout_chords() {
    for platform in PLATFORMS {
        let registry = preset_registry(platform);
        let secondary = if platform == Platform::MacOs {
            "cmd"
        } else {
            "ctrl"
        };
        for (chord, command_name) in [
            ("left", SPLIT_LEFT_COMMAND),
            ("up", SPLIT_UP_COMMAND),
            ("right", SPLIT_HORIZONTAL_COMMAND),
            ("down", SPLIT_VERTICAL_COMMAND),
            ("=", GROW_PANE_COMMAND),
            ("-", SHRINK_PANE_COMMAND),
            ("0", EQUALIZE_PANES_COMMAND),
        ] {
            let key = format!("{secondary}-k {chord}");
            assert_eq!(
                resolved(&registry, &key, Some("editor")).as_deref(),
                Some(command_name),
                "{platform:?}: {key}"
            );
        }
    }
}

#[test]
fn windows_and_linux_presets_use_control_shortcuts() {
    for platform in [Platform::Windows, Platform::Linux] {
        let registry = preset_registry(platform);
        for (key, command_name) in [
            ("ctrl-shift-p", SHOW_COMMAND_PALETTE_COMMAND),
            ("ctrl-o", OPEN_COMMAND),
            ("ctrl-s", SAVE_COMMAND),
            ("ctrl-shift-s", SAVE_AS_COMMAND),
            ("ctrl-c", COPY_COMMAND),
            ("ctrl-x", CUT_COMMAND),
            ("ctrl-v", PASTE_COMMAND),
            ("ctrl-w", CLOSE_TAB_COMMAND),
            ("ctrl-shift-w", CLOSE_WINDOW_COMMAND),
            ("ctrl-q", QUIT_COMMAND),
            ("ctrl-y", REDO_COMMAND),
            ("ctrl-shift-z", REDO_COMMAND),
            ("ctrl-left", MOVE_WORD_LEFT_COMMAND),
            ("ctrl-right", MOVE_WORD_RIGHT_COMMAND),
            ("ctrl-home", MOVE_DOCUMENT_START_COMMAND),
            ("ctrl-end", MOVE_DOCUMENT_END_COMMAND),
        ] {
            assert_eq!(
                resolved(&registry, key, Some("editor")).as_deref(),
                Some(command_name),
                "{platform:?}: {key}"
            );
        }
        assert!(
            registry.keys().iter().all(|key| !key.contains("cmd-")),
            "{platform:?} binds the platform key"
        );
    }
}

#[test]
fn windows_and_linux_terminals_leave_plain_control_keys_to_the_shell() {
    for platform in [Platform::Windows, Platform::Linux] {
        let registry = preset_registry(platform);
        for key in [
            "ctrl-c",
            "ctrl-v",
            "ctrl-w",
            "ctrl-a",
            "ctrl-z",
            "ctrl-k right",
        ] {
            assert_eq!(
                resolved(&registry, key, Some("terminal")).as_deref(),
                Some("<unbound>"),
                "{platform:?}: {key}"
            );
        }
        assert_eq!(
            resolved(&registry, "ctrl-shift-c", Some("terminal")).as_deref(),
            Some(COPY_COMMAND)
        );
        assert_eq!(
            resolved(&registry, "ctrl-shift-v", Some("terminal")).as_deref(),
            Some(PASTE_COMMAND)
        );
        assert_eq!(
            resolved(&registry, "ctrl-shift-p", Some("terminal")).as_deref(),
            Some(SHOW_COMMAND_PALETTE_COMMAND)
        );
        assert_eq!(resolved(&registry, "ctrl-shift-c", Some("editor")), None);
    }
}

#[test]
fn macos_preset_binds_cocoa_editing_keys_in_the_editor_only() {
    let registry = preset_registry(Platform::MacOs);
    for (key, command_name) in [
        ("cmd-s", SAVE_COMMAND),
        ("cmd-shift-z", REDO_COMMAND),
        ("alt-left", MOVE_WORD_LEFT_COMMAND),
        ("cmd-up", MOVE_DOCUMENT_START_COMMAND),
        ("ctrl-a", MOVE_LINE_START_COMMAND),
        ("ctrl-e", MOVE_LINE_END_COMMAND),
        ("ctrl-f", MOVE_RIGHT_COMMAND),
        ("ctrl-b", MOVE_LEFT_COMMAND),
        ("ctrl-n", MOVE_DOWN_COMMAND),
        ("ctrl-p", MOVE_UP_COMMAND),
        ("ctrl-d", DELETE_FORWARD_COMMAND),
        ("ctrl-h", DELETE_BACKWARD_COMMAND),
        ("ctrl-k", DELETE_TO_LINE_END_COMMAND),
    ] {
        assert_eq!(
            resolved(&registry, key, Some("editor")).as_deref(),
            Some(command_name),
            "{key}"
        );
    }
    assert_eq!(
        resolved(&registry, "cmd-c", Some("terminal")).as_deref(),
        Some(COPY_COMMAND)
    );
    for key in ["ctrl-a", "ctrl-c", "ctrl-k", "ctrl-y"] {
        assert_eq!(resolved(&registry, key, Some("terminal")), None, "{key}");
    }
}

fn command(name: &str) -> Command {
    Command {
        name: name.into(),
        arguments: CommandArgumentValue::Null,
    }
}

fn resolved(registry: &BindingRegistry, key: &str, view: Option<&str>) -> Option<String> {
    match registry.resolve(&canonical_key(key).unwrap(), view) {
        Some(BindingResolution::Command(command)) => Some(command.name.to_string()),
        Some(BindingResolution::Unbound) => Some("<unbound>".into()),
        None => None,
    }
}

#[test]
fn extension_global_beats_editor_default_and_editor_unbind_leaves_terminal_binding() {
    let mut registry = BindingRegistry::new();
    let personal = BindingOwner::Extension(ExtensionId::new(1), ExtensionLifecycleId::new(1));
    registry
        .set(
            BindingOwner::Preset,
            "Cmd-C",
            Some("editor"),
            Some(command("copy")),
        )
        .unwrap();
    registry
        .set(
            BindingOwner::Preset,
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
fn owner_replacement_removal_and_lifecycle_cleanup_reveal_earlier_writes() {
    let mut registry = BindingRegistry::new();
    let extension = BindingOwner::Extension(ExtensionId::new(2), ExtensionLifecycleId::new(1));
    let personal = BindingOwner::Extension(ExtensionId::new(3), ExtensionLifecycleId::new(1));
    registry
        .set(BindingOwner::Preset, "cmd-a", None, Some(command("native")))
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
fn shadowed_writes_update_the_one_active_map_only_when_visible() {
    let mut registry = BindingRegistry::new();
    let extension = BindingOwner::Extension(ExtensionId::new(2), ExtensionLifecycleId::new(1));
    let personal = BindingOwner::Extension(ExtensionId::new(3), ExtensionLifecycleId::new(1));
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
    // A native default beneath extension bindings leaves the active map
    // unchanged.
    assert!(
        !registry
            .set(BindingOwner::Preset, "cmd-a", None, Some(command("native")))
            .unwrap()
    );
    assert_eq!(registry.keys(), vec![canonical_key("cmd-a").unwrap()]);
    // A later write beats an earlier owner's, and removal reveals it again.
    assert!(
        registry
            .set(extension, "cmd-a", None, Some(command("updated")))
            .unwrap()
    );
    assert_eq!(
        resolved(&registry, "cmd-a", Some("editor")).as_deref(),
        Some("updated")
    );
    assert!(registry.remove_owner(extension));
    assert_eq!(
        resolved(&registry, "cmd-a", Some("editor")).as_deref(),
        Some("personal")
    );
    assert!(registry.remove_owner(personal));
    assert_eq!(
        resolved(&registry, "cmd-a", Some("editor")).as_deref(),
        Some("native")
    );
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
    let owner = BindingOwner::Preset;
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
    assert_eq!(
        registry.keys(),
        vec![if cfg!(target_os = "macos") {
            "cmd-k right"
        } else if cfg!(windows) {
            "win-k right"
        } else {
            "super-k right"
        }]
    );
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
        .set(BindingOwner::Preset, "A", None, Some(command("shifted")))
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
            BindingOwner::Preset,
            "cmd-k right",
            Some("editor"),
            Some(command("split")),
        )
        .unwrap();
    registry
        .set(
            BindingOwner::Preset,
            "cmd-j",
            Some("outline"),
            Some(command("copy")),
        )
        .unwrap();
    assert_eq!(
        registry.gpui_bindings(),
        vec![
            (
                canonical_key("cmd-j").unwrap(),
                format!("product > {}", view_context("outline"))
            ),
            (
                canonical_key("cmd-k right").unwrap(),
                "product > editor".into()
            ),
        ]
    );
    let personal = BindingOwner::Extension(ExtensionId::new(0), ExtensionLifecycleId::new(1));
    registry
        .set(personal, "cmd-k right", None, Some(command("global")))
        .unwrap();
    assert_eq!(
        registry.gpui_bindings()[1],
        (
            canonical_key("cmd-k right").unwrap(),
            "product && !palette && !transient".into()
        )
    );
}

#[test]
fn unbound_keys_do_not_install_actions_in_their_scope() {
    let mut registry = BindingRegistry::new();
    let personal = BindingOwner::Extension(ExtensionId::new(0), ExtensionLifecycleId::new(1));
    registry.set(personal, "a", Some("editor"), None).unwrap();
    assert!(registry.gpui_bindings().is_empty());

    registry
        .set(BindingOwner::Preset, "cmd-n", None, Some(command("new")))
        .unwrap();
    registry
        .set(personal, "cmd-n", Some("editor"), None)
        .unwrap();
    assert_eq!(
        registry.gpui_bindings(),
        vec![(
            canonical_key("cmd-n").unwrap(),
            "product && !palette && !transient && !editor".into()
        )]
    );
}

#[gpui::test]
fn rebuilt_map_retains_fixed_config_error_binding(cx: &mut TestAppContext) {
    cx.update(install);
    let quit_key = if cfg!(windows) { "ctrl-q" } else { "cmd-q" };
    cx.update(|cx| {
        let owner = BindingOwner::Extension(ExtensionId::new(0), ExtensionLifecycleId::new(1));
        assert!(
            cx.global_mut::<ApplicationKeymaps>()
                .0
                .set(owner, quit_key, None, None)
                .unwrap()
        );
        rebuild(cx);
    });
    cx.read(|cx| {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let input = [Keystroke::parse(quit_key).unwrap()];
        let contexts = [KeyContext::parse("config-error").unwrap()];
        let (bindings, _) = keymap.bindings_for_input(&input, &contexts);
        assert_eq!(bindings.len(), 1);
        assert!(
            bindings[0]
                .action()
                .as_any()
                .is::<crate::app::commands::dispatch::ProductCommandSource>()
        );
    });
}

#[test]
fn batch_is_ordered_atomic_and_reports_only_effective_changes() {
    let mut registry = BindingRegistry::new();
    let owner = BindingOwner::Extension(ExtensionId::new(0), ExtensionLifecycleId::new(1));
    let set = |name: &str| KeybindingMutation::Set {
        key: "cmd-c".into(),
        view: None,
        command: Some(command(name)),
    };
    assert!(
        registry
            .apply_batch(owner, &[set("first"), set("second")])
            .unwrap()
    );
    assert_eq!(
        resolved(&registry, "cmd-c", None).as_deref(),
        Some("second")
    );
    assert!(
        !registry
            .apply_batch(owner, &[set("temporary"), set("second")])
            .unwrap()
    );
    let error = registry
        .apply_batch(
            owner,
            &[
                set("uncommitted"),
                KeybindingMutation::Set {
                    key: "bad-modifier-key".into(),
                    view: None,
                    command: Some(command("invalid")),
                },
            ],
        )
        .unwrap_err();
    assert!(error.contains("operation 2"));
    assert_eq!(
        resolved(&registry, "cmd-c", None).as_deref(),
        Some("second")
    );
    assert!(
        registry
            .apply_batch(
                owner,
                &[KeybindingMutation::Remove {
                    key: "cmd-c".into(),
                    view: None,
                }],
            )
            .unwrap()
    );
    assert_eq!(resolved(&registry, "cmd-c", None), None);
}

#[test]
fn shown_bindings_prefer_short_view_scoped_and_latest_bindings() {
    let mut registry = BindingRegistry::new();
    let personal = BindingOwner::Extension(ExtensionId::new(1), ExtensionLifecycleId::new(1));
    for (owner, key, view, name) in [
        (BindingOwner::Preset, "cmd-k cmd-s", None, "file.save"),
        (BindingOwner::Preset, "cmd-s", None, "file.save"),
        (BindingOwner::Preset, "cmd-b", None, "first"),
        (BindingOwner::Preset, "cmd-e", None, "first"),
        (BindingOwner::Preset, "cmd-c", None, "copy"),
        (
            BindingOwner::Preset,
            "cmd-c",
            Some("terminal"),
            "terminal.copy",
        ),
        (personal, "cmd-j", Some("editor"), "editor.only"),
    ] {
        registry.set(owner, key, view, Some(command(name))).unwrap();
    }
    registry
        .set(
            personal,
            "cmd-g",
            None,
            Some(Command {
                name: "with.arguments".into(),
                arguments: CommandArgumentValue::String("x".into()),
            }),
        )
        .unwrap();

    let editor = registry.shown_bindings(Some("editor"));
    assert_eq!(editor["file.save"], "cmd-s");
    // Equal length: the later write wins.
    assert_eq!(editor["first"], "cmd-e");
    assert_eq!(editor["copy"], "cmd-c");
    assert_eq!(editor["editor.only"], "cmd-j");
    assert!(!editor.contains_key("with.arguments"));

    let terminal = registry.shown_bindings(Some("terminal"));
    assert_eq!(terminal["terminal.copy"], "cmd-c");
    assert!(!terminal.contains_key("copy"));
    assert!(!terminal.contains_key("editor.only"));
}

#[test]
fn keys_display_in_platform_style() {
    for (key, mac, other) in [
        ("cmd-shift-p", "⇧⌘P", "Shift+Super+P"),
        ("cmd-k right", "⌘K →", "Super+K Right"),
        ("ctrl-alt-enter", "⌃⌥↩", "Ctrl+Alt+Enter"),
        ("ctrl-k ctrl-s", "⌃K ⌃S", "Ctrl+K Ctrl+S"),
        ("f5", "F5", "F5"),
        ("ctrl-space", "⌃Space", "Ctrl+Space"),
    ] {
        let canonical = canonical_key(key).unwrap();
        assert_eq!(display_key_for(&canonical, true), mac, "{key}");
        if !cfg!(windows) {
            assert_eq!(display_key_for(&canonical, false), other, "{key}");
        }
    }
}

fn preset(name: &str, bindings: &[(&str, Option<&str>, Option<&str>)]) -> KeymapPreset {
    KeymapPreset {
        name: name.into(),
        title: name.to_uppercase(),
        bindings: bindings
            .iter()
            .map(|&(key, command, view)| PresetBinding {
                key: key.into(),
                command: command.map(self::command),
                view: view.map(ToOwned::to_owned),
            })
            .collect(),
    }
}

fn preset_owner(lifecycle: u64) -> PresetOwner {
    PresetOwner {
        extension: ExtensionId::new(1),
        lifecycle: ExtensionLifecycleId::new(lifecycle),
    }
}

#[test]
fn latest_preset_registration_wins_and_removal_reveals_the_previous_one() {
    let mut presets = PresetRegistry::default();
    let first = presets.register(
        preset_owner(1),
        preset("emacs", &[("ctrl-x ctrl-s", Some("first"), None)]),
    );
    presets.register(
        preset_owner(2),
        preset("emacs", &[("ctrl-x ctrl-s", Some("second"), None)]),
    );
    presets.register(preset_owner(2), preset("vscode", &[]));
    let command_of = |presets: &PresetRegistry| {
        presets.get("emacs").unwrap().bindings[0]
            .command
            .clone()
            .unwrap()
            .name
            .to_string()
    };
    assert_eq!(command_of(&presets), "second");
    assert_eq!(
        presets
            .list()
            .into_iter()
            .map(|info| (info.name, info.title))
            .collect::<Vec<_>>(),
        [
            ("emacs".to_owned(), "EMACS".to_owned()),
            ("knot".to_owned(), "Knot".to_owned()),
            ("vscode".to_owned(), "VSCODE".to_owned()),
        ]
    );
    presets.remove_lifecycle(preset_owner(2));
    assert_eq!(command_of(&presets), "first");
    assert!(presets.get("vscode").is_none());
    assert!(!presets.unregister(preset_owner(2), first));
    assert!(presets.unregister(preset_owner(1), first));
    assert!(presets.get("emacs").is_none());
}

#[test]
fn knot_is_native_and_the_fallback_for_unknown_selections() {
    let presets = PresetRegistry::default();
    let knot = presets.get(KNOT_PRESET).unwrap();
    assert_eq!(knot.bindings, knot_preset(Platform::current()));
    assert_eq!(presets.bindings("missing"), knot.bindings);
    assert_eq!(
        preset(KNOT_PRESET, &[]).invalid_value(),
        Some("the knot keymap preset cannot be replaced")
    );
    assert!(preset("", &[]).invalid_value().is_some());
    assert!(
        preset("bad", &[("bad-modifier-key", Some("x"), None)])
            .invalid_value()
            .is_some()
    );
    assert!(
        preset("bad", &[("cmd-x", Some(""), None)])
            .invalid_value()
            .is_some()
    );
    assert_eq!(
        preset("good", &[("cmd-x", None, Some("editor"))]).invalid_value(),
        None
    );
}

#[test]
fn replacing_the_preset_keeps_extension_bindings_on_top_and_skips_unknown_views() {
    let mut registry = BindingRegistry::new();
    let personal = BindingOwner::Extension(ExtensionId::new(1), ExtensionLifecycleId::new(1));
    registry.replace_preset(
        &preset(
            "a",
            &[
                ("cmd-s", Some("save"), None),
                ("cmd-j", Some("preset.j"), None),
            ],
        )
        .bindings,
    );
    registry
        .set(personal, "cmd-j", None, Some(command("personal.j")))
        .unwrap();
    assert!(
        registry.replace_preset(
            &preset(
                "b",
                &[
                    ("cmd-r", Some("run"), None),
                    ("cmd-j", Some("other.j"), None),
                    ("cmd-o", Some("outline.open"), Some("example.outline")),
                ],
            )
            .bindings
        )
    );
    assert_eq!(resolved(&registry, "cmd-s", None), None);
    assert_eq!(resolved(&registry, "cmd-r", None).as_deref(), Some("run"));
    assert_eq!(
        resolved(&registry, "cmd-j", None).as_deref(),
        Some("personal.j")
    );
    assert_eq!(resolved(&registry, "cmd-o", Some("example.outline")), None);
    assert!(
        !registry.replace_preset(
            &preset(
                "b",
                &[
                    ("cmd-r", Some("run"), None),
                    ("cmd-j", Some("other.j"), None)
                ],
            )
            .bindings
        )
    );
}

#[test]
fn listing_names_each_slot_and_the_owner_that_overrides_it() {
    let mut registry = BindingRegistry::new();
    let package = BindingOwner::Extension(ExtensionId::new(1), ExtensionLifecycleId::new(1));
    registry.replace_preset(
        &preset(
            "p",
            &[
                ("cmd-s", Some("save"), None),
                ("cmd-c", Some("copy"), None),
                ("cmd-k", Some("kill"), Some("editor")),
            ],
        )
        .bindings,
    );
    registry
        .set(package, "cmd-s", None, Some(command("package.save")))
        .unwrap();
    registry
        .set(package, "cmd-c", Some("terminal"), None)
        .unwrap();
    let rows = registry
        .listing()
        .into_iter()
        .map(|row| {
            (
                row.key,
                row.view,
                row.owner,
                row.command.map(|command| command.name.to_string()),
                row.shadowed_by,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            (
                "cmd-c".into(),
                None,
                BindingOwner::Preset,
                Some("copy".into()),
                None
            ),
            ("cmd-c".into(), Some("terminal".into()), package, None, None),
            (
                "cmd-k".into(),
                Some("editor".into()),
                BindingOwner::Preset,
                Some("kill".into()),
                None
            ),
            (
                "cmd-s".into(),
                None,
                package,
                Some("package.save".into()),
                None
            ),
            (
                "cmd-s".into(),
                None,
                BindingOwner::Preset,
                Some("save".into()),
                Some(package)
            ),
        ]
    );
}

#[gpui::test]
fn menu_shortcuts_follow_the_effective_global_keymap(cx: &mut TestAppContext) {
    fn menu_key(cx: &gpui::App, command: &str) -> Option<String> {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let action = ProductCommandSource {
            name: command.to_owned().into(),
        };
        let keys = keymap
            .bindings_for_action(&action)
            .filter(|binding| {
                binding
                    .predicate()
                    .is_some_and(|predicate| predicate.to_string() == MENU_SHORTCUT_CONTEXT)
            })
            .map(|binding| {
                binding
                    .keystrokes()
                    .iter()
                    .map(|stroke| stroke.unparse())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>();
        assert!(keys.len() <= 1, "{command} has several menu keys: {keys:?}");
        keys.into_iter().next()
    }
    let key = |source: &str| canonical_key(source).unwrap();
    cx.update(install);
    cx.read(|cx| {
        assert_eq!(menu_key(cx, SAVE_COMMAND), Some(key("secondary-s")));
        assert_eq!(menu_key(cx, SPLIT_HORIZONTAL_COMMAND), None);
        assert_eq!(menu_key(cx, MOVE_LEFT_COMMAND), None);
    });

    let personal = BindingOwner::Extension(ExtensionId::new(0), ExtensionLifecycleId::new(1));
    cx.update(|cx| {
        let registry = &mut cx.global_mut::<ApplicationKeymaps>().0;
        registry
            .set(personal, "secondary-s", None, Some(command(NEW_COMMAND)))
            .unwrap();
        registry
            .set(personal, "secondary-o", Some("editor"), None)
            .unwrap();
        rebuild(cx);
    });
    cx.read(|cx| {
        assert_eq!(menu_key(cx, SAVE_COMMAND), None);
        // Equally short keys show the latest write.
        assert_eq!(menu_key(cx, NEW_COMMAND), Some(key("secondary-s")));
        assert_eq!(menu_key(cx, OPEN_COMMAND), None);
    });

    cx.update(|cx| {
        cx.global_mut::<ApplicationKeymaps>().0.replace_preset(
            &preset("other", &[("secondary-g", Some(SAVE_COMMAND), None)]).bindings,
        );
        rebuild(cx);
    });
    cx.read(|cx| {
        assert_eq!(menu_key(cx, SAVE_COMMAND), Some(key("secondary-g")));
        assert_eq!(menu_key(cx, NEW_COMMAND), Some(key("secondary-s")));
    });
}
