use super::*;

fn s(b: &TextBuffer) -> String {
    b.read_range(0..b.len())
}

#[test]
fn empty_buffer() {
    let b = TextBuffer::new();
    assert!(b.is_empty());
    assert_eq!(b.len(), 0);
    assert_eq!(s(&b), "");
    assert_eq!(b.read_range(0..0), "");
}

#[test]
fn from_text_round_trips() {
    let b = TextBuffer::from_text("hello world");
    assert_eq!(b.len(), 11);
    assert_eq!(s(&b), "hello world");
    assert_eq!(b.read_range(0..5), "hello");
    assert_eq!(b.read_range(6..11), "world");
    assert_eq!(b.read_range(3..8), "lo wo");
}

#[test]
fn read_range_empty() {
    let b = TextBuffer::from_text("abc");
    assert_eq!(b.read_range(0..0), "");
    assert_eq!(b.read_range(1..1), "");
    assert_eq!(b.read_range(3..3), "");
}

#[test]
fn insert_at_start() {
    let mut b = TextBuffer::from_text("world");
    b.insert(0, "hello ");
    assert_eq!(s(&b), "hello world");
}

#[test]
fn insert_at_end() {
    let mut b = TextBuffer::from_text("hello");
    b.insert(5, " world");
    assert_eq!(s(&b), "hello world");
}

#[test]
fn insert_at_interior() {
    let mut b = TextBuffer::from_text("hello world");
    b.insert(5, ",");
    assert_eq!(s(&b), "hello, world");
}

#[test]
fn insert_empty_is_noop() {
    let mut b = TextBuffer::from_text("hello");
    b.insert(2, "");
    assert_eq!(s(&b), "hello");
    assert_eq!(b.len(), 5);
}

#[test]
fn insert_into_empty_buffer() {
    let mut b = TextBuffer::new();
    b.insert(0, "first");
    assert_eq!(s(&b), "first");
    b.insert(0, "second ");
    assert_eq!(s(&b), "second first");
    b.insert(b.len(), " last");
    assert_eq!(s(&b), "second first last");
}

#[test]
fn delete_whole_piece() {
    let mut b = TextBuffer::from_text("hello world");
    b.delete(6..11);
    assert_eq!(s(&b), "hello ");
    assert_eq!(b.len(), 6);
}

#[test]
fn delete_interior_split_left() {
    // "hello world" (len 11): delete indices 2..8 removes "llo wo",
    // leaving "he" + "rld" = "herld".
    let mut b = TextBuffer::from_text("hello world");
    b.delete(2..8);
    assert_eq!(s(&b), "herld");
}

#[test]
fn delete_start_boundary() {
    let mut b = TextBuffer::from_text("hello world");
    b.delete(0..6);
    assert_eq!(s(&b), "world");
}

#[test]
fn delete_empty_is_noop() {
    let mut b = TextBuffer::from_text("hello");
    b.delete(2..2);
    assert_eq!(s(&b), "hello");
}

#[test]
fn delete_entire_buffer() {
    let mut b = TextBuffer::from_text("abc");
    b.delete(0..3);
    assert!(b.is_empty());
    assert_eq!(s(&b), "");
}

#[test]
fn replace_basic() {
    let mut b = TextBuffer::from_text("hello world");
    b.replace(0..5, "goodbye");
    assert_eq!(s(&b), "goodbye world");
}

#[test]
fn replace_empty_range_inserts() {
    let mut b = TextBuffer::from_text("hello world");
    b.replace(5..5, ",");
    assert_eq!(s(&b), "hello, world");
}

#[test]
fn replace_with_empty_deletes() {
    let mut b = TextBuffer::from_text("hello, world");
    b.replace(5..6, "");
    assert_eq!(s(&b), "hello world");
}

#[test]
fn repeated_inserts_preserve_text() {
    let mut b = TextBuffer::from_text("abc");
    for (i, ch) in "def".chars().enumerate() {
        b.insert(3 + i, &ch.to_string());
    }
    assert_eq!(s(&b), "abcdef");
}

#[test]
fn interleaved_insert_delete() {
    let mut b = TextBuffer::from_text("the quick brown fox");
    b.insert(0, "ADDED ");
    assert_eq!(s(&b), "ADDED the quick brown fox");
    b.delete(0..6);
    assert_eq!(s(&b), "the quick brown fox");
    b.replace(4..9, "slow");
    assert_eq!(s(&b), "the slow brown fox");
}

#[test]
fn multi_byte_utf8() {
    let mut b = TextBuffer::from_text("héllo"); // é is 2 bytes
    assert_eq!(b.len(), 6); // h(1)+é(2)+l(1)+l(1)+o(1)
    assert_eq!(s(&b), "héllo");
    b.insert(1, "X"); // insert at byte 1, before é
    assert_eq!(s(&b), "hXéllo");
    b.insert(4, "Y"); // insert between é and l (byte 4 = after é)
    assert_eq!(s(&b), "hXéYllo");
}

