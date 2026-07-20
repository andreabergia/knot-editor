# Knot Code Architecture

This document is an orientation map for the code that exists today. It covers
component boundaries, ownership, and the important runtime flows.

Knot is still a prototype. Some directories contain completed experiments
rather than code on the current application path.

## System map

Knot is one Rust package with a library and several binaries. Its intended
dependency direction is:

```text
gpui application and views        extension runtimes
             |                           |
             +------ editor host --------+
                         |
                  core editor model
```

The boundaries are being validated as modules before any crate split.

The library exports four top-level modules:

- `core` owns editor data models and is independent of UI, rendering, and
  scripting.
- `app` owns the gpui application shell, editor view, foreground buffer
  models, and buffer registry.
- `host` owns the native boundary to extension runtimes.
- `view` is a renderer benchmark harness, not the future editor view layer.

## Current application paths

The default `knot` binary calls `app::run` and opens the gpui shell and editor.
It loads `rust_sample.kfx` by default and creates the active core-backed
`BufferModel`; it does not yet connect the scripting host.

The standalone `step3-gpui` binary delegates to `app::run` and uses the same
application path as the default binary.

The remaining binaries benchmark the renderer, text buffer, and annotation
models. Their supporting code and fixtures are experiments, not application
architecture.

## Core editor model

`src/core` contains three cooperating models:

- `buffer::TextBuffer` owns text, stable positions, line lookup, and the edit
  log. It is currently implemented as a stable-ID piece table.
- `annotation::AnnotationStore` owns anchored ranges from sources such as
  diagnostics, search, git, and folding.
- `transaction::EditTransaction` records one reversible group of primitive
  buffer edits. It is not an undo history manager.

Buffer offsets and ranges are UTF-8 byte offsets. Grapheme-aware movement and
other presentation concerns belong above `core`.

Annotations follow the buffer's edit stream:

```text
TextBuffer mutation
        |
        v
BufferEdit log
        |
        v
AnnotationStore::stabilize
        |
        v
annotation resolution and queries
```

The buffer and annotation Rustdoc define stable-position behavior, endpoint
semantics, and lifecycle details.

## Application model and view

gpui's foreground thread exclusively owns editor state. Each `BufferModel`
entity owns one `core::TextBuffer`, its open/closed lifecycle, and a public
revision that advances once per editor-visible atomic commit. The core
buffer's `edit_seq` remains a private primitive-edit-log cursor.

`BufferRegistry` gives models monotonic, never-reused transport handles and
stores only weak entity references. It also tracks the optional active buffer.
`EditorView` owns cursor, selection, scroll, IME, annotations, and a derived
line/segment rendering projection; local edits commit through its model and
model notifications refresh the projection.

```text
gpui Shell / BufferRegistry
             |
             v
      BufferModel entity
             |
             v
      core::TextBuffer
             |
       notifications
             v
 EditorView projection and view state
```

## Extension host

`src/host` is the Knot-owned isolation boundary around V8 and `deno_core`.
`V8Host` owns process-wide V8 initialization and shared native asynchronous
work. Each extension runs a persistent JavaScript runtime on its own OS thread
and communicates with the host through typed request and response messages.

`host::protocol` contains the transport-level identities, buffer data, and
errors without depending on V8, Deno, gpui, or the concrete core buffer. This
keeps runtime mechanics behind the host boundary and leaves editor API
dispatch as a future layer.

Extension-local lifecycle state owns pending work, termination, resource
limits, and teardown. A failed or terminated isolate does not take down other
extensions. The current JavaScript API and module loading are fixture-level
probes; command registration and the complete editor API do not exist yet.

```text
extension JavaScript
        |
        v
extension runtime thread
        |
  typed request/response
        |
        v
editor host (future integration)
```

Detailed runtime behavior and experiment results live in
`step7-v8-runtime.md` and the host module's Rustdoc.

## Architectural invariants

- `core` must not depend on platform, rendering, or scripting details.
- Text and edit history are buffer state. Cursor, selection, folding, scroll,
  and zoom are view state.
- Stable positions and buffer edit events form one contract: buffer changes
  must preserve the information annotation stabilization needs.
- `AnnotationStore` is the shared range-composition mechanism. Visual
  precedence, hit testing, and presentation belong to the future view layer.
- Performance-sensitive editor services stay native. Built-ins and extensions
  should use the same public editor APIs wherever practical.
- V8 and Deno objects remain contained within `host`; other components cross
  that boundary using Knot-owned types.

## Major gaps

- The extension runtime is not connected to the foreground buffer registry.
- Undo history, edit grouping, and view-state restoration are not implemented.
- Commands, capability aggregation, filesystem providers, terminal state, and
  the public extension API remain roadmap work.

Update this document when component boundaries, ownership, dependency
direction, or a major runtime flow changes. Keep implementation contracts in
the code and decision history in the roadmap or step-specific documents.
