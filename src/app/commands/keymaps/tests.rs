use super::*;
use gpui::{KeyContext, TestAppContext};

#[cfg(windows)]
#[test]
fn windows_native_defaults_use_control_shortcuts() {
    let registry = native_registry();
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
    ] {
        assert_eq!(
            resolved(&registry, key, Some("editor")).as_deref(),
            Some(command_name)
        );
    }
    for (key, command_name) in [
        ("ctrl-left", MOVE_WORD_LEFT_COMMAND),
        ("ctrl-right", MOVE_WORD_RIGHT_COMMAND),
        ("ctrl-home", MOVE_DOCUMENT_START_COMMAND),
        ("ctrl-end", MOVE_DOCUMENT_END_COMMAND),
    ] {
        assert_eq!(
            resolved(&registry, key, Some("editor")).as_deref(),
            Some(command_name)
        );
    }
    assert_eq!(resolved(&registry, "win-shift-p", Some("editor")), None);
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
fn owner_replacement_removal_and_lifecycle_cleanup_reveal_earlier_writes() {
    let mut registry = BindingRegistry::new();
    let extension = BindingOwner::Extension(ExtensionId::new(2), ExtensionLifecycleId::new(1));
    let personal = BindingOwner::Extension(ExtensionId::new(3), ExtensionLifecycleId::new(1));
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
            .set(BindingOwner::Native, "cmd-a", None, Some(command("native")))
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
        .set(BindingOwner::Native, "cmd-n", None, Some(command("new")))
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
        (BindingOwner::Native, "cmd-k cmd-s", None, "file.save"),
        (BindingOwner::Native, "cmd-s", None, "file.save"),
        (BindingOwner::Native, "cmd-b", None, "first"),
        (BindingOwner::Native, "cmd-e", None, "first"),
        (BindingOwner::Native, "cmd-c", None, "copy"),
        (
            BindingOwner::Native,
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
