// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use super::*;
use std::os::unix::ffi::OsStrExt as _;

#[test]
fn git_quoted_object_paths_are_exact_and_cannot_inject_inventory_rows() {
    assert_eq!(
        directory(b"/plain path/objects").unwrap(),
        Path::new("/plain path/objects")
    );
    assert_eq!(
        directory(br#""/tab\tline\nbell\aquote\"slash\\octal\377""#)
            .unwrap()
            .as_os_str()
            .as_bytes(),
        b"/tab\tline\nbell\x07quote\"slash\\octal\xff"
    );
    for raw in [
        b"relative".as_slice(),
        br#""/unfinished\""#,
        br#""/bad\400""#,
        br#""/bad\12""#,
        br#""/bad\q""#,
        br#""/bad"tail""#,
        br#""/nul\000""#,
        b"/line\nalternate: /other",
        br#""/missing-end"#,
    ] {
        assert!(directory(raw).is_err(), "{raw:?}");
    }
}
