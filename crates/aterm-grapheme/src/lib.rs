// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![deny(unsafe_op_in_unsafe_fn)]

//! Grapheme cluster support for terminal text handling.
//!
//! Unicode grapheme cluster segmentation and width calculation. A grapheme
//! cluster is what users perceive as a single "character", even when composed
//! of multiple codepoints.
//!
//! # Why grapheme clusters matter for terminals
//!
//! - **Emoji sequences**: family emoji is 7 codepoints but displays as 1 grapheme (2 cells wide)
//! - **Combining marks**: "e" + combining acute is 2 codepoints, 1 grapheme
//! - **Regional indicators**: flag emoji is two codepoints but one flag
//! - **Skin tone modifiers**: wave + modifier is 2 codepoints, 1 grapheme
//! - **ZWJ sequences**: Family emoji joined with Zero Width Joiner
//!
//! # Example
//!
//! ```
//! use aterm_grapheme::{GraphemeInfo, grapheme_width};
//!
//! // Simple text
//! let info = grapheme_width("Hello");
//! assert_eq!(info.grapheme_count, 5);
//! assert_eq!(info.display_width, 5);
//! ```
//!
//! # Architecture
//!
//! - A grapheme cluster iterator implementing the UAX #29 boundary rules
//! - Internal Unicode 16.0 width tables for display width (wcwidth equivalent)

#![deny(clippy::all)]

mod emoji_presentation;
mod grapheme_iter;
mod grid;
mod position;
pub mod tables;
mod types;
mod width;
mod word;

// === Public API ===
pub use emoji_presentation::{is_emoji_presentation, is_vs16_emoji_capable};
pub use grapheme_iter::{GraphemeClusters, GraphemeIndices, Graphemes};
pub use grid::{
    GridColumns, GridGrapheme, GridGraphemes, GridStep, grid_graphemes, grid_graphemes_with,
    str_grid_columns, str_grid_columns_with,
};
pub use position::{byte_to_column, byte_to_column_with, column_to_char_index};
pub use tables::{char_width, char_width_cjk, is_ambiguous_width, str_width};
pub use types::{Grapheme, GraphemeInfo};
pub use width::{
    grapheme_display_width, grapheme_display_width_with_config, grapheme_width,
    grapheme_width_with_config, is_emoji_char, is_emoji_modifier_base, split_graphemes,
    split_graphemes_with_config,
};
pub use word::{Words, is_word_char, word_break_before, word_start_before, words};

// === Test-only API ===
#[cfg(test)]
pub use position::{
    GraphemeCells, GraphemeSegmenter, ascii_width, assign_cells, column_to_byte, grapheme_at_byte,
    grapheme_at_column, is_ascii_only, pad_to_width, truncate_to_width,
};
#[cfg(test)]
pub use types::GraphemeType;
#[cfg(test)]
pub use width::{
    classify_grapheme, has_skin_tone, has_zwj, is_flag_emoji, is_regional_indicator,
    is_skin_tone_modifier,
};

#[cfg(test)]
mod ucs_detect_tests;

#[cfg(test)]
mod tests;
