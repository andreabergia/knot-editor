---
name: implement-plan
description: Implement the next unfinished checkpoint of a named Knot execution plan, or the one explicitly requested, then stop at its review gate. Use for executing a plan checkpoint, not drafting a plan.
---

# Implement the next checkpoint

- Read the requested plan and repository instructions. If the user names a checkpoint, implement that checkpoint; otherwise select the next unfinished checkpoint. Check its prerequisites and follow the plan's scope, review gate, and delivery protocol. Do not start a later checkpoint in the same request.
- Check the working tree before editing. Preserve unrelated user changes. Resolve open implementation choices from the plan, code, and repository guidance; raise only decisions that genuinely need the user's input.
- Complete the selected checkpoint, including relevant automated tests and documentation. Keep the plan current as work lands, marking completed items with ✅ and recording material scope decisions. Do not mark an item complete until its acceptance criteria are met.
- Make logical commits as work lands when the repository's instructions or the request call for commits. Respect local commit and formatting rules. If the request authorizes subagents, use them where independent work benefits from delegation.
- Stop at the selected checkpoint's review or manual gate. Report what is ready for review and what, if anything, remains for the user to verify.

## Completing a whole plan

When the selected checkpoint is the **last** one and every completion criterion and required gate has passed, finish implementation, tests, documentation, and their commits first. Then move the completed plan from `docs/plans/` to `docs/archive/plans/` in a **separate final commit**. Include any link fixes caused by the move in that commit. Do not archive while another checkpoint or gate remains open.
