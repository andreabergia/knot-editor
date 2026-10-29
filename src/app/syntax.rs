//! Native syntax highlighting: compiled-in grammars, per-buffer incremental
//! parse state, and visible-range highlight queries.
//!
//! A `SyntaxLanguage` pairs a grammar with its compiled highlight and
//! injection queries. A `SyntaxState` keeps a tree of parse layers per
//! buffer: the buffer's own language at the root, and beneath it every region
//! an injection query hands to another language, such as a Markdown code
//! fence or an HTML `<script>` element, nested as deeply as the queries
//! produce. Every committed primitive edit edits each affected layer's tree,
//! so node positions always describe the current text even before a reparse.
//! After a commit the foreground reparses within a small budget; a parse that
//! runs out of budget continues as a `BackgroundParse` over a `TextSnapshot`
//! while highlights keep coming from the edited previous layers. A background
//! result is rebased over the edits committed while it ran, and a result for a
//! replaced state is rejected, so a tree is never applied to text it did not
//! see. Dropping a state cancels its running parse.
//!
//! A reparse runs a layer's injection query again only where an edit or the
//! reparse changed that layer; injections elsewhere keep their trees. An
//! injection found again where a stale one of the same language was reuses
//! its tree, so typing inside a code fence reparses incrementally.
//!
//! Highlights follow nvim-treesitter's precedence, since its queries are the
//! ones Knot adapts: captures paint in query-cursor order, so a later pattern
//! overrides an earlier one on the same node and an inner node overrides its
//! ancestors, and a pattern's `(#set! priority N)` (default 100) orders it
//! before or after every pattern of another priority. An injected layer
//! paints over the layer that hosts it.

pub(crate) mod grammars;

