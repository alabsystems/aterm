// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 conformance for the W1 window-fit + padding-absorption policy: the
//! SHIPPING [`aterm_render::pad_split`] / [`aterm_render::band_offset`] /
//! [`aterm_render::place_frame_bands`] trio that kills the compositor stretch.
//!
//! The sin being fixed: the swapchain was sized to the grid-quantized frame
//! (`cols*cell + 2*pad`), never the window's raw physical pixels, so the
//! compositor (CAMetalLayer's default resize gravity) non-integrally rescaled
//! the whole frame after nearly any drag/tile/zoom (~1.005x, permanent
//! softness). The fix sizes the surface to the RAW window and absorbs the
//! `0..cell-1` remainder into per-edge padding bands — which is only correct if
//! the split law holds EXACTLY, hence the proofs.
//!
//! ## Two-tier proof
//!
//! * **Tier-0 (abstract, model-checked by the Trust `ty` compiler)** — the
//!   `PadAbsorption` derived model (`aterm_spec::derive::pad_absorption_model`)
//!   carries `ExactCover` / `PadsFloor` / `NearEvenSplit` / `Maximal`.
//!   `cargo test -p aterm-spec`
//!   (`derived_pad_absorption_proves_and_catches_lopsided_split`) runs the REAL
//!   `ty` binary over the whole bounded lattice: it PROVES the four invariants
//!   at `Buggy=0` and CATCHES the lopsided all-remainder-on-one-edge split at
//!   `Buggy=1` (counterexample).
//! * **Tier-1 (concrete, this file)** — the same invariants checked directly
//!   against the shipping `pad_split` over a deliberate odd/even lattice far
//!   larger than the model's, PLUS a point-for-point conformance drive of the
//!   model's own executable interpreter against `pad_split` over the model's
//!   entire domain (model ↔ code can't drift), PLUS the placement law for
//!   `place_frame_bands` (content byte-identical at the band offset — zero
//!   scaling — bands exactly the theme background).
//!
//! The full-bit-width arithmetic twin lives in `aterm-render`'s
//! `pad_split_kani` trust-mc harnesses (`verify.sh --full`).

use aterm_render::{band_offset, band_offset_y, pad_split, place_frame_bands};
use aterm_spec::derive::pad_absorption_model;

/// A deliberate odd/even lattice: every pad and cell parity combination, cells
/// spanning tiny (1 px — degenerate) through typical glyph boxes (7..=17) to
/// large (24), pads through the `pad_for_scale` range and beyond.
const PADS: &[usize] = &[0, 1, 2, 3, 4, 7, 8];
const CELLS: &[usize] = &[1, 2, 3, 5, 7, 8, 9, 10, 16, 17, 24];

/// THE INVARIANT (the same four properties `ty` model-checks abstractly in
/// aterm-spec), exhaustive over every window size up to 12 columns past the
/// domain edge for every (pad, cell) lattice point.
#[test]
fn pad_split_exact_cover_maximal_floor_near_even() {
    let mut odd_remainder_seen = 0usize;
    for &pad in PADS {
        for &cell in CELLS {
            for w in 0..=(2 * pad + 13 * cell) {
                let s = pad_split(w, pad, cell);
                // Totality everywhere (a window smaller than one padded cell
                // still yields a 1-cell grid; the presents then crop, centred).
                assert!(
                    s.cells >= 1,
                    "cells must never be 0 (w={w} pad={pad} cell={cell})"
                );
                if w < 2 * pad + cell {
                    continue; // below the proven domain
                }
                // Exact cover: no pixel scaled or dropped.
                assert_eq!(
                    s.pad_lo + s.cells * cell + s.pad_hi,
                    w,
                    "pad_lo + cols*cell + pad_hi must equal the window exactly \
                     (w={w} pad={pad} cell={cell}, split={s:?})"
                );
                // Maximality: one more column would overflow the usable extent.
                assert!(
                    (s.cells + 1) * cell > w - 2 * pad,
                    "cols must be maximal (w={w} pad={pad} cell={cell}, split={s:?})"
                );
                // Pads floor: the configured border never shrinks.
                assert!(
                    s.pad_lo >= pad && s.pad_hi >= pad,
                    "pads must keep the configured floor (w={w} pad={pad} cell={cell}, split={s:?})"
                );
                // Near-even split, leaning to the trailing edge.
                assert!(
                    s.pad_lo <= s.pad_hi && s.pad_hi - s.pad_lo <= 1,
                    "|pad_hi - pad_lo| <= 1 (w={w} pad={pad} cell={cell}, split={s:?})"
                );
                // Present-side consistency: the centred blit offset equals the
                // split's leading band — the swapchain placement and the grid
                // computation cannot disagree.
                let frame = s.cells * cell + 2 * pad;
                assert_eq!(
                    band_offset(w, frame),
                    (s.pad_lo - pad) as i64,
                    "band_offset must equal pad_lo - pad (w={w} pad={pad} cell={cell})"
                );
                if s.pad_lo != s.pad_hi {
                    odd_remainder_seen += 1;
                }
            }
        }
    }
    // NON-VACUITY: the odd-remainder branch (the one the lopsided pre-fix split
    // would get wrong) is genuinely exercised.
    assert!(
        odd_remainder_seen > 0,
        "the lattice must reach an odd remainder (pad_lo != pad_hi)"
    );
}

