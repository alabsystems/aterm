// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Conformance for the window-fit padding-absorption policy:
//! [`aterm_render::pad_split`], [`aterm_render::band_offset`] and
//! [`aterm_render::place_frame_bands`].
//!
//! The surface is sized to the raw window, not the grid-quantized frame (which
//! the compositor would rescale non-integrally), and the `0..cell-1` remainder
//! goes into per-edge padding bands; that is correct only if the split law
//! holds exactly.
//!
//! The `PadAbsorption` model (`aterm_models_render::pad_absorption_model`)
//! carries `ExactCover` / `PadsFloor` / `NearEvenSplit` / `Maximal`, and
//! aterm-spec's registered-model checks run `ty` over it. This file checks the
//! same invariants on `pad_split` over a larger lattice, drives the model's
//! interpreter against `pad_split` over the model's whole domain, and pins the
//! placement law of `place_frame_bands`.

use aterm_models_render::pad_absorption_model;
use aterm_render::{band_offset, band_offset_y, pad_split, place_frame_bands};

/// Every pad and cell parity, with cells from degenerate through typical glyph
/// boxes to large.
const PADS: &[usize] = &[0, 1, 2, 3, 4, 7, 8];
/// `0` is the degenerate cell box a broken font can report: every window is
/// then below the domain, and must still get a one-cell grid.
const CELLS: &[usize] = &[0, 1, 2, 3, 5, 7, 8, 9, 10, 16, 17, 24];

