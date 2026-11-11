// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::ResizeLaw;
use crate::grid::Grid;
use crate::grid::reflow::ResizePolicy;

fn cleared_with_history(rows: u16, history: u16, lines: u16) -> Grid {
    drawn(Grid::with_scrollback(rows, 10, 100), history, lines)
}

fn tiered_cleared_with_history(rows: u16, ring: usize, history: u16, lines: u16) -> Grid {
    let store = aterm_scrollback::Scrollback::new(100, 1000, 10_000_000);
    drawn(
        Grid::with_tiered_scrollback(rows, 10, ring, store),
        history,
        lines,
    )
}

fn drawn(mut grid: Grid, history: u16, lines: u16) -> Grid {
    for i in 0..history {
        write_line(&mut grid, &format!("h{i}"));
    }
    grid.erase_screen();
    grid.set_cursor(0, 0);
    for i in 0..lines {
        write_line(&mut grid, &format!("L{i}"));
    }
    assert!(grid.scrollback_lines() > 0, "history above the screen");
    grid
}

fn write_line(grid: &mut Grid, text: &str) {
    for c in text.chars() {
        grid.write_char(c);
    }
    grid.carriage_return();
    grid.line_feed();
}

fn screen(grid: &Grid) -> Vec<String> {
    (0..grid.rows())
        .map(|r| grid.row(r).map(ToString::to_string).unwrap_or_default())
        .collect()
}

fn picture(grid: &Grid) -> (Vec<String>, u16, u16, usize) {
    (
        screen(grid),
        grid.cursor_row(),
        grid.cursor_col(),
        grid.scrollback_lines(),
    )
}

#[test]
fn a_trim_then_a_grow_is_an_identity() {
    for heights in [
        &[7u16, 8][..],
        &[5, 8],
        &[7, 6, 8],
        &[6, 7, 8],
        &[5, 7, 6, 8],
    ] {
        let mut grid = cleared_with_history(8, 12, 2);
        let before = picture(&grid);
        for &h in heights {
            grid.resize(h, 10);
        }
        assert_eq!(picture(&grid), before, "{heights:?}");
        assert_eq!(grid.trimmed_rows(), 0, "{heights:?}: all handed back");
        grid.assert_invariants();
    }

    let mut grid = cleared_with_history(8, 12, 2);
    grid.resize(7, 10);
    let shrink = grid.take_last_resize_shape();
    assert_eq!((shrink.trimmed, grid.trimmed_rows()), (1, 1));
    grid.resize(8, 10);
    let grow = grid.take_last_resize_shape();
    assert_eq!((grow.revealed, grow.appended), (0, 1));
    assert_eq!(grid.take_last_resize_row_shift(), 0, "nothing moved");
}

#[test]
fn a_flap_that_grows_first_or_overshoots_is_an_identity() {
    for heights in [
        &[9u16, 8][..],
        &[10, 8],
        &[7, 9, 8],
        &[5, 10, 8],
        &[7, 8, 9, 8],
        &[10, 9, 8],
    ] {
        let mut grid = cleared_with_history(8, 12, 2);
        let before = picture(&grid);
        for &h in heights {
            grid.resize(h, 10);
        }
        assert_eq!(picture(&grid), before, "{heights:?}");
        assert_eq!(grid.handback_rows(), 0, "{heights:?}: all handed back");
        grid.assert_invariants();
    }

    let mut grid = cleared_with_history(8, 12, 2);
    grid.resize(9, 10);
    assert_eq!(grid.take_last_resize_shape().revealed, 1);
    assert_eq!((grid.revealed_rows(), grid.handback_rows()), (1, -1));
    grid.resize(8, 10);
    let shrink = grid.take_last_resize_shape();
    assert_eq!((shrink.demoted, shrink.trimmed), (1, 0));
    assert_eq!(grid.revealed_rows(), 0);
}

#[test]
fn every_height_a_run_returns_to_shows_the_same_screen() {
    let shapes = [
        ("blank tail", 12, 2),
        ("short history", 9, 2),
        ("no blank tail", 12, 7),
    ];
    let range = 1u16..=12;
    for (shape, history, lines) in shapes {
        for a in range.clone() {
            for b in range.clone() {
                for c in range.clone() {
                    let mut grid = cleared_with_history(8, history, lines);
                    let mut seen = std::collections::BTreeMap::new();
                    seen.insert(8u16, picture(&grid));
                    for h in [a, b, c, 8] {
                        grid.resize(h, 10);
                        let now = picture(&grid);
                        let first = seen.entry(h).or_insert_with(|| now.clone());
                        assert_eq!(&now, first, "{shape}: [{a}, {b}, {c}, 8] at {h}");
                    }
                    grid.assert_invariants();
                }
            }
        }
    }
}

#[test]
fn the_natural_move_after_an_owed_one_goes_back_first() {
    let mut grid = cleared_with_history(8, 12, 2);
    grid.resize(3, 10);
    let at_three = picture(&grid);
    grid.resize(2, 10);
    assert_eq!(grid.take_last_resize_shape().demoted, 1);
    assert_eq!((grid.trimmed_rows(), grid.handback_above()), (5, 1));
    grid.resize(3, 10);
    let grow = grid.take_last_resize_shape();
    assert_eq!((grow.revealed, grow.appended), (1, 0));
    assert_eq!(picture(&grid), at_three);
    assert_eq!((grid.trimmed_rows(), grid.handback_above()), (5, 0));

    let mut grid = cleared_with_history(8, 9, 2);
    grid.resize(10, 10);
    let at_ten = picture(&grid);
    grid.resize(12, 10);
    let grow = grid.take_last_resize_shape();
    assert_eq!(
        (grow.revealed, grow.appended),
        (0, 2),
        "the history ran out"
    );
    assert_eq!((grid.revealed_rows(), grid.handback_above()), (2, 2));
    grid.resize(10, 10);
    let shrink = grid.take_last_resize_shape();
    assert_eq!((shrink.trimmed, shrink.demoted), (2, 0));
    assert_eq!(picture(&grid), at_ten);
    grid.assert_invariants();
}

#[test]
fn a_grow_past_the_start_reveals_history_for_the_rest() {
    let mut grid = cleared_with_history(8, 12, 2);
    let history = grid.scrollback_lines();
    grid.resize(7, 10);
    grid.resize(10, 10);
    let grow = grid.take_last_resize_shape();
    assert_eq!((grow.revealed, grow.appended), (2, 1));
    assert_eq!(grid.scrollback_lines(), history - 2);
    assert_eq!(&screen(&grid)[2..4], ["L0", "L1"]);
    assert_eq!((grid.trimmed_rows(), grid.revealed_rows()), (0, 2));
    grid.assert_invariants();
}

#[test]
fn a_line_scrolled_into_history_uses_one_trimmed_row() {
    let mut never = cleared_with_history(8, 12, 6);
    let mut flapped = cleared_with_history(8, 12, 6);
    flapped.resize(7, 10);
    assert_eq!(flapped.trimmed_rows(), 1, "one blank row under the cursor");
    for grid in [&mut never, &mut flapped] {
        write_line(grid, "more");
    }
    assert_eq!(flapped.trimmed_rows(), 0, "the scroll used it");
    flapped.resize(8, 10);
    assert_eq!(flapped.take_last_resize_shape().revealed, 1);
    assert_eq!(picture(&flapped), picture(&never));
    flapped.assert_invariants();
}

