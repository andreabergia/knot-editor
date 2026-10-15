# D053 — Handler-defined operations, errors, and commands

Status: ✅ complete and approved on 2026-10-15. All four checkpoints and review
gates are accepted. Local validation passed; remote Linux/Windows results remain
unverified as a recorded validation limitation.
Execute after D047 is approved and archived, before D048 adds more host APIs.
This is a separate refactoring, not another D047 acceptance requirement.

Source: the protocol/command design discussion of 2026-10-15; scheduled by the
[roadmap](../../roadmap.md) and the
[extension platform plan](../../plans/extension-platform-and-projects-plan.md).
Current constraints: [architecture](../../architecture.md), [decisions](../../decisions.md),
[extension host](../../architecture/extension-host.md), and
[commands](../../architecture/commands.md). D047's implementation and acceptance
record live in its [completed plan](d047-application-state-and-operations-plan.md).

## Outcome

Adding a native host operation or command means defining an annotated handler
function with its argument, success, and error types. The function signature is
the source of truth. Shared payload/error structs remain ordinary reusable Rust
types. Adding a handler does not require parallel request/result/kind enum entries,
a handwritten registration list, or a central per-operation codec match.

Preserve D046/D047 behavior and their regression coverage while replacing the
registration and transport plumbing. Keep proposed rationale here until validated;
update architecture references and decisions when implementation lands.

## Agreed design

- `#[host_operation(...)]` declares a native capability callable from JS, with
  typed arguments and semantic results. Do not introduce namespace/domain enums
  or group handlers into protocol families as a replacement for the current enums.
  Definitions live alongside their implementation; share types when useful.
- `#[command(...)]` declares a native command handler and its catalog metadata,
  typed arguments, and errors. Both attributes share adapter/registration machinery
  while retaining their different invocation semantics.
- Registration is automatic. An annotation contributes its descriptor without a
  handwritten central list. Assemble and validate registries deterministically at
  startup. Reject duplicate operation names and conflicting native command entries;
  allow the same command name at distinct routing participants/view kinds.
- Async handlers are supported from the first implementation checkpoint. Immediate
  handlers use the same async contract without awaiting. An owned context handle
  provides short foreground access closures; borrowed editor state cannot survive
  an await. Retain owned captures and revalidate targets/cancellation before commit.
- A handler returns `Result<Success, Error>`. A derive such as `HostError` supplies
  explicit stable public codes, diagnostic messages, and typed structured details.
  Errors are local or shared/composed as appropriate. Shared cancellation, target,
  and ownership failures do not require a central error enum containing every case.
  Rust variant renames must not silently rename public error codes.
- Keep a common transport envelope for operation identity and extension, lifecycle,
  request, invocation, and cancellation metadata. Type erasure is confined to the
  transport adapters; generated adapters pair the handler's argument, success, and
  error types. An unrelated response cannot be passed through a typed responder.
- V8 values/codecs stay in `host`; gpui models and mutation stay in `app`. Worker
  transport carries owned Knot data, never V8 handles or foreground borrows. Macro
  expansion must respect that dependency direction.
- Argument and result conversion preserves recognized-field validation, numeric
  precision, opaque-target authority, and TypeError/RangeError versus host errors.
  Do not replace direct boundary decoding with author-controlled JSON conversion
  hooks or infer semantic errors from status/message strings.
- Native commands retain focus-based participant precedence, explicit decline,
  captured targets, keymap/menu/palette routing, structured command outcomes, and
  awaited composition. Adapt typed handler failures to those outcomes. Commands
  continue reporting execution status; host operations return semantic values.
- JS `commands.register(...)` remains dynamic, with its existing ownership,
  unregistration and unload semantics, feeding the same application catalog.
  Automatic static native registration must coexist with those registrations.

Representative production signatures:

```rust
#[host_operation("openDocument")]
async fn open_document(context: HostContext, arguments: OpenArguments)
    -> Result<DocumentPresentation, OpenError>;

#[command("file.open", title = "Open…", id = OPEN_COMMAND, participant = Workbench)]
async fn open(context: CommandContext, (): ())
    -> Result<CommandOutcome, CommandOutcome>;
```

