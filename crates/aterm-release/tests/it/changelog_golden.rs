// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Golden tests for the changelog gate, roll and extract: the greedy `<!--.*-->` strip and
//! the blank and `###` scaffold removal, over `[Unreleased]` (the cut's gate) and over a
//! dated section that runs to the end of the file (the recut's gate), plus the roll's exact
//! bytes.

use crate::changelog;

// fixtures

/// Real CHANGELOG.md entries (one with indented sub-bullets), and the header prose that
/// sits outside any section.
const FIX_REAL_CASES: &str = "\
<!-- SPDX-License-Identifier: Apache-2.0 -->
# Changelog

All notable changes to aterm are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

> **Every release requires a hand-written `[Unreleased]` entry.**

## [Unreleased]

### Added
- **Signed updates on by default (DEFAULT-3/DEFAULT-4)** — ship builds now compile
  in the Ed25519 update-signing pubkey (updater Tier SIG: clients refuse unsigned
  or wrongly-signed release manifests) and the atpkg root key (the package manager
  verifies its index instead of shipping inert).
- **`aterm-ctl` ships inside the app (CLIENT-2)** — the macOS bundle now includes
  the control-socket CLI co-located next to the executable, and every aterm shell
  gets that directory on its `PATH` automatically.

### Fixed
- **Cmd-F find, second hardening pass (adversarial review).** A multi-agent review of the
  FIND-1 hardening surfaced two more real defects, both fixed with regression tests:
  - **the whole live screen is now searchable under a tiny history cap** — the visible-row
    floor was `visible_rows`, but the search index evicts to a 3/4 low-water mark.
  - **no more click dead-zone after a tab/pane switch** — switching tabs or focusing another
    pane with the find bar open left the clickable `Aa`/`.*` indicator geometry behind.

## [0.25] - 2026-07-06

### Added
- **Screen-reader access to the terminal (DEFAULT-2)** — the default build now
  publishes the terminal grid through AccessKit (`a11y-accesskit`, on by
  default), so VoiceOver and other screen readers can read the screen contents
  of every shipped binary. Caret tracking on this surface lands in v1.1.
";

/// Adversarial comment/scaffold/blank shapes, all in one section.
const FIX_NASTY: &str = "\
# Changelog

## [Unreleased]

### Added
- kept one
a <!-- x --> b <!-- y --> c
keep <!-- open comment
hidden inside
still hidden ### not a scaffold
--> tail survives
   ### Fixed

- kept two <!-- inline --> end

## [0.25] - 2026-01-01
- other
";

/// Minimal roll fixture with a byte-exact golden result.
const FIX_SMALL: &str = "\
# Changelog

## [Unreleased]

### Added
- **X** — thing.

## [0.25] - 2026-07-06

- old.
";

const FIX_SMALL_ROLLED: &str = "\
# Changelog

## [Unreleased]

## [0.2.0] - 2026-07-06

### Added
- **X** — thing.

## [0.25] - 2026-07-06

- old.
";

// real_body goldens

#[test]
fn real_body_strips_comments_scaffolds_and_blanks() {
    // The greedy `<!--.*-->` strips from the first `<!--` to the last `-->` of a line
    // ("a  c", not "a  b  c"), and text around a stripped comment keeps its spaces.
    assert_eq!(
        changelog::real_body(FIX_NASTY, "Unreleased"),
        vec![
            "- kept one".to_string(),
            "a  c".to_string(),
            "keep ".to_string(),
            " tail survives".to_string(),
            "- kept two  end".to_string(),
        ]
    );
    // Sub-bullets and multi-line entries of the real FIND-1 case survive.
    let body = changelog::real_body(FIX_REAL_CASES, "Unreleased");
    assert!(body.iter().any(|l| l.contains("DEFAULT-3/DEFAULT-4")));
    assert!(
        body.iter()
            .any(|l| l.starts_with("  - **the whole live screen"))
    );
    // Scaffolds and blanks are gone.
    assert!(body.iter().all(|l| !l.trim_start().starts_with("###")));
    assert!(body.iter().all(|l| !l.trim().is_empty()));
}

