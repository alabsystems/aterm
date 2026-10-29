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
    let keep = available.min(limit.min(i64::MAX as usize) as i64);
    if keep <= 0 {
        return;
    }
    out.reserve(keep as usize);
    let start = available - keep;
    if dr == 0 {
        let step_c = if major == 0 {
            0i64
        } else if origin.1 < destination.1 {
            1i64
        } else {
            -1i64
        };
        let r = origin.0;
        let mut c = i64::from(origin.1) + step_c * start;
        let mut idx = start;
        while idx < available {
            out.push((r, c as i32));
            c += step_c;
            idx += 1;
        }
        return;
    }
    if dc == 0 {
        let sr = if origin.0 < destination.0 {
            1i64
        } else {
            -1i64
        };
        let mut r = i64::from(origin.0) + sr * start;
        let c = origin.1;
        let mut idx = start;
        while idx < available {
            out.push((r as i32, c));
            r += sr;
            idx += 1;
        }
        return;
    }
    let sr = if origin.0 < destination.0 {
        1i64
    } else {
        -1i64
    };
    let sc = if origin.1 < destination.1 {
        1i64
    } else {
        -1i64
    };
    let bias = i128::from(major.saturating_sub(1)) / 2;
    let major128 = i128::from(major);
    let start128 = i128::from(start);

    if dc >= dr {
        let num0 = start128 * i128::from(dr) + bias;
        let mut r = i64::from(origin.0) + sr * ((num0 / major128) as i64);
        let mut rem = (num0 % major128) as i64;
        let mut c = i64::from(origin.1) + sc * start;
        let mut idx = start;
        while idx < available {
            out.push((r as i32, c as i32));
            c += sc;
            rem += dr;
            if rem >= major {
                rem -= major;
                r += sr;
            }
            idx += 1;
        }
    } else {
        let num0 = start128 * i128::from(dc) + bias;
        let mut r = i64::from(origin.0) + sr * start;
        let mut c = i64::from(origin.1) + sc * ((num0 / major128) as i64);
        let mut rem = (num0 % major128) as i64;
        let mut idx = start;
        while idx < available {
            out.push((r as i32, c as i32));
            r += sr;
            rem += dc;
            if rem >= major {
                rem -= major;
                c += sc;
            }
            idx += 1;
        }
    }
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
    fn a_physical_margin_fold_runs_from_the_panes_last_column_to_its_first() {
        assert!(physical_margin_fold((5, 51), (6, 12), 12, 40));
        assert!(
            !physical_margin_fold((5, 51), (6, 11), 12, 40),
            "a neighboring pane cannot borrow the fold shape"
        );
    }
}
