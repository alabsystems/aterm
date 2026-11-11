// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn configure_window_omits_words_above_u16_mask() {
    let values = ValueList::new().set(1, 100).set(2, 200).set(1 << 17, 999);
    let req = configure_window(42, &values);
    let b = req.bytes();
    let word_len = u16::from_ne_bytes([b[2], b[3]]);
    let mask = u16::from_ne_bytes([b[8], b[9]]);
    assert_eq!(mask, 0b11);
    assert_eq!(usize::from(word_len), 3 + mask.count_ones() as usize);
    assert_eq!(b.len(), usize::from(word_len) * 4);
}

#[test]
fn change_property_invalid_format_keeps_wire_framing_consistent() {
    let req = change_property(PropMode::Replace, 1, 2, 3, 0, &[1, 2, 3, 4]);
    let b = req.bytes();
    assert_eq!(b.len(), 24);
    assert_eq!(u32::from_ne_bytes([b[20], b[21], b[22], b[23]]), 0);
}
