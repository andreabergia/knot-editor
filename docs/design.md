# Knot — A Modern Programmable Editor

Knot is a programmable editing environment inspired by Emacs, with a modern
native interface, asynchronous execution, and language tooling. It should feel
lightweight, fast, inspectable, and extensible.

This document describes product direction, including capabilities still to be
built. [Architecture](architecture.md) describes the current implementation;
[decisions](decisions.md) explains validated choices, and the
[roadmap](roadmap.md) identifies selected and deferred work.

## Programmability

Configuration, extensions, keybindings, and workflows use JavaScript. Built-in
high-level behavior should use the same public APIs available to extensions
wherever practical. Bundled origin must not grant privileged editor APIs.

The native core supplies inseparable mechanics: text editing, shaping, input,
document persistence, workbench ownership, scheduling, and capability
aggregation. User-visible programmability does not require exposing Rust, gpui,
GPU, or platform objects, or implementing every feature in script.

Configuration is ordinary code, with its own startup and failure policy.
Packages add distribution and lifecycle ownership, rather than a different
programming model.

## Text and views

Text is the preferred representation when content benefits from ordinary
selection, scrolling, copying, and editing: search results, logs, help, and many
version-control workflows. Semantic controllers retain identity and actions;
features should not recover semantics by parsing displayed text.

Use concrete native surfaces where text-buffer semantics do not fit, such as
terminals and trees. Do not force them into a common polymorphic buffer model.
A buffer may have several views with independent cursor, selection, folding,
scroll, and zoom state. Shared annotations describe buffer facts; views decide
how to present them.

Terminal support is a native editor capability backed by a mature emulator.
The session survives view reconstruction and window transfer. Simultaneous
views of one terminal session are outside current scope because resize and
input authority would need another policy.

## Filesystem and workspaces

A workspace is a set of URI-addressed filesystem roots, not a privileged project
object. Documents can be opened independently of those roots. Local files are
the default; provider boundaries should also accommodate remote or virtual
resources without changing generic document ownership.

Named projects, membership, and restoration policy belong to extensions. A
project can combine roots, document references, and presentation metadata without
making the native application understand project names or workflow policy.

## Commands and responsiveness

Commands are reusable semantic operations, discoverable in the palette, bound
to keys, callable from scripts, and composable. Raw input protocols such as IME,
pointer motion, scrolling, and terminal bytes remain native view behavior.
Commands capture their target before focus changes and preserve that intent
across asynchronous work.

Expensive work must not block the interface: language servers, indexing,
search, Git, external commands, and optional AI services all cross asynchronous
boundaries. Independent extensions may progress concurrently. Runtime isolation
alone is not a security boundary; cancellation and resource policy must reflect
real workloads. Current mechanisms and remaining policy work are described in
[extension host architecture](architecture/extension-host.md) and the roadmap.

## Extension UI and capabilities

Extensions contribute semantic content, actions, decorations, gutters, native
views, panels, and transient interactions. Native code owns rendering, shaping,
hit testing, IME, and raw input. Selected high-level surfaces, such as completion,
hover, and the palette, should be replaceable without requiring extensions to
reimplement low-level editor mechanics.

Distinguish capability providers from their presentation. Several completion or
diagnostic sources may contribute together, while one selected surface presents
the aggregate. Composition rules belong to each capability, not a universal
provider framework. A general declarative widget tree is not planned; arbitrary
UI needs must first demonstrate why semantic surfaces are insufficient.

Language support and editing features remain independent. A language provider
can supply syntax, indentation, parsing, embedded languages, and text objects.
Features such as diagnostics, completion, formatting, spelling, Git, and AI can
combine on the same buffer. Tree-sitter and LSP contribute complementary
information rather than defining exclusive modes.

## Native mechanics and bundled workflows

Current-buffer Find, editing, persistence, and workbench ownership are native
mechanics. Replaceable workflows such as Go to File, recent-file navigation,
and cross-file search should ship as ordinary bundled extensions using public
primitives. Users should be able to disable or replace them independently.

For example, cross-file search can vary its matching engine, ignore rules,
scope, ordering, and presentation. Those choices belong in the extension;
provider-neutral resource access and opening remain native primitives. If a
bundled workflow needs a private host bypass, the public boundary is incomplete.

## Recovery and session policy

The Rust application owns durable preservation and recovery of unsaved content,
independently of extension health. Extensions own project membership and session
restoration policy, storing document references and presentation metadata rather
than the only recovery copy of text.

The intended APIs support restoring documents, layouts, and view state. Native
recovery must remain available when an extension or its session metadata fails.
Native recovery and extension-retained captures are implemented; session,
layout, and view-state restoration are planned in the
[extension platform initiative](plans/extension-platform-and-projects-plan.md).

## Non-goals

- Real-time collaborative editing and CRDTs. External screen sharing and
  communication cover the intended collaborative workflows.
- A generic model abstraction shared by editable text, terminals, trees, and
  binary resources. Shared abstractions need demonstrated common semantics.
- Out-of-process extensions by default. A process boundary remains an option
  if stronger isolation proves necessary.
- AI as a mandatory editor concept. AI integrations may become optional
  capabilities when concrete workflows justify them.
