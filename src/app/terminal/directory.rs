use std::path::PathBuf;

use super::super::resource::ResourceUri;

/// The directory a new terminal starts in: the innermost root containing
/// `document`, otherwise the first root, otherwise `home`. Only roots that are
/// existing local directories count.
pub(crate) fn working_directory(
    roots: &[ResourceUri],
    document: Option<&ResourceUri>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let local = roots
        .iter()
        .filter_map(|root| {
            let path = root.as_url().to_file_path().ok()?;
            path.is_dir().then_some((root, path))
        })
        .collect::<Vec<_>>();
    document
        .and_then(|document| {
            local
                .iter()
                .filter(|(root, _)| document.is_within(root))
                .max_by_key(|(_, path)| path.components().count())
        })
        .or(local.first())
        .map(|(_, path)| path.clone())
        .or(home)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(path: &std::path::Path) -> ResourceUri {
        ResourceUri::from_url(url::Url::from_directory_path(path).unwrap())
    }

    fn file(path: &std::path::Path) -> ResourceUri {
        ResourceUri::from_url(url::Url::from_file_path(path).unwrap())
    }

    #[test]
    fn the_innermost_root_containing_the_document_wins() {
        let temp = tempfile::tempdir().unwrap();
        let outer = temp.path().join("repo");
        let inner = outer.join("crates/app");
        let other = temp.path().join("other");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let roots = [directory(&other), directory(&outer), directory(&inner)];

        let nested = file(&inner.join("src/main.rs"));
        assert_eq!(
            working_directory(&roots, Some(&nested), None),
            Some(inner.clone())
        );
        let outer_only = file(&outer.join("README.md"));
        assert_eq!(
            working_directory(&roots, Some(&outer_only), None),
            Some(outer)
        );
    }

    #[test]
    fn a_document_outside_every_root_falls_back_to_the_first_root() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let roots = [directory(&first), directory(&second)];

        let outside = file(&temp.path().join("notes.md"));
        assert_eq!(
            working_directory(&roots, Some(&outside), None),
            Some(first.clone())
        );
        assert_eq!(working_directory(&roots, None, None), Some(first));
    }

    #[test]
    fn missing_and_non_local_roots_are_skipped() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let present = temp.path().join("present");
        std::fs::create_dir_all(&present).unwrap();
        let roots = [
            ResourceUri::parse("mem://workspace/").unwrap(),
            directory(&missing),
            directory(&present),
        ];

        let in_missing = file(&missing.join("lib.rs"));
        assert_eq!(
            working_directory(&roots, Some(&in_missing), None),
            Some(present)
        );
    }

    #[test]
    fn a_window_without_usable_roots_uses_home() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().to_owned();
        let document = file(&temp.path().join("notes.md"));
        assert_eq!(
            working_directory(&[], Some(&document), Some(home.clone())),
            Some(home.clone())
        );
        let missing = [directory(&temp.path().join("missing"))];
        assert_eq!(
            working_directory(&missing, None, Some(home.clone())),
            Some(home)
        );
    }
}
