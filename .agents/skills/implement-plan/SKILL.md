---
name: implement-plan
description: Implement a named execution plan or checkpoint in a code repository, keeping the plan, tests, docs, and commits aligned with the work. Use when asked to execute an existing plan; not for drafting one.
---

# Implement a plan

- Read the requested plan and repository instructions. Follow the plan's scope, dependencies, review gates, and delivery protocol. A request for one checkpoint does not authorize starting the next one.
- Check the working tree before editing. Preserve unrelated user changes. Resolve open implementation choices from the plan, code, and repository guidance; raise only decisions that genuinely need the user's input.
- Complete the requested scope, including relevant automated tests and documentation. Keep the plan current as work lands, marking completed items with ✅ and recording material scope decisions. Do not mark an item complete until its acceptance criteria are met.
- Make logical commits as work lands when the repository's instructions or the request call for commits. Respect local commit and formatting rules. If the request authorizes subagents, use them where independent work benefits from delegation.
- Honor explicit review or manual gates. Report what is ready for review and stop there when the gate requires user action.

## Completing a whole plan

When **every** checkpoint and completion criterion has passed, finish implementation, tests, documentation, and their commits first. Then move the completed plan from `docs/plans/` to `docs/archive/plans/` in a **separate final commit**. Include any link fixes caused by the move in that commit. Do not archive a plan after completing only one checkpoint, or while a required gate remains open.
