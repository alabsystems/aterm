// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// A resident tray card (settings panel, transient card, build badge) must not
// disable the scissored present, and must still look identical.
//
// The card composites over the throwaway `present_offscreen` copy (as the
// comet halo does), so the persistent offscreen stays a clean scissor base.
// This drives the real present seam: `present_virtual` runs the same
// `present_to_view` body as the swapchain arm, and the video tap harvests the
// presented bytes.
//
// Properties:
//   1. a scissored present under a resident card equals the same frame as a
//      fresh full repaint, and the scissor actually fires;
//   2. dropping the card leaves no stale card pixels, though the drop frame
//      repaints nothing;
//   3. the card sits above the comet halo, with a negative control proving
//      the halo reaches those pixels.
//
// No GPU / no system font: no-op.

use aterm_core::terminal::Terminal;
use aterm_gpu::video_tap::{CaptureOpts, DEFAULT_BUDGET, VideoTake};
use aterm_gpu::{GpuRenderer, TrayQuad, WindowGpu};
use aterm_render::{GlowQuad, RenderInput, Theme, premul_rgb};

use crate::common;

fn gpu_or_skip() -> Option<GpuRenderer> {
    common::gpu_or_skip(18.0, Theme::default())
}

fn opts() -> CaptureOpts {
    CaptureOpts {
        half_res: false,
        budget_bytes: DEFAULT_BUDGET,
        fps_cap: None,
        requested_ms: 0,
    }
}

