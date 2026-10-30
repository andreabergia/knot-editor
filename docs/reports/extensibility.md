# Extensibility and a JavaScript Vim mode

October 2026. Compares how far Knot's extension API reaches with Emacs,
Neovim, VS Code, and Zed, and tests the proposal that building a complete Vim
mode in JavaScript (D058) is a good way to bring Knot to Neovim-level
extensibility. The new and reordered items are in the
[roadmap](../roadmap.md#editor-extensibility). The current contract is
`src/host/js/knot.d.ts`; extension runtime behavior is owned by
[extension host](../architecture/extension-host.md) and
[commands](../architecture/commands.md).

## Summary

Knot is not unusual in how much of the editor is scripted. Its split, native
mechanics with workflows as bundled extensions on the public API, is the same
as VS Code's, and close to Neovim's C core with a growing Lua layer. Knot looks
emptier mainly because most workflows don't exist yet.

Editors really differ in how deep a script can reach. Emacs can redefine or
advise anything. Neovim can't redefine its C motions, but it can drive every
buffer, window, cursor, mark, and mode, hook every lifecycle event, and draw
virtual text and floating windows. VS Code and Knot expose a curated API, and
Zed exposes very little. The target is Neovim's depth, keeping Knot's rule
that bundled code gets no private bypass.

A Vim mode in JavaScript is a good forcing function for the editing half of
that API: views, selections, positions, modal input, undo grouping, layout
queries, recording, and syntax-tree queries. It barely touches the other half:
lifecycle hooks, presentation that extensions own, and interactive generated
buffers. A language client (D067) or a magit-style view (D027) should drive
those.

Recommendation: before designing D058, run a spike on keystroke ordering and
latency through the extension pool (D110). That one question decides how the
whole Vim extension is shaped. Build the primitives as generic items
(D109–D114) that a Helix or Kakoune mode could use too, and keep Vim's own
vocabulary inside the extension.

## How far scripts reach

| | Emacs | Neovim | VS Code | Zed | Knot |
| --- | --- | --- | --- | --- | --- |
| Scripted share | Nearly everything above a C core | C core, growing Lua layer | TS core; many built-ins are bundled extensions | Nearly everything native | Mechanics native; workflows bundled on the public API |
| Script reach | Total: any function can be redefined or advised | Deep: buffers, windows, cursors, extmarks, modes, autocmds, floats | Curated; no editor internals or DOM; webviews | Narrow: languages, themes, LSP adapters | Curated semantic API |
| Isolation | None | Lua in-process; remote plugins | One shared extension-host process | wasmtime per extension | Persistent V8 isolate per package, in-process |
| Configuration | Code | Code | JSON | JSON | Code (pre-init / post-init) |

## Knot's current surface

What an extension can do today, from `knot.d.ts`:

- **Buffers:** snapshot text at a revision, apply edit batches under
  `ifRevision`, and observe committed changes. Offsets are UTF-8 bytes.
- **Contributions:** anchored ranges with an `info` / `warning` / `error`
  decoration or gutter token and an optional command.
- **Commands:** register global commands, handle a command for one contributed
  view, and compose child commands under the captured target.
- **Keymaps:** bind gpui key sequences, globally or for one view kind. The
  latest write wins, and every extension binding beats native defaults.
- **Workbench:** list and observe documents, windows, and workspaces; open,
  present, and close; capture layouts and replace windows; register tree and
  text tool views; show pickers, inputs, confirmations, and messages.
- **Other:** languages, completion providers, extension storage, retained
  recovery captures, and writing the clipboard.

What is missing for editing-level extensibility:

- No handle on an editor view: an extension can't read or set the cursor or
  selections of a native editor. `selectedText` serves contributed views only.
- No way to reveal a position, scroll, or ask which lines are visible.
- No anchored positions apart from decorated contributions, so there is
  nothing to build marks on.
- No key context other than the focused view kind, and no counts or pending
  operator state in the keymap.
- No undo grouping: each `applyEdits` batch is its own step.
- No layout facts: visual lines, wrapped lines, and shaped-x vertical movement
  are native and invisible to scripts.
- No syntax-tree queries (D066).
- No clipboard read.
- Cursor and selection changes surface only through the debounced
  `viewStateChanged` window event. D080 adds a real observation API.

## What a Vim mode forces

| Vim needs | Primitive | Today | Item |
| --- | --- | --- | --- |
| Normal, visual, and operator-pending modes; `d2w`-style counts and sequences | Extension-set key contexts and ordered key delivery | View-kind scope only; async commands race later keys | D110 |
| Insert mode | Native typing with IME | Exists | — |
| Cursor and selection movement, visual and visual-block modes | Read and set selections per view, including several and rectangular | Not exposed | D109 |
| Cursor shape per mode | View cursor style | Not exposed | D109 |
| `h`/`l`, `w`/`e`/`b`, `f`/`t` | Text and byte/grapheme math on snapshots | Possible in JS | — |
| `j`/`k`, `gj`, `H`/`M`/`L`, `zz`/`zt` | Visual-line movement, visible range, reveal | Native only | D109, D112 |
| `iw`, `ip`, `i(` | Text scanning | Possible in JS | — |
| `if`, `ia`, `ic` (function, argument, class) | Syntax-tree node queries | Not exposed | D066 |
| `ciw…<Esc>` as one undo step | Undo groups spanning batches and typing | One step per batch | D111 |
| `.` repeat, `q` macros | Record commands with arguments plus typed text; replay through dispatch | Not possible | D113 |
| Registers | Extension state plus clipboard read and write | Write only | D109 |
| Marks, jumplist | Anchored positions without decoration | Only decorated contributions | D109 |
| `/` search highlights, visual feedback | Theme decoration tokens | Three severity tokens | D114 |
| `:s` | Regex over snapshots and an edit batch | Possible in JS; bulk edits need D092 | D092 |
| Ex command line | Transient input with completion | `showInput` without completion | D058 |
| Folds | Fold state and commands | None | D066 |
| Relative line numbers, mode indicator | Gutter lanes, status bar items | Not yet | D068, D077 |
| `<C-w>` window commands | Workbench navigation commands | Daily-driver plan | D072 |

Most of the editing half of Neovim's API is in that table.

## What a Vim mode does not force

Neovim-level extensibility also includes things Vim emulation barely uses:

- **Lifecycle hooks:** run before save, on open or close, or on language
  change, and wrap or replace a native command (D115). Today an extension
  observes metadata changes after they commit.
- **Presentation that extensions own:** virtual text, inlay hints, floating
  panels anchored to a position, and decoration tokens beyond severities
  (D114).
- **Interactive generated buffers:** magit- or oil-style text with semantic
  actions per range (D027).
- **Settings per buffer and language:** D070 already scopes values by language
  and view.

D067 exercises hooks, presentation, and decorations hard; D027 exercises
interactive buffers. Pairing D058 with one of them covers the whole API.

## Keystroke ordering and latency

Neovim handles each key synchronously. In Knot, a key bound to an extension
command enters dispatch, the extension's handler runs as an async V8 turn on
the worker pool, and the architecture forbids raw input from calling
JavaScript synchronously. Short foreground work already runs in the dispatch
turn to keep order with later keys, but the extension's own work does not.

That leaves three problems:

- **Ordering:** keys typed while a handler is running must reach it in order,
  and must not be handled natively in between.
- **Mode switches:** after `i`, the next keys must go to native insert even if
  the turn that entered insert mode hasn't finished. After `<Esc>`, they must
  stop going there.
- **Latency:** each normal-mode key costs a pool hop, a turn, and a foreground
  commit. That has to stay well under one frame.

Approaches:

1. **Native buffering:** while a mode-owning extension handles a key, native
   code queues later keys for that view and delivers them in order. JavaScript
   owns the whole Vim grammar. Native code only needs to know which views have
   a mode owner and when it's done.
2. **Native modal keymaps:** the extension sets a key context on the view
   (`normal`, `visual`, …) and keymaps resolve per context natively, including
   counts and multi-key sequences. JavaScript runs only complete commands such
   as `delete { motion: "word", count: 2 }`. This keeps input fully native, but
   pulls part of the operator grammar into the keymap.
3. **Both:** contexts and sequences resolve natively, and the context switch
   happens in the dispatch turn so it never races later keys.

The spike should measure key-to-commit latency for a trivial extension command
under the current pool, and try a context switch that races typed text. If
latency is low enough, the third approach looks simplest: contexts are a small
generic addition to keymaps (VS Code's `when` contexts, Zed's key contexts)
and avoid a second input queue.

## Guardrails

- **Generic primitives.** Key contexts, selection sets, undo groups, and
  recorded command streams, not registers or operators. A Helix or Kakoune
  mode should fit on the same primitives, as D055 warns against baking one
  feature's vocabulary into the boundary.
- **No private bypass.** If the Vim extension needs something, every extension
  gets it.
- **Commands over data.** Prefer exposing native movement as parameterized
  commands (move by visual line, page, screen line) over exporting shaping and
  layout data to scripts.

## Proposed order

The daily-driver plan comes first: it supplies D070 settings, D074 native
undo grouping, D078 visual lines, D080 cursor observation, D086 keymap
presets, and D068 and D077 surfaces. Then:

1. D110 spike and key contexts, because it decides the shape of everything
   after it.
2. D109 view, selection, and position API, designed with D080's observation;
   D092 before multi-selection edits.
3. D111 undo groups and D112 visual-line movement, in parallel.
4. D113 recording, once commands, contexts, and selections are stable.
5. D066 syntax-tree queries, folds, and text objects, and D114's theme
   decoration keys, which Vim's search and mode highlights need.
6. D058 Vim mode as a bundled or installed extension.
7. The rest of D114 (virtual text, floating panels) and D115 alongside the
   first language client (D067), which drives them harder than Vim does.
