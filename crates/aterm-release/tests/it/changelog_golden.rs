// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Golden tests for the changelog gate, roll, and extract.

use crate::changelog;

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

#[test]
fn real_body_strips_comments_scaffolds_and_blanks() {
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
    let body = changelog::real_body(FIX_REAL_CASES, "Unreleased");
    assert!(body.iter().any(|l| l.contains("DEFAULT-3/DEFAULT-4")));
    assert!(
        body.iter()
            .any(|l| l.starts_with("  - **the whole live screen"))
    );
    assert!(body.iter().all(|l| !l.trim_start().starts_with("###")));
    assert!(body.iter().all(|l| !l.trim().is_empty()));
}

#[test]
fn real_body_reads_a_dated_section_to_the_end_of_the_file() {
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

#[test]
fn gate_passes_and_counts_top_level_bullets() {
    assert_eq!(changelog::gate_unreleased(FIX_NASTY).unwrap().entries, 2);
    assert_eq!(
        changelog::gate_unreleased(FIX_REAL_CASES).unwrap().entries,
        3
    );
}

#[test]
fn gate_refuses_every_empty_shape() {
    let empties = [
        "## [Unreleased]\n\n## [0.25] - x\n- y\n",
        "## [Unreleased]\n   \n\t\n## [0.25] - x\n- y\n",
        "## [Unreleased]\n<!-- multi\nline comment -->\n\n## [0.25] - x\n- y\n",
        "## [Unreleased]\n\n### Added\n### Fixed\n\n## [0.25] - x\n- y\n",
    ];
    for text in empties {
        let err = changelog::gate_unreleased(text).unwrap_err().to_string();
        assert!(err.contains("no release notes"), "{text:?} → {err}");
        assert!(changelog::real_body(text, "Unreleased").is_empty());
    }
    let err = changelog::gate_unreleased("# Changelog\n\n## [0.25] - x\n- y\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no \"## [Unreleased]\""), "{err}");
}

#[test]
fn recut_gate_judges_the_rolled_section() {
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

    let err = changelog::gate_section(FIX_SMALL, "9.99.0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no \"## [9.99.0]\" section"), "{err}");

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
    let line = fix.lines().position(|l| l.contains("'''")).unwrap() + 1;
    let err = changelog::gate_unreleased(fix).unwrap_err().to_string();
    assert!(err.contains(&format!("line {line}")), "{err}");
    assert!(err.contains("'''"), "{err}");

    let commented =
        "## [Unreleased]\n- real entry\n<!-- has ''' inside -->\n\n## [0.25] - x\n- y\n";
    assert!(changelog::gate_unreleased(commented).is_err());

    let quad = "## [Unreleased]\n- entry with '''' run\n\n## [0.25] - x\n- y\n";
    assert!(changelog::gate_unreleased(quad).is_err());

    let elsewhere = "## [Unreleased]\n- clean entry\n\n## [0.25] - x\n- has ''' here\n";
    assert!(changelog::gate_unreleased(elsewhere).is_ok());
}

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

#[test]
fn rolled_body_extracts_verbatim_with_scaffolds_and_comments_kept() {
    let rolled = changelog::roll(FIX_SMALL, "0.2.0", "2026-07-06").unwrap();
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

#[test]
fn has_section_is_exact_about_versions() {
    assert!(changelog::has_section(FIX_SMALL, "0.25"));
    assert!(!changelog::has_section(FIX_SMALL, "9.99.0"));
    assert!(!changelog::has_section("## [0.2.0.1] - x\n", "0.2.0"));
    assert!(!changelog::has_section("## [0.2.10] - x\n", "0.2.1"));
    assert!(!changelog::has_section(FIX_SMALL, "0.25.0"));
}
