// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Whole-row scroll blit on the GPU: byte-identity gate. `compute_dirty_rows`
// returns `FullRepaint` for scrollback navigation even though the grid slid by a
// known row delta; like the CPU, the GPU consults `scroll_blit_plan`, shifts the
// offscreen's grid band with a staged texture-to-texture copy and re-rasters
// only the exposed strip. A rescued frame must equal a fresh full repaint. The
// fixture targets upward glyph overshoot at the exposed/retained boundary
// (`OVERSHOOT_APRON_ROWS`; hence parentheses, pipes, underscores, descenders)
// and wide cells riding the shift, in both directions.
//
// No GPU or no font: COULD NOT RUN.

use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuRenderer, WindowGpu};
use aterm_render::{RenderInput, Theme};

use crate::common;

const ROWS: usize = 10;
const COLS: usize = 32;

/// A fresh GpuRenderer (no GPU: COULD NOT RUN), with lazy fallback parses settled so
/// every renderer rasterizes CJK identically, and the wall-clock heat shimmer
/// off so two renders of one input can agree.
fn fresh_gpu() -> GpuRenderer {
    let mut g = common::gpu(18.0, Theme::default());
    g.debug_block_on_lazy_fallbacks();
    g.set_shimmer(false);
    g
}

/// The ground truth: a brand-new GpuRenderer renders `input` with a full repaint
/// (Clear + every row) and reads it back.
fn fresh_render(input: &RenderInput) -> Vec<u32> {
    let mut g = fresh_gpu();
    let mut win = WindowGpu::new();
    g.render_input(&mut win, input, None).pixels
}

/// Deep scrollback whose rows carry overshooting glyphs and wide cells.
/// `hide_cursor` does not decide rescuability (`RenderInput::cursor_row` does not
/// change with `display_offset`); hiding it only drops the cursor's re-stamp and
/// ghost rows from the dirty set.
fn scrollback_term(hide_cursor: bool) -> Terminal {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    if hide_cursor {
        term.process(b"\x1b[?25l");
    }
    for i in 0..140 {
        term.process(format!("(|_gy) line {i} 日本語 tail\r\n").as_bytes());
    }
    term
}

#[test]
fn history_scroll_is_rescued_and_stays_byte_identical_to_a_full_repaint() {
    let mut gpu = fresh_gpu();
    let mut term = scrollback_term(true);
    let mut win = WindowGpu::new();
    // Warm the window at the bottom: no prior frame ⇒ a full repaint.
    let input = term.cell_frame(ROWS, COLS);
    let warm = gpu.present_input_readback(&mut win, &input);
    assert_eq!(
        warm.pixels,
        fresh_render(&input),
        "warm frame != full repaint"
    );

    let mut rescued = 0u64;
    let mut checked = 0usize;
    // Into history (delta < 0, top strip exposed), then back (delta > 0,
    // bottom strip exposed, the apron direction). Different magnitudes sweep
    // odd and even pixel shifts (the shade-phase parity clause).
    for (notches, step) in [(10, 3i32), (10, -2i32)] {
        for n in 0..notches {
            term.scroll_display(step);
            let input = term.cell_frame(ROWS, COLS);
            let before = gpu.scroll_rescues();
            let got = gpu.present_input_readback(&mut win, &input);
            rescued += gpu.scroll_rescues() - before;
            checked += 1;
            assert_eq!(
                got.pixels,
                fresh_render(&input),
                "scrolled frame (step {step}, notch {n}) != a full repaint"
            );
        }
    }
    // Reach: the rescue must carry the sweep. Some notches legitimately refuse
    // (the first after a direction change, or identical content hitting the
    // gate), so the floor is generous but non-trivial.
    assert!(
        rescued >= 14,
        "the scroll-blit rescue barely fired ({rescued}/{checked} notches) — the \
         byte-identity gate above is not testing it"
    );
    eprintln!("gpu scroll-blit: {rescued}/{checked} notches rescued");
}

#[test]
fn a_cursor_that_really_moves_still_refuses_the_scroll_rescue() {
    let mut gpu = fresh_gpu();
    // Move the cursor for real (a CUP in the same frame that scrolls, as when
    // a scroll lands on the same tick as shell output). The frame must fall
    // back to a full repaint and still match one.
    let mut term = scrollback_term(false);
    let mut win = WindowGpu::new();
    let input = term.cell_frame(ROWS, COLS);
    let _ = gpu.present_input_readback(&mut win, &input);

    term.scroll_display(1);
    term.process(b"\x1b[3;7H");
    let input = term.cell_frame(ROWS, COLS);
    let before = gpu.scroll_rescues();
    let got = gpu.present_input_readback(&mut win, &input);
    assert_eq!(
        gpu.scroll_rescues(),
        before,
        "a scroll frame that also MOVES the cursor must not be rescued"
    );
    assert_eq!(
        got.pixels,
        fresh_render(&input),
        "the refused frame must still match a full repaint"
    );
}

