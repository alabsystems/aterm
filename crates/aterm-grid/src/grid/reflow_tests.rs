// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn resize_clamps_oversize_dimensions() {
    for reflow in [true, false] {
        let mut grid = Grid::new(5, 10);
        if reflow {
            grid.resize(u16::MAX, u16::MAX);
        } else {
            grid.resize_no_reflow(u16::MAX, u16::MAX);
        }
        assert_eq!((grid.rows(), grid.cols()), (MAX_GRID_ROWS, MAX_GRID_COLS));
        grid.assert_invariants();
    }
}

#[test]
fn reflow_shrink_to_1_column() {
    let mut grid = Grid::new(5, 4);
    for c in "ABCD".chars() {
        grid.write_char(c);
    }
    grid.set_cursor(0, 0);

    grid.resize(5, 1);

    assert_eq!(grid.row(0).unwrap().to_string(), "A");
    assert_eq!(grid.row(1).unwrap().to_string(), "B");
    assert_eq!(grid.row(2).unwrap().to_string(), "C");
    assert_eq!(grid.row(3).unwrap().to_string(), "D");
    assert!(grid.row(1).unwrap().is_wrapped());
    assert!(grid.row(2).unwrap().is_wrapped());
    assert!(grid.row(3).unwrap().is_wrapped());
    grid.assert_invariants();
}

#[test]
fn reflow_disabled_grow_does_not_unwrap() {
    let mut grid = Grid::new(5, 5);
    for c in "ABCDE".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
    grid.carriage_return();
    if let Some(row) = grid.row_mut(1) {
        row.set_wrapped(true);
        for (i, c) in "FGHIJ".chars().enumerate() {
            row.write_char(i as u16, c);
        }
    }

    grid.resize_with_reflow_mode(5, 20, ReflowMode::Disabled);

    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDE");
    assert_eq!(grid.row(1).unwrap().to_string(), "FGHIJ");
    grid.assert_invariants();
}

fn ten_lines_in_four_rows() -> Grid {
    let mut grid = Grid::with_scrollback(4, 10, 100);
    for i in 0..10 {
        grid.write_char('L');
        grid.write_char(char::from(b'0' + i));
        grid.line_feed();
        grid.carriage_return();
    }
    assert_eq!(grid.scrollback_lines(), 7);
    assert_eq!(grid.row(0).unwrap().to_string(), "L7");
    assert_eq!(grid.cursor_row(), 3);
    grid
}

#[test]
fn conpty_rows_grow_appends_blanks_and_reveals_nothing() {
    let mut grid = ten_lines_in_four_rows();
    let epoch = grid.history_renumber_epoch();
    let reveal_gen = grid.storage.history_reveal_gen;
    let oldest = grid.oldest_absolute_row();
    let top = grid.visible_to_absolute(0);
    let counter = grid.absolute_row_counter();

    grid.resize_with_policy(8, 10, ResizePolicy::ConPty);

    assert_eq!(grid.rows(), 8);
    assert_eq!(grid.scrollback_lines(), 7, "no history revealed");
    assert_eq!(grid.row(0).unwrap().to_string(), "L7");
    assert_eq!(grid.row(1).unwrap().to_string(), "L8");
    assert_eq!(grid.row(2).unwrap().to_string(), "L9");
    for r in 3..8 {
        assert!(grid.row(r).unwrap().is_empty(), "row {r} is a fresh blank");
    }
    assert_eq!(grid.cursor_row(), 3, "cursor row unchanged");
    assert_eq!(grid.take_last_resize_row_shift(), 0, "nothing moved");
    assert_eq!(grid.oldest_absolute_row(), oldest, "history keys kept");
    assert_eq!(grid.visible_to_absolute(0), top, "`L7` keeps its key");
    assert_eq!(
        grid.absolute_row_counter(),
        counter + 4,
        "the four appended rows took fresh keys"
    );
    assert_eq!(
        grid.history_renumber_epoch(),
        epoch,
        "no key slid, so nothing was renumbered"
    );
    assert_eq!(
        grid.storage.history_reveal_gen, reveal_gen,
        "nothing was revealed, so the reveal fence does not move"
    );
    assert_eq!(
        grid.get_history_line(6).map(|l| l.to_string()),
        Some("L6".to_string()),
        "the newest history line is still history"
    );
    grid.assert_invariants();
}

