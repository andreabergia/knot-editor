# Installed extension example

`@example/git-status` is an installed package written in TypeScript. Build it
with the repository's pinned toolchain, which emits `dist/` beside `src/`:

```sh
npm --prefix scripts/extensions ci
npm --prefix scripts/extensions run build examples/extensions/@example/git-status
```

Then copy the `@example` directory into Knot's per-user `extensions` directory
and restart Knot. On macOS the destination is
`~/Library/Application Support/Knot/extensions/@example`, and
`just install-example-apps-mac` builds and copies it in one step. On Linux it is
`~/.local/share/Knot/extensions/@example`; on Windows use the local app-data
`Knot\extensions\@example` directory. Knot loads only `knot.jsonc` and the
built JavaScript. Use the **Developer: Show Extension Startup Report** command
to inspect load results.

The manifest declares `apiVersion: 1`. See the
[extension author guide](../../docs/extension-api.md) for the API reference,
manifest schema, lifetime rules, and checked examples.

The package contributes a **Source Control** frame and a **Source Control Log**
text frame using fixture data; it never touches a real repository. The frame
groups staged files and changed files, shows Git-style status badges, and
exposes row, group, and frame actions. Click a file to copy its path and add it
to the log; use Stage, Unstage, Discard, Stage all, and Unstage all to mutate the
fixture and refresh both frames. Move the selection and press Right to navigate
a row's actions from the keyboard. The log frame is a read-only text frame
replaced through `setText`. Both frames are driven only through the public API.

A **Repositories** group demonstrates the transient interactions. **Switch
repository** in the frame toolbar opens a searchable picker; **Add** on the group
header opens a text input for a path; **Rename** and **Remove** on a repository
row open an input and a native confirmation. The same picker and input are
triggered by **example.git.select-repository** and
**example.git.add-repository** from the command palette, and each flow awaits
its result inside the extension command before refreshing the frame.

`src/model.ts` holds the fixture state and tree as plain data, and
`src/extension.ts` exports `start()`, which registers the frames and commands;
`src/main.ts` only calls it.
