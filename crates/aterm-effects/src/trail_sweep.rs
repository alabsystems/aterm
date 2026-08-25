// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The SHARED cursor-trail path substrate — the cell-sweep primitives every
//! trail engine lays its wake with, extracted so continuity is solved ONCE and
//! every style (fire, rainbow kitty, phaser, comet, …) inherits the same guarantees
//! while keeping its own tuning (lifetimes, coverage curves, palettes).
//!
//! ## The continuity contract
//!
//! Every primitive in this module returns a path whose consecutive cells are
//! CHEBYSHEV-ADJACENT (no cell-to-cell step larger than 1 in either axis) —
//! a trail laid along any of these paths can never show an interior hole. The
//! property tests at the bottom pin this for every primitive across sweeps of
//! geometries; new primitives added here must join them.
//!
//! ## Why trails get gaps (and what fixed the fire trail)
//!
//! Engines observe the cursor ONCE per rendered frame, so real movement
//! arrives quantized: a fast typing burst, a key-repeat run, or a PTY that
//! batches echoes can move the cursor several cells between observations, and
//! ConPTY hides the cursor entirely during each echo repaint. The fire trail
//! reads as gapless because it (a) sweeps the full vector of every observed
//! move, (b) renders real jumps as one pixel-space streak along the true jump
//! vector, and (c) bridges the OS hide window (see
//! [`crate::cursor_trail::HIDE_BRIDGE_MS`]). These primitives make (a) — and
//! the wrap/coalesce cases it misses — reusable:
//!
//! * [`line_cells_tail`] — the canonical bounded Bresenham sweep (tail→head),
//!   forward-byte-identical and O(limit) whatever the jump distance.
//! * [`row_sweep_cells`] — a same-row typed-coalesce sweep: the cells a
//!   batched echo skipped between two observations.
//! * [`wrap_fold_cells`] — the typewriter fold: a typing wrap finishes the old
//!   row and continues from the new row's start, never sweeping a diagonal
//!   across cells the cursor didn't visit.

/// Walk the straight cell-line from `origin` toward `destination` and return
/// (in `out`) the up-to-`limit` cells NEAREST the destination, ordered
/// tail→head (oldest first, destination-adjacent last). `include_destination`
/// controls whether the destination cell itself is laid (a live cursor usually
/// draws there). The start index is solved directly on the FORWARD Bresenham
/// lattice, so cost is O(limit), not O(distance), without changing the legacy
/// forward walk's asymmetric tie choices on shallow diagonals.
pub fn line_cells_tail(
    out: &mut Vec<(i32, i32)>,
    origin: (i32, i32),
    destination: (i32, i32),
    limit: usize,
    include_destination: bool,
) {
    out.clear();
    if limit == 0 {
        return;
    }
    let dr = (i64::from(destination.0) - i64::from(origin.0)).abs();
    let dc = (i64::from(destination.1) - i64::from(origin.1)).abs();
    let major = dr.max(dc);
    // Excluding the destination leaves indices 0..major; including it leaves
    // 0..=major. Keep the nearest suffix without materialising its prefix.
    let available = major + i64::from(include_destination);
    let keep = available.min(i64::try_from(limit).unwrap_or(i64::MAX));
    let start = available - keep;
    let sr = if origin.0 < destination.0 { 1i128 } else { -1i128 };
    let sc = if origin.1 < destination.1 { 1i128 } else { -1i128 };
    let bias = i128::from(major.saturating_sub(1)) / 2;

    for k in start..available {
        let k = i128::from(k);
        let (r_steps, c_steps) = if major == 0 {
            (0, 0)
        } else if dc >= dr {
            (
                (k * i128::from(dr) + bias) / i128::from(major),
                k,
            )
        } else {
            (
                k,
                (k * i128::from(dc) + bias) / i128::from(major),
            )
        };
        let r = i128::from(origin.0) + sr * r_steps;
        let c = i128::from(origin.1) + sc * c_steps;
        out.push((r as i32, c as i32));
    }
}

/// The SAME-ROW typed-coalesce sweep: the cells between two same-row
/// observations of a typing cursor, EXCLUDING the origin (its spark was laid
/// by the previous key) and INCLUDING the destination (the new head). Ordered
/// tail→head. Appends to `out` (the fold primitive chains it). A batched echo
/// that advanced the cursor `k` columns lays exactly the `k` cells a per-key
/// observer would have lit — the fast-typing hole this closes.
pub fn row_sweep_cells(out: &mut Vec<(i32, i32)>, row: i32, from_col: i32, to_col: i32) {
    if to_col > from_col {
        out.extend((from_col + 1..=to_col).map(|c| (row, c)));
    } else {
        out.extend((to_col..from_col).rev().map(|c| (row, c)));
    }
}

