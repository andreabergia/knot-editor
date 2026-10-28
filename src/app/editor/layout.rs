//! Line shaping and the horizontal geometry shared by caret, selection,
//! hit testing, and painting.

use super::*;

impl EditorView {
    pub(super) fn caret_x(&self, model: &BufferModel, window: &Window) -> Pixels {
        let text = line_text(model, self.cursor_line);
        let shaped =
            shape_editor_line(window, text.clone().into(), px(FONT_SIZE), &runs_for(&text));
        x_for_index_dir(&shaped, self.cursor_col, &text)
            + self.line_x_offset(&text, shaped.width, self.bounds.size.width)
            + px(self.scroll_x)
    }

    pub(super) fn column_for_x(
        &self,
        model: &BufferModel,
        line: usize,
        x: Pixels,
        window: &Window,
    ) -> usize {
        let text = line_text(model, line);
        let shaped =
            shape_editor_line(window, text.clone().into(), px(FONT_SIZE), &runs_for(&text));
        let offset =
            self.line_x_offset(&text, shaped.width, self.bounds.size.width) + px(self.scroll_x);
        text.grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .min_by(|a, b| {
                f32::from((x_for_index_dir(&shaped, *a, &text) + offset - x).abs()).total_cmp(
                    &f32::from((x_for_index_dir(&shaped, *b, &text) + offset - x).abs()),
                )
            })
            .unwrap_or(0)
    }

    /// Per-line horizontal offset for RTL lines: right-align the line within
    /// the pane so a pure-Arabic line starts at the right edge instead of the
    /// left. Returns 0 for LTR lines, `(pane_w - shaped_w).max(0)` for RTL.
    /// The caret, selection, IME preedit, decoration, and hit-test all add
    /// this offset to their x computations so they stay consistent with the
    /// right-aligned text.
    pub(super) fn line_x_offset(&self, text: &str, shaped_w: Pixels, pane_w: Pixels) -> Pixels {
        (if is_rtl_line(text) {
            (pane_w - shaped_w).max(px(0.))
        } else {
            px(0.)
        }) - px(self.scroll_x)
    }
}
/// Four-column tab stops are a rendering projection; source byte offsets stay intact.
pub(super) fn expand_tabs(text: &str) -> String {
    let mut out = String::new();
    let mut column = 0;
    for grapheme in text.graphemes(true) {
        if grapheme == "\t" {
            let count = 4 - column % 4;
            out.extend(std::iter::repeat_n(' ', count));
            column += count;
        } else {
            out.push_str(grapheme);
            column += 1;
        }
    }
    out
}

pub(super) fn shape_editor_line(
    window: &Window,
    text: SharedString,
    font_size: Pixels,
    runs: &[TextRun],
) -> ShapedLine {
    if !text.contains('\t') {
        return window.text_system().shape_line(text, font_size, runs, None);
    }
    let mut offset = 0;
    let projected_runs = runs
        .iter()
        .map(|run| {
            let end = offset + run.len;
            let mut projected = run.clone();
            projected.len = expand_tabs(&text[..end]).len() - expand_tabs(&text[..offset]).len();
            offset = end;
            projected
        })
        .collect::<Vec<_>>();
    window
        .text_system()
        .shape_line(expand_tabs(&text).into(), font_size, &projected_runs, None)
}

/// One default-styled run covering the whole line.
pub(super) fn runs_for(line: &str) -> Vec<TextRun> {
    if line.is_empty() {
        return Vec::new();
    }
    vec![TextRun {
        len: line.len(),
        font: make_font(false, false),
        color: rgb(DEFAULT_COLOR).into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }]
}

/// Heuristic: is this line's base direction right-to-left? Scans for the
/// first strong directional character (a letter) and checks whether it
/// falls in an RTL Unicode block (Arabic or Hebrew). Lines starting with
/// LTR content (code, comments, markdown) return false even if they
/// contain embedded RTL runs — the base direction stays LTR.
pub(super) fn is_rtl_line(s: &str) -> bool {
    for ch in s.chars() {
        if !ch.is_alphabetic() {
            continue;
        }
        return char_is_strong_rtl(ch);
    }
    false
}

