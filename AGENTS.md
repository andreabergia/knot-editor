# Knot

Knot is a modern text editor, inspired in philosophy by Emacs, but built with
modern technologies and paradigms. It is implemented in Rust.

Use @docs/architecture.md for current boundaries and @docs/decisions.md for
validated choices and rationale. Load the linked @docs/architecture subsystem
references only when the task needs their runtime flows or behavioral constraints.
Read @docs/design.md only when product vision is relevant,
and @docs/roadmap.md for product sequencing and deferred work. Active execution
plans live under @docs/plans. Remove completed plans after preserving durable
behavior, rationale, and essential evidence in the current docs; Git retains
the execution history.

The architecture exploration is complete. Product development evolves the
validated prototype in place through end-to-end usable slices. Knot is now
building the real product: correctness and regression safety take priority over
prototype speed. Automated tests are required for durable behavior and every
completed slice must leave its important model, lifecycle, command, and
persistence paths covered. Use focused tests where they provide confidence,
and add UI or integration coverage when behavior cannot be validated at a
lower boundary.

No one is using the product at the moment; we are free to break any backward
compatibility concerns at any time.

## Repository map

```text
src/core    UI-, platform-, and scripting-independent text and edit primitives
src/app     gpui shell, foreground editor state, native views, resource providers,
            commands, startup, and application persistence
src/host    V8, bounded scheduling, module-graph validation, and typed transport
crates/knot-handler-macros   adapters for annotated native host handlers and commands
extensions/@knot    bundled TypeScript extension packages, compiled by build.rs
scripts/extensions  extension toolchain, fake knot host, and package test runner
```

Use the table below to route a change to its authoritative documentation and
canonical source before reading broadly.

| Change area | Documentation | Canonical source |
| --- | --- | --- |
| Text storage, history, anchored ranges | [core and buffers](docs/architecture/core-and-buffers.md) | `src/core/` |
| Document identity, dirty state, providers, Open/Save | [documents and persistence](docs/architecture/documents-and-persistence.md) | `src/app/documents.rs`, `src/app/open.rs`, `src/app/product/{open,save}.rs`, `src/app/model.rs`, `src/app/resource.rs` |
| Windows, panes, tabs, closure, terminal, tool layout | [workbench and lifecycle](docs/architecture/workbench-and-lifecycle.md) | `src/app/workbench/`, `src/app/product/` (`closure.rs`, `tools.rs`, `launch.rs`, `render.rs`), `src/app/terminal_*.rs` |
| Commands, keymaps, captured targets, focus routing | [commands](docs/architecture/commands.md) | `src/app/keymaps.rs`, `src/app/product/commands.rs` |
| Extension runtime, scheduling, lifecycle, records | [extension host](docs/architecture/extension-host.md) | `src/host/`, `src/app/extension_host.rs`, `src/app/extension_storage.rs` |
| Contributed UI, trees, completion, generated text | [semantic UI](docs/architecture/semantic-ui.md) | `src/app/extension_semantics.rs`, `src/app/tree_view.rs`, `src/app/completion.rs` |
| Extension public contract | `src/host/js/knot.d.ts`, [extension guide](docs/extension-api.md) | `docs/schemas/knot.schema.json`, `examples/extensions/` |
| Bundled extensions, authoring toolchain, extension tests | [extension guide](docs/extension-api.md#write-and-test-in-typescript), [development](docs/development.md) | `extensions/@knot/`, `scripts/extensions/`, `build.rs` |
| Application-owned SQLite stores | [automatic tool-layout memory](docs/architecture/workbench-and-lifecycle.md#automatic-tool-layout-memory) | `src/app/tool_layout.rs` |

### Registering an application subsystem

- Declare the module in `src/app/mod.rs`; keep the deliberately-unused
  `dead_code` allowances only for boundaries the product surface does not exercise.
- Expose application-wide state as a gpui `Global` (for example
  `ApplicationToolLayout`, `ApplicationDocuments`, `ApplicationExtensionHost`).
- A subsystem that writes durably off the foreground flushes or drains pending
  work from `extension_host::quit_after_storage` before `cx.quit()`.
- Extension-facing operations are annotated in
  `src/app/extension_host/handlers.rs`; `host::declarations` validates the context
  types at startup.

## Rules

- Whenever implementing a plan or design document, always keep the plan
updated. Use ✅ and other emoji to mark executed steps.
- Keep @docs/architecture.md up-to-date when a change alters architectural
  boundaries, ownership, or dependency direction. Update the relevant
  @docs/architecture subsystem reference when runtime flows or behavioral
  constraints change. Keep rationale in @docs/decisions.md and implementation
  details and API contracts in the code.
- Follow @docs/README.md for documentation ownership. Link to authoritative
  contracts rather than duplicating them; repeat only enough context for orientation.
- Code comments and module documentation must describe the current design and
  behavior only. Do not record migrations, previous locations, roadmap phases,
  or implementation history in code; Git history is the source for that.
- When executing multiple steps or tasks, commit each logical step as soon as
  it is done and verified, before starting the next one. Never accumulate
  several steps of work and try to split them into commits at the end.
- Don't use conventional commits.
- Run `cargo fmt` once at the end of Rust work, before committing. Do not use
  `cargo fmt --check` or manually format individual files.
- Keep the design simple. Let's not build overly complex abstractions "just in
case".
- There's a thing as being _too_ careful. This is a text editor, used by
humans. Incredibily unlikely edge cases shouldn't be handled.
