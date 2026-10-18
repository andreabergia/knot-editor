# Knot Code Architecture

Current boundaries, ownership, dependency direction, and cross-cutting
invariants. [Subsystem references](#subsystem-references) explain relevant runtime
flows; implementation details and API contracts live in code.
[Decisions](decisions.md) records rationale, while [roadmap](roadmap.md) and
[active plans](plans/) track unimplemented work.

## System map

Knot has one runtime Rust package and a compile-time handler macro crate:

```text
gpui application and native views
             |            ^
             v            | foreground requests / results
       core models     typed host boundary
                              |
                     extension runtimes
```

- `core` owns UI-, platform-, and scripting-independent text and edit primitives.
- `app` owns the gpui shell, foreground editor state, native views, resource
  providers, commands, package discovery, startup, and extension storage.
- `host` owns V8, bounded scheduling, immutable module-graph validation, and
  Knot-owned typed transport. Its public control surface and protocol expose no
  V8 or gpui objects.
- `knot-handler-macros` generates adapters for annotated native host handlers
  and commands, without depending on V8, gpui, or editor models. Application
  owners expose semantic operations; declaration adapters own transport.

The default `knot` binary enters the product through `app::run`.

## Ownership

gpui's foreground thread exclusively owns editor state; native I/O runs off it.
The application owns documents, resource providers, the command catalog and
keymap registry, terminal sessions, and one extension pool.

A document owns persistence identity and a shared `BufferModel`. The model owns
text, session-local history, anchored geometry, and semantic contributions.
Each window owns a workbench whose split panes contain typed document, terminal, or native tool tabs.
The workbench retains native tool definitions independently of visible tab
attachments, including dock placement and hide/reopen state. The application
owns one versioned tool-only layout snapshot in SQLite; fresh windows inherit
the last user-customized arrangement while already-open windows stay
independent. Views own presentation and interaction; terminal sessions own
PTYs, processes, and emulator state independently of their sole attached view.
The workbench owns layout transitions; the product shell coordinates windows,
tab transfers, and application-wide protected closure.

Extension runtimes retain JavaScript state in persistent isolates. Host workers
enter one turn at a time and yield while awaiting native work. The application
owns foreground protocol routing and resource cleanup; the host owns scheduling
and runtime disposal. Lifecycle teardown and host release share foreground
cleanup so registrations and contributions cannot outlive their host.

Application queries derive metadata from existing document and window owners.
Observation retains a comparison baseline and lifecycle-scoped subscriptions,
not a second ownership catalog. Each window owns its native semantic surfaces;
extensions supply asynchronous content rather than rendering callbacks.

## Invariants

- Text and history are shared buffer state. Cursor, selection, folding, scroll,
  and zoom are view state. A view need not have a text buffer.
- `BufferModel` owns editable text, never resource identity or persistence.
- Stable positions and edit events form one contract. `AnchoredRangeStore` owns
  geometry; feature and presentation policy belong above `core`.
- V8 objects stay inside `host`; extension requests carry Knot-owned semantic
  data and opaque identities.
- Commands retain admitted presentation authority across awaits. Explicit buffer
  access is independent of presentation lifetime.
- Asynchronous results revalidate captured targets, revisions or generations,
  and lifecycle before changing foreground state.
- Generic resource state uses normalized URIs. Platform paths and native I/O
  errors stay in providers or explicit diagnostic code.
- Native painting and raw input never synchronously call JavaScript.

## Subsystem references

- [Core and buffers](architecture/core-and-buffers.md): text storage, stable
  positions, anchored ranges, model commits, and reversible edits.
- [Documents and persistence](architecture/documents-and-persistence.md):
  identity, dirty state, resource providers, workspaces, Open, and Save.
- [Workbench and lifecycle](architecture/workbench-and-lifecycle.md): startup,
  windows, panes, tabs, protected closure, relocation, and terminal ownership.
- [Commands](architecture/commands.md): captured targets, focus routing,
  composition, cancellation, and keymaps.
- [Extension host](architecture/extension-host.md): runtime isolation, transport,
  scheduling, lifecycle, observation, and extension storage.
- [Semantic UI](architecture/semantic-ui.md): contributions, trees, completion
  aggregation, and generated text surfaces.
