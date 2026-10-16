// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Scrollback tests: tiered storage, attach/detach, content preservation.

use super::super::*;
use crate::grid::test_support::feed_numbered_lines;

#[test]
fn grid_scroll_display() {
    let mut grid = Grid::with_scrollback(3, 80, 100);

    for i in 0..10 {
        grid.write_char((b'A' + i) as char);
        grid.line_feed();
    }

    assert!(grid.scrollback_lines() > 0);

    grid.scroll_display(2);
    assert_eq!(grid.display_offset(), 2);

    grid.scroll_to_bottom();
    assert_eq!(grid.display_offset(), 0);
}

#[test]
fn grid_erase_scrollback_preserves_live_rows() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 4, 2, scrollback);

    for i in 0..8 {
        grid.carriage_return();
        for c in format!("L{i}").chars() {
            grid.write_char(c);
        }
        if i < 7 {
            grid.line_feed();
        }
    }

    assert!(grid.scrollback_lines() > 0);
    assert!(grid.tiered_scrollback_lines() > 0);

    let live_rows: Vec<String> = (0..grid.rows())
        .map(|row| grid.row(row).unwrap().to_string())
        .collect();

    grid.scroll_display(1);
    assert!(grid.display_offset() > 0);

    grid.erase_scrollback();

    assert_eq!(grid.scrollback_lines(), 0);
    assert_eq!(grid.tiered_scrollback_lines(), 0);
    assert_eq!(grid.display_offset(), 0);
    assert_eq!(grid.total_lines(), grid.rows() as usize);

    for (row_idx, expected) in live_rows.iter().enumerate() {
        assert_eq!(grid.row(row_idx as u16).unwrap().to_string(), *expected);
    }
}

#[test]
fn grid_with_tiered_scrollback() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 80, 5, scrollback);

    let sb = grid
        .scrollback()
        .expect("scrollback must be present after construction");
    assert_eq!(sb.line_count(), 0, "initial scrollback should have 0 lines");
    assert_eq!(grid.tiered_scrollback_lines(), 0);

    for i in 0..20 {
        for c in format!("Line {i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    assert!(grid.tiered_scrollback_lines() > 0);

    assert!(grid.scrollback_lines() > grid.storage.ring_buffer_scrollback());
}

/// A pre-filled store (a restored checkpoint's history) numbers its carried
/// lines from `0` and the live top past them, as `oldest_absolute_row()`
/// documents.
#[test]
fn a_prefilled_tiered_store_is_numbered_below_the_live_top() {
    let mut scrollback = Scrollback::new(100, 1000, 10_000_000);
    for i in 0..35 {
        scrollback.push_str(&format!("carried {i}"));
    }
    let grid = Grid::with_tiered_scrollback(6, 24, 1000, scrollback);

    assert_eq!(grid.scrollback_lines(), 35);
    assert_eq!(
        grid.oldest_absolute_row(),
        0,
        "the oldest carried line is row 0"
    );
    assert_eq!(
        grid.visible_to_absolute(0),
        35,
        "the live top is numbered past every carried line"
    );
    assert_eq!(grid.absolute_row_counter(), 41);
}

#[test]
fn grid_scrollback_content_preserved() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    // A 2-line ring, so lines reach the tiered store early.
    let mut grid = Grid::with_tiered_scrollback(3, 80, 2, scrollback);

    for i in 0..10 {
        for c in format!("Line {i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    let sb = grid.scrollback_mut().unwrap();
    assert!(
        sb.line_count() > 0,
        "Expected scrollback lines after writing 10 lines"
    );

    let line = sb
        .get_line(0)
        .expect("get_line(0) should not error")
        .expect("get_line(0) must return data when line_count > 0");
    let text = line.to_string();
    assert!(text.starts_with("Line "), "Expected 'Line X', got '{text}'");
}

#[test]
fn grid_scrollback_wrapped_lines() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 5, 2, scrollback);

    for c in "HelloWorld".chars() {
        grid.write_char_wrap(c);
    }
    grid.line_feed();

    for _ in 0..10 {
        grid.line_feed();
    }

    let sb = grid.scrollback_mut().unwrap();
    assert!(
        sb.line_count() > 1,
        "Expected >1 scrollback lines after wrapping; got {}",
        sb.line_count()
    );
    let line = sb.get_line(1).expect("no I/O error").expect("line present");
    assert!(
        line.is_wrapped(),
        "Wrapped line should preserve wrapped flag"
    );
}

