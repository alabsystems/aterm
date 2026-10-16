// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Inline images survive a width-changing reflow through `Terminal::resize`: an image cell
//! carries a cell extra and no glyph, so an image-only row has `Row::len() == 0`, and
//! column reflow must still carry its cells.

use aterm_core::terminal::Terminal;

use crate::osc1337::{PNG_MAGIC, osc_1337_file};

/// Every image-covered cell as `(row, col, tile_row, tile_col)`, in scan order.
fn image_cells(term: &Terminal) -> Vec<(u16, u16, u16, u16)> {
    let grid = term.grid();
    let mut v = Vec::new();
    for r in 0..grid.rows() {
        for c in 0..grid.cols() {
            if let Some(i) = grid.cell_extra(r, c).and_then(|e| e.image()) {
                v.push((r, c, i.cell_row, i.cell_col));
            }
        }
    }
    v
}

fn place_card(term: &mut Terminal, args: &str) {
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    term.process(&osc_1337_file(args, &payload));
}

/// A 4x3 image placed between two text lines keeps every tile, at the same
/// anchor, across a shrink, a grow, and the round trip back.
#[test]
fn a_column_resize_keeps_every_tile_of_an_inline_image() {
    let mut term = Terminal::new(10, 20);
    term.process(b"IMGTOP\r\n");
    place_card(&mut term, "inline=1;width=4;height=3");
    term.process(b"IMGBOT\r\n");

    let before = image_cells(&term);
    assert_eq!(before.len(), 12, "4x3 footprint covers 12 cells");

    term.resize(10, 19);
    assert_eq!(
        image_cells(&term),
        before,
        "80->79-style shrink must keep the image, tiles and anchor intact"
    );

    // Grow past the original width, then back: still every tile.
    term.resize(10, 40);
    assert_eq!(image_cells(&term), before, "a grow must keep the image");
    term.resize(10, 20);
    assert_eq!(
        image_cells(&term),
        before,
        "returning to the original width must leave the image exactly as it was"
    );

    // Image rows stay blank as text: reflow must not leak spaces into the row
    // content, or copying an image row would yield whitespace.
    for r in 1..=3u16 {
        assert_eq!(
            term.grid().row_text(r).as_deref(),
            Some(""),
            "row {r} is an image row and must still extract as empty text"
        );
    }
}

/// The image follows its line: when a long line above rewraps into more rows,
/// the footprint moves down with its text.
#[test]
fn an_image_follows_the_text_it_was_printed_under() {
    let mut term = Terminal::new(10, 20);
    // 25 chars: one wrap at 20 columns, two at 10.
    term.process(b"ABCDEFGHIJKLMNOPQRSTUVWXY\r\n");
    place_card(&mut term, "inline=1;width=4;height=2");
    term.process(b"BOT");

    let rows_before: Vec<u16> = image_cells(&term).iter().map(|c| c.0).collect();
    assert_eq!(rows_before, vec![2, 2, 2, 2, 3, 3, 3, 3]);

    term.resize(10, 10);
    // The header now occupies three rows instead of two, so the footprint sits
    // one row lower — directly under its text, with `BOT` directly below it.
    let rows_after: Vec<u16> = image_cells(&term).iter().map(|c| c.0).collect();
    assert_eq!(
        rows_after,
        vec![3, 3, 3, 3, 4, 4, 4, 4],
        "the footprint must ride the rewrap with its line"
    );
    assert_eq!(term.grid().row_text(2).as_deref(), Some("UVWXY"));
    assert_eq!(term.grid().row_text(5).as_deref(), Some("BOT"));
}

/// A footprint wider than the new window clips at the right margin rather
/// than wrapping onto an invented row.
#[test]
fn an_over_wide_footprint_clips_instead_of_wrapping() {
    let mut term = Terminal::new(8, 20);
    term.process(b"TOP\r\n");
    place_card(&mut term, "inline=1;width=18;height=2");
    term.process(b"BOT");
    assert_eq!(image_cells(&term).len(), 36);

    term.resize(8, 10);
    let after = image_cells(&term);
    // Exactly the columns that still fit, on exactly the two original rows.
    assert_eq!(after.len(), 20, "10 columns x 2 rows survive");
    assert!(
        after
            .iter()
            .all(|&(r, c, _, tc)| (r == 1 || r == 2) && c < 10 && tc == c),
        "surviving tiles keep their column and tile index: {after:?}"
    );
    // No invented rows: the text below is where it was.
    assert_eq!(term.grid().row_text(0).as_deref(), Some("TOP"));
    assert_eq!(term.grid().row_text(3).as_deref(), Some("BOT"));
}

/// A rows+columns resize keeps the image, and so does the alt-screen round
/// trip, where the saved primary is the grid that reflows.
#[test]
fn image_survives_a_combined_resize_and_an_alt_screen_round_trip() {
    let mut term = Terminal::new(10, 20);
    term.process(b"TOP\r\n");
    place_card(&mut term, "inline=1;width=4;height=3");
    let before = image_cells(&term);
    assert_eq!(before.len(), 12);

    term.resize(6, 14);
    assert_eq!(image_cells(&term), before, "rows+cols together");

    // Alt screen up: the active grid is app-managed (no reflow) and the saved
    // primary — the one holding the image — is the grid that rewraps.
    term.process(b"\x1b[?1049h");
    term.resize(6, 13);
    term.process(b"\x1b[?1049l");
    assert_eq!(
        image_cells(&term),
        before,
        "the saved primary's image must survive a width change made while alt was up"
    );
}
