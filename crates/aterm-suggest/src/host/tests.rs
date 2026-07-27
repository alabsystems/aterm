// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the host adapter.
//!
//! `line_buffer` is where a wrong answer is cheapest to produce and hardest to
//! notice: it turns grid rows plus two offsets into "what the user typed", and
//! every caller downstream trusts it. These pin the prompt boundary, the
//! rejoining of a wrapped line, the cursor cut, grid padding, and the
//! character-vs-byte contract that a non-ASCII prompt would otherwise panic on.

use super::*;

#[test]
fn buffer_starts_after_the_prompt() {
    // "~/repo $ cargo bu" with the command starting at column 9.
    let rows = ["~/repo $ cargo bu"];
    assert_eq!(line_buffer(&rows, 9, (0, 17)), "cargo bu");
}

#[test]
fn a_wrapped_line_is_rejoined_without_the_visual_break() {
    let rows = ["$ cargo test --workspace --all-fea", "tures --no-fail-fast"];
    assert_eq!(
        line_buffer(&rows, 2, (1, 20)),
        "cargo test --workspace --all-features --no-fail-fast"
    );
}

#[test]
fn text_after_the_cursor_is_excluded() {
    // The user moved back into the line: cursor sits after "cargo b".
    let rows = ["$ cargo build --release"];
    assert_eq!(line_buffer(&rows, 2, (0, 9)), "cargo b");
}

#[test]
fn grid_padding_is_trimmed() {
    let rows = ["$ ls -la          "];
    assert_eq!(line_buffer(&rows, 2, (0, 18)), "ls -la");
}

#[test]
fn non_ascii_prompts_do_not_panic_and_slice_by_character() {
    // A powerline prompt with multi-byte glyphs before the command.
    let rows = ["❯ ⎇ main ❯ git st"];
    let start = "❯ ⎇ main ❯ ".chars().count();
    let end = rows[0].chars().count();
    assert_eq!(line_buffer(&rows, start, (0, end)), "git st");
}

#[test]
fn an_empty_or_out_of_range_region_is_empty() {
    assert_eq!(line_buffer(&[], 0, (0, 0)), "");
    assert_eq!(line_buffer(&["$ x"], 2, (5, 0)), "");
}

#[test]
fn cursor_at_the_command_start_yields_an_empty_buffer() {
    let rows = ["$ "];
    assert_eq!(line_buffer(&rows, 2, (0, 2)), "");
}

#[test]
fn record_block_feeds_the_corpus() {
    use crate::{Engine, SuggestConfig, SuggestMode};
    let mut e = Engine::new(SuggestConfig {
        mode: SuggestMode::History,
        ..SuggestConfig::default()
    });
    e.record_block(&BlockRecord {
        command: "cargo build --release",
        cwd: Some("/repo"),
        exit_code: Some(0),
        at_ms: 1_000,
    });
    assert_eq!(e.len(), 1);
}
