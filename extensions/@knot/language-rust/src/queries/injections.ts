// Injection queries for the injected languages Knot bundles. Adapted from
// nvim-treesitter (Apache-2.0); see extensions/@knot/LICENSE-APACHE. Patterns
// that need `#offset!`, `injection.combined`, or a language without a bundled
// grammar are left out.

export const rust = String.raw`
(macro_invocation
  macro: [
    (scoped_identifier
      name: (_) @_macro_name)
    (identifier) @_macro_name
  ]
  (token_tree) @injection.content
  (#not-any-of? @_macro_name "slint" "html" "json" "xml")
  (#set! injection.language "rust")
  (#set! injection.include-children))

(macro_definition
  (macro_rule
    left: (token_tree_pattern) @injection.content
    (#set! injection.language "rust")))

(macro_definition
  (macro_rule
    right: (token_tree) @injection.content
    (#set! injection.language "rust")))
`;