#[test]
fn read_range_multibyte() {
    let b = TextBuffer::from_text("αβγδε"); // 5 chars, 10 bytes
    assert_eq!(b.len(), 10);
    assert_eq!(b.read_range(0..2), "α");
    assert_eq!(b.read_range(2..4), "β");
    assert_eq!(b.read_range(0..10), "αβγδε");
    assert_eq!(b.read_range(2..6), "βγ");
}

#[test]
fn delete_multibyte() {
    let mut b = TextBuffer::from_text("αβγδε");
    b.delete(2..4); // delete β
    assert_eq!(s(&b), "αγδε");
    assert_eq!(b.len(), 8);
}

#[test]
fn piece_chain_grows_under_interior_edits() {
    // Multiple interior inserts force several splits; the chain grows
    // but the logical text stays correct.
    let mut b = TextBuffer::from_text("aaaa");
    b.insert(1, "b"); // "abaaa"
    b.insert(3, "c"); // "abacaa"
    b.insert(5, "d"); // "abacada"
    b.insert(7, "e"); // at end == len, append -> "abacadae"
    let mut e = String::from("aaaa");
    e.insert(1, 'b'); // "abaaa"
    e.insert(3, 'c'); // "abacaa"
    e.insert(5, 'd'); // "abacada"
    e.insert(7, 'e'); // "abacadae"
    assert_eq!(s(&b), e);
    assert_eq!(b.len(), 8);
    assert!(b.pieces.len() > 1);
}

#[test]
fn piece_ids_are_monotonic() {
    let mut b = TextBuffer::from_text("hello");
    let id0 = b.pieces[0].id;
    b.insert(2, "X");
    // The original split: left half retains id0, right half gets a
    // higher id; the inserted piece gets an even higher id.
    let ids: Vec<PieceId> = b.pieces.iter().map(|p| p.id).collect();
    assert!(ids.iter().all(|&id| id >= id0));
    // All ids distinct (counter never reuses).
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len());
}

#[test]
fn left_half_retains_id_on_split() {
    let mut b = TextBuffer::from_text("hello");
    let orig_id = b.pieces[0].id;
    b.insert(2, "X");
    // First piece is the left half: same id, shortened to len 2.
    assert_eq!(b.pieces[0].id, orig_id);
    assert_eq!(b.pieces[0].len, 2);
    assert_eq!(b.backing_slice(b.pieces[0]), "he");
}

#[test]
fn boundary_insert_elides_split() {
    let mut b = TextBuffer::from_text("hello");
    b.insert(0, "X");
    // No new piece from splitting — original piece intact, inserted piece
    // spliced in front.
    assert_eq!(b.pieces.len(), 2);
    assert_eq!(s(&b), "Xhello");

    let mut b = TextBuffer::from_text("hello");
    b.insert(5, "X");
    assert_eq!(b.pieces.len(), 2);
    assert_eq!(s(&b), "helloX");
}

#[test]
fn delete_crossing_multiple_pieces() {
    // Build a multi-piece buffer then delete across piece boundaries.
    let mut b = TextBuffer::from_text("abcdefghij");
    b.insert(3, "1"); // "abc1defghij"
    b.insert(7, "2"); // "abc1def2ghij"
    assert_eq!(s(&b), "abc1def2ghij");
    // Logical: a b c 1 d e f 2 g h i j
    //          0 1 2 3 4 5 6 7 8 9 ...
    // range 2..9 removes "c1def2g", leaving "ab" + "hij".
    b.delete(2..9);
    assert_eq!(s(&b), "abhij");
}

// ---- Line index ----------------------------------------------

/// Peek the byte line starts directly.
fn ls(b: &TextBuffer) -> Vec<usize> {
    b.line_starts.iter().map(|start| start.byte).collect()
}

/// Assert the incrementally maintained index equals a rebuild from text.
fn assert_index_matches_text(b: &TextBuffer) {
    let mut rebuilt = vec![BUFFER_START];
    scan_line_starts(&s(b), BUFFER_START, &mut rebuilt);
    assert_eq!(b.line_starts, rebuilt, "text {:?}", s(b));
}

#[test]
fn line_index_empty_buffer() {
    let b = TextBuffer::new();
    assert_eq!(b.line_count(), 1);
    assert_eq!(b.line_start(0), 0);
    assert_eq!(b.line_of_offset(0), 0);
    assert_eq!(ls(&b), &[0]);
}

#[test]
fn line_index_single_line() {
    let b = TextBuffer::from_text("hello");
    assert_eq!(b.line_count(), 1);
    assert_eq!(b.line_start(0), 0);
    for off in 0..=b.len() {
        assert_eq!(b.line_of_offset(off), 0, "offset {off}");
    }
    assert_eq!(ls(&b), &[0]);
}

