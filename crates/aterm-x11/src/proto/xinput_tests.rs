// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn xi_select_events_slices_mask_words_to_u16_header_count() {
    let mask = EventMask {
        deviceid: 2,
        mask: vec![0xaa55_aa55; usize::from(u16::MAX) + 5],
    };
    let req = xi_select_events(131, 9, &[mask]);
    let b = req.bytes();
    let mask_len = u16::from_ne_bytes([b[14], b[15]]);
    assert_eq!(mask_len, u16::MAX);
    assert_eq!(b.len(), 16 + usize::from(u16::MAX) * 4);
}
