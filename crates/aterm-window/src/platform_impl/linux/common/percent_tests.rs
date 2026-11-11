// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use super::*;

#[test]
fn percent_decode_basic_and_incomplete() {
    assert_eq!(percent_decode_utf8(b"a%20b").as_deref(), Ok("a b"));
    assert_eq!(percent_decode_utf8(b"x%2F").as_deref(), Ok("x/"));
    assert_eq!(percent_decode_utf8(b"100%").as_deref(), Ok("100%"));
    assert_eq!(percent_decode_utf8(b"a%zzb").as_deref(), Ok("a%zzb"));
    assert!(percent_decode_utf8(b"%FF").is_err());
    assert!(percent_decode_utf8(b"\xFF").is_err());

    let entries: Vec<&[u8]> =
        uri_list_entries(b"#comment\r\nfile:///tmp/a%0D%0Afile:///tmp/b%20\r\nfile:///tmp/%23c\n")
            .collect();
    assert_eq!(entries, [b"file:///tmp/a%0D%0Afile:///tmp/b%20".as_slice(), b"file:///tmp/%23c"]);
    assert_eq!(percent_decode_utf8(entries[0]).as_deref(), Ok("file:///tmp/a\r\nfile:///tmp/b "));
}