An optional synchronous `claim` predicate represents participant decline before
argument decoding and async execution. Decline continues focus routing; it does
not become an error or completion outcome. Repeated attributes can share a typed
implementation. Native command statuses reuse `CommandOutcome`, while host
operations return semantic values and explicit-code failures.

## Implementation choices to resolve in checkpoint 1

Choose the smallest automatic registration/linking mechanism that works in product
and test binaries on supported platforms. Finalize macro-crate placement, owned
context/future signatures, codec traits, and error composition syntax with a real
async handler. Do not reopen automatic registration or async support as optional.
Decide how descriptors drive generated/checkable TS error-code declarations so
removing the Rust enum does not create another manually mirrored error registry.
Do not expand this into a general schema language or arbitrary RPC framework.

## Checkpoints and review gates

Implement one checkpoint per request, keep this plan updated with ✅ and evidence,
and stop at each gate. Split logical commits without conventional commit titles.
Run `cargo fmt` once at the end of Rust work before committing.

### 1. Typed async handler declarations and automatic registration ✅

- ✅ Introduce the minimal macros, typed adapters, common envelope, and automatic
  registry assembly. Keep request/result/error pairing local to the definition.
- ✅ Route an immediate production operation and an operation that awaits native work
  end to end through the new machinery. Include a reusable/composed error type.
- ✅ Keep any temporary migration adapter explicit and bounded to unmigrated handlers.
- **Validation:** compile-fail cases for invalid handler/error declarations and
  mismatched typed replies; deterministic discovery and duplicate diagnostics;
  actual argument/result/error conversion, async suspension and foreground commits;
  product/test binary registration and supported-platform build checks.
- **Review gate:** inspect representative handlers, error declarations, context
  ownership, generated adapters and automatic registration. Approve the authoring
  shape before migrating the rest of the boundary.

Checkpoint 1 implementation evidence and choices:

- ✅ `knot-handler-macros` is a workspace proc-macro companion. `host_operation`
  emits a typed handler implementation and an `inventory` contribution. There is
  no handwritten list of annotated operations. Assembly sorts names, rejects
  duplicate names, and validates application context types at startup.
- ✅ `writeClipboardText` and `openDocument` use declarations end to end. Their
  operation variants and clipboard result/kind entries are removed. Shared
  document-presentation results still serve Create/Present. The common existing
  request envelope retains identity and invocation/cancellation metadata.
- ✅ `HostContext` owns a weak host, async application handle and admitted request
  authority metadata.
  Its short foreground closures check lifecycle/cancellation and return owned
  results. Open awaits a native completion, retaining destination/caller authority,
  and checks the exact resulting presentation again before replying.
- ✅ Host-owned argument readers retain direct recognized-field V8 decoding.
  Success/error encoding checks numeric precision. Generated typed responders
  enforce result/error pairing; runtime resolver checks retain operation identity.
- ✅ `HostError` uses explicit `code`/`message` attributes, named structured details,
  and a single-field `transparent` composition form. Descriptor error codes plus
  derived codes for the initially unmigrated error enum checked the closed
  TypeScript union. Checkpoint 2 removes that enum and the bounded Open failure
  adapter; native operation failures now return their local/shared types directly.
- ✅ Coverage includes compile-fail declarations/replies, sorted discovery and
  duplicate/context diagnostics, structured errors through V8, precision and
  operation pairing, actual async suspension, immediate foreground mutation and
  disposed-context rejection. A separate integration binary checks discovery
  from the ordinary product library without `cfg(test)`.
- ✅ Local macOS validation: `cargo test --workspace --all-targets` (616 library
  tests, 2 macro tests, the product-library linking integration test, and 5 example
  tests), `cargo test --doc` (4 compile-fail cases and a positive authoring example),
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `scripts/check-extension-api.sh`, `scripts/build-macos-app.sh`,
  `git diff --check`, and 53 local Markdown links in changed documents.
  `cargo fmt --all` ran once after Rust edits and before the final checks.
