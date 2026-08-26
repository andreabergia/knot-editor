use std::{collections::BTreeMap, sync::RwLock};

use super::{
    FileSystemProvider, ProviderFuture, ResourceEntry, ResourceError, ResourceKind, ResourceStat,
    child_uri, validate_and_normalize_uri,
};
use crate::app::resource::ResourceUri;

#[derive(Default)]
struct MemoryDirectory {
    children: BTreeMap<String, MemoryNode>,
}

enum MemoryNode {
    File(Vec<u8>),
    Directory(MemoryDirectory),
}

pub(crate) struct MemoryFileSystemProvider {
    root: ResourceUri,
    hierarchy: RwLock<MemoryDirectory>,
}

impl MemoryFileSystemProvider {
    pub(crate) fn new(root: ResourceUri) -> Result<Self, ResourceError> {
        let root = normalize_memory_uri(root)?;
        if root.as_url().host_str().is_none() {
            return Err(ResourceError::InvalidUri {
                uri: root.to_string(),
                reason: "memory resource URIs require an authority".into(),
            });
        }
        Ok(Self {
            root,
            hierarchy: RwLock::new(MemoryDirectory::default()),
        })
    }

    pub(crate) fn root(&self) -> &ResourceUri {
        &self.root
    }

    pub(crate) fn seed_directory(&self, uri: ResourceUri) -> Result<(), ResourceError> {
        let uri = self.normalize_scoped(uri)?;
        let path = relative_segments(&self.root, &uri);
        let mut hierarchy = self
            .hierarchy
            .write()
            .map_err(|error| ResourceError::io("lock memory provider", error))?;
        ensure_directory(&mut hierarchy, &path, &uri)
    }

    pub(crate) fn seed_file(
        &self,
        uri: ResourceUri,
        bytes: impl Into<Vec<u8>>,
    ) -> Result<(), ResourceError> {
        let uri = self.normalize_scoped(uri)?;
        let mut path = relative_segments(&self.root, &uri);
        let Some(name) = path.pop() else {
            return Err(ResourceError::WrongKind {
                uri,
                expected: ResourceKind::File,
                actual: ResourceKind::Directory,
            });
        };
        let mut hierarchy = self
            .hierarchy
            .write()
            .map_err(|error| ResourceError::io("lock memory provider", error))?;
        let directory = directory_mut(&mut hierarchy, &path, &uri)?;
        if matches!(
            directory.children.get(&name),
            Some(MemoryNode::Directory(_))
        ) {
            return Err(ResourceError::WrongKind {
                uri,
                expected: ResourceKind::File,
                actual: ResourceKind::Directory,
            });
        }
        directory
            .children
            .insert(name, MemoryNode::File(bytes.into()));
        Ok(())
    }

    fn normalize_scoped(&self, uri: ResourceUri) -> Result<ResourceUri, ResourceError> {
        let uri = normalize_memory_uri(uri)?;
        if !uri.is_within(&self.root) {
            return Err(ResourceError::OutsideWorkspace {
                uri,
                root: self.root.clone(),
            });
        }
        Ok(uri)
    }
}

impl FileSystemProvider for MemoryFileSystemProvider {
    fn normalize(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceUri> {
        Box::pin(async move { self.normalize_scoped(uri) })
    }

    fn enumerate(&self, uri: ResourceUri) -> ProviderFuture<'_, Vec<ResourceEntry>> {
        Box::pin(async move {
            let uri = self.normalize_scoped(uri)?;
            let path = relative_segments(&self.root, &uri);
            let hierarchy = self
                .hierarchy
                .read()
                .map_err(|error| ResourceError::io("lock memory provider", error))?;
            let node = find_node(&hierarchy, &path);
            let directory = match node {
                Some(MemoryNode::Directory(directory)) => directory,
                Some(MemoryNode::File(_)) => {
                    return Err(ResourceError::WrongKind {
                        uri,
                        expected: ResourceKind::Directory,
                        actual: ResourceKind::File,
                    });
                }
                None if path.is_empty() => &hierarchy,
                None => return Err(ResourceError::NotFound { uri }),
            };
            directory
                .children
                .iter()
                .map(|(name, node)| {
                    Ok(ResourceEntry {
                        uri: child_uri(&uri, name)?,
                        name: name.clone(),
                        kind: node.kind(),
                    })
                })
                .collect()
        })
    }

