# Step 10: Text as a Primary Surface Plan

This experiment decides whether generated, command-bearing content remains
useful as an ordinary text buffer, and whether file-like and generated buffers
can share one user-visible buffer lifecycle without adding structured-buffer
semantics to the core.

## Validated constraints

- ✅ Search results are generated text rendered by the normal `EditorView`.
- ✅ Search-specific state and target metadata stay in an application-owned
  controller. Neither `TextBuffer` nor `BufferModel` learns search semantics.
- ✅ The textual representation is output, not an identity format. Navigation
  uses metadata recorded while formatting and never parses displayed text.
- ✅ The first representation is one result per line. The semantic result model
  must also permit grouped formats such as one source heading followed by
  several line-and-preview entries.
- ✅ Read-only is enforced by `BufferModel`, not by an individual view.
- ✅ Search-result buffers remain open and can be revisited through the same
  buffer list as the source buffer.
- ✅ The existing `BufferRegistry` remains a weak transport-handle registry. A
  separate application-owned collection strongly owns user-visible open
  buffers.
- ✅ The prototype is native-only. A future extension may generate the same
  text and contributions through public APIs, but step 10 does not extend the
  JavaScript protocol.
- ✅ Refresh, incremental result updates, and view-state restoration are
  deferred. Each search creates an immutable result snapshot.

## Prototype boundary

```text
source BufferModel
        |
        v
native search + semantic matches
        |
        v
SearchResultsController
  - generated read-only BufferModel
  - emitted result ranges -> source targets
        |
        +--------> OpenBufferCollection
                         |
                    EditorView
```

`OpenBufferCollection` owns the models that appear in the buffer list and
minimal presentation metadata such as a local identity and title. It does not
own protocol handles, filesystem URIs, persistence, dirty state, or view state.
When a model must cross the extension boundary, `BufferRegistry` independently
assigns its existing opaque transport handle.

The controller holds semantic matches separately from their textual layout. A
formatter consumes matches and returns:

- the complete generated text;
- the byte range emitted for each actionable result; and
- the corresponding source buffer, source revision, and source byte range.

The initial formatter emits:

```text
src/app/model.rs:11: foo
src/app/model.rs:17: struct Bar {
```

Its input does not assume this layout. A later formatter can instead emit:

```text
src/app/model.rs
11: foo
17: struct Bar {
```

without changing search, target identity, or navigation.

## Execution

### 1. Add model-level access policy

- ✅ Add a small application-level access policy to `BufferModel`, initially
  editable or read-only.
- ✅ Keep `core::TextBuffer` unconditionally mutable; access policy belongs to
  its foreground application owner.
- ✅ Make every text mutation entry point reject read-only models, including
  local editor edits and extension edit batches.
- ✅ Keep non-text mutations such as selection anchors, contributions,
  snapshots, and closure available on read-only models.
- ✅ Construct generated buffers from their final text as read-only; do not add
  a privileged refresh path for this experiment.
- ✅ Add focused model tests proving that local and extension edit paths cannot
  bypass the policy.

### 2. Introduce user-visible open-buffer ownership

- ⬜ Add an application-owned `OpenBufferCollection` with monotonic local
  identities, display titles, strong `Entity<BufferModel>` ownership, and one
  selected entry for the prototype shell.
- ⬜ Add the source fixture as the initial file-like entry.
- ⬜ Replace the shell's static explorer labels with entries from the
  collection and label the pane `BUFFERS`.
- ⬜ Keep `BufferRegistry` unchanged in purpose: weak model lookup for opaque
  extension transport handles plus the current prototype command context.
- ⬜ Do not introduce URI, filesystem, dirty-state, save, persistence, MRU, or
  close-confirmation behavior.

### 3. Make the editor usable with arbitrary buffer text

- ⬜ Add a normal `EditorView` construction path from any `BufferModel`, using
  the existing default text projection.