#[test]
fn conpty_rows_grow_then_trim_keeps_every_key() {
    let mut grid = ten_lines_in_four_rows();
    let epoch = grid.history_renumber_epoch();
    let keys = |g: &Grid| (g.oldest_absolute_row(), g.visible_to_absolute(0));
    let before = keys(&grid);
    let counter = grid.absolute_row_counter();

    grid.resize_with_policy(8, 10, ResizePolicy::ConPty);
    assert_eq!(keys(&grid), before, "after the grow");
    grid.resize_with_policy(4, 10, ResizePolicy::ConPty);
    assert_eq!(keys(&grid), before, "after the trim");
    assert_eq!(grid.scrollback_lines(), 7, "the trim demoted nothing");
    assert_eq!(grid.row(0).unwrap().to_string(), "L7");
    assert_eq!(grid.absolute_row_counter(), counter, "the counter is back");
    assert_eq!(grid.history_renumber_epoch(), epoch);
    grid.assert_invariants();
}

#[test]
fn conpty_rows_grow_inside_an_offload_window_keeps_the_reader_aim() {
    use aterm_scrollback::{Scrollback, ScrollbackStorage};
    let rows = 10u16;
    let sb: ScrollbackStorage = Scrollback::new(64, 512, 8_000_000).into();
    let mut grid = Grid::with_tiered_scrollback(rows, 80, 8, sb);
    for i in 0..500 {
        grid.set_cursor(rows - 1, 0);
        for c in format!("H{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
        grid.carriage_return();
    }
    let pending = grid
        .resize_offloading_scrollback_with_policy(rows, 60, ResizePolicy::ConPty)
        .expect("a width change with a tiered store offloads");
    grid.scroll_display(30);
    assert!(grid.reader_aim_held(), "precondition: an aim past the top");

    assert!(
        grid.resize_offloading_scrollback_with_policy(16, 60, ResizePolicy::ConPty)
            .is_none(),
        "a rows-only grow offloads nothing"
    );
    grid.reattach_reflowed_scrollback(pending.reflow());
    assert_eq!(grid.display_offset(), 30, "the view lands where they asked");
    grid.assert_invariants();
}

#[test]
fn native_rows_grow_still_reveals_history() {
    let mut grid = ten_lines_in_four_rows();
    let reveal_gen = grid.storage.history_reveal_gen;
    grid.resize(8, 10);
    assert_eq!(grid.scrollback_lines(), 3);
    assert_eq!(grid.row(0).unwrap().to_string(), "L3");
    assert_eq!(grid.cursor_row(), 7);
    assert_eq!(grid.take_last_resize_row_shift(), 4);
    assert_eq!(
        grid.storage.history_reveal_gen,
        reveal_gen.wrapping_add(1),
        "the reveal moves the history carry's fence"
    );
    grid.assert_invariants();
}

fn boundary_straddling_line() -> Grid {
    let mut grid = Grid::with_scrollback(3, 10, 100);
    for c in "ABCDEFGHIJ".chars() {
        grid.write_char(c);
    }
    grid.line_feed();
    grid.carriage_return();
    for c in "KLMNO".chars() {
        grid.write_char(c);
    }
    grid.row_mut(1).unwrap().set_wrapped(true);
    grid.line_feed();
    grid.carriage_return();
    grid.write_char('x');
    grid.line_feed();
    grid.carriage_return();
    grid.write_char('y');
    assert_eq!(grid.scrollback_lines(), 1);
    assert_eq!(
        grid.get_history_line(0).map(|l| l.to_string()),
        Some("ABCDEFGHIJ".to_string())
    );
    assert_eq!(grid.row(0).unwrap().to_string(), "KLMNO");
    assert!(grid.row(0).unwrap().is_wrapped());
    assert_eq!((grid.cursor_row(), grid.cursor_col()), (2, 1));
    grid
}

#[test]
fn conpty_widen_skips_the_boundary_lift_and_the_deficit_fill() {
    let mut grid = boundary_straddling_line();
    grid.resize_with_policy(3, 20, ResizePolicy::ConPty);
    assert_eq!(grid.scrollback_lines(), 1, "the head is not lifted");
    assert_eq!(
        grid.get_history_line(0).map(|l| l.to_string()),
        Some("ABCDEFGHIJ".to_string()),
        "history rewraps only itself"
    );
    assert_eq!(grid.row(0).unwrap().to_string(), "KLMNO");
    assert!(
        !grid.row(0).unwrap().is_wrapped(),
        "severed from its head, as conhost treats it"
    );
    assert_eq!(grid.row(1).unwrap().to_string(), "x");
    assert_eq!(grid.row(2).unwrap().to_string(), "y");
    assert_eq!((grid.cursor_row(), grid.cursor_col()), (2, 1));
    grid.assert_invariants();
}

#[test]
fn conpty_width_round_trip_keeps_the_history_line_count() {
    let mut grid = boundary_straddling_line();
    for (step, cols) in [20u16, 10, 20, 10].into_iter().enumerate() {
        grid.resize_with_policy(3, cols, ResizePolicy::ConPty);
        assert_eq!(grid.scrollback_lines(), 1, "step {step} ({cols} cols)");
        assert_eq!(
            grid.get_history_line(0).map(|l| l.to_string()),
            Some("ABCDEFGHIJ".to_string()),
            "step {step}: the head, and nothing after it"
        );
        assert_eq!(grid.row(0).unwrap().to_string(), "KLMNO", "step {step}");
        grid.assert_invariants();
    }
}

#[test]
fn native_widen_still_rejoins_and_refills() {
    let mut grid = boundary_straddling_line();
    grid.resize(3, 20);
    assert_eq!(
        grid.scrollback_lines(),
        0,
        "the rejoined line was pulled back in"
    );
    assert_eq!(grid.row(0).unwrap().to_string(), "ABCDEFGHIJKLMNO");
    assert_eq!(grid.row(1).unwrap().to_string(), "x");
    assert_eq!(grid.row(2).unwrap().to_string(), "y");
    grid.assert_invariants();
}

#[test]
fn conpty_rows_shrink_equals_native() {
    let mut native = ten_lines_in_four_rows();
    let mut conpty = ten_lines_in_four_rows();
    native.resize(2, 10);
    conpty.resize_with_policy(2, 10, ResizePolicy::ConPty);
    assert_eq!(native.scrollback_lines(), conpty.scrollback_lines());
    for r in 0..2 {
        assert_eq!(
            native.row(r).unwrap().to_string(),
            conpty.row(r).unwrap().to_string()
        );
    }
    assert_eq!(native.cursor_row(), conpty.cursor_row());
    assert_eq!(
        native.history_renumber_epoch(),
        conpty.history_renumber_epoch()
    );
    conpty.assert_invariants();
}

fn labelled_screen(rows: u16, cursor_row: u16, max_scrollback: usize) -> Grid {
    let mut grid = Grid::with_scrollback(rows, 10, max_scrollback);
    for r in 0..rows {
        grid.set_cursor(r, 0);
        for c in format!("R{r}").chars() {
            grid.write_char(c);
        }
    }
    grid.set_cursor(cursor_row, 0);
    grid
}

#[test]
fn last_resize_shape_in_the_trim_demote_and_bottom_push_regimes() {
    use crate::ResizeShape;
    let shape = |trimmed, demoted, pushed, revealed, appended| ResizeShape {
        trimmed,
        demoted,
        pushed,
        revealed,
        appended,
        ..ResizeShape::default()
    };

    let mut grid = labelled_screen(2, 1, 100);
    grid.resize(6, 10);
    assert_eq!(grid.take_last_resize_shape(), shape(0, 0, 0, 0, 4));
    grid.resize(4, 10);
    assert_eq!(grid.last_resize_shape(), shape(2, 0, 0, 0, 0), "peek");
    assert_eq!(grid.take_last_resize_shape(), shape(2, 0, 0, 0, 0));
    assert_eq!(grid.last_resize_shape(), ResizeShape::default(), "drained");
    assert_eq!(grid.take_last_resize_row_shift(), 0, "a trim moves nothing");
    assert_eq!(grid.row(0).unwrap().to_string(), "R0");

    let mut grid = labelled_screen(6, 5, 100);
    grid.erase_line();
    grid.set_cursor(4, 0);
    grid.resize(3, 10);
    assert_eq!(grid.take_last_resize_shape(), shape(1, 2, 0, 0, 0));
    assert_eq!(grid.row(0).unwrap().to_string(), "R2");
    assert_eq!(grid.cursor_row(), 2);

    let mut grid = labelled_screen(6, 4, 100);
    grid.resize(4, 10);
    assert_eq!(grid.take_last_resize_shape(), shape(0, 2, 0, 0, 0));
    assert_eq!(
        grid.take_last_resize_row_shift(),
        2,
        "the selection's input"
    );
    assert_eq!(grid.row(0).unwrap().to_string(), "R2");
    grid.resize(6, 10);
    assert_eq!(grid.take_last_resize_shape(), shape(0, 0, 0, 2, 0));
    assert_eq!(grid.row(0).unwrap().to_string(), "R0");
    assert_eq!(grid.row(5).unwrap().to_string(), "R5");
    assert_eq!(grid.cursor_row(), 4);

    let mut grid = labelled_screen(6, 4, 0);
    grid.resize(4, 10);
    assert_eq!(
        grid.take_last_resize_shape(),
        ResizeShape {
            stashed: 2,
            ..shape(0, 2, 0, 0, 0)
        }
    );
    assert_eq!(grid.scrollback_lines(), 0, "retention 0 evicted the demote");
    grid.resize(6, 10);
    assert_eq!(
        grid.take_last_resize_shape(),
        ResizeShape {
            restored_top: 2,
            undone: true,
            ..shape(0, 0, 0, 2, 0)
        }
    );
    assert_eq!(grid.row(0).unwrap().to_string(), "R0");
    assert_eq!(grid.row(5).unwrap().to_string(), "R5");
    assert_eq!(grid.cursor_row(), 4);
    assert_eq!(grid.scrollback_lines(), 0, "handed back, not retained");

    let mut grid = labelled_screen(6, 4, 0);
    grid.resize(4, 10);
    grid.write_char('x');
    grid.resize(6, 10);
    assert_eq!(grid.take_last_resize_shape(), shape(0, 0, 0, 0, 2));
    assert_eq!(grid.row(0).unwrap().to_string(), "R2");
    assert_eq!(grid.row(3).unwrap().to_string(), "R5");
    assert!(grid.row(4).unwrap().is_empty());
    assert!(grid.row(5).unwrap().is_empty());
    assert_eq!(grid.cursor_row(), 2);

    let mut grid = labelled_screen(6, 0, 100);
    grid.resize(4, 10);
    assert_eq!(grid.take_last_resize_shape(), shape(0, 0, 2, 0, 0));
    assert_eq!(grid.row(0).unwrap().to_string(), "R0", "the top stays put");

    let mut grid = labelled_screen(6, 1, 100);
    grid.resize(4, 10);
    assert_eq!(grid.take_last_resize_shape(), shape(0, 1, 1, 0, 0));

    grid.resize(4, 10);
    grid.resize(4, 10);
    assert_eq!(grid.take_last_resize_shape(), ResizeShape::default());

    let mut grid = labelled_screen(6, 4, 100);
    grid.resize(6, 12);
    assert!(grid.take_last_resize_shape().reflowed);
    grid.resize_no_reflow(6, 10);
    assert!(!grid.take_last_resize_shape().reflowed);

    let mut conpty = ten_lines_in_four_rows();
    conpty.resize_with_policy(8, 10, ResizePolicy::ConPty);
    assert_eq!(conpty.take_last_resize_shape(), shape(0, 0, 0, 0, 4));
    let mut native = ten_lines_in_four_rows();
    native.resize(8, 10);
    assert_eq!(native.take_last_resize_shape(), shape(0, 0, 0, 4, 0));
    native.assert_invariants();
    conpty.assert_invariants();
}

#[test]
fn a_narrowing_with_no_rewrap_counts_the_drawn_columns_it_cuts() {
    let mut grid = labelled_screen(4, 0, 0);
    let draw = |grid: &mut Grid, row: u16, text: &str| {
        grid.set_cursor(row, 0);
        for c in text.chars() {
            grid.write_char(c);
        }
    };
    draw(&mut grid, 1, "0123456");
    draw(&mut grid, 2, "abcdefgh");
    draw(&mut grid, 3, "xy        ");
    grid.resize_no_reflow(4, 9);
    assert_eq!(grid.take_last_resize_shape().cut, 0, "only blanks past 9");
    grid.resize_no_reflow(4, 6);
    assert_eq!(
        grid.take_last_resize_shape().cut,
        2,
        "row 2 drew to column 7"
    );
    grid.resize_no_reflow(4, 10);
    assert_eq!(grid.take_last_resize_shape().cut, 0, "a widening");
    assert_eq!(grid.row(2).unwrap().to_string(), "abcdef", "the cut cells");

    let mut grid = labelled_screen(4, 0, 100);
    draw(&mut grid, 1, "abcdefghij");
    grid.resize(4, 6);
    let shape = grid.take_last_resize_shape();
    assert!(shape.reflowed);
    assert_eq!(shape.cut, 0, "a rewrap keeps every cell");
}

fn prompt_above_a_non_blank_row() -> Grid {
    let mut grid = Grid::with_scrollback(6, 10, 100);
    for i in 0..10 {
        grid.write_char('L');
        grid.write_char(char::from(b'0' + i));
        grid.line_feed();
        grid.carriage_return();
    }
    grid.write_char('>');
    grid.line_feed();
    grid.carriage_return();
    grid.write_char('>');
    grid.write_char('>');
    grid.move_cursor_to(4, 1);
    assert_eq!(grid.scrollback_lines(), 6);
    assert_eq!(grid.row(4).unwrap().to_string(), ">");
    assert_eq!(grid.row(5).unwrap().to_string(), ">>");
    grid
}

fn history_text(grid: &Grid) -> Vec<String> {
    (0..grid.scrollback_lines())
        .map(|i| grid.get_history_line(i).unwrap().to_string())
        .collect()
}

fn screen_text(grid: &Grid) -> Vec<String> {
    (0..grid.rows())
        .map(|r| grid.row(r).unwrap().to_string())
        .collect()
}

#[test]
fn conpty_width_shrink_demotes_the_excess_above_a_non_blank_row() {
    let lines =
        |r: std::ops::RangeInclusive<u8>| -> Vec<String> { r.map(|i| format!("L{i}")).collect() };
    let mut conpty = prompt_above_a_non_blank_row();
    conpty.resize_with_policy(4, 8, ResizePolicy::ConPty);
    assert_eq!(
        history_text(&conpty),
        lines(0..=7),
        "both excess rows demoted"
    );
    assert_eq!(screen_text(&conpty), ["L8", "L9", ">", ">>"]);
    assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (2, 1));
    conpty.assert_invariants();

    let mut native = prompt_above_a_non_blank_row();
    native.resize(4, 8);
    assert_eq!(
        history_text(&native),
        lines(0..=7),
        "Native: the window's end stays on screen"
    );
    assert_eq!(screen_text(&native), ["L8", "L9", ">", ">>"]);
    assert_eq!((native.cursor_row(), native.cursor_col()), (2, 1));
    native.assert_invariants();

    let mut one_pass = prompt_above_a_non_blank_row();
    one_pass.forget_handback();
    one_pass.resize(4, 8);
    assert_eq!(
        history_text(&one_pass),
        lines(0..=6),
        "one pass: the cursor's minimum"
    );
    assert_eq!(screen_text(&one_pass), ["L7", "L8", "L9", ">"]);
    assert_eq!(one_pass.cursor_row(), 3);
}

