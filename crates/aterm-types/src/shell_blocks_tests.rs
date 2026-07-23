// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for block-based output model.

use super::*;

fn make_block(id: u64, prompt_row: u64) -> OutputBlock {
    OutputBlock {
        id,
        state: BlockState::PromptOnly,
        prompt_start_row: prompt_row,
        prompt_start_col: 0,
        command_start_row: None,
        command_start_col: None,
        output_start_row: None,
        end_row: None,
        exit_code: None,
        working_directory: None,
        commandline: None,
        collapsed: false,
        prompt_time_ms: None,
        command_input_start_time_ms: None,
        command_exec_start_time_ms: None,
        command_end_time_ms: None,
    }
}

#[test]
fn block_is_complete_requires_complete_state() {
    let mut block = make_block(0, 0);
    assert!(!block.is_complete());
    block.state = BlockState::Executing;
    assert!(!block.is_complete());
    block.state = BlockState::Complete;
    assert!(block.is_complete());
}

#[test]
fn block_succeeded_requires_exit_code_zero() {
    let mut block = make_block(0, 0);
    assert!(!block.succeeded());
    block.exit_code = Some(0);
    assert!(block.succeeded());
    block.exit_code = Some(1);
    assert!(!block.succeeded());
}

#[test]
fn block_failed_requires_nonzero_exit_code() {
    let mut block = make_block(0, 0);
    assert!(!block.failed());
    block.exit_code = Some(0);
    assert!(!block.failed());
    block.exit_code = Some(1);
    assert!(block.failed());
    block.exit_code = Some(-1);
    assert!(block.failed());
}

#[test]
fn block_exec_duration_ms_normal() {
    let mut block = make_block(0, 0);
    block.command_exec_start_time_ms = Some(1000);
    block.command_end_time_ms = Some(4500);
    assert_eq!(block.exec_duration_ms(), Some(3500));
}

#[test]
fn block_exec_duration_ms_zero_elapsed() {
    let mut block = make_block(0, 0);
    block.command_exec_start_time_ms = Some(5000);
    block.command_end_time_ms = Some(5000);
    assert_eq!(block.exec_duration_ms(), Some(0));
}

#[test]
fn block_exec_duration_ms_none_when_start_missing() {
    let mut block = make_block(0, 0);
    block.command_end_time_ms = Some(2500);
    assert_eq!(block.exec_duration_ms(), None);
}

#[test]
fn block_exec_duration_ms_none_when_end_missing() {
    let mut block = make_block(0, 0);
    block.command_exec_start_time_ms = Some(1000);
    assert_eq!(block.exec_duration_ms(), None);
}

#[test]
fn block_exec_duration_ms_none_when_both_missing() {
    let block = make_block(0, 0);
    assert_eq!(block.exec_duration_ms(), None);
}

#[test]
fn block_exec_duration_ms_none_when_end_before_start() {
    let mut block = make_block(0, 0);
    block.command_exec_start_time_ms = Some(3000);
    block.command_end_time_ms = Some(1000);
    assert_eq!(block.exec_duration_ms(), None);
}

#[test]
fn block_command_duration_ms_returns_prompt_to_end() {
    let mut block = make_block(0, 0);
    block.prompt_time_ms = Some(1000);
    block.command_end_time_ms = Some(5000);
    assert_eq!(block.command_duration_ms(), Some(4000));
}

#[test]
fn block_command_duration_ms_none_when_prompt_missing() {
    let mut block = make_block(0, 0);
    block.command_end_time_ms = Some(5000);
    assert_eq!(block.command_duration_ms(), None);
}

#[test]
fn block_command_duration_ms_none_when_end_before_prompt() {
    let mut block = make_block(0, 0);
    block.prompt_time_ms = Some(5000);
    block.command_end_time_ms = Some(1000);
    assert_eq!(block.command_duration_ms(), None);
}

