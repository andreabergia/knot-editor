//! Built-in extension packages compiled into the binary.
//!
//! Bundled packages use the same manifest, identity, dependency, and startup
//! rules as installed packages. Their sources are captured at build time, so
//! discovery never reads them from disk.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use super::extension_package::{DiscoveryReport, PackageDiagnostic, bundled_package};

struct BundledSource {
    manifest: &'static str,
    sources: &'static [(&'static str, &'static str)],
}

const PACKAGES: &[BundledSource] = &[BundledSource {
    manifest: include_str!("../../extensions/@knot/projects/knot.jsonc"),
    sources: &[(
        "dist/main.js",
        include_str!("../../extensions/@knot/projects/dist/main.js"),
    )],
}];

/// Admits every compiled-in package as a discovery result. A malformed bundled
/// manifest is an authoring defect and is reported like an installed one.
pub(crate) fn report() -> DiscoveryReport {
    let mut report = DiscoveryReport::default();
    for package in PACKAGES {
        let sources = package
            .sources
            .iter()
            .map(|(path, source)| ((*path).to_owned(), Arc::from(*source)))
            .collect::<BTreeMap<_, _>>();
        match bundled_package(package.manifest, sources) {
            Ok(package) => report.packages.push(package),
            Err(cause) => report.diagnostics.push(PackageDiagnostic {
                directory: PathBuf::from("<bundled>"),
                name: None,
                cause,
            }),
        }
    }
    report
}
