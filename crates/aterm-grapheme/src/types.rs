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

#[cfg(any(test, kani))]
impl Grapheme<'_> {
    /// Check if this grapheme is a single ASCII character.
    ///
    /// REQUIRES: `self.text` is valid UTF-8 (`&str` invariant).
    /// ENSURES: returns `true` iff `self.text` has length 1 and its byte is `< 128`.
    #[inline]
    #[must_use]
    pub fn is_ascii(&self) -> bool {
        self.text.len() == 1 && self.text.as_bytes()[0] < 128
    }

    /// Check if this grapheme is whitespace.
    ///
    /// REQUIRES: `self.text` is valid UTF-8 (`&str` invariant).
    /// ENSURES: returns `true` iff every codepoint in `self.text` satisfies `char::is_whitespace()`.
    #[inline]
    #[must_use]
    pub fn is_whitespace(&self) -> bool {
        self.text.chars().all(char::is_whitespace)
    }

    /// Check if this grapheme is a control character.
    ///
    /// REQUIRES: `self.text` is valid UTF-8 (`&str` invariant).
    /// ENSURES: returns `true` iff at least one codepoint in `self.text` satisfies `char::is_control()`.
    #[inline]
    #[must_use]
    pub fn is_control(&self) -> bool {
        self.text.chars().any(char::is_control)
    }
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

/// Classify the type of a grapheme for rendering decisions.
#[cfg(any(test, kani))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphemeType {
    /// Simple ASCII character
    Ascii,
    /// CJK or other wide character (2 cells)
    Wide,
    /// Emoji (typically 2 cells)
    Emoji,
    /// ZWJ sequence (emoji joined by Zero Width Joiner)
    ZwjSequence,
    /// Flag emoji (regional indicator pair)
    Flag,
    /// Character with combining marks
    Combining,
    /// Control character (0 width)
    Control,
    /// Other Unicode character
    Other,
}