- ✅ CI now runs workspace tests/lints and compile-fail examples on Linux, plus
  macro tests, linked test/product-library discovery, compile-fail examples and
  product builds on macOS/Windows.
- ⏳ Remote Linux/Windows build/linking results remain unverified; only the
  installed `aarch64-apple-darwin` target has been executed locally.
- ✅ Review gate: user approved representative handler/error syntax, context
  ownership, adapters and automatic linking on 2026-10-15. Remote build evidence
  remains pending and is carried into checkpoint 2 validation.

### 2. Migrate the host capability boundary ✅

- ✅ Migrate every existing JS-to-native capability, including buffer/query/mutation,
  observation/disposal, clipboard/context, commands, providers, and keymap batches.
  Preserve synchronous public keybinding batching despite the internal async model.
- ✅ Remove the per-operation `HostOperation`, `HostResponseValue`, `HostOperationKind`
  mirrors and their handwritten decode/encode/dispatch tables once unused.
- ✅ Replace central `HostRequestError` membership with handler-local/shared errors
  and the common envelope. Keep current public codes, names, messages/details
  semantics and closed declaration coverage synchronized through descriptors.
- **Validation:** existing D046/D047 boundary and product-host regressions; invalid
  fields/coercion, precision, exact targets, missing context, racing I/O, cancellation
  and queued responses, self-disposal, failed activation, unload and host release.
- **Review gate:** verify every current capability uses its declaration, no parallel
  operation/error registry remains, and representative JS consumers behave the same.

Checkpoint 2 implementation evidence:

- ✅ All 33 existing native capabilities are discovered from annotations. The
  separate product-library integration binary derives expected request names from
  the checked JS facade and compares the entire discovered registry, including
  inline completion and the native-turn keybinding batch. No handwritten operation
  registration list or per-operation transport sum/codec/foreground dispatch table
  remains.
- ✅ Buffer and semantic-provider owners expose typed methods; their lower-level
  revision, cancellation, ownership and cleanup tests exercise those methods.
  Snapshots keep the external UTF-16 representation. Provider identities remain
  owned metadata for invalidation/disposal barriers.
- ✅ Resource operations return shared/local typed failures directly. Command
  composition awaits typed channels retaining caller ownership; lifecycle cleanup,
  inline frames and window-handled children still use the existing command tree.
  Dynamic JS registrations continue using the same application catalog.
- ✅ Native keybinding batches use the same async contract while public calls stay
  synchronous/void. Batch admission remains after a V8 turn, atomic application and
  response ordering are preserved, and typed diagnostic messages retain native
  failure reasons without parsing status/message strings.
- ✅ The public error-code union is checked from handler descriptors and common
  runtime transport errors. No central `HostRequestError` membership remains.
  Argument conversion still uses the existing direct field readers. Native output
  precision failures retain `UnsupportedOperation`; bad JS numeric arguments retain
  `RangeError`. Structured error details receive the same precision checks.
- ✅ Local macOS validation: `cargo test --workspace --all-targets` (615 library
  tests, 2 macro tests, complete facade/product-library registration coverage,
  and 5 example tests); `cargo test --doc` (4 compile-fail cases plus the positive
  authoring example); `cargo clippy --workspace --all-targets -- -D warnings`;
  `scripts/check-extension-api.sh`; `scripts/build-macos-app.sh`;
  `git diff --check`; and 56 local Markdown links in changed documents.
  `cargo fmt --all` ran once after Rust edits and before final checks. The removed
  routing-table unit test is superseded by the complete facade/registry check;
  durable model, lifecycle and command coverage remains exercised.
- ✅ Existing D046/D047 product-host regressions pass through declarations:
  direct field validation/coercion, precision, captured/exact targets, missing
  context, racing I/O, cancellation and queued replies, self-disposal, failed
  activation, unload and host release. Searches confirm no obsolete
  operation/result/kind/query/error mirrors remain in Rust source or tests.
