// Injection queries for the injected languages Knot bundles. Adapted from
// nvim-treesitter (Apache-2.0); see extensions/@knot/LICENSE-APACHE. Patterns
// that need `#offset!`, `injection.combined`, or a language without a bundled
// grammar are left out.

export const markdown = String.raw`
(fenced_code_block
  (info_string
    (language) @injection.language)
  (code_fence_content) @injection.content)

((html_block) @injection.content
  (#set! injection.language "html")
  (#set! injection.include-children))

((minus_metadata) @injection.content
  (#set! injection.language "yaml")
  (#set! injection.include-children))

([
  (inline)
  (pipe_table_cell)
] @injection.content
  (#set! injection.language "markdown_inline"))
`;

export const markdownInline = String.raw`
((html_tag) @injection.content
  (#set! injection.language "html"))
`;
