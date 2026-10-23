// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rapid width-sweep reflow round-trip tests.

use super::super::super::*;

fn physical_snapshot(grid: &Grid) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for i in 0..grid.scrollback_lines() {
        let line = grid.get_history_line(i).expect("history line present");
        out.push((
            line.to_string().trim_end_matches(' ').to_string(),
            line.is_wrapped(),
        ));
    }
    for r in 0..grid.rows() {
        let row = grid.row(r).expect("visible row present");
        out.push((
            row.to_string().trim_end_matches(' ').to_string(),
            row.is_wrapped(),
        ));
    }
    out
}

fn logical_lines(snapshot: &[(String, bool)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (text, wrapped) in snapshot {
        if *wrapped && !out.is_empty() {
            out.last_mut().unwrap().push_str(text);
        } else {
            out.push(text.clone());
        }
    }
    out
}

fn type_line(grid: &mut Grid, text: &str) {
    for c in text.chars() {
        grid.write_char_wrap(c);
    }
}

fn feed(grid: &mut Grid) {
    grid.line_feed();
    grid.carriage_return();
}

fn build_sweep_grid(grid: &mut Grid) -> String {
    for i in 0..8u16 {
        type_line(grid, &format!("filler-{i}"));
        feed(grid);
    }
    let long: String = format!("{}8UtaKZtFqk", "x".repeat(60));
    type_line(grid, &long);
    feed(grid);
    type_line(grid, "$ ");
    long
}

const SWEEP: [u16; 6] = [24, 16, 9, 13, 21, 30];

fn sweep(grid: &mut Grid) {
    for w in SWEEP {
        grid.resize(6, w);
        grid.assert_invariants();
    }
}

fn tiered_grid() -> Grid {
    let scrollback = Scrollback::new(1000, 10_000, 10_000_000);
    Grid::with_tiered_scrollback(6, 30, 32, scrollback)
}

fn offloaded_resize(grid: &mut Grid, w: u16) {
    match grid.resize_offloading_scrollback(6, w) {
        Some(job) => grid.reattach_reflowed_scrollback(job.reflow()),
        None => unreachable!("tiered store must detach on a width change"),
    }
    grid.assert_invariants();
}

fn assert_sweep_restores_wrap_topology(mut grid: Grid) {
    let long = build_sweep_grid(&mut grid);

    let before = physical_snapshot(&grid);
    assert!(
        logical_lines(&before).iter().any(|l| l == &long),
        "sanity: the long line is one logical line before the sweep"
    );

    sweep(&mut grid);

    let after = physical_snapshot(&grid);
    assert!(
        logical_lines(&after).iter().any(|l| l == &long),
        "logical line must survive the sweep intact: {after:#?}"
    );
    assert_eq!(
        after, before,
        "returning to the original width must restore the exact wrap topology"
    );
}

#[test]
fn width_sweep_round_trip_restores_wrap_topology_ring() {
    assert_sweep_restores_wrap_topology(Grid::with_scrollback(6, 30, 1000));
}

#[test]
fn width_sweep_round_trip_restores_wrap_topology_tiered() {
    assert_sweep_restores_wrap_topology(tiered_grid());
}

#[test]
fn width_sweep_preserves_spaces_at_chunk_boundaries() {
    let mut grid = Grid::with_scrollback(6, 30, 1000);
    let line = "The quick brown fox jumps over the lazy dog 0123456789";
    type_line(&mut grid, line);
    feed(&mut grid);
    type_line(&mut grid, "$ ");

    sweep(&mut grid);

    let joined: String = logical_lines(&physical_snapshot(&grid)).join("\n");
    assert!(
        joined.contains(line),
        "every space must survive the sweep: {joined:?}"
    );
}

#[test]
fn width_sweep_round_trip_restores_wrap_topology_offloaded() {
    let mut grid = tiered_grid();
    let long = build_sweep_grid(&mut grid);

    let before = physical_snapshot(&grid);

    for w in SWEEP {
        offloaded_resize(&mut grid, w);
    }

    let after = physical_snapshot(&grid);
    assert!(
        logical_lines(&after).iter().any(|l| l == &long),
        "logical line must survive the offloaded sweep intact: {after:#?}"
    );
    assert_eq!(
        after, before,
        "the offloaded sweep must restore the exact wrap topology at settle"
    );
}

fn settled_content(grid: &Grid) -> Vec<(String, bool)> {
    let mut snap = physical_snapshot(grid);
    while snap.last().is_some_and(|(t, w)| t.is_empty() && !w) {
        snap.pop();
    }
    snap
}

#[test]
fn offloaded_resize_while_scrolled_back_matches_unscrolled_topology() {
    let build = || {
        let mut grid = tiered_grid();
        build_sweep_grid(&mut grid);
        grid
    };

    let steps = [9u16, 26];

    let mut reference = build();
    for w in steps {
        offloaded_resize(&mut reference, w);
    }

    let mut grid = build();
    grid.scroll_display(3);
    assert!(grid.display_offset() > 0, "sanity: reader is scrolled back");
    for w in steps {
        offloaded_resize(&mut grid, w);
    }
    grid.scroll_to_bottom();

    assert_eq!(
        settled_content(&grid),
        settled_content(&reference),
        "a scrolled-back offloaded resize must produce the same wrap \
         topology as the never-scrolled resize of the identical grid"
    );
}

#[test]
fn offloaded_resize_while_scrolled_back_keeps_erased_band_blank() {
    let mut grid = tiered_grid();
    for i in 0..9u16 {
        type_line(&mut grid, &format!("filler-{i}"));
        feed(&mut grid);
    }
    grid.move_cursor_to(2, 0);
    grid.erase_to_end_of_screen();
    assert_eq!(grid.row(0).unwrap().to_string().trim_end(), "filler-4");

    grid.scroll_display(2);
    offloaded_resize(&mut grid, 40);
    grid.scroll_to_bottom();

    assert_eq!(
        grid.row(0).unwrap().to_string().trim_end(),
        "filler-4",
        "the deficit fill must not resurrect history above the erased band"
    );
    assert_eq!(grid.cursor_row(), 2, "the cursor must not be displaced");
    for r in 3..grid.rows() {
        assert!(
            grid.row(r).unwrap().is_empty(),
            "row {r} of the erased band must stay blank, found {:?}",
            grid.row(r).unwrap().to_string()
        );
    }
}

#[test]
fn clear_mid_flight_does_not_resurrect_history_offloaded() {
    let mut grid = tiered_grid();
    build_sweep_grid(&mut grid);

    let job = grid
        .resize_offloading_scrollback(6, 40)
        .expect("tiered store must detach on a width change");

    grid.erase_screen();
    grid.move_cursor_to(0, 0);

    grid.reattach_reflowed_scrollback(job.reflow());
    grid.assert_invariants();

    for r in 0..grid.rows() {
        assert!(
            grid.row(r).unwrap().is_empty(),
            "row {r} must stay blank after a mid-flight ED 2, found {:?}",
            grid.row(r).unwrap().to_string()
        );
    }
    assert!(
        grid.scrollback_lines() > 0,
        "ED 2 must not erase the re-attached history"
    );
}

#[test]
fn early_wrap_pad_preserves_real_trailing_spaces() {
    use crate::CellFlags;
    let mut grid = Grid::new(5, 10);
    type_line(&mut grid, "ABCDEF   "); // cols 0..=8: text + three real spaces
    grid.write_wide_char_wrap_styled(
        '世',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert!(grid.row(1).unwrap().is_wrapped());
    assert!(grid.row(1).unwrap().get(0).unwrap().is_wide());

    grid.resize(5, 20);
    grid.assert_invariants();
    let row0 = grid.row(0).unwrap();
    for col in 6..9 {
        assert_eq!(
            row0.get(col).unwrap().char(),
            ' ',
            "real trailing space at col {col} must survive the merge, got {:?}",
            row0.to_string()
        );
    }
    assert!(
        row0.get(9).unwrap().is_wide(),
        "the wide char follows the three REAL spaces — only the single \
         early-wrap hole vanishes, got {:?}",
        row0.to_string()
    );
}

#[test]
fn grow_merges_early_wrapped_wide_char_without_phantom_space() {
    use crate::CellFlags;
    let mut grid = Grid::new(5, 10);
    type_line(&mut grid, "ABCDEFGHI");
    grid.write_wide_char_wrap_styled(
        '世',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    assert!(
        grid.row(1).unwrap().is_wrapped(),
        "sanity: the wide char early-wrapped onto a continuation row"
    );
    assert!(grid.row(1).unwrap().get(0).unwrap().is_wide());

    grid.resize(5, 20);
    grid.assert_invariants();

    let row0 = grid.row(0).unwrap();
    assert!(
        row0.get(9).unwrap().is_wide(),
        "the wide char must sit directly after 'I' — the early-wrap hole is \
         not content, got {:?}",
        row0.to_string()
    );

    grid.resize(5, 10);
    grid.assert_invariants();
    assert!(grid.row(1).unwrap().is_wrapped());
    assert!(
        grid.row(1).unwrap().get(0).unwrap().is_wide(),
        "the round trip must restore the early wrap, got {:?}",
        grid.row(1).unwrap().to_string()
    );
}

#[test]
fn early_wrapped_wide_char_survives_sweep_across_history_boundary() {
    use crate::CellFlags;
    let mut grid = Grid::with_scrollback(3, 10, 100);
    type_line(&mut grid, "ABCDEFGHI");
    grid.write_wide_char_wrap_styled(
        '世',
        crate::PackedColor::DEFAULT_FG,
        crate::PackedColor::DEFAULT_BG,
        CellFlags::empty(),
    );
    feed(&mut grid);
    feed(&mut grid);
    assert!(grid.scrollback_lines() >= 1, "sanity: head is in history");
    assert!(
        grid.row(0).unwrap().is_wrapped(),
        "sanity: the tail is the boundary-straddling continuation"
    );

    grid.resize(3, 20);
    grid.assert_invariants();
    let joined = logical_lines(&physical_snapshot(&grid)).join("\n");
    assert!(
        joined.contains("ABCDEFGHI世"),
        "no phantom space may appear before the early-wrapped wide char: {joined:?}"
    );

    grid.resize(3, 10);
    grid.assert_invariants();
    let joined = logical_lines(&physical_snapshot(&grid)).join("\n");
    assert!(
        joined.contains("ABCDEFGHI世"),
        "the round trip must keep the logical line intact: {joined:?}"
    );
}