#[test]
fn a_partial_grow_after_scrolled_output_shows_the_screen_that_stayed() {
    for prior in [0u16, 60] {
        for target in [12u16, 30, 40, 50] {
            let feed = |grid: &mut Grid, part: u8| {
                if part == 0 {
                    for i in 0..prior {
                        write_line(grid, &format!("old{i}"));
                    }
                    if prior > 0 {
                        grid.erase_screen();
                        grid.set_cursor(0, 0);
                    }
                    for i in 0..6 {
                        write_line(grid, &format!("p{i}"));
                    }
                } else {
                    for i in 0..20 {
                        write_line(grid, &format!("o{i}"));
                    }
                }
            };
            let mut flapped = Grid::with_scrollback(50, 10, 1000);
            feed(&mut flapped, 0);
            flapped.resize(10, 10);
            feed(&mut flapped, 1);
            flapped.resize(target, 10);
            let mut stayed = Grid::with_scrollback(target, 10, 1000);
            feed(&mut stayed, 0);
            feed(&mut stayed, 1);
            let at = format!("{prior} lines before, 50 -> 10, 20 lines, -> {target}");
            assert_eq!(screen(&flapped), screen(&stayed), "{at}");
            assert_eq!(flapped.cursor_row(), stayed.cursor_row(), "{at}");
            flapped.assert_invariants();
        }
    }

    let drawn = || {
        let mut grid = Grid::with_scrollback(8, 10, 100);
        for i in 0..4 {
            write_line(&mut grid, &format!("L{i}"));
        }
        grid
    };
    let mut never = drawn();
    let mut flapped = drawn();
    flapped.resize(9, 10);
    flapped.resize(5, 10);
    for grid in [&mut never, &mut flapped] {
        write_line(grid, "X0");
    }
    flapped.resize(8, 10);
    assert_eq!(picture(&flapped), picture(&never), "8 -> 9 -> 5, X0, -> 8");
    flapped.assert_invariants();
}

#[test]
fn output_keeps_the_revealed_rows_until_the_shrink() {
    for scroll in [false, true] {
        let mut never = cleared_with_history(8, 12, 2);
        let mut flapped = cleared_with_history(8, 12, 2);
        flapped.resize(9, 10);
        for grid in [&mut never, &mut flapped] {
            write_line(grid, "drawn");
            if scroll {
                grid.scroll_up(1);
            }
        }
        assert_eq!(flapped.revealed_rows(), 1, "scroll={scroll}");
        flapped.resize(8, 10);
        assert_eq!(picture(&flapped), picture(&never), "scroll={scroll}");
        flapped.assert_invariants();
    }
}

#[test]
fn an_erase_of_the_screen_forgets_the_revealed_rows() {
    let mut never = cleared_with_history(8, 12, 2);
    let mut flapped = cleared_with_history(8, 12, 2);
    flapped.resize(9, 10);
    for grid in [&mut never, &mut flapped] {
        grid.erase_screen();
        grid.set_cursor(0, 0);
        for i in 0..3 {
            write_line(grid, &format!("N{i}"));
        }
    }
    assert_eq!(flapped.revealed_rows(), 0);
    flapped.resize(8, 10);
    let shrink = flapped.take_last_resize_shape();
    assert_eq!((shrink.trimmed, shrink.demoted), (1, 0));
    assert_eq!(screen(&flapped), screen(&never));
    assert_eq!(flapped.cursor_row(), never.cursor_row());
    flapped.assert_invariants();
}

#[test]
fn a_cursor_among_the_revealed_rows_forgets_them() {
    let mut grid = cleared_with_history(8, 12, 2);
    grid.resize(10, 10);
    assert_eq!(grid.revealed_rows(), 2);
    grid.set_cursor(1, 0);
    grid.resize(8, 10);
    let shrink = grid.take_last_resize_shape();
    assert_eq!((shrink.demoted, shrink.trimmed), (0, 2));
    assert_eq!(grid.handback_rows(), 2, "the trims are owed instead");
    grid.assert_invariants();
}

#[test]
fn output_that_scrolls_nothing_keeps_the_trimmed_rows() {
    let mut grid = cleared_with_history(8, 12, 2);
    grid.resize(7, 10);
    grid.erase_screen();
    grid.set_cursor(0, 0);
    for c in "redrawn".chars() {
        grid.write_char(c);
    }
    assert_eq!(grid.trimmed_rows(), 1);
    grid.resize(8, 10);
    assert_eq!(grid.row(0).unwrap().to_string(), "redrawn");
    assert_eq!(grid.cursor_row(), 0);
}

#[test]
fn a_width_change_without_a_rewrap_conpty_and_forget_drop_what_is_owed() {
    for grow_first in [false, true] {
        let first = if grow_first { 9 } else { 7 };
        let mut grid = cleared_with_history(8, 12, 2);
        grid.resize(first, 10);
        let owed = (grid.handback_rows(), grid.handback_above());
        assert_ne!(owed.0, 0);
        grid.resize(first, 12);
        assert_eq!(
            (grid.handback_rows(), grid.handback_above()),
            owed,
            "a rewrap keeps it"
        );

        let mut grid = cleared_with_history(8, 12, 2);
        grid.resize(first, 10);
        grid.resize_no_reflow(first, 12);
        assert_eq!(grid.handback_rows(), 0, "a width change with no rewrap");

        let mut grid = cleared_with_history(8, 12, 2);
        grid.resize_with_policy(first, 10, ResizePolicy::ConPty);
        assert_eq!(grid.handback_rows(), 0, "ConPTY");
    }

    let mut grid = cleared_with_history(8, 12, 2);
    grid.resize(7, 10);
    grid.forget_handback();
    grid.resize(8, 10);
    let grow = grid.take_last_resize_shape();
    assert_eq!((grow.revealed, grow.appended), (1, 0), "the old law");
    assert_eq!(grid.row(1).unwrap().to_string(), "L0", "every row down one");
    grid.forget_handback();
    grid.resize(7, 10);
    let shrink = grid.take_last_resize_shape();
    assert_eq!((shrink.trimmed, shrink.demoted), (1, 0), "the old law");
    assert_eq!(grid.row(1).unwrap().to_string(), "L0", "still down one");
    grid.assert_invariants();
}

#[test]
fn a_grow_that_narrows_shows_what_the_grid_never_shrunk_shows() {
    let grid = |law: ResizeLaw, wide: bool| {
        let mut grid = cleared_with_history(8, 12, 2);
        grid.set_resize_law(law);
        if wide {
            write_line(&mut grid, "LLLLLLLLLL");
        }
        grid
    };
    for law in [ResizeLaw::Rewrap, ResizeLaw::PushedRows] {
        for wide in [false, true] {
            let mut never = grid(law, wide);
            never.resize(9, 9);
            let mut kept = grid(law, wide);
            kept.resize(7, 10);
            assert_eq!(kept.trimmed_rows(), 1);
            let mut forgot = grid(law, wide);
            forgot.resize(7, 10);
            forgot.forget_handback();
            kept.resize(9, 9);
            forgot.resize(9, 9);
            kept.assert_invariants();
            let label = format!("{law:?}, wide {wide}");
            if law == ResizeLaw::PushedRows && wide {
                assert_eq!(picture(&kept), picture(&forgot), "{label}");
                assert_eq!((kept.handback_rows(), kept.handback_above()), (0, 0));
                assert_ne!(picture(&kept), picture(&never), "{label}");
            } else {
                assert_eq!(picture(&kept), picture(&never), "{label}");
            }
        }
    }
}

