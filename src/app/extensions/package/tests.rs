use super::*;

fn package(root: &Path, directory: &str, manifest: &str) -> PathBuf {
    let path = root.join(directory);
    fs::create_dir_all(path.join("dist")).unwrap();
    fs::write(path.join("knot.jsonc"), manifest).unwrap();
    fs::write(path.join("dist/main.js"), "import './helper.js';").unwrap();
    fs::write(path.join("dist/helper.js"), "export const value = 1;").unwrap();
    path
}

const VALID: &str = r#"{
    // Human-facing metadata is optional.
    "name": "@example/tools",
    "apiVersion": 1, "version": "1.2.3",
    "main": "dist/main.js",
    "requires": ["@example/base"],
    "displayName": "Tools",
    "description": "Editing tools",
    "author": "Example",
    "website": "https://example.com/tools",
    "license": "MIT",
}"#;

#[test]
fn api_version_diagnostics_preserve_identity_before_entry_capture() {
    let root = tempfile::tempdir().unwrap();
    let path = package(root.path(), "@example/tools", VALID);
    // Unsupported admission must win over an unreadable entry.
    fs::write(path.join("dist/main.js"), [0xff]).unwrap();
    for (value, kind) in [
        (None, "missing"),
        (Some(serde_json::Value::Null), "malformed"),
        (Some(serde_json::json!("1")), "malformed"),
        (Some(serde_json::json!(true)), "malformed"),
        (Some(serde_json::json!([])), "malformed"),
        (Some(serde_json::json!({})), "malformed"),
        (Some(serde_json::json!(0)), "malformed"),
        (Some(serde_json::json!(-1)), "malformed"),
        (Some(serde_json::json!(1.5)), "malformed"),
        (Some(serde_json::json!(2)), "unsupported"),
        (Some(serde_json::json!(2.0)), "unsupported"),
        (Some(serde_json::json!(u64::MAX)), "unsupported"),
    ] {
        let mut manifest =
            serde_json::json!({"name":"@example/tools","version":"1.0.0","main":"dist/main.js"});
        if let Some(value) = &value {
            manifest["apiVersion"] = value.clone();
        }
        fs::write(path.join("knot.jsonc"), manifest.to_string()).unwrap();
        let report = discover(root.path());
        assert!(report.packages.is_empty());
        let diagnostic = &report.diagnostics[0];
        assert_eq!(diagnostic.name.as_deref(), Some("@example/tools"));
        assert!(
            diagnostic.cause.contains(&format!("{kind} apiVersion")),
            "{diagnostic:?}"
        );
        assert!(diagnostic.cause.contains("expected"));
        assert!(diagnostic.cause.contains("1"));
        let declared = value
            .as_ref()
            .map(serde_json::Value::to_string)
            .unwrap_or_else(|| "<missing>".into());
        assert!(
            diagnostic.cause.contains(&format!("declared {declared}")),
            "{diagnostic:?}"
        );
        assert!(!diagnostic.cause.contains("main dist/main.js"));
    }
}

