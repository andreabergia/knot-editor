// SPDX-License-Identifier: MIT
//
// A piece-rope text buffer.
//
// This is fixture content for the knot renderer benchmark (fixture 1).
// It is real, idiomatic Rust chosen to exercise a tokenizer's
// multi-attribute shaping path: keywords (bold), comments (italic),
// strings, numbers, types, functions, and macros, with a mix of line
// lengths. It is not compiled as part of the crate.

use std::cmp::min;
use std::ops::Range;

/// Maximum byte length of a single leaf before a split is forced.
const LEAF_MAX: usize = 1 << 12;

/// Minimum arity of an internal node before a merge is considered.
const INTERNAL_MIN: usize = 2;

/// A persistent piece-rope.
///
/// The rope is a balanced binary tree whose leaves are immutable string
/// slices ("pieces") and whose internal nodes record the aggregate byte
/// length of their subtree. Edits produce new roots sharing structure
/// with the old; the old root remains valid until dropped.
pub struct Rope {
    root: Node,
    len: usize,
}

enum Node {
    Leaf { text: String },
    Internal { left: Box<Node>, right: Box<Node>, len: usize },
}

impl Rope {
    /// Build a rope from a single owned string.
    pub fn from_string(text: String) -> Self {
        let len = text.len();
        let mut rope = Rope {
            root: Node::Leaf { text },
            len,
        };
        rope.balance();
        rope
    }

    /// Total byte length.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the rope is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Insert `s` at byte offset `at`.
    ///
    /// Offsets are byte offsets into the UTF-8 content; callers are
    /// responsible for ensuring `at` lands on a char boundary. Returns
    /// `Err` if `at` is out of range.
    pub fn insert(&mut self, at: usize, s: &str) -> Result<(), RopeError> {
        if at > self.len {
            return Err(RopeError::OffsetOutOfBounds { at, len: self.len });
        }
        let new_len = self.len + s.len();
        let piece = Node::Leaf { text: s.to_owned() };
        self.root = Self::insert_at(self.root.take(), at, piece);
        self.len = new_len;
        self.balance();
        Ok(())
    }

    /// Delete the byte range `range`.
    pub fn delete(&mut self, range: Range<usize>) -> Result<(), RopeError> {
        if range.start > self.len || range.end > self.len || range.start > range.end {
            return Err(RopeError::InvalidRange { range });
        }
        if range.start == range.end {
            return Ok(());
        }
        let (left, _mid, right) = self.split_three(range.clone());
        self.root = Self::concat(left, right);
        self.len -= range.end - range.start;
        self.balance();
        Ok(())
    }

    /// Replace the byte range `range` with `s`.
    pub fn replace(&mut self, range: Range<usize>, s: &str) -> Result<(), RopeError> {
        if range.start > self.len || range.end > self.len || range.start > range.end {
            return Err(RopeError::InvalidRange { range });
        }
        let (left, _mid, right) = self.split_three(range.clone());
        let piece = Node::Leaf { text: s.to_owned() };
        let joined = Self::concat(Self::concat(left, piece), right);
        self.root = joined;
        self.len = self.len - (range.end - range.start) + s.len();
        self.balance();
        Ok(())
    }

    /// Read the entire rope into a string.
    pub fn to_string(&self) -> String {
        let mut out = String::with_capacity(self.len);
        self.append_to(&mut out);
        out
    }

    /// Read a byte range into a string.
    pub fn slice(&self, range: Range<usize>) -> Result<String, RopeError> {
        if range.start > self.len || range.end > self.len || range.start > range.end {
            return Err(RopeError::InvalidRange { range });
        }
        let mut out = String::with_capacity(range.end - range.start);
        Self::append_range(&self.root, range, &mut out);
        Ok(out)
    }

    /// Number of lines (counting a trailing newline as ending a line).
    pub fn line_count(&self) -> usize {
        let mut count = 1usize;
        self.for_each_chunk(|chunk| {
            count += chunk.bytes().filter(|&b| b == b'\n').count();
        });
        count
    }

    /// Iterate over the leaf chunks in order.
    fn for_each_chunk(&self, mut f: impl FnMut(&str)) {
        Self::walk(&self.root, &mut f);
    }

    fn walk<'a>(node: &'a Node, f: &mut impl FnMut(&'a str)) {
        match node {
            Node::Leaf { text } => f(text),
            Node::Internal { left, right, .. } => {
                Self::walk(left, f);
                Self::walk(right, f);
            }
        }
    }

    fn append_to(&self, out: &mut String) {
        Self::walk(&self.root, &mut |chunk| out.push_str(chunk));
    }

    fn append_range(node: &Node, range: Range<usize>, out: &mut String) {
        match node {
            Node::Leaf { text } => {
                let start = min(range.start, text.len());
                let end = min(range.end, text.len());
                if start < end {
                    out.push_str(&text[start..end]);
                }
            }
            Node::Internal { left, right, len, .. } => {
                let left_len = left.len();
                if range.start < left_len {
                    let sub = range.start..min(range.end, left_len);
                    Self::append_range(left, sub, out);
                }
                if range.end > left_len {
                    let sub = range.start.saturating_sub(left_len)..range.end - left_len;
                    let _ = len;
                    Self::append_range(right, sub, out);
                }
            }
        }
    }