#[test]
fn line_index_multi_line() {
    let b = TextBuffer::from_text("ab\ncd\nef");
    // Bytes: a(0) b(1) \n(2) c(3) d(4) \n(5) e(6) f(7). len 8.
    assert_eq!(b.line_count(), 3);
    assert_eq!(b.line_start(0), 0);
    assert_eq!(b.line_start(1), 3);
    assert_eq!(b.line_start(2), 6);
    // Offset → line mappings.
    assert_eq!(b.line_of_offset(0), 0);
    assert_eq!(b.line_of_offset(2), 0); // the '\n' belongs to line 0
    assert_eq!(b.line_of_offset(3), 1); // first byte of line 1
    assert_eq!(b.line_of_offset(5), 1); // trailing '\n' of line 1
    assert_eq!(b.line_of_offset(6), 2);
    assert_eq!(b.line_of_offset(8), 2); // one-past-end maps to last line
    assert_eq!(ls(&b), &[0, 3, 6]);
}

#[test]
fn line_index_trailing_newline_creates_empty_last_line() {
    let b = TextBuffer::from_text("abc\n");
    assert_eq!(b.line_count(), 2);
    assert_eq!(b.line_start(0), 0);
    assert_eq!(b.line_start(1), 4);
    assert_eq!(ls(&b), &[0, 4]);
}

#[test]
fn line_index_tracks_utf16_starts_across_edits() {
    let mut b = TextBuffer::from_text("a😀\nβ\n日本\n");
    assert_index_matches_text(&b);
    b.insert(1, "é\n🎉");
    assert_index_matches_text(&b);
    b.delete(0..1);
    assert_index_matches_text(&b);
    let len = b.len();
    b.replace(3..len - 1, "x\n😀\ny");
    assert_index_matches_text(&b);
    b.delete(0..b.len());
    assert_index_matches_text(&b);
    assert_eq!(b.len(), 0);
}

#[test]
fn utf16_conversions_round_trip_and_round_up_inside_pairs() {
    let b = TextBuffer::from_text("a😀\nβc");
    // a(0) 😀(1..5) \n(5) β(6..8) c(8); UTF-16: a 0, 😀 1..3, \n 3, β 4, c 5.
    for (byte, utf16) in [(0, 0), (1, 1), (5, 3), (6, 4), (8, 5), (9, 6)] {
        assert_eq!(b.utf16_of_offset(byte), utf16, "byte {byte}");
        assert_eq!(b.offset_of_utf16(utf16), byte, "utf16 {utf16}");
    }
    assert_eq!(b.offset_of_utf16(2), 5, "inside the surrogate pair");
    assert_eq!(b.offset_of_utf16(100), b.len());
}

#[test]
fn line_index_insert_with_newlines_interior() {
    let mut b = TextBuffer::from_text("abcdef"); // starts [0]
    b.insert(3, "X\nY");
    // "abcX\nYdef": a(0) b(1) c(2) X(3) \n(4) Y(5) d(6) e(7) f(8). len 9.
    assert_eq!(s(&b), "abcX\nYdef");
    assert_eq!(b.line_count(), 2);
    assert_eq!(b.line_start(0), 0);
    assert_eq!(b.line_start(1), 5);
    assert_eq!(ls(&b), &[0, 5]);
}

#[test]
fn line_index_insert_at_existing_line_start() {
    // Inserting right at a line start must preserve that line start.
    let mut b = TextBuffer::from_text("ab\ncd"); // starts [0, 3]
    b.insert(3, "x"); // "ab\nxcd"
    assert_eq!(s(&b), "ab\nxcd");
    assert_eq!(b.line_count(), 2);
    assert_eq!(b.line_start(0), 0);
    assert_eq!(b.line_start(1), 3);
    assert_eq!(ls(&b), &[0, 3]);
}

#[test]
fn line_index_insert_newline_at_offset_zero() {
    let mut b = TextBuffer::from_text("abc\ndef\nghi"); // [0, 4, 8]
    b.insert(0, "\n");
    // "\nabc\ndef\nghi" → \n(0) a(1) b(2) c(3) \n(4) d(5) e(6) f(7) \n(8) g(9) h(10) i(11)
    assert_eq!(s(&b), "\nabc\ndef\nghi");
    assert_eq!(b.line_count(), 4);
    assert_eq!(ls(&b), &[0, 1, 5, 9]);
}

#[test]
fn line_index_delete_removes_newline_in_span() {
    let mut b = TextBuffer::from_text("abc\ndef\nghi"); // [0, 4, 8]
    // Delete "\ndef" (offsets 3..8), collapse to one line "abcghi"
    b.delete(3..8);
    assert_eq!(s(&b), "abcghi");
    assert_eq!(b.line_count(), 1);
    assert_eq!(ls(&b), &[0]);
}

#[test]
fn line_index_delete_to_end_keeps_trailing_empty_line() {
    // "ab\ncd" → delete [3, 5) (deletes "cd"), leaving "ab\n".
    // The trailing '\n' demarcates an empty final line.
    let mut b = TextBuffer::from_text("ab\ncd"); // [0, 3]
    b.delete(3..5);
    assert_eq!(s(&b), "ab\n");
    assert_eq!(b.line_count(), 2);
    assert_eq!(ls(&b), &[0, 3]);
}

