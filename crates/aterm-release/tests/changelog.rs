// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Golden + oracle tests for the changelog gate/roll/extract port (release
//! spec §3).
//!
//! The Rust port replaced a retired shell pipeline (`changelog_real_body` and
//! the roll awk from tools/prepare-release.sh, the section/trim extraction
//! from tools/extract-changelog.sh). Those exact awk programs are vendored
//! below as the ORACLE: several tests run them via `/usr/bin/awk` against the
//! same input — including the repo's real, living CHANGELOG.md — and demand
//! line-for-line agreement. Parity with the code that cut every release
//! through v0.25 is the correctness bar, not this author's reading of it.

// The release crate is a binary on purpose (the spec's §9 file plan has no
// lib.rs), so the integration tests compile the modules under test directly.
// `changelog` and `ledger` cross-reference through `crate::`, hence both are
// mounted even though only `changelog` is exercised here.
#[path = "../src/changelog.rs"]
#[allow(dead_code)] // test mount: only the gate/roll/extract surface is exercised here
mod changelog;
#[path = "../src/ledger.rs"]
#[allow(dead_code)] // mounted only because changelog cross-references crate::ledger
mod ledger;

use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

// --------------------------------------------------------------------------
// vendored oracles (verbatim from the retired scripts)
// --------------------------------------------------------------------------

/// `changelog_real_body` from tools/prepare-release.sh, verbatim.
const REAL_BODY_AWK: &str = r###"
		$0 ~ ("^## \\[" want "\\]") { insec = 1; next }
		insec && /^## / { exit }
		!insec { next }
		{
			line = $0
			if (incomment) {
				if (match(line, /-->/)) { line = substr(line, RSTART + RLENGTH); incomment = 0 }
				else { next }
			}
			while (match(line, /<!--.*-->/))
				line = substr(line, 1, RSTART - 1) substr(line, RSTART + RLENGTH)
			if (match(line, /<!--/)) { line = substr(line, 1, RSTART - 1); incomment = 1 }
			if (line ~ /^[[:space:]]*###/) next
			if (line ~ /^[[:space:]]*$/) next
			print line
		}
"###;

/// The roll transform from tools/prepare-release.sh, verbatim.
const ROLL_AWK: &str = r###"
	/^## \[Unreleased\]/ && !done {
		print
		print ""
		print "## [" ver "] - " date
		done = 1
		next
	}
	{ print }
"###;

/// The `section` extraction from tools/extract-changelog.sh, verbatim.
const SECTION_AWK: &str = r###"
	$0 ~ ("^## \\[" want "\\]") { grab = 1; next }
	grab && /^## \[/ { exit }
	grab { print }
"###;

/// The `trim_blanks` filter from tools/extract-changelog.sh, verbatim.
const TRIM_BLANKS_AWK: &str = r###"NF { if (!started) started = 1 } started { buf[n++] = $0 }
	     END { last = n; while (last > 0 && buf[last-1] ~ /^[[:space:]]*$/) last--;
	           for (i = 0; i < last; i++) print buf[i] }"###;

