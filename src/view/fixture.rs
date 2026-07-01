//! Fixture loading for the renderer benchmark.
//!
//! A fixture is a text file paired with a tokenized representation giving
//! the style segments per line. The on-disk format is documented inline
//! below; it is intentionally hand-authorable and version-controlled.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::view::Segment;

/// A loaded fixture: raw text plus per-line style segments.
///
/// `lines` holds owned line text (without newline). `segments` is parallel
/// to `lines`; each inner vec holds the styled segments for that line. The
/// segments borrow from `lines`, so the struct is self-referential — we
/// work around that by exposing `lines` and `segments_of` separately and
/// handing out short-lived borrows from a borrow of the `Fixture`.
pub struct Fixture {
    /// Owned line text, one entry per line (no trailing newline).
    pub lines: Vec<String>,
    /// Per-line style table. Each entry is a list of `(byte_range, color,
    /// bold, italic)`. Byte ranges index into the matching `lines` entry.
    styles: Vec<Vec<SegSpec>>,
}

#[derive(Clone, Copy)]
struct SegSpec {
    start: usize,
    end: usize,
    color: u32,
    bold: bool,
    italic: bool,
}

impl Fixture {
    /// Load a fixture from a `.kfx` file.
    ///
    /// Format (line-oriented, UTF-8):
    /// ```text
    /// knot-fixture v1
    /// <line count N>
    /// N lines of raw text (may be empty)
    /// ---
    /// M segment specs, one per line: `<line> <start> <end> <color> <bold> <italic>`
    /// ```
    /// where `color` is a 0xRRGGBB hex literal, `bold`/`italic` are `0` or `1`.
    /// Lines with no specs are rendered in a default style.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading fixture {}", path.display()))?;

        let mut lines = raw.lines();

        let header = lines.next().context("missing fixture header")?;
        if header.trim() != "knot-fixture v1" {
            bail!("unsupported fixture header: {header:?}");
        }

        let count_line = lines.next().context("missing line count")?;
        let count: usize = count_line
            .trim()
            .parse()
            .with_context(|| format!("parsing line count from {count_line:?}"))?;

        let mut text_lines = Vec::with_capacity(count);
        for _ in 0..count {
            text_lines.push(lines.next().context("fixture truncated in text body")?.to_owned());
        }

        let mut styles = vec![Vec::new(); count];

        // The segment table is optional: fixtures whose styles are not
        // load-bearing (CJK, Arabic, emoji, minified) omit it entirely.
        // If present, it must be preceded by a `---` separator.
        if let Some(sep) = lines.next() {
            if sep.trim() != "---" {
                bail!("expected `---` separator, found {sep:?}");
            }
            for spec_line in lines {
                let s = spec_line.trim();
                if s.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = s.split_whitespace().collect();
                if parts.len() != 6 {
                    bail!("bad segment spec `{s}` (expected 6 fields)");
                }
                let line_idx: usize = parts[0].parse()?;
                let start: usize = parts[1].parse()?;
                let end: usize = parts[2].parse()?;
                let color = u32::from_str_radix(parts[3].trim_start_matches("0x"), 16)?;
                let bold = parts[4] != "0";
                let italic = parts[5] != "0";
                if line_idx >= count {
                    bail!("segment spec references line {line_idx} >= count {count}");
                }
                styles[line_idx].push(SegSpec {
                    start,
                    end,
                    color,
                    bold,
                    italic,
                });
            }
        }

        Ok(Self {
            lines: text_lines,
            styles,
        })
    }

    /// Number of lines in the fixture.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Construct a fixture from raw text with no authored style segments.
    /// Each line is rendered with the default style. Used by callers that
    /// synthesize a fixture in memory (e.g. an editor pane's load fallback).
    pub fn from_lines(lines: Vec<String>) -> Self {
        let n = lines.len();
        Fixture {
            lines,
            styles: vec![Vec::new(); n],
        }
    }

    /// Tile the fixture `n` times, producing a synthetic large fixture.
    ///
    /// Used for fixture 2 (1M-line tiled Rust): the on-disk fixture stays
    /// small, and the harness synthesizes the large workload in memory at
    /// startup. Styles are tiled alongside the text so the multi-attribute
    /// shaping path is exercised at full scale.
    pub fn tiled(&self, n: usize) -> Self {
        if n <= 1 {
            return self.clone_shallow();
        }
        let base = self.lines.len();
        let total = base * n;
        let mut lines = Vec::with_capacity(total);
        let mut styles = Vec::with_capacity(total);
        for _ in 0..n {
            for (i, line) in self.lines.iter().enumerate() {
                lines.push(line.clone());
                styles.push(self.styles[i].clone());
            }
        }
        Fixture { lines, styles }
    }

    fn clone_shallow(&self) -> Self {
        Fixture {
            lines: self.lines.clone(),
            styles: self.styles.clone(),
        }
    }

    /// Build the segment list for a single line, borrowing from `self`.
    ///
    /// Lines with no authored segments yield a single default segment
    /// covering the whole line (color 0xC0C0C0, plain) so the renderer
    /// always has something to draw.
    pub fn segments_of<'a>(&'a self, line: usize) -> Vec<Segment<'a>> {
        let text = self.lines.get(line).map(|s| s.as_str()).unwrap_or("");
        let specs = &self.styles[line.min(self.styles.len().saturating_sub(1))];
        if specs.is_empty() {
            return vec![Segment {
                text,
                color: 0xC0C0C0,
                bold: false,
                italic: false,
            }];
        }
        specs
            .iter()
            .map(|s| Segment {
                text: &text[s.start.min(text.len())..s.end.min(text.len())],
                color: s.color,
                bold: s.bold,
                italic: s.italic,
            })
            .collect()
    }
}
