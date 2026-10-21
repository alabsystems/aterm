// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// The anchor attributes are `macro_rules!` ones where Cargo resolved no proc macro
// under them (crates/aterm-spec-anchor/anchor_rules.rs).
#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

//! Selection system for terminal text.
//!
//! This crate provides two selection systems:
//!
//! ## Smart Selection (Pattern-based)
//!
//! Intelligent selection rules that recognize and select semantic text units
//! like URLs, file paths, email addresses, IP addresses, git hashes, and
//! quoted strings.
//!
//! - **Pattern-based rules**: Use regex patterns to identify semantic text units
//! - **Built-in rules**: Pre-configured rules for common patterns (URLs, paths, etc.)
//! - **Priority-based**: Rules are matched in priority order
//!
//! ### Example
//!
//! ```
//! use aterm_selection::SmartSelection;
//!
//! let smart = SmartSelection::with_builtin_rules();
//!
//! // Given a line of text and a cursor column
//! let line = "Check out https://example.com for more info";
//! let bounds = smart.word_boundaries_at(line, 15); // cursor is on the URL
//!
//! if let Some((start, end)) = bounds {
//!     assert_eq!(&line[start..end], "https://example.com");
//! }
//! ```
//!
//! ## Text Selection (Mouse-based)
//!
//! State machine for mouse-based text selection. Supports:
//!
//! - **Simple selection**: Character-by-character (single click + drag)
//! - **Block selection**: Rectangular selection (Alt + click + drag)
//! - **Semantic selection**: Word/URL selection (double-click)
//! - **Line selection**: Full line selection (triple-click)
//!
//! ### Example
//!
//! ```
//! use aterm_selection::{SelectionSide, SelectionType, TextSelection};
//!
//! let mut sel = TextSelection::new();
//!
//! // Start selection on mouse down
//! sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
//!
//! // Update on mouse drag
//! sel.update_selection(0, 15, SelectionSide::Right);
//!
//! // Complete on mouse up
//! sel.complete_selection();
//!
//! // Check if a cell is selected
//! assert!(sel.contains(0, 10));
//! ```

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
pub const fn cell_span_width(start_col: usize, end_col: usize) -> usize {
    end_col.saturating_sub(start_col)
}

/// Encode selection bounds (`start_row`, `start_col`, `end_row`, `end_col`) into 16 LE bytes.
#[must_use]
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
