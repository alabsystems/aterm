// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for shell integration types (CommandMark, ShellEvent, timestamps).

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

#[test]
fn prompt_duration_ms_normal() {
    let mut mark = make_mark(0, 0);
    mark.prompt_time_ms = Some(1000);
    mark.command_input_start_time_ms = Some(1500);
    assert_eq!(mark.prompt_duration_ms(), Some(500));
}

#[test]
fn prompt_duration_ms_zero_for_instant() {
    let mut mark = make_mark(0, 0);
    mark.prompt_time_ms = Some(5000);
    mark.command_input_start_time_ms = Some(5000);
    assert_eq!(mark.prompt_duration_ms(), Some(0));
}

#[test]
fn prompt_duration_ms_none_when_both_missing() {
    let mark = make_mark(0, 0);
    assert_eq!(mark.prompt_duration_ms(), None);
}

#[test]
fn prompt_duration_ms_none_when_start_missing() {
    let mut mark = make_mark(0, 0);
    mark.command_input_start_time_ms = Some(3500);
    assert_eq!(mark.prompt_duration_ms(), None);
}

#[test]
fn prompt_duration_ms_none_when_end_missing() {
    let mut mark = make_mark(0, 0);
    mark.prompt_time_ms = Some(1000);
    assert_eq!(mark.prompt_duration_ms(), None);
}

#[test]
fn prompt_duration_ms_none_when_end_before_start() {
    let mut mark = make_mark(0, 0);
    mark.prompt_time_ms = Some(2000);
    mark.command_input_start_time_ms = Some(1000);
    assert_eq!(mark.prompt_duration_ms(), None);
}

#[test]
fn input_duration_ms_normal() {
    let mut mark = make_mark(0, 0);
    mark.command_input_start_time_ms = Some(1000);
    mark.command_exec_start_time_ms = Some(3000);
    assert_eq!(mark.input_duration_ms(), Some(2000));
}

#[test]
fn input_duration_ms_none_when_end_missing() {
    let mut mark = make_mark(0, 0);
    mark.command_input_start_time_ms = Some(1000);
    assert_eq!(mark.input_duration_ms(), None);
}

#[test]
fn input_duration_ms_none_when_both_missing() {
    let mark = make_mark(0, 0);
    assert_eq!(mark.input_duration_ms(), None);
}

#[test]
fn input_duration_ms_none_when_end_before_start() {
    let mut mark = make_mark(0, 0);
    mark.command_input_start_time_ms = Some(8000);
    mark.command_exec_start_time_ms = Some(3000);
    assert_eq!(mark.input_duration_ms(), None);
}

#[test]
fn exec_duration_ms_normal() {
    let mut mark = make_mark(0, 0);
    mark.command_exec_start_time_ms = Some(5000);
    mark.command_end_time_ms = Some(8000);
    assert_eq!(mark.exec_duration_ms(), Some(3000));
}

#[test]
fn exec_duration_ms_zero_for_instant_command() {
    let mut mark = make_mark(0, 0);
    mark.command_exec_start_time_ms = Some(5000);
    mark.command_end_time_ms = Some(5000);
    assert_eq!(mark.exec_duration_ms(), Some(0));
}

#[test]
fn exec_duration_ms_none_when_both_missing() {
    let mark = make_mark(0, 0);
    assert_eq!(mark.exec_duration_ms(), None);
}

#[test]
fn exec_duration_ms_none_when_end_before_start() {
    let mut mark = make_mark(0, 0);
    mark.command_exec_start_time_ms = Some(5000);
    mark.command_end_time_ms = Some(4000);
    assert_eq!(mark.exec_duration_ms(), None);
}

#[test]
fn command_duration_ms_returns_prompt_to_end() {
    let mut mark = make_mark(0, 0);
    mark.prompt_time_ms = Some(1000);
    mark.command_end_time_ms = Some(5000);
    assert_eq!(mark.command_duration_ms(), Some(4000));
}

#[test]
fn command_duration_ms_none_when_prompt_missing() {
    let mut mark = make_mark(0, 0);
    mark.command_end_time_ms = Some(5000);
    assert_eq!(mark.command_duration_ms(), None);
}

#[test]
fn command_duration_ms_none_when_end_missing() {
    let mut mark = make_mark(0, 0);
    mark.prompt_time_ms = Some(1000);
    assert_eq!(mark.command_duration_ms(), None);
}

#[test]
fn command_duration_ms_none_when_end_before_prompt() {
    let mut mark = make_mark(0, 0);
    mark.prompt_time_ms = Some(5000);
    mark.command_end_time_ms = Some(1000);
    assert_eq!(mark.command_duration_ms(), None);
}

/// Verify all 4 phase durations are consistent and non-overlapping (#5705).
#[test]
fn four_phase_timestamp_consistency() {
    let mut mark = make_mark(0, 0);
    mark.prompt_time_ms = Some(1000);
    mark.command_input_start_time_ms = Some(1500);
    mark.command_exec_start_time_ms = Some(3000);
    mark.command_end_time_ms = Some(5000);

    assert_eq!(mark.prompt_duration_ms(), Some(500)); // A→B
    assert_eq!(mark.input_duration_ms(), Some(1500)); // B→C
    assert_eq!(mark.exec_duration_ms(), Some(2000)); // C→D
    assert_eq!(mark.command_duration_ms(), Some(4000)); // A→D (total)

    // Total equals sum of phases
    let total = mark.prompt_duration_ms().unwrap()
        + mark.input_duration_ms().unwrap()
        + mark.exec_duration_ms().unwrap();
    assert_eq!(mark.command_duration_ms().unwrap(), total);
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
