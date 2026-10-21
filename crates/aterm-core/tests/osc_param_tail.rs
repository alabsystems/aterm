// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An OSC payload's semicolons survive past the parser's param cap (`MAX_OSC_PARAMS`): the
//! last slot keeps the tail and consumers re-join it, so a title or OSC 8 URI arrives whole.

use aterm_core::terminal::Terminal;

/// A title is joined from `params[1..]`; 15 and 16 fields straddle the 16-slot cap.
#[test]
fn a_title_keeps_every_semicolon_separated_field() {
    for fields in [4usize, 15, 16, 20, 40] {
        let mut term = Terminal::new(24, 80);
        let title: Vec<String> = (0..fields).map(|i| format!("f{i}")).collect();
        let title = title.join(";");
        term.process(format!("\x1b]0;{title}\x07").as_bytes());
        assert_eq!(
            term.title(),
            title,
            "a {fields}-field title must arrive whole, not truncated at the param cap"
        );
    }
}

/// A hyperlink URI is joined from `params[2..]`; a truncated one still looks valid, so
/// nothing downstream would reject it. 13 and 14 parameters straddle the cap.
#[test]
fn a_hyperlink_uri_keeps_every_semicolon_and_is_never_silently_shortened() {
    for params in [2usize, 13, 14, 20, 40] {
        let mut term = Terminal::new(24, 80);
        let query: Vec<String> = (0..params).map(|i| format!("p{i}=1")).collect();
        let uri = format!("https://example.com/a;{}", query.join(";"));
        term.process(format!("\x1b]8;;{uri}\x1b\\X\x1b]8;;\x1b\\").as_bytes());
        assert_eq!(
            term.hyperlink_at(0, 0),
            Some(uri.as_str()),
            "a {params}-parameter URI must be stored exactly as emitted — a \
             truncated one still looks valid and is what a click would open"
        );
    }
}