/// The four properties the aterm-spec model checks, exhaustive over every
/// window size up to 12 columns past the domain edge at every lattice point.
#[test]
fn pad_split_exact_cover_maximal_floor_near_even() {
    let mut odd_remainder_seen = 0usize;
    for &pad in PADS {
        for &cell in CELLS {
            for w in 0..=(2 * pad + 13 * cell) {
                let s = pad_split(w, pad, cell);
                // Total: a window smaller than one padded cell still gets a
                // 1-cell grid, which the presents crop.
                assert!(
                    s.cells >= 1,
                    "cells must never be 0 (w={w} pad={pad} cell={cell})"
                );
                if w < 2 * pad + cell.max(1) {
                    // Below the domain (a zero cell box included) the grid pins
                    // to exactly one cell.
                    assert_eq!(
                        s.cells, 1,
                        "below the domain the grid pins to one cell (w={w} pad={pad} cell={cell})"
                    );
                    continue;
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
                // The centred blit offset equals the split's leading band.
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
    // Non-vacuity: the odd-remainder branch, the one a lopsided split gets
    // wrong, is exercised.
    assert!(
        odd_remainder_seen > 0,
        "the lattice must reach an odd remainder (pad_lo != pad_hi)"
    );
}

/// Model/code conformance: the `PadAbsorption` model's interpreter
/// (`Model::fire`) settles to the same `(acc, pad_lo, pad_hi)` as `pad_split`
/// at every point of the model's bounded domain.
#[test]
fn pad_split_conforms_to_the_ty_checked_model() {
    let m = pad_absorption_model();
    let mut points = 0usize;
    for cell in 1..=4i64 {
        for pad in 0..=2i64 {
            for w in (2 * pad + cell)..=14 {
                // Enter at phase 3: this loop does the Pick* enumeration.
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
    // Non-vacuity: the whole model domain was walked.
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

/// Exact fit (`dst == src` dims) is the identity copy, and the bell invert XORs
/// content only.
#[test]
fn place_frame_bands_exact_fit_is_identity_and_invert_is_xor() {
    let (fw, fh) = (23usize, 11usize);
    let src = synth_frame(fw, fh);
    let mut dst = vec![0u32; fw * fh];
    place_frame_bands(&mut dst, fw, fh, &src, fw, fh, false, BAND, 0..0);
    assert_eq!(dst, src, "exact fit must be the identity copy");

    let mut inv = vec![0u32; fw * fh];
    place_frame_bands(&mut inv, fw, fh, &src, fw, fh, true, BAND, 0..0);
    for (i, (&a, &b)) in inv.iter().zip(src.iter()).enumerate() {
        assert_eq!(a, b ^ 0x00ff_ffff, "invert must be the bell XOR at {i}");
    }
}

/// A destination smaller than the frame (mid-drag, or a tiny window) takes an
/// unscaled sub-rect, centred horizontally and vertically per the platform
/// policy (top-pinned on Linux, centred elsewhere); a one-axis remainder bands
/// only that axis.
#[test]
fn place_frame_bands_crops_centred_and_handles_one_axis() {
    let (fw, fh) = (20usize, 12usize);
    let src = synth_frame(fw, fh);

    // Crop both axes: dst 15x9 inside a 20x12 frame, checked against the
    // shipping offset pair.
    let (dw, dh) = (15usize, 9usize);
    let mut dst = vec![0u32; dw * dh];
    place_frame_bands(&mut dst, dw, dh, &src, fw, fh, false, BAND, 0..0);
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
    place_frame_bands(&mut dst, dw, dh, &src, fw, fh, false, BAND, 0..0);
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

/// Beside the source rows in `edge_rows` the remainder bands continue the row's
/// own first and last pixel, so a chrome band (and a meter on it) reaches the
/// true window edge. Every other band pixel stays the theme background,
/// including the bands above and below the frame and the rows above a range
/// that starts past 0 (`ChromeBleed::first`); the continued pixels are band
/// pixels, so the bell invert never flips them; an empty range leaves every
/// remainder pixel the theme background.
#[test]
fn chrome_rows_continue_their_edge_pixels_through_the_remainder_bands() {
    let (fw, fh) = (20usize, 12usize);
    let src = synth_frame(fw, fh);
    let (dw, dh) = (fw + 7, fh + 7);
    let (ox, oy) = (band_offset(dw, fw), band_offset_y(dh, fh));
    for (edge_rows, invert) in [(0..0, false), (0..5, false), (0..5, true), (2..5, false)] {
        let mut dst = vec![0u32; dw * dh];
        place_frame_bands(
            &mut dst,
            dw,
            dh,
            &src,
            fw,
            fh,
            invert,
            BAND,
            edge_rows.clone(),
        );
        let (mut continued, mut banded) = (0usize, 0usize);
        for y in 0..dh {
            for x in 0..dw {
                let d = dst[y * dw + x];
                let (sx, sy) = (x as i64 - ox, y as i64 - oy);
                let in_rows = sy >= 0 && (sy as usize) < fh;
                let in_cols = sx >= 0 && (sx as usize) < fw;
                if in_rows && in_cols {
                    let s = src[sy as usize * fw + sx as usize];
                    assert_eq!(
                        d,
                        if invert { s ^ 0x00ff_ffff } else { s },
                        "content ({x},{y})"
                    );
                } else if in_rows && edge_rows.contains(&(sy as usize)) {
                    let row = sy as usize * fw;
                    let edge = if sx < 0 { src[row] } else { src[row + fw - 1] };
                    assert_eq!(d, edge, "a chrome row's band continues its edge ({x},{y})");
                    continued += 1;
                } else {
                    assert_eq!(d, BAND, "every other band pixel is the theme bg ({x},{y})");
                    banded += 1;
                }
            }
        }
        // Non-vacuity: both arms ran.
        assert_eq!(continued, edge_rows.len() * (dw - fw));
        assert_eq!(banded, dw * dh - fw * fh - continued);
    }
    // `edge_rows` past the frame is clamped by the frame itself.
    let mut all = vec![0u32; dw * dh];
    place_frame_bands(&mut all, dw, dh, &src, fw, fh, false, BAND, 0..usize::MAX);
    for y in 0..dh {
        let sy = y as i64 - oy;
        if sy < 0 || sy as usize >= fh {
            assert!(
                all[y * dw..y * dw + dw].iter().all(|&p| p == BAND),
                "row {y}"
            );
        }
    }
}