/// Verify all 4 phase durations are consistent for OutputBlock (#5705).
#[test]
fn block_four_phase_timestamp_consistency() {
    let mut block = make_block(0, 0);
    block.prompt_time_ms = Some(1000);
    block.command_input_start_time_ms = Some(1500);
    block.command_exec_start_time_ms = Some(3000);
    block.command_end_time_ms = Some(5000);

    assert_eq!(block.exec_duration_ms(), Some(2000)); // C→D
    assert_eq!(block.command_duration_ms(), Some(4000)); // A→D (total)

    // Total > exec-only because it includes prompt + input phases
    assert!(block.command_duration_ms().unwrap() > block.exec_duration_ms().unwrap());
}

#[test]
fn prompt_rows_defaults_to_single_row() {
    let block = make_block(0, 10);
    assert_eq!(block.prompt_row_span(), RowSpan::new(10, 11));
}

#[test]
fn prompt_rows_ends_at_command_start() {
    let mut block = make_block(0, 5);
    block.command_start_row = Some(8);
    assert_eq!(block.prompt_row_span(), RowSpan::new(5, 8));
}

#[test]
fn prompt_rows_ends_at_output_start_when_no_command() {
    let mut block = make_block(0, 5);
    block.output_start_row = Some(7);
    assert_eq!(block.prompt_row_span(), RowSpan::new(5, 7));
}

#[test]
fn prompt_rows_ends_at_end_row_when_no_command_or_output() {
    let mut block = make_block(0, 5);
    block.end_row = Some(6);
    assert_eq!(block.prompt_row_span(), RowSpan::new(5, 6));
}

#[test]
fn command_rows_none_when_no_command() {
    let block = make_block(0, 0);
    assert_eq!(block.command_row_span(), None);
}

#[test]
fn command_rows_ends_at_output_start() {
    let mut block = make_block(0, 0);
    block.command_start_row = Some(2);
    block.output_start_row = Some(5);
    assert_eq!(block.command_row_span(), Some(RowSpan::new(2, 5)));
}

#[test]
fn command_rows_ends_at_end_row_when_no_output() {
    let mut block = make_block(0, 0);
    block.command_start_row = Some(2);
    block.end_row = Some(4);
    assert_eq!(block.command_row_span(), Some(RowSpan::new(2, 4)));
}

#[test]
fn command_rows_defaults_to_single_row() {
    let mut block = make_block(0, 0);
    block.command_start_row = Some(2);
    assert_eq!(block.command_row_span(), Some(RowSpan::new(2, 3)));
}

#[test]
fn output_rows_none_when_no_output() {
    let block = make_block(0, 0);
    assert_eq!(block.output_row_span(), None);
}

#[test]
fn output_rows_uses_end_row() {
    let mut block = make_block(0, 0);
    block.output_start_row = Some(10);
    block.end_row = Some(20);
    assert_eq!(block.output_row_span(), Some(RowSpan::new(10, 20)));
}

#[test]
fn output_rows_defaults_to_single_row() {
    let mut block = make_block(0, 0);
    block.output_start_row = Some(10);
    assert_eq!(block.output_row_span(), Some(RowSpan::new(10, 11)));
}

#[test]
fn tuple_row_helpers_remain_available_as_compatibility_shims() {
    let mut block = make_block(0, 0);
    block.command_start_row = Some(2);
    block.output_start_row = Some(5);
    block.end_row = Some(8);
    assert_eq!(block.prompt_rows(), (0, 2));
    assert_eq!(block.command_rows(), Some((2, 5)));
    assert_eq!(block.output_rows(), Some((5, 8)));
}

#[test]
fn row_span_helpers_preserve_count_and_tuple_shape() {
    let rows = RowSpan::new(4, 9);
    assert_eq!(rows.row_count(), 5);
    assert_eq!(rows.as_tuple(), (4, 9));
}

#[test]
fn contains_row_in_progress_block() {
    let mut block = make_block(0, 5);
    assert!(!block.contains_row(4));
    assert!(block.contains_row(5));
    assert!(block.contains_row(100));

    block.end_row = Some(10);
    assert!(block.contains_row(5));
    assert!(block.contains_row(9));
    assert!(!block.contains_row(10));
}

#[test]
fn is_row_visible_when_not_collapsed() {
    let mut block = make_block(0, 5);
    block.output_start_row = Some(8);
    block.end_row = Some(12);
    assert!(block.is_row_visible(5));
    assert!(block.is_row_visible(8));
    assert!(block.is_row_visible(11));
}