#[test]
fn ring_buffer_scrollback_preserves_hyperlinks() {
    use std::sync::Arc;

    let mut grid = Grid::with_scrollback(4, 10, 10);

    let url: Arc<str> = Arc::from("https://example.com");
    grid.set_cursor(0, 0);
    for c in "Hello".chars() {
        grid.write_char(c);
    }
    for col in 0..5u16 {
        grid.extras_mut()
            .get_or_create(CellCoord::new(0, col))
            .set_hyperlink(Some(url.clone()));
    }

    // Four line feeds push row 0 into the ring, which has room for 10 history rows.
    for _ in 0..4 {
        grid.line_feed();
    }

    let ring_sb = grid.storage.ring_buffer_scrollback();
    assert!(ring_sb > 0, "should have ring buffer scrollback");

    // The oldest ring line (rev index `ring_sb - 1`) keeps its hyperlinks.
    let line = grid
        .history_line_rev(ring_sb - 1)
        .expect("scrollback line should exist");
    let spans = line
        .hyperlinks()
        .expect("ring buffer scrollback line should preserve hyperlinks");
    assert!(!spans.is_empty(), "should have at least one hyperlink span");
    assert_eq!(&*spans[0].url, "https://example.com");
}

/// `ring_extras.len() == ring_buffer_scrollback()` after every `scroll_up`, while the ring grows
/// and once it reuses slots.
#[test]
fn ring_extras_len_equals_ring_buffer_scrollback() {
    // Capacity 4 + 6 = 10 rows.
    let mut grid = Grid::with_scrollback(4, 10, 6);

    let check = |grid: &Grid, label: &str| {
        let ring_sb = grid.storage.ring_buffer_scrollback();
        let extras_len = grid.storage.ring_extras.len();
        assert_eq!(
            extras_len, ring_sb,
            "{label}: ring_extras.len()={extras_len} != ring_buffer_scrollback()={ring_sb}"
        );
    };

    check(&grid, "initial");

    // Growth: each scroll adds a row until capacity.
    for i in 1..=6 {
        grid.scroll_up(1);
        check(&grid, &format!("growth scroll {i}"));
    }

    // Reuse: rows are recycled.
    for i in 1..=5 {
        grid.scroll_up(1);
        check(&grid, &format!("reuse scroll {i}"));
    }

    grid.scroll_up(3);
    check(&grid, "batch scroll 3");

    grid.scroll_up(20);
    check(&grid, "large batch scroll 20");
}

/// Complex chars, RGB colours and combining marks survive the ring scrollback.
#[test]
fn scrollback_preserves_complex_chars_rgb_and_combining() {
    use std::sync::Arc;

    let mut grid = Grid::with_scrollback(3, 10, 5);

    // Col 0: 'A' in RGB red (flagged colour plus extras).
    let fg_red = PackedColor::rgb(0xFF, 0x00, 0x00);
    let bg_default = PackedColor::DEFAULT_BG;
    let cell_a = Cell::with_style('A', fg_red, bg_default, CellFlags::empty());
    grid.row_mut(0).unwrap().set(0, cell_a);
    grid.storage
        .extras
        .get_or_create(CellCoord::new(0, 0))
        .set_fg_rgb(Some([0xFF, 0x00, 0x00]));

    // Col 1: 🐛 as an overflow cell.
    let cell_complex = Cell::with_overflow_index(42);
    grid.row_mut(0).unwrap().set(1, cell_complex);
    grid.storage
        .extras
        .get_or_create(CellCoord::new(0, 1))
        .set_complex_char(Some(Arc::from("🐛")));

    grid.row_mut(0).unwrap().set(2, Cell::new('e'));
    grid.storage
        .extras
        .get_or_create(CellCoord::new(0, 2))
        .add_combining('\u{0301}');

    grid.scroll_up(3);

    let ring_sb = grid.storage.ring_buffer_scrollback();
    assert!(
        ring_sb >= 3,
        "expected at least 3 ring buffer scrollback lines"
    );

    let line = grid
        .get_history_line(0)
        .expect("first scrollback line should exist");
    let text = line.as_str().expect("line should have text");

    assert!(
        text.contains('🐛'),
        "scrollback text should contain 🐛, got: {text:?}"
    );

    assert!(
        text.contains('A'),
        "scrollback text should contain 'A', got: {text:?}"
    );

    assert!(
        text.contains('\u{0301}'),
        "scrollback text should contain combining accent U+0301, got: {text:?}"
    );

    let attrs_a = line.get_attr(0);
    assert_eq!(
        attrs_a.fg, 0x01_FF0000,
        "scrollback 'A' fg should be RGB red (0x01_FF0000), got: {:#010x}",
        attrs_a.fg
    );

    assert_ne!(
        attrs_a,
        CellAttrs::DEFAULT,
        "scrollback 'A' attrs should not be default"
    );
}