fn script(seed: u64, steps: usize) -> Vec<(u64, u16, u16)> {
    let mut state: u64 = seed;
    (0..steps)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let rows = 3 + u16::try_from((state >> 20) % 10).unwrap();
            let cols = 6 + u16::try_from((state >> 28) % 20).unwrap();
            (state, rows, cols)
        })
        .collect()
}

fn feed(grid: &mut Grid, step: usize, r: u64) {
    grid.scroll_to_bottom();
    let tail = "x".repeat(usize::try_from(r % 25).unwrap());
    write_line(grid, &format!("s{step} {tail}"));
    if (r >> 8).is_multiple_of(7) {
        grid.erase_screen();
        grid.set_cursor(0, 0);
    }
    if (r >> 12).is_multiple_of(5) {
        grid.cursor_up(u16::try_from((r >> 16) % 4).unwrap());
    }
    if (r >> 40).is_multiple_of(6) {
        grid.scroll_display(i32::try_from((r >> 44) % 5).unwrap());
    }
}

#[test]
fn an_offloaded_resize_owes_what_the_synchronous_one_does() {
    fn state(grid: &Grid) -> (Vec<String>, u16, u16, usize, super::Edge, u16) {
        let rows = (0..grid.rows())
            .map(|r| {
                grid.storage
                    .row_at_screen(r)
                    .map(ToString::to_string)
                    .unwrap_or_default()
            })
            .collect();
        (
            rows,
            grid.cursor_row(),
            grid.cursor_col(),
            grid.scrollback_lines(),
            grid.handback_owed(true),
            grid.window_skew(),
        )
    }
    let mut owed = 0;
    for (law, seed) in [ResizeLaw::Rewrap, ResizeLaw::PushedRows]
        .into_iter()
        .flat_map(|law| [0x9e37_79b9_7f4a_7c15, 0x2545_f491_4f6c_dd1d].map(|seed| (law, seed)))
    {
        let steps = script(seed, 50);
        for at in 0..steps.len() {
            let fed = || {
                let mut grid = tiered_cleared_with_history(8, 6, 12, 2);
                grid.set_resize_law(law);
                for (step, &(r, rows, cols)) in steps[..at].iter().enumerate() {
                    feed(&mut grid, step, r);
                    grid.resize(rows, cols);
                }
                feed(&mut grid, at, steps[at].0);
                grid
            };
            let (_, rows, cols) = steps[at];
            let mut sync = fed();
            if law == ResizeLaw::PushedRows && rows > sync.rows() && cols < sync.cols() {
                continue;
            }
            let mut offloaded = fed();
            sync.resize(rows, cols);
            let mut job = offloaded.resize_offloading_scrollback(rows, cols);
            while let Some(pending) = job {
                job = offloaded.reattach_reflowed_scrollback_or_redetach(pending.reflow());
            }
            offloaded.assert_invariants();
            assert_eq!(
                state(&offloaded),
                state(&sync),
                "{law:?} seed {seed:#x} step {at}: {:?}",
                steps[at]
            );
            owed += usize::from(sync.handback_rows() != 0);
        }
    }
    assert!(owed > 40, "only {owed} steps owed anything");
    let homed = || {
        let mut grid = tiered_cleared_with_history(8, 2, 12, 0);
        for line in ["L0", "LLLLLLLLL1", "L2", "", "L4", "L5", "", "L7"] {
            write_line(&mut grid, line);
        }
        grid.set_cursor(0, 0);
        grid
    };
    let mut pushed = 0;
    for (_, rows, cols) in script(0x8cb9_2ba7_2f3d_8dd7, 40) {
        let (mut sync, mut offloaded) = (homed(), homed());
        for (rows, cols) in [(rows.min(7), cols), (8, 10)] {
            sync.resize(rows, cols);
            let mut job = offloaded.resize_offloading_scrollback(rows, cols);
            while let Some(pending) = job {
                job = offloaded.reattach_reflowed_scrollback_or_redetach(pending.reflow());
            }
            offloaded.assert_invariants();
            assert_eq!(state(&offloaded), state(&sync), "homed at {rows}x{cols}");
            pushed += usize::from(sync.handback_owed(true).pushed_rows() != 0);
        }
        assert_eq!(picture(&sync), picture(&homed()), "homed back at 8x10");
    }
    assert!(pushed > 10, "only {pushed} steps owed pushed rows");
    let split = || {
        let store = aterm_scrollback::Scrollback::new(100, 1000, 10_000_000);
        let mut grid = Grid::with_tiered_scrollback(8, 10, 2, store);
        for i in 0..12 {
            write_line(&mut grid, &format!("h{i}"));
        }
        for c in "abcdefghijklmnopqrstuvwxy".chars() {
            grid.write_char_wrap(c);
        }
        for row in ["a", "b", "c", "d", "e", ""] {
            grid.carriage_return();
            grid.line_feed();
            for c in row.chars() {
                grid.write_char(c);
            }
        }
        grid.set_cursor(0, 1);
        grid
    };
    let mut skewed = 0;
    for (_, rows, cols) in script(0x5851_f42d_4c95_7f2d, 40) {
        let (mut sync, mut offloaded) = (split(), split());
        for (rows, cols) in [(rows, cols), (8, 9), (8, 10)] {
            sync.resize(rows, cols);
            let mut job = offloaded.resize_offloading_scrollback(rows, cols);
            while let Some(pending) = job {
                job = offloaded.reattach_reflowed_scrollback_or_redetach(pending.reflow());
            }
            offloaded.assert_invariants();
            assert_eq!(state(&offloaded), state(&sync), "split at {rows}x{cols}");
            skewed += usize::from(sync.window_skew() != 0);
        }
        assert_eq!(picture(&sync), picture(&split()), "split back at 8x10");
    }
    assert!(skewed > 10, "only {skewed} steps kept a cell");
}

