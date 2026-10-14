# Installed extension example

Copy the `@example` directory into Knot's per-user `extensions` directory,
then restart Knot. On macOS the destination is
`~/Library/Application Support/Knot/extensions/@example`; on Linux it is
`~/.local/share/Knot/extensions/@example`. On Windows use the local app-data
`Knot\extensions\@example` directory.

Open the command palette and run **example.greet**. The `word-tools` package
imports its local `dist/names.js` module and invokes the command registered by
its `text-utils` dependency. Use the toolbar's **extensions** button or the
**Show Extension Startup Report** command to inspect load results.

Each installed manifest declares `apiVersion: 1`. See the
[extension author guide](../../docs/extension-api.md) for the API reference,
manifest schema, lifetime rules and checked examples.