#[test]
fn line_index_delete_entire_buffer() {
    let mut b = TextBuffer::from_text("ab\ncd\nef"); // [0, 3, 6]
    b.delete(0..8);
    assert_eq!(s(&b), "");
    assert!(b.is_empty());
    assert_eq!(b.line_count(), 1);
    assert_eq!(ls(&b), &[0]);
}

#[test]
fn line_index_delete_inside_one_line_no_change_to_index() {
    let mut b = TextBuffer::from_text("ab\ncd\nef"); // [0, 3, 6]
    b.delete(4..5); // delete 'd' from line 1 → "ab\nc\nef"
    assert_eq!(s(&b), "ab\nc\nef");
    assert_eq!(b.line_count(), 3);
    assert_eq!(ls(&b), &[0, 3, 5]); // line 2 shifted by -1
}

#[test]
fn line_index_replace_with_multiline_text() {
    let mut b = TextBuffer::from_text("abc\ndef\nghi"); // [0, 4, 8]
    // Replace "\ndef" → "X\nY": result "abcX\nYghi", starts [0, 5]
    b.replace(3..8, "X\nY");
    assert_eq!(s(&b), "abcX\nYghi");
    assert_eq!(b.line_count(), 2);
    assert_eq!(ls(&b), &[0, 5]);
}

#[test]
fn line_index_replace_collapsing_lines() {
    let mut b = TextBuffer::from_text("abc\ndef\nghi"); // [0, 4, 8]
    // Replace "\ndef\n" (offsets 3..8) with "": result "abcghi", 1 line.
    b.replace(3..8, "");
    assert_eq!(s(&b), "abcghi");
    assert_eq!(b.line_count(), 1);
    assert_eq!(ls(&b), &[0]);
}

#[test]
fn line_index_replace_inserts_new_lines_at_start() {
    let mut b = TextBuffer::from_text("abc"); // [0]
    // Replace whole buffer with multi-line content.
    b.replace(0..3, "ab\ncd\nef");
    assert_eq!(s(&b), "ab\ncd\nef");
    assert_eq!(b.line_count(), 3);
    assert_eq!(ls(&b), &[0, 3, 6]);
}

#[test]
fn line_index_append_newline_at_end() {
    let mut b = TextBuffer::from_text("abc"); // [0]
    b.insert(3, "\n"); // "abc\n" → [0, 4]
    assert_eq!(b.line_count(), 2);
    assert_eq!(ls(&b), &[0, 4]);
}

#[test]
fn line_index_survives_many_random_edits() {
    // Stress: build a buffer, do many inserts/deletes of '\n'-bearing
    // spans, and verify the index matches a fresh scan of the text.
    let text = "the quick brown fox\njumps over\nthe lazy dog\n";
    let mut b = TextBuffer::from_text(text);

    let mut rng_state: u64 = 0xdead_beef;
    let mut next_rand = || {
        // xorshift64
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        rng_state
    };
    for _ in 0..200 {
        let len = b.len();
        let start = (next_rand() % (len as u64 + 1)) as usize;
        let end = (next_rand() % (len as u64 + 1)) as usize;
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        // Pick one of a few candidate replacements.
        let pick = next_rand() % 4;
        let new_text = match pick {
            0 => "",
            1 => "x",
            2 => "\n",
            _ => "ab\ncd",
        };
        // Only apply if both ends are char boundaries (we use ASCII
        // source text + ASCII inserts, so every offset is a boundary).
        b.replace(start..end, new_text);
    }

    // Re-derive expected line starts from the final text.
    let final_text = s(&b);
    let mut expected: Vec<usize> = vec![0];
    for (i, byte) in final_text.as_bytes().iter().enumerate() {
        if *byte == b'\n' {
            expected.push(i + 1);
        }
    }
    assert_eq!(b.line_count(), expected.len(), "line count mismatch");
    assert_eq!(ls(&b), &expected[..], "line starts mismatch");
}

// ---- Position tokens -------------------------------------

#[test]
fn position_at_empty_buffer_returns_none() {
    let b = TextBuffer::new();
    assert!(b.position_at(0).is_none());
}

#[test]
fn position_at_zero_is_first_piece_offset_zero() {
    let b = TextBuffer::from_text("abc");
    let p = b.position_at(0).unwrap();
    assert_eq!(p.piece(), b.pieces[0].id);
    assert_eq!(p.offset(), 0);
    assert_eq!(b.resolve(p), Some(0));
}

#[test]
fn position_at_end_is_last_piece_len_sticky_left() {
    // Single piece "abc": at==3 returns (piece0, 3).
    let b = TextBuffer::from_text("abc");
    let p = b.position_at(3).unwrap();
    assert_eq!(p.piece(), b.pieces[0].id);
    assert_eq!(p.offset(), 3);
    assert_eq!(b.resolve(p), Some(3));
}

