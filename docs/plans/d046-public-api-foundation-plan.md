# D046 — Production public API foundation

Status: 🔎 checkpoint 1 artifact implemented and validated; contract review pending.
Runtime checkpoints 2–4 have not started.

Source: checkpoint 1 of the
[extension platform and projects plan](extension-platform-and-projects-plan.md)
and [D046 in the roadmap](../roadmap.md). Design discussion: 2026-10-13.

## Outcome

An extension author can use every currently shipped `knot` operation against a
documented, typed contract. Commands retain their originating context across
focus changes and awaits. Activation and provider callbacks cannot accidentally
operate on whichever product window happens to be focused. Asynchronous cleanup
and invalidation have observable completion and failures. Unsupported package
API versions fail before extension code evaluates.

This is the foundation checkpoint, not the application-query, observation,
resource-operation, storage, or native-contribution implementation. Those remain
D047–D049 with explicit follow-up tasks in the parent plan and roadmap.

## Agreed choices

- Retain `editor`, `commands`, `workbench`, `keybinding`, and `removeKeybinding`.
  Introduce further namespaces alongside real operations in later checkpoints.
- Expose captured window/view/buffer command context. Context-dependent current
  operations use that capture or an explicitly supplied valid target. There is
  no implicit global-focus fallback outside a command.
- Buffer listeners and completion providers use their supplied buffer. Tree
  window/instance context arrives with D049; current trees cannot infer it from
  global focus.
- There is no backward-compatibility requirement, now or in later work. Update
  contracts and repository consumers together without aliases, shims, deprecated
  overloads, dual protocols, or migration layers for old extension APIs.
- Asynchronous disposal and tree invalidation return promises. Disposal is
  idempotent. Keep synchronous keybinding batching as an intentional exception.
- Host failures expose stable structured codes; command invocation still returns
  execution outcomes rather than semantic values.

## Existing boundaries and audit starting points

Read [architecture](../architecture.md), [decisions](../decisions.md),
[extension host](../architecture/extension-host.md), and
[commands](../architecture/commands.md). Keep foreground state in `app`, V8 in
`host::engine`, and V8/gpui-free transport in `host::protocol`. Reuse the existing
pool, promise resolver lifecycle, command tree, cancellation, and teardown.

The audit must cover every export, returned object method, callback payload,
argument decoder, and response conversion, including:

| Area | Current operations and contract risks |
| --- | --- |
| Buffers | `activeBuffer`, snapshot and coordinate conversion, revision-checked edits, change subscriptions, contributions; active lookup currently reads foreground focus at request delivery |
| Commands | registration, view registration, invocation, invalid arguments, handler context and cancellation; top-level invocation currently captures current focus |
| Keymaps | synchronous binding/removal, JSON arguments, scope, atomic turn batch, failure reporting and lifecycle ownership |
| Workbench | selected text and clipboard; selected text currently requires an admitted view handler and must not imply support for arbitrary native views |
| Trees | provider registration, invalidation, child results and disposal; current declared kinds remain until D049 |
| Completion | registration, callback context, items, request failure/cancellation and disposal |
| Lifecycle | pending registrations, callbacks, requests, explicit cleanup, failed activation, unload and shutdown |

Starting files: `src/host/js/{knot,bootstrap}.js`, `src/host/{protocol,engine,pool}.rs`,
`src/app/extension_{host,buffers,commands,semantics,package,load}.rs`, the model's
buffer/subscription registries, and installed extension examples. Audit decoder
behavior, not only the facade's checks: option defaults, numeric limits, JSON
conversion, unknown fields, and malformed callback results must agree with types.

Several registration/subscription disposers and tree invalidation currently use
`void request(...)`. Contribution disposal marks itself disposed before its host
request succeeds. Treat these as lifecycle contracts to resolve, not just missing
`await` keywords.

## Contract to establish at the first review gate

### Identity and context

Publish an identity/context matrix with ownership, lifetime, validity checks,
public representation, and whether each identity may be persisted:

- Canonical package identity is distinct from a runtime lifecycle. Personal config
  has its own lifecycle; durable storage ownership is implemented in D048.
- Buffer, window, view, registration, subscription, and invocation identities are
  opaque live identities. Do not expose gpui handles, entities, pointers, or V8
  objects. Do not claim persistence or cross-restart validity.
- Persistent document identity is a documented distinct concept, implemented in
  D047 and persisted later. A buffer handle and a URI are not substitutes for it.
- The command target is captured before palette/focus changes and conveyed to
  JavaScript with nullable view/buffer members where the surface has neither.
  Reuse the application's existing target authority; extend semantic transport
  only as needed. No general window registry/query is added in D046.
- Child commands inherit the parent's originating window and existing composition
  rules. Same-lifecycle inline and cross-lifecycle execution must receive the
  same authorized context. A focus change cannot select another buffer.
- Existing contextual convenience methods resolve through the invocation capture.
  Explicit contextual targets must be foreground-authorized and validated; their
  validity is scoped to the invocation, not a promise of reusable window access.
  Detached work must use explicit buffer objects or await D047's live-target API.
- Outside a command, contextual operations reject with a context-unavailable
  error. A valid captured context with no text buffer returns `null`; unavailable
  context, stale target, and cancellation are distinct failures.
- Activation may register commands/providers/keymaps. Buffer listeners and
  completion callbacks may operate on their explicit buffers. No tree callback
  or activation request discovers context from foreground focus.
