// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{GLYPHS, bitmap};

/// The binary search needs a sorted table, so the ordering is asserted.
#[test]
fn the_table_is_sorted_and_has_no_duplicates() {
    for pair in GLYPHS.windows(2) {
        let (a, b) = (pair[0].0, pair[1].0);
        assert!(a < b, "table out of order at U+{a:04X} / U+{b:04X}");
    }
}

/// Every family the material sampler advertises is present, and something
/// outside them is absent.
#[test]
fn every_advertised_family_is_present() {
    for (label, ch) in [
        ("basic latin", 'A'),
        ("latin-1", 'e'),
        ("greek", '\u{3b1}'),
        ("box drawing", '\u{2500}'),
        ("block elements", '\u{2588}'),
        ("hiragana", '\u{3042}'),
    ] {
        assert!(bitmap(ch).is_some(), "{label} missing from the table");
    }
    assert!(bitmap(' ').is_none(), "a blank must stay unsupported");
    assert!(
        bitmap('\u{4e00}').is_none(),
        "CJK is not a supported family"
    );
}