#[test]
fn a_resize_moves_rows_as_one_pass_did_where_nothing_is_drawn_after_the_cursor() {
    fn lines(grid: &Grid) -> Vec<String> {
        (0..grid.scrollback_lines())
            .map(|i| {
                grid.get_history_line(i)
                    .map(|l| l.to_string())
                    .unwrap_or_default()
            })
            .chain((0..grid.rows()).map(|r| {
                grid.storage
                    .row_at_screen(r)
                    .map(ToString::to_string)
                    .unwrap_or_default()
            }))
            .collect()
    }
    fn logical(grid: &Grid) -> std::collections::BTreeSet<String> {
        let rows = (0..grid.scrollback_lines())
            .filter_map(|i| {
                grid.get_history_line(i)
                    .map(|l| (l.to_string(), l.is_wrapped()))
            })
            .chain((0..grid.rows()).filter_map(|r| {
                grid.storage
                    .row_at_screen(r)
                    .map(|row| (row.to_string(), row.is_wrapped()))
            }));
        let mut lines: Vec<String> = Vec::new();
        for (text, continues) in rows {
            match lines.last_mut() {
                Some(last) if continues => last.push_str(&text),
                _ => lines.push(text),
            }
        }
        lines
            .into_iter()
            .map(|line| line.trim_end().to_string())
            .collect()
    }
    fn drawn_after_the_cursor(grid: &Grid) -> bool {
        let (row, col) = (grid.cursor_row(), grid.cursor_col());
        (row..grid.rows()).any(|r| {
            grid.storage.row_at_screen(r).is_some_and(|cells| {
                let from = if r == row { col } else { 0 };
                (from..cells.cols()).any(|c| cells.get(c).is_some_and(|cell| !cell.is_empty()))
            })
        })
    }
    let mut checked = 0;
    let seeds = [
        0x9e37_79b9_7f4a_7c15,
        0x2545_f491_4f6c_dd1d,
        0xd1b5_4a32_d192_ed03,
        0x8cb9_2ba7_2f3d_8dd7,
        0x94d0_49bb_1331_11eb,
        0xbf58_476d_1ce4_e5b9,
        0xfb5a_3753_9839_4d27,
        0x4c0e_d98a_b144_8f08,
    ];
    for (law, seed) in [ResizeLaw::Rewrap, ResizeLaw::PushedRows]
        .into_iter()
        .flat_map(|law| seeds.map(|seed| (law, seed)))
    {
        let steps = script(seed, 60);
        for at in 0..steps.len() {
            let grown = |one_pass: bool| {
                let mut grid = Grid::with_scrollback(8, 12, 1000);
                grid.set_resize_law(law);
                for (step, &(r, rows, cols)) in steps[..at].iter().enumerate() {
                    feed(&mut grid, step, r);
                    grid.resize(rows, cols);
                }
                let (r, rows, cols) = steps[at];
                feed(&mut grid, at, r);
                let drawn_below = drawn_after_the_cursor(&grid);
                if one_pass {
                    grid.forget_handback();
                } else {
                    grid.drop_handback();
                }
                grid.resize(rows, cols);
                grid.assert_invariants();
                (grid, drawn_below)
            };
            let ((split, drawn_below), (one, _)) = (grown(false), grown(true));
            let label = format!("{law:?} seed {seed:#x} step {at}: {:?}", steps[at]);
            if drawn_below {
                let kept: String = lines(&split).concat().split_whitespace().collect();
                for line in logical(&one) {
                    let line: String = line.split_whitespace().collect();
                    assert!(
                        kept.contains(&line),
                        "{label}: the resize lost {line:?}\nresize {:?}\none pass {:?}",
                        lines(&split),
                        lines(&one)
                    );
                }
            } else {
                checked += 1;
                assert_eq!(picture(&split), picture(&one), "{label}");
                assert_eq!(split.display_offset(), one.display_offset(), "{label}");
                assert_eq!(lines(&split), lines(&one), "{label}");
            }
        }
    }
    assert!(checked > 360, "only {checked} steps compared whole");
}

#[test]
fn a_rewrap_owes_the_rows_it_pushes_above_the_trims() {
    for distance in [32_767_u16, 32_768, 35_980, u16::MAX] {
        let offset = isize::try_from(distance).unwrap();
        let above = super::Edge::after_rewrap(-offset, 0, 2, 0);
        assert_eq!(above.top(), -offset);
        assert_eq!(
            above.scrolled(1, ResizeLaw::Rewrap).top(),
            -isize::try_from(distance.saturating_add(1)).unwrap()
        );
        let below = super::Edge::after_rewrap(offset, 2, 2, 0);
        assert_eq!(below.top(), offset);
    }
    let long = |grid: &mut Grid| {
        for i in 0..12 {
            write_line(grid, &format!("h{i}"));
        }
        grid.erase_screen();
        grid.set_cursor(0, 0);
        for i in 0..3 {
            write_line(grid, &format!("LLLLLLLLL{i}"));
        }
    };
    for scrolled in [false, true] {
        let mut never = Grid::with_scrollback(8, 10, 100);
        long(&mut never);
        never.resize(8, 5);

        let mut grid = Grid::with_scrollback(8, 10, 100);
        long(&mut grid);
        grid.resize(5, 10);
        assert_eq!(grid.trimmed_rows(), 3);
        grid.resize(5, 5);
        assert_eq!(
            (grid.trimmed_rows(), grid.handback_above()),
            (4, 2),
            "two rows pushed above the trims"
        );
        if scrolled {
            write_line(&mut never, "x");
            write_line(&mut grid, "x");
            assert_eq!((grid.trimmed_rows(), grid.handback_above()), (3, 3));
        }
        grid.resize(8, 5);
        assert_eq!(picture(&grid), picture(&never), "scrolled {scrolled}");
        grid.assert_invariants();
    }
}

#[test]
fn a_grid_without_history_owes_nothing_here() {
    let mut grid = Grid::with_scrollback(8, 10, 0);
    grid.set_cursor(2, 0);
    grid.resize(7, 10);
    assert_eq!(grid.trimmed_rows(), 0);
    assert_eq!(grid.resize_undo_rows(), 0, "a trimmed blank is not a row");
    grid.drop_resize_undo();
    grid.resize(8, 10);
    let grow = grid.take_last_resize_shape();
    assert_eq!((grow.revealed, grow.appended), (0, 1));

    let mut grid = Grid::with_scrollback(4, 10, 0);
    for r in 0..4 {
        grid.set_cursor(r, 0);
        grid.write_char('x');
    }
    grid.resize(3, 10);
    grid.resize(4, 10);
    assert_eq!(grid.take_last_resize_shape().restored_top, 1);
    assert_eq!(grid.handback_rows(), 0, "the undo's row is not owed");
}

#[test]
fn the_handback_is_seeded_from_a_signed_count() {
    for path in [&[7u16][..], &[9], &[3, 2]] {
        let flapped = || {
            let mut grid = cleared_with_history(8, 12, 2);
            for &h in path {
                grid.resize(h, 10);
            }
            grid
        };
        let (owed, above) = (flapped().handback_rows(), flapped().handback_above());
        assert_ne!(owed, 0, "{path:?}");
        let mut seeded = flapped();
        seeded.forget_handback();
        seeded.seed_handback(owed, above);
        assert_eq!(
            (seeded.handback_rows(), seeded.handback_above()),
            (owed, above)
        );
        seeded.resize(8, 10);
        assert_eq!(
            picture(&seeded),
            picture(&cleared_with_history(8, 12, 2)),
            "{path:?}"
        );
    }
    let mut grid = cleared_with_history(8, 12, 2);
    for distance in [4096_u16, 4097, 32_768, u16::MAX] {
        let offset = isize::try_from(distance).unwrap();
        for edge in [
            super::Edge::after_rewrap(-offset, 0, 2, 0),
            super::Edge::after_rewrap(offset, 2, 2, 0),
        ] {
            grid.set_handback(edge);
            let saved = (grid.handback_rows(), grid.handback_above());
            grid.forget_handback();
            grid.seed_handback(saved.0, saved.1);
            assert_eq!((grid.handback_rows(), grid.handback_above()), saved);
            assert_eq!(grid.handback_owed(true), edge);
        }
    }
    grid.seed_handback(i32::MIN, u16::MAX);
    assert_eq!(grid.revealed_rows(), u16::MAX);
    assert_eq!(grid.handback_above(), u16::MAX);
    grid.seed_handback(0, 3);
    assert_eq!((grid.handback_rows(), grid.handback_above()), (0, 0));
}

