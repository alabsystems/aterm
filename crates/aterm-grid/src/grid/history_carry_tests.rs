// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The history carry's grid half: the fence, the fenced read, and the import.

use super::super::Grid;
use super::{HistoryFenceBroken, OlderHistory, OlderHistoryRefusal};
use aterm_scrollback::{Line, Scrollback};

/// A grid with a small ring and a tiered store whose hot tier spills into
/// compressed blocks early, so a few hundred lines cross every tier.
fn tiered(rows: u16, cols: u16) -> Grid {
    let mut store = Scrollback::new(16, 64, 64 * 1024 * 1024);
    store.set_line_limit(None);
    Grid::with_tiered_scrollback(rows, cols, 8, store)
}

fn write_lines(grid: &mut Grid, prefix: &str, from: usize, count: usize) {
    for i in from..from + count {
        grid.carriage_return();
        for c in format!("{prefix}{i}").chars() {
            grid.write_char(c);
        }
        grid.line_feed();
    }
}

fn text(line: &Line) -> String {
    line.to_string().trim_end().to_string()
}

fn history(grid: &Grid) -> Vec<String> {
    (0..grid.scrollback_lines())
        .map(|i| {
            grid.get_history_line(i)
                .map(|l| l.to_string().trim_end().to_string())
                .unwrap_or_default()
        })
        .collect()
}

/// Read the whole fenced history the way the exporter does: short chunks by
/// absolute row.
fn export(grid: &Grid, chunk: usize) -> Result<Vec<Line>, HistoryFenceBroken> {
    let fence = grid.history_fence();
    let mut out = Vec::new();
    let mut cursor = fence.oldest;
    while cursor < fence.end {
        let lines = grid.history_lines_since_fence(&fence, cursor, chunk)?;
        assert!(
            !lines.is_empty(),
            "a fenced read below the end returns lines"
        );
        cursor += lines.len() as u64;
        out.extend(lines);
    }
    Ok(out)
}

#[test]
fn a_fenced_export_reads_every_history_line_in_order_across_every_tier() {
    let mut grid = tiered(4, 20);
    write_lines(&mut grid, "h", 0, 300);
    let fence = grid.history_fence();
    assert_eq!(fence.lines() as usize, grid.scrollback_lines());
    let exported = export(&grid, 7).expect("nothing moved");
    assert_eq!(
        exported.iter().map(text).collect::<Vec<_>>(),
        history(&grid),
        "the export is the history, oldest first"
    );
}

#[test]
fn output_after_the_fence_leaves_it_standing_and_stays_outside_it() {
    let mut grid = tiered(4, 20);
    write_lines(&mut grid, "h", 0, 100);
    let fence = grid.history_fence();
    let before = history(&grid);
    write_lines(&mut grid, "late", 0, 30);
    let now = grid.history_fence();
    assert!(fence.holds_at(&now), "plain output never breaks the fence");
    assert!(now.end > fence.end, "the new lines land past the fence");
    let mut cursor = fence.oldest;
    let mut read = Vec::new();
    while cursor < fence.end {
        let lines = grid
            .history_lines_since_fence(&fence, cursor, 11)
            .expect("the fence holds");
        cursor += lines.len() as u64;
        read.extend(lines.iter().map(text));
    }
    assert_eq!(
        read, before,
        "exactly the fenced lines, none of the late ones"
    );
}

#[test]
fn a_width_reflow_breaks_the_fence() {
    let mut grid = tiered(4, 20);
    write_lines(&mut grid, "h", 0, 50);
    let fence = grid.history_fence();
    grid.resize(4, 13);
    assert!(!fence.holds_at(&grid.history_fence()));
    assert_eq!(
        grid.history_lines_since_fence(&fence, fence.oldest, 8)
            .err(),
        Some(HistoryFenceBroken::Moved)
    );
}