#[test]
fn real_body_reads_a_dated_section_to_the_end_of_the_file() {
    // The recut gate reads `## [X.Y.Z] - <date>`, not `[Unreleased]`: the heading carries
    // text after the bracket, and the file's last section ends at EOF rather than at a next
    // `## `. Every fixture's `[0.25]` is both.
    assert_eq!(changelog::real_body(FIX_NASTY, "0.25"), ["- other"]);
    assert_eq!(changelog::real_body(FIX_SMALL, "0.25"), ["- old."]);
    assert_eq!(
        changelog::real_body(FIX_REAL_CASES, "0.25"),
        [
            "- **Screen-reader access to the terminal (DEFAULT-2)** — the default build now",
            "  publishes the terminal grid through AccessKit (`a11y-accesskit`, on by",
            "  default), so VoiceOver and other screen readers can read the screen contents",
            "  of every shipped binary. Caret tracking on this surface lands in v1.1.",
        ]
    );
}

// the pre-claim gate

#[test]
fn gate_passes_and_counts_top_level_bullets() {
    // "a  c", "keep " etc. are body lines but not entries: entries are the
    // transcript's headline number, counting top-level bullets only.
    assert_eq!(changelog::gate_unreleased(FIX_NASTY).unwrap().entries, 2);
    assert_eq!(
        changelog::gate_unreleased(FIX_REAL_CASES).unwrap().entries,
        3
    );
}

#[test]
fn gate_refuses_every_empty_shape() {
    // No release ships note-less: empty, whitespace-only, comment-only (multi-line
    // included) and bare-`###` scaffold sections all fail.
    let empties = [
        "## [Unreleased]\n\n## [0.25] - x\n- y\n",
        "## [Unreleased]\n   \n\t\n## [0.25] - x\n- y\n",
        "## [Unreleased]\n<!-- multi\nline comment -->\n\n## [0.25] - x\n- y\n",
        "## [Unreleased]\n\n### Added\n### Fixed\n\n## [0.25] - x\n- y\n",
    ];
    for text in empties {
        let err = changelog::gate_unreleased(text).unwrap_err().to_string();
        assert!(err.contains("no release notes"), "{text:?} → {err}");
        // The refusal is real_body's verdict, not a second opinion of the gate's.
        assert!(changelog::real_body(text, "Unreleased").is_empty());
    }
    let err = changelog::gate_unreleased("# Changelog\n\n## [0.25] - x\n- y\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no \"## [Unreleased]\""), "{err}");
}

#[test]
fn recut_gate_judges_the_rolled_section() {
    // A recut gates the rolled `## [X.Y.Z] - <date>` section (gates.rs `changelog_gate_at`):
    // the fresh `[Unreleased]` scaffold above it is legitimately empty.
    assert_eq!(
        changelog::gate_section(FIX_REAL_CASES, "0.25")
            .unwrap()
            .entries,
        1
    );
    let rolled = changelog::roll(FIX_SMALL, "0.2.0", "2026-07-06").unwrap();
    assert!(changelog::gate_unreleased(&rolled).is_err());
    assert_eq!(
        changelog::gate_section(&rolled, "0.2.0").unwrap().entries,
        1
    );

    // A version with no section of its own is refused by name.
    let err = changelog::gate_section(FIX_SMALL, "9.99.0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no \"## [9.99.0]\" section"), "{err}");

    // The `'''` scan covers the section being recut, not `[Unreleased]`.
    let poisoned = "## [Unreleased]\n\n## [0.2.0] - 2026-07-06\n- has ''' here\n";
    let err = changelog::gate_section(poisoned, "0.2.0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("line 4"), "{err}");
}