- ⏳ Remote Linux/Windows checks remain pending; no remote run exists for these
  local changes.
- ✅ Review gate: user approved complete capability coverage, removed parallel
  registries, and representative JS consumer behavior on 2026-10-15.

### 3. Migrate native command declarations ✅

- ✅ Automatically discover annotated native command handlers and catalog metadata.
  Migrate native commands across editor views, workbench and application routing;
  retain explicit participant decline and reuse shared handlers/types where useful.
- ✅ Remove replaced command-name/metadata/registration boilerplate. Handler internals
  may still match semantic values; do not mechanically ban useful Rust enums/matches.
- ✅ Keep extension registrations, view-specific overrides, name ownership, and
  lifecycle cleanup in the current catalog and dispatcher.
- **Validation:** view/workbench/application precedence, decline/unavailable,
  typed argument failure and outcome adaptation, captured/palette/delayed targets,
  menus/keymaps, native async work, inline/cross-lifecycle composition, cancellation,
  extension registration/disposal/unload, and product/diagnostic startup commands.
- **Review gate:** review representative native commands and exercise palette,
  keymap/menu, async Open/Save, and extension command composition in the product.

Checkpoint 3 implementation evidence and choices:

- ✅ 55 native participant declarations provide the original 53 public catalog
  names/titles. Shared editor motion/editing implementations use repeated
  attributes; terminal Copy/Paste share those definitions at their own participant.
  Workbench/application handlers own their metadata beside implementation.
  Find remains advertised and explicitly declines, preserving current behavior.
- ✅ The common `AsyncHandler` contract pairs owned context, typed arguments,
  success and errors. Host transport constraints remain in its adapter; command
  adapters decode Knot-owned arguments and convert typed results/failures to
  structured command outcomes. Existing native routines reuse `CommandOutcome`.
  Generated constants serve menu/keymap references; the separate native catalog
  list and per-participant name-dispatch boilerplate are removed.
- ✅ Synchronous claim predicates preserve explicit decline and participant
  selection for inline extension composition. Owned command contexts queue short
  foreground closures. The adapter drains them in the dispatch turn, preserving
  command/text-input ordering, and schedules only genuinely suspended futures.
  Native Open/Save/protected-close handlers await existing completion channels;
  foreground access revalidates captured identity after suspension.
- ✅ Dynamic extension registrations, view overrides, ownership and lifecycle
  cleanup still use the existing catalog and command tree.
- ✅ Local macOS validation: `cargo test --workspace --all-targets` (619 library
  tests, 3 macro tests, ordinary product-library capability linking and 5 example
  tests); `cargo test --doc` (4 compile-fail cases plus a positive authoring example);
  `cargo clippy --workspace --all-targets -- -D warnings`;
  `scripts/check-extension-api.sh`; `scripts/build-macos-app.sh`;
  `git diff --check`; and 53 local Markdown links in changed documents.
  `cargo fmt --all` ran once after Rust work and before final checks.
- ✅ Existing regressions exercise view/workbench/application precedence and
  decline, typed argument rejection, captured/palette/delayed targets, keymaps/menu
  turn ordering, pending Open/Save/closure, nested inline/cross-lifecycle composition,
  cancellation, extension disposal/unload, and product/diagnostic startup commands.
  New checks cover deterministic duplicate/title diagnostics, shared participant
  metadata, typed outcome conversion, foreground execution and suspension/resumption.
  A comparison with the previous catalog confirms all 53 names/titles are preserved.
- ⏳ Remote Linux/Windows checks remain pending; no remote run exists for these
  local changes.
- ✅ Product review gate: user accepted checkpoint 3 on 2026-10-15 after reviewing
  native declarations and product behavior. UI automation was excluded by the
  user's repository instructions.

### 4. Cleanup, documentation and completion ✅

- ✅ Remove remaining migration adapters and obsolete registry plumbing. Useful domain
  sums such as application events, document kinds and command outcomes remain.
