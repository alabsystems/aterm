// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_handwritten_entries_make_a_section_nonempty() {
    let generated =
        format!("## [Unreleased]\n\n### Changed\n\n{HARNESS_ENTRY_HEAD}0.191.0:\n  tool 1.2.**\n");
    let prose = "## [Unreleased]\n\n### Changed\n\nA paragraph without an entry.\n";
    let refused = [&generated, prose]
        .into_iter()
        .filter(|text| gate_unreleased(text).is_err())
        .count();
    assert_eq!(
        refused, 2,
        "generated continuations and prose are not entries"
    );
    let handwritten = generated.replace(HARNESS_ENTRY_HEAD, "- A handwritten note: ");
    assert!(gate_unreleased(&handwritten).is_ok());
}

const GOOD: &str = "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- A new feature.\n\n### Fixed\n\n- A repair.\n  Continuation.\n\n- Another repair.\n";

/// Each mutation used to pass the structural gate. Removing its rule makes its assertion
/// fail, while the unmodified document is the positive control.
#[test]
fn structural_mutations_are_refused() {
    assert!(findings(GOOD, "Unreleased", Some("0.191.0")).is_empty());
    for (text, expected) in [
        (
            GOOD.replacen("## [Unreleased]", "- Stray entry.\n\n## [Unreleased]", 1),
            "above the first",
        ),
        (
            format!("{GOOD}\n### Fixed\n\n- Third repair.\n"),
            "duplicate ### Fixed",
        ),
        (
            GOOD.replace("### Added", "### Security"),
            "out of Keep a Changelog order",
        ),
        (
            GOOD.replace("Continuation.\n\n-", "Continuation.\n-"),
            "glued",
        ),
        (GOOD.replace("A new feature.", "'''"), "contains '''"),
        (GOOD.replace("A new feature.", "\u{1}"), "control character"),
        (
            GOOD.replace("A new feature.", "a feature.\n<<<<<<< branch"),
            "conflict marker",
        ),
        ("## [Unreleased]\n\n### Fixed\n".into(), "no release notes"),
    ] {
        let found = findings(&text, "Unreleased", Some("0.191.0")).join("\n");
        assert!(found.contains(expected), "missing {expected}: {found}");
        assert!(gate_unreleased(&text).is_err());
    }
    let lazy_glued = GOOD.replace("  Continuation.\n\n-", "Continuation.\n-");
    assert!(
        findings(&lazy_glued, "Unreleased", None)
            .iter()
            .any(|s| s.contains("glued"))
    );
    let harness = format!("{HARNESS_ENTRY_HEAD}0.191.0: tool 1.2.**\n");
    let one = format!("{GOOD}\n{harness}");
    assert!(findings(&one, "Unreleased", Some("0.191.0")).is_empty());
    assert!(
        findings(&format!("{one}\n{harness}"), "Unreleased", Some("0.191.0"))
            .iter()
            .any(|s| s.contains("duplicate Harness"))
    );
    assert!(
        findings(
            &one.replace("since v0.191.0", "since v0.188.0"),
            "Unreleased",
            Some("0.191.0")
        )
        .iter()
        .any(|s| s.contains("previous public release"))
    );
    assert!(
        findings(
            &GOOD.replace("### Added", "### Surprise"),
            "Unreleased",
            None
        )
        .iter()
        .any(|s| s.contains("unknown subsection"))
    );
    assert!(
        findings(
            &format!("{GOOD}\n## [Unreleased]\n\n- Split notes.\n"),
            "Unreleased",
            None
        )
        .iter()
        .any(|s| s.contains("duplicate [Unreleased]"))
    );
    let scaffold = GOOD.replace(
        "### Added",
        "<!--\n### Fixed\n- scaffolding\n-->\n### Added",
    );
    assert!(gate_unreleased(&scaffold).is_ok());
}

/// Exact files from 34ec7372a and its repair 990c063dc6, as ordinary Markdown fixtures.
/// No git subprocess or mutable main is part of the fixture oracle.
#[test]
fn the_failed_cut_and_its_repair() {
    let before = include_str!("../tests/fixtures/changelog-before.md");
    let repaired = include_str!("../tests/fixtures/changelog-repaired.md");
    let scratch = aterm_tempfile::tempdir().unwrap();
    std::fs::write(scratch.path().join(CHANGELOG_FILE), before).unwrap();
    // Baseline: the dev set's unchanged source-guard implementation, which predates this
    // shared rule. It has no structural changelog judgment; the repeat rule is distinct.
    let old = crate::run_guards(scratch.path(), &Err("fixture has no engine".into())).unwrap();
    let old_changelog = old
        .findings
        .iter()
        .filter(|f| f.rule.starts_with("changelog"))
        .count();
    assert_eq!(old_changelog, 0);
    let began = std::time::Instant::now();
    let after = findings(before, "Unreleased", Some("0.191.0"));
    eprintln!(
        "changelog planted-red: before={old_changelog} findings; after={} findings, shared_guard_wall_us={}",
        after.len(),
        began.elapsed().as_micros()
    );
    let found = after.join("\n");
    for defect in [
        "above the first",
        "duplicate ### Fixed",
        "out of Keep a Changelog order",
        "glued",
        "duplicate Harness",
        "previous public release",
    ] {
        assert!(found.contains(defect), "missing {defect}: {found}");
    }
    assert!(findings(repaired, "Unreleased", Some("0.191.0")).is_empty());
    assert!(gate_unreleased(repaired).is_ok());
}
