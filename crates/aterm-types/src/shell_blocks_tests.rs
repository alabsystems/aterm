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
fn row_span_helpers_preserve_count_and_tuple_shape() {
    let rows = RowSpan::new(4, 9);
    assert_eq!(rows.row_count(), 5);
    assert_eq!(rows.as_tuple(), (4, 9));
}

// ========================================================================
// Regression: u64::MAX overflow (#5715)
//
// Before the fix, `start + 1` wrapped to 0 when start == u64::MAX,
// creating an inverted RowSpan where start > end.
// ========================================================================

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