- Top-level `commands.invoke` cannot silently capture active focus in D046.
  Remove that fallback; D047 introduces explicit targets and the separately
  documented admission-time active-window convenience. Audit config/examples for
  callers affected by this restriction.
- Clipboard writing is application-scoped and does not require or borrow focus.
  Selected text remains limited to the supported captured view kinds; a missing
  selection is distinct from missing/unsupported context.

Define the exact context field names and contextual target argument shapes at
gate 1, alongside declarations. Buffer methods use their explicit proxy and do
not retarget through context. Closing one presentation must not invalidate an
otherwise live shared buffer. Actual product document/buffer removal remains
D047 checkpoint 2; D046 tests existing explicit close and lifecycle boundaries
without claiming production closure wiring is complete.

### Arguments, errors, completion, and cancellation

- Document parameter types, required options, defaults, nullability, successful
  completion points, and failures for every operation. In particular, make the
  required revision contract for edits/contributions explicit; avoid silently
  relaxing conflict checks.
- Use `TypeError`/`RangeError` for author-side argument and coordinate mistakes.
  Host errors carry a stable `code` and useful `message`; names and messages are
  not the dispatch contract. Include context-unavailable, invalid target,
  disposed ownership, cancellation, and existing resource-specific failures.
- Construct public errors from typed failures at the V8 boundary. Do not infer
  codes from arbitrary thrown strings or native diagnostic messages. Specify
  how authors identify a host error in the declarations and examples.
- Commands return their existing structured execution outcomes. Provider failures
  and listener failures retain their feature-specific isolation and diagnostics.
- Accepted asynchronous operations settle exactly once at the documented host
  acknowledgement. Cancellation before a mutation prevents it; cancellation
  cannot imply rollback of a committed mutation. Retain foreground revalidation
  and rejection of late/duplicate responses.
- Use the current command cancellation signal and provider cancellation machinery;
  do not add a generic task graph or universal new cancellation framework.
  Document the supported signal interface accurately rather than declaring a
  browser `AbortSignal` that the runtime does not implement.
- Synchronous keybinding calls remain void and turn-batched. Invalid batches
  fail atomically through the turn/command/startup result, with no partial map.

### Disposal and callbacks

- Every host-backed disposer returns `Promise<void>`. Mark the object locally
  inactive immediately, and share one in-flight cleanup promise across repeated
  calls. Resolution means foreground ownership has been released. A second call
  after success resolves without another mutation.
- Define cleanup failure behavior explicitly: preserve the rejected completion
  for repeated calls, keep the local object inactive, and rely on authoritative
  lifecycle teardown to remove remaining ownership. Do not falsely acknowledge
  cleanup or reactivate callbacks after failure.
- Concurrent registration and unload must not create a late callable object.
  Pending registration cleanup and lifecycle teardown converge on exact-once
  foreground release.
- After disposal starts, no new listener/provider callback is admitted locally.
  Cancel pending provider requests through existing machinery; discard queued
  deliveries after ownership validation. A callback already running may finish
  or be cancelled according to its existing callback contract. Disposal does
  not wait for the currently executing callback to finish, avoiding self-disposal
  deadlocks. Document that acknowledgement is an ownership barrier, not JS unwind.
- `invalidate()` is awaitable and rejects on disposed/stale ownership. Its
  completion acknowledges native invalidation, not completion of the next fetch
  or paint. Pending invalidation/disposal ordering must be deterministic.
- Unload and shutdown remain Rust-owned cleanup. Do not depend on an extension
  awaiting disposers or receiving a final JavaScript callback.

### Package API version and authoring artifacts

Use a required positive integer `apiVersion` in installed-package manifests,
initially `1`, checked for exact equality against the shipped API contract.
Reject missing, malformed, or unsupported versions before entry evaluation and
report expected/declared versions through existing startup diagnostics. Update
all repository manifests/fixtures directly. Keep package `version` and dependency
`requires` separate; add no semver compatibility range negotiation.

Personal config uses the running application's contract directly and keeps its
existing fatal-startup error policy; do not add a config manifest. API-version
validation must not suppress duplicate identity or dependency-failure diagnostics.

Ship `src/host/js/knot.d.ts` declaring module `"knot"`, an author guide at
`docs/extension-api.md`, and checked authoring examples. TypeScript is authoring
tooling only. Add a small reproducible compiler check for declarations and JS
examples, with pinned tooling and no dependency on a globally installed `tsc`.
Runtime tests remain authoritative for V8 behavior; declarations cannot replace
argument/response/callback validation.

## Implementation checkpoints

Execute one subcheckpoint per implementation request and stop at its review
gate. These gates refine D046 without authorizing D047 work. Keep this plan
updated, mark completed work ✅ after validation/review, and use logical commits
without conventional titles. Run `cargo fmt` once at the end of Rust work before
committing. Architecture/decisions continue describing implemented behavior
until the corresponding contracts land.

### 1. Complete audit and freeze the contract — 🔎 ready for review

- ✅ Produce the complete operation/identity/context matrix in this plan’s audit appendix,
  with each current mismatch assigned to subcheckpoint 2, 3, or 4, or explicitly
  linked to D047/D049. No unassigned prototype cleanup remains.
- ✅ Draft complete declarations, context/target shapes, error representation,
  disposal state transitions, and version diagnostics. Include the current
  provider item/contribution/keymap shapes and exact revision/offset units.
