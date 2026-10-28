use std::time::{Duration, Instant};

use super::*;
use crate::core::buffer::ADD_CHUNK_SIZE;

/// Deterministic xorshift generator for edit sequences.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

/// A char boundary in `b` near `offset`.
fn boundary(b: &TextBuffer, mut offset: usize) -> usize {
    while !b.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn whole(snapshot: &TextSnapshot) -> String {
    snapshot.read_range(0..snapshot.len())
}

/// Concatenates `chunk_at` reads the way tree-sitter's input callback does.
fn streamed(snapshot: &TextSnapshot) -> String {
    let mut out = String::new();
    loop {
        let chunk = snapshot.chunk_at(out.len());
        if chunk.is_empty() {
            return out;
        }
        out.push_str(chunk);
    }
}

#[test]
fn snapshot_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<TextSnapshot>();
}

#[test]
fn empty_buffer_snapshot() {
    let snapshot = TextBuffer::new().snapshot();
    assert!(snapshot.is_empty());
    assert_eq!(whole(&snapshot), "");
    assert_eq!(snapshot.chunk_at(0), "");
}

#[test]
fn snapshot_is_unchanged_by_later_edits() {
    let mut b = TextBuffer::from_text("fn main() {}\n");
    b.insert(11, "\n    let");
    let snapshot = b.snapshot();
    let text = whole(&snapshot);

    // Coalescing extends the tail piece the snapshot copied.
    b.insert(19, " x = 1;");
    b.insert(3, "αβ");
    b.delete(0..2);
    b.insert(b.len(), &"y".repeat(ADD_CHUNK_SIZE));
    b.replace(0..b.len(), "gone");

    assert_eq!(whole(&snapshot), text);
    assert_eq!(text, "fn main() {\n    let}\n");
    assert_eq!(streamed(&snapshot), text);
}

#[test]
fn chunk_at_reads_from_inside_a_piece() {
    let mut b = TextBuffer::from_text("hello world");
    b.insert(5, ",");
    let snapshot = b.snapshot();
    assert_eq!(snapshot.chunk_at(0), "hello");
    assert_eq!(snapshot.chunk_at(2), "llo");
    assert_eq!(snapshot.chunk_at(5), ",");
    assert_eq!(snapshot.chunk_at(8), "orld");
    assert_eq!(snapshot.chunk_at(12), "");
}

#[test]
fn snapshot_reads_match_buffer_after_random_edits() {
    let mut rng = Rng(0x5eed_cafe);
    let mut b = TextBuffer::from_text("línea uno\nline two\n日本語\n");
    let fragments = ["a", "é", "\n", "語", "😀", "word ", "\r\n"];
    for step in 0..2_000 {
        let at = boundary(&b, rng.below(b.len() + 1));
        match rng.below(4) {
            0 if !b.is_empty() => {
                let end = boundary(&b, (at + rng.below(16)).min(b.len()));
                b.delete(at.min(end)..at.max(end));
            }
            1 => b.insert(at, &"x".repeat(rng.below(ADD_CHUNK_SIZE / 4))),
            _ => b.insert(at, fragments[rng.below(fragments.len())]),
        }
        if step % 50 != 0 {
            continue;
        }
        let snapshot = b.snapshot();
        assert_eq!(snapshot.len(), b.len());
        assert_eq!(whole(&snapshot), b.read_range(0..b.len()));
        assert_eq!(streamed(&snapshot), whole(&snapshot));
        for _ in 0..20 {
            let start = boundary(&b, rng.below(b.len() + 1));
            let end = boundary(&b, start + rng.below(b.len() - start + 1));
            assert_eq!(snapshot.read_range(start..end), b.read_range(start..end));
        }
    }
    assert!(b.sealed.len() > 1, "the sequence must seal add chunks");
}

/// Measures snapshot and streamed-read cost on a 29 MiB, million-line
/// buffer after a long editing session.
///
/// Run with `cargo test --release -p knot snapshot_cost -- --ignored --nocapture`.
#[test]
#[ignore = "measurement; run in release with --nocapture"]
fn snapshot_cost_on_large_buffer() {
    let mut text = String::new();
    for line in 0..1_001_395 {
        text.push_str(&format!("    let v{line} = f(&x, {line});\n"));
        if text.len() >= 29 * 1024 * 1024 {
            break;
        }
    }
    let mut b = TextBuffer::from_text(text);
    let mut rng = Rng(0xfeed_f00d);

    // Bursts of typing at random lines, with occasional word deletion and
    // pastes, as in a long session.
    for burst in 0..5_000 {
        let line = rng.below(b.line_count());
        let mut at = b.line_start(line);
        for ch in "let x = 1;".chars() {
            b.insert(at, &ch.to_string());
            at += ch.len_utf8();
        }
        if burst % 3 == 0 {
            b.delete(at..(at + 6).min(b.len()));
        }
        if burst % 500 == 0 {
            b.insert(at, &"pasted line\n".repeat(400));
        }
    }

    let mut timings = Vec::new();
    for _ in 0..200 {
        let started = Instant::now();
        let snapshot = b.snapshot();
        timings.push(started.elapsed());
        std::hint::black_box(snapshot);
    }
    timings.sort();
    let percentile = |p: usize| timings[timings.len() * p / 100];

    let snapshot = b.snapshot();
    let started = Instant::now();
    let mut offset = 0;
    while offset < snapshot.len() {
        offset += snapshot.chunk_at(offset).len();
    }
    let streamed = started.elapsed();

    println!(
        "{} bytes, {} lines, {} pieces, {} sealed chunks, {} tail bytes",
        b.len(),
        b.line_count(),
        b.pieces.len(),
        b.sealed.len(),
        b.tail.len()
    );
    println!(
        "snapshot p50 {:?}, p99 {:?}, max {:?}; streamed read {:?}",
        percentile(50),
        percentile(99),
        timings.last().copied().unwrap_or(Duration::ZERO),
        streamed
    );
}
