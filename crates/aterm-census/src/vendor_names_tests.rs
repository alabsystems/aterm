// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn a_vendor_word_counts_only_as_a_whole_word_in_any_case() {
    assert!(names_a_vendor("let c = Claude::new();"));
    assert!(names_a_vendor("// the CODEX home"));
    assert!(names_a_vendor("\"grok-alab\""));
    assert!(!names_a_vendor("claude_code and agyness"));
    assert!(!names_a_vendor("museum"));
}

#[test]
fn test_fixture_and_bench_paths_are_not_counted() {
    assert!(counted("aterm-agent/src/harness/cli.rs"));
    assert!(
        counted("aterm-census/src/vendor_names.rs"),
        "the census counts its own source"
    );
    for rel in [
        "aterm-agent/src/harness/cli_tests.rs",
        "aterm/tests/live.rs",
        "aterm-phase/src/fixtures/claude.rs",
        "aterm-render/benches/b.rs",
        "aterm-x/testdata/a.rs",
        "aterm-agent/SPECS.md",
        "aterm-census/src/vendor_words.txt",
    ] {
        assert!(!counted(rel), "{rel}");
    }
}

/// The census's own source names no vendor, so it counts what §1's command counts, and the word
/// list it reads is the ten words that command greps for.
#[test]
fn the_census_source_names_no_vendor_and_reads_the_ten_words() {
    let own = include_str!("vendor_names.rs");
    assert_eq!(own.lines().filter(|l| names_a_vendor(l)).count(), 0);
    assert_eq!(
        vendor_words().collect::<Vec<_>>(),
        [
            "claude",
            "codex",
            "agy",
            "muse",
            "kimi",
            "grok",
            "gemini",
            "anthropic",
            "openai",
            "antigravity"
        ]
    );
}
