// Injection queries for the injected languages Knot bundles. Adapted from
// nvim-treesitter (Apache-2.0); patterns that need `#offset!`,
// `injection.combined`, or a language without a bundled grammar are left out.

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

export const html = String.raw`
((style_element
  (start_tag) @_no_type_lang
  (raw_text) @injection.content)
  (#not-match? @_no_type_lang "\\slang\\s*=")
  (#not-match? @_no_type_lang "\\stype\\s*=")
  (#set! injection.language "css"))

((style_element
  (start_tag
    (attribute
      (attribute_name) @_type
      (quoted_attribute_value
        (attribute_value) @_css)))
  (raw_text) @injection.content)
  (#eq? @_type "type")
  (#eq? @_css "text/css")
  (#set! injection.language "css"))

; <script>...</script>
((script_element
  (start_tag) @_no_type_lang
  (raw_text) @injection.content)
  (#not-match? @_no_type_lang "\\slang\\s*=")
  (#not-match? @_no_type_lang "\\stype\\s*=")
  (#set! injection.language "javascript"))

; <script type="module">, <script type="text/javascript">
((script_element
  (start_tag
    (attribute
      (attribute_name) @_attr
      (quoted_attribute_value
        (attribute_value) @_type)))
  (raw_text) @injection.content)
  (#eq? @_attr "type")
  (#any-of? @_type "module" "text/javascript" "application/javascript")
  (#set! injection.language "javascript"))

; <script type="importmap">, <script type="application/ld+json">
((script_element
  (start_tag
    (attribute
      (attribute_name) @_attr
      (quoted_attribute_value
        (attribute_value) @_type)))
  (raw_text) @injection.content)
  (#eq? @_attr "type")
  (#any-of? @_type "importmap" "application/json" "application/ld+json")
  (#set! injection.language "json"))

; <a style="color: red">
((attribute
  (attribute_name) @_attr
  (quoted_attribute_value
    (attribute_value) @injection.content))
  (#eq? @_attr "style")
  (#set! injection.language "css"))

; <button onclick="go()">
((attribute
  (attribute_name) @_name
  (quoted_attribute_value
    (attribute_value) @injection.content))
  (#match? @_name "^on[a-z]+$")
  (#set! injection.language "javascript"))
`;

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

export const cpp = String.raw`
; R"json(...)json"
(raw_string_literal
  delimiter: (raw_string_delimiter) @injection.language
  (raw_string_content) @injection.content)
`;
