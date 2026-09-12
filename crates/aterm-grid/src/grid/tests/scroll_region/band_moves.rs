// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The row-band move record (`RowBandMoves`) — the NUMBERS behind
//! `coordinates_invalidated`, recorded so a host that caches screen coordinates
//! (the rainbow ribbon under a Codex composer) can translate instead of discard.
//!
//! The shapes are the ones measured on the real Codex CLI (scratchpad
//! `codex-bytes.bin`, 57x151 cells): an Enter is `ESC[12;57r ESC[12;1H` and five
//! reverse indices on that region; a streamed line while the viewport is not yet
//! at the screen bottom (phase A) is one reverse index on `[vt..57]` followed by an
//! LF that does NOT scroll (`ESC[1;vt r ESC[vt-1;1H \r\n` lands on the region's
//! bottom row); a streamed line with the viewport pinned at the bottom (phase B) is
//! the top-anchored archival scroll `ESC[1;52r ESC[52;1H \r\n`. The grid has no
//! parser, so each sequence is driven through the `Grid` methods the VT handler
//! calls for it; the byte-verbatim replays live in `aterm-core`'s processing tests.

use super::*;
use crate::{AbsoluteRowUpdate, ROW_BAND_MOVES_PER_BATCH, RowBandMove, RowBandMoves};

const ROWS: u16 = 57;
const COLS: u16 = 151;

fn codex_grid() -> Grid {
    Grid::with_scrollback(ROWS, COLS, 1000)
}

fn one(top: u16, bottom: u16, delta: i16) -> RowBandMoves {
    let mut moves = RowBandMoves::default();
    moves.moves[0] = RowBandMove { top, bottom, delta };
    moves.len = 1;
    moves
}

/// `ESC[12;57r ESC[12;1H (ESC M)x5` — the Codex Enter: five reverse indices at the
/// top of the 0-based region `[11..=56]` compose into ONE `+5` band, and the
/// coordinate flag the hosts already consume is still set (the contract is kept,
/// the record is additional).
#[test]
fn a_reverse_index_at_a_region_top_records_one_composed_down_band() {
    let mut grid = codex_grid();
    grid.set_scroll_region(11, 56);
    grid.set_cursor(11, 0);
    for _ in 0..5 {
        grid.scroll_region_down(1);
    }
    assert_eq!(grid.take_row_band_moves(), one(11, 56, 5));
    assert!(
        grid.take_coordinates_invalidated(),
        "the epoch contract is unchanged: a region scroll still sets the flag"
    );
    assert_eq!(
        grid.take_row_band_moves(),
        RowBandMoves::default(),
        "the record drains with the batch"
    );
}

/// rec642 of the capture: `ESC[25;57r ESC[25;1H ESC M ESC[r ESC[1;25r ESC[24;1H \r\n`
/// — the viewport band `[24..=56]` moves down one row; the LF that follows starts
/// on row 23 of a `[0..=24]` region and lands ON its bottom row, so nothing scrolls,
/// nothing is archived and no splice is recorded.
#[test]
fn a_codex_phase_a_line_records_the_viewport_band_and_no_splice() {
    let mut grid = codex_grid();
    let history_before = grid.scrollback_lines();
    grid.set_scroll_region(24, 56);
    grid.set_cursor(24, 0);
    grid.scroll_region_down(1);
    grid.reset_scroll_region();
    grid.set_scroll_region(0, 24);
    grid.set_cursor(23, 0);
    grid.line_feed();
    assert_eq!(grid.cursor().row, 24, "the LF lands on the region bottom");
    assert_eq!(grid.take_row_band_moves(), one(24, 56, 1));
    assert_eq!(
        grid.take_selection_row_update(),
        None,
        "phase A never splices"
    );
    assert_eq!(
        grid.scrollback_lines(),
        history_before,
        "phase A never archives"
    );
}

/// rec720 of the capture: `ESC[1;52r ESC[52;1H \r\n` with transcript on the screen
/// — the top-anchored archival scroll records the transcript band `[0..=51]` moving
/// UP one row BESIDE its splice (the splice is the history insertion point for
/// durable absolute-row metadata; the band is the screen transform), and history
/// grows by the one row that left the screen.
#[test]
fn a_top_anchored_archival_scroll_records_the_transcript_band_beside_its_splice() {
    let mut grid = codex_grid();
    grid.set_cursor(0, 0);
    grid.write_char('X');
    let history_before = grid.scrollback_lines();
    let old_live_top = grid.absolute_row_counter() - u64::from(ROWS);
    grid.set_scroll_region(0, 51);
    grid.set_cursor(51, 0);
    grid.line_feed();
    assert_eq!(grid.take_row_band_moves(), one(0, 51, -1));
    assert_eq!(
        grid.take_selection_row_update(),
        Some(AbsoluteRowUpdate::Splice {
            at: old_live_top + 52,
            inserted: 1,
        })
    );
    assert_eq!(grid.scrollback_lines(), history_before + 1);
}

fn recorded(moves: &[(u16, u16, i16)]) -> RowBandMoves {
    let mut grid = Grid::new(60, 10);
    for &(top, bottom, delta) in moves {
        grid.storage
            .presentation
            .record_row_band_move(top, bottom, delta);
    }
    grid.take_row_band_moves()
}

