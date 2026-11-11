// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::LineSize;
use crate::grid::Grid;

fn make_storage(rows: u16, cols: u16) -> Grid {
    Grid::new(rows, cols)
}

/// Every row accessor refuses a row past the screen.
#[test]
fn row_accessors_refuse_rows_past_the_screen() {
    let mut g = make_storage(5, 10);
    for row in [5, 100] {
        assert!(g.storage.row(row).is_none(), "row {row}");
        assert!(g.storage.row_mut(row).is_none(), "row {row}");
        assert!(
            g.storage.row_mut_with_effective_cols(row).is_none(),
            "row {row}"
        );
    }
}

/// DECLRMM margins: a valid pair is set and flagged, an empty one (and a
/// reset) returns to the full width with the flag lowered.
#[test]
fn horizontal_margins_set_reject_and_reset() {
    let mut g = make_storage(24, 80);
    g.storage.set_horizontal_margins(10, 70);
    let margins = g.storage.horizontal_margins();
    assert_eq!((margins.left, margins.right), (10, 70));
    assert!(g.storage.has_horizontal_margins);
    g.storage.reset_horizontal_margins();
    assert!(g.storage.horizontal_margins().is_full(80));
    assert!(!g.storage.has_horizontal_margins);
    g.storage.set_horizontal_margins(50, 50);
    assert!(g.storage.horizontal_margins().is_full(80));
}

/// A row's addressable width halves on every double-width or
/// double-height line size, columns clamp to it, and `set_row_line_size`
/// maintains `any_double_width` across transitions.
#[test]
fn line_size_halves_the_addressable_columns() {
    for (size, cols) in [
        (LineSize::SingleWidth, 80),
        (LineSize::DoubleWidth, 40),
        (LineSize::DoubleHeightTop, 40),
        (LineSize::DoubleHeightBottom, 40),
    ] {
        let mut g = make_storage(5, 80);
        assert_eq!(
            g.set_row_line_size(2, size),
            size != LineSize::SingleWidth,
            "{size:?}: changed flag"
        );
        assert_eq!(
            g.has_any_double_width(),
            size != LineSize::SingleWidth,
            "{size:?}: any_double_width"
        );
        assert_eq!(g.storage.effective_cols_for_row(2), cols, "{size:?}");
        assert_eq!(g.storage.max_col_for_row(2), cols - 1, "{size:?}");
        assert_eq!(g.storage.effective_cols_for_row(0), 80, "{size:?}: row 0");
        for (col, want) in [(cols / 2, cols / 2), (cols - 1, cols - 1), (200, cols - 1)] {
            assert_eq!(
                g.storage.clamp_col_for_row(2, col),
                want,
                "{size:?} col {col}"
            );
        }
        if size != LineSize::SingleWidth {
            assert!(g.set_row_line_size(2, LineSize::SingleWidth));
            assert!(!g.has_any_double_width());
            assert_eq!(g.storage.effective_cols_for_row(2), 80);
        }
    }
    let mut g = make_storage(5, 80);
    assert!(!g.set_row_line_size(99, LineSize::DoubleWidth));
    assert!(!g.has_any_double_width());
}

/// A shift by zero rows, or by more than the range holds, moves nothing.
#[test]
fn degenerate_row_shifts_move_nothing() {
    let mut g = make_storage(5, 10);
    label_rows(&mut g);
    let before = first_cells(&g);
    for (top, bottom, n) in [(0, 4, 0), (0, 2, 5)] {
        g.storage.shift_visible_rows_up(top, bottom, n);
        assert_eq!(first_cells(&g), before, "up ({top}, {bottom}, {n})");
        g.storage.shift_visible_rows_down(top, bottom, n);
        assert_eq!(first_cells(&g), before, "down ({top}, {bottom}, {n})");
    }
}

/// Every visible row's first cell, top to bottom: the snapshot a no-op
/// shift must leave unchanged.
fn first_cells(g: &Grid) -> Vec<Option<char>> {
    (0..g.storage.visible_rows)
        .map(|r| {
            g.storage
                .row(r)
                .and_then(|row| row.get(0).map(|c| c.char()))
        })
        .collect()
}

/// A distinct letter at column 0 of every visible row, so a shift that
/// moved any row would show up in [`first_cells`].
fn label_rows(g: &mut Grid) {
    for (r, ch) in (0..g.storage.visible_rows).zip('A'..) {
        g.set_cursor(r, 0);
        g.write_char(ch);
    }
}

/// Every ring row's slice identity — `(cell pointer, cells length, page id)`,
/// the tuple PROOF_CARRYING_PERFORMANCE.md A3 names — as a sorted multiset.
fn ring_slice_identities(g: &Grid) -> Vec<(usize, usize, usize)> {
    let mut v: Vec<(usize, usize, usize)> = g
        .storage
        .rows
        .iter()
        .map(|r| (r.as_slice().as_ptr() as usize, r.cells_len(), r.page_id()))
        .collect();
    v.sort_unstable();
    v
}

