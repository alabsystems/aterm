// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn words(ws: &[&str]) -> Vec<String> {
    ws.iter().map(|w| (*w).to_string()).collect()
}

#[test]
fn the_line_splits_and_only_a_plain_argv_is_spelled_by_it() {
    assert_eq!(
        split_line("  /a b/aterm link\tserve ; rm\n"),
        words(&["/a", "b/aterm", "link", "serve", ";", "rm"])
    );
    assert!(split_line(" \t ").is_empty(), "a blank line names nothing");
    let plain = words(&[
        "/usr/bin/aterm",
        "link",
        "serve",
        "--state",
        "/Users//u/Caf\u{e9}",
    ]);
    assert!(line_spells(&plain));
    assert_eq!(split_line(&plain.join(" ")), plain);
    for spaced in [
        words(&[
            "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm",
            "link",
        ]),
        words(&["aterm", "tab\there"]),
        words(&["aterm", "nb\u{a0}sp"]),
        words(&["aterm", ""]),
    ] {
        assert!(!line_spells(&spaced), "{spaced:?}");
        assert_ne!(split_line(&spaced.join(" ")), spaced, "{spaced:?}");
    }
    assert!(!line_spells(&[]), "no words is no command");
}

#[test]
fn an_empty_or_nul_element_is_refused_by_name() {
    for ok in ["aterm", "/a b/aterm", "it's", "\"q\"", "Caf\u{e9}", " "] {
        assert_eq!(check_word(0, ok), Ok(()), "{ok:?}");
    }
    let empty = check_word(2, "").expect_err("an empty word");
    assert!(
        empty.contains("element 3") && empty.contains("empty"),
        "{empty}"
    );
    let nul = check_word(0, "a\0b").expect_err("a NUL");
    assert!(nul.contains("element 1") && nul.contains("NUL"), "{nul}");
}
