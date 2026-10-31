# Writing extensions for Knot

The [TypeScript declarations](../src/host/js/knot.d.ts) are the API version 1
reference for parameters, defaults, errors, completion, and lifetime rules.
This guide covers setup and common workflows. Knot runs JavaScript; TypeScript
is the recommended authoring language and compiles before installation.

## Package and check your code

Install a directory containing `knot.jsonc` and built `.js`/`.mjs` sources under
the platform's local application-data `Knot/extensions` root. For example:

```json
{
  "name": "@example/tools",
  "apiVersion": 1,
  "version": "1.0.0",
  "main": "dist/main.js"
}
```

Use the [manifest schema](schemas/knot.schema.json) for tooling and the
[installed examples](../examples/extensions/) for directory layout. The host
accepts API version 1 before evaluation; package version and dependency ordering
are separate. Static relative imports stay inside captured package sources;
dynamic imports reject. Startup diagnostics report rejected or failed packages
while independent packages continue.

Personal config has no manifest. Its optional `pre-init.js` and `post-init.js`
entries share one lifecycle around installed-package startup; config failure
stops product startup. Directory selection and loading are described in
[host startup](architecture/extension-host.md#package-admission-and-startup).

Run `scripts/check-extensions.sh` with Node/npm to check declarations and every
TypeScript package with the lockfile-pinned compiler, then run package tests. The
[checked examples](../scripts/extensions/examples/) cover commands, providers,
cleanup, errors, and keybindings. This check runs in CI; Rust/V8 tests separately
validate runtime behavior.

## Write and test in TypeScript

A TypeScript package keeps `src/*.ts`, `test/*.test.ts`, and a `tsconfig.json`
beside its manifest; Knot loads only the emitted `dist/` modules, one per source
file. The [git-status example](../examples/extensions/@example/git-status/) and
the bundled [project registry](../extensions/@knot/projects/) show the layout.
In this repository, packages extend the shared
[base configuration](../scripts/extensions/tsconfig.base.json), which sees only
ES2022 and the `knot` declarations. Import sibling modules by their emitted
name, such as `./model.js`. `npm --prefix scripts/extensions run build <dir>`
emits one package's `dist/`; Cargo builds bundled packages itself.

Keep policy in plain functions over plain data and keep mutable state out of
module scope: export `start()` to create state and register contributions, and
make `main.ts` only `await start()`. Tests then exercise policy directly and run
whole flows from a fresh start.

`npm --prefix scripts/extensions test` compiles every package's sources and tests
and runs them with `node:test`. Under test, `"knot"` resolves to an
[in-memory host](../scripts/extensions/testing/host.ts) typed against the
declarations, and tests import its controls from `"knot/testing"`: arrange
windows, documents, and records; queue prompt answers; run commands and emit
application events; make the next call to an operation fail with a host error;
and inspect records, captures, messages, and views. Each test file runs in its
own process; call `reset()` before each test. The fake implements only what tests
need, and anything else throws `not faked`. It does not reproduce host
validation, scheduling, or durability, so behavior crossing the native boundary
still needs product tests.

## Register a command

Knot runs JavaScript. Import its public API from `"knot"` and register handlers
during package activation:

```js
import { commands } from "knot";

await commands.register("example.insert-greeting", async ({ buffer }) => {
  if (buffer === null) return;
  const snapshot = await buffer.snapshot();
  await buffer.applyEdits([{
    range: { startByteOffset: 0, endByteOffset: 0 },
    text: "Hello ",
  }], { ifRevision: snapshot.revision });
}, { title: "Insert Greeting", description: "Insert a greeting at the start." });
```

The command palette lists the title, which defaults to the command name, with
its description and binding, and still matches the name as typed. Pass
`palette: false` for a command that only makes sense from a key.

The buffer belongs to the command’s captured context. Focus changes during
`await` do not select another buffer. Offsets are UTF-8 bytes; edits require
the revision read from a snapshot. Completion and change callbacks receive an
explicit buffer to use outside commands.

Registering commands/providers/keymaps is valid during activation. Contextual
lookup and command invocation require a running command. Await disposal and
view invalidation to observe completion, and use host error `code` values
when handling failures. See the API reference for precise rules.

## Contribute a dockable view

Register one lifecycle-owned view definition before or after product windows
exist. The contribution ID is stable, namespaced, and doubles as the native view
kind for commands and keybindings:

```js
const view = await workbench.registerView({
  id: "example.outline",
  title: "Outline",
  content: {
    kind: "tree",
    provider: {
      getChildren(parentId, context) {
        // context.window identifies the requesting window; parentId null is the root.
        return [];
      },
    },
  },
  placement: { location: "left", visible: true },
});

await view.invalidate(null);        // every live window
await view.show();                  // every live window
await view.hide();
await view.focus();
await view.dispose();
```

Each product window owns at most one native instance. Users can drag, dock, and
hide frames; remembered user layout overrides registration defaults in later
windows. Programmatic `show`/`hide`/`focus` affects live windows only and never
changes the fresh-window default. Pass a live `WindowTarget` from
`context.target.windowTarget` or `windows.*` to target one window.

### Groups, status, and actions

An item is `item` by default. A `group` is a non-activatable header that
organizes child rows: the native view fetches its children immediately, shows no
disclosure, and keeps them visible. Items carry optional `statusText` plus a
`badge` naming a [color key](#register-a-theme-and-define-colors), such as
`warning` or `text.muted`, and either kind may declare command-backed `actions`. A
contribution's own `actions` populate the frame toolbar. Action labels are
text; a general extensible icon system is deferred (roadmap D055).

```js
const view = await workbench.registerView({
  id: "example.changes",
  title: "Changes",
  content: {
    kind: "tree",
    provider: {
      getChildren(parentId) {
        if (parentId === null) return [{
          id: "changed", label: "Changes", collapsibleState: "none", kind: "group",
          actions: [{ label: "Stage all", command: "example.stage-all" }],
        }];
        if (parentId === "changed") return [{
          id: "src/main.rs", label: "src/main.rs", collapsibleState: "none",
          statusText: "M", badge: "warning",
          actions: [{ label: "Stage", command: "example.stage",
                      arguments: { path: "src/main.rs" } }],
        }];
        return [];
      },
    },
  },
  actions: [{ label: "Refresh", command: "example.refresh" }],
});
```

Actions invoke existing registered commands. Their JSON `arguments` become the
handler's `context.arguments`, so an action retains the row identity it was
created for even after focus changes. Clicking an action never activates the
row, and the captured window is the one where the click happened. Rows remain
usable without a pointer: move the selection, press Right to enter its actions,
cycle with Up/Down, press Enter to invoke, and Escape to return.

### Read-only text frames

A text frame owns a private read-only buffer rendered by the native editor; it is
not a document and has no resource, persistence, or document tab. Replace the
whole text with `setText`. The last text is retained for windows created later.

```js
const log = await workbench.registerView({
  id: "example.log",
  title: "Log",
  content: { kind: "text" },
  placement: { location: "bottom" },
});

await log.setText("first line\n");
await log.setText("replacement text\n");
```

`invalidate` applies to tree frames and `setText` to text frames; the wrong kind
rejects with `ContentKindMismatch`. Selection and copy work through the normal
editor surface, but edits are rejected. Pass `language` with the registration to
highlight the frame's text as a registered language.

## Register a language

Knot parses with tree-sitter grammars compiled into the binary;
`languages.grammars()` lists their IDs. A language registration names one of
them, claims file patterns for detection, and supplies the highlight and
optional injection queries:

```js
import { languages } from "knot";

await languages.register({
  id: "rust",
  name: "Rust",
  grammar: "rust",
  fileExtensions: ["rs"],
  queries: {
    highlights: `(line_comment) @comment
"fn" @keyword.function`,
  },
});
```

Use LSP `languageId` values for IDs (`typescriptreact`, `shellscript`) and
[nvim-treesitter's capture names](https://github.com/nvim-treesitter/nvim-treesitter/blob/main/CONTRIBUTING.md#highlights)
in queries; the theme resolves a capture by its longest known dotted prefix and
ignores names it does not know. Predicates are the ones the Rust tree-sitter
binding evaluates (`#eq?`, `#match?` with Rust regex syntax, `#any-of?`, and
their negations), plus `(#set! priority N)`. Injections use
`@injection.content` with `@injection.language` or
`(#set! injection.language "…")`, and may name a language by a registered file
extension, as Markdown fences do.

Queries compile before `register` resolves: an unknown grammar rejects
`UnknownGrammar` and a bad query `InvalidQuery`, with the query, message, row,
and column in `details`. Register in activation so restored documents highlight
from their first frame. The latest registration of an ID or file pattern
wins, so personal config replaces a package's language from `post-init`. The
bundled `@knot/language-*` packages, one per language, are complete examples.

To send files to a language without replacing its registration, associate
patterns with its ID. The claim competes with registrations' claims, so the
latest one wins and disposing it reveals the previous one:

```js
await languages.associate("markdown", { fileExtensions: ["mdx"] });
```

`DocumentInfo.language` reports a document's language, or `plaintext`.
`documents.setLanguage(id, "python")` makes a language explicit, which is
recorded in window layouts and survives restarts and project switches; `null`
returns the document to detection. Users do the same with Set Language Mode.

## Read and write settings

Settings are one application-wide store that personal config and every package
share. Define your own keys with a type, a default, and a description, and
declare their types by merging into the `Settings` interface so `set` and `get`
stay typed:

```ts
import { settings } from "knot";

declare module "knot" {
  interface Settings {
    "lsp.timeout.launch": number;
  }
}

await settings.define({
  key: "lsp.timeout.launch",
  type: "integer",
  default: 52,
  description: "Timeout in seconds for launching the LSP process",
});
const timeout = await settings.get("lsp.timeout.launch");
await settings.observe("lsp.timeout.launch", async () => {
  /* read the new value with get */
});
```

A write applies everywhere, or only where every fact of its scope matches:
`language` is a document's language ID and `view` a surface kind such as
`editor` or a contributed view ID. A read picks the matching value with the
most facts, so `settings.set("editor.tabSize", 8, { language: "rust" })` beats
an unscoped value however late that one was written. Among equally specific
values the latest write wins, whoever made it, so personal overrides belong in
`post-init`.

Values may be written before their key is defined, which lets `pre-init`
configure a package that starts later. Definition drops a value of the wrong
type and notes it in the extension report; after definition, a mismatching
write rejects `SettingTypeMismatch`. Values live in memory and are written
again on every launch: teardown and Restart Extension remove only your
definitions and observers. To remember a choice, such as one a command makes,
keep it in [storage](#durable-storage) and set it again in `start()`.

## Register a theme and define colors

A theme names its appearance, sets colors by key, and styles capture names.
Colors are `#rrggbb` or `#rrggbbaa`; the native keys are declared in
`ThemeColors` in `knot.d.ts`, and a key a theme leaves out takes its default for
the theme's appearance:

```js
import { themes } from "knot";

await themes.register({
  name: "Ember",
  appearance: "dark",
  colors: { background: "#1c1917", "editor.selection": "#f9731640" },
  syntax: {
    comment: { color: "#78716c", fontStyle: "italic" },
    keyword: { color: "#f97316", fontWeight: 700 },
  },
});
```

`syntax` keys are capture names or a dotted prefix of them, such as `markup`;
a capture takes its longest styled prefix, and an unknown name rejects
`UnknownCapture`. Themes are code, so derive one by spreading a registered
theme:

```js
const base = await themes.get("One Dark");
await themes.register({
  ...base,
  name: "My Dark",
  colors: { ...base.colors, "editor.background": "#101010" },
});
```

The `theme.mode` (`dark`, `light`, or `system`), `theme.dark`, and
`theme.light` [settings](#read-and-write-settings) select the theme. Knot
resolves it when startup completes and again whenever the selection changes,
so register themes in activation; a theme registered or disposed later shows
at the next selection change. The latest registration of a name wins.

The bundled [`@knot/themes`](../extensions/@knot/themes/) package registers
the default themes and the `themes.select` command, which remembers its choice
and sets it again when the package starts. Customize themes in `post-init`:
it runs after the package, so its settings win over the remembered choice, and
`themes.get` can see the bundled themes to derive from them, which `pre-init`
cannot.

Define the color keys your package paints with, giving each a default per
appearance, either a color or another key to follow. Themes may then set it,
and keys stay open, so a theme can style a package that loads after it:

```js
await themes.defineColor({
  key: "diff.add",
  description: "Added lines in diffs",
  defaults: { dark: "success", light: "#2ea04340" },
});
```

A key defined by native code or a live lifecycle rejects `DuplicateColor`;
teardown removes your definitions.

Editor decorations and gutter markers and tree badges name the color key they
paint with, native or defined, so a contribution looks right under every
theme. A key nobody has defined paints as interface text until its definition
arrives. Where decorations overlap, `error`, `warning`, and `info` paint above
other keys, the most severe on top:

```js
await buffer.contributions.replace([
  { range, decoration: "diff.add", gutter: "diff.add" },
  { range: problem, decoration: "error" },
], { ifRevision: revision });
```

## Register a keymap preset

A preset is a named set of default bindings that replaces Knot's when the
`keymap.preset` [setting](#read-and-write-settings) selects it; every
`keymaps.bind` still wins over it. Presets are code, so derive one by
spreading another and branch on `platform` for OS-specific keys:

```js
import { keymaps, platform, settings } from "knot";

const knot = await keymaps.getPreset("knot");
const mod = platform === "macos" ? "cmd" : "ctrl";
await keymaps.registerPreset({
  name: "mine",
  title: "Mine",
  bindings: [
    ...knot.bindings,
    { key: `${mod}-shift-d`, command: "me.insert-date", view: "editor" },
    { key: `${mod}-k down`, command: null },
  ],
});
await settings.set("keymap.preset", "mine");
```

A binding needs `command`, which is a name, a `{ name, arguments }` object, or
`null` to unbind. The latest registration of a name wins and teardown reveals
the previous one; `"knot"` cannot be replaced. Knot resolves the selection
when startup completes and again whenever the setting changes, so registering
the selected preset later takes effect at the next change, and an unregistered
name selects Knot.

## Ask the user transiently

`workbench` exposes window-local pickers, text input, and native confirmation or
message prompts. Each operation captures an explicit `WindowTarget` or the
admitting command's window, so focus changes during an `await` cannot retarget
it. Results are typed: acceptance carries a value, dismissal is a normal value,
and operational failure is a host error.

```js
const picked = await workbench.showPicker({
  title: "Switch repository",
  items: [{ id: "knot", label: "knot", description: "/src/knot-editor" }],
});
if (picked.kind === "selected") await select(picked.id);

const named = await workbench.showInput({ title: "Rename", value: "knot" });
if (named.kind === "accepted") await rename(named.value);

const sure = await workbench.showConfirmation({ title: "Remove knot?" });
if (sure.kind === "confirmed") await remove();

await workbench.showMessage({ title: "Saved" });
```

Picker items are fuzzy-matched locally, label first and then description, so
the complete list is supplied up front; there is no streaming provider. An item
may add a `detail` line and a right-aligned `hint`. Pass `onHighlight` to
preview the highlighted item as the user moves through the list:

```js
const before = await settings.get("theme.dark");
const picked = await workbench.showPicker({
  title: "Preview",
  items,
  onHighlight: async (id) => { if (id !== null) await settings.set("theme.dark", id); },
});
if (picked.kind === "dismissed") await settings.set("theme.dark", before);
```

The callback runs inside the awaiting command, one call at a time; changes
made while it runs collapse into the latest highlight. Undoing a preview after
dismissal is the caller's job. Inputs are single-line and
support the usual editing, clipboard, IME, and selection behavior. Confirmation
and message use the native platform prompt.

One extension transient interaction may own a window at a time. A second
request rejects `OperationInProgress`. The command palette is independent and
may be open at the same time. Window closure, extension unload, and caller
cancellation settle a pending interaction as cancellation; focus returns to the
surface that was focused before it opened.

## Compose commands

`await commands.invoke(name, arguments)` routes a child through the originating
command's captured target and returns a structured outcome. Native and registered
JavaScript commands use the same route. Check `outcome.kind`; command return values
do not become semantic results. A participant can decline, and a valid invocation
can return `unavailable` for a command unsupported by its captured surface. Calling
from activation or unrelated callbacks rejects `ContextUnavailable`.

The checked [activation example](../scripts/extensions/examples/activation.js)
includes `guide.compose`, which awaits another JavaScript command, and
`guide.select-all`, which awaits the native `editor.select-all` command. The
contributed view's `copy` registration shares the native command name and handles
it only when that view instance claims the command. Dispose these registrations
when removing the feature; native definitions retain their application ownership.

The [Git-status fixture](../examples/extensions/@example/git-status/src/extension.ts)
uses only the public API for grouped frames, row/group/frame actions, file
activation, a read-only log frame, and refresh after actions.

## Identity and lifetime

| Identity | Ownership and validity | Persist? |
| --- | --- | --- |
| Package name | Canonical installed identity, independent of activation | Yes, as a package name |
| Runtime/config lifecycle | Hidden; config phases share their own lifecycle | No |
| Window/view IDs | Opaque live identities in a command capture | No |
| Command target/invocation | Invocation-scoped authority; invocation ID is hidden | No |
| Document ID | Application-allocated; stable across Save As and presentations | Only to refer to a document restored from automatic recovery, which keeps its ID across restart |
| Live window/pane/view target | Exact discoverable product presentation, independent of commands | No |
| Root ID | Window-owned; retained while its normalized URI remains installed | No |
| Buffer proxy | Live shared model, independent of any one presentation | No |
| Registration/subscription | Extension-owned until disposal, resource loss or teardown; IDs hidden | No |
| Contribution set | One set per buffer and extension lifecycle | No |
| Revision/generation | Content/request correlation, not reusable authority | No |
| Tree item ID | Author-defined identity within a provider tree | Provider-defined |

A buffer proxy or URI is not persistent document identity. Command targets expose
no native handles and cannot be reconstructed from saved IDs. Retained explicit
buffers support asynchronous work while live; retained command targets do not
extend the invocation's authority. Lifecycle teardown releases ownership without
requiring the extension to await cleanup or receive a final callback.

## Inspect and change application state

Use `documents` for metadata and shared buffers, `windows` for live product
presentations, and `workspaces` for window-owned roots. Queries work during
activation and callbacks, including before product windows exist. Document IDs
survive Save As; across restart only a document restored from automatic recovery
keeps its ID. Retain returned live target objects rather than serializing or
reconstructing them. `windows.focus(target)` activates a window, and
`windows.open({ roots })` opens a new one with those roots, closing it again if
a root is rejected.

Use `documents.open` for files, `createUntitled` for new documents, and `present`
for another presentation of a live document. These return committed document and
presentation data. Pass `{ preview: true }` to `documents.open` when the user is
browsing, as a file list's single click does: the document replaces the pane's
current preview tab instead of adding one, until the user edits, pins, or
double-clicks it. The `workbench.previewTabs` setting turns previews off. Use `workspaces.openFolder` to replace a window's roots with
one folder, or `replaceRoots` with an ordered list. Retained roots keep their IDs,
so add, remove, and reorder by replacing the list read from `workspaces.get`;
duplicates collapse after normalization and nested roots are allowed. Documents
remain independent of workspace containment. A missing file opens as a dirty destination-associated
document rather than failing. Use `workspaces.pickFolder` when a workflow asks
the user for a folder:

```js
const folder = await workspaces.pickFolder();
if (folder.kind === "selected") await workspaces.openFolder(folder.uri);
```

The picker reuses the native directory prompt, owns the window's transient
interaction while open, and returns a provider-normalized directory URI. Dismissal
is a normal result; a missing directory rejects `ResourceNotFound` and a file
rejects `ResourceWrongKind`.

Use `terminals.open` to open a terminal tab, optionally starting in a local
directory:

```js
await terminals.open({ cwd: "file:///Users/me/project/" });
```

Without `cwd` the shell starts where New Terminal would: the focused
terminal's current directory, else the root containing the window's most
recently focused document, else its first root, else the home directory. A docked target opens the terminal in the window's document area.

Pass an explicit live target when acting on a previously selected window, pane,
or presentation. Omitting it captures the command's originating window, or the
active product window outside a command, at admission. It never waits for a
future window or follows later focus. Stale targets reject before commit.
Discoverable targets and invocation-scoped command targets are different objects;
consult the declarations for accepted target types.

`workbench.close` uses native protected closure, including dirty-document prompts.
Check its closed/cancelled result. Final document closure invalidates retained
buffers; closing a terminal stops its session. A save already accepted during
closure can remain committed even if the later close cancels.

For change-driven features, call `workbench.observe` before querying separately:
it returns an atomic initial snapshot, sequence watermark, and disposer. Later
events describe committed metadata changes; read text through buffer APIs.
`viewStateChanged` reports a window whose cursor, selection, scroll, focus,
dock sizes, bounds, or structure settled about a second after a change. Packages
activate before startup has restored windows: when the snapshot's `launched` is
false, wait for the one-time `launched` event before reconciling state against
the startup windows.
Listeners can await host work or their own disposal because commits do not wait
for listeners. A query may already see newer state than the event. Dispose and
observe again when a fresh baseline is needed.

The bundled [project registry](../extensions/@knot/projects/src/) is a
complete observer: it waits for `launched`, then tracks windows and documents.

## Acknowledge cleanup and handle failures

Await disposal to observe native release. It makes an object locally inactive
immediately and repeated calls share the same result, including failure. A
callback may await its own disposal. Await tree invalidation for native acceptance;
fetching and painting happen separately. Lifecycle teardown releases registrations
without requiring a final callback.

Handle `KnotHostError` by stable `code`, as in the
[error example](../scripts/extensions/examples/errors.js), rather than matching
message text. Narrow optional diagnostic `details` before reading fields. On
`RevisionConflict`, choose whether to abandon work or deliberately read a new
snapshot. Cancellation does not roll back a commit that already succeeded.

Keybinding registration through `keymaps.bind` and `keymaps.unbind` is
synchronous; mutations from one V8 turn apply as one atomic batch. Invalid
input fails the batch. Bindings sit above the selected keymap preset and the
latest write wins, so personal overrides of package bindings belong in
`post-init`. The activation example demonstrates registration and cleanup
through the public API. Show Keybindings lists every binding with its owner.

## Durable storage

`storage` keeps private, application-global JSON records for the package's
canonical identity; personal config has its own owner. Storage works during
activation without a window and survives reload, updates, and uninstall.

```js
import { storage } from "knot";
const result = await storage.read("preferences");
const preferences = result.found ? result.value : { schema: 1 };
await storage.write("preferences", preferences);
await storage.write("optional", null);
await storage.delete("obsolete");
const keys = await storage.list();
```

Missing records differ from stored null. Keep related state in one record for
atomic replacement: read/modify/write across calls is not atomic. Extensions own
value shapes and schema versions. Awaited writes acknowledge commit; rejection
following cancellation does not establish whether a write committed. Damaged or
incompatible databases fail without replacement.

The bundled [project registry](../extensions/@knot/projects/src/) keeps
its records in storage through public APIs only. Precise JSON validation,
ordering, cancellation, and failure contracts live in the TypeScript reference;
SQLite ownership and shutdown belong to
[host architecture](architecture/extension-host.md#extension-record-storage).

The bundled `@knot/projects` package is a worked example that combines one
registry record, a contributed tree, a folder picker, text input, confirmation,
and workspace opening through the public API only.

## Retained document captures

`recovery` keeps durable copies of open documents' text for workflows that must
restore them later, such as project sessions. Native unsaved-document protection
works without it; use it when your own records need to reference exact text.

```js
import { recovery, storage } from "knot";
const capture = await recovery.retain(document.id); // durable once resolved
await storage.write("session", { schema: 1, capture: capture.id });
await recovery.release(previous.capture); // only after the new record committed
const restored = await recovery.restore(capture.id);
```

Captures belong to your package's canonical identity; you never pass an owner,
and other packages cannot see or release yours. They are immutable, survive
unload, failure, restart, and uninstall, and are removed only by `release` or by
the user discarding them from File › Recover Unsaved Documents…, which lists
them with their owner. Reconcile `recovery.list()` with your records to release
captures a cancelled call left unreferenced. Restore opens an ordinary document
with an empty undo history and never writes the file; its `outcome` reports
whether the text replaced nothing (`restored`) or opened beside a live document
(`preserved`). Exact outcomes and errors live in the TypeScript reference.

## Project sessions

`workbench.replaceWindows` closes a set of windows and opens their replacements
in one native commit, without prompts. It is how `@knot/projects` switches
projects, and any package can build the same workflow. The caller first retains
the text of every document whose last view it removes, captures each window's
layout, records both durably, and then asks for the replacement:

```js
const capture = await recovery.retain(document.id);
const layout = await workbench.captureLayout(window.target);
await storage.write("session", { windows: [{ entries: [{ uri, document: document.id, capture: capture.id }], layout }] });
const result = await workbench.replaceWindows({
  suspend: [{ document: document.id, capture: capture.id }], // every document in the released windows
  windows: [{ roots: [root], entries: destinationEntries, layout: destinationLayout }],
  release: [window.target],
});
```

Every destination window is created with its layout and geometry, and every
released window closes as any window does, stopping its terminals. A layout is
plain versioned JSON: store it verbatim. Its document tabs find their entry by
document identity, else by file; unmatched documents and tools no longer
registered are pruned, splits of one document come back as independent views of
one shared document, and terminal tabs return in place as fresh shells. Each
window reports whether its layout was `applied` or `ignored`, the latter for an
unsupported version, in which case its entries open as without a layout.

Files load and captures restore before the commit, which then rechecks that
nothing changed since your captures: an edit after retain, an unexpected tab, or
a closed window rejects with `SessionConflict` and leaves every window intact, so
retain again and retry. Each entry reports its outcome. A live document matching
the entry's identity or file always wins over its capture, so a session never
opens a second copy of an open document. Missing clean files and entries that
fail to load are skipped and reported while the rest commits. When `protected`
is true, restored dirty text is already covered by automatic recovery and the
captures you restored from can be released once your new record is durable.

`@knot/projects` keeps one `session:<projectId>` record per project, separate
from its registry record, with each window's entries and layout. A session is
`active` while the project has windows; a window belongs to it when its roots
equal the project's roots, in any order, and the session is active, so opening a
suspended project's folders by hand never binds it. Adding or removing a folder
in a bound window updates the project and its other windows; clearing the
folders, or choosing another project's folders, takes the window out of the
project. The Projects tool's New Window action (`projects.new-window`) opens
another window on the current window's project, while File › New Blank Window
opens one outside any project. Selecting a project suspends the current one,
retaining its unsaved and untitled text and layouts in the suspended record,
and restores the destination's windows with their splits, docks, and view
state; a project may span several windows. While active, the record follows
tabs and view state live. At startup Knot itself restores every window from the
last session and only then reports `launched`; the registry then rebinds the
restored windows by their roots and suspends active projects whose windows did
not return. Closing a project's last window suspends it without captures,
because closure already asked about dirty documents. Removing a suspended
project says how many unsaved documents its record discards. A session record
written by a newer version is kept unchanged, marked unreadable in the Projects
tool, and never switched to; while one exists, no retained capture is released
as an orphan.

If a package stops responding, the palette's Developer: Restart Extension…
stops and reloads it, re-reading an installed package from disk.

## Native contributors

Handler annotation and conversion contracts live in the
[host handler module](../src/app/extensions/host/handlers.rs) and
[command declarations](../src/app/commands/declarations.rs). See
[host architecture](architecture/extension-host.md) and
[commands](architecture/commands.md) for ownership and routing.