#[test]
fn supported_version_accepts_json_integer_spellings_and_retains_other_validation() {
    let root = tempfile::tempdir().unwrap();
    let path = package(root.path(), "@example/tools", VALID);
    for version in ["1", "1.0", "1e0"] {
        let manifest = format!(
            r#"{{"name":"@example/tools","version":"1.0.0","apiVersion":{version},"main":"dist/main.js"}}"#
        );
        fs::write(path.join("knot.jsonc"), manifest).unwrap();
        let report = discover(root.path());
        assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
        assert_eq!(
            report.packages[0].manifest.api_version,
            SUPPORTED_API_VERSION
        );
    }
    fs::write(path.join("knot.jsonc"), r#"{"name":"@example/tools","version":"1.0.0","apiVersion":1,"main":"dist/main.js","unknown":true}"#).unwrap();
    let report = discover(root.path());
    assert_eq!(
        report.diagnostics[0].name.as_deref(),
        Some("@example/tools")
    );
    assert!(report.diagnostics[0].cause.contains("unknown field"));
}

#[test]
fn duplicate_id_with_rejected_version_invalidates_all_peers() {
    for version in [
        None,
        Some(serde_json::json!("1")),
        Some(serde_json::json!(2)),
    ] {
        let root = tempfile::tempdir().unwrap();
        package(root.path(), "@example/tools", VALID);
        let mut manifest =
            serde_json::json!({"name":"@example/tools","version":"1.0.0","main":"dist/main.js"});
        if let Some(version) = version {
            manifest["apiVersion"] = version;
        }
        package(root.path(), "@example/other", &manifest.to_string());
        let report = discover(root.path());
        assert!(report.packages.is_empty());
        assert_eq!(report.diagnostics.len(), 2);
        assert!(
            report
                .diagnostics
                .iter()
                .all(
                    |diagnostic| diagnostic.name.as_deref() == Some("@example/tools")
                        && diagnostic.cause.contains("duplicate package ID")
                )
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.cause.contains("apiVersion"))
        );
    }
}

#[test]
fn discovers_jsonc_and_captures_local_modules() {
    let temp = tempfile::tempdir().unwrap();
    package(temp.path(), "@example/tools", VALID);
    fs::create_dir_all(temp.path().join("ignored/subdirectory")).unwrap();
    let report = discover(temp.path());
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(report.packages.len(), 1);
    let installed = &report.packages[0];
    assert_eq!(installed.manifest.requires, ["@example/base"]);
    assert_eq!(installed.manifest.display_name.as_deref(), Some("Tools"));
    assert_eq!(installed.sources.len(), 2);
    assert_eq!(
        installed.sources["dist/helper.js"].as_ref(),
        "export const value = 1;"
    );
    fs::write(installed.directory.join("dist/helper.js"), "changed").unwrap();
    assert_eq!(
        installed.sources["dist/helper.js"].as_ref(),
        "export const value = 1;"
    );
}

#[test]
fn validates_required_fields_and_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let path = package(temp.path(), "@example/tools", VALID);
    for (manifest, expected) in [
        (
            r#"{"name":"@example/tools","apiVersion":1,"main":"dist/main.js"}"#,
            "version",
        ),
        (
            r#"{"name":"@example/tools","apiVersion": 1, "version":"nope","main":"dist/main.js"}"#,
            "version",
        ),
        (
            r#"{"name":"@example/tools","apiVersion": 1, "version":"1.0.0","main":"../main.js"}"#,
            "main path",
        ),
        (
            r#"{"name":"@example/tools","apiVersion": 1, "version":"1.0.0","main":"dist/main.js","website":"file:///tmp/x"}"#,
            "website",
        ),
        (
            r#"{"name":"@example/tools","apiVersion": 1, "version":"1.0.0","main":"dist/main.js","author":" "}"#,
            "author",
        ),
        (
            r#"{"name":"@knot/tools","apiVersion": 1, "version":"1.0.0","main":"dist/main.js"}"#,
            "reserved",
        ),
        (
            r#"{"name":"@Example/tools","apiVersion": 1, "version":"1.0.0","main":"dist/main.js"}"#,
            "lowercase",
        ),
        ("{ invalid", "invalid knot.jsonc"),
    ] {
        fs::write(path.join("knot.jsonc"), manifest).unwrap();
        let report = discover(temp.path());
        assert!(report.packages.is_empty());
        assert!(
            report.diagnostics[0].cause.contains(expected),
            "{:?}",
            report.diagnostics
        );
    }
}

#[test]
fn reports_name_mismatch_and_duplicate_ids() {
    let temp = tempfile::tempdir().unwrap();
    package(temp.path(), "@example/other", VALID);
    let report = discover(temp.path());
    assert!(report.packages.is_empty());
    assert!(
        report.diagnostics[0]
            .cause
            .contains("directory/name mismatch")
    );

    package(temp.path(), "@example/tools", VALID);
    let report = discover(temp.path());
    assert!(report.packages.is_empty());
    assert_eq!(report.diagnostics.len(), 2);
    assert!(
        report
            .diagnostics
            .iter()
            .all(|item| item.cause.contains("duplicate package ID"))
    );
}

