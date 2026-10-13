# D046 — Production public API foundation

Status: 📝 design agreed; implementation checkpoints have not started.

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

### 1. Complete audit and freeze the contract — ⬜

- Produce the complete operation/identity/context matrix in the author guide,
  with each current mismatch assigned to subcheckpoint 2, 3, or 4, or explicitly
  linked to D047/D049. No unassigned prototype cleanup remains.
- Draft complete declarations, context/target shapes, error representation,
  disposal state transitions, and version diagnostics. Include the current
  provider item/contribution/keymap shapes and exact revision/offset units.
- Inventory all consumers, including personal-config documentation, embedded
  product fixtures, pool/engine tests, and examples. Define replacements for
  context-free `activeBuffer()` test fixtures so transport tests retain coverage
  using explicit buffers or captured commands.
- **Validation:** manually cross-check every declaration against facade exports,
  native argument decoders and response/callback converters; run the initial
  declaration/example type check. Record intentional not-yet-implemented contract
  changes clearly so the guide does not claim they already ship.
- **Review gate:** approve the complete contract and audit allocation before
  changing runtime behavior. Commit audit, draft declarations, guide, and checker
  as one reviewable artifact.

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