    fn insert_at(node: Node, at: usize, piece: Node) -> Node {
        match node {
            Node::Leaf { text } => {
                let at = min(at, text.len());
                let (head, tail) = text.split_at(at);
                let head = Node::Leaf { text: head.to_owned() };
                let tail = Node::Leaf { text: tail.to_owned() };
                Node::Internal {
                    left: Box::new(Self::concat(head, piece)),
                    right: Box::new(tail),
                    len: 0, // recomputed below
                }
                .recomputed()
            }
            Node::Internal { left, right, .. } => {
                let left_len = left.len();
                if at <= left_len {
                    let new_left = Self::insert_at(*left, at, piece);
                    Node::Internal {
                        left: Box::new(new_left),
                        right,
                        len: 0,
                    }
                    .recomputed()
                } else {
                    let new_right = Self::insert_at(*right, at - left_len, piece);
                    Node::Internal {
                        left,
                        right: Box::new(new_right),
                        len: 0,
                    }
                    .recomputed()
                }
            }
        }
    }

    fn concat(a: Node, b: Node) -> Node {
        match (&a, &b) {
            (Node::Leaf { text: ta }, Node::Leaf { text: tb })
                if ta.len() + tb.len() <= LEAF_MAX =>
            {
                let mut text = String::with_capacity(ta.len() + tb.len());
                text.push_str(ta);
                text.push_str(tb);
                Node::Leaf { text }
            }
            _ => Node::Internal {
                left: Box::new(a),
                right: Box::new(b),
                len: 0,
            }
            .recomputed(),
        }
    }

    /// Split the rope into three parts at `range`: bytes before, the
    /// removed middle, and the bytes after.
    fn split_three(&mut self, range: Range<usize>) -> (Node, Node, Node) {
        let (left, mid_right) = Self::split_at(self.root.take(), range.start);
        let (mid, right) = Self::split_at(mid_right, range.end - range.start);
        (left, mid, right)
    }

    fn split_at(node: Node, at: usize) -> (Node, Node) {
        match node {
            Node::Leaf { text } => {
                let at = min(at, text.len());
                let (head, tail) = text.split_at(at);
                (
                    Node::Leaf { text: head.to_owned() },
                    Node::Leaf { text: tail.to_owned() },
                )
            }
            Node::Internal { left, right, .. } => {
                let left_len = left.len();
                if at <= left_len {
                    let (l, r) = Self::split_at(*left, at);
                    (l, Self::concat(r, *right))
                } else {
                    let (l, r) = Self::split_at(*right, at - left_len);
                    (Self::concat(*left, l), r)
                }
            }
        }
    }

    fn balance(&mut self) {
        // Repeatedly split over-long leaves and merge under-full internals.
        // A full self-balancing implementation would track heights; for the
        // prototype we rebalance by flattening and rebuilding when the tree
        // gets too deep.
        let depth = Self::depth(&self.root);
        if depth > 32 {
            let chunks: Vec<String> = {
                let mut v = Vec::new();
                Self::walk(&self.root, &mut |c| v.push(c.to_owned()));
                v
            };
            self.root = Self::build_balanced(&chunks);
        }
    }

    fn build_balanced(chunks: &[String]) -> Node {
        if chunks.is_empty() {
            return Node::Leaf { text: String::new() };
        }
        if chunks.len() == 1 {
            return Node::Leaf { text: chunks[0].clone() };
        }
        let mid = chunks.len() / 2;
        let left = Self::build_balanced(&chunks[..mid]);
        let right = Self::build_balanced(&chunks[mid..]);
        Node::Internal {
            left: Box::new(left),
            right: Box::new(right),
            len: 0,
        }
        .recomputed()
    }

    fn depth(node: &Node) -> usize {
        match node {
            Node::Leaf { .. } => 1,
            Node::Internal { left, right, .. } => {
                1 + Self::depth(left).max(Self::depth(right))
            }
        }
    }

    fn take(&mut self) -> Node {
        std::mem::replace(&mut self.root, Node::Leaf { text: String::new() })
    }
}

impl Node {
    fn len(&self) -> usize {
        match self {
            Node::Leaf { text } => text.len(),
            Node::Internal { len, .. } => *len,
        }
    }

    fn recomputed(mut self) -> Node {
        if let Node::Internal { left, right, len } = &mut self {
            *len = left.len() + right.len();
            if *len < INTERNAL_MIN {
                // Under-full: would normally trigger a merge with a sibling;
                // left as a placeholder for the real implementation.
            }
        }
        self
    }
}

/// Errors produced by rope operations.
#[derive(Debug)]
pub enum RopeError {
    OffsetOutOfBounds { at: usize, len: usize },
    InvalidRange { range: Range<usize> },
}

