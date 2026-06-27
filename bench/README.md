# Bench fixtures

Version-controlled fixtures for the renderer benchmark (step 2). Each
fixture is a `.kfx` file in the format understood by
`src/view/fixture.rs`:

```
knot-fixture v1
<line count N>
N lines of raw text
---                              (optional; only if there are segment specs)
<line> <start> <end> <color> <bold> <italic>   (0xRRGGBB, 0/1)
...
```

The segment table is optional. Fixtures whose styles are not
load-bearing (CJK, Arabic, emoji, minified) omit it and render in a
single default style.

## Fixture set

| # | File                | Stresses                                            |
|---|---------------------|-----------------------------------------------------|
| 1 | `rust_sample.kfx`   | Multi-color segments with bold/italic (keywords bold, comments italic, strings, types, functions), ASCII, mix of line lengths |
| 2 | `rust_sample.kfx` + `--tile 635` | 1M-line tiled Rust. Visible-range culling, peak RSS, sustained throughput |
| 3 | `cjk.kfx`           | CJK wide glyphs, mixed CJK+ASCII                    |
| 4 | `arabic.kfx`        | BiDi reordering, contextual shaping, RTL            |
| 5 | `emoji.kfx`         | ZWJ sequences, variation selectors, wide emoji      |
| 6 | `minified_js.kfx`   | Very long single lines, horizontal scroll           |

Fixture 2 is generated at startup by tiling fixture 1 with `--tile`;
it is not checked in as 1M lines.

## Regenerating fixture 1

`rust_sample.kfx` is produced from `rust_sample.rs` by a throwaway
codegen tool. The bench runtime pulls in no parser; the `.kfx` is the
version-controlled artifact. To regenerate after editing the source:

```
python3 bench/tools/tokenize_rust.py bench/fixtures/rust_sample.rs bench/fixtures/rust_sample.kfx
```

The tokenizer is a coarse regex approximation, not a real syntax
highlighter — good enough to exercise the multi-attribute shaping
path, which is the point.

## Running

```
cargo run --bin bench -- --fixture bench/fixtures/rust_sample.kfx --backend stub --duration 5
cargo run --bin bench -- --fixture bench/fixtures/rust_sample.kfx --backend stub --tile 635 --duration 5
```
