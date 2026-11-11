// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::Grid;
use super::super::lazy_buffer::FLOOD_MARKER_PREFIX;
use super::super::test_support::{feed_numbered_lines, history_trimmed};
use crate::CellFlags;
use aterm_scrollback::Scrollback;

const DEFAULT_BUDGET: usize = 100 * 1024 * 1024;

const FLOOR_BUDGET: usize = 1024 * 1024;

const BATCH: usize = 256;

fn flooded_grid(cols: u16, ring: usize, budget: usize) -> Grid {
    let mut grid = Grid::with_tiered_scrollback(3, cols, ring, Scrollback::new(100, 1000, budget));
    grid.set_compress_offload_active(true);
    grid
}

fn marker_count(row: &str) -> Option<u64> {
    row.strip_prefix(FLOOD_MARKER_PREFIX)?
        .split(' ')
        .next()?
        .parse()
        .ok()
}

fn line_number(row: &str) -> usize {
    row.strip_prefix('L')
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("expected a content row `L<n>`, got {row:?}"))
}

fn assert_no_silent_hole(hist: &[String], origin: usize) -> Vec<(usize, u64)> {
    let mut markers = Vec::new();
    let mut expect_next: Option<usize> = None;
    for (idx, row) in hist.iter().enumerate() {
        if let Some(count) = marker_count(row) {
            let after = expect_next.unwrap_or(origin) + usize::try_from(count).expect("count fits");
            markers.push((idx, count));
            expect_next = Some(after);
            continue;
        }
        let n = line_number(row);
        if let Some(expected) = expect_next {
            assert_eq!(
                n,
                expected,
                "silent hole: row {idx} is L{n} but L{expected} should follow \
                 (history {hist_len} rows, markers so far {markers:?})",
                hist_len = hist.len()
            );
        }
        expect_next = Some(n + 1);
    }
    markers
}

fn newest(total: usize, limit: usize) -> Vec<String> {
    let first_on_screen = total - 2;
    (first_on_screen - limit..first_on_screen)
        .map(|n| format!("L{n}"))
        .collect()
}

fn window_top(total: usize, limit: usize) -> usize {
    (total - 2).saturating_sub(limit)
}

/// What staging the lightest row these floods stage once past their first lines (`L1000`
/// on) costs.
fn staged_row_bytes() -> usize {
    super::super::lazy_buffer::LazyBuffer::row_bytes(5)
}

fn assert_floor_is_the_cap(grid: &Grid) {
    let store = grid.scrollback().expect("a tiered store");
    let headroom = store.memory_budget() - store.budgeted_memory_used();
    assert!(
        headroom / staged_row_bytes() < Grid::ASYNC_COMPRESS_BACKPRESSURE,
        "the budget must hold fewer staged rows than the floor"
    );
}

#[test]
fn a_120k_burst_into_a_100k_scrollback_keeps_the_newest_100k_with_no_hole() {
    let mut grid = flooded_grid(80, 8, DEFAULT_BUDGET);
    grid.set_scrollback_line_limit(Some(100_000));
    feed_numbered_lines(&mut grid, 0, 120_000);
    assert_eq!(
        grid.lazy_backlog_len(),
        100_000 - 8,
        "the backlog is held at the store's share"
    );

    let hist = history_trimmed(&grid);
    assert!(
        assert_no_silent_hole(&hist, window_top(120_000, 100_000)).is_empty(),
        "no marker"
    );
    assert_eq!(
        hist,
        newest(120_000, 100_000),
        "the newest 100k, contiguous"
    );
    assert_eq!(
        grid.truncated_lines(),
        0,
        "the limit's eviction is not a loss"
    );

    assert!(grid.scrollback_mut().is_some());
    assert_eq!(history_trimmed(&grid), newest(120_000, 100_000));
    assert_eq!(grid.truncated_lines(), 0);
    grid.assert_invariants();
}

#[test]
fn a_short_line_flood_is_weighed_by_its_cells_not_the_window_width() {
    let mut grid = flooded_grid(200, 8, DEFAULT_BUDGET);
    grid.set_scrollback_line_limit(Some(100_000));
    feed_numbered_lines(&mut grid, 0, 120_000);

    let hist = history_trimmed(&grid);
    assert!(
        assert_no_silent_hole(&hist, window_top(120_000, 100_000)).is_empty(),
        "no marker"
    );
    assert_eq!(
        hist,
        newest(120_000, 100_000),
        "the newest 100k, contiguous"
    );
    assert_eq!(grid.truncated_lines(), 0, "nothing lost");
    let store = grid.scrollback().expect("a tiered store");
    assert!(
        grid.storage.lazy_buffer.staged_bytes()
            <= store.memory_budget() - store.budgeted_memory_used(),
        "and the staged rows still fit the memory headroom"
    );
    grid.assert_invariants();
}