/// Run an awk oracle over `input`, return raw stdout. The input goes through
/// a temp file (not stdin) exactly as the retired scripts read CHANGELOG.md.
fn awk(script: &str, vars: &[(&str, &str)], input: &str) -> String {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let p = env::temp_dir().join(format!(
        "aterm-changelog-oracle-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&p, input).unwrap();
    let mut cmd = Command::new("/usr/bin/awk");
    for (k, v) in vars {
        cmd.arg("-v").arg(format!("{k}={v}"));
    }
    cmd.arg(script).arg(&p);
    let out = cmd.output().expect("spawn /usr/bin/awk");
    let _ = fs::remove_file(&p);
    assert!(
        out.status.success(),
        "awk failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn awk_real_body(section: &str, input: &str) -> Vec<String> {
    awk(REAL_BODY_AWK, &[("want", section)], input)
        .lines()
        .map(str::to_string)
        .collect()
}

/// The full extract-changelog.sh pipeline (section | trim_blanks), minus its
/// fall-back-to-Unreleased lane (deliberately not ported — see rolled_body).
fn awk_extract(section: &str, input: &str) -> String {
    let sect = awk(SECTION_AWK, &[("want", section)], input);
    awk(TRIM_BLANKS_AWK, &[], &sect)
}

/// The repo's real CHANGELOG.md — the living document the tool will actually
/// run against; parity here is parity where it counts.
fn real_changelog() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../CHANGELOG.md");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// --------------------------------------------------------------------------
// fixtures — the entry text is copied from real CHANGELOG.md cases
// --------------------------------------------------------------------------

/// Real cases copied from CHANGELOG.md (v0.26 [Unreleased]: the DEFAULT-3/4
/// and CLIENT-2 entries; the FIND-1 entry with its indented sub-bullets),
/// plus the file's real header prose that sits OUTSIDE any section.
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

// --------------------------------------------------------------------------
// real_body: awk parity + goldens
// --------------------------------------------------------------------------

#[test]
fn real_body_matches_the_retired_awk_on_the_real_changelog() {
    let real = real_changelog();
    for section in ["Unreleased", "0.25"] {
        assert_eq!(
            changelog::real_body(&real, section),
            awk_real_body(section, &real),
            "real_body diverges from changelog_real_body on CHANGELOG.md [{section}]"
        );
    }
    // Substance guard: [0.25] is history and stays non-empty forever, so this
    // parity can never be two trivially-empty outputs agreeing.
    assert!(!changelog::real_body(&real, "0.25").is_empty());
}

#[test]
fn real_body_matches_the_retired_awk_on_copied_real_cases() {
    for fix in [FIX_REAL_CASES, FIX_NASTY, FIX_SMALL] {
        for section in ["Unreleased", "0.25"] {
            assert_eq!(
                changelog::real_body(fix, section),
                awk_real_body(section, fix),
                "real_body diverges from the awk oracle on a fixture [{section}]"
            );
        }
    }
}

#[test]
fn real_body_strips_comments_scaffolds_and_blanks() {
    // Golden expectations for the adversarial fixture. Note the awk parity
    // quirks held deliberately: the greedy `<!--.*-->` strips from the FIRST
    // `<!--` to the LAST `-->` of a line ("a  c", not "a  b  c"), and text
    // around stripped comments keeps its surrounding spaces.
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

// --------------------------------------------------------------------------
// the pre-claim gate
// --------------------------------------------------------------------------

#[test]
fn gate_passes_and_counts_top_level_bullets() {
    // "a  c", "keep " etc. are body lines but not entries: entries are the
    // transcript's headline number, counting top-level bullets only.
    assert_eq!(changelog::gate_unreleased(FIX_NASTY).unwrap().entries, 2);
    assert_eq!(
        changelog::gate_unreleased(FIX_REAL_CASES).unwrap().entries,
        3
    );
    // On the living file, the count must agree with the oracle's view.
    let real = real_changelog();
    let expected = awk_real_body("Unreleased", &real)
        .iter()
        .filter(|l| l.starts_with("- ") || l.starts_with("* "))
        .count();
    if expected > 0 {
        assert_eq!(changelog::gate_unreleased(&real).unwrap().entries, expected);
    }
}

#[test]
fn gate_refuses_every_empty_shape() {
    // The gate exists so no release ever ships note-less: truly empty,
    // whitespace-only, comment-only (multi-line included) and bare-###
    // scaffold sections must ALL fail, exactly like the retired guard.
    let empties = [
        "## [Unreleased]\n\n## [0.25] - x\n- y\n",
        "## [Unreleased]\n   \n\t\n## [0.25] - x\n- y\n",
        "## [Unreleased]\n<!-- multi\nline comment -->\n\n## [0.25] - x\n- y\n",
        "## [Unreleased]\n\n### Added\n### Fixed\n\n## [0.25] - x\n- y\n",
    ];
    for text in empties {
        let err = changelog::gate_unreleased(text).unwrap_err().to_string();
        assert!(err.contains("no release notes"), "{text:?} → {err}");
        // Cross-check emptiness with the oracle.
        assert!(awk_real_body("Unreleased", text).is_empty());
    }
    let err = changelog::gate_unreleased("# Changelog\n\n## [0.25] - x\n- y\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no \"## [Unreleased]\""), "{err}");
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

    // But ''' in an ALREADY-SHIPPED section is none of the gate's business —
    // only [Unreleased] is about to become manifest bytes.
    let elsewhere = "## [Unreleased]\n- clean entry\n\n## [0.25] - x\n- has ''' here\n";
    assert!(changelog::gate_unreleased(elsewhere).is_ok());
}

// --------------------------------------------------------------------------
// roll
// --------------------------------------------------------------------------

#[test]
fn roll_matches_the_byte_exact_golden() {
    let rolled = changelog::roll(FIX_SMALL, "0.2.0", "2026-07-06").unwrap();
    assert_eq!(rolled, FIX_SMALL_ROLLED);
}

#[test]
fn roll_matches_the_retired_awk_on_the_real_changelog() {
    // Version 99.99.0 will never exist in the file, so this stays valid across
    // every future real roll of CHANGELOG.md.
    let real = real_changelog();
    let mine = changelog::roll(&real, "99.99.0", "2026-07-06").unwrap();
    let oracle = awk(
        ROLL_AWK,
        &[("ver", "99.99.0"), ("date", "2026-07-06")],
        &real,
    );
    assert_eq!(mine, oracle, "roll diverges from prepare-release.sh's awk");
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

// --------------------------------------------------------------------------
// extract (the verbatim body that ships in the manifest and release notes)
// --------------------------------------------------------------------------

#[test]
fn rolled_body_extracts_verbatim_with_scaffolds_and_comments_kept() {
    let rolled = changelog::roll(FIX_SMALL, "0.2.0", "2026-07-06").unwrap();
    // VERBATIM: the ### scaffold ships (unlike real_body, which is a gate
    // filter, not the shipping text) — only edge blank lines are trimmed.
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
fn rolled_body_matches_the_retired_extract_pipeline_after_a_real_roll() {
    // End-to-end shape of a real cut: roll the living CHANGELOG.md, then
    // extract the rolled section — and demand extract-changelog.sh's
    // section|trim_blanks pipeline agrees byte-for-byte.
    let real = real_changelog();
    let rolled = changelog::roll(&real, "99.99.0", "2026-07-06").unwrap();
    let oracle = awk_extract("99.99.0", &rolled);
    match changelog::rolled_body(&rolled, "99.99.0") {
        // awk `print` terminates the last line; rolled_body returns unterminated.
        Ok(mine) => assert_eq!(format!("{mine}\n"), oracle),
        // If [Unreleased] is empty right after a real cut, both sides must
        // agree it is empty (the port hard-errors where the script fell back).
        Err(_) => assert!(oracle.trim().is_empty()),
    }
}

// --------------------------------------------------------------------------
// odds and ends
// --------------------------------------------------------------------------

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
    // The retired two-component heading is NOT the current-scheme release of
    // the same numbers: a recut probe for 0.25.0 must not latch onto `[0.25]`.
    assert!(!changelog::has_section(FIX_SMALL, "0.25.0"));
}

#[test]
fn today_la_is_a_calendar_date() {
    // The function shells out to date(1) with TZ=America/Los_Angeles (exact
    // prepare-release.sh parity) and shape-checks; assert the contract holds.
    let d = changelog::today_la().unwrap();
    assert_eq!(d.len(), 10, "{d}");
    assert!(
        d.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            _ => b.is_ascii_digit(),
        }),
        "{d}"
    );
}
