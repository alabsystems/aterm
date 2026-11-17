// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn claim(byte: u8) -> Vec<u8> {
    let mut claim = vec![byte; CLAIM_LEN];
    claim[4] = b'1';
    claim
}

/// The cold payloads round-trip, and open only for the custody and under the key that kept them,
/// and not once a byte has changed.
#[test]
fn cold_payloads_round_trip_and_open_only_for_their_custody_and_key() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = root.path();
    let bound = 1 << 20;
    assert!(read(dir, &claim(1), &[7; 32], bound).unwrap().is_empty());
    let payloads: Payloads = [([1; 32], vec![1, 2, 3]), ([2; 32], Vec::new())]
        .into_iter()
        .map(|(id, payload)| (id, Zeroizing::new(payload)))
        .collect();
    write(dir, &claim(1), &[7; 32], &payloads).unwrap();
    assert_eq!(read(dir, &claim(1), &[7; 32], bound).unwrap(), payloads);
    for (claim, key) in [(claim(2), [7; 32]), (claim(1), [8; 32])] {
        let error = read(dir, &claim, &key, bound).unwrap_err();
        assert!(error.0.contains("do not open"), "{}", error.0);
    }
    let mut bytes = std::fs::read(path(dir)).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(path(dir), &bytes).unwrap();
    assert!(read(dir, &claim(1), &[7; 32], bound).is_err());
    assert!(write(dir, b"not a claim", &[7; 32], &payloads).is_err());
}
