// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn every_entry_sits_in_its_own_hash_slot() {
    let mut occupied = 0;
    for (slot, entry) in CSI_DISPATCH_TABLE.iter().enumerate() {
        if let Some((inter_enc, final_byte, _)) = *entry {
            occupied += 1;
            assert_eq!(
                csi_hash(inter_enc, final_byte),
                slot,
                "entry ({inter_enc:#04x}, {final_byte:#04x}) is in slot {slot}"
            );
        }
    }
    assert!(occupied > 0);
}

#[test]
fn test_unknown_returns_none() {
    assert_eq!(lookup_csi_handler(&[], b'Q'), None);
    assert_eq!(lookup_csi_handler(&[b'%'], b'A'), None);
    assert_eq!(lookup_csi_handler(&[b'?'], b'z'), None);
    assert_eq!(lookup_csi_handler(&[b'?', b'$', b'!'], b'p'), None);
    assert_eq!(lookup_csi_handler(&[b'>', b'$'], b'p'), None);
}