- ✅ Inventory all consumers, including personal-config documentation, embedded
  product fixtures, pool/engine tests, and examples. Define replacements for
  context-free `activeBuffer()` test fixtures so transport tests retain coverage
  using explicit buffers or captured commands.
- ✅ **Validation:** manually cross-check every declaration against facade exports,
  native argument decoders and response/callback converters; run the initial
  declaration/example type check. Record intentional not-yet-implemented contract
  changes clearly so the guide does not claim they already ship.
- ⬜ **Review gate:** approve the complete contract and audit allocation before
  changing runtime behavior. Commit audit, draft declarations, guide, and checker
  as one reviewable artifact.

Checkpoint 1 delivery (2026-10-13):

- ✅ The internal audit appendix inventories all public operations, returned
  methods, callback payloads, identities, decoders/converters and consumers,
  with explicit checkpoint 2/3/4 or D047–D049 allocation. The
  [author guide](../extension-api.md) introduces usage; documented declarations
  are the API reference.
- ✅ [Draft declarations](../../src/host/js/knot.d.ts) and checked JavaScript
  samples cover the complete proposed version 1 surface. `CommandContext` fields
  are `target`, `buffer`, `arguments`, `signal`; target is a host-issued opaque
  `{ window, view }` object accepted only by contextual buffer/selection calls
  within its originating invocation tree. Buffer proxies retain hidden identity.
  D047 still owns top-level explicit-target invocation.
- ✅ Error contract retains resource codes and adds context-unavailable, invalid
  target, disposed ownership and read-only buffer codes. Recognized fields use
  strict types/safe integers; unknown API record fields remain ignored. Provider
  results require iterables; JSON arguments reject silent serialization changes.
- ✅ `scripts/check-extension-api.sh` passed with lockfile-pinned TypeScript
  5.7.3 (Node 25.9.0 / npm 11.12.1). Checks include declarations, both installed
  JS packages, two draft JS examples, positive fixtures and 18 expected-error
  fixtures. No global compiler or runtime transpilation is used.
- ✅ Manual cross-check: `knot.js` exports; every `bootstrap.js` proxy method and
  callback; protocol data/outcomes/errors; native operation/keybinding/numeric/
  range/edit/contribution decoders; provider/command JSON and response/snapshot/
  change conversion; foreground buffer/view/command/semantic routing and model
  revision/ownership rules; package decoder/schema and consumer fixture searches.
  Differences are documented as unimplemented draft contracts, not shipped claims.
- No Rust or host runtime implementation was changed; Rust tests and formatting
  are not needed
  for this documentation/declaration/tooling checkpoint. The installed greeting
  example now handles the already-nullable command buffer before editing.
- ⬜ Await approval of the contract and audit allocation before checkpoint 2.
  Architecture and decisions retain their current implemented behavior.

### 2. Captured context from product routing through JavaScript — ⬜

- Convey opaque captured command targets and buffer context through root, nested,
  inline, and cross-lifecycle invocation paths. Fix contextual buffer lookup and
  remove request-delivery focus lookup/global active-buffer bridge state where
  no supported caller needs it.
- Validate explicit contextual targets and contextual convenience calls against
  their originating invocation. Restrict selected-text access to supported views;
  make clipboard scope explicit. Remove context-free script invocation's focus
  fallback and update affected consumers.
- **Validation:** focused gpui product tests with two windows/buffers and a focus
  change while a command awaits; palette capture; nested same/cross-lifecycle
  inheritance; non-text contexts; stale/replaced/closed presentations; activation
  and provider/listener calls without context. Verify explicit buffer operations
  still work in asynchronous callbacks and no context leaks between roots.
- **Review gate:** inspect the JS command fixture and confirm target validity,
  null/error distinctions, and D047/D049 follow-up boundaries. Update extension
  host/command references and validated rationale in this commit.

### 3. Structured failures and acknowledged lifecycle operations — ⬜

- Align facade validation, native decoding, typed errors, and public error codes.
  Implement promise-returning, idempotent disposal and acknowledged invalidation
  for commands, subscriptions, contributions, trees, and completion registrations.
- Cover registration/disposal/unload races, locally inactive objects, pending
  callback cancellation, self-disposal, cleanup failure, and mutation ordering.
  Reuse exact-once pool teardown and foreground lifecycle ownership.
- **Validation:** V8 boundary and foreground bridge tests for malformed arguments,
  revision conflicts, error conversion, cancellation before/after commit,
  duplicate/late responses, repeated/concurrent disposal, invalidate/dispose
  races, self-disposal, failed activation, unload, and shutdown. Verify cleanup
  using foreground ownership assertions, not only JS promise resolution. Retain
  command, buffer, keymap, provider and listener failure-isolation regressions.
- **Review gate:** confirm errors and completion match declarations, no internal
  disposer/invalidation discards a request promise, and no new lifecycle deadlock
  or double cleanup exists. Update affected subsystem references and decisions.

### 4. Version diagnostics and complete author-facing baseline — ⬜

- Enforce package `apiVersion` before evaluation; integrate diagnostics with
  dependency loading/startup reporting. Adapt all manifests and package fixtures.
- Finalize guide/declarations and examples against the implemented facade. Remove
  draft markers and stale prototype explanations only in touched material.
  Include activation registration, captured commands, explicit buffer callbacks,
  awaitable disposal/invalidation, error-code handling, and keybinding batching.
