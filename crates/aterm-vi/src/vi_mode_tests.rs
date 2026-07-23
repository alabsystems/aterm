// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for [`ViMode`] state machine: toggle, motions, marks, search.

use super::test_utils::MockGrid;
use super::*;

#[test]
fn toggle_activates_at_cursor() {
    let mut vi = ViMode::new();
    assert!(!vi.is_active());

    vi.toggle(ViPoint::new(5, 10));
    assert!(vi.is_active());
    assert_eq!(vi.cursor_point(), ViPoint::new(5, 10));

    vi.toggle(ViPoint::new(0, 0));
    assert!(!vi.is_active());
}

#[test]
fn motion_ignored_when_inactive() {
    let mut vi = ViMode::new();
    vi.motion(24, 80, 0, 23, ViMotion::Down, ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::default());
}

#[test]
fn basic_motion_down() {
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(5, 10));
    vi.motion(24, 80, 0, 23, ViMotion::Down, ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::new(6, 10));
}

#[test]
fn mark_set_and_goto() {
    let grid = MockGrid::new(24, 80);
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(5, 10));

    assert!(vi.set_mark('a'));
    vi.motion(24, 80, 0, 23, ViMotion::Down, ViBoundary::Grid);
    vi.motion(24, 80, 0, 23, ViMotion::Down, ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::new(7, 10));

    vi.motion_with_grid(&grid, ViMotion::GotoMark('a'), ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::new(5, 10));
}

#[test]
fn first_occupied_finds_non_space() {
    let grid = MockGrid::new(24, 80).with_line(5, "   hello");
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(5, 20));

    vi.motion_with_grid(&grid, ViMotion::FirstOccupied, ViBoundary::Grid);
    assert_eq!(vi.cursor_point().col, 3);
}

#[test]
fn first_occupied_all_spaces_goes_to_zero() {
    let grid = MockGrid::new(24, 80);
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(5, 20));

    vi.motion_with_grid(&grid, ViMotion::FirstOccupied, ViBoundary::Grid);
    assert_eq!(vi.cursor_point().col, 0);
}

#[test]
fn first_occupied_double_tap_walks_back_through_wrapped_lines() {
    // Row 3: "  hello world that is very long..." (logical line start)
    // Row 4: "...continuation"   (wrapped from row 3)
    // Row 5: "  more cont"       (wrapped from row 4)
    let grid = MockGrid::new(24, 80)
        .with_line(3, "  hello world")
        .with_line(4, "continuation text")
        .with_wrapped(4)
        .with_line(5, "  more cont")
        .with_wrapped(5);

    let mut vi = ViMode::new();
    // Start cursor in the middle of row 5
    vi.toggle(ViPoint::new(5, 10));

    // First ^ → first non-blank on row 5 (col 2, 'm' of "more")
    vi.motion_with_grid(&grid, ViMotion::FirstOccupied, ViBoundary::Grid);
    assert_eq!(vi.cursor_point().line, 5);
    assert_eq!(vi.cursor_point().col, 2);

    // Second ^ (already at first non-blank on a wrapped row) →
    // walk back to logical line start (row 3) and find first non-blank (col 2)
    vi.motion_with_grid(&grid, ViMotion::FirstOccupied, ViBoundary::Grid);
    assert_eq!(vi.cursor_point().line, 3);
    assert_eq!(vi.cursor_point().col, 2);
}

#[test]
fn first_occupied_in_scrollback_finds_non_blank() {
    // `^` on a scrollback line must resolve first-non-blank from a single
    // `line_text` read of that row (first_non_blank_col scrollback path).
    let grid = MockGrid::new(2, 10).with_scrollback_lines(&["    hi"]); // line -1
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(-1, 8));

    vi.motion_with_grid(&grid, ViMotion::FirstOccupied, ViBoundary::Grid);
    assert_eq!(vi.cursor_point().line, -1);
    assert_eq!(vi.cursor_point().col, 4);
}

#[test]
fn first_occupied_no_double_tap_on_unwrapped_line() {
    let grid = MockGrid::new(24, 80).with_line(3, "  hello");

    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(3, 10));

    // First ^ → col 2
    vi.motion_with_grid(&grid, ViMotion::FirstOccupied, ViBoundary::Grid);
    assert_eq!(vi.cursor_point().col, 2);
    assert_eq!(vi.cursor_point().line, 3);

    // Second ^ on unwrapped line → stays at col 2, same line
    vi.motion_with_grid(&grid, ViMotion::FirstOccupied, ViBoundary::Grid);
    assert_eq!(vi.cursor_point().col, 2);
    assert_eq!(vi.cursor_point().line, 3);
}

