//! Line shaping and the horizontal geometry shared by caret, selection,
//! hit testing, and painting.

use super::*;

impl EditorView {
    /// Text runs for `line`, whose content is `text`, from the view's
    /// highlight layers.
    pub(super) fn line_runs(&self, line: usize, text: &str, cx: &App) -> Vec<TextRun> {
        let decorations = self.decoration_layers(line);
        let layers = decorations.each_ref().map(Vec::as_slice);
        runs_for(text, &layers, cx.global::<Theme>())
    }

    /// Shape `line`, whose content is `text`, with its highlight runs.
    pub(super) fn shape_line(
        &self,
        line: usize,
        text: &str,
        window: &Window,
        cx: &App,
    ) -> ShapedLine {
        let runs = self.line_runs(line, text, cx);
        shape_editor_line(window, text.to_owned().into(), px(FONT_SIZE), &runs)
    }

    /// The decorations on `line`, one layer per severity in increasing
    /// order so the most severe underline wins where they overlap.
    fn decoration_layers(&self, line: usize) -> [Vec<HighlightSpan>; 3] {
        let severities = [
            DecorationToken::Info,
            DecorationToken::Warning,
            DecorationToken::Error,
        ];
        severities.map(|token| {
            let mut ranges: Vec<Range<usize>> = self
                .decorations
                .iter()
                .filter(|decoration| decoration.line == line && decoration.token == token)
                .map(|decoration| decoration.start..decoration.end)
                .collect();
            ranges.sort_unstable_by_key(|range| range.start);
            let mut merged: Vec<Range<usize>> = Vec::new();
            for range in ranges {
                match merged.last_mut() {
                    Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                    _ => merged.push(range),
                }
            }
            let highlight = decoration_highlight(token);
            merged
                .into_iter()
                .map(|range| HighlightSpan { range, highlight })
                .collect()
        })
    }

    pub(super) fn caret_x(&self, model: &BufferModel, window: &Window, cx: &App) -> Pixels {
        let text = line_text(model, self.cursor_line);
        let shaped = self.shape_line(self.cursor_line, &text, window, cx);
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
        cx: &App,
    ) -> usize {
        let text = line_text(model, line);
        let shaped = self.shape_line(line, &text, window, cx);
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

/// A highlight over a line-relative byte range.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct HighlightSpan {
    pub(super) range: Range<usize>,
    pub(super) highlight: Highlight,
}

/// Compose `line`'s text runs from ordered highlight layers.
///
/// Each layer's spans are sorted and non-overlapping, on char boundaries, and
/// may extend past the line, which clips them. Layers apply in order, so a
/// later layer's color and font override an earlier one's and its underline
/// joins them. Bytes no span covers use the theme's foreground.
pub(super) fn runs_for(line: &str, layers: &[&[HighlightSpan]], theme: &Theme) -> Vec<TextRun> {
    if line.is_empty() {
        return Vec::new();
    }
    let mut boundaries = vec![0, line.len()];
    for span in layers.iter().flat_map(|layer| layer.iter()) {
        boundaries.extend([
            span.range.start.min(line.len()),
            span.range.end.min(line.len()),
        ]);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut cursors = vec![0; layers.len()];
    let mut runs: Vec<(usize, HighlightStyle)> = Vec::new();
    for segment in boundaries.windows(2) {
        let (start, end) = (segment[0], segment[1]);
        let mut style = HighlightStyle::default();
        for (layer, cursor) in layers.iter().zip(&mut cursors) {
            while layer
                .get(*cursor)
                .is_some_and(|span| span.range.end <= start)
            {
                *cursor += 1;
            }
            if let Some(span) = layer.get(*cursor)
                && span.range.start <= start
            {
                style = style.highlight(theme.style(span.highlight));
            }
        }
        match runs.last_mut() {
            Some((len, last)) if *last == style => *len += end - start,
            _ => runs.push((end - start, style)),
        }
    }
    runs.into_iter()
        .map(|(len, style)| {
            let mut font = make_font(false, false);
            font.weight = style.font_weight.unwrap_or(font.weight);
            font.style = style.font_style.unwrap_or(font.style);
            TextRun {
                len,
                font,
                color: style.color.unwrap_or(theme.foreground),
                background_color: None,
                underline: style.underline,
                strikethrough: style.strikethrough,
            }
        })
        .collect()
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
