import type { ThemeDefinition } from "knot";
import { CATPPUCCIN_LATTE, CATPPUCCIN_MOCHA } from "./themes/catppuccin.js";
import { ONE_DARK } from "./themes/one-dark.js";
import { ONE_LIGHT } from "./themes/one-light.js";
import { SOLARIZED_DARK, SOLARIZED_LIGHT } from "./themes/solarized.js";
import { TOKYO_NIGHT, TOKYO_NIGHT_LIGHT } from "./themes/tokyo-night.js";

export const THEMES: readonly ThemeDefinition[] = [
  ONE_DARK,
  ONE_LIGHT,
  CATPPUCCIN_MOCHA,
  CATPPUCCIN_LATTE,
  TOKYO_NIGHT,
  TOKYO_NIGHT_LIGHT,
  SOLARIZED_DARK,
  SOLARIZED_LIGHT,
];
