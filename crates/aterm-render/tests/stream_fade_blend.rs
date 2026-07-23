// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! L0 proofs for the M2 "ink that dries" blend seam: the stream fade tints a
//! drying cell's foreground with `blend_text(bg, fg, bg, alpha, false)` — the
//! EXACT linear-light alpha-over — at an ease-out coverage that is monotone in
//! age (proven in aterm-gui's `stream_fade` module). These tests discharge the
//! blend half of the M2 PROVE bullets on the shipping blend itself:
//!
//! * **CONVERGENCE-TO-EXACT** — coverage 255 returns the fg bytes EXACTLY and
//!   coverage 0 the destination bytes EXACTLY, over the ENTIRE per-channel
//!   byte domain (256×256 pairs, every channel position). With the envelope's
//!   `age >= fade_ms ⇒ alpha == 255` theorem this closes "no permanent tint":
//!   the steady frame is byte-identical to the no-feature frame.
//! * **MONOTONICITY** — the blended channel is monotone in coverage
//!   (nondecreasing toward the fg when `fg >= bg`, nonincreasing when
//!   `fg <= bg`) across ALL 256×256 (bg, fg) channel pairs × all 256 coverage
//!   steps — the full 16.7M-point input space of one channel, which the other
//!   two channels share by construction (`blend_channel_linear` is applied
//!   per-channel with identical code; a cross-channel lattice pins that too).
//!   So a drying glyph only ever approaches its final ink.
//!
//! The uncorrected (`corrected = false`) mode is deliberate: the tint picks an
//! intermediate COLOUR in linear light (fringe-free on any fg/bg pair); the
//! perceptual coverage remap stays where it lives — the glyph raster seam.

use aterm_render::blend_text;

/// CONVERGENCE endpoints, full byte domain: `t == 255` is exactly the fg and
/// `t == 0` exactly the destination, for every (bg, fg) byte pair in every
/// channel position. (Both are structural early returns in `blend_text`; this
/// pins them against regressions that would route endpoints through the LUTs.)
#[test]
fn endpoints_exact_over_full_byte_domain() {
    for shift in [16u32, 8, 0] {
        for b in 0u32..=255 {
            for f in 0u32..=255 {
                let bg = b << shift;
                let fg = f << shift;
                assert_eq!(
                    blend_text(bg, fg, bg, 255, false),
                    fg,
                    "t=255 must be the exact fg (shift {shift}, b={b}, f={f})"
                );
                assert_eq!(
                    blend_text(bg, fg, bg, 0, false),
                    bg,
                    "t=0 must be the exact bg (shift {shift}, b={b}, f={f})"
                );
            }
        }
    }
    // Full-pixel spot anchors (all three channels at once, distinct values).
    assert_eq!(
        blend_text(0x0010_2030, 0x00AA_BBCC, 0x0010_2030, 255, false),
        0x00AA_BBCC
    );
    assert_eq!(
        blend_text(0x0010_2030, 0x00AA_BBCC, 0x0010_2030, 0, false),
        0x0010_2030
    );
}

/// MONOTONICITY, full per-channel domain: for EVERY (bg, fg) byte pair the
/// blended red channel is monotone in coverage over all 256 steps — the
/// complete 16.7M-point proof for the channel function all three positions
/// share. Includes the non-vacuity control: an interior coverage genuinely
/// lands strictly between bg and fg for a contrasting pair.
#[test]
fn blend_monotone_in_coverage_over_full_byte_domain() {
    for b in 0u32..=255 {
        for f in 0u32..=255 {
            let bg = b << 16;
            let fg = f << 16;
            let mut prev = (blend_text(bg, fg, bg, 0, false) >> 16) & 0xff;
            for t in 1u32..=255 {
                let cur = (blend_text(bg, fg, bg, t as u8, false) >> 16) & 0xff;
                if f >= b {
                    assert!(
                        cur >= prev,
                        "channel regressed toward fg: b={b} f={f} t={t}: {prev} -> {cur}"
                    );
                } else {
                    assert!(
                        cur <= prev,
                        "channel overshot past fg: b={b} f={f} t={t}: {prev} -> {cur}"
                    );
                }
                prev = cur;
            }
            // The sweep must END on the exact fg (convergence, again).
            assert_eq!(prev, f, "the t=255 sample must equal fg exactly");
        }
    }
    // Non-vacuity: linear-light midpoint of white over black is the physically
    // correct ~sRGB 188 (strictly interior — the guarded ramp is real).
    let mid = (blend_text(0, 0x00FF_0000, 0, 128, false) >> 16) & 0xff;
    assert!(
        mid > 0 && mid < 255,
        "interior coverage must be interior, got {mid}"
    );
    assert!(
        (180..=195).contains(&mid),
        "linear-light midpoint sanity, got {mid}"
    );
}

/// The green and blue channel positions compute the SAME per-channel function
/// as red (identical `blend_channel_linear` calls): pinned over a dense lattice
/// so a channel-order regression in `blend_text`'s packing cannot hide behind
/// the red-only full sweep above.
#[test]
fn channels_agree_on_lattice() {
    let lattice: Vec<u32> = (0..=255).step_by(15).collect(); // 0,15,…,255 (18 values)
    for &b in &lattice {
        for &f in &lattice {
            for t in [1u8, 37, 128, 200, 254] {
                let r = (blend_text(b << 16, f << 16, b << 16, t, false) >> 16) & 0xff;
                let g = (blend_text(b << 8, f << 8, b << 8, t, false) >> 8) & 0xff;
                let bl = blend_text(b, f, b, t, false) & 0xff;
                assert_eq!(r, g, "green diverged from red at b={b} f={f} t={t}");
                assert_eq!(r, bl, "blue diverged from red at b={b} f={f} t={t}");
            }
        }
    }
}