/// The TYPEWRITER FOLD for a typing wrap: finish the old row from the origin
/// to the right edge, then continue from the new row's left edge to the
/// landing cell — the ribbon folds around the line end instead of either
/// sweeping a diagonal across unvisited cells or leaving the rows disjoint.
/// `prev` must be at/near the right edge and `cur` at/near the new row's start
/// (the wrap SHAPE the caller already classified); `cols` is the grid width.
/// Ordered tail→head, origin excluded, destination included.
pub fn wrap_fold_cells(out: &mut Vec<(i32, i32)>, prev: (i32, i32), cur: (i32, i32), cols: i32) {
    out.clear();
    let (pr, pc) = prev;
    let (cr, cc) = cur;
    out.extend((pc + 1..cols).map(|c| (pr, c)));
    out.extend((0..=cc).map(|c| (cr, c)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every consecutive pair of a laid path must be Chebyshev-adjacent — the
    /// module's continuity contract. (The wrap fold is exempt at exactly its
    /// fold seam, where the path wraps the row edge by design.)
    fn assert_adjacent(path: &[(i32, i32)], allow_fold_at: Option<usize>) {
        for (i, w) in path.windows(2).enumerate() {
            if allow_fold_at == Some(i) {
                continue;
            }
            let ((r0, c0), (r1, c1)) = (w[0], w[1]);
            assert!(
                (r1 - r0).abs() <= 1 && (c1 - c0).abs() <= 1,
                "gap between {:?} and {:?} in {path:?}",
                w[0],
                w[1]
            );
        }
    }

    /// line_cells_tail: gap-free, bounded, origin-inclusive walk that keeps the
    /// destination-nearest cells, across a sweep of jump geometries.
    #[test]
    fn line_cells_tail_is_gapless_and_bounded() {
        let mut out = Vec::new();
        for &(origin, dest) in &[
            ((0, 0), (0, 12)),
            ((3, 7), (3, 2)),
            ((0, 0), (9, 9)),
            ((8, 1), (2, 30)),
            ((5, 5), (5, 5)),
            ((0, 40), (7, 0)),
        ] {
            for limit in [1usize, 2, 3, 8, 64] {
                for include in [false, true] {
                    line_cells_tail(&mut out, origin, dest, limit, include);
                    assert!(out.len() <= limit, "limit respected");
                    assert_adjacent(&out, None);
                    if include && origin != dest {
                        assert_eq!(*out.last().unwrap(), dest, "head is the destination");
                    }
                    if !include {
                        assert!(!out.contains(&dest), "destination excluded");
                    }
                }
            }
        }
    }

    /// The bounded solver must be byte-identical to the historical FORWARD
    /// Bresenham walk, including its direction-sensitive tie law. Reversing
    /// Bresenham is not equivalent: `(0,0)→(1,2)` chooses `(0,1)`, while a
    /// backward walk chooses `(1,1)`. Exhaust every small direction, limit and
    /// destination policy so a visually shifted diagonal cannot hide behind a
    /// handful of non-tie examples.
    #[test]
    fn bounded_tail_matches_forward_bresenham_exhaustively() {
        fn reference(origin: (i32, i32), destination: (i32, i32)) -> Vec<(i32, i32)> {
            let ((r0, c0), (r1, c1)) = (origin, destination);
            let (dr, dc) = ((r1 - r0).abs(), (c1 - c0).abs());
            let (sr, sc) = (if r0 < r1 { 1 } else { -1 }, if c0 < c1 { 1 } else { -1 });
            let mut err = dc - dr;
            let (mut r, mut c) = (r0, c0);
            let mut cells = Vec::new();
            loop {
                cells.push((r, c));
                if (r, c) == (r1, c1) {
                    return cells;
                }
                let e2 = 2 * err;
                if e2 > -dr {
                    err -= dr;
                    c += sc;
                }
                if e2 < dc {
                    err += dc;
                    r += sr;
                }
            }
        }

        let mut actual = Vec::new();
        for r0 in -4..=3 {
            for c0 in -4..=3 {
                for r1 in -4..=3 {
                    for c1 in -4..=3 {
                        for include_destination in [false, true] {
                            for limit in 0..=10 {
                                let mut expected = reference((r0, c0), (r1, c1));
                                if !include_destination {
                                    expected.pop();
                                }
                                let keep_from = expected.len().saturating_sub(limit);
                                expected.drain(..keep_from);
                                line_cells_tail(
                                    &mut actual,
                                    (r0, c0),
                                    (r1, c1),
                                    limit,
                                    include_destination,
                                );
                                assert_eq!(
                                    actual, expected,
                                    "({r0},{c0})→({r1},{c1}), limit={limit}, include={include_destination}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// row_sweep_cells: exactly the skipped cells, ordered tail→head, both
    /// directions, gap-free.
    #[test]
    fn row_sweep_lays_exactly_the_skipped_cells() {
        let mut out = Vec::new();
        row_sweep_cells(&mut out, 4, 3, 7);
        assert_eq!(out, vec![(4, 4), (4, 5), (4, 6), (4, 7)]);
        assert_adjacent(&out, None);
        out.clear();
        row_sweep_cells(&mut out, 2, 7, 3);
        assert_eq!(out, vec![(2, 6), (2, 5), (2, 4), (2, 3)]);
        assert_adjacent(&out, None);
    }

    /// wrap_fold_cells: the old row is finished, the new row is started, the
    /// only non-adjacent step is the fold seam itself, and both segments are
    /// internally gap-free.
    #[test]
    fn wrap_fold_finishes_old_row_and_starts_new() {
        let mut out = Vec::new();
        // Wrap from (5, 38) in a 40-col grid to (6, 1).
        wrap_fold_cells(&mut out, (5, 38), (6, 1), 40);
        assert_eq!(out, vec![(5, 39), (6, 0), (6, 1)]);
        let seam = out.iter().position(|&(r, _)| r == 6).unwrap() - 1;
        assert_adjacent(&out, Some(seam));
        // Wrap from the very last column: no old-row cells remain.
        wrap_fold_cells(&mut out, (5, 39), (6, 0), 40);
        assert_eq!(out, vec![(6, 0)]);
    }
}
