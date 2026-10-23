// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for a tiered grid whose total scrollback limit is at or below its ring cap.

use super::super::*;
use crate::grid::test_support::history_trimmed;

const RING_CAP: usize = 8;

fn zero_share_grid(rows: u16, cols: u16, limit: usize) -> Grid {
    assert!(limit <= RING_CAP, "the fixture is the zero-share regime");
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(rows, cols, RING_CAP, scrollback);
    grid.set_scrollback_line_limit(Some(limit));
    grid
}

fn write_numbered(grid: &mut Grid, n: usize) {
    if n > 0 {
        grid.line_feed();
    }
    grid.carriage_return();
    for c in format!("L{n}").chars() {
        grid.write_char(c);
    }
}

fn transcript(grid: &Grid) -> Vec<usize> {
    let screen = (0..=grid.cursor_row()).map(|r| {
        grid.row(r)
            .map(|row| row.to_string().trim_end().to_string())
            .unwrap_or_default()
    });
    history_trimmed(grid)
        .into_iter()
        .chain(screen)
        .filter(|text| !text.is_empty())
        .map(|text| {
            text.strip_prefix('L')
                .and_then(|n| n.parse().ok())
                .unwrap_or_else(|| panic!("not a numbered line: {text:?}"))
        })
        .collect()
}

fn assert_contiguous_suffix(grid: &Grid, newest: usize, limit: usize, ctx: &str) {
    let got = transcript(grid);
    let first = got.first().copied().unwrap_or(newest);
    assert_eq!(
        got,
        (first..=newest).collect::<Vec<_>>(),
        "{ctx}: history must be a contiguous suffix of the transcript"
    );
    assert!(
        grid.scrollback_lines() <= limit,
        "{ctx}: {} history lines retained under a total limit of {limit}",
        grid.scrollback_lines()
    );
    grid.assert_invariants();
}

/// One step of a resize script: a reflowing resize, a resize that rewraps
/// nothing, or `n` more numbered lines.
#[derive(Clone, Copy, Debug)]
enum Step {
    Resize(u16, u16),
    ResizeNoReflow(u16, u16),
    Write(usize),
}

#[test]
fn zero_share_grid_matches_its_ring_only_twin_through_resizes() {
    use Step::{Resize, ResizeNoReflow, Write};
    let script = [
        Write(20),
        Resize(3, 10),
        Write(3),
        Resize(6, 20),
        Write(3),
        Resize(6, 12),
        Write(3),
        ResizeNoReflow(6, 9),
        Write(3),
        Write(RING_CAP),
        ResizeNoReflow(6, 14),
        Write(RING_CAP),
        Resize(4, 7),
        Resize(8, 7),
        Write(2),
        Resize(5, 16),
        Write(RING_CAP),
    ];
    for offload in [false, true] {
        for limit in [0, 1, 4, RING_CAP] {
            let mut tiered = zero_share_grid(6, 20, limit);
            let mut twin = Grid::with_scrollback(6, 20, limit);
            let mut next = 0;
            for (i, step) in script.into_iter().enumerate() {
                match step {
                    Resize(rows, cols) if offload => {
                        if let Some(job) = tiered.resize_offloading_scrollback(rows, cols) {
                            let follow =
                                tiered.reattach_reflowed_scrollback_or_redetach(job.reflow());
                            assert!(follow.is_none(), "the width settled: no follow-up job");
                        }
                        twin.resize(rows, cols);
                    }
                    Resize(rows, cols) => {
                        tiered.resize(rows, cols);
                        twin.resize(rows, cols);
                    }
                    ResizeNoReflow(rows, cols) => {
                        tiered.resize_no_reflow(rows, cols);
                        twin.resize_no_reflow(rows, cols);
                    }
                    Write(n) => {
                        for k in next..next + n {
                            write_numbered(&mut tiered, k);
                            write_numbered(&mut twin, k);
                        }
                        next += n;
                    }
                }
                let ctx = format!("offload {offload}, limit {limit}, step {i} {step:?}");
                assert_eq!(
                    transcript(&tiered),
                    transcript(&twin),
                    "{ctx}: a zero-share tiered grid must read as its ring-only twin"
                );
                assert_contiguous_suffix(&tiered, next - 1, limit, &ctx);
            }
        }
    }
}

