// One Light, from Atom's one-light-syntax and one-light-ui.
import type { ThemeDefinition } from "knot";
import { syntax } from "../syntax.js";

const bg = "#fafafa";
const panel = "#eaeaeb";
const fg = "#383a42";
const cyan = "#0184bc";
const blue = "#4078f2";
const purple = "#a626a4";
const green = "#50a14f";
const red = "#e45649";
const orange = "#986801";
const yellow = "#c18401";
const accent = "#526fff";

export const ONE_LIGHT: ThemeDefinition = {
  name: "One Light",
  appearance: "light",
  colors: {
    "background": panel,
    "border": "#dbdbdc",
    "border.hover": accent,
    "button.background": "#dbdbdc",
    "button.hover.background": "#cfcfd0",
    "dropTarget.background": "#526fff33",
    "editor.background": bg,
    "editor.cursor": accent,
    "editor.foreground": fg,
    "editor.markedText": "#383a4299",
    "editor.selection": "#e5e5e6",
    "error": red,
    "info": blue,
    "list.group.background": "#e5e5e6",
    "list.hover.background": "#dbdbdc",
    "list.match.foreground": blue,
    "list.selected.background": "#d0d0d1",
    "list.selected.foreground": "#232324",
    "popover.background": "#f2f2f2",
    "popover.border": "#dbdbdc",
    "scrim": "#00000033",
    "scrollbar.thumb": "#696c7766",
    "success": green,
    "tab.active.background": bg,
    "tab.active.foreground": "#232324",
    "tab.bar.background": panel,
    "tab.inactive.background": panel,
    "tab.inactive.foreground": "#696c77",
    "terminal.ansi.black": fg,
    "terminal.ansi.blue": blue,
    "terminal.ansi.brightBlack": "#4f525e",
    "terminal.ansi.brightBlue": "#61afef",
    "terminal.ansi.brightCyan": "#56b6c2",
    "terminal.ansi.brightGreen": "#98c379",
    "terminal.ansi.brightMagenta": "#c678dd",
    "terminal.ansi.brightRed": "#e06c75",
    "terminal.ansi.brightWhite": "#ffffff",
    "terminal.ansi.brightYellow": "#e5c07b",
    "terminal.ansi.cyan": cyan,
    "terminal.ansi.green": green,
    "terminal.ansi.magenta": purple,
    "terminal.ansi.red": red,
    "terminal.ansi.white": "#a0a1a7",
    "terminal.ansi.yellow": yellow,
    "terminal.background": bg,
    "terminal.cursor": accent,
    "terminal.foreground": fg,
    "terminal.selection": "#e5e5e6",
    "text": fg,
    "text.accent": blue,
    "text.muted": "#696c77",
    "warning": yellow,
  },
  syntax: syntax({
    comment: "#a0a1a7",
    keyword: purple,
    function: blue,
    special: cyan,
    type: yellow,
    string: green,
    constant: orange,
    member: red,
    link: blue,
  }),
};
