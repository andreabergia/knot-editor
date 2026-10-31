// The languages this package registers: one per LSP language ID, each
// claiming the file names, extensions, and shebang interpreters detection
// matches.
import type { LanguageDefinition } from "knot";
import * as injections from "./queries/injections.js";
import * as markdown from "./queries/markdown.js";
import * as markdownInline from "./queries/markdown_inline.js";

export const LANGUAGES: readonly LanguageDefinition[] = [
  {
    id: "markdown",
    name: "Markdown",
    grammar: "markdown",
    fileExtensions: ["md", "markdown", "mkd"],
    queries: { highlights: markdown.highlights, injections: injections.markdown },
  },
  {
    // Markdown's inline syntax, which Markdown injects into each paragraph.
    id: "markdown_inline",
    name: "Markdown Inline",
    grammar: "markdown_inline",
    queries: { highlights: markdownInline.highlights, injections: injections.markdownInline },
  },
];
