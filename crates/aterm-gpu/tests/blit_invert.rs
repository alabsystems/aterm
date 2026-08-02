// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// APPLICATION BLIT coverage — the present-path fullscreen-triangle blit
// (`vs_blit` + `fs_blit`) that copies the offscreen frame into the swapchain
// destination.
//
// The real `GpuRenderer::present_input` does NOT re-render into the window; it
// BLITS the single-source-of-truth offscreen `Rgba8Unorm` texture into the
// swapchain with a fullscreen triangle, sampled NEAREST. Two contracts:
//   * `BlitUniform.flag == 0` (the normal application blit): the destination
//     bytes are BYTE-IDENTICAL to the offscreen frame at the app-owned encoder
//     boundary.
//   * `BlitUniform.flag != 0` (the visual-bell flash): RGB is inverted
//     (`1.0 - rgb`), the GPU twin of the CPU softbuffer `px ^ 0x00ffffff`.
//
// The existing GPU tests only ever read the OFFSCREEN back (`render_input` /
// `present_input_readback`); NEITHER the blit's byte-exactness NOR the invert
// path had any coverage. This test closes that hole. The swapchain surface isn't
// readable headless, so it drives the EXACT same blit pipeline + `fs_blit` shader
// + `blit_sampler` (NEAREST) + `BlitUniform` against a readable `Rgba8Unorm`
// target via the test-only `blit_to_offscreen_for_test`, and reads that back.
// This stand-in test does not execute `present()`, WSI, compositor, or scanout.
//
// Gated: no GPU or no system font => the test no-ops (returns).

use std::sync::Arc;

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ};
use aterm_core::terminal::Terminal;
use aterm_gpu::GpuRenderer;
use aterm_render::{Frame, RenderInput, SceneAtlas, Theme};

mod common;
use common::{bb, gg, rr};

const ROWS: usize = 6;
const COLS: usize = 24;

fn fresh_gpu() -> Option<GpuRenderer> {
    match GpuRenderer::new(18.0, Theme::default()) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("SKIP: no GPU/font available: {e}");
            None
        }
    }
}

/// Render `input` to the renderer's offscreen, capture those pixels (the existing
/// readback — the SINGLE SOURCE OF TRUTH), then run the REAL blit (`invert`) into
/// a readable target and read it back. Returns `(offscreen_source, blit_output)`.
fn source_and_blit(
    gpu: &mut GpuRenderer,
    win: &mut aterm_gpu::WindowGpu,
    input: &RenderInput,
    invert: bool,
) -> (Frame, Frame) {
    // `render_input` does a FULL repaint into the resident offscreen and reads it
    // back: that returned Frame IS the offscreen the present path blits.
    let source = gpu.render_input(win, input, None);
    let blit = gpu.blit_to_offscreen_for_test(win, invert);
    assert_eq!(
        (source.width, source.height),
        (blit.width, blit.height),
        "blit target dims must equal the offscreen source dims"
    );
    (source, blit)
}

/// A representative changed frame: a prompt, coloured text (red/green/blue via
/// SGR), and a glyph, so the blit is exercised over real glyph + colour pixels.
fn representative_input() -> RenderInput {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    // Prompt + a glyph on row 0; saturated red/green/blue runs on rows below.
    term.process(b"$ blit check >_\r\n");
    term.process(b"\x1b[31mRED\x1b[0m \x1b[32mGREEN\x1b[0m \x1b[34mBLUE\x1b[0m\r\n");
    term.process(b"\x1b[1mbold\x1b[0m plain 0123456789");
    term.cell_frame(ROWS, COLS)
}

