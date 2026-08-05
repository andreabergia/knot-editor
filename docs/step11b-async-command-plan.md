# Step 11b: Async Command Invocation Exploration

This follow-up explores whether commands can invoke and await other commands
without compromising the extension runtime's serial callback model. It is not
required for the step 11 focus-routing checkpoint.

## Starting constraints

- ✅ JavaScript, rather than a Knot-specific composition DSL, is the intended
  way to sequence, branch, loop over, and name composed behavior.
- ✅ A registered JavaScript function may eventually invoke commands and be
  registered under its own command name.
- ✅ Composite command definitions, typed pipelines, repetition wrappers,
  decorators, rollback, and implicit undo grouping are not planned.
- ✅ Step 11 supports only top-level programmatic invocation and rejects
  invocation from inside a running extension command handler.
- ✅ Extension callbacks are currently serial within one extension and may run
  concurrently across extensions.

The intended user-level expression is ordinary JavaScript:

```js
await commands.register("knot.format-and-save", async () => {
  await commands.invoke("knot.format", {});
  await commands.invoke("knot.save", {});
});
```

## Questions to answer

- Can a native command be awaited from an extension handler without adding a
  second execution model?
- Can one extension await a command owned by another extension while both
  retain serial callback ordering?
- What should happen when an extension invokes one of its own commands: inline
  reentrant execution, queued execution with deadlock detection, or explicit
  rejection?
- Are command results limited to success/failure, or is a small serializable
  return value justified by real composition examples?
- Does cancellation of a parent invocation cancel its awaited child, merely
  stop waiting, or leave the child independent?
- Which captured context does a child receive by default, and should explicit
  retargeting ever be allowed?
- Can independent invocations run concurrently without weakening foreground
  mutation ordering or lifecycle cleanup?

## Exploration sequence

### 1. Establish concrete composition fixtures

- ⬜ Use one native-to-extension or extension-to-native sequence with an actual
  editor workflow; do not design from `format-and-save` alone.
- ⬜ Include one failure and one cancellation between the two awaited commands.
- ⬜ Record whether success/failure is sufficient or a returned value is truly
  needed.

### 2. Probe runtime scheduling alternatives

- ⬜ Prototype awaiting a native command from an extension handler.
- ⬜ Prototype cross-extension invocation while both runtimes preserve serial
  callbacks.
- ⬜ Test same-extension invocation separately; do not generalize from the
  cross-extension result.
- ⬜ Compare inline reentrancy, a scheduler aware of parent/child waits, and
  continued rejection using observable ordering and failure behavior.

### 3. Define invocation relationships only if justified

- ⬜ Decide whether awaited commands require explicit parent/child identities.
- ⬜ Decide cancellation propagation and lifecycle teardown behavior.
- ⬜ Preserve the parent's captured target by default and revalidate before
  every child mutation; never resolve a child against whatever is focused
  later.
- ⬜ Avoid a general task graph or pipeline type unless the fixtures demonstrate
  a concrete requirement.

### 4. Record or reject the capability

- ⬜ If a small scheduling rule is sufficient, implement the minimum
  command-to-command invocation path and record it in architecture and
  decisions.
- ⬜ If same-extension serial execution makes the feature disproportionately
  complex, retain the explicit rejection and direct extensions toward
  semantic host APIs rather than command composition.
- ⬜ Update this plan and the roadmap with the result.

## Decision checkpoint

Awaitable command-to-command invocation is justified only if ordinary
JavaScript composition can reuse meaningful editor operations with predictable
ordering, cancellation, target identity, and lifecycle behavior, without
turning commands into a second typed function or task-graph system.
