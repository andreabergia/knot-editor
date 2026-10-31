//! Highlights the fixtures in each bundled package's `test/fixtures` with the
//! bundled packages' registered languages and checks their assertions.
//!
//! A fixture is an ordinary source file whose language is detected from its
//! name. An assertion is a line holding only a comment (`//`, `#`, `--`, or
//! `<!--`) followed by an arrow and a highlight name. It checks the nearest
//! line above it that is not an assertion: `^` points at its own column, and
//! `<-` at the column where the comment starts. The name is the painted
//! highlight, or `none` for an unpainted byte.
//!
//! ```text
//! fn main() {}
//! // <- keyword.function
//! //  ^ function
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::TestAppContext;

use crate::app::syntax::SyntaxState;
use crate::core::buffer::TextBuffer;

const COMMENTS: &[&str] = &["//", "#", "--", "<!--"];

struct Assertion {
    /// 1-based, for messages.
    line: usize,
    column: usize,
    /// The byte the assertion checks.
    offset: usize,
    expected: String,
}

/// The column `line` asserts about and its expected name, if it is an
/// assertion.
fn parse_assertion(line: &str) -> Option<(usize, String)> {
    let comment_column = line.len() - line.trim_start().len();
    let rest = &line[comment_column..];
    let comment = COMMENTS
        .iter()
        .find(|comment| rest.starts_with(**comment))?;
    let after = rest[comment.len()..].trim_start();
    let (column, name) = if let Some(name) = after.strip_prefix("<-") {
        (comment_column, name)
    } else {
        (line.len() - after.len(), after.strip_prefix('^')?)
    };
    let name = name.trim().trim_end_matches("-->").trim();
    (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| (column, name.to_owned()))
}

fn assertions(text: &str) -> Vec<Assertion> {
    let mut assertions = Vec::new();
    let mut offset = 0;
    let mut target = None;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let content = line.trim_end_matches(['\n', '\r']);
        match parse_assertion(content) {
            Some((column, expected)) => {
                let (start, length): (usize, usize) =
                    target.expect("an assertion follows a line it can check");
                assert!(
                    column < length,
                    "line {}: the assertion points past the end of its target",
                    index + 1
                );
                assertions.push(Assertion {
                    line: index + 1,
                    column: column + 1,
                    offset: start + column,
                    expected,
                });
            }
            None => target = Some((offset, content.len())),
        }
        offset += line.len();
    }
    assertions
}

fn fixtures() -> Vec<PathBuf> {
    let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/@knot");
    let mut paths: Vec<_> = std::fs::read_dir(bundled)
        .unwrap()
        .map(|package| package.unwrap().path().join("test/fixtures"))
        .filter(|directory| directory.is_dir())
        .flat_map(|directory| std::fs::read_dir(directory).unwrap())
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    paths
}

#[gpui::test]
fn bundled_languages_highlight_their_fixtures(cx: &mut TestAppContext) {
    let (_host, _root) = super::tests::start_bundled_packages(cx);
    let mut failures = Vec::new();
    let mut checked = 0;
    cx.read(|cx| {
        let registry = crate::app::languages::registry(cx);
        for path in fixtures() {
            let name = path.file_name().unwrap().to_str().unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            let first_line = text.lines().next().unwrap_or_default();
            let id = registry
                .detect(Some(name), first_line)
                .unwrap_or_else(|| panic!("{name}: no language detected"));
            let mut state = SyntaxState::new(registry.resolve(&id).unwrap(), registry.resolver());
            let buffer = TextBuffer::from_text(text.as_str());
            assert!(state.parse_in_foreground(&buffer.snapshot(), Duration::from_secs(60)));
            let spans = state.highlights(&buffer.snapshot(), 0..buffer.len());
            let fixture_assertions = assertions(&text);
            assert!(!fixture_assertions.is_empty(), "{name}: no assertions");
            for assertion in fixture_assertions {
                let actual = spans
                    .iter()
                    .find(|(range, _)| range.contains(&assertion.offset))
                    .map_or("none", |(_, highlight)| highlight.name());
                if actual != assertion.expected {
                    let target = text[assertion.offset..].chars().next().unwrap();
                    failures.push(format!(
                        "{name}:{}:{}: expected {} at {target:?}, painted {actual}",
                        assertion.line, assertion.column, assertion.expected
                    ));
                }
                checked += 1;
            }
        }
    });
    assert!(
        failures.is_empty(),
        "{} of {checked} highlight assertions failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn assertions_point_at_their_target_line() {
    let text = "fn main() {}\n// <- keyword\n//  ^ function\n  let\n  # ^ none -->\n";
    let found: Vec<_> = assertions(text)
        .into_iter()
        .map(|assertion| (assertion.line, assertion.offset, assertion.expected))
        .collect();
    assert_eq!(
        found,
        [
            (2, 0, "keyword".to_owned()),
            (3, 4, "function".to_owned()),
            (5, 46, "none".to_owned()),
        ]
    );
}