#[test]
fn is_row_visible_collapsed_hides_output() {
    let mut block = make_block(0, 5);
    block.output_start_row = Some(8);
    block.end_row = Some(12);
    block.collapsed = true;

    assert!(block.is_row_visible(5));
    assert!(block.is_row_visible(7));
    assert!(!block.is_row_visible(8));
    assert!(!block.is_row_visible(11));
    assert!(block.is_row_visible(4));
    assert!(block.is_row_visible(12));
}

#[test]
fn is_row_visible_collapsed_no_output_yet() {
    let mut block = make_block(0, 5);
    block.collapsed = true;
    assert!(block.is_row_visible(5));
    assert!(block.is_row_visible(100));
}

#[test]
fn visible_row_count_uncollapsed_complete_block() {
    let mut block = make_block(0, 0);
    block.command_start_row = Some(1);
    block.output_start_row = Some(2);
    block.end_row = Some(10);
    assert_eq!(block.visible_row_count(), 10);
}

#[test]
fn visible_row_count_collapsed_excludes_output() {
    let mut block = make_block(0, 0);
    block.command_start_row = Some(1);
    block.output_start_row = Some(2);
    block.end_row = Some(10);
    block.collapsed = true;
    assert_eq!(block.visible_row_count(), 2);
}

#[test]
fn visible_row_count_prompt_only() {
    let block = make_block(0, 5);
    assert_eq!(block.visible_row_count(), 1);
}

#[test]
fn hidden_row_count_zero_when_not_collapsed() {
    let mut block = make_block(0, 0);
    block.output_start_row = Some(2);
    block.end_row = Some(10);
    assert_eq!(block.hidden_row_count(), 0);
}

#[test]
fn hidden_row_count_counts_output_rows_when_collapsed() {
    let mut block = make_block(0, 0);
    block.output_start_row = Some(2);
    block.end_row = Some(10);
    block.collapsed = true;
    assert_eq!(block.hidden_row_count(), 8);
}

#[test]
fn hidden_row_count_zero_when_collapsed_but_no_output() {
    let mut block = make_block(0, 0);
    block.collapsed = true;
    assert_eq!(block.hidden_row_count(), 0);
}

// ========================================================================
// Regression: u64::MAX overflow (#5715)
//
// Before the fix, `start + 1` wrapped to 0 when start == u64::MAX,
// creating an inverted RowSpan where start > end.
// ========================================================================

#[test]
fn prompt_row_span_saturates_at_u64_max() {
    let block = make_block(0, u64::MAX);
    let span = block.prompt_row_span();
    assert!(
        span.start_row <= span.end_row_exclusive,
        "prompt_row_span must not wrap: start={}, end={}",
        span.start_row,
        span.end_row_exclusive
    );
    assert_eq!(span.row_count(), 0);
}

#[test]
fn command_row_span_saturates_at_u64_max() {
    let mut block = make_block(0, 0);
    block.command_start_row = Some(u64::MAX);
    let span = block.command_row_span().expect("should have command span");
    assert!(
        span.start_row <= span.end_row_exclusive,
        "command_row_span must not wrap: start={}, end={}",
        span.start_row,
        span.end_row_exclusive
    );
    assert_eq!(span.row_count(), 0);
}

#[test]
fn output_row_span_saturates_at_u64_max() {
    let mut block = make_block(0, 0);
    block.output_start_row = Some(u64::MAX);
    let span = block.output_row_span().expect("should have output span");
    assert!(
        span.start_row <= span.end_row_exclusive,
        "output_row_span must not wrap: start={}, end={}",
        span.start_row,
        span.end_row_exclusive
    );
    assert_eq!(span.row_count(), 0);
}

#[test]
fn visible_row_count_saturates_at_u64_max() {
    let block = make_block(0, u64::MAX);
    // Should not panic or wrap — returns 1 (prompt-only, single row clamped)
    let count = block.visible_row_count();
    assert!(
        count <= 1,
        "visible_row_count at u64::MAX should be 0 or 1, got {count}"
    );
}
