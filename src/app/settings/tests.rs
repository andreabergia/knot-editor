use serde_json::json;

use super::*;

const KEY: &str = "test.width";

fn scope(language: Option<&str>, view: Option<&str>) -> SettingsScope {
    SettingsScope {
        language: language.map(Into::into),
        view: view.map(Into::into),
    }
}

fn rust_editor() -> SettingsScope {
    scope(Some("rust"), Some("editor"))
}

fn extension(lifecycle: u64) -> SettingOwner {
    SettingOwner::Extension {
        extension: ExtensionId::new(1),
        lifecycle: ExtensionLifecycleId::new(lifecycle),
    }
}

fn definition(value_type: SettingType, default: Value) -> SettingDefinition {
    SettingDefinition {
        key: KEY.into(),
        value_type,
        default,
        description: "A test width".into(),
    }
}

fn store_with_integer() -> SettingsStore {
    let mut store = SettingsStore::default();
    store
        .define(extension(1), definition(SettingType::Integer, json!(10)))
        .unwrap();
    store
}

#[test]
fn native_definitions_resolve_their_default() {
    let store = SettingsStore::default();
    assert_eq!(store.get(EDITOR_TAB_SIZE, &rust_editor()), Some(&json!(4)));
    assert_eq!(
        store.definition(EDITOR_TAB_SIZE).unwrap().value_type,
        SettingType::Integer
    );
}

#[test]
fn unscoped_value_replaces_the_default() {
    let mut store = store_with_integer();
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(10)));
    store.set(KEY, json!(2), SettingsScope::default()).unwrap();
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(2)));
    assert_eq!(store.get(KEY, &SettingsScope::default()), Some(&json!(2)));
}

#[test]
fn scoped_value_beats_a_later_unscoped_one() {
    let mut store = store_with_integer();
    store.set(KEY, json!(8), scope(Some("rust"), None)).unwrap();
    store.set(KEY, json!(2), SettingsScope::default()).unwrap();
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(8)));
    assert_eq!(
        store.get(KEY, &scope(Some("python"), Some("editor"))),
        Some(&json!(2))
    );
}

#[test]
fn more_facts_beat_fewer() {
    let mut store = store_with_integer();
    store.set(KEY, json!(3), rust_editor()).unwrap();
    store.set(KEY, json!(8), scope(Some("rust"), None)).unwrap();
    store
        .set(KEY, json!(5), scope(None, Some("editor")))
        .unwrap();
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(3)));
    assert_eq!(
        store.get(KEY, &scope(Some("rust"), Some("search.results"))),
        Some(&json!(8))
    );
}

#[test]
fn ties_go_to_the_last_write_and_a_re_set_moves_later() {
    let mut store = store_with_integer();
    store.set(KEY, json!(8), scope(Some("rust"), None)).unwrap();
    store
        .set(KEY, json!(5), scope(None, Some("editor")))
        .unwrap();
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(5)));

    store.set(KEY, json!(9), scope(Some("rust"), None)).unwrap();
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(9)));
}

#[test]
fn reset_reveals_the_next_match() {
    let mut store = store_with_integer();
    store.set(KEY, json!(2), SettingsScope::default()).unwrap();
    store.set(KEY, json!(8), scope(Some("rust"), None)).unwrap();
    assert!(store.reset(KEY, &scope(Some("rust"), None)));
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(2)));
    assert!(!store.reset(KEY, &scope(Some("rust"), None)));
    assert!(store.reset(KEY, &SettingsScope::default()));
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(10)));
}

#[test]
fn mismatched_facts_are_ignored() {
    let mut store = store_with_integer();
    store
        .set(KEY, json!(8), scope(Some("python"), None))
        .unwrap();
    store
        .set(KEY, json!(6), scope(None, Some("terminal")))
        .unwrap();
    store
        .set(KEY, json!(7), scope(Some("rust"), Some("terminal")))
        .unwrap();
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(10)));
    // A context without a language matches no language-scoped value.
    assert_eq!(
        store.get(KEY, &scope(None, Some("editor"))),
        Some(&json!(10))
    );
}

#[test]
fn definition_adopts_pending_values() {
    let mut store = SettingsStore::default();
    store.set(KEY, json!(6), scope(Some("rust"), None)).unwrap();
    let dropped = store
        .define(extension(1), definition(SettingType::Integer, json!(10)))
        .unwrap();
    assert!(dropped.is_empty());
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(6)));
}