#[test]
fn position_at_interior_of_piece() {
    let b = TextBuffer::from_text("abcde");
    let p = b.position_at(2).unwrap();
    assert_eq!(p.piece(), b.pieces[0].id);
    assert_eq!(p.offset(), 2);
    assert_eq!(b.resolve(p), Some(2));
}

#[test]
fn position_at_piece_boundary_is_sticky_left() {
    // Two pieces from prior edits; insert a boundary to make two
    // pieces. After "ab" + insert(2, "cd") + insert(2, "X") we have:
    //   piece A "ab" (id 1)
    //   piece C "X"  (id 4 — the latest insert)
    //   piece B "cd" (id 3)
    // Boundary between A and C is offset 2; sticky-left → (A, 2).
    let mut b = TextBuffer::from_text("ab");
    b.insert(2, "cd"); // "abcd": A=ab, B=cd
    b.insert(2, "X"); // "abXcd": elided inserts in front of B
    // Layout: pieces = [A(ab), X("X"), B(cd)]. Find offsets.
    // A.len=2, X.len=1, B.len=2 → total 5.
    assert_eq!(s(&b), "abXcd");
    let boundary_offset = 2; // between A and X
    let p = b.position_at(boundary_offset).unwrap();
    // Sticky-left → anchored to A whose end is at offset 2.
    assert_eq!(p.piece(), b.pieces[0].id, "boundary should be sticky-left");
    assert_eq!(p.offset(), 2);
    assert_eq!(b.resolve(p), Some(2));
}

#[test]
fn position_round_trips_through_resolve_for_all_offsets() {
    let b = TextBuffer::from_text("ab\ncd\nef");
    for off in 0..=b.len() {
        let p = b.position_at(off).unwrap();
        assert_eq!(b.resolve(p), Some(off), "offset {off}");
    }
}

#[test]
fn resolve_unknown_piece_id_returns_none() {
    let b = TextBuffer::from_text("abc");
    let bogus = Position {
        piece: 999,
        offset: 0,
    };
    assert!(b.resolve(bogus).is_none());
}

#[test]
fn resolve_offset_past_piece_len_returns_none() {
    // Construct a stale position past the piece's current length.
    let b = TextBuffer::from_text("abc");
    let stale = Position {
        piece: b.pieces[0].id,
        offset: 99,
    };
    assert!(b.resolve(stale).is_none());
}

#[test]
fn left_half_position_survives_interior_split_unchanged() {
    // "hello" → insert "X" at offset 2 splits the original piece:
    // left half retains id (len 2), right half is a fresh piece.
    // A position anchored to the left half stays valid and resolves
    // to the same byte offset.
    let mut b = TextBuffer::from_text("hello");
    let left_pos = b.position_at(1).unwrap(); // (id0, 1) — left half
    b.insert(2, "X");
    assert_eq!(s(&b), "heXllo");
    // (id0, 1) is still valid: id0 still exists with len 2.
    assert_eq!(b.resolve(left_pos), Some(1));
}

#[test]
fn right_half_position_becomes_stale_after_split() {
    // "hello" (id0, len 5). A position at offset 4 sits in the right
    // half of an interior split at offset 2: id0 is shortened to len 2,
    // so (id0, 4) is now stale (offset > current len).
    let mut b = TextBuffer::from_text("hello");
    let right_pos = b.position_at(4).unwrap(); // (id0, 4) — right half
    assert_eq!(right_pos.piece(), b.pieces[0].id);
    assert_eq!(right_pos.offset(), 4);
    b.insert(2, "X");
    // id0 has len 2 now; (id0, 4) is stale → resolve None.
    assert_eq!(b.resolve(right_pos), None);
}

#[test]
fn position_in_deleted_piece_resolves_none() {
    // Build a multi-piece buffer so we can have the token anchored in
    // a piece that gets drained wholesale by a later delete.
    let mut b = TextBuffer::from_text("hello");
    b.insert(5, "world"); // pieces: id0 "hello", id1 "world"
    assert_eq!(s(&b), "helloworld");
    // Position anchored in id1 at offset 2 (= logical offset 7).
    let p = b.position_at(7).unwrap();
    assert_eq!(p.piece(), b.pieces[1].id);
    // Delete from offset 5 to end (whole of id1).
    b.delete(5..10);
    assert_eq!(s(&b), "hello");
    // id1 no longer exists → resolve returns None.
    assert!(b.resolve(p).is_none());
}

#[test]
fn position_survives_unrelated_edit() {
    // A position is stable across edits elsewhere: insert at one end
    // of the buffer, position anchored at the other end stays valid.
    let mut b = TextBuffer::from_text("abc");
    let p = b.position_at(0).unwrap();
    b.insert(3, "XYZ");
    assert_eq!(s(&b), "abcXYZ");
    assert_eq!(b.resolve(p), Some(0));
}

// ---- Edit log: BufferEdit surface ---------------------------------