fn feed_wide(grid: &mut Grid, start: usize, n: usize, cols: u16) {
    for i in start..start + n {
        grid.carriage_return();
        let text = format!("L{i}");
        for c in text.chars() {
            grid.write_char(c);
        }
        for _ in text.len()..usize::from(cols) {
            grid.write_char('-');
        }
        grid.line_feed();
    }
}

#[test]
fn a_full_width_flood_is_cut_at_the_memory_headroom_and_marked_exactly() {
    let per_row = super::super::lazy_buffer::LazyBuffer::row_bytes(80);
    let mut grid = flooded_grid(80, 8, 25_000 * per_row);
    feed_wide(&mut grid, 0, 5_000, 80);
    assert!(grid.scrollback_mut().is_some(), "drain it into the store");
    let stored = grid.tiered_scrollback_lines();
    assert!(
        grid.scrollback()
            .is_some_and(|store| store.budgeted_memory_used() > 0),
        "precondition: the store holds part of the budget"
    );
    feed_wide(&mut grid, 5_000, 40_000, 80);

    let store = grid.scrollback().expect("a tiered store");
    let headroom = store.memory_budget() - store.budgeted_memory_used();
    assert!(
        grid.storage.lazy_buffer.staged_bytes() <= headroom,
        "the staged rows fit the headroom"
    );
    let staged = grid.lazy_backlog_len();
    assert!(
        staged > Grid::ASYNC_COMPRESS_BACKPRESSURE && staged < 30_000,
        "the headroom, not the floor or the share, set the cap ({staged} staged)"
    );
    let hist = history_trimmed(&grid);
    let markers = assert_no_silent_hole(&hist, 0);
    let [(idx, count)] = markers[..] else {
        panic!("exactly one cut expected: {markers:?}");
    };
    assert_eq!(idx, stored, "the cut sits right below the stored history");
    assert_eq!(count, grid.truncated_lines(), "marker and counter agree");
    grid.assert_invariants();
}

#[test]
fn a_trickling_worker_is_evicted_ahead_of_the_flood_not_cut_around() {
    let mut grid = flooded_grid(80, 8, DEFAULT_BUDGET);
    grid.set_scrollback_line_limit(Some(100_000));
    let epoch = grid.history_renumber_epoch();
    let mut fed = 0;
    while fed < 120_000 {
        feed_numbered_lines(&mut grid, fed, 10_000);
        fed += 10_000;
        grid.drain_lazy_bounded(BATCH);
    }
    assert!(
        grid.tiered_scrollback_lines() > grid.lazy_backlog_len(),
        "precondition: the trickle left rows in the store"
    );

    let hist = history_trimmed(&grid);
    assert!(
        assert_no_silent_hole(&hist, window_top(120_000, 100_000)).is_empty(),
        "no marker"
    );
    assert_eq!(
        hist,
        newest(120_000, 100_000),
        "the newest 100k, contiguous"
    );
    assert_eq!(
        grid.truncated_lines(),
        0,
        "the limit's eviction is not a loss"
    );
    assert_eq!(
        grid.history_renumber_epoch(),
        epoch,
        "evicting from the top keeps every survivor's absolute key"
    );
    grid.assert_invariants();
}

