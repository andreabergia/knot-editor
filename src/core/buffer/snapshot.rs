//! `TextSnapshot`: an immutable, `Send` view of a `TextBuffer`'s text at one
//! moment, for readers off the foreground such as background parsing.
//!
//! A snapshot shares the buffer's original text and sealed add chunks and
//! owns copies of the add tail and the piece list, so later edits never
//! change what it reads. Piece end offsets are precomputed, so a read at any
//! offset is a binary search rather than a walk over the piece chain.

use std::ops::Range;
use std::sync::Arc;

use super::{Backing, Piece, TextBuffer};

/// Immutable text of a `TextBuffer` at the moment `TextBuffer::snapshot` ran.
#[derive(Clone)]
pub struct TextSnapshot {
    original: Arc<str>,
    /// The buffer's sealed add chunks followed by a copy of its tail, so
    /// `Backing::Add(chunk)` indexes this list directly.
    chunks: Vec<Arc<str>>,
    pieces: Vec<Piece>,
    /// Logical end offset of each piece, ascending.
    piece_ends: Vec<usize>,
}

impl TextBuffer {
    /// Capture the current text as an immutable snapshot.
    ///
    /// Costs O(pieces) plus a copy of the unsealed add tail, which is shorter
    /// than `ADD_CHUNK_SIZE`; the original text and sealed chunks are shared.
    pub fn snapshot(&self) -> TextSnapshot {
        let mut chunks = Vec::with_capacity(self.sealed.len() + 1);
        chunks.extend(self.sealed.iter().cloned());
        chunks.push(Arc::from(self.tail.as_str()));
        let piece_ends = self
            .pieces
            .iter()
            .scan(0, |end, p| {
                *end += p.len;
                Some(*end)
            })
            .collect();
        TextSnapshot {
            original: Arc::clone(&self.original),
            chunks,
            pieces: self.pieces.clone(),
            piece_ends,
        }
    }
}

impl TextSnapshot {
    /// Total length of the text, in UTF-8 bytes.
    pub fn len(&self) -> usize {
        self.piece_ends.last().copied().unwrap_or(0)
    }

    /// Whether the text is empty.
    // Only tests read these; product readers stream chunks.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The contiguous text from `offset` to the end of the piece containing
    /// it; empty at or past the end of the text.
    ///
    /// Concatenating successive chunks reads the whole text, which is the
    /// access pattern of tree-sitter's chunked input callback. `offset` must
    /// land on a UTF-8 char boundary.
    pub fn chunk_at(&self, offset: usize) -> &str {
        let index = self.piece_ends.partition_point(|&end| end <= offset);
        let Some(&piece) = self.pieces.get(index) else {
            return "";
        };
        let piece_start = self.piece_ends[index] - piece.len;
        &self.slice(piece)[offset - piece_start..]
    }

    /// The text in `range` as borrowed slices, in order.
    ///
    /// `range.start <= range.end <= len()` is required, and both ends must
    /// land on UTF-8 char boundaries.
    pub fn chunks(&self, range: Range<usize>) -> impl Iterator<Item = &str> {
        assert!(range.start <= range.end);
        assert!(range.end <= self.len());
        let first = self.piece_ends.partition_point(|&end| end <= range.start);
        self.pieces[first..]
            .iter()
            .zip(&self.piece_ends[first..])
            .map_while(move |(&piece, &end)| {
                let start = end - piece.len;
                if start >= range.end {
                    return None;
                }
                let slice = self.slice(piece);
                Some(&slice[range.start.max(start) - start..range.end.min(end) - start])
            })
    }

    /// Read a byte range of the text into a `String`, with the same
    /// requirements as `chunks`.
    #[allow(dead_code)]
    pub fn read_range(&self, range: Range<usize>) -> String {
        let mut out = String::with_capacity(range.len());
        for chunk in self.chunks(range) {
            out.push_str(chunk);
        }
        out
    }

    fn slice(&self, p: Piece) -> &str {
        let buf = match p.backing {
            Backing::Original => &self.original,
            Backing::Add(chunk) => &self.chunks[chunk as usize],
        };
        &buf[p.start..p.start + p.len]
    }
}

#[cfg(test)]
mod tests;
