// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! changelog-repeat: no entry under `CHANGELOG.md`'s `## [Unreleased]` repeats one a release
//! already shipped, or one `[Unreleased]` already holds.
//!
//! `ship release` ships `[Unreleased]` verbatim as the release's notes (the Software Update
//! window's, the GitHub release's and the update manifest's changelog), then renames it
//! `## [X.Y.Z] - <date>` under a fresh `[Unreleased]`. A branch that merged `main` before a cut
//! and merges it again after can bring an entry the cut rolled back under `[Unreleased]` while
//! the rolled copy stays in its release. Every line of the result is one someone wrote, so no
//! conflict marker and no gate of `ship release` reads it as wrong, and the next release ships
//! the entry a second time. The same holds for one entry landed twice under `[Unreleased]`.
//!
//! An entry is a top-level `- ` line and every indented line after it, the blank lines between
//! them included; it ends at the first line that is neither blank nor indented. Only a whole
//! entry that matches word for word is a repeat: a follow-up that rewrites one opens with the
//! same bold line and says more. Repeats among released sections are shipped history, which the
//! rule leaves alone.

use std::collections::BTreeMap;

use super::{Finding, Rule, Tree};

pub(super) const CHANGELOG_REPEAT: Rule = Rule {
    id: "changelog-repeat",
    why: "`ship release` ships `CHANGELOG.md`'s `[Unreleased]` verbatim as the release's notes \
          (the Software Update window, the GitHub release, the update manifest): an entry \
          there that a released `## [X.Y.Z]` section already holds, which a merge across a \
          release cut brings back, or one `[Unreleased]` holds twice, would ship again",
    scope: &[],
    check: changelog_repeat,
};

/// The release notes' one hand-written source, root-relative.
const CHANGELOG: &str = "CHANGELOG.md";

fn changelog_repeat(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    if let Some(text) = tree.text(CHANGELOG) {
        repeats(&text, out);
    }
}

/// Which section of the changelog an entry sits in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section<'a> {
    /// `## [Unreleased]`: what the next release ships.
    Unreleased,
    /// `## [X.Y.Z] …`: a release that shipped, by its version.
    Released(&'a str),
    /// Above every `## ` heading, or under one that names neither (`## Earlier releases`).
    Other,
}

/// The section a `## ` heading opens.
fn section_of(heading: &str) -> Section<'_> {
    let Some((name, after)) = heading
        .strip_prefix("## [")
        .and_then(|rest| rest.split_once(']'))
    else {
        return Section::Other;
    };
    if !(after.trim().is_empty() || after.starts_with([' ', '\t', '-'])) {
        return Section::Other;
    }
    if name == "Unreleased" {
        return Section::Unreleased;
    }
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    {
        Section::Released(name)
    } else {
        Section::Other
    }
}

/// One top-level entry: its section, its 1-based first line and its whole text.
struct Entry<'a> {
    section: Section<'a>,
    line: usize,
    text: String,
}

/// Every top-level entry of `changelog`, in file order.
fn entries(changelog: &str) -> Vec<Entry<'_>> {
    let lines: Vec<&str> = changelog.lines().collect();
    let mut out = Vec::new();
    let mut section = Section::Other;
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        if line.starts_with("## ") {
            section = section_of(line);
            at += 1;
            continue;
        }
        if !line.starts_with("- ") {
            at += 1;
            continue;
        }
        // `end` is one past the entry's last line: a blank line joins it only once an indented
        // line follows.
        let mut end = at + 1;
        let mut next = end;
        while let Some(more) = lines.get(next) {
            if more.trim().is_empty() {
                next += 1;
            } else if more.starts_with([' ', '\t']) {
                next += 1;
                end = next;
            } else {
                break;
            }
        }
        out.push(Entry {
            section,
            line: at + 1,
            text: lines[at..end].join("\n"),
        });
        at = end;
    }
    out
}

/// One finding per `[Unreleased]` entry of `changelog` whose whole text a released section's
/// entry has (the first in the file, the newest release), else an earlier `[Unreleased]` one.
fn repeats(changelog: &str, out: &mut Vec<Finding>) {
    let entries = entries(changelog);
    let mut shipped: BTreeMap<&str, (&str, usize)> = BTreeMap::new();
    for entry in &entries {
        if let Section::Released(version) = entry.section {
            shipped
                .entry(entry.text.as_str())
                .or_insert((version, entry.line));
        }
    }
    let mut unreleased: BTreeMap<&str, usize> = BTreeMap::new();
    for entry in entries.iter().filter(|e| e.section == Section::Unreleased) {
        let detail = if let Some(&(version, line)) = shipped.get(entry.text.as_str()) {
            format!(
                "this entry is word for word the one [{version}] shipped at {CHANGELOG}:{line}, so \
                 the next release would ship it again: delete this copy from [Unreleased]"
            )
        } else if let Some(&line) = unreleased.get(entry.text.as_str()) {
            format!(
                "this entry is word for word the [Unreleased] one at {CHANGELOG}:{line}, so the \
                 next release would ship it twice: delete this copy from [Unreleased]"
            )
        } else {
            unreleased.insert(entry.text.as_str(), entry.line);
            continue;
        };
        out.push(Finding {
            rule: CHANGELOG_REPEAT.id,
            path: CHANGELOG.to_string(),
            line: entry.line,
            detail,
        });
    }
}

#[cfg(test)]
mod tests {
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
            detail.contains("the one [0.2.0] shipped at CHANGELOG.md:18")
                && detail.contains("delete this copy from [Unreleased]"),
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
                .contains("the one [0.2.0] shipped at CHANGELOG.md:24"),
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
}
