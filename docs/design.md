# Knot - A Modern Programmable Editor

## Introduction

This project aims to build Knot, a modern programmable editor inspired by the philosophy of Emacs while embracing contemporary expectations regarding user interface, performance, asynchronous execution, and language tooling.

The goal is not to recreate Emacs, nor to build another IDE. Instead, the objective is to create a programmable editing environment where:

- the editor itself is programmable;
- commands are first-class objects;
- all functionality is implemented in the extension language;
- the user interface is customizable;
- text remains central;
- modern tooling such as Tree-sitter and LSP integrate naturally.

The editor should feel lightweight, fast, inspectable, and extensible.

---

# Philosophy

## Everything is programmable

The editor is fundamentally an embedded runtime.

Configuration, extensions, commands, keybindings, UI customizations, and workflows are all written in the embedded language.

There is no strong distinction between:

- user configuration;
- packages;
- built-in functionality.

Packages are simply collections of code.

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

A `FileSystemProvider` interface defines the operations: open, read, write, watch, stat. This allows remote and virtual filesystems to participate naturally without changing the core workspace model.

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

## The UI is fully customizable

The user interface is not a privileged layer. UI elements are scriptable and replaceable by extensions.

The built-in UI components are implemented using the same APIs available to extensions. There is no internal path that extensions cannot reach.

This commitment is strict: if built-in components use a separate internal path, that path becomes a ceiling on what extensions can achieve.

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

Buffers represent editor state.

Initially the editor supports three buffer types.

### TextBuffer

Represents editable text.

Examples:

- source files;
- markdown;
- logs;
- directory listings;
- git status;
- search results.

### BinaryBuffer

Represents arbitrary binary resources.

Examples:

- images;
- PDFs;
- binary files.

### TerminalBuffer

Represents a terminal session attached to a subprocess.

---

## Views

Views render buffers.

A single buffer may have multiple views.

Examples:

- two editors displaying the same file;
- source and preview views;
- different zoom levels;
- minimaps.

Views contain presentation state:

- scroll position;
- cursor state;
- selections;
- rendering options.

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

The editor discourages specialized UI components that bypass the buffer/view model. Views remain the primary UI abstraction. However, widgets are used in some places where there is not a logical buffer to render, for example the autocompletion popup is a special widget triggered by a text view.

---

# Commands

Commands are first-class objects.

Everything the user can do is a command.

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

The editor progressively enriches documents.

```text
Text
    ↓
Tree-sitter
    ↓
LSP
    ↓
Additional providers
```

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

---

# Extension System

The editor contains a single embedded runtime.

All extensions execute inside this runtime.

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

UI elements are scriptable and replaceable by extensions. The built-in implementations use the same APIs available to all extensions.

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

The current design intentionally avoids introducing general structured buffers.

Future experience may reveal whether additional buffer types are necessary.

---

# Rejected Ideas

## Everything is a generic model

Too abstract.

The editor currently prefers a small number of concrete buffer types.

---

## Plugin processes

Running extensions out-of-process adds complexity.

The current design assumes a single embedded runtime.

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
