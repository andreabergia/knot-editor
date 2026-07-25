# Knot

Knot is a modern text editor, inspired in philosophy by Emacs, but built with
modern technologies and paradigms. It is implemented in Rust.

Use @docs/architecture.md for current boundaries and @docs/decisions.md for
validated choices. Read @docs/design.md only when product vision is relevant,
and @docs/roadmap.md only for prototype sequencing. Load benchmark evidence
only when the task depends on it.

It is currently in the prototype phase, and we are attempting to validate the
various assumptions and the architecture. In the current state, speed and ease
of iteration is paramount, so leave automated tests to a minimum. We will start
from scratch after the core choices are validated.

## Rules

- Whenever implementing a plan or design document, always keep the plan
updated. Use ✅ and other emoji to mark executed steps.
- Keep @docs/architecture.md up-to-date when a change alters architectural
  boundaries, ownership, dependency direction, or a major runtime flow. Keep
  implementation details and API contracts in the code.
- Code comments and module documentation must describe the current design and
  behavior only. Do not record migrations, previous locations, roadmap phases,
  or implementation history in code; Git history is the source for that.
- When executing multiple steps or tasks, split them into logical commits as
  you go.
- Don't use conventional commits.
- Run `cargo fmt` once at the end of Rust work, before committing. Do not use
  `cargo fmt --check` or manually format individual files.
