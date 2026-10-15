# D046 — Public API foundation

Status: ✅ complete. Checkpoint 1 approved on 2026-10-14; checkpoints 2–4
approved by the user on 2026-10-15 after passing manual smoke checks.

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

## 2. Captured context — ✅ complete

- ✅ Convey opaque command capture through product roots, nested commands,
  same-lifecycle inline calls and cross-lifecycle handlers.
- ✅ Resolve contextual buffer/selection requests against that capture; validate
  explicit contextual targets. Remove request-delivery focus lookup and unused
  global active-buffer bridge state.
- ✅ Remove top-level script invocation's focus fallback. Make clipboard requests
  application-scoped; keep selected text limited to supported captured views.
- ✅ Update affected startup, transport, command, listener and provider fixtures.
- ✅ Update command/extension-host references and validated rationale.

Validation:

- Two windows/buffers, focus changes while awaiting, palette capture, and nested
  same/cross-lifecycle inheritance; verify roots cannot leak context.
- Valid nontext contexts, unsupported selection, forged/stale/replaced/closed/
  transferred presentations, and distinct null/context/target/cancellation results.
- Activation and listener/provider calls without command context; explicit buffer
  work in asynchronous callbacks and a shared buffer surviving presentation closure.
- Existing explicit buffer-close and lifecycle tests; do not claim product buffer
  removal is wired before D047.

✅ Automated evidence: `cargo test --all-targets` passed (537 library tests and
5 example tests); `scripts/check-extension-api.sh` and `git diff --check` passed. Product regressions
cover palette origin, two buffers, focus while awaiting, inline/cross-lifecycle
inheritance, forged and previous-root targets, replaced/closed/transferred
presentations, outline and terminal nontext captures, null selection, unsupported
selection, activation/listener/provider context absence, clipboard access and
explicit shared-buffer callbacks after presentation closure. Command bridge tests
cover lifecycle authority and cancellation; existing explicit buffer-close and
lifecycle suites remain green. Product buffer-removal wiring remains D047.

Unused activation-time fixture proxies were removed; buffer-dependent runtime
fixtures now acquire proxies in native-admitted setup commands. Context failures
carry codes; full coded-error conversion and decoder alignment remain checkpoint 3.

✅ Review gate approved on 2026-10-15: inspect the JS command fixture, target validity and failure distinctions.

## 3. Failures and lifecycle acknowledgements — ✅ complete

- ✅ Align facade and native decoding with the declarations. Construct coded public
  errors from typed boundary failures; eliminate diagnostic-string inference.
- ✅ Implement acknowledged, idempotent disposal for command registrations, buffer
  subscriptions/contributions, trees and completion registrations; make tree
  invalidation awaitable.
- ✅ Preserve local inactivity and a shared cleanup result, including rejection.
  Converge registration rollback and authoritative lifecycle teardown on exact-once
  release; cancel pending provider work and reject unowned queued deliveries.
- ✅ Define deterministic mutation/invalidation/disposal ordering and avoid callback
  self-disposal deadlocks.
- ✅ Update affected subsystem references and decisions.

Validation:

- Malformed arguments/results, required revisions, conflicts, coordinate units,
  typed error conversion and exact native-number representation.
- Cancellation before/after commit, response-kind mismatch, late/duplicate responses.
- Repeated/concurrent disposal, cleanup failure, registration/unload races,
  invalidate/dispose races, queued/active callbacks and self-disposal.
- Foreground ownership assertions after cleanup, failed activation, unload and
  shutdown; retain command, buffer, keymap, provider and listener failure isolation.
- Atomic synchronous keybinding batches with no partial map on failure.