    fn read(&self, uri: ResourceUri) -> ProviderFuture<'_, Vec<u8>> {
        Box::pin(async move {
            let uri = self.normalize_scoped(uri)?;
            let path = relative_segments(&self.root, &uri);
            let hierarchy = self
                .hierarchy
                .read()
                .map_err(|error| ResourceError::io("lock memory provider", error))?;
            match find_node(&hierarchy, &path) {
                Some(MemoryNode::File(bytes)) => Ok(bytes.clone()),
                Some(MemoryNode::Directory(_)) => Err(ResourceError::WrongKind {
                    uri,
                    expected: ResourceKind::File,
                    actual: ResourceKind::Directory,
                }),
                None if path.is_empty() => Err(ResourceError::WrongKind {
                    uri,
                    expected: ResourceKind::File,
                    actual: ResourceKind::Directory,
                }),
                None => Err(ResourceError::NotFound { uri }),
            }
        })
    }

    fn write(&self, uri: ResourceUri, bytes: Vec<u8>) -> ProviderFuture<'_, ()> {
        Box::pin(async move {
            let uri = self.normalize_scoped(uri)?;
            let path = relative_segments(&self.root, &uri);
            let mut hierarchy = self
                .hierarchy
                .write()
                .map_err(|error| ResourceError::io("lock memory provider", error))?;
            match find_node_mut(&mut hierarchy, &path) {
                Some(MemoryNode::File(contents)) => {
                    *contents = bytes;
                    Ok(())
                }
                Some(MemoryNode::Directory(_)) => Err(ResourceError::WrongKind {
                    uri,
                    expected: ResourceKind::File,
                    actual: ResourceKind::Directory,
                }),
                None if path.is_empty() => Err(ResourceError::WrongKind {
                    uri,
                    expected: ResourceKind::File,
                    actual: ResourceKind::Directory,
                }),
                None => Err(ResourceError::NotFound { uri }),
            }
        })
    }

    fn stat(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceStat> {
        Box::pin(async move {
            let uri = self.normalize_scoped(uri)?;
            let path = relative_segments(&self.root, &uri);
            if path.is_empty() {
                return Ok(ResourceStat::Directory);
            }
            let hierarchy = self
                .hierarchy
                .read()
                .map_err(|error| ResourceError::io("lock memory provider", error))?;
            Ok(match find_node(&hierarchy, &path) {
                Some(MemoryNode::File(_)) => ResourceStat::File,
                Some(MemoryNode::Directory(_)) => ResourceStat::Directory,
                None => ResourceStat::Missing,
            })
        })
    }
}

impl MemoryNode {
    fn kind(&self) -> ResourceKind {
        match self {
            Self::File(_) => ResourceKind::File,
            Self::Directory(_) => ResourceKind::Directory,
        }
    }
}

fn normalize_memory_uri(uri: ResourceUri) -> Result<ResourceUri, ResourceError> {
    validate_and_normalize_uri(uri, "mem")
}

fn relative_segments(root: &ResourceUri, uri: &ResourceUri) -> Vec<String> {
    let root_len = decoded_segments(root).len();
    decoded_segments(uri).into_iter().skip(root_len).collect()
}

fn decoded_segments(uri: &ResourceUri) -> Vec<String> {
    uri.as_url()
        .path_segments()
        .unwrap()
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            percent_encoding::percent_decode_str(segment)
                .decode_utf8()
                .expect("provider normalization validated UTF-8 segments")
                .into_owned()
        })
        .collect()
}

