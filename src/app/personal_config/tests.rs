use super::*;

#[cfg(windows)]
#[test]
fn windows_directory_contract() {
    const CHILD: &str = "KNOT_WINDOWS_CONFIG_CONTRACT_CHILD";
    if let Some(case) = std::env::var_os(CHILD) {
        let case = case.to_str().unwrap();
        let base = BaseDirs::new().unwrap();
        let native = ProjectDirs::from("", "", "Knot").unwrap();
        let native_path = base.config_dir().join("Knot").join("config");
        assert_eq!(
            base.config_dir(),
            Path::new(&std::env::var_os("APPDATA").unwrap())
        );
        assert_eq!(native.config_dir(), native_path);
        assert_eq!(
            base.home_dir(),
            Path::new(&std::env::var_os("USERPROFILE").unwrap())
        );
        let selected = user_config_root().unwrap();
        let default_xdg = base.home_dir().join(".config/knot");
        let expected = match case {
            "absolute-existing" | "absolute-invalid" => {
                PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap()).join("knot")
            }
            "unset" | "empty" | "relative" | "absolute-missing" => {
                if default_xdg.exists() {
                    default_xdg
                } else {
                    native_path
                }
            }
            _ => panic!("unknown case: {case}"),
        };
        assert_eq!(selected, expected, "{case}");
        if case == "absolute-invalid" {
            let diagnostic = capture(&selected).unwrap_err();
            assert_eq!(diagnostic.path, selected);
            assert!(diagnostic.cause.contains("must be a directory"));
        }
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let xdg_home = temp.path().join("Windows config ünicode");
    let xdg_root = xdg_home.join("knot");
    fs::create_dir_all(&xdg_home).unwrap();
    for case in [
        "unset",
        "empty",
        "relative",
        "absolute-missing",
        "absolute-existing",
        "absolute-invalid",
    ] {
        if case == "absolute-existing" {
            fs::create_dir(&xdg_root).unwrap();
        } else if case == "absolute-invalid" {
            fs::remove_dir(&xdg_root).unwrap();
            fs::write(&xdg_root, "invalid root").unwrap();
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "app::personal_config::tests::windows_directory_contract",
                "--nocapture",
            ])
            .env(CHILD, case)
            .env_remove("XDG_CONFIG_HOME");
        match case {
            "empty" => {
                child.env("XDG_CONFIG_HOME", "");
            }
            "relative" => {
                child.env("XDG_CONFIG_HOME", "relative");
            }
            "absolute-missing" => {
                child.env("XDG_CONFIG_HOME", temp.path().join("missing"));
            }
            "absolute-existing" | "absolute-invalid" => {
                child.env("XDG_CONFIG_HOME", &xdg_home);
            }
            _ => {}
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "child test did not run for {case}"
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_precedence_and_file_url_capture() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home ünicode");
    let native = temp.path().join("Roaming/Knot/config");
    let default = home.join(".config/knot");
    let xdg_home = temp.path().join("XDG with spaces ünicode");
    let xdg = xdg_home.join("knot");
    fs::create_dir_all(&native).unwrap();
    fs::create_dir_all(&home).unwrap();
    assert_eq!(select_config_root(&native, Some(&home), None), native);
    fs::create_dir_all(&default).unwrap();
    assert_eq!(select_config_root(&native, Some(&home), None), default);
    assert_eq!(
        select_config_root(&native, Some(&home), Some(OsStr::new("relative"))),
        default
    );
    assert_eq!(
        select_config_root(&native, Some(&home), Some(xdg_home.as_os_str())),
        default
    );
    fs::create_dir_all(&xdg).unwrap();
    assert_eq!(
        select_config_root(&native, Some(&home), Some(xdg_home.as_os_str())),
        xdg
    );

    let entry = xdg.join(PRE_INIT);
    fs::write(&entry, "import './lib/shared.mjs';").unwrap();
    fs::create_dir(xdg.join("lib")).unwrap();
    fs::write(xdg.join("lib/shared.mjs"), "export const value = 1;").unwrap();
    let captured = capture(&xdg).unwrap();
    assert!(captured.pre_init && !captured.post_init);
    assert_eq!(
        captured.sources["lib/shared.mjs"].as_ref(),
        "export const value = 1;"
    );
    let file_url = captured
        .clone()
        .into_graph()
        .unwrap()
        .entry_url(PRE_INIT)
        .unwrap()
        .to_owned();
    assert!(file_url.starts_with("file:///"));
    assert!(file_url.contains("%20") && file_url.contains("%C3%BC"));
    assert_eq!(
        fs::canonicalize(url::Url::parse(&file_url).unwrap().to_file_path().unwrap()).unwrap(),
        fs::canonicalize(&entry).unwrap()
    );
    fs::write(xdg.join("lib/shared.mjs"), "changed").unwrap();
    assert_eq!(
        captured.sources["lib/shared.mjs"].as_ref(),
        "export const value = 1;"
    );
    assert_eq!(
        capture(&xdg).unwrap().sources["lib/shared.mjs"].as_ref(),
        "changed"
    );

    fs::write(xdg.join("lib/shared.mjs"), "export const value = 1;").unwrap();
    fs::write(xdg.join(POST_INIT), "import './lib/shared.mjs';").unwrap();
    let both = capture(&xdg).unwrap();
    assert!(both.pre_init && both.post_init);
    assert!(both.post_init_error.is_none());

    fs::write(xdg.join(POST_INIT), "import './missing.mjs';").unwrap();
    let missing = capture(&xdg).unwrap().post_init_error.unwrap();
    assert_eq!(missing.phase, Some(ConfigPhase::PostInit));
    assert_eq!(missing.path, xdg.join(POST_INIT));
    assert!(missing.cause.contains("configuration module not found"));
    fs::write(xdg.join(POST_INIT), "import './lib/bad.mjs';").unwrap();
    fs::write(xdg.join("lib/bad.mjs"), "export const = ;").unwrap();
    let malformed = capture(&xdg).unwrap().post_init_error.unwrap();
    assert_eq!(malformed.path, xdg.join("lib/bad.mjs"));
    fs::write(xdg.join(POST_INIT), "import '../outside.js';").unwrap();
    let escaped = capture(&xdg).unwrap().post_init_error.unwrap();
    assert!(escaped.cause.contains("escapes configuration"));

    fs::remove_dir_all(&xdg).unwrap();
    fs::write(&xdg, "invalid preferred root").unwrap();
    fs::write(default.join(PRE_INIT), "export const lower = true;").unwrap();
    assert_eq!(
        select_config_root(&native, Some(&home), Some(xdg_home.as_os_str())),
        xdg
    );
    assert_eq!(capture(&xdg).unwrap_err().path, xdg);
}

/// Each child reads HOME/XDG through the real directories implementation.
#[cfg(target_os = "linux")]
#[test]
fn linux_environment_contract() {
    const CHILD: &str = "KNOT_CONFIG_CONTRACT_CHILD";
    if let Ok(case) = std::env::var(CHILD) {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let default = home.join(".config/knot");
        let xdg = home.join("xdg/knot");
        assert_eq!(BaseDirs::new().unwrap().home_dir(), home);
        let native = if case.starts_with("absolute") {
            &xdg
        } else {
            &default
        };
        assert_eq!(
            ProjectDirs::from("", "", "Knot").unwrap().config_dir(),
            native
        );
        let selected = user_config_root().unwrap();
        let expected = if case == "absolute-default" {
            &default
        } else {
            native
        };
        assert_eq!(&selected, expected);
        if case == "absolute-invalid" || case == "absolute-dangling" {
            let error = capture(&selected).unwrap_err();
            assert_eq!(error.path, selected);
            return;
        }
        if case == "absolute-escape" {
            let error = capture(&selected).unwrap_err();
            assert_eq!(error.phase, Some(ConfigPhase::PreInit));
            assert!(error.cause.contains("escapes configuration"));
            return;
        }
        let captured = capture(&selected).unwrap();
        match case.as_str() {
            "absolute-import" | "absolute-symlink" => {
                assert!(captured.pre_init && !captured.post_init);
                assert_eq!(captured.sources.len(), 2);
                assert_eq!(captured.sources["helper.mjs"].as_ref(), "export {};");
            }
            "absolute-post" => assert!(!captured.pre_init && captured.post_init),
            _ => assert!(!captured.pre_init && !captured.post_init),
        }
        return;
    }
    for case in [
        "unset",
        "empty",
        "relative",
        "unset-existing",
        "empty-existing",
        "relative-existing",
        "absolute-missing",
        "absolute-default",
        "absolute-both",
        "absolute-invalid",
        "absolute-dangling",
        "absolute-import",
        "absolute-symlink",
        "absolute-post",
        "absolute-escape",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let default = home.join(".config/knot");
        let xdg_home = home.join("xdg");
        let xdg = xdg_home.join("knot");
        if matches!(
            case,
            "unset-existing"
                | "empty-existing"
                | "relative-existing"
                | "absolute-default"
                | "absolute-both"
                | "absolute-invalid"
                | "absolute-dangling"
        ) {
            fs::create_dir_all(&default).unwrap();
            // A lower root must never be captured after selecting an invalid root.
            if case == "absolute-invalid" || case == "absolute-dangling" {
                fs::write(default.join(PRE_INIT), "export {};").unwrap();
            }
        }
        if case == "absolute-invalid" {
            fs::create_dir_all(&xdg_home).unwrap();
            fs::write(&xdg, "invalid root").unwrap();
        } else if case == "absolute-dangling" {
            fs::create_dir_all(&xdg_home).unwrap();
            std::os::unix::fs::symlink(home.join("missing"), &xdg).unwrap();
        } else if case == "absolute-symlink" {
            let target = home.join("dotfiles");
            fs::create_dir_all(&target).unwrap();
            fs::create_dir_all(&xdg_home).unwrap();
            fs::write(target.join(PRE_INIT), "import './helper.mjs';").unwrap();
            fs::write(target.join("helper.mjs"), "export {};").unwrap();
            std::os::unix::fs::symlink(&target, &xdg).unwrap();
        } else if matches!(
            case,
            "absolute-both" | "absolute-import" | "absolute-post" | "absolute-escape"
        ) {
            fs::create_dir_all(&xdg).unwrap();
            if case == "absolute-import" {
                fs::write(xdg.join(PRE_INIT), "import './helper.mjs';").unwrap();
                fs::write(xdg.join("helper.mjs"), "export {};").unwrap();
            } else if case == "absolute-escape" {
                fs::write(home.join("outside.js"), "export {};").unwrap();
                fs::write(xdg.join(PRE_INIT), "import '../../outside.js';").unwrap();
            } else if case == "absolute-post" {
                fs::write(xdg.join(POST_INIT), "export {};").unwrap();
            }
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "app::personal_config::tests::linux_environment_contract",
                "--nocapture",
            ])
            .env(CHILD, case)
            .env("HOME", home)
            .env_remove("XDG_CONFIG_HOME");
        match case {
            "unset" | "unset-existing" => {}
            "empty" | "empty-existing" => {
                child.env("XDG_CONFIG_HOME", "");
            }
            "relative" | "relative-existing" => {
                child.env("XDG_CONFIG_HOME", "relative");
            }
            _ => {
                child.env("XDG_CONFIG_HOME", &xdg_home);
            }
        }
        let output = child.output().unwrap();
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "child test did not run for {case}"
        );
        assert!(
            output.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn selects_one_existing_xdg_root_before_native_root() {
    let temp = tempfile::tempdir().unwrap();
    let native = temp.path().join("native/Knot");
    let home = temp.path().join("home");
    let xdg = temp.path().join("xdg");
    fs::create_dir_all(&native).unwrap();
    fs::create_dir_all(&home).unwrap();
    fs::write(native.join(PRE_INIT), "globalThis.native = true;").unwrap();
    assert_eq!(select_config_root(&native, Some(&home), None), native);

    let default_xdg = home.join(".config/knot");
    fs::create_dir_all(&default_xdg).unwrap();
    fs::write(default_xdg.join(POST_INIT), "export {};").unwrap();
    assert_eq!(select_config_root(&native, Some(&home), None), default_xdg);
    assert_eq!(
        select_config_root(&native, Some(&home), Some(OsStr::new("relative"))),
        default_xdg
    );

    fs::create_dir_all(xdg.join("knot")).unwrap();
    let selected = select_config_root(&native, Some(&home), Some(xdg.as_os_str()));
    assert_eq!(selected, xdg.join("knot"));
    let captured = capture(&selected).unwrap();
    assert!(!captured.pre_init && !captured.post_init);
    assert!(captured.sources.is_empty());
}

#[test]
fn missing_directory_and_entries_are_optional() {
    let temp = tempfile::tempdir().unwrap();
    let missing = capture(&temp.path().join("missing")).unwrap();
    assert!(!missing.pre_init && !missing.post_init);
    let empty = capture(temp.path()).unwrap();
    assert!(empty.sources.is_empty());
}

#[test]
fn captures_either_phase_and_local_modules_as_immutable_sources() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("lib")).unwrap();
    fs::write(temp.path().join(PRE_INIT), "import './lib/state.mjs';").unwrap();
    fs::write(temp.path().join("lib/state.mjs"), "export let value = 1;").unwrap();
    let pre = capture(temp.path()).unwrap();
    assert!(pre.pre_init && !pre.post_init);
    fs::write(temp.path().join(POST_INIT), "export {};").unwrap();
    let both = capture(temp.path()).unwrap();
    assert!(both.pre_init && both.post_init);
    assert_eq!(both.sources.len(), 3);
    assert!(
        both.clone()
            .into_graph()
            .unwrap()
            .entry_url(POST_INIT)
            .is_some()
    );
    fs::write(temp.path().join("lib/state.mjs"), "changed").unwrap();
    assert_eq!(
        both.sources["lib/state.mjs"].as_ref(),
        "export let value = 1;"
    );
}