#[test]
fn edit_seq_starts_at_zero_and_increments() {
    let mut b = TextBuffer::from_text("abc");
    assert_eq!(b.edit_seq(), 0);
    b.insert(3, "X");
    assert_eq!(b.edit_seq(), 1);
    b.delete(0..1);
    assert_eq!(b.edit_seq(), 2);
    // No-op edits don't push events.
    b.insert(0, "");
    b.delete(2..2);
    assert_eq!(b.edit_seq(), 2);
}

#[test]
fn edits_since_returns_incremental_slice() {
    let mut b = TextBuffer::from_text("abc");
    b.insert(3, "X");
    b.insert(0, "Y");
    let after_one = b.edit_seq() - 1;
    let latest = b.edits_since(after_one);
    assert_eq!(latest.len(), 1);
    assert!(matches!(latest[0], BufferEdit::Insert { at: 0, .. }));
    // Earlier edits still visible too.
    assert_eq!(b.edits_since(0).len(), 2);
    // Stale cursor past the end → empty slice (clamped).
    assert!(b.edits_since(99).is_empty());
}

#[test]
fn discarding_edits_keeps_sequence_numbers() {
    let mut b = TextBuffer::from_text("abc");
    b.insert(3, "X");
    b.delete(0..1);
    b.discard_edits_before(1);
    assert_eq!(b.edit_seq(), 2);
    assert_eq!(b.edits_since(1).len(), 1);
    assert!(matches!(b.edits_since(1)[0], BufferEdit::Delete { .. }));
    b.discard_edits_before(b.edit_seq());
    assert!(b.edits_since(2).is_empty());
    b.insert(0, "Y");
    assert_eq!(b.edit_seq(), 3);
    assert!(matches!(
        b.edits_since(2)[0],
        BufferEdit::Insert { at: 0, .. }
    ));
}

#[test]
#[should_panic(expected = "were discarded")]
fn reading_discarded_edits_panics() {
    let mut b = TextBuffer::from_text("abc");
    b.insert(3, "X");
    b.discard_edits_before(1);
    b.edits_since(0);
}

#[test]
fn insert_emits_insert_event_with_correct_at_and_len() {
    let mut b = TextBuffer::from_text("abc");
    // Insert at the very end: boundary-elided, no split.
    b.insert(3, "XY");
    let edits = b.edits_since(0);
    assert_eq!(edits.len(), 1);
    match &edits[0] {
        BufferEdit::Insert {
            at,
            inserted_len,
            splits,
            ..
        } => {
            assert_eq!(*at, 3);
            assert_eq!(*inserted_len, 2);
            assert!(splits.is_empty(), "boundary insert must not split");
        }
        other => panic!("expected Insert, got {other:?}"),
    }
}

#[test]
fn contiguous_typing_coalesces_into_one_piece() {
    let mut b = TextBuffer::new();
    for (at, ch) in "hello".char_indices() {
        b.insert(at, &ch.to_string());
    }
    assert_eq!(b.pieces.len(), 1);
    let inside = b.position_at(2).unwrap();
    let end = b.position_at(5).unwrap();
    b.insert(5, "!");
    assert_eq!(b.pieces.len(), 1);
    assert_eq!(s(&b), "hello!");
    assert_eq!(b.resolve(inside), Some(2));
    assert_eq!(b.resolve(end), Some(5), "sticky-left end stays before");
    match b.edits_since(0).last() {
        Some(BufferEdit::Insert { inserted_end, .. }) => {
            assert_eq!(*inserted_end, Position::new(b.pieces[0].id, 6));
        }
        other => panic!("expected Insert, got {other:?}"),
    }
}

#[test]
fn inserts_not_at_the_sink_end_do_not_coalesce() {
    // Original-backed pieces never extend.
    let mut b = TextBuffer::from_text("ab");
    b.insert(2, "c");
    assert_eq!(b.pieces.len(), 2);
    // A typed piece no longer ends at the sink once something else was
    // appended.
    b.insert(0, "x");
    b.insert(4, "d");
    assert_eq!(b.pieces.len(), 4);
    b.insert(5, "e");
    assert_eq!(b.pieces.len(), 4, "\"d\" ends at the sink and extends");
    // The left half of a split typed piece never extends, because its
    // right half references the sink after it.
    b.insert(5, "f");
    assert_eq!(b.pieces.len(), 6);
    b.insert(5, "g");
    assert_eq!(b.pieces.len(), 7);
    assert_eq!(s(&b), "xabcdgfe");
}

#[test]
fn interior_insert_emits_one_split() {
    let mut b = TextBuffer::from_text("hello");
    let id0 = b.pieces[0].id;
    b.insert(2, "X"); // strict interior split at offset 2
    let edits = b.edits_since(0);
    match &edits[0] {
        BufferEdit::Insert { splits, .. } => {
            assert_eq!(splits.len(), 1);
            assert_eq!(
                splits[0],
                Split {
                    old_piece: id0,
                    split_offset: 2,
                    new_piece: id0 + 1,
                }
            );
        }
        other => panic!("expected Insert, got {other:?}"),
    }
}

