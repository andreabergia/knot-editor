// The languages this package registers: one per LSP language ID, each
// claiming the file names, extensions, and shebang interpreters detection
// matches.
import type { LanguageDefinition } from "knot";
import * as injections from "./queries/injections.js";
import * as rust from "./queries/rust.js";

export const LANGUAGES: readonly LanguageDefinition[] = [
  {
    id: "rust",
    name: "Rust",
    grammar: "rust",
    fileExtensions: ["rs"],
    queries: { highlights: rust.highlights, injections: injections.rust },
  },
];
