// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Operations xterm reaches under a second final byte.

use aterm_conformance::Screen;

fn scrolled(input: &[u8]) -> Vec<String> {
    let mut s = Screen::new(4, 6);
    s.feed(b"A\r\nB\r\nC\r\nD");
    s.feed(input);
    (0..4).map(|r| s.row(r)).collect()
}

#[test]
fn sd_spelled_with_caret_scrolls_down_like_sd_spelled_with_t() {
    assert_eq!(
        scrolled(b"\x1b[2^"),
        scrolled(b"\x1b[2T"),
        "CSI Ps ^ and CSI Ps T are one operation, so one screen"
    );
    assert_eq!(
        scrolled(b"\x1b[2^"),
        vec!["", "", "A", "B"],
        "two lines of blank scrolled in at the top, D pushed off the bottom"
    );
}

#[test]
fn sd_spelled_with_caret_defaults_to_one_line() {
    assert_eq!(scrolled(b"\x1b[^"), vec!["", "A", "B", "C"]);
    assert_eq!(
        scrolled(b"\x1b[0^"),
        scrolled(b"\x1b[^"),
        "Ps = 0 takes the default of 1, as it does for CSI Ps T"
    );
}

#[test]
fn sd_spelled_with_caret_scrolls_only_inside_the_scroll_region() {
    let mut s = Screen::new(5, 6);
    s.feed(b"1\r\n2\r\n3\r\n4\r\n5");
    s.feed(b"\x1b[2;4r\x1b[1^");
    let rows: Vec<String> = (0..5).map(|r| s.row(r)).collect();
    assert_eq!(
        rows,
        vec!["1", "", "2", "3", "5"],
        "rows 2-4 scroll down; rows 1 and 5 are outside the region"
    );
}

fn style_after(input: &[u8]) -> (u32, u32, u16) {
    let mut s = Screen::new(2, 20);
    s.feed(input);
    s.style_fingerprint()
}

#[test]
fn xtpushsgr_spelled_hash_p_restores_the_same_style_as_hash_brace() {
    let aliased = style_after(b"\x1b[1m\x1b[#p\x1b[0m\x1b[#q");
    let primary = style_after(b"\x1b[1m\x1b[#{\x1b[0m\x1b[#}");
    assert_eq!(aliased, primary, "the alias is the same operation");
    assert_ne!(
        aliased,
        style_after(b"\x1b[1m\x1b[0m"),
        "precondition: the pop really restored something"
    );
}

#[test]
fn xtpushsgr_spelled_hash_p_takes_the_same_pm_selection_as_hash_brace() {
    let aliased = style_after(b"\x1b[1;4m\x1b[1#p\x1b[0m\x1b[#q");
    let primary = style_after(b"\x1b[1;4m\x1b[1#{\x1b[0m\x1b[#}");
    assert_eq!(aliased, primary, "Pm is read identically under both finals");
    assert_ne!(
        aliased,
        style_after(b"\x1b[1;4m\x1b[#p\x1b[0m\x1b[#q"),
        "Pm = 1 restores bold alone; the bare push restores bold and underline"
    );
}

#[test]
fn xtpopsgr_spelled_hash_q_pops_a_stack_pushed_with_hash_brace() {
    let mixed = style_after(b"\x1b[1m\x1b[#{\x1b[0m\x1b[#q");
    let primary = style_after(b"\x1b[1m\x1b[#{\x1b[0m\x1b[#}");
    assert_eq!(mixed, primary, "one stack, reachable under either spelling");
}

#[test]
fn unbalanced_alias_pops_are_ignored_without_disturbing_the_style() {
    let over_popped = style_after(b"\x1b[1m\x1b[#q\x1b[#q\x1b[#q");
    assert_eq!(
        over_popped,
        style_after(b"\x1b[1m"),
        "pops with an empty stack leave the current style alone"
    );
}