#[test]
fn a_cut_beneath_promoted_lines_sits_exactly_where_the_hole_is() {
    let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
    assert_floor_is_the_cap(&grid);
    feed_numbered_lines(&mut grid, 0, 1_500);
    assert!(
        grid.drain_lazy_bounded(BATCH) > 0,
        "a backlog remains after one batch"
    );
    assert_eq!(
        grid.tiered_scrollback_lines() - grid.lazy_backlog_len(),
        BATCH,
        "the store holds exactly the one promoted batch"
    );
    let epoch = grid.history_renumber_epoch();
    feed_numbered_lines(&mut grid, 1_500, 23_500);

    let hist = history_trimmed(&grid);
    let markers = assert_no_silent_hole(&hist, 0);
    let [(idx, count)] = markers[..] else {
        panic!("exactly one cut expected, got {markers:?}");
    };
    assert_eq!(
        idx, BATCH,
        "the marker stands right after the promoted prefix"
    );
    assert_eq!(hist[BATCH - 1], format!("L{}", BATCH - 1));
    assert_eq!(count, grid.truncated_lines(), "marker and counter agree");
    assert_eq!(
        line_number(&hist[BATCH + 1]),
        BATCH + usize::try_from(count).unwrap()
    );
    assert!(count > 0, "the flood must have cut something");
    assert_eq!(
        hist.last().map(String::as_str),
        newest(25_000, 1).last().map(String::as_str),
        "the history still ends at the newest line"
    );
    let marker = grid.get_history_line(BATCH).expect("marker row");
    assert!(
        marker.get_attr(0).flags & CellFlags::DIM.bits() != 0,
        "the marker row is dim"
    );
    assert!(
        grid.history_renumber_epoch() > epoch,
        "renumber epoch advanced"
    );
    assert!(grid.lazy_backlog_len() <= Grid::ASYNC_COMPRESS_BACKPRESSURE);
    grid.assert_invariants();
}

#[test]
fn a_cut_deeper_than_the_limit_names_only_what_the_limit_would_keep() {
    const LIMIT: usize = 30_000;
    const TOTAL: usize = 60_000;
    let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
    assert_floor_is_the_cap(&grid);
    grid.set_scrollback_line_limit(Some(LIMIT));
    feed_numbered_lines(&mut grid, 0, 1_500);
    grid.drain_lazy_bounded(BATCH);
    feed_numbered_lines(&mut grid, 1_500, TOTAL - 1_500);

    let hist = history_trimmed(&grid);
    let markers = assert_no_silent_hole(&hist, window_top(TOTAL, LIMIT));
    let [(0, count)] = markers[..] else {
        panic!("exactly one cut, at the top, expected: {markers:?}");
    };
    assert_eq!(
        usize::try_from(count).unwrap() + hist.len() - 1,
        LIMIT,
        "the marker plus the survivors are exactly the limit's window"
    );
    assert_eq!(
        hist.last().map(String::as_str),
        newest(TOTAL, 1).last().map(String::as_str),
        "the history still ends at the newest line"
    );
    assert_eq!(
        grid.tiered_scrollback_lines(),
        grid.lazy_backlog_len(),
        "the promoted batch was older than the window, and was evicted"
    );
    assert_eq!(count, grid.truncated_lines(), "marker and counter agree");
    grid.assert_invariants();
}

#[test]
fn a_marker_shrinks_as_the_window_slides_past_its_hole() {
    let mut grid = flooded_grid(80, 8, DEFAULT_BUDGET);
    grid.set_scrollback_line_limit(Some(21_000));
    feed_numbered_lines(&mut grid, 0, 1_500);
    let counted = grid.storage.lazy_buffer.drop_oldest(10);
    assert_eq!(counted, 10);
    grid.storage.flood_truncated_lines += counted;
    feed_numbered_lines(&mut grid, 1_500, 25_000);

    let hist = history_trimmed(&grid);
    let markers = assert_no_silent_hole(&hist, window_top(26_500, 21_000));
    assert_eq!(
        markers,
        [(0, 1)],
        "one line of the window is missing: the one the marker stands in"
    );
    assert_eq!(hist.len(), 21_000, "the limit's window, marker included");
    assert_eq!(
        grid.truncated_lines(),
        10,
        "the counter keeps the loss the cut named at its deepest"
    );
    grid.assert_invariants();
}

#[test]
fn a_drain_promotes_the_marker_first_and_a_later_drop_opens_a_new_cut() {
    let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
    assert_floor_is_the_cap(&grid);
    feed_numbered_lines(&mut grid, 0, 22_000);
    let first_cut = grid.truncated_lines();
    assert!(first_cut > 0, "precondition: the flood cut");

    grid.drain_lazy_bounded(300);
    assert_eq!(
        marker_count(&history_trimmed(&grid)[0]),
        Some(first_cut),
        "the store's oldest row is the marker for the first cut"
    );
    assert_eq!(
        grid.tiered_scrollback_lines() - grid.lazy_backlog_len(),
        300
    );

    feed_numbered_lines(&mut grid, 22_000, 2_000);
    let hist = history_trimmed(&grid);
    let markers = assert_no_silent_hole(&hist, 0);
    assert_eq!(markers.len(), 2, "two cuts: {markers:?}");
    assert_eq!(markers[0], (0, first_cut));
    assert_eq!(
        markers[1].0, 300,
        "the second cut follows the promoted batch"
    );
    assert_eq!(
        markers.iter().map(|(_, c)| c).sum::<u64>(),
        grid.truncated_lines(),
        "the markers sum to the counter"
    );
    grid.assert_invariants();
}

