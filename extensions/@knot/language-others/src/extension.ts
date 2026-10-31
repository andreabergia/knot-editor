// Bundled syntax highlighting languages. Every registration completes before
// start() resolves, because startup restores windows only after bundled
// packages start, so restored documents highlight from their first frame.
import { languages } from "knot";
import { LANGUAGES } from "./languages.js";

export async function start() {
  await Promise.all(LANGUAGES.map((definition) => languages.register(definition)));
}
