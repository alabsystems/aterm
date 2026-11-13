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

/// The line harness-bump generates is no release notes: alone it refuses, beside a hand-written
/// entry it passes. CONTROL: the same section with only the hand-written entry passes.
#[test]
fn the_generated_harness_line_alone_is_no_release_notes() {
    let generated = format!(
        "{}0.149.0: `alab claude harness 0.9`.** Each harness's minor moves once per release \
         in which it changed.",
        changelog::HARNESS_ENTRY_HEAD
    );
    let alone = format!("## [Unreleased]\n\n### Changed\n\n{generated}\n\n## [0.149.0] - x\n- y\n");
    let err = changelog::gate_unreleased(&alone).unwrap_err().to_string();
    assert!(err.contains("no release notes"), "{err}");
    let beside = format!(
        "## [Unreleased]\n\n### Changed\n\n{generated}\n\n### Fixed\n\n- **A fix.**\n\n## [0.149.0] - x\n"
    );
    assert_eq!(changelog::gate_unreleased(&beside).unwrap().entries, 2);
    let hand = "## [Unreleased]\n\n### Fixed\n\n- **A fix.**\n\n## [0.25] - x\n";
    assert_eq!(changelog::gate_unreleased(hand).unwrap().entries, 1);
}

/// An entry left above `## [Unreleased]` (a merge that placed it after a release rolled the
/// section) belongs to no section, so the gate names its line. CONTROL: the same entry under
/// `[Unreleased]` passes, and so do a bullet inside a preamble comment and a preamble blockquote.
#[test]
fn an_entry_above_every_section_is_refused() {
    let stranded = "# Changelog\n\nPreamble prose.\n\n- **A fix.** Its words.\n## [Unreleased]\n\n\
                    ### Fixed\n\n- **Another fix.**\n\n## [0.25] - x\n- y\n";
    for section in ["Unreleased", "0.25"] {
        let err = changelog::gate_section(stranded, section)
            .unwrap_err()
            .to_string();
        assert!(err.contains("line 5: an entry above the first"), "{err}");
    }
    let placed = "# Changelog\n\nPreamble prose.\n\n## [Unreleased]\n\n### Fixed\n\n\
                  - **A fix.** Its words.\n- **Another fix.**\n\n## [0.25] - x\n- y\n";
    assert_eq!(changelog::gate_unreleased(placed).unwrap().entries, 2);
    let commented = "<!-- a note\n- not an entry\n-->\n> - quoted\n## [Unreleased]\n- **A fix.**\n";
    assert_eq!(changelog::gate_unreleased(commented).unwrap().entries, 1);
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

#[test]
fn gate_refuses_git_conflict_markers_with_the_file_line_number() {
    for (marker_line, head) in [
        ("<<<<<<< HEAD", "<<<<<<<"),
        ("||||||| base", "|||||||"),
        ("=======", "======="),
        (">>>>>>> origin/main", ">>>>>>>"),
    ] {
        let fix = format!(
            "# Changelog\n\n## [Unreleased]\n\n### Fixed\n- clean entry\n{marker_line}\n\n## [0.25] - 2026-01-01\n- old\n"
        );
        let err = changelog::gate_unreleased(&fix).unwrap_err().to_string();
        assert!(err.contains("line 7"), "{marker_line}: {err}");
        assert!(err.contains(head), "{marker_line}: {err}");
    }

    let outside = "## [Unreleased]\n\n### Fixed\n- clean entry\n\n## [0.25] - x\n>>>>>>> origin/main\n- old\n";
    assert!(changelog::gate_unreleased(outside).is_ok());
}

/// A live-shaped window: entries of the 0.199.0 window as written by hand, the harness line in
/// the middle of `### Changed` (a committed copy, never the live file).
const LIVE: &str = include_str!("../fixtures/changelog/live-window.txt");

/// `LIVE`'s `[Unreleased]` split as a writer would: each entry its own file under its kind,
/// named so name order is file order, and the harness line apart (the release derives it).
fn live_split() -> (String, Vec<changelog::Fragment>, String) {
    let lines: Vec<&str> = LIVE.lines().collect();
    let start = lines.iter().position(|l| *l == "## [Unreleased]").unwrap();
    let end = start
        + 1
        + lines[start + 1..]
            .iter()
            .position(|l| l.starts_with("## ["))
            .unwrap();
    let (mut kind, mut files, mut harness) = ("", Vec::new(), String::new());
    let mut entry: Vec<&str> = Vec::new();
    let flush = |kind: &str, entry: &mut Vec<&str>, files: &mut Vec<changelog::Fragment>| {
        while entry.last() == Some(&"") {
            entry.pop();
        }
        if !entry.is_empty() {
            files.push(changelog::Fragment {
                path: format!("changes/{kind}/e{:03}.md", files.len()),
                text: format!("{}\n", entry.join("\n")),
            });
        }
        entry.clear();
    };
    for line in &lines[start + 1..end] {
        if let Some(k) = line.strip_prefix("### ") {
            flush(&kind.to_lowercase(), &mut entry, &mut files);
            kind = k;
        } else if line.starts_with(changelog::HARNESS_ENTRY_HEAD) {
            flush(&kind.to_lowercase(), &mut entry, &mut files);
            harness = (*line).to_string();
        } else if line.starts_with("- ") {
            flush(&kind.to_lowercase(), &mut entry, &mut files);
            entry.push(line);
        } else if !entry.is_empty() {
            entry.push(line);
        }
    }
    flush(&kind.to_lowercase(), &mut entry, &mut files);
    let scaffold = format!(
        "{}\n\n{}\n",
        lines[..=start].join("\n"),
        lines[end..].join("\n")
    );
    (scaffold, files, harness)
}

/// T-R1. THE ROLL OF FRAGMENTS EQUALS TODAY'S BYTE FOR BYTE, apart from where the harness line
/// stands: the live-shaped window written as files and rolled, and the same window left raw (the
/// drain) and rolled, are the hand-laid window rolled with its harness line leading `### Changed`
/// (the place the release derives it), in the changelog, the section a release ships and the
/// notes document. NEGATIVE CONTROL: the hand-laid roll itself differs only by that line's place.
#[test]
fn the_roll_of_fragments_equals_todays_byte_for_byte() {
    let (scaffold, files, harness) = live_split();
    assert!(files.len() >= 8 && harness.starts_with(changelog::HARNESS_ENTRY_HEAD));
    let moved = LIVE.replace(&format!("{harness}\n\n"), "").replacen(
        "### Changed\n\n",
        &format!("### Changed\n\n{harness}\n\n"),
        1,
    );
    assert_ne!(moved, LIVE, "the live line stands mid-Changed");
    let today = changelog::roll(&moved, "0.199.0", "2026-11-12").unwrap();
    let assembled =
        aterm_verify::changelog_fragments::assemble(&scaffold, &files, Some(&harness)).unwrap();
    let from_files = changelog::roll(&assembled, "0.199.0", "2026-11-12").unwrap();
    let drained = aterm_verify::changelog_fragments::assemble(LIVE, &[], Some(&harness)).unwrap();
    let from_raw = changelog::roll(&drained, "0.199.0", "2026-11-12").unwrap();
    for rolled in [&from_files, &from_raw] {
        assert_eq!(rolled, &today);
        let body = changelog::rolled_body(rolled, "0.199.0").unwrap();
        assert_eq!(body, changelog::rolled_body(&today, "0.199.0").unwrap());
        assert_eq!(
            changelog::release_notes_document("0.199.0", &body),
            changelog::release_notes_document(
                "0.199.0",
                &changelog::rolled_body(&today, "0.199.0").unwrap()
            )
        );
    }
    let hand = changelog::roll(LIVE, "0.199.0", "2026-11-12").unwrap();
    assert_ne!(hand, today);
    assert_eq!(
        hand.replace(&format!("{harness}\n\n"), ""),
        today.replace(&format!("{harness}\n\n"), ""),
        "only the harness line's place differs"
    );
}