/// PASSTHROUGH (invert = false): the blit output must be BYTE-IDENTICAL to the
/// offscreen source for EVERY pixel. This is the hard "blit is byte-exact"
/// invariant — NEAREST sampling at 1:1, no interpolation smear, no colour-space
/// drift. A single mismatch is an application blit-path bug, not a tolerance miss.
#[test]
fn blit_passthrough_is_byte_identical() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();
    let input = representative_input();
    let (source, blit) = source_and_blit(&mut gpu, &mut win, &input, false);

    // Byte-exact whole-frame equality (the strongest possible assertion).
    if source.pixels != blit.pixels {
        // Locate the first divergence for a useful failure message.
        let mut first = None;
        for (i, (&s, &b)) in source.pixels.iter().zip(blit.pixels.iter()).enumerate() {
            if s != b {
                first = Some((i, s, b));
                break;
            }
        }
        if let Some((i, s, b)) = first {
            let (x, y) = (i % source.width, i / source.width);
            panic!(
                "BLIT PASSTHROUGH NOT BYTE-IDENTICAL (real present-path bug): \
                 first mismatch at pixel {i} (x={x}, y={y}): offscreen {s:#08x} \
                 != blit {b:#08x}"
            );
        }
        panic!("blit passthrough differs from offscreen (length mismatch)");
    }
    eprintln!(
        "blit passthrough: byte-identical over {} pixels ({}x{})",
        source.pixels.len(),
        source.width,
        source.height
    );
}

/// INVERT (invert = true): each output channel must equal `255 - source` and the
/// frame must be opaque (the readback stores `0x00RRGGBB`, so alpha is implicitly
/// equal between source and output). We assert the TIGHTEST bound that holds and
/// report it: either exactly `255 - x` (delta 0) or within <= 1 LSB if the shader
/// does a float round-trip (`round(255 * (1 - x/255))` vs the integer `255 - x`).
#[test]
fn blit_invert_is_one_minus_rgb() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();
    let input = representative_input();
    let (source, blit) = source_and_blit(&mut gpu, &mut win, &input, true);

    let mut max_delta = 0u32; // tightest bound that holds across the whole frame
    let mut worst: Option<(usize, i32, i32)> = None;
    for (i, (&s, &b)) in source.pixels.iter().zip(blit.pixels.iter()).enumerate() {
        for (sc, bc) in [(rr(s), rr(b)), (gg(s), gg(b)), (bb(s), bb(b))] {
            let expected = 255 - sc; // 8-bit `1.0 - rgb`
            let d = bc.abs_diff(expected);
            if d > max_delta {
                max_delta = d;
                worst = Some((i, expected, bc));
            }
        }
    }

    // The 8-bit invert MUST be within 1 LSB of `255 - x` everywhere; anything
    // larger is a broken invert (wrong channel, gamma shift, smear). This is the
    // correctness floor that must hold on ANY backend.
    assert!(
        max_delta <= 1,
        "blit invert diverges from (255 - x) by {max_delta} (> 1 LSB) — worst {worst:?}"
    );
    eprintln!(
        "blit invert: max |out - (255 - src)| = {max_delta} LSB over {} pixels",
        source.pixels.len()
    );

    // TIGHTEST BOUND THAT ACTUALLY HOLDS: on this backend (Metal) the invert is
    // EXACTLY `255 - x` (max_delta == 0) — no float round-trip drift. Assert that
    // exact equality so a regression that introduced even 1 LSB of drift (e.g. an
    // sRGB target, a colour-space shift, or a smearing sampler) would fail LOUDLY
    // rather than slip under the <= 1 floor above.
    assert_eq!(
        max_delta, 0,
        "blit invert was expected to be EXACTLY 255 - x (byte-exact) but drifted by \
         {max_delta} LSB — worst {worst:?}. If a backend genuinely round-trips through \
         float, relax this to the <= 1 floor and document WHY."
    );
    eprintln!("blit invert is EXACTLY 255 - x (byte-exact, no float round-trip drift)");
}