#[test]
fn conpty_width_shrink_demotes_no_further_than_the_cursor_and_drops_blanks() {
    let mut blank = Grid::with_scrollback(3, 10, 100);
    for line in ["0123456789", "abcdefghij"] {
        for c in line.chars() {
            blank.write_char_wrap(c);
        }
        blank.carriage_return();
        blank.line_feed();
    }
    assert_eq!(blank.cursor_row(), 2);
    blank.resize_with_policy(3, 5, ResizePolicy::ConPty);
    assert_eq!(history_text(&blank), ["01234", "56789"]);
    assert_eq!(screen_text(&blank), ["abcde", "fghij", ""]);
    assert_eq!((blank.cursor_row(), blank.cursor_col()), (2, 0));
    blank.assert_invariants();

    let blank_tail = || {
        let mut grid = Grid::with_scrollback(4, 10, 100);
        for line in ["L0", ">"] {
            for c in line.chars() {
                grid.write_char_wrap(c);
            }
            grid.carriage_return();
            grid.line_feed();
        }
        for c in "abcdefghijk".chars() {
            grid.write_char_wrap(c);
        }
        grid.move_cursor_to(3, 0);
        grid.erase_chars(1);
        grid.move_cursor_to(2, 5);
        grid.delete_chars(5);
        grid.move_cursor_to(1, 1);
        let tail = grid.row(3).unwrap();
        assert!(tail.is_empty() && tail.is_wrapped());
        grid
    };
    let mut one_step = blank_tail();
    one_step.resize_with_policy(3, 5, ResizePolicy::ConPty);
    let mut two_steps = blank_tail();
    two_steps.resize_with_policy(4, 5, ResizePolicy::ConPty);
    two_steps.resize_with_policy(3, 5, ResizePolicy::ConPty);
    assert_eq!(history_text(&one_step), ["L0"]);
    assert_eq!(history_text(&one_step), history_text(&two_steps));
    assert_eq!(screen_text(&one_step), screen_text(&two_steps));
    assert_eq!(one_step.cursor_row(), two_steps.cursor_row());

    let mut high = prompt_above_a_non_blank_row();
    high.move_cursor_to(1, 0);
    high.resize_with_policy(3, 8, ResizePolicy::ConPty);
    assert_eq!(high.scrollback_lines(), 7);
    assert_eq!(high.row(0).unwrap().to_string(), "L7");
    assert_eq!(high.cursor_row(), 0);
    high.assert_invariants();

    let mut conpty = prompt_above_a_non_blank_row();
    let mut native = prompt_above_a_non_blank_row();
    for grid in [&mut conpty, &mut native] {
        grid.move_cursor_to(5, 0);
        grid.erase_line();
        grid.move_cursor_to(4, 1);
    }
    conpty.resize_with_policy(4, 8, ResizePolicy::ConPty);
    native.resize(4, 8);
    assert_eq!(history_text(&conpty), history_text(&native));
    assert_eq!(screen_text(&conpty), ["L7", "L8", "L9", ">"]);
    assert_eq!(conpty.cursor_row(), native.cursor_row());
}

