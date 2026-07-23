// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The pixel layer of the replay / lash-mirror seed (astream determinism thesis,
// ATERM_DESIGN §8 read_image). The aterm-core seed proves the SCREEN STATE
// (text + full checkpoint projection) is a pure function of the output-byte
// log. This proves the next layer: the RASTERIZER is a pure function of that
// state, so a replayed/lashed viewer renders byte-identical PIXELS to the live
// host. Same-run comparison (one machine, one font), so it is independent of
// which font a machine happens to have — what it asserts is purity, not a
// pinned cross-machine image.

use aterm_core::terminal::{ClockReading, Terminal};
use aterm_render::{Renderer, Theme};

const ROWS: usize = 8;
const COLS: usize = 24;

// Output log with color, attributes, and a full-width CJK character so the
// rendered frame is non-trivial.
const SESSION: &[&[u8]] = &[
    b"\x1b[1;38;5;202mhi\x1b[0m ",
    b"\x1b[7mrev\x1b[0m\r\n",
    b"\x1b[32mwide \xe6\x97\xa5\xe6\x9c\xac\x1b[0m\r\n",
    b"\x1b[4munder\x1b[0m line",
];

fn fixed_clock() -> ClockReading {
    ClockReading {
        monotonic: std::time::Instant::now(), // CLOCK-EXEMPT: captured once, reused so deltas are zero (determinism)
        wall_ms: Some(0),
    }
}

fn feed(term: &mut Terminal, clock: ClockReading) {
    for rec in SESSION {
        term.process_at(rec, clock);
    }
}

#[test]
fn replay_renders_pixel_identical_frames() {
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system monospace font found (headless without fonts)");
        return;
    };
    // Deterministic pixels: a lazy fallback parse landing between the live and
    // replay renders would make the two frames legitimately differ.
    r.debug_block_on_lazy_fallbacks();
    let clock = fixed_clock();

    // Live host and a fresh replay/viewer fed the same output log.
    let mut live = Terminal::new(ROWS as u16, COLS as u16);
    feed(&mut live, clock);
    let mut replay = Terminal::new(ROWS as u16, COLS as u16);
    feed(&mut replay, clock);

    let live_frame = r.render_input(&live.cell_frame(ROWS, COLS));
    let replay_frame = r.render_input(&replay.cell_frame(ROWS, COLS));

    assert_eq!(
        (live_frame.width, live_frame.height),
        (replay_frame.width, replay_frame.height),
        "replay frame dimensions must match the live host"
    );
    assert_eq!(
        live_frame.pixels, replay_frame.pixels,
        "replay must render a pixel-identical framebuffer to the live host"
    );
    assert_eq!(
        live_frame.to_png(),
        replay_frame.to_png(),
        "replay must encode a byte-identical PNG to the live host"
    );

    // Guard against a vacuous pass: the frame is a real, non-empty image.
    assert!(
        live_frame.width > 0 && live_frame.height > 0 && !live_frame.pixels.is_empty(),
        "rendered frame must be non-empty"
    );
}
