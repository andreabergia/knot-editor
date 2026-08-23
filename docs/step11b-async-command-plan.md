# Step 11b: Async Command Invocation Exploration

This follow-up validates whether commands can invoke and await other commands
without compromising focus routing or the extension runtime's serial callback
model. It is not required for the step 11 focus-routing checkpoint.

## Starting constraints

- ✅ JavaScript, rather than a Knot-specific composition DSL, sequences,
  branches, loops over, and names composed behavior.
- ✅ Public JavaScript keeps the awaitable `commands.invoke(name, arguments)`
  API. Enqueueing is an internal dispatcher concept, not a second public API.
- ✅ Keybindings and the palette do not need to observe completion; JavaScript
  callers receive the same structured completion outcome whether or not they
  await it immediately.
- ✅ Extension callbacks remain serial within one extension. Independent
  extension runtimes may progress concurrently.
- ✅ Composite command definitions, typed pipelines, repetition wrappers,
  decorators, rollback, implicit undo grouping, and serializable command return
  values are not planned.
- ✅ Step 11 supports only top-level programmatic invocation and rejects
  invocation from inside a running extension command handler.

The intended user-level expression remains ordinary JavaScript:

```js
await commands.register("knot.format-and-save", async () => {
  const format = await commands.invoke("knot.format", {});
  if (format.kind !== "completed") return format;
  return commands.invoke("knot.save", {});
});
```

## Scheduling model to validate

The dispatcher should admit work through one internal operation equivalent to:

```text
enqueue(Command, captured context, optional parent) -> invocation + completion
```

Admission allocates the invocation identity and captures the origin immediately.
Routing and execution may start later. Keybindings and the palette enqueue and
ignore completion; `commands.invoke` awaits it.

The prototype should validate these deliberately narrow rules:

- Root invocations are admitted in FIFO order and remain serialized.
- A command invoked by an active handler is its child. Native and
  cross-extension children may run while their parent is suspended awaiting
  completion; unrelated roots do not run in that gap.
- A command owned by the calling extension executes as a nested JavaScript
  handler call, using the current serial callback rather than queueing behind
  itself.
- A cross-extension cycle targeting a runtime already suspended in the active
  ancestry is rejected instead of deadlocking.
- Children inherit the parent's captured window, workspace, focus, and optional
  buffer. This step adds no retargeting API.
- Cancellation of a parent propagates to its unfinished children. Cancelling a
  child does not cancel its parent; the parent observes the child's cancelled
  outcome and decides what to do.
- Command composition uses the existing structured outcomes only. A handler
  failure or unsuccessful child does not implicitly fail the parent if the
  parent's JavaScript chooses to handle it.

This is scheduling around one active root and its descendants, not a general
concurrent command executor or task graph.

## Exploration sequence

### 1. Establish concrete composition fixtures

- ✅ Add a composite extension command that asks a second extension command to
  insert a unique marker into the captured source buffer, then invokes a native
  search command for that marker. Verify that the search observes the edit and
  produces the normal generated results buffer.
- ✅ Invoke the same-extension path with a small wrapper command and verify that
  the nested handler completes inline without admitting another extension
  callback.
- ✅ Make the first child fail and verify that the second operation runs only if
  the composite JavaScript explicitly continues.
- ✅ Suspend a child, cancel the root, and verify that neither the child nor the
  remaining sequence performs a late mutation.
- ✅ Keep command completion limited to the existing structured outcome unless
  these fixtures demonstrate that a serializable value is necessary.

### 2. Separate admission from execution

- ✅ Replace the fire-and-forget `dispatch_command` entry point with one internal
  enqueue path that captures context, allocates identity, and owns completion.
- ✅ Route keybindings, palette confirmation, and top-level scripts through that
  path without changing gpui focus dispatch.
- ✅ Queue root invocations rather than rejecting them merely because another
  root is active; start the next root after the current invocation tree settles.
- ✅ Preserve the current behavior for invalid origins, missing commands,
  disposed lifecycles, and unclaimed native actions.
- ✅ Ensure every admitted invocation completes exactly once during success,
  rejection, cancellation, target loss, lifecycle teardown, and shell teardown.

### 3. Permit awaited child execution

- ✅ Carry the active invocation identity on handler-originated host requests and
  reject requests whose claimed parent is no longer active.
- ✅ Route a native child through gpui from the inherited captured focus and
  complete its promise from the native action outcome.
- ✅ Route a cross-extension child to its owning runtime while retaining the
  parent's suspended state and serial callback ownership in both runtimes.
- ✅ Detect ancestry cycles before dispatching to a runtime already occupied by
  an ancestor and return a structured unavailable outcome.
- ✅ Resume root scheduling only after the root and all attached child work have
  settled.

### 4. Handle same-extension invocation inline

- ✅ Resolve command ownership through the catalog before selecting the inline
  path; do not bypass global name ownership in JavaScript.
- ✅ Invoke the registered handler as a nested JavaScript call with inherited
  buffer, arguments, context, and cancellation signal.
- ✅ Verify nested ordering across synchronous work and asynchronous waits while
  no unrelated callback enters that extension runtime.
- ✅ Bound or reject recursive same-extension command cycles so accidental
  recursion fails predictably rather than exhausting the isolate stack.
- ✅ Keep ordinary shared implementation functions as the recommended mechanism
  when an extension does not need command lookup or dispatch semantics.

### 5. Define cancellation and lifecycle cleanup

- ✅ Record explicit parent/child identities only to the extent needed for
  ancestry checks, completion, and cancellation propagation.
- ✅ Propagate root cancellation and lifecycle teardown through unfinished
  descendants, including a child owned by another extension.
- ✅ Revalidate the inherited invocation context immediately before every
  delayed foreground mutation; never fall back to current focus or active
  buffer.
- ✅ Ignore late completion after cancellation without allowing it to overwrite
  the terminal outcome or start the next composed step.
- ⬜ Confirm that disposing either participating extension settles all affected
  promises and allows the root queue to continue.

### 6. Record or reject the capability

- ⬜ Compare the observed scheduling and failure behavior with direct JavaScript
  function composition and confirm that command reuse adds real value.
- ⬜ If these rules are sufficient, update `architecture.md` with admission,
  root/child scheduling, context inheritance, and runtime ownership.
- ⬜ Record the validated choices and remaining liabilities in `decisions.md`.
- ⬜ Update this plan and `roadmap.md` with ✅ markers and the checkpoint result.
- ⬜ Run focused tests, run `cargo fmt` once at the end of Rust work, then run
  the full test suite.

## Commit boundaries

1. Dispatcher enqueue/completion abstraction and serialized root queue.
2. Native and cross-extension awaited child scheduling.
3. Same-extension inline invocation and recursion/cycle rejection.
4. Cancellation, lifecycle cleanup, and failure fixtures.
5. Architecture, decisions, roadmap, and completed-plan updates.

## Out of scope

Serializable command return values, arbitrary concurrent roots, detached or
fire-and-forget child commands, priorities, backpressure policy, explicit
retargeting, cancellation trees exposed to extensions, progress reporting,
timeouts, pipelines, rollback, implicit undo grouping, and a general task-graph
scheduler remain deferred.

## Decision checkpoint

Awaitable command-to-command invocation is justified only if the editor fixture
can reuse native, cross-extension, and same-extension commands with predictable
ordering, cancellation, inherited target identity, and lifecycle behavior using
the narrow scheduling rules above. If execution requires general dependency
scheduling or weakens serial extension callbacks, retain explicit rejection and
direct extensions toward ordinary JavaScript functions and semantic host APIs.