#[test]
fn ordinary_top_eviction_is_neither_counted_nor_marked() {
    let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
    assert_floor_is_the_cap(&grid);
    grid.set_scrollback_line_limit(Some(5_000));
    feed_numbered_lines(&mut grid, 0, 3_000);
    grid.drain_lazy_bounded(BATCH);
    feed_numbered_lines(&mut grid, 3_000, 18_000);
    assert_eq!(
        grid.lazy_backlog_len(),
        Grid::ASYNC_COMPRESS_BACKPRESSURE,
        "the floor beats a tiny share: the backlog is held at the floor"
    );
    let hist = history_trimmed(&grid);
    assert!(
        assert_no_silent_hole(&hist, window_top(21_000, 5_000)).is_empty(),
        "no marker for ordinary eviction"
    );
    assert_eq!(grid.truncated_lines(), 0, "ordinary eviction is not a loss");

    assert!(grid.scrollback_mut().is_some());
    assert_eq!(
        history_trimmed(&grid),
        newest(21_000, 5_000),
        "ring share + store share"
    );
    assert_eq!(grid.truncated_lines(), 0);
    grid.assert_invariants();
}

fn trickled_flood_markers(total: usize, guarded: bool) -> Vec<(usize, u64)> {
    let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
    assert_floor_is_the_cap(&grid);
    let mut fed = 0;
    while fed < total {
        feed_numbered_lines(&mut grid, fed, 1_000);
        fed += 1_000;
        if guarded {
            grid.trickle_lazy_bounded(BATCH);
        } else {
            grid.drain_lazy_bounded(BATCH);
        }
    }
    while grid.drain_lazy_bounded(BATCH) > 0 {}
    let hist = history_trimmed(&grid);
    let markers = assert_no_silent_hole(&hist, 0);
    assert_eq!(
        markers.iter().map(|(_, count)| count).sum::<u64>(),
        grid.truncated_lines(),
        "the markers sum to the counter"
    );
    grid.assert_invariants();
    markers
}

#[test]
fn a_trickle_that_skips_an_open_cut_leaves_one_marker_per_flood() {
    let markers = trickled_flood_markers(40_000, true);
    assert_eq!(markers.len(), 1, "one flood, one cut: {markers:?}");
    let fragmented = trickled_flood_markers(40_000, false);
    assert!(
        fragmented.len() > 1,
        "the unguarded trickle fragments the cut: {fragmented:?}"
    );
}

fn slow_tail(per_tick: usize, skip_while_cut: bool) -> (u64, Vec<(usize, u64)>) {
    let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
    assert_floor_is_the_cap(&grid);
    let opportunity = |grid: &mut Grid| {
        if skip_while_cut {
            if !grid.storage.lazy_buffer.cut_open() {
                grid.drain_lazy_bounded(BATCH);
            }
        } else {
            grid.trickle_lazy_bounded(BATCH);
        }
    };
    let mut fed = 0;
    while fed < 40_000 {
        feed_numbered_lines(&mut grid, fed, 1_000);
        fed += 1_000;
        opportunity(&mut grid);
    }
    assert!(
        grid.storage.lazy_buffer.cut_open(),
        "precondition: the flood is cutting"
    );
    feed_numbered_lines(&mut grid, fed, per_tick);
    fed += per_tick;
    opportunity(&mut grid);
    let before = grid.truncated_lines();
    for _ in 1..20 {
        feed_numbered_lines(&mut grid, fed, per_tick);
        fed += per_tick;
        opportunity(&mut grid);
    }
    let lost = grid.truncated_lines() - before;
    let hist = history_trimmed(&grid);
    let markers = assert_no_silent_hole(&hist, 0);
    assert_eq!(
        hist.last().map(String::as_str),
        newest(fed, 1).last().map(String::as_str),
        "the history ends at the newest line"
    );
    grid.assert_invariants();
    (lost, markers)
}

#[test]
fn a_slow_tail_after_a_flood_is_promoted_not_dropped() {
    let (lost, markers) = slow_tail(50, false);
    assert_eq!(lost, 0, "the tail loses nothing once a batch outruns it");
    assert_eq!(markers.len(), 1, "one flood, one cut: {markers:?}");
    let (lost, _) = slow_tail(50, true);
    assert_eq!(lost, 19 * 50, "skipping while cut drops every tail line");
}

