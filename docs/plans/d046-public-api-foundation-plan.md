# D046 — Public API foundation

Status: ✅ checkpoint 1 approved; runtime checkpoints 2–4 not started.
Checkpoint 2 implementation is deferred until requested.

Source: [extension platform plan](extension-platform-and-projects-plan.md),
[D046 roadmap entry](../roadmap.md).
Contract: [documented declarations](../../src/host/js/knot.d.ts).
Authoring: [extension guide](../extension-api.md).

## Scope and delivery

Implement the documented contract for every existing `knot` operation. Update
repository consumers directly, without compatibility layers. Reuse current
command routing, cancellation, pool teardown and foreground ownership; follow
[architecture](../architecture.md), [commands](../architecture/commands.md) and
[extension host](../architecture/extension-host.md).

Execute one checkpoint per request and stop at its review gate. Keep progress
current, make logical commits, and run `cargo fmt` once after Rust work before
committing. Update architecture references and decisions as runtime changes land.

D047–D049 retain application queries/events, live targets, persistent document
identity, real buffer-removal wiring, resource operations, storage/project policy,
new tree-instance protocols and native contributions. Do not add restructuring,
scheduling quotas, rendering changes, source maps or a generic command schema.

## 1. Contract and audit — ✅ complete

- ✅ Draft and document the complete API in `knot.d.ts`, including context,
  errors, revision/offset rules, provider data and cleanup completion.
- ✅ Add the author guide, identity/lifetime summary and checked JS examples.
- ✅ Cross-check exports, returned methods, callbacks, native decoders and
  response conversions; allocate outstanding mismatches below.
- ✅ Add a reproducible compiler check with pinned tooling. It passes for the
  declarations, both installed examples, draft examples and 18 negative fixtures.
- ✅ Contract approved on 2026-10-14. Retain explicit contextual target
  arguments and a shared disposal result, including failure; no automatic retry.

Validation: `scripts/check-extension-api.sh` and `git diff --check` passed.

## 2. Captured context — ⬜

- Convey opaque command capture through product roots, nested commands,
  same-lifecycle inline calls and cross-lifecycle handlers.
- Resolve contextual buffer/selection requests against that capture; validate
  explicit contextual targets. Remove request-delivery focus lookup and unused
  global active-buffer bridge state.
- Remove top-level script invocation's focus fallback. Make clipboard requests
  application-scoped; keep selected text limited to supported captured views.
- Update affected startup, transport, command, listener and provider fixtures.
- Update command/extension-host references and validated rationale.

Validation:

- Two windows/buffers, focus changes while awaiting, palette capture, and nested
  same/cross-lifecycle inheritance; verify roots cannot leak context.
- Valid nontext contexts, unsupported selection, forged/stale/replaced/closed/
  transferred presentations, and distinct null/context/target/cancellation results.
- Activation and listener/provider calls without command context; explicit buffer
  work in asynchronous callbacks and a shared buffer surviving presentation closure.
- Existing explicit buffer-close and lifecycle tests; do not claim product buffer
  removal is wired before D047.

Review gate: inspect the JS command fixture, target validity and failure distinctions.

## 3. Failures and lifecycle acknowledgements — ⬜

- Align facade and native decoding with the declarations. Construct coded public
  errors from typed boundary failures; eliminate diagnostic-string inference.
- Implement acknowledged, idempotent disposal for command registrations, buffer
  subscriptions/contributions, trees and completion registrations; make tree
  invalidation awaitable.
- Preserve local inactivity and a shared cleanup result, including rejection.
  Converge registration rollback and authoritative lifecycle teardown on exact-once
  release; cancel pending provider work and reject unowned queued deliveries.
- Define deterministic mutation/invalidation/disposal ordering and avoid callback
  self-disposal deadlocks.
- Update affected subsystem references and decisions.

Validation:

- Malformed arguments/results, required revisions, conflicts, coordinate units,
  typed error conversion and exact native-number representation.
- Cancellation before/after commit, response-kind mismatch, late/duplicate responses.
- Repeated/concurrent disposal, cleanup failure, registration/unload races,
  invalidate/dispose races, queued/active callbacks and self-disposal.
- Foreground ownership assertions after cleanup, failed activation, unload and
  shutdown; retain command, buffer, keymap, provider and listener failure isolation.
- Atomic synchronous keybinding batches with no partial map on failure.

Review gate: verify contracts against declarations, no discarded cleanup/invalidation
promises, and no deadlock or double release.

## 4. Package admission and authoring baseline — ⬜

- Enforce required positive integer `apiVersion`, exactly `1`, before installed
  entry evaluation. Preserve recoverable package identity for duplicate/dependency
  diagnostics; report expected and declared versions.
- Update manifests, schema and every package fixture directly. Keep personal
  config manifest-free and retain its fatal-startup policy.
- Finalize guide, declarations and examples against runtime behavior; remove draft
  markers. Cover activation, captured commands, explicit-buffer callbacks,
  disposal/invalidation, coded errors and keybinding batching.
- Integrate the authoring checker into continuous verification.

Validation:

- Missing/malformed/unsupported versions, no evaluation of rejected packages,
  failed dependents, independent success and duplicate-ID reporting.
- Positive/negative authoring checks and an installed package through the real
  product host; run `cargo test --all-targets` and `scripts/check-extension-api.sh`.
- Record results and confirm the parent plan/roadmap retain D047–D049 follow-ups.

Review gate: review runtime imports/context/errors/cleanup/version admission against
its documented contract. Only after all four gates pass, mark D046 complete in the
parent plan and roadmap. Commit completion documentation separately; stop before D047.
Delete this temporary plan after completion and fix links that pointed to it.

## Outstanding audit gaps

Remove entries as implementation and regression coverage close them.

| Checkpoint | Open gap / affected code |
| --- | --- |
| 2 | `extension_host.rs` captures focus per request; `extension_buffers.rs`/`model.rs` retain global active-buffer state. Replace with invocation capture. |
| 2 | JS command frames expose only buffer; protocol/engine/command bridge need authorized target transport for root/inline/cross-lifecycle paths. View requests conflate missing/stale context with cancellation; clipboard is view-gated. |
| 2 | `PRIMARY_FIXTURE`/`SECONDARY_FIXTURE` and product/engine tests use context-free `activeBuffer()`. Acquire proxies in native-admitted setup commands. Listener/completion waits use supplied buffers; generic transport/tree waits use application clipboard requests with typed acknowledgements. |
| 3 | Numeric decoding coerces values; snapshot conversion accepts inherited/string indices; outgoing integers may lose precision. Required revision errors differ between edits/contributions; read-only buffers map to `UnsupportedOperation`. |
| 3 | JSON stringify permits custom conversion/omission missed by facade validation; tree `Array.from` accepts accidental array-like results. Align recursive JSON, iterable/item and recognized-field validation; retain ignored unknown fields. |
| 3 | Errors are string/name based; `invalidArguments` coerces its message. Most disposers/invalidation discard request promises, and repeated contribution disposal falsely succeeds during pending/failed cleanup. Add ownership/race assertions in engine and foreground tests. |
| 4 | `extension_package.rs` lacks `apiVersion`; adapt discovery/loading, manifest constructors and JSON fixtures in package/load/host/product tests, example manifests and `docs/schemas/knot.schema.json`. |

Completion requires all audit gaps closed, every public operation matched to its
reference, automated coverage of important context/lifecycle/command/provider/keymap
paths, passing authoring/runtime checks and all review gates accepted. Native-app or
browser automation is not required.