#[test]
fn boundary_insert_emits_no_splits() {
    let mut b = TextBuffer::from_text("hello");
    b.insert(0, "X"); // boundary, elided
    match &b.edits_since(0)[0] {
        BufferEdit::Insert { splits, .. } => {
            assert!(splits.is_empty(), "boundary insert must not split");
        }
        other => panic!("expected Insert, got {other:?}"),
    }
    b.insert(b.len(), "Y"); // insert at end, no split
    // Two insert events, both with empty splits.
    for e in b.edits_since(0) {
        if let BufferEdit::Insert { splits, .. } = e {
            assert!(splits.is_empty());
        }
    }
}

#[test]
fn delete_emits_delete_event_with_correct_range() {
    let mut b = TextBuffer::from_text("hello world");
    // Delete the whole content: both ends are piece-boundary-elided,
    // so the event carries the correct `range` and zero splits.
    b.delete(0..11);
    match &b.edits_since(0)[0] {
        BufferEdit::Delete { range, splits, .. } => {
            assert_eq!(*range, 0..11);
            assert!(splits.is_empty(), "whole-buffer delete must not split");
        }
        other => panic!("expected Delete, got {other:?}"),
    }
    assert!(b.is_empty());
}

#[test]
fn delete_with_two_interior_ends_emits_two_splits() {
    let mut b = TextBuffer::from_text("abcdefghij");
    let id0 = b.pieces[0].id;
    // Two interior splits: range.start=2 on the original piece, and
    // range.end=8 on the right half (R1) created by the first split.
    // R1 starts at logical offset 2 with len 8; the second split is
    // therefore at R1's offset 8-2 = 6.
    b.delete(2..8);
    let r1_id = id0 + 1; // first split's right-half piece
    let r2_id = id0 + 2; // second split's right-half piece
    match &b.edits_since(0)[0] {
        BufferEdit::Delete { range, splits, .. } => {
            assert_eq!(*range, 2..8);
            assert_eq!(splits.len(), 2);
            assert_eq!(splits[0].old_piece, id0);
            assert_eq!(splits[0].split_offset, 2);
            assert_eq!(splits[0].new_piece, r1_id);
            assert_eq!(splits[1].old_piece, r1_id);
            assert_eq!(splits[1].split_offset, 6);
            assert_eq!(splits[1].new_piece, r2_id);
        }
        other => panic!("expected Delete, got {other:?}"),
    }
}

#[test]
fn replace_emits_delete_then_insert_pair() {
    let mut b = TextBuffer::from_text("hello world");
    b.replace(0..5, "goodbye");
    let edits = b.edits_since(0);
    assert_eq!(edits.len(), 2);
    assert!(matches!(edits[0], BufferEdit::Delete { .. }));
    match &edits[1] {
        BufferEdit::Insert {
            at, inserted_len, ..
        } => {
            assert_eq!(*at, 0);
            assert_eq!(*inserted_len, "goodbye".len());
        }
        other => panic!("expected Insert, got {other:?}"),
    }
    assert_eq!(s(&b), "goodbye world");
}

#[test]
fn edit_points_describe_the_text_each_edit_saw() {
    fn point(text: &str, offset: usize) -> TextPoint {
        let before = &text[..offset];
        let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
        TextPoint {
            line: before.matches('\n').count(),
            column: offset - line_start,
        }
    }

    let mut b = TextBuffer::from_text("one\ntwo\nthree\n");
    let mut text = s(&b);
    let mut rng_state: u64 = 0x5eed_1234;
    let mut next_rand = || {
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        rng_state
    };
    for _ in 0..300 {
        let seq = b.edit_seq();
        let len = b.len() as u64 + 1;
        let (start, end) = {
            let (a, b) = ((next_rand() % len) as usize, (next_rand() % len) as usize);
            (a.min(b), a.max(b))
        };
        let insert = ["", "x", "\n", "ab\ncd\n"][(next_rand() % 4) as usize];
        b.replace(start..end, insert);

        for edit in b.edits_since(seq) {
            match edit {
                BufferEdit::Delete {
                    range,
                    start_point,
                    end_point,
                    ..
                } => {
                    assert_eq!(*start_point, point(&text, range.start));
                    assert_eq!(*end_point, point(&text, range.end));
                    text.replace_range(range.clone(), "");
                }
                BufferEdit::Insert {
                    at,
                    inserted_len,
                    at_point,
                    end_point,
                    ..
                } => {
                    assert_eq!(*at_point, point(&text, *at));
                    text.insert_str(*at, insert);
                    assert_eq!(*end_point, point(&text, at + inserted_len));
                }
            }
        }
        assert_eq!(text, s(&b));
    }
}

// ---- End-to-end remap-by-log (the design payoff of the edit log) -----------

