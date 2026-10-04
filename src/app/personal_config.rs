//! Personal configuration source capture before JavaScript enters the host.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use directories::{BaseDirs, ProjectDirs};

use crate::host::engine::RuntimeError;
use crate::host::{engine::validate_module_graph_entry, module_graph::ModuleGraph};

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

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::PreInit => "Pre-init",
            Self::PostInit => "Post-init",
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
    pub(crate) stack: Option<String>,
}

impl ConfigDiagnostic {
    pub(crate) fn new(phase: Option<ConfigPhase>, path: &Path, cause: impl Into<String>) -> Self {
        Self {
            phase,
            path: path.to_path_buf(),
            line: None,
            column: None,
            cause: cause.into(),
            stack: None,
        }
    }

    pub(crate) fn from_runtime(
        phase: ConfigPhase,
        directory: &Path,
        canonical_directory: &Path,
        error: &RuntimeError,
    ) -> Self {
        let path = error
            .source()
            .and_then(|source| url::Url::parse(source).ok())
            .and_then(|url| url.to_file_path().ok())
            .map(|path| {
                path.strip_prefix(canonical_directory)
                    .map(|relative| directory.join(relative))
                    .unwrap_or(path)
            })
            .unwrap_or_else(|| directory.join(phase.entry()));
        Self {
            phase: Some(phase),
            path,
            line: error.line(),
            column: error.column(),
            cause: config_message(error.message()),
            stack: error.stack().map(config_message),
        }
    }

    pub(crate) fn clipboard_text(&self) -> String {
        let phase = self.phase.map(ConfigPhase::label).unwrap_or("Discovery");
        let mut location = self.path.display().to_string();
        if let Some(line) = self.line {
            location.push_str(&format!(":{line}"));
            if let Some(column) = self.column {
                location.push_str(&format!(":{column}"));
            }
        }
        let mut diagnostic = format!(
            "Knot personal configuration failed during {phase}\n{location}\n{}",
            self.cause
        );
        if let Some(stack) = &self.stack {
            diagnostic.push('\n');
            diagnostic.push_str(stack);
        }
        diagnostic
    }
}

fn config_message(message: &str) -> String {
    message
        .replace(
            "Knot package module not found",
            "Knot configuration module not found",
        )
        .replace("extension import", "configuration import")
        .replace("escapes package", "escapes configuration")
}

#[derive(Clone, Debug)]
pub(crate) struct ConfigSources {
    pub(crate) directory: PathBuf,
    canonical_directory: PathBuf,
    pub(crate) pre_init: bool,
    pub(crate) post_init: bool,
    pub(crate) post_init_error: Option<ConfigDiagnostic>,
    /// Slash-separated paths relative to the config directory.
    pub(crate) sources: BTreeMap<String, Arc<str>>,
    graph: Option<ModuleGraph>,
}

impl ConfigSources {
    pub(crate) fn canonical_directory(&self) -> &Path {
        &self.canonical_directory
    }

