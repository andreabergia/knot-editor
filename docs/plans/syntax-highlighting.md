# D002 — Syntax highlighting

Status: 📝 proposed, awaiting review. Promotes D002 from the
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
| Data and docs | `json` (also serves JSONC), `toml`, `yaml`, `sql`, `markdown`, `markdown_inline` |
| Tooling | `query` (tree-sitter queries, which `@knot/languages` itself is written in) |

Crates link through `tree-sitter-language` so grammar ABI versions are checked
against the core `tree-sitter` crate at build time. Binary size and build time
are measured when the set lands; each grammar costs C compile time once.

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
- A registration or teardown re-resolves every open detected document. Windows
  restore before extensions start, so restored documents show plain text until
  `@knot/languages` registers; this is one startup frame, not a lost state.
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
adapted from the upstream grammar repositories (MIT) and normalized to Knot's
highlight names, with attribution in the package.

### Highlight names and theme

One vocabulary of dotted highlight names serves tree-sitter captures now and
LSP semantic tokens later: `keyword`, `string`, `string.escape`, `comment`,
`comment.doc`, `number`, `constant`, `constant.builtin`, `type`,
`type.builtin`, `function`, `function.method`, `function.macro`,
`variable`, `variable.parameter`, `variable.builtin`, `property`,
`namespace`, `operator`, `punctuation.bracket`, `punctuation.delimiter`,
`tag`, `attribute`, `label`, and `markup.*` for Markdown. LSP token types and
modifiers map onto the same names (`parameter` → `variable.parameter`,
`defaultLibrary` → `.builtin`). Captures outside the vocabulary are ignored.

A `Theme` gpui `Global` replaces the editor color constants and resolves a name
by longest dotted prefix (`function.method.call` → `function.method` →
`function`) to a color plus bold/italic. One built-in dark palette ships; user
themes are D064.

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

The application owns one `SyntaxState` per buffer with a language, keyed by
buffer and released at buffer closure. It subscribes to committed edit batches
like any other edit-stream consumer:

- each commit edits the tree (`Tree::edit`) and then reparses on the foreground
  within a ~1 ms budget using tree-sitter's progress callback;
- when the budget runs out (initial parse, large paste, huge files) the parse
  continues on the background executor from a snapshot. The foreground keeps
  painting the previous tree's spans shifted by the edits since, and the result
  is accepted only if its revision still matches; otherwise it is edited forward
  and reparsed again;
- injection layers are separate trees over the ranges matched by the injection
  query, reparsed when their host ranges change. Nested injections are allowed
  to the depth the queries produce.

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
2. 📝 **Theme and layered runs.** `Theme` global, the highlight vocabulary, and
   the layer merge feeding `runs_for`; decorations move onto the layer path.
   View tests cover run composition with a fixture layer, tabs, and RTL lines.
3. 📝 **Native syntax engine.** Grammar table, `SyntaxState`, incremental edits,
   budgeted foreground parse with background continuation, visible-range
   highlight queries. Tests: after random edit sequences, incremental spans equal
   a fresh parse; stale background results are rejected or rebased.
4. 📝 **Injections.** Layered trees for Markdown inline and fenced code and for
   HTML `<script>`/`<style>`; tests over edits that create and remove injected
   regions.
5. 📝 **Language registry and API.** `languages` namespace, owner precedence,
   detection, `DocumentInfo.language`, `documents.setLanguage`, text-frame
   `language`, error codes, declarations, schema, and the fake host. Product
   tests for detection on Open, Save As, restore, and re-resolution on
   registration and teardown.
6. 📝 **`@knot/languages`.** Registrations and normalized queries for every
   bundled grammar, with `node:test` coverage that each query compiles against
   its grammar name and captures only vocabulary names.
7. 📝 **Set Language Mode and persistence.** Palette command backed by a picker,
   explicit language in the window layout, restoration through hot exit and
   project sessions.
8. 📝 **Documentation.** Architecture references (core and buffers, semantic UI,
   documents), decisions with measured parse and snapshot costs, extension
   guide; remove this plan.

Queries travel as strings rather than `.scm` package assets: packages stay
JavaScript-only and installed packages need no new capture path.

## Open questions for review

- The foreground budget value, to be set from checkpoint 3 measurements.
