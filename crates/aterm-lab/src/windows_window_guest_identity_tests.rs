// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::guest_identity;

#[test]
fn cleanup_identity_cannot_be_a_pid_or_missing_start() {
    let text = r#"{"pid":42,"birth":"123456","image":"C:\\lane\\target\\debug\\aterm-gui.exe"}"#;
    assert!(guest_identity(text, 42).is_some());
    assert!(guest_identity(text, 43).is_none());
    for text in [
        r#"{"pid":42}"#,
        r#"{"pid":42,"birth":"0","image":"C:\\aterm-gui.exe"}"#,
        r#"{"pid":42,"birth":"unknown","image":"C:\\aterm-gui.exe"}"#,
        r#"{"pid":42,"birth":"123456","image":"C:\\other.exe"}"#,
    ] {
        assert!(guest_identity(text, 42).is_none());
    }
}