#[test]
fn definition_drops_mismatching_pending_values() {
    let mut store = SettingsStore::default();
    store
        .set(KEY, json!("wide"), scope(Some("rust"), None))
        .unwrap();
    store.set(KEY, json!(6), SettingsScope::default()).unwrap();
    let dropped = store
        .define(extension(1), definition(SettingType::Integer, json!(10)))
        .unwrap();
    assert_eq!(dropped, vec![scope(Some("rust"), None)]);
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(6)));
}

#[test]
fn typed_writes_are_checked_after_definition() {
    let mut store = store_with_integer();
    for value in [json!("4"), json!(4.5), json!(true), json!(null)] {
        assert_eq!(
            store.set(KEY, value, SettingsScope::default()),
            Err(SettingsError::TypeMismatch {
                expected: SettingType::Integer
            })
        );
    }
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(10)));

    let mut number = SettingsStore::default();
    number
        .define(extension(1), definition(SettingType::Number, json!(1.5)))
        .unwrap();
    number.set(KEY, json!(2), SettingsScope::default()).unwrap();
    number
        .set(KEY, json!(2.5), SettingsScope::default())
        .unwrap();
    assert_eq!(
        number.set(KEY, json!("2"), SettingsScope::default()),
        Err(SettingsError::TypeMismatch {
            expected: SettingType::Number
        })
    );
}

#[test]
fn definition_rejects_a_mismatching_default() {
    let mut store = SettingsStore::default();
    assert_eq!(
        store.define(extension(1), definition(SettingType::Boolean, json!(1))),
        Err(SettingsError::TypeMismatch {
            expected: SettingType::Boolean
        })
    );
    assert_eq!(
        store.define(extension(1), definition(SettingType::String, json!(true))),
        Err(SettingsError::TypeMismatch {
            expected: SettingType::String
        })
    );
    assert!(store.definition(KEY).is_none());
}

#[test]
fn duplicate_definitions_are_rejected() {
    let mut store = store_with_integer();
    assert_eq!(
        store.define(extension(2), definition(SettingType::Integer, json!(1))),
        Err(SettingsError::DuplicateSetting)
    );
    let native = SettingDefinition {
        key: EDITOR_TAB_SIZE.into(),
        ..definition(SettingType::Integer, json!(2))
    };
    assert_eq!(
        store.define(extension(2), native),
        Err(SettingsError::DuplicateSetting)
    );
}

#[test]
fn teardown_removes_definitions_and_keeps_values() {
    let mut store = store_with_integer();
    store.set(KEY, json!(3), SettingsScope::default()).unwrap();
    assert!(!store.remove_lifecycle(ExtensionId::new(1), ExtensionLifecycleId::new(2)));
    assert!(store.remove_lifecycle(ExtensionId::new(1), ExtensionLifecycleId::new(1)));
    assert!(store.definition(KEY).is_none());
    assert_eq!(store.get(KEY, &rust_editor()), None);
    assert!(store.definition(EDITOR_TAB_SIZE).is_some());

    // The restarted lifecycle defines the key again and finds its value.
    store
        .define(extension(2), definition(SettingType::Integer, json!(10)))
        .unwrap();
    assert_eq!(store.get(KEY, &rust_editor()), Some(&json!(3)));
}

#[test]
fn undefined_keys_are_inert() {
    let mut store = SettingsStore::default();
    store
        .set(
            "test.typo",
            json!({ "any": "json" }),
            SettingsScope::default(),
        )
        .unwrap();
    assert_eq!(store.get("test.typo", &SettingsScope::default()), None);
    assert!(store.reset("test.typo", &SettingsScope::default()));
}

#[test]
fn string_maps_accept_only_objects_of_strings() {
    let mut store = SettingsStore::default();
    let scope = SettingsScope::default();
    store
        .set(
            FILES_ASSOCIATIONS,
            json!({ "*.mdx": "markdown" }),
            scope.clone(),
        )
        .unwrap();
    assert_eq!(
        store.get(FILES_ASSOCIATIONS, &rust_editor()),
        Some(&json!({ "*.mdx": "markdown" }))
    );
    for value in [
        json!({ "*.mdx": 1 }),
        json!(["markdown"]),
        json!("markdown"),
    ] {
        assert_eq!(
            store.set(FILES_ASSOCIATIONS, value, scope.clone()),
            Err(SettingsError::TypeMismatch {
                expected: SettingType::StringMap
            })
        );
    }
}