/// A cursor over a rope, yielding byte ranges per chunk.
pub struct Cursor<'a> {
    stack: Vec<(&'a Node, usize)>,
    offset: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(rope: &'a Rope) -> Self {
        Cursor {
            stack: vec![(&rope.root, 0)],
            offset: 0,
        }
    }

    /// Advance to the next chunk, returning its byte range and text.
    pub fn next_chunk(&mut self) -> Option<(Range<usize>, &'a str)> {
        while let Some((node, _)) = self.stack.pop() {
            match node {
                Node::Leaf { text } => {
                    let start = self.offset;
                    self.offset += text.len();
                    return Some((start..self.offset, text));
                }
                Node::Internal { left, right, .. } => {
                    self.stack.push((right, self.offset + left.len()));
                    self.stack.push((left, self.offset));
                }
            }
        }
        None
    }
}

/// Convert a byte offset to a (line, column) pair.
pub fn offset_to_line_col(rope: &Rope, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 0usize;
    let mut seen = 0usize;
    rope.for_each_chunk(|chunk| {
        if seen >= offset {
            return;
        }
        for (i, b) in chunk.bytes().enumerate() {
            if b == b'\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
            seen += 1;
            if seen >= offset {
                let _ = i;
                break;
            }
        }
    });
    (line, col)
}

// ---------------------------------------------------------------------------
// Gap buffer
// ---------------------------------------------------------------------------

/// A gap buffer, the classic single-editor backing store.
///
/// The buffer keeps an empty "gap" of unused capacity at the edit point
/// so that typing at the cursor is O(1) amortized. Moves relocate the
/// gap. It is not suitable for concurrent edits but is a useful baseline.
pub struct GapBuffer {
    storage: Vec<u8>,
    gap_start: usize,
    gap_end: usize,
}

impl GapBuffer {
    /// Build a gap buffer preloaded with `text`, gap at offset 0.
    pub fn from_string(text: &str) -> Self {
        let len = text.len();
        let cap = len.max(64).next_power_of_two();
        let mut storage = Vec::with_capacity(cap);
        storage.extend_from_slice(text.as_bytes());
        GapBuffer {
            storage,
            gap_start: 0,
            gap_end: 0,
        }
    }

    /// Total content length (excluding the gap).
    pub fn len(&self) -> usize {
        self.storage.len() - (self.gap_end - self.gap_start)
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Move the gap so that it starts at byte offset `at`.
    pub fn move_gap_to(&mut self, at: usize) {
        let at = min(at, self.len());
        if at == self.gap_start {
            return;
        }
        if at < self.gap_start {
            let count = self.gap_start - at;
            self.storage
                .copy_within(at..at + count, self.gap_end - count);
            self.gap_start -= count;
            self.gap_end -= count;
        } else {
            let count = at - self.gap_start;
            self.storage
                .copy_within(self.gap_end..self.gap_end + count, self.gap_start);
            self.gap_start += count;
            self.gap_end += count;
        }
    }

    /// Ensure the gap is at least `needed` bytes; grow if not.
    fn ensure_gap(&mut self, needed: usize) {
        let current = self.gap_end - self.gap_start;
        if current >= needed {
            return;
        }
        let grow_to = self.storage.len() + needed.max(self.storage.len());
        let mut new_storage = Vec::with_capacity(grow_to.next_power_of_two());
        new_storage.extend_from_slice(&self.storage[..self.gap_start]);
        let tail_start = self.gap_end;
        let tail_end = self.storage.len();
        new_storage.extend_from_slice(&self.storage[tail_start..tail_end]);
        let new_gap_start = self.gap_start;
        let new_gap_end = new_gap_start + (grow_to - (self.storage.len() - current));
        self.storage = new_storage;
        self.gap_start = new_gap_start;
        self.gap_end = new_gap_end;
    }

    /// Insert `s` at byte offset `at`.
    pub fn insert(&mut self, at: usize, s: &str) {
        self.move_gap_to(at);
        self.ensure_gap(s.len());
        self.storage[self.gap_start..self.gap_start + s.len()]
            .copy_from_slice(s.as_bytes());
        self.gap_start += s.len();
    }

    /// Delete `n` bytes starting at byte offset `at`.
    pub fn delete(&mut self, at: usize, n: usize) {
        self.move_gap_to(at);
        let n = min(n, self.storage.len() - self.gap_end);
        self.gap_end += n;
    }

    /// Snapshot the entire buffer as a string.
    pub fn to_string(&self) -> String {
        let mut out = String::with_capacity(self.len());
        out.push_str(std::str::from_utf8(&self.storage[..self.gap_start]).unwrap_or(""));
        out.push_str(std::str::from_utf8(&self.storage[self.gap_end..]).unwrap_or(""));
        out
    }
}

impl Default for GapBuffer {
    fn default() -> Self {
        GapBuffer::from_string("")
    }
}

// ---------------------------------------------------------------------------
// Piece table
// ---------------------------------------------------------------------------

/// A piece table: the original document plus an append-only log of edits.
///
/// Unlike the rope, the piece table never copies the original; edits are
/// recorded as references into a scratch buffer. It is simpler than the
/// rope but slower for random access, since reads must walk the piece list.
pub struct PieceTable {
    original: String,
    scratch: String,
    pieces: Vec<Piece>,
    len: usize,
}

#[derive(Clone, Copy)]
struct Piece {
    source: Source,
    start: usize,
    len: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Source {
    Original,
    Scratch,
}

impl PieceTable {
    /// Build a piece table from the original document text.
    pub fn new(original: String) -> Self {
        let len = original.len();
        let pieces = if len == 0 {
            Vec::new()
        } else {
            vec![Piece {
                source: Source::Original,
                start: 0,
                len,
            }]
        };
        PieceTable {
            original,
            scratch: String::new(),
            pieces,
            len,
        }
    }

