// The capture styles every bundled theme shares, from a palette of roles, so
// themes agree on what each capture means and differ only in color.
import type { ThemeSyntaxStyle } from "knot";

export type SyntaxPalette = {
  comment: string;
  keyword: string;
  function: string;
  /** Builtins, escapes, operators, and other special forms. */
  special: string;
  /** Types, constructors, and modules. */
  type: string;
  string: string;
  /** Numbers, booleans, constants, and attributes. */
  constant: string;
  /** Properties, tags, labels, and headings. */
  member: string;
  link: string;
};

export function syntax(palette: SyntaxPalette): Record<string, ThemeSyntaxStyle> {
  const color = (value: string): ThemeSyntaxStyle => ({ color: value });
  return {
    attribute: color(palette.constant),
    boolean: color(palette.constant),
    character: color(palette.string),
    "character.special": color(palette.special),
    comment: { color: palette.comment, fontStyle: "italic" },
    constant: color(palette.constant),
    constructor: color(palette.type),
    "diff.delta": color(palette.constant),
    "diff.minus": color(palette.member),
    "diff.plus": color(palette.string),
    function: color(palette.function),
    "function.builtin": color(palette.special),
    keyword: color(palette.keyword),
    label: color(palette.member),
    "markup.heading": { color: palette.member, fontWeight: 700 },
    "markup.link": color(palette.link),
    "markup.link.url": { color: palette.special, underline: true },
    "markup.list": color(palette.member),
    "markup.quote": { color: palette.comment, fontStyle: "italic" },
    "markup.raw": color(palette.string),
    module: color(palette.type),
    number: color(palette.constant),
    operator: color(palette.special),
    property: color(palette.member),
    string: color(palette.string),
    "string.escape": color(palette.special),
    "string.regexp": color(palette.special),
    "string.special": color(palette.special),
    tag: color(palette.member),
    "tag.attribute": color(palette.constant),
    type: color(palette.type),
    "variable.builtin": color(palette.member),
    "variable.member": color(palette.member),
  };
}
