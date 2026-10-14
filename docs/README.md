# Documentation

Current sources of truth:

- [architecture.md](architecture.md) is the short system overview: boundaries,
  ownership, dependency direction, and cross-cutting invariants. It links to
  focused [subsystem references](architecture/) for runtime flows and behavioral
  constraints; load only the references relevant to the task.
- [decisions.md](decisions.md) records validated choices and rationale, with
  links to subsystem references and supporting evidence.
- [design.md](design.md) describes product vision and long-term principles.
- [roadmap.md](roadmap.md) records the prioritized initiative and the unordered
  register of deferred work. The current initiative is
  [production extension APIs and JavaScript projects](plans/extension-platform-and-projects-plan.md).
- [plans/](plans/) contains active executable slice plans.
- [extension-api.md](extension-api.md) is the draft extension-author guide; API reference
  documentation lives beside the TypeScript declarations, and the internal audit
  lives in the D046 plan. Runtime implementation and review are still pending.
- [development.md](development.md) covers local checks, CI, and the Rust
  toolchain.

Completed prototype plans, measurements, and their roadmap live under
[archive/exploration/](archive/exploration/). They provide historical evidence
for decisions but do not describe current implementation status or product
sequence.
