# D070 — Application settings

Status: 🚧 checkpoint 1 in review. Item of the
[daily-driver plan](daily-driver.md#phase-2--foundations).

## Outcome

Personal config and extensions set editor and extension preferences through
one public `settings` API, globally or for a scope such as a language or a
view, and open views repaint immediately. Extensions define their own keys
with a type, a default, and a description, which a future settings UI can
list. This item builds the mechanism and proves it with a single native key,
`editor.tabSize`. Later items add their own keys when they first read them.

```ts
import { settings } from "knot";

settings.define({
  key: "lsp.timeout.launch",
  type: "integer",
  default: 52,
  description: "Timeout in seconds for launching the LSP process",
});
const watch = settings.observe("lsp.timeout.launch", () => { /* re-read */ });

settings.set("editor.tabSize", 2);
settings.set("editor.tabSize", 8, { language: "rust" });
settings.set({ "editor.tabSize": 4 }, { view: "editor", language: "rust" });
settings.get("editor.tabSize", { language: "rust" }); // 4 for that context
settings.reset("editor.tabSize", { language: "rust" });
```

## Design

### One store, last write wins

Settings work like Emacs variables. There is one application-wide store and
no layering by owner: personal config, installed packages, and bundled packages
all write the same entries, and a later write replaces an earlier one. To beat
a package's value, set it in `post-init`. Lifecycle teardown and
Restart Extension remove nothing, so a restarted package simply writes its
values again. The store is in memory and is rebuilt by config and packages on
every launch. Nothing is persisted natively: a UI that changes a setting, such
as a future theme picker, is an ordinary extension that calls `settings.set`
and keeps its choice in its own extension storage.

### Definitions

Keys are flat dotted names that say *what* they set (`editor.tabSize`, later
`font.size`, `theme`, `lsp.timeout.launch`). The scope says *where*. A
definition gives a key its type (`boolean`, `integer`, `number`, or `string`),
its default, and a description, like Emacs `defcustom`. Native keys and
extension keys share one registry: Rust defines the native keys at startup,
and extensions call `settings.define`. The definitions are what a future
settings UI lists.

- **Values before definition.** `pre-init` runs before packages start, so a
  write to an undefined key is kept as raw JSON. `define` validates the stored
  values against the type. A mismatching value is dropped and reported in the
  extension report, and the default applies. After definition, a write with
  the wrong type throws `TypeError`. A key that is never defined stays inert,
  so typos are silent, as in Emacs.
- **Ownership.** A definition belongs to the lifecycle that made it. Teardown
  removes the definition and keeps the values, and Restart Extension defines
  the key again. Defining a key that a live lifecycle already defined fails
  with `DuplicateSetting`. Native keys can never be redefined.
- **Types in TypeScript.** `knot.d.ts` declares the native keys in a
  `Settings` interface. Packages add their keys through declaration merging,
  so `set` and `get` stay typed for extension keys too.

A setting that names a contribution, such as `theme` (D064), holds only the
name. An unregistered name falls back the way an unknown language ID paints as
plain text, so the store never depends on package load order.

### Scopes

Every read happens *for a context*: the facts about the view that reads the
value. D070 defines two facts:

- `language`: the document's effective language ID.
- `view`: a native surface kind (`editor`, `terminal`) or a contributed view
  ID, using the same names as the `view` option on keybindings.

A write's optional scope is a set of facts that must all match. Resolution
picks the matching entry with the **most facts**, and ties go to the **last
write**. An unscoped entry has zero facts, so any matching scoped value beats
it, like an Emacs buffer-local value beats `setq-default`, and narrowing a
scope always takes effect. `set(key, value, scope)` replaces the entry for
exactly that key and scope and counts as a new write. `reset(key, scope)`
removes exactly that entry. When no entry matches, the native default applies.
`get(key, facts?)` resolves for the given facts, which lets a UI show the
effective value.

`path` is the third fact, added with project-local settings (D089) and used by
EditorConfig (D090). It reuses this rule unchanged.

### Change notification

The store is an application `Global` with a generation counter. Native views
that read settings observe it, notify on change, and resolve their values for
their own context during layout, so a change repaints every open window.

Personal `post-init` runs after packages start, so extensions need to see later
writes. `settings.observe(key, listener)` returns a `Disposable` and fires
after a write or reset to that key, in any scope. The listener receives the
key and calls `get` for the contexts it cares about. Listeners run on the
existing serial listener path and are removed with their lifecycle. A package
that only reads a value when it needs it can call `get` instead.

## Exclusions

- Every key other than `editor.tabSize`. Font family and size, insert spaces,
  theme (D064), soft wrap (D078), trim whitespace and final newline (D079), and
  the keymap preset (D086) are added by the items that first consume them.
- Natively persisted settings.
- Richer definition types (enums, ranges, arrays, objects). Add them when a
  key needs one.
- Project-local settings (D089), `.editorconfig` support (D090), and a
  settings UI (D091), which are deferred roadmap items.
- Terminal typography (D045, D059), which later becomes `font.*` keys scoped
  with `{ view: "terminal" }`.

## Checkpoints

### 1. Review owner layering in keybindings and languages 🚧

**Outcome (pending review):** both registries drop owner ranking, matching
Emacs and Neovim, whose config is code that runs in order. VS Code and Zed
rank the user's keymap above extensions only because it is a declarative file
with no place in a load order.

- Keymaps keep native bindings as a base level beneath every extension
  binding, like a setting's default and Zed's base keymap, so switching a
  D086 preset never beats extension or personal bindings. Extension and
  personal bindings form one last-write-wins stack; view-scoped still beats
  global. Personal overrides belong in `post-init`.
- Languages are last-write-wins with no base level: nothing switches bundled
  languages at runtime. User overrides of detection move to a setting
  (checkpoint 4), as in VS Code's `files.associations` and Zed's `file_types`.
- Accepted cost: a package that restarts or binds late beats earlier personal
  writes until Knot restarts. Teardown still reveals the previous write.

Settings drop layering by owner. Before building them, review the
personal > installed > bundled > native precedence that keybindings and
languages use today, and decide with the user whether each keeps it, moves to
the unified last-write-wins model, or changes later. D086's preset slot also
depends on the keymap answer.

- Deliverable: a short written assessment covering each registry: what the
  layering protects, which flows rely on teardown revealing a lower owner
  (Restart Extension, package failure, presets), and the cost of each.
  Act on whatever the user agrees: implement a small simplification here
  and record its rationale in `decisions.md`, or add a roadmap item for a
  larger one.
- Validation: if behavior changes, run the existing keymap and language
  registry tests and the product keybinding tests, updated to the new rule.
- Review gate: the user approves the outcome for both registries before
  checkpoint 2 starts.

**Implementation context:** keymap owner slots and precedence live under
`src/app/commands/` (see [commands](../architecture/commands.md) on keymap
precedence). Language precedence is `src/app/languages.rs`
(`LanguageOwner`, `register`, `remove_lifecycle`), with its contract written in
the `languages` doc comment in `src/host/js/knot.d.ts`. Teardown reaches both
through the shared foreground cleanup in `src/app/extensions/`.

### 2. Native settings store and `editor.tabSize`

- Deliverable: `src/app/settings.rs` with the definition registry (native
  definitions installed at startup, lifecycle-owned extension definitions),
  JSON values kept for undefined keys and validated at definition, entries
  keyed by key and scope with a write sequence, resolution by fact count then
  sequence, and `define`, `set`, `reset`, and `get`, exposed as an
  `ApplicationSettings` `Global`.
  Editor views resolve `editor.tabSize` for `{ view: "editor" or the contributed
  view ID, language: effective language }`, and tab expansion uses it instead of
  the fixed 4. Changing the setting repaints open views.
- Validation: unit tests for resolution (unscoped default, scoped beats
  unscoped, more facts beat fewer, ties go to the last write, re-set moves an
  entry later, reset reveals the next match, mismatched facts are ignored), for
  definitions (a pending value adopted, a mismatching pending value dropped,
  a typed write rejected after definition, a duplicate definition rejected,
  removal on teardown keeping values), and for an inert undefined key. A gpui test changes the global and a
  Rust-scoped value and checks that the caret x position and shaped width of a
  line with tabs follow the change in an already-open view.
- Review gate: the store shape and the editor wiring, before any public API
  exists.

**Implementation context:** register the module in `src/app/mod.rs` and
install the global at startup next to the other application globals (see
`ApplicationLanguages` in `src/app/languages.rs`). Tab expansion is
`expand_tabs` in `src/app/editor/layout.rs`, used by `shape_editor_line` and
`x_for_index_dir`. Thread the tab size through these instead of the hard-coded
`4`, and check other callers of `expand_tabs` for hit testing. The editor finds
its language through `languages::effective_language(model, cx)`. Mirror an
existing `cx.observe_global` subscription for repaint, or add the first one in
`src/app/editor.rs`. Product test helpers live in `src/app/product/tests/`.

### 3. Public `settings` API

- Deliverable: `settings.define(definition)`, `settings.set(key, value,
  scope?)`, `settings.set(values, scope?)`, `settings.get(key, facts?)`,
  `settings.reset(key, scope?)`, and `settings.observe(key, listener)` in
  `knot.d.ts`, with mergeable `Settings`, `SettingDefinition`, and
  `SettingsScope` types and contract doc comments. Matching host operations,
  the `knot.js` export, bootstrap validation, the in-memory fake host, and a
  short section in the extension guide. `define`, `set`, and `reset` resolve
  after the foreground commit. Mismatching pending values appear in the
  extension report.
- Validation: handler tests for invalid values, invalid scopes, and
  `DuplicateSetting`. A product test in which personal config sets an
  extension key in `pre-init`, a package defines it and reads the value,
  `post-init` changes it, and the package's observer sees the change. A product test in which personal config sets a global value and a Rust
  value, and an open Rust editor and a plain-text editor render their tabs
  accordingly. A product test showing that a package's later write beats an
  earlier pre-init write, and that Restart Extension leaves values in place. A
  fake-host test that runs through `node:test`. The extension contract test
  passes.
- Review gate: the public API shape and its documentation.

**Implementation context:** host operations are annotated with
`#[host_operation("...")]` in `src/app/extensions/host/handlers.rs`. Mirror
`listLanguages` and the language registration handlers near line 2060, and
`host::declarations` checks the context types at startup. The JavaScript side
is `src/host/js/knot.js` (exports, mirroring `languages`) and
`src/host/js/bootstrap.js` (argument validation, mirroring
`languages.register`). `settings.observe` mirrors the listener registration
and lifecycle cleanup of buffer-change subscriptions
(`subscribe_buffer_changes` in `handlers.rs`). Update `docs/schemas/knot.schema.json` if it covers the
API surface. The fake host is `scripts/extensions/testing/host.ts` and
`knot.ts`, and `scripts/extensions/contract.test.ts` checks it against the
declarations. Document in `docs/extension-api.md`.

### 4. Language file associations setting

- Deliverable: a native setting through which personal config maps file names
  or extensions to a language ID, overriding registration-based detection, as
  VS Code's `files.associations` and Zed's `file_types` do. Choose its shape
  here: a richer definition type (a string-to-string map) or one scoped key
  per pattern. Changing it re-detects open documents whose language is not
  explicit, like a registration change. An unregistered target ID paints as
  plain text.
- Validation: detection tests that the setting beats a registration and that
  resetting it reveals registration detection, and a product test in which
  personal config remaps an extension and an open document re-detects.
- Review gate: the setting's shape and its interaction with detection.

**Implementation context:** detection is `LanguageRegistry::detect` in
`src/app/languages.rs`; re-detection on registration change is already there.

### 5. Documentation

- Deliverable: the settings store and its owner in `docs/architecture.md`
  (application ownership) and the relevant subsystem reference: extension host
  for the API flow, workbench for view repaint. A `decisions.md` entry for
  unified last-write-wins settings, scope resolution, `defcustom`-style
  definitions with values that precede them, theme by name, and no native
  persistence. Roadmap D045 and D059 point at scoped `font.*` keys.
  Remove this plan and mark D070 ✅ in the daily-driver index.
- Validation: the docs follow `docs/README.md` ownership, with no contract
  duplicated from `knot.d.ts`.
- Review gate: final review of D070.

## Deferred decisions

- Whether `settings.get` also accepts a view or command target and derives the
  facts itself. Add this when a consumer needs it.
- Glob syntax and path normalization for the `path` fact (D089).
