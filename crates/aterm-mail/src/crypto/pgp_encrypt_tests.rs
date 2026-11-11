// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn generated_message_roundtrips_through_authenticated_decryptor() {
    let keys =
        super::super::unlock(include_str!("../../tests/fixtures/proton-key-v4.asc"), b"").unwrap();
    let content = EncryptedBody::new(b"Hello from aterm").unwrap();
    let armored = content.armored(&keys[0].recipient()).unwrap();
    assert_eq!(
        &*super::super::decrypt(&armored, &keys).unwrap(),
        b"Hello from aterm"
    );
}