#[test]
fn a_visible_cursor_rides_the_rescue_and_stays_byte_identical() {
    let mut gpu = fresh_gpu();
    // With the cursor shown, every rescued frame must erase the blitted ghost
    // at `cursor_row - da` and re-stamp the cursor at `cursor_row`.
    let mut term = scrollback_term(false);
    let mut win = WindowGpu::new();
    let input = term.cell_frame(ROWS, COLS);
    let warm = gpu.present_input_readback(&mut win, &input);
    assert_eq!(
        warm.pixels,
        fresh_render(&input),
        "warm frame != full repaint"
    );

    let mut rescued = 0u64;
    for (notches, step) in [(6, 3i32), (6, -2i32)] {
        for n in 0..notches {
            term.scroll_display(step);
            let input = term.cell_frame(ROWS, COLS);
            let before = gpu.scroll_rescues();
            let got = gpu.present_input_readback(&mut win, &input);
            rescued += gpu.scroll_rescues() - before;
            assert_eq!(
                got.pixels,
                fresh_render(&input),
                "cursor-visible scrolled frame (step {step}, notch {n}) != a full repaint"
            );
        }
    }
    assert!(
        rescued >= 8,
        "the rescue barely fired with a visible cursor ({rescued}) — the ghost-row \
         handling above is not being tested"
    );
    eprintln!("gpu scroll-blit (cursor shown): {rescued} notches rescued");
}

/// On the armed Metal arm the rescue's band copies ride the armed encode's own
/// command buffer ahead of the scissored pass (Submit A), so a rescued present
/// never waits on a command buffer of its own: the inline-wait count stays flat
/// and `metal_encode_band_shifts` grows by exactly the rescues taken. The
/// byte-identity oracle catches a copy that landed after the pass.
#[cfg(target_os = "macos")]
#[test]
fn a_rescued_scroll_present_parks_on_no_command_buffer_of_its_own() {
    let mut gpu = fresh_gpu();
    let mut term = scrollback_term(true);
    let mut win = WindowGpu::new();
    // Warm at the bottom (a full repaint), then take the baselines after it:
    // the warm frame is not a rescue and must not be charged to the sweep.
    let input = term.cell_frame(ROWS, COLS);
    let warm = gpu.present_input_readback(&mut win, &input);
    assert_eq!(
        warm.pixels,
        fresh_render(&input),
        "warm frame != full repaint"
    );
    let waits_before = gpu.metal_encode_inline_waits();
    let shifts_before = gpu.metal_encode_band_shifts();
    let rescues_before = gpu.scroll_rescues();

    for (notches, step) in [(6, 3i32), (6, -2i32)] {
        for n in 0..notches {
            term.scroll_display(step);
            let input = term.cell_frame(ROWS, COLS);
            let got = gpu.present_input_readback(&mut win, &input);
            assert_eq!(
                got.pixels,
                fresh_render(&input),
                "scrolled frame (step {step}, notch {n}) != a full repaint"
            );
        }
    }

    let rescued = gpu.scroll_rescues() - rescues_before;
    assert!(
        rescued >= 8,
        "the rescue barely fired ({rescued} notches) — the no-wait contract \
         below is not being tested"
    );
    assert_eq!(
        gpu.metal_encode_band_shifts() - shifts_before,
        rescued,
        "every rescued present must record its band copies INTO Submit A"
    );
    assert_eq!(
        gpu.metal_encode_inline_waits(),
        waits_before,
        "a rescued scroll present parked the UI thread on a command buffer of \
         its own ({} inline waits across {rescued} rescues)",
        gpu.metal_encode_inline_waits() - waits_before
    );
}

