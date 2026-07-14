# Step 5 — Annotation Benchmark Findings

Full stress-test of the `AnnotationStore` token-anchor model against a naive
offset-remap baseline (D5). Load: 1M-line buffer (`rust_sample.kfx --tile 635`),
10k annotations, randomized interleaved edits from 3 and 6 independent sources
(5 seconds per workload, `--release`).

## Hardware

Apple M4 Pro, macOS 15.

## Results

### 3 sources (10k annotations, 1M lines)

| Workload           | Metric              | Value     |
|--------------------|---------------------|-----------|
| stabilize (token)  | p50 / edit          | 12.2 µs   |
| stabilize (token)  | p99 / edit          | 171 µs    |
| stabilize (baseline)| p50 / edit          | 69.5 µs   |
| stabilize (baseline)| p99 / edit          | 88.3 µs   |
| **token vs baseline**| **p50 ratio**    | **5.7× faster** |
| resolve-all        | p50 / frame (9999)  | 1.79 ms   |
| resolve-all        | p99 / frame         | 2.49 ms   |
| query              | p50 / query         | 15.5 µs   |
| query              | p99 / query         | 29.2 µs   |
| peak RSS           |                     | 472 MiB   |
| throughput         | edits / s           | 1,080     |

### 6 sources (10k annotations, 1M lines)

| Workload           | Metric              | Value     |
|--------------------|---------------------|-----------|
| stabilize (token)  | p50 / edit          | 12.4 µs   |
| stabilize (token)  | p99 / edit          | 165 µs    |
| stabilize (baseline)| p50 / edit          | 69.7 µs   |
| stabilize (baseline)| p99 / edit          | 90.3 µs   |
| **token vs baseline**| **p50 ratio**    | **5.6× faster** |
| resolve-all        | p50 / frame (9996)  | 1.79 ms   |
| resolve-all        | p99 / frame         | 2.48 ms   |
| query              | p50 / query         | 15.6 µs   |
| query              | p99 / query         | 29.6 µs   |
| peak RSS           |                     | 490 MiB   |
| throughput         | edits / s           | 1,100     |

## Findings

1. **Token-anchored stabilization beats the offset-remap baseline by 5.6–5.7×**
   at 10k annotations. The ratio compresses from the development-fixture
   smoke data (17× at 500 annotations) because the edit-log walk and piece
   table operations dominate more at scale, but the O(affected) property
   holds: untouched annotations cost nothing.

2. **Per-annotation resolve cost:** ~179 ns (1.79 ms / 9,999 annotations).
   Consistent with step 4's sub-µs claim for warm resolve chains.

3. **6 sources are indistinguishable from 3.** The number of annotation
   sources does not affect stabilization performance — only the total
   annotation count matters. This confirms step 6 (composition of independent
   sources) has no per-source scaling penalty.

4. **Query latency** (~15.5 µs p50) benefits from the lazily-rebuilt interval
   index. The first query after edits pays an O(annotations) rebuild (~1.79 ms),
   but amortized over the frame's queries it's negligible.

5. **RSS:** ~472 MiB for the full 1M-line buffer + 10k annotations.
   The buffer's piece table accounts for most of this; annotations are
   ~56 bytes each (two `Position` tokens + metadata).

6. **p99 tail:** ~171 µs for token stabilize, ~88 µs for baseline. The
   token store's tail is higher because occasionally an edit touches many
   annotated pieces (e.g. a large delete spanning multiple annotation
   endpoints), while the baseline is uniformly O(all) every time.

## Decision

The token-anchor representation (D1) is confirmed as the correct choice.
Stabilization is fast enough to run inline with every edit at 10k+ annotations
on a 1M-line buffer — well within a rendering frame budget. No reason to reopen
step 4. Proceed to step 6 (annotation composition).
