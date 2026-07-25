# Step 8 — Extension View API Plan

Status: 🚧 implementation in progress.

## Question

What extension-facing UI boundary remains responsive across the
isolate/foreground-thread boundary, keeps fundamental editor behavior native,
and provides useful customization without inventing a general-purpose Knot UI
framework?

## Decisions

### D1 — Do not build a general declarative UI tree

Knot will not expose gpui elements, renderer commands, or a serializable
QML/React-like widget tree to extensions.

The prototype will follow the model used successfully by Emacs/Neovim and
VS Code:

- buffer-backed views for text-oriented surfaces;
- semantic native contribution APIs for common editor and workbench surfaces;
- a possible WebView escape hatch for genuinely arbitrary UI.

WebView embedding is explicitly deferred. Step 8 neither implements nor spikes
it.

### D2 — Prototype one semantic workbench surface: a tree view

The current outline pane will become a native tree view populated by an
extension-owned asynchronous data provider.

The extension supplies semantic data: stable item identity, label, optional
description/icon token, collapsibility, and an optional command to invoke.
Native code owns layout, rendering, focus, keyboard navigation, selection,
expansion, scrolling, loading/error presentation, and future accessibility
mapping.

This is a surface-specific provider API, not the seed of a universal widget
model.

### D3 — JavaScript never participates synchronously in rendering or input

The foreground thread renders the latest cached provider result. Expanding or
refreshing a node queues an asynchronous request to the owning extension and
shows native loading state until a response arrives.

Each request carries a provider generation. A late response is ignored after a
newer request, provider disposal, or extension termination. A slow or failed
provider cannot block gpui input, painting, the foreground heartbeat, or other
extensions.

Provider callbacks remain serial within their owning extension, consistent
with the step-7 runtime.

### D4 — Keep editor contributions semantic and constrained

Step 8 exposes three native editor contribution types:

- range decorations;
- gutter markers;
- command-backed click actions on those contributions.

The prototype needs only a small set of theme-aware decoration and icon tokens
sufficient to exercise diagnostics, warnings, and a second overlapping source.
It will not expose arbitrary colors, gpui styles, drawing callbacks, inline
widgets, custom text layout, hover UI, popups, or embedded child views.

### D5 — Publish contributions as revisioned, source-owned replacement sets

An extension publishes a complete contribution set for one source and buffer,
against a specific buffer revision. The foreground validates every UTF-8 byte
range and rejects stale revisions before changing visible state.

Accepted ranges are converted to the core's stable anchors. Later text edits
stabilize them through the existing `AnchoredRangeStore`; extensions do not need
to republish merely because unrelated offsets moved.

Replacing a set removes its previous contributions atomically. Disposing the
set, closing the buffer, or terminating the owning extension removes it
entirely. Late updates from an obsolete extension lifecycle are rejected.

### D6 — Buffer data is shared; presentation state is per view

`BufferModel` will own:

- the authoritative `TextBuffer`;
- the shared `AnchoredRangeStore`;
- source/lifecycle ownership and renderer-neutral contribution metadata.

Presentation payloads stay in `app`; core anchored ranges contain only stable
geometry and acquire no feature, provider, renderer, or extension metadata.

Each `EditorView` continues to own its cursor, selection, scroll position,
collapsed-fold set, focus, and rendering choices. Provider-supplied folding
ranges, when added later, are shared buffer data; whether a range is collapsed
is per-view state.

Two `EditorView` entities over the same `BufferModel` will prove this ownership
split.

### D7 — Built-ins use the same native contribution registry

The fixture diagnostics currently stored directly in `EditorView` will move
through the same `BufferModel` contribution path used by extensions. Built-in
Rust code need not cross the JavaScript transport, but it must not receive a
privileged rendering or editor-contribution API.

### D8 — Accessibility is represented semantically, not completed here

Tree items carry enough semantics for a future accessibility mapping: tree and
tree-item roles are implied by the surface, while labels, expansion, selection,
focusability, and ordering are explicit native state.

gpui 0.2.2 does not expose an obvious public platform accessibility bridge.
Step 8 will record that framework gap and preserve the required semantics; a
production platform accessibility implementation is out of scope.

### D9 — Linux portability validation is deferred until after the prototype

The Windows portability checkpoint is complete. Its Unicode and startup
failures were corrected at Knot's platform and UTF-8/UTF-16 boundaries using
public gpui APIs; the full fixture sweep now passes on Windows as well as
macOS.

Linux validation is intentionally deferred until after the prototype phase and
before production development begins. Knot does not currently have practical
access to a Linux desktop, while the native boundary has evidence from macOS
and Windows and gpui/Zed have an established Linux path. This is a recorded
risk, not a claim that Knot's Linux behavior has been validated. It does not
block Step 8.

## Prototype public surface

