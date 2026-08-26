use std::{fmt, str::FromStr};

use url::Url;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourceUriError {
    input: String,
    reason: String,
}

impl fmt::Display for ResourceUriError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid resource URI {:?}: {}",
            self.input, self.reason
        )
    }
}

impl std::error::Error for ResourceUriError {}

/// A parsed absolute resource identity.
///
/// Scheme-specific validity and canonicalization belong to filesystem
/// providers. This type preserves the generic URI boundary and provides
/// segment-aware containment for already-normalized identities.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ResourceUri(Url);

impl ResourceUri {
    pub(crate) fn parse(input: &str) -> Result<Self, ResourceUriError> {
        let url = Url::parse(input).map_err(|error| ResourceUriError {
            input: input.to_owned(),
            reason: error.to_string(),
        })?;
        if url.cannot_be_a_base() {
            return Err(ResourceUriError {
                input: input.to_owned(),
                reason: "resource URI must be hierarchical".into(),
            });
        }
        Ok(Self(url))
    }

    pub(crate) fn scheme(&self) -> &str {
        self.0.scheme()
    }

    pub(crate) fn as_url(&self) -> &Url {
        &self.0
    }

    pub(crate) fn is_within(&self, root: &Self) -> bool {
        self.same_authority(root) && path_segments(root).is_prefix_of(&path_segments(self))
    }

    pub(crate) fn is_descendant_of(&self, root: &Self) -> bool {
        self.is_within(root) && path_segments(self).len() > path_segments(root).len()
    }

    pub(super) fn from_url(url: Url) -> Self {
        Self(url)
    }

    fn same_authority(&self, other: &Self) -> bool {
        self.0.scheme() == other.0.scheme()
            && self.0.username() == other.0.username()
            && self.0.password() == other.0.password()
            && self.0.host_str() == other.0.host_str()
            && self.0.port() == other.0.port()
    }
}

impl fmt::Display for ResourceUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for ResourceUri {
    type Err = ResourceUriError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

fn path_segments(uri: &ResourceUri) -> UriPathSegments<'_> {
    UriPathSegments {
        segments: uri
            .0
            .path_segments()
            .expect("ResourceUri rejects non-hierarchical URIs")
            .filter(|segment| !segment.is_empty())
            .collect(),
    }
}

struct UriPathSegments<'a> {
    segments: Vec<&'a str>,
}

impl UriPathSegments<'_> {
    fn len(&self) -> usize {
        self.segments.len()
    }

    fn is_prefix_of(&self, target: &Self) -> bool {
        target.segments.starts_with(&self.segments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_an_absolute_hierarchical_uri() {
        assert!(ResourceUri::parse("relative/path").is_err());
        assert!(ResourceUri::parse("mailto:user@example.com").is_err());
        assert_eq!(
            ResourceUri::parse("MEM://workspace/a/../b")
                .unwrap()
                .to_string(),
            "mem://workspace/b"
        );
    }

    #[test]
    fn containment_compares_authority_and_complete_segments() {
        let root = ResourceUri::parse("mem://workspace/project/").unwrap();
        assert!(
            ResourceUri::parse("mem://workspace/project")
                .unwrap()
                .is_within(&root)
        );
        assert!(
            ResourceUri::parse("mem://workspace/project/src/lib.rs")
                .unwrap()
                .is_descendant_of(&root)
        );
        assert!(
            !ResourceUri::parse("mem://workspace/projectile/file")
                .unwrap()
                .is_within(&root)
        );
        assert!(
            !ResourceUri::parse("mem://other/project/file")
                .unwrap()
                .is_within(&root)
        );
    }
}