fn prompt_with_the_cursor_past_its_end() -> Grid {
    let mut grid = Grid::with_scrollback(6, 10, 100);
    for i in 0..10 {
        grid.write_char('L');
        grid.write_char(char::from(b'0' + i));
        grid.line_feed();
        grid.carriage_return();
    }
    for c in "PPPPPPPPPPQQQQQ".chars() {
        grid.write_char_wrap(c);
    }
    grid.move_cursor_to(5, 6);
    assert_eq!(grid.scrollback_lines(), 6);
    assert_eq!(screen_text(&grid)[4..], ["PPPPPPPPPP", "QQQQQ"]);
    assert!(grid.row(5).unwrap().is_wrapped());
    grid
}

#[test]
fn conpty_rewrap_keeps_the_cursor_cell_past_the_prompt_as_a_row() {
    let lines =
        |r: std::ops::RangeInclusive<u8>| -> Vec<String> { r.map(|i| format!("L{i}")).collect() };
    let mut conpty = prompt_with_the_cursor_past_its_end();
    conpty.resize_with_policy(6, 5, ResizePolicy::ConPty);
    assert_eq!(history_text(&conpty), lines(0..=7), "two rows demoted");
    assert_eq!(
        screen_text(&conpty),
        ["L8", "L9", "PPPPP", "PPPPP", "QQQQQ", ""]
    );
    assert!(
        conpty.row(5).unwrap().is_wrapped(),
        "the cursor's row continues the prompt"
    );
    assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (5, 1));
    conpty.assert_invariants();

    conpty.resize_with_policy(6, 10, ResizePolicy::ConPty);
    assert_eq!(history_text(&conpty), lines(0..=7));
    assert_eq!(
        screen_text(&conpty),
        ["L8", "L9", "PPPPPPPPPP", "QQQQQ", "", ""]
    );
    assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (3, 6));
    conpty.assert_invariants();

    // Native carries the cursor's cell the same way, so the next glyph goes where it would
    // have before the resize rather than onto the last Q.
    let mut native = prompt_with_the_cursor_past_its_end();
    native.resize(6, 5);
    assert_eq!(history_text(&native), lines(0..=7));
    assert_eq!(
        screen_text(&native),
        ["L8", "L9", "PPPPP", "PPPPP", "QQQQQ", ""]
    );
    assert!(native.row(5).unwrap().is_wrapped());
    assert_eq!(
        (
            native.cursor_row(),
            native.cursor_col(),
            native.pending_wrap()
        ),
        (5, 1, false)
    );
    native.assert_invariants();
}

