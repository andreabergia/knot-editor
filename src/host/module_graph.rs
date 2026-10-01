//! Immutable source input for one extension lifecycle.

use std::collections::BTreeMap;
use std::sync::Arc;

use url::Url;

#[derive(Clone, Debug)]
pub(crate) struct ModuleGraph {
    root: Arc<str>,
    entry: Arc<str>,
    sources: Arc<BTreeMap<String, Arc<str>>>,
}

impl ModuleGraph {
    /// `root` is a directory file URL; file names and `entry` are relative to it.
    pub(crate) fn new(
        root: &str,
        entry: &str,
        files: BTreeMap<String, Arc<str>>,
    ) -> Result<Self, String> {
        let root_url = Url::parse(root).map_err(|error| format!("invalid package URL: {error}"))?;
        if root_url.scheme() != "file"
            || !root_url.as_str().ends_with('/')
            || root_url.query().is_some()
            || root_url.fragment().is_some()
        {
            return Err("package root must be a directory file URL".into());
        }
        validate_path(entry)?;
        let mut sources = BTreeMap::new();
        for (path, source) in files {
            validate_path(&path)?;
            let url = file_url(&root_url, &path)?;
            if !url.as_str().starts_with(root_url.as_str()) {
                return Err(format!("module path escapes package: {path}"));
            }
            if sources.insert(url.to_string(), source).is_some() {
                return Err(format!("duplicate module URL for {path}"));
            }
        }
        let entry = file_url(&root_url, entry)?.to_string();
        if !sources.contains_key(&entry) {
            return Err(format!("entry module is unavailable: {entry}"));
        }
        Ok(Self {
            root: root_url.to_string().into(),
            entry: entry.into(),
            sources: Arc::new(sources),
        })
    }

    pub(crate) fn root(&self) -> &str {
        &self.root
    }

    pub(crate) fn entry(&self) -> &str {
        &self.entry
    }

    pub(crate) fn sources(&self) -> &BTreeMap<String, Arc<str>> {
        &self.sources
    }
}

fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path.starts_with('/')
    {
        return Err(format!("invalid package module path: {path}"));
    }
    Ok(())
}

fn file_url(root: &Url, path: &str) -> Result<Url, String> {
    let mut url = root.clone();
    url.path_segments_mut()
        .map_err(|_| format!("invalid package module path: {path}"))?
        .pop_if_empty()
        .extend(path.split('/'));
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_entry_and_rejects_invalid_paths() {
        let files = BTreeMap::from([("dist/main.js".into(), Arc::from("export {};"))]);
        let graph = ModuleGraph::new(
            "file:///extensions/%40example/tools/",
            "dist/main.js",
            files.clone(),
        )
        .unwrap();
        assert_eq!(
            graph.entry(),
            "file:///extensions/%40example/tools/dist/main.js"
        );
        assert_eq!(graph.sources().len(), 1);
        assert!(
            ModuleGraph::new("file:///extensions/tools/", "../main.js", files.clone()).is_err()
        );
        assert!(
            ModuleGraph::new("file:///extensions/tools/", "missing.js", files.clone()).is_err()
        );
        assert!(
            ModuleGraph::new("https://example.com/tools/", "dist/main.js", files.clone()).is_err()
        );
        assert!(
            ModuleGraph::new(
                "file:///extensions/tools/",
                "dist/main.js",
                BTreeMap::from([
                    ("../outside.js".into(), Arc::from("")),
                    ("dist/main.js".into(), Arc::from("")),
                ])
            )
            .is_err()
        );
    }

    #[test]
    fn encodes_file_name_punctuation_without_changing_package_root() {
        let graph = ModuleGraph::new(
            "file:///extensions/%40example/tools/",
            "main #1.js",
            BTreeMap::from([("main #1.js".into(), Arc::from("export {};"))]),
        )
        .unwrap();
        assert_eq!(
            graph.entry(),
            "file:///extensions/%40example/tools/main%20%231.js"
        );
    }
}
