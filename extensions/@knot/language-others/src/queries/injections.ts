// Injection queries for the injected languages Knot bundles. Adapted from
// nvim-treesitter (Apache-2.0); patterns that need `#offset!`,
// `injection.combined`, or a language without a bundled grammar are left out.

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

export const cpp = String.raw`
; R"json(...)json"
(raw_string_literal
  delimiter: (raw_string_delimiter) @injection.language
  (raw_string_content) @injection.content)
`;
