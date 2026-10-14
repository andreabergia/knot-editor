# Writing extensions for Knot

**Draft author guide.** This is intended as official extension-author
documentation once the foundation is implemented and validated. The proposed
contracts below do not all ship yet.

The [documented TypeScript declarations](../src/host/js/knot.d.ts) are the API
reference: parameter meanings, defaults, errors, completion and lifetime rules
live beside their types. Pending work is tracked in the
[implementation plan](plans/d046-public-api-foundation-plan.md).

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

## Package and check your code

Installed packages use a `knot.jsonc` manifest and built `.js`/`.mjs` files.
The proposed manifest requires `apiVersion: 1`, independent of the package’s
`version` and dependency `requires`. Version admission is pending implementation;
current manifests do not accept this field yet. Personal config has no manifest
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
The [checked draft examples](../scripts/extension-api/examples/) show captured
commands, providers, cleanup, error handling and keybinding batching.

Run `scripts/check-extension-api.sh` with Node/npm available to check the
declarations and JavaScript examples using the lockfile-pinned compiler.
Runtime correctness is validated separately by Rust/V8 tests.
