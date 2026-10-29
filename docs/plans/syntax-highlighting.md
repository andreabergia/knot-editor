# D002 — Syntax highlighting

Status: 🚧 approved, in progress. Promotes D002 from the
[roadmap](../roadmap.md); follow-up work is registered there as D064–D067.

## Problem

Every editor line paints as one default-colored run (`runs_for` in
`src/app/editor/layout.rs`). Colors are constants in `src/app/editor.rs`,
documents carry no language, and the only styled ranges are `info`/`warning`/
`error` contribution decorations. Source readability is now the dominant
problem in daily use.

## Goals

- Incremental tree-sitter highlighting for a bundled set of popular languages,
  including injected languages (Markdown inline syntax and fenced code, HTML
  `<script>` and `<style>`).
- Typing never shows stale colors in ordinary files; large files and large
  pastes never block the foreground.
- Languages are registered through a public extension API by a bundled
  `@knot/languages` package; native code owns grammars and the parse engine.
- A document's language is visible and settable through the API and a palette
  command; an explicit choice survives hot exit and project sessions.
- The pieces a future LSP integration needs are built once and not redone:
  language IDs, text snapshots, the highlight-name vocabulary, and the layered
  paint pipeline.

Non-goals, each registered in the roadmap: a theming system (D064), runtime
grammar loading (D065), tree-driven folding, indentation, selection expansion,
text objects, and a public syntax-tree API (D066), and language servers,
including semantic tokens (D067).

## Design

### Ownership split

```text
@knot/languages (JS)                   native (Rust)
  languages.register({ id, grammar,      grammar table (compiled in)
    fileExtensions, fileNames,    --->   LanguageRegistry (owner slots)
    interpreters, queries })              |
                                          v
documents.setLanguage(doc, id) --->  BufferModel language
                                          |
                                     SyntaxState per buffer
                                     (tree, injection layers, generation)
                                          |
                                     HighlightLayer --> EditorView paint
                                                            ^
                                     Theme (highlight name -> style)
```

Running the grammar is a native mechanic, like shaping: the parse must follow
every commit synchronously, and painting never calls JavaScript. The language
*definition* is contributed data, so extensions and personal config register,
replace, and override languages through the same path the bundled package uses.

### Grammars

Grammars are compiled in from tree-sitter crates and named by a stable native
grammar ID. The first set:

| Group | Grammars |
| --- | --- |
| Web | `html`, `css`, `javascript` (includes JSX), `typescript`, `tsx` |
| Systems | `rust`, `c`, `cpp`, `go`, `java` |
| Scripting | `python`, `bash` |
| Data and docs | `json` (also serves JSONC), `toml`, `yaml`, `markdown`, `markdown_inline` |
| Tooling | `query` (tree-sitter queries, which `@knot/languages` itself is written in) |

