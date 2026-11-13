// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A look says a count only where the record and the text line agree, the line naming none
/// for `0`.
#[test]
fn a_look_says_a_count_only_where_the_record_and_the_line_agree() {
    let at = |bg: &str, line: &str| Look {
        bg: bg.to_string(),
        line: line.to_string(),
    };
    let sid = "s-0123456789abcdef0123";
    assert!(at("2", &format!("{sid} codex unknown 3s task=- bg=2")).says("2"));
    assert!(!at("2", &format!("{sid} codex unknown 3s")).says("2"));
    assert!(!at("-", &format!("{sid} codex unknown 3s bg=2")).says("2"));
    assert!(!at("2", &format!("{sid} codex unknown 3s bg=20")).says("2"));
    assert!(at("0", &format!("{sid} codex unknown 3s")).says("0"));
    assert!(!at("0", &format!("{sid} codex unknown 3s bg=0")).says("0"));
    assert!(!at("-", &format!("{sid} codex unknown 3s")).says("0"));
}
