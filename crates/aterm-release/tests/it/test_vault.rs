// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Integration-only generated ring fixture above the shared vault primitives.

use std::path::Path;

#[path = "../../src/test_vault_tests.rs"]
mod shared;
pub(crate) use shared::*;

/// The store's release ring under `home` as its three entries (key, record, profile), all for one
/// fresh key, or the record naming `record_key` where a test wants the three apart: the profile's
/// base64 PKCS#8 and the public key.
pub(crate) fn seed_ring(home: &Path, record_key: Option<&str>) -> (String, String) {
    use ring::signature::KeyPair as _;
    let pkcs8 =
        ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
    let pair = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let pubkey = aterm_codec::base64::encode(pair.public_key().as_ref()).unwrap();
    seed_ring_as(home, "m22", pkcs8.as_ref(), record_key.unwrap_or(&pubkey));
    (aterm_codec::base64::encode(pkcs8.as_ref()).unwrap(), pubkey)
}
