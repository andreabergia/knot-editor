//! HTTP(S) destinations in the currently displayed emulator grid.

use alacritty_terminal::{
    grid::Dimensions,
    term::{
        Term,
        cell::{Cell, Flags},
        point_to_viewport,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkTarget {
    pub(crate) uri: String,
    identity: Option<String>,
    pub(crate) cells: Vec<(usize, usize)>,
}

impl LinkTarget {
    pub(crate) fn contains(&self, column: usize, row: usize) -> bool {
        self.cells.contains(&(column, row))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkGrid {
    columns: usize,
    rows: usize,
    offset: usize,
    cells: Vec<Cell>,
    links: Vec<LinkTarget>,
}

pub(crate) fn http_destination(uri: &str) -> Option<String> {
    if uri.chars().any(char::is_control) {
        return None;
    }
    let url = url::Url::parse(uri).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    Some(url.to_string())
}

impl LinkGrid {
    pub(crate) fn capture<T: alacritty_terminal::event::EventListener>(terminal: &Term<T>) -> Self {
        let mut grid = Self {
            columns: terminal.columns(),
            rows: terminal.screen_lines(),
            offset: terminal.grid().display_offset(),
            cells: vec![Cell::default(); terminal.columns() * terminal.screen_lines()],
            links: Vec::new(),
        };
        for indexed in terminal.renderable_content().display_iter {
            if let Some(point) = point_to_viewport(grid.offset, indexed.point) {
                grid.cells[point.line * grid.columns + point.column.0] = indexed.cell.clone();
            }
        }
        grid.find_links();
        grid
    }

    pub(crate) fn links_contain(&self, target: &LinkTarget) -> bool {
        self.links.contains(target)
    }

    pub(crate) fn at(&self, column: usize, row: usize) -> Option<&LinkTarget> {
        self.links.iter().find(|link| link.contains(column, row))
    }

    fn find_links(&mut self) {
        for (index, cell) in self.cells.iter().enumerate() {
            let Some(link) = cell.hyperlink() else {
                continue;
            };
            let Some(uri) = http_destination(link.uri()) else {
                continue;
            };
            let identity = Some(link.id().to_owned());
            let coordinate = (index % self.columns, index / self.columns);
            if let Some(target) = self
                .links
                .iter_mut()
                .find(|target| target.uri == uri && target.identity == identity)
            {
                target.cells.push(coordinate);
            } else {
                self.links.push(LinkTarget {
                    uri,
                    identity,
                    cells: vec![coordinate],
                });
            }
        }
        let mut text = String::new();
        let mut coordinates = Vec::new();
        for row in 0..self.rows {
            for column in 0..self.columns {
                let cell = &self.cells[row * self.columns + column];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let start = text.len();
                // Explicit destinations, including unsupported schemes, are authoritative.
                if cell.hyperlink().is_some() || cell.flags.contains(Flags::HIDDEN) {
                    text.push(' ');
                } else {
                    text.push(cell.c);
                    if let Some(zero) = cell.zerowidth() {
                        text.extend(zero);
                    }
                }
                coordinates.push((start..text.len(), (column, row)));
                if cell.flags.contains(Flags::WIDE_CHAR) && column + 1 < self.columns {
                    coordinates.push((start..text.len(), (column + 1, row)));
                }
            }
            let wrapped = self.cells[(row + 1) * self.columns - 1]
                .flags
                .contains(Flags::WRAPLINE);
            if !wrapped || row + 1 == self.rows {
                self.detect(&text, &coordinates, wrapped);
                text.clear();
                coordinates.clear();
            }
        }
    }

    fn detect(
        &mut self,
        text: &str,
        coordinates: &[(std::ops::Range<usize>, (usize, usize))],
        clipped: bool,
    ) {
        let bytes = text.as_bytes();
        for (start, _) in text.char_indices() {
            let tail = &text[start..];
            if !(tail
                .get(..7)
                .is_some_and(|s| s.eq_ignore_ascii_case("http://"))
                || tail
                    .get(..8)
                    .is_some_and(|s| s.eq_ignore_ascii_case("https://")))
            {
                continue;
            }
            if start > 0
                && text[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                continue;
            }
            let length = tail
                .find(|c: char| {
                    c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '"' | '\'' | '`')
                })
                .unwrap_or(tail.len());
            let mut end = start + length;
            if clipped && end == text.len() {
                continue;
            }
            loop {
                if end <= start {
                    break;
                }
                let last = bytes[end - 1] as char;
                let unmatched = match last {
                    ')' => Some(('(', ')')),
                    ']' => Some(('[', ']')),
                    '}' => Some(('{', '}')),
                    _ => None,
                }
                .is_some_and(|(open, close)| {
                    text[start..end].matches(close).count() > text[start..end].matches(open).count()
                });
                if ".,;:!?".contains(last) || unmatched {
                    end -= 1;
                } else {
                    break;
                }
            }
            let Some(uri) = http_destination(&text[start..end]) else {
                continue;
            };
            let cells = coordinates
                .iter()
                .filter(|(range, _)| range.start < end && range.end > start)
                .map(|(_, point)| *point)
                .collect();
            self.links.push(LinkTarget {
                uri,
                identity: None,
                cells,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::terminal::session::TerminalSize;
    use alacritty_terminal::{event::VoidListener, term::Config, vte::ansi::Processor};

    fn grid(columns: usize, rows: usize, bytes: &[u8]) -> LinkGrid {
        let size = TerminalSize {
            columns,
            lines: rows,
            cell_width: 8,
            cell_height: 16,
        };
        let mut terminal = Term::new(Config::default(), &size, VoidListener);
        {
            let mut parser: Processor = Processor::new();
            parser.advance(&mut terminal, bytes);
        }
        LinkGrid::capture(&terminal)
    }

    #[test]
    fn explicit_identity_and_destination_override_visible_label() {
        let grid = grid(
            80,
            3,
            b"\x1b]8;id=one;https://destination.test/path\x1b\\https://label.test\x1b]8;;\x1b\\",
        );
        let link = grid.at(0, 0).unwrap();
        assert_eq!(link.uri, "https://destination.test/path");
        assert_eq!(link.identity.as_deref(), Some("one"));
        assert_eq!(grid.links.len(), 1);
    }

    #[test]
    fn unsupported_explicit_scheme_blocks_detection_and_controls_are_rejected() {
        let grid = grid(
            80,
            3,
            b"\x1b]8;;file:///tmp/a\x1b\\https://label.test\x1b]8;;\x1b\\",
        );
        assert!(grid.links.is_empty());
        for uri in [
            "file:///tmp/a",
            "javascript:alert(1)",
            "https://test/\nfoo",
            "https://",
        ] {
            assert!(http_destination(uri).is_none(), "{uri:?}");
        }
    }

    #[test]
    fn detects_soft_wraps_but_keeps_hard_lines_separate() {
        let wrapped = grid(12, 4, b"https://example.test/path");
        assert_eq!(wrapped.at(0, 1).unwrap().uri, "https://example.test/path");
        let hard = grid(40, 3, b"https://example.test/\r\nother");
        assert_eq!(hard.at(0, 0).unwrap().uri, "https://example.test/");
        assert!(hard.at(0, 1).is_none());
    }

    #[test]
    fn maps_wide_and_combining_unicode_cells() {
        let grid = grid(16, 4, "界 https://test/界e\u{301}".as_bytes());
        let link = grid.at(3, 0).unwrap();
        assert_eq!(link.uri, "https://test/%E7%95%8Ce%CC%81");
        assert!(link.contains(0, 1));
        assert!(link.contains(1, 1));
        assert!(grid.at(0, 0).is_none());
    }

    #[test]
    fn trims_sentence_punctuation_and_unbalanced_delimiters() {
        let grid = grid(
            100,
            3,
            b"(https://test/a(b)). https://[::1]/x], HTTPS://test/end! wordhttps://ignored.test",
        );
        assert_eq!(
            grid.links
                .iter()
                .map(|link| link.uri.as_str())
                .collect::<Vec<_>>(),
            ["https://test/a(b)", "https://[::1]/x", "https://test/end"]
        );
    }

    #[test]
    fn refuses_detected_destination_clipped_below_viewport() {
        let grid = grid(12, 2, b"https://example.test/long/path");
        // Scrolling removed the prefix; no partial destination is actionable.
        assert!(grid.links.is_empty());
    }

    #[test]
    fn scrolled_view_rejects_cut_off_detected_url_but_keeps_explicit_destination() {
        let size = TerminalSize {
            columns: 12,
            lines: 2,
            cell_width: 8,
            cell_height: 16,
        };
        let mut terminal = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut terminal, b"https://example.test/long/path/continued");
        terminal.scroll_display(alacritty_terminal::grid::Scroll::Top);
        assert!(LinkGrid::capture(&terminal).links.is_empty());
        let mut terminal = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(
            &mut terminal,
            b"\x1b]8;;https://example.test/full\x1b\\long visible label continuing below viewport",
        );
        terminal.scroll_display(alacritty_terminal::grid::Scroll::Top);
        assert_eq!(
            LinkGrid::capture(&terminal).at(0, 0).unwrap().uri,
            "https://example.test/full"
        );
    }

    #[test]
    fn displayed_scrollback_and_identity_changes_are_distinct() {
        let size = TerminalSize {
            columns: 40,
            lines: 2,
            cell_width: 8,
            cell_height: 16,
        };
        let mut terminal = Term::new(Config::default(), &size, VoidListener);
        {
            let mut parser: Processor = Processor::new();
            parser.advance(&mut terminal, b"https://history.test\r\nnext\r\nlast");
        }
        assert!(LinkGrid::capture(&terminal).links.is_empty());
        terminal.scroll_display(alacritty_terminal::grid::Scroll::Top);
        assert_eq!(
            LinkGrid::capture(&terminal).at(0, 0).unwrap().uri,
            "https://history.test/"
        );
        assert_ne!(
            grid(40, 2, b"\x1b]8;id=a;https://test\x1b\\label"),
            grid(40, 2, b"\x1b]8;id=b;https://test\x1b\\label")
        );
    }
}