/// A CARD RIDES THE GPU'S RESCUE (design §4.4 as built, B3): over scrollback
/// with an inline card on screen (a Window Under layer, a Grid Over layer,
/// its line's animated ink and its in-band star), each wheel notch that moves
/// the card with its content is rescued, the planner leaving the card's rows
/// to the band copy instead of re-encoding them, and equals a fresh full
/// repaint: the offscreen's band copy carries the layers' light and the ink
/// exactly as the encode draws them. A card whose layers stay put while the
/// text scrolls is still rescued, its rows re-encoded; one whose ink stays put
/// refuses the rescue (a retained row changed). Every frame equals a full
/// repaint.
///
/// The negative controls are mutations of the shared planner (run by hand,
/// recorded in the lane report): dropping the offset clause of
/// `fx_damage::rides_the_blit` fails "layers held", and dropping
/// `row_overlays_differ_shifted` from its `Rigid` check fails "ink held", each
/// on the light or ink the band copy slid.
#[test]
fn a_card_rides_the_rescue_byte_identical() {
    use std::sync::Arc;

    use aterm_core::render::{
        DecoBlend, DecoGlyph, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRect, FxSlot, FxSpace,
        InkCell, WordDecoration,
    };

    let mut gpu = fresh_gpu();
    let (cw, ch) = gpu.cell_size();
    let (top, chi) = (
        i32::try_from(gpu.pad()).expect("small"),
        i32::try_from(ch).expect("small"),
    );
    let rect = |x: i32, y: i32, w: usize, h: usize| {
        FxItem::Rect(FxRect {
            x,
            y,
            w: u16::try_from(w).expect("small"),
            h: u16::try_from(h).expect("small"),
            ink: 0,
        })
    };
    // The card's line is row 4 as the scroll starts.
    let card = [
        FxLayer::new(
            1,
            Arc::new(FxContent::new(
                vec![rect(cw as i32, top + 3 * chi + 3, 12 * cw, 2 * ch - 6)],
                vec![FxInk::add(0x0030_2010), FxInk::add(0x0010_2030)],
            )),
            FxSlot::Under,
            FxSpace::Window,
            FxBlend::Exact,
        ),
        FxLayer::new(
            2,
            Arc::new(FxContent::new(
                vec![rect(3 * cw as i32, 4 * chi + 2, 4 * cw, ch - 4)],
                vec![FxInk::over(0x0040_2030, 0x90); 2],
            )),
            FxSlot::Over,
            FxSpace::Grid,
            FxBlend::Exact,
        ),
    ];
    // The card's layers `layers` rows and its line's overlays `line` rows
    // below where they start.
    let frame = |term: &mut Terminal, layers: i32, line: i32| {
        let mut input = term.cell_frame(ROWS, COLS);
        input.fx_layers = card
            .iter()
            .map(|l| {
                let mut l = l.clone();
                l.offset.1 += layers * chi;
                l
            })
            .collect();
        let row = u16::try_from(4 + line).expect("on the grid");
        input.ink = (0..6)
            .map(|i| InkCell {
                row,
                col: 2 + i,
                color: [0x60, 0x40 + 0x20 * i as u8, 0xe0],
            })
            .collect();
        input.word_decorations = vec![WordDecoration {
            row,
            col: 0,
            dx: 0,
            dy: 0,
            glyph: DecoGlyph::Star4,
            blend: DecoBlend::Add,
            color: 0x0060_5030,
            alpha: 220,
        }];
        input
    };
    let mut term = scrollback_term(true);
    let mut win = WindowGpu::new();
    let _ = gpu.present_input_readback(&mut win, &frame(&mut term, 0, 0));
    let mut shown = 0i32;
    // (name, notch, the card's layers ride, its ink rides, rescued)
    // "ink held" comes last, into history after a notch the whole card rode,
    // so no other mark (the exposed strip, the bottom row, a layer that did not
    // ride) rebuilds the rows its ink left and reached.
    let notches = [
        ("rides", 1, true, true, true),
        ("layers held", 1, false, true, true),
        ("layers held", 1, false, true, true),
        ("layers catch up", -1, true, true, true),
        ("rides", 1, true, true, true),
        ("ink held", 1, true, false, false),
    ];
    let (mut layers_at, mut ink_at) = (0i32, 0i32);
    for (n, (name, step, layers_ride, ink_rides, rescues)) in notches.into_iter().enumerate() {
        term.scroll_display(step);
        shown += step;
        if layers_ride {
            layers_at = shown;
        }
        if ink_rides {
            ink_at = shown;
        }
        let input = frame(&mut term, layers_at, ink_at);
        let before = gpu.scroll_rescues();
        let got = gpu.present_input_readback(&mut win, &input);
        // The pixels first, so a planner that slid what it should have
        // rebuilt shows as what it left behind.
        assert_eq!(
            got.pixels,
            fresh_render(&input),
            "notch {n} ({name}): the frame equals a full repaint"
        );
        assert_eq!(
            gpu.scroll_rescues() - before,
            u64::from(rescues),
            "notch {n} ({name}): rescued"
        );
    }
}