Exact TypeScript naming may change during implementation. The intended shape
is:

```ts
interface TreeItem {
  readonly id: string;
  readonly label: string;
  readonly description?: string;
  readonly icon?: TreeIcon;
  readonly collapsibleState: "none" | "collapsed" | "expanded";
  readonly command?: string;
}

interface TreeDataProvider {
  getChildren(parentId: string | null): Promise<readonly TreeItem[]>;
}

interface TreeViewRegistration extends Disposable {
  invalidate(parentId?: string): void;
}

interface EditorContribution {
  readonly range: ByteRange;
  readonly decoration?: DecorationToken;
  readonly gutter?: GutterToken;
  readonly command?: string;
}

interface EditorContributionSet extends Disposable {
  replace(
    contributions: readonly EditorContribution[],
    options: { ifRevision: number },
  ): Promise<void>;
}
```

Each buffer's foreground registry is keyed directly by
`ContributionSource`: either built-in code or `ExtensionId` plus
`ExtensionLifecycleId`. The extension identity identifies the installed
extension, while the lifecycle identifies one particular runtime incarnation.
Each source has one complete, atomically replaceable contribution set per
buffer. Delayed work from an older lifecycle therefore targets a different
source and cannot replace or dispose the restarted runtime's set.
The JavaScript facade may represent that set as an `EditorContributionSet`
object, but repeated access for the same extension lifecycle and buffer refers
to the same logical set.

Transport messages use Knot-owned opaque handles; JavaScript does not see
gpui entities, Rust references, core anchored-range IDs, or runtime resource
IDs. Contribution-set operations need no separate native handle: the buffer
handle comes from the JavaScript buffer proxy, and the host derives the
extension source from the request envelope.

`invalidate` is a notification, not a synchronous fetch. The foreground may
coalesce repeated invalidations before requesting children.

Commands remain the action vocabulary. Step 8 reuses the step-7 command
registry rather than adding arbitrary callback objects to UI protocol types.
General command/keymap dispatch remains step 11.

## Architecture changes

The intended dependency flow is:

```text
extension TreeDataProvider / contribution set
                    |
             typed host protocol
                    |
             gpui foreground
              /           \
     native TreeView    BufferModel
                           |
                 AnchoredRangeStore + metadata
                           |
                     EditorView(s)
```

- `host::protocol` carries renderer-neutral tree and contribution data,
  identities, requests, responses, and recoverable errors.
- `host` owns JavaScript proxies, provider callbacks, pending requests, and
  extension-local disposal.
- `app` owns foreground registries, stale-generation checks, contribution
  validation, native tree rendering, and view presentation state.
- `core` remains unaware of gpui, JavaScript, provider identities, icons,
  commands, or visual styling.

`docs/architecture.md` must be updated when these ownership and runtime flows
land.

## Execution

### 0. Record the portability disposition

1. ✅ Correct and verify UTF-16/grapheme boundary handling for the Windows
   emoji fixture. Keep grapheme policy above `core`.
2. ✅ Verify visual left/right selection for BiDi text on macOS and Windows.
3. ⏭️ Defer the Linux fixture smoke test until after prototype validation and
   before production development. Keep roadmap step 3b as the explicit
   checkpoint.

### 1. Introduce shared contribution ownership

1. ✅ Move `AnchoredRangeStore` ownership into `BufferModel` beside `TextBuffer`.
   Stabilize it after every accepted local or extension edit, before notifying
   views.
2. ✅ Add an application-owned contribution registry keyed by source identity.
   Keep visual metadata outside `core` and associate it with core anchored-range
   IDs.
3. ✅ Implement atomic revision-checked replacement, explicit disposal, buffer
   cleanup, and extension-lifecycle cleanup.
4. ✅ Route the existing fixture diagnostics through this registry and remove
   `EditorView`'s private fixture-decoration model.
5. ✅ Render at least two overlapping sources with deterministic native
   precedence. Record precedence as a local presentation policy, not a core
   anchored-range rule.
6. ⏭️ *Defer:* Strengthen `AnchoredRangeStore` validation with property/model tests.
   Cover multiple seeds, Unicode boundaries, empty-buffer transitions,
   add/remove interleavings, the endpoint-stickiness combination exposed by
   `AnchoredRangeStore::add`, query equivalence, and internal endpoint-index
   consistency. Prefer a shrinking property-test harness so failures produce a
   minimal edit sequence. Cover other stickiness combinations if a custom
   constructor is introduced.

### 2. Expose editor contributions to JavaScript

1. ✅ Add Knot-owned protocol types and recoverable errors for contribution-set
   replacement and disposal.
2. ✅ Add the private bootstrap bindings and the small public
   `knot:editor` facade.
3. ✅ Recheck extension identity, lifecycle, buffer liveness, revision, ranges,
   and cancellation immediately before foreground mutation.
