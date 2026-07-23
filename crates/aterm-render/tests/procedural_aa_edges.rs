// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PROOF (seam-tiling law) — every ANTI-ALIASED procedural glyph keeps hard
//! 0/255 cell-EDGE texels, at every cell size.
//!
//! Two-tier proof, same idiom as `presentation_gate.rs`:
//!
//! * Tier-0 (abstract): `aterm_spec::derive::aa_edge_hardening_model()` — the
//!   ty-checked twin. Its invariant (`edge = 1 => out ∈ {0, MAX}`) is exactly
//!   the law asserted here; its `Buggy = 1` branch (skip the hardening pass)
//!   yields the counterexample. Checked by the real Trust `ty` in
//!   aterm-spec's `derived_ring_ty.rs`.
//! * Tier-1 (this file): the SAME invariant checked against the REAL
//!   rasterizer over an exhaustive `1..=16 × 1..=16` size lattice plus
//!   larger odd/even sentinels — for every glyph of every AA family
//!   (diagonals U+2571–2573, arcs U+256D–2570, Powerline U+E0B0–E0BF, wedges
//!   U+1FB3C–1FB6F). The lattice is exhaustive over the domain where seam
//!   rounding can break, so this is a complete proof there, not a sample.
//!
//! WHY it matters: a cell-edge texel with fractional coverage would blend
//! differently on the CPU (software sRGB) and GPU (hardware sRGB) AND compose
//! a visible half-covered line where two cells meet. Hard edges keep the
//! seam byte-exact on both backends — the property the box-drawing rounding
//! rule (procedural.rs module docs) guarantees for the orthogonal families.

use aterm_render::procedural;

/// Exhaustive lattice 1..=16 on both axes, plus larger odd/even sentinels.
fn sizes() -> Vec<(usize, usize)> {
    let mut v: Vec<(usize, usize)> = (1..=16usize)
        .flat_map(|w| (1..=16usize).map(move |h| (w, h)))
        .collect();
    v.extend_from_slice(&[
        (9, 19),
        (10, 20),
        (11, 21),
        (12, 24),
        (17, 33),
        (20, 8),
        (24, 48),
        (32, 15),
    ]);
    v
}

fn aa_chars() -> impl Iterator<Item = char> {
    (0x256Du32..=0x2573)
        .chain(0x1FB3C..=0x1FB6F)
        .chain(0xE0B0..=0xE0BF)
        .map(|cp| char::from_u32(cp).unwrap())
}

/// The seam-tiling law: every cell-edge texel of every AA glyph is 0 or 255,
/// at every lattice size.
#[test]
fn aa_family_cell_edge_texels_are_hard() {
    for (w, h) in sizes() {
        for ch in aa_chars() {
            assert!(
                procedural::antialiased(ch),
                "U+{:04X} must be in the AA family set",
                u32::from(ch)
            );
            let cov = procedural::coverage(ch, w, h)
                .unwrap_or_else(|| panic!("{ch:?} must be procedural at {w}x{h}"));
            let edge_idx = (0..w)
                .flat_map(|x| [x, (h - 1) * w + x])
                .chain((0..h).flat_map(|y| [y * w, y * w + w - 1]));
            for i in edge_idx {
                assert!(
                    cov[i] == 0 || cov[i] == 255,
                    "{ch:?} at {w}x{h}: SOFT cell-edge texel {} at index {i} — \
                     the seam-tiling law is broken",
                    cov[i]
                );
            }
        }
    }
}

/// Non-vacuity control: the AA regime is real. If every texel of every AA
/// glyph were hard 0/255 the edge law above would hold trivially (and the
/// supersampler would be dead code) — so require that plenty of interior
/// texels genuinely anti-alias at representative sizes.
#[test]
fn aa_family_interiors_actually_antialias() {
    let mut soft = 0usize;
    for &(w, h) in &[(9usize, 19usize), (10, 20), (12, 24)] {
        for ch in aa_chars() {
            let cov = procedural::coverage(ch, w, h).unwrap();
            soft += (1..h - 1)
                .flat_map(|y| (1..w - 1).map(move |x| y * w + x))
                .filter(|&i| !matches!(cov[i], 0 | 255))
                .count();
        }
    }
    assert!(
        soft > 500,
        "only {soft} soft interior texels across all AA glyphs — the 4x \
         supersampler is not actually anti-aliasing"
    );
}

/// Negative control for the hardening pass itself: a raw (unhardened)
/// supersampled diagonal DOES land fractional coverage on its corner-adjacent
/// edge texels, so `aa_family_cell_edge_texels_are_hard` cannot pass by
/// accident of geometry — the explicit edge pass is load-bearing. Modeled
/// here by re-deriving one edge texel's raw box-filter value: at 11x21 the ╱
/// corner pixel is partially covered (the ideal line crosses it), so its raw
/// coverage is strictly between 0 and 255 while the shipped bitmap holds 255.
#[test]
fn edge_hardening_is_load_bearing() {
    let (w, h) = (11usize, 21usize);
    let cov = procedural::coverage('╱', w, h).unwrap();
    // The shipped corner texel is hard...
    let corner = cov[(h - 1) * w]; // bottom-left, on the ideal line
    assert_eq!(corner, 255, "hardened ╱ corner must be fully lit");
    // ...but the raw box-filtered coverage of that texel is fractional:
    // reproduce the rasterizer's own subsample rule for the corner pixel.
    let (wf, hf) = (w as f32, h as f32);
    let half = (((w.min(h) + 4) / 8) as f32 / 2.0).max(0.6);
    let norm = (wf * wf + hf * hf).sqrt();
    let mut hits = 0u32;
    for sy in 0..4 {
        for sx in 0..4 {
            let px = (sx as f32 + 0.5) / 4.0;
            let py = (h - 1) as f32 + (sy as f32 + 0.5) / 4.0;
            if (hf * px + wf * py - wf * hf).abs() / norm <= half {
                hits += 1;
            }
        }
    }
    assert!(
        hits > 0 && hits < 16,
        "raw corner coverage is {hits}/16 — expected fractional; if this is \
         now naturally 0/16 or 16/16 the negative control needs a new texel"
    );
}
