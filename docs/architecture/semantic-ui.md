# Semantic UI and generated text

Part of the [architecture](../architecture.md). Design rationale is recorded in
[decisions](../decisions.md); deferred work lives in [roadmap](../roadmap.md).

## Boundary

Extension APIs carry native, surface-specific semantic data. gpui objects,
drawing callbacks, layout protocols, V8 values, and Rust objects do not cross
the public boundary. Native surfaces own layout and interaction; painting and
input never call JavaScript synchronously.

## Semantic extension UI

Editor contributions follow one foreground-owned path:

```text
extension replacement set
        |
typed host request
        |
revision, range, and lifecycle validation
        |
BufferModel source replacement
        |
stable anchors and model notification
```

The request envelope supplies source identity. Replacement, explicit disposal,
buffer closure, and extension teardown remove the complete source set.
Built-ins and extensions use the same application-owned contribution registry.
Editor contributions include anchored decorations, gutter markers, and
command-backed actions.

Views paint styled text by composing each line's runs from ordered highlight
layers of line-relative `(byte range, style)` spans; a later layer overrides an
earlier one's color and font and adds its underline. Syntax sources resolve
into nvim-treesitter's standard capture names by longest dotted prefix, and the
`Theme` global maps each capture to a style. A decoration or gutter marker
names a color key instead, so a query can never draw one: decorations form one
wavy-underline layer per key in the model's paint order, which puts `error`,
`warning`, and `info` above other keys with the most severe on top.

## Themes and color keys

Native surfaces and contributions paint with color keys rather than colors.
A color definition gives a key a description and a default per appearance,
either a color or another key; native keys are defined in one table beside the
code that paints them, because native surfaces paint before packages start and
when they fail, and extensions define more through `themes.defineColor`. A
registered theme sets values for any keys, including ones nobody has defined
yet, and styles capture names or their dotted prefixes.

`ApplicationThemes` owns lifecycle-owned registrations, where the latest
registration of a name wins, and extension color definitions. The
`theme.mode`, `theme.dark`, and `theme.light` settings select a theme, which
resolves into the `Theme` global: each defined key takes the theme's value,
else its default for the theme's appearance, following references. An
unregistered selection resolves the defaults alone, so the native defaults are
also the fallback when the theme package fails. Resolution happens once when
startup completes and again when the selection settings or, in `system` mode,
the system appearance change; registering or removing a theme later takes
effect at the next selection change rather than repainting mid-session.

## Syntax highlighting and languages