#[test]
fn duplicate_id_invalidates_a_valid_package_even_when_peer_has_an_invalid_main() {
    let temp = tempfile::tempdir().unwrap();
    package(temp.path(), "@example/tools", VALID);
    let invalid = package(temp.path(), "@example/other", VALID);
    fs::remove_file(invalid.join("dist/main.js")).unwrap();
    let report = discover(temp.path());
    assert!(report.packages.is_empty());
    assert_eq!(report.diagnostics.len(), 2);
    assert!(
        report
            .diagnostics
            .iter()
            .all(|item| item.cause.contains("duplicate package ID"))
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|item| item.cause.contains("main dist/main.js"))
    );
}

#[test]
fn accepts_missing_root_and_rejects_missing_main() {
    let temp = tempfile::tempdir().unwrap();
    assert!(discover(&temp.path().join("missing")).packages.is_empty());
    let path = package(temp.path(), "@example/tools", VALID);
    fs::remove_file(path.join("dist/main.js")).unwrap();
    let report = discover(temp.path());
    assert!(report.diagnostics[0].cause.contains("main dist/main.js"));
}

#[test]
fn defaults_optional_fields_and_rejects_duplicate_dependencies() {
    let temp = tempfile::tempdir().unwrap();
    let path = package(
        temp.path(),
        "@example/tools",
        r#"{
        "name": "@example/tools", "apiVersion": 1, "version": "1.0.0", "main": "dist/main.js"
    }"#,
    );
    let report = discover(temp.path());
    assert!(report.diagnostics.is_empty());
    assert!(report.packages[0].manifest.requires.is_empty());
    assert_eq!(report.packages[0].manifest.author, None);
    fs::write(
        path.join("knot.jsonc"),
        r#"{
        "name": "@example/tools", "apiVersion": 1, "version": "1.0.0", "main": "dist/main.js",
        "requires": ["@example/base", "@example/base"]
    }"#,
    )
    .unwrap();
    let report = discover(temp.path());
    assert!(report.diagnostics[0].cause.contains("duplicate requires"));
}

#[test]
fn accepts_mjs_entry_and_local_module() {
    let temp = tempfile::tempdir().unwrap();
    let path = package(
        temp.path(),
        "@example/tools",
        r#"{
        "name": "@example/tools", "apiVersion": 1, "version": "1.0.0", "main": "dist/main.mjs"
    }"#,
    );
    fs::write(path.join("dist/main.mjs"), "import './helper.mjs';").unwrap();
    fs::write(path.join("dist/helper.mjs"), "export {};").unwrap();
    let report = discover(temp.path());
    assert!(report.diagnostics.is_empty());
    assert!(report.packages[0].sources.contains_key("dist/main.mjs"));
    assert!(report.packages[0].sources.contains_key("dist/helper.mjs"));
}

#[cfg(unix)]
#[test]
fn rejects_file_and_directory_symlink_escapes() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("external.js"), "export {};").unwrap();
    let path = package(temp.path(), "@example/tools", VALID);
    fs::remove_file(path.join("dist/main.js")).unwrap();
    symlink(
        outside.path().join("external.js"),
        path.join("dist/main.js"),
    )
    .unwrap();
    let report = discover(temp.path());
    assert!(report.diagnostics[0].cause.contains("path escapes package"));

    fs::remove_file(path.join("dist/main.js")).unwrap();
    fs::write(path.join("dist/main.js"), "export {};").unwrap();
    symlink(outside.path(), path.join("linked")).unwrap();
    let report = discover(temp.path());
    assert!(report.diagnostics[0].cause.contains("symlinked directory"));
}

