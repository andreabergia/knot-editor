# Knot - A Modern Programmable Editor

## Introduction

This project aims to build Knot, a modern programmable editor inspired by the philosophy of Emacs while embracing contemporary expectations regarding user interface, performance, asynchronous execution, and language tooling.

The goal is not to recreate Emacs, nor to build another IDE. Instead, the objective is to create a programmable editing environment where:

- the editor itself is programmable;
- commands are first-class objects;
- user-visible editor behavior is exposed through the extension API;
- the user interface is customizable;
- text remains central;
- modern tooling such as Tree-sitter and LSP integrate naturally.

The editor should feel lightweight, fast, inspectable, and extensible.

---

# Philosophy

## Everything is programmable

The editor is fundamentally an embedded runtime wrapped around a small native core.

Configuration, extensions, commands, keybindings, UI customizations, and workflows are written in the embedded language.

There is no strong distinction between:

- user configuration;
- packages;
- built-in functionality.

Packages are simply collections of code.

The native core provides the performance-critical substrate: buffers, rendering integration, scheduling, capability aggregation, and the host API. Built-in behavior should be authored against the same public APIs exposed to extensions wherever practical. The important invariant is not that every byte of editor implementation is scripted, but that built-ins do not rely on privileged editor APIs unavailable to users.

---

## Text remains important

Text is the primary representation for most editor functionality.

Features such as:

- file browsers;
- search results;
- version control interfaces;
- help systems;
- diagnostics;
- logs;

should preferably be represented as editable or inspectable text buffers.

This preserves the flexibility and discoverability that made Emacs successful.

---

## The filesystem is the workspace

The editor does not introduce a separate project model.

A workspace is simply:

- one or more folders;
- optionally individual files.

Resources are identified by URI, allowing the filesystem abstraction to extend beyond local files. The built-in local filesystem provider is the default implementation. Extensions may register providers for other URI schemes.

Examples:

- `file:///home/user/foo.rs` — local file;
- `ssh://host/path/to/file` — remote file;
- `zip:///archive.zip!/entry.txt` — archive entry.

A `FileSystemProvider` interface defines the operations needed for filesystems to participate in editor workflows. The minimum surface includes open, read, write, watch, stat, directory enumeration, URI normalization, and capability discovery. Providers may also expose optimized operations such as search, atomic write/rename, and remote process integration. This allows remote and virtual filesystems to participate naturally without changing the core workspace model.

Workspaces are created implicitly when a file or folder is open, though multiple files and folders can be added to a workspace. A workspace can be saved as a simple text file in the filesystem, if they contain more than one root directory or files.

---

## Asynchronous by default

Potentially expensive operations must never block the interface.

Examples include:

- language servers;
- indexing;
- search;
- git operations;
- AI services;
- external commands.

---

## The UI is extensively customizable

Extensions can contribute custom views, panels, sidebars, popups, status items,
annotations, decorations, gutters, and contextual actions. Selected high-level
surfaces, such as completion, hover, and the command palette, are replaceable.

The native platform shell and the fundamental text editor view are part of the
trusted core. Extensions are not expected to reimplement text shaping, hit
testing, selection, IME, accessibility, or low-level rendering. Instead, the
text view exposes public contribution points that allow extensions to alter its
presentation and behavior substantially.

Built-in high-level behavior should use the same editor-state and contribution
APIs exposed to extensions wherever practical. This is a promise of
user-visible extensibility, not literal access to native Rust, gpui, GPU, or
platform internals.

---

# Core Models

## Workspace

A workspace is a collection of filesystem roots, addressed by URI.

```text
Workspace
    roots[]  (URIs)
```

---

## Buffers

Buffers represent editable or inspectable text. The prototype has one buffer
type: `TextBuffer`.

Represents editable text.

Examples:

- source files;
- markdown;
- logs;
- directory listings;
- git status;
- search results.

Binary resources and terminals do not need to implement a common buffer
abstraction merely to participate in the UI. Introduce another buffer type only
if a future resource demonstrates useful buffer semantics.

---

## Views

Views are the primary content instances placed in windows. Some views render a
buffer; others own a surface-specific model.

An `EditorView` renders a `TextBuffer`. A single text buffer may have multiple
editor views with independent presentation state.

Examples:

- two editors displaying the same file;
- source and preview views;
- different zoom levels;
- minimaps.

Views contain presentation and interaction state:

- scroll position;
- cursor state;
- selections;
- rendering options.

