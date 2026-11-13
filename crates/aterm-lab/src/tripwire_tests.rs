// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn logs_the_account_and_service_and_never_a_secret_value() {
    let a: Vec<String> = [
        "find-generic-password",
        "-a",
        "me",
        "-w",
        "-s",
        "Claude Code-credentials-1234abcd",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(
        parse_argv(&a),
        (
            "find-generic-password".into(),
            "me".into(),
            "Claude Code-credentials-1234abcd".into()
        )
    );
    let w: Vec<String> = [
        "add-generic-password",
        "-a",
        "me",
        "-s",
        "svc",
        "-X",
        "deadbeef",
    ]
    .map(String::from)
    .to_vec();
    let (c, ac, s) = parse_argv(&w);
    assert!(!format!("{c}{ac}{s}").contains("deadbeef"));
    assert_eq!(answer("find-generic-password"), 44);
    assert_eq!(answer("add-generic-password"), 0);
    assert_eq!(answer("dump-keychain"), 1);
}

#[test]
fn interactive_lines_name_their_last_account_and_service() {
    let l = r#"add-generic-password -U -a "x" -s "Claude Code" -a "me" -X "6869""#;
    assert_eq!(quoted_after(l, "-a"), "me");
    assert_eq!(quoted_after(l, "-s"), "Claude Code");
    assert_eq!(quoted_after("show-keychain-info", "-a"), "-");
}
