// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! INLINE IMAGES SURVIVE THE SCROLLBACK BOUNDARY.
//!
//! The defect this pins: an inline image (iTerm2 OSC 1337 `File=`, and the sixel
//! path that reuses the same placement) is stored as one `ImageRef` per covered
//! cell in the grid's `CellExtras` side table. `aterm_scrollback::Line` carried
//! hyperlinks and SGR 58 underline colours but had NO image field at all, so when
//! a footprint row scrolled off the top there was nowhere for its `ImageRef` to
//! go. Scrolling back showed a blank hole where the picture had been, and no
//! later action ever brought it back.
//!
//! These tests drive the REAL `Terminal`: OSC 1337 in, ordinary line feeds to
//! push it off the top, `scroll_to_top`, then the render snapshot the GUI and
//! the GPU renderer both consume (`cell_frame(...).images`).

use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;

/// A payload the OSC 1337 handler classifies as PNG. The engine never decodes
/// pixels (that is the renderer's job), so the bytes after the magic are inert.
const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

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

fn place_card(term: &mut Terminal, args: &str) {
    let payload = [PNG_MAGIC, &[0u8; 16][..]].concat();
    term.process(&osc_1337_file(args, &payload));
}

/// Every image placement in the rendered frame as `(row, col, tile_row, tile_col)`.
fn frame_image_cells(frame: &RenderInput) -> Vec<(usize, usize, u16, u16)> {
    let mut v = Vec::new();
    for (r, row) in frame.images.iter().enumerate() {
        for (c, iref) in row {
            v.push((r, *c, iref.cell_row, iref.cell_col));
        }
    }
    v
}

/// A 4x3 image printed at the top, then pushed off the top by ordinary output,
/// is still there when the user scrolls back to it.
///
/// THE USER-VISIBLE OUTCOME, not the mechanism. Scrolled rows reach history
/// through two tiers — a fast ring that carries their extras directly, and the
/// `Line` store a row is converted into once it leaves that ring — and this
/// asserts only that the picture comes back, which it does through either.
/// Tearing the `Line` conversion out leaves this green, so it is NOT the test
/// that pins the storage this lane is about. That one has to reach the tier
/// directly, and does:
/// `aterm_grid::grid::tests::scrollback_images::a_scrolled_off_image_row_round_
/// trips_through_a_line_back_to_per_cell_refs`, with
/// `the_ring_fast_materializer_agrees_with_the_line_round_trip_on_an_image_row`
/// holding the two tiers to the same answer. Both fail without it.
#[test]
fn an_image_scrolled_off_the_top_is_still_there_when_you_scroll_back() {
    let mut term = Terminal::new(10, 20);
    term.process(b"IMGTOP\r\n");
    place_card(&mut term, "inline=1;width=4;height=3");
    term.process(b"IMGBOT\r\n");

    let live = frame_image_cells(&term.cell_frame(10, 20));
    assert_eq!(live.len(), 12, "4x3 footprint covers 12 cells while live");

    // Push it well off the top with ordinary text.
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
    // Same tile geometry, same anchor column, and the three footprint rows are
    // adjacent — i.e. the picture, not twelve scattered fragments.
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

/// The payload is SHARED, not copied per row: the scrolled-back tiles must point
/// at one allocation, or a full-width image would cost its own megabytes per row
/// of history.
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

/// A PARTIAL scroll puts history above and live content below in one frame, and
/// a live image has to land where the text it sits in landed.
///
/// The image cells live in the grid's side table at LIVE coordinates while the
/// viewport is drawn through `display_offset`, so a frame that reads that table
/// at the raw viewport row paints a live picture `display_offset` rows too high
/// — over whatever history is occupying those rows. Only a partial scroll shows
/// it: at offset 0 the two coordinate spaces coincide, and scrolled fully to the
/// top there is no live row on screen to misplace.
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

    // Scroll back by ONE line: every row on screen shifts down by exactly one,
    // the image included, and nothing may be dropped.
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