/// A SYNTHETIC UNIFORM frame: SGR 48;2 truecolor background painted over every
/// cell of the grid with spaces, cursor hidden. The grid interior is then a
/// constant `(r,g,b)`. The window PADDING around the grid keeps the renderer's
/// theme background — measured, not assumed: the caller asserts only that the
/// anchor colour is PRESENT in the source, never that the whole frame is it.
fn uniform_bg_input(r: u8, g: u8, b: u8) -> RenderInput {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[?25l"); // DECTCEM off: no cursor pixels in the mix
    term.process(format!("\x1b[48;2;{r};{g};{b}m").as_bytes());
    for row in 0..ROWS {
        term.process(format!("\x1b[{};1H", row + 1).as_bytes()); // CUP, 1-based
        term.process(" ".repeat(COLS).as_bytes());
    }
    term.cell_frame(ROWS, COLS)
}

/// Drive the invert across the FULL channel range with SYNTHETIC UNIFORM frames:
/// pure black, pure white, mid-grey, mid-grey's complement, and a saturated
/// colour. Each anchor is asserted EXACTLY — 0 → 255, 128 → 127, 255 → 0 — and
/// the complement PAIRS compose into the double-invert identity: the black
/// frame's 0 inverts to 255 and the white frame's 255 inverts to 0, so
/// invert(invert(0)) == 0; likewise 128 → 127 and 127 → 128.
///
/// WHAT THIS USED TO DO, and why it was rewritten: the doc already made all
/// three claims and the body made none of them. It rendered
/// `representative_input()` — the same ordinary terminal frame as the two tests
/// above — so no synthetic frame ever existed; the mid anchor was computed and
/// then discarded (`let _ = spanned_mid`); the endpoints were asserted only as
/// "<= 8" and ">= 247", never as 0 and 255; and the "double invert" leg
/// re-blitted the SAME offscreen source a second time, which measures
/// determinism, not involution.
///
/// MEASURED, not inferred, on this host (Metal, the repo's fixture font): a
/// `fs_blit` invert made wrong ONLY at channel 255 — an exact-255 source
/// inverting to 6 instead of 0 — passed ALL THREE tests in this file before
/// this rewrite, and fails the pure-white anchor after it. The old fixture
/// never reached 255; ">= 247" was as close as it got. A mid-tone-only
/// regression, by contrast, was already caught here, because that frame happens
/// to carry channel 127 in its glyph AA — incidentally, not by construction.
/// The synthetic frames make every anchor structural instead of font-dependent.
///
/// The complement pairs compose the round trip out of two measured legs; the
/// LITERAL involution — the blit's own output bytes fed back through the same
/// blit — is `blit_double_invert_round_trips_the_real_output` below.
#[test]
fn blit_invert_hits_range_anchors() {
    let Some(mut gpu) = fresh_gpu() else { return };
    let mut win = aterm_gpu::WindowGpu::new();

    let anchors: [([u8; 3], &str); 5] = [
        ([0, 0, 0], "pure black (0 -> 255)"),
        ([255, 255, 255], "pure white (255 -> 0)"),
        ([128, 128, 128], "mid-grey (128 -> 127)"),
        ([127, 127, 127], "mid-grey's complement (127 -> 128)"),
        ([255, 0, 128], "saturated (255/0/128 in one frame)"),
    ];
    for (rgb, name) in anchors {
        let input = uniform_bg_input(rgb[0], rgb[1], rgb[2]);
        let (source, inv) = source_and_blit(&mut gpu, &mut win, &input, true);

        // The relation holds EXACTLY for every pixel of the frame — grid
        // interior AND the theme-bg padding.
        for (&s, &o) in source.pixels.iter().zip(inv.pixels.iter()) {
            for (sc, oc) in [(rr(s), rr(o)), (gg(s), gg(o)), (bb(s), bb(o))] {
                assert_eq!(
                    oc,
                    255 - sc,
                    "{name}: invert must be EXACTLY 255 - x (src {sc} -> out {oc})"
                );
            }
        }

        // NON-VACUITY: the anchor colour really is in the source. Without this
        // the loop above is satisfied by a frame that never reaches the anchor
        // at all — which is exactly how the mid-grey claim went unmet before.
        let want_src = u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]]);
        let want_out = u32::from_be_bytes([0, 255 - rgb[0], 255 - rgb[1], 255 - rgb[2]]);
        let hit = source
            .pixels
            .iter()
            .zip(inv.pixels.iter())
            .find(|&(&s, _)| s & 0x00ff_ffff == want_src);
        let Some((_, &out)) = hit else {
            panic!(
                "{name}: the synthetic frame never contained the anchor colour \
                 {want_src:#08x} — the SGR truecolor bg did not paint the grid"
            );
        };
        assert_eq!(
            out & 0x00ff_ffff,
            want_out,
            "{name}: anchor {want_src:#08x} must blit-invert to {want_out:#08x}"
        );
        eprintln!("blit invert anchor {name}: {want_src:#08x} -> {want_out:#08x} exact");
    }

    // Separately (and it is a DIFFERENT property from the identity above): the
    // blit is deterministic — the same offscreen source blitted twice gives the
    // same bytes. Named for what it measures.
    let input = representative_input();
    let (_, inv_a) = source_and_blit(&mut gpu, &mut win, &input, true);
    let (_, inv_b) = source_and_blit(&mut gpu, &mut win, &input, true);
    assert_eq!(
        inv_a.pixels, inv_b.pixels,
        "invert must be deterministic across runs"
    );
}

