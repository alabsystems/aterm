// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Inline images survive width-changing reflow through `Terminal::resize`.

use aterm_core::terminal::Terminal;

use crate::osc1337::{PNG_MAGIC, osc_1337_file};

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

    term.resize(10, 40);
    assert_eq!(image_cells(&term), before, "a grow must keep the image");
    term.resize(10, 20);
    assert_eq!(
        image_cells(&term),
        before,
        "returning to the original width must leave the image exactly as it was"
    );

    for r in 1..=3u16 {
        assert_eq!(
            term.grid().row_text(r).as_deref(),
            Some(""),
            "row {r} is an image row and must still extract as empty text"
        );
    }
}

#[test]
fn an_image_follows_the_text_it_was_printed_under() {
    let mut term = Terminal::new(10, 20);
    term.process(b"ABCDEFGHIJKLMNOPQRSTUVWXY\r\n");
    place_card(&mut term, "inline=1;width=4;height=2");
    term.process(b"BOT");

    let rows_before: Vec<u16> = image_cells(&term).iter().map(|c| c.0).collect();
    assert_eq!(rows_before, vec![2, 2, 2, 2, 3, 3, 3, 3]);

    term.resize(10, 10);
    let rows_after: Vec<u16> = image_cells(&term).iter().map(|c| c.0).collect();
    assert_eq!(
        rows_after,
        vec![3, 3, 3, 3, 4, 4, 4, 4],
        "the footprint must ride the rewrap with its line"
    );
    assert_eq!(term.grid().row_text(2).as_deref(), Some("UVWXY"));
    assert_eq!(term.grid().row_text(5).as_deref(), Some("BOT"));
}

#[test]
fn an_over_wide_footprint_clips_instead_of_wrapping() {
    let mut term = Terminal::new(8, 20);
    term.process(b"TOP\r\n");
    place_card(&mut term, "inline=1;width=18;height=2");
    term.process(b"BOT");
    assert_eq!(image_cells(&term).len(), 36);

    term.resize(8, 10);
    let after = image_cells(&term);
    assert_eq!(after.len(), 20, "10 columns x 2 rows survive");
    assert!(
        after
            .iter()
            .all(|&(r, c, _, tc)| (r == 1 || r == 2) && c < 10 && tc == c),
        "surviving tiles keep their column and tile index: {after:?}"
    );
    assert_eq!(term.grid().row_text(0).as_deref(), Some("TOP"));
    assert_eq!(term.grid().row_text(3).as_deref(), Some("BOT"));
}

#[test]
fn image_survives_a_combined_resize_and_an_alt_screen_round_trip() {
    let mut term = Terminal::new(10, 20);
    term.process(b"TOP\r\n");
    place_card(&mut term, "inline=1;width=4;height=3");
    let before = image_cells(&term);
    assert_eq!(before.len(), 12);

    term.resize(6, 14);
    assert_eq!(image_cells(&term), before, "rows+cols together");

    term.process(b"\x1b[?1049h");
    term.resize(6, 13);
    term.process(b"\x1b[?1049l");
    assert_eq!(
        image_cells(&term),
        before,
        "the saved primary's image must survive a width change made while alt was up"
    );
}
