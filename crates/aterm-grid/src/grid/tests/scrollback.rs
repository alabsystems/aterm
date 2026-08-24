// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Scrollback tests — tiered storage, attach/detach, content preservation.
//!
//! Migrated from aterm-core as part of #6556 Batch 2.

use super::super::*;

#[test]
fn grid_scroll_display() {
    let mut grid = Grid::with_scrollback(3, 80, 100);

    // Fill some content
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

    // Fill content to trigger scrollback
    for i in 0..20 {
        for c in format!("Line {i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    // Some lines should be in tiered scrollback now
    assert!(grid.tiered_scrollback_lines() > 0);

    // Total scrollback should include both ring buffer and tiered
    assert!(grid.scrollback_lines() > grid.storage.ring_buffer_scrollback());
}

#[test]
fn grid_scrollback_content_preserved() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    // Small ring buffer of 2 lines to force early promotion
    let mut grid = Grid::with_tiered_scrollback(3, 80, 2, scrollback);

    // Write 10 lines
    for i in 0..10 {
        for c in format!("Line {i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }

    // Check that content is preserved in tiered scrollback
    let sb = grid.scrollback_mut().unwrap();
    assert!(
        sb.line_count() > 0,
        "Expected scrollback lines after writing 10 lines"
    );

    // First line in scrollback must be retrievable and contain expected content
    let line = sb
        .get_line(0)
        .expect("get_line(0) should not error")
        .expect("get_line(0) must return data when line_count > 0");
    let text = line.to_string();
    assert!(text.starts_with("Line "), "Expected 'Line X', got '{text}'");
}

#[test]
fn grid_attach_detach_scrollback() {
    let mut grid = Grid::new(24, 80);
    assert!(grid.scrollback().is_none());

    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    grid.attach_scrollback(scrollback);
    let sb = grid
        .scrollback()
        .expect("scrollback must be present after attach");
    assert_eq!(
        sb.line_count(),
        0,
        "freshly attached scrollback should have 0 lines"
    );

    let detached = grid
        .detach_scrollback()
        .expect("detach must return the scrollback");
    assert_eq!(
        detached.line_count(),
        0,
        "detached scrollback should still have 0 lines"
    );
    assert!(
        grid.scrollback().is_none(),
        "scrollback must be None after detach"
    );
}

#[test]
fn grid_scrollback_wrapped_lines() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 5, 2, scrollback);

    // Write a line that wraps
    for c in "HelloWorld".chars() {
        grid.write_char_wrap(c);
    }
    grid.line_feed();

    // Force more scrolling to push lines to tiered scrollback
    for _ in 0..10 {
        grid.line_feed();
    }

    // Check that wrapped flag is preserved
    let sb = grid.scrollback_mut().unwrap();
    assert!(
        sb.line_count() > 1,
        "Expected >1 scrollback lines after wrapping; got {}",
        sb.line_count()
    );
    // The second line should be marked as wrapped
    let line = sb.get_line(1).expect("no I/O error").expect("line present");
    assert!(
        line.is_wrapped(),
        "Wrapped line should preserve wrapped flag"
    );
}

#[test]
fn ring_buffer_scrollback_preserves_hyperlinks() {
    // Regression test for #4149: hyperlinks are preserved when rows scroll from
    // the visible area into ring buffer scrollback (max_scrollback > 0).
    // Before the fix, row_to_line_static used empty CellExtras, losing hyperlinks.
    use std::sync::Arc;

    let mut grid = Grid::with_scrollback(4, 10, 10);

    // Write "Hello" on row 0 with a hyperlink on columns 0-4
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

    // Scroll 4 times to push row 0 into ring buffer scrollback (not tiered,
    // since max_scrollback=10 means ring buffer has room for 10 scrollback rows).
    for _ in 0..4 {
        grid.line_feed();
    }

    // Row 0 is now in ring buffer scrollback. Retrieve it via history_line_rev.
    let ring_sb = grid.storage.ring_buffer_scrollback();
    assert!(ring_sb > 0, "should have ring buffer scrollback");

    // The oldest scrollback line (rev_idx = ring_sb-1) should have hyperlinks.
    let line = grid
        .history_line_rev(ring_sb - 1)
        .expect("scrollback line should exist");
    let spans = line
        .hyperlinks()
        .expect("ring buffer scrollback line should preserve hyperlinks");
    assert!(!spans.is_empty(), "should have at least one hyperlink span");
    assert_eq!(&*spans[0].url, "https://example.com");
}

/// Verify invariant: ring_extras.len() == ring_buffer_scrollback() after
/// scroll_up, across both growth phase (under capacity) and reuse phase (at
/// capacity). This invariant is critical for correct extras display in
/// ring buffer scrollback (#4149, #4215).
#[test]
fn ring_extras_len_equals_ring_buffer_scrollback() {
    // Grid with 4 visible rows and max_scrollback=6.
    // Total capacity = 4 + 6 = 10 rows.
    let mut grid = Grid::with_scrollback(4, 10, 6);

    // Helper: check the invariant
    let check = |grid: &Grid, label: &str| {
        let ring_sb = grid.storage.ring_buffer_scrollback();
        let extras_len = grid.storage.ring_extras.len();
        assert_eq!(
            extras_len, ring_sb,
            "{label}: ring_extras.len()={extras_len} != ring_buffer_scrollback()={ring_sb}"
        );
    };

    check(&grid, "initial");

    // Phase 1: Growth — each scroll_up(1) adds a row until capacity.
    // ring_buffer_scrollback = total_lines - visible_rows
    for i in 1..=6 {
        grid.scroll_up(1);
        check(&grid, &format!("growth scroll {i}"));
    }
    // Now at capacity: total_lines=10, ring_buffer_scrollback=6

    // Phase 2: Reuse — rows are recycled, oldest goes to (no) tiered scrollback.
    for i in 1..=5 {
        grid.scroll_up(1);
        check(&grid, &format!("reuse scroll {i}"));
    }

    // Batch scroll
    grid.scroll_up(3);
    check(&grid, "batch scroll 3");

    // Large batch exceeding total rows
    grid.scroll_up(20);
    check(&grid, "large batch scroll 20");
}

/// Verify that complex chars, RGB colors, and combining marks survive
/// scrollback transit through the ring buffer (#4215).
#[test]
fn scrollback_preserves_complex_chars_rgb_and_combining() {
    use std::sync::Arc;

    // Grid: 3 visible rows, 10 cols, max_scrollback=5 (ring buffer only)
    let mut grid = Grid::with_scrollback(3, 10, 5);

    // Col 0: 'A' with RGB fg red — set cell with RGB-flagged colors + extras
    let fg_red = PackedColor::rgb(0xFF, 0x00, 0x00);
    let bg_default = PackedColor::DEFAULT_BG;
    let cell_a = Cell::with_style('A', fg_red, bg_default, CellFlags::empty());
    grid.row_mut(0).unwrap().set(0, cell_a);
    grid.storage
        .extras
        .get_or_create(CellCoord::new(0, 0))
        .set_fg_rgb(Some([0xFF, 0x00, 0x00]));

    // Col 1: complex char 🐛 — set overflow index + complex_char in extras
    let cell_complex = Cell::with_overflow_index(42);
    grid.row_mut(0).unwrap().set(1, cell_complex);
    grid.storage
        .extras
        .get_or_create(CellCoord::new(0, 1))
        .set_complex_char(Some(Arc::from("🐛")));

    // Col 2: 'e' with combining acute accent
    grid.row_mut(0).unwrap().set(2, Cell::new('e'));
    grid.storage
        .extras
        .get_or_create(CellCoord::new(0, 2))
        .add_combining('\u{0301}');

    // Scroll 3 lines so row 0 enters ring buffer scrollback.
    grid.scroll_up(3);

    let ring_sb = grid.storage.ring_buffer_scrollback();
    assert!(
        ring_sb >= 3,
        "expected at least 3 ring buffer scrollback lines"
    );

    // Row 0 was the first to scroll, so it's the oldest line (index 0).
    let line = grid
        .get_history_line(0)
        .expect("first scrollback line should exist");
    let text = line.as_str().expect("line should have text");

    // Complex char 🐛 should be in the text (not U+FFFD)
    assert!(
        text.contains('🐛'),
        "scrollback text should contain 🐛, got: {text:?}"
    );

    // 'A' should be in the text
    assert!(
        text.contains('A'),
        "scrollback text should contain 'A', got: {text:?}"
    );

    // Combining accent should be in the text
    assert!(
        text.contains('\u{0301}'),
        "scrollback text should contain combining accent U+0301, got: {text:?}"
    );

    // RGB fg for first char ('A') should be 0x01_FF0000 (red)
    let attrs_a = line.get_attr(0);
    assert_eq!(
        attrs_a.fg, 0x01_FF0000,
        "scrollback 'A' fg should be RGB red (0x01_FF0000), got: {:#010x}",
        attrs_a.fg
    );

    // attrs should not be default (RGB was preserved)
    assert_ne!(
        attrs_a,
        CellAttrs::DEFAULT,
        "scrollback 'A' attrs should not be default"
    );
}

/// Erase scrollback when ring buffer is exactly at capacity.
#[test]
fn erase_scrollback_at_ring_buffer_capacity() {
    // 4 visible rows, max_scrollback=6 → capacity=10 rows
    let mut grid = Grid::with_scrollback(4, 10, 6);

    // Fill visible rows with identifiable content
    for row in 0..4u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Scroll enough to fill ring buffer to capacity (6 scrollback lines)
    for _ in 0..6 {
        grid.set_cursor(3, 0);
        grid.line_feed();
    }
    assert_eq!(
        grid.scrollback_lines(),
        6,
        "ring buffer should be at max scrollback"
    );

    // Scroll a few more to exercise reuse phase (ring_head advances)
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

    // Snapshot live rows before erase
    let live_rows: Vec<String> = (0..grid.rows())
        .map(|row| grid.row(row).unwrap().to_string())
        .collect();

    // The critical operation: erase scrollback with non-zero ring_head
    grid.erase_scrollback();

    // Invariants must hold
    assert_eq!(grid.scrollback_lines(), 0);
    assert_eq!(grid.display_offset(), 0);
    assert_eq!(grid.total_lines(), grid.rows() as usize);
    assert_eq!(grid.storage.ring_head, 0, "ring_head should reset to 0");

    // Live rows must be preserved identically
    for (row_idx, expected) in live_rows.iter().enumerate() {
        assert_eq!(
            grid.row(row_idx as u16).unwrap().to_string(),
            *expected,
            "live row {row_idx} must be preserved after erase_scrollback at capacity"
        );
    }
    grid.assert_invariants();
}

/// scroll_up at the exact growth-to-reuse transition boundary.
#[test]
fn scroll_up_spans_growth_and_reuse_phases() {
    // 3 visible rows, max_scrollback=2 → capacity=5
    let mut grid = Grid::with_scrollback(3, 10, 2);

    // Fill visible rows
    for row in 0..3u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'A' + row as u8) as char);
    }

    // Scroll up by 1 to enter growth phase (total_lines: 3→4)
    grid.scroll_up(1);
    assert_eq!(grid.total_lines(), 4);
    assert_eq!(grid.scrollback_lines(), 1);

    // Scroll up by 3: first 1 goes to growth (4→5, at capacity),
    // remaining 2 go to reuse phase (oldest rows recycled).
    grid.scroll_up(3);
    assert_eq!(grid.total_lines(), 5, "should be at capacity");
    assert_eq!(grid.scrollback_lines(), 2, "max_scrollback=2");

    // Verify ring buffer is valid
    grid.assert_invariants();

    // Bottom visible row should be empty (newly scrolled in)
    assert!(
        grid.row(2).unwrap().is_empty(),
        "bottom row should be blank after scroll"
    );
}