- **Validation:** package discovery/load tests for missing/malformed/unsupported
  versions, no evaluation of rejected packages, failed dependents, independent
  package success and duplicate-ID reporting. Run declaration/positive/negative
  authoring checks and an installed-package fixture through the real product host.
  Run the full Rust suite for this cross-boundary foundation change, plus the
  authoring check; record commands and outcomes.
- **Review gate:** review shipped imports/context/errors/disposal/version behavior
  against the audit and examples. Confirm explicit roadmap and parent-plan tasks
  for later namespaces and targets. Mark D046 ✅ in both plans only after review;
  keep the parent initiative active and stop before D047. Commit completion docs
  separately from any later implementation.

## Exclusions and completion

No application snapshots/events, persistent document IDs, real closure-removal
wiring, general window/view queries, active-window convenience, resource opening,
SQLite, project policy, dynamic native UI contribution registry, or new tree
instance protocol lands in D046. No module/crate restructuring, scheduling quotas,
new renderer, source-map work, generic command schema, or backwards API support.

Completion requires every existing operation covered by the published contract,
all four gates reviewed, important context/lifecycle/command/provider/keymap paths
covered by automated tests, declarations/examples checked reproducibly, and no
unexplained audit gaps. Manual review may use a product build; no native-app or
browser automation is required. Planning does not implement these contracts or
mark the foundation complete.

## Checkpoint 1 review summary

After author feedback, the detailed audit lives in this implementation plan.
`docs/extension-api.md` is the short author guide, intended as official product
documentation once runtime validation passes. `src/host/js/knot.d.ts` is the
API reference with documentation next to its types; both are explicitly drafts
while implementation is pending. This reorganizes the review artifact without
changing the proposed runtime contract.

✅ Documentation revision validated on 2026-10-14: declaration signatures are
unchanged, `scripts/check-extension-api.sh` passed (including the 18 negative
fixtures), and `git diff --check` passed. No runtime implementation changed.

Only these contract decisions need review:

1. **Context:** commands retain their originating window/view/buffer across
   awaits and nested calls. Outside commands, contextual methods reject;
   buffer callbacks use their explicit buffer. Clipboard writing is app-scoped.
2. **Completion and failures:** cleanup and tree invalidation return promises;
   repeated disposal shares one result, including failure. Host failures have
   stable codes; commands still return execution outcomes.
3. **Admission:** installed packages require exactly `apiVersion: 1`, checked
   before evaluation. Personal config has no manifest.

The matrices below are implementation evidence and allocation, not a request
to review every row or a second public API reference. The existing gate remains
open; this documentation revision does not authorize checkpoint 2.

## Internal contract audit and implementation allocation

## Identity and authority

| Identity | Owner / lifetime | Public representation | Validation / persistence |
| --- | --- | --- | --- |
| Canonical package | Installed package discovery; independent of an activation | Manifest `name`, e.g. `@example/text-utils` | Canonical directory/name and duplicate checks; may persist as package identity, not runtime authority |
| Config lifecycle | Application startup; pre-init and post-init share one runtime | No manifest or public lifecycle ID | Separate from packages; fatal config failure tears down startup lifecycles; no durable storage until D048 |
| Runtime lifecycle | Rust pool and foreground ownership registries; one activation to teardown | Hidden | Every request checks owner/lifecycle; never persist |
| Window | Application; live product window | Opaque branded string `CommandTarget.window` | Capture authority and live window check; never persist or reconstruct |
| View/presentation | Foreground captured command surface; may be absent | Opaque branded string or `null` in `CommandTarget.view` | Captured presentation, tab, instance and registration checks; replacements/transfers invalidate capture; never persist |
| Command target | Foreground routing; originating invocation tree | Frozen opaque branded object `{ window, view }` | Must be host-issued and authorized for current frame/tree; neither object copies nor serialized IDs grant access; never persist |
| Buffer | Application buffer registry/model; independent of any one presentation | Cached opaque `Buffer` proxy; no public numeric handle | Resolve live buffer and lifecycle on each request; may retain for explicit asynchronous work within lifecycle, never persist |
| Registration | Foreground command/tree/completion owner; until dispose or teardown | `Disposable` or `TreeRegistration`; ID hidden | Owner/lifecycle and registration validity; never persist |
| Subscription | Foreground buffer subscription registry; until dispose, buffer close or teardown | `Disposable`; ID hidden | Buffer plus owner/lifecycle checked before delivery; never persist |
| Contribution set | Model; one set per buffer and extension lifecycle | `buffer.contributions`; ID hidden | Revision and ownership checked; disposed proxy remains inactive; never persist |
| Invocation | Foreground command tree; admission to completion/cancellation | Hidden ID; `CommandContext` exposes target, buffer, arguments, signal | Root/child admission and ancestry checks; never persist |
| Provider generation/revision | Native request session / buffer content | Safe integer callback fields | Request correlation, not reusable authority; never persist |
| Tree item | Provider semantics within its tree | String `id`, stable and unique within provider tree | Used for parent/selection/expansion; not window identity or a resource handle |
| Persistent document | D047 defines it; D048 persists it | No D046 representation | A buffer proxy or URI is not persistent document identity |