fn find_node<'a>(directory: &'a MemoryDirectory, path: &[String]) -> Option<&'a MemoryNode> {
    let (first, rest) = path.split_first()?;
    let node = directory.children.get(first)?;
    if rest.is_empty() {
        Some(node)
    } else if let MemoryNode::Directory(directory) = node {
        find_node(directory, rest)
    } else {
        None
    }
}

fn find_node_mut<'a>(
    directory: &'a mut MemoryDirectory,
    path: &[String],
) -> Option<&'a mut MemoryNode> {
    let (first, rest) = path.split_first()?;
    let node = directory.children.get_mut(first)?;
    if rest.is_empty() {
        Some(node)
    } else if let MemoryNode::Directory(directory) = node {
        find_node_mut(directory, rest)
    } else {
        None
    }
}

fn directory_mut<'a>(
    directory: &'a mut MemoryDirectory,
    path: &[String],
    uri: &ResourceUri,
) -> Result<&'a mut MemoryDirectory, ResourceError> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(directory);
    };
    match directory.children.get_mut(first) {
        Some(MemoryNode::Directory(child)) => directory_mut(child, rest, uri),
        Some(MemoryNode::File(_)) => Err(ResourceError::WrongKind {
            uri: uri.clone(),
            expected: ResourceKind::Directory,
            actual: ResourceKind::File,
        }),
        None => Err(ResourceError::NotFound { uri: uri.clone() }),
    }
}

fn ensure_directory(
    directory: &mut MemoryDirectory,
    path: &[String],
    uri: &ResourceUri,
) -> Result<(), ResourceError> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(());
    };
    let node = directory
        .children
        .entry(first.clone())
        .or_insert_with(|| MemoryNode::Directory(MemoryDirectory::default()));
    match node {
        MemoryNode::Directory(child) => ensure_directory(child, rest, uri),
        MemoryNode::File(_) => Err(ResourceError::WrongKind {
            uri: uri.clone(),
            expected: ResourceKind::Directory,
            actual: ResourceKind::File,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run<T>(future: impl std::future::Future<Output = T>) -> T {
        pollster::block_on(future)
    }

    #[test]
    fn hierarchy_supports_normalize_enumerate_read_write_and_stat() {
        let root = ResourceUri::parse("mem://workspace/").unwrap();
        let provider = MemoryFileSystemProvider::new(root.clone()).unwrap();
        let directory = ResourceUri::parse("mem://workspace/src/").unwrap();
        let file = ResourceUri::parse("mem://workspace/src/lib%2Ers").unwrap();
        provider.seed_directory(directory.clone()).unwrap();
        provider.seed_file(file.clone(), b"old".to_vec()).unwrap();

        assert_eq!(
            run(provider.normalize(ResourceUri::parse("mem://workspace/src/./lib%2ers").unwrap()))
                .unwrap()
                .to_string(),
            "mem://workspace/src/lib.rs"
        );
        assert_eq!(
            run(provider.stat(file.clone())).unwrap(),
            ResourceStat::File
        );
        assert_eq!(run(provider.read(file.clone())).unwrap(), b"old");
        run(provider.write(file.clone(), b"new".to_vec())).unwrap();
        assert_eq!(run(provider.read(file.clone())).unwrap(), b"new");
        assert_eq!(
            run(provider.enumerate(directory)).unwrap(),
            vec![ResourceEntry {
                uri: ResourceUri::parse("mem://workspace/src/lib.rs").unwrap(),
                name: "lib.rs".into(),
                kind: ResourceKind::File
            }]
        );
    }

    #[test]
    fn rejects_outside_and_wrong_kind_operations() {
        let provider =
            MemoryFileSystemProvider::new(ResourceUri::parse("mem://workspace/project").unwrap())
                .unwrap();
        assert!(matches!(
            run(provider.stat(ResourceUri::parse("mem://workspace/other").unwrap())),
            Err(ResourceError::OutsideWorkspace { .. })
        ));
        assert!(matches!(
            run(provider.read(ResourceUri::parse("mem://workspace/project").unwrap())),
            Err(ResourceError::WrongKind { .. })
        ));
    }
}