#[test]
fn validates_imports_with_phase_and_source_location() {
    let temp = tempfile::tempdir().unwrap();
    let entry = temp.path().join(POST_INIT);
    fs::write(&entry, "import './missing.mjs';").unwrap();
    let missing = capture(temp.path()).unwrap().post_init_error.unwrap();
    assert_eq!(missing.phase, Some(ConfigPhase::PostInit));
    assert_eq!(missing.path, entry);
    assert_eq!(missing.line, Some(1));
    assert!(missing.cause.contains("configuration module not found"));

    fs::write(&entry, "import '../outside.js';").unwrap();
    let escape = capture(temp.path()).unwrap().post_init_error.unwrap();
    assert_eq!(escape.phase, Some(ConfigPhase::PostInit));
    assert_eq!(escape.line, Some(1));
    assert!(escape.cause.contains("escapes configuration"));

    fs::write(&entry, "import './valid.js';").unwrap();
    fs::write(temp.path().join("valid.js"), "export const value = 1;").unwrap();
    assert!(capture(temp.path()).is_ok());

    fs::write(temp.path().join("valid.js"), "export const = ;").unwrap();
    let invalid = capture(temp.path()).unwrap().post_init_error.unwrap();
    assert_eq!(invalid.path, temp.path().join("valid.js"));
    assert_eq!(invalid.line, Some(1));
}

