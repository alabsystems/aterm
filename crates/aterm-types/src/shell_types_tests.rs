// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for shell integration types (CommandMark, timestamps).

use super::*;

// =========================================================================
// CommandMark tests
// =========================================================================

fn make_mark(prompt: u64, col: u16) -> CommandMark {
    CommandMark {
        prompt_start_row: prompt,
        prompt_start_col: col,
        command_start_row: None,
        command_start_col: None,
        output_start_row: None,
        output_end_row: None,
        exit_code: None,
        working_directory: None,
        commandline: None,
        prompt_time_ms: None,
        command_input_start_time_ms: None,
        command_exec_start_time_ms: None,
        command_end_time_ms: None,
    }
}

#[test]
fn command_mark_is_complete_requires_exit_code() {
    let mut mark = make_mark(0, 0);
    assert!(!mark.is_complete());
    mark.exit_code = Some(0);
    assert!(mark.is_complete());
}

#[test]
fn command_mark_succeeded_only_on_zero() {
    let mut mark = make_mark(0, 0);
    assert!(!mark.succeeded());
    mark.exit_code = Some(0);
    assert!(mark.succeeded());
    mark.exit_code = Some(1);
    assert!(!mark.succeeded());
}

// =========================================================================
// current_time_ms
// =========================================================================

#[test]
fn current_time_ms_returns_some() {
    let ts = current_time_ms();
    assert!(ts.is_some());
    // Should be a reasonable value (after 2020-01-01)
    assert!(ts.unwrap() > 1_577_836_800_000);
}
