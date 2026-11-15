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
        .filter(|text| gate_section(text, "Unreleased").is_err())
        .count();
    assert_eq!(
        refused, 2,
        "generated continuations and prose are not entries"
    );
    let handwritten = generated.replace(HARNESS_ENTRY_HEAD, "- A handwritten note: ");
    assert!(gate_section(&handwritten, "Unreleased").is_ok());
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
        assert!(gate_section(&text, "Unreleased").is_err());
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
    assert!(gate_section(&scaffold, "Unreleased").is_ok());
}

/// The six defects of the failed cut 34ec7372a, planted as its `CHANGELOG.md` held them, and
/// its repair 990c063dc6's shape: an entry above the first heading, a second `### Fixed`, an
/// `### Added` after it, an entry glued to the one before, and two harness lines, the second
/// naming a release before the previous public one.
const FAILED_CUT: &str = "# Changelog

Write each change as its own file under `changes/`.

### Fixed

- An idle agent is relaunched after a restart.

## [Unreleased]

### Fixed

- Native brew compatibility keeps each formula's files.

### Added

- **Kimi Code runs under the `kimi-alab` harness.** Kimi sessions use
  aterm's turn-end policy.

### Changed

- **Mixed-direction lines are laid out with less work.**

### Fixed

- **Setup no longer mistakes the command-line-tools stubs for a compiler.** A stub
  is no longer reported as a toolchain.
- **Harness versions since v0.191.0: `alab claude harness 0.30`.** Each harness's minor moves once.

- **Harness versions since v0.188.0: `alab kimi harness 0.19`.** Each harness's minor moves once.

## [0.191.0] - 2026-11-09

### Changed

- **Release measurements wait less for idle memory samples.**
";

/// [`FAILED_CUT`] as 990c063dc6 repaired it.
const REPAIRED: &str = "# Changelog

Write each change as its own file under `changes/`.

## [Unreleased]

### Added

- **Kimi Code runs under the `kimi-alab` harness.** Kimi sessions use
  aterm's turn-end policy.

### Changed

- **Mixed-direction lines are laid out with less work.**

- **Harness versions since v0.191.0: `alab claude harness 0.30`, `alab kimi harness 0.19`.** Each harness's minor moves once.

### Fixed

- An idle agent is relaunched after a restart.

- Native brew compatibility keeps each formula's files.

- **Setup no longer mistakes the command-line-tools stubs for a compiler.** A stub
  is no longer reported as a toolchain.

## [0.191.0] - 2026-11-09

### Changed

- **Release measurements wait less for idle memory samples.**
";

/// THE FAILED CUT AND ITS REPAIR: each of the six defects is named, and the repair has none.
#[test]
fn the_failed_cut_and_its_repair() {
    let found = findings(FAILED_CUT, "Unreleased", Some("0.191.0")).join("\n");
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
    assert!(findings(REPAIRED, "Unreleased", Some("0.191.0")).is_empty());
    assert!(gate_section(REPAIRED, "Unreleased").is_ok());
}
