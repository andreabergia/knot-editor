// The "knot" module seen by extension code under test. Tests control it
// through "knot/testing".
import { api } from "./host.js";

export const {
  commands,
  documents,
  editor,
  keybinding,
  languages,
  recovery,
  removeKeybinding,
  settings,
  storage,
  themes,
  windows,
  workbench,
  workspaces,
} = api;