/// Erase scrollback with the ring exactly at capacity.
#[test]
fn erase_scrollback_at_ring_buffer_capacity() {
    // Capacity 4 + 6 = 10 rows.
    let mut grid = Grid::with_scrollback(4, 10, 6);

    for row in 0..4u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    for _ in 0..6 {
        grid.set_cursor(3, 0);
        grid.line_feed();
    }
    assert_eq!(
        grid.scrollback_lines(),
        6,
        "ring buffer should be at max scrollback"
    );

    // A few more, so the ring reuses slots (ring_head advances).
    for i in 0..4u16 {
        grid.set_cursor(3, 0);
        grid.write_char((b'W' + i as u8) as char);
        grid.line_feed();
    }
    assert_eq!(
        grid.scrollback_lines(),
        6,
        "scrollback should stay at max after reuse"
    );
    assert!(
        grid.storage.ring_head != 0,
        "ring_head should have advanced"
    );

    let live_rows: Vec<String> = (0..grid.rows())
        .map(|row| grid.row(row).unwrap().to_string())
        .collect();

    // Erase with a nonzero ring_head.
    grid.erase_scrollback();

    assert_eq!(grid.scrollback_lines(), 0);
    assert_eq!(grid.display_offset(), 0);
    assert_eq!(grid.total_lines(), grid.rows() as usize);
    assert_eq!(grid.storage.ring_head, 0, "ring_head should reset to 0");

    for (row_idx, expected) in live_rows.iter().enumerate() {
        assert_eq!(
            grid.row(row_idx as u16).unwrap().to_string(),
            *expected,
            "live row {row_idx} must be preserved after erase_scrollback at capacity"
        );
    }
    grid.assert_invariants();
}

/// `scroll_up` across the growth-to-reuse boundary.
#[test]
fn scroll_up_spans_growth_and_reuse_phases() {
    // Capacity 3 + 2 = 5 rows.
    let mut grid = Grid::with_scrollback(3, 10, 2);

    for row in 0..3u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    grid.scroll_up(1);
    assert_eq!(grid.total_lines(), 4);
    assert_eq!(grid.scrollback_lines(), 1);

    // One row of growth to capacity, then two reused.
    grid.scroll_up(3);
    assert_eq!(grid.total_lines(), 5, "should be at capacity");
    assert_eq!(grid.scrollback_lines(), 2, "max_scrollback=2");

    grid.assert_invariants();

    assert!(
        grid.row(2).unwrap().is_empty(),
        "bottom row should be blank after scroll"
    );
}

/// Erase scrollback holding exactly one line.
#[test]
fn erase_scrollback_single_line() {
    let mut grid = Grid::with_scrollback(4, 10, 10);

    for row in 0..4u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'P' + row as u8) as char);
    }

    grid.set_cursor(3, 0);
    grid.line_feed();
    assert_eq!(grid.scrollback_lines(), 1);

    let live_rows: Vec<String> = (0..grid.rows())
        .map(|row| grid.row(row).unwrap().to_string())
        .collect();

    grid.erase_scrollback();

    assert_eq!(grid.scrollback_lines(), 0);
    assert_eq!(grid.total_lines(), grid.rows() as usize);
    for (row_idx, expected) in live_rows.iter().enumerate() {
        assert_eq!(
            grid.row(row_idx as u16).unwrap().to_string(),
            *expected,
            "live row {row_idx} preserved after erasing single scrollback line"
        );
    }
    grid.assert_invariants();
}

