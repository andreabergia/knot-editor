# D070 — Application settings

Status: 🚧 checkpoints 1–3 done; checkpoint 4 implemented, awaiting review. Item of the
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
- Richer definition types (enums, ranges, arrays, objects) and a
  `settings.merge` for them. Add them when a key needs one.
- Project-local settings (D089), `.editorconfig` support (D090), and a
  settings UI (D091), which are deferred roadmap items.
- Terminal typography (D045, D059), which later becomes `font.*` keys scoped
  with `{ view: "terminal" }`.

## Checkpoints

### 1. Review owner layering in keybindings and languages ✅

Keybindings and languages no longer rank writers by owner: the latest write
wins and teardown reveals the previous one. Native bindings remain a base
beneath every extension and personal binding, so a D086 preset never overrides
them, and view-scoped bindings still beat global ones. User overrides of
language detection move to `languages.associate` (checkpoint 4). Rationale is in
[decisions](../decisions.md#commands-and-keymaps); the keymap rule is in
[commands](../architecture/commands.md#keymaps).

### 2. Native settings store and `editor.tabSize` ✅

`src/app/settings.rs` holds the `ApplicationSettings` store: native and
lifecycle-owned definitions, pending JSON values validated at definition, and
resolution by fact count then write sequence. Editor views resolve
`editor.tabSize` for their view kind and effective language and repaint when
the global changes. There is no generation counter, because gpui notifies
global observers on every mutable access. Tab size is clamped to 1–32
columns, and an `integer` is a JSON integer that fits `i64`. The store's write
and definition paths carry a `dead_code` allowance outside tests until
checkpoint 3 gives them product callers.

### 3. Public `settings` API ✅

`settings.define`, `set`, `get`, `reset`, and `observe` are public; contracts
live in `knot.d.ts` and the
[extension guide](../extension-api.md#read-and-write-settings). A write of the
wrong type for a defined key rejects with the host error `SettingTypeMismatch`
rather than `TypeError`, because only the native store knows the definition.
Writing several values is atomic, `get` of an undefined key is `undefined`,
observers also see their own lifecycle's writes, and `define` notifies nobody.
Values dropped at definition appear as warnings in the extension report.
Integral JavaScript numbers within the safe range become JSON integers so they
satisfy `integer`. Product tests cover the pre-init, package, and post-init
flow, Restart Extension, painted tab stops in open windows, and observer
teardown; the fake host's own tests live in `scripts/extensions/testing`.

### 4. Language file associations 🔍

Implemented ✅, review gate pending. Personal config redirects detection with
`languages.associate(id, { fileExtensions, fileNames, interpreters })`
rather than a setting. A single `files.associations` map setting was built
first and reverted: a map is one value, so any writer's `set` replaced every
other writer's associations, and teardown could not remove one package's
entries. An association is a lifecycle-owned claim in the registry's single
order, so the latest registration or association claiming a pattern wins,
disposal or teardown reveals the previous claim, and detection keeps its
rules (file name, longest extension, interpreter). An association defines no
language and is absent from `languages.list`; an unregistered target paints
as plain text. Claims re-detect open documents through the registration path.
A `settings.merge` for map values was considered and not added: merged
entries would still have no owner, so teardown and `reset` could not separate
them. No map setting exists, so the `stringMap` type was dropped too.

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
