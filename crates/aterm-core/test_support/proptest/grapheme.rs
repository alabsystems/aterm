// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grapheme width property tests. This file was `scrollback.rs`; its Scrollback
//! properties moved to aterm-scrollback beside the type; the RLE and trigger
//! properties were deleted (aterm-rle's exhaustive and fuzz suites cover RLE,
//! and the test-only trigger module is gone).

use proptest::prelude::*;

// ============== Grapheme Property Tests (#1931) ==============

proptest! {
    /// Grapheme display width is always 0, 1, or 2.
    ///
    /// Property: For any single grapheme, display width is bounded [0, 2].
    #[test]
    fn grapheme_width_bounded(c in proptest::char::any()) {
        use crate::grapheme::grapheme_display_width;

        let s = c.to_string();
        let width = grapheme_display_width(&s);

        prop_assert!(
            width <= 2,
            "grapheme_display_width({:?}) = {} should be <= 2",
            s, width
        );
    }

    /// ASCII printable characters have width 1.
    ///
    /// Property: For printable ASCII (0x20-0x7E), display width is 1.
    #[test]
    fn grapheme_ascii_printable_width_one(c in 0x20u8..=0x7E) {
        use crate::grapheme::grapheme_display_width;

        let s = String::from(c as char);
        let width = grapheme_display_width(&s);

        prop_assert_eq!(
            width, 1,
            "ASCII printable {:?} (0x{:02x}) should have width 1, got {}",
            s, c, width
        );
    }

    /// grapheme_width aggregate is consistent with split_graphemes.
    ///
    /// Property: The aggregate display_width equals the sum of individual
    /// grapheme widths from split_graphemes.
    #[test]
    fn grapheme_width_consistent_with_split(s in "[a-zA-Z0-9 ]{0,50}") {
        use crate::grapheme::{grapheme_width, split_graphemes};

        let info = grapheme_width(&s);
        let split_width: usize = split_graphemes(&s).map(|g| g.width).sum();
        let split_count: usize = split_graphemes(&s).count();

        prop_assert_eq!(
            info.display_width, split_width,
            "aggregate width {} should match sum of splits {}",
            info.display_width, split_width
        );
        prop_assert_eq!(
            info.grapheme_count, split_count,
            "aggregate count {} should match split count {}",
            info.grapheme_count, split_count
        );
    }

    /// grapheme_width byte_count always matches string length.
    ///
    /// Property: byte_count == s.len() for any input.
    #[test]
    fn grapheme_byte_count_matches(s in "\\PC{0,100}") {
        use crate::grapheme::grapheme_width;

        let info = grapheme_width(&s);

        prop_assert_eq!(
            info.byte_count, s.len(),
            "byte_count {} should match s.len() {}",
            info.byte_count, s.len()
        );
    }
}
