// The languages this package registers: one per LSP language ID, each
// claiming the file names, extensions, and shebang interpreters detection
// matches.
import type { LanguageDefinition } from "knot";
import * as toml from "./queries/toml.js";

export const LANGUAGES: readonly LanguageDefinition[] = [
  {
    id: "toml",
    name: "TOML",
    grammar: "toml",
    fileExtensions: ["toml"],
    fileNames: ["Cargo.lock", "Pipfile"],
    queries: { highlights: toml.highlights },
  },
];
