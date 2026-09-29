// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Resize allocation regressions and the ResizeRowReuse Tier-1 bind.

use super::*;

#[test]
fn repeated_rows_only_resize_reuses_page_allocations() {
    let mut grid = Grid::with_scrollback(52, 121, 0);
    for row in 0..52 {
        grid.set_cursor(row, 0);
        grid.write_char('x');
    }
    grid.set_cursor(51, 0);
    let before = grid.storage.pages.stats();
    let memory_before = grid.storage.pages.active_pages() * crate::page::PAGE_SIZE;
    for _ in 0..512 {
        grid.resize_no_reflow(51, 121);
        grid.resize_no_reflow(52, 121);
    }
    let after = grid.storage.pages.stats();
    let memory_after = grid.storage.pages.active_pages() * crate::page::PAGE_SIZE;
    eprintln!(
        "resize census: 512 quiet 52->51->52 flaps at 121 columns; allocations {} -> {}, page bytes {} -> {}",
        before.allocations, after.allocations, memory_before, memory_after,
    );
    assert_eq!(after.allocations, before.allocations);
    assert_eq!(memory_after, memory_before);
    grid.assert_invariants();
}

#[test]
fn repeated_resize_shapes_and_intervening_output_keep_the_allocation_highwater() {
    use crate::grid::reflow::ResizePolicy;
    for (name, policy, painted, cursor, interrupt) in [
        ("quiet demote", ResizePolicy::Native, true, 51, false),
        ("quiet push", ResizePolicy::Native, true, 0, false),
        ("blank trim", ResizePolicy::Native, false, 0, false),
        ("output demote", ResizePolicy::Native, true, 51, true),
        ("output push", ResizePolicy::Native, true, 0, true),
        ("ConPTY", ResizePolicy::ConPty, true, 0, true),
    ] {
        let mut grid = Grid::with_scrollback(52, 121, 0);
        if painted {
            for row in 0..52 {
                grid.set_cursor(row, 0);
                for c in format!("R{row}").chars() {
                    grid.write_char(c);
                }
            }
        }
        grid.set_cursor(cursor, 0);
        let before = grid.storage.pages.stats().allocations;
        let picture: Vec<_> = (0..52).map(|r| grid.row_text(r).unwrap()).collect();
        for cycle in 0..256 {
            grid.resize_with_policy(49, 121, policy);
            if interrupt {
                grid.drop_resize_undo();
                grid.write_char('y');
            }
            grid.resize_with_policy(52, 121, policy);
            assert_eq!(
                grid.storage.pages.stats().allocations,
                before,
                "{name}: {cycle}"
            );
            assert_eq!(grid.storage.pages.recycled_rows(), 0, "{name}: {cycle}");
            if !interrupt {
                let after: Vec<_> = (0..52).map(|r| grid.row_text(r).unwrap()).collect();
                assert_eq!(after, picture, "{name}: {cycle}");
            }
            grid.assert_invariants();
        }
    }
}

/// Retaining history can make a grow reveal a row rather than consume a freed
/// blank. The pool must conserve those bodies until later output grows the
/// ring again, even when it holds more rows than the visible viewport.
#[test]
fn resize_reveal_then_output_reuses_the_retained_ring_highwater() {
    let mut grid = Grid::with_scrollback(4, 80, 64);
    grid.set_cursor(3, 0);
    for _ in 0..64 {
        grid.line_feed();
    }
    let highwater = grid.storage.pages.stats().allocations;
    for cycle in 0..8 {
        for _ in 0..64 {
            grid.set_cursor(0, 0);
            grid.resize_no_reflow(3, 80);
            grid.resize_no_reflow(4, 80);
        }
        assert!(grid.storage.pages.recycled_rows() > usize::from(grid.rows()));
        for _ in 0..64 {
            grid.set_cursor(3, 0);
            grid.line_feed();
        }
        assert_eq!(
            grid.storage.pages.stats().allocations,
            highwater,
            "cycle {cycle}"
        );
        grid.assert_invariants();
    }
}

/// Tier-1: all six-step schedules over real resize, width rebuild and output
/// transitions agree with the derived ownership machine. The counters come
/// from the shipping arena, rather than a second allocator in the test.
#[test]
fn resize_row_reuse_conforms() {
    use aterm_spec::{derive::resize_row_reuse_model, interp};
    use std::collections::{BTreeMap, BTreeSet};
    fn project(grid: &Grid, peak: i64) -> BTreeMap<&'static str, i64> {
        BTreeMap::from([
            ("live", grid.storage.rows.len() as i64),
            ("free", grid.storage.pages.recycled_rows() as i64),
            ("allocated", grid.storage.pages.stats().allocations as i64),
            ("peak", peak),
        ])
    }
    let model = resize_row_reuse_model();
    let actions = ["Shrink", "Grow", "Rebuild", "Output"];
    let mut seen = BTreeSet::new();
    for mut schedule in 0..4usize.pow(6) {
        let mut grid = Grid::with_scrollback(2, 4, 0);
        let mut state = model.init_state();
        let mut peak = 2;
        assert_eq!(project(&grid, peak), state);
        for _ in 0..6 {
            let action = actions[schedule % 4];
            schedule /= 4;
            if !model.action_enabled(action, &state) {
                break;
            }
            match action {
                "Shrink" => grid.resize_no_reflow(grid.rows() - 1, grid.cols()),
                "Grow" => {
                    grid.resize_no_reflow(grid.rows() + 1, grid.cols());
                    peak = peak.max(i64::from(grid.rows()));
                }
                "Rebuild" => {
                    grid.resize_no_reflow(grid.rows(), if grid.cols() == 4 { 5 } else { 4 });
                    peak = i64::from(grid.rows());
                }
                "Output" => {
                    grid.drop_resize_undo();
                    grid.write_char('x');
                }
                _ => unreachable!(),
            }
            assert!(model.fire(action, &mut state));
            assert_eq!(project(&grid, peak), state, "after {action}");
            for invariant in ["OwnedExactlyOnce", "BoundedByHighWater"] {
                assert!(model.check_invariant(invariant, &state), "{invariant}");
            }
            grid.assert_invariants();
            seen.insert(action);
        }
    }
    assert_eq!(seen.len(), actions.len());

    // The historical shrink discarded its unique allocation token. The same
    // actual allocation census projected with that lost token breaks ownership.
    let mutant = interp::with_buggy(&model, 1);
    let mut state = mutant.init_state();
    assert!(mutant.fire("Shrink", &mut state));
    assert!(!model.check_invariant("OwnedExactlyOnce", &state));
    assert!(mutant.fire("Grow", &mut state));
    assert!(!model.check_invariant("BoundedByHighWater", &state));
}