    pub(crate) fn into_graph(self) -> Option<ModuleGraph> {
        self.graph
    }
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
        canonical_directory: directory.to_path_buf(),
        pre_init: false,
        post_init: false,
        post_init_error: None,
        sources: BTreeMap::new(),
        graph: None,
    };
    match fs::symlink_metadata(directory) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(captured),
        Err(error) => return Err(ConfigDiagnostic::new(None, directory, error.to_string())),
    }
    let metadata = fs::metadata(directory)
        .map_err(|error| ConfigDiagnostic::new(None, directory, error.to_string()))?;
    if !metadata.is_dir() {
        return Err(ConfigDiagnostic::new(
            None,
            directory,
            "configuration root must be a directory",
        ));
    }
    let canonical = fs::canonicalize(directory)
        .map_err(|error| ConfigDiagnostic::new(None, directory, error.to_string()))?;
    captured.canonical_directory = canonical.clone();
    for phase in [ConfigPhase::PreInit, ConfigPhase::PostInit] {
        let path = directory.join(phase.entry());
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.is_dir() {
                    let diagnostic = ConfigDiagnostic::new(
                        Some(phase),
                        &path,
                        "phase entry is a directory, not a JavaScript file",
                    );
                    match phase {
                        ConfigPhase::PreInit => return Err(diagnostic),
                        ConfigPhase::PostInit => captured.post_init_error = Some(diagnostic),
                    }
                }
                match phase {
                    ConfigPhase::PreInit => captured.pre_init = true,
                    ConfigPhase::PostInit => captured.post_init = true,
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                let diagnostic = ConfigDiagnostic::new(Some(phase), &path, error.to_string());
                match phase {
                    ConfigPhase::PreInit => return Err(diagnostic),
                    ConfigPhase::PostInit => {
                        captured.post_init = true;
                        captured.post_init_error = Some(diagnostic);
                    }
                }
            }
        }
    }
    if !captured.pre_init && !captured.post_init {
        return Ok(captured);
    }
    let mut source_failures = BTreeMap::new();
    collect(
        directory,
        directory,
        &canonical,
        &mut captured.sources,
        &mut source_failures,
    )?;
    if let Some(mut error) = source_failures.remove(PRE_INIT) {
        error.phase = Some(ConfigPhase::PreInit);
        return Err(error);
    }
    if captured.pre_init && !captured.sources.contains_key(PRE_INIT) {
        return Err(ConfigDiagnostic::new(
            Some(ConfigPhase::PreInit),
            &directory.join(PRE_INIT),
            "pre-init entry is not a readable JavaScript file",
        ));
    }
    if let Some(mut error) = source_failures.remove(POST_INIT) {
        error.phase = Some(ConfigPhase::PostInit);
        captured.post_init_error = Some(error);
    }
    if captured.post_init
        && !captured.sources.contains_key(POST_INIT)
        && captured.post_init_error.is_none()
    {
        captured.post_init_error = Some(ConfigDiagnostic::new(
            Some(ConfigPhase::PostInit),
            &directory.join(POST_INIT),
            "post-init entry is not a readable JavaScript file",
        ));
    }
    let root = url::Url::from_directory_path(&canonical).map_err(|_| {
        ConfigDiagnostic::new(None, directory, "configuration directory has no file URL")
    })?;
    let entries: Vec<_> = [ConfigPhase::PreInit, ConfigPhase::PostInit]
        .into_iter()
        .filter(|phase| captured.sources.contains_key(phase.entry()))
        .map(ConfigPhase::entry)
        .collect();
    if entries.is_empty() {
        return Ok(captured);
    }
    let graph = ModuleGraph::with_entries(root.as_str(), &entries, captured.sources.clone())
        .map_err(|cause| ConfigDiagnostic::new(None, directory, cause))?;
    for phase in [ConfigPhase::PreInit, ConfigPhase::PostInit] {
        let present = match phase {
            ConfigPhase::PreInit => captured.pre_init,
            ConfigPhase::PostInit => captured.post_init,
        };
        if !present
            || !captured.sources.contains_key(phase.entry())
            || (phase == ConfigPhase::PostInit && captured.post_init_error.is_some())
        {
            continue;
        }
        let validation = validate_module_graph_entry(&graph, phase.entry()).map_err(|error| {
            let mut diagnostic =
                ConfigDiagnostic::from_runtime(phase, directory, &canonical, &error);
            for (relative, failure) in &source_failures {
                let url = root.join(relative).expect("captured module path is valid");
                if error.message().contains(url.as_str()) {
                    diagnostic.cause.push_str(&format!(
                        "; {}: {}",
                        failure.path.display(),
                        failure.cause
                    ));
                    break;
                }
            }
            diagnostic
        });
        match (phase, validation) {
            (_, Ok(())) => {}
            (ConfigPhase::PreInit, Err(error)) => return Err(error),
            (ConfigPhase::PostInit, Err(error)) => captured.post_init_error = Some(error),
        }
    }
    captured.graph = Some(graph);
    Ok(captured)
}

fn collect(
    root: &Path,
    directory: &Path,
    canonical_root: &Path,
    sources: &mut BTreeMap<String, Arc<str>>,
    failures: &mut BTreeMap<String, ConfigDiagnostic>,
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
            if path.parent() == Some(root) && path.file_name().is_some_and(|name| name == POST_INIT)
            {
                continue;
            }
            if let Err(error) = collect(root, &path, canonical_root, sources, failures) {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .components()
                    .map(|part| {
                        part.as_os_str().to_str().ok_or_else(|| {
                            ConfigDiagnostic::new(None, &path, "module path is not UTF-8")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join("/");
                failures.insert(relative, error);
            }
            continue;
        }
        let phase = match path.file_name().and_then(|name| name.to_str()) {
            Some(PRE_INIT) if path.parent() == Some(root) => Some(ConfigPhase::PreInit),
            Some(POST_INIT) if path.parent() == Some(root) => Some(ConfigPhase::PostInit),
            _ => None,
        };
        let symlinked_directory = metadata.file_type().is_symlink() && path.is_dir();
        let javascript_file = matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("js" | "mjs")
        );
        if !symlinked_directory && !javascript_file {
            continue;
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
        if symlinked_directory {
            failures.insert(
                relative,
                ConfigDiagnostic::new(
                    phase,
                    &path,
                    "symlinked configuration directory is unsupported",
                ),
            );
            continue;
        }
        let source = (|| {
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
            fs::read_to_string(&target)
                .map_err(|error| ConfigDiagnostic::new(phase, &path, error.to_string()))
        })();
        match source {
            Ok(source) => {
                sources.insert(relative, Arc::from(source));
            }
            Err(error) => {
                failures.insert(relative, error);
            }
        }
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
}