#[test]
fn a_window_that_slides_past_the_whole_hole_takes_the_marker() {
    let mut grid = flooded_grid(80, 8, DEFAULT_BUDGET);
    grid.set_scrollback_line_limit(Some(15_000));
    feed_numbered_lines(&mut grid, 0, 1_500);
    let counted = grid.storage.lazy_buffer.drop_oldest(10);
    grid.storage.flood_truncated_lines += counted;
    assert_eq!(marker_count(&history_trimmed(&grid)[0]), Some(10));
    assert_eq!(history_trimmed(&grid)[1], "L10");
    feed_numbered_lines(&mut grid, 1_500, 25_000);
    let hist = history_trimmed(&grid);
    assert!(
        assert_no_silent_hole(&hist, window_top(26_500, 15_000)).is_empty(),
        "the marker went with the top"
    );
    assert_eq!(grid.truncated_lines(), 10);
    grid.assert_invariants();
}

#[test]
fn the_marker_states_the_count_first_and_fits_the_default_width() {
    use super::super::lazy_buffer::flood_marker_line;
    let line = flood_marker_line(9_999_999);
    let text = line.to_string();
    let width = text.chars().count();
    assert_eq!(marker_count(&text), Some(9_999_999), "{text:?}");
    assert!(width <= 80, "{text:?} is {width} columns");
    assert!(
        (0..width).all(|i| line.get_attr(i).flags & CellFlags::DIM.bits() != 0),
        "the whole row is dim"
    );
}

#[test]
fn with_no_line_limit_every_drop_is_a_loss() {
    let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
    assert_floor_is_the_cap(&grid);
    grid.set_scrollback_line_limit(None);
    feed_numbered_lines(&mut grid, 0, 25_000);
    assert_eq!(grid.lazy_backlog_len(), Grid::ASYNC_COMPRESS_BACKPRESSURE);
    let hist = history_trimmed(&grid);
    let markers = assert_no_silent_hole(&hist, 0);
    let [(0, count)] = markers[..] else {
        panic!("exactly one cut, at the top, expected: {markers:?}");
    };
    assert_eq!(
        usize::try_from(count).unwrap(),
        25_000 - 2 - (hist.len() - 1),
        "every line not retained is named"
    );
    assert_eq!(count, grid.truncated_lines());
    grid.assert_invariants();
}

#[test]
fn a_drained_flood_backlog_gives_its_spine_back() {
    let mut grid = flooded_grid(80, 8, DEFAULT_BUDGET);
    feed_numbered_lines(&mut grid, 0, 40_000);
    assert!(grid.storage.lazy_buffer.spine_capacity() >= 32_768);
    while grid.drain_lazy_bounded(BATCH) > BATCH {}
    assert!(
        grid.storage.lazy_buffer.spine_capacity() <= 4_000,
        "spine kept {} rows of capacity for a {}-row backlog",
        grid.storage.lazy_buffer.spine_capacity(),
        grid.lazy_backlog_len()
    );
    grid.assert_invariants();
}

#[test]
fn pressure_relief_after_a_late_worker_leaves_one_marker_for_continued_overload() {
    let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
    assert_floor_is_the_cap(&grid);
    feed_numbered_lines(&mut grid, 0, 22_000);
    assert!(grid.truncated_lines() > 0, "the worker was already late");
    for start in (22_000..42_000).step_by(1_000) {
        feed_numbered_lines(&mut grid, start, 1_000);
        let before = grid.lazy_backlog_len();
        assert_eq!(grid.flood_promotion_due(), 0, "the cut stays open");
        assert_eq!(grid.drain_lazy_pressure_bounded(BATCH), before);
        assert_eq!(
            grid.trickle_lazy_bounded(BATCH),
            before,
            "the skipped pressure batch did not erase the arrival count"
        );
    }
    while grid.drain_lazy_bounded(BATCH) > 0 {}
    let markers = assert_no_silent_hole(&history_trimmed(&grid), 0);
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].1, grid.truncated_lines());
    grid.assert_invariants();
}

