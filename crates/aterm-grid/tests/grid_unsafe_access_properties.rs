// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Property tests for unsafe grid access paths over generated inputs.

use aterm_grid::{Cell, Grid, PageStore, Row};
use aterm_spec::rng::XorShift;

fn range(rng: &mut XorShift, lo: u16, hi: u16) -> u16 {
    u16::try_from(rng.range(usize::from(lo), usize::from(hi))).expect("below hi")
}

fn char_in(rng: &mut XorShift, lo: char, hi: char) -> char {
    char::from(range(rng, lo as u16, hi as u16 + 1) as u8)
}

#[test]
fn row_get_unchecked_matches_safe_access() {
    let mut rng = XorShift::new(0x0A11_0C0D);
    for _ in 0..256 {
        let cols = range(&mut rng, 1, 200);
        let mut pages = PageStore::new();
        // SAFETY: Test-local `pages` outlives `row` for the full scope.
        let mut row = unsafe { Row::new(cols, &mut pages) };

        for _ in 0..range(&mut rng, 0, 200) {
            let (col, ch) = (range(&mut rng, 0, 300), char_in(&mut rng, ' ', '~'));
            if col < cols {
                row.write_char(col, ch);
            }
        }

        for col in 0..cols {
            let safe = row.get(col).expect("col from 0..cols must be in-bounds");
            // SAFETY: `col` is drawn from `0..cols`.
            let unchecked = unsafe { row.get_unchecked(col) };
            assert_eq!(unchecked.char(), safe.char(), "col {col} (cols={cols})");
            assert_eq!(unchecked.flags(), safe.flags(), "col {col} (cols={cols})");
        }

        assert!(row.get(cols).is_none(), "safe get must reject col == cols");
    }
}

#[test]
fn row_get_unchecked_mut_updates_only_target_cell() {
    let mut rng = XorShift::new(0x007A_26E7);
    for _ in 0..256 {
        let cols = range(&mut rng, 1, 160);
        let target = range(&mut rng, 0, 300) % cols;
        let replacement = char_in(&mut rng, '!', '~');
        let mut pages = PageStore::new();
        // SAFETY: Test-local `pages` outlives `row` for the full scope.
        let mut row = unsafe { Row::new(cols, &mut pages) };

        for col in 0..cols {
            let offset = u8::try_from(col % 26).expect("col % 26 always fits in u8");
            row.write_char(col, char::from(b'a' + offset));
        }

        let before: Vec<char> = (0..cols)
            .map(|col| {
                row.get(col)
                    .expect("row initialized across all cols")
                    .char()
            })
            .collect();

        // SAFETY: modulo ensures `target < cols`.
        unsafe { *row.get_unchecked_mut(target) = Cell::new(replacement) };

        for col in 0..cols {
            let actual = row.get(col).expect("in bounds").char();
            if col == target {
                assert_eq!(actual, replacement, "target col {col} should be updated");
            } else {
                assert_eq!(actual, before[usize::from(col)], "col {col} changed");
            }
        }
    }
}

#[test]
fn grid_row_storage_does_not_alias_across_rows() {
    let mut rng = XorShift::new(0x0A11_A5ED);
    for _ in 0..256 {
        let rows = range(&mut rng, 2, 40);
        let cols = range(&mut rng, 4, 120);
        let source_row = range(&mut rng, 0, 40) % rows;
        let mut mutate_row = range(&mut rng, 0, 40) % rows;
        if source_row == mutate_row {
            mutate_row = (mutate_row + 1) % rows;
        }
        let mut grid = Grid::new(rows, cols);

        {
            let source = grid
                .row_mut(source_row)
                .expect("source row index must be in bounds");
            for col in 0..cols {
                let offset = u8::try_from(col % 26).expect("col % 26 always fits in u8");
                source.write_char(col, char::from(b'a' + offset));
            }
        }

        let row_chars = |grid: &Grid, r: u16| -> Vec<char> {
            let row = grid.row(r).expect("row in bounds");
            (0..cols)
                .map(|col| row.get(col).expect("col in bounds").char())
                .collect()
        };
        let baseline = row_chars(&grid, source_row);

        {
            let row = grid
                .row_mut(mutate_row)
                .expect("mutate row index must be in bounds");
            for _ in 0..range(&mut rng, 1, 200) {
                let (col, ch) = (range(&mut rng, 0, 200), char_in(&mut rng, 'A', 'Z'));
                if col < cols {
                    row.write_char(col, ch);
                }
            }
        }

        assert_eq!(
            row_chars(&grid, source_row),
            baseline,
            "source row {source_row} changed after mutating row {mutate_row}"
        );
    }
}
