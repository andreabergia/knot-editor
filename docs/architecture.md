# Knot Code Architecture

Current component boundaries, ownership, and dependency direction. Follow the
subsystem references for runtime flows and behavioral constraints; implementation
details and API contracts live in code. [Decisions](decisions.md) records
rationale, while [deferred work](roadmap.md) and [active plans](plans/) track
work that is not yet implemented.

## System map

Knot is one Rust package whose module boundaries may become crate boundaries:

```text
gpui application and views        extension runtimes
             |                           |
             +------ editor host --------+
                         |
                  core editor model
```

- `core` owns UI- and scripting-independent editor data.
- `app` owns the gpui shell, foreground models, and native views.
- `host` contains V8 and the typed extension transport.
- `host::module_graph` validates Knot-owned, immutable package source graphs
  before they enter an extension isolate.
- `app::extension_package` owns installed-directory manifest discovery and
  captures package-local JavaScript sources without entering V8.
- `app::personal_config` selects one per-user config directory, preferring an
  existing XDG location over the platform directory, and captures optional
  phase entries and contained JavaScript sources. The host validates reachable
  static module graphs without evaluating them.
- `app::extension_load` plans dependency order and records startup outcomes
  using Knot-owned package data and a rollback guard for each attempted load.

The default `knot` binary runs the product application through `app::run` and
discovers installed extensions from the per-user local application-data root.
`knot --fixture [name]` opens the same product shell with two static diagnostic
extensions loaded through the package graph API and its application-owned pool.

## Ownership

gpui's foreground thread exclusively owns editor state. The application owns
documents, resource providers, and product command dispatch. A document owns its
buffer model and persistence identity; a buffer model owns text, session-local
edit history, anchored geometry, and semantic contributions. Each window owns
a workbench whose typed tabs own document editor views or terminal views and
identify their respective surfaces. Terminal tabs do not own document models.
An application-owned terminal session owns its
PTY, emulator grid, process, and event delivery independently of its view.

Views own presentation and interaction. Extension runtimes communicate through
Knot-owned typed messages and opaque identities. Asynchronous results revalidate
their captured target, revision or generation, and lifecycle before changing
foreground state. Native I/O runs off the foreground thread.

The application owns one extension pool. Its fixed workers enter persistent V8
isolates for one turn at a time; awaiting host work retains JavaScript state without
holding a worker. The host owns scheduling and runtime disposal, while the
application owns protocol routing and foreground resource cleanup.
Installed-package filesystem discovery runs on gpui's background executor.
The application then loads valid packages in dependency order, keeps the
startup report, and removes foreground registrations when a load fails or an
extension unloads. Product windows expose the report through a native command
and toolbar control.

## Subsystem references

- [Core and buffers](architecture/core-and-buffers.md): text storage, stable
  positions, anchored ranges, model commits, and reversible edits.
- [Documents and persistence](architecture/documents-and-persistence.md):
  document identity, dirty state, resource providers, workspaces, Open, and Save.
- [Workbench and lifecycle](architecture/workbench-and-lifecycle.md): windows,
  panes, tabs, protected closure, and terminal ownership.
- [Commands](architecture/commands.md): captured targets, focus routing,
  asynchronous outcomes, composition, cancellation, and keymaps.
- [Extension host](architecture/extension-host.md): runtime isolation, transport,
  lifecycle, snapshots, and the production scheduling direction.
- [Semantic UI](architecture/semantic-ui.md): contributions, trees, completion
  aggregation, and generated text surfaces.

## Invariants

- `core` has no platform, rendering, or scripting dependencies.
- Text and edit history are buffer state. Cursor, selection, folding state,
  scroll, and zoom are view state.
- A view need not own a text buffer.
- Stable positions and buffer edit events are one contract.
- `AnchoredRangeStore` owns geometry; visual policy belongs above `core`.
- V8 and Deno objects remain inside `host`.
- Extension requests cross boundaries as Knot-owned semantic data and opaque
  identities.
- Generic resource state uses normalized URIs; platform paths and native I/O
  errors remain inside the local provider or explicit diagnostic code.
- `BufferModel` owns editable text, never resource identity or persistence.