/// Erase scrollback with exactly 1 line of scrollback (minimal edge case).
#[test]
fn erase_scrollback_single_line() {
    let mut grid = Grid::with_scrollback(4, 10, 10);

    // Write identifiable content
    for row in 0..4u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'P' + row as u8) as char);
    }

    // Generate exactly 1 scrollback line
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

/// scroll_up with n larger than total capacity still produces valid state.
#[test]
fn scroll_up_n_exceeds_capacity() {
    let mut grid = Grid::with_scrollback(3, 5, 4);

    for row in 0..3u16 {
        grid.set_cursor(row, 0);
        grid.write_char((b'X' + row as u8) as char);
    }

    // Scroll by a huge amount (well beyond capacity of 7)
    grid.scroll_up(100);

    // State must be valid
    assert!(
        grid.total_lines() <= 3 + 4,
        "total_lines bounded by capacity"
    );
    assert!(grid.scrollback_lines() <= 4, "scrollback bounded by max");
    grid.assert_invariants();

    // All visible rows should be blank (100 blank rows scrolled in)
    for row in 0..3u16 {
        assert!(
            grid.row(row).unwrap().is_empty(),
            "row {row} should be blank after scrolling 100 lines in a 3-row grid"
        );
    }
}

/// Regression test for #7783: extras (hyperlinks, RGB colors) on visible rows
/// must be preserved when those rows are pushed to scrollback during terminal
/// height decrease.
///
/// Before the fix, `adjust_row_count` used `u16::MAX` as `row_idx` for
/// `extract_row_extras`, causing HashMap-keyed extras (hyperlinks, combining
/// marks) to be orphaned because `CellCoord::new(u16::MAX, col)` never matches
/// any entry stored at the actual visible row index.
#[test]
fn height_decrease_preserves_extras_on_pushed_rows() {
    use std::sync::Arc;

    // 6 visible rows, 10 columns, tiered scrollback so adjust_row_count
    // pushes excess rows to the lazy buffer instead of dropping them.
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(6, 10, 2, scrollback);

    // Write "Hello" on row 0 with a hyperlink on columns 0-4, cursor at the
    // bottom. The shrink anchors at the cursor (audit-2 item 1), so the TOP
    // rows — the linked one included — are what transit into scrollback now;
    // the #7783 keying rule this test exists for (extras extracted by their
    // REAL visible row index, never u16::MAX) guards that transit the same.
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

    // Verify the hyperlink is present before resize.
    assert!(
        grid.extras().row_has_hyperlinks(0),
        "row 0 should have hyperlinks before resize"
    );

    // Shrink height from 6 to 3: the top rows (the linked row among them)
    // are demoted into scrollback.
    grid.resize_no_reflow(3, 10);

    // The pushed rows should now be in scrollback (lazy buffer or tiered).
    let sb_lines = grid.scrollback_lines();
    assert!(
        sb_lines > 0,
        "should have scrollback lines after height decrease"
    );

    // Search the scrollback for the line with "Hello" and check its hyperlinks.
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

// =========================================================================
// Ring-only retention limit — set_scrollback_line_limit WITHOUT a tiered
// store (the wasm engines' configuration). The ring IS retention there, so
// the limit must re-cap the ring: lazy growth, immediate oldest-first
// eviction on shrink, `None` = unlimited.
// =========================================================================

/// Write `n` numbered lines `L{start}..L{start+n-1}` (CR + text + LF each).
fn write_numbered_lines(grid: &mut Grid, start: usize, n: usize) {
    for i in start..start + n {
        grid.carriage_return();
        for c in format!("L{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }
}

fn history_text(grid: &Grid, idx: usize) -> String {
    grid.get_history_line(idx)
        .map(|l| l.to_string().trim_end().to_string())
        .unwrap_or_default()
}

#[test]
fn ring_only_limit_shrink_evicts_oldest_immediately() {
    let mut grid = Grid::with_scrollback(5, 20, 10_000);
    write_numbered_lines(&mut grid, 0, 300);
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
    // Eviction is oldest-first: the newest history line is untouched and the
    // new oldest is the line that sat `sb_before - 100` in.
    assert_eq!(history_text(&grid, 99), newest_before);
    let evicted = sb_before - 100;
    assert_eq!(history_text(&grid, 0), format!("L{evicted}"));
    // Absolute-row identity: eviction advances the oldest retained row by
    // exactly the evicted count — surviving lines keep their numbers.
    assert_eq!(
        grid.oldest_absolute_row(),
        oldest_abs_before + evicted as u64
    );
    // The live viewport is untouched.
    assert_eq!(grid.row(0).unwrap().to_string().trim_end(), "L296");
    grid.assert_invariants();

    // The new cap also governs future retention.
    write_numbered_lines(&mut grid, 300, 50);
    assert_eq!(grid.scrollback_lines(), 100);
    grid.assert_invariants();
}

#[test]
fn ring_only_limit_grow_extends_retention_lazily() {
    let mut grid = Grid::with_scrollback(3, 20, 50);
    write_numbered_lines(&mut grid, 0, 100);
    assert_eq!(grid.scrollback_lines(), 50, "precondition: at the old cap");

    grid.set_scrollback_line_limit(Some(120));
    // Growing the cap must not eagerly allocate ring rows — the ring only
    // grows as lines actually scroll off (grow_scrollback_ring).
    assert_eq!(
        grid.storage.rows.len(),
        53,
        "raising the limit allocates nothing up front"
    );

    write_numbered_lines(&mut grid, 100, 200);
    assert_eq!(
        grid.scrollback_lines(),
        120,
        "retention now grows to the raised limit"
    );
    // Newest history line is the last one scrolled off (the final LF leaves
    // L298/L299 + the blank cursor row visible, so L297 scrolled off last).
    assert_eq!(history_text(&grid, 119), "L297");
    grid.assert_invariants();
}

#[test]
fn ring_only_limit_none_is_unlimited() {
    let mut grid = Grid::with_scrollback(3, 20, 10);
    grid.set_scrollback_line_limit(None);
    write_numbered_lines(&mut grid, 0, 500);
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
    write_numbered_lines(&mut grid, 0, 300);
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

/// Retention shrink IS a content change: evicting lines removes them from the
/// indexed/addressable set, so `content_gen` must bump exactly like a write —
/// on BOTH arms (the ring-only eviction and the tiered store truncation; the
/// tiered arm was the introspection-harness stale-search finding). A re-apply
/// that evicts nothing must NOT bump (no cache thrash on config re-applies).
#[test]
fn retention_shrink_bumps_content_gen_only_when_lines_evict() {
    // Ring-only arm.
    let mut grid = Grid::with_scrollback(5, 20, 10_000);
    write_numbered_lines(&mut grid, 0, 300);
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

    // Tiered arm (store truncation).
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(5, 20, 8, scrollback);
    write_numbered_lines(&mut grid, 0, 300);
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

// =========================================================================
// Unified TOTAL retention limit on tiered grids (audit E1, Codex-required):
// `set_scrollback_line_limit(L)` caps ring + lazy + store TOGETHER. The old
// contract ("the limit lands on the store, the ring rides on top") silently
// retained L + ring-cap lines; these tests pin the ONE-total replacement.
// =========================================================================

#[test]
fn tiered_limit_is_one_total_across_ring_and_store() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 8, scrollback);
    write_numbered_lines(&mut grid, 0, 100);
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
    // The store's share is exactly the remainder after the ring's share.
    assert_eq!(
        grid.scrollback()
            .map(aterm_scrollback::ScrollbackStorage::line_limit),
        Some(Some(20 - ring_sb)),
        "store share = total - ring cap"
    );

    // The total keeps governing future retention at steady state.
    write_numbered_lines(&mut grid, 100, 50);
    let _ = grid.scrollback_mut(); // quiescent point: drain staged lazy lines
    assert_eq!(
        grid.scrollback_lines(),
        20,
        "steady state stays at the total"
    );
    // Eviction is oldest-first across the tiers: the newest history line is
    // the last one that scrolled off, and absolute numbering is contiguous.
    let newest = grid.scrollback_lines() - 1;
    assert_eq!(history_text(&grid, newest), "L147");
    assert_eq!(history_text(&grid, 0), "L128");
    grid.assert_invariants();
}

#[test]
fn tiered_limit_below_ring_cap_recaps_the_ring() {
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 8, scrollback);
    write_numbered_lines(&mut grid, 0, 100);
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
    // Newest-5 survive, oldest evicted (oldest-first semantics).
    assert_eq!(history_text(&grid, 4), "L97");
    assert_eq!(history_text(&grid, 0), "L93");

    // A later RAISE widens only the store share — the hot-tier size is a
    // construction decision and does not grow back.
    grid.set_scrollback_line_limit(Some(30));
    assert_eq!(grid.scrollback_line_limit(), Some(30));
    write_numbered_lines(&mut grid, 100, 60);
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
    write_numbered_lines(&mut grid, 0, 100);

    assert_eq!(
        grid.scrollback_lines(),
        8,
        "total == ring cap: the ring is the whole retention"
    );
    // Evicted ring rows are discarded directly (stages_evicted_rows): a
    // per-line materialize+push+truncate cycle through a zero-share store
    // would tax the PTY-drain hot path for nothing.
    assert_eq!(
        grid.storage.lazy_buffer_lines() + grid.tiered_scrollback_lines(),
        0,
        "no staged or stored lines with a zero store share"
    );
    assert_eq!(history_text(&grid, 7), "L97");
    grid.assert_invariants();
}

// =========================================================================
// Out-of-band truncation accounting (audit E10a): retention loss the user
// did not ask for is COUNTED, never surfaced as sentinel content.
// =========================================================================

#[test]
fn flood_backpressure_drops_are_counted_out_of_band() {
    let scrollback = Scrollback::new(100, 1000, 100_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 8, scrollback);
    // A compress worker owns the drain but never runs (the starved-worker
    // flood): staged lines accumulate to the THRU-5 cap, then the oldest drop.
    grid.set_compress_offload_active(true);
    let over = 500;
    write_numbered_lines(&mut grid, 0, Grid::ASYNC_COMPRESS_BACKPRESSURE + over + 8);
    assert!(
        grid.truncated_lines() > 0,
        "backpressure drops must register in the loss counter"
    );
    // The retained set holds no sentinel — its lines are exactly the written
    // content (check the oldest retained line's shape).
    let oldest = history_text(&grid, 0);
    assert!(
        oldest.starts_with('L'),
        "no sentinel content is ever injected (oldest retained: {oldest:?})"
    );
    // A user-requested shrink is NOT loss: the counter must not move.
    let counted = grid.truncated_lines();
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
    write_numbered_lines(&mut grid, 0, 2_000);
    assert_eq!(
        grid.ring_watermark_level(),
        WatermarkLevel::Green,
        "no configured budget → no watermark (the pre-E10a behavior)"
    );
    let used = grid.memory_used();
    assert!(used > 0);
    // Budget far above use → Green; at ~use → Red; between → Yellow.
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

/// Staged lazy-buffer bytes count toward `memory_used` — and hence toward the
/// ring byte watermark (Wave-3 adversarial review): under flood backpressure
/// the deferred scroll-off lines are real memory the store has not absorbed
/// yet. Pins the inclusion exactly: clearing the staged lines (capacity
/// retained) removes exactly their per-line heap from the estimate.
#[test]
fn staged_lazy_buffer_bytes_count_toward_memory_used() {
    let scrollback = Scrollback::new(100, 10_000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(3, 20, 8, scrollback);
    // Stage well below the drain threshold so the deferred lines stay parked.
    write_numbered_lines(&mut grid, 0, 500);
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
    // Scrollback::new defaults to DEFAULT_LINE_LIMIT — the getter reports
    // that store share folded back into a total.
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
    write_numbered_lines(&mut grid, 0, 100);
    let _ = grid.scrollback_mut();
    assert_eq!(
        grid.scrollback_lines(),
        98,
        "nothing evicted when unlimited"
    );
    grid.assert_invariants();
}