#[test]
fn a_cut_opened_between_pressure_batches_overrides_the_owed_count() {
    let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
    assert_floor_is_the_cap(&grid);
    feed_numbered_lines(&mut grid, 0, 15_000);
    let owed = grid.flood_promotion_due();
    assert!(owed > BATCH, "several batches were counted before the cut");
    let before = grid.lazy_backlog_len();
    assert_eq!(grid.drain_lazy_pressure_bounded(BATCH), before - BATCH);
    let owed = owed - BATCH;
    assert!(owed > 0);

    feed_numbered_lines(&mut grid, 15_000, 10_000);
    assert!(grid.truncated_lines() > 0, "the reader opened a cut");
    let before = grid.lazy_backlog_len();
    assert_eq!(
        grid.drain_lazy_pressure_bounded(BATCH),
        before,
        "a counted batch must recheck before taking the marker"
    );
    assert_eq!(grid.trickle_lazy_bounded(BATCH), before);
    assert_eq!(grid.flood_promotion_due(), 0);

    while grid.drain_lazy_bounded(BATCH) > 0 {}
    let markers = assert_no_silent_hole(&history_trimmed(&grid), 0);
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].0, BATCH, "the cut follows the first batch");
    assert_eq!(markers[0].1, grid.truncated_lines());
    grid.assert_invariants();
}

fn feed_long(grid: &mut Grid, start: usize, n: usize, width: usize) {
    let mut row = Vec::with_capacity(width);
    for i in start..start + n {
        row.clear();
        row.extend_from_slice(format!("L{i} ").as_bytes());
        row.resize(width, b'p');
        grid.carriage_return();
        grid.write_ascii_blast(&row);
        grid.line_feed();
    }
}

fn relieve(grid: &mut Grid) {
    loop {
        let due = grid.flood_promotion_due();
        if due == 0 {
            return;
        }
        grid.drain_lazy_pressure_bounded(due.min(BATCH));
    }
}

fn maximized_flood(relieving: bool) -> (Grid, usize, u64) {
    let mut grid = flooded_grid(253, 8, DEFAULT_BUDGET);
    grid.set_scrollback_line_limit(Some(100_000));
    let mut peak = 0;
    let mut fed = 0;
    let mut reads = 0;
    while fed < 120_000 {
        feed_long(&mut grid, fed, 300, 230);
        fed += 300;
        reads += 1;
        peak = peak.max(grid.storage.lazy_buffer.staged_bytes());
        if relieving {
            relieve(&mut grid);
        }
        if reads % 57 == 0 {
            grid.trickle_lazy_bounded(BATCH);
        }
    }
    let lost = grid.truncated_lines();
    (grid, peak, lost)
}

#[test]
fn a_maximized_flood_of_long_lines_keeps_the_whole_limit_when_promoted_under_pressure() {
    let (mut grid, peak, lost) = maximized_flood(true);
    assert_eq!(lost, 0, "nothing was cut");
    let hist = history_trimmed(&grid);
    assert!(assert_no_silent_hole(&hist, 0).is_empty(), "no marker");
    assert!(hist.len() >= 100_000);
    assert_eq!(line_number(&hist[hist.len() - 1]), 120_000 - 3);

    while grid.drain_lazy_bounded(BATCH) > 0 {}
    let hist = history_trimmed(&grid);
    assert!(
        assert_no_silent_hole(&hist, window_top(120_000, 100_000)).is_empty(),
        "no marker"
    );
    assert_eq!(hist.len(), 100_000, "the whole configured limit");
    assert_eq!(line_number(&hist[0]), window_top(120_000, 100_000));
    assert_eq!(
        line_number(&hist[hist.len() - 1]),
        120_000 - 3,
        "the newest line"
    );
    assert_eq!(grid.truncated_lines(), 0);
    let per_row = super::super::lazy_buffer::LazyBuffer::row_bytes(253);
    assert!(
        peak <= DEFAULT_BUDGET / 2 + 300 * per_row,
        "the backlog peaked at {peak} bytes"
    );
    let store = grid.scrollback().expect("a tiered store");
    assert!(store.budgeted_memory_used() <= store.memory_budget());
    grid.assert_invariants();

    let (starved, _, lost) = maximized_flood(false);
    assert!(
        lost > 40_000 && starved.scrollback_lines() < 60_000,
        "the trickle alone keeps {} lines and loses {lost}",
        starved.scrollback_lines()
    );
}