#[cfg(unix)]
#[test]
fn permits_file_symlinks_within_package_and_rejects_scope_symlinks() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let path = package(temp.path(), "@example/tools", VALID);
    symlink(path.join("dist/helper.js"), path.join("dist/alias.js")).unwrap();
    let report = discover(temp.path());
    assert!(report.diagnostics.is_empty());
    assert_eq!(
        report.packages[0].sources["dist/alias.js"].as_ref(),
        "export const value = 1;"
    );

    symlink(temp.path().join("@example"), temp.path().join("@linked")).unwrap();
    let report = discover(temp.path());
    assert_eq!(report.packages.len(), 1);
    assert!(
        report.diagnostics[0]
            .cause
            .contains("scope directory cannot be a symlink")
    );
}

#[test]
fn bundled_packages_merge_with_installed_discovery() {
    let temp = tempfile::tempdir().unwrap();
    package(temp.path(), "@example/tools", VALID);
    let report = discover_all(temp.path());
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    let bundled = report
        .packages
        .iter()
        .find(|package| {
            package.origin == PackageOrigin::Bundled && package.manifest.name == "@knot/projects"
        })
        .expect("bundled package is discovered");
    assert!(bundled.sources.contains_key("dist/main.js"));
    assert!(bundled.sources.contains_key("dist/switch.js"));
    assert!(
        bundled
            .sources
            .keys()
            .all(|path| path.starts_with("dist/") && path.ends_with(".js"))
    );
    assert_eq!(
        bundled.module_root().unwrap(),
        "file:///knot/bundled/%40knot/projects/"
    );
    assert!(report.packages.iter().any(|package| {
        package.origin == PackageOrigin::Installed && package.manifest.name == "@example/tools"
    }));
}

#[test]
fn installed_packages_cannot_claim_the_bundled_identity() {
    let temp = tempfile::tempdir().unwrap();
    package(
        temp.path(),
        "@knot/projects",
        r#"{"name":"@knot/projects","apiVersion":1,"version":"1.0.0","main":"dist/main.js"}"#,
    );
    let report = discover_all(temp.path());
    let claimed = report
        .packages
        .iter()
        .filter(|package| package.manifest.name == "@knot/projects")
        .collect::<Vec<_>>();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].origin, PackageOrigin::Bundled);
    let conflict = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.cause.contains("@knot/projects"))
        .expect("the rejected directory is reported");
    assert_eq!(conflict.name, None);
    assert!(conflict.cause.contains("reserved"));
    assert!(conflict.cause.contains("bundled package supplies"));
}

#[test]
fn reserved_scope_is_owned_by_bundled_packages_only() {
    assert!(validate_name("@knot/projects").is_err());
    assert!(validate_bundled_name("@knot/projects").is_ok());
    let dependent: Manifest = serde_json::from_str(
        r#"{"name":"@example/tools","apiVersion":1,"version":"1.0.0","main":"main.js","requires":["@knot/projects"]}"#,
    )
    .unwrap();
    assert!(validate_manifest(&dependent, false).is_ok());
    let owned: Manifest = serde_json::from_str(
        r#"{"name":"@knot/tools","apiVersion":1,"version":"1.0.0","main":"main.js"}"#,
    )
    .unwrap();
    assert!(validate_manifest(&owned, false).is_err());
    assert!(validate_manifest(&owned, true).is_ok());
}

#[test]
fn bundled_admission_rejects_missing_sources_and_installed_scope() {
    let sources = BTreeMap::from([("dist/main.js".to_owned(), Arc::from("export {};"))]);
    assert!(
        bundled_package(
            r#"{"name":"@knot/projects","apiVersion":1,"version":"1.0.0","main":"dist/main.js"}"#,
            sources.clone(),
        )
        .is_ok()
    );
    assert!(
        bundled_package(
            r#"{"name":"@knot/projects","apiVersion":1,"version":"1.0.0","main":"missing.js"}"#,
            sources.clone(),
        )
        .unwrap_err()
        .contains("not captured")
    );
    assert!(
        bundled_package(
            r#"{"name":"@example/projects","apiVersion":1,"version":"1.0.0","main":"dist/main.js"}"#,
            sources,
        )
        .unwrap_err()
        .contains("@knot scope")
    );
}