    /// Total byte length of the current document.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the document is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Insert `s` at byte offset `at`.
    pub fn insert(&mut self, at: usize, s: &str) {
        if s.is_empty() {
            return;
        }
        let start = self.scratch.len();
        self.scratch.push_str(s);
        let piece = Piece {
            source: Source::Scratch,
            start,
            len: s.len(),
        };
        self.insert_piece(at, piece);
        self.len += s.len();
    }

    fn insert_piece(&mut self, at: usize, piece: Piece) {
        if at == 0 {
            self.pieces.insert(0, piece);
            return;
        }
        let mut consumed = 0usize;
        let mut idx = 0usize;
        while idx < self.pieces.len() {
            let p = self.pieces[idx];
            if consumed + p.len >= at {
                let split_at = at - consumed;
                if split_at == 0 {
                    self.pieces.insert(idx, piece);
                } else if split_at == p.len {
                    self.pieces.insert(idx + 1, piece);
                } else {
                    let left = Piece {
                        source: p.source,
                        start: p.start,
                        len: split_at,
                    };
                    let right = Piece {
                        source: p.source,
                        start: p.start + split_at,
                        len: p.len - split_at,
                    };
                    self.pieces[idx] = left;
                    self.pieces.insert(idx + 1, piece);
                    self.pieces.insert(idx + 2, right);
                }
                return;
            }
            consumed += p.len;
            idx += 1;
        }
        self.pieces.push(piece);
    }

    /// Delete `n` bytes starting at byte offset `at`.
    pub fn delete(&mut self, at: usize, n: usize) {
        if n == 0 {
            return;
        }
        let end = min(at + n, self.len);
        let mut consumed = 0usize;
        let mut idx = 0usize;
        while idx < self.pieces.len() && consumed < end {
            let mut p = self.pieces[idx];
            let p_end = consumed + p.len;
            if p_end <= at {
                consumed = p_end;
                idx += 1;
                continue;
            }
            let rel_start = at.saturating_sub(consumed);
            let rel_end = end.saturating_sub(consumed).min(p.len);
            if rel_start == 0 && rel_end == p.len {
                self.pieces.remove(idx);
                self.len -= p.len;
                consumed += p.len;
                continue;
            }
            if rel_start > 0 {
                let left = Piece {
                    source: p.source,
                    start: p.start,
                    len: rel_start,
                };
                self.pieces.insert(idx, left);
                idx += 1;
                p.start += rel_end;
                p.len -= rel_end;
                self.pieces[idx] = p;
                self.len -= rel_end - rel_start;
                consumed += rel_start + (rel_end - rel_start);
            } else {
                p.start += rel_end;
                p.len -= rel_end;
                self.pieces[idx] = p;
                self.len -= rel_end;
                consumed += rel_end;
            }
            idx += 1;
        }
    }