/// `scroll_up` past the total capacity leaves a valid state.
#[test]
fn scroll_up_n_exceeds_capacity() {
    let mut grid = Grid::with_scrollback(3, 5, 4);

    for row in 0..3u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'X' + row as u8) as char);
    }

    // Far past the capacity of 7.
    grid.scroll_up(100);

    assert!(
        grid.total_lines() <= 3 + 4,
        "total_lines bounded by capacity"
    );
    assert!(grid.scrollback_lines() <= 4, "scrollback bounded by max");
    grid.assert_invariants();

    for row in 0..3u16 {
        assert!(
            grid.row(row).unwrap().is_empty(),
            "row {row} should be blank after scrolling 100 lines in a 3-row grid"
        );
    }
}

/// A full-screen scroll past the screen height (`CSI 65535 S`) archives exactly one screenful, in
/// order, and leaves the screen blank, as xterm and Alacritty clamp it.
#[test]
fn a_full_screen_scroll_past_the_screen_archives_one_screenful() {
    let mut grid = Grid::with_scrollback(3, 5, 1000);
    for row in 0..3u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'X' + row as u8) as char);
    }
    grid.scroll_region_up(65535);
    assert_eq!(grid.scrollback_lines(), 3, "one screenful archived");
    for row in 0..3u16 {
        assert!(grid.row(row).unwrap().is_empty(), "row {row} blank");
    }
    grid.assert_invariants();
}

/// Extras on visible rows survive a height decrease into scrollback: they are extracted by their
/// real row index.
#[test]
fn height_decrease_preserves_extras_on_pushed_rows() {
    use std::sync::Arc;

    // A tiered store, so the pushed rows are staged rather than dropped.
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(6, 10, 2, scrollback);

    // The shrink anchors at the bottom-row cursor, so the linked top row goes to scrollback.
    let url: Arc<str> = Arc::from("https://example.com/7783");
    grid.set_cursor(0, 0);
    for c in "Hello".chars() {
        grid.write_char(c);
    }
    for col in 0..5u16 {
        grid.extras_mut()
            .get_or_create(CellCoord::new(0, col))
            .set_hyperlink(Some(url.clone()));
    }
    grid.set_cursor(5, 0);

    assert!(
        grid.extras().row_has_hyperlinks(0),
        "row 0 should have hyperlinks before resize"
    );

    grid.resize_no_reflow(3, 10);

    let sb_lines = grid.scrollback_lines();
    assert!(
        sb_lines > 0,
        "should have scrollback lines after height decrease"
    );

    let mut found_hyperlink = false;
    for rev_idx in 0..sb_lines {
        if let Some(line) = grid.history_line_rev(rev_idx) {
            let text = line.as_str().unwrap_or("");
            if text.contains("Hello")
                && let Some(spans) = line.hyperlinks()
                && !spans.is_empty()
            {
                assert_eq!(
                    &*spans[0].url, "https://example.com/7783",
                    "hyperlink URL should be preserved"
                );
                found_hyperlink = true;
                break;
            }
        }
    }
    assert!(
        found_hyperlink,
        "hyperlink on the demoted row should survive height decrease into scrollback (#7783)"
    );
    grid.assert_invariants();
}

// Ring-only retention limit (the wasm engines' configuration): the ring is the retention, with
// lazy growth, immediate oldest-first eviction on shrink, and `None` = unlimited.

fn history_text(grid: &Grid, idx: usize) -> String {
    grid.get_history_line(idx)
        .map(|l| l.to_string().trim_end().to_string())
        .unwrap_or_default()
}

