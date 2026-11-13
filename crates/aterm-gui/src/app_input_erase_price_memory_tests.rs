// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ErasePriceMemory, grapheme_advances};

#[test]
fn large_commits_keep_only_the_bounded_erase_tail_without_retaining_their_allocation() {
    let long = "x日e\u{301}👩🏽‍💻·".repeat(4096);
    for double_width in [false, true] {
        let mut memory = ErasePriceMemory::default();
        let mut reference = Vec::new();
        memory.push(1);
        memory.push_zeros(ErasePriceMemory::CAP);
        reference.resize(ErasePriceMemory::CAP, 0);
        for text in ["ab", "\u{301}", "\u{301}日本", long.as_str(), "⚠️z"] {
            if aterm_grapheme::str_width(text) > 0 {
                reference.extend(grapheme_advances(text, double_width));
            }
            if text == long {
                assert!(reference.capacity() > 2 * ErasePriceMemory::CAP);
            }
            let excess = reference.len().saturating_sub(ErasePriceMemory::CAP);
            reference.drain(..excess);
            memory.price_graphemes(text, double_width);
            assert_eq!(memory.widths(), reference, "{double_width}");
            assert!(memory.0.capacity() < 4 * ErasePriceMemory::CAP);
            assert_eq!(memory.pop(), reference.pop());
        }
        memory.clear();
        assert!(memory.widths().is_empty());
        assert!(memory.0.capacity() < 4 * ErasePriceMemory::CAP);
        memory.price_graphemes("é", double_width);
        assert_eq!(memory.pop(), Some(if double_width { 2 } else { 1 }));
        assert_eq!(memory.pop(), None);
    }
}
