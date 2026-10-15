# Writing extensions for Knot

The [TypeScript declarations](../src/host/js/knot.d.ts) are the API version 1
reference: parameter meanings, defaults, errors, completion and lifetime rules
live beside their types.

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
tree invalidation to observe completion, and use host error `code` values
when handling failures. See the API reference for precise rules.

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
outline's `copy` registration shares the native command name and handles it only
when that outline instance claims the command. Dispose these registrations when
removing the feature; native definitions retain their application ownership.

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

## Inspect application state

`documents.list()` and `documents.get(id)` return current metadata and a shared
buffer proxy. Read text through that buffer. Document IDs survive Save As and
additional presentations; reopening after closure allocates a fresh ID. No
cross-restart identity reuse is promised yet.

`windows.list()` and `windows.get(target)` return product windows, panes, ordered
tabs, selected tabs and the actual focused tab target. A terminal tab has kind
`terminal` and null document/buffer fields. Focus outside tabs has a null current
target. Diagnostic and startup-error windows are excluded.

`workspaces.get(windowTarget)` returns that window's ordered roots and generation.
An empty root set is valid; the model accepts at most one root. A retained normalized
root keeps its ID. Clearing/reinstalling changes the generation and root identity.

Queries work during activation and callbacks, including before windows exist.
They describe current state without reserving targets for later use. A closed
document/window lookup returns null. Discoverable window, pane and view targets
are opaque live objects: retain the returned objects, not copies or serialized
representations. They are separate from invocation-scoped command targets and
cannot be passed to contextual buffer/selection APIs. Replacing a workbench or
tab, or transferring a view to another pane/window, invalidates its old exact target.

`documents.open(uri, { target })` opens a file through its registered provider and
returns `{ document, presentation }` after foreground commit. `target` can be a live
window, pane or exact view. A window/pane captures its selected presentation at
admission. Omit options to use the originating command window, or the active product
window outside a command. Without a product window it rejects `ContextUnavailable`;
it never waits for a future window. Focus changes during I/O cannot retarget it,
and closure, replacement or transfer of the captured presentation rejects
`InvalidTarget` before mutation.

Open reuses an existing tab for that document in the destination window; otherwise
it creates a native editor tab in the captured pane, preserving terminal tabs.
A missing file becomes a dirty destination-associated document. Directories reject
`ResourceWrongKind`; malformed URIs, invalid UTF-8 and provider failures reject
`InvalidUri`, `InvalidUtf8` and `ProviderFailure`. A provider resource disappearing
after stat rejects `ResourceNotFound`. Cancellation or unload before commit prevents
mutation. Results contain committed metadata and opaque live targets, not rendering
surfaces. Operations finish without awaiting observation listeners; listeners can
open using explicit targets.

`documents.createUntitled(options)` creates a clean, empty ordinary document and
its native tab together. Edits make it dirty; undo to its initial content makes it
clean again. `documents.present(id, options)` shares a live document in another
window or selects its existing tab there. It rejects `DocumentNotFound` after the
document's final presentation closes. Both accept the same captured destinations
and return the same `{ document, presentation }` result as Open. Creating or
presenting from a terminal preserves the terminal and selects a document tab.

`workspaces.openFolder(uri, options)` validates an existing directory and returns
the window's committed roots/generation. `workspaces.replaceRoots(uris, options)`
accepts zero or one directory URI; an empty array clears the roots and tree.
`options.target` accepts a live window, with the same command/active-window capture
on omission. These operations do not depend on a document presentation and work in
an empty product workbench. They preserve open documents and never resolve focus
after validation starts. A newer Open/root request, changed root generation,
closed/replaced window or cancelled caller rejects pending work before commit.

Missing folders reject `ResourceNotFound`, files reject `ResourceWrongKind`, and
more than one root rejects `TooManyWorkspaceRoots`. All validation finishes before
changing either roots or tree. Retained normalized roots keep their IDs; generation
increases on replacement, including clear/reinstall, which allocates a new root ID.

`workbench.close(viewTarget)` closes that exact tab through protected closure.
A dirty document's final presentation offers Save / Don't Save / Cancel; other
presentations keep its shared model live without prompting. Cancel resolves to
`{ kind: "cancelled" }`, and a committed close resolves to `{ kind: "closed" }`.
The final document close invalidates retained buffer proxies; an empty workbench
receives the ordinary untitled replacement. Terminal closure stops that session
without removing unrelated documents.

A fabricated, replaced, transferred or closed target rejects `InvalidTarget`;
concurrent protected closure rejects `OperationInProgress`. A failed save or an
edit racing the save rejects `PersistenceFailed` and leaves the presentation
intact. Caller cancellation/unload prevents the close before commit. A save already
accepted during the flow may persist content and remains observable even if the
later close is cancelled. Close never waits for observation listeners and can be
awaited from a listener, followed by that listener's own disposal. Closing a
presentation invalidates command context captured from that presentation; existing
command target-validity and outcome rules still apply.

