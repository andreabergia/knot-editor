use std::{collections::HashMap, fmt, future::Future, pin::Pin, sync::Arc};

use super::resource::ResourceUri;

mod local;
mod memory;

pub(crate) use local::{LocalFileSystemProvider, normalize_file_uri};
#[cfg(test)]
pub(crate) use memory::MemoryFileSystemProvider;

pub(crate) type ProviderFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ResourceError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourceEntry {
    pub(crate) uri: ResourceUri,
    pub(crate) name: String,
    pub(crate) kind: ResourceKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResourceStat {
    Missing,
    File,
    Directory,
}

/// Opaque provider-owned identity for one observed file version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourceVersion(Vec<u8>);

impl ResourceVersion {
    pub(crate) fn new(value: impl Into<Vec<u8>>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourceFile {
    pub(crate) bytes: Vec<u8>,
    pub(crate) version: ResourceVersion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResourceError {
    InvalidUri {
        uri: String,
        reason: String,
    },
    OutsideWorkspace {
        uri: ResourceUri,
        root: ResourceUri,
    },
    ProviderNotFound {
        scheme: String,
    },
    SchemeAlreadyRegistered {
        scheme: String,
    },
    NotFound {
        uri: ResourceUri,
    },
    AlreadyExists {
        uri: ResourceUri,
    },
    Conflict {
        uri: ResourceUri,
    },
    WrongKind {
        uri: ResourceUri,
        expected: ResourceKind,
        actual: ResourceKind,
    },
    Io {
        operation: &'static str,
        message: String,
    },
}

impl ResourceError {
    fn io(operation: &'static str, error: impl fmt::Display) -> Self {
        Self::Io {
            operation,
            message: error.to_string(),
        }
    }
}

impl fmt::Display for ResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUri { uri, reason } => {
                write!(formatter, "invalid resource URI {uri:?}: {reason}")
            }
            Self::OutsideWorkspace { uri, root } => {
                write!(formatter, "resource {uri} is outside workspace {root}")
            }
            Self::ProviderNotFound { scheme } => write!(
                formatter,
                "no filesystem provider is registered for {scheme:?}"
            ),
            Self::SchemeAlreadyRegistered { scheme } => write!(
                formatter,
                "a filesystem provider is already registered for {scheme:?}"
            ),
            Self::NotFound { uri } => write!(formatter, "resource not found: {uri}"),
            Self::AlreadyExists { uri } => write!(formatter, "resource already exists: {uri}"),
            Self::Conflict { uri } => write!(formatter, "resource changed outside Knot: {uri}"),
            Self::WrongKind {
                uri,
                expected,
                actual,
            } => write!(
                formatter,
                "resource {uri} is {actual:?}, expected {expected:?}"
            ),
            Self::Io { operation, message } => {
                write!(formatter, "filesystem {operation} failed: {message}")
            }
        }
    }
}

impl std::error::Error for ResourceError {}

pub(crate) trait FileSystemProvider: Send + Sync {
    fn normalize(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceUri>;
    fn enumerate(&self, uri: ResourceUri) -> ProviderFuture<'_, Vec<ResourceEntry>>;
    fn read(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceFile>;
    fn create(&self, uri: ResourceUri, bytes: Vec<u8>) -> ProviderFuture<'_, ResourceVersion>;
    fn replace(
        &self,
        uri: ResourceUri,
        expected: ResourceVersion,
        bytes: Vec<u8>,
    ) -> ProviderFuture<'_, ResourceVersion>;
    fn stat(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceStat>;
}

pub(crate) struct FileSystemProviderRegistry {
    providers: HashMap<String, Arc<dyn FileSystemProvider>>,
}

impl FileSystemProviderRegistry {
    pub(crate) fn new() -> Self {
        Self {
            providers: HashMap::new(),
        }
    }

    pub(crate) fn register(
        &mut self,
        scheme: impl Into<String>,
        provider: Arc<dyn FileSystemProvider>,
    ) -> Result<(), ResourceError> {
        let scheme = scheme.into().to_ascii_lowercase();
        let mut characters = scheme.chars();
        if !characters
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic())
            || !characters.all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
            })
        {
            return Err(ResourceError::InvalidUri {
                uri: scheme,
                reason: "provider scheme is invalid".into(),
            });
        }
        if self.providers.contains_key(&scheme) {
            return Err(ResourceError::SchemeAlreadyRegistered { scheme });
        }
        self.providers.insert(scheme, provider);
        Ok(())
    }

    pub(crate) fn provider(
        &self,
        uri: &ResourceUri,
    ) -> Result<Arc<dyn FileSystemProvider>, ResourceError> {
        self.providers
            .get(uri.scheme())
            .cloned()
            .ok_or_else(|| ResourceError::ProviderNotFound {
                scheme: uri.scheme().to_owned(),
            })
    }

    pub(crate) fn normalize(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceUri> {
        match self.provider(&uri) {
            Ok(provider) => Box::pin(async move { provider.normalize(uri).await }),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
}

fn validate_and_normalize_uri(
    uri: ResourceUri,
    scheme: &str,
) -> Result<ResourceUri, ResourceError> {
    let source = uri.to_string();
    let url = uri.as_url();
    if url.scheme() != scheme {
        return Err(ResourceError::InvalidUri {
            uri: source,
            reason: format!("expected {scheme:?} scheme"),
        });
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ResourceError::InvalidUri {
            uri: source,
            reason: "userinfo, ports, queries, and fragments are unsupported".into(),
        });
    }

    let mut decoded = Vec::new();
    for segment in url
        .path_segments()
        .expect("ResourceUri rejects non-hierarchical URIs")
    {
        if segment.is_empty() {
            continue;
        }
        let segment = percent_encoding::percent_decode_str(segment)
            .decode_utf8()
            .map_err(|error| ResourceError::InvalidUri {
                uri: source.clone(),
                reason: error.to_string(),
            })?
            .into_owned();
        if segment.contains(['/', '\\', '\0']) {
            return Err(ResourceError::InvalidUri {
                uri: source,
                reason: "encoded path separators and NUL are unsupported".into(),
            });
        }
        match segment.as_str() {
            "." => {}
            ".." => {
                decoded.pop();
            }
            _ => decoded.push(segment),
        }
    }

    let mut normalized = url.clone();
    normalized.set_path("/");
    {
        let mut target = normalized
            .path_segments_mut()
            .map_err(|_| ResourceError::InvalidUri {
                uri: source.clone(),
                reason: "URI cannot contain hierarchical path segments".into(),
            })?;
        target.clear();
        target.extend(decoded.iter().map(String::as_str));
    }
    if !decoded.is_empty() {
        let path = normalized.path().trim_end_matches('/').to_owned();
        normalized.set_path(&path);
    }
    Ok(ResourceUri::from_url(normalized))
}

fn child_uri(parent: &ResourceUri, name: &str) -> Result<ResourceUri, ResourceError> {
    let mut url = parent.as_url().clone();
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| ResourceError::InvalidUri {
                uri: parent.to_string(),
                reason: "URI cannot contain hierarchical path segments".into(),
            })?;
        segments.pop_if_empty();
        segments.push(name);
    }
    Ok(ResourceUri::from_url(url))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    struct IdentityProvider;

    impl FileSystemProvider for IdentityProvider {
        fn normalize(&self, uri: ResourceUri) -> ProviderFuture<'_, ResourceUri> {
            Box::pin(async move { Ok(uri) })
        }
        fn enumerate(&self, _: ResourceUri) -> ProviderFuture<'_, Vec<ResourceEntry>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn read(&self, _: ResourceUri) -> ProviderFuture<'_, ResourceFile> {
            Box::pin(async {
                Ok(ResourceFile {
                    bytes: Vec::new(),
                    version: ResourceVersion::new([]),
                })
            })
        }
        fn create(&self, _: ResourceUri, _: Vec<u8>) -> ProviderFuture<'_, ResourceVersion> {
            Box::pin(async { Ok(ResourceVersion::new([])) })
        }
        fn replace(
            &self,
            _: ResourceUri,
            _: ResourceVersion,
            _: Vec<u8>,
        ) -> ProviderFuture<'_, ResourceVersion> {
            Box::pin(async { Ok(ResourceVersion::new([])) })
        }
        fn stat(&self, _: ResourceUri) -> ProviderFuture<'_, ResourceStat> {
            Box::pin(async { Ok(ResourceStat::Missing) })
        }
    }

    #[test]
    fn registry_routes_by_scheme_and_rejects_duplicates() {
        let mut registry = FileSystemProviderRegistry::new();
        assert!(matches!(
            registry.register("not a scheme", Arc::new(IdentityProvider)),
            Err(ResourceError::InvalidUri { .. })
        ));
        registry
            .register("MEM", Arc::new(IdentityProvider))
            .unwrap();
        assert!(matches!(
            registry.register("mem", Arc::new(IdentityProvider)),
            Err(ResourceError::SchemeAlreadyRegistered { .. })
        ));
        assert!(
            registry
                .provider(&ResourceUri::parse("mem://workspace/").unwrap())
                .is_ok()
        );
        assert!(matches!(
            registry.provider(&ResourceUri::parse("file:///tmp").unwrap()),
            Err(ResourceError::ProviderNotFound { .. })
        ));
    }

    fn run<T>(future: impl Future<Output = T>) -> T {
        pollster::block_on(future)
    }

    fn assert_provider_contract(
        provider: &dyn FileSystemProvider,
        root: ResourceUri,
        directory: ResourceUri,
        file: ResourceUri,
        missing: ResourceUri,
    ) {
        assert_eq!(
            run(provider.stat(root.clone())).unwrap(),
            ResourceStat::Directory
        );
        assert_eq!(
            run(provider.stat(directory.clone())).unwrap(),
            ResourceStat::Directory
        );
        assert_eq!(
            run(provider.stat(file.clone())).unwrap(),
            ResourceStat::File
        );
        assert_eq!(
            run(provider.stat(missing.clone())).unwrap(),
            ResourceStat::Missing
        );

        let before = run(provider.read(file.clone())).unwrap();
        assert_eq!(before.bytes, b"before");
        let stale_version = before.version.clone();
        let after_version =
            run(provider.replace(file.clone(), before.version, b"after".to_vec())).unwrap();
        let after = run(provider.read(file.clone())).unwrap();
        assert_eq!(after.bytes, b"after");
        assert_eq!(after.version, after_version);
        assert!(matches!(
            run(provider.replace(file.clone(), stale_version, b"stale".to_vec())),
            Err(ResourceError::Conflict { uri }) if uri == file
        ));
        assert_eq!(run(provider.read(file.clone())).unwrap().bytes, b"after");
        assert_eq!(
            run(provider.enumerate(directory.clone())).unwrap(),
            vec![ResourceEntry {
                uri: file.clone(),
                name: "file.txt".into(),
                kind: ResourceKind::File,
            }]
        );
        let created = child_uri(&directory, "created.txt").unwrap();
        run(provider.create(created.clone(), b"created".to_vec())).unwrap();
        assert_eq!(
            run(provider.read(created.clone())).unwrap().bytes,
            b"created"
        );
        assert!(
            matches!(run(provider.create(created.clone(), Vec::new())), Err(ResourceError::AlreadyExists { uri }) if uri == created)
        );

        assert!(matches!(
            run(provider.enumerate(file.clone())),
            Err(ResourceError::WrongKind {
                expected: ResourceKind::Directory,
                actual: ResourceKind::File,
                ..
            })
        ));
        assert!(matches!(
            run(provider.read(directory)),
            Err(ResourceError::WrongKind {
                expected: ResourceKind::File,
                actual: ResourceKind::Directory,
                ..
            })
        ));
        assert!(matches!(
            run(provider.read(missing.clone())),
            Err(ResourceError::NotFound { uri }) if uri == missing
        ));
        assert!(matches!(
            run(provider.replace(missing.clone(), ResourceVersion::new([]), b"new".to_vec())),
            Err(ResourceError::NotFound { uri }) if uri == missing
        ));
    }

    #[test]
    fn memory_provider_satisfies_the_shared_contract() {
        let root = ResourceUri::parse("mem://workspace/").unwrap();
        let directory = ResourceUri::parse("mem://workspace/directory").unwrap();
        let file = ResourceUri::parse("mem://workspace/directory/file.txt").unwrap();
        let missing = ResourceUri::parse("mem://workspace/directory/missing.txt").unwrap();
        let provider = MemoryFileSystemProvider::new(root.clone()).unwrap();
        provider.seed_directory(directory.clone()).unwrap();
        provider
            .seed_file(file.clone(), b"before".to_vec())
            .unwrap();

        assert_provider_contract(&provider, root, directory, file, missing);
    }

    #[test]
    fn local_provider_satisfies_the_shared_contract() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir(temporary.path().join("directory")).unwrap();
        fs::write(temporary.path().join("directory/file.txt"), b"before").unwrap();
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        let provider = LocalFileSystemProvider::new(temporary.path(), runtime).unwrap();
        let root = provider.root().clone();
        let directory = child_uri(&root, "directory").unwrap();
        let file = child_uri(&directory, "file.txt").unwrap();
        let missing = child_uri(&directory, "missing.txt").unwrap();

        assert_provider_contract(&provider, root, directory, file, missing);
    }

    #[test]
    fn providers_reject_lexical_dot_escape_after_uri_resolution() {
        let memory =
            MemoryFileSystemProvider::new(ResourceUri::parse("mem://workspace/project").unwrap())
                .unwrap();
        let memory_escape = ResourceUri::parse("mem://workspace/project/../outside.txt").unwrap();
        assert!(matches!(
            run(memory.normalize(memory_escape)),
            Err(ResourceError::OutsideWorkspace { .. })
        ));

        let temporary = tempfile::tempdir().unwrap();
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        let local = LocalFileSystemProvider::new(temporary.path(), runtime).unwrap();
        let local_escape = ResourceUri::parse(&format!("{}/../outside.txt", local.root())).unwrap();
        assert!(matches!(
            run(local.normalize(local_escape)),
            Err(ResourceError::OutsideWorkspace { .. })
        ));
    }

    #[test]
    fn providers_canonicalize_equivalent_percent_encoded_paths() {
        let memory =
            MemoryFileSystemProvider::new(ResourceUri::parse("mem://workspace/").unwrap()).unwrap();
        let memory_encoded =
            ResourceUri::parse("mem://workspace/%64irectory/%66ile%2Etxt").unwrap();
        assert_eq!(
            run(memory.normalize(memory_encoded)).unwrap(),
            ResourceUri::parse("mem://workspace/directory/file.txt").unwrap()
        );

        let temporary = tempfile::tempdir().unwrap();
        let runtime = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        let local = LocalFileSystemProvider::new(temporary.path(), runtime).unwrap();
        let encoded =
            ResourceUri::parse(&format!("{}/%64irectory/%66ile%2Etxt", local.root())).unwrap();
        assert_eq!(
            run(local.normalize(encoded)).unwrap(),
            child_uri(&child_uri(local.root(), "directory").unwrap(), "file.txt").unwrap()
        );
    }
}
