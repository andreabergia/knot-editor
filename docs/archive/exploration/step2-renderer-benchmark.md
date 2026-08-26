# Renderer Benchmark Evidence

The renderer experiment compared Skia with wgpu + cosmic-text using the same
visible-line harness. It measured rendering primitives, not complete UI
frameworks.

Measured on Apple M1 Pro, macOS, release builds. Frame times include shaping and
painting the visible range.

| Fixture | Backend | p50 | p99 | Peak RSS |
| --- | --- | ---: | ---: | ---: |
| Rust, 1,577 lines | wgpu/cosmic | 9.96 ms | 13.29 ms | 125.5 MiB |
| Rust, 1,577 lines | Skia | 1.82 ms | 11.73 ms | 295.5 MiB |
| Rust, 1,001,395 lines | wgpu/cosmic | 9.91 ms | 13.41 ms | 527.1 MiB |
| Rust, 1,001,395 lines | Skia | 1.82 ms | 12.15 ms | 703.6 MiB |
| CJK | wgpu/cosmic | 2.14 ms | 10.84 ms | 126.5 MiB |
| CJK | Skia | 1.41 ms | 12.29 ms | 102.1 MiB |
| Arabic | wgpu/cosmic | 6.43 ms | 7.03 ms | 124.4 MiB |
| Arabic | Skia | 1.38 ms | 13.64 ms | 93.4 MiB |
| Emoji | wgpu/cosmic | 12.80 ms | 13.54 ms | 127.9 MiB |
| Emoji | Skia | 1.84 ms | 12.77 ms | 98.6 MiB |
| Minified JavaScript | wgpu/cosmic | 6.87 ms | 7.47 ms | 124.6 MiB |
| Minified JavaScript | Skia | 2.19 ms | 15.61 ms | 91.2 MiB |

## Findings

- Skia had the lowest p50 on every fixture and handled CJK, BiDi, and emoji
  without API changes.
- Skia had higher and less predictable p99 spikes and a higher baseline RSS.
- cosmic-text's shaping quality was inadequate for the CJK fixture. Raw
  rustybuzz would require rebuilding the layout, fallback, caching, and atlas
  layers, so it was not a meaningfully separate candidate.
- Visible-range computation was negligible compared with shaping and painting.

## Decision

Skia cleared the rendering-primitive bar, but the experiment did not select
Knot's product UI stack. The subsequent framework evaluation selected gpui,
whose integrated text path made a separate renderer unnecessary.
