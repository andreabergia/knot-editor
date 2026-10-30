# Knot

**A modern programmable editor.**

Knot is a text editor inspired by Emacs: everything is scriptable, but with a
native GPU-rendered interface, asynchronous execution, and language tooling
built in. It is written in Rust on [gpui](https://www.gpui.rs), and you extend
it in TypeScript or JavaScript.

Website: <https://knot-editor.com>

> **Status: early development.** There are no releases yet. Knot is being
> dogfooded on macOS; Windows and Linux build and run but get less attention.
> Expect rough edges and breaking changes.

## Why Knot

- **Programmable to the core.** Configuration, keybindings, commands, and
  extensions are ordinary JavaScript. Built-in workflows use the same public API
  your extensions do, with no special privileges.
- **Native and responsive.** Rendering, text shaping, and input stay in Rust.
  Extensions run in their own V8 isolates and never block typing or painting.
- **Your work is safe.** Unsaved text is saved to a recovery store as you type.
  Quitting is a hot exit: the next launch brings back your windows and unsaved
  documents, even if an extension misbehaves.
- **Replaceable workflows.** Higher-level features, such as named projects, ship
  as bundled extensions you can replace with your own.

## What works today

- Editing with undo/redo, several views of one buffer with their own cursor
  and scroll, and correct handling of CJK, emoji, and right-to-left text
- Tree-sitter syntax highlighting for about twenty languages, including Rust,
  TypeScript, Python, Go, C/C++, and Markdown
- Themes: One Dark, One Light, Catppuccin, Tokyo Night, and Solarized, with a
  light/dark/system mode
- Multiple windows, split panes, and tabs you can drag between them
- Multi-root workspaces with a file explorer
- An integrated terminal (backed by `alacritty_terminal`) that opens in your
  project's folder and survives moving between panes and windows
- A command palette, plus settings and keybindings you can override from your
  config
- Named projects that switch with their layouts and unsaved text
- Extensions that contribute commands, dockable tree and text views, pickers,
  prompts, decorations, themes, languages, and completion

Not there yet: Find, line numbers, a status bar, and language servers. Work
toward a daily-driver editor is under way; the [roadmap](docs/roadmap.md) has
the details.

## Build and run

You need:

- [Rust via rustup](https://rustup.rs). The repository pins its toolchain, and
  rustup installs it automatically.
- [Node.js](https://nodejs.org) and npm. The build compiles the bundled
  TypeScript extensions.
- [just](https://github.com/casey/just) (optional), for the shortcuts below.

```bash
just run
```

That is the same as `cargo run --bin knot`. Other useful recipes:

| Command | What it does |
| --- | --- |
| `just check` | Format, lint, and run the Rust and extension tests |
| `just app` | Build a local `Knot.app` bundle on macOS |
| `just ext` | Type-check and test the extension packages |

[docs/development.md](docs/development.md) covers CI and the toolchain in more
detail.

## Make it yours

Your personal configuration is plain JavaScript in `~/.config/knot/` (or
`$XDG_CONFIG_HOME/knot/`). Knot uses that folder if it exists, and otherwise
your platform's config folder. It runs an optional `pre-init.js` before
extensions load and `post-init.js` after. For example, in `post-init.js`:

```js
import { commands, keybinding, settings } from "knot";

await settings.set("theme.mode", "system");
await settings.set("editor.tabSize", 2);
await settings.set("editor.tabSize", 4, { language: "python" });

await commands.register("me.insert-date", async ({ buffer }) => {
  if (buffer === null) return;
  const snapshot = await buffer.snapshot();
  await buffer.applyEdits([{
    range: { startByteOffset: 0, endByteOffset: 0 },
    text: new Date().toDateString() + "\n",
  }], { ifRevision: snapshot.revision });
});

keybinding("cmd-shift-d", "me.insert-date");
```

If your config throws, Knot shows an error window instead of starting with a
half-applied setup.

## Write an extension

An extension is a directory with a `knot.jsonc` manifest and built JavaScript,
dropped into Knot's `extensions` folder (on macOS,
`~/Library/Application Support/Knot/extensions/`). Packages are usually written
in TypeScript and tested with `node:test` against an in-memory fake of the
editor.

- [Extension guide](docs/extension-api.md): setup, testing, and common workflows
- [API declarations](src/host/js/knot.d.ts): the precise reference
- [Git status example](examples/extensions/): a complete installed package
- [Projects](extensions/@knot/projects/): the bundled project manager

## Learn more

- [Design](docs/design.md): where Knot is headed and why
- [Architecture](docs/architecture.md): how the code is organized
- [Decisions](docs/decisions.md): the choices we made, with the evidence
- [Roadmap](docs/roadmap.md): what's selected next and what's deferred

## License

Knot is free software, licensed under the [GNU GPL v3](LICENSE).
