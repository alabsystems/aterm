// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! Pattern-based smart selection and mouse-driven text selection for terminal cells.

#![warn(missing_docs)]

mod builtin_patterns;
mod rules;
mod text_selection;

pub use rules::SelectionRuleKind;
pub use rules::SmartSelection;

pub use text_selection::SelectionState;
pub use text_selection::{
    SelectionAnchor, SelectionProjection, SelectionSide, SelectionType, TextSelection,
};

/// Return `true` when `(start_row, start_col) <= (end_row, end_col)` in row-major order.
#[must_use]
#[inline]
pub const fn is_ordered_cell_pair(
    start_row: i64,
    start_col: usize,
    end_row: i64,
    end_col: usize,
) -> bool {
    if start_row < end_row {
        true
    } else if start_row > end_row {
        false
    } else {
        start_col <= end_col
    }
}

/// Return the column span width `end_col.saturating_sub(start_col)`.
#[must_use]
#[inline]
pub const fn cell_span_width(start_col: usize, end_col: usize) -> usize {
    end_col.saturating_sub(start_col)
}

/// Encode selection bounds (`start_row`, `start_col`, `end_row`, `end_col`) into 16 LE bytes.
#[must_use]
#[inline]
pub const fn encode_selection_bounds_le(
    start_row: i32,
    start_col: u32,
    end_row: i32,
    end_col: u32,
) -> [u8; 16] {
    let sr = start_row as u32;
    let er = end_row as u32;
    [
        sr as u8,
        (sr >> 8) as u8,
        (sr >> 16) as u8,
        (sr >> 24) as u8,
        start_col as u8,
        (start_col >> 8) as u8,
        (start_col >> 16) as u8,
        (start_col >> 24) as u8,
        er as u8,
        (er >> 8) as u8,
        (er >> 16) as u8,
        (er >> 24) as u8,
        end_col as u8,
        (end_col >> 8) as u8,
        (end_col >> 16) as u8,
        (end_col >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into selection bounds (`start_row`, `start_col`, `end_row`, `end_col`).
#[must_use]
#[inline]
pub const fn decode_selection_bounds_le(bytes: [u8; 16]) -> (i32, u32, i32, u32) {
    let sr = (bytes[0] as u32)
        | ((bytes[1] as u32) << 8)
        | ((bytes[2] as u32) << 16)
        | ((bytes[3] as u32) << 24);
    let sc = (bytes[4] as u32)
        | ((bytes[5] as u32) << 8)
        | ((bytes[6] as u32) << 16)
        | ((bytes[7] as u32) << 24);
    let er = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let ec = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (sr as i32, sc, er as i32, ec)
}

#[cfg(test)]
mod tests;
