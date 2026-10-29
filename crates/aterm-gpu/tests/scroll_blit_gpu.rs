// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::terminal::Terminal;
use aterm_gpu::{GpuRenderer, WindowGpu};
use aterm_render::{RenderInput, Theme};

use crate::common;

const ROWS: usize = 10;
const COLS: usize = 32;

/// A fresh GpuRenderer.
fn fresh_gpu() -> GpuRenderer {
    let mut g = common::gpu(18.0, Theme::default());
    g.debug_block_on_lazy_fallbacks();
    g.set_shimmer(false);
    g
}

/// The ground truth.
fn fresh_render(input: &RenderInput) -> Vec<u32> {
    let mut g = fresh_gpu();
    let mut win = WindowGpu::new();
    g.render_input(&mut win, input, None).pixels
}

/// Deep scrollback whose rows carry overshooting glyphs and wide cells.
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
    let input = term.cell_frame(ROWS, COLS);
    let warm = gpu.present_input_readback(&mut win, &input);
    assert_eq!(
        warm.pixels,
        fresh_render(&input),
        "warm frame != full repaint"
    );

    let mut rescued = 0u64;
    let mut checked = 0usize;
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
    assert_eq!(
        rescued, 20,
        "all 20 cursor-hidden scroll notches must be rescued ({rescued}/{checked})"
    );
    eprintln!("gpu scroll-blit: {rescued}/{checked} notches rescued");
}

#[test]
fn a_cursor_that_really_moves_still_refuses_the_scroll_rescue() {
    let mut gpu = fresh_gpu();
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
    let mut term = scrollback_term(false);
    let mut win = WindowGpu::new();
    let mut warm_input = term.cell_frame(ROWS, COLS);
    warm_input.cursor_visible = true;
    warm_input.cursor_row = 4;
    warm_input.cursor_col = 5;
    let warm = gpu.present_input_readback(&mut win, &warm_input);
    assert_eq!(
        warm.pixels,
        fresh_render(&warm_input),
        "warm frame != full repaint"
    );

    // Hold a visible stationary cursor across all 12 viewport notches so `cur_shown == true`
    // exercises `scroll_shift_plan`'s cursor + ghost-row dirtying on both +da and -da.
    let mut shown_rescued = 0u64;
    for (notches, step) in [(6, 3i32), (6, -2i32)] {
        for n in 0..notches {
            term.scroll_display(step);
            let mut input = term.cell_frame(ROWS, COLS);
            input.cursor_visible = true;
            input.cursor_row = 4;
            input.cursor_col = 5;
            let before = gpu.scroll_rescues();
            let got = gpu.present_input_readback(&mut win, &input);
            let delta = gpu.scroll_rescues() - before;
            assert_eq!(
                delta, 1,
                "stationary visible-cursor scroll notch (step {step}, notch {n}) must be rescued"
            );
            shown_rescued += delta;
            assert_eq!(
                got.pixels,
                fresh_render(&input),
                "visible-cursor scrolled frame (step {step}, notch {n}) != a full repaint"
            );
        }
    }
    assert_eq!(shown_rescued, 12);

    // Also pin `Terminal::cell_frame`'s natural `cursor_visible` transitions across `display_offset`:
    // leaving 0 (`true -> false`) and returning to 0 (`false -> true`) refuse rescue, while every
    // interior scroll (`false -> false`) rescues.
    term.scroll_to_bottom();
    let mut gpu = fresh_gpu();
    let mut win = WindowGpu::new();
    let live = term.cell_frame(ROWS, COLS);
    assert!(live.cursor_visible);
    let _ = gpu.present_input_readback(&mut win, &live);

    let mut toggle_rescued = 0u64;
    for (notches, step) in [(6, 3i32), (6, -2i32), (1, -6i32)] {
        for n in 0..notches {
            term.scroll_display(step);
            let input = term.cell_frame(ROWS, COLS);
            let before = gpu.scroll_rescues();
            let got = gpu.present_input_readback(&mut win, &input);
            let delta = gpu.scroll_rescues() - before;
            let expected = u64::from(!((step == 3 && n == 0) || step == -6));
            assert_eq!(
                delta, expected,
                "cursor-toggle notch (step {step}, notch {n}): expected rescue={expected}, got {delta}"
            );
            toggle_rescued += delta;
            assert_eq!(
                got.pixels,
                fresh_render(&input),
                "cursor-toggled scrolled frame (step {step}, notch {n}) != a full repaint"
            );
        }
    }
    assert_eq!(toggle_rescued, 11);
    eprintln!(
        "gpu scroll-blit (cursor shown): {shown_rescued}/12 stationary-cursor + {toggle_rescued}/13 toggle notches rescued"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_rescued_scroll_present_parks_on_no_command_buffer_of_its_own() {
    let mut gpu = fresh_gpu();
    let mut term = scrollback_term(true);
    let mut win = WindowGpu::new();
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
    assert_eq!(
        rescued, 12,
        "all 12 cursor-hidden scroll notches must be rescued ({rescued})"
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