- ⬜ Preserve the fixture-specific constructor only for fixture styling needed
  by earlier experiments.
- ⬜ When the selected buffer changes, reconstruct the secondary editor over
  the retained model and replace its action subscription.
- ⬜ Leave the primary editor fixed on the source model so result activation
  has an unambiguous navigation target.
- ⬜ Do not preserve cursor, selection, or scroll across secondary-view
  reconstruction; buffer retention, not view-history restoration, is under
  test.

### 4. Generate search-result buffers

- ⬜ Add a native `SearchResultsController` that searches an immutable snapshot
  of the source fixture and records the source revision.
- ⬜ Represent semantic matches independently of formatting, including source
  identity, byte range, line number, and preview text.
- ⬜ Add one formatter that emits a complete line per match and records the
  exact output byte range associated with each target.
- ⬜ Attach one built-in action contribution to each emitted result range.
- ⬜ Provide two small fixture search actions so the shell can retain and list
  more than one result buffer without building a search-input widget.
- ⬜ Give each generated entry a descriptive title such as `Search: "Node"`.
- ⬜ Keep every generated buffer and controller alive in the open-buffer
  collection for the duration of the prototype.

### 5. Navigate without parsing displayed text

- ⬜ Include the activated contribution's result-buffer range in the editor
  action event; do not introduce general command arguments ahead of step 11.
- ⬜ Resolve that range through the selected result controller to obtain the
  recorded source target.
- ⬜ Reject activation if the source revision no longer matches the search
  snapshot instead of navigating to a potentially incorrect byte range.
- ⬜ Add a narrow editor operation that selects, reveals, and focuses a source
  byte range.
- ⬜ Exercise activation by clicking the command-bearing result region. Keyboard
  command routing remains step 11 work.

### 6. Validate the text-surface checkpoint

- ⬜ Create at least two searches and switch repeatedly among their retained
  entries and the source entry through `BUFFERS`.
- ⬜ Confirm generated content supports ordinary inspection, selection,
  scrolling, and copying while rejecting edits through every available path.
- ⬜ Confirm result activation reaches the recorded source range after switching
  away from and back to the result buffer.
- ⬜ Confirm changing only the formatter would not affect semantic match or
  navigation identity.
- ⬜ Record any duplicated structured state required solely to keep the text
  actionable.
- ⬜ Run the focused test suite, then run `cargo fmt` once at the end of Rust
  work and run the full test suite.

### 7. Record the decision

- ⬜ Update `architecture.md` with open-buffer ownership, read-only enforcement,
  and the generated-text controller flow if the experiment validates them.
- ⬜ Record the result and liabilities in `decisions.md`.
- ⬜ Mark roadmap step 10 complete and summarize the checkpoint.
- ⬜ Update this plan with the executed and deliberately skipped work.

## Commit boundaries

1. Model access policy and generic editor construction.
2. Open-buffer collection and buffer-list switching.
3. Search generation, result actions, and source navigation.
4. Decision records and completed-plan updates.

## Out of scope

Filesystem search, URI-backed buffers, arbitrary search input, asynchronous or
incremental search, refreshing an existing result buffer, extension-generated
surfaces, general command arguments, focus-derived command context, buffer
persistence, dirty-state handling, per-pane history, restored view state, and a
second generated surface remain deferred.

## Decision checkpoint

Representing generated content as text is validated if:

- generated and file-like text share one buffer list and editor pipeline;
- text remains independently useful for inspection, selection, and copying;
- action identity survives presentation changes without parsing text;
- read-only policy stays above the core text representation;
- retaining and revisiting multiple generated buffers needs no
  search-specific behavior in generic buffer ownership; and
- the structured metadata needed for navigation remains a small semantic
  companion rather than a second authoritative presentation model.

Text becomes a liability if ordinary interaction requires reconstructing
identity from display text, if action metadata must duplicate most of the
visible document structure, or if the editor pipeline must special-case search
results.
