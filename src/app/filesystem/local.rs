use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use url::Url;

use super::{
    FileSystemProvider, ProviderFuture, ResourceEntry, ResourceError, ResourceKind, ResourceStat,
    validate_and_normalize_uri,
};
use crate::app::resource::ResourceUri;

/// A provider for one trusted local folder.
///
/// Containment is lexical. Filesystem operations follow symlinks, including
/// symlinks whose targets are outside the selected folder.
pub(crate) struct LocalFileSystemProvider {
    root: Option<ResourceUri>,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl LocalFileSystemProvider {
    pub(crate) fn new(
        root_path: impl AsRef<Path>,
        runtime: Arc<tokio::runtime::Runtime>,
    ) -> Result<Self, ResourceError> {
        let root_path = root_path.as_ref().to_path_buf();
        if !root_path.is_absolute() {
            return Err(ResourceError::InvalidUri {
                uri: root_path.display().to_string(),
                reason: "local provider root must be an absolute path".into(),
            });
        }
        let root_url = Url::from_file_path(&root_path).map_err(|_| ResourceError::InvalidUri {
            uri: root_path.display().to_string(),
            reason: "local provider root cannot be represented as a file URI".into(),
        })?;
        let root = normalize_file_uri(ResourceUri::from_url(root_url))?;
        Ok(Self {
            root: Some(root),
            runtime,
        })
    }

    /// Creates the application-wide provider for user-selected local resources.
    /// Workspace containment remains a workspace consumer concern.
    pub(crate) fn unrestricted(runtime: Arc<tokio::runtime::Runtime>) -> Self {
        Self {
            root: None,
            runtime,
        }
    }

    pub(crate) fn root(&self) -> &ResourceUri {
        self.root
            .as_ref()
            .expect("an unrestricted local provider has no root")
    }

    fn normalize_scoped(&self, uri: ResourceUri) -> Result<ResourceUri, ResourceError> {
        let uri = normalize_file_uri(uri)?;
        if self.root.as_ref().is_some_and(|root| !uri.is_within(root)) {
            return Err(ResourceError::OutsideWorkspace {
                uri,
                root: self.root.clone().unwrap(),
            });
        }
        Ok(uri)
    }

    fn uri_to_path(&self, uri: &ResourceUri) -> Result<PathBuf, ResourceError> {
        uri.as_url()
            .to_file_path()
            .map_err(|_| ResourceError::InvalidUri {
                uri: uri.to_string(),
                reason: "file URI cannot be represented as a platform path".into(),
            })
    }

    #[cfg(test)]
    fn path_to_uri(&self, path: &Path) -> Result<ResourceUri, ResourceError> {
        path_to_uri(self.root(), path)
    }

    fn spawn<T: Send + 'static>(
        &self,
        operation: &'static str,
        future: impl std::future::Future<Output = Result<T, ResourceError>> + Send + 'static,
    ) -> ProviderFuture<'_, T> {
        let task = self.runtime.spawn(future);
        Box::pin(async move {
            task.await
                .map_err(|error| ResourceError::io(operation, error))?
        })
    }
}

fn path_to_uri(root: &ResourceUri, path: &Path) -> Result<ResourceUri, ResourceError> {
    let url = Url::from_file_path(path).map_err(|_| ResourceError::InvalidUri {
        uri: path.display().to_string(),
        reason: "platform path cannot be represented as a file URI".into(),
    })?;
    let uri = normalize_file_uri(ResourceUri::from_url(url))?;
    if !uri.is_within(root) {
        return Err(ResourceError::OutsideWorkspace {
            uri,
            root: root.clone(),
        });
    }
    Ok(uri)
}

fn path_to_unrestricted_uri(path: &Path) -> Result<ResourceUri, ResourceError> {
    let url = Url::from_file_path(path).map_err(|_| ResourceError::InvalidUri {
        uri: path.display().to_string(),
        reason: "platform path cannot be represented as a file URI".into(),
    })?;
    normalize_file_uri(ResourceUri::from_url(url))
}

