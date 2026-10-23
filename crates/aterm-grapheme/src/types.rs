// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Structs and enums for grapheme cluster metadata.

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

impl Grapheme<'_> {
    /// Byte length of this grapheme cluster slice.
    #[must_use]
    #[inline]
    pub const fn byte_len(&self) -> usize {
        self.text.len()
    }

    /// Return `true` iff this grapheme occupies 0 terminal cells.
    #[must_use]
    #[inline]
    pub const fn is_zero_width(&self) -> bool {
        self.width == 0
    }

    /// Return `true` iff this grapheme occupies 2 terminal cells.
    #[must_use]
    #[inline]
    pub const fn is_wide(&self) -> bool {
        self.width == 2
    }

    /// Return `true` iff this grapheme consists of a single Unicode codepoint.
    #[must_use]
    #[inline]
    pub const fn is_single_codepoint(&self) -> bool {
        self.codepoint_count == 1
    }
}

impl GraphemeInfo {
    /// Return `true` iff the analyzed string contains 0 bytes.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.byte_count == 0
    }

    /// Return `true` iff every byte is a single-width ASCII grapheme.
    #[must_use]
    #[inline]
    pub const fn is_ascii_single_width(&self) -> bool {
        self.byte_count == self.grapheme_count
            && self.grapheme_count == self.display_width
            && self.codepoint_count == self.grapheme_count
            && !self.has_emoji
            && !self.has_combining
            && !self.has_wide
    }

    /// Encode the four primary 32-bit counts into 16 little-endian bytes.
    #[must_use]
    #[inline]
    pub const fn encode_counts_le(&self) -> [u8; 16] {
        let g = self.grapheme_count as u32;
        let w = self.display_width as u32;
        let c = self.codepoint_count as u32;
        let b = self.byte_count as u32;
        [
            g as u8,
            (g >> 8) as u8,
            (g >> 16) as u8,
            (g >> 24) as u8,
            w as u8,
            (w >> 8) as u8,
            (w >> 16) as u8,
            (w >> 24) as u8,
            c as u8,
            (c >> 8) as u8,
            (c >> 16) as u8,
            (c >> 24) as u8,
            b as u8,
            (b >> 8) as u8,
            (b >> 16) as u8,
            (b >> 24) as u8,
        ]
    }

    /// Decode 16 little-endian bytes into the four primary 32-bit counts.
    #[must_use]
    #[inline]
    pub const fn decode_counts_le(bytes: [u8; 16]) -> (u32, u32, u32, u32) {
        let g = (bytes[0] as u32)
            | ((bytes[1] as u32) << 8)
            | ((bytes[2] as u32) << 16)
            | ((bytes[3] as u32) << 24);
        let w = (bytes[4] as u32)
            | ((bytes[5] as u32) << 8)
            | ((bytes[6] as u32) << 16)
            | ((bytes[7] as u32) << 24);
        let c = (bytes[8] as u32)
            | ((bytes[9] as u32) << 8)
            | ((bytes[10] as u32) << 16)
            | ((bytes[11] as u32) << 24);
        let b = (bytes[12] as u32)
            | ((bytes[13] as u32) << 8)
            | ((bytes[14] as u32) << 16)
            | ((bytes[15] as u32) << 24);
        (g, w, c, b)
    }
}
