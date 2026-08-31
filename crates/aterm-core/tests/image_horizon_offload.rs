// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! THE IMAGE RETENTION HORIZON, end to end through the real `Terminal`.
//!
//! `image_survives_scrollback.rs` pins that a picture follows its text over the
//! scrollback boundary. This pins how FAR — the limit on the other side of that
//! promise, which is a decision and not an oversight:
//!
//! an inline image is retained for exactly as long as its line is held
//! UNCOMPRESSED — through the grid's scrolled-row ring, the deferred-line
//! staging buffer behind it, and then the tiered store's hot tier. The wire
//! format the warm/cold/disk tiers keep lines in has no image section, because
//! a payload shared by every row of a footprint would be written once per
//! COVERED ROW there — see `Line::serialize`.
//!
//! So the outcome a person gets is decided rather than accidental, and stated
//! in three parts, one test each:
//!   * inside the horizon the picture is whole;
//!   * past it the row is the EMPTY row it always was under the picture, with
//!     the surrounding text untouched — nothing half-drawn, nothing invented;
//!   * the crossing is REPORTED (`Terminal::scrollback_image_rows_dropped`),
//!     never silent.

use aterm_core::render::RenderInput;
use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

/// A payload the OSC 1337 handler classifies as PNG. The engine never decodes
/// pixels, so the bytes after the magic are inert.
const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

const ROWS: u16 = 10;
const COLS: u16 = 20;
/// Footprint height of the card every test places.
const CARD_ROWS: usize = 3;

fn osc_1337_file(args: &str, payload: &[u8]) -> Vec<u8> {
    let b64 = aterm_codec::base64::encode(payload).expect("encode");
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1b]1337;File=");
    out.extend_from_slice(args.as_bytes());
    out.push(b':');
    out.extend_from_slice(b64.as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

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

/// A terminal whose horizon is as SHORT as this layer allows: a 4-line
/// scrolled-row ring in front of a store that holds 4 uncompressed lines and
/// compresses in blocks of 4. The staging buffer between them drains on its own
/// fixed threshold, which is why [`PAST_HORIZON`] is a four-figure number and
/// not a handful of lines. The production numbers (a 10k ring, the same staging
/// buffer, a 1000-line hot tier) are the same shape, just further away.
fn terminal_with_short_horizon() -> Terminal {
    Terminal::with_scrollback(
        ROWS,
        COLS,
        4,
        Scrollback::with_block_size(4, 1_000, 100_000_000, 4),
    )
}

/// Enough scrolled-off text to carry a line ALL the way across: past the ring,
/// past the deferred-line staging buffer's drain threshold, and out of the
/// store's uncompressed hot tier.
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
    // The user-visible outcome, and the whole reason the limit is stated: the
    // picture goes, the text it sat between does NOT, and nothing is drawn in
    // its place. A blank row here is the row a picture-less terminal would have
    // shown — not a hole punched in history.
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

    // The text around it is intact — this is retention loss of a PICTURE, not
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

    // And the footprint's own rows are BLANK — not spaces the screen never
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
    // A named limit a host can surface. One count per FOOTPRINT ROW, so the
    // 4x3 card reports three — the same unit the sibling retention signal
    // (`scrollback_truncated_lines`) uses.
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
