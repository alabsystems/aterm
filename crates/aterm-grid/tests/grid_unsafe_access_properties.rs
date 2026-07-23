// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Property tests for unsafe grid access paths.
//!
//! Migrated from aterm-core as part of #6556 Batch 3.

use aterm_grid::{Cell, Grid, PageStore, Row};
use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    /// In-bounds unchecked reads must match safe reads for all columns.
    #[test]
    fn row_get_unchecked_matches_safe_access(
        cols in 1u16..200,
        writes in prop::collection::vec((0u16..300, prop::char::range(' ', '~')), 0..200),
    ) {
        let mut pages = PageStore::new();
        // SAFETY: Test-local `pages` outlives `row` for the full scope.
        let mut row = unsafe { Row::new(cols, &mut pages) };

        for (col, ch) in writes {
            if col < cols {
                row.write_char(col, ch);
            }
        }

        for col in 0..cols {
            let safe = row.get(col).expect("col from 0..cols must be in-bounds");
            // SAFETY: `col` is drawn from `0..cols`.
            let unchecked = unsafe { row.get_unchecked(col) };
            prop_assert_eq!(
                unchecked.char(),
                safe.char(),
                "char mismatch at col {} (cols={})",
                col,
                cols
            );
            prop_assert_eq!(
                unchecked.flags(),
                safe.flags(),
                "flags mismatch at col {} (cols={})",
                col,
                cols
            );
        }

        prop_assert!(row.get(cols).is_none(), "safe get must reject col == cols");
    }

    /// In-bounds unchecked mutable access must update only the targeted cell.
    #[test]
    fn row_get_unchecked_mut_updates_only_target_cell(
        cols in 1u16..160,
        target in 0u16..300,
        replacement in prop::char::range('!', '~'),
    ) {
        let mut pages = PageStore::new();
        // SAFETY: Test-local `pages` outlives `row` for the full scope.
        let mut row = unsafe { Row::new(cols, &mut pages) };

        for col in 0..cols {
            let offset = u8::try_from(col % 26).expect("col % 26 always fits in u8");
            row.write_char(col, char::from(b'a' + offset));
        }

        let before: Vec<char> = (0..cols)
            .map(|col| row.get(col).expect("row initialized across all cols").char())
            .collect();

        let target = target % cols;
        // SAFETY: modulo ensures `target < cols`.
        unsafe { *row.get_unchecked_mut(target) = Cell::new(replacement) };

        for col in 0..cols {
            let actual = row
                .get(col)
                .expect("all tested columns remain in-bounds")
                .char();
            if col == target {
                prop_assert_eq!(
                    actual,
                    replacement,
                    "target col {} should be updated",
                    col
                );
            } else {
                prop_assert_eq!(
                    actual,
                    before[usize::from(col)],
                    "non-target col {} changed unexpectedly",
                    col
                );
            }
        }
    }

    /// Distinct grid rows must not alias each other under random writes.
    #[test]
    fn grid_row_storage_does_not_alias_across_rows(
        rows in 2u16..40,
        cols in 4u16..120,
        source_row in 0u16..40,
        mutate_row in 0u16..40,
        writes in prop::collection::vec((0u16..200, prop::char::range('A', 'Z')), 1..200),
    ) {
        let mut grid = Grid::new(rows, cols);
        let source_row = source_row % rows;
        let mut mutate_row = mutate_row % rows;
        if source_row == mutate_row {
            mutate_row = (mutate_row + 1) % rows;
        }

        {
            let source = grid
                .row_mut(source_row)
                .expect("source row index must be in bounds");
            for col in 0..cols {
                let offset = u8::try_from(col % 26).expect("col % 26 always fits in u8");
                source.write_char(col, char::from(b'a' + offset));
            }
        }

        let baseline: Vec<char> = (0..cols)
            .map(|col| {
                grid.row(source_row)
                    .expect("source row should still exist")
                    .get(col)
                    .expect("source col should be in bounds")
                    .char()
            })
            .collect();

        {
            let row = grid
                .row_mut(mutate_row)
                .expect("mutate row index must be in bounds");
            for (col, ch) in writes {
                if col < cols {
                    row.write_char(col, ch);
                }
            }
        }

        let source = grid
            .row(source_row)
            .expect("source row should remain available");
        for col in 0..cols {
            let actual = source
                .get(col)
                .expect("source col should remain in bounds")
                .char();
            let expected = baseline[usize::from(col)];
            prop_assert_eq!(
                actual,
                expected,
                "source row {} changed at col {} after mutating row {}",
                source_row,
                col,
                mutate_row
            );
        }
    }
}
