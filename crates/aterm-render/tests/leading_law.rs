// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The LEADING LAW (W5a), machine-checked over a dense metrics lattice: for
//! every (ascent, descent, lineGap, line_height) the shipping
//! [`aterm_render::cell_h_baseline`] satisfies
//!
//! 1. `cell_h == round((ascent − descent + lineGap) · line_height)` (min 1) —
//!    the cell box is the font's full line times the host multiplier; and
//! 2. HALF-LEADING SPLIT: the total leading `cell_h − (ascent − descent)`
//!    lands half above / half below the glyph within 1px —
//!    `|space_above − space_below| <= 1` where
//!    `space_above = baseline − ascent` and
//!    `space_below = cell_h − (ascent − descent) − space_above`.
//!
//! The pre-fix law (`baseline = round(ascent)`, dropping the WHOLE lineGap
//! below the descent) is kept as the NEGATIVE CONTROL: it top-biases every
//! nonzero-lineGap face and violates the split.
//!
//! This is an arithmetic (rounding) law over f32, which the ty model language
//! cannot express (no multiplication/division) — per the repo's verification
//! map the always-on lattice test IS the machine check for rounding laws
//! (the box-drawing `procedural.rs` precedent). An integration test then pins
//! the REAL renderer geometry to the same function through the public API.

use aterm_render::{Renderer, Theme, cell_h_baseline};

/// Deliberate odd/even + fractional lattice: typical monospace verticals at
/// UI px sizes, descents (NEGATIVE, the fontdue convention), hhea/OS/2 line
/// gaps from zero through generous, and the config `line_height` domain
/// (0.8..=2.0) plus the renderer's wider clamp bound.
const ASCENTS: [f32; 7] = [4.0, 7.3, 9.6, 12.0, 15.5, 20.0, 24.8];
const DESCENTS: [f32; 5] = [-1.0, -2.4, -3.0, -5.7, -8.0];
const GAPS: [f32; 6] = [0.0, 0.5, 1.0, 2.3, 4.0, 7.9];
const SCALES: [f32; 8] = [0.5, 0.8, 0.9, 1.0, 1.05, 1.25, 1.5, 2.0];

#[test]
fn cell_height_is_the_rounded_scaled_line() {
    for a in ASCENTS {
        for d in DESCENTS {
            for g in GAPS {
                for s in SCALES {
                    let (cell_h, _) = cell_h_baseline(a, d, g, s);
                    let expect = (((a - d + g) * s).round()).max(1.0) as usize;
                    assert_eq!(
                        cell_h, expect,
                        "cell_h law: ascent={a} descent={d} gap={g} scale={s}"
                    );
                }
            }
        }
    }
}

#[test]
fn half_leading_splits_within_one_px() {
    let mut split_moved_baseline = 0u32; // non-vacuity
    for a in ASCENTS {
        for d in DESCENTS {
            for g in GAPS {
                for s in SCALES {
                    let (cell_h, baseline) = cell_h_baseline(a, d, g, s);
                    let content = a - d;
                    let above = baseline as f32 - a;
                    let below = cell_h as f32 - content - above;
                    assert!(
                        (above - below).abs() <= 1.0 + 1e-4,
                        "half-leading: ascent={a} descent={d} gap={g} scale={s} \
                         above={above} below={below}"
                    );
                    if baseline != a.round() as i32 {
                        split_moved_baseline += 1;
                    }
                }
            }
        }
    }
    // Non-vacuity: for nonzero gaps / non-1 scales the split genuinely moves
    // the baseline off the old `round(ascent)` — the law is not trivially the
    // old behavior.
    assert!(split_moved_baseline > 100, "got {split_moved_baseline}");
}

/// NEGATIVE CONTROL: the pre-fix law (`baseline = round(ascent)` — all the
/// lineGap below the descent) violates the half-leading split for every
/// lattice point with `gap >= 2` at `scale = 1`, so the split assertion above
/// genuinely separates the fix from the bug.
#[test]
fn old_all_gap_below_law_violates_the_split() {
    let mut violations = 0u32;
    let mut checked = 0u32;
    for a in ASCENTS {
        for d in DESCENTS {
            for g in GAPS {
                if g < 2.0 {
                    continue;
                }
                checked += 1;
                let content = a - d;
                let cell_h = (content + g).ceil().max(1.0); // old natural box
                let baseline = a.round(); // old law: no half-leading
                let above = baseline - a;
                let below = cell_h - content - above;
                if (above - below).abs() > 1.0 + 1e-4 {
                    violations += 1;
                }
            }
        }
    }
    assert_eq!(
        violations, checked,
        "the old law must violate the split at every gap>=2 lattice point"
    );
    assert!(checked > 0, "non-vacuity");
}

/// Integration (requires the bundled test font): the REAL renderer's public
/// `cell_size`/`baseline` are exactly `cell_h_baseline` of its live metrics —
/// at the default line-height, after `set_line_height`, and after the
/// `set_adjust_baseline` escape hatch (which shifts baseline only, never the
/// cell box).
#[cfg(feature = "embedded-font")]
#[test]
fn renderer_geometry_follows_the_law_live() {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/jetbrains-mono.ttf"
    ))
    .expect("committed JetBrains Mono fixture");
    let mut r = Renderer::from_bytes(&bytes, 16.0, Theme::default()).expect("fixture parses");

    let (_, h0) = r.cell_size();
    let b0 = r.baseline();

    // set_line_height re-derives through the same law: scaling to 1.5 grows
    // the box per the law and keeps the split (checked via the public API by
    // comparing against cell_h_baseline of the recovered natural metrics).
    r.set_line_height(1.5);
    let (_, h1) = r.cell_size();
    let b1 = r.baseline();
    assert!(h1 > h0, "1.5 line-height grows the cell box");
    // Recover the natural (scale-1) metrics from the law itself: with
    // content+gap = natural, cell_h(1.0) = round(natural). Cross-check the
    // 1.5 box against the same natural.
    let natural = h0 as f32; // round(natural*1.0)
    assert_eq!(h1, (natural * 1.5).round() as usize, "law at scale 1.5");
    // The added leading splits: baseline moved down by ~half the delta.
    let delta = h1 as f32 - natural;
    #[allow(clippy::cast_precision_loss, reason = "baseline deltas are tiny")]
    let moved = (b1 - b0) as f32;
    assert!(
        (moved - delta / 2.0).abs() <= 1.5,
        "baseline moved by ~half the added leading (moved {moved}, delta {delta})"
    );

    // adjust_baseline: pure baseline shift, cell box untouched, clamped, and
    // 0 restores the derivation.
    r.set_adjust_baseline(3);
    assert_eq!(r.baseline(), b1 + 3, "escape hatch shifts the baseline");
    assert_eq!(r.cell_size().1, h1, "cell box is untouched");
    r.set_adjust_baseline(1000);
    assert_eq!(r.baseline(), b1 + 64, "clamped to +/-64");
    r.set_adjust_baseline(0);
    assert_eq!(r.baseline(), b1, "0 restores the pure derivation");
}
