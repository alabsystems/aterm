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
    let major64 = major as u64;
    let bias = major64.saturating_sub(1) / 2;
    let start64 = start as u64;

    if dc >= dr {
        let num0 = start64 * (dr as u64) + bias;
        let mut r = i64::from(origin.0) + sr * ((num0 / major64) as i64);
        let mut rem = (num0 % major64) as i64;
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
        let num0 = start64 * (dc as u64) + bias;
        let mut r = i64::from(origin.0) + sr * start;
        let mut c = i64::from(origin.1) + sc * ((num0 / major64) as i64);
        let mut rem = (num0 % major64) as i64;
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
#[path = "trail_sweep_tests.rs"]
mod tests;
