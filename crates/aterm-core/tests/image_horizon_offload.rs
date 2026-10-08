// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The inline-image retention horizon, end to end through the real `Terminal`.
//!
//! An inline image is retained only while its line is held uncompressed: the
//! grid's scrolled-row ring, the deferred-line staging buffer, then the tiered
//! store's hot tier. The warm/cold/disk wire format has no image section (see
//! `Line::serialize`). One test each:
//!   * inside the horizon the picture is whole;
//!   * past it the row is empty and the surrounding text is untouched;
//!   * the crossing is reported (`Terminal::scrollback_image_rows_dropped`).

use aterm_core::render::RenderInput;
use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

use crate::osc1337::{PNG_MAGIC, osc_1337_file};

const ROWS: u16 = 10;
const COLS: u16 = 20;
/// Footprint height of the card every test places.
const CARD_ROWS: usize = 3;

fn place_card(term: &mut Terminal) {
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    term.process(&osc_1337_file("inline=1;width=4;height=3", &payload));
}

/// Every image placement in the rendered frame.
fn frame_image_cells(frame: &RenderInput) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    for (r, row) in frame.images.iter().enumerate() {
        for (c, _) in row {
            v.push((r, *c));
        }
    }
    v
}

/// A terminal with the shortest horizon this layer allows: a 4-line ring in
/// front of a store holding 4 uncompressed lines, compressing in blocks of 4.
/// The staging buffer drains on its own fixed threshold, hence the size of
/// [`PAST_HORIZON`].
fn terminal_with_short_horizon() -> Terminal {
    Terminal::with_scrollback(
        ROWS,
        COLS,
        4,
        Scrollback::with_block_size(4, 1_000, 100_000_000, 4),
    )
}

/// Enough scrolled-off text to carry a line past the ring, the staging
/// buffer's drain threshold, and the store's hot tier.
const PAST_HORIZON: usize = 1_100;

/// Scroll off `n` lines of ordinary text.
fn feed(term: &mut Terminal, n: usize) {
    for i in 0..n {
        term.process(format!("f{i}\r\n").as_bytes());
    }
}

#[test]
fn inside_the_horizon_a_scrolled_off_picture_is_whole() {
    // The premise the limit is a limit ON. Without this the "past the horizon"
    // test below would pass on a terminal that never kept a picture at all.
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
    // The picture goes, the text around it stays, and nothing is drawn in its
    // place: the row is what a picture-less terminal would show.
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

    // The text around it is intact — this is retention loss of a picture, not
    // of history.
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

    // And the footprint's own rows are blank — not spaces the screen never
    // showed, not a fragment of the card.
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
    // One count per footprint row (the 4x3 card reports three), the same unit
    // as `scrollback_truncated_lines`.
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

    // Monotonic: more crossings only ever add.
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
    // Non-vacuity for the signal itself: the counter must be driven by pictures
    // crossing, not by history being compressed.
    let mut term = terminal_with_short_horizon();
    feed(&mut term, PAST_HORIZON);
    assert_eq!(
        term.scrollback_image_rows_dropped(),
        0,
        "text-only history compresses without reporting an image crossing"
    );
}