#[test]
fn defers_post_init_entry_and_import_read_failures() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(PRE_INIT), "globalThis.ready = true;").unwrap();
    fs::write(temp.path().join(POST_INIT), [0xff]).unwrap();
    let captured = capture(temp.path()).unwrap();
    assert!(captured.pre_init && captured.post_init);
    assert_eq!(
        captured.post_init_error.as_ref().unwrap().path,
        temp.path().join(POST_INIT)
    );
    assert!(
        captured
            .into_graph()
            .unwrap()
            .entry_url(POST_INIT)
            .is_none()
    );

    fs::write(temp.path().join(POST_INIT), "import './helper.mjs';").unwrap();
    fs::write(temp.path().join("helper.mjs"), [0xff]).unwrap();
    let captured = capture(temp.path()).unwrap();
    let error = captured.post_init_error.unwrap();
    assert_eq!(error.phase, Some(ConfigPhase::PostInit));
    assert_eq!(error.path, temp.path().join(POST_INIT));
    assert_eq!(error.line, Some(1));
    assert!(error.cause.contains("helper.mjs"));
    assert!(error.cause.contains("utf-8") || error.cause.contains("UTF-8"));

    fs::remove_file(temp.path().join(POST_INIT)).unwrap();
    fs::create_dir(temp.path().join(POST_INIT)).unwrap();
    let captured = capture(temp.path()).unwrap();
    assert_eq!(
        captured.post_init_error.unwrap().phase,
        Some(ConfigPhase::PostInit)
    );
}

