// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

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
//! use aterm_grapheme::{GraphemeInfo, grapheme_width};
//!
//! # Architecture
//!
//! - A grapheme cluster iterator implementing the UAX #29 boundary rules
//! - Internal Unicode 16.0 width tables for display width (wcwidth equivalent)

#![cfg_attr(
    trust_verify,
    feature(custom_inner_attributes, register_tool),
    register_tool(trust)
)]

mod emoji_presentation;
mod grapheme_iter;
mod grid;
mod hyphenate;
mod line_break;
mod position;
mod props;
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
pub use hyphenate::hyphen_points;
pub use line_break::{
    BreakKind, LineBreak, Piece, TokenKind, break_inside, break_into_line, is_break_space,
    line_breaks, never_starts_a_line, pieces, token_kind, word_breaks,
};
pub use position::{byte_to_column, byte_to_column_with, column_to_char_index};
pub use props::{
    GeneralCategory, PROPS_UNICODE_VERSION, Script, general_category, script, script_runs,
};
pub use tables::{char_width, char_width_cjk, is_ambiguous_width, str_width};
pub use types::{Grapheme, GraphemeInfo};
pub use width::{
    grapheme_display_width, grapheme_display_width_with_config, grapheme_width,
    grapheme_width_with_config, is_emoji_char, is_emoji_modifier_base, split_graphemes,
    split_graphemes_with_config,
};
pub use word::{Words, is_word_char, word_break_before, word_start_before, words};

#[cfg(test)]
mod tests;