Grammars come from upstream crates over the `tree-sitter` crate directly rather
than through [arborium](https://docs.rs/arborium), which bundles grammars and
queries behind a whole-string `Grammar::parse(&str)` highlighter: it has no
`Tree::edit`, old-tree reuse, chunked input, progress callback, or byte-range
query, and it ships its own queries and tag vocabulary, so the engine below
could only use it as a grammar source tied to its `arborium-tree-sitter` fork.

Every grammar crate depends on `tree-sitter-language ^0.1`, so they link
against one `tree-sitter` 0.27; the runtime checks each ABI in `set_language`,
and a test loads and parses a sample with every grammar. The query grammar is
`tree-sitter-tsquery` (tree-sitter-grammars/tree-sitter-query), because the
`tree-sitter-query` crate still targets tree-sitter 0.20. SQL is left out:
`tree-sitter-sequel` pins `cc ~1.2.1`, which would hold the whole build's `cc`
at 1.2.x; it can return when the pin relaxes or through runtime grammar
loading (D065).

### Language identity

Language IDs follow LSP `languageId` values (`rust`, `typescript`,
`typescriptreact`, `javascriptreact`, `shellscript`, `jsonc`, …) so a future
language client passes them through unchanged.

- `BufferModel` owns an optional language ID plus whether it is explicit or
  detected. It is a buffer fact shared by every view.
- Detection resolves from registered file names, then file extensions, then a
  shebang interpreter on the first line. Open, Save As, and restore re-detect
  unless the language is explicit.
- Registration owners follow keymap precedence: personal config, then installed
  packages, then bundled. A conflicting pattern resolves to the
  highest-priority owner; removal reveals the next.
- A registration or teardown re-resolves every open detected document.
  Startup awaits every package entry, and bundled entries `await start()`,
  before restoring windows, so `@knot/languages` must await its registrations
  in `start()`; restored documents then paint highlighted from their first
  frame. Only a failed package leaves documents as plain text.
- `DocumentInfo.language` exposes it; changes publish `documentChanged` through
  the existing metadata comparison, so no new event type is needed.
- `documents.setLanguage(id, language | null)` makes it explicit, or returns to
  detection with `null`. An unregistered ID is accepted and paints plain text
  until something registers it, so personal config and restoration need no
  ordering against package startup.
- Contributed text frames take an optional `language` at registration.
- The window layout's document entry records an optional explicit language.
  Restoration applies it, so it survives hot exit and project sessions without
  another store.

### Registration API

```ts
languages.register({
  id: "rust",
  name: "Rust",
  grammar: "rust",
  fileExtensions: ["rs"],
  fileNames: [],
  interpreters: [],
  queries: { highlights: "…", injections: "…" },
}): Promise<Disposable>;
languages.list(): Promise<readonly LanguageInfo[]>;
languages.grammars(): Promise<readonly string[]>;
```

Registration is lifecycle-owned and torn down with its extension. Queries
compile at registration; an unknown grammar rejects `UnknownGrammar` and an
invalid query rejects `InvalidQuery` with the query error's location, so a bad
package fails visibly instead of painting nothing. `@knot/languages` keeps each
query in its own TypeScript module exporting the query text; queries are
adapted from the upstream grammar repositories (MIT) and nvim-treesitter
(Apache-2.0), using nvim-treesitter's capture names, with attribution in the package.

### Highlight names and theme

Capture names are nvim-treesitter's standard captures (`keyword.return`,
`function.method.call`, `variable.member`, `tag.attribute`, `markup.heading.1`,
…), listed in `src/app/theme.rs`. Zed, Helix, and Neovim all style dotted
capture names with longest-prefix fallback; adopting Neovim's published list
rather than a Knot-specific one lets adapted upstream queries keep their names,
and prevents a missing leaf from falling back to the wrong parent
(`tag.attribute` styled as `tag`). LSP token types and modifiers map onto the
same names (`parameter` → `variable.parameter`, `defaultLibrary` →
`.builtin`), as VS Code maps semantic tokens onto TextMate scopes. A capture
resolves to the capture name with the longest dotted prefix; captures with
none, such as Neovim's `@spell`, are ignored.

Decoration names (`diagnostic.*`) live beside the captures but are never a
capture's resolution, so a query cannot draw a diagnostic. VS Code and Zed keep
diagnostic colors among UI colors; Helix keeps them among highlight names, as
Knot does, so decorations reuse the layer path.

A `Theme` gpui `Global` replaces the editor color constants and resolves every
name by longest dotted prefix (`function.method.call` → `function.method` →
`function`) to a style. One built-in dark palette ships; user themes are D064.

### Layered painting

The view composes a line's runs from ordered highlight layers, each producing
sorted, non-overlapping `(byte range, highlight name)` spans for the visible
range:

1. syntax (this slice);
2. semantic tokens (D067, overrides syntax where present);
3. contribution decorations;
4. selection, painted as today.

`runs_for(line)` becomes a merge of layer spans into `TextRun`s; tab projection
already handles multiple runs. Syntax highlights are derived, never stored as
anchored ranges, and are queried for the visible byte range only
(`QueryCursor::set_byte_range`), cached per view by syntax generation and range.

### Text snapshots

`TextBuffer` gains an immutable, `Send` `TextSnapshot` for background parsing,
and later for Find (D031) and language-server document sync (D067):

- the original text becomes an `Arc<str>`;
- the add buffer becomes sealed fixed-size `Arc<str>` chunks plus a mutable
  tail; a snapshot shares sealed chunks and copies only the tail;
- a snapshot copies the piece list, which is O(pieces) and stays small while
  contiguous typing coalesces (D013 tracks compaction);
- precomputed piece end offsets let `chunk_at` serve tree-sitter's
  sequential `parse_with` reads by binary search without rescanning pieces.

### Parsing

Each `BufferModel` with a language owns one `SyntaxState`, released at buffer
closure. It reads committed primitive edits from the buffer's edit log, which
records each edit's line and byte-column points because a later primitive in
the same commit makes them unrecoverable:

- each commit edits the tree (`Tree::edit`) and then reparses on the foreground
  within a 1 ms budget using tree-sitter's progress callback;
- when the budget runs out (initial parse, large paste, huge files) the parse
  continues on the background executor from a snapshot. The foreground keeps
  painting the previous tree's spans shifted by the edits since, and the result
  is accepted only if its revision still matches; otherwise it is edited forward
  and reparsed again;