impl FileSystemProvider for LocalFileSystemProvider {
    fn normalize(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceUri> {
        Box::pin(async move { self.normalize_scoped(uri) })
    }

    fn enumerate(&self, uri: ResourceUri) -> ProviderFuture<'_, Vec<ResourceEntry>> {
        let uri = match self.normalize_scoped(uri) {
            Ok(uri) => uri,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let path = match self.uri_to_path(&uri) {
            Ok(path) => path,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let root = self.root.clone();
        self.spawn("enumerate task", async move {
            let directory_metadata = metadata(&path, &uri, "stat directory").await?;
            if !directory_metadata.is_dir() {
                return Err(ResourceError::WrongKind {
                    uri,
                    expected: ResourceKind::Directory,
                    actual: ResourceKind::File,
                });
            }
            let mut directory = tokio::fs::read_dir(&path)
                .await
                .map_err(|error| map_io("enumerate directory", &uri, error))?;
            let mut entries = Vec::new();
            while let Some(entry) = directory
                .next_entry()
                .await
                .map_err(|error| ResourceError::io("enumerate directory", error))?
            {
                let name =
                    entry
                        .file_name()
                        .into_string()
                        .map_err(|name| ResourceError::InvalidUri {
                            uri: entry.path().display().to_string(),
                            reason: format!("resource name is not UTF-8: {name:?}"),
                        })?;
                let child_uri = match &root {
                    Some(root) => path_to_uri(root, &entry.path())?,
                    None => path_to_unrestricted_uri(&entry.path())?,
                };
                let child_metadata = metadata(&entry.path(), &child_uri, "stat child").await?;
                let kind = if child_metadata.is_dir() {
                    ResourceKind::Directory
                } else if child_metadata.is_file() {
                    ResourceKind::File
                } else {
                    return Err(ResourceError::Io {
                        operation: "stat child",
                        message: format!("unsupported resource kind at {child_uri}"),
                    });
                };
                entries.push(ResourceEntry {
                    uri: child_uri,
                    name,
                    kind,
                });
            }
            Ok(entries)
        })
    }

    fn read(&self, uri: ResourceUri) -> ProviderFuture<'_, Vec<u8>> {
        let uri = match self.normalize_scoped(uri) {
            Ok(uri) => uri,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let path = match self.uri_to_path(&uri) {
            Ok(path) => path,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        self.spawn("read task", async move {
            let metadata = metadata(&path, &uri, "stat file").await?;
            if !metadata.is_file() {
                return Err(ResourceError::WrongKind {
                    uri,
                    expected: ResourceKind::File,
                    actual: ResourceKind::Directory,
                });
            }
            tokio::fs::read(path)
                .await
                .map_err(|error| map_io("read", &uri, error))
        })
    }

    fn write(&self, uri: ResourceUri, bytes: Vec<u8>) -> ProviderFuture<'_, ()> {
        let uri = match self.normalize_scoped(uri) {
            Ok(uri) => uri,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let path = match self.uri_to_path(&uri) {
            Ok(path) => path,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        self.spawn("write task", async move {
            let metadata = metadata(&path, &uri, "stat file").await?;
            if !metadata.is_file() {
                return Err(ResourceError::WrongKind {
                    uri,
                    expected: ResourceKind::File,
                    actual: ResourceKind::Directory,
                });
            }
            tokio::fs::write(path, bytes)
                .await
                .map_err(|error| map_io("write", &uri, error))
        })
    }

    fn stat(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceStat> {
        let uri = match self.normalize_scoped(uri) {
            Ok(uri) => uri,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        let path = match self.uri_to_path(&uri) {
            Ok(path) => path,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        self.spawn("stat task", async move {
            match tokio::fs::metadata(path).await {
                Ok(metadata) if metadata.is_file() => Ok(ResourceStat::File),
                Ok(metadata) if metadata.is_dir() => Ok(ResourceStat::Directory),
                Ok(_) => Err(ResourceError::Io {
                    operation: "stat",
                    message: format!("unsupported resource kind at {uri}"),
                }),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Ok(ResourceStat::Missing)
                }
                Err(error) => Err(ResourceError::io("stat", error)),
            }
        })
    }
}

pub(crate) fn normalize_file_uri(uri: ResourceUri) -> Result<ResourceUri, ResourceError> {
    if uri.as_url().host_str().is_some() {
        return Err(ResourceError::InvalidUri {
            uri: uri.to_string(),
            reason: "file URI authorities are unsupported".into(),
        });
    }
    validate_and_normalize_uri(uri, "file")
}

async fn metadata(
    path: &Path,
    uri: &ResourceUri,
    operation: &'static str,
) -> Result<std::fs::Metadata, ResourceError> {
    tokio::fs::metadata(path)
        .await
        .map_err(|error| map_io(operation, uri, error))
}

fn map_io(operation: &'static str, uri: &ResourceUri, error: std::io::Error) -> ResourceError {
    if error.kind() == std::io::ErrorKind::NotFound {
        ResourceError::NotFound { uri: uri.clone() }
    } else {
        ResourceError::io(operation, error)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn run<T>(future: impl std::future::Future<Output = T>) -> T {
        pollster::block_on(future)
    }

    fn io_runtime() -> Arc<tokio::runtime::Runtime> {
        Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap(),
        )
    }

    #[test]
    fn local_folder_supports_normalize_enumerate_read_write_and_stat() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("src")).unwrap();
        fs::write(temporary.path().join("src/lib.rs"), b"old").unwrap();
        let provider = LocalFileSystemProvider::new(temporary.path(), io_runtime()).unwrap();
        let directory = provider.path_to_uri(&temporary.path().join("src")).unwrap();
        let file = provider
            .path_to_uri(&temporary.path().join("src/lib.rs"))
            .unwrap();

        assert_eq!(
            run(provider.stat(file.clone())).unwrap(),
            ResourceStat::File
        );
        assert_eq!(run(provider.read(file.clone())).unwrap(), b"old");
        run(provider.write(file.clone(), b"new".to_vec())).unwrap();
        assert_eq!(
            fs::read(temporary.path().join("src/lib.rs")).unwrap(),
            b"new"
        );
        assert_eq!(
            run(provider.enumerate(directory)).unwrap(),
            vec![ResourceEntry {
                uri: file,
                name: "lib.rs".into(),
                kind: ResourceKind::File
            }]
        );
    }

    #[test]
    fn rejects_outside_and_wrong_kind_operations() {
        let temporary = tempfile::tempdir().unwrap();
        let provider = LocalFileSystemProvider::new(temporary.path(), io_runtime()).unwrap();
        assert!(matches!(
            run(provider.read(provider.root().clone())),
            Err(ResourceError::WrongKind { .. })
        ));
        assert!(matches!(
            run(provider.stat(ResourceUri::parse("file:///definitely/outside").unwrap())),
            Err(ResourceError::OutsideWorkspace { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlinks_outside_the_trusted_root() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("target"), b"outside").unwrap();
        symlink(outside.path().join("target"), root.path().join("link")).unwrap();
        let provider = LocalFileSystemProvider::new(root.path(), io_runtime()).unwrap();
        let link = provider.path_to_uri(&root.path().join("link")).unwrap();
        assert_eq!(run(provider.read(link)).unwrap(), b"outside");
    }
}