#[test]
fn conpty_rewrap_keeps_the_cursor_offset_on_an_unwrapped_line() {
    let prompt = || {
        let mut grid = Grid::new(3, 10);
        for c in "PPPPPP".chars() {
            grid.write_char(c);
        }
        grid.move_cursor_to(0, 8);
        grid
    };
    let mut conpty = prompt();
    conpty.resize_with_policy(3, 4, ResizePolicy::ConPty);
    assert_eq!(screen_text(&conpty), ["PPPP", "PP", ""]);
    assert!(conpty.row(2).unwrap().is_wrapped());
    assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (2, 0));
    conpty.assert_invariants();
    // Native carries the cursor's cell too: offset 8 is past the line's last row, so the
    // cursor starts a row that continues the line, where its next glyph lands.
    let mut native = prompt();
    native.resize(3, 4);
    assert_eq!(screen_text(&native), ["PPPP", "PP", ""]);
    assert!(native.row(2).unwrap().is_wrapped());
    assert_eq!(
        (
            native.cursor_row(),
            native.cursor_col(),
            native.pending_wrap()
        ),
        (2, 0, false)
    );
    native.write_char_wrap('X');
    assert_eq!(screen_text(&native), ["PPPP", "PP", "X"]);
    native.assert_invariants();

    let mut conpty = prompt();
    conpty.resize_with_policy(3, 5, ResizePolicy::ConPty);
    assert_eq!(screen_text(&conpty), ["PPPPP", "P", ""]);
    assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (1, 3));
    conpty.assert_invariants();
    let mut native = prompt();
    native.resize(3, 5);
    assert_eq!(
        (
            native.cursor_row(),
            native.cursor_col(),
            native.pending_wrap()
        ),
        (1, 3, false)
    );
}

