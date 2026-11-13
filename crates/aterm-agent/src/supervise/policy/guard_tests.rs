// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_phase::prompt::fixtures;

/// The shipped matcher: what the server compiles a `key if=<re>` with.
fn matcher(pattern: &str) -> impl Fn(&str) -> bool + use<> {
    let m = aterm_observe::row_matcher(pattern).expect("the server compiles the guard");
    move |row| m.matches(row)
}

/// The guard compiles on the server's engine, is one wire token, and
/// matches the judged row and no other row of the recorded box.
#[test]
fn a_row_guard_matches_its_row_and_nothing_else_on_the_box() {
    let rows = fixtures::bash_one_row();
    let cmd_row = rows
        .iter()
        .position(|r| r == "   git log --oneline -5")
        .expect("the command row");
    let g = row_guard(&rows[cmd_row]);
    assert_eq!(g, "^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$");
    assert!(!g.contains(char::is_whitespace));
    let m = matcher(&g);
    for (k, row) in rows.iter().enumerate() {
        assert_eq!(m(row), k == cmd_row, "row {k}: {row:?}");
    }
    assert!(m("   git log --oneline -5      "));
}

/// Negative controls: the same command in a transcript copy, a longer
/// command that starts the same, another indentation, a swapped box.
#[test]
fn a_copy_or_a_swapped_box_does_not_satisfy_the_guard() {
    let g = row_guard("   touch x");
    let m = matcher(&g);
    assert!(m("   touch x"));
    assert!(!m("  ⎿  $ touch x"));
    assert!(!m("   touch x; rm -rf y"));
    assert!(!m("    touch x"));
    assert!(!m("   touch y"));
    assert!(!m(" Do you want to proceed?"));
}

/// Every ASCII punctuation mark, a `│` bar and a non-ASCII space are
/// literals, never regex syntax.
#[test]
fn punctuation_and_bars_are_literal() {
    let row = "   │ S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1 [x] (y) {z} ^a|b.c*+?\\ '\"#&~`<>=,!@%";
    let g = row_guard(row);
    assert!(!g.contains(char::is_whitespace), "{g}");
    let m = matcher(&g);
    assert!(m(row));
    assert!(!m(&row.replace("$S", "$T")));
    let nbsp = "   a\u{a0}b";
    let g = row_guard(nbsp);
    let m = matcher(&g);
    assert!(m(nbsp));
    assert!(!m("   a b"));
}

/// Claude Code's empty composer is drawn as the caret and a no-break space,
/// which `text` reads trimmed to `❯`.
#[test]
fn the_guard_of_a_trimmed_read_matches_the_row_the_server_tests() {
    let g = row_guard("❯");
    assert_eq!(g, "^❯\\s*$");
    let m = matcher(&g);
    assert!(m("❯\u{a0}"));
    assert!(m("❯\u{a0}   "));
    assert!(m("❯"));
    assert!(!m("❯\u{a0}keep going"));
    assert!(!m("  ❯ 1. Yes"));
}

#[test]
fn a_long_row_is_guarded_by_an_anchored_prefix_that_fits() {
    let row = format!("   {}", "a.b ".repeat(400));
    let g = row_guard(&row);
    assert!(g.len() <= MAX_GUARD_BYTES, "{}", g.len());
    assert!(!g.ends_with('$'));
    let m = matcher(&g);
    assert!(m(&row));
    assert!(!m(&format!(" x{row}")));
}

#[test]
fn the_gen_fence_is_added_only_where_the_server_says_it_can() {
    assert_eq!(key_args("^x$", "1", None), "if=^x$ 1");
    assert_eq!(key_args("^x$", "1", Some("2.15")), "if-gen=2.15 if=^x$ 1");
    assert!(!server_fences_gen(
        "key [id=<key>] [if=<re>] <name>: send a named key"
    ));
    let help_key = aterm_types::control_verbs::spec("key")
        .expect("the key verb")
        .entry_lines()
        .join("\n");
    assert!(!help_key.lines().next().unwrap().contains("if-gen="));
    assert!(server_fences_gen(&help_key), "{help_key}");
    let shipped = aterm_types::control_verbs::catalog_lines_full()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(server_fences_gen(&shipped));
    assert!(!server_fences_gen(
        "status: …\nkey [id=<key>] [if-seq=<n>] [if=<re>] <name>: send a named key"
    ));
    assert!(!server_fences_gen(
        "status … gen=<e.s> … (key if-gen=)\nkey [if=<re>] <name>\n  a named key"
    ));
    assert!(!server_fences_gen(
        "key [if=<re>] <name>\n  a named key\nsend [if-gen=<g>] <text>\n  gen fenced"
    ));
}

/// `act=` is read off the `help key`
/// reply the fence probe already asks for: the shipped `key` entry and
/// catalog name it; an older entry, or `act=` under another verb, does
/// not.
#[test]
fn the_act_word_is_sent_only_where_help_key_names_it() {
    let help_key = aterm_types::control_verbs::spec("key")
        .expect("the key verb")
        .entry_lines()
        .join("\n");
    assert!(server_takes_act(&help_key), "{help_key}");
    let shipped = aterm_types::control_verbs::catalog_lines_full()
        .collect::<Vec<_>>()
        .join("\n");
    assert!(server_takes_act(&shipped));
    assert!(!server_takes_act(
        "key [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] <name>: send a named key"
    ));
    assert!(!server_takes_act(
        "key [if=<re>] <name>\n  a named key\nsend [act=approve] <text>\n  acted"
    ));
    assert_eq!(
        [KeyAct::Approve, KeyAct::Deny, KeyAct::Answer].map(KeyAct::option),
        ["act=approve", "act=deny", "act=answer"]
    );
}
