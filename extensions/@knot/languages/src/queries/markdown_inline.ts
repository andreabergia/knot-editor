export const highlights = String.raw`
; From MDeiml/tree-sitter-markdown
(code_span) @markup.raw

(emphasis) @markup.italic

(strong_emphasis) @markup.strong

(strikethrough) @markup.strikethrough

[
  (code_span_delimiter)
  (emphasis_delimiter)
] @punctuation.delimiter

(inline_link
  [
    "["
    "]"
    "("
    (link_destination)
    ")"
  ] @markup.link)

[
  (link_label)
  (link_text)
  (link_title)
  (image_description)
] @markup.link.label

(image
  [
    "!"
    "["
    "]"
    "("
    (link_destination)
    ")"
  ] @markup.link)

(full_reference_link
  [
    "["
    "]"
    (link_label)
  ] @markup.link)

(collapsed_reference_link
  [
    "["
    "]"
  ] @markup.link)

(shortcut_link
  [
    "["
    "]"
  ] @markup.link)

[
  (link_destination)
  (uri_autolink)
  (email_autolink)
] @markup.link.url

(backslash_escape) @string.escape

(entity_reference) @character.special
`;
