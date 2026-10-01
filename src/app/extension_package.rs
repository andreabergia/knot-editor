//! Installed extension manifests and immutable source graphs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use directories::ProjectDirs;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub main: String,
    #[serde(default)]
    pub requires: Vec<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub website: Option<String>,
    pub license: Option<String>,
}

#[derive(Clone, Debug)]
pub struct InstalledPackage {
    pub directory: PathBuf,
    pub manifest: Manifest,
    /// Slash-separated paths relative to `directory`, captured during discovery.
    pub sources: BTreeMap<String, Arc<str>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDiagnostic {
    pub directory: PathBuf,
    pub name: Option<String>,
    pub cause: String,
}

#[derive(Default)]
pub struct DiscoveryReport {
    pub packages: Vec<InstalledPackage>,
    pub diagnostics: Vec<PackageDiagnostic>,
}

/// Platform-local application data, with the root passed explicitly to discovery.
pub fn user_extensions_root() -> Result<PathBuf, String> {
    let dirs = ProjectDirs::from("", "", "Knot")
        .ok_or_else(|| "cannot determine the user application-data directory".to_owned())?;
    Ok(dirs.data_local_dir().join("extensions"))
}

pub fn discover(root: &Path) -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    if !root.exists() {
        return report;
    }
    let scopes = match sorted_entries(root) {
        Ok(entries) => entries,
        Err(cause) => {
            report.diagnostics.push(diagnostic(root, None, cause));
            return report;
        }
    };
    for scope in scopes {
        let scope_name = scope.file_name().unwrap().to_string_lossy();
        if !scope_name.starts_with('@') || !scope.is_dir() {
            continue;
        }
        if fs::symlink_metadata(&scope).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            report.diagnostics.push(diagnostic(
                &scope,
                None,
                "scope directory cannot be a symlink".into(),
            ));
            continue;
        }
        let packages = match sorted_entries(&scope) {
            Ok(entries) => entries,
            Err(cause) => {
                report.diagnostics.push(diagnostic(&scope, None, cause));
                continue;
            }
        };
        for directory in packages {
            if !directory.is_dir() {
                continue;
            }
            match read_package(&directory) {
                Ok(package) => report.packages.push(package),
                Err((name, cause)) => report.diagnostics.push(diagnostic(&directory, name, cause)),
            }
        }
    }
    let mut counts = BTreeMap::<String, usize>::new();
    for package in &report.packages {
        *counts.entry(package.manifest.name.clone()).or_default() += 1;
    }
    for diagnostic in &report.diagnostics {
        if let Some(name) = &diagnostic.name {
            *counts.entry(name.clone()).or_default() += 1;
        }
    }
    for diagnostic in &mut report.diagnostics {
        if let Some(name) = &diagnostic.name
            && counts[name] > 1
        {
            diagnostic.cause = format!("duplicate package ID {name}; {}", diagnostic.cause);
        }
    }
    report.packages.retain(|package| {
        if counts[&package.manifest.name] > 1 {
            report.diagnostics.push(diagnostic(
                &package.directory,
                Some(package.manifest.name.clone()),
                format!("duplicate package ID {}", package.manifest.name),
            ));
            false
        } else {
            let expected = package
                .directory
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
                + "/"
                + &package.directory.file_name().unwrap().to_string_lossy();
            if package.manifest.name != expected {
                report.diagnostics.push(diagnostic(
                    &package.directory,
                    Some(package.manifest.name.clone()),
                    format!("directory/name mismatch: expected {expected}"),
                ));
                false
            } else {
                true
            }
        }
    });
    report
}

fn diagnostic(directory: &Path, name: Option<String>, cause: String) -> PackageDiagnostic {
    PackageDiagnostic {
        directory: directory.to_path_buf(),
        name,
        cause,
    }
}

fn sorted_entries(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("cannot read directory: {error}"))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();
    Ok(entries)
}

fn read_package(directory: &Path) -> Result<InstalledPackage, (Option<String>, String)> {
    let fail = |cause: String| (None, cause);
    if fs::symlink_metadata(directory)
        .map_err(|error| fail(error.to_string()))?
        .file_type()
        .is_symlink()
    {
        return Err(fail("package directory cannot be a symlink".into()));
    }
    let canonical = fs::canonicalize(directory).map_err(|error| fail(error.to_string()))?;
    let manifest_path = directory.join("knot.jsonc");
    let manifest_text = read_contained_file(&canonical, &manifest_path).map_err(fail)?;
    let manifest: Manifest =
        jsonc_parser::parse_to_serde_value(&manifest_text, &Default::default())
            .map_err(|error| fail(format!("invalid knot.jsonc: {error}")))?;
    let name = Some(manifest.name.clone());
    validate_manifest(&manifest).map_err(|cause| (name.clone(), cause))?;
    let main_path = directory.join(&manifest.main);
    read_contained_file(&canonical, &main_path)
        .map_err(|cause| (name.clone(), format!("main {}: {cause}", manifest.main)))?;
    let mut sources = BTreeMap::new();
    collect_sources(directory, directory, &canonical, &mut sources)
        .map_err(|cause| (name, cause))?;
    Ok(InstalledPackage {
        directory: directory.to_path_buf(),
        manifest,
        sources,
    })
}

