# D073 — Language editing configuration

Status: 🚧 approved, not started. Item of the
[daily-driver plan](daily-driver.md); D074 (editing commands) and D075
(bracket matching and auto-closing) consume it.

## Goal

A language registration carries the data editing commands need: comment
tokens, bracket pairs with auto-closing flags, and indentation patterns.
Native code evaluates that data in pure functions: the indentation of a new
line and of a line that starts with a closer, and which comment token
toggling adds or strips. The bundled Rust, TypeScript and JavaScript (with
their JSX variants), Python, TOML, and Markdown packages supply the data, and
tests check concrete results per language. Personal config can replace one
value by spreading the registered definition. No command behaves differently
yet; D074 and D075 call the evaluators.

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
    A language with only a block comment (Markdown) falls back to wrapping
    each line; one with neither makes the commands no-ops.
  - One block comment style per language, as in Zed.
  - `brackets` is one list for matching, auto-closing, and surrounding. A
    pair whose `open === close` (quotes) never takes part in matching or
    indentation. `notIn` names syntax capture families at the cursor where
    auto-closing is suppressed. `newline` means Enter between the pair opens
    an indented middle line. Auto-closing happens only when the next
    character is whitespace, the end of the line, or one of
    `autoCloseBefore`.
- **Indentation evaluation.** Brackets come first: a line whose last
  non-whitespace character opens a bracket pair indents the next line one
  level, and a line whose first non-whitespace character closes one outdents
  to the level of the line holding its opener's line, or one level when the
  opener is not found nearby. `indentation.increase`, matched against the
  line above, indents the next line; `indentation.decrease`, matched against
  the current line, outdents it. Patterns use Rust `regex` syntax, as in
  Zed, and cover what brackets miss, such as Python's trailing `:` and
  `else`/`elif`/`except`. The evaluator returns a change in levels relative
  to a reference line, not whitespace; D074 turns levels into tabs or spaces
  with `editor.tabSize`.
- **Comment evaluation.** Given a set of lines, the evaluator decides
  whether toggling comments or uncomments them, and returns the token and
  column per line: comment at the smallest indentation of non-blank lines,
  uncomment when every non-blank line starts with a known token, strip the
  longest match. Block-only languages return a wrap per line.
- **Native ownership.** The definition crosses as one JSON argument decoded
  with serde, as theme definitions do. Patterns compile with the `regex`
  crate during the existing off-foreground compile step, and an invalid one
  rejects the registration with a typed `InvalidPattern` host error naming
  `increase` or `decrease`. The registry exposes the compiled configuration
  of a language ID so native editing code reads it without calling
  JavaScript; injected layers resolve their own language's configuration.
  The evaluators are plain functions over text lines and the configuration,
  with no gpui or buffer dependency.
- **Bundled scope.** Rust, `javascript`, `javascriptreact`, `typescript`,
  `typescriptreact`, Python, TOML, and Markdown get the data. Indentation
  patterns are written only where brackets do not suffice: Python.
  `markdown_inline` exists only for injection and gets none. The languages in
  `@knot/language-others` are deferred as D132.

## Exclusions

- Editing commands: auto-indent on Enter, comment toggling, and auto-closing
  are D074 and D075, which call these evaluators.
- Data for the languages in `@knot/language-others`: deferred as D132.
- Syntax-scoped overrides such as JSX's `{/* */}`: deferred as D130.
- On-Enter comment continuation, word characters, and a second block comment
  style: deferred as D131.
- Tree-sitter `indents` queries: D066.

## Deferred decisions

- Whether `notIn` checks the syntax tree or highlight captures at the
  cursor: decided in D075.
- How the evaluators treat a cursor in the middle of a line (Enter splits
  the line, so the remainder may start with a closer): decided in D074,
  which may add an input to the indentation evaluator.

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
  the evaluators are written.

### 2. Indentation and comment evaluators

- Deliverables: pure native functions for the indentation change of a new
  line after a given line, the outdent of a line from its own text, and the
  comment toggle plan for a set of lines, over the compiled configuration.