`CommandContext` has exactly `target`, `buffer`, `arguments`, and `signal`.
`target.window` is present for every product-admitted command. `target.view` is
nullable when the capture has no extension-addressable view; native editor or
terminal surfaces do not thereby gain public view operations. `buffer` is the
captured text buffer or `null` when there is none. The complete native capture
remains private: no gpui handles, entities, pointers, or V8 objects cross transport.

The root captures its target before palette focus changes. Awaiting work and
changing focus cannot change that capture. Child commands inherit its originating
window and buffer; same-lifecycle inline and cross-lifecycle handlers receive
equivalent authority. There is one unfinished child per parent; recursive handler
ancestry and a cross-lifecycle cycle back to an ancestor are unavailable. Await
children serially. Command results describe execution, never handler return values.

`editor.activeBuffer(target?)` and `workbench.selectedText(target?)` accept only
the current invocation's host-issued `CommandTarget` (or its authorized inherited
capture). Omission uses the current frame. They revalidate on foreground delivery.
An unrelated, forged, stale, transferred, closed or replaced target fails with
`InvalidTarget`. No invocation fails with `ContextUnavailable`; a cancelled
invocation fails with `Cancelled`. A valid capture with no text buffer returns
`null` from `activeBuffer`. A live explicit buffer remains usable after one of
its presentations closes, unless the buffer itself is removed/closed.

Activation and tree callbacks have no contextual focus access. Completion and
change callbacks use the buffer supplied in their payload. Detached work uses an
explicit buffer; retaining a target does not extend invocation validity.
Top-level `commands.invoke` rejects with `ContextUnavailable`; it never captures
active focus. D047 introduces explicit live targets and a separately documented
admission-time active-window convenience. D046 adds no general window query.

## Operation matrix

All asynchronous methods return promises. Unless stated otherwise, resolution
acknowledges the foreground operation, not a repaint or completion of subsequent
callbacks. Every host request can reject with `Cancelled` during cancellation or
teardown. Resource operations additionally check the calling lifecycle. Author
argument errors are `TypeError` or `RangeError`, as specified below.

| Operation / callback | Inputs, defaults and result | Completion and resource failures | Audit allocation |
| --- | --- | --- | --- |
| `editor.activeBuffer(target?)` | Optional authorized capture; `Buffer` or `null` | Validated captured lookup; context/target failures | 2: current facade takes no target and foreground refreshes global active buffer per request |
| `buffer.snapshot(range?)` | Omitted/`null` range means whole buffer; `TextSnapshot` | Immutable requested text and revision captured together; `BufferClosed`, `InvalidRange` | 3: strict numeric/range argument checks and typed errors |
| `snapshot.byteOffsetAtUtf16(offset)` | Relative UTF-16 code-unit offset to relative UTF-8 bytes | Synchronous; `RangeError` for noninteger, out of bounds or split surrogate | 3: current array lookup accepts string keys and inherited properties |
| `snapshot.utf16OffsetAtByte(offset)` | Relative UTF-8 bytes to relative UTF-16 units | Synchronous; `RangeError` for noninteger, out of bounds or split scalar | 3: explicit numeric validation; do not rebase to whole-buffer coordinates |
| `buffer.applyEdits(edits, { ifRevision })` | Required array and revision; `{ revision }` | Atomic revision-checked batch; `BufferClosed`, `BufferReadOnly`, `RevisionConflict`, `InvalidRange`, `InvalidEditBatch` | 3: required options/type/range validation; read-only currently maps to `UnsupportedOperation` |
| `buffer.onDidChange(listener)` | Function; `Disposable` | Foreground subscription exists before return; `BufferClosed` | 3: pending registration/unload and queued callback ownership |
| Change listener | `{ buffer, beforeRevision, revision, edits }`; void or promise | Precommit edit coordinates; FIFO roots; rejection diagnoses only this callback, later deliveries continue | 2: fixtures must use supplied buffer; 3: disposal delivery barrier |
| `buffer.contributions.replace(items, { ifRevision })` | Required array and revision; void | Atomic replacement of this lifecycle's set; `BufferClosed`, `RevisionConflict`, `InvalidRange`, `OwnershipDisposed` | 3: decoder parity and coded locally disposed failure |
| `buffer.contributions.dispose()` | No arguments; void | Removes this lifecycle's set; absent set is already released; `BufferClosed` possible | 3: current async method returns success on repeat even while first request is pending/failed |
| Subscription `dispose()` | No arguments; void | Removes subscription; stale ownership -> `OwnershipDisposed` | 3: current method returns void and discards request; missing subscription currently maps to `BufferClosed` |
| `commands.register(name, handler)` | String and function; `Disposable`; title is name | Foreground name registered; `CommandNameInUse` | 3: registration/teardown race; no title option or command schema added |
| `commands.registerForView(viewKind, name, handler)` | Currently `"outline"`, existing command name and function; `Disposable` | Bound to live instances; `ViewNotFound`, `CommandNotFound`, `CommandNameInUse` | 2: capture/context parity; 3: cleanup; D049: new native view kinds |
| Command handler | `CommandContext`; void or promise | Structured outcome after handler and children settle; throws become `handlerFailure`, invalid-arguments helper becomes `invalidArgument` | 2: add target and convey root/inline/cross-lifecycle capture; 3: cancellation/error parity |
| Command registration `dispose()` | No arguments; void | Foreground registration and bindings released; `CommandNotFound` for stale registration | 3: currently void/discarded request, suppress future local admission immediately |
| `commands.invoke(name, arguments?)` | String, JSON value default `null`; `CommandOutcome` | Child completion; normal routing failures are outcomes, transport/context errors reject | 2: remove top-level focus capture; 3: JSON conversion parity |
| `commands.invalidArguments(message)` | String; never returns | Synchronous throw recognized by command wrapper as `invalidArgument` | 3: currently `String(message)` coerces author input |
| `keybinding(key, command, options?)` | String key; name, `{ name, arguments? }` or `null`; view omitted/`null` means global | Void, atomic ordered turn batch; invalid batch fails turn/startup/command with no partial map | 3: decoder/JSON parity; retain turn batching |
| `removeKeybinding(key, options?)` | Same key/scope | Void, removes only caller's slot, may reveal lower owner | 3: maintain batch failure and lifecycle ownership tests |
| `workbench.selectedText(target?)` | Authorized capture; string or `null` | Only admitted supported outline view handler; no selected item -> `null`; unsupported surface -> `UnsupportedOperation`; context/target failures distinct | 2: current authorization maps all failures to cancellation; no arbitrary editor/terminal selection API |
| `workbench.writeClipboardText(text)` | String; void | Clipboard write accepted by application; no selection/window requirement | 2: current request requires admitted view handler; 3: lifecycle/cancellation validation |
| `workbench.registerTreeDataProvider(viewKind, provider)` | Currently `"outline"`, object with `getChildren`; `TreeRegistration` | Declared kind ownership registered even without windows; `TreeViewNotFound`, `TreeProviderInUse` | 3: cleanup/callback admission; D049: instance/window protocol |
| Tree `getChildren(parentId)` | String or `null` for root; iterable items or promise | Isolated per-request failure/diagnostic; native generation rejects stale result | 3: strict iterable/item conversion, callback disposal; no context/signal field invented |
| Tree registration `invalidate(parentId?)` | Omitted/`null` means root; string means children of that item; void | Native invalidation acknowledged, not next fetch/paint; `TreeProviderNotFound`, `OwnershipDisposed` | 3: currently void, disposed silently returns; deterministic invalidation/disposal ordering |
| Tree registration `dispose()` | No arguments; void | Releases provider on all surfaces; `TreeProviderNotFound` | 3: currently void/discarded request; pending callback cancellation |
| `editor.registerCompletionProvider(label, provider)` | String and object with `provideCompletions`; `Disposable` | Registered for native aggregation; label is diagnostic, not identity | 3: registration/cleanup/callback admission |
| Completion `provideCompletions(context)` | `{ buffer, revision, cursorByteOffset, prefix, generation }`; iterable items or promise | Isolated request failure; native revision/generation and cancellation checks | 2: use explicit buffer in fixtures; 3: result conversion/disposal |
| Completion registration `dispose()` | No arguments; void | Foreground registry released; `CompletionProviderNotFound` | 3: currently void/discarded request; cancel pending requests |