#[test]
fn pre_init_cannot_import_post_init() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join(PRE_INIT), "import './helper.js';").unwrap();
    fs::write(temp.path().join("helper.js"), "import './post-init.js';").unwrap();
    fs::write(temp.path().join(POST_INIT), "export {};").unwrap();
    let error = capture(temp.path()).unwrap_err();
    assert_eq!(error.phase, Some(ConfigPhase::PreInit));
    assert_eq!(error.path, temp.path().join("helper.js"));
    assert_eq!(error.line, Some(1));
    assert!(error.cause.contains("pre-init cannot import post-init"));
}

#[test]
fn reports_unreadable_source_and_invalid_root() {
    let temp = tempfile::tempdir().unwrap();
    let entry = temp.path().join(PRE_INIT);
    fs::write(&entry, [0xff]).unwrap();
    let invalid = capture(temp.path()).unwrap_err();
    assert_eq!(invalid.phase, Some(ConfigPhase::PreInit));
    assert_eq!(invalid.path, entry);
    assert!(invalid.cause.contains("utf-8") || invalid.cause.contains("UTF-8"));
    let file_root = temp.path().join("file-root");
    fs::write(&file_root, "").unwrap();
    assert_eq!(capture(&file_root).unwrap_err().path, file_root);
}

#[cfg(unix)]
#[test]
fn follows_selected_root_symlink_but_keeps_imports_contained() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("dotfiles/knot");
    let xdg = temp.path().join("xdg");
    let selected = xdg.join("knot");
    fs::create_dir_all(&target).unwrap();
    fs::create_dir(&xdg).unwrap();
    fs::write(target.join(PRE_INIT), "import './helper.js';").unwrap();
    fs::write(target.join("helper.js"), "export {};").unwrap();
    symlink(&target, &selected).unwrap();

    assert_eq!(
        select_config_root(temp.path(), None, Some(xdg.as_os_str())),
        selected
    );
    let captured = capture(&selected).unwrap();
    assert_eq!(
        captured.canonical_directory(),
        fs::canonicalize(&target).unwrap()
    );
    assert!(captured.pre_init);
    assert_eq!(captured.sources.len(), 2);

    let outside = temp.path().join("outside.js");
    fs::write(&outside, "export {};").unwrap();
    fs::remove_file(target.join("helper.js")).unwrap();
    symlink(&outside, target.join("helper.js")).unwrap();
    let escaped = capture(&selected).unwrap_err();
    assert_eq!(escaped.phase, Some(ConfigPhase::PreInit));
    assert!(escaped.cause.contains("escapes"));
}

