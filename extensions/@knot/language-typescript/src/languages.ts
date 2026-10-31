// The languages this package registers: one per LSP language ID, each
// claiming the file names, extensions, and shebang interpreters detection
// matches. Languages that share a grammar family compose their highlight
// queries the way nvim-treesitter's `; inherits:` does: the inherited query
// comes first, so the more specific patterns after it win.
import type { LanguageDefinition } from "knot";
import * as ecma from "./queries/ecma.js";
import * as javascript from "./queries/javascript.js";
import * as jsx from "./queries/jsx.js";
import * as typescript from "./queries/typescript.js";

const javascriptHighlights = ecma.highlights + jsx.highlights + javascript.highlights;

export const LANGUAGES: readonly LanguageDefinition[] = [
  {
    id: "javascript",
    name: "JavaScript",
    grammar: "javascript",
    fileExtensions: ["js", "mjs", "cjs"],
    interpreters: ["node"],
    queries: { highlights: javascriptHighlights },
  },
  {
    id: "javascriptreact",
    name: "JavaScript JSX",
    grammar: "javascript",
    fileExtensions: ["jsx"],
    queries: { highlights: javascriptHighlights },
  },
  {
    id: "typescript",
    name: "TypeScript",
    grammar: "typescript",
    fileExtensions: ["ts", "mts", "cts"],
    interpreters: ["deno", "ts-node"],
    queries: { highlights: ecma.highlights + typescript.highlights },
  },
  {
    id: "typescriptreact",
    name: "TypeScript JSX",
    grammar: "tsx",
    fileExtensions: ["tsx"],
    queries: { highlights: ecma.highlights + typescript.highlights + jsx.highlights },
  },
];