#[test]
fn ring_only_limit_shrink_evicts_oldest_immediately() {
    let mut grid = Grid::with_scrollback(5, 20, 10_000);
    feed_numbered_lines(&mut grid, 0, 300);
    let sb_before = grid.scrollback_lines();
    assert!(sb_before > 100, "precondition: deep ring history");
    let newest_before = history_text(&grid, sb_before - 1);
    let oldest_abs_before = grid.oldest_absolute_row();

    grid.set_scrollback_line_limit(Some(100));

    assert_eq!(
        grid.scrollback_lines(),
        100,
        "shrink truncates retention to the new limit immediately"
    );
    // Oldest first: the newest history line is untouched.
    assert_eq!(history_text(&grid, 99), newest_before);
    let evicted = sb_before - 100;
    assert_eq!(history_text(&grid, 0), format!("L{evicted}"));
    // Eviction advances the oldest absolute row by exactly the evicted count.
    assert_eq!(
        grid.oldest_absolute_row(),
        oldest_abs_before + evicted as u64
    );
    assert_eq!(grid.row(0).unwrap().to_string().trim_end(), "L296");
    grid.assert_invariants();

    feed_numbered_lines(&mut grid, 300, 50);
    assert_eq!(grid.scrollback_lines(), 100);
    grid.assert_invariants();
}

#[test]
fn ring_only_limit_grow_extends_retention_lazily() {
    let mut grid = Grid::with_scrollback(3, 20, 50);
    feed_numbered_lines(&mut grid, 0, 100);
    assert_eq!(grid.scrollback_lines(), 50, "precondition: at the old cap");

    grid.set_scrollback_line_limit(Some(120));
    // The ring grows only as lines scroll off (`grow_scrollback_ring`).
    assert_eq!(
        grid.storage.rows.len(),
        53,
        "raising the limit allocates nothing up front"
    );

    // The raise left the ring wrapped below its new cap: appended rows land at the ring's
    // logical end (see `linearize_ring`), so every line feed scrolls and history stays in order.
    feed_numbered_lines(&mut grid, 100, 30);
    let want: Vec<String> = (48..128).map(|i| format!("L{i}")).collect();
    assert_eq!(crate::grid::test_support::history_trimmed(&grid), want);
    assert_eq!(grid.row(1).unwrap().to_string().trim_end(), "L129");
    feed_numbered_lines(&mut grid, 130, 170);
    assert_eq!(
        grid.scrollback_lines(),
        120,
        "retention now grows to the raised limit"
    );
    // L298, L299 and the blank cursor row are visible, so L297 scrolled off last.
    assert_eq!(history_text(&grid, 119), "L297");
    grid.assert_invariants();
}

#[test]
fn ring_only_limit_none_is_unlimited() {
    let mut grid = Grid::with_scrollback(3, 20, 10);
    grid.set_scrollback_line_limit(None);
    feed_numbered_lines(&mut grid, 0, 500);
    assert_eq!(
        grid.scrollback_lines(),
        498,
        "None lifts the cap: every scrolled-off line is retained"
    );
    assert_eq!(history_text(&grid, 0), "L0");
    grid.assert_invariants();
}

#[test]
fn ring_only_limit_shrink_clamps_scrolled_back_viewport() {
    let mut grid = Grid::with_scrollback(5, 20, 10_000);
    feed_numbered_lines(&mut grid, 0, 300);
    grid.scroll_display(200);
    assert_eq!(grid.display_offset(), 200);

    grid.set_scrollback_line_limit(Some(50));
    assert!(
        grid.display_offset() <= grid.scrollback_lines(),
        "a scrolled-back viewport re-clamps into the shrunk history"
    );
    assert_eq!(grid.display_offset(), 50);
    grid.assert_invariants();
}

#[test]
fn ring_only_limit_reflects_in_the_getter() {
    let mut grid = Grid::with_scrollback(5, 20, 10_000);
    assert_eq!(
        grid.scrollback_line_limit(),
        Some(10_000),
        "ring-only: the ring cap IS the effective limit"
    );
    grid.set_scrollback_line_limit(Some(100));
    assert_eq!(grid.scrollback_line_limit(), Some(100));
    grid.set_scrollback_line_limit(None);
    assert_eq!(grid.scrollback_line_limit(), None);
}

