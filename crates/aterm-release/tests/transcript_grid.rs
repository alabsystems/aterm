// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The transcript grid, enforced.
//!
//! `cargo ship provision` is read by exactly one audience — an operator at a terminal,
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

// The release crate is a binary on purpose (the spec's §9 file plan has no lib.rs), so
// the integration tests compile the modules under test directly. `publish.rs` reaches
// every pipeline stage through `crate::`, hence the full mount list.
#[path = "../src/apple.rs"]
#[allow(dead_code)]
mod apple;
#[path = "../src/buildplan.rs"]
#[allow(dead_code)]
mod buildplan;
#[path = "../src/bundle.rs"]
#[allow(dead_code)]
mod bundle;
#[path = "../src/changelog.rs"]
#[allow(dead_code)]
mod changelog;
#[path = "../src/cli.rs"]
#[allow(dead_code)]
mod cli;
#[path = "../src/dmg.rs"]
#[allow(dead_code)]
mod dmg;
#[path = "../src/gates.rs"]
#[allow(dead_code)]
mod gates;
#[path = "../src/ledger.rs"]
#[allow(dead_code)]
mod ledger;
#[path = "../src/machines.rs"]
#[allow(dead_code)]
mod machines;
#[path = "../src/manifest_out.rs"]
#[allow(dead_code)]
mod manifest_out;
#[path = "../src/mirror.rs"]
#[allow(dead_code)]
mod mirror;
#[path = "../src/provision.rs"]
#[allow(dead_code)]
mod provision;
#[path = "../src/publish.rs"]
#[allow(dead_code)]
mod publish;
#[path = "../src/seedpack.rs"]
#[allow(dead_code)]
mod seedpack;
#[path = "../src/sign.rs"]
#[allow(dead_code)]
mod sign;
#[path = "../src/verify.rs"]
#[allow(dead_code)]
mod verify;

use publish::{LABEL_MAX, VALUE_COL, grid_block};
use std::path::{Path, PathBuf};

