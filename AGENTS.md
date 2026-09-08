# Knot

Knot is a modern text editor, inspired in philosophy by Emacs, but built with
modern technologies and paradigms. It is implemented in Rust.

Use @docs/architecture.md for current boundaries and @docs/decisions.md for
validated choices and rationale. Load the linked @docs/architecture subsystem
references only when the task needs their runtime flows or behavioral constraints.
Read @docs/design.md only when product vision is relevant,
and @docs/product-slices.md only for product sequencing. Active execution plans
live under @docs/plans. Load archived exploration plans and benchmark evidence
only when the task depends on them.

The architecture exploration is complete. Product development evolves the
validated prototype in place through end-to-end usable slices. Knot is now
building the real product: correctness and regression safety take priority over
prototype speed. Automated tests are required for durable behavior and every
completed slice must leave its important model, lifecycle, command, and
persistence paths covered. Use focused tests where they provide confidence,
and add UI or integration coverage when behavior cannot be validated at a
lower boundary.

## Rules

- Whenever implementing a plan or design document, always keep the plan
updated. Use ✅ and other emoji to mark executed steps.
- Keep @docs/architecture.md up-to-date when a change alters architectural
  boundaries, ownership, or dependency direction. Update the relevant
  @docs/architecture subsystem reference when runtime flows or behavioral
  constraints change. Keep rationale in @docs/decisions.md and implementation
  details and API contracts in the code.
- Code comments and module documentation must describe the current design and
  behavior only. Do not record migrations, previous locations, roadmap phases,
  or implementation history in code; Git history is the source for that.
- When executing multiple steps or tasks, split them into logical commits as
  you go.
- Don't use conventional commits.
- Run `cargo fmt` once at the end of Rust work, before committing. Do not use
  `cargo fmt --check` or manually format individual files.
