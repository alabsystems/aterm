// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Conformance tests for window-fit padding absorption ([`aterm_render::pad_split`]).

use aterm_models_render::pad_absorption_model;
use aterm_render::{
    AxisPlacement, band_offset, band_offset_y, continue_chrome_edges, frame_placement, pad_split,
    place_frame_bands,
};

const PADS: &[usize] = &[0, 1, 2, 3, 4, 7, 8];
const CELLS: &[usize] = &[0, 1, 2, 3, 5, 7, 8, 9, 10, 16, 17, 24];

#[test]
fn pad_split_exact_cover_maximal_floor_near_even() {
    let mut odd_remainder_seen = 0usize;
    for &pad in PADS {
        for &cell in CELLS {
            for w in 0..=(2 * pad + 13 * cell) {
                let s = pad_split(w, pad, cell);
                assert!(
                    s.cells >= 1,
                    "cells must never be 0 (w={w} pad={pad} cell={cell})"
                );
                if w < 2 * pad + cell.max(1) {
                    assert_eq!(
                        s.cells, 1,
                        "below the domain the grid pins to one cell (w={w} pad={pad} cell={cell})"
                    );
                    continue;
                }
                assert_eq!(
                    s.pad_lo + s.cells * cell + s.pad_hi,
                    w,
                    "pad_lo + cols*cell + pad_hi must equal the window exactly \
                     (w={w} pad={pad} cell={cell}, split={s:?})"
                );
                assert!(
                    (s.cells + 1) * cell > w - 2 * pad,
                    "cols must be maximal (w={w} pad={pad} cell={cell}, split={s:?})"
                );
                assert!(
                    s.pad_lo >= pad && s.pad_hi >= pad,
                    "pads must keep the configured floor (w={w} pad={pad} cell={cell}, split={s:?})"
                );
                assert!(
                    s.pad_lo <= s.pad_hi && s.pad_hi - s.pad_lo <= 1,
                    "|pad_hi - pad_lo| <= 1 (w={w} pad={pad} cell={cell}, split={s:?})"
                );
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
    assert!(
        odd_remainder_seen > 0,
        "the lattice must reach an odd remainder (pad_lo != pad_hi)"
    );
}

#[test]
fn pad_split_conforms_to_the_ty_checked_model() {
    let m = pad_absorption_model();
    let mut points = 0usize;
    for cell in 1..=4i64 {
        for pad in 0..=2i64 {
            for w in (2 * pad + cell)..=14 {
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
    assert!(
        points > 100,
        "conformance must cover the model lattice ({points} points)"
    );
}

fn synth_frame(w: usize, h: usize) -> Vec<u32> {
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            (((x & 0xfff) as u32) << 12) | ((y & 0xfff) as u32)
        })
        .collect()
}

const BAND: u32 = 0x0018_2830; // a recognizable theme background

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

#[test]
fn place_frame_bands_crops_centred_and_handles_one_axis() {
    let (fw, fh) = (20usize, 12usize);
    let src = synth_frame(fw, fh);

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

#[test]
fn frame_placement_partitions_surface_and_frame() {
    let parts = |axis: AxisPlacement| {
        (
            axis.origin,
            axis.band_before,
            axis.visible,
            axis.band_after,
            axis.crop_before,
            axis.crop_after,
        )
    };
    let banded = frame_placement(111, 111, 100, 100);
    assert_eq!(
        parts(banded.x),
        (5, 5, 100, 6, 0, 0),
        "odd spare pixel trails"
    );
    let cropped = frame_placement(100, 100, 111, 111);
    assert_eq!(parts(cropped.x), (-6, 0, 100, 0, 6, 5), "centred crop");
    if cfg!(target_os = "linux") {
        assert_eq!(parts(banded.y), (0, 0, 100, 11, 0, 0));
        assert_eq!(parts(cropped.y), (0, 0, 100, 0, 0, 11));
    } else {
        assert_eq!(banded.y, banded.x);
        assert_eq!(cropped.y, cropped.x);
    }

    for surface in 0..=24usize {
        for frame in 0..=24usize {
            let place = frame_placement(surface, surface, frame, frame);
            for (axis, offset) in [
                (place.x, band_offset(surface, frame)),
                (place.y, band_offset_y(surface, frame)),
            ] {
                let at = (surface, frame);
                assert_eq!(axis.origin, offset, "{at:?}");
                assert_eq!(axis.band_before + axis.visible + axis.band_after, surface);
                assert_eq!(axis.crop_before + axis.visible + axis.crop_after, frame);
                for px in 0..surface {
                    let shown = px as i64 - offset;
                    let covered = (0..frame as i64).contains(&shown);
                    assert_eq!(
                        axis.source(px),
                        covered.then_some(shown as usize),
                        "{at:?} surface px {px}"
                    );
                }
            }
            let rect = place
                .visible_rect(surface, surface)
                .expect("a placement in its own surface");
            let shown =
                |axis: AxisPlacement| (0..surface).filter(move |&px| axis.source(px).is_some());
            assert_eq!(
                (rect.x..rect.x + rect.width).collect::<Vec<_>>(),
                shown(place.x).collect::<Vec<_>>(),
                "{surface} {frame}"
            );
            assert_eq!(
                (rect.y..rect.y + rect.height).collect::<Vec<_>>(),
                shown(place.y).collect::<Vec<_>>(),
                "{surface} {frame}"
            );
            assert_eq!(place.visible_rect(surface + 1, surface), None);
            assert_eq!(place.visible_rect(surface, surface + 1), None);
        }
    }
}

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
        assert_eq!(continued, edge_rows.len() * (dw - fw));
        assert_eq!(banded, dw * dh - fw * fh - continued);
    }
    let rule = 0x00AB_CDEF;
    for (edge_rows, rule_row) in [(0..5, 3usize), (2..5, 4), (0..0, 3), (0..5, 7)] {
        let mut dst = vec![0u32; dw * dh];
        place_frame_bands(
            &mut dst,
            dw,
            dh,
            &src,
            fw,
            fh,
            false,
            BAND,
            edge_rows.clone(),
        );
        let y = (rule_row as i64 + oy) as usize;
        let frame = ox as usize..ox as usize + fw;
        dst[y * dw..y * dw + dw][frame.clone()].fill(rule);
        let before = dst.clone();
        continue_chrome_edges(&mut dst, dw, dh, fw, fh, edge_rows.clone());
        let chrome = edge_rows.contains(&rule_row);
        for (i, (&d, &b)) in dst.iter().zip(&before).enumerate() {
            let (x, py) = (i % dw, i / dw);
            if py == y && !frame.contains(&x) && chrome {
                assert_eq!(
                    d, rule,
                    "the chrome row's band continues the card ({x},{py})"
                );
            } else {
                assert_eq!(d, b, "every other pixel is left as placed ({x},{py})");
            }
        }
        assert_eq!(dst != before, chrome, "{edge_rows:?}, rule on {rule_row}");
    }
    let mut exact = src.clone();
    continue_chrome_edges(&mut exact, fw, fh, fw, fh, 0..fh);
    assert_eq!(exact, src);
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
