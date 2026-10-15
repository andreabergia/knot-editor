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

The `application-state` package observes during activation, before product windows
exist. After opening documents, splitting or moving tabs, or opening a folder,
run **example.application-state** to copy its initial counts, current metadata,
and ordered event summary to the clipboard. Paste it into an untitled document
for inspection. Terminal tabs appear without document/buffer associations. Run
**example.stop-application-observer** to await disposal; later reports still query
current state while their event history stays unchanged.

Run **example.open-document** in another window to present the first open resource
there and copy the committed document metadata. From JavaScript, pass an explicit
URI with `commands.invoke("example.open-document", { uri: "file:///…" })`.
The operation uses the originating command window captured by the native host.

**example.open-folder** opens the first resource's parent in the originating
window, or accepts `{ uri }` directory arguments from JavaScript.
**example.clear-roots** clears that window's roots. Both copy committed roots and
generation to the clipboard and leave open documents intact.