/// Present `seq` (input + tray per frame) through the virtual target with the
/// tap armed and harvest every frame's bytes. A fresh `WindowGpu` per call, so
/// each arm starts without a prior frame; only renderer state (atlas,
/// pipelines) carries across arms.
fn harvest(
    gpu: &mut GpuRenderer,
    w: u32,
    h: u32,
    seq: &[(&RenderInput, Option<TrayQuad<'_>>)],
) -> VideoTake {
    let mut win = WindowGpu::new();
    gpu.virtual_begin(&mut win, w, h, opts())
        .expect("virtual tap");
    gpu.reset_glow_ease_for_test(&mut win);
    for (i, (input, tray)) in seq.iter().enumerate() {
        assert!(
            gpu.present_virtual(&mut win, input, false, None, *tray),
            "the virtual present cannot drop"
        );
        gpu.video_after_present(&mut win, i as u64 + 1);
    }
    let take = gpu.video_finish(&mut win).expect("virtual take");
    assert_eq!(
        take.frames.len(),
        seq.len(),
        "every present must harvest exactly one frame"
    );
    assert_eq!(take.dropped, 0, "the tap lost frames");
    take
}

/// A small grid with the cursor hidden (so no blink phase can perturb an A/B),
/// plus a second frame differing from it in ONE row.
fn two_frames(rows: usize, cols: usize) -> (RenderInput, RenderInput) {
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\x1b[?25l");
    term.process(b"alpha\r\nbravo\r\ncharlie\r\ndelta");
    let a = term.cell_frame(rows, cols);
    // One row changes; everything else is byte-identical, so the second present
    // of `a` then `b` has a small dirty set and MUST take the scissored path.
    term.process(b"\x1b[3;1Hbravissimo");
    let b = term.cell_frame(rows, cols);
    (a, b)
}

/// A `pw`×`ph` card of one straight-alpha colour.
fn card(pw: u32, ph: u32, rgba: [u8; 4]) -> Vec<u8> {
    rgba.repeat((pw * ph) as usize)
}

#[test]
fn tray_present_scissors_and_stays_byte_identical_to_a_full_repaint() {
    let Some(mut gpu) = gpu_or_skip() else { return };
    let (rows, cols) = (6usize, 20usize);
    let (a, b) = two_frames(rows, cols);
    let (cw, ch) = gpu.cell_size();
    let (pw, ph) = ((cw * 3) as u32, ch as u32);
    let pixels = card(pw, ph, [30, 30, 34, 220]);
    let quad = TrayQuad {
        rgba: &pixels,
        pw,
        ph,
        dx: (cw * 2) as u32,
        dy: (ch * 2) as u32,
    };
    let (fw, fh) = gpu.frame_size(rows, cols);
    let (fw, fh) = (fw as u32, fh as u32);

    // ARM A: two presents on ONE window with the card resident throughout. The
    // second one has a prior frame to diff against, so it is the frame under test.
    let scissors_before = gpu.scissor_taken();
    let fulls_before = gpu.full_repaints();
    let seq = [(&a, Some(quad)), (&b, Some(quad))];
    let incremental = harvest(&mut gpu, fw, fh, &seq);
    let scissored = gpu.scissor_taken() - scissors_before;
    let fulls = gpu.full_repaints() - fulls_before;

    // A resident card does not cost the scissor: the first present is Full (no
    // prior frame) and the second scissors its one dirty row.
    assert_eq!(
        (scissored, fulls),
        (1, 1),
        "a resident tray card must not disable the scissored repaint"
    );

    // ARM B: the SAME frame presented into a fresh window — a full repaint by
    // construction (no prior frame). The scissored frame must be byte-identical.
    let full = harvest(&mut gpu, fw, fh, &[(&b, Some(quad))]);
    assert!(
        incremental.frames[1].rgba == full.frames[0].rgba,
        "the scissored tray present must be byte-identical to the full repaint \
         (first diff at byte {:?})",
        incremental.frames[1]
            .rgba
            .iter()
            .zip(full.frames[0].rgba.iter())
            .position(|(x, y)| x != y)
    );
}

#[test]
fn dropping_the_tray_leaves_no_stale_card_pixels() {
    let Some(mut gpu) = gpu_or_skip() else { return };
    let (rows, cols) = (6usize, 20usize);
    let (a, _b) = two_frames(rows, cols);
    let (cw, ch) = gpu.cell_size();
    let (pw, ph) = ((cw * 4) as u32, (ch * 2) as u32);
    let pixels = card(pw, ph, [220, 30, 180, 255]);
    let quad = TrayQuad {
        rgba: &pixels,
        pw,
        ph,
        dx: cw as u32,
        dy: ch as u32,
    };
    let (fw, fh) = gpu.frame_size(rows, cols);
    let (fw, fh) = (fw as u32, fh as u32);

    // Card up, then card gone — with the SAME input, so any difference between
    // the drop frame and a never-carded present is a stranded card pixel.
    let dropped = harvest(&mut gpu, fw, fh, &[(&a, Some(quad)), (&a, None)]);
    let never = harvest(&mut gpu, fw, fh, &[(&a, None)]);
    assert!(
        dropped.frames[0].rgba != never.frames[0].rgba,
        "negative control: the card must actually reach the presented frame"
    );
    assert!(
        dropped.frames[1].rgba == never.frames[0].rgba,
        "the present that drops the card must leave no stale card pixels \
         (first diff at byte {:?})",
        dropped.frames[1]
            .rgba
            .iter()
            .zip(never.frames[0].rgba.iter())
            .position(|(x, y)| x != y)
    );
}

#[test]
fn an_opaque_tray_still_covers_the_comet_halo() {
    let Some(mut gpu) = gpu_or_skip() else { return };
    let (rows, cols) = (6usize, 20usize);
    let (plain, _) = two_frames(rows, cols);
    let (cw, ch) = gpu.cell_size();
    // A live comet: additive glow quads mid-grid. With bloom on (the default)
    // this takes the `fx_present` route the card also composites on.
    let mut glowing = plain.clone();
    for i in 0..3u16 {
        glowing.cursor_glow_add.push(GlowQuad {
            row: 1,
            x: (cw as u16) * (i + 1),
            y: ch as u16,
            w: cw as u16,
            h: ch as u16,
            color: premul_rgb(0x00FF_6A00, 220),
            // ADDITIVE light (see `GlowQuad::alpha`).
            alpha: 0,
            color2: premul_rgb(0x00FF_6A00, 220),
            alpha2: 0,
        });
    }
    let (fw, fh) = gpu.frame_size(rows, cols);
    let (fw, fh) = (fw as u32, fh as u32);

    // Negative control first: with no card, the glow must change the frame,
    // or the equality below could pass because the halo never drew.
    let bare_glow = harvest(&mut gpu, fw, fh, &[(&glowing, None)]);
    let bare_plain = harvest(&mut gpu, fw, fh, &[(&plain, None)]);
    assert!(
        bare_glow.frames[0].rgba != bare_plain.frames[0].rgba,
        "negative control: the comet halo must reach the presented frame"
    );

    // A fully opaque full-frame card. Above the halo it hides it entirely, so
    // glowing and plain frames present identically; below it, the additive
    // light would show through.
    let pixels = card(fw, fh, [18, 20, 24, 255]);
    let quad = TrayQuad {
        rgba: &pixels,
        pw: fw,
        ph: fh,
        dx: 0,
        dy: 0,
    };
    let covered_glow = harvest(&mut gpu, fw, fh, &[(&glowing, Some(quad))]);
    let covered_plain = harvest(&mut gpu, fw, fh, &[(&plain, Some(quad))]);
    assert!(
        covered_glow.frames[0].rgba == covered_plain.frames[0].rgba,
        "an opaque tray must composite ABOVE the comet halo (first diff at byte \
         {:?})",
        covered_glow.frames[0]
            .rgba
            .iter()
            .zip(covered_plain.frames[0].rgba.iter())
            .position(|(x, y)| x != y)
    );
}
