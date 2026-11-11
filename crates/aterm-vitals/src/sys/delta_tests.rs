// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::sys::query::View;

fn line(key: &str, text: &str) -> Line {
    Line {
        key: key.into(),
        text: text.into(),
        value: true,
    }
}

fn reply(lines: Vec<Line>) -> Reply {
    Reply {
        view: View::Overview,
        scope: None,
        subject: None,
        lines,
        json: None,
    }
}

#[test]
fn unchanged_lines_fold_into_one_same_line_and_changed_ones_stay() {
    let before = reply(vec![
        line("global", "global  3.1 of 10 cores busy"),
        line("aterm", "aterm   1.6 cores"),
        line("tab 1", "tab 1   claude; 0.9 cores"),
    ]);
    let base = fingerprints(&before);
    let mut now = reply(vec![
        line("global", "global  3.1 of 10 cores busy"),
        line("aterm", "aterm   2.6 cores"),
        line("tab 1", "tab 1   claude; 0.9 cores"),
        Line {
            key: "e".into(),
            text: "e seq=9 kind=tab".into(),
            value: false,
        },
    ]);
    let same = apply_delta(&mut now, &base);
    assert_eq!(same, ["global", "tab 1"]);
    let texts: Vec<&str> = now.lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(
        texts,
        ["same global,tab_1", "aterm   2.6 cores", "e seq=9 kind=tab"]
    );
}

#[test]
fn a_new_key_is_a_change_and_events_are_never_folded() {
    let base = fingerprints(&reply(vec![line("global", "x")]));
    let mut now = reply(vec![line("global", "x"), line("tab 2", "y")]);
    assert_eq!(apply_delta(&mut now, &base), ["global"]);
    assert_eq!(now.lines.len(), 2);
    assert_eq!(now.lines[1].key, "tab 2");
}

#[test]
fn fnv_is_aterm_hashs_fold() {
    for text in [
        "",
        "a",
        "foobar",
        "global 3.0 of 8 cores busy",
        "é \u{1F600}",
    ] {
        assert_eq!(fnv(text), aterm_hash::fnv1a_64(text.as_bytes()), "{text:?}");
    }
}
