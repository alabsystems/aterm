// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

fn found(changelog: &str) -> Vec<Finding> {
    run(&CHANGELOG_REPEAT, &[(CHANGELOG, changelog)])
}

/// An entry of two paragraphs, as the file writes one.
const LIGHT: &str = "- **The mode light no longer presses into a reply.** A click on the light\n  \
                     could press Shift+Tab into the reply.\n\n  It now holds the press.\n";

#[test]
fn an_unreleased_copy_of_a_released_entry_fails_naming_the_release() {
    let changelog = format!(
        "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n{LIGHT}\n- **Fresh.** New.\n\n\
         ## [0.2.0] - 2026-11-07\n\n### Fixed\n\n{LIGHT}\n## [0.1.0] - 2026-11-06\n\n{LIGHT}"
    );
    let found = found(&changelog);
    assert_eq!(spots(&found), ["CHANGELOG.md:7"], "{found:?}");
    let detail = &found[0].detail;
    assert!(
        detail.contains("the one [0.2.0] holds at CHANGELOG.md:18")
            && detail.contains("if v0.2.0 shipped it")
            && detail.contains("so delete this copy from [Unreleased]; else delete that one"),
        "{detail}"
    );
}

#[test]
fn an_entry_landed_twice_under_unreleased_fails_at_the_second_copy() {
    let changelog = format!(
        "## [Unreleased]\n\n### Added\n\n{LIGHT}\n### Fixed\n\n- **Other.** Text.\n\n{LIGHT}"
    );
    let found = found(&changelog);
    assert_eq!(spots(&found), ["CHANGELOG.md:14"], "{found:?}");
    assert!(
        found[0]
            .detail
            .contains("the [Unreleased] one at CHANGELOG.md:5"),
        "{found:?}"
    );
}

#[test]
fn an_entry_runs_through_a_blank_line_to_the_indented_line_after_it() {
    let first_paragraph = LIGHT.split("\n\n").next().unwrap_or(LIGHT);
    let changelog = format!(
        "## [Unreleased]\n\n{first_paragraph}\n\n- **Fresh.** New.\n\n{LIGHT}\n\
         ## [0.2.0] - 2026-11-07\n\n{LIGHT}"
    );
    let found = found(&changelog);
    assert_eq!(spots(&found), ["CHANGELOG.md:8"], "{found:?}");
}

#[test]
fn a_follow_up_that_shares_only_its_first_line_passes() {
    let rewrite = "- **The mode light no longer presses into a reply.** A click on the light\n  \
                   now waits for the turn's end.\n";
    let changelog = format!(
        "## [Unreleased]\n\n### Fixed\n\n{rewrite}\n{LIGHT}\n## [0.2.0] - 2026-11-07\n\n\
         ### Fixed\n\n{LIGHT}"
    );
    let found = found(&changelog);
    assert_eq!(spots(&found), ["CHANGELOG.md:8"], "{found:?}");
}

#[test]
fn entries_repeated_among_released_sections_are_shipped_history() {
    let changelog = format!(
        "## [Unreleased]\n\n- **Old.** Shipped.\n\n## [0.3.0] - 2026-11-07\n\n{LIGHT}\n{LIGHT}\n\
         ## [0.2.0] - 2026-11-06\n\n{LIGHT}\n- **Old.** Shipped.\n\n## Earlier releases\n\n\
         - **Older.** Cut.\n"
    );
    let found = found(&changelog);
    assert_eq!(spots(&found), ["CHANGELOG.md:3"], "{found:?}");
    assert!(
        found[0]
            .detail
            .contains("the one [0.2.0] holds at CHANGELOG.md:24"),
        "{found:?}"
    );
}

#[test]
fn a_changelog_with_no_unreleased_section_has_nothing_to_repeat() {
    let released =
        format!("## [0.2.0] - 2026-11-07\n\n{LIGHT}\n## [0.1.0] - 2026-11-06\n\n{LIGHT}");
    assert_eq!(spots(&found(&released)), Vec::<String>::new());
    let with_unreleased = format!("## [Unreleased]\n\n{LIGHT}\n{released}");
    assert_eq!(spots(&found(&with_unreleased)), ["CHANGELOG.md:3"]);
}
