//! Platform entry requests for the product application.

use std::path::{Path, PathBuf};

use url::Url;

use super::{filesystem::normalize_file_uri, resource::ResourceUri};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OpenRequest {
    uri: ResourceUri,
}

impl OpenRequest {
    pub(crate) fn from_path(path: &Path, current_dir: &Path) -> Result<Self, String> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            current_dir.join(path)
        };
        let url = Url::from_file_path(&path).map_err(|_| {
            format!(
                "path cannot be represented as a file URI: {}",
                path.display()
            )
        })?;
        Self::from_uri(ResourceUri::from_url(url))
    }

    pub(crate) fn from_url(url: &str) -> Result<Self, String> {
        let uri = ResourceUri::parse(url).map_err(|error| error.to_string())?;
        Self::from_uri(uri)
    }

    fn from_uri(uri: ResourceUri) -> Result<Self, String> {
        normalize_file_uri(uri)
            .map(|uri| Self { uri })
            .map_err(|error| error.to_string())
    }

    pub(crate) fn from_uri_for_product(uri: ResourceUri) -> Self {
        Self { uri }
    }

    pub(crate) fn uri(&self) -> &ResourceUri {
        &self.uri
    }

    #[cfg(test)]
    pub(crate) fn title(&self) -> String {
        self.uri
            .as_url()
            .path_segments()
            .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
            .map(percent_encoding::percent_decode_str)
            .and_then(|name| name.decode_utf8().ok())
            .filter(|name| !name.is_empty())
            .map(|name| name.into_owned())
            .unwrap_or_else(|| self.uri.to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LaunchConfiguration {
    Product(Option<OpenRequest>),
    TerminalFixture,
}

impl LaunchConfiguration {
    pub(crate) fn parse(
        arguments: impl IntoIterator<Item = String>,
        current_dir: &Path,
    ) -> Result<Self, String> {
        let mut arguments = arguments.into_iter();
        let _executable = arguments.next();
        let Some(first) = arguments.next() else {
            return Ok(Self::Product(None));
        };

        if first == "--terminal-fixture" {
            if arguments.next().is_some() {
                return Err("usage: knot [path] | knot --terminal-fixture".into());
            }
            return Ok(Self::TerminalFixture);
        }

        if arguments.next().is_some() {
            return Err("usage: knot [path] | knot --terminal-fixture".into());
        }
        Ok(Self::Product(Some(OpenRequest::from_path(
            &PathBuf::from(first),
            current_dir,
        )?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_defaults_to_the_product_without_a_target() {
        assert_eq!(
            LaunchConfiguration::parse(["knot".into()], Path::new("/tmp")).unwrap(),
            LaunchConfiguration::Product(None)
        );
    }

    #[test]
    fn launch_accepts_exactly_one_path_and_normalizes_it_at_the_boundary() {
        let configuration = LaunchConfiguration::parse(
            ["knot".into(), "project/../notes ü.md".into()],
            Path::new("/tmp"),
        )
        .unwrap();
        let LaunchConfiguration::Product(Some(request)) = configuration else {
            panic!("expected a product open request");
        };

        assert_eq!(request.uri().to_string(), "file:///tmp/notes%20%C3%BC.md");
        assert_eq!(request.title(), "notes ü.md");
        assert!(
            LaunchConfiguration::parse(
                ["knot".into(), "one".into(), "two".into()],
                Path::new("/tmp")
            )
            .is_err()
        );
    }

    #[test]
    fn terminal_fixture_requires_an_explicit_flag() {
        assert_eq!(
            LaunchConfiguration::parse(
                ["knot".into(), "--terminal-fixture".into()],
                Path::new("/tmp")
            )
            .unwrap(),
            LaunchConfiguration::TerminalFixture
        );
    }

    #[test]
    fn macos_open_urls_enter_as_the_same_normalized_request() {
        let request = OpenRequest::from_url("file:///tmp/project/../notes%20%C3%BC.md").unwrap();

        assert_eq!(request.uri().to_string(), "file:///tmp/notes%20%C3%BC.md");
        assert_eq!(request.title(), "notes ü.md");
        assert!(OpenRequest::from_url("https://example.com/file.txt").is_err());
    }
}