- injection layers are separate trees over the ranges matched by the injection
  query, reparsed when their host ranges change. Nested injections are allowed
  to the depth the queries produce, capped at eight so a self-injecting query
  terminates.

A language change or registration replacement discards the state and parses
fresh.

## Checkpoints

1. ✅ **Text snapshots.** `TextSnapshot` and `chunk_at` in `core`
   (`src/core/buffer/snapshot.rs`), with tests that a snapshot is unchanged by
   later edits and reads equal `read_range`. The add sink seals 16 KiB chunks.
   On Apple M1 Pro/macOS release builds, a 29 MiB, 932k-line buffer after
   5,000 typing bursts with deletes and pastes (9,988 pieces) measured
   snapshot p50 19 µs, p99 33 µs, and a full streamed `chunk_at` read 0.39 ms
   (`snapshot_cost_on_large_buffer`, ignored). The snapshot module carries
   `dead_code` allowances until checkpoint 3 takes snapshots.
2. ✅ **Theme and layered runs.** `Theme` global, the highlight vocabulary, and
   the layer merge feeding `runs_for`; decorations move onto the layer path.
   View tests cover run composition with a fixture layer, tabs, and RTL lines.
   `src/app/theme.rs` holds the nvim-treesitter capture names, the separate
   `diagnostic.*` decoration names, and the dark palette; `runs_for` in `src/app/editor/layout.rs`
   merges layers, and `EditorView::line_runs` is where checkpoint 3 adds the
   syntax layer beneath the decoration layers. Every shaping path (paint,
   caret, hit testing, IME bounds, completion anchor) uses the same runs, so
   bold and italic highlights keep geometry consistent. Decoration underlines
   are now text-run underlines rather than a separate overlay.
   `Highlight::resolve` carries a `dead_code` allowance until checkpoint 3
   resolves captures.
3. ✅ **Native syntax engine.** Grammar table (`src/app/syntax/grammars.rs`),
   `SyntaxState` (`src/app/syntax.rs`), incremental edits, budgeted foreground
   parse with background continuation, and visible-range highlight queries.
   Tests cover random edit sequences whose incremental highlights equal a fresh
   parse, and background results that are rebased over later edits or rejected
   for a replaced state.
   - `BufferEdit` carries `TextPoint`s for `InputEdit`. The model feeds the
     syntax state in the same commit step that stabilizes anchors, and an
     `observe_self` subscription starts background parses after any notify
     that leaves the tree behind, so commit paths need no gpui context.
     Replacing or closing a buffer's syntax state cancels its background parse.
     `BufferModel::set_syntax` attaches a language; it and the syntax module
     carry `dead_code` allowances until checkpoint 5 registers languages.
   - Captures paint with nvim-treesitter precedence: cursor order, so a later
     pattern beats an earlier one on the same node and inner nodes beat outer
     ones, with `(#set! priority N)` (default 100) ordering patterns. General
     predicates the Rust binding does not evaluate (`#lua-match?`, …) are
     ignored; checkpoint 6 normalizes queries away from them.
   - Views query the visible lines with some overscan once per syntax
     generation, which is globally unique so a replaced state never reuses a
     cached one. Painting and caret, hit-test, and IME geometry share that
     query, small scrolls reuse it, and the syntax layer composes beneath the
     decoration layers.
   - On Apple M1 Pro/macOS release builds (`syntax_costs_on_large_rust_files`,
     ignored): a 3,500-line, 96 KiB Rust file parses initially in 10.5 ms and
     reparses after valid typing at p50 220 µs, p99 475 µs; a 70,000-line,
     1.9 MiB file parses in 206 ms and reparses at p50 5.4 ms, p99 13.4 ms.
     Typing that leaves an unclosed `(` forces error recovery over the rest of
     the file: p50 21 ms and 444 ms respectively. A 60-line highlight query
     costs p50 about 220 µs at both sizes. This sets the 1 ms foreground
     budget: ordinary files reparse inline and huge files or broken syntax
     continue in the background.
   - The grammar set grows the release binary from about 74 MB to 88.5 MB and
     adds about 28 s of grammar C compilation, spread across parallel build
     scripts (typescript, sql, bash, cpp, and markdown are the largest), measured
     with SQL, which has since been dropped.
