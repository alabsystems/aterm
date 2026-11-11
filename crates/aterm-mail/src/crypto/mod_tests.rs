// SPDX-License-Identifier: Apache-2.0 AND MIT
// Copyright 2026 Andrew Yates

#[test]
fn proton_mailbox_key_password_vector() {
    let salt = aterm_codec::base64::decode("imK9IHsRcA2Zsv+yROZgbw==").unwrap();
    let password = super::mailbox_password("password", &salt).unwrap();
    assert_eq!(password.expose(), "Q.Gd9rSsqE0xQ8Qcf0Q9ckInb4hIzOu");
}
