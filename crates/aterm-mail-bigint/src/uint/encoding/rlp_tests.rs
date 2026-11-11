// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::U256;
use hex_literal::hex;

/// U256 test vectors from the `rlp` crate.
///
/// <https://github.com/paritytech/parity-common/blob/faad8b6/rlp/tests/tests.rs#L216-L222>
const U256_VECTORS: &[(U256, &[u8])] = &[
    (U256::ZERO, &hex!("80")),
    (
        U256::from_be_hex("0000000000000000000000000000000000000000000000000000000001000000"),
        &hex!("8401000000"),
    ),
    (
        U256::from_be_hex("00000000000000000000000000000000000000000000000000000000ffffffff"),
        &hex!("84ffffffff"),
    ),
    (
        U256::from_be_hex("8090a0b0c0d0e0f00910203040506077000000000000000100000000000012f0"),
        &hex!("a08090a0b0c0d0e0f00910203040506077000000000000000100000000000012f0"),
    ),
];

#[test]
fn round_trip() {
    for &(uint, expected_bytes) in U256_VECTORS {
        assert_eq!(rlp::encode(&uint), expected_bytes);
        assert_eq!(rlp::decode::<U256>(expected_bytes).unwrap(), uint);
    }
}
