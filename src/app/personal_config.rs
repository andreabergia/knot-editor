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
mod tests;