4. ✅ Route contribution actions through registered commands. A disposed or
   terminated owner cannot receive a late action.
5. ✅ Add a fixture extension that publishes diagnostics and gutter markers,
   updates them once, receives one action, and disposes them.
6. ✅ Resolve contribution metadata once per model notification and project
   byte ranges through one indexed line table. Avoid per-contribution scans
   from the start of the document and duplicate view refreshes.

### 3. Add the asynchronous native tree-provider surface

1. ✅ Define tree provider/registration identities and renderer-neutral item
   types in `host::protocol`.
2. ✅ Add extension-local registration, invalidation, child-request dispatch,
   pending-request cleanup, and callback error reporting.
3. ✅ Add a foreground `TreeView` entity that owns cached items, expansion,
   selection, focus, scroll, loading/error state, and provider generations.
4. ✅ Replace the hard-coded outline pane with the native tree view populated
   by a fixture extension.
5. ✅ Ignore stale responses after reinvalidation, disposal, or extension
   termination. Remove the view's provider state cleanly without affecting
   the shell or other extensions.
6. ✅ Preserve tree semantics independently of gpui and record the missing
   platform accessibility bridge.

### 4. Prove shared-buffer/per-view ownership

1. ✅ Create two `EditorView` entities backed by one `BufferModel`.
2. ✅ Verify that an edit and shared contributions appear in both views.
3. ✅ Give the views distinct cursors, selections, scroll positions, and
   rendering choices; changing one must not mutate the other.
4. ✅ Stabilize each view's cursor and selection endpoints through shared
   buffer edits without coupling the views' presentation state.
5. ⏭️ *Defer:* Exercise distinct collapsed-fold state when folding is exposed.
   The focused two-view model test verifies the ownership boundary across the
   view-local mutable state that exists today; adding folding state without
   folding behavior would not strengthen the prototype evidence.

### 5. Responsiveness, lifecycle, and decision evidence

1. ⬜ Make the fixture tree provider deliberately await before returning
   children. During the wait, verify foreground heartbeat, editor input,
   scrolling, and painting continue.
2. ⬜ Fail one tree callback and terminate one contributing extension. Verify
   native cached/loading state is cleared, its editor contributions disappear,
   and another extension remains usable.
3. ⬜ Add focused deterministic tests for registries, revision/range
   validation, stale generations, source replacement, and lifecycle cleanup.
   Avoid broad UI snapshot or end-to-end suites.
4. ⬜ Record findings and the final boundary decision in this document and
   `docs/roadmap.md`.
5. ⬜ Update `docs/architecture.md` with the resulting ownership and request
   flows.
6. ⬜ Run `cargo fmt` once at the end of Rust work, then run the focused tests
   and any existing suite affected by the changes.

## Acceptance criteria

- The extension-owned outline is useful and interactive without exposing a
  general widget/layout API.
- Tree rendering and input never synchronously enter JavaScript.
- A slow tree provider does not stall editor input, painting, or the foreground
  heartbeat.
- Late tree responses cannot resurrect disposed, refreshed, or terminated
  provider state.
- An extension can atomically publish, replace, interact with, and dispose
  anchored decorations and gutter markers.
- Contributions survive unrelated text edits through stable anchors and are
  removed with their source lifecycle.
- Built-in fixture diagnostics use the same native contribution registry as
  extensions.
- Two views share buffer text and contributions but retain independent cursor,
  selection, scroll, folding, focus, and rendering state.
- No gpui, Deno, V8, core anchored-range ID, or Rust object crosses the public
  JavaScript boundary.
- The Windows portability checkpoint is complete, and the deferred Linux
  checkpoint remains explicitly required before production development.

## Explicitly deferred

- A general Knot widget tree or declarative layout language.
- WebView embedding and arbitrary HTML/CSS/JavaScript UI.
- Arbitrary drawing, canvas/display-list APIs, shaders, or renderer access.
- Inline editor widgets and custom text layout.
- Text input controls inside extension-provided workbench surfaces.
- Replaceable completion, hover, command-palette, or diagnostic UI surfaces;
  these remain step 12 work.
- A general view-container, docking, tab, persistence, or layout API.
- Production accessibility integration while gpui lacks the necessary public
  bridge.
- Production tree virtualization, paging, backpressure, manifests, activation
  events, and contribution declarations.

## Logical commits during execution

1. Portability disposition and Step 8 plan update.
2. Shared anchored contribution ownership and built-in migration.
3. Extension editor-contribution protocol and fixture.
4. Asynchronous native tree provider and outline migration.
5. Two-view ownership proof, failure evidence, and final architecture/roadmap
   updates.

Split further if a platform correction or lifecycle change forms an
independent reviewable unit. Do not use conventional-commit prefixes.