fn zoom_shapes(history: u16, lines: u16) -> [(&'static str, Grid); 4] {
    [
        ("ring", cleared_with_history(8, history, lines)),
        (
            "tiered ring 100",
            tiered_cleared_with_history(8, 100, history, lines),
        ),
        (
            "tiered ring 2",
            tiered_cleared_with_history(8, 2, history, lines),
        ),
        (
            "tiered ring 0",
            tiered_cleared_with_history(8, 0, history, lines),
        ),
    ]
}

#[test]
fn a_flap_that_changes_the_width_too_is_an_identity() {
    assert_flaps_are_identities(&[
        &[(7, 9), (8, 10)],
        &[(9, 11), (8, 10)],
        &[(7, 10), (7, 9), (8, 9), (8, 10)],
        &[(6, 8), (7, 9), (8, 10)],
        &[(7, 9), (9, 11), (8, 10)],
        &[(7, 12), (8, 12), (8, 10)],
        &[(9, 11), (6, 8), (10, 12), (8, 10)],
    ]);
}

#[test]
fn a_flap_with_a_taller_and_narrower_half_is_an_identity() {
    assert_flaps_are_identities(&[&[(9, 9), (8, 10)], &[(5, 12), (8, 10)]]);
}

fn assert_flaps_are_identities(paths: &[&[(u16, u16)]]) {
    for (history, lines) in [(12, 2), (9, 2), (12, 7)] {
        for path in paths {
            for (shape, mut grid) in zoom_shapes(history, lines) {
                let before = picture(&grid);
                for &(rows, cols) in *path {
                    grid.resize(rows, cols);
                }
                assert_eq!(
                    picture(&grid),
                    before,
                    "{shape}, history {history}, {lines} lines: {path:?}"
                );
                grid.assert_invariants();
            }
        }
    }
}

#[test]
fn a_bottom_push_flap_is_an_identity() {
    let numbered = |cursor: u16, blank_row: bool| {
        let mut grid = Grid::with_scrollback(8, 10, 100);
        for i in 1..=19 {
            write_line(&mut grid, &i.to_string());
        }
        if blank_row {
            write_line(&mut grid, "20");
        } else {
            grid.write_char('2');
            grid.write_char('0');
        }
        grid.set_cursor(cursor, 0);
        grid
    };
    let state = |grid: &Grid| {
        let history: Vec<String> = (0..grid.scrollback_lines())
            .map(|i| {
                grid.get_history_line(i)
                    .map(|l| l.to_string())
                    .unwrap_or_default()
            })
            .collect();
        (picture(grid), history)
    };

    let mut grid = numbered(0, true);
    grid.resize(6, 10);
    let shrink = grid.take_last_resize_shape();
    assert_eq!((shrink.trimmed, shrink.pushed), (1, 1));
    grid.resize(8, 10);
    let grow = grid.take_last_resize_shape();
    assert_eq!(
        (grow.restored_bottom, grow.revealed, grow.appended),
        (1, 0, 1)
    );
    assert_eq!(state(&grid), state(&numbered(0, true)));

    let linked = || {
        let mut grid = numbered(0, true);
        for row in [5u16, 6] {
            let url: std::sync::Arc<str> = std::sync::Arc::from(format!("https://row/{row}"));
            grid.extras_mut()
                .get_or_create(crate::grid::CellCoord::new(row, 0))
                .set_hyperlink(Some(url));
        }
        grid
    };
    let link = |grid: &Grid, row: u16| {
        grid.extras()
            .get(crate::grid::CellCoord::new(row, 0))
            .and_then(|e| e.hyperlink())
            .map(ToString::to_string)
    };
    for (down, up) in [(&[5u16][..], &[8u16][..]), (&[7, 6, 5], &[6, 7, 8])] {
        let mut grid = linked();
        for &h in down {
            grid.resize(h, 10);
        }
        let newest: Vec<String> = (0..2)
            .map(|i| grid.history_line_rev(1 - i).unwrap().to_string())
            .collect();
        assert_eq!(
            newest,
            ["19", "20"],
            "{down:?}: pushed in the screen's order"
        );
        for &h in up {
            grid.resize(h, 10);
        }
        assert_eq!(state(&grid), state(&numbered(0, true)), "{down:?} {up:?}");
        for row in [5u16, 6] {
            assert_eq!(
                link(&grid, row),
                Some(format!("https://row/{row}")),
                "{down:?}"
            );
        }
        grid.assert_invariants();
    }

    let gapped = || {
        let mut grid = Grid::with_scrollback(3, 10, 100);
        for line in ["h0", "h1", "A", ""] {
            write_line(&mut grid, line);
        }
        grid.write_char('B');
        grid.set_cursor(0, 0);
        grid
    };
    let mut grid = gapped();
    for h in [2, 1, 3] {
        grid.resize(h, 10);
    }
    assert_eq!(state(&grid), state(&gapped()));
    let gaps = || {
        let mut grid = Grid::with_scrollback(8, 10, 100);
        for i in 1..=20 {
            let line = if i % 3 == 0 {
                String::new()
            } else {
                i.to_string()
            };
            write_line(&mut grid, &line);
        }
        grid.write_char('x');
        grid.set_cursor(0, 0);
        grid
    };

    let range = 1u16..=12;
    let shapes: [(&str, &dyn Fn() -> Grid); 4] = [
        ("homed", &|| numbered(0, true)),
        ("cursor on row 2", &|| numbered(2, true)),
        ("no blank row", &|| numbered(0, false)),
        ("blank rows between, homed", &gaps),
    ];
    for (shape, start) in shapes {
        for a in range.clone() {
            for b in range.clone() {
                for c in range.clone() {
                    let mut grid = start();
                    let mut seen = std::collections::BTreeMap::new();
                    seen.insert(8u16, state(&grid));
                    for h in [a, b, c, 8] {
                        grid.resize(h, 10);
                        let now = state(&grid);
                        let first = seen.entry(h).or_insert_with(|| now.clone());
                        assert_eq!(&now, first, "{shape}: [{a}, {b}, {c}, 8] at {h}");
                    }
                    grid.assert_invariants();
                }
            }
        }
    }
}

#[test]
fn a_push_of_drawn_on_appended_rows_forgets_what_is_owed() {
    let mut grid = Grid::with_scrollback(3, 10, 100);
    for s in ["h0", "a", "b"] {
        write_line(&mut grid, s);
    }
    grid.write_char('c');
    grid.resize(6, 10);
    assert_eq!(grid.revealed_rows(), 1, "h0 revealed, two blanks appended");
    for s in ["d", "e"] {
        grid.carriage_return();
        grid.line_feed();
        grid.write_char(s.chars().next().unwrap());
    }
    grid.set_cursor(1, 0);
    assert_eq!(grid.revealed_rows(), 1, "the cursor stays below h0");
    grid.resize(1, 10);
    let shrink = grid.take_last_resize_shape();
    assert_eq!((shrink.trimmed, shrink.demoted, shrink.pushed), (0, 1, 4));
    assert_eq!((grid.trimmed_rows(), grid.revealed_rows()), (0, 0));
    grid.resize(6, 10);
    let mut lines: Vec<String> = (0..grid.scrollback_lines())
        .filter_map(|i| grid.get_history_line(i).map(|l| l.to_string()))
        .chain(screen(&grid))
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    lines.sort();
    assert_eq!(lines, ["a", "b", "c", "d", "e", "h0"]);
    grid.assert_invariants();
}

#[test]
fn an_unscroll_forgets_the_pushed_rows() {
    let mut grid = Grid::with_scrollback(4, 10, 100);
    for i in 1..=5 {
        write_line(&mut grid, &i.to_string());
    }
    grid.write_char('6');
    grid.set_cursor(0, 0);
    grid.resize(2, 10);
    assert_eq!(grid.take_last_resize_shape().pushed, 2, "5 and 6 pushed");
    assert_eq!(grid.unscroll_from_scrollback(1), 1);
    assert_eq!(screen(&grid), ["6", "3"]);
    grid.resize(4, 10);
    assert_eq!(grid.take_last_resize_shape().restored_bottom, 0);
    assert_eq!(screen(&grid), ["2", "5", "6", "3"]);
    assert_eq!(grid.scrollback_lines(), 1);
    grid.assert_invariants();
}

#[test]
fn an_erase_forgets_the_pushed_rows() {
    let history = |grid: &Grid| -> Vec<String> {
        (0..grid.scrollback_lines())
            .filter_map(|i| grid.get_history_line(i).map(|l| l.to_string()))
            .collect()
    };
    let erased = |law: ResizeLaw, ed2: bool| {
        let mut grid = Grid::with_scrollback(8, 10, 100);
        grid.set_resize_law(law);
        for i in 0..10 {
            write_line(&mut grid, &format!("h{i}"));
        }
        grid.erase_screen();
        grid.set_cursor(0, 0);
        for row in ["A", "B", "C", "D", "E", "F", "G"] {
            write_line(&mut grid, row);
        }
        grid.write_char('H');
        grid.set_cursor(0, 0);
        grid.resize(6, 10);
        assert_eq!(grid.take_last_resize_shape().pushed, 2, "G and H pushed");
        if ed2 {
            grid.erase_screen();
        } else {
            grid.erase_to_end_of_screen();
        }
        grid.write_char('n');
        grid.set_cursor(0, 0);
        grid
    };
    let old = ["h0", "h1", "h2"];
    for ed2 in [true, false] {
        for heights in [&[8u16][..], &[4, 8], &[3, 5, 8]] {
            let mut grid = erased(ResizeLaw::Rewrap, ed2);
            for &h in heights {
                grid.resize(h, 10);
                assert!(
                    history(&grid).iter().all(|line| !line.trim().is_empty()),
                    "ED 2 {ed2}, {heights:?} at {h}: a blank row archived: {:?}",
                    history(&grid)
                );
            }
            assert_eq!(
                screen(&grid),
                ["G", "H", "n", "", "", "", "", ""],
                "ED 2 {ed2}, {heights:?}"
            );
            assert_eq!(history(&grid), old, "ED 2 {ed2}, {heights:?}");
            grid.assert_invariants();
        }
        let mut grid = erased(ResizeLaw::PushedRows, ed2);
        grid.resize(8, 10);
        assert_eq!(
            screen(&grid),
            ["n", "", "", "", "", "", "G", "H"],
            "ED 2 {ed2}: the law before"
        );
    }
    let mut grid = erased(ResizeLaw::Rewrap, true);
    grid.resize(8, 10);
    grid.erase_screen();
    grid.set_cursor(0, 0);
    for row in ["A", "B", "C", "D", "E", "F", "G"] {
        write_line(&mut grid, row);
    }
    grid.write_char('H');
    grid.set_cursor(0, 0);
    grid.resize(6, 10);
    for row in 0..6 {
        grid.set_cursor(row, 0);
        grid.erase_line();
    }
    grid.set_cursor(0, 0);
    grid.write_char('n');
    grid.set_cursor(0, 0);
    grid.resize(4, 10);
    assert!(
        history(&grid).iter().all(|line| !line.trim().is_empty()),
        "EL: a blank row archived: {:?}",
        history(&grid)
    );
    grid.resize(8, 10);
    assert_eq!(screen(&grid), ["G", "H", "n", "", "", "", "", ""], "EL");
    grid.assert_invariants();
}

#[test]
fn a_push_cuts_a_line_at_its_seams_and_the_grow_back_mends_it() {
    fn lines(grid: &Grid) -> (Vec<String>, Vec<bool>) {
        let rows: Vec<(String, bool)> = (0..grid.scrollback_lines())
            .filter_map(|i| {
                grid.get_history_line(i)
                    .map(|l| (l.to_string(), l.is_wrapped()))
            })
            .chain(
                (0..grid.rows())
                    .filter_map(|r| grid.row(r).map(|row| (row.to_string(), row.is_wrapped()))),
            )
            .collect();
        let mut lines: Vec<String> = Vec::new();
        for (text, continues) in &rows {
            match lines.last_mut() {
                Some(last) if *continues => last.push_str(text),
                _ => lines.push(text.clone()),
            }
        }
        let lines = lines
            .into_iter()
            .map(|line| line.trim_end().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        (lines, rows.into_iter().map(|(_, wrap)| wrap).collect())
    }
    let write = |grid: &mut Grid, line: &str| {
        for c in line.chars() {
            grid.write_char_wrap(c);
        }
    };
    let straddle = |law: ResizeLaw| {
        let mut grid = Grid::with_scrollback(6, 10, 100);
        grid.set_resize_law(law);
        for i in 0..8 {
            write_line(&mut grid, &format!("h{i}"));
        }
        grid.erase_screen();
        grid.set_cursor(0, 0);
        for row in ["a", "b", "c"] {
            write_line(&mut grid, row);
        }
        write(&mut grid, &"L".repeat(25));
        grid.set_cursor(0, 0);
        grid
    };
    let on_the_tail = |law: ResizeLaw| {
        let mut grid = Grid::with_scrollback(6, 10, 100);
        grid.set_resize_law(law);
        for i in 0..4 {
            write_line(&mut grid, &format!("h{i}"));
        }
        write(&mut grid, &"L".repeat(25));
        for row in ["", "a", "b", "c"] {
            grid.carriage_return();
            grid.line_feed();
            write(&mut grid, row);
        }
        grid.set_cursor(0, 1);
        assert!(grid.row(0).is_some_and(|row| row.is_wrapped()));
        grid
    };
    let shapes: [(&str, &dyn Fn(ResizeLaw) -> Grid); 2] = [
        ("straddling the bottom", &straddle),
        ("the cursor on its tail", &on_the_tail),
    ];
    let spliced = |grid: &Grid| {
        lines(grid)
            .0
            .iter()
            .any(|line| line.contains('L') && line.contains(|c: char| c != 'L'))
    };
    for (shape, start) in shapes {
        let before = lines(&start(ResizeLaw::Rewrap));
        for run in [&[4u16, 6][..], &[5, 4, 6], &[4, 5, 6], &[3, 5, 4, 6]] {
            let mut grid = start(ResizeLaw::Rewrap);
            for &h in run {
                grid.resize(h, 10);
                assert!(
                    !spliced(&grid),
                    "{shape}: {run:?} at {h}: {:?}",
                    lines(&grid)
                );
                grid.assert_invariants();
            }
            assert_eq!(lines(&grid), before, "{shape}: {run:?}");
        }
        let mut grid = start(ResizeLaw::PushedRows);
        grid.resize(4, 10);
        assert!(spliced(&grid), "{shape}: the law before splices");
    }
}

#[test]
fn a_zoom_flap_over_a_line_the_screen_top_cuts_is_an_identity() {
    let start = || {
        let mut grid = Grid::with_scrollback(8, 10, 100);
        for i in 0..4 {
            write_line(&mut grid, &format!("h{i}"));
        }
        for c in "abcdefghijklmnopqrstuvwxy".chars() {
            grid.write_char_wrap(c);
        }
        for row in ["a", "b", "c", "d", "e", ""] {
            grid.carriage_return();
            grid.line_feed();
            for c in row.chars() {
                grid.write_char(c);
            }
        }
        grid.set_cursor(0, 1);
        assert_eq!(screen(&grid)[0], "klmnopqrst");
        grid
    };
    for run in [
        &[(8u16, 9u16), (8, 10)][..],
        &[(8, 8), (8, 9), (8, 10)],
        &[(7, 9), (6, 8), (7, 9), (8, 10)],
        &[(8, 6), (5, 7), (8, 10)],
    ] {
        let mut grid = start();
        for &(rows, cols) in run {
            grid.resize(rows, cols);
            let mut straight = start();
            straight.resize(rows, cols);
            assert_eq!(
                picture(&grid),
                picture(&straight),
                "{run:?} at {rows}x{cols}"
            );
            grid.assert_invariants();
        }
        assert_eq!(picture(&grid), picture(&start()), "{run:?}");
    }
    let skewed = || {
        let mut grid = start();
        grid.resize(8, 9);
        assert_eq!(grid.window_skew(), 1, "10 cells in: one into `jklmnopqr`");
        grid
    };
    let mut grid = skewed();
    grid.set_cursor(3, 0);
    assert_eq!(grid.window_skew(), 1, "the cursor below the top keeps it");
    grid.set_cursor(0, 0);
    assert_eq!(grid.window_skew(), 0, "the cursor on the top row");
    let mut grid = skewed();
    grid.erase_screen();
    assert_eq!(grid.window_skew(), 0, "the screen erased");
    let mut grid = skewed();
    grid.set_cursor(7, 0);
    write_line(&mut grid, "x");
    assert_eq!(grid.window_skew(), 0, "a line scrolled past it");
}

#[test]
fn a_width_change_counts_the_pushed_rows_it_put_back_and_pushed() {
    let mut grid = Grid::with_scrollback(8, 10, 100);
    for i in 0..4 {
        write_line(&mut grid, &format!("h{i}"));
    }
    grid.erase_screen();
    grid.set_cursor(0, 0);
    for row in ["A", "B", "C", "D", "E", "F", "G"] {
        write_line(&mut grid, row);
    }
    grid.write_char('H');
    grid.set_cursor(0, 0);
    grid.resize(6, 10);
    assert_eq!(grid.take_last_resize_shape().pushed, 2);
    grid.resize(6, 9);
    let narrow = grid.take_last_resize_shape();
    assert_eq!(
        (narrow.restored_bottom, narrow.pushed, narrow.reflowed),
        (2, 2, true)
    );
    assert_eq!(screen(&grid), ["A", "B", "C", "D", "E", "F"]);
    grid.resize(8, 10);
    let back = grid.take_last_resize_shape();
    assert_eq!((back.restored_bottom, back.pushed), (2, 0));
    assert_eq!(screen(&grid), ["A", "B", "C", "D", "E", "F", "G", "H"]);
    grid.assert_invariants();
}

#[test]
fn every_size_a_zoom_run_returns_to_shows_the_same_screen() {
    let cols = |rows: u16| match rows.cmp(&8) {
        std::cmp::Ordering::Less => 9,
        std::cmp::Ordering::Equal => 10,
        std::cmp::Ordering::Greater => 11,
    };
    let range = 1u16..=12;
    for (history, lines) in [(12, 2), (9, 2), (12, 7)] {
        for a in range.clone() {
            for b in range.clone() {
                for c in range.clone() {
                    let mut grid = cleared_with_history(8, history, lines);
                    let mut seen = std::collections::BTreeMap::new();
                    seen.insert(8u16, picture(&grid));
                    for h in [a, b, c, 8] {
                        grid.resize(h, cols(h));
                        let now = picture(&grid);
                        let first = seen.entry(h).or_insert_with(|| now.clone());
                        assert_eq!(
                            &now, first,
                            "history {history}, {lines} lines: [{a}, {b}, {c}, 8] at {h}"
                        );
                    }
                    grid.assert_invariants();
                }
            }
        }
    }
}

#[test]
fn every_size_a_run_reaches_shows_what_one_resize_there_shows() {
    fn kept_lines(grid: &Grid) -> Vec<String> {
        let rows = (0..grid.scrollback_lines())
            .filter_map(|i| {
                grid.get_history_line(i)
                    .map(|l| (l.to_string(), l.is_wrapped()))
            })
            .chain((0..grid.rows()).filter_map(|r| {
                grid.storage
                    .row_at_screen(r)
                    .map(|row| (row.to_string(), row.is_wrapped()))
            }));
        let mut lines: Vec<String> = Vec::new();
        for (text, continues) in rows {
            match lines.last_mut() {
                Some(last) if continues => last.push_str(&text),
                _ => lines.push(text),
            }
        }
        let mut lines: Vec<String> = lines
            .into_iter()
            .map(|line| line.trim_end().to_string())
            .filter(|line| !line.is_empty())
            .collect();
        lines.sort();
        lines
    }
    fn text(lines: &[String]) -> String {
        let mut chars: Vec<char> = lines
            .iter()
            .flat_map(|line| line.chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        chars.sort_unstable();
        chars.into_iter().collect()
    }
    struct Shape {
        name: &'static str,
        lines: &'static [&'static str],
        cursor: Option<(u16, u16)>,
        tiered: bool,
        scrolled: bool,
    }
    const fn shape(
        name: &'static str,
        lines: &'static [&'static str],
        cursor: Option<(u16, u16)>,
        tiered: bool,
    ) -> Shape {
        Shape {
            name,
            lines,
            cursor,
            tiered,
            scrolled: false,
        }
    }
    const fn scrolled(
        name: &'static str,
        lines: &'static [&'static str],
        cursor: Option<(u16, u16)>,
        tiered: bool,
    ) -> Shape {
        Shape {
            scrolled: true,
            ..shape(name, lines, cursor, tiered)
        }
    }
    const SHORT: &[&str] = &["L0", "LLLLLLLLL1", "L2"];
    const HOMED: &[&str] = &["L0", "LLLLLLLLL1", "L2", "", "L4", "L5", "", "L7"];
    const MID: &[&str] = &["L0", "LLLLLLLLL1", "L2", "L3", "L4", "L5"];
    const ON_A_WRAP: &[&str] = &["L0", "LLLLLLLLLLLLLLLLLLLLLLLL1", "L2", "L3", "L4", "L5"];
    const LONG: &str = "LLLLLLLLLLLLLLLLLLLLLLLLL1";
    const SPLIT: &[&str] = &[LONG, "L2", "L3", "L4", "L5", "L6"];
    const SPLIT_ON_TAIL: &[&str] = &[LONG, "L2", "L3", "L4", "L5", "L6", ""];
    const SPLIT_PROMPT: &[&str] = &[LONG, "L2", "L3", "$ ", "", "", ""];
    const SHAPES: [Shape; 19] = [
        shape("a line that wraps below 10", SHORT, None, false),
        shape("the same, tiered", SHORT, None, true),
        shape(
            "a wrapped line",
            &["L0", "LLLLLLLLLLLLL1", "L2"],
            None,
            false,
        ),
        shape("long history", SHORT, None, false),
        shape(
            "a full screen",
            &["L0", "LLLLLLLLL1", "L2", "L3", "L4", "L5", "L6"],
            None,
            false,
        ),
        shape("a prompt", &["L0", "LLLLLLLLL1", "$ "], Some((2, 2)), false),
        shape("homed", HOMED, Some((0, 0)), false),
        shape("homed, tiered", HOMED, Some((0, 0)), true),
        shape(
            "homed over a blank tail",
            &["L0", "LLLLLLLLL1", "L2", "", "L4"],
            Some((0, 0)),
            false,
        ),
        shape("mid-screen", MID, Some((2, 1)), false),
        shape("mid-screen, tiered", MID, Some((2, 1)), true),
        shape(
            "on a wrapped line's second row",
            ON_A_WRAP,
            Some((2, 1)),
            false,
        ),
        shape("the same, tiered ", ON_A_WRAP, Some((2, 1)), true),
        scrolled("the screen's top inside a line", SPLIT, None, false),
        scrolled("the same, tiered  ", SPLIT, None, true),
        scrolled("the cursor on that top", SPLIT_ON_TAIL, Some((0, 1)), false),
        scrolled("the same, tiered   ", SPLIT_ON_TAIL, Some((0, 1)), true),
        scrolled("a prompt under that top", SPLIT_PROMPT, Some((4, 2)), false),
        scrolled("the same, tiered    ", SPLIT_PROMPT, Some((4, 2)), true),
    ];
    let start = |shape: &Shape| {
        let mut grid = if shape.tiered {
            let store = aterm_scrollback::Scrollback::new(100, 1000, 10_000_000);
            Grid::with_tiered_scrollback(8, 10, 2, store)
        } else {
            Grid::with_scrollback(8, 10, 100)
        };
        let write = |grid: &mut Grid, line: &str| {
            for c in line.chars() {
                grid.write_char_wrap(c);
            }
        };
        for i in 0..12 {
            let line = if shape.name == "long history" {
                format!("hhhhhhhhh{i}xx")
            } else {
                format!("h{i}")
            };
            write(&mut grid, &line);
            grid.carriage_return();
            grid.line_feed();
        }
        if !shape.scrolled {
            grid.erase_screen();
            grid.set_cursor(0, 0);
        }
        for (i, line) in shape.lines.iter().enumerate() {
            write(&mut grid, line);
            if i + 1 < shape.lines.len() || shape.cursor.is_none() {
                grid.carriage_return();
                grid.line_feed();
            }
        }
        if let Some((row, col)) = shape.cursor {
            grid.set_cursor(row, col);
        }
        if shape.scrolled {
            assert!(
                grid.row(0).is_some_and(|row| row.is_wrapped()),
                "{}: the screen's top row continues a line",
                shape.name
            );
        }
        grid
    };
    let mut state: u64 = 0x94d0_49bb_1331_11eb;
    let runs: Vec<Vec<(u16, u16, bool)>> = (0..300)
        .map(|_| {
            let mut cols = 10;
            (0..5)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    let rows = 3 + u16::try_from((state >> 20) % 8).unwrap();
                    if (state >> 40).is_multiple_of(2) {
                        cols = [6, 7, 8, 9, 10, 11, 12, 14]
                            [usize::try_from((state >> 28) % 8).unwrap()];
                    }
                    (rows, cols, (state >> 50).is_multiple_of(3))
                })
                .collect()
        })
        .collect();
    let census = |law: ResizeLaw| {
        let mut failed = 0;
        let mut first = None;
        for shape in &SHAPES {
            let writes = shape.cursor.is_none() || shape.name.starts_with("a prompt");
            for run in &runs {
                let mut grid = start(shape);
                grid.set_resize_law(law);
                let mut lines = kept_lines(&grid);
                let mut chars = text(&lines);
                let mut written: Vec<String> = Vec::new();
                for (i, &(rows, cols, write)) in run.iter().enumerate() {
                    grid.resize(rows, cols);
                    grid.assert_invariants();
                    let mut straight = start(shape);
                    straight.set_resize_law(law);
                    for line in &written {
                        write_line(&mut straight, line);
                    }
                    straight.resize(rows, cols);
                    let kept = kept_lines(&grid);
                    let whole = text(&kept) == chars
                        && kept
                            .iter()
                            .all(|piece| lines.iter().any(|line| line.contains(piece.as_str())));
                    if !whole
                        || kept != kept_lines(&straight)
                        || picture(&grid) != picture(&straight)
                    {
                        failed += 1;
                        first.get_or_insert_with(|| {
                            format!(
                                "{}: {run:?} at {i}: {:?} {kept:?}, straight there {:?} {:?}",
                                shape.name,
                                picture(&grid),
                                picture(&straight),
                                kept_lines(&straight)
                            )
                        });
                        break;
                    }
                    if write && writes {
                        let line = format!("o{i}");
                        let mut before = kept;
                        write_line(&mut grid, &line);
                        written.push(line);
                        let after = kept_lines(&grid);
                        chars = text(&after);
                        for line in after {
                            match before.iter().position(|old| *old == line) {
                                Some(at) => drop(before.swap_remove(at)),
                                None => lines.push(line),
                            }
                        }
                    }
                }
            }
        }
        (failed, first)
    };
    let (failed, first) = census(ResizeLaw::Rewrap);
    assert_eq!(failed, 0, "{}", first.unwrap_or_default());
    let (failed, _) = census(ResizeLaw::PushedRows);
    assert!(
        failed > runs.len(),
        "only {failed} runs fail under the old law"
    );
}