#[test]
fn a_scrollback_clear_breaks_the_fence() {
    let mut grid = tiered(4, 20);
    write_lines(&mut grid, "h", 0, 50);
    let fence = grid.history_fence();
    grid.erase_scrollback();
    write_lines(&mut grid, "after", 0, 60);
    assert_eq!(
        grid.history_lines_since_fence(&fence, fence.oldest, 8)
            .err(),
        Some(HistoryFenceBroken::Moved),
        "a cleared history is never read back under the old fence"
    );
}

/// A ROWS-GROW THAT REVEALS HISTORY BREAKS THE FENCE (2026-09-26 review).
/// The reveal hands the newest history lines back to the screen under their
/// own keys; rewritten there and pushed back by later output, they name other
/// text under the SAME keys, and before `reveal_gen` the fence still held
/// (measured on a terminal first: rows 31 to 35 read `NEW1..NEW5` where the
/// export had read `old31..old35`).
/// NEGATIVE CONTROL: a rows-shrink (a top-demote, the reveal's inverse) and
/// later output leave the fence standing.
#[test]
fn a_rows_grow_that_reveals_history_breaks_the_fence() {
    let mut grid = tiered(5, 30);
    write_lines(&mut grid, "old", 0, 40);
    let fence = grid.history_fence();
    let exported: Vec<String> = export(&grid, 7).expect("fenced").iter().map(text).collect();
    grid.resize(10, 30);
    for row in 0..5_u16 {
        grid.set_cursor(row, 0);
        for c in format!("NEW{row}").chars() {
            grid.write_char(c);
        }
        grid.erase_to_end_of_line();
    }
    grid.set_cursor(9, 0);
    write_lines(&mut grid, "late", 0, 20);
    let now = grid.history_fence();
    assert!(now.end >= fence.end, "the total alone would not say it");
    let reread: Vec<String> = (fence.oldest..fence.end)
        .map(|key| text(&grid.get_history_line((key - now.oldest) as usize).unwrap()))
        .collect();
    assert_ne!(reread, exported, "the rig rewrote rows the export had read");
    assert!(!fence.holds_at(&now), "a reveal moves the fence");
    assert_eq!(
        grid.history_lines_since_fence(&fence, fence.oldest, 8)
            .err(),
        Some(HistoryFenceBroken::Moved)
    );

    // Negative control: nothing revealed, nothing moved.
    let mut quiet = tiered(5, 30);
    write_lines(&mut quiet, "old", 0, 40);
    let fence = quiet.history_fence();
    quiet.resize(4, 30);
    write_lines(&mut quiet, "late", 0, 3);
    assert!(
        fence.holds_at(&quiet.history_fence()),
        "a shrink demotes the top row into history: nothing is revealed"
    );
}

#[test]
fn eviction_past_the_reader_is_evicted_not_a_gap() {
    let mut grid = tiered(4, 20);
    grid.set_scrollback_line_limit(Some(40));
    write_lines(&mut grid, "h", 0, 60);
    let fence = grid.history_fence();
    write_lines(&mut grid, "flood", 0, 200);
    // Rows leave the ring into a lazy staging buffer and are evicted when it
    // settles into the store (any history read settles it).
    let _ = grid.scrollback_mut();
    assert!(fence.holds_at(&grid.history_fence()));
    assert_eq!(
        grid.history_lines_since_fence(&fence, fence.oldest, 8)
            .err(),
        Some(HistoryFenceBroken::Evicted),
        "a row retention already dropped is refused, never skipped"
    );
}

#[test]
fn older_history_lands_before_the_oldest_line_and_keeps_every_key() {
    let mut source = tiered(4, 20);
    write_lines(&mut source, "old", 0, 120);
    let exported = export(&source, 9).expect("exported");

    // The successor: a grid built from a small carry, its counter reserved.
    let mut grid = tiered(4, 20);
    write_lines(&mut grid, "carried", 0, 6);
    let claim = grid.reserve_older_history_keys(exported.len() as u64);
    let base_before = grid.base_y();
    let history_before = history(&grid);
    let epoch_before = grid.history_renumber_epoch();

    let older = OlderHistory::build(&exported, 20, 20);
    assert_eq!(older.lines(), exported.len());
    let retained = grid.attach_older_history(older, claim).expect("attached");
    assert_eq!(retained, exported.len());

    let mut expected: Vec<String> = exported.iter().map(text).collect();
    expected.extend(history_before);
    assert_eq!(
        history(&grid),
        expected,
        "imported first, then what was there"
    );
    assert_eq!(grid.base_y(), base_before, "no live or retained key moved");
    assert!(
        grid.history_renumber_epoch() > epoch_before,
        "the cached search index is told to rebuild"
    );
}

