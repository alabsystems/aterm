// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The transcript grid, enforced.
//!
//! `targo --unverified ship provision` is read by exactly one audience — an operator at a terminal,
//! mildly stressed, doing this once every few months — and the layout is the only thing
//! that decides which of a hundred lines they actually read. Every assertion here pins a
//! rule that was broken in a REAL run and cost something:
//!
//! * a label of exactly the gutter width printed `seed source10 program(s) staged at …`,
//!   because `{:<11}` pads only labels SHORTER than 11. Two facts, glued.
//! * a five-sentence errand paragraph was skimmed, the operator downloaded a
//!   pre-existing certificate from the portal's list, and one of team A66A9P66Z7's five
//!   PERMANENT Developer ID slots was spent to fix what was never a download problem.
//! * `step("signing", "")` rendered as the word `signing` followed by nothing, twice,
//!   bracketing the loudest warning the tool can print.
//!
//! A hand-checked string is checked once. These are checked on every commit.

use crate::{apple, publish};

use publish::{LABEL_MAX, VALUE_COL, grid_block};
use std::path::Path;

/// The constant labels reached through a `const` rather than a literal.
#[test]
fn the_named_labels_fit_the_gutter_too() {
    assert!(
        apple::APPLE_LABEL.chars().count() <= LABEL_MAX,
        "{:?}",
        apple::APPLE_LABEL
    );
}

/// A label at the limit still gets its separator. This is the exact shape of the bug:
/// the old primitive was `format!("  {label:<11}{msg}")`, which is correct for every
/// label but the one that fills the field.
#[test]
fn a_label_at_the_limit_never_touches_its_value() {
    let long = "x".repeat(LABEL_MAX);
    let line = grid_block(&long, "10 program(s) staged");
    assert!(line.starts_with(&format!("  {long} ")), "{line:?}");
    assert_eq!(
        &line[..VALUE_COL],
        format!("  {long} "),
        "the value must start at {VALUE_COL}"
    );
    // And every shorter label lands in the same column, so the values line up.
    for label in ["seed", "apple id", "channel", ""] {
        let line = grid_block(label, "value");
        assert_eq!(
            &line[VALUE_COL..],
            "value",
            "{label:?} put its value in the wrong column"
        );
    }
}

/// Nothing to say prints nothing — not a labelled empty row, not a line of invisible
/// gutter. Both existed: `step("", "")` emitted thirteen spaces, and `step("signing",
/// "")` emitted the word `signing` alone, twice, around the stranding warning.
#[test]
fn an_empty_message_prints_a_genuinely_empty_line() {
    assert_eq!(grid_block("", ""), "");
    assert_eq!(grid_block("signing", ""), "");
    assert_eq!(grid_block("signing", "   "), "");
    // And a deliberate blank line INSIDE a block is a real empty line, not a gutter.
    let block = grid_block("apple id", "first\n\nsecond");
    for row in block.lines() {
        assert!(
            !row.trim().is_empty() || row.is_empty(),
            "a whitespace-only row leaves invisible trailing space: {row:?}"
        );
    }
    assert!(
        block.contains("\n\n"),
        "the author's blank line must survive: {block:?}"
    );
}

/// A prompt ends `"… [y/N] "` and the cursor has to sit one space clear of the question.
#[test]
fn a_prompts_trailing_space_survives_the_wrap() {
    let block = grid_block(
        "apple id",
        "spend one of five permanent slots? Continue? [y/N] ",
    );
    assert!(block.ends_with("[y/N] "), "{block:?}");
}

/// Paths, base64 public keys and commands go out WHOLE, even when they overrun. A
/// hyphenated public key is a public key the operator cannot paste, and the transcript's
/// whole job at that moment is to be pasteable.
#[test]
fn a_long_token_overruns_rather_than_breaking() {
    // Split so a secret scanner reads no `key = "<base64>"` assignment (gitleaks
    // `generic-api-key`); the value is the same 44 characters.
    let key = concat!("cw5gIGYQzX6xrhTXjXU9", "nYfLWeoIkiZ1yUX7d1wmdz8=");
    let block = grid_block("roster", &format!("the head key {key} signs a real cut"));
    assert!(block.contains(key), "the key must survive intact:\n{block}");
    let path =
        "/Users//example/aterm/dist/toolchain-seed/a-very-long-artifact-name-that-overruns.tar.zst";
    assert!(grid_block("seed", path).contains(path));
}