✅ Automated evidence: `cargo test --all-targets` passed (550 library tests and
5 example tests); `scripts/check-extension-api.sh` and `git diff --check` passed.
Coverage includes typed host codes, argument/revision/offset validation, exact
native integers, deep plain JSON, rejected conversion hooks/array-like providers,
ignored unknown item fields, all disposer kinds with concurrent/repeated success
and failure, invalidate/replace versus cleanup ordering, queued deliveries,
listener/tree/completion self-disposal, registration/unload races and foreground
ownership after disposal, failed activation, unload and host release. Existing
cancellation, response identity/kind, duplicate/late response, keymap atomicity and
command/provider/listener isolation suites remain green.

Completion unregistration removes pending work and completed candidates from live
editor aggregates. Host release uses authoritative foreground cleanup while
models/views remain live. Native JSON/result decoding invokes no author conversion
hooks; cancellation listeners validate their interface, prevent replay and isolate
callback failures. Package version admission and final authoring integration remain
checkpoint 4.

✅ Review gate approved on 2026-10-15: verify contracts against declarations, no discarded cleanup/invalidation
promises, and no deadlock or double release.

## 4. Package admission and authoring baseline — ✅ complete

- ✅ Enforce required positive integer `apiVersion`, exactly `1`, before installed
  entry evaluation. Preserve recoverable package identity for duplicate/dependency
  diagnostics; report expected and declared versions.
- ✅ Update manifests, schema and every package fixture directly. Keep personal
  config manifest-free and retain its fatal-startup policy.
- ✅ Finalize guide, declarations and examples against runtime behavior; remove draft
  markers. Cover activation, captured commands, explicit-buffer callbacks,
  disposal/invalidation, coded errors and keybinding batching.
- ✅ Integrate the authoring checker into continuous verification.

Validation:

- Missing/malformed/unsupported versions, no evaluation of rejected packages,
  failed dependents, independent success and duplicate-ID reporting.
- Positive/negative authoring checks and an installed package through the real
  product host; run `cargo test --all-targets` and `scripts/check-extension-api.sh`.
- Record results and confirm the parent plan/roadmap retain D047–D049 follow-ups.

✅ Validation: `cargo test --all-targets` passed (559 library tests and 5 example
tests), `cargo clippy --locked --all-targets -- -D warnings`,
`scripts/check-extension-api.sh` and `git diff --check` passed. Tests prove missing,
malformed and unsupported versions never evaluate entry code, preserve duplicate
IDs and dependency causes, and do not skip independent packages. Checked authoring
examples run as an installed version-1 package through commands, buffer callbacks,
trees/completion, keybinding batching and cleanup. Personal-config startup suites
remain green. CI runs the pinned authoring checker alongside Rust verification.

The final import/context audit additionally moved private native dispatch hooks
from author globals into engine-owned handles, with visibility/replacement tests.
Requests attempted during teardown reject with `Cancelled` before native admission.
Mechanical Clippy fixes preserve existing view/relocation behavior. The parent plan
and roadmap retain D047–D049 application queries/events, live targets, real buffer
removal, resource operations, storage and new contribution protocols.

✅ Review gate approved on 2026-10-15: review runtime imports/context/errors/cleanup/version admission against
its documented contract. Only after all four gates pass, mark D046 complete in the
parent plan and roadmap. Commit completion documentation separately; stop before D047.
Delete this temporary plan after completion and fix links that pointed to it.

## Manual validation — ✅ passed

The user completed the product smoke checks on 2026-10-15: installed examples
appear in the startup report, `example.greet` targets the originating buffer in
two windows, unsupported `apiVersion: 2` rejects the package and its dependent
(with version restored to `1`), and personal config/keybindings still work.
Installation required the packages under `extensions/@example/`; copying into a
nonexistent `extensions` destination had initially omitted that scope directory.

## Audit result

✅ All implementation audit gaps are closed, all four review gates are accepted,
and manual smoke checks passed. D046 is complete in the parent plan and roadmap.
D047 is not started.

Completion requires all audit gaps closed, every public operation matched to its
reference, automated coverage of important context/lifecycle/command/provider/keymap
paths, passing authoring/runtime checks and all review gates accepted. Native-app or
browser automation is not required.
