// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::*;

fn transcript(grid: &Grid) -> Vec<(String, bool)> {
    let history = (0..grid.scrollback_lines()).map(|i| {
        let line = grid.get_history_line(i).expect("retained history");
        (line.to_string().trim_end().to_owned(), line.is_wrapped())
    });
    let screen = (0..=grid.cursor_row()).map(|i| {
        let row = grid.row(i).expect("visible row");
        (row.to_string().trim_end().to_owned(), row.is_wrapped())
    });
    history.chain(screen).collect()
}

#[test]
fn synchronous_width_changes_keep_the_total_and_write_order() {
    for payload in [0, 9, 37] {
        for width in [13, 29] {
            let mut tiered =
                Grid::with_tiered_scrollback(6, 20, 8, Scrollback::new(4, 16, 10_000_000));
            tiered.set_scrollback_line_limit(Some(38));
            let mut twin = Grid::with_scrollback(6, 20, 38);
            for n in 0..76 {
                let text = format!("L{n}:{}", "x".repeat(payload));
                for grid in [&mut tiered, &mut twin] {
                    for ch in text.chars() {
                        grid.write_char_wrap(ch);
                    }
                    grid.carriage_return();
                    grid.line_feed();
                }
            }
            tiered.drain_lazy_buffer();
            assert_eq!(tiered.scrollback_lines(), 38);
            for cols in [width, 20, width] {
                tiered.resize(6, cols);
                twin.resize(6, cols);
                tiered.drain_lazy_buffer();
                assert_eq!(
                    transcript(&tiered),
                    transcript(&twin),
                    "payload {payload}, width {cols}: preserve the ring-only twin's history",
                );
                tiered.assert_invariants();
            }
        }
    }
}
