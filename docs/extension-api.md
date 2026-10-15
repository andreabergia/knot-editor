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
operation or deliberately read a new snapshot. `Cancelled` does not roll back a
commit that already succeeded. Invalid argument types use `TypeError`; invalid
numeric limits and snapshot conversion coordinates use `RangeError`.

The [activation example](../scripts/extension-api/examples/activation.js) combines
captured commands, explicit-buffer callbacks, providers, awaitable cleanup and
synchronous keybindings. Keybindings registered during one V8 turn form one atomic
batch: invalid input fails that entire batch through its turn result.

Current contextual selection supports admitted outline handlers. Application
queries/events, live window/view targets, persistent document identity, resource
operations, storage and new native contribution protocols remain follow-up work;
retaining an ID does not expose those capabilities.
