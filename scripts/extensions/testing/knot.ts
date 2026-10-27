// The "knot" module seen by extension code under test. Tests control it
// through "knot/testing".
import { api } from "./host.js";

export const {
  commands,
  documents,
  editor,
  keybinding,
  recovery,
  removeKeybinding,
  storage,
  windows,
  workbench,
  workspaces,
} = api;