Highlight layers compose in a fixed order: syntax, then contribution
decorations, then selection painted as before. Semantic tokens from a future
language client (D067) slot in above syntax. Syntax highlights are derived and
never stored as anchored ranges: each view queries its visible lines, with some
overscan, once per [syntax generation](core-and-buffers.md#syntax-state), and
painting, caret, hit testing, and IME geometry share that query, so bold and
italic captures keep geometry consistent. An injected layer paints over its
host, clipped to its included ranges.

Grammars are compiled in and named by stable native grammar IDs. Language
definitions are contributed data: extensions and personal config register an
LSP language ID, the grammar it parses with, the file names, extensions, and
shebang interpreters it claims, and its highlight and injection queries, through
the same `languages` API the bundled `@knot/languages` package uses. An
association claims patterns for an ID without defining the language, so
personal config redirects detection without replacing a package's language.

```text
languages.register --> background query compilation --> LanguageRegistry
                                                            |
Open / Save As / restore / registration change --> detection
                                                            |
documents.setLanguage / Set Language Mode --> explicit ID   |
                                                            v
                                      BufferModel language --> SyntaxState
```

- `ApplicationLanguages` owns the registry and tracks every buffer with a
  language. Registrations and associations are lifecycle-owned and share one
  order: the latest registration defines an ID, and the latest registration or
  association claiming a file name, extension, or interpreter wins it, so
  removal reveals the previous one.
- Queries compile on the background executor at registration, so a bad query
  rejects with its location and a package's registrations compile in
  parallel; only the insertion runs on the foreground.
- Detection tries exact file names, then extensions, then the shebang
  interpreter. It runs when a document enters the collection and when
  persistence moves it to a new resource, never while typing, and never for a
  document whose language is explicit.
- A buffer keeps its ID even while nothing registers it and reports
  `plaintext` meanwhile, so personal config and restoration need no ordering
  against package startup. A registration change re-detects open documents and
  reattaches syntax wherever the resolved language changed; a language that
  injects reparses too, since its injections may resolve differently.
  Injections resolve by language ID, its lowercase, or a registered extension.
- Startup awaits every package entry before restoring windows, and the bundled
  package awaits its registrations in `start()`, so restored documents paint
  highlighted from their first frame.
- The native Set Language Mode command offers the registered languages, or Auto
  Detect, in the window's transient picker. Contributed text frames take a
  fixed language at registration.

Tree providers use the reverse path:

```text
extension registration/invalidation
        |
native TreeView generation
        |
reverse runtime command
        |
extension getChildren callback
        |
generation and lifecycle validation
        |
native cached presentation
```

Completion providers use the same asynchronous reverse-runtime direction but
remain completion-specific: the shell snapshots ordered registrations, the
view-owned controller tracks one-shot provider states and merge policy, and a
native surface consumes semantic snapshots. Provider objects and request
machinery never enter surfaces, buffers, or `core`.

## Completion ownership and aggregation

The shell-owned `CompletionProviderRegistry` assigns monotonic registration
identities and order to lifecycle-owned, shell-wide providers. An editor
completion invocation snapshots registry membership, buffer identity and
revision, cursor and prefix range, and a view-local generation before the shell
fans requests out to independent extension runtimes.

```text
CompletionProviderRegistry snapshot
                 |
                 v
EditorView -> CompletionController -> independent extension runtimes
   |                    ^                         |
   |                    +--- validated results ---+
   |                              |
   +----- native list surface <----+
```

Responses reach a controller only while the registration and lifecycle remain
live and weak editor identity, active generation, buffer handle, and public
revision still match. The controller merges partial results deterministically
and assigns stable semantic item identities. The native list surface presents
the current immutable snapshot and tracks selection by item identity, so
presentation updates never restart provider work. Acceptance
resolves the selected identity through the controller and performs one
revision-checked prefix replacement.

- Completion composition is feature-specific and independent of arrival timing.
  Filtering, ranking, and deduplication rules live in the
  [completion controller](../../src/app/completion.rs).
- Recoverable failure is local to one provider and request. Other candidates
  remain visible, and the provider participates again on the next invocation
  without re-registration.

Applicability, automatic triggers, streaming, active cancellation of superseded
requests, timeouts, backpressure, richer edits, and popup polish remain deferred.

## Extension trees

`TreeView` owns cached semantic items, expansion, selection, focus, scroll, and
per-parent loading/error generations. It renders and handles input from cached
foreground state only.
Each product window owns at most one instance of a contributed view. The
foreground semantic bridge retains weak references to live instances, accepts
view registration before windows open, and routes invalidations and responses
to each instance. Its generation rejects responses from a replaced request.
Clicking an item focuses its tree before emitting its command. The product
shell routes that event after the tree update through the captured window
command path.

Items are ordinary content or `group` headers. A group is neither activatable
nor disclosable: its children are fetched when it first appears and remain
visible, so a grouped list is one provider tree rather than a second model.
Items expose optional right-aligned status text and a badge dot in a named
color key. Actions are labeled text
commands; icon glyphs are not part of the boundary yet. A general,
extension-replaceable icon/status source (icon packs) is deferred (roadmap
D055).

Frame, group, and row actions are semantic command references with JSON
arguments, never drawing callbacks. The native view renders them as controls
and emits the same captured-window command event as activation, with the action
arguments instead of null. Action clicks stop propagation so they cannot also
activate the row. The same actions are reachable from the keyboard by entering
an action-focus mode on the current row; keyboard navigation indexes the
rendered rows directly so scrolling stays aligned when a transient loading or
error row is present. A root invalidation keeps stale rows visible while the
refetch is pending and reloads group children when the refreshed result arrives,
so selection and navigation stay usable during a refresh.

Contributed text frames are a separate content kind. Each instance owns a
private read-only `BufferModel` rendered by the ordinary `EditorView`; it is not
registered with the document collection and has no resource or persistence.
The extension replaces the whole text through a foreground request, and the
bridge keeps the last text so a window created later presents current content.
Wrapping the editor surface is provisional; the [roadmap](../roadmap.md) tracks a
dedicated read-only presentation (D054).

Semantic tree state retains accessibility information, but gpui 0.2.2 lacks
the required public platform bridge.

Provider disposal follows the shared
[host lifecycle](extension-host.md#disposal-cancellation-and-teardown). Native
unregistration cancels retained requests across live surfaces. Completion removal
also removes that provider's already-completed aggregate results; other providers
retain their state. Tree invalidation acknowledges native acceptance separately
from later fetch/paint. Invalid provider results fail only their request.

## Extension transient interactions

Pickers and text prompts are window-local overlays owned by the product shell,
not by the extension runtime. The host operation resolves the originating
window from an explicit target or the admitted command capture, installs the
overlay on the foreground, and awaits its settled outcome. A window owns at most
one extension interaction; the palette is a separate native surface and may be
open at the same time. Native confirmation, message, and directory prompts occupy
the same slot without a rendered overlay; the folder picker normalizes and stats
its selected path through providers before delivering a URI.

```text
extension host request (showPicker/showInput/...)
        |
        v
window resolution (explicit target or command capture)
        |
        v
shell-owned overlay + shared TextInput
        |
        v
one settled outcome (selection, acceptance, dismissal, cancellation)
```

`TextInput` is one shared single-line gpui `EntityInputHandler`: typing, IME
preedit, and clipboard flow through gpui, while the field owns caret, selection,
keyboard movement, and pointer selection. It backs both transient fields and
the command palette query, so text entry has one native path. Picker filtering
is local to the supplied item list; no provider, scheduler, or event bus is
added.

Settlement happens once on acceptance, dismissal, native-prompt answer, window
closure, extension unload, or caller cancellation. Window closure and unload
release the overlay and settle it as cancellation; no remembered layout or
extension state is touched. Focus returns to the previously focused surface
when the overlay closes and that surface is still live.

## Generated text

Generated output that benefits from ordinary selection, scrolling, and copying
uses a `BufferModel` rendered by `EditorView` rather than a second presentation
model. Contributed [text frames](#extension-trees) are the current generated
surface; their private buffers stay outside document identity and persistence.
Feature controllers keep semantic identities beside the buffer instead of
recovering them from displayed text, so `core` and `BufferModel` carry no
feature semantics. Every application document is a user document with
persistence state; interactive generated views remain D027.