/// A retention shrink that evicts bumps `content_gen`, ring-only and tiered; a re-apply that
/// evicts nothing does not.
#[test]
fn retention_shrink_bumps_content_gen_only_when_lines_evict() {
    let mut grid = Grid::with_scrollback(5, 20, 10_000);
    feed_numbered_lines(&mut grid, 0, 300);
    let g0 = grid.content_gen();
    grid.set_scrollback_line_limit(Some(100));
    assert!(
        grid.content_gen() > g0,
        "ring shrink that evicts is a content change"
    );
    let g1 = grid.content_gen();
    grid.set_scrollback_line_limit(Some(100));
    assert_eq!(
        grid.content_gen(),
        g1,
        "ring re-apply that evicts nothing must not bump"
    );

    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(5, 20, 8, scrollback);
    feed_numbered_lines(&mut grid, 0, 300);
    let sb_before = grid.scrollback_lines();
    let g0 = grid.content_gen();
    grid.set_scrollback_line_limit(Some(50));
    assert!(
        grid.scrollback_lines() < sb_before,
        "precondition: the store truncation really evicted"
    );
    assert!(
        grid.content_gen() > g0,
        "tiered shrink that evicts is a content change"
    );
    let g1 = grid.content_gen();
    grid.set_scrollback_line_limit(Some(50));
    assert_eq!(
        grid.content_gen(),
        g1,
        "tiered re-apply that evicts nothing must not bump"
    );
    grid.assert_invariants();
}

// Total retention limit on tiered grids: `set_scrollback_line_limit(L)` caps
// ring + lazy + store together.

#[test]
fn tiered_limit_is_one_total_across_ring_and_store() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 8, scrollback);
    feed_numbered_lines(&mut grid, 0, 100);
    let ring_sb = grid.storage.ring_buffer_scrollback();
    assert!(ring_sb > 0, "precondition: the hot ring holds history");

    grid.set_scrollback_line_limit(Some(20));
    assert_eq!(
        grid.scrollback_line_limit(),
        Some(20),
        "the getter round-trips the ONE total the setter took"
    );
    assert_eq!(
        grid.storage.ring_buffer_scrollback(),
        ring_sb,
        "limit >= ring cap: the hot ring keeps its fixed fast-tier role"
    );
    assert_eq!(
        grid.scrollback_lines(),
        20,
        "TOTAL retention (ring + lazy + store) equals the limit, not limit + ring"
    );
    assert_eq!(
        grid.scrollback()
            .map(aterm_scrollback::ScrollbackStorage::line_limit),
        Some(Some(20 - ring_sb)),
        "store share = total - ring cap"
    );

    feed_numbered_lines(&mut grid, 100, 50);
    let _ = grid.scrollback_mut(); // quiescent point: drain staged lazy lines
    assert_eq!(
        grid.scrollback_lines(),
        20,
        "steady state stays at the total"
    );
    // Eviction is oldest-first across the tiers.
    let newest = grid.scrollback_lines() - 1;
    assert_eq!(history_text(&grid, newest), "L147");
    assert_eq!(history_text(&grid, 0), "L128");
    grid.assert_invariants();
}

#[test]
fn tiered_limit_below_ring_cap_recaps_the_ring() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 8, scrollback);
    feed_numbered_lines(&mut grid, 0, 100);
    assert!(grid.storage.ring_buffer_scrollback() > 5);

    grid.set_scrollback_line_limit(Some(5));
    assert_eq!(
        grid.scrollback_line_limit(),
        Some(5),
        "limit < ring cap still round-trips the total"
    );
    assert_eq!(
        grid.scrollback_lines(),
        5,
        "the ring alone must not retain more than the total"
    );
    assert_eq!(
        grid.storage.ring_buffer_scrollback(),
        5,
        "the ring was re-capped (store share is zero)"
    );
    assert_eq!(history_text(&grid, 4), "L97");
    assert_eq!(history_text(&grid, 0), "L93");

    // A later raise widens only the store share: the ring size is fixed at construction.
    grid.set_scrollback_line_limit(Some(30));
    assert_eq!(grid.scrollback_line_limit(), Some(30));
    feed_numbered_lines(&mut grid, 100, 60);
    let _ = grid.scrollback_mut();
    assert_eq!(grid.scrollback_lines(), 30, "raised total governs");
    assert!(
        grid.storage.ring_buffer_scrollback() <= 5,
        "ring cap stayed at the shrunk value; the raise landed on the store share"
    );
    grid.assert_invariants();
}