#[test]
fn gate_refuses_triple_quote_with_the_file_line_number() {
    let fix = "\
# Changelog

## [Unreleased]

### Added
- an entry quoting '''docstring''' fences

## [0.25] - 2026-01-01
- old
";
    // Self-locating expectation: the assert names the line the fixture puts
    // the ''' on, so editing the fixture cannot silently detune the test.
    let line = fix.lines().position(|l| l.contains("'''")).unwrap() + 1;
    let err = changelog::gate_unreleased(fix).unwrap_err().to_string();
    assert!(err.contains(&format!("line {line}")), "{err}");
    assert!(err.contains("'''"), "{err}");

    // Even inside an HTML comment: raw section bytes ship into the TOML
    // literal, so a commented-out ''' poisons the manifest all the same.
    let commented =
        "## [Unreleased]\n- real entry\n<!-- has ''' inside -->\n\n## [0.25] - x\n- y\n";
    assert!(changelog::gate_unreleased(commented).is_err());

    // A run of four quotes still contains the terminator.
    let quad = "## [Unreleased]\n- entry with '''' run\n\n## [0.25] - x\n- y\n";
    assert!(changelog::gate_unreleased(quad).is_err());

    // ''' in an already-shipped section is none of the gate's business: only [Unreleased]
    // is about to become manifest bytes.
    let elsewhere = "## [Unreleased]\n- clean entry\n\n## [0.25] - x\n- has ''' here\n";
    assert!(changelog::gate_unreleased(elsewhere).is_ok());
}

// roll

#[test]
fn roll_matches_the_byte_exact_golden() {
    let rolled = changelog::roll(FIX_SMALL, "0.2.0", "2026-07-06").unwrap();
    assert_eq!(rolled, FIX_SMALL_ROLLED);
}

#[test]
fn roll_refuses_missing_unreleased_and_double_roll() {
    let err = changelog::roll("# Changelog\n\n## [0.25] - x\n- y\n", "0.2.0", "2026-07-06")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no \"## [Unreleased]\""), "{err}");

    let rolled = changelog::roll(FIX_SMALL, "0.2.0", "2026-07-06").unwrap();
    let err = changelog::roll(&rolled, "0.2.0", "2026-07-07")
        .unwrap_err()
        .to_string();
    assert!(err.contains("already"), "{err}");
}

// extract (the verbatim body that ships in the manifest and release notes)

#[test]
fn rolled_body_extracts_verbatim_with_scaffolds_and_comments_kept() {
    let rolled = changelog::roll(FIX_SMALL, "0.2.0", "2026-07-06").unwrap();
    // Verbatim: the ### scaffold ships (real_body is a gate filter, not the shipping text);
    // only edge blank lines are trimmed.
    assert_eq!(
        changelog::rolled_body(&rolled, "0.2.0").unwrap(),
        "### Added\n- **X** — thing."
    );

    let with_comment =
        "## [0.2.0] - 2026-07-06\n\n<!-- note to self -->\n- entry\n\n## [0.25] - x\n- y\n";
    assert_eq!(
        changelog::rolled_body(with_comment, "0.2.0").unwrap(),
        "<!-- note to self -->\n- entry"
    );

    assert!(
        changelog::rolled_body(FIX_SMALL, "9.99.0").is_err(),
        "missing section must error"
    );
    let empty = "## [0.2.0] - 2026-07-06\n\n\n## [0.25] - x\n- y\n";
    assert!(
        changelog::rolled_body(empty, "0.2.0").is_err(),
        "empty section must error"
    );
}

// odds and ends

#[test]
fn has_section_is_exact_about_versions() {
    // `## [0.25]` is a real retired-scheme heading — CHANGELOG.md is
    // append-only history, so has_section must keep finding those verbatim.
    assert!(changelog::has_section(FIX_SMALL, "0.25"));
    assert!(!changelog::has_section(FIX_SMALL, "9.99.0"));
    // Prefix discipline: 0.2.0 must not match a hypothetical 0.2.0.1 heading,
    // and 0.2.1 must not match the numerically distinct 0.2.10.
    assert!(!changelog::has_section("## [0.2.0.1] - x\n", "0.2.0"));
    assert!(!changelog::has_section("## [0.2.10] - x\n", "0.2.1"));
    // The retired two-component heading is not the current-scheme release of the same
    // numbers: a recut probe for 0.25.0 must not latch onto `[0.25]`.
    assert!(!changelog::has_section(FIX_SMALL, "0.25.0"));
}
