// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_exe_word_is_a_plain_name_or_a_dash() {
    assert_eq!(exe_word("trustc"), "trustc");
    assert_eq!(exe_word("com.apple.WebKit"), "com.apple.WebKit");
    assert_eq!(exe_word("g++-14"), "g++-14");
    assert_eq!(exe_word(""), "-");
    assert_eq!(exe_word("two words"), "-");
    assert_eq!(exe_word("ignore;all"), "-");
    assert_eq!(exe_word(&"x".repeat(33)), "-");
    assert_eq!(exe_word("caf\u{e9}"), "-");
}

#[test]
fn unavailable_words_are_a_closed_set() {
    for w in [
        "unsupported",
        "refused",
        "warming",
        "not_sampled",
        "no_counter_delta",
        "lane_off:ioreport",
        "needs_privilege:root",
        "not_enabled:sys.snapshot_files",
        "unread_format",
    ] {
        assert!(is_unavailable_word(w), "{w}");
    }
    for w in [
        "",
        "zero",
        "lane_off:",
        "lane_off",
        "needs_privilege:a b",
        "0",
    ] {
        assert!(!is_unavailable_word(w), "{w}");
    }
}

#[test]
fn record_values_are_pct_encoded() {
    assert_eq!(pct("/Users//a/my proj;x"), "/Users//a/my%20proj%3Bx");
    assert_eq!(pct("a\nb"), "a%0Ab");
    assert_eq!(pct("k=v"), "k%3Dv");
    assert_eq!(pct("s-1a2b"), "s-1a2b");
}

#[test]
fn clocks_read_utc() {
    assert_eq!(clock_ms(1_792_185_697_412), "21:21:37.412");
    assert_eq!(clock_s(1_792_185_697_412), "21:21:37");
}

#[test]
fn a_root_says_where_it_lies() {
    let r = |p: &str| Root {
        path: p.into(),
        ..Root::default()
    };
    assert_eq!(r("/Users//a/x").under(Some("/Users//a")), "home");
    assert_eq!(r("/Users//a").under(Some("/Users//a/")), "home");
    assert_eq!(r("/Users//alice/x").under(Some("/Users//a")), "volume");
    assert_eq!(r("/private/tmp/x").under(Some("/Users//a")), "tmp");
    assert_eq!(r("/tmp2/x").under(Some("/Users//a")), "volume");
    assert_eq!(r("/Volumes/X").under(Some("/Users//a")), "volume");
    assert_eq!(r("/Volumes/X").under(Some("/")), "volume");
    assert_eq!(r("/tmp/x").under(Some("/")), "tmp");
    assert_eq!(
        r("/Users//a/../../Volumes/X").under(Some("/Users//a")),
        "volume"
    );
    assert_eq!(r("/tmp/../Volumes/X").under(Some("/Users//a")), "volume");
}