/// A pixel ramp for `w * h` pixels that visits EVERY 8-bit value in EVERY
/// channel: `i` for red, and odd strides (coprime with 256) for green and blue,
/// so each channel's residues cycle through all 256 values. Whole-domain
/// coverage is not assumed — the caller MEASURES it on the readback.
fn full_domain_ramp(w: usize, h: usize) -> Vec<u32> {
    (0..w * h)
        .map(|i| {
            let r = (i % 256) as u32;
            let g = ((i * 5) % 256) as u32; // gcd(5, 256) == 1
            let b = ((i * 3 + 128) % 256) as u32; // gcd(3, 256) == 1
            (r << 16) | (g << 8) | b
        })
        .collect()
}

/// Put EXACT `0x00RRGGBB` bytes into the renderer's resident offscreen and
/// return the readback, having asserted the injection was byte-exact.
///
/// The seam is the FREE-SPRITE layer: one opaque (`alpha == 255`, untinted)
/// NEAREST 1:1 rect covering the whole frame, drawn `OverText` over a blank
/// cursor-hidden grid. That is a plain replacement of every frame pixel, so the
/// offscreen ends up carrying `pixels` verbatim — MEASURED here, per call, not
/// assumed: if the sprite path ever stopped being a byte-exact 1:1 copy this
/// panics on the spot instead of silently weakening the caller.
///
/// Bloom and shimmer are the caller's responsibility to disable; they add light
/// to the offscreen and would break the injection.
fn inject_offscreen(
    gpu: &mut GpuRenderer,
    win: &mut aterm_gpu::WindowGpu,
    pixels: &[u32],
    w: usize,
    h: usize,
) -> Frame {
    let mut rgba = Vec::with_capacity(w * h * 4);
    for &p in pixels {
        rgba.extend_from_slice(&[rr(p) as u8, gg(p) as u8, bb(p) as u8, 255]);
    }
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(b"\x1b[?25l"); // no cursor pixels over the injected art
    let mut input = term.cell_frame(ROWS, COLS);
    input.free_atlas = Some(Arc::new(SceneAtlas {
        width: w as u32,
        height: h as u32,
        rgba,
        version: 1,
    }));
    input.free_sprites.push(FreeSprite {
        x: 0,
        y: 0,
        w: w as u16,
        h: h as u16,
        ax: 0,
        ay: 0,
        aw: w as u16, // bake == dest: the NEAREST 1:1 contract
        ah: h as u16,
        tint: 0x00FF_FFFF, // no tint
        alpha: 255,        // fully opaque: src-over is a replacement
        flip_x: false,
        z: FreeZ::OverText,
        sampler: FreeSampler::Nearest,
    });
    let got = gpu.render_input(win, &input, None);
    assert_eq!(
        (got.width, got.height),
        (w, h),
        "injection frame size changed under us"
    );
    for (i, (&want, &have)) in pixels.iter().zip(got.pixels.iter()).enumerate() {
        assert_eq!(
            have & 0x00ff_ffff,
            want,
            "PREMISE: the 1:1 opaque free sprite must inject bytes verbatim — \
             pixel {i} wanted {want:#08x}, offscreen has {have:#08x}"
        );
    }
    got
}

