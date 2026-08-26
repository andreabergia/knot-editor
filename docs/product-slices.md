# Product Slice Horizon

This document records the agreed direction after the architecture exploration.
It is intentionally high level: only the active slice has an execution plan,
and later slices are refined when the preceding product is usable enough to
provide evidence.

Production development remains vertical. Native services and extension APIs
are introduced by the user-facing slice that needs them, not in advance as a
foundations phase. User-facing semantic operations are commands so menus,
keybindings, the palette, and JavaScript share one invocation path.

Knot follows VS Code's default macOS keybindings where an established binding
fits the interaction, while retaining native macOS conventions for platform
operations such as Open and Save.

## 1. Dogfoodable text editing

Status: planned in [slice1-dogfood-editing-plan.md](slice1-dogfood-editing-plan.md).

Make Knot safe and practical for editing its own UTF-8 source on macOS. The
slice includes native windows and file dialogs, tabs and splits, new and
existing documents, ordinary editing, undo/redo, safe persistence, and
current-file Find. It establishes the real workbench and document ownership
model while evolving the prototype in place.

## 2. Integrated terminal

Move the validated Alacritty terminal into the product workbench. A stable
terminal session owns the PTY, emulator, and subprocess lifecycle while tabs
and splits own disposable terminal views. Terminal creation, splitting,
movement, closure, and restart use the same command-first interaction model as
text documents.

The slice targets a dependable local interactive shell inside ordinary windows
and layouts. Broader compatibility, remote PTYs, detached persistence, and
shell integration remain deferred until terminal dogfooding requires them.

## 3. Programmable file navigation

Productionize the minimum extension path by shipping two useful bundled
JavaScript extension commands through public APIs:

- Go to File uses the VS Code macOS binding `cmd-p`.
- Open Recent uses the VS Code macOS binding `ctrl-r`.

Both commands use a focused, palette-like file-selection experience with fuzzy
completion. The exact presentation boundary is decided by the implementation;
this horizon does not prescribe a general picker API or widget protocol.

The native application owns durable editor primitives:

- an application-level, persisted, deduplicated history of recently opened
  resource-backed documents;
- an asynchronous, cancellable, provider-neutral workspace file query filtered
  and ordered for fuzzy path search; and
- explicit resource arguments for the registered file-open command.

These services belong to the application layer rather than `src/core`, which
remains the UI- and scripting-independent text model. They are exposed to the
bundled extension through the same typed public boundary available to other
extensions.

The extension owns the commands, default bindings, orchestration, and selected
file-navigation behavior. This slice is the concrete reason to introduce the
required production extension loading, scheduling, resource APIs, and narrow
presentation capability. It must not build a general extension foundation
before the bundled features exercise it.

## Later slices

Later ordering is deliberately open. Daily use of editing, terminals, and the
first bundled extension determines whether navigation depth, language-aware
editing, workspace management, platform support, or another capability should
come next. Known candidates and their reconsideration triggers remain in
[deferred.md](deferred.md).
