//! Installed extension manifests and immutable source graphs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use directories::ProjectDirs;
use serde::Deserialize;

pub(crate) const SUPPORTED_API_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    #[serde(deserialize_with = "deserialize_api_version")]
    pub api_version: u32,
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

/// Where a package's immutable sources came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageOrigin {
    /// Discovered under the user extensions root.
    Installed,
    /// Compiled into the binary.
    Bundled,
}

#[derive(Clone, Debug)]
pub struct InstalledPackage {
    /// Installed directory, or a display-only placeholder for bundled packages.
    pub directory: PathBuf,
    pub origin: PackageOrigin,
    pub manifest: Manifest,
    /// Slash-separated paths relative to the module root, captured during discovery.
    pub sources: BTreeMap<String, Arc<str>>,
}

impl InstalledPackage {
    /// Directory file URL used as the immutable module graph root.
    pub(crate) fn module_root(&self) -> Result<String, String> {
        match self.origin {
            PackageOrigin::Installed => url::Url::from_directory_path(&self.directory)
                .map(String::from)
                .map_err(|_| format!("invalid package directory: {}", self.directory.display())),
            // Bundled identities are validated to `@knot/name`, so only `@` needs
            // escaping to stay inside the synthetic root's path.
            PackageOrigin::Bundled => Ok(format!(
                "file:///knot/bundled/{}/",
                self.manifest.name.replace('@', "%40")
            )),
        }
    }
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

/// Product discovery: installed packages plus compiled-in packages. An installed
/// directory that claims a bundled identity becomes a directory conflict rather
/// than rejecting that identity, so a named diagnostic always means its identity
/// is genuinely unavailable.
pub fn discover_all(root: &Path) -> DiscoveryReport {
    let mut report = super::bundled::report();
    let bundled = report
        .packages
        .iter()
        .map(|package| package.manifest.name.clone())
        .collect::<BTreeSet<_>>();
    let mut installed = discover(root);
    for diagnostic in &mut installed.diagnostics {
        if diagnostic
            .name
            .as_ref()
            .is_some_and(|name| bundled.contains(name))
        {
            let name = diagnostic.name.take().expect("checked above");
            diagnostic.cause = format!(
                "{name}: {}; the bundled package supplies this identity",
                diagnostic.cause
            );
        }
    }
    report.packages.extend(installed.packages);
    report.diagnostics.extend(installed.diagnostics);
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
    let value: serde_json::Value =
        jsonc_parser::parse_to_serde_value(&manifest_text, &Default::default())
            .map_err(|error| fail(format!("invalid knot.jsonc: {error}")))?;
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    validate_api_version(value.get("apiVersion")).map_err(|cause| (name.clone(), cause))?;
    let manifest: Manifest = serde_json::from_value(value)
        .map_err(|error| (name.clone(), format!("invalid knot.jsonc: {error}")))?;
    validate_manifest(&manifest, false).map_err(|cause| (name.clone(), cause))?;
    let main_path = directory.join(&manifest.main);
    read_contained_file(&canonical, &main_path)
        .map_err(|cause| (name.clone(), format!("main {}: {cause}", manifest.main)))?;
    let mut sources = BTreeMap::new();
    collect_sources(directory, directory, &canonical, &mut sources)
        .map_err(|cause| (name, cause))?;
    Ok(InstalledPackage {
        directory: directory.to_path_buf(),
        origin: PackageOrigin::Installed,
        manifest,
        sources,
    })
}

fn validate_api_version(value: Option<&serde_json::Value>) -> Result<u32, String> {
    let Some(value) = value else {
        return Err(format!(
            "missing apiVersion: expected {SUPPORTED_API_VERSION}, declared <missing>"
        ));
    };
    let valid = value
        .as_f64()
        .is_some_and(|number| number.is_finite() && number > 0.0 && number.fract() == 0.0);
    if !valid {
        return Err(format!(
            "malformed apiVersion: expected positive integer {SUPPORTED_API_VERSION}, declared {value}"
        ));
    }
    if value.as_f64() != Some(SUPPORTED_API_VERSION as f64) {
        return Err(format!(
            "unsupported apiVersion: expected {SUPPORTED_API_VERSION}, declared {value}"
        ));
    }
    Ok(SUPPORTED_API_VERSION)
}

fn deserialize_api_version<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u32, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    validate_api_version(Some(&value)).map_err(serde::de::Error::custom)
}

pub(crate) fn validate_package_api(manifest: &Manifest) -> Result<(), String> {
    validate_api_version(Some(&serde_json::Value::from(manifest.api_version))).map(|_| ())
}

/// Admits a compiled-in package with the same manifest and source rules used for
/// installed packages. `sources` are relative to the package root.
pub(crate) fn bundled_package(
    manifest_text: &str,
    sources: BTreeMap<String, Arc<str>>,
) -> Result<InstalledPackage, String> {
    let value: serde_json::Value =
        jsonc_parser::parse_to_serde_value(manifest_text, &Default::default())
            .map_err(|error| format!("invalid knot.jsonc: {error}"))?;
    validate_api_version(value.get("apiVersion"))?;
    let manifest: Manifest =
        serde_json::from_value(value).map_err(|error| format!("invalid knot.jsonc: {error}"))?;
    validate_manifest(&manifest, true)?;
    validate_bundled_name(&manifest.name)?;
    if !sources.contains_key(&manifest.main) {
        return Err(format!("main {}: not captured", manifest.main));
    }
    Ok(InstalledPackage {
        directory: PathBuf::from(format!("<bundled>/{}", manifest.name)),
        origin: PackageOrigin::Bundled,
        manifest,
        sources,
    })
}

fn validate_manifest(manifest: &Manifest, bundled: bool) -> Result<(), String> {
    validate_package_api(manifest)?;
    validate_name_inner(&manifest.name, bundled)?;
    semver::Version::parse(&manifest.version)
        .map_err(|error| format!("invalid version: {error}"))?;
    validate_relative_module_path(&manifest.main)?;
    let mut requires = BTreeSet::new();
    for dependency in &manifest.requires {
        // Any package may depend on a built-in package, which is the only owner
        // of the reserved scope.
        validate_name_inner(dependency, true)
            .map_err(|error| format!("invalid requires entry: {error}"))?;
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
    validate_name_inner(name, false)
}

/// Built-in identities supplied by the application must use the reserved scope.
pub(crate) fn validate_bundled_name(name: &str) -> Result<(), String> {
    validate_name_inner(name, true)?;
    if !name.starts_with("@knot/") {
        return Err(format!(
            "built-in package ID {name} must use the @knot scope"
        ));
    }
    Ok(())
}

fn validate_name_inner(name: &str, allow_reserved_scope: bool) -> Result<(), String> {
    let Some((scope, package)) = name.strip_prefix('@').and_then(|name| name.split_once('/'))
    else {
        return Err(format!(
            "invalid package ID {name}: expected @scope/package"
        ));
    };
    if scope == "knot" && !allow_reserved_scope {
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
mod tests;
