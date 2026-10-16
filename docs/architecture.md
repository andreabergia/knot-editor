# Knot Code Architecture

Current component boundaries, ownership, and dependency direction. Follow the
subsystem references for runtime flows and behavioral constraints; implementation
details and API contracts live in code. [Decisions](decisions.md) records
rationale, while [deferred work](roadmap.md) and [active plans](plans/) track
work that is not yet implemented.

## System map

Knot has one runtime Rust package and a compile-time handler macro crate. Runtime
module boundaries may become crate boundaries:

```text
gpui application and views        extension runtimes
             |                           |
             +------ editor host --------+
                         |
                  core editor model
```

- `core` owns UI- and scripting-independent editor data.
- `app` owns the gpui shell, foreground models, and native views.
- `host` contains V8 and the typed extension transport. Its declaration adapters
  discover annotated operations automatically; `app` supplies owned foreground
  contexts and handlers. Foreground resource owners expose typed methods, and
  declaration adapters own response transport. `knot-handler-macros` generates
  adapters without depending on V8, gpui, or editor models.
- `app::command_declarations` discovers native command metadata and adapters,
  sharing the typed async call contract with host operations. The application
  catalog retains dynamic registrations and name ownership; the captured dispatcher
  retains participant precedence and extension composition.
- `host::module_graph` validates Knot-owned, immutable package source graphs
  and personal-config source graphs before they enter an isolate.
- `app::extension_package` owns installed-directory manifest discovery and
  admits installed API versions before capturing package-local JavaScript sources
  without entering V8.
- `app::personal_config` selects one per-user config directory, preferring an
  existing XDG location over the platform directory, and captures optional
  phase entries and contained JavaScript sources. The host validates reachable
  static module graphs without evaluating them.
- `app::extension_storage` owns application-global JSON record databases selected
  by canonical package identity or personal config. Its native async boundary
  runs SQLite operations on the background executor with a reused, serialized
  connection per owner. The product host binds storage to admitted package and
  personal-config lifecycles and exposes it through typed async handlers.
  Product quit stops storage admission and awaits pending work and connection
  release asynchronously before exiting.
- `app::extension_load` plans dependency order and records startup outcomes
  using Knot-owned package data and a rollback guard for each attempted load.

The default `knot` binary runs the product application through `app::run` and
discovers installed extensions from the per-user local application-data root.

## Ownership

gpui's foreground thread exclusively owns editor state. The application owns
documents, resource providers, the product keymap registry, and command
dispatch. A document owns its buffer model and persistence identity; a buffer
model owns text, session-local edit history, anchored geometry, and semantic
contributions. Each window owns
a workbench whose typed tabs own document editor views or terminal views and
identify their respective surfaces. The workbench owns ordered tab relocation
and split transitions; the product shell coordinates transfers and window
creation on the foreground thread. A transient application-wide drag coordinator
retains weak shell/workbench targets and window-local painted geometry for
release dispatch and previews. Terminal tabs do not own document models.
An application-owned terminal session owns its
PTY, emulator grid, process, session-local terminal preferences, and event
delivery independently of its view.

Views own presentation and interaction. Extension runtimes communicate through
Knot-owned typed messages and opaque identities. Command trees retain admitted
presentation authority; contextual requests resolve it in the product host,
while explicit buffer access remains independent of presentation lifetime. Asynchronous results revalidate
their captured target, revision or generation, and lifecycle before changing
foreground state. Native I/O runs off the foreground thread.
Each product window owns its extension-backed semantic tree surface; the
application's foreground catalog binds command handlers to live view instances.

A foreground application observation owner compares document and live product
window metadata at committed transitions. It owns a session sequence and
lifecycle-scoped subscriptions, while queries derive current state from the
existing owners. No window catalog replica or durable layout model is introduced.

The application owns one extension pool. Its fixed workers enter persistent V8
isolates for one turn at a time; awaiting host work retains JavaScript state without
holding a worker. The host owns scheduling and runtime disposal, while the
application owns protocol routing and foreground resource cleanup. Foreground
cleanup is shared by lifecycle teardown and host release, so registrations and
contributions cannot outlive the host on retained models or views.
Installed-package filesystem discovery runs on gpui's background executor.
The application then loads valid packages in dependency order, keeps the
startup report, and removes foreground registrations when a load fails or an
extension unloads. Product windows expose the report through a native command
and toolbar control.
Product launch captures personal config off the foreground, awaits pre-init,
installed extensions, and post-init, then opens queued product windows. Fatal
config failures unload every startup lifecycle and show a dedicated diagnostic
window while product windows remain unavailable.

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