/// The LITERAL double invert, over the WHOLE 8-BIT DOMAIN.
///
/// The two legs are real inverts of the REAL blit, and the second leg's INPUT
/// IS THE FIRST LEG'S OUTPUT BYTES — `invert(invert(x)) == x` is performed, not
/// composed from two separately-measured frames. What makes it expressible is
/// that the offscreen is writable from a `RenderInput` after all: a single
/// opaque NEAREST 1:1 `FreeSprite` covering the frame replaces every pixel with
/// atlas bytes (`inject_offscreen`, which asserts that exactness on every call).
/// The earlier note in `blit_invert_hits_range_anchors` — that no such feedback
/// path existed through this seam — was wrong, and is corrected there.
///
/// Two properties, both stronger than what the frames above could reach:
///   * DOMAIN-COMPLETE `255 - x`: the injected ramp carries all 256 values in
///     all three channels (MEASURED on the readback, not assumed), so the
///     invert is checked at every input value rather than at the handful the
///     font, theme and SGR anchors happen to produce.
///   * INVOLUTION: re-injecting the inverted frame and inverting again returns
///     the original frame byte-for-byte.
#[test]
fn blit_double_invert_round_trips_the_real_output() {
    let Some(mut gpu) = fresh_gpu() else { return };
    // The injected bytes must survive to the blit untouched: bloom and shimmer
    // both write extra light into the offscreen.
    gpu.set_bloom(false);
    gpu.set_shimmer(false);
    let mut win = aterm_gpu::WindowGpu::new();

    // Size the ramp to the renderer's own frame (one throwaway render).
    let probe = gpu.render_input(&mut win, &representative_input(), None);
    let (w, h) = (probe.width, probe.height);

    // LEG 1: ramp -> offscreen, blit invert.
    let ramp = full_domain_ramp(w, h);
    let src = inject_offscreen(&mut gpu, &mut win, &ramp, w, h);
    let inv1 = gpu.blit_to_offscreen_for_test(&mut win, true);

    // NON-VACUITY, measured on the SOURCE READBACK: every 8-bit value really is
    // present in every channel, so the per-pixel check below is domain-complete.
    let mut seen = [[false; 256]; 3];
    for &p in &src.pixels {
        seen[0][rr(p) as usize] = true;
        seen[1][gg(p) as usize] = true;
        seen[2][bb(p) as usize] = true;
    }
    for (c, name) in ["red", "green", "blue"].iter().enumerate() {
        let missing = (0..256).filter(|&v| !seen[c][v]).count();
        assert_eq!(
            missing, 0,
            "the injected ramp must cover all 256 {name} values ({missing} missing)"
        );
    }

    for (i, (&s, &o)) in src.pixels.iter().zip(inv1.pixels.iter()).enumerate() {
        for (sc, oc) in [(rr(s), rr(o)), (gg(s), gg(o)), (bb(s), bb(o))] {
            assert_eq!(
                oc,
                255 - sc,
                "domain-complete invert: pixel {i} channel {sc} must invert to {} (got {oc})",
                255 - sc
            );
        }
    }

    // LEG 2: the FIRST BLIT'S OUTPUT is what gets blitted this time.
    let inv1_px: Vec<u32> = inv1.pixels.iter().map(|p| p & 0x00ff_ffff).collect();
    let _ = inject_offscreen(&mut gpu, &mut win, &inv1_px, w, h);
    let inv2 = gpu.blit_to_offscreen_for_test(&mut win, true);

    // Compact failure: name the first pixel that failed to come home rather
    // than dumping two 33k-element frames into the log.
    if let Some((i, (&want, &have))) = src
        .pixels
        .iter()
        .zip(inv2.pixels.iter())
        .enumerate()
        .find(|&(_, (&a, &b))| a != b)
    {
        let (x, y) = (i % w, i / w);
        panic!(
            "LITERAL DOUBLE INVERT BROKEN: blitting the invert's own output with \
             invert on must return the original frame byte-for-byte — first \
             mismatch at pixel {i} (x={x}, y={y}): wanted {want:#08x}, got {have:#08x}"
        );
    }
    // And the intermediate really was a different image — otherwise the
    // identity above would hold for a blit that did nothing at all.
    assert_ne!(
        inv1.pixels, src.pixels,
        "NON-VACUITY: the first invert must actually change the frame"
    );
    eprintln!(
        "literal double invert: {} pixels round-tripped byte-for-byte over the full 8-bit domain",
        src.pixels.len()
    );
}