#[test]
fn conpty_rewrap_carries_a_cursor_on_a_blank_row_past_the_width() {
    let parked = || {
        let mut grid = Grid::new(24, 80);
        grid.move_cursor_to(5, 70);
        grid
    };
    let mut conpty = parked();
    conpty.resize_with_policy(24, 60, ResizePolicy::ConPty);
    assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (6, 10));
    assert!(conpty.row(6).unwrap().is_wrapped());
    conpty.resize_with_policy(24, 40, ResizePolicy::ConPty);
    assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (6, 30));
    conpty.resize_with_policy(24, 80, ResizePolicy::ConPty);
    assert_eq!((conpty.cursor_row(), conpty.cursor_col()), (5, 70));
    assert!((0..24).all(|r| conpty.row(r).unwrap().is_empty()));
    assert_eq!(conpty.scrollback_lines(), 0);
    conpty.assert_invariants();

    let mut native = parked();
    native.resize(24, 60);
    assert_eq!((native.cursor_row(), native.cursor_col()), (6, 10));
    assert!(native.row(6).unwrap().is_wrapped());
    native.resize(24, 80);
    assert_eq!((native.cursor_row(), native.cursor_col()), (5, 70));
    assert!((0..24).all(|r| native.row(r).unwrap().is_empty()));
    native.assert_invariants();
}