    /// Snapshot the entire document as a string.
    pub fn to_string(&self) -> String {
        let mut out = String::with_capacity(self.len);
        for p in &self.pieces {
            let src = match p.source {
                Source::Original => &self.original,
                Source::Scratch => &self.scratch,
            };
            out.push_str(&src[p.start..p.start + p.len]);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Positions
// ---------------------------------------------------------------------------

/// A byte offset that survives edits.
///
/// Positions are stored as `(line, column)` so that insertions elsewhere
/// in the document keep the logical location stable. They are resolved
/// back to a concrete byte offset on demand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

impl Position {
    pub const ZERO: Position = Position { line: 0, column: 0 };

    pub fn new(line: usize, column: usize) -> Self {
        Position { line, column }
    }

    /// Advance by one character, given the character's byte length and
    /// whether it was a newline.
    pub fn advance(self, is_newline: bool) -> Position {
        if is_newline {
            Position {
                line: self.line + 1,
                column: 0,
            }
        } else {
            Position {
                line: self.line,
                column: self.column + 1,
            }
        }
    }

    /// Step back by one character. Best-effort; does not cross line
    /// boundaries without line-length context.
    pub fn retreat(self) -> Position {
        if self.column > 0 {
            Position {
                line: self.line,
                column: self.column - 1,
            }
        } else if self.line > 0 {
            Position {
                line: self.line - 1,
                column: usize::MAX,
            }
        } else {
            self
        }
    }

    /// Compare two positions lexicographically.
    pub fn cmp(self, other: Position) -> std::cmp::Ordering {
        self.line
            .cmp(&other.line)
            .then(self.column.cmp(&other.column))
    }
}

/// A half-open range between two positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionRange {
    pub start: Position,
    pub end: Position,
}

impl PositionRange {
    pub fn new(start: Position, end: Position) -> Self {
        PositionRange { start, end }
    }

    /// Whether the range is empty (a caret).
    pub fn is_caret(&self) -> bool {
        self.start == self.end
    }

    /// Whether `pos` is within `[start, end)`.
    pub fn contains(&self, pos: Position) -> bool {
        pos.cmp(self.start) != std::cmp::Ordering::Less
            && pos.cmp(self.end) == std::cmp::Ordering::Less
    }
}

// ---------------------------------------------------------------------------
// Undo / redo
// ---------------------------------------------------------------------------

/// A recorded edit, used by the undo stack.
#[derive(Clone, Debug)]
pub enum Edit {
    Insert { at: usize, text: String },
    Delete { range: Range<usize> },
    Replace { range: Range<usize>, text: String },
}

/// An undo/redo stack tracking edits against a buffer.
pub struct History {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    bound: usize,
}

impl History {
    pub fn new() -> Self {
        History {
            undo: Vec::new(),
            redo: Vec::new(),
            bound: 1024,
        }
    }

    /// Maximum number of undo entries kept.
    pub fn with_bound(bound: usize) -> Self {
        History {
            undo: Vec::new(),
            redo: Vec::new(),
            bound,
        }
    }

    /// Record an edit and clear the redo stack.
    pub fn record(&mut self, edit: Edit) {
        if self.undo.len() >= self.bound {
            self.undo.remove(0);
        }
        self.undo.push(edit);
        self.redo.clear();
    }

    /// Pop the most recent edit for undo, leaving it on the redo stack.
    pub fn undo(&mut self) -> Option<&Edit> {
        if let Some(edit) = self.undo.pop() {
            self.redo.push(edit);
            self.redo.last()
        } else {
            None
        }
    }

    /// Pop the most recent undone edit for redo.
    pub fn redo(&mut self) -> Option<&Edit> {
        if let Some(edit) = self.redo.pop() {
            self.undo.push(edit);
            self.undo.last()
        } else {
            None
        }
    }

    /// Whether an undo is available.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether a redo is available.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Drop all history.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

impl Default for History {
    fn default() -> Self {
        History::new()
    }
}

// ---------------------------------------------------------------------------
// Token kinds (placeholder for syntax highlighting integration)
// ---------------------------------------------------------------------------

/// A coarse syntactic classification, sufficient for coloring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    Type,
    Function,
    Macro,
    String,
    Number,
    Comment,
    Punctuation,
    Whitespace,
    Ident,
    Lifetime,
    Attribute,
}

/// A single token: kind plus byte range within a line.
#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub range: Range<usize>,
}

impl Token {
    pub fn new(kind: TokenKind, range: Range<usize>) -> Self {
        Token { kind, range }
    }

    /// Map this token to a 0xRRGGBB color.
    pub fn color(self) -> u32 {
        match self.kind {
            TokenKind::Keyword => 0x569CD6,
            TokenKind::Type => 0x4EC9B0,
            TokenKind::Function => 0xDCDCAA,
            TokenKind::Macro => 0xC586C0,
            TokenKind::String => 0xCE9178,
            TokenKind::Number => 0xB5CEA8,
            TokenKind::Comment => 0x6A9955,
            TokenKind::Punctuation => 0xD4D4D4,
            TokenKind::Whitespace => 0xD4D4D4,
            TokenKind::Ident => 0x9CDCFE,
            TokenKind::Lifetime => 0xD7BA7D,
            TokenKind::Attribute => 0xC586C0,
        }
    }

    /// Whether this token should render bold.
    pub fn bold(self) -> bool {
        matches!(
            self.kind,
            TokenKind::Keyword | TokenKind::Macro | TokenKind::Attribute
        )
    }

    /// Whether this token should render italic.
    pub fn italic(self) -> bool {
        matches!(self.kind, TokenKind::Comment)
    }
}

// ---------------------------------------------------------------------------
// Line map
// ---------------------------------------------------------------------------

/// A precomputed map of line start byte offsets.
///
/// Built once per buffer snapshot; supports O(log n) line lookups.
pub struct LineMap {
    starts: Vec<usize>,
}

impl LineMap {
    /// Build a line map from the full document bytes.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut starts = vec![0];
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        LineMap { starts }
    }

    /// Number of lines (including a possible empty trailing line).
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// Byte offset where line `n` begins.
    pub fn line_start(&self, n: usize) -> Option<usize> {
        self.starts.get(n).copied()
    }

