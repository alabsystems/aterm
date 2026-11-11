// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::LendReach;
use crate::{AnchorRole, Grid};

/// A grid holding `lines` (each its own logical line, `\n`-separated as a
/// shell prints them), the cursor left on the row after the last.
fn grid_printing(rows: u16, cols: u16, lines: &[&str]) -> Grid {
    let mut g = Grid::with_scrollback(rows, cols, 1000);
    for line in lines {
        for ch in line.chars() {
            g.write_char_wrap(ch);
        }
        g.carriage_return();
        g.line_feed();
    }
    g
}

fn row_text(g: &Grid, abs: u64) -> String {
    let row = g.live_row_at_absolute(abs).expect("a readable row");
    row.as_slice()
        .iter()
        .map(|c| char::from_u32(u32::from(c.char_data())).unwrap_or('?'))
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn a_widening_rewrap_lands_each_head_anchor_on_its_line() {
    let mut g = grid_printing(6, 8, &["one", "abcdefghij", "card", "tail"]);
    let base = g.visible_to_absolute(0);
    let card = (0..6)
        .map(|r| base + r)
        .find(|&r| row_text(&g, r) == "card")
        .unwrap();
    let start = g.anchors_mut().place(card, AnchorRole::Start).unwrap();
    let end = g.anchors_mut().place(card + 1, AnchorRole::End).unwrap();
    let lend = g.lend_anchors(LendReach::Ring).unwrap();
    assert!(
        g.anchors().is_empty(),
        "the resize runs with no anchor in the grid"
    );
    g.resize(6, 20);
    g.take_back_anchors(lend);
    assert!(!g.anchors().lost());
    let s = g.anchors().row(start).unwrap();
    assert_eq!(row_text(&g, s), "card");
    assert_eq!(g.anchors().row(end), Some(s + 1));

    let mut g = grid_printing(6, 8, &["one", "abcdefghij", "card", "tail"]);
    let cursor = g.cursor();
    g.set_cursor(0, 0);
    g.write_emoji_autowrap_fast(
        '😀',
        crate::PackedColors::DEFAULT,
        crate::CellFlags::COMPLEX,
    );
    g.set_cursor(cursor.row, cursor.col);
    let base = g.visible_to_absolute(0);
    let card = (0..6)
        .map(|r| base + r)
        .find(|&r| row_text(&g, r) == "card")
        .unwrap();
    let start = g.anchors_mut().place(card, AnchorRole::Start).unwrap();
    let end = g.anchors_mut().place(card + 1, AnchorRole::End).unwrap();
    let lend = g.lend_anchors(LendReach::Ring).unwrap();
    g.resize(6, 20);
    g.take_back_anchors(lend);
    assert!(
        !g.anchors().lost(),
        "unchanged resolved text survived reflow"
    );
    let start = g.anchors().row(start).unwrap();
    assert_eq!(row_text(&g, start), "card");
    assert_eq!(g.anchors().row(end), Some(start + 1));

    for (row, placeable) in [(u64::MAX - 1, false), (4, true)] {
        let mut g = Grid::with_scrollback(4, 10, 100);
        assert_eq!(g.visible_to_absolute(0), 0);
        let start = g.anchors_mut().place(row, AnchorRole::Start).unwrap();
        let lend = g.lend_anchors(LendReach::Ring).unwrap();
        g.resize(4, 12);
        g.take_back_anchors(lend);
        assert_eq!(!g.anchors().lost(), placeable);
        if placeable {
            assert_eq!(
                g.anchors().row(start),
                Some(1),
                "past-screen boundary follows the blank gap"
            );
        }
    }

    let mut g = Grid::with_scrollback(8, 10, 100);
    let start = g.anchors_mut().place(8, AnchorRole::Start).unwrap();
    g.resize(4, 10);
    assert_eq!(g.visible_to_absolute(0), 0, "precondition: a trim");
    assert_eq!(g.anchors().row(start), Some(8), "precondition: past it");
    let lend = g.lend_anchors(LendReach::Ring).unwrap();
    g.resize(4, 12);
    g.take_back_anchors(lend);
    assert!(!g.anchors().lost(), "a trimmed-past Start is no unknown");
    assert_eq!(g.anchors().row(start), Some(1), "after the cursor's line");

    let mut g = Grid::with_scrollback(4, 10, 100);
    g.set_cursor(1, 0);
    g.write_char_at_cursor_packed(
        'p',
        crate::PackedColors::DEFAULT,
        crate::CellFlags::PROTECTED,
    );
    g.set_cursor(0, 0);
    let start = g.anchors_mut().place(3, AnchorRole::Start).unwrap();
    let lend = g.lend_anchors(LendReach::Ring).unwrap();
    g.resize(4, 12);
    g.take_back_anchors(lend);
    assert!(!g.anchors().lost());
    assert_eq!(
        g.anchors().row(start),
        Some(4),
        "protected ink is not a blank gap"
    );

    let mut g = Grid::with_scrollback(4, 8, 1000);
    g.write_emoji_autowrap_fast(
        '😀',
        crate::PackedColors::DEFAULT,
        crate::CellFlags::COMPLEX,
    );
    g.cell_extra_mut(0, 0).add_combining('\u{301}');
    g.carriage_return();
    g.line_feed();
    for line in ["one", "abcdefghij", "card", "tail"] {
        for ch in line.chars() {
            g.write_char_wrap(ch);
        }
        g.carriage_return();
        g.line_feed();
    }
    let oldest = g.oldest_absolute_row();
    let start = g.anchors_mut().place(oldest, AnchorRole::Start).unwrap();
    let lend = g.lend_anchors(LendReach::Ring).unwrap();
    g.resize(4, 20);
    g.take_back_anchors(lend);
    assert!(
        !g.anchors().lost(),
        "resolved ring-history text survived reflow"
    );
    assert!(g.anchors().row(start).is_some());
}

#[test]
fn a_reflow_landing_inside_a_row_excludes_the_shared_row_for_start_and_end() {
    let mut g = grid_printing(6, 8, &["one", "abcdefghij", "two"]);
    let base = g.visible_to_absolute(0);
    let ij = (0..6)
        .map(|r| base + r)
        .find(|&r| row_text(&g, r) == "ij")
        .unwrap();
    let start = g.anchors_mut().place(ij, AnchorRole::Start).unwrap();
    let end = g.anchors_mut().place(ij, AnchorRole::End).unwrap();
    let lend = g.lend_anchors(LendReach::Ring).unwrap();
    g.resize(6, 20);
    g.take_back_anchors(lend);
    assert!(!g.anchors().lost());
    let line = g.anchors().row(end).unwrap();
    assert_eq!(
        row_text(&g, line),
        "abcdefghij",
        "an End goes to its line's head"
    );
    let s = g.anchors().row(start).unwrap();
    assert_eq!(
        row_text(&g, s),
        "two",
        "a Start goes to the next line's head"
    );
}

#[test]
fn a_start_beyond_the_reach_is_clamped_to_the_oldest_line_read() {
    let mut g = grid_printing(4, 10, &["h0", "h1", "h2", "h3", "h4", "h5", "s0", "s1"]);
    let oldest = g.oldest_absolute_row();
    let start = g.anchors_mut().place(oldest, AnchorRole::Start).unwrap();
    let lend = g.lend_anchors(LendReach::Rows(2)).unwrap();
    g.resize(4, 12);
    g.take_back_anchors(lend);
    assert!(!g.anchors().lost());
    let s = g.anchors().row(start).unwrap();
    assert!(s > oldest, "clamped later, never earlier");
    assert_eq!(
        s,
        g.visible_to_absolute(0) - 2,
        "the oldest line the walk read"
    );
}

#[test]
fn a_rewrap_that_changes_the_lines_marks_the_anchors_lost() {
    let mut g = grid_printing(4, 10, &["aaa", "bbb"]);
    let base = g.visible_to_absolute(0);
    g.anchors_mut().place(base, AnchorRole::Start).unwrap();
    let lend = g.lend_anchors(LendReach::Ring).unwrap();
    g.set_cursor(0, 0);
    g.write_char('X');
    g.set_cursor(2, 0);
    g.take_back_anchors(lend);
    assert!(g.anchors().lost());

    for (before, after, ring) in [
        ("😀", "🚀", false),
        ("😀", "🚀", true),
        ("👩\u{200d}💻", "👩\u{200d}🚀", false),
    ] {
        let mut g = grid_printing(4, 10, &["a", "b"]);
        let put = |g: &mut Grid, text: &str| {
            if ring {
                g.set_cursor(0, 0);
                g.write_emoji_autowrap_fast(
                    text.chars().next().unwrap(),
                    crate::PackedColors::DEFAULT,
                    crate::CellFlags::COMPLEX,
                );
            } else {
                g.set_cell_complex_char(0, 0, text);
            }
            g.set_cursor(2, 0);
        };
        put(&mut g, before);
        let base = g.visible_to_absolute(0);
        g.anchors_mut().place(base, AnchorRole::Start).unwrap();
        let lend = g.lend_anchors(LendReach::Ring).unwrap();
        put(&mut g, after);
        g.take_back_anchors(lend);
        assert!(
            g.anchors().lost(),
            "changed complex payload {before:?} → {after:?}"
        );
    }

    let mut g = grid_printing(4, 10, &["a", "b"]);
    g.set_cursor(0, 0);
    g.write_char_at_cursor_packed(
        'p',
        crate::PackedColors::DEFAULT,
        crate::CellFlags::PROTECTED,
    );
    g.set_cursor(2, 0);
    let base = g.visible_to_absolute(0);
    g.anchors_mut().place(base, AnchorRole::Start).unwrap();
    let lend = g.lend_anchors(LendReach::Ring).unwrap();
    g.set_cursor(0, 0);
    g.write_char_at_cursor_packed(
        'q',
        crate::PackedColors::DEFAULT,
        crate::CellFlags::PROTECTED,
    );
    g.set_cursor(2, 0);
    g.take_back_anchors(lend);
    assert!(
        g.anchors().lost(),
        "protected glyph changes are verified too"
    );
}

#[test]
fn a_lend_of_an_empty_set_is_free() {
    let mut g = grid_printing(4, 10, &["a"]);
    assert!(g.lend_anchors(LendReach::Ring).is_none());
}
