# D053 — Handler-defined operations, errors, and commands

Status: 🟡 checkpoint 1 authoring shape approved on 2026-10-15; checkpoint 2
in progress. Remote platform checks remain pending.
Execute after D047 is approved and archived, before D048 adds more host APIs.
This is a separate refactoring, not another D047 acceptance requirement.

Source: the protocol/command design discussion of 2026-10-15; scheduled by the
[roadmap](../roadmap.md) and the
[extension platform plan](extension-platform-and-projects-plan.md).
Current constraints: [architecture](../architecture.md), [decisions](../decisions.md),
[extension host](../architecture/extension-host.md), and
[commands](../architecture/commands.md). D047's implementation and acceptance
record live in its [completed plan](../archive/plans/d047-application-state-and-operations-plan.md).

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

Illustrative signatures, not final API contracts:

```rust
#[host_operation("documents.open")]
async fn open_document(context: HostContext, args: OpenDocumentArgs)
    -> Result<DocumentPresentation, OpenError>;

#[command("file.open", title = "Open…")]
async fn open(context: CommandContext, args: OpenArguments)
    -> Result<(), OpenError>;
```

The command adapter must also represent a participant declining a command; the
illustration does not replace that routing result with an error or completion.

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
- ✅ `HostContext` owns a weak host, async application handle and admitted request.
  Its short foreground closures check lifecycle/cancellation and return owned
  results. Open awaits a native completion, retaining destination/caller authority,
  and checks the exact resulting presentation again before replying.
- ✅ Host-owned argument readers retain direct recognized-field V8 decoding.
  Success/error encoding checks numeric precision. Generated typed responders
  enforce result/error pairing; runtime resolver checks retain operation identity.
- ✅ `HostError` uses explicit `code`/`message` attributes, named structured details,
  and a single-field `transparent` composition form. Descriptor error codes plus
  derived codes for the unmigrated error enum check the closed TypeScript union.
  That enum remains only for existing handlers and shared product-operation
  failures; Open has a bounded typed translation of those failures.
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

### 2. Migrate the host capability boundary 🟡

- Migrate every existing JS-to-native capability, including buffer/query/mutation,
  observation/disposal, clipboard/context, commands, providers, and keymap batches.
  Preserve synchronous public keybinding batching despite the internal async model.
- Remove the per-operation `HostOperation`, `HostResponseValue`, `HostOperationKind`
  mirrors and their handwritten decode/encode/dispatch tables once unused.
- Replace central `HostRequestError` membership with handler-local/shared errors
  and the common envelope. Keep current public codes, names, messages/details
  semantics and closed declaration coverage synchronized through descriptors.
- **Validation:** existing D046/D047 boundary and product-host regressions; invalid
  fields/coercion, precision, exact targets, missing context, racing I/O, cancellation
  and queued responses, self-disposal, failed activation, unload and host release.
- **Review gate:** verify every current capability uses its declaration, no parallel
  operation/error registry remains, and representative JS consumers behave the same.

### 3. Migrate native command declarations ⬜

- Automatically discover annotated native command handlers and catalog metadata.
  Migrate native commands across editor views, workbench and application routing;
  retain explicit participant decline and reuse shared handlers/types where useful.
- Remove replaced command-name/metadata/registration boilerplate. Handler internals
  may still match semantic values; do not mechanically ban useful Rust enums/matches.
- Keep extension registrations, view-specific overrides, name ownership, and
  lifecycle cleanup in the current catalog and dispatcher.
- **Validation:** view/workbench/application precedence, decline/unavailable,
  typed argument failure and outcome adaptation, captured/palette/delayed targets,
  menus/keymaps, native async work, inline/cross-lifecycle composition, cancellation,
  extension registration/disposal/unload, and product/diagnostic startup commands.
- **Review gate:** review representative native commands and exercise palette,
  keymap/menu, async Open/Save, and extension command composition in the product.

### 4. Cleanup, documentation and completion ⬜

- Remove remaining migration adapters and obsolete registry plumbing. Useful domain
  sums such as application events, document kinds and command outcomes remain.
- Update architecture/decisions, extension authoring docs/declarations, checked
  examples, this initiative's sequencing, and the roadmap. No speculative public API.
- **Validation:** run `cargo test --all-targets`, `cargo clippy --all-targets -- -D warnings`,
  `scripts/check-extension-api.sh`, `git diff --check`, changed Markdown-link checks,
  and the product build. Verify representative operations, structured errors, native
  commands and installed JS commands end to end with the complete existing suite.
- **Review gate:** approve the behavior-preserving refactoring and its final
  operation/command/error authoring shape before marking D053 complete.

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