#[test]
fn a_rows_grow_reveals_the_history_a_tier_holds() {
    for lines in [2u16, 7] {
        for ring in [0usize, 2] {
            for promote in [false, true] {
                for heights in [&[9u16][..], &[11], &[7, 8], &[5, 10, 8], &[3, 2, 9]] {
                    let at = format!("lines {lines}, ring {ring}, promoted {promote}");
                    let mut tiered = tiered_cleared_with_history(8, ring, 12, lines);
                    if promote {
                        tiered.drain_lazy_buffer();
                    }
                    let mut reference = cleared_with_history(8, 12, lines);
                    for &h in heights {
                        tiered.resize(h, 10);
                        reference.resize(h, 10);
                        assert_eq!(
                            picture(&tiered),
                            picture(&reference),
                            "{at}: {heights:?} at {h}"
                        );
                    }
                    tiered.assert_invariants();
                }
            }
        }
    }
}

#[test]
fn an_erase_from_home_forgets_the_revealed_rows() {
    let mut never = cleared_with_history(8, 12, 2);
    let mut flapped = cleared_with_history(8, 12, 2);
    flapped.resize(9, 10);
    for grid in [&mut never, &mut flapped] {
        grid.set_cursor(0, 0);
        grid.erase_to_end_of_screen();
        for i in 0..3 {
            write_line(grid, &format!("N{i}"));
        }
    }
    assert_eq!(flapped.revealed_rows(), 0, "a full clear from home");
    flapped.resize(8, 10);
    assert_eq!(screen(&flapped), screen(&never));
    assert_eq!(flapped.cursor_row(), never.cursor_row());
    flapped.assert_invariants();

    let mut grid = cleared_with_history(8, 12, 2);
    grid.resize(9, 10);
    grid.set_cursor(3, 0);
    grid.erase_to_end_of_screen();
    assert_eq!(grid.revealed_rows(), 1, "an ED 0 below the top");
}

