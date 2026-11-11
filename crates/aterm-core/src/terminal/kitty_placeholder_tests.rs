// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn table_is_sorted_and_full() {
    assert_eq!(DIACRITICS.len(), 297);
    assert!(
        DIACRITICS.windows(2).all(|w| w[0] < w[1]),
        "table must be strictly ascending for binary_search"
    );
}

#[test]
fn known_diacritic_values() {
    assert_eq!(diacritic_value('\u{0305}'), Some(0));
    assert_eq!(diacritic_value('\u{030D}'), Some(1));
    assert_eq!(diacritic_value('\u{030E}'), Some(2));
    assert_eq!(diacritic_value('\u{1D244}'), Some(296));
    assert_eq!(diacritic_value('A'), None);
    assert_eq!(diacritic_value(PLACEHOLDER), None);
}
