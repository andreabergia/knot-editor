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

For checkpoint 3's manual smoke check, use the updated package and product build:

1. Open a file in window A. In window B, run **example.open-document**: the same
   document should appear there and the clipboard should report its committed ID.
2. In B, run **example.open-folder**, then **example.clear-roots**: the tree should
   open the file's parent, then clear; document tabs should remain open.
3. In A, run **example.create-untitled**, then edit that document. In B, run
   **example.close-created-document** and choose Cancel in A's dirty-close prompt:
   the tab/content should remain, and the clipboard should say `{"kind":"cancelled"}`.
   Run it again and choose Don't Save to close the document; the clipboard should
   say `{"kind":"closed"}`. Running the command from B preserves its command target
   while the captured presentation in A closes.
4. Run **example.application-state** to inspect document/presentation events and
   the workspace generations. Report the results for checkpoint approval.