#[test]
fn tiered_limit_equal_to_ring_cap_discards_evictions_without_store_churn() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 8, scrollback);
    grid.set_scrollback_line_limit(Some(8)); // total == ring cap → store share 0
    feed_numbered_lines(&mut grid, 0, 100);

    assert_eq!(
        grid.scrollback_lines(),
        8,
        "total == ring cap: the ring is the whole retention"
    );
    // Evicted ring rows are discarded (`stages_evicted_rows`), not pushed through a zero-share
    // store.
    assert_eq!(
        grid.storage.lazy_buffer_lines() + grid.tiered_scrollback_lines(),
        0,
        "no staged or stored lines with a zero store share"
    );
    assert_eq!(history_text(&grid, 7), "L97");
    grid.assert_invariants();
}

// Truncation accounting: retention loss the user did not ask for is counted (`metrics`, `lines`)
// and marked in band by one row at the cut naming the same count.

#[test]
fn flood_backpressure_drops_are_counted_and_marked() {
    // A 2 MiB budget holds fewer staged rows than the 20k floor, so the staging cap is the floor;
    // the store limit is far away, so every drop is a real loss.
    let scrollback = Scrollback::new(100, 1000, 2 * 1024 * 1024);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 8, scrollback);
    // A compress worker owns the drain but never runs: staged lines reach the staging cap, then
    // the oldest drop.
    grid.set_compress_offload_active(true);
    let over = 500;
    feed_numbered_lines(&mut grid, 0, Grid::ASYNC_COMPRESS_BACKPRESSURE + over + 8);
    let counted = grid.truncated_lines();
    assert!(
        counted > 0,
        "backpressure drops must register in the loss counter"
    );
    // The oldest retained row is the marker naming the counter; the next is the first survivor.
    let oldest = history_text(&grid, 0);
    let marked: u64 = oldest
        .strip_prefix(crate::grid::scroll_convert::FLOOD_MARKER_PREFIX)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("oldest retained row is the flood marker, got {oldest:?}"));
    assert_eq!(marked, counted, "the marker and the counter are one number");
    assert_eq!(
        history_text(&grid, 1),
        format!("L{counted}"),
        "lines L0..L{} were dropped and nothing else",
        counted - 1
    );
    // A user-requested shrink is not loss.
    grid.set_scrollback_line_limit(Some(5));
    assert_eq!(
        grid.truncated_lines(),
        counted,
        "user limit shrink is intentional, never counted as truncation"
    );
    grid.assert_invariants();
}

#[test]
fn ring_watermark_reports_pressure_only_when_configured() {
    use aterm_scrollback::WatermarkLevel;
    let mut grid = Grid::with_scrollback(3, 20, 50_000);
    grid.set_scrollback_line_limit(None); // ring-only "unlimited" mode
    feed_numbered_lines(&mut grid, 0, 2_000);
    assert_eq!(
        grid.ring_watermark_level(),
        WatermarkLevel::Green,
        "no configured budget → no watermark (the pre-E10a behavior)"
    );
    let used = grid.memory_used();
    assert!(used > 0);
    grid.set_ring_byte_watermark(Some(used * 10));
    assert_eq!(grid.ring_watermark_level(), WatermarkLevel::Green);
    grid.set_ring_byte_watermark(Some(used));
    assert_eq!(
        grid.ring_watermark_level(),
        WatermarkLevel::Red,
        "at 100% of budget the level is Red (>= the 95% threshold)"
    );
    grid.set_ring_byte_watermark(Some(used + used / 8));
    assert_eq!(
        grid.ring_watermark_level(),
        WatermarkLevel::Yellow,
        "~89% of budget sits in the Yellow band (80%..95%)"
    );
}

/// Staged lazy-buffer bytes count toward `memory_used` (and so the ring byte
/// watermark): clearing the staged lines removes exactly their per-line heap.
#[test]
fn staged_lazy_buffer_bytes_count_toward_memory_used() {
    let scrollback = Scrollback::new(100, 10_000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 8, scrollback);
    // Below the drain threshold, so the deferred lines stay staged.
    feed_numbered_lines(&mut grid, 0, 500);
    assert!(
        grid.storage.lazy_buffer.len() >= 400,
        "precondition: scroll-off lines are STAGED, not yet drained"
    );
    let with_staged = grid.memory_used();
    let lazy_with = grid.storage.lazy_buffer.memory_used();
    grid.storage.lazy_buffer.clear();
    let lazy_cleared = grid.storage.lazy_buffer.memory_used();
    assert!(
        lazy_with > lazy_cleared,
        "staged lines carry heap bytes beyond the retained backing capacity"
    );
    assert_eq!(
        grid.memory_used(),
        with_staged - (lazy_with - lazy_cleared),
        "memory_used must include exactly the staged lazy-buffer bytes"
    );
}