    /// Index of the line containing byte offset `at`.
    pub fn line_at(&self, at: usize) -> usize {
        match self.starts.binary_search(&at) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    /// Byte range covering line `n` (exclusive of the trailing newline).
    pub fn line_range(&self, n: usize) -> Option<Range<usize>> {
        let start = self.line_start(n)?;
        let end = self.line_start(n + 1).unwrap_or(start);
        Some(start..end)
    }
}

// ---------------------------------------------------------------------------
// Markers
// ---------------------------------------------------------------------------

/// A marker that tracks a logical position as the buffer is edited.
///
/// Markers are the foundation annotations depend on: an annotation
/// attached to "the third word on line 12" must follow that word as text
/// is inserted or deleted around it.
pub struct Marker {
    pos: Position,
    stickiness: Stickiness,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stickiness {
    /// Stays put when text is inserted at its position.
    Left,
    /// Moves with inserted text at its position.
    Right,
}

impl Marker {
    pub fn new(pos: Position, stickiness: Stickiness) -> Self {
        Marker { pos, stickiness }
    }

    pub fn position(&self) -> Position {
        self.pos
    }

    /// Update the marker in response to an edit at `range` inserting
    /// `inserted` lines and `inserted_cols` columns on the affected line.
    pub fn on_edit(&mut self, range: PositionRange, inserted: Position) {
        let stick = self.stickiness;
        let edit_start = range.start;
        let edit_end = range.end;
        let at_start = self.pos.cmp(edit_start) != std::cmp::Ordering::Less;
        let at_end = self.pos.cmp(edit_end) == std::cmp::Ordering::Less;
        if !at_start {
            return;
        }
        if at_end {
            return;
        }
        if self.pos.line == edit_start.line {
            if self.pos.column > edit_start.column
                || (self.pos.column == edit_start.column && stick == Stickiness::Right)
            {
                self.pos.column = self.pos.column + inserted.column - (edit_end.column - edit_start.column);
                self.pos.line += inserted.line;
            }
        } else {
            self.pos.line += inserted.line;
        }
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Editor configuration, loaded from a TOML file in the final product.
#[derive(Debug, Clone)]
pub struct Config {
    pub tab_width: usize,
    pub font_size: f32,
    pub font_family: String,
    pub line_height: f32,
    pub max_undo: usize,
    pub auto_indent: bool,
    pub word_wrap: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            tab_width: 4,
            font_size: 14.0,
            font_family: "JetBrains Mono".to_owned(),
            line_height: 1.5,
            max_undo: 1024,
            auto_indent: true,
            word_wrap: false,
        }
    }
}

impl Config {
    /// Pixel width of one tab character at the configured font.
    pub fn tab_pixel_width(&self, char_width: f32) -> f32 {
        char_width * self.tab_width as f32
    }

    /// Line height in pixels.
    pub fn line_pixel_height(&self) -> f32 {
        self.font_size * self.line_height
    }
}

// ---------------------------------------------------------------------------
// Viewport
// ---------------------------------------------------------------------------

/// The visible region of the document.
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub first_line: usize,
    pub visible_lines: usize,
    pub first_column: usize,
    pub visible_columns: usize,
}

impl Viewport {
    pub fn new(first_line: usize, visible_lines: usize) -> Self {
        Viewport {
            first_line,
            visible_lines,
            first_column: 0,
            visible_columns: 120,
        }
    }

    /// Scroll down by `n` lines, clamped to `total_lines`.
    pub fn scroll_down(&mut self, n: usize, total_lines: usize) {
        let max = total_lines.saturating_sub(self.visible_lines);
        self.first_line = (self.first_line + n).min(max);
    }

    /// Scroll up by `n` lines.
    pub fn scroll_up(&mut self, n: usize) {
        self.first_line = self.first_line.saturating_sub(n);
    }

    /// Scroll right by `n` columns.
    pub fn scroll_right(&mut self, n: usize) {
        self.first_column += n;
    }

    /// Scroll left by `n` columns.
    pub fn scroll_left(&mut self, n: usize) {
        self.first_column = self.first_column.saturating_sub(n);
    }

    /// Whether line `n` is currently visible.
    pub fn line_visible(&self, n: usize) -> bool {
        n >= self.first_line && n < self.first_line + self.visible_lines
    }

    /// Range of visible line indices.
    pub fn visible_line_range(&self) -> Range<usize> {
        self.first_line..self.first_line + self.visible_lines
    }
}

// ---------------------------------------------------------------------------
// Style
// ---------------------------------------------------------------------------

/// A resolved text style, combining color and weight.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub color: u32,
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    pub const PLAIN: Style = Style {
        color: 0xD4D4D4,
        bold: false,
        italic: false,
    };