/// NEGATIVE CONTROL: the pre-fix behavior — sizing the surface to the quantized
/// frame and letting the compositor stretch — corresponds to pretending
/// `pad_lo == pad_hi == pad` for a window with a remainder. Show the exact-cover
/// law genuinely rejects it (the invariant is not satisfiable by the old world).
#[test]
fn pad_split_rejects_the_prefix_quantized_frame() {
    // 80 columns of a 9px cell + 4px pad + a 7px drag remainder.
    let (pad, cell) = (4usize, 9usize);
    let w = 2 * pad + 80 * cell + 7;
    let s = pad_split(w, pad, cell);
    assert_eq!(s.cells, 80);
    // The old world: both pads at the base value -> the cover misses by 7px,
    // which is exactly what the compositor then rescaled the frame to hide.
    assert_ne!(pad + s.cells * cell + pad, w, "pre-fix cover must miss");
    assert_eq!(
        s.pad_lo + s.cells * cell + s.pad_hi,
        w,
        "fixed cover is exact"
    );
    assert_eq!((s.pad_lo, s.pad_hi), (pad + 3, pad + 4), "7px splits 3/4");
}

/// Tier-1 MODEL ↔ CODE conformance: drive the `PadAbsorption` model's own
/// executable interpreter (`Model::fire` — the same semantics `ty` checks) over
/// its ENTIRE bounded domain and assert the settled `(acc, pad_lo, pad_hi)`
/// equals the shipping `pad_split` at every point. The abstract twin and the
/// real policy cannot drift.
#[test]
fn pad_split_conforms_to_the_ty_checked_model() {
    let m = pad_absorption_model();
    let mut points = 0usize;
    for cell in 1..=4i64 {
        for pad in 0..=2i64 {
            for w in (2 * pad + cell)..=14 {
                // Enter the model at phase 3 (the Pick* actions are the
                // nondeterministic lattice enumeration this loop performs).
                let mut state = m.init_state();
                state.insert("phase", 3);
                state.insert("cell", cell);
                state.insert("pad", pad);
                state.insert("w", w);
                while m.action_enabled("FitColumn", &state) {
                    assert!(m.fire("FitColumn", &mut state));
                }
                assert!(
                    m.action_enabled("Settle", &state),
                    "Settle must be enabled once no more columns fit (w={w} pad={pad} cell={cell})"
                );
                assert!(m.fire("Settle", &mut state));
                let s = pad_split(w as usize, pad as usize, cell as usize);
                assert_eq!(
                    (
                        state["acc"] as usize,
                        state["pad_lo"] as usize,
                        state["pad_hi"] as usize
                    ),
                    (s.cells * cell as usize, s.pad_lo, s.pad_hi),
                    "model and pad_split must agree (w={w} pad={pad} cell={cell})"
                );
                points += 1;
            }
        }
    }
    // NON-VACUITY: the whole model domain was walked.
    assert!(
        points > 100,
        "conformance must cover the model lattice ({points} points)"
    );
}

/// A recognizable synthetic frame: pixel = position-derived, unique per (x, y).
fn synth_frame(w: usize, h: usize) -> Vec<u32> {
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            (((x & 0xfff) as u32) << 12) | ((y & 0xfff) as u32)
        })
        .collect()
}

const BAND: u32 = 0x0018_2830; // a recognizable theme background