#[test]
fn scroll_moves_cursor() {
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(10, 5));
    vi.scroll(0, 23, 5);
    assert_eq!(vi.cursor_point().line, 15);
}

#[test]
fn inline_search_stored() {
    let mut vi = ViMode::new();
    vi.set_inline_search(InlineSearchState {
        char: 'x',
        kind: InlineSearchKind::FindRight,
    });
    let s = vi.inline_search().unwrap();
    assert_eq!(s.char, 'x');
    assert_eq!(s.kind, InlineSearchKind::FindRight);
}

#[test]
fn word_motion_dispatches_through_vi_mode() {
    let grid = MockGrid::new(1, 30).with_line(0, "hello world foo");
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(0, 0));

    vi.motion_with_grid(&grid, ViMotion::SemanticRight, ViBoundary::Grid);
    assert_eq!(vi.cursor_point().col, 6); // 'w' of "world"

    vi.motion_with_grid(&grid, ViMotion::SemanticRight, ViBoundary::Grid);
    assert_eq!(vi.cursor_point().col, 12); // 'f' of "foo"
}

#[test]
fn bracket_motion_dispatches_through_vi_mode() {
    let grid = MockGrid::new(1, 20).with_line(0, "(hello)");
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(0, 0));

    vi.motion_with_grid(&grid, ViMotion::Bracket, ViBoundary::Grid);
    assert_eq!(vi.cursor_point().col, 6);

    vi.motion_with_grid(&grid, ViMotion::Bracket, ViBoundary::Grid);
    assert_eq!(vi.cursor_point().col, 0);
}

#[test]
fn paragraph_motion_dispatches_through_vi_mode() {
    let grid = MockGrid::new(5, 10)
        .with_line(0, "aaa")
        .with_line(1, "bbb")
        .with_line(3, "ccc")
        .with_line(4, "ddd");
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(0, 0));

    vi.motion_with_grid(&grid, ViMotion::ParagraphDown, ViBoundary::Grid);
    assert_eq!(vi.cursor_point().line, 2);
}

#[test]
fn inline_search_execute_moves_cursor() {
    let grid = MockGrid::new(1, 20).with_line(0, "hello world");
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(0, 0));

    assert!(vi.inline_search_execute(&grid, 'o', InlineSearchKind::FindRight));
    assert_eq!(vi.cursor_point().col, 4);
}

#[test]
fn inline_search_till_stops_before_char() {
    let grid = MockGrid::new(1, 20).with_line(0, "hello world");
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(0, 0));

    assert!(vi.inline_search_execute(&grid, 'o', InlineSearchKind::TillRight));
    assert_eq!(vi.cursor_point().col, 3); // one before 'o'
}

#[test]
fn inline_search_repeat_and_reverse() {
    let grid = MockGrid::new(1, 20).with_line(0, "abcabc");
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(0, 0));

    // Find first 'c'
    assert!(vi.inline_search_execute(&grid, 'c', InlineSearchKind::FindRight));
    assert_eq!(vi.cursor_point().col, 2);

    // Repeat: find next 'c'
    assert!(vi.inline_search_repeat(&grid));
    assert_eq!(vi.cursor_point().col, 5);

    // Reverse: find previous 'c'
    assert!(vi.inline_search_repeat_reverse(&grid));
    assert_eq!(vi.cursor_point().col, 2);
}

// -----------------------------------------------------------------------
// Search motion (n/N) integration tests
// -----------------------------------------------------------------------

#[test]
fn search_next_navigates_to_match() {
    let grid = MockGrid::new(5, 20);
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(0, 0));

    vi.search_mut()
        .set_matches(vec![ViPoint::new(1, 5), ViPoint::new(3, 10)]);

    vi.motion_with_grid(&grid, ViMotion::SearchNext, ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::new(1, 5));

    vi.motion_with_grid(&grid, ViMotion::SearchNext, ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::new(3, 10));
}

#[test]
fn search_next_wraps_to_first() {
    let grid = MockGrid::new(5, 20);
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(4, 0));

    vi.search_mut()
        .set_matches(vec![ViPoint::new(1, 5), ViPoint::new(3, 10)]);

    // Cursor past all matches: wraps to first.
    vi.motion_with_grid(&grid, ViMotion::SearchNext, ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::new(1, 5));
}

#[test]
fn search_prev_navigates_backward() {
    let grid = MockGrid::new(5, 20);
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(4, 0));

    vi.search_mut()
        .set_matches(vec![ViPoint::new(1, 5), ViPoint::new(3, 10)]);

    vi.motion_with_grid(&grid, ViMotion::SearchPrevious, ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::new(3, 10));

    vi.motion_with_grid(&grid, ViMotion::SearchPrevious, ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::new(1, 5));
}

