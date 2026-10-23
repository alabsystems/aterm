// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Inline-image retention horizon tests across scrollback compression.

use aterm_core::render::RenderInput;
use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

use crate::osc1337::{PNG_MAGIC, osc_1337_file};

const ROWS: u16 = 10;
const COLS: u16 = 20;
const CARD_ROWS: usize = 3;

fn place_card(term: &mut Terminal) {
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    term.process(&osc_1337_file("inline=1;width=4;height=3", &payload));
}

fn frame_image_cells(frame: &RenderInput) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    for (r, row) in frame.images.iter().enumerate() {
        for (c, _) in row {
            v.push((r, *c));
        }
    }
    v
}

fn terminal_with_short_horizon() -> Terminal {
    Terminal::with_scrollback(
        ROWS,
        COLS,
        4,
        Scrollback::with_block_size(4, 1_000, 100_000_000, 4),
    )
}

const PAST_HORIZON: usize = 1_100;

fn feed(term: &mut Terminal, n: usize) {
    for i in 0..n {
        term.process(format!("f{i}\r\n").as_bytes());
    }
}

#[test]
fn inside_the_horizon_a_scrolled_off_picture_is_whole() {
    let mut term = terminal_with_short_horizon();
    term.process(b"ABOVE\r\n");
    place_card(&mut term);
    term.process(b"BELOW\r\n");
    feed(&mut term, 4);

    term.scroll_to_top();
    assert_eq!(
        frame_image_cells(&term.cell_frame(ROWS.into(), COLS.into())).len(),
        12,
        "still uncompressed: all twelve tiles of the 4x3 card are there"
    );
    assert_eq!(
        term.scrollback_image_rows_dropped(),
        0,
        "nothing has crossed the horizon yet"
    );
}

#[test]
fn past_the_horizon_the_row_is_the_empty_row_it_always_was_under_the_picture() {
    let mut term = terminal_with_short_horizon();
    term.process(b"ABOVE\r\n");
    place_card(&mut term);
    term.process(b"BELOW\r\n");
    feed(&mut term, PAST_HORIZON);

    term.scroll_to_top();
    let frame = term.cell_frame(ROWS.into(), COLS.into());
    assert!(
        frame_image_cells(&frame).is_empty(),
        "past the horizon no tile of the card may be painted"
    );

    let text: Vec<String> = frame
        .cells
        .iter()
        .map(|line| line.iter().map(|c| c.ch).collect::<String>())
        .collect();
    let trimmed: Vec<&str> = text.iter().map(|s| s.trim_end()).collect();
    assert!(
        trimmed.contains(&"ABOVE"),
        "the line above the picture is still history: {trimmed:?}"
    );
    assert!(
        trimmed.contains(&"BELOW"),
        "the line below the picture is still history: {trimmed:?}"
    );

    let above = trimmed
        .iter()
        .position(|s| *s == "ABOVE")
        .expect("ABOVE is on screen");
    for offset in 1..=CARD_ROWS {
        assert_eq!(
            trimmed.get(above + offset),
            Some(&""),
            "footprint row {offset} must render empty, not invented content"
        );
    }
}

#[test]
fn crossing_the_horizon_is_reported_and_never_silent() {
    let mut term = terminal_with_short_horizon();
    place_card(&mut term);
    feed(&mut term, PAST_HORIZON);

    assert_eq!(
        term.scrollback_image_rows_dropped(),
        CARD_ROWS as u64,
        "every footprint row that crossed must be reported exactly once"
    );
    assert_eq!(
        term.scrollback_truncated_lines(),
        0,
        "no LINE was lost — the image horizon is a different quantity and must \
         not be folded into the truncation signal"
    );

    place_card(&mut term);
    feed(&mut term, PAST_HORIZON);
    assert_eq!(
        term.scrollback_image_rows_dropped(),
        (CARD_ROWS * 2) as u64,
        "a second card that crossed adds its own rows"
    );
}

#[test]
fn a_session_that_never_drew_a_picture_never_reports_a_crossing() {
    let mut term = terminal_with_short_horizon();
    feed(&mut term, PAST_HORIZON);
    assert_eq!(
        term.scrollback_image_rows_dropped(),
        0,
        "text-only history compresses without reporting an image crossing"
    );
}
