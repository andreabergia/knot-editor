# Writing extensions for Knot

**Draft author guide.** This is intended as official extension-author
documentation once the foundation is implemented and validated. The proposed
contracts below do not all ship yet.

The [documented TypeScript declarations](../src/host/js/knot.d.ts) are the API
reference: parameter meanings, defaults, errors, completion and lifetime rules
live beside their types. The [implementation plan](plans/d046-public-api-foundation-plan.md)
contains the internal audit and runtime gaps.

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

## Package and check your code

Installed packages use a `knot.jsonc` manifest and built `.js`/`.mjs` files.
The proposed manifest requires `apiVersion: 1`, independent of the package’s
`version` and dependency `requires`. Version admission is pending implementation;
current manifests do not accept this field yet. Personal config has no manifest
and uses the running application’s API.

Static relative imports stay within the package/config source graph; dynamic
imports reject. TypeScript is optional authoring tooling, not a runtime.

The [installed examples](../examples/extensions/) demonstrate package layout.
The [checked draft examples](../scripts/extension-api/examples/) show captured
commands, providers, cleanup, error handling and keybinding batching.

Run `scripts/check-extension-api.sh` with Node/npm available to check the
declarations and JavaScript examples using the lockfile-pinned compiler.
Runtime correctness is validated separately by Rust/V8 tests.
