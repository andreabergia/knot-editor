# Step 6 — Anchored-Range Composition

## Question

Can independent range producers coexist without coupling the stable-range
mechanism to a closed list of editor features?

## Answer

Yes, but Step 8 clarified the boundary.

`AnchoredRangeStore` composes only stable geometry. It stores IDs and anchored
endpoints, permits identical and overlapping ranges, and answers unfiltered
range queries. It has no concept of diagnostics, search, git, breakpoints,
folding, extensions, rendering, or provider payloads.

`BufferModel` owns the semantic layer:

```text
ContributionSource → ContributionSet
                         |
                         v
                  Contributions
                    /       \
          AnchoredRangeId   presentation metadata
```

Each built-in or extension source publishes one complete contribution set per
buffer. Multiple sources may create distinct anchored ranges with identical
resolved byte ranges. The application retains every contribution and owns
surface-specific composition policy. The current decoration path orders tokens
by precedence; durable exact-overlap policies for each presentation channel
remain open.

This supersedes the prototype's earlier `AnchoredRangeKind`,
`AnchoredRangeData`, and `query_range_for_kinds` design. Those types mixed
editor semantics into the core geometry mechanism and were removed during
Step 8.

## Remaining presentation decision

Coexistence in storage is proven. Exact visual overlaps still need explicit
per-channel policies:

- decorations may merge or choose token-specific precedence;
- gutter markers may stack or aggregate;
- commands and actions must remain individually reachable.

That policy belongs in `app`, not `AnchoredRangeStore`.
