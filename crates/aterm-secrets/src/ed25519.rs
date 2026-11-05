// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crate's one Ed25519 verification: astream's strict check
//! ([`astream_aead::signature::verify`]). ring alone takes a key of small order, the identity
//! among them, whose one signature verifies every message; the strict check refuses such a key
//! and such a commitment, so a signature binds its key to what it signed.

/// Whether `signature` is `key`'s Ed25519 signature of `message`, strictly. A key that is not 32
/// bytes or a signature that is not 64 verifies nothing, as under ring.
pub(crate) fn verifies(key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    match (<&[u8; 32]>::try_from(key), <&[u8; 64]>::try_from(signature)) {
        (Ok(key), Ok(signature)) => astream_aead::signature::verify(key, message, signature),
        _ => false,
    }
}

/// The identity point `01 00..00` as a key: a key of small order.
#[cfg(test)]
pub(crate) const SMALL_ORDER_KEY: [u8; 32] = {
    let mut key = [0; 32];
    key[0] = 1;
    key
};

/// The identity as commitment and `s = 0`: under ring alone, [`SMALL_ORDER_KEY`]'s signature of
/// every message (astream-aead's `a_small_order_key_signs_nothing`).
#[cfg(test)]
pub(crate) const FORGED: [u8; 64] = {
    let mut signature = [0; 64];
    signature[0] = 1;
    signature
};

/// The zero key `00..00`: the point `(√-1, 0)`, of order 4, which the record's shape and the
/// Node proof check once refused by name.
#[cfg(test)]
pub(crate) const ZERO_KEY: [u8; 32] = [0; 32];

/// The four points of [`ZERO_KEY`]'s subgroup, encoded: the identity, the key itself, its
/// negation and its double `(0, -1)`. With `s = 0`, ring alone takes a signature by the zero key
/// whose commitment is `-[k]A`, so one of these four signs a message when `k mod 4` matches it,
/// about one message in four for each.
#[cfg(test)]
const ZERO_KEY_SUBGROUP: [[u8; 32]; 4] = {
    let identity = SMALL_ORDER_KEY;
    let negated = {
        let mut point = [0; 32];
        point[31] = 0x80;
        point
    };
    let doubled = {
        let mut point = [0xff; 32];
        point[0] = 0xec;
        point[31] = 0x7f;
        point
    };
    [identity, ZERO_KEY, negated, doubled]
};

/// Whether ring's lax check alone takes `signature` as `key`'s of `message`: a test's proof that
/// the forgery the strict check refuses is one the replaced verifier took.
#[cfg(test)]
pub(crate) fn ring_takes(key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    #[expect(
        clippy::disallowed_methods,
        reason = "a test's non-vacuity control: the lax check the strict one replaced"
    )]
    let key = ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key);
    key.verify(message, signature).is_ok()
}

/// A signature by [`ZERO_KEY`] that ring alone takes over `message` (a commitment in the key's
/// subgroup and `s = 0`), if one of the four is.
#[cfg(test)]
pub(crate) fn zero_key_forgery(message: &[u8]) -> Option<[u8; 64]> {
    ZERO_KEY_SUBGROUP
        .iter()
        .map(|commitment| {
            let mut signature = [0; 64];
            signature[..32].copy_from_slice(commitment);
            signature
        })
        .find(|signature| ring_takes(&ZERO_KEY, message, signature))
}