    pub fn from_token(token: Token) -> Self {
        Style {
            color: token.color(),
            bold: token.bold(),
            italic: token.italic(),
        }
    }
}

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

/// Pack an (r, g, b) triple into a 0xRRGGBB word.
pub fn pack_color(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Unpack a 0xRRGGBB word into (r, g, b).
pub fn unpack_color(c: u32) -> (u8, u8, u8) {
    (
        ((c >> 16) & 0xFF) as u8,
        ((c >> 8) & 0xFF) as u8,
        (c & 0xFF) as u8,
    )
}

/// Linearly interpolate between two colors.
pub fn lerp_color(a: u32, b: u32, t: f32) -> u32 {
    let (ar, ag, ab) = unpack_color(a);
    let (br, bg, bb) = unpack_color(b);
    let r = (ar as f32 + (br as f32 - ar as f32) * t) as u8;
    let g = (ag as f32 + (bg as f32 - ag as f32) * t) as u8;
    let bl = (ab as f32 + (bb as f32 - ab as f32) * t) as u8;
    pack_color(r, g, bl)
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Layout metrics for a monospace font.
#[derive(Debug, Clone, Copy)]
pub struct FontMetrics {
    pub char_width: f32,
    pub line_height: f32,
    pub ascent: f32,
    pub descent: f32,
}

impl FontMetrics {
    pub fn new(char_width: f32, font_size: f32) -> Self {
        FontMetrics {
            char_width,
            line_height: font_size * 1.5,
            ascent: font_size * 0.9,
            descent: font_size * 0.2,
        }
    }

    /// Pixel x-offset for column `col`.
    pub fn x_for_column(&self, col: usize) -> f32 {
        col as f32 * self.char_width
    }

    /// Column for a pixel x-offset.
    pub fn column_for_x(&self, x: f32) -> usize {
        (x / self.char_width).round() as usize
    }

    /// Pixel y-offset for line `line` (top of the line).
    pub fn y_for_line(&self, line: usize) -> f32 {
        line as f32 * self.line_height
    }

    /// Line for a pixel y-offset.
    pub fn line_for_y(&self, y: f32) -> usize {
        (y / self.line_height) as usize
    }
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// An immutable snapshot of the buffer at a point in time.
///
/// Snapshots are cheap to clone and keep the document alive for as long
/// as any snapshot references it.
pub struct Snapshot {
    text: String,
    version: u64,
}

impl Snapshot {
    pub fn new(text: String, version: u64) -> Self {
        Snapshot { text, version }
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl Clone for Snapshot {
    fn clone(&self) -> Self {
        Snapshot {
            text: self.text.clone(),
            version: self.version,
        }
    }
}

// ---------------------------------------------------------------------------
// Buffer
// ---------------------------------------------------------------------------

/// The top-level editable buffer, tying together the rope and history.
pub struct Buffer {
    rope: Rope,
    history: History,
    version: u64,
    line_map: LineMap,
}

impl Buffer {
    /// Build a buffer from initial text.
    pub fn from_string(text: String) -> Self {
        let bytes = text.as_bytes();
        let line_map = LineMap::from_bytes(bytes);
        Buffer {
            rope: Rope::from_string(text),
            history: History::new(),
            version: 0,
            line_map,
        }
    }

    /// Total byte length.
    pub fn len(&self) -> usize {
        self.rope.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.rope.is_empty()
    }

    /// Current version counter.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Number of lines.
    pub fn line_count(&self) -> usize {
        self.line_map.line_count()
    }

    /// Insert `s` at byte offset `at`, recording the edit in history.
    pub fn insert(&mut self, at: usize, s: &str) -> Result<(), RopeError> {
        self.rope.insert(at, s)?;
        self.history.record(Edit::Insert {
            at,
            text: s.to_owned(),
        });
        self.version += 1;
        self.rebuild_line_map();
        Ok(())
    }

    /// Delete the byte range `range`, recording the edit in history.
    pub fn delete(&mut self, range: Range<usize>) -> Result<(), RopeError> {
        let removed = self.rope.slice(range.clone())?;
        self.rope.delete(range.clone())?;
        self.history.record(Edit::Delete { range });
        self.version += 1;
        self.rebuild_line_map();
        let _ = removed;
        Ok(())
    }

    /// Replace the byte range `range` with `s`.
    pub fn replace(&mut self, range: Range<usize>, s: &str) -> Result<(), RopeError> {
        self.rope.replace(range.clone(), s)?;
        self.history.record(Edit::Replace {
            range,
            text: s.to_owned(),
        });
        self.version += 1;
        self.rebuild_line_map();
        Ok(())
    }

    /// Undo the most recent edit.
    pub fn undo(&mut self) -> Result<bool, RopeError> {
        match self.history.undo() {
            Some(edit) => {
                self.apply_inverse(edit)?;
                self.version += 1;
                self.rebuild_line_map();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Redo the most recently undone edit.
    pub fn redo(&mut self) -> Result<bool, RopeError> {
        match self.history.redo() {
            Some(edit) => {
                self.apply(edit.clone())?;
                self.version += 1;
                self.rebuild_line_map();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Take a snapshot of the current contents.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot::new(self.rope.to_string(), self.version)
    }

    /// Apply an edit forward (for redo).
    fn apply(&mut self, edit: Edit) -> Result<(), RopeError> {
        match edit {
            Edit::Insert { at, text } => self.rope.insert(at, &text),
            Edit::Delete { range } => self.rope.delete(range),
            Edit::Replace { range, text } => self.rope.replace(range, &text),
        }
    }

    /// Apply the inverse of an edit (for undo).
    fn apply_inverse(&mut self, edit: &Edit) -> Result<(), RopeError> {
        match edit {
            Edit::Insert { at, text } => self.rope.delete(*at..*at + text.len()),
            Edit::Delete { range } => {
                let _ = range;
                Ok(())
            }
            Edit::Replace { range, text } => {
                let _ = (range, text);
                Ok(())
            }
        }
    }

    fn rebuild_line_map(&mut self) {
        let bytes = self.rope.to_string();
        self.line_map = LineMap::from_bytes(bytes.as_bytes());
    }

    /// Byte range covering line `n`.
    pub fn line_range(&self, n: usize) -> Option<Range<usize>> {
        self.line_map.line_range(n)
    }

    /// The text of line `n` as an owned string.
    pub fn line_text(&self, n: usize) -> Option<String> {
        let range = self.line_range(n)?;
        self.rope.slice(range)
    }
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// A selection is a list of ranges; the last range is the "primary"
/// (caret + active anchor).
#[derive(Debug, Clone)]
pub struct Selection {
    ranges: Vec<PositionRange>,
    primary: usize,
}

impl Selection {
    pub fn empty() -> Self {
        Selection {
            ranges: Vec::new(),
            primary: 0,
        }
    }

    pub fn single(range: PositionRange) -> Self {
        Selection {
            ranges: vec![range],
            primary: 0,
        }
    }

    pub fn primary(&self) -> Option<&PositionRange> {
        self.ranges.get(self.primary)
    }

    pub fn ranges(&self) -> &[PositionRange] {
        &self.ranges
    }

    /// Add a range, making it primary.
    pub fn add(&mut self, range: PositionRange) {
        self.ranges.push(range);
        self.primary = self.ranges.len() - 1;
    }

    /// Replace all ranges with a single one.
    pub fn set(&mut self, range: PositionRange) {
        self.ranges.clear();
        self.ranges.push(range);
        self.primary = 0;
    }

    /// Collapse every range to its start (caret).
    pub fn collapse(&mut self) {
        for r in &mut self.ranges {
            r.end = r.start;
        }
    }

    /// Number of ranges in the selection.
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Whether the selection is empty.
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Word boundaries
// ---------------------------------------------------------------------------

/// Classify a byte for word-boundary purposes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CharClass {
    Whitespace,
    Word,
    Punctuation,
    Other,
}

pub fn classify(b: u8) -> CharClass {
    match b {
        b' ' | b'\t' | b'\n' | b'\r' => CharClass::Whitespace,
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' => CharClass::Word,
        b'!'..=b'/' | b':'..=b'@' | b'['..=b'`' | b'{'..=b'~' => CharClass::Punctuation,
        _ => CharClass::Other,
    }
}

/// Find the start of the word at byte offset `at`.
pub fn word_start(bytes: &[u8], at: usize) -> usize {
    if at == 0 {
        return 0;
    }
    let class = classify(bytes[at - 1]);
    let mut i = at;
    while i > 0 && classify(bytes[i - 1]) == class {
        i -= 1;
    }
    i
}

/// Find the end of the word at byte offset `at`.
pub fn word_end(bytes: &[u8], at: usize) -> usize {
    let mut i = at;
    if i >= bytes.len() {
        return i;
    }
    let class = classify(bytes[i]);
    while i < bytes.len() && classify(bytes[i]) == class {
        i += 1;
    }
    i
}

/// The byte range of the word containing offset `at`.
pub fn word_at(bytes: &[u8], at: usize) -> Range<usize> {
    word_start(bytes, at)..word_end(bytes, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_read() {
        let r = Rope::from_string("hello, world".to_owned());
        assert_eq!(r.len(), 12);
        assert_eq!(r.to_string(), "hello, world");
    }

    #[test]
    fn insert_in_middle() {
        let mut r = Rope::from_string("hello world".to_owned());
        r.insert(5, ", cruel".to_owned().as_str()).unwrap();
        assert_eq!(r.to_string(), "hello, cruel world");
    }

    #[test]
    fn delete_range() {
        let mut r = Rope::from_string("hello, cruel world".to_owned());
        r.delete(5..12).unwrap();
        assert_eq!(r.to_string(), "hello world");
    }

    #[test]
    fn slice_subrange() {
        let r = Rope::from_string("the quick brown fox".to_owned());
        assert_eq!(r.slice(4..9).unwrap(), "quick");
    }

    #[test]
    fn line_count() {
        let r = Rope::from_string("a\nb\nc\n".to_owned());
        assert_eq!(r.line_count(), 4);
    }
}