#[test]
fn search_prev_wraps_to_last() {
    let grid = MockGrid::new(5, 20);
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(0, 0));

    vi.search_mut()
        .set_matches(vec![ViPoint::new(1, 5), ViPoint::new(3, 10)]);

    // Cursor before all matches: wraps to last.
    vi.motion_with_grid(&grid, ViMotion::SearchPrevious, ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::new(3, 10));
}

#[test]
fn search_no_op_with_empty_matches() {
    let grid = MockGrid::new(5, 20);
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(2, 5));

    // No matches set: cursor stays put.
    vi.motion_with_grid(&grid, ViMotion::SearchNext, ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::new(2, 5));

    vi.motion_with_grid(&grid, ViMotion::SearchPrevious, ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::new(2, 5));
}

#[test]
fn search_no_op_when_inactive() {
    let grid = MockGrid::new(5, 20);
    let mut vi = ViMode::new();
    // Not toggled on.

    vi.search_mut().set_matches(vec![ViPoint::new(1, 5)]);
    vi.motion_with_grid(&grid, ViMotion::SearchNext, ViBoundary::Grid);
    assert_eq!(vi.cursor_point(), ViPoint::default());
}

#[test]
fn search_accessor_exposes_state() {
    let mut vi = ViMode::new();
    assert!(vi.search().is_empty());

    vi.search_mut().set_matches(vec![ViPoint::new(0, 0)]);
    assert_eq!(vi.search().match_count(), 1);
    assert!(!vi.search().is_empty());
}

/// Regression test for #7622: search motions must clamp cursor to grid
/// bounds. After a grid resize, stale match positions can exceed the
/// current grid dimensions.
#[test]
fn search_clamps_cursor_to_grid_bounds() {
    // Small 5x10 grid.
    let grid = MockGrid::new(5, 10);
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(0, 0));

    // Set matches that are beyond the current grid dimensions (sorted order):
    // line -100 is below topmost (0), line 20 exceeds visible rows (0..4),
    // col 50 exceeds cols (0..9).
    vi.search_mut()
        .set_matches(vec![ViPoint::new(-100, 5), ViPoint::new(20, 50)]);

    // SearchNext: target (20, 50) should be clamped to (4, 9).
    vi.motion_with_grid(&grid, ViMotion::SearchNext, ViBoundary::Grid);
    assert_eq!(
        vi.cursor_point().line,
        4,
        "line should be clamped to bottommost"
    );
    assert_eq!(
        vi.cursor_point().col,
        9,
        "col should be clamped to last column"
    );

    // SearchPrevious from (4, 9): target (-100, 5) should be clamped to (0, 5).
    // (topmost is -total_lines = 0 since no scrollback configured)
    vi.motion_with_grid(&grid, ViMotion::SearchPrevious, ViBoundary::Grid);
    assert_eq!(
        vi.cursor_point().line,
        0,
        "line should be clamped to topmost"
    );
    assert_eq!(
        vi.cursor_point().col,
        5,
        "col within bounds should be unchanged"
    );
}

/// Regression test for #7622: search with scrollback should clamp line
/// to topmost (negative) but not over-clamp valid scrollback positions.
#[test]
fn search_clamps_respects_scrollback_bounds() {
    // 5 visible rows, 10 scrollback lines.
    let grid = MockGrid::new(5, 20).with_scrollback(10);
    let mut vi = ViMode::new();
    vi.toggle(ViPoint::new(0, 0));

    // Match in scrollback at line -5 (valid) and one beyond at -20 (invalid).
    vi.search_mut()
        .set_matches(vec![ViPoint::new(-20, 3), ViPoint::new(-5, 8)]);

    // SearchNext from (0,0): first match past cursor is none (both are before),
    // wraps to first (-20, 3) which clamps to (-10, 3).
    vi.motion_with_grid(&grid, ViMotion::SearchNext, ViBoundary::Grid);
    assert_eq!(
        vi.cursor_point().line,
        -10,
        "line should clamp to -total_lines"
    );
    assert_eq!(vi.cursor_point().col, 3);

    // Reset cursor and test that valid scrollback position is not over-clamped.
    vi.search_mut().set_matches(vec![ViPoint::new(-5, 8)]);
    vi.motion_with_grid(&grid, ViMotion::SearchNext, ViBoundary::Grid);
    assert_eq!(
        vi.cursor_point().line,
        -5,
        "valid scrollback line should be preserved"
    );
    assert_eq!(vi.cursor_point().col, 8, "valid col should be preserved");
}
