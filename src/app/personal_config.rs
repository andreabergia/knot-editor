//! Personal configuration source capture before JavaScript enters the host.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use directories::{BaseDirs, ProjectDirs};

use crate::host::{engine::validate_module_graph, module_graph::ModuleGraph};

const PRE_INIT: &str = "pre-init.js";
const POST_INIT: &str = "post-init.js";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigPhase {
    PreInit,
    PostInit,
}

impl ConfigPhase {
    pub(crate) fn entry(self) -> &'static str {
        match self {
            Self::PreInit => PRE_INIT,
            Self::PostInit => POST_INIT,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConfigDiagnostic {
    pub(crate) phase: Option<ConfigPhase>,
    pub(crate) path: PathBuf,
    pub(crate) line: Option<u32>,
    pub(crate) column: Option<u32>,
    pub(crate) cause: String,
}

impl ConfigDiagnostic {
    fn new(phase: Option<ConfigPhase>, path: &Path, cause: impl Into<String>) -> Self {
        Self {
            phase,
            path: path.to_path_buf(),
            line: None,
            column: None,
            cause: cause.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ConfigSources {
    pub(crate) directory: PathBuf,
    pub(crate) pre_init: bool,
    pub(crate) post_init: bool,
    /// Slash-separated paths relative to the config directory.
    pub(crate) sources: BTreeMap<String, Arc<str>>,
}

pub(crate) fn user_config_root() -> Result<PathBuf, String> {
    let dirs = ProjectDirs::from("", "", "Knot")
        .ok_or_else(|| "cannot determine the user configuration directory".to_owned())?;
    let base = BaseDirs::new();
    let xdg_home = std::env::var_os("XDG_CONFIG_HOME");
    Ok(select_config_root(
        dirs.config_dir(),
        base.as_ref().map(BaseDirs::home_dir),
        xdg_home.as_deref(),
    ))
}

fn select_config_root(native: &Path, home: Option<&Path>, xdg_home: Option<&OsStr>) -> PathBuf {
    let xdg = xdg_home
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("knot"));
    let default_xdg = home.map(|home| home.join(".config/knot"));
    for path in [xdg, default_xdg].into_iter().flatten() {
        // Select an inaccessible or invalid existing path so capture can report
        // its failure instead of silently loading a lower-priority directory.
        if !matches!(fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            return path;
        }
    }
    native.to_path_buf()
}

/// Call on a background executor. `directory` is explicit so tests and launch
/// paths can select the same source contract without changing process state.
pub(crate) fn capture(directory: &Path) -> Result<ConfigSources, ConfigDiagnostic> {
    let mut captured = ConfigSources {
        directory: directory.to_path_buf(),
        pre_init: false,
        post_init: false,
        sources: BTreeMap::new(),
    };
    let metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(captured),
        Err(error) => return Err(ConfigDiagnostic::new(None, directory, error.to_string())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ConfigDiagnostic::new(
            None,
            directory,
            "configuration root must be a directory, not a symlink",
        ));
    }
    let canonical = fs::canonicalize(directory)
        .map_err(|error| ConfigDiagnostic::new(None, directory, error.to_string()))?;
    for phase in [ConfigPhase::PreInit, ConfigPhase::PostInit] {
        let path = directory.join(phase.entry());
        match fs::symlink_metadata(&path) {
            Ok(_) => match phase {
                ConfigPhase::PreInit => captured.pre_init = true,
                ConfigPhase::PostInit => captured.post_init = true,
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ConfigDiagnostic::new(Some(phase), &path, error.to_string()));
            }
        }
    }
    if !captured.pre_init && !captured.post_init {
        return Ok(captured);
    }
    collect(directory, directory, &canonical, &mut captured.sources)?;
    let root = url::Url::from_directory_path(&canonical).map_err(|_| {
        ConfigDiagnostic::new(None, directory, "configuration directory has no file URL")
    })?;
    for phase in [ConfigPhase::PreInit, ConfigPhase::PostInit] {
        let present = match phase {
            ConfigPhase::PreInit => captured.pre_init,
            ConfigPhase::PostInit => captured.post_init,
        };
        if !present {
            continue;
        }
        let graph = ModuleGraph::new(root.as_str(), phase.entry(), captured.sources.clone())
            .map_err(|cause| {
                ConfigDiagnostic::new(Some(phase), &directory.join(phase.entry()), cause)
            })?;
        validate_module_graph(&graph).map_err(|error| {
            let path = error
                .source()
                .and_then(|source| url::Url::parse(source).ok())
                .and_then(|url| url.to_file_path().ok())
                .map(|path| {
                    path.strip_prefix(&canonical)
                        .map(|relative| directory.join(relative))
                        .unwrap_or(path)
                })
                .unwrap_or_else(|| directory.join(phase.entry()));
            ConfigDiagnostic {
                phase: Some(phase),
                path,
                line: error.line(),
                column: error.column(),
                cause: error
                    .message()
                    .replace("extension import", "configuration import")
                    .replace("package", "configuration"),
            }
        })?;
    }
    Ok(captured)
}

fn collect(
    root: &Path,
    directory: &Path,
    canonical_root: &Path,
    sources: &mut BTreeMap<String, Arc<str>>,
) -> Result<(), ConfigDiagnostic> {
    let mut paths = fs::read_dir(directory)
        .map_err(|error| ConfigDiagnostic::new(None, directory, error.to_string()))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| ConfigDiagnostic::new(None, directory, error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    for path in paths {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| ConfigDiagnostic::new(None, &path, error.to_string()))?;
        if metadata.is_dir() {
            collect(root, &path, canonical_root, sources)?;
            continue;
        }
        if metadata.file_type().is_symlink() && path.is_dir() {
            return Err(ConfigDiagnostic::new(
                None,
                &path,
                "symlinked configuration directory is unsupported",
            ));
        }
        if !matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("js" | "mjs")
        ) {
            continue;
        }
        let phase = match path.file_name().and_then(|name| name.to_str()) {
            Some(PRE_INIT) if path.parent() == Some(root) => Some(ConfigPhase::PreInit),
            Some(POST_INIT) if path.parent() == Some(root) => Some(ConfigPhase::PostInit),
            _ => None,
        };
        let target = fs::canonicalize(&path)
            .map_err(|error| ConfigDiagnostic::new(phase, &path, error.to_string()))?;
        if !target.starts_with(canonical_root) {
            return Err(ConfigDiagnostic::new(
                phase,
                &path,
                "module path escapes the configuration directory",
            ));
        }
        if !target.is_file() {
            return Err(ConfigDiagnostic::new(phase, &path, "module is not a file"));
        }
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .components()
            .map(|part| {
                part.as_os_str()
                    .to_str()
                    .ok_or_else(|| ConfigDiagnostic::new(phase, &path, "module path is not UTF-8"))
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("/");
        let source = fs::read_to_string(&target)
            .map_err(|error| ConfigDiagnostic::new(phase, &path, error.to_string()))?;
        sources.insert(relative, Arc::from(source));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let missing = capture(temp.path()).unwrap_err();
        assert_eq!(missing.phase, Some(ConfigPhase::PostInit));
        assert_eq!(missing.path, entry);
        assert_eq!(missing.line, Some(1));
        assert!(missing.cause.contains("configuration module not found"));

        fs::write(&entry, "import '../outside.js';").unwrap();
        let escape = capture(temp.path()).unwrap_err();
        assert_eq!(escape.phase, Some(ConfigPhase::PostInit));
        assert_eq!(escape.line, Some(1));
        assert!(escape.cause.contains("escapes configuration"));

        fs::write(&entry, "import './valid.js';").unwrap();
        fs::write(temp.path().join("valid.js"), "export const value = 1;").unwrap();
        assert!(capture(temp.path()).is_ok());

        fs::write(temp.path().join("valid.js"), "export const = ;").unwrap();
        let invalid = capture(temp.path()).unwrap_err();
        assert_eq!(invalid.path, temp.path().join("valid.js"));
        assert_eq!(invalid.line, Some(1));
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
    }
}
