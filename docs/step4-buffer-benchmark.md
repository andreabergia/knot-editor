# Buffer Benchmark Evidence

The benchmark exercises the stable-ID piece table and position-token edit log
on a 1,001,395-line, 29 MiB Rust fixture.

Measured on Apple M1 Pro, macOS, release builds. Absolute tail values include
system noise; steady-state p50 values are the primary signal.

| Workload | Result |
| --- | --- |
| Construct text backing | 10.3 ms |
| Build line index lazily | 17.4 ms |
| Edit with line index | p50 678 µs; p99 1,851 µs |
| Edit without line index | p50 75 µs; p99 201 µs |
| Interleaved producers | p50 504 µs; p99 1,914 µs |
| Idle edit-log remap | p50 41 ns; p99 292 ns |
| `line_of_offset`, 513 pieces | p50 541 ns; p99 2,416 ns |
| `line_start`, 513 pieces | p50 41 ns; p99 125 ns |
| `position_at`, 513 pieces | p50 250 ns; p99 459 ns |
| `resolve`, 513 pieces | below timer resolution at p50; p99 42 ns |

## Findings

- The piece table sustained roughly 10k edits/second without the line index on
  a 79k-piece chain. This is comfortably above single-user editing rates.
- The `Vec<usize>` line index caused about 88% of live-index edit latency
  through its linear suffix shift. Its public lookup surface permits a later
  tree/Fenwick replacement.
- Position issuance and resolution were sub-microsecond. Edit-log replay was
  effectively free when no split affected a token and about 100 ns per matching
  split in the focused proof.
- Text backing and the line index added roughly 30 MiB of resident data. Peak
  benchmark RSS near 480 MiB was dominated by retained randomized inserted text
  and the intentionally unbounded edit log.

## Decision

Keep the stable-ID piece table and position-token surface. Defer replacing the
line index until a real workload requires it. Compaction of inserted backing
storage and edit history remains production work.