#[test]
fn promotion_is_due_only_past_half_the_headroom_and_names_the_excess() {
    let mut grid = flooded_grid(253, 8, DEFAULT_BUDGET);
    feed_long(&mut grid, 0, 20_000, 253);
    assert_eq!(grid.flood_promotion_due(), 0, "under the mark");
    feed_long(&mut grid, 20_000, 20_000, 253);
    let due = grid.flood_promotion_due();
    assert!(due > 0, "past the mark");
    let store = grid.scrollback().expect("a tiered store");
    let mark = (store.memory_budget() - store.budgeted_memory_used()) / 2;
    let bytes = grid.storage.lazy_buffer.staged_bytes();
    let row = bytes / grid.storage.lazy_buffer.staged_rows();
    assert_eq!(
        due,
        (bytes - mark).div_ceil(row),
        "exactly the rows that take {bytes} bytes back to {mark}"
    );
    grid.drain_lazy_pressure_bounded(due);
    assert!(grid.flood_promotion_due() < BATCH);
    relieve(&mut grid);
    assert_eq!(grid.flood_promotion_due(), 0, "the loop clears it");
    assert_eq!(grid.truncated_lines(), 0);

    let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
    assert_floor_is_the_cap(&grid);
    let half_floor = Grid::ASYNC_COMPRESS_BACKPRESSURE / 2;
    feed_numbered_lines(&mut grid, 0, half_floor);
    assert!(grid.lazy_backlog_len() <= half_floor);
    assert_eq!(grid.flood_promotion_due(), 0, "within half the floor");
    feed_numbered_lines(&mut grid, half_floor, 5_000);
    assert_eq!(
        grid.flood_promotion_due(),
        grid.lazy_backlog_len() - half_floor,
        "every row past half the floor"
    );
    assert_eq!(grid.truncated_lines(), 0, "and the cut has not started");

    let mut ring_only = Grid::new(3, 253);
    feed_long(&mut ring_only, 0, 1_000, 253);
    assert_eq!(ring_only.flood_promotion_due(), 0);
}

#[test]
fn no_promotion_is_due_while_the_store_is_away_for_a_reflow() {
    let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
    let pending = grid
        .resize_offloading_scrollback(3, 100)
        .expect("a width change detaches the store");
    feed_numbered_lines(&mut grid, 0, Grid::ASYNC_COMPRESS_BACKPRESSURE);
    assert!(
        grid.lazy_backlog_len() > Grid::ASYNC_COMPRESS_BACKPRESSURE / 2,
        "precondition: past the mark had the store been here"
    );
    assert_eq!(grid.flood_promotion_due(), 0);
    let before = grid.lazy_backlog_len();
    assert_eq!(grid.drain_lazy_pressure_bounded(BATCH), before);
    grid.reattach_reflowed_scrollback(pending.reflow());
    grid.assert_invariants();
}

#[test]
fn a_promoted_flood_under_the_floor_budget_leaves_no_cut() {
    let mut grid = flooded_grid(80, 8, FLOOR_BUDGET);
    assert_floor_is_the_cap(&grid);
    let mut fed = 0;
    while fed < 40_000 {
        feed_numbered_lines(&mut grid, fed, 1_000);
        fed += 1_000;
        relieve(&mut grid);
        grid.trickle_lazy_bounded(BATCH);
    }
    assert_eq!(grid.truncated_lines(), 0, "nothing lost");
    let hist = history_trimmed(&grid);
    assert!(assert_no_silent_hole(&hist, 0).is_empty(), "no marker");
    assert_eq!(hist, newest(40_000, 40_000 - 2), "every line, contiguous");
    grid.assert_invariants();
}

fn row_bytes(width: usize) -> usize {
    super::super::lazy_buffer::LazyBuffer::row_bytes(width)
}

#[test]
fn a_flood_the_line_limit_holds_never_makes_a_promotion_due() {
    for (cols, width) in [(80u16, 80usize), (120, 100)] {
        assert!(
            100_000 * row_bytes(width) < DEFAULT_BUDGET,
            "precondition: 100k raw rows of {width} cells fit the budget"
        );
        let mut grid = flooded_grid(cols, 8, DEFAULT_BUDGET);
        grid.set_scrollback_line_limit(Some(100_000));
        let mut past_half = false;
        let mut fed = 0;
        while fed < 120_000 {
            feed_long(&mut grid, fed, 300, width);
            fed += 300;
            assert_eq!(
                grid.flood_promotion_due(),
                0,
                "{width} cells at {cols} columns, {fed} lines in"
            );
            let store = grid.scrollback().expect("a tiered store");
            let headroom = store.memory_budget() - store.budgeted_memory_used();
            past_half |= grid.storage.lazy_buffer.outweighs(headroom / 2);
        }
        assert!(
            past_half,
            "the backlog of {width}-cell rows passed half the headroom"
        );
        assert_eq!(grid.truncated_lines(), 0, "nothing was cut");
        let hist = history_trimmed(&grid);
        let top = window_top(120_000, 100_000);
        assert!(assert_no_silent_hole(&hist, top).is_empty(), "no marker");
        assert_eq!(hist.len(), 100_000, "the whole configured limit");
        assert_eq!(line_number(&hist[0]), top);
        grid.assert_invariants();
    }
}