/// A grid that CONTINUES the source's numbering (the checkpoint restore) has
/// the imported lines' own keys free below its oldest row, so the reserve
/// raises nothing and a key recorded before the handoff (a carried shell
/// mark) keeps naming its line through the import. Past the free keys only
/// the shortfall is raised (2026-09-27 review: raising by the whole import
/// renumbered every restored row while the carried marks stayed put).
#[test]
fn a_continued_numbering_reserves_only_the_shortfall() {
    let mut grid = tiered(4, 20);
    write_lines(&mut grid, "carried", 0, 6);
    grid.continue_absolute_numbering(1_000);
    let oldest = grid.oldest_absolute_row();
    let marked = grid.base_y();
    let marked_text = grid.row_text(0).unwrap_or_default();
    let lines: Vec<Line> = (0..50)
        .map(|i| Line::from(format!("old{i}").as_str()))
        .collect();
    let claim = grid.reserve_older_history_keys(lines.len() as u64);
    assert_eq!(grid.absolute_row_counter(), 1_000, "no key moved");
    assert_eq!(grid.base_y(), marked);
    grid.attach_older_history(OlderHistory::build(&lines, 20, 20), claim)
        .expect("attached");
    assert_eq!(grid.base_y(), marked, "the top row keeps its key");
    assert_eq!(grid.row_text(0).unwrap_or_default(), marked_text);
    assert_eq!(grid.oldest_absolute_row(), oldest - lines.len() as u64);
    assert_eq!(history(&grid)[0], "old0");

    // Fewer free keys than lines: only the difference is raised.
    let mut short = tiered(4, 20);
    write_lines(&mut short, "carried", 0, 6);
    let floor = u64::from(short.rows()) + short.scrollback_lines() as u64;
    short.continue_absolute_numbering(floor + 20);
    assert_eq!(short.oldest_absolute_row(), 20);
    let _ = short.reserve_older_history_keys(50);
    assert_eq!(short.absolute_row_counter(), floor + 50);
    assert_eq!(short.oldest_absolute_row(), 50);
}

#[test]
fn an_unreserved_import_raises_the_counter_and_says_it_renumbered() {
    let mut grid = tiered(4, 20);
    write_lines(&mut grid, "carried", 0, 6);
    let lines: Vec<Line> = (0..50)
        .map(|i| Line::from(format!("old{i}").as_str()))
        .collect();
    let epoch_before = grid.history_renumber_epoch();
    // A claim that reserves nothing: the counter stays where it was.
    let claim = grid.reserve_older_history_keys(0);
    grid.attach_older_history(OlderHistory::build(&lines, 20, 20), claim)
        .expect("attached");
    assert!(
        grid.absolute_row_counter() >= u64::from(grid.rows()) + grid.scrollback_lines() as u64,
        "the counter covers every retained line"
    );
    assert!(grid.history_renumber_epoch() > epoch_before);
    assert_eq!(history(&grid)[0], "old0");
}