/// Is this character a strong RTL character (Arabic/Hebrew block)?
pub(super) fn char_is_strong_rtl(ch: char) -> bool {
    let c = ch as u32;
    (0x0590..=0x05FF).contains(&c)   // Hebrew
        || (0x0600..=0x06FF).contains(&c)      // Arabic
        || (0x0700..=0x074F).contains(&c)      // Syriac
        || (0x0750..=0x077F).contains(&c)      // Arabic Supplement
        || (0x08A0..=0x08FF).contains(&c)      // Arabic Extended-A
        || (0xFB1D..=0xFB4F).contains(&c)      // Hebrew presentation forms
        || (0xFB50..=0xFDFF).contains(&c)      // Arabic presentation forms-A
        || (0xFE70..=0xFEFF).contains(&c) // Arabic presentation forms-B
}

/// Direction-aware `x_for_index`. gpui's `ShapedLine::x_for_index` walks
/// glyphs in visual order assuming increasing logical indices — correct for
/// pure LTR, broken for RTL where Core Text reorders glyphs so visual order
/// has DECREASING logical indices.
///
/// This function detects the direction of the character AT `index` (the char
/// after the boundary) rather than relying on per-run heuristics, because
/// gpui merges CTRuns by font — a single `ShapedRun` can contain both RTL
/// and LTR glyphs (e.g. an Arabic line with embedded English, where the
/// surrounding spaces share the same font as the English text).
///
/// For an LTR character at `index`: the caret is at the LEFT edge of that
/// character's glyph (find the glyph with `index == target`).
/// For an RTL character at `index`: the caret is at the LEFT edge of the
/// glyph with the LARGEST index < `index` (the char before the boundary
/// in logical order, which sits to the RIGHT in visual order).
///
/// Special cases: `index == 0` returns `s.width` for RTL-base lines (right
/// edge) or `px(0.)` for LTR; `index >= s.len` is the mirror.
///
/// Uses only public fields: `LineLayout.runs`, `ShapedRun.glyphs`,
/// `ShapedGlyph.index`, `ShapedGlyph.position`, `LineLayout.width`,
/// `LineLayout.len`.
pub(super) fn x_for_index_dir(s: &ShapedLine, index: usize, line_str: &str) -> Pixels {
    let expanded;
    let (index, line_str) = if line_str.contains('\t') {
        let mapped = expand_tabs(&line_str[..index]).len();
        expanded = expand_tabs(line_str);
        (mapped, expanded.as_str())
    } else {
        (index, line_str)
    };
    let rtl_base = is_rtl_line(line_str);
    if index == 0 {
        return if rtl_base { s.width } else { px(0.) };
    }
    if index >= s.len {
        return if rtl_base { px(0.) } else { s.width };
    }

    let rtl = line_str[index..]
        .chars()
        .next()
        .map(char_is_strong_rtl)
        .unwrap_or(rtl_base);

    let all_glyphs: Vec<&ShapedGlyph> = s.runs.iter().flat_map(|r| r.glyphs.iter()).collect();

    if !rtl {
        // A grapheme boundary can fall after a single glyph representing an
        // entire emoji/ZWJ cluster. In that case there is no glyph *at* the
        // boundary: use the next glyph's left edge, or the line's right edge
        // at end-of-line. Falling back to the preceding glyph's position
        // would paint the caret on the emoji's left edge.
        return all_glyphs
            .iter()
            .filter(|g| g.index >= index)
            .min_by_key(|g| g.index)
            .map_or(s.width, |g| g.position.x);
    }

    let mut best: Option<Pixels> = None;
    let mut best_idx: i64 = -1;
    for g in &all_glyphs {
        if (g.index as i64) < (index as i64) && (g.index as i64) > best_idx {
            best_idx = g.index as i64;
            best = Some(g.position.x);
        }
    }
    best.unwrap_or(if rtl_base { px(0.) } else { s.width })
}
