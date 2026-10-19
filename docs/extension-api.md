# Writing extensions for Knot

The [TypeScript declarations](../src/host/js/knot.d.ts) are the API version 1
reference for parameters, defaults, errors, completion, and lifetime rules.
This guide covers setup and common workflows. Knot runs JavaScript; TypeScript
is optional authoring tooling, not a runtime.

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

Run `scripts/check-extension-api.sh` with Node/npm to check declarations and
JavaScript examples using the lockfile-pinned TypeScript compiler. The
[checked examples](../scripts/extension-api/examples/) cover commands, providers,
cleanup, errors, and keybindings. This check runs in CI; Rust/V8 tests separately
validate runtime behavior.

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
});
```

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
severity `badge` (`neutral`, `info`, `success`, `warning`, `error`) that the
theme colors, and either kind may declare command-backed `actions`. A
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
editor surface, but edits are rejected.

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

Picker items are filtered locally by label and description, so the complete list
is supplied up front; there is no streaming provider. Inputs are single-line and
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

The checked [activation example](../scripts/extension-api/examples/activation.js)
includes `guide.compose`, which awaits another JavaScript command, and
`guide.select-all`, which awaits the native `editor.select-all` command. The
contributed view's `copy` registration shares the native command name and handles
it only when that view instance claims the command. Dispose these registrations
when removing the feature; native definitions retain their application ownership.

The [Git-status fixture](../examples/extensions/@example/git-status/dist/main.js)
uses only the public API for grouped frames, row/group/frame actions, file
activation, a read-only log frame, and refresh after actions.

## Identity and lifetime

| Identity | Ownership and validity | Persist? |
| --- | --- | --- |
| Package name | Canonical installed identity, independent of activation | Yes, as a package name |
| Runtime/config lifecycle | Hidden; config phases share their own lifecycle | No |
| Window/view IDs | Opaque live identities in a command capture | No |
| Command target/invocation | Invocation-scoped authority; invocation ID is hidden | No |
| Document ID | Application-allocated; stable across Save As and presentations | No restart reuse guarantee yet |
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
survive Save As but have no restart reuse guarantee. Retain returned live target
objects rather than serializing or reconstructing them.

Use `documents.open` for files, `createUntitled` for new documents, and `present`
for another presentation of a live document. These return committed document and
presentation data. Use `workspaces.openFolder` or `replaceRoots` for directory
roots; currently zero or one root is supported. Documents remain independent of
workspace containment. A missing file opens as a dirty destination-associated
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
Listeners can await host work or their own disposal because commits do not wait
for listeners. A query may already see newer state than the event. Dispose and
observe again when a fresh baseline is needed.

The checked [application observer](../examples/extensions/@example/application-state/dist/main.js)
loads through the product host. Run `example.application-state` to copy current
metadata and event history for inspection.

## Acknowledge cleanup and handle failures

Await disposal to observe native release. It makes an object locally inactive
immediately and repeated calls share the same result, including failure. A
callback may await its own disposal. Await tree invalidation for native acceptance;
fetching and painting happen separately. Lifecycle teardown releases registrations
without requiring a final callback.

Handle `KnotHostError` by stable `code`, as in the
[error example](../scripts/extension-api/examples/errors.js), rather than matching
message text. Narrow optional diagnostic `details` before reading fields. On
`RevisionConflict`, choose whether to abandon work or deliberately read a new
snapshot. Cancellation does not roll back a commit that already succeeded.

Keybinding registration is synchronous; mutations from one V8 turn apply as one
atomic batch. Invalid input fails the batch. The activation example demonstrates
registration and cleanup through the public API.

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

The [storage demo](../examples/extensions/@example/storage-demo/dist/main.js) uses
only public APIs. Run `example.storage-report`, restart Knot, and run it again:
its activation count increases and records remain. Precise JSON validation,
ordering, cancellation, and failure contracts live in the TypeScript reference;
SQLite ownership and shutdown belong to
[host architecture](architecture/extension-host.md#extension-record-storage).

The bundled `@knot/projects` package is a worked example that combines one
registry record, a contributed tree, a folder picker, text input, confirmation,
and workspace opening through the public API only.

## Native contributors

Handler annotation and conversion contracts live in the
[host handler module](../src/app/extension_host/handlers.rs) and
[command declarations](../src/app/command_declarations.rs). See
[host architecture](architecture/extension-host.md) and
[commands](architecture/commands.md) for ownership and routing.
