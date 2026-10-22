// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for shell integration types (CommandMark, timestamps).

use super::*;

// CommandMark tests

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
fn command_mark_completes_on_an_exit_code_and_succeeds_only_on_zero() {
    let mut mark = make_mark(0, 0);
    assert!(!mark.is_complete());
    assert!(!mark.succeeded());
    mark.exit_code = Some(0);
    assert!(mark.is_complete());
    assert!(mark.succeeded());
    mark.exit_code = Some(1);
    assert!(!mark.succeeded());
}