/// The compose rule and its refusals — see `record_row_band_move`'s proof. Same band
/// twice composes in either direction; a DOWNWARD band narrowed from above by
/// exactly the first displacement composes (the excluded rows are the blanks the
/// first move vacated); everything else is a separate entry, and a ninth distinct
/// band or an `i16` overflow poisons the record instead of dropping a move.
#[test]
fn band_moves_compose_exactly_and_refuse_everything_else() {
    assert_eq!(recorded(&[(11, 56, 1); 5]), one(11, 56, 5), "the Enter");
    assert_eq!(
        recorded(&[(24, 56, 1), (25, 56, 1)]),
        one(24, 56, 2),
        "a viewport growing one row per line while its top slides down"
    );
    assert_eq!(recorded(&[(0, 51, -1); 3]), one(0, 51, -3), "phase B");

    let widened_up = recorded(&[(3, 10, -1), (2, 10, -1)]);
    assert_eq!(
        widened_up.as_slice(),
        &[
            RowBandMove {
                top: 3,
                bottom: 10,
                delta: -1
            },
            RowBandMove {
                top: 2,
                bottom: 10,
                delta: -1
            },
        ],
        "an upward band widened from above destroys rows the first never touched: two entries"
    );
    assert!(!widened_up.inexact);
    let narrowed_up = recorded(&[(3, 10, -1), (4, 10, -1)]);
    assert_eq!(
        narrowed_up.len, 2,
        "an upward band narrowed from above excludes moved rows"
    );
    assert!(!narrowed_up.inexact);

    let other_top = recorded(&[(11, 56, 5), (17, 56, 1)]);
    assert_eq!(
        other_top.as_slice(),
        &[
            RowBandMove {
                top: 11,
                bottom: 56,
                delta: 5
            },
            RowBandMove {
                top: 17,
                bottom: 56,
                delta: 1
            },
        ]
    );
    assert!(!other_top.inexact);

    let mixed_sign = recorded(&[(1, 3, -1), (1, 3, 1)]);
    assert_eq!(mixed_sign.len, 2, "mixed signs never compose");
    assert!(!mixed_sign.inexact);

    let other_bottom = recorded(&[(1, 5, 1), (1, 6, 1)]);
    assert_eq!(
        other_bottom.len, 2,
        "a different bottom is a different band"
    );

    let nine: Vec<(u16, u16, i16)> = (0..9u16).map(|i| (i, 20 + i, 1)).collect();
    let overflowed = recorded(&nine);
    assert_eq!(usize::from(overflowed.len), ROW_BAND_MOVES_PER_BATCH);
    assert!(
        overflowed.inexact,
        "a ninth distinct band poisons the record"
    );

    let overflowed_delta = recorded(&[(0, 5, i16::MAX), (0, 5, 1)]);
    assert!(
        overflowed_delta.inexact,
        "a composed displacement past i16 poisons"
    );

    assert_eq!(recorded(&[(3, 3, 0), (5, 2, 1)]), RowBandMoves::default());
}

/// Every coordinate move that is NOT a row translate poisons the record: a
/// margined (DECLRMM) SU moves a rectangle, DECIC moves cells within rows, a forced
/// invalidation replaces the coordinate space. Full-width IL and DL are row
/// translates and record exact bands over `cursor_row..=region.bottom`.
#[test]
fn margined_scrolls_line_ops_and_forced_invalidations_poison_the_band_record() {
    let mut grid = Grid::new(20, 20);
    grid.set_scroll_region(1, 5);
    grid.set_horizontal_margins(1, 5);
    grid.set_cursor(1, 1);
    grid.scroll_region_up_margined(1, 1, 5);
    assert!(
        grid.take_row_band_moves().inexact,
        "a margined SU is a rectangle"
    );
    assert!(grid.take_coordinates_invalidated());

    grid.set_cursor(2, 2);
    grid.insert_columns(2);
    assert!(
        grid.take_row_band_moves().inexact,
        "DECIC moves cells, not rows"
    );
    assert!(grid.take_coordinates_invalidated());

    grid.force_selection_invalidation();
    assert!(
        grid.take_row_band_moves().inexact,
        "a forced invalidation is wholesale"
    );
    assert!(grid.take_coordinates_invalidated());

    grid.invalidate_host_coordinates();
    assert!(
        grid.take_row_band_moves().inexact,
        "a buffer swap is wholesale"
    );
    assert!(grid.take_coordinates_invalidated());

    grid.reset_horizontal_margins();
    grid.set_scroll_region(2, 10);
    grid.set_cursor(4, 3);
    grid.insert_lines(2);
    assert_eq!(
        grid.take_row_band_moves(),
        one(4, 10, 2),
        "IL is a region-down band"
    );
    assert!(grid.take_coordinates_invalidated());

    grid.set_cursor(4, 3);
    grid.delete_lines(2);
    assert_eq!(
        grid.take_row_band_moves(),
        one(4, 10, -2),
        "DL is a region-up band"
    );
    assert!(grid.take_coordinates_invalidated());
}
