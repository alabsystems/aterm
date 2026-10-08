// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An OSC payload's semicolons survive past the param cap.
//!
//! The parser splits an OSC body on ';' into a bounded buffer and keeps the
//! tail past the cap. Consumers re-join (`set_title` joins `params[1..]`,
//! `handle_osc_8` joins `params[2..]`), so titles and URIs with any number of
//! literal semicolons survive; a truncated OSC 8 URI would still pass the gate
//! and open a different address on click.

use aterm_core::terminal::Terminal;

/// Titles are joined from `params[1..]`, so a title with many semicolons must
/// arrive whole. 15 is the old cliff edge; 20 and 40 are past it.
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

/// A hyperlink URI is joined from `params[2..]`; a truncated one would still
/// look valid, so nothing downstream would reject it.
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
