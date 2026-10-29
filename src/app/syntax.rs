//! Native syntax highlighting: compiled-in grammars, per-buffer incremental
//! parse state, and visible-range highlight queries.
//!
//! A `SyntaxLanguage` pairs a grammar with its compiled highlight query. A
//! `SyntaxState` keeps one tree per buffer and edits it with every committed
//! primitive edit, so node positions always describe the current text even
//! before a reparse. After a commit the foreground reparses within a small
//! budget; a parse that runs out of budget continues as a `BackgroundParse`
//! over a `TextSnapshot` while highlights keep coming from the edited
//! previous tree. A background result is rebased over the edits committed
//! while it ran, and a result for a replaced state is rejected, so a tree is
//! never applied to text it did not see.
//!
//! Highlights follow nvim-treesitter's precedence, since its queries are the
//! ones Knot adapts: captures paint in query-cursor order, so a later pattern
//! overrides an earlier one on the same node and an inner node overrides its
//! ancestors, and a pattern's `(#set! priority N)` (default 100) orders it
//! before or after every pattern of another priority.

pub(crate) mod grammars;

use std::{
    ops::{ControlFlow, Range},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tree_sitter::{
    InputEdit, Language, Node, ParseOptions, Parser, Point, Query, QueryCursor, StreamingIterator,
    Tree,
};

use crate::core::buffer::{BufferEdit, TextBuffer, TextPoint, TextSnapshot};

use super::theme::Highlight;

/// How long a commit may spend reparsing on the foreground before the parse
/// continues in the background.
pub(crate) const FOREGROUND_BUDGET: Duration = Duration::from_millis(1);

/// Priority of a pattern without a `priority` property, as in Neovim.
const DEFAULT_PRIORITY: u32 = 100;

/// Issues the identity that matches a background result to the parse that
/// started it.
static NEXT_PARSE: AtomicU64 = AtomicU64::new(1);

/// A grammar and the queries that highlight it.
pub(crate) struct SyntaxLanguage {
    grammar: Language,
    highlights: Query,
    /// The highlight each capture resolves to, indexed by capture index.
    capture_highlights: Vec<Option<Highlight>>,
    /// Indexed by pattern index.
    pattern_priorities: Vec<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SyntaxLanguageError {
    UnknownGrammar,
    /// The query failed to compile at 0-indexed `row` and `column`.
    InvalidQuery {
        message: String,
        row: usize,
        column: usize,
    },
}

impl SyntaxLanguage {
    /// Compile `highlights` against the grammar named `grammar`.
    pub(crate) fn new(grammar: &str, highlights: &str) -> Result<Self, SyntaxLanguageError> {
        let grammar = grammars::grammar(grammar).ok_or(SyntaxLanguageError::UnknownGrammar)?;
        let highlights = Query::new(&grammar, highlights).map_err(|error| {
            SyntaxLanguageError::InvalidQuery {
                message: error.message,
                row: error.row,
                column: error.column,
            }
        })?;
        let capture_highlights = highlights
            .capture_names()
            .iter()
            .map(|name| Highlight::resolve(name))
            .collect();
        let pattern_priorities = (0..highlights.pattern_count())
            .map(|pattern| {
                highlights
                    .property_settings(pattern)
                    .iter()
                    .find(|property| &*property.key == "priority")
                    .and_then(|property| property.value.as_deref()?.parse().ok())
                    .unwrap_or(DEFAULT_PRIORITY)
            })
            .collect();
        Ok(Self {
            grammar,
            highlights,
            capture_highlights,
            pattern_priorities,
        })
    }
}

/// One buffer's syntax tree and the parse that keeps it current.
pub(crate) struct SyntaxState {
    language: Arc<SyntaxLanguage>,
    parser: Parser,
    /// The latest tree, edited forward to the current text.
    tree: Option<Tree>,
    /// Whether `tree` is a parse of the current text rather than an edited
    /// parse of older text.
    current: bool,
    /// Changes whenever the highlights may have changed.
    generation: u64,
    /// The running background parse and the edits committed since it took
    /// its snapshot.
    background: Option<(u64, Vec<InputEdit>)>,
}

/// A parse to run off the foreground. `Send`, and independent of the state
/// that started it.
pub(crate) struct BackgroundParse {
    id: u64,
    language: Arc<SyntaxLanguage>,
    tree: Option<Tree>,
    text: TextSnapshot,
}

/// The result of a `BackgroundParse`.
pub(crate) struct ParsedTree {
    id: u64,
    tree: Tree,
}

impl SyntaxState {
    /// A state with no tree yet; the first parse builds one.
    pub(crate) fn new(language: Arc<SyntaxLanguage>) -> Self {
        let mut parser = Parser::new();
        parser
            .set_language(&language.grammar)
            .expect("compiled-in grammars match the tree-sitter ABI");
        Self {
            language,
            parser,
            tree: None,
            current: false,
            generation: 0,
            background: None,
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the tree lags the text and no background parse is running.
    pub(crate) fn needs_background_parse(&self) -> bool {
        !self.current && self.background.is_none()
    }

    /// Apply committed primitive edits, in log order, to the tree and to the
    /// running background parse's pending edits.
    pub(crate) fn edit(&mut self, edits: &[BufferEdit]) {
        for edit in edits {
            let edit = input_edit(edit);
            if let Some(tree) = &mut self.tree {
                tree.edit(&edit);
            }
            if let Some((_, pending)) = &mut self.background {
                pending.push(edit);
            }
        }
        if !edits.is_empty() {
            self.current = false;
            self.generation += 1;
        }
    }

    /// Reparse `text`, the current text, within `budget`. Returns whether the
    /// tree is now current; a running background parse defers reparsing to
    /// its rebase.
    pub(crate) fn parse_in_foreground(&mut self, text: &TextSnapshot, budget: Duration) -> bool {
        if self.current {
            return true;
        }
        if self.background.is_some() {
            return false;
        }
        let deadline = Instant::now() + budget;
        let mut progress = |_: &_| {
            if Instant::now() >= deadline {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let tree = self.parser.parse_with_options(
            &mut |offset, _| text.chunk_at(offset),
            self.tree.as_ref(),
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        match tree {
            Some(tree) => {
                self.accept(tree, true);
                true
            }
            None => {
                // A cancelled parse otherwise resumes on the next call.
                self.parser.reset();
                false
            }
        }
    }

    /// Start parsing `text`, the current text, off the foreground.
    pub(crate) fn start_background_parse(&mut self, text: TextSnapshot) -> BackgroundParse {
        debug_assert!(self.background.is_none(), "one background parse at a time");
        let id = NEXT_PARSE.fetch_add(1, Ordering::Relaxed);
        self.background = Some((id, Vec::new()));
        BackgroundParse {
            id,
            language: Arc::clone(&self.language),
            tree: self.tree.clone(),
            text,
        }
    }

    /// Accept a background result, rebased over the edits committed since its
    /// snapshot. Returns false, changing nothing, for a result this state did
    /// not start.
    pub(crate) fn finish_background_parse(&mut self, parsed: ParsedTree) -> bool {
        let Some((_, edits)) = self.background.take_if(|(id, _)| *id == parsed.id) else {
            return false;
        };
        let mut tree = parsed.tree;
        for edit in &edits {
            tree.edit(edit);
        }
        self.accept(tree, edits.is_empty());
        true
    }

    fn accept(&mut self, tree: Tree, current: bool) {
        self.tree = Some(tree);
        self.current = current;
        self.generation += 1;
    }

    /// Sorted, non-overlapping highlights of `text`, the current text, within
    /// `range`.
    pub(crate) fn highlights(
        &self,
        text: &TextBuffer,
        range: Range<usize>,
    ) -> Vec<(Range<usize>, Highlight)> {
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let language = &*self.language;
        let mut cursor = QueryCursor::new();
        cursor.set_byte_range(range.clone());
        let node_text = |node: Node| {
            let range = node.byte_range();
            text.chunks(range.start.min(text.len())..range.end.min(text.len()))
                .map(str::as_bytes)
        };
        let mut captures = cursor.captures(&language.highlights, tree.root_node(), node_text);
        let mut painted = Vec::new();
        while let Some((found, index)) = captures.next() {
            let capture = found.captures()[*index];
            let Some(highlight) = language.capture_highlights[capture.index as usize] else {
                continue;
            };
            let node = capture.node.byte_range();
            let clipped = node.start.max(range.start)..node.end.min(range.end);
            if !clipped.is_empty() {
                let priority = language.pattern_priorities[found.pattern_index];
                painted.push((priority, clipped, highlight));
            }
        }
        // Stable, so equal priorities keep cursor order.
        painted.sort_by_key(|(priority, ..)| *priority);
        let mut spans = Vec::new();
        for (_, range, highlight) in painted {
            paint(&mut spans, range, highlight);
        }
        spans
    }
}

impl BackgroundParse {
    pub(crate) fn run(self) -> ParsedTree {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language.grammar)
            .expect("compiled-in grammars match the tree-sitter ABI");
        let tree = parser
            .parse_with_options(
                &mut |offset, _| self.text.chunk_at(offset),
                self.tree.as_ref(),
                None,
            )
            .expect("an uncancellable parse with a language completes");
        ParsedTree { id: self.id, tree }
    }
}

/// Paint `highlight` over `range`, replacing whatever `spans` had there.
fn paint(spans: &mut Vec<(Range<usize>, Highlight)>, range: Range<usize>, highlight: Highlight) {
    let first = spans.partition_point(|(span, _)| span.end <= range.start);
    let last = spans.partition_point(|(span, _)| span.start < range.end);
    let mut replacement = Vec::with_capacity(3);
    if first < last {
        let (head, head_highlight) = &spans[first];
        if head.start < range.start {
            replacement.push((head.start..range.start, *head_highlight));
        }
    }
    let tail = (first < last)
        .then(|| &spans[last - 1])
        .filter(|(tail, _)| tail.end > range.end)
        .map(|(tail, tail_highlight)| (range.end..tail.end, *tail_highlight));
    replacement.push((range, highlight));
    replacement.extend(tail);
    spans.splice(first..last, replacement);
}

fn input_edit(edit: &BufferEdit) -> InputEdit {
    let point = |point: TextPoint| Point::new(point.line, point.column);
    match edit {
        BufferEdit::Insert {
            at,
            inserted_len,
            at_point,
            end_point,
            ..
        } => InputEdit {
            start_byte: *at,
            old_end_byte: *at,
            new_end_byte: at + inserted_len,
            start_position: point(*at_point),
            old_end_position: point(*at_point),
            new_end_position: point(*end_point),
        },
        BufferEdit::Delete {
            range,
            start_point,
            end_point,
            ..
        } => InputEdit {
            start_byte: range.start,
            old_end_byte: range.end,
            new_end_byte: range.start,
            start_position: point(*start_point),
            old_end_position: point(*end_point),
            new_end_position: point(*start_point),
        },
    }
}

#[cfg(test)]
mod tests;
