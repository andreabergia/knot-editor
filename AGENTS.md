# Knot

Knot is a modern text editor, inspired in philosophy by Emacs, but built with
modern technologies and paradigms. It is implemented in Rust. Refer to
@docs/design.md for the high level vision and @docs/architecture.md for the
current code architecture.

It is currently in the prototype phase, and we are attempting to validate the
various assumptions and the architecture. In the current state, speed and ease
of iteration is paramount, so leave automated tests to a minimum. We will start
from scratch after the core choices are validated.

## Rules

- Whenever implementing a plan or design document, always keep the plan
updated. Use ✅ and other emoji to mark executed steps.
- Keep @docs/architecture.md up-to-date whenever a change alters the code
  architecture, module responsibilities, or important runtime flows.
- When executing multiple steps or tasks, split them into logical commits as
you go.
* Don't use conventional commits
- Run `cargo fmt` once at the end of Rust work, before committing. Do not use
  `cargo fmt --check` or manually format individual files.
