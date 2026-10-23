// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Tests for `scroll_display` targeted damage patterns and
//! `content_scroll_delta` consumption.

use super::*;

#[test]
fn scroll_display_damages_exactly_the_exposed_rows() {
    #[expect(clippy::type_complexity, reason = "a test's case table")]
    let cases: &[(&str, usize, i32, usize, Option<&[u16]>)] = &[
        ("up 3", 0, 3, 3, Some(&[0, 1, 2])),
        ("down 3", 5, -3, 2, Some(&[7, 8, 9])),
        ("up past the screen", 0, 10, 10, None),
        ("zero", 3, 0, 3, Some(&[])),
        ("clamped at the top", 20, 100, 20, Some(&[])),
    ];
    for &(name, before, delta, after, damaged) in cases {
        let mut grid = Grid::with_scrollback(10, 10, 20);
        for _ in 0..40 {
            grid.set_cursor(9, 0);
            grid.write_char('X');
            grid.line_feed();
        }
        assert_eq!(grid.scrollback_lines(), 20, "{name}: precondition");
        grid.scroll_display(i32::try_from(before).expect("small"));
        grid.clear_damage();
        grid.scroll_display(delta);
        assert_eq!(grid.display_offset(), after, "{name}");
        match damaged {
            None => assert!(grid.damage().is_full(), "{name}"),
            Some(rows) => {
                assert!(!grid.damage().is_full(), "{name}");
                let got: Vec<u16> = (0..10)
                    .filter(|&r| grid.damage().is_row_damaged(r))
                    .collect();
                assert_eq!(got, rows, "{name}");
            }
        }
    }
}

#[test]
fn content_scroll_delta_set_by_scroll_up_and_consumed() {
    let mut grid = Grid::with_scrollback(5, 10, 50);

    for row in 0..5u16 {
        grid.set_cursor(row, 0);
        grid.write_char('X');
    }
    grid.clear_damage();

    grid.set_cursor(4, 0);
    grid.line_feed();

    let content_delta = grid.take_content_scroll_delta();
    assert_eq!(
        content_delta, 1,
        "scroll_up(1) should set content_scroll_delta"
    );
    assert_eq!(
        grid.take_content_scroll_delta(),
        0,
        "take_content_scroll_delta should return 0 after consumption"
    );
}
