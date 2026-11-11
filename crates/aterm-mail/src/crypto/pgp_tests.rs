// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn decrypts_proton_published_message_and_rejects_damage() {
    let keys = unlock(include_str!("../../tests/fixtures/proton-key-v4.asc"), b"").unwrap();
    let message = include_str!("../../tests/fixtures/proton-message-v4.asc");
    assert_eq!(&*decrypt(message, &keys).unwrap(), b"hello world");
    let mut bytes = armor(message, "MESSAGE").unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let encoded = aterm_codec::base64::encode(&bytes).unwrap();
    let changed = format!("-----BEGIN PGP MESSAGE-----\n\n{encoded}\n-----END PGP MESSAGE-----");
    assert!(decrypt(&changed, &keys).is_err());
}
