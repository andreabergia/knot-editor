# Bench fixtures

Version-controlled fixtures used by the annotation benchmark. Each
fixture is a `.kfx` file in the format understood by
`src/fixture.rs`:

```
knot-fixture v1
<line count N>
N lines of raw text
---                              (optional; only if there are segment specs)
<line> <start> <end> <color> <bold> <italic>   (0xRRGGBB, 0/1)
...
```

The segment table is optional. The annotation benchmark used the text only.

## Fixture set

| # | File                | Stresses                                            |
|---|---------------------|-----------------------------------------------------|
| 1 | `rust_sample.kfx`   | Multi-color segments with bold/italic (keywords bold, comments italic, strings, types, functions), ASCII, mix of line lengths |
| 2 | `rust_sample.kfx` + `--tile 635` | 1M-line tiled Rust. Peak RSS and sustained throughput |
| 3 | `cjk.kfx`           | CJK wide glyphs, mixed CJK+ASCII                    |
| 4 | `arabic.kfx`        | BiDi reordering, contextual shaping, RTL            |
| 5 | `emoji.kfx`         | ZWJ sequences, variation selectors, wide emoji      |
| 6 | `minified_js.kfx`   | Very long single lines, horizontal scroll           |

Fixture 2 was generated at startup by tiling fixture 1 with `--tile`.

## Regenerating fixture 1

`rust_sample.kfx` is produced from `rust_sample.rs` by a small tokenizer.
The benchmark used the generated `.kfx` file. To regenerate after editing
the source:

```
python3 bench/tools/tokenize_rust.py bench/fixtures/rust_sample.rs bench/fixtures/rust_sample.kfx
```

The tokenizer is a coarse regex approximation, not a syntax highlighter.
