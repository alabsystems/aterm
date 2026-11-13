// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn the_relay_takes_an_optional_log_then_dashes_then_a_command() {
    let a = v(&["--log", "/r/relay.log", "--", "env", "A=1", "claude"]);
    let (log, cmd) = parse_args(&a).unwrap();
    assert_eq!(log.as_deref(), Some("/r/relay.log"));
    assert_eq!(cmd, &v(&["env", "A=1", "claude"])[..]);
    let a = v(&["--", "true"]);
    assert_eq!(parse_args(&a).map(|(l, c)| (l, c.len())), Some((None, 1)));
    assert!(parse_args(&v(&["ctty", "--", "true"])).is_none());
    assert!(parse_args(&v(&["--log", "x", "--"])).is_none());
    assert!(parse_args(&v(&["echo", "hi"])).is_none());
}

/// The recorder's grammar, and its line: the shape aterm-effects'
/// `recording` parses (`<stamp> <O|I> <hex>`, lower-case, two digits a byte).
#[test]
fn a_take_names_its_log_then_dashes_then_a_command_and_writes_ptyrecs_lines() {
    let a = v(&["/r/take.log", "--", "claude", "--resume"]);
    let (take, cmd) = parse_ptyrec(&a).unwrap();
    assert_eq!(take, "/r/take.log");
    assert_eq!(cmd, &v(&["claude", "--resume"])[..]);
    assert!(parse_ptyrec(&v(&["/r/take.log", "--"])).is_none());
    assert!(parse_ptyrec(&v(&["/r/take.log", "claude"])).is_none());
    assert!(parse_ptyrec(&v(&["--", "claude"])).is_none());
    assert_eq!(
        take_line(17, Way::Out, b"\x1b[0mA\n"),
        "17 O 1b5b306d410a\n"
    );
    assert_eq!(take_line(18, Way::In, b"\r"), "18 I 0d\n");
    assert_eq!(take_line(19, Way::In, b""), "19 I \n");
}
