// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// THE TRAY MUST NOT COST THE SCISSOR — and must still look identical.
//
// A resident tray card (the modal settings panel, the update notice, the
// level-up burst, and — for the whole session, from one cosmetic Settings
// toggle — the static build BADGE) used to force `present_prev = None` on every
// present, turning every keystroke echo and every cursor blink into a full
// O(rows·cols) grid re-encode. The card now composites over the THROWAWAY
// `present_offscreen` copy (the route the comet halo already takes) instead of
// into the persistent offscreen, so the offscreen stays a clean scissor base.
//
// That is a PRESENT-PATH change, and the present path is exactly where the tray
// had no coverage: `tray_upload_skips_unchanged_card_bytes` and
// `fractional_scroll_keeps_tray_pixels_pinned` both drive `render_input` (the
// readback/full-repaint entry), and `present_real.rs` passes `tray: None`
// everywhere. So this suite drives the REAL seam — `present_virtual` runs the
// same `present_to_view` compose-and-blit body as the swapchain arm — and
// harvests the presented bytes through the VIDEO tap.
//
// Three properties, each of which the change could plausibly have broken:
//   1. a scissored present under a resident card is BYTE-IDENTICAL to the same
//      frame presented as a fresh full repaint, and the scissor actually fires
//      (the negative control: it did not before);
//   2. dropping the card leaves NO stale card pixels, even though the drop frame
//      repaints nothing;
//   3. the card still sits ABOVE the comet halo (the z-order the in-place bake
//      produced), with a negative control proving the halo reaches those pixels.
//
// Gated like the rest of the GPU suite: no GPU / no system font -> no-op.

use aterm_core::terminal::Terminal;
use aterm_gpu::video_tap::{CaptureOpts, DEFAULT_BUDGET, VideoTake};
use aterm_gpu::{GpuRenderer, TrayQuad, WindowGpu};
use aterm_render::{GlowQuad, RenderInput, Theme, premul_rgb};

fn gpu_or_skip() -> Option<GpuRenderer> {
    match GpuRenderer::new(18.0, Theme::default()) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            None
        }
    }
}

fn opts() -> CaptureOpts {
    CaptureOpts {
        half_res: false,
        budget_bytes: DEFAULT_BUDGET,
        fps_cap: None,
        requested_ms: 0,
    }
}

/// Present `seq` (input + this frame's tray) through the headless VIRTUAL target
/// with the tap armed, and harvest every presented frame's exact bytes. One fresh
/// `WindowGpu` per call, so each arm starts with no prior frame — the only state
/// that carries across arms is the renderer's (atlas, pipelines), which is what
/// makes an A/B of two arms meaningful.
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

    // THE HEADLINE: a resident card no longer costs the scissor. Before this
    // change both presents were Full (the card unconditionally cleared
    // `present_prev`); now the first is Full (no prior frame) and the second
    // scissors its one dirty row.
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
    // A live comet: additive glow quads in the middle of the grid. With the bloom
    // on (the default) this is the `fx_present` route — the one the card used to
    // share via `bake_in_place`, and the one whose ORDER this change moves.
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
        });
    }
    let (fw, fh) = gpu.frame_size(rows, cols);
    let (fw, fh) = (fw as u32, fh as u32);

    // NEGATIVE CONTROL FIRST: with no card, the glow must change the frame.
    // Without this the equality below could pass for the boring reason that the
    // halo never drew at all.
    let bare_glow = harvest(&mut gpu, fw, fh, &[(&glowing, None)]);
    let bare_plain = harvest(&mut gpu, fw, fh, &[(&plain, None)]);
    assert!(
        bare_glow.frames[0].rgba != bare_plain.frames[0].rgba,
        "negative control: the comet halo must reach the presented frame"
    );

    // A FULLY OPAQUE, FULL-FRAME card. Composited ABOVE the halo (the in-place
    // bake's z-order) it hides every pixel of it, so the glowing and plain frames
    // present identically. Composited BELOW it — the hazard of moving the card
    // onto the halo's own target — the additive light would show through and the
    // two would differ.
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
