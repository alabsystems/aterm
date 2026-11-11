// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn contracted_scalar_core_matches_the_std_forms_it_replaced() {
    for byte in u8::MIN..=u8::MAX {
        assert_eq!(utf8_char_start(byte), !(0x80..=0xBF).contains(&byte));
        for other in u8::MIN..=u8::MAX {
            assert_eq!(taller_child(byte, other), byte.max(other));
        }
    }
    let source = "a\u{e9}\u{20ac}\u{1f980}\n".repeat(400);
    let rope = TextRope::from(source.as_str());
    for offset in 0..=source.len().saturating_add(2) {
        assert_eq!(
            rope.is_char_boundary(offset),
            source.is_char_boundary(offset)
        );
    }
}

fn collect_leaf_lengths(node: &Node, out: &mut Vec<usize>) {
    match node {
        Node::Empty => {}
        Node::Leaf { text, newlines } => {
            assert!(!text.is_empty());
            assert!(text.len() <= CHUNK_MERGE_MAX);
            assert_eq!(*newlines, text.bytes().filter(|&b| b == b'\n').count());
            out.push(text.len());
        }
        Node::Branch {
            left,
            right,
            bytes: b,
            newlines: nl,
            height: h,
        } => {
            assert!(bytes(left) > 0 && bytes(right) > 0);
            assert_eq!(*b, bytes(left) + bytes(right));
            assert_eq!(*nl, newlines(left) + newlines(right));
            assert_eq!(*h, taller_child(height(left), height(right)) + 1);
            let factor = i16::from(height(left)) - i16::from(height(right));
            assert!(
                (-1..=1).contains(&factor),
                "strict AVL violated: left={} right={}",
                height(left),
                height(right),
            );
            collect_leaf_lengths(left, out);
            collect_leaf_lengths(right, out);
        }
    }
}

fn assert_rope_invariants(rope: &TextRope) {
    let mut leaves = Vec::new();
    collect_leaf_lengths(&rope.root, &mut leaves);
    assert_eq!(leaves.len(), rope.chunk_count());
    for pair in leaves.windows(2) {
        assert!(
            pair[0] + pair[1] > CHUNK_MERGE_MAX,
            "adjacent leaves {} + {} <= CHUNK_MERGE_MAX ({CHUNK_MERGE_MAX})",
            pair[0],
            pair[1],
        );
    }
    if !leaves.is_empty() {
        let max_h = 2 * (leaves.len().ilog2() as usize + 1);
        assert!(
            usize::from(rope.height()) <= max_h,
            "height {} exceeds {max_h} for {} chunks",
            rope.height(),
            leaves.len(),
        );
    }
}

#[test]
fn large_text_is_chunked_balanced_and_round_trips() {
    let source = (0..5_000)
        .map(|index| format!("line {index} — 🦀\n"))
        .collect::<String>();
    let rope = TextRope::from(source.as_str());
    assert_rope_invariants(&rope);
    assert!(rope.chunk_count() > 2);
    assert!(usize::from(rope.height()) <= rope.chunk_count().ilog2() as usize + 2);
    assert_eq!(rope.to_flat_string(), source);
    assert_eq!(rope.line_count(), 5_001);

    for chunks in [1usize, 2, 3, 5, 9, 17, 33] {
        let sample = "x".repeat(chunks * CHUNK_TARGET + 1);
        let built = TextRope::from(sample.as_str());
        assert_rope_invariants(&built);
        assert_eq!(built.to_flat_string(), sample);
    }
}

#[test]
fn repeated_edits_preserve_strict_avl_balance_and_coalesce_small_leaves() {
    let mut expected = "abcd\n".repeat(40_960);
    let mut rope = TextRope::from(expected.as_str());
    assert_rope_invariants(&rope);

    for step in 0..300usize {
        let at = 4_097 + step;
        let ch = if step % 2 == 0 { "x" } else { "\n" };
        expected.replace_range(at..at, ch);
        rope = rope.replace(at..at, ch).unwrap();
        if step % 50 == 49 {
            assert_rope_invariants(&rope);
        }
    }
    for step in 0..300usize {
        let at = expected.len() - 13;
        let ch = if step % 3 == 0 { "y" } else { "z" };
        expected.replace_range(at..at, ch);
        rope = rope.replace(at..at, ch).unwrap();
        if step % 50 == 49 {
            assert_rope_invariants(&rope);
        }
    }
    assert_rope_invariants(&rope);
    assert_eq!(rope.to_flat_string(), expected);

    for step in 0..400usize {
        let at = 8_200 + (step * 37) % 60_000;
        expected.replace_range(at..at + 1, "q");
        rope = rope.replace(at..at + 1, "q").unwrap();
    }
    assert_rope_invariants(&rope);
    assert_eq!(rope.to_flat_string(), expected);
}

#[test]
fn persistent_replace_shares_old_snapshot_semantics() {
    let original = TextRope::from("alpha\nbeta\ngamma");
    let changed = original.replace(6..10, "BETA!!!").unwrap();
    assert_eq!(original.to_flat_string(), "alpha\nbeta\ngamma");
    assert_eq!(changed.to_flat_string(), "alpha\nBETA!!!\ngamma");
}

#[test]
fn multi_edit_uses_original_coordinates() {
    let rope = TextRope::from("one two three");
    let changed = rope
        .replace_many(&[(0..3, "1"), (4..7, "2"), (8..13, "3")])
        .unwrap();
    assert_eq!(changed.to_flat_string(), "1 2 3");
}

#[test]
fn invalid_utf8_boundary_is_rejected_without_mutation() {
    let rope = TextRope::from("aé🦀z");
    assert_eq!(rope.replace(2..3, "x"), Err(TextRopeError::InvalidRange));
    assert_eq!(rope.to_flat_string(), "aé🦀z");
}