Returned snapshots, proxy objects, command contexts/outcomes and listener payloads
are read-only. Freeze nested public records/arrays; providers need not freeze the
objects they return. Providers expose no public cancellation signal today.
Native receiver cancellation can discard queued work or release active callback
promises and pending host resolvers; it does not promise JavaScript stack unwind.

## Data, decoding and errors

Offsets, revisions and generations are finite, nonnegative safe integers
(`0..Number.MAX_SAFE_INTEGER`), with no string/boolean/null coercion. Outgoing
native numbers must be checked for exact representation too. An ill-typed value
raises `TypeError`; a numeric limit or coordinate boundary error raises
`RangeError`. Unknown fields in API option/data records are ignored; required
fields and every recognized optional field are validated. Optional nullable
fields below accept omission or `null`; required fields do not. Native decoding
must enforce the same contract even when facade checks are bypassed.

Ranges are ordered half-open absolute UTF-8 bytes with scalar-aligned endpoints
within the addressed buffer. Snapshot conversion methods are relative to the
returned text, so add `snapshot.range.startByteOffset` when constructing an
absolute edit from a ranged snapshot. They reject split surrogate/scalar offsets.
Edits are `{ range, text }`, expressed against one precommit revision, ordered
by start and nonoverlapping. Repeated empty insertions at the same position are
allowed in input order. Empty/no-op batches retain revision and emit no change.
A real batch commits once, advances revision once, and reports original edit
coordinates. Required `ifRevision` never defaults to current revision. A conflict
rejects the entire operation; taking a new snapshot is an author decision.

Contributions are `{ range, decoration?, gutter?, command? }`. Decoration and
gutter are `"info" | "warning" | "error"`; command is a string name.
All three optional fields are nullable. Replacement replaces the entire set
owned by this lifecycle/buffer, including with an empty array; overlap is allowed.
The model rejects nonempty contribution lists on an empty buffer. Empty ranges
on a nonempty buffer follow the existing anchored-range semantics. Edits stabilize
anchors; disposing the set does not dispose the buffer.

Tree items are `{ id, label, collapsibleState, description?, icon?, command? }`.
Required strings are not implicitly coerced. `collapsibleState` is `"none"`,
`"collapsed"` or `"expanded"`; nullable icon is `"file"`, `"folder"` or
`"symbol"`; description and command are nullable strings. Authors supply stable,
unique IDs within the provider tree; D046 does not add a general tree identity
registry. Commands on tree/contribution items are names, not structured command
arguments. Completion items contain only required string `label` and `insertText`.
Provider results must be actual iterables (not strings, null, noniterable objects,
or accidental array-like values); facade and Rust conversion validate each item.
Malformed results and thrown/rejected callbacks remain feature-specific failures.