fn widening_flood(relieving: bool) -> (Grid, Option<bool>) {
    let mut grid = flooded_grid(253, 8, DEFAULT_BUDGET);
    grid.set_scrollback_line_limit(Some(100_000));
    let mut fed = 0;
    while fed < 100_000 {
        feed_numbered_lines(&mut grid, fed, 1_000);
        fed += 1_000;
        assert_eq!(grid.flood_promotion_due(), 0, "short rows: none due");
    }
    let mut average_lagged = None;
    while fed < 160_000 {
        feed_long(&mut grid, fed, 300, 230);
        fed += 300;
        if !relieving {
            continue;
        }
        if average_lagged.is_none() && grid.flood_promotion_due() > 0 {
            let buffer = &grid.storage.lazy_buffer;
            let store = grid.scrollback().expect("a tiered store");
            let headroom = store.memory_budget() - store.budgeted_memory_used();
            let share = store.line_limit().expect("a line limit");
            let average = buffer.staged_bytes() / buffer.staged_rows();
            average_lagged = Some(average * share <= headroom);
        }
        relieve(&mut grid);
    }
    (grid, average_lagged)
}

#[test]
fn a_flood_that_turns_wide_after_its_backlog_filled_is_promoted_before_it_cuts() {
    let (mut grid, average_lagged) = widening_flood(true);
    assert_eq!(grid.truncated_lines(), 0, "nothing was cut");
    assert_eq!(
        average_lagged,
        Some(true),
        "relief began while the whole backlog's average projected no cut"
    );
    while grid.drain_lazy_bounded(BATCH) > 0 {}
    let hist = history_trimmed(&grid);
    let top = window_top(160_000, 100_000);
    assert!(assert_no_silent_hole(&hist, top).is_empty(), "no marker");
    assert_eq!(hist.len(), 100_000, "the whole configured limit");
    assert_eq!(line_number(&hist[0]), top);
    assert_eq!(line_number(&hist[hist.len() - 1]), 160_000 - 3);
    grid.assert_invariants();

    let (starved, _) = widening_flood(false);
    assert!(
        starved.truncated_lines() > 0,
        "the unrelieved flood kept {} lines and lost none",
        starved.scrollback_lines()
    );
}

#[test]
fn the_projection_weighs_the_flood_by_its_newest_rows() {
    let cap = 100_000;
    let mut grid = flooded_grid(253, 8, DEFAULT_BUDGET);
    feed_numbered_lines(&mut grid, 0, 5_000);
    feed_long(&mut grid, 5_000, 2_000, 253);
    let buffer = &grid.storage.lazy_buffer;
    let average = buffer.staged_bytes() / buffer.staged_rows();
    let peak = buffer.projected_peak(cap, Grid::FLOOD_WEIGHT_SAMPLE);
    assert_eq!(peak, cap * row_bytes(253), "cap rows of the newest weight");
    assert!(peak > 2 * average * cap, "far above the whole average");

    let mut grid = flooded_grid(253, 8, DEFAULT_BUDGET);
    feed_long(&mut grid, 0, 5_000, 253);
    feed_numbered_lines(&mut grid, 5_000, 2_000);
    let buffer = &grid.storage.lazy_buffer;
    let staged = buffer.staged_bytes();
    let peak = buffer.projected_peak(cap, Grid::FLOOD_WEIGHT_SAMPLE);
    assert!(peak >= staged, "the staged rows are part of the peak");
    assert!(
        peak < staged + cap * row_bytes(16),
        "the rows to come weigh what the newest (short) rows do"
    );
}

#[test]
fn a_line_share_within_the_floor_never_makes_a_promotion_due() {
    let mut grid = flooded_grid(253, 8, FLOOR_BUDGET);
    grid.set_scrollback_line_limit(Some(15_000));
    feed_long(&mut grid, 0, 30_000, 253);
    assert!(
        grid.storage.lazy_buffer.outweighs(FLOOR_BUDGET),
        "precondition: far past the mark"
    );
    assert_eq!(grid.flood_promotion_due(), 0);
    assert_eq!(grid.truncated_lines(), 0, "nothing was cut");
}
