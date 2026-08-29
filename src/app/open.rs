//! Resource loading for product entry and Open commands.

use std::sync::Arc;

use super::{
    filesystem::{
        FileSystemProviderRegistry, ResourceEntry, ResourceError, ResourceKind, ResourceStat,
    },
    resource::ResourceUri,
};

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum OpenResource {
    Missing(ResourceUri),
    File { uri: ResourceUri, text: String },
    Directory(ResourceUri),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OpenResourceError {
    Provider(ResourceError),
    InvalidUtf8 { uri: ResourceUri },
}

impl std::fmt::Display for OpenResourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => error.fmt(formatter),
            Self::InvalidUtf8 { uri } => write!(formatter, "resource is not valid UTF-8: {uri}"),
        }
    }
}

impl From<ResourceError> for OpenResourceError {
    fn from(error: ResourceError) -> Self {
        Self::Provider(error)
    }
}

pub(crate) async fn load_resource(
    providers: Arc<FileSystemProviderRegistry>,
    uri: ResourceUri,
) -> Result<OpenResource, OpenResourceError> {
    let uri = providers.normalize(uri).await?;
    let provider = providers.provider(&uri)?;
    match provider.stat(uri.clone()).await? {
        ResourceStat::Missing => Ok(OpenResource::Missing(uri)),
        ResourceStat::Directory => Ok(OpenResource::Directory(uri)),
        ResourceStat::File => {
            let bytes = provider.read(uri.clone()).await?;
            let text = String::from_utf8(bytes)
                .map_err(|_| OpenResourceError::InvalidUtf8 { uri: uri.clone() })?;
            Ok(OpenResource::File { uri, text })
        }
    }
}

pub(crate) async fn enumerate_directory(
    providers: Arc<FileSystemProviderRegistry>,
    uri: ResourceUri,
) -> Result<Vec<ResourceEntry>, ResourceError> {
    let uri = providers.normalize(uri).await?;
    let provider = providers.provider(&uri)?;
    match provider.stat(uri.clone()).await? {
        ResourceStat::Directory => provider.enumerate(uri).await,
        ResourceStat::File => Err(ResourceError::WrongKind {
            uri,
            expected: ResourceKind::Directory,
            actual: ResourceKind::File,
        }),
        ResourceStat::Missing => Err(ResourceError::NotFound { uri }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::filesystem::{FileSystemProviderRegistry, MemoryFileSystemProvider};

    fn fixture() -> (Arc<FileSystemProviderRegistry>, ResourceUri) {
        let root = ResourceUri::parse("mem://open/").unwrap();
        let provider = Arc::new(MemoryFileSystemProvider::new(root.clone()).unwrap());
        provider
            .seed_file(
                ResourceUri::parse("mem://open/notes.txt").unwrap(),
                b"hello",
            )
            .unwrap();
        provider
            .seed_file(
                ResourceUri::parse("mem://open/invalid.txt").unwrap(),
                [0xff],
            )
            .unwrap();
        provider
            .seed_directory(ResourceUri::parse("mem://open/src").unwrap())
            .unwrap();
        let mut providers = FileSystemProviderRegistry::new();
        providers.register("mem", provider).unwrap();
        (Arc::new(providers), root)
    }

    #[test]
    fn load_distinguishes_files_missing_destinations_and_directories() {
        let (providers, root) = fixture();
        assert_eq!(
            pollster::block_on(load_resource(
                providers.clone(),
                ResourceUri::parse("mem://open/notes.txt").unwrap(),
            ))
            .unwrap(),
            OpenResource::File {
                uri: ResourceUri::parse("mem://open/notes.txt").unwrap(),
                text: "hello".into(),
            }
        );
        assert_eq!(
            pollster::block_on(load_resource(
                providers.clone(),
                ResourceUri::parse("mem://open/new.txt").unwrap(),
            ))
            .unwrap(),
            OpenResource::Missing(ResourceUri::parse("mem://open/new.txt").unwrap())
        );
        assert_eq!(
            pollster::block_on(load_resource(
                providers,
                ResourceUri::parse("mem://open/src").unwrap(),
            ))
            .unwrap(),
            OpenResource::Directory(ResourceUri::parse("mem://open/src").unwrap())
        );
        assert_eq!(root.to_string(), "mem://open/");
    }

    #[test]
    fn load_reports_decode_and_provider_failures() {
        let (providers, _) = fixture();
        assert!(matches!(
            pollster::block_on(load_resource(
                providers.clone(),
                ResourceUri::parse("mem://open/invalid.txt").unwrap(),
            )),
            Err(OpenResourceError::InvalidUtf8 { .. })
        ));
        assert!(matches!(
            pollster::block_on(load_resource(
                providers,
                ResourceUri::parse("other://open/file.txt").unwrap(),
            )),
            Err(OpenResourceError::Provider(
                ResourceError::ProviderNotFound { .. }
            ))
        ));
    }
}