#[test]
fn tiered_limit_none_is_unlimited_total() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 8, scrollback);
    // `Scrollback::new` defaults to DEFAULT_LINE_LIMIT; the getter folds the ring back in.
    assert_eq!(
        grid.scrollback_line_limit(),
        Some(aterm_scrollback::DEFAULT_LINE_LIMIT + 8),
        "construction default round-trips as a total too"
    );
    grid.set_scrollback_line_limit(None);
    assert_eq!(
        grid.scrollback_line_limit(),
        None,
        "None = unlimited total (store unlimited, ring stays its fixed cap)"
    );
    feed_numbered_lines(&mut grid, 0, 100);
    let _ = grid.scrollback_mut();
    assert_eq!(
        grid.scrollback_lines(),
        98,
        "nothing evicted when unlimited"
    );
    grid.assert_invariants();
}

// The dense history walk (`Grid::history_lines_from`) equals the per-line `get_history_line`
// across store, lazy buffer and ring, at every start, with and without a corrupt segment: line
// `k` is row `oldest + start + k`.

/// A grid whose history spans every source: a pre-filled tiered store (cold +
/// warm + hot, optionally with a corrupt warm block between cold and warm),
/// then live output that stages lines in the lazy buffer and the ring.
fn three_source_history_grid(corrupt_lines: usize) -> Grid {
    let mut sb = Scrollback::with_block_size(4, 12, 10_000_000, 4);
    for i in 0..40 {
        sb.push_str(&format!("carried {i:03}"));
    }
    if corrupt_lines > 0 {
        sb.inject_corrupted_warm_block(corrupt_lines);
    }
    let mut grid = Grid::with_tiered_scrollback(3, 24, 6, sb);
    for i in 0..30 {
        grid.carriage_return();
        for c in format!("live {i:03}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }
    let tiered = grid.scrollback().map_or(0, |sb| sb.line_count());
    assert!(tiered > 0, "the fixture needs tiered lines");
    assert!(
        grid.storage.lazy_buffer_lines() > 0,
        "the fixture needs staged lazy lines"
    );
    assert!(
        grid.storage.ring_buffer_scrollback() > 0,
        "the fixture needs ring lines"
    );
    grid
}

/// Lines compare by their `Debug` rendering (`Line` has no `PartialEq`).
fn history_oracle(grid: &Grid, start: usize) -> Vec<Option<String>> {
    (start..grid.scrollback_lines())
        .map(|i| grid.get_history_line(i).map(|l| format!("{:?}", *l)))
        .collect()
}

fn history_walk(grid: &Grid, start: usize) -> Vec<Option<String>> {
    let walk = grid.history_lines_from(start);
    assert_eq!(walk.len(), grid.scrollback_lines().saturating_sub(start));
    walk.map(|l| l.map(|l| format!("{:?}", *l))).collect()
}

#[test]
fn history_lines_from_matches_get_history_line_across_every_source() {
    let grid = three_source_history_grid(0);
    let total = grid.scrollback_lines();
    for start in 0..=total + 1 {
        assert_eq!(
            history_walk(&grid, start),
            history_oracle(&grid, start),
            "start={start}"
        );
    }
    assert!(
        grid.history_lines_from(0).all(|l| l.is_some()),
        "a healthy history has no placeholders"
    );
}

#[test]
fn history_lines_from_matches_get_history_line_over_a_corrupt_segment() {
    let corrupt = 5;
    let grid = three_source_history_grid(corrupt);
    let total = grid.scrollback_lines();
    for start in 0..=total {
        assert_eq!(
            history_walk(&grid, start),
            history_oracle(&grid, start),
            "start={start}"
        );
    }
    let holes = grid.history_lines_from(0).filter(Option::is_none).count();
    assert_eq!(
        holes, corrupt,
        "one placeholder per unreadable line, and every later line keeps its row"
    );
}
