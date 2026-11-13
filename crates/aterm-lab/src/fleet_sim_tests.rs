// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_size_floor_is_four_machines() {
    let a = |v: &[&str]| parse(&v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
    assert!(a(&["--n", "3"]).is_err());
    assert_eq!(a(&["--n", "4"]).map(|a| a.n).ok(), Some(4));
    assert!(a(&["--record"]).is_ok_and(|a| a.record && a.n == 30));
}

#[test]
fn what_a_record_keeps_is_the_lines_between_its_markers() {
    let text = "x\n| at | head |\n<!-- invocations -->\n| a | 1 |\n| b | 2 |\n\
                <!-- /invocations -->\n| run | 1 |\n<!-- notes -->\n- found\n\n\
                <!-- /notes -->\n";
    assert_eq!(between(text, INVOCATIONS), ["| a | 1 |", "| b | 2 |"]);
    assert_eq!(between(text, NOTES), ["- found", ""]);
    assert!(between("no markers\n| a |\n", INVOCATIONS).is_empty());
    assert!(between("<!-- notes -->\n- a\n", NOTES).is_empty());
}

/// Verify that `tty::one_line` produces every expected substring in [`HOSTILE_ESCAPED`].
#[test]
fn the_hostile_words_escape_to_the_forms_the_check_looks_for() {
    let escaped = aterm_codec::tty::one_line(fixture::HOSTILE_SAID, 160);
    for form in HOSTILE_ESCAPED {
        assert!(escaped.contains(form), "{form} in {escaped}");
    }
    assert!(!escaped.contains("trust=human"), "{escaped}");
}
