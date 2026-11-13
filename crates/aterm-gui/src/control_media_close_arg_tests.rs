// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::close_no_arg;

#[test]
fn close_takes_no_argument_and_a_guess_never_retires_a_session() {
    assert_eq!(close_no_arg(""), Ok(()));
    assert_eq!(close_no_arg("   "), Ok(()));
    for bad in [
        "--dry-run",
        "2",
        "now",
        "force",
        "@s-0123456789abcdef0123",
        "trim",
    ] {
        assert_eq!(
            close_no_arg(bad),
            Err("ERR usage: close\n".to_string()),
            "`close {bad}` must be refused, not honoured"
        );
    }
}