`await workbench.observe(listener)` registers application-wide observation and
returns an initial `snapshot`, a sequence `watermark`, and an awaitable `dispose()`.
Registration, snapshot and watermark are captured together on the foreground;
subsequent events have larger sequence numbers. The listener is installed before
queued deliveries can enter it, and the observing activation/command root finishes
first. Observe before windows exist to see their later attachment.

Events distinguish document open/change/close, window open/close, presentation
changes, workspace changes and active-target changes (including null). Document
changes carry current metadata, not buffer edit payloads. Events describe committed
changes; a query inside a listener can already see newer state. They do not promise
one event per gesture or a cross-restart layout record. There is no replay/retry
protocol; dispose and observe again for a fresh baseline.

Listeners run as asynchronous FIFO scheduler roots. They can await queries, buffer
mutations or their own disposer. Exceptions are isolated. Native commits finish
without awaiting listeners; local disposal prevents queued author callbacks and
its promise acknowledges native release. Unload, failed activation and host release
also release observation ownership.

The checked [application observer](../examples/extensions/@example/application-state/dist/main.js)
loads through the product host. Run `example.application-state` to copy its initial
counts, current document/presentation metadata and event history for inspection.

## Package and check your code

Installed packages use a `knot.jsonc` manifest and built `.js`/`.mjs` files.
The manifest requires the positive integer `apiVersion: 1`, independent of the
package’s `version` and dependency `requires`. The running host accepts exactly
API version 1 before capturing/evaluating an installed entry. Personal config has no manifest
and uses the running application’s API. Its optional `pre-init.js` and
`post-init.js` entries share one lifecycle; configuration failure is fatal to
startup.

Version diagnostics distinguish missing, malformed and unsupported declarations
and report the expected and declared versions. Rejected packages are not evaluated;
dependents fail while independent packages may load. Duplicate package identities
remain errors even when a manifest also has an invalid API version.

Static relative imports stay within the package/config source graph; dynamic
imports reject. TypeScript is optional authoring tooling, not a runtime.

The [installed examples](../examples/extensions/) demonstrate package layout.
The [checked examples](../scripts/extension-api/examples/) show captured
commands, providers, cleanup, error handling and keybinding batching.

Run `scripts/check-extension-api.sh` with Node/npm available to check the
declarations and JavaScript examples using the lockfile-pinned compiler.
Runtime correctness is validated separately by Rust/V8 tests.

A minimal manifest is:

```json
{
  "name": "@example/tools",
  "apiVersion": 1,
  "version": "1.0.0",
  "main": "dist/main.js"
}
```

Use the [manifest schema](schemas/knot.schema.json) for editor tooling.
The authoring checker runs in CI alongside Rust verification. Its checked examples
are also installed through the real product host in integration tests.

## Acknowledge cleanup and handle failures

A disposer makes its object inactive immediately; await its shared promise to
observe native release. Repeated calls retain the same success or rejection.
There is no automatic retry. Await tree invalidation to observe native acceptance;
fetching and painting happen separately. Earlier accepted mutations/invalidation
settle before cleanup acknowledgement. A callback can await its own disposal.

Narrow an `Error` by `name === "KnotHostError"` and its `code`, as in the
[error example](../scripts/extension-api/examples/errors.js). A `RevisionConflict`
means the captured revision is no longer current; choose whether to abandon the
operation or deliberately read a new snapshot. Optional `details` contains
handler-specific JSON diagnostics and can be null; narrow its value and fields
before reading them. Choose failure categories by `code`, independently of Rust
variant names or diagnostic message text. The closed TypeScript code union is
checked against native declarations. `Cancelled` does not roll back a
commit that already succeeded. Invalid argument types use `TypeError`; invalid
numeric limits and snapshot conversion coordinates use `RangeError`.

The [activation example](../scripts/extension-api/examples/activation.js) combines
captured commands, explicit-buffer callbacks, providers, awaitable cleanup and
synchronous keybindings. Keybindings registered during one V8 turn form one atomic
batch: invalid input fails that entire batch through its turn result.

Current contextual selection supports admitted outline handlers. Restart-stable
document identity, durable storage and new native contribution protocols remain
follow-up work; retaining an ID does not extend its lifetime or grant authority.

Native contributors can find the annotation, argument/result and error contracts
in the [host handler module](../src/app/extension_host/handlers.rs) and
[command declarations module](../src/app/command_declarations.rs). Runtime
ownership and routing are documented in the
[extension host](architecture/extension-host.md) and
[command architecture](architecture/commands.md) references.
