# Knot

Knot is a modern text editor, inspired in philosophy by Emacs, but built with
modern technologies and paradigms. It is implemented in Rust. Refer to
@docs/design.md for the high level vision.

It is currently in the prototype phase, and we are attempting to validate the
various assumptions and the architecture. In the current state, speed and ease
of iteration is paramount, so leave automated tests to a minimum. We will start
from scratch after the core choices are validated.

## Rules

- Whenever implementing a plan or design document, always keep the plan
updated. Use ✅ and other emoji to mark executed steps.
- When executing multiple steps or tasks, split them into logical commits as
you go.
* Don't use conventional commits
