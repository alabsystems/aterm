// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Adapted encoding tests to deterministic cases without upstream test packages.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
#[test]
fn roundtrip() {
    for value in [0, 1, 255, 256, Word::MAX / 2, Word::MAX] {
        let a = Limb(value);
        assert_eq!(a, Limb::from_be_bytes(a.to_be_bytes()));
        assert_eq!(a, Limb::from_le_bytes(a.to_le_bytes()));
    }
}

#[test]
fn reverse() {
    for value in [0, 1, 255, 256, Word::MAX / 2, Word::MAX] {
        let a = Limb(value);
        let mut bytes = a.to_be_bytes();
        bytes.reverse();
        assert_eq!(a, Limb::from_le_bytes(bytes));

        let mut bytes = a.to_le_bytes();
        bytes.reverse();
        assert_eq!(a, Limb::from_be_bytes(bytes));
    }
}