- ✅ Update architecture/decisions, extension authoring docs/declarations, checked
  examples, this initiative's sequencing, and the roadmap. No speculative public API.
- **Validation:** run `cargo test --all-targets`, `cargo clippy --all-targets -- -D warnings`,
  `scripts/check-extension-api.sh`, `git diff --check`, changed Markdown-link checks,
  and the product build. Verify representative operations, structured errors, native
  commands and installed JS commands end to end with the complete existing suite.
- **Review gate:** approve the behavior-preserving refactoring and its final
  operation/command/error authoring shape before marking D053 complete.

Checkpoint 4 implementation evidence:

- ✅ Audited application/host/macro code and tests: no per-operation request,
  result, kind, query or central error mirrors, handwritten host/native command
  registration lists, or temporary migration adapters remain. Renamed the command
  bridge's pending reply state to describe its typed channel and retained caller
  lifecycle ownership. Semantic command outcomes and domain sums remain intact.
- ✅ Code-owned native authoring contracts document both attributes, generated
  constants, typed arguments/results/errors, synchronous decline and owned
  foreground access. The extension guide/declarations/checked examples explain
  awaited native/JS composition and optional structured diagnostics. Removed stale
  claims that already available application APIs remain deferred.
- ✅ The checked authoring package demonstrates a JS command awaiting native
  Select All; its existing product-host regression now verifies the resulting
  captured selection through native Copy. TypeScript checks cover optional JSON
  details and read-only diagnostics. Platform CI also selects native command
  declaration/discovery tests alongside host declarations and ordinary library
  linking. The roadmap and parent initiative retain D048 as the next numbered
  checkpoint after D053's final approval and archival.
- ✅ Final local validation: `cargo test --workspace --all-targets` (619 library
  tests, 3 macro tests, the ordinary product-library linking test and 5 example
  tests); `cargo test --doc` (4 compile-fail cases plus a positive authoring example);
  `cargo clippy --workspace --all-targets -- -D warnings`;
  `scripts/check-extension-api.sh`; `scripts/build-macos-app.sh`;
  `git diff --check`; and 73 local Markdown links in changed documents.
  `cargo fmt --all` ran once at the end of Rust work before final checks/commit.
- ✅ The full suite verifies representative declared capabilities, structured
  errors and precision, native menu/keymap/palette routing and async work,
  installed/checked JS command composition, lifecycle ownership and cancellation.
  Product behavior was accepted at checkpoint 3; no UI automation was used.
- ⚪ Validation limitation: remote Linux/Windows build/linking results remain
  unverified. These local changes have no remote run. CI is configured for host
  and native-command discovery plus product builds on supported platforms. No
  remote success is claimed.
- ✅ Final review gate: user approved the behavior-preserving refactoring and
  final operation/command/error authoring shape on 2026-10-15, following delivery
  of the passing local validation record and disclosed remote-platform limitation.
  No new public capabilities or D048 implementation were introduced.
- ✅ Completion audit: all 33 current host capabilities and 55 native participant
  declarations (53 catalog definitions) use automatic typed declarations; obsolete
  mirrors/adapters are absent. Durable boundary, command, lifecycle and persistence
  coverage passes. Architecture, authoring contracts, examples and sequencing are
  updated. Archived in a separate commit; D048 remains the next product checkpoint.

## Scope and completion

Exclude D048 storage, new public capabilities, public command-schema introspection,
user macros/aliases/repetition/task graphs (D020), scheduling policy (D019), generic
UI/WebViews, and unrelated editor/model restructuring. No backward compatibility
layer is required; do not retain dual transports or legacy registries at completion.

Complete only after all checkpoints/gates pass and current operations/native
commands use the new mechanism. Keep tests proving durable model/lifecycle/command
paths. Archive this plan in a separate final commit, fixing links, after approval.
Then resume the existing parent checkpoint 4 (D048); do not renumber parent
checkpoints or mark the broader extension/project initiative complete.
