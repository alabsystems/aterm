// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Inline images survive the scrollback boundary and appear in `cell_frame(..).images`.

use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;

use crate::osc1337::{PNG_MAGIC, osc_1337_file};

fn place_card(term: &mut Terminal, args: &str) {
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    term.process(&osc_1337_file(args, &payload));
}

fn frame_image_cells(frame: &RenderInput) -> Vec<(usize, usize, u16, u16)> {
    let mut v = Vec::new();
    for (r, row) in frame.images.iter().enumerate() {
        for (c, iref) in row {
            v.push((r, *c, iref.cell_row, iref.cell_col));
        }
    }
    v
}

#[test]
fn an_image_scrolled_off_the_top_is_still_there_when_you_scroll_back() {
    let mut term = Terminal::new(10, 20);
    term.process(b"IMGTOP\r\n");
    place_card(&mut term, "inline=1;width=4;height=3");
    term.process(b"IMGBOT\r\n");

    let live = frame_image_cells(&term.cell_frame(10, 20));
    assert_eq!(live.len(), 12, "4x3 footprint covers 12 cells while live");

    for i in 0..40 {
        term.process(format!("f{i}\r\n").as_bytes());
    }
    assert!(
        frame_image_cells(&term.cell_frame(10, 20)).is_empty(),
        "the image must be off-screen after 40 lines of output"
    );

    term.scroll_to_top();
    let back = frame_image_cells(&term.cell_frame(10, 20));
    assert_eq!(
        back.len(),
        12,
        "scrolling back to a 4x3 inline image must show all 12 tiles, not a hole"
    );
    let tiles: Vec<(u16, u16)> = back.iter().map(|&(_, _, tr, tc)| (tr, tc)).collect();
    let expected: Vec<(u16, u16)> = (0..3u16)
        .flat_map(|r| (0..4u16).map(move |c| (r, c)))
        .collect();
    assert_eq!(
        tiles, expected,
        "every tile keeps its place in the footprint"
    );
    let rows: Vec<usize> = back.iter().map(|&(r, ..)| r).collect();
    assert_eq!(rows[0] + 1, rows[4], "footprint rows must stay adjacent");
    assert_eq!(rows[4] + 1, rows[8], "footprint rows must stay adjacent");
}

#[test]
fn a_scrolled_back_image_shares_one_payload_across_its_rows() {
    let mut term = Terminal::new(10, 20);
    place_card(&mut term, "inline=1;width=4;height=3");
    for i in 0..40 {
        term.process(format!("f{i}\r\n").as_bytes());
    }
    term.scroll_to_top();
    let frame = term.cell_frame(10, 20);
    let mut refs = Vec::new();
    for row in &frame.images {
        for (_, iref) in row {
            refs.push(std::sync::Arc::as_ptr(&iref.image));
        }
    }
    assert_eq!(refs.len(), 12, "all twelve tiles are back");
    assert!(
        refs.windows(2).all(|w| w[0] == w[1]),
        "every restored tile must share the ONE image payload"
    );
}

#[test]
fn a_live_image_keeps_its_row_while_the_viewport_is_partly_scrolled_back() {
    let mut term = Terminal::new(10, 20);
    for i in 0..6 {
        term.process(format!("hist{i}\r\n").as_bytes());
    }
    place_card(&mut term, "inline=1;width=4;height=3");
    term.process(b"AFTER\r\n");

    let unscrolled: Vec<(usize, usize)> = frame_image_cells(&term.cell_frame(10, 20))
        .iter()
        .map(|&(r, c, ..)| (r, c))
        .collect();
    assert_eq!(
        unscrolled.len(),
        12,
        "the footprint is on screen to begin with"
    );

    term.scroll_display(1);
    let scrolled: Vec<(usize, usize)> = frame_image_cells(&term.cell_frame(10, 20))
        .iter()
        .map(|&(r, c, ..)| (r, c))
        .collect();
    let expected: Vec<(usize, usize)> = unscrolled.iter().map(|&(r, c)| (r + 1, c)).collect();
    assert_eq!(
        scrolled, expected,
        "a one-line scroll moves the live footprint down exactly one row"
    );
}

#[test]
fn an_image_a_taller_window_brings_back_keeps_its_tiles() {
    for erase in ["", "\x1b[2J\x1b[H"] {
        let mut term = Terminal::new(8, 20);
        place_card(&mut term, "inline=1;width=4;height=1");
        term.process(b"\r\none\r\ntwo\r\nthree\r\nfour\r\nfive");
        let live = frame_image_cells(&term.cell_frame(8, 20));
        assert_eq!(live.len(), 4, "precondition: the picture is on screen");
        term.resize(4, 20);
        term.process(erase.as_bytes());
        term.resize(8, 20);
        if erase.is_empty() {
            assert_eq!(
                frame_image_cells(&term.cell_frame(8, 20)),
                live,
                "the revealed row keeps its picture"
            );
        }
        for i in 0..20 {
            term.process(format!("\r\nf{i}").as_bytes());
        }
        term.scroll_to_top();
        assert_eq!(
            frame_image_cells(&term.cell_frame(8, 20)).len(),
            4,
            "{erase:?}: scrolled off again, the picture is still in history"
        );
    }
}