/// A3, bounded and exhaustive: with the ring full and its head at every
/// rotation, every `shift_visible_rows_up/down(top, bottom, n)` a 6-row
/// screen admits keeps the multiset of row slices and allocates nothing
/// from the page store.
#[test]
fn shift_visible_rows_permutes_row_slices_and_allocates_nothing_at_capacity() {
    const ROWS: u16 = 6;
    const SCROLLBACK: usize = 4;
    let capacity = usize::from(ROWS) + SCROLLBACK;
    let mut shifts = 0usize;
    for rotate in 0..capacity {
        let mut g = Grid::with_scrollback(ROWS, 8, SCROLLBACK);
        for line in 0..(capacity + rotate) {
            g.write_char(char::from(b'a' + (line % 26) as u8));
            g.carriage_return();
            g.line_feed();
        }
        assert_eq!(g.storage.display_offset, 0);
        assert_eq!(
            g.storage.total_lines,
            g.storage.rows.len(),
            "steady state: the ring is full (rotate {rotate})"
        );
        assert_eq!(g.storage.rows.len(), capacity, "rotate {rotate}");
        let identities = ring_slice_identities(&g);
        let allocations = g.storage.pages.stats().allocations;
        for top in 0..usize::from(ROWS) {
            for bottom in top..usize::from(ROWS) {
                for n in 0..=usize::from(ROWS) {
                    for down in [false, true] {
                        if down {
                            g.storage.shift_visible_rows_down(top, bottom, n);
                        } else {
                            g.storage.shift_visible_rows_up(top, bottom, n);
                        }
                        shifts += 1;
                        assert_eq!(
                            ring_slice_identities(&g),
                            identities,
                            "rotate {rotate}: shift {} ({top}, {bottom}, {n}) changed the \
                             multiset of row slices",
                            if down { "down" } else { "up" }
                        );
                        assert_eq!(
                            g.storage.pages.stats().allocations,
                            allocations,
                            "rotate {rotate}: a shift allocated"
                        );
                    }
                }
            }
        }
    }
    assert!(shifts > 0);
}

/// Each ring slot's index in `origin`: where the row now in it started.
fn slot_origins(g: &Grid, origin: &[usize]) -> Vec<usize> {
    g.storage
        .rows
        .iter()
        .map(|r| {
            let at = r.as_slice().as_ptr() as usize;
            origin.iter().position(|&o| o == at).unwrap_or(usize::MAX)
        })
        .collect()
}

/// The ring index wrap is the remainder for every sum, inside the two-ring range it shortcuts
/// and past it.
#[test]
fn ring_index_wrap_is_the_remainder() {
    for len in 1..40 {
        for index in 0..5 * len {
            assert_eq!(
                super::wrap_ring_index(index, len),
                index % len,
                "{index} % {len}"
            );
        }
        let top = usize::MAX - 3;
        assert_eq!(super::wrap_ring_index(top, len), top % len);
    }
}

/// The rotations put every `Row` slot exactly where the swap chains do, slot for slot, for
/// every band and shift a 7-row screen admits, at every ring rotation (a ring of 7 rows and of
/// 7 + 3 with the head anywhere): bands that sit contiguously in the ring and bands that wrap
/// its end both occur.
#[test]
fn row_shift_rotations_place_every_slot_where_the_swap_chains_do() {
    const ROWS: u16 = 7;
    let mut seen = (0usize, 0usize);
    for scrollback in [0usize, 3] {
        let capacity = usize::from(ROWS) + scrollback;
        let build = |rotate: usize| {
            let mut g = Grid::with_scrollback(ROWS, 4, scrollback);
            for _ in 0..(capacity + rotate) {
                g.write_char('a');
                g.carriage_return();
                g.line_feed();
            }
            g
        };
        for rotate in 0..capacity {
            for top in 0..usize::from(ROWS) {
                for bottom in top..usize::from(ROWS) {
                    for n in 0..=usize::from(ROWS) + 1 {
                        for down in [false, true] {
                            let (mut fast, mut chain) = (build(rotate), build(rotate));
                            let fast_origin = ring_slice_identities_in_order(&fast);
                            let chain_origin = ring_slice_identities_in_order(&chain);
                            let lo = (fast.storage.row_index_base() + top) % capacity;
                            if lo + bottom - top < capacity {
                                seen.0 += 1;
                            } else {
                                seen.1 += 1;
                            }
                            if down {
                                fast.storage.shift_visible_rows_down(top, bottom, n);
                                chain
                                    .storage
                                    .shift_visible_rows_down_by_swaps(top, bottom, n);
                            } else {
                                fast.storage.shift_visible_rows_up(top, bottom, n);
                                chain.storage.shift_visible_rows_up_by_swaps(top, bottom, n);
                            }
                            assert_eq!(
                                slot_origins(&fast, &fast_origin),
                                slot_origins(&chain, &chain_origin),
                                "ring {capacity} rotated {rotate}: shift {} ({top}, {bottom}, \
                                 {n})",
                                if down { "down" } else { "up" }
                            );
                        }
                    }
                }
            }
        }
    }
    assert!(
        seen.0 > 0 && seen.1 > 0,
        "contiguous and wrapping bands: {seen:?}"
    );
}

/// Every ring slot's cell pointer, in slot order.
fn ring_slice_identities_in_order(g: &Grid) -> Vec<usize> {
    g.storage
        .rows
        .iter()
        .map(|r| r.as_slice().as_ptr() as usize)
        .collect()
}

#[test]
fn scrollback_line_limit() {
    let mut g = make_storage(24, 80);
    let sb = aterm_scrollback::Scrollback::new(100, 1000, 1_000_000);
    g.storage.attach_scrollback(sb);
    g.storage.set_store_line_limit(Some(500));
    assert_eq!(
        g.storage.scrollback_line_limit(),
        Some(500 + g.storage.max_scrollback)
    );
}
