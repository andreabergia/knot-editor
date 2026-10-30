// One Dark, from Atom's one-dark-syntax and one-dark-ui.
import type { ThemeDefinition } from "knot";
import { syntax } from "../syntax.js";

const bg = "#282c34";
const panel = "#21252b";
const fg = "#abb2bf";
const cyan = "#56b6c2";
const blue = "#61afef";
const purple = "#c678dd";
const green = "#98c379";
const red = "#e06c75";
const orange = "#d19a66";
const yellow = "#e5c07b";
const accent = "#528bff";

export const ONE_DARK: ThemeDefinition = {
  name: "One Dark",
  appearance: "dark",
  colors: {
    "background": panel,
    "border": "#181a1f",
    "border.hover": accent,
    "button.background": "#2c313a",
    "button.hover.background": "#3a3f4b",
    "dropTarget.background": "#528bff40",
    "editor.background": bg,
    "editor.cursor": accent,
    "editor.foreground": fg,
    "editor.markedText": "#abb2bf99",
    "editor.selection": "#3e4451",
    "error": red,
    "info": blue,
    "list.group.background": bg,
    "list.hover.background": "#2c313a",
    "list.match.foreground": blue,
    "list.selected.background": "#3a3f4b",
    "list.selected.foreground": "#d7dae0",
    "popover.background": panel,
    "popover.border": "#3b4048",
    "scrim": "#00000080",
    "scrollbar.thumb": "#4e566680",
    "success": green,
    "tab.active.background": bg,
    "tab.active.foreground": "#d7dae0",
    "tab.bar.background": panel,
    "tab.inactive.background": panel,
    "tab.inactive.foreground": "#9da5b4",
    "terminal.ansi.black": "#3f4451",
    "terminal.ansi.blue": blue,
    "terminal.ansi.brightBlack": "#5c6370",
    "terminal.ansi.brightBlue": "#85c1ff",
    "terminal.ansi.brightCyan": "#6ed5de",
    "terminal.ansi.brightGreen": "#aad581",
    "terminal.ansi.brightMagenta": "#d398eb",
    "terminal.ansi.brightRed": "#ea858b",
    "terminal.ansi.brightWhite": "#ffffff",
    "terminal.ansi.brightYellow": "#ffd885",
    "terminal.ansi.cyan": cyan,
    "terminal.ansi.green": green,
    "terminal.ansi.magenta": purple,
    "terminal.ansi.red": red,
    "terminal.ansi.white": "#d7dae0",
    "terminal.ansi.yellow": yellow,
    "terminal.background": bg,
    "terminal.cursor": accent,
    "terminal.foreground": fg,
    "terminal.selection": "#3e4451",
    "text": "#9da5b4",
    "text.accent": blue,
    "text.muted": "#6b717d",
    "warning": yellow,
  },
  syntax: syntax({
    comment: "#5c6370",
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
