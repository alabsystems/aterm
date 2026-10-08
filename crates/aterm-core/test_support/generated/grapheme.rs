// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Grapheme width properties over generated input.

use crate::grapheme::{grapheme_display_width, grapheme_width, split_graphemes};
use aterm_spec::rng::XorShift;

/// Grapheme display width is always 0, 1, or 2, for every scalar value.
#[test]
fn grapheme_width_bounded() {
    for c in (0..=0x10FFFFu32).step_by(7).filter_map(char::from_u32) {
        let s = c.to_string();
        let width = grapheme_display_width(&s);
        assert!(width <= 2, "grapheme_display_width({s:?}) = {width}");
    }
}

/// Printable ASCII (0x20-0x7E) has width 1.
#[test]
fn grapheme_ascii_printable_width_one() {
    for c in 0x20u8..=0x7E {
        let s = String::from(c as char);
        assert_eq!(grapheme_display_width(&s), 1, "{s:?}");
    }
}

/// The aggregate width and count equal the sums over `split_graphemes`.
#[test]
fn grapheme_width_consistent_with_split() {
    const ALPHABET: &[u8] = b"abcXYZ0189 ";
    let mut rng = XorShift::new(0x6EA9_4E3F);
    for _ in 0..256 {
        let s: String = (0..rng.range(0, 51))
            .map(|_| *rng.pick(ALPHABET) as char)
            .collect();
        let info = grapheme_width(&s);
        let split_width: usize = split_graphemes(&s).map(|g| g.width).sum();
        assert_eq!(info.display_width, split_width, "{s:?}");
        assert_eq!(info.grapheme_count, split_graphemes(&s).count(), "{s:?}");
    }
}

/// `byte_count` matches the string length for any printable input.
#[test]
fn grapheme_byte_count_matches() {
    let mut rng = XorShift::new(0xB17E_C0DF);
    for _ in 0..256 {
        let s: String = (0..rng.range(0, 101))
            .filter_map(|_| char::from_u32(rng.range(0x20, 0x2_0000) as u32))
            .filter(|c| !c.is_control())
            .collect();
        assert_eq!(grapheme_width(&s).byte_count, s.len(), "{s:?}");
    }
}
