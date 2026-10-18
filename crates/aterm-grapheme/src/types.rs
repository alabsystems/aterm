// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Grapheme data types — structs and enums for grapheme cluster metadata.

/// Information about a single grapheme cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grapheme<'a> {
    /// The grapheme cluster string slice.
    pub text: &'a str,
    /// Byte offset in the source string.
    pub byte_offset: usize,
    /// Display width in terminal cells (0, 1, or 2).
    pub width: usize,
    /// Number of Unicode codepoints in this grapheme.
    pub codepoint_count: usize,
    /// Whether this is an emoji grapheme.
    pub is_emoji: bool,
    /// Whether this grapheme contains combining marks.
    pub has_combining: bool,
}

/// Aggregate information about graphemes in a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraphemeInfo {
    /// Total number of grapheme clusters.
    pub grapheme_count: usize,
    /// Total display width in terminal cells.
    pub display_width: usize,
    /// Total number of Unicode codepoints.
    pub codepoint_count: usize,
    /// Number of bytes in the string.
    pub byte_count: usize,
    /// Whether any grapheme is an emoji.
    pub has_emoji: bool,
    /// Whether any grapheme has combining marks.
    pub has_combining: bool,
    /// Whether any grapheme is wide (2 cells).
    pub has_wide: bool,
}