/// Simulate the anchored-range store: hold a position token across an interior
/// split, find it stale, walk the edit log, remap it to the new
/// piece, and confirm the remapped token resolves to the same content
/// the original token pointed at.
#[test]
fn stale_right_half_position_remaps_via_edit_log() {
    let mut b = TextBuffer::from_text("hello");
    // Token at byte offset 3 (the second 'l').
    let original = b.position_at(3).unwrap();
    assert_eq!(original.offset(), 3);
    let original_byte = b.resolve(original).unwrap();
    let content_before: char = b
        .read_range(original_byte..original_byte + 1)
        .chars()
        .next()
        .unwrap();
    assert_eq!(content_before, 'l');

    // Split at offset 2: original piece's right half moves to a new
    // piece; (id0, 4) becomes stale.
    b.insert(2, "X");
    assert_eq!(s(&b), "heXllo");

    // Stale detection.
    assert!(b.resolve(original).is_none());

    // Walk the edit log to find the matching Split and remap.
    let remapped = remap_via_log(&b, original).expect("log should contain a matching Split");
    // The remapped token should resolve to the same byte the
    // original would have resolved to before the edit, *plus* the
    // inserted span's length, since the insertion happened before
    // our position (at offset 2 < 4). Note: the design's remap
    // relocates us within the *new* piece; the byte offset shifts
    // by the inserted span length because the insert was at a
    // smaller offset than our position.
    let remapped_byte = b.resolve(remapped).unwrap();
    let content_after: char = b
        .read_range(remapped_byte..remapped_byte + 1)
        .chars()
        .next()
        .unwrap();
    assert_eq!(
        content_after, content_before,
        "remapped token must point at same content"
    );
    // And specifically the byte offset should have advanced by 1
    // (the inserted 'X').
    assert_eq!(remapped_byte, original_byte + 1);
}

/// Step-5-style helper: given a stale `Position`, walk the buffer's
/// edit log forward and apply the split remap rule for any matching
/// `Split`. Returns the remapped token, or `None` if no remap applies
/// (the position was deleted wholesale, or no longer stale).
fn remap_via_log(b: &TextBuffer, mut p: Position) -> Option<Position> {
    for edit in b.edits_since(0) {
        let splits = match edit {
            BufferEdit::Insert { splits, .. } | BufferEdit::Delete { splits, .. } => splits,
        };
        for sp in splits {
            if sp.old_piece == p.piece && (p.offset as usize) > sp.split_offset {
                p = Position {
                    piece: sp.new_piece,
                    offset: p.offset - sp.split_offset as u32,
                };
            }
        }
    }
    Some(p)
}

#[test]
fn remap_walks_multiple_splits_in_order() {
    // Two interior splits on the same original piece: the original
    // right half (which became a fresh piece) is split again later.
    // A position at the far right of the original piece should
    // remap through both splits.
    let mut b = TextBuffer::from_text("abcdefghij"); // id0, len 10
    let original = b.position_at(9).unwrap(); // anchored at id0, offset 9
    // Split at offset 2: id0 shortens to 2; right half (offsets 2..10) becomes a fresh piece R1.
    b.insert(2, "X"); // "abXcdefghij"
    // Now split R1 at its interior by inserting at offset 5 (within R1, offset 5 - 3 = 2).
    // Logical offset 5 is bytes 0..2 = "ab", "X", then within R1 ("cdefghij", len 8) offset 5 - 3 = 2.
    b.insert(5, "Y"); // "abXcYdefghij"
    // The original token (id0, 9) is stale after the first split;
    // remapping gives (R1, 9-2=7). R1 is then split at its offset 2,
    // so we remap again to (R2, 7-2=5). The final byte offset should
    // point at the 'j' (the last byte).
    assert!(b.resolve(original).is_none());
    let remapped = remap_via_log(&b, original).unwrap();
    let final_byte = b.resolve(remapped).unwrap();
    assert_eq!(
        b.read_range(final_byte..final_byte + 1),
        "j",
        "remapped token must point at 'j'"
    );
}

#[test]
fn full_add_tail_seals_and_stops_coalescing() {
    let mut b = TextBuffer::new();
    b.insert(0, &"a".repeat(ADD_CHUNK_SIZE - 1));
    assert!(b.sealed.is_empty());
    b.insert(b.len(), "b");
    assert_eq!(b.sealed.len(), 1, "a full tail seals");
    assert!(b.tail.is_empty());
    assert_eq!(b.pieces.len(), 1, "the sealing insert still coalesced");

    b.insert(b.len(), "c");
    b.insert(b.len(), "d");
    assert_eq!(b.pieces.len(), 2, "typing starts a piece in the new tail");
    assert_eq!(b.read_range(ADD_CHUNK_SIZE - 1..b.len()), "bcd");

    b.insert(1, &"z".repeat(3 * ADD_CHUNK_SIZE));
    assert_eq!(b.sealed.len(), 2, "an oversized insert seals at once");
    assert_eq!(b.len(), 4 * ADD_CHUNK_SIZE + 2);
    assert_eq!(b.read_range(0..3), "azz");
}
