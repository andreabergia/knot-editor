---
name: brainstorm-plan
description: Discuss the design of a Knot task, then write a high-level execution plan with review and testing checkpoints in docs/plans. Use for planning a task, not implementing an existing plan.
---

# Brainstorm a task into an execution plan

The invocation supplies the task and any relevant notes, either directly or by reference. Treat those as the starting brief, not as settled design. If the task itself is missing, ask for it.

1. Read the task and notes. Check `docs/architecture.md` and `docs/decisions.md` for existing boundaries and choices, then inspect only the subsystem references, code, roadmap entries, or prior plans needed to understand this task. Distinguish validated constraints from open decisions.
2. Discuss the central shape of the solution before writing the plan. Present the few consequential open points, plausible alternatives, tradeoffs, and a recommendation in short batches. Ask for the user's judgment where product behavior, ownership, or scope cannot be inferred. Continue the discussion until the important choices are agreed; keep implementation detail proportional to the decision at hand.
3. Once aligned, create a high-level plan under `docs/plans/`. State the user-visible outcome, chosen design and boundaries, explicit exclusions, and the decisions still deferred. Split work into coherent, end-to-end checkpoints with concrete deliverables, relevant automated validation, and a review gate for each checkpoint. Sequence checkpoints so review can change later work without undoing completed work.
4. Cross-check the plan against the agreed choices and repository instructions. Link the source task and relevant docs. Keep design rationale in the plan while decisions are proposed; update `docs/decisions.md` only when a choice has been validated. Do not implement the plan as part of this skill unless separately requested.

Keep the conversation concise. Do not produce a detailed execution plan before the design discussion has resolved the material open points.
