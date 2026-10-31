// The languages this package registers: one per LSP language ID, each
// claiming the file names, extensions, and shebang interpreters detection
// matches. Languages that share a grammar family compose their highlight
// queries the way nvim-treesitter's `; inherits:` does: the inherited query
// comes first, so the more specific patterns after it win.
import type { LanguageDefinition } from "knot";
import * as bash from "./queries/bash.js";
import * as c from "./queries/c.js";
import * as cpp from "./queries/cpp.js";
import * as css from "./queries/css.js";
import * as go from "./queries/go.js";
import * as html from "./queries/html.js";
import * as injections from "./queries/injections.js";
import * as java from "./queries/java.js";
import * as json from "./queries/json.js";
import * as query from "./queries/query.js";
import * as yaml from "./queries/yaml.js";

export const LANGUAGES: readonly LanguageDefinition[] = [
  {
    id: "c",
    name: "C",
    grammar: "c",
    fileExtensions: ["c", "h"],
    queries: { highlights: c.highlights },
  },
  {
    id: "cpp",
    name: "C++",
    grammar: "cpp",
    fileExtensions: ["cpp", "cc", "cxx", "c++", "hpp", "hh", "hxx", "h++", "ipp", "inl", "tpp"],
    queries: { highlights: c.highlights + cpp.highlights, injections: injections.cpp },
  },
  {
    id: "css",
    name: "CSS",
    grammar: "css",
    fileExtensions: ["css"],
    queries: { highlights: css.highlights },
  },
  {
    id: "go",
    name: "Go",
    grammar: "go",
    fileExtensions: ["go"],
    queries: { highlights: go.highlights },
  },
  {
    id: "html",
    name: "HTML",
    grammar: "html",
    fileExtensions: ["html", "htm", "xhtml"],
    queries: { highlights: html.highlights, injections: injections.html },
  },
  {
    id: "java",
    name: "Java",
    grammar: "java",
    fileExtensions: ["java"],
    queries: { highlights: java.highlights },
  },
  {
    id: "json",
    name: "JSON",
    grammar: "json",
    fileExtensions: ["json", "webmanifest"],
    fileNames: [".prettierrc", ".babelrc"],
    queries: { highlights: json.highlights },
  },
  {
    id: "jsonc",
    name: "JSON with Comments",
    grammar: "json",
    fileExtensions: ["jsonc"],
    fileNames: ["tsconfig.json", "jsconfig.json", ".eslintrc.json", "devcontainer.json"],
    queries: { highlights: json.highlights },
  },
  {
    id: "query",
    name: "Tree-sitter Query",
    grammar: "query",
    fileExtensions: ["scm"],
    queries: { highlights: query.highlights },
  },
  {
    id: "shellscript",
    name: "Shell Script",
    grammar: "bash",
    fileExtensions: ["sh", "bash", "zsh", "ksh"],
    fileNames: [".bashrc", ".bash_profile", ".bash_aliases", ".profile", ".zshrc", ".zprofile", ".zshenv"],
    interpreters: ["sh", "bash", "zsh", "dash", "ksh"],
    queries: { highlights: bash.highlights },
  },
  {
    id: "yaml",
    name: "YAML",
    grammar: "yaml",
    fileExtensions: ["yaml", "yml"],
    queries: { highlights: yaml.highlights },
  },
];