/// An author's own newline is absolute, and a segment's leading spaces become ITS
/// hanging indent — that is how a bullet stays attached to the bullet above it.
#[test]
fn authored_structure_survives_and_sub_bullets_hang() {
    let block = grid_block(
        "seed",
        "WARNING — no x86_64-apple-darwin artifacts\nship it anyway with ATERM_SEED_ARCH_ACK=1\n  · an Intel Mac installs NOTHING from this seal, and the sentence explaining why runs on long enough to wrap at least once",
    );
    let rows: Vec<&str> = block.lines().collect();
    assert!(rows[0].starts_with("  seed"), "{:?}", rows[0]);
    assert!(rows[1].starts_with(&" ".repeat(VALUE_COL)), "{:?}", rows[1]);
    assert!(
        rows[1].trim_start().starts_with("ship it anyway"),
        "{:?}",
        rows[1]
    );
    let bullet = rows
        .iter()
        .position(|r| r.contains("· an Intel Mac"))
        .expect("the bullet");
    let cont = rows[bullet + 1];
    assert!(
        cont.starts_with(&format!("{}  ", " ".repeat(VALUE_COL))),
        "a wrapped sub-bullet must hang under its own text, not under the value column: {cont:?}"
    );
}

/// Rendered COLUMNS, not bytes, and measured from the gutter the line actually prints in.
///
/// The guard this replaces was `line.len() <= 76` inside `apple.rs`: bytes (an em-dash
/// costs 3 for one column) against a budget measured from the wrong origin (the rendered
/// line is `13 + len`, so 76 permitted 89 columns). It was simultaneously too loose and
/// too tight, and it passed while the defect shipped.
#[test]
fn hand_authored_structure_fits_an_eighty_column_window() {
    let csr = Path::new("/Users//example/Downloads/devid-m22.certSigningRequest");
    let mut over = Vec::new();
    // Both shapes of the errand: the named-folder one, and the one with nothing named
    // (which carries the ⌘⇧G / --cert-dir hint instead).
    let named = apple::Watch::new(Some(Path::new("/Users//example/Downloads")));
    let own = apple::Watch::new(None);
    let lines = apple::errand_lines(csr, true, &named)
        .into_iter()
        .chain(apple::errand_lines(csr, true, &own));
    for line in lines {
        for row in publish::grid_block_at(80, "apple id", &line).lines() {
            // A row that is a single unbreakable token (a path, a URL) is allowed to
            // overrun — see `wrapped`, rule 3.
            let one_token = row.trim().split(' ').count() == 1;
            if row.chars().count() > 80 && !one_token {
                over.push(format!("{} cols: {row:?}", row.chars().count()));
            }
        }
    }
    assert!(
        over.is_empty(),
        "wider than an 80-column window:\n{}",
        over.join("\n")
    );
}

/// A list stays a list when it wraps. Continuations hang under the marker's TEXT — a
/// five-item warning whose items unwrap flush against each other is a paragraph with
/// dots in it, and the whole reason it is a list is that the items are countable.
#[test]
fn a_wrapped_list_item_hangs_under_its_marker() {
    let block = publish::grid_block_at(
        60,
        "seed",
        "· it does NOT fall back to a network install: the published index carries no x86_64 packages at all",
    );
    let rows: Vec<&str> = block.lines().collect();
    assert!(rows.len() > 1, "the fixture must wrap: {block}");
    assert!(rows[0][VALUE_COL..].starts_with("· "), "{:?}", rows[0]);
    assert!(
        rows[1].starts_with(&format!("{}  ", " ".repeat(VALUE_COL)))
            && !rows[1][VALUE_COL..].starts_with("· "),
        "the continuation must hang under the item's text: {:?}",
        rows[1]
    );
}

/// NOTHING is printed after the errand.
///
/// The trap has to be the last thing on the screen when the wait begins. It used to be
/// followed by `step("", "waiting for the certificate to appear…")`, and that sentence in
/// that position reads as permission to go to the portal and collect whatever is in the
/// list — which is exactly the act the trap exists to prevent, and exactly what happened
/// in the field, at the cost of one of five permanent certificate slots.
///
/// Asserted over the SOURCE of `await_then_install`, because the harm is a call site: a
/// test over `errand_lines`' contents cannot see a `step` added below the loop.
#[test]
fn nothing_is_printed_after_the_errand() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/apple.rs"))
        .expect("apple.rs");
    let body = &text[text.find("fn await_then_install(").expect("the function")..];
    let loop_at = body
        .find("for line in errand_lines(")
        .expect("the errand loop");
    // Past the loop's own body — its `step(label, &line)` is the errand itself.
    let after = &body[loop_at + body[loop_at..].find("\n    }").expect("the loop's close")..];
    // Up to the end of the wait loop's opening — everything between the errand and the
    // first `while` is unconditional output on the way into a thirty-minute wait.
    let head = &after[..after
        .find("while started.elapsed()")
        .expect("the wait loop")];
    assert!(
        !head.contains("step("),
        "the errand's trap must be the last line before the wait, and this prints below \
         it:\n{head}"
    );
}
