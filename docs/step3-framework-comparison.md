# UI Framework Evaluation

The framework spike asked whether Knot could build its editor and shell using
only a framework's public APIs. gpui 0.2.2 was evaluated first. It cleared the
bar, so iced and floem were not implemented.

## Exercised surface

The prototype built:

- a three-pane shell, selectable lists, and draggable dividers;
- styled, viewport-culled text;
- keyboard and pointer cursor movement and selection;
- macOS IME preedit and candidate-window geometry;
- annotation underlines;
- Arabic/BiDi, CJK, ZWJ emoji, long-line, and large Rust fixtures.

All required primitives were available through gpui's public API. The complete
widget and fixture evaluation landed in nine focused commits.

## Important findings

- gpui's macOS Core Text path shaped connected Arabic, CJK, and ZWJ sequences
  correctly when supplied with font fallbacks.
- gpui 0.2.2 lacked an explicit RTL paragraph API. `ShapedLine::paint`
  left-aligned RTL text, and `x_for_index` assumed increasing logical glyph
  indices.
- Knot recovered correct alignment and caret mapping using public line-layout,
  run, glyph-index, and glyph-position data. This passed the access-boundary
  test but exposed weak BiDi ergonomics.
- Caret movement in the spike was code-point based, not grapheme based.
- gpui had no built-in splitter; public drag events were sufficient to build
  one.

## Decision

Use gpui. It supports Knot's demanding editor path without privileged Zed
internals and inherits a mature platform text stack. The prototype stopped
before implementing iced or floem because another comparison would not reduce
the remaining architectural uncertainty.
