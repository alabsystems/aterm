// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![deny(unsafe_op_in_unsafe_fn)]

//! Grapheme cluster support for terminal text handling.
//!
//! This crate provides Unicode grapheme cluster segmentation and width calculation
//! for proper terminal text handling. A grapheme cluster is what users perceive as
//! a single "character", even when composed of multiple Unicode codepoints.
//!
//! # Why Grapheme Clusters Matter for Terminals
//!
//! Terminals must correctly handle complex Unicode text:
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
//! This crate builds on:
//! - An inline grapheme cluster iterator implementing UAX #29 boundary rules (#7698)
//! - Internal Unicode 16.0 width tables for display width calculation (wcwidth equivalent)

#![deny(clippy::all)]

mod emoji_presentation;
mod grapheme_iter;
mod position;
pub mod tables;
mod types;
mod width;

// === Public API ===
pub use emoji_presentation::{is_emoji_presentation, is_vs16_emoji_capable};
pub use grapheme_iter::{GraphemeClusters, GraphemeIndices, Graphemes};
pub use position::{byte_to_column, column_to_char_index};
pub use tables::{char_width, char_width_cjk, is_ambiguous_width, str_width, str_width_cjk};
pub use types::{Grapheme, GraphemeInfo};
pub use width::{
    grapheme_display_width, grapheme_display_width_with_config, grapheme_width,
    grapheme_width_with_config, is_emoji_char, is_emoji_modifier_base, split_graphemes,
    split_graphemes_with_config,
};

// === Test/Kani-only API ===
#[cfg(any(test, kani))]
pub use position::{
    GraphemeCells, GraphemeSegmenter, ascii_width, assign_cells, column_to_byte, grapheme_at_byte,
    grapheme_at_column, is_ascii_only, pad_to_width, truncate_to_width,
};
#[cfg(any(test, kani))]
pub use types::GraphemeType;
#[cfg(any(test, kani))]
pub use width::{
    classify_grapheme, has_skin_tone, has_zwj, is_flag_emoji, is_regional_indicator,
    is_skin_tone_modifier,
};

#[cfg(test)]
mod ucs_detect_tests;

#[cfg(test)]
mod tests;

#[cfg(kani)]
mod verification;