JSON command arguments consist of null, booleans, finite numbers, strings,
arrays and plain string-keyed records recursively. Reject cycles, undefined,
functions, symbols, bigint, sparse arrays, nonplain objects and custom `toJSON`
conversion rather than silently dropping/coercing them through JSON serialization.
The same rules apply to keybinding command arguments. Omission defaults to null.
Keys use gpui key-sequence syntax (for example `"cmd-g"`); the native keymap
registry validates keys and view scopes. `{ view }` may select `"editor"`,
`"terminal"` or `"outline"`. This is keymap scope, not authorization to register
native view handlers. Null bindings mask lower-priority owners; removal reveals
them. Personal config outranks extensions, which outrank native defaults;
view-specific and later slots resolve ties within an owner priority. Keybinding
validation reports through the atomic turn batch even for malformed JS arguments;
these synchronous void calls do not produce a separately awaited host error.

Host failures are `Error` instances with `name === "KnotHostError"`, useful
`message`, and a stable `code` from `HostErrorCode`. Narrow using the name and
code properties (see the checked example); there is no exported error constructor.
Codes come from typed Rust failures at the V8 boundary, including decoder failures,
never from matching diagnostic strings. The code union retains existing resource
failures and adds `ContextUnavailable`, `InvalidTarget`, `OwnershipDisposed`, and
`BufferReadOnly`. Names/messages/stacks are diagnostics, not the dispatch contract.
`TypeError` and `RangeError` identify author input mistakes; a foreground failure
such as `RevisionConflict` or a range invalidated by host state is a coded host
error. Locally disposed objects reject with `OwnershipDisposed`.

Command outcomes are `{ kind }` for `completed`, `unavailable`, `invalidTarget`,
or `cancelled`, and `{ kind, message }` for `invalidArgument` or `handlerFailure`.
A handler result is ignored. `commands.invalidArguments(string)` throws the
feature-specific error interpreted by its handler wrapper. `signal` implements
only `aborted`, `reason`, `throwIfAborted()`, and add/remove `"abort"` listeners
with `{ type: "abort", target: signal }`; no browser options, `onabort`, static
constructors or other `AbortSignal` facilities are promised.

Accepted requests settle once at the host acknowledgement. Native lifecycle,
request, invocation, revision and generation validation rejects late/duplicate
responses. Cancellation before a mutation prevents it; once committed, later
cancellation cannot imply rollback. Cancellation can therefore reject a waiting
request whose side effect already committed. Async callers must not assume a
rejection proves no mutation occurred. Buffer listeners/providers isolate their
failures; command failures retain the structured execution outcome.

## Cleanup state machine

Every host-backed disposer returns `Promise<void>`. First call changes local
state from active to inactive immediately, removes the local handler/listener/
provider, and issues one cleanup request. Concurrent/repeated calls return the
same in-flight promise. Successful acknowledgement changes pending to released;
subsequent calls resolve without a second mutation. Failure changes pending to
failed; the rejected completion is retained and repeated calls reject with it.
The object stays inactive and Rust lifecycle teardown releases any ownership left
behind. Cleanup failure does not retry automatically or reactivate callbacks.

After disposal begins, no new local callback is admitted. Queued delivery also
checks authoritative ownership; pending native provider work is cancelled using
the existing receiver/request machinery. A callback already executing may finish
or be cancelled under its existing contract. Disposal acknowledges ownership
release and does not await JS callback unwind, so `await registration.dispose()`
inside its own callback cannot deadlock. Pending registration followed by teardown
must reject rather than publish a late callable object; registration rollback and
lifecycle finalization converge on exact-once foreground release.

Buffer mutations admitted before contribution disposal retain request order;
new `replace()` calls after disposal starts reject with `OwnershipDisposed`.
Cleanup acknowledgement follows preceding accepted ownership mutations.

`invalidate()` validates active ownership locally and in the foreground. Calls
issued before disposal are delivered in request order: their acknowledgements
precede cleanup acknowledgement. Calls after disposal begins reject with
`OwnershipDisposed`; stale foreground registration rejects with
`TreeProviderNotFound`. Refetch callbacks run independently; invalidation neither
waits for fetch/paint nor grants context to a tree provider. Unload, failed
activation and shutdown are Rust-owned cleanup, independent of extension awaits
or final JavaScript callbacks.

## Package admission and authoring audit

The proposed installed manifest includes required positive integer `apiVersion: 1`.
It must equal the running contract exactly before entry evaluates. Package
`version` remains semver for the package itself; `requires` remains canonical
package IDs defining load order. There are no API ranges or compatibility shims.

Diagnostics retain directory and canonical name when recoverable and distinguish
`missing apiVersion (expected 1; declared missing)`, malformed declaration
(`expected positive integer 1; declared <JSON value>`), and unsupported version
(`expected 1; declared <integer>`). Keep duplicate-ID diagnostics even if one
manifest has an invalid API version; failed dependents report their failed
requirement while independent packages still load. Existing startup reporting
surfaces these failures. Schema, decoder and fixture updates belong to checkpoint
4; adding `apiVersion` to current repository manifests now would fail their
strict decoder's unknown-field check.

