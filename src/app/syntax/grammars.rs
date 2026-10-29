//! The tree-sitter grammars compiled into Knot, named by stable grammar IDs.
//!
//! Language registrations name a grammar by ID; the grammar itself is native
//! code, so an installed package can only use grammars listed here.

use tree_sitter::Language;

/// A grammar ID and its constructor.
type Grammar = (&'static str, fn() -> Language);

/// Sorted by ID.
const GRAMMARS: &[Grammar] = &[
    ("bash", || tree_sitter_bash::LANGUAGE.into()),
    ("c", || tree_sitter_c::LANGUAGE.into()),
    ("cpp", || tree_sitter_cpp::LANGUAGE.into()),
    ("css", || tree_sitter_css::LANGUAGE.into()),
    ("go", || tree_sitter_go::LANGUAGE.into()),
    ("html", || tree_sitter_html::LANGUAGE.into()),
    ("java", || tree_sitter_java::LANGUAGE.into()),
    ("javascript", || tree_sitter_javascript::LANGUAGE.into()),
    ("json", || tree_sitter_json::LANGUAGE.into()),
    ("markdown", || tree_sitter_md::LANGUAGE.into()),
    ("markdown_inline", || tree_sitter_md::INLINE_LANGUAGE.into()),
    ("python", || tree_sitter_python::LANGUAGE.into()),
    ("query", || tree_sitter_tsquery::LANGUAGE.into()),
    ("rust", || tree_sitter_rust::LANGUAGE.into()),
    ("toml", || tree_sitter_toml_ng::LANGUAGE.into()),
    ("tsx", || tree_sitter_typescript::LANGUAGE_TSX.into()),
    ("typescript", || {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }),
    ("yaml", || tree_sitter_yaml::LANGUAGE.into()),
];

/// The grammar named `id`.
pub(crate) fn grammar(id: &str) -> Option<Language> {
    GRAMMARS
        .binary_search_by_key(&id, |(known, _)| known)
        .ok()
        .map(|index| GRAMMARS[index].1())
}

/// Every compiled-in grammar ID, sorted.
pub(crate) fn grammar_ids() -> impl Iterator<Item = &'static str> {
    GRAMMARS.iter().map(|(id, _)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    #[test]
    fn grammar_ids_are_sorted_and_unique() {
        let ids: Vec<_> = grammar_ids().collect();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(grammar("nope").map(|_| ()), None);
    }

    /// The in-memory extension test host lists the same grammars.
    #[test]
    fn fake_host_lists_every_grammar() {
        let fake = include_str!("../../../scripts/extensions/testing/host.ts")
            .split("const GRAMMARS = [")
            .nth(1)
            .unwrap()
            .split("];")
            .next()
            .unwrap()
            .split('"')
            .skip(1)
            .step_by(2)
            .collect::<Vec<_>>();
        assert_eq!(fake, grammar_ids().collect::<Vec<_>>());
    }

    /// Every grammar's ABI is accepted by the linked tree-sitter runtime and
    /// parses a representative sample without errors.
    #[test]
    fn every_grammar_loads_and_parses() {
        let samples = [
            ("bash", "if [ -n \"$x\" ]; then echo hi; fi\n"),
            ("c", "int main(void) { return 0; }\n"),
            ("cpp", "template <typename T> T id(T x) { return x; }\n"),
            ("css", "a > b { color: red; }\n"),
            ("go", "package main\nfunc main() {}\n"),
            ("html", "<p class=\"x\">hi</p>\n"),
            ("java", "class A { void f() {} }\n"),
            ("javascript", "const x = <div>{1}</div>;\n"),
            ("json", "{\"a\": [1, true, null]}\n"),
            ("markdown", "# Title\n\n- item\n"),
            ("markdown_inline", "some *emphasis* and `code`"),
            ("python", "def f(x):\n    return x\n"),
            ("query", "(identifier) @variable\n"),
            ("rust", "fn main() { let x = 1; }\n"),
            ("toml", "[table]\nkey = \"value\"\n"),
            ("tsx", "const x = <A b={1} />;\n"),
            ("typescript", "let x: number = 1;\n"),
            ("yaml", "key:\n  - value\n"),
        ];
        assert_eq!(
            samples.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            grammar_ids().collect::<Vec<_>>()
        );
        for (id, sample) in samples {
            let mut parser = Parser::new();
            parser
                .set_language(&grammar(id).unwrap())
                .unwrap_or_else(|error| panic!("{id}: {error}"));
            let tree = parser.parse(sample, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{id}: {}",
                tree.root_node().to_sexp()
            );
        }
    }
}