- Validation: unit tests over hand-built configurations, independent of any
  bundled package: bracket indent and outdent, a closer that finds its
  opener several lines up, quotes ignored, increase and decrease patterns,
  blank lines skipped when choosing the reference line, commenting at the
  minimum indentation, uncommenting with the longest token, mixed commented
  and uncommented lines commenting, block-only wrapping, and no tokens.
- **Implementation context:**
  - New `src/app/languages/editing.rs` (declared from `languages.rs`)
    holding the configuration type from checkpoint 1 and the evaluators, so
    they stay beside the registry and free of gpui and `BufferModel`. Inputs
    are line strings; outputs are levels and byte columns per line.
  - Tests in `src/app/languages/editing/tests.rs`, following the sibling
    `tests.rs` layout.
- Review gate: the evaluation rules match what D074 and D075 need before
  real language data depends on them.

### 3. Bundled language data with per-language behavior tests

- Deliverables: comments, brackets, and `autoCloseBefore` for Rust, the four
  ECMAScript languages, Python, TOML, and Markdown, and indentation patterns
  for Python.
- Validation:
  - A product test starts the bundled language packages, asserts the
    startup report has no failures (so a pattern the `regex` crate rejects
    fails CI), and runs table-driven cases through the evaluators on each
    language's registered configuration. Each case names a language, the
    text above the cursor, and the expected level change or comment result.
    Every language's table is a full suite, not a sample: each bracket pair
    in both directions, each comment token in both directions, each
    indentation pattern with matching and non-matching lines, nesting, blank
    and comment lines between a line and its reference, and the language's
    common constructs (Rust closures, `match` arms and `where` clauses;
    ECMAScript arrow functions, object literals, template literals, and JSX;
    Python decorators, multi-line calls, `try`/`except`/`finally`, and
    dedenting keywords; TOML tables and inline arrays; Markdown lists and
    code fences). The following are illustrative only: Rust `fn main() {` indents the next line and `}` returns to the
    `fn` line's level; `match x {` inside an indented block nests one more
    level; `let s = "{";` does not indent; TypeScript `const a = [` and
    `foo(` indent and `]);` outdents; TSX's `<div>` keeps the level; Python
    `def f():` and `if x:` indent, `else:` outdents to its `if`, and a
    `# comment:` does not indent; TOML `[table]` keeps the level and an
    inline array `a = [` indents; Rust toggling on a `///` line strips
    `///`; TypeScript commenting a block indented at mixed levels puts `//`
    at the shallowest column; Markdown commenting wraps in `<!-- -->`.
  - Shared package tests check token shapes: nonempty tokens, and every
    language in scope declares comments.
- **Implementation context:**
  - `extensions/@knot/language-{rust,typescript,python,toml,markdown}/src/languages.ts`:
    add the fields; share the ECMAScript sets as constants within the
    TypeScript package.
  - `scripts/extensions/testing/languages.ts`: extend
    `testLanguagePackage` with the shape checks, applied only to packages
    in scope.
  - New `src/app/product/tests/language_editing.rs`, registered beside
    `src/app/product/tests/languages.rs`; reuse the product test harness
    that starts bundled packages, as `startup.rs` reads `startup_report`.
- Review gate: review the case tables per language; add cases where review
  finds a gap.

### 4. Documentation

- Deliverables: the language registration section of the extension guide
  shows editing data and deriving a language with `languages.get`; the
  semantic UI reference notes that the registry owns editing configuration
  and its evaluators for native editing code; a decision records data in the
  registration, Zed's merged bracket list, bracket-first indentation, and
  Rust regex syntax for patterns. Remove this plan and mark D073 ✅ in the
  daily-driver plan.
- **Implementation context:** `docs/extension-api.md` (Register a
  language), `docs/architecture/semantic-ui.md` (Syntax highlighting and
  languages), `docs/decisions.md` (a new Language editing configuration
  section after Syntax highlighting), `docs/plans/daily-driver.md`.
- Review gate: final review of D073.