#[test]
fn a_redraw_from_home_forgets_the_revealed_rows() {
    type Home = (&'static str, fn(&mut Grid));
    let homes: [Home; 3] = [
        ("CUP", |grid| grid.set_cursor(0, 0)),
        ("CUU", |grid| grid.cursor_up(u16::MAX)),
        ("RI", |grid| {
            while grid.cursor_row() > 0 {
                grid.reverse_line_feed();
            }
        }),
    ];
    for (how, home) in homes {
        let mut never = cleared_with_history(8, 12, 2);
        let mut flapped = cleared_with_history(8, 12, 2);
        flapped.resize(9, 10);
        for grid in [&mut never, &mut flapped] {
            home(grid);
            for i in 0..3 {
                write_line(grid, &format!("N{i}p"));
            }
        }
        assert_eq!(flapped.revealed_rows(), 0, "{how}: drawn from the top");
        flapped.resize(8, 10);
        assert_eq!(screen(&flapped), screen(&never), "{how}");
        assert_eq!(flapped.cursor_row(), never.cursor_row(), "{how}");
        flapped.assert_invariants();
    }

    let mut grid = cleared_with_history(8, 12, 2);
    grid.resize(9, 10);
    grid.cursor_up(2);
    assert_eq!(grid.revealed_rows(), 1, "up over the drawn rows only");
}
