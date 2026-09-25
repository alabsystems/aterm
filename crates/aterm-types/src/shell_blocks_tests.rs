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

/// A block carrying the four OSC 133 phase timestamps `[A, B, C, D]`: prompt
/// shown, input started, execution started, command ended.
fn block_at([a, b, c, d]: [Option<u64>; 4]) -> OutputBlock {
    let mut block = make_block(0, 0);
    block.prompt_time_ms = a;
    block.command_input_start_time_ms = b;
    block.command_exec_start_time_ms = c;
    block.command_end_time_ms = d;
    block
}

/// Each phase duration is `end - start` over its own pair of timestamps, and
/// `None` when either is missing or they are inverted.
#[test]
#[rustfmt::skip]
fn block_phase_durations() {
    type Phase = fn(&OutputBlock) -> Option<u64>;
    let exec: Phase = OutputBlock::exec_duration_ms; // C→D
    let command: Phase = OutputBlock::command_duration_ms; // A→D
    for (what, phase, times, want) in [
        ("exec normal", exec, [None, None, Some(1000), Some(4500)], Some(3500)),
        ("exec zero elapsed", exec, [None, None, Some(5000), Some(5000)], Some(0)),
        ("exec start missing", exec, [None, None, None, Some(2500)], None),
        ("exec end missing", exec, [None, None, Some(1000), None], None),
        ("exec both missing", exec, [None, None, None, None], None),
        ("exec inverted", exec, [None, None, Some(3000), Some(1000)], None),
        ("command normal", command, [Some(1000), None, None, Some(5000)], Some(4000)),
        ("command prompt missing", command, [None, None, None, Some(5000)], None),
        ("command inverted", command, [Some(5000), None, None, Some(1000)], None),
    ] {
        assert_eq!(phase(&block_at(times)), want, "{what}: {times:?}");
    }
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

/// A block whose prompt starts at `prompt` with the given command, output and
/// end rows.
fn block_rows(
    prompt: u64,
    command: Option<u64>,
    output: Option<u64>,
    end: Option<u64>,
) -> OutputBlock {
    let mut block = make_block(0, prompt);
    block.command_start_row = command;
    block.output_start_row = output;
    block.end_row = end;
    block
}

/// Each section's span ends where the NEXT recorded section starts (falling back
/// to `end_row`), and is a single row when nothing after it is recorded yet.
#[test]
#[rustfmt::skip]
fn block_row_spans() {
    type Span = fn(&OutputBlock) -> Option<RowSpan>;
    let prompt: Span = |block| Some(block.prompt_row_span());
    let command: Span = OutputBlock::command_row_span;
    let output: Span = OutputBlock::output_row_span;
    for (what, span, block, want) in [
        ("prompt defaults to one row", prompt, block_rows(10, None, None, None), Some((10, 11))),
        ("prompt ends at command", prompt, block_rows(5, Some(8), None, None), Some((5, 8))),
        ("prompt ends at output", prompt, block_rows(5, None, Some(7), None), Some((5, 7))),
        ("prompt ends at end row", prompt, block_rows(5, None, None, Some(6)), Some((5, 6))),
        ("no command", command, block_rows(0, None, None, None), None),
        ("command ends at output", command, block_rows(0, Some(2), Some(5), None), Some((2, 5))),
        ("command ends at end row", command, block_rows(0, Some(2), None, Some(4)), Some((2, 4))),
        ("command defaults to one row", command, block_rows(0, Some(2), None, None), Some((2, 3))),
        ("no output", output, block_rows(0, None, None, None), None),
        ("output ends at end row", output, block_rows(0, None, Some(10), Some(20)), Some((10, 20))),
        ("output defaults to one row", output, block_rows(0, None, Some(10), None), Some((10, 11))),
    ] {
        let want = want.map(|(start, end)| RowSpan::new(start, end));
        assert_eq!(span(&block), want, "{what}");
    }
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

/// Collapsing hides exactly the output rows: `visible_row_count` drops them and
/// `hidden_row_count` counts them, and neither moves before output exists.
#[test]
#[rustfmt::skip]
fn block_row_counts_under_collapse() {
    let complete = block_rows(0, Some(1), Some(2), Some(10));
    let output_only = block_rows(0, None, Some(2), Some(10));
    for (what, block, collapsed, visible, hidden) in [
        ("uncollapsed complete block", complete.clone(), false, Some(10), None),
        ("collapsed excludes output", complete, true, Some(2), None),
        ("prompt only", block_rows(5, None, None, None), false, Some(1), None),
        ("hidden zero when not collapsed", output_only.clone(), false, None, Some(0)),
        ("hidden counts output rows", output_only, true, None, Some(8)),
        ("hidden zero without output", block_rows(0, None, None, None), true, None, Some(0)),
    ] {
        let mut block = block;
        block.collapsed = collapsed;
        if let Some(visible) = visible {
            assert_eq!(block.visible_row_count(), visible, "{what}");
        }
        if let Some(hidden) = hidden {
            assert_eq!(block.hidden_row_count(), hidden, "{what}");
        }
    }
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