A `TerminalView` is the sole presentation of an application-owned
`TerminalSession`. The session owns the pseudoterminal, process, emulator state,
and authoritative grid size; the view owns presentation and input state. A
terminal is not a `Buffer`. The view can be rebuilt or moved to another window
without restarting the session. Sharing one session between simultaneous views
is intentionally unsupported because independently sized or focused views
would introduce ambiguous resize and input semantics.

Terminal support remains a core editor capability, not an optional extension
or text-buffer emulation. Knot should reuse a mature terminal parser/state
machine rather than implement escape-sequence handling from scratch.

---

## Windows

Windows are layout containers.

Windows contain views.

Windows may:

- split horizontally;
- split vertically;
- be arranged in trees;
- participate in tabs or workspaces.

---

## Widgets

The editor discourages specialized UI components that bypass its native view
and semantic-surface APIs. Text-oriented content should normally remain
buffer-backed. Native views may own a surface-specific model where text-buffer
semantics do not fit, as with terminals and trees. Widgets are used where there
is no independent content view, for example an autocompletion popup triggered
by an editor view.

---

# Commands

Commands are first-class objects representing reusable semantic editor
operations.

Commands are distinct from raw input events such as pointer motion, scroll
deltas, focus changes, and IME composition updates. Input handlers and widgets
may translate interactions into commands where doing so is useful.

Examples:

- save-buffer
- split-window
- next-error
- rename-symbol

Commands may:

- appear in command palettes;
- receive key bindings;
- be invoked programmatically;
- be composed.

A command invocation captures context rather than assuming every operation
targets a buffer. The context identifies the focused view, its optional
associated `TextBuffer`, the containing window and workspace, and the
invocation itself. The captured target remains stable across asynchronous
work; implementations revalidate it before mutation rather than retargeting a
command when focus changes.

Native commands route from the focused widget through its view, window,
workspace, and global scopes. This lets generic commands such as copy,
select-all, and close-view acquire surface-appropriate behavior without making
terminal, tree, and editor state implement one data model. Extension APIs
receive semantic context and opaque handles, never native view or gpui objects.

---

# Capabilities

The editor defines a fixed, small set of well-known capabilities that the core understands and can coordinate. Extensions may define additional custom capabilities beyond this set.

Well-known capabilities include:

- diagnostics;
- completion;
- formatting;
- folding;
- hover;
- references;
- renaming.

This hybrid model ensures interoperability between extensions while preserving flexibility. For example, two independent completion extensions can both contribute results to a single shared completion UI, because the core knows how to aggregate completion providers.

## UI Surfaces vs. Capability Providers

A key distinction governs how capabilities are rendered:

- **Capability providers** (completion sources, diagnostic sources, formatters): multiple providers may be active simultaneously; the core aggregates their results.
- **UI surfaces** (completion popup, command palette, hover tooltip, diagnostic display): one active provider at a time, replaceable by extensions.

The editor ships with default UI surface implementations. An extension may replace any surface by registering a new provider for it. The UI surface simply renders whatever the aggregated capability providers return.

This means, for example, that the completion popup is overridable — a plugin can replace the built-in popup entirely — while multiple completion sources continue to contribute results regardless of which popup is active.

---

# Languages and Features

The editor separates language support from editor features.

## Language

A language provider is responsible for:

- syntax highlighting;
- indentation;
- parsing;
- embedded languages;
- text objects.

Examples:

- Rust
- Markdown
- HTML

---

## Features

Features provide additional functionality.

Examples:

- diagnostics
- completion
- formatting
- spell checking
- git integration
- AI assistance

A buffer may have many active features.

This replaces the traditional distinction Emacs has between major and minor modes.

---

# Semantic Information

The editor enriches documents through independent providers.

```text
Tree-sitter ─┐
LSP         ├─→ capability aggregation ─→ views and commands
Providers   ─┘
```

Providers may run concurrently, operate on different document revisions,
overlap in what they contribute, or be temporarily unavailable. The model does
not assume that one provider consumes or supersedes another; composition rules
belong to each capability.

Tree-sitter provides:

- syntax trees;
- folding;
- structural selections;
- symbols.

LSP provides:

- diagnostics;
- references;
- renaming;
- completion.

Additional providers may contribute further information.

---

# Annotations

Annotations are attached to buffers.

Examples include:

- diagnostics;
- search matches;
- git changes;
- bookmarks;
- folding regions;
- breakpoints;
- highlights.

Annotations do not modify the underlying text.

Views decide how annotations are rendered.

This allows features to compose naturally.

Some annotations describe shared facts about a buffer, while views own presentation choices. For example, a folding provider may contribute foldable regions as annotations, but whether a particular region is currently collapsed belongs to each view. Rich visual conflict policy between overlapping annotations is a UI concern that can evolve after the storage and query model is proven.

---