/// PLACEMENT LAW (the CPU half of the readback pin): a destination exactly
/// `frame + 7px` per axis holds the frame BYTE-IDENTICAL at the band offsets
/// (x centred: 3 leading / 4 trailing; y per the platform `band_offset_y` —
/// top-pinned on Linux, centred elsewhere — zero scaling either way), and
/// every band pixel is exactly the theme background.
#[test]
fn place_frame_bands_offsets_without_scaling() {
    let (fw, fh) = (46usize, 30usize);
    let src = synth_frame(fw, fh);
    let (dw, dh) = (fw + 7, fh + 7);
    let mut dst = vec![0xffff_ffffu32; dw * dh];
    place_frame_bands(&mut dst, dw, dh, &src, fw, fh, false, BAND);

    let (ox, oy) = (band_offset(dw, fw), band_offset_y(dh, fh));
    assert_eq!(
        (ox, oy),
        (3, if cfg!(target_os = "linux") { 0 } else { 3 }),
        "x: 7px remainder splits 3 leading / 4 trailing; y: platform policy"
    );
    let mut band_px = 0usize;
    for y in 0..dh {
        for x in 0..dw {
            let d = dst[y * dw + x];
            let (sx, sy) = (x as i64 - ox, y as i64 - oy);
            if sx >= 0 && sy >= 0 && (sx as usize) < fw && (sy as usize) < fh {
                assert_eq!(
                    d,
                    src[sy as usize * fw + sx as usize],
                    "content must be byte-identical at ({x},{y}) — zero scaling"
                );
            } else {
                assert_eq!(d, BAND, "band pixel at ({x},{y}) must be the theme bg");
                band_px += 1;
            }
        }
    }
    // NON-VACUITY: both the content and the band branches were exercised, and
    // the band is exactly the remainder area.
    assert_eq!(band_px, dw * dh - fw * fh);
}

/// Exact fit (`dst == src` dims) is byte-identical to the historical whole-buffer
/// copy — offset 0, zero band pixels — and the bell invert XORs content only.
#[test]
fn place_frame_bands_exact_fit_is_identity_and_invert_is_xor() {
    let (fw, fh) = (23usize, 11usize);
    let src = synth_frame(fw, fh);
    let mut dst = vec![0u32; fw * fh];
    place_frame_bands(&mut dst, fw, fh, &src, fw, fh, false, BAND);
    assert_eq!(dst, src, "exact fit must be the identity copy");

    let mut inv = vec![0u32; fw * fh];
    place_frame_bands(&mut inv, fw, fh, &src, fw, fh, true, BAND);
    for (i, (&a, &b)) in inv.iter().zip(src.iter()).enumerate() {
        assert_eq!(a, b ^ 0x00ff_ffff, "invert must be the bell XOR at {i}");
    }
}

/// CROP (transient mid-drag / degenerate tiny window): a destination SMALLER
/// than the frame takes an unscaled sub-rect — centred horizontally, and
/// vertically per the platform policy (top-pinned on Linux keeps the frame's
/// TOP rows and crops the bottom; centred elsewhere) — and an asymmetric
/// (one-axis) remainder bands only that axis.
#[test]
fn place_frame_bands_crops_centred_and_handles_one_axis() {
    let (fw, fh) = (20usize, 12usize);
    let src = synth_frame(fw, fh);

    // Crop both axes: dst 15x9 inside a 20x12 frame. Verify against the
    // shipping offset pair directly, so the mapping below cannot drift from
    // what `place_frame_bands` computes internally.
    let (dw, dh) = (15usize, 9usize);
    let mut dst = vec![0u32; dw * dh];
    place_frame_bands(&mut dst, dw, dh, &src, fw, fh, false, BAND);
    let (ox, oy) = (band_offset(dw, fw), band_offset_y(dh, fh));
    assert!(ox < 0);
    if cfg!(target_os = "linux") {
        assert_eq!(oy, 0, "top-pinned crop keeps the frame's top rows");
    } else {
        assert!(oy < 0);
    }
    for y in 0..dh {
        for x in 0..dw {
            let (sx, sy) = ((x as i64 - ox) as usize, (y as i64 - oy) as usize);
            assert_eq!(
                dst[y * dw + x],
                src[sy * fw + sx],
                "crop must be 1:1 at ({x},{y})"
            );
        }
    }

    // One-axis remainder: width +5, height exact.
    let (dw, dh) = (fw + 5, fh);
    let mut dst = vec![0u32; dw * dh];
    place_frame_bands(&mut dst, dw, dh, &src, fw, fh, false, BAND);
    let ox = band_offset(dw, fw);
    assert_eq!(ox, 2);
    for y in 0..dh {
        for x in 0..dw {
            let d = dst[y * dw + x];
            let sx = x as i64 - ox;
            if sx >= 0 && (sx as usize) < fw {
                assert_eq!(d, src[y * fw + sx as usize]);
            } else {
                assert_eq!(d, BAND);
            }
        }
    }
}