use std::{
    ops::{ControlFlow, Range},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tree_sitter::{
    InputEdit, Language, Node, ParseOptions, Parser, Point, Query, QueryCursor, Range as TreeRange,
    StreamingIterator, Tree,
};

use crate::core::buffer::{BufferEdit, TextPoint, TextSnapshot};

use super::theme::Highlight;

/// How long a commit may spend reparsing on the foreground before the parse
/// continues in the background.
pub(crate) const FOREGROUND_BUDGET: Duration = Duration::from_millis(1);

/// Priority of a pattern without a `priority` property, as in Neovim.
const DEFAULT_PRIORITY: u32 = 100;

/// How deeply injections nest, so a query that injects a region into its own
/// language cannot recurse without bound.
const MAX_INJECTION_DEPTH: usize = 8;

/// Issues the identity that matches a background result to the parse that
/// started it.
static NEXT_PARSE: AtomicU64 = AtomicU64::new(1);

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// Resolves a language name as an injection query gives it. Returning the
/// same `Arc` for a name lets an injection found again reuse its tree.
pub(crate) type LanguageResolver = Arc<dyn Fn(&str) -> Option<Arc<SyntaxLanguage>> + Send + Sync>;

/// A grammar and the queries that highlight it.
pub(crate) struct SyntaxLanguage {
    grammar: Language,
    highlights: Query,
    /// The highlight each capture resolves to, indexed by capture index.
    capture_highlights: Vec<Option<Highlight>>,
    /// Indexed by pattern index.
    pattern_priorities: Vec<u32>,
    injections: Query,
    /// The `@injection.content` capture index.
    injection_content: Option<u32>,
    /// The `@injection.language` capture index.
    injection_language: Option<u32>,
    /// Indexed by injection pattern index.
    injection_patterns: Vec<InjectionPattern>,
}

struct InjectionPattern {
    /// The language a `(#set! injection.language "…")` names.
    language: Option<Box<str>>,
    /// Whether `(#set! injection.include-children)` injects the content
    /// nodes' named children too, rather than only the text between them.
    include_children: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyntaxQuery {
    Highlights,
    Injections,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SyntaxLanguageError {
    UnknownGrammar,
    /// `query` failed to compile at 0-indexed `row` and `column`.
    InvalidQuery {
        query: SyntaxQuery,
        message: String,
        row: usize,
        column: usize,
    },
}

impl SyntaxLanguage {
    /// Compile `highlights` and `injections` against the grammar named
    /// `grammar`.
    pub(crate) fn new(
        grammar: &str,
        highlights: &str,
        injections: &str,
    ) -> Result<Self, SyntaxLanguageError> {
        let grammar = grammars::grammar(grammar).ok_or(SyntaxLanguageError::UnknownGrammar)?;
        let compile = |query, source| {
            Query::new(&grammar, source).map_err(|error| SyntaxLanguageError::InvalidQuery {
                query,
                message: error.message,
                row: error.row,
                column: error.column,
            })
        };
        let highlights = compile(SyntaxQuery::Highlights, highlights)?;
        let injections = compile(SyntaxQuery::Injections, injections)?;
        let property = |query: &Query, pattern, key| {
            query
                .property_settings(pattern)
                .iter()
                .find(|property| &*property.key == key)
                .map(|property| property.value.clone())
        };
        let capture_highlights = highlights
            .capture_names()
            .iter()
            .map(|name| Highlight::resolve(name))
            .collect();
        let pattern_priorities = (0..highlights.pattern_count())
            .map(|pattern| {
                property(&highlights, pattern, "priority")
                    .flatten()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(DEFAULT_PRIORITY)
            })
            .collect();
        let injection_patterns = (0..injections.pattern_count())
            .map(|pattern| InjectionPattern {
                language: property(&injections, pattern, "injection.language").flatten(),
                include_children: property(&injections, pattern, "injection.include-children")
                    .is_some(),
            })
            .collect();
        Ok(Self {
            grammar,
            highlights,
            capture_highlights,
            pattern_priorities,
            injection_content: injections.capture_index_for_name("injection.content"),
            injection_language: injections.capture_index_for_name("injection.language"),
            injections,
            injection_patterns,
        })
    }

    /// Whether the injection query can hand regions to other languages, so
    /// its highlights depend on which languages are registered.
    pub(crate) fn can_inject(&self) -> bool {
        self.injections.pattern_count() > 0
    }
}

/// One buffer's parse layers and the parse that keeps them current.
pub(crate) struct SyntaxState {
    /// The buffer's own language, edited forward to the current text.
    root: Layer,
    resolver: LanguageResolver,
    parser: Parser,
    /// Ranges of the current text that edits touched since `root` was last a
    /// parse of the text, where injections must be found again.
    edited: Vec<Range<usize>>,
    /// Whether `root` is a parse of the current text rather than an edited
    /// parse of older text.
    current: bool,
    /// Changes whenever the highlights may have changed, and is never shared
    /// with another state, so a view's cache cannot outlive a replacement.
    generation: u64,
    background: Option<RunningParse>,
}

/// A parse tree over the whole text or over the ranges an injection hands to
/// another language, and the layers injected into it.
#[derive(Clone)]
struct Layer {
    language: Arc<SyntaxLanguage>,
    /// The span of the nodes the injection matched, edited forward; an edit
    /// touching it finds the injection again. Unused for the root.
    host: Range<usize>,
    /// `None` until the first parse.
    tree: Option<Tree>,
    injections: Vec<Layer>,
}

/// An injection a query found, before its layer is parsed.
struct FoundInjection {
    language: Arc<SyntaxLanguage>,
    host: Range<usize>,
    ranges: Vec<TreeRange>,
}

/// What every layer of one parse shares.
struct LayerParse<'a> {
    parser: &'a mut Parser,
    text: &'a TextSnapshot,
    resolver: &'a LanguageResolver,
    stop: Stop<'a>,
}

/// When a parse gives up.
enum Stop<'a> {
    At(Instant),
    When(&'a AtomicBool),
}

impl Stop<'_> {
    fn reached(&self) -> bool {
        match self {
            Self::At(deadline) => Instant::now() >= *deadline,
            Self::When(cancelled) => cancelled.load(Ordering::Relaxed),
        }
    }
}

/// A started `BackgroundParse`, as its state tracks it.
struct RunningParse {
    id: u64,
    cancelled: Arc<AtomicBool>,
    /// The edits committed since the parse took its snapshot.
    pending: Vec<InputEdit>,
}

/// A parse to run off the foreground. `Send`, and independent of the state
/// that started it except for cancellation when that state is dropped.
pub(crate) struct BackgroundParse {
    id: u64,
    root: Layer,
    edited: Vec<Range<usize>>,
    resolver: LanguageResolver,
    text: TextSnapshot,
    cancelled: Arc<AtomicBool>,
}

/// The result of a `BackgroundParse`.
pub(crate) struct ParsedTree {
    id: u64,
    root: Layer,
}

impl SyntaxState {
    /// A state with no tree yet; the first parse builds one. Injections name
    /// their languages through `resolver`.
    pub(crate) fn new(language: Arc<SyntaxLanguage>, resolver: LanguageResolver) -> Self {
        Self {
            root: Layer::new(language, 0..usize::MAX),
            resolver,
            parser: Parser::new(),
            edited: Vec::new(),
            current: false,
            generation: next_generation(),
            background: None,
        }
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// The buffer's own language.
    pub(crate) fn language(&self) -> &Arc<SyntaxLanguage> {
        &self.root.language
    }

    /// Whether the tree lags the text and no background parse is running,
    /// so a parse would make progress.
    pub(crate) fn needs_parse(&self) -> bool {
        !self.current && self.background.is_none()
    }

    /// Apply committed primitive edits, in log order, to the layers and to
    /// the running background parse's pending edits.
    pub(crate) fn edit(&mut self, edits: &[BufferEdit]) {
        for edit in edits {
            let edit = input_edit(edit);
            self.root.edit(&edit);
            record_edit(&mut self.edited, &edit);
            if let Some(running) = &mut self.background {
                running.pending.push(edit);
            }
        }
        if !edits.is_empty() {
            self.current = false;
            self.generation = next_generation();
        }
    }

    /// Reparse `text`, the current text, within `budget`. Returns whether the
    /// layers are now current; a running background parse defers reparsing
    /// to its rebase.
    pub(crate) fn parse_in_foreground(&mut self, text: &TextSnapshot, budget: Duration) -> bool {
        if self.current {
            return true;
        }
        if self.background.is_some() {
            return false;
        }
        // A parse that runs out of budget leaves its layers half updated, so
        // it works on a copy.
        let mut root = self.root.clone();
        let mut parse = LayerParse {
            parser: &mut self.parser,
            text,
            resolver: &self.resolver,
            stop: Stop::At(Instant::now() + budget),
        };
        if root.parse(&[], &self.edited, 0, &mut parse).is_none() {
            return false;
        }
        self.edited.clear();
        self.accept(root, true);
        true
    }

    /// Start parsing `text`, the current text, off the foreground.
    pub(crate) fn start_background_parse(&mut self, text: TextSnapshot) -> BackgroundParse {
        debug_assert!(self.background.is_none(), "one background parse at a time");
        let id = NEXT_PARSE.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.background = Some(RunningParse {
            id,
            cancelled: Arc::clone(&cancelled),
            pending: Vec::new(),
        });
        BackgroundParse {
            id,
            root: self.root.clone(),
            // From now on `edited` collects only the edits the result is
            // rebased over.
            edited: std::mem::take(&mut self.edited),
            resolver: Arc::clone(&self.resolver),
            text,
            cancelled,
        }
    }

    /// Accept a background result, rebased over the edits committed since its
    /// snapshot. Returns false, changing nothing, for a result this state did
    /// not start.
    pub(crate) fn finish_background_parse(&mut self, parsed: ParsedTree) -> bool {
        let Some(running) = self.background.take_if(|running| running.id == parsed.id) else {
            return false;
        };
        let mut root = parsed.root;
        for edit in &running.pending {
            root.edit(edit);
        }
        self.accept(root, running.pending.is_empty());
        true
    }

    fn accept(&mut self, root: Layer, current: bool) {
        self.root = root;
        self.current = current;
        self.generation = next_generation();
    }

    /// Sorted, non-overlapping highlights of `text`, the current text, within
    /// `range`.
    pub(crate) fn highlights(
        &self,
        text: &TextSnapshot,
        range: Range<usize>,
    ) -> Vec<(Range<usize>, Highlight)> {
        let mut spans = Vec::new();
        self.root
            .paint_highlights(text, &range, &mut QueryCursor::new(), &mut spans);
        spans
    }
}

impl Drop for SyntaxState {
    fn drop(&mut self) {
        if let Some(running) = &self.background {
            running.cancelled.store(true, Ordering::Relaxed);
        }
    }
}

impl BackgroundParse {
    /// Parse to completion, or `None` once the state that started this parse
    /// has been dropped.
    pub(crate) fn run(self) -> Option<ParsedTree> {
        let Self {
            id,
            mut root,
            edited,
            resolver,
            text,
            cancelled,
        } = self;
        let mut parser = Parser::new();
        let mut parse = LayerParse {
            parser: &mut parser,
            text: &text,
            resolver: &resolver,
            stop: Stop::When(&cancelled),
        };
        root.parse(&[], &edited, 0, &mut parse)?;
        Some(ParsedTree { id, root })
    }
}

impl Layer {
    fn new(language: Arc<SyntaxLanguage>, host: Range<usize>) -> Self {
        Self {
            language,
            host,
            tree: None,
            injections: Vec::new(),
        }
    }

    /// Apply a committed edit to this layer's tree and to the injections that
    /// do not end before it.
    fn edit(&mut self, edit: &InputEdit) {
        if let Some(tree) = &mut self.tree {
            tree.edit(edit);
        }
        for injection in &mut self.injections {
            if injection.host.end >= edit.start_byte {
                injection.host = edit_range(&injection.host, edit);
                injection.edit(edit);
            }
        }
    }

    /// Reparse this layer over `ranges`, or the whole text when empty, then
    /// find its injections again where `edited` or the reparse changed it
    /// and parse those. `None` when the parse stops early, leaving the layer
    /// partly updated.
    fn parse(
        &mut self,
        ranges: &[TreeRange],
        edited: &[Range<usize>],
        depth: usize,
        parse: &mut LayerParse,
    ) -> Option<()> {
        let LayerParse {
            parser, text, stop, ..
        } = parse;
        parser
            .set_language(&self.language.grammar)
            .expect("compiled-in grammars match the tree-sitter ABI");
        if parser.set_included_ranges(ranges).is_err() {
            // Ranges from overlapping content nodes; leave the layer unparsed.
            return Some(());
        }
        let mut progress = |_: &_| {
            if stop.reached() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let Some(tree) = parser.parse_with_options(
            &mut |offset, _| text.chunk_at(offset),
            self.tree.as_ref(),
            Some(ParseOptions::new().progress_callback(&mut progress)),
        ) else {
            // A cancelled parse otherwise resumes on the next call.
            parser.reset();
            return None;
        };
        let old = self.tree.replace(tree);
        // Comparing trees costs as much as a small reparse, so only a layer
        // that can inject does it.
        if !self.language.can_inject() || depth == MAX_INJECTION_DEPTH {
            return Some(());
        }
        let changed = match old {
            Some(old) => old
                .changed_ranges(self.tree.as_ref()?)
                .map(|range| range.start_byte..range.end_byte)
                .chain(edited.iter().cloned())
                .collect(),
            None => std::iter::once(0..text.len()).collect(),
        };
        self.reinject(&merge(changed), depth, parse)
    }

    /// Replace the injections touching `changed` with those the injection
    /// query finds there now, and parse them.
    fn reinject(
        &mut self,
        changed: &[Range<usize>],
        depth: usize,
        parse: &mut LayerParse,
    ) -> Option<()> {
        let (mut stale, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.injections)
            .into_iter()
            .partition(|injection| changed.iter().any(|range| touches(&injection.host, range)));
        self.injections = kept;
        for found in self.find_injections(changed, parse.text, parse.resolver) {
            let same = |layer: &Layer| Arc::ptr_eq(&layer.language, &found.language);
            if self
                .injections
                .iter()
                .any(|layer| same(layer) && layer.host == found.host)
            {
                continue;
            }
            let mut layer = stale
                .iter()
                .position(|layer| same(layer) && touches(&layer.host, &found.host))
                .map(|index| stale.swap_remove(index))
                .unwrap_or_else(|| Layer::new(Arc::clone(&found.language), 0..0));
            layer.host = found.host;
            layer.parse(&found.ranges, changed, depth + 1, parse)?;
            self.injections.push(layer);
        }
        Some(())
    }

    /// The injections whose matched nodes touch `changed`, resolved and
    /// clipped to this layer's ranges.
    fn find_injections(
        &self,
        changed: &[Range<usize>],
        text: &TextSnapshot,
        resolver: &LanguageResolver,
    ) -> Vec<FoundInjection> {
        let language = &*self.language;
        let Some(tree) = &self.tree else {
            return Vec::new();
        };
        let parent_ranges = tree.included_ranges();
        let mut cursor = QueryCursor::new();
        let mut found: Vec<FoundInjection> = Vec::new();
        for range in changed {
            cursor.set_byte_range(range.start.saturating_sub(1)..range.end.saturating_add(1));
            let node_text = |node: Node| {
                text.chunks(clamp(node.byte_range(), text))
                    .map(str::as_bytes)
            };
            let mut matches = cursor.matches(&language.injections, tree.root_node(), node_text);
            while let Some(found_match) = matches.next() {
                let pattern = &language.injection_patterns[found_match.pattern_index];
                let mut name = pattern.language.as_deref().map(str::to_owned);
                let mut content = Vec::new();
                let captures = found_match.captures();
                let host = captures
                    .iter()
                    .map(|capture| capture.node.start_byte())
                    .min()
                    .unwrap_or(0)
                    ..captures
                        .iter()
                        .map(|capture| capture.node.end_byte())
                        .max()
                        .unwrap_or(0);
                for capture in captures {
                    let node = capture.node;
                    if Some(capture.index) == language.injection_content {
                        content.push(node);
                    } else if Some(capture.index) == language.injection_language {
                        name = Some(text.read_range(clamp(node.byte_range(), text)));
                    }
                }
                let Some(injected) = name.and_then(|name| resolver(name.trim())) else {
                    continue;
                };
                let ranges = injection_ranges(&parent_ranges, &content, pattern.include_children);
                let duplicate = found
                    .iter()
                    .any(|other| other.host == host && Arc::ptr_eq(&other.language, &injected));
                if !ranges.is_empty() && !duplicate {
                    found.push(FoundInjection {
                        language: injected,
                        host,
                        ranges,
                    });
                }
            }
        }
        found
    }

    /// Paint this layer's highlights within `range`, then its injections'
    /// over them.
    fn paint_highlights(
        &self,
        text: &TextSnapshot,
        range: &Range<usize>,
        cursor: &mut QueryCursor,
        spans: &mut Vec<(Range<usize>, Highlight)>,
    ) {
        let Some(tree) = &self.tree else {
            return;
        };
        let language = &*self.language;
        let mut painted = Vec::new();
        for included in tree.included_ranges() {
            let clipped = included.start_byte.max(range.start)..included.end_byte.min(range.end);
            if clipped.is_empty() {
                continue;
            }
            cursor.set_byte_range(clipped.clone());
            let node_text = |node: Node| {
                text.chunks(clamp(node.byte_range(), text))
                    .map(str::as_bytes)
            };
            let mut captures = cursor.captures(&language.highlights, tree.root_node(), node_text);
            while let Some((found, index)) = captures.next() {
                let capture = found.captures()[*index];
                let Some(highlight) = language.capture_highlights[capture.index as usize] else {
                    continue;
                };
                let node = capture.node.byte_range();
                let node = node.start.max(clipped.start)..node.end.min(clipped.end);
                if !node.is_empty() {
                    let priority = language.pattern_priorities[found.pattern_index];
                    painted.push((priority, node, highlight));
                }
            }
        }
        // Stable, so equal priorities keep cursor order.
        painted.sort_by_key(|(priority, ..)| *priority);
        for (_, range, highlight) in painted {
            paint(spans, range, highlight);
        }
        for injection in &self.injections {
            if injection.host.start < range.end && range.start < injection.host.end {
                injection.paint_highlights(text, range, cursor, spans);
            }
        }
    }
}

/// The parts of the `content` nodes inside `parent`, the ranges of the layer
/// hosting them, without each node's named children unless
/// `include_children`, as nvim-treesitter injects them.
fn injection_ranges(
    parent: &[TreeRange],
    content: &[Node],
    include_children: bool,
) -> Vec<TreeRange> {
    let mut pieces = Vec::new();
    for node in content {
        let mut start = (node.start_byte(), node.start_position());
        if !include_children {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                pieces.push(tree_range(
                    start,
                    (child.start_byte(), child.start_position()),
                ));
                start = (child.end_byte(), child.end_position());
            }
        }
        pieces.push(tree_range(start, (node.end_byte(), node.end_position())));
    }
    let mut ranges: Vec<_> = pieces
        .iter()
        .flat_map(|piece| {
            parent.iter().filter_map(move |outer| {
                let start = if outer.start_byte > piece.start_byte {
                    (outer.start_byte, outer.start_point)
                } else {
                    (piece.start_byte, piece.start_point)
                };
                let end = if outer.end_byte < piece.end_byte {
                    (outer.end_byte, outer.end_point)
                } else {
                    (piece.end_byte, piece.end_point)
                };
                (start.0 < end.0).then(|| tree_range(start, end))
            })
        })
        .collect();
    ranges.sort_by_key(|range| range.start_byte);
    ranges
}

fn tree_range(start: (usize, Point), end: (usize, Point)) -> TreeRange {
    TreeRange {
        start_byte: start.0,
        end_byte: end.0,
        start_point: start.1,
        end_point: end.1,
    }
}

/// `range` within the text, for nodes of a tree whose root spans past it.
fn clamp(range: Range<usize>, text: &TextSnapshot) -> Range<usize> {
    range.start.min(text.len())..range.end.min(text.len())
}

/// `range` after `edit`. A bound inside replaced text moves to the edge of
/// the replacement that keeps it covered.
fn edit_range(range: &Range<usize>, edit: &InputEdit) -> Range<usize> {
    let shift = |offset: usize, inside: usize| {
        if offset < edit.start_byte {
            offset
        } else if offset >= edit.old_end_byte {
            offset - edit.old_end_byte + edit.new_end_byte
        } else {
            inside
        }
    };
    shift(range.start, edit.start_byte)..shift(range.end, edit.new_end_byte)
}

/// Add `edit`'s replacement to `edited`, moving the earlier ranges with it.
fn record_edit(edited: &mut Vec<Range<usize>>, edit: &InputEdit) {
    for range in edited.iter_mut() {
        *range = edit_range(range, edit);
    }
    let range = edit.start_byte..edit.new_end_byte;
    match edited.last_mut() {
        // Typing extends one range rather than adding one per keystroke.
        Some(last) if touches(last, &range) => {
            *last = last.start.min(range.start)..last.end.max(range.end);
        }
        _ => edited.push(range),
    }
}

/// Sort `ranges` and merge those that overlap or touch.
fn merge(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// Whether two ranges overlap or share an endpoint.
fn touches(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start <= b.end && b.start <= a.end
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
