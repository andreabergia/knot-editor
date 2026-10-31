# D073 — Language editing configuration

Status: 🚧 approved, not started. Item of the
[daily-driver plan](daily-driver.md); D074 (editing commands) and D075
(bracket matching and auto-closing) consume it.

## Goal

A language registration carries the data editing commands need: comment
tokens, bracket pairs with auto-closing flags, and indentation patterns. Every
bundled language supplies it, and personal config can replace one value by
spreading the registered definition. Nothing in the editor behaves differently
yet; D074 and D075 read the data.

## Design

- **Part of the language, not settings.** Comment tokens and bracket pairs
  describe the language like its grammar does, so they are optional fields of
  `LanguageDefinition`, registered with it and replaced with it under the
  existing latest-registration-wins rule. Toggles such as whether
  auto-closing is enabled remain language-scoped settings, owned by D075 and
  D079.
- **`languages.get(id)`** returns the effective full definition, queries and
  editing data included, so a derived language spreads it and changes a key,
  as `themes.get` allows for themes. `list` stays the light summary.
- **Fields**, all optional, following Zed's language config:

  ```ts
  comments?: { line?: string[]; block?: [open: string, close: string] };
  brackets?: { open: string; close: string;
               autoClose?: boolean;  // default true
               newline?: boolean;    // default open !== close
               notIn?: ("string" | "comment")[] }[];
  autoCloseBefore?: string;
  indentation?: { increase?: string; decrease?: string };
  ```

  - `comments.line` lists every line comment token. The first is what
    commenting inserts; uncommenting strips the longest one that matches, so
    Rust lists `//`, `///`, and `//!` and toggling a doc comment never leaves
    `/ foo`. Tokens omit the trailing space: commenting inserts the token and
    one space, and uncommenting removes the token and one following space.
    A language with only a block comment (HTML, CSS, Markdown) falls back to
    wrapping each line; one with neither (JSON) makes the commands no-ops.
  - One block comment style per language, as in Zed.
  - `brackets` is one list for matching, auto-closing, and surrounding. A
    pair whose `open === close` (quotes) never takes part in matching or
    indentation. `notIn` names syntax capture families at the cursor where
    auto-closing is suppressed. `newline` means Enter between the pair opens
    an indented middle line. Auto-closing happens only when the next
    character is whitespace, the end of the line, or one of
    `autoCloseBefore`.
  - Indentation is derived from `brackets` first: a line ending in an
    opening bracket indents the next line, and a line starting with a
    closing bracket outdents. `indentation.increase` and `decrease` are
    optional regular expressions, in Rust `regex` syntax as in Zed, for what
    brackets miss, such as Python's trailing `:` and dedenting keywords.
    D074 defines the exact evaluation; D073 stores and validates.
- **Native ownership.** The definition crosses as one JSON argument decoded
  with serde, as theme definitions do. Patterns compile with the `regex`
  crate during the existing off-foreground compile step, and an invalid one
  rejects the registration with a typed `InvalidPattern` host error naming
  `increase` or `decrease`. The registry exposes the compiled configuration
  of a language ID so native editing code reads it without calling
  JavaScript; injected layers resolve their own language's configuration.
- **Bundled data.** Every bundled language supplies comments, brackets, and
  auto-closing data. Indentation patterns are written only where brackets do
  not suffice: Python and YAML.

## Exclusions

- Any editing behavior: auto-indent, comment toggling, and auto-closing are
  D074 and D075.
- Syntax-scoped overrides such as JSX's `{/* */}`: deferred as D130.
- On-Enter comment continuation, word characters, and a second block comment
  style: deferred as D131.
- Tree-sitter `indents` queries: D066.

## Deferred decisions

- How increase and decrease patterns combine with bracket-derived
  indentation, and which line they match: decided in D074.
- Whether `notIn` checks the syntax tree or highlight captures at the
  cursor: decided in D075.

## Checkpoints

### 1. Contract and native storage

- Deliverables: the `LanguageDefinition` fields and `languages.get` in the
  public declarations, JavaScript validation of field shapes, a native
  editing configuration with compiled patterns retained per registration,
  the `InvalidPattern` error, a registry accessor for a language ID's
  effective configuration, and the fake host updated to store and return the
  new fields.
- Validation: registry unit tests for resolving configuration through
  latest registration and teardown; a product test that registers a
  language with editing data, reads it back through `languages.get`, and
  rejects an invalid pattern with `InvalidPattern`; the extension contract
  test covers the new types.
- **Implementation context:**
  - `src/host/js/knot.d.ts`: add the fields to `LanguageDefinition`, `get`
    to `languages`, `InvalidPattern` to the host error union near
    `InvalidQuery`, and document the field semantics there.
  - `src/host/js/bootstrap.js` `registerLanguage`: validate the new fields
    and pass them as one trailing JSON argument; add `getLanguage` and export
    it from `src/host/js/knot.js`.
  - `src/app/extensions/host/handlers.rs`: extend the `LanguageDefinition`
    decoder with `reader.command_arguments(10)` decoded through serde,
    mirroring `ThemeDefinition`'s decoder; add `InvalidPattern` to
    `LanguageError`; add a `getLanguage` operation beside `listLanguages`.
  - `src/app/languages.rs`: add the editing configuration to
    `LanguageDefinition` and compile its patterns in `compile()`; retain the
    source definition in `Registration` for `get`; add an accessor on
    `LanguageRegistry` and an application-level function beside
    `effective_language`. Add `regex` as a direct dependency (already in the
    lockfile through tree-sitter).
  - Tests: `src/app/languages/tests.rs`, `src/app/product/tests/languages.rs`,
    `scripts/extensions/contract.test.ts`, and `scripts/extensions/testing/host.ts`
    (`languageDefinitions` already stores whole definitions; add `get`).
- Review gate: the contract shape and validation errors are agreed before
  bundled data is written.

### 2. Bundled language data

- Deliverables: comments, brackets, and `autoCloseBefore` for every bundled
  language, and indentation patterns for Python and YAML.
- Validation: shared package tests check that bracket and comment tokens are
  nonempty, that every language declares comments unless it has none, and
  that patterns compile as JavaScript regular expressions as a cheap proxy;
  a product test starts the bundled language packages and asserts the
  startup report has no failures, so a pattern the `regex` crate rejects
  fails CI.
- **Implementation context:**
  - `extensions/@knot/language-*/src/languages.ts`: add the fields to each
    definition. Share common sets (C-family comments and brackets) within a
    package as constants.
  - `scripts/extensions/testing/languages.ts`: extend
    `testLanguagePackage` and `testLanguageSet` with the data checks.
- Review gate: spot-check the data for each package.

### 3. Documentation

- Deliverables: the language registration section of the extension guide
  shows editing data and deriving a language with `languages.get`; the
  semantic UI reference notes that the registry owns editing configuration
  for native editing code; a decision records data in the registration,
  Zed's merged bracket list, and Rust regex syntax for patterns. Remove this
  plan and mark D073 ✅ in the daily-driver plan.
- **Implementation context:** `docs/extension-api.md` (Register a
  language), `docs/architecture/semantic-ui.md` (Syntax highlighting and
  languages), `docs/decisions.md` (Syntax highlighting or a new Language
  editing configuration section), `docs/plans/daily-driver.md`.
- Review gate: final review of D073.