4. ✅ **Injections.** Layered trees for Markdown inline and fenced code and for
   HTML `<script>`/`<style>`; tests over edits that create and remove injected
   regions.
   - `SyntaxState` holds a tree of `Layer`s: the buffer's language at the
     root and each injection beneath its host, with the host's span edited
     forward. A reparse runs a layer's injection query only over the tree's
     `changed_ranges` and the edited ranges; injections touching them are
     found again and reuse the edited tree of a stale injection of the same
     language there, so typing in a fence reparses incrementally. Layers that
     cannot inject skip the tree comparison. Injected highlights paint over
     their host, clipped to the injection's included ranges.
   - Queries follow nvim-treesitter: `@injection.content`,
     `@injection.language` or `(#set! injection.language "…")`, and
     `injection.include-children`; otherwise named children of the content
     node are excluded, as Markdown block continuations must be.
     `injection.combined` is not supported, so each match is its own layer;
     checkpoint 6 normalizes queries that rely on it or on `#offset!`.
   - `SyntaxLanguage::new` takes the injection query, and invalid-query errors
     name which query failed. `SyntaxState::new` takes a `LanguageResolver`
     from injection names to languages, which checkpoint 5's registry
     provides; it should return the same `Arc` for a name so injections
     reuse their trees.
   - Tests cover fences, inline syntax across block continuations, HTML
     script and style, nesting through Markdown HTML blocks, unresolved
     languages, the depth cap, scripted edits that open, close, retarget, and
     remove injections, random Markdown and HTML edits against a fresh parse,
     and a background parse rebased over typing in a fence.
   - On Apple M1 Pro/macOS release builds
     (`injection_costs_on_a_large_markdown_file`, ignored): a 1,400-line,
     21 KiB Markdown file parses initially in 12.6 ms and reparses after
     typing at p50 115–150 µs, p99 0.4–0.9 ms. A 28,000-line, 427 KiB file
     with 10,000 layers parses in 286 ms and reparses at p50 6–9 ms, p99
     19–24 ms, of which the Markdown root reparse is about 7 ms and editing
     every later layer's tree about 4 ms; it continues in the background. A
     60-line highlight query costs p50 about 85 µs at both sizes.
5. ✅ **Language registry and API.** `languages` namespace, owner precedence,
   detection, `DocumentInfo.language`, `documents.setLanguage`, text-frame
   `language`, error codes, declarations, and the fake host. Product
   tests for detection on Open, Save As, restore, and re-resolution on
   registration and teardown, including that a restored window's documents
   have their languages before the window first paints.
   - `src/app/languages.rs` holds the `LanguageRegistry` in an
     `ApplicationLanguages` global. Owners rank personal config, installed,
     then bundled (`@knot/` storage owners), later registrations winning
     within a rank; the same ranking picks the language of an ID and the
     claimant of a file name, extension, or interpreter, so removal reveals
     the next.
   - Detection tries exact file names, then the longest case-insensitive
     extension (so `d.ts` is possible), then the shebang program, looking
     through `env` and its options; `python3.12` also matches `python`.
     `DocumentCollection` detects on every insert (Open, untitled, restore)
     and after every persistence (Save, Save As), skipping explicit
     languages. Untitled documents are not re-detected while typing.
   - `BufferModel` records the ID and whether it is explicit; the registry
     attaches a `SyntaxState` only when the resolved `SyntaxLanguage` changes.
     A registration change re-detects documents and reattaches every tracked
     buffer, and also reparses languages that can inject, since their
     injections may now resolve. Injections resolve by language ID, its
     lowercase, or a registered file extension (fence `py`, `rs`).
   - Host operations `registerLanguage`, `unregisterLanguage`,
     `listLanguages`, `listGrammars`, and `setDocumentLanguage`; new codes
     `UnknownGrammar`, `InvalidQuery` (details: `query`, `message`, `row`,
     `column`), and `LanguageRegistrationNotFound`. A text frame's
     `language` is applied as each window instance attaches.
   - The manifest schema needed no change, since languages are registered
     through the API. The fake host lists the native grammars, checked by a
     Rust test, and does not compile queries.
6. 📝 **`@knot/languages`.** Registrations and normalized queries for every
   bundled grammar, with `node:test` coverage that each query compiles against
   its grammar name and uses only names in `CAPTURE_NAMES`.
7. 📝 **Set Language Mode and persistence.** Palette command backed by a picker,
   explicit language in the window layout, restoration through hot exit and
   project sessions.
8. 📝 **Documentation.** Architecture references (core and buffers, semantic UI,
   documents), decisions with measured parse and snapshot costs, extension
   guide; remove this plan.

Queries travel as strings rather than `.scm` package assets: packages stay
JavaScript-only and installed packages need no new capture path.

## Open questions for review

None open; checkpoint 3 measurements set the foreground budget.