#[test]
fn zero_share_replacement_store_in_an_offload_window_stages_nothing() {
    let limit = 4;
    let scrollback = Scrollback::new(100, 1000, 10_000_000);
    let mut grid = Grid::with_tiered_scrollback(6, 20, RING_CAP, scrollback);
    let mut newest = 0;
    write_numbered(&mut grid, newest);
    for _ in 0..30 {
        newest += 1;
        write_numbered(&mut grid, newest);
    }
    let job = grid
        .resize_offloading_scrollback(6, 16)
        .expect("a tiered width change offloads its history");
    grid.erase_scrollback();
    grid.attach_scrollback(Scrollback::new(100, 1000, 10_000_000));
    grid.set_scrollback_line_limit(Some(limit));
    for _ in 0..RING_CAP {
        newest += 1;
        write_numbered(&mut grid, newest);
    }
    assert_contiguous_suffix(&grid, newest, limit, "before the in-window resize");

    grid.resize(3, 12);
    assert_contiguous_suffix(&grid, newest, limit, "after the in-window resize");
    assert_eq!(
        grid.scrollback_lines(),
        limit,
        "the ring keeps the total across the in-window resize"
    );
    for _ in 0..RING_CAP {
        newest += 1;
        write_numbered(&mut grid, newest);
        assert_contiguous_suffix(&grid, newest, limit, &format!("in the window at L{newest}"));
    }

    grid.reattach_reflowed_scrollback(job.reflow());
    assert_contiguous_suffix(&grid, newest, limit, "after re-attach");
}

#[test]
fn zero_share_drag_step_during_the_offloaded_rewrap_keeps_the_rings_history() {
    let cases = [
        (0, (6, 12)),
        (3, (6, 12)),
        (RING_CAP + 3, (6, 12)),
        (0, (3, 12)),
    ];
    for redetach in [false, true] {
        for (in_window, (rows, cols)) in cases {
            for limit in [0, 1, 4, RING_CAP] {
                let ctx = format!(
                    "redetach {redetach}, {in_window} lines in the window, step to {rows}x{cols}, limit {limit}"
                );
                let mut tiered = zero_share_grid(6, 20, limit);
                let mut twin = Grid::with_scrollback(6, 20, limit);
                for grid in [&mut tiered, &mut twin] {
                    for n in 0..30 {
                        write_numbered(grid, n);
                    }
                }
                let mut newest = 29;

                let job = tiered
                    .resize_offloading_scrollback(6, 16)
                    .expect("a tiered width change offloads its history");
                let held = tiered.ring_buffer_scrollback();
                twin.resize(6, 16);
                for _ in 0..in_window {
                    newest += 1;
                    write_numbered(&mut tiered, newest);
                    write_numbered(&mut twin, newest);
                }
                assert!(
                    tiered.resize_offloading_scrollback(rows, cols).is_none(),
                    "{ctx}: the store is out, so the step resizes in place"
                );
                twin.resize(rows, cols);
                if rows == 6 {
                    assert_eq!(
                        !tiered.storage.lazy_buffer.is_empty(),
                        held + in_window > limit,
                        "{ctx}: reach, the window stages its oldest rows once its output passes the ring's cap"
                    );
                }

                if redetach {
                    let mut pending = Some(job);
                    let mut passes = 0;
                    while let Some(job) = pending {
                        passes += 1;
                        assert!(
                            passes <= 2,
                            "{ctx}: the width settled, one follow-up at most"
                        );
                        pending = tiered.reattach_reflowed_scrollback_or_redetach(job.reflow());
                    }
                } else {
                    tiered.reattach_reflowed_scrollback(job.reflow());
                }
                assert_eq!(
                    transcript(&tiered),
                    transcript(&twin),
                    "{ctx}: a drag re-attach keeps what the ring-only twin keeps"
                );
                assert_contiguous_suffix(&tiered, newest, limit, &ctx);
                for _ in 0..RING_CAP {
                    newest += 1;
                    write_numbered(&mut tiered, newest);
                    write_numbered(&mut twin, newest);
                    assert_eq!(transcript(&tiered), transcript(&twin), "{ctx} at L{newest}");
                    assert_contiguous_suffix(
                        &tiered,
                        newest,
                        limit,
                        &format!("{ctx} at L{newest}"),
                    );
                }
            }
        }
    }
}

#[test]
fn zero_share_rows_shrinking_rewrap_free_width_change_reads_as_its_twin() {
    for limit in [0, 1, 4, RING_CAP] {
        let mut tiered = zero_share_grid(6, 20, limit);
        let mut twin = Grid::with_scrollback(6, 20, limit);
        for grid in [&mut tiered, &mut twin] {
            for n in 0..30 {
                write_numbered(grid, n);
            }
            grid.resize_no_reflow(4, 14);
        }
        for step in 0..=RING_CAP {
            let ctx = format!("limit {limit}, {step} lines after the shrink");
            assert_eq!(
                transcript(&tiered),
                transcript(&twin),
                "{ctx}: a zero-share tiered grid must read as its ring-only twin"
            );
            assert!(
                tiered.scrollback_lines() <= limit,
                "{ctx}: {} history lines retained under a total limit of {limit}",
                tiered.scrollback_lines()
            );
            tiered.assert_invariants();
            write_numbered(&mut tiered, 30 + step);
            write_numbered(&mut twin, 30 + step);
        }
    }
}
