// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::caption;

/// An elevated process badges every caption once; an unelevated one
/// passes the title through untouched.
#[test]
fn an_elevated_caption_says_administrator_once() {
    assert_eq!(caption("~\\aterm", false), "~\\aterm");
    assert_eq!(caption("~\\aterm", true), "Administrator: ~\\aterm");
    assert_eq!(caption("aterm", true), "Administrator: aterm");
    assert_eq!(
        caption("Administrator: C:\\Windows\\system32\\cmd.exe", true),
        "Administrator: C:\\Windows\\system32\\cmd.exe"
    );
    assert_eq!(caption("", true), "Administrator: ");
}
