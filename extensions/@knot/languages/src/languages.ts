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
import * as ecma from "./queries/ecma.js";
import * as go from "./queries/go.js";
import * as html from "./queries/html.js";
import * as injections from "./queries/injections.js";
import * as java from "./queries/java.js";
import * as javascript from "./queries/javascript.js";
import * as json from "./queries/json.js";
import * as jsx from "./queries/jsx.js";
import * as markdown from "./queries/markdown.js";
import * as markdownInline from "./queries/markdown_inline.js";
import * as python from "./queries/python.js";
import * as query from "./queries/query.js";
import * as rust from "./queries/rust.js";
import * as toml from "./queries/toml.js";
import * as typescript from "./queries/typescript.js";
import * as yaml from "./queries/yaml.js";

const javascriptHighlights = ecma.highlights + jsx.highlights + javascript.highlights;

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
  {
    id: "python",
    name: "Python",
    grammar: "python",
    fileExtensions: ["py", "pyi", "pyw"],
    interpreters: ["python", "python3"],
    queries: { highlights: python.highlights },
  },
  {
    id: "query",
    name: "Tree-sitter Query",
    grammar: "query",
    fileExtensions: ["scm"],
    queries: { highlights: query.highlights },
  },
  {
    id: "rust",
    name: "Rust",
    grammar: "rust",
    fileExtensions: ["rs"],
    queries: { highlights: rust.highlights, injections: injections.rust },
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
    id: "toml",
    name: "TOML",
    grammar: "toml",
    fileExtensions: ["toml"],
    fileNames: ["Cargo.lock", "Pipfile"],
    queries: { highlights: toml.highlights },
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
  {
    id: "yaml",
    name: "YAML",
    grammar: "yaml",
    fileExtensions: ["yaml", "yml"],
    queries: { highlights: yaml.highlights },
  },
];
