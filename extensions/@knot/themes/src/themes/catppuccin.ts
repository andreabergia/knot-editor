// Catppuccin Mocha and Latte, from the published Catppuccin palette and its
// style guide.
import type { ThemeDefinition } from "knot";
import { syntax } from "../syntax.js";

const mocha = {
  rosewater: "#f5e0dc", pink: "#f5c2e7", mauve: "#cba6f7", red: "#f38ba8", maroon: "#eba0ac",
  peach: "#fab387", yellow: "#f9e2af", green: "#a6e3a1", teal: "#94e2d5", sky: "#89dceb",
  sapphire: "#74c7ec", blue: "#89b4fa", lavender: "#b4befe", text: "#cdd6f4", subtext1: "#bac2de",
  subtext0: "#a6adc8", overlay2: "#9399b2", overlay1: "#7f849c", overlay0: "#6c7086",
  surface2: "#585b70", surface1: "#45475a", surface0: "#313244", base: "#1e1e2e",
  mantle: "#181825", crust: "#11111b",
};

const latte = {
  rosewater: "#dc8a78", pink: "#ea76cb", mauve: "#8839ef", red: "#d20f39", maroon: "#e64553",
  peach: "#fe640b", yellow: "#df8e1d", green: "#40a02b", teal: "#179299", sky: "#04a5e5",
  sapphire: "#209fb5", blue: "#1e66f5", lavender: "#7287fd", text: "#4c4f69", subtext1: "#5c5f77",
  subtext0: "#6c6f85", overlay2: "#7c7f93", overlay1: "#8c8fa1", overlay0: "#9ca0b0",
  surface2: "#acb0be", surface1: "#bcc0cc", surface0: "#ccd0da", base: "#eff1f5",
  mantle: "#e6e9ef", crust: "#dce0e8",
};

type Flavor = typeof mocha;

function catppuccin(name: string, appearance: "dark" | "light", p: Flavor): ThemeDefinition {
  const dark = appearance === "dark";
  return {
    name,
    appearance,
    colors: {
      "background": p.mantle,
      "border": dark ? p.crust : p.surface0,
      "border.hover": p.blue,
      "button.background": p.surface0,
      "button.hover.background": p.surface1,
      "dropTarget.background": `${p.blue}40`,
      "editor.background": p.base,
      "editor.cursor": p.rosewater,
      "editor.foreground": p.text,
      "editor.markedText": `${p.text}99`,
      "editor.selection": `${p.overlay2}40`,
      "error": p.red,
      "info": p.blue,
      "list.group.background": dark ? p.base : p.crust,
      "list.hover.background": p.surface0,
      "list.match.foreground": p.blue,
      "list.selected.background": p.surface1,
      "list.selected.foreground": p.text,
      "popover.background": dark ? p.mantle : p.base,
      "popover.border": dark ? p.surface0 : p.surface1,
      "scrim": dark ? "#11111b99" : "#4c4f6933",
      "scrollbar.thumb": `${p.surface2}99`,
      "success": p.green,
      "tab.active.background": p.base,
      "tab.active.foreground": p.text,
      "tab.bar.background": p.mantle,
      "tab.inactive.background": p.mantle,
      "tab.inactive.foreground": p.subtext0,
      "terminal.ansi.black": dark ? p.surface1 : p.subtext1,
      "terminal.ansi.blue": p.blue,
      "terminal.ansi.brightBlack": dark ? p.surface2 : p.subtext0,
      "terminal.ansi.brightBlue": p.blue,
      "terminal.ansi.brightCyan": p.teal,
      "terminal.ansi.brightGreen": p.green,
      "terminal.ansi.brightMagenta": p.pink,
      "terminal.ansi.brightRed": p.red,
      "terminal.ansi.brightWhite": dark ? p.subtext0 : p.surface1,
      "terminal.ansi.brightYellow": p.yellow,
      "terminal.ansi.cyan": p.teal,
      "terminal.ansi.green": p.green,
      "terminal.ansi.magenta": p.pink,
      "terminal.ansi.red": p.red,
      "terminal.ansi.white": dark ? p.subtext1 : p.surface2,
      "terminal.ansi.yellow": p.yellow,
      "terminal.background": p.base,
      "terminal.cursor": p.rosewater,
      "terminal.foreground": p.text,
      "terminal.selection": `${p.surface2}80`,
      "text": dark ? p.subtext1 : p.text,
      "text.accent": p.blue,
      "text.muted": p.overlay1,
      "warning": p.yellow,
    },
    syntax: syntax({
      comment: p.overlay2,
      keyword: p.mauve,
      function: p.blue,
      special: p.sky,
      type: p.yellow,
      string: p.green,
      constant: p.peach,
      member: p.lavender,
      link: p.blue,
    }),
  };
}

export const CATPPUCCIN_MOCHA = catppuccin("Catppuccin Mocha", "dark", mocha);
export const CATPPUCCIN_LATTE = catppuccin("Catppuccin Latte", "light", latte);
