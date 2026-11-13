// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

#[test]
fn clean_docs_setext_headings_and_fenced_examples_pass() {
    let found = run(
        &CONFLICT_MARKER,
        &[
            (
                "CHANGELOG.md",
                "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- **Clean entry.**\n",
            ),
            (
                "docs/GUIDE.md",
                "Summary\n=======\n\nExample merge conflict:\n\n```diff\n<<<<<<< HEAD\na\n=======\nb\n>>>>>>> main\n```\n",
            ),
            (
                "crates/a/src/tests.rs",
                "const FIXTURE: &str = \"<<<<<<< HEAD\\n=======\\n>>>>>>> main\\n\";\n",
            ),
            ("vendor/sub/README.md", ">>>>>>> origin/main\n"),
        ],
    );
    assert_eq!(spots(&found), Vec::<String>::new(), "{found:?}");
}

#[test]
fn leftover_conflict_markers_in_changelog_docs_and_manifests_fail() {
    let found = run(
        &CONFLICT_MARKER,
        &[
            (
                "CHANGELOG.md",
                "# Changelog\n\n## [Unreleased]\n\n- entry\n\n>>>>>>> origin/main\n",
            ),
            (
                "docs/DESIGN.md",
                "# Design\n\n<<<<<<< HEAD\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> topic\n",
            ),
            ("crates/a/Cargo.toml", "[package]\n=======\nname = \"a\"\n"),
        ],
    );
    assert_eq!(
        spots(&found),
        vec![
            "CHANGELOG.md:7",
            "crates/a/Cargo.toml:2",
            "docs/DESIGN.md:3",
            "docs/DESIGN.md:5",
            "docs/DESIGN.md:7",
            "docs/DESIGN.md:9",
        ],
        "{found:?}"
    );
}

/// Readers take the paths a batch at a time: every file of a tree many batches wide is
/// read, and each marker found once, in its file's place.
#[test]
fn every_file_of_many_batches_is_read() {
    let files: Vec<(String, &str)> = (0..5 * BATCH)
        .map(|i| {
            let body = if i % 3 == 0 {
                "# N\n\n<<<<<<< HEAD\n"
            } else {
                "# N\n"
            };
            (format!("docs/n{i:04}.md"), body)
        })
        .collect();
    let files: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), *b)).collect();
    let expected: Vec<String> = (0..5 * BATCH)
        .step_by(3)
        .map(|i| format!("docs/n{i:04}.md:3"))
        .collect();
    assert_eq!(spots(&run(&CONFLICT_MARKER, &files)), expected);
}