Personal config has no manifest and uses the running contract directly. Its
`pre-init.js` and `post-init.js` phases may register commands/providers/keymaps,
but cannot borrow focus or invoke a top-level command. Clipboard writing is
application-scoped. Config failure remains fatal to startup.

Run `scripts/check-extension-api.sh` from a clean checkout with Node/npm available.
It installs the lockfile-pinned TypeScript compiler with `npm ci`, then checks the
ambient declaration, both installed JavaScript examples, draft JavaScript
examples, and positive/negative type fixtures. No global `tsc` is used. It emits
no runtime code and includes no browser type library. V8/foreground tests remain
the runtime authority. The draft samples in
[`scripts/extension-api/examples`](../../scripts/extension-api/examples/) illustrate
activation, captured target use, explicit buffer callbacks, acknowledged cleanup,
error-code handling and synchronous keymap batching; they are checked, not loaded
as current product fixtures.

## Consumer inventory and implementation handoff

The audit covers the public facade and all returned methods/callback payloads,
`engine::parse_host_operation`, `parse_keybinding_mutation`, scalar/range/edit/
contribution decoders, command JSON conversion, provider result conversion,
`host_response_to_v8`, snapshot/change conversion, pool admission/response/
teardown, and foreground buffer/command/view/semantic bridges and model registries.
The matrices allocate all observed contract mismatches; these cross-cutting
allocations complete the handoff:

| Current consumer / boundary | Required follow-up |
| --- | --- |
| `src/host/js/{knot,bootstrap}.js` | 2: target parameters/frame context and explicit contextual authorization; 3: strict validation, typed error construction, shared disposal promises, iterable/item checks and cleanup races; 4: remove draft discrepancy markers |
| `src/host/protocol.rs`, `engine.rs`, `pool.rs` | 2: semantic capture for root/inline/cross-lifecycle dispatch; 3: typed decoder failures rather than strings, precise native-number response checks, response-kind validation, exact-once resolver cancellation and teardown |
| `src/app/extension_host.rs` | 2: remove per-request focus capture and global buffer refresh; separate clipboard from view access; fix view failure distinctions; replace activation-time fixture buffer acquisition |
| `src/app/extension_{buffers,commands,semantics}.rs`, `model.rs` | 2: resolve capture without global active registry; 3: ownership/disposal failure mapping, release assertions and callback races; preserve revision and lifecycle validation |
| Embedded `PRIMARY_FIXTURE` / `SECONDARY_FIXTURE` and `src/app/product.rs` tests | 2: register an explicit fixture setup command that receives/stores its captured buffer, invoke through native product routing before using it; retain two-window and nested routing coverage |
| `engine.rs` host-request/pool lifecycle tests | 2: use captured commands for context assertions; for generic yield/parallel-response/cancellation/shutdown tests, use application-scoped `writeClipboardText` with typed acknowledgements; for buffer tests, obtain proxy in a native-admitted setup command then use explicit `buffer.snapshot()`; listener/completion await fixtures use their supplied buffer |
| Engine tree-provider await fixture | 2: use application-scoped clipboard acknowledgement as transport wait, never active buffer; preserve receiver-drop cancellation and worker-release evidence |
| Existing disposer/invalidation tests in `engine.rs`, product tests | 3: await acknowledgements; assert foreground registry removal, concurrent/shared promise and failed cleanup state; preserve self-disposal, unload and failed activation cases |
| `examples/extensions/@example/{text-utils,word-tools}` | Existing command-based composition retained; text-utils handles nullable buffer; 4: add manifest API version and complete author examples |
| `extension_package.rs`, `extension_load.rs`, `extension_host.rs`, `product.rs` manifest constructors / JSON fixtures | 4: add version field everywhere, test diagnostic recovery, duplicate IDs, dependent failures and no entry evaluation |
| `docs/schemas/knot.schema.json` | 4: required positive integer API version, explain shipped exact-equality validation |
| `docs/architecture/{commands,extension-host}.md`, `docs/decisions.md` | 2–3: replace current active-focus/void cleanup behavior only when implementation lands; personal-config prose must describe context-free startup restrictions |
| `docs/README.md`, extension example README, `docs/development.md`, `.github/workflows/ci.yml`, `justfile` | 1: link draft guide and local checker; 4: publish finalized baseline and add authoring checker to continuous verification |
| Parent plan and roadmap D047–D049 | D047: live queries/observation, persistent document concept and real removal; D048: durable owner storage; D049: tree instance/window context and native contributions; no D046 implementation of these |

Current numeric decoding uses V8 `number_value` coercion; revision failures vary
between `InvalidEditBatch` and `UnsupportedOperation`. Unknown record fields are
already generally ignored; preserve that policy, while all recognized fields
become strict. JSON stringify currently permits custom conversion and omissions
that facade recursion misses. Tree conversion currently uses `Array.from`, which
accepts accidental array-like objects; serde requires `collapsibleState` and
valid enum values but ignores extra fields. Completion facade strips extra fields
and validates the two strings. Snapshot conversion can expose native integers as
imprecise doubles. These are checkpoint 3 work, not solved by declarations.
Internal inline completion/provider envelopes stay private and typed, without
exporting transport IDs. Buffer proxy caching remains lifecycle-local; D046 tests
explicit buffer close and teardown, while real product removal wiring stays D047.
