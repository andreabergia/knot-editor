//! Immutable source input for one extension lifecycle.

use std::collections::BTreeMap;
use std::sync::Arc;

use url::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigPhase {
    PreInit,
    PostInit,
}

impl ConfigPhase {
    pub(crate) const fn entry(self) -> &'static str {
        match self {
            Self::PreInit => "pre-init.js",
            Self::PostInit => "post-init.js",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ModuleGraph {
    root: Arc<str>,
    entry: Arc<str>,
    entries: Arc<BTreeMap<String, Arc<str>>>,
    sources: Arc<BTreeMap<String, Arc<str>>>,
}

impl ModuleGraph {
    /// `root` is a directory file URL; file names and `entry` are relative to it.
    pub(crate) fn new(
        root: &str,
        entry: &str,
        files: BTreeMap<String, Arc<str>>,
    ) -> Result<Self, String> {
        Self::with_entries(root, &[entry], files)
    }

    pub(crate) fn with_entries(
        root: &str,
        entries: &[&str],
        files: BTreeMap<String, Arc<str>>,
    ) -> Result<Self, String> {
        if entries.is_empty() {
            return Err("module graph needs an entry".into());
        }
        let root_url = Url::parse(root).map_err(|error| format!("invalid package URL: {error}"))?;
        if root_url.scheme() != "file"
            || !root_url.as_str().ends_with('/')
            || root_url.query().is_some()
            || root_url.fragment().is_some()
        {
            return Err("package root must be a directory file URL".into());
        }
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
        let mut entry_urls = BTreeMap::<String, Arc<str>>::new();
        for entry in entries {
            validate_path(entry)?;
            let url = file_url(&root_url, entry)?.to_string();
            if !sources.contains_key(&url) {
                return Err(format!("entry module is unavailable: {url}"));
            }
            if entry_urls
                .insert((*entry).to_owned(), Arc::from(url))
                .is_some()
            {
                return Err(format!("duplicate module entry: {entry}"));
            }
        }
        let entry = entry_urls[entries[0]].clone();
        Ok(Self {
            root: root_url.to_string().into(),
            entry,
            entries: Arc::new(entry_urls),
            sources: Arc::new(sources),
        })
    }

    pub(crate) fn root(&self) -> &str {
        &self.root
    }

    pub(crate) fn entry(&self) -> &str {
        &self.entry
    }

    pub(crate) fn entry_url(&self, name: &str) -> Option<&str> {
        self.entries.get(name).map(AsRef::as_ref)
    }

    pub(crate) fn entries(&self) -> &BTreeMap<String, Arc<str>> {
        &self.entries
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

    #[test]
    fn multiple_entries_must_exist_and_keep_distinct_urls() {
        let files = BTreeMap::from([
            ("pre-init.js".into(), Arc::from("export {};")),
            ("post-init.js".into(), Arc::from("export {};")),
        ]);
        let graph = ModuleGraph::with_entries(
            "file:///config/knot/",
            &["pre-init.js", "post-init.js"],
            files.clone(),
        )
        .unwrap();
        assert_eq!(graph.entry(), "file:///config/knot/pre-init.js");
        assert_eq!(
            graph.entry_url("post-init.js"),
            Some("file:///config/knot/post-init.js")
        );
        assert!(ModuleGraph::with_entries("file:///config/knot/", &[], files.clone()).is_err());
        assert!(
            ModuleGraph::with_entries(
                "file:///config/knot/",
                &["pre-init.js", "pre-init.js"],
                files.clone()
            )
            .is_err()
        );
        assert!(ModuleGraph::with_entries("file:///config/knot/", &["missing.js"], files).is_err());
    }
}
