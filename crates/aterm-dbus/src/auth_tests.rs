// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_first_line_carries_the_decimal_uid_in_hex() {
    assert_eq!(first(1000), b"\0AUTH EXTERNAL 31303030\r\n");
    assert_eq!(first(0), b"\0AUTH EXTERNAL 30\r\n");
}

#[test]
fn only_ok_with_a_guid_is_acceptance() {
    let guid = "8e4f0c2a1b7d6e5f8e4f0c2a1b7d6e5f";
    assert_eq!(answer(format!("OK {guid}").as_bytes()).unwrap(), guid);
    assert_eq!(
        answer(b"OK 8E4F0C2A1B7D6E5F8E4F0C2A1B7D6E5F").unwrap(),
        guid
    );
    for refusal in [
        &b"OK"[..],
        b"OK 8e4f",
        b"OK 8e4f0c2a1b7d6e5f8e4f0c2a1b7d6e5fzz",
        b"REJECTED DBUS_COOKIE_SHA1 ANONYMOUS",
        b"ERROR",
        b"DATA",
        b"AGREE_UNIX_FD",
        b"\xff",
        b"",
    ] {
        assert!(
            matches!(answer(refusal), Err(Error::Auth(_))),
            "{refusal:?}"
        );
    }
}