# Extension System

Extensions execute in isolated runtime contexts. Independent extensions may
run concurrently and must not block the user interface.

There is no strict distinction between:

- configuration;
- packages;
- built-in code.

Extensions may provide:

- commands;
- views;
- language support;
- features;
- annotations;
- keymaps;
- filesystem providers;
- UI surface implementations;
- capability providers.

## Built-in mechanics and bundled extensions

Knot distinguishes inseparable editor mechanics from opinionated product
workflows. Text editing, current-buffer Find, document persistence, and
workbench ownership are built into the native application because the editor
cannot function coherently without them.

Workflows whose policy is meaningfully replaceable ship as bundled extensions.
Examples include Go to File, recent-file navigation, and search across files,
where users may reasonably prefer different matching engines, scope rules,
ignore behavior, ordering, or presentation.

A bundled extension is a normal extension with a distribution relationship to
Knot:

- it ships and updates with Knot;
- it is enabled by default, remains visible, and can be disabled independently;
- users may replace it by disabling it and installing another extension;
- it uses only the typed public APIs available to user-installed extensions;
  and
- disabling it removes all of its commands, keybindings, providers, and
  contributions through the ordinary extension lifecycle.

Bundled origin grants no privileged editor API. If bundled behavior requires a
private host path, the public extension boundary is incomplete. The native
application may still provide durable primitives such as recent-resource
history, provider-neutral workspace file queries, resource opening, and native
presentation. Extensions compose those primitives into replaceable workflows.

Current-file Find remains built in because it is a direct interaction with one
editor buffer. Search across files is bundled behavior because choosing grep,
vimgrep, ripgrep, fuzzy matching, ignore semantics, workspace scope, and result
presentation is product policy rather than a fundamental text-model operation.

## Isolation and cancellation

Runtime isolation is not by itself a security boundary. A misbehaving extension
must not be able to freeze the editor indefinitely, exhaust resources
unchecked, corrupt core state, or prevent cancellation of work it started.
The scheduling and enforcement mechanisms are implementation decisions to be
validated by the prototype. The current runtime experiments and candidate
mechanisms are recorded in the archived
[V8 runtime exploration](archive/exploration/step7-v8-runtime.md).

---

# Session Persistence

Session persistence is not a core editor concern.

The core exposes sufficient API for extensions to serialize and restore:

- open buffers;
- window layouts;
- view state.

Extensions are responsible for deciding what to persist, when, and how. This allows users to choose their own persistence semantics, from minimal (reopen last files) to maximal (full workspace restoration).

---

# User Interface

The interface should be:

- modern;
- responsive;
- GPU accelerated;
- highly configurable.

Extensions can contribute UI elements through the public view and contribution
APIs. High-level surfaces intended for replacement use the same editor-state
APIs whether their implementation is built in or supplied by an extension.

Examples of replaceable UI surfaces:

- status bars;
- panels;
- sidebars;
- popups;
- command palettes;
- completion popups;
- hover tooltips.

---

# Open Questions

## Embedded language

Candidates include:

- V8 / JavaScript / TypeScript
- JavaScriptCore
- WebAssembly with a standard SDK

Questions:

- startup time;
- memory usage;
- API design;
- packaging;
- type safety.

---

## Structured buffers

The current design intentionally keeps buffers text-specific and avoids both
general structured buffers and a common polymorphic resource model.

Future experience may reveal whether additional buffer types are necessary.

---

# Rejected Ideas

## Collaborative editing

Real-time collaborative editing is not a goal for Knot. The editor will not
adopt CRDTs or another shared-document model, and its buffer, position, history,
workspace, and extension APIs should not be designed around future
collaborative-editing requirements.

Screen sharing and external communication tools are sufficient for the
collaborative workflows Knot intends to support.

---

## Everything is a generic model

Too abstract.

The editor prefers concrete, surface-specific models. Shared abstraction should
come from demonstrated common semantics rather than requiring terminals,
trees, binary resources, and editable text to look alike.

---

## Plugin processes

Running extensions out-of-process adds complexity.

The current design prefers in-process isolation. A process boundary remains an
option where stronger isolation proves necessary.

---

## AI as a central editor concept

The current design avoids making AI a mandatory architectural component.

AI services may eventually become standardized capabilities, but this remains optional.

---

# Summary

The editor can currently be described as:

- programmable;
- text-centric;
- command-oriented;
- asynchronous;
- filesystem-based (URI-addressed);
- view-driven;
- annotation-based;
- capability-structured with replaceable UI surfaces.

The project aims to preserve the flexibility and inspectability of Emacs while adopting modern rendering, tooling, and extension mechanisms.
