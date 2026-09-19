// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AN OSC PAYLOAD'S SEMICOLONS SURVIVE PAST THE PARAM CAP.
//!
//! The parser splits an OSC body on ';' into a bounded buffer. It used to DROP
//! every segment past the cap, which is the defect #7268 "fixed" by raising the
//! cap from 8 to 16 — a payload that carries semicolons has no bound to raise it
//! to, so raising it only moved the cliff.
//!
//! It matters because the consumers re-JOIN: `set_title` joins `params[1..]` and
//! `handle_osc_8` joins `params[2..]`, added deliberately so titles and URIs with
//! literal semicolons survive. Past 15 semicolons the tail was gone — and the
//! truncated OSC 8 URI still passed the scheme/length/control gate, was stored on
//! the cells, and was what a Cmd-click opened. That is the serious half: a click
//! went to a URL the emitter never wrote.

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

/// The one that opens something. A hyperlink URI is joined from `params[2..]`;
/// a truncated one is still a VALID-looking URI, so nothing downstream rejects
/// it — it is simply a different address.
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