#[cfg(unix)]
#[test]
fn rejects_escaping_and_unreadable_module_sources() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("outside.js"), "export {};").unwrap();
    symlink(
        outside.path().join("outside.js"),
        temp.path().join(PRE_INIT),
    )
    .unwrap();
    let escaped = capture(temp.path()).unwrap_err();
    assert_eq!(escaped.phase, Some(ConfigPhase::PreInit));
    assert!(escaped.cause.contains("escapes"));

    fs::remove_file(temp.path().join(PRE_INIT)).unwrap();
    symlink(temp.path().join("missing.js"), temp.path().join(PRE_INIT)).unwrap();
    let missing = capture(temp.path()).unwrap_err();
    assert_eq!(missing.path, temp.path().join(PRE_INIT));
    assert_eq!(missing.phase, Some(ConfigPhase::PreInit));

    fs::remove_file(temp.path().join(PRE_INIT)).unwrap();
    fs::write(temp.path().join(PRE_INIT), "export {};").unwrap();
    symlink(
        outside.path().join("outside.js"),
        temp.path().join(POST_INIT),
    )
    .unwrap();
    let deferred = capture(temp.path()).unwrap().post_init_error.unwrap();
    assert_eq!(deferred.phase, Some(ConfigPhase::PostInit));
    assert!(deferred.cause.contains("escapes"));

    fs::remove_file(temp.path().join(POST_INIT)).unwrap();
    fs::write(temp.path().join(POST_INIT), "import './linked/helper.js';").unwrap();
    fs::create_dir(outside.path().join("directory")).unwrap();
    fs::write(outside.path().join("directory/helper.js"), "export {};").unwrap();
    symlink(outside.path().join("directory"), temp.path().join("linked")).unwrap();
    let deferred = capture(temp.path()).unwrap().post_init_error.unwrap();
    assert_eq!(deferred.phase, Some(ConfigPhase::PostInit));
    assert!(deferred.cause.contains("symlinked configuration directory"));
}
