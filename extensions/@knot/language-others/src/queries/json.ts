// Adapted from nvim-treesitter (Apache-2.0); see extensions/@knot/LICENSE-APACHE.
export const highlights = String.raw`
[
  (true)
  (false)
] @boolean

(null) @constant.builtin

(number) @number

(pair
  key: (string) @property)

(pair
  value: (string) @string)

(array
  (string) @string)

[
  ","
  ":"
] @punctuation.delimiter

[
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

(escape_sequence) @string.escape

(comment) @comment
`;
