// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn h6_matches_fnv1a_64_prefix_and_is_stable() {
    for line in ["", "fn main() {", "  let x = 1;", "é漢🦀", "tab\there"] {
        let hash = aterm_hash::fnv1a_64(line.as_bytes());
        let hex16 = aterm_hash::fnv1a_64_to_hex16(hash);
        let prefix = std::str::from_utf8(&hex16[..6]).unwrap();
        assert_eq!(Hash6::of(line).to_string(), prefix, "{line:?}");
        assert_eq!(u64::from(Hash6::of(line).value()), hash >> 40);
    }
    assert_eq!(Hash6::of("").to_string(), "cbf29c");
    assert_eq!(Hash6::of("a").to_string(), "af63dc");
    assert_eq!(Hash6::of("hello").to_string(), "a430d8");

    let lf = Lines::of("one\ntwo\n");
    let crlf = Lines::of("one\r\ntwo\r\n");
    let bom = Lines::of("\u{feff}one\ntwo");
    for line in 1..=2 {
        assert_eq!(lf.h6(line), crlf.h6(line));
        assert_eq!(lf.h6(line), bom.h6(line));
    }
    assert_eq!(lf.h6(1), Some(Hash6::of("one")));
    assert_eq!(lf.h6(3), None);
    assert_eq!(crlf.eol(), "\r\n");
    assert_eq!(lf.eol(), "\n");
    assert_eq!(bom.origin(), 3);
    assert!(bom.open_ended());
    assert!(!lf.open_ended());
}

#[test]
fn anchors_round_trip_and_refuse_what_a_read_never_prints() {
    let anchor: Anchor = "12:a3f9c1".parse().unwrap();
    assert_eq!(anchor.line, 12);
    assert_eq!(anchor.h6.value(), 0x00a3_f9c1);
    assert_eq!(anchor.to_string(), "12:a3f9c1");
    assert_eq!("12:A3F9C1".parse::<Anchor>().unwrap(), anchor);
    assert_eq!(
        Anchor::of_line("x\nhello\n", 2).unwrap().to_string(),
        "2:a430d8"
    );
    assert_eq!(Anchor::of_line("x\n", 2), None);
    for bad in [
        "12",
        "0:a3f9c1",
        "-1:a3f9c1",
        "+1:a3f9c1",
        ":a3f9c1",
        "12:a3f9c",
        "12:a3f9c10",
        "12:a3f9cg",
        "x:a3f9c1",
        "12:a3f9c1|text",
    ] {
        assert!(bad.parse::<Anchor>().is_err(), "{bad}");
    }
    assert_eq!(Hash6::from_value(0x0100_0000), None);
    assert_eq!(Hash6::from_value(0xff_ffff).unwrap().to_string(), "ffffff");
}

#[test]
fn lines_split_at_newline_and_keep_a_final_open_line() {
    let empty = Lines::of("");
    assert_eq!(empty.count(), 0);
    let one = Lines::of("\n");
    assert_eq!(one.count(), 1);
    assert_eq!(one.content(1), Some(""));
    let text = "a\r\nb\nc";
    let lines = Lines::of(text);
    assert_eq!(lines.count(), 3);
    assert_eq!(lines.content(1), Some("a"));
    assert_eq!(lines.content(2), Some("b"));
    assert_eq!(lines.content(3), Some("c"));
    assert_eq!(lines.line_at(0), Some(1));
    assert_eq!(lines.line_at(2), Some(1), "the LF of a CRLF is line 1's");
    assert_eq!(lines.line_at(3), Some(2));
    assert_eq!(lines.line_at(text.len() - 1), Some(3));
    assert_eq!(lines.line_at(text.len()), None);
    let cr = Lines::of("a\rb\n");
    assert_eq!(cr.count(), 1);
    assert_eq!(cr.content(1), Some("a\rb"));
    assert_eq!(Lines::of("\u{feff}").count(), 0);
    assert_eq!(Lines::of("\u{feff}x").line_at(0), None);
}
