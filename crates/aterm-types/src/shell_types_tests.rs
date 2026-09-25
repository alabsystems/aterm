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

/// A mark carrying the four OSC 133 phase timestamps `[A, B, C, D]`: prompt
/// shown, input started, execution started, command ended.
fn mark_at([a, b, c, d]: [Option<u64>; 4]) -> CommandMark {
    let mut mark = make_mark(0, 0);
    mark.prompt_time_ms = a;
    mark.command_input_start_time_ms = b;
    mark.command_exec_start_time_ms = c;
    mark.command_end_time_ms = d;
    mark
}

/// Every phase duration is `end - start` over ITS OWN pair of timestamps, and
/// `None` when either is missing or they are inverted. Each row names the
/// phase, the four timestamps and the expected duration.
#[test]
#[rustfmt::skip]
fn command_mark_phase_durations() {
    type Phase = fn(&CommandMark) -> Option<u64>;
    let prompt: Phase = CommandMark::prompt_duration_ms; // A→B
    let input: Phase = CommandMark::input_duration_ms; // B→C
    let exec: Phase = CommandMark::exec_duration_ms; // C→D
    let command: Phase = CommandMark::command_duration_ms; // A→D
    for (what, phase, times, want) in [
        ("prompt normal", prompt, [Some(1000), Some(1500), None, None], Some(500)),
        ("prompt instant", prompt, [Some(5000), Some(5000), None, None], Some(0)),
        ("prompt both missing", prompt, [None, None, None, None], None),
        ("prompt start missing", prompt, [None, Some(3500), None, None], None),
        ("prompt end missing", prompt, [Some(1000), None, None, None], None),
        ("prompt inverted", prompt, [Some(2000), Some(1000), None, None], None),
        ("input normal", input, [None, Some(1000), Some(3000), None], Some(2000)),
        ("input end missing", input, [None, Some(1000), None, None], None),
        ("input both missing", input, [None, None, None, None], None),
        ("input inverted", input, [None, Some(8000), Some(3000), None], None),
        ("exec normal", exec, [None, None, Some(5000), Some(8000)], Some(3000)),
        ("exec instant", exec, [None, None, Some(5000), Some(5000)], Some(0)),
        ("exec both missing", exec, [None, None, None, None], None),
        ("exec inverted", exec, [None, None, Some(5000), Some(4000)], None),
        ("command normal", command, [Some(1000), None, None, Some(5000)], Some(4000)),
        ("command prompt missing", command, [None, None, None, Some(5000)], None),
        ("command end missing", command, [Some(1000), None, None, None], None),
        ("command inverted", command, [Some(5000), None, None, Some(1000)], None),
    ] {
        assert_eq!(phase(&mark_at(times)), want, "{what}: {times:?}");
    }
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