/// Every `.rs` file under `src/`, as (path, text).
fn sources() -> Vec<(PathBuf, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("src/ is readable") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let text = std::fs::read_to_string(&path).expect("source is utf-8");
            // Comment lines are BLANKED, not dropped: these scanners report file:line,
            // and a doc comment that quotes the old layout ("  {label:<11}{msg}", the
            // `seed source` collision) is documentation, not a call site. Blanking keeps
            // the line numbers honest while taking prose out of the census.
            let code = text
                .lines()
                .map(|l| {
                    if l.trim_start().starts_with("//") {
                        ""
                    } else {
                        l
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            out.push((path, code));
        }
    }
    assert!(
        out.len() > 10,
        "the mount list says there are more modules than this"
    );
    out
}

/// The census. Every label literal handed to a grid printer fits the gutter.
///
/// This is the test that matters most, because the defect it kills is INVISIBLE in
/// review: an over-long label does not fail, does not warn and does not truncate — it
/// prints, with the separator silently eaten. `seed source` + `10 program(s) staged…`
/// came out as one word and one number, and the operator read it as a filename.
///
/// Matched by source text rather than at runtime on purpose: a label is chosen at the
/// call site, and the call site is what has to be constrained. A runtime assertion only
/// fires on the code path that ran.
#[test]
fn every_label_in_the_crate_fits_the_gutter() {
    let mut checked = 0usize;
    let mut over: Vec<String> = Vec::new();
    for (path, text) in sources() {
        // Whole-text, not line-by-line: rustfmt puts a long call's label on its own
        // line, and a scanner that only reads single lines would quietly skip exactly
        // the calls whose arguments were long enough to need wrapping.
        for call in ["step(", "print_check(", "record("] {
            let mut from = 0usize;
            while let Some(at) = text[from..].find(call) {
                let after = from + at + call.len();
                from = after;
                let rest = text[after..].trim_start();
                let Some(body) = rest.strip_prefix('"') else {
                    continue; // a variable label (`label`, `APPLE_LABEL`) — see below
                };
                let Some(end) = body.find('"') else { continue };
                let label = &body[..end];
                checked += 1;
                if label.chars().count() > LABEL_MAX {
                    let line = text[..after].lines().count();
                    over.push(format!(
                        "{}:{line}: {label:?} is {} columns, the grid carries {LABEL_MAX}",
                        path.display(),
                        label.chars().count()
                    ));
                }
            }
        }
    }
    assert!(
        checked > 60,
        "the census found only {checked} labels — did the call spelling change?"
    );
    assert!(
        over.is_empty(),
        "labels that eat their own separator:\n{}",
        over.join("\n")
    );
}

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

/// The grid has ONE definition. Two gutters three columns apart is the defect, and
/// "fixing" a hand-rolled gutter with a second hand-rolled gutter is how the last one
/// survived: `confirm_slot` counted 13 by hand, the notary prompt counted 11 by hand,
/// and both sat directly under lines produced by the primitive.
#[test]
fn no_source_file_rolls_its_own_gutter() {
    let banned = [":<11}", "\" \".repeat(13)", "\\n  notary   "];
    let mut hits = Vec::new();
    for (path, text) in sources() {
        for (n, line) in text.lines().enumerate() {
            for b in banned {
                if line.contains(b) {
                    hits.push(format!("{}:{}: {b}", path.display(), n + 1));
                }
            }
        }
    }
    assert!(hits.is_empty(), "hand-rolled gutters:\n{}", hits.join("\n"));
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
    for line in apple::errand_lines(csr, true) {
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

/// The x86 warning keeps every fact, and keeps the SPACE that was lost in it.
///
/// `It doesNOT fall back to a network install` reached a real operator's terminal — a
/// space eaten by a `\` line continuation. The sentence it damaged is the one that stops
/// someone assuming an Intel Mac will simply fetch the toolchain later, so it is worth a
/// test rather than a memory. The remaining assertions pin the arrangement: headline
/// first, the ACT second (it used to be word 91), then one fact per line.
#[test]
fn the_x86_warning_keeps_its_facts_its_order_and_its_space() {
    let text =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/seedpack.rs"))
            .expect("seedpack.rs");
    // As the compiler sees it: the `\` continuations joined, so a lost space shows up.
    let joined = text.replace("\\\n", "").replace("\\", "");
    let mut lines = joined
        .lines()
        .skip_while(|l| !l.contains("WARNING — no x86_64-apple-darwin artifacts"));
    let block: String = lines.by_ref().take(24).collect::<Vec<_>>().join("\n");
    // The ACT (restage; the ACK is a mute, not a gate) comes second — before any
    // mechanism — never at word 91 of a paragraph, which is where the original
    // buried it.
    assert!(
        block.find("restage from a current index") < block.find("an Intel Mac installs NOTHING"),
        "the act comes second, not at word 91: {block}"
    );
    assert!(
        block.contains("a warning-mute, not a gate"),
        "the ACK must be described as the mute it is — the old text's \"Acknowledge\" \
         read as a gate the code does not have: {block}"
    );
    // The FACTS, as of atpkg index 14 (which superseded the index-12 set: the
    // registry now publishes EVERY pinned program dual-arch — the rustc
    // coherence group included, pkg-trust-6808.toml carries the
    // x86_64-apple-darwin row — so the old "six programs / rustc_private"
    // story is history, and an x86-less seal now also means no Intel DMG
    // variant).
    for fact in [
        "seed-unusable: no build for this Mac's architecture",
        "STALE STAGE",
        "pinned program since index 14",
        "pkg-trust-6808.toml carries the row",
        "dmg_x86_64",
    ] {
        assert!(
            block.contains(fact),
            "the warning dropped {fact:?}: {block}"
        );
    }
    // The class of defect that started all this: a space lost across a `\`
    // continuation ("doesNOT"). The joined form must never fuse words.
    assert!(
        !block.contains("doesNOT") && !block.contains("NOTfall"),
        "{block}"
    );
}
