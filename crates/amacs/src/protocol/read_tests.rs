// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::protocol::anchor::Anchor;

#[test]
fn each_line_prints_its_anchor_and_text() {
    let text = "fn main() {\r\n    run();\r\n}";
    let read = format_lines(text, 7, LineRange::ALL, DOC_READ_CAP);
    let expected = format!(
        "1:{}|fn main() {{\n2:{}|    run();\n3:{}|}}\n-- lines=3 seq=7\n",
        Hash6::of("fn main() {"),
        Hash6::of("    run();"),
        Hash6::of("}")
    );
    assert_eq!(read, expected);
    for row in read.lines().filter(|row| !row.starts_with("--")) {
        let (anchor, _) = row.split_once('|').unwrap();
        let anchor: Anchor = anchor.parse().unwrap();
        assert_eq!(Anchor::of_line(text, anchor.line), Some(anchor));
    }
    assert_eq!(
        format_lines("", 0, LineRange::ALL, 64),
        "-- lines=0 seq=0\n"
    );
    assert_eq!(
        format_lines("a\nb\nc\n", 1, "2".parse().unwrap(), 64),
        format!("2:{}|b\n-- lines=3 seq=1\n", Hash6::of("b"))
    );
    assert_eq!(
        format_lines("a\n", 1, "5-9".parse().unwrap(), 64),
        "-- lines=1 seq=1\n"
    );
}

#[test]
fn cap_and_next_cursor() {
    let text = (1..=10)
        .map(|n| format!("line {n:02}\n"))
        .collect::<String>();
    let row = format_lines(&text, 3, "1".parse().unwrap(), 1024);
    let row_len = row.lines().next().unwrap().len() + 1;
    assert_eq!(row_len, 17);
    let read = format_lines(&text, 3, LineRange::ALL, row_len * 4 + 5);
    assert_eq!(read.lines().count(), 5, "four lines and the trailer");
    assert!(read.ends_with("-- lines=10 seq=3 next=5\n"), "{read}");
    let rest = format_lines(&text, 3, "5-".parse().unwrap(), 1024);
    assert!(rest.starts_with("5:"), "{rest}");
    assert!(rest.ends_with("-- lines=10 seq=3\n"), "{rest}");
    let bounded = format_lines(&text, 3, "1-2".parse().unwrap(), 1024);
    assert!(bounded.ends_with("-- lines=10 seq=3\n"), "{bounded}");
    let wide = format!("{}\nshort\n", "é".repeat(40));
    let read = format_lines(&wide, 0, LineRange::ALL, 30);
    let first = read.lines().next().unwrap();
    assert!(first.len() < 30, "{first}");
    assert!(first.starts_with(&format!("1:{}|é", Hash6::of(&"é".repeat(40)))));
    assert!(read.ends_with("-- lines=2 seq=0 next=2 cut=1\n"), "{read}");
    let read = format_lines(&wide, 0, "1-1".parse().unwrap(), 30);
    assert!(read.ends_with("-- lines=2 seq=0 cut=1\n"), "{read}");
}

#[test]
fn line_ranges_parse_what_they_print() {
    for (text, range) in [
        (
            "3-9",
            LineRange {
                first: 3,
                last: Some(9),
            },
        ),
        (
            "4-",
            LineRange {
                first: 4,
                last: None,
            },
        ),
        (
            "2",
            LineRange {
                first: 2,
                last: Some(2),
            },
        ),
    ] {
        assert_eq!(text.parse::<LineRange>(), Ok(range));
        assert_eq!(range.to_string(), text);
    }
    for bad in ["", "0", "0-3", "5-3", "-3", "a-b", "1-2-3", "+1"] {
        assert!(bad.parse::<LineRange>().is_err(), "{bad}");
    }
}
