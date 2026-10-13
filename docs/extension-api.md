# Extension API

**D046 gate 1 draft, not the shipped runtime contract.** This guide and
[`knot.d.ts`](../src/host/js/knot.d.ts) define the proposed API version 1.
Checkpoint 2 implements context, checkpoint 3 implements validation/errors and
acknowledged cleanup, and checkpoint 4 implements version admission and finalizes
these artifacts. The audit below identifies every intentional runtime mismatch.
Do not use draft-only features in an installed package yet.

Import `editor`, `commands`, `workbench`, `keybinding`, and `removeKeybinding`
from `"knot"`. These are the complete public exports. There are no public Knot globals,
private bootstrap imports, browser APIs, or arbitrary native filesystem imports.
Relative static imports stay within the captured package/config source graph;
dynamic imports reject. TypeScript is optional authoring tooling; Knot evaluates
built JavaScript only.

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

## Packages, config and authoring

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
[`scripts/extension-api/examples`](../scripts/extension-api/examples/) illustrate
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
