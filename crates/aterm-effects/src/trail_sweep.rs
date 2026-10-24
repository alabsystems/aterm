// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared cell-sweep primitives for laying continuous cursor-trail wakes.

/// Writes the up-to-`limit` Bresenham cells nearest `destination` along `origin` to `destination`
/// into `out`.
#[inline]
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
    let available = major + i64::from(include_destination);
    let keep = available.min(i64::try_from(limit).unwrap_or(i64::MAX));
    if keep <= 0 {
        return;
    }
    out.reserve(keep as usize);
    let start = available - keep;
    if dr == 0 {
        let sc = if origin.1 < destination.1 {
            1i64
        } else {
            -1i64
        };
        let r = origin.0;
        let c0 = i64::from(origin.1);
        for k in start..available {
            let c_steps = if major == 0 { 0 } else { k };
            out.push((r, (c0 + sc * c_steps) as i32));
        }
        return;
    }
    if dc == 0 {
        let sr = if origin.0 < destination.0 {
            1i64
        } else {
            -1i64
        };
        let r0 = i64::from(origin.0);
        let c = origin.1;
        for k in start..available {
            out.push(((r0 + sr * k) as i32, c));
        }
        return;
    }
    let sr = if origin.0 < destination.0 {
        1i128
    } else {
        -1i128
    };
    let sc = if origin.1 < destination.1 {
        1i128
    } else {
        -1i128
    };
    let bias = i128::from(major.saturating_sub(1)) / 2;
    let origin_r = i128::from(origin.0);
    let origin_c = i128::from(origin.1);
    let major128 = i128::from(major);

    if dc >= dr {
        let dr128 = i128::from(dr);
        for k in start..available {
            let k = i128::from(k);
            let r_steps = (k * dr128 + bias) / major128;
            let r = origin_r + sr * r_steps;
            let c = origin_c + sc * k;
            out.push((r as i32, c as i32));
        }
    } else {
        let dc128 = i128::from(dc);
        for k in start..available {
            let k = i128::from(k);
            let c_steps = (k * dc128 + bias) / major128;
            let r = origin_r + sr * k;
            let c = origin_c + sc * c_steps;
            out.push((r as i32, c as i32));
        }
    }
}

/// Appends the same-row cells from `from_col` (exclusive) to `to_col` (inclusive) into `out`.
#[cfg(test)]
pub fn row_sweep_cells(out: &mut Vec<(i32, i32)>, row: i32, from_col: i32, to_col: i32) {
    if to_col > from_col {
        out.extend((from_col + 1..=to_col).map(|c| (row, c)));
    } else {
        out.extend((to_col..from_col).rev().map(|c| (row, c)));
    }
}

/// Writes the typewriter-wrap cell sweep from `prev` across `cols` to `cur` into `out`.
#[cfg(test)]
pub fn wrap_fold_cells(out: &mut Vec<(i32, i32)>, prev: (i32, i32), cur: (i32, i32), cols: i32) {
    wrap_fold_cells_in_pane(out, prev, cur, 0, cols);
}

/// Writes the typewriter-wrap cell sweep within a pane starting at `col0` of width `cols` into
/// `out`.
#[cfg(test)]
pub fn wrap_fold_cells_in_pane(
    out: &mut Vec<(i32, i32)>,
    prev: (i32, i32),
    cur: (i32, i32),
    col0: i32,
    cols: i32,
) {
    out.clear();
    let (pr, pc) = prev;
    let (cr, cc) = cur;
    let col1 = col0.saturating_add(cols.max(0));
    out.extend((pc + 1..col1).map(|c| (pr, c)));
    out.extend((col0..=cc).map(|c| (cr, c)));
}

/// Returns whether `prev` to `cur` is a physical margin wrap from the last pane column to `col0` on
/// the next row.
#[inline]
#[must_use]
pub const fn physical_margin_fold(prev: (u16, u16), cur: (u16, u16), col0: u16, cols: u16) -> bool {
    if cols == 0 {
        return false;
    }
    let col1 = (col0 as u32) + (cols as u32);
    let last = col1.saturating_sub(1);
    matches!(prev.0.checked_add(1), Some(r) if r == cur.0)
        && (prev.1 as u32) == last
        && cur.1 == col0
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn wrap_fold_finishes_old_row_and_starts_new() {
        let mut out = Vec::new();
        wrap_fold_cells(&mut out, (5, 38), (6, 1), 40);
        assert_eq!(out, vec![(5, 39), (6, 0), (6, 1)]);
        let seam = out.iter().position(|&(r, _)| r == 6).unwrap() - 1;
        assert_adjacent(&out, Some(seam));
        wrap_fold_cells(&mut out, (5, 39), (6, 0), 40);
        assert_eq!(out, vec![(6, 0)]);

        wrap_fold_cells_in_pane(&mut out, (5, 51), (6, 12), 12, 40);
        assert_eq!(out, vec![(6, 12)]);
        assert!(physical_margin_fold((5, 51), (6, 12), 12, 40));
        assert!(
            !physical_margin_fold((5, 51), (6, 11), 12, 40),
            "a neighboring pane cannot borrow the fold shape"
        );
    }
}