fn validate_manifest(manifest: &Manifest) -> Result<(), String> {
    validate_name(&manifest.name)?;
    semver::Version::parse(&manifest.version)
        .map_err(|error| format!("invalid version: {error}"))?;
    validate_relative_module_path(&manifest.main)?;
    let mut requires = BTreeSet::new();
    for dependency in &manifest.requires {
        validate_name(dependency).map_err(|error| format!("invalid requires entry: {error}"))?;
        if !requires.insert(dependency) {
            return Err(format!("duplicate requires entry {dependency}"));
        }
    }
    for (field, value) in [
        ("displayName", &manifest.display_name),
        ("description", &manifest.description),
        ("author", &manifest.author),
        ("license", &manifest.license),
    ] {
        if value.as_ref().is_some_and(|value| value.trim().is_empty()) {
            return Err(format!("{field} cannot be empty"));
        }
    }
    if let Some(website) = &manifest.website {
        let url = url::Url::parse(website).map_err(|error| format!("invalid website: {error}"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
            return Err("website must be an absolute HTTP(S) URL".into());
        }
    }
    Ok(())
}

pub fn validate_name(name: &str) -> Result<(), String> {
    let Some((scope, package)) = name.strip_prefix('@').and_then(|name| name.split_once('/'))
    else {
        return Err(format!(
            "invalid package ID {name}: expected @scope/package"
        ));
    };
    if scope == "knot" {
        return Err("@knot is reserved for built-in packages".into());
    }
    for part in [scope, package] {
        if part.is_empty()
            || !part.as_bytes()[0].is_ascii_lowercase()
            || !part.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.')
            })
        {
            return Err(format!(
                "invalid package ID {name}: use lowercase ASCII names"
            ));
        }
    }
    Ok(())
}

fn validate_relative_module_path(path: &str) -> Result<(), String> {
    let value = Path::new(path);
    if !(path.ends_with(".js") || path.ends_with(".mjs"))
        || path.contains('\\')
        || path.split('/').any(str::is_empty)
        || value
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!(
            "invalid main path {path}: expected a relative .js or .mjs path within the package"
        ));
    }
    Ok(())
}

fn read_contained_file(canonical_directory: &Path, file: &Path) -> Result<String, String> {
    let target = fs::canonicalize(file)
        .map_err(|error| format!("cannot resolve {}: {error}", file.display()))?;
    if !target.starts_with(canonical_directory) {
        return Err(format!("path escapes package: {}", file.display()));
    }
    if !target.is_file() {
        return Err(format!("not a file: {}", file.display()));
    }
    fs::read_to_string(target).map_err(|error| format!("cannot read {}: {error}", file.display()))
}

fn collect_sources(
    root: &Path,
    directory: &Path,
    canonical_root: &Path,
    sources: &mut BTreeMap<String, Arc<str>>,
) -> Result<(), String> {
    for path in sorted_entries(directory)? {
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.is_dir() {
            collect_sources(root, &path, canonical_root, sources)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "js" || extension == "mjs")
        {
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|part| {
                    part.as_os_str()
                        .to_str()
                        .map(str::to_owned)
                        .ok_or_else(|| format!("non-UTF-8 module path: {}", path.display()))
                })
                .collect::<Result<Vec<_>, _>>()?
                .join("/");
            let source = read_contained_file(canonical_root, &path)?;
            sources.insert(relative, Arc::from(source));
        } else if metadata.file_type().is_symlink() && path.is_dir() {
            return Err(format!(
                "symlinked directory is unsupported: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
        "version": "1.2.3",
        "main": "dist/main.js",
        "requires": ["@example/base"],
        "displayName": "Tools",
        "description": "Editing tools",
        "author": "Example",
        "website": "https://example.com/tools",
        "license": "MIT",
    }"#;

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
                r#"{"name":"@example/tools","main":"dist/main.js"}"#,
                "version",
            ),
            (
                r#"{"name":"@example/tools","version":"nope","main":"dist/main.js"}"#,
                "version",
            ),
            (
                r#"{"name":"@example/tools","version":"1.0.0","main":"../main.js"}"#,
                "main path",
            ),
            (
                r#"{"name":"@example/tools","version":"1.0.0","main":"dist/main.js","website":"file:///tmp/x"}"#,
                "website",
            ),
            (
                r#"{"name":"@example/tools","version":"1.0.0","main":"dist/main.js","author":" "}"#,
                "author",
            ),
            (
                r#"{"name":"@knot/tools","version":"1.0.0","main":"dist/main.js"}"#,
                "reserved",
            ),
            (
                r#"{"name":"@Example/tools","version":"1.0.0","main":"dist/main.js"}"#,
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
            "name": "@example/tools", "version": "1.0.0", "main": "dist/main.js"
        }"#,
        );
        let report = discover(temp.path());
        assert!(report.diagnostics.is_empty());
        assert!(report.packages[0].manifest.requires.is_empty());
        assert_eq!(report.packages[0].manifest.author, None);
        fs::write(
            path.join("knot.jsonc"),
            r#"{
            "name": "@example/tools", "version": "1.0.0", "main": "dist/main.js",
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
            "name": "@example/tools", "version": "1.0.0", "main": "dist/main.mjs"
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
}
