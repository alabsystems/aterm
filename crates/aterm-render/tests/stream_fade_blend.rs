// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The stream fade's blend: a drying cell's foreground is
//! `blend_text(bg, fg, bg, alpha, false)`, the exact linear-light alpha-over,
//! at an ease-out coverage monotone in age (proven in aterm-gui's
//! `stream_fade`). These tests pin the blend half:
//!
//! - Convergence: coverage 255 returns the fg bytes and coverage 0 the
//!   destination bytes, exactly (early returns in `blend_text`;
//!   `text_blending.rs::endpoint_exactness_both_modes` pins both modes). With
//!   alpha 255 once a cell's age reaches `stream_fade_ms`, the steady frame
//!   equals the no-fade frame.
//! - Monotonicity: each channel moves monotonically toward the fg across the
//!   full 256×256×256 (bg, fg, coverage) domain of one channel, which the other
//!   two share (`blend_channel_linear` per channel), so a drying glyph only
//!   approaches its final ink.
//!
//! The uncorrected mode is deliberate: the tint picks an intermediate colour in
//! linear light; the perceptual coverage remap belongs to the glyph raster.

use aterm_render::blend_text;

/// For every (bg, fg) byte pair the blended red channel is monotone in
/// coverage over all 256 steps, and an interior coverage lands strictly
/// between bg and fg.
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
            // The sweep ends on the exact fg.
            assert_eq!(prev, f, "the t=255 sample must equal fg exactly");
        }
    }
    // Non-vacuity: the linear-light midpoint of white over black is ~sRGB 188.
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

/// Green and blue compute the same per-channel function as red, over a dense
/// lattice, so a channel-order slip in `blend_text`'s packing cannot hide
/// behind the red-only sweep.
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