#[test]
fn the_grids_own_retention_decides_what_an_import_keeps() {
    let mut grid = tiered(4, 20);
    grid.set_scrollback_line_limit(Some(30));
    write_lines(&mut grid, "carried", 0, 6);
    let ring_history = history(&grid);
    let lines: Vec<Line> = (0..100)
        .map(|i| Line::from(format!("old{i}").as_str()))
        .collect();
    let claim = grid.reserve_older_history_keys(100);
    grid.attach_older_history(OlderHistory::build(&lines, 20, 20), claim)
        .expect("attached");
    let kept = history(&grid);
    assert!(
        kept.len() <= 30,
        "the limit holds after the import: {kept:?}"
    );
    assert!(kept.len() > ring_history.len(), "imported lines were kept");
    assert_eq!(
        kept[kept.len() - ring_history.len()..],
        ring_history[..],
        "what the grid held stays newest"
    );
    let imported = &kept[..kept.len() - ring_history.len()];
    let first: usize = imported[0]
        .strip_prefix("old")
        .and_then(|n| n.parse().ok())
        .expect("an imported line");
    let expected: Vec<String> = (first..100).map(|i| format!("old{i}")).collect();
    assert_eq!(
        imported,
        &expected[..],
        "the OLDEST imported lines are evicted first, and the rest stay contiguous"
    );
    assert_eq!(
        grid.scrollback_line_limit(),
        Some(30),
        "the limit is unchanged"
    );
}

#[test]
fn a_width_that_moved_since_the_build_is_refused_and_the_history_handed_back() {
    let mut grid = tiered(4, 20);
    write_lines(&mut grid, "carried", 0, 6);
    let lines: Vec<Line> = (0..10)
        .map(|i| Line::from(format!("old{i}").as_str()))
        .collect();
    let older = OlderHistory::build(&lines, 20, 20);
    let claim = grid.reserve_older_history_keys(0);
    grid.resize(4, 30);
    match grid.attach_older_history(older, claim) {
        Err(OlderHistoryRefusal::WidthMoved { now, older }) => {
            assert_eq!(now, 30);
            assert_eq!(older.lines(), 10, "nothing was consumed");
        }
        other => panic!("expected WidthMoved, got {other:?}"),
    }
}

#[test]
fn a_ring_only_grid_has_nothing_to_place_history_in_front_of() {
    let mut grid = Grid::with_scrollback(4, 20, 100);
    write_lines(&mut grid, "ring", 0, 10);
    let lines = vec![Line::from("old")];
    let claim = grid.reserve_older_history_keys(0);
    assert!(matches!(
        grid.attach_older_history(OlderHistory::build(&lines, 20, 20), claim),
        Err(OlderHistoryRefusal::NoTieredStore(_))
    ));
}

/// A SCROLLBACK CLEARED AFTER THE RESERVE STAYS CLEARED (2026-09-26 review):
/// the successor imports on a worker after Commit, while the adopted shell and
/// the output it replays already run, and an attach that ignored an ED3 in
/// that window put back the lines the clear had just removed. The refusal
/// hands the built history back untouched. NEGATIVE CONTROL: the same attach
/// with no clear in between lands.
#[test]
fn a_scrollback_cleared_after_the_reserve_refuses_the_import() {
    let lines: Vec<Line> = (0..40)
        .map(|i| Line::from(format!("secret{i}").as_str()))
        .collect();
    let mut grid = tiered(4, 20);
    write_lines(&mut grid, "carried", 0, 6);
    let claim = grid.reserve_older_history_keys(40);
    assert!(grid.older_history_claim_holds(claim));
    grid.erase_scrollback();
    assert!(!grid.older_history_claim_holds(claim));
    match grid.attach_older_history(OlderHistory::build(&lines, 20, 20), claim) {
        Err(OlderHistoryRefusal::Cleared(older)) => assert_eq!(older.lines(), 40),
        other => panic!("expected Cleared, got {other:?}"),
    }
    assert!(history(&grid).is_empty(), "nothing came back");

    let mut control = tiered(4, 20);
    write_lines(&mut control, "carried", 0, 6);
    let claim = control.reserve_older_history_keys(40);
    assert_eq!(
        control
            .attach_older_history(OlderHistory::build(&lines, 20, 20), claim)
            .expect("no clear, so it lands"),
        40
    );
}

#[test]
fn a_build_for_another_width_rewraps() {
    let long = "x".repeat(30);
    let lines = vec![Line::from(long.as_str())];
    let older = OlderHistory::build(&lines, 40, 10);
    assert_eq!(older.cols(), 10);
    assert_eq!(older.lines(), 3, "30 cells at 10 columns are three rows");
    let same = OlderHistory::build(&lines, 40, 40);
    assert_eq!(same.lines(), 1);
}
