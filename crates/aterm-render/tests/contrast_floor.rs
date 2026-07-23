// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 conformance for the MINIMUM-CONTRAST FLOOR (W5b): the shipping
//! [`aterm_render::floor_fg_contrast`] delivers, for EVERY `fg`/`bg`/ratio,
//!
//! ```text
//! contrast(floor_fg_contrast(fg, bg, r), bg) >= min(r, max_achievable(bg))
//! ```
//!
//! where `max_achievable(bg) = max(contrast(BLACK, bg), contrast(WHITE, bg))`
//! — the highest WCAG ratio ANY color can reach against `bg` (relative
//! luminance is bounded by the black/white poles). In words: the floor
//! delivers the requested ratio whenever the background physically admits it,
//! and the best achievable contrast otherwise. This is the bound the config
//! key `minimum_contrast` (and the OSC-12 cursor floor riding the same
//! function) promises the user.
//!
//! ## Two-tier proof
//!
//! * **Tier-0 (abstract, model-checked by the Trust `ty` compiler)** — the
//!   `ContrastFloor` derived model (`aterm_spec::derive::contrast_floor_model`)
//!   carries the same `FloorDelivers` invariant over an abstract contrast
//!   lattice. `cargo test -p aterm-spec` runs the REAL `ty` binary over the
//!   whole bounded state space: it PROVES the invariant at `Buggy=0` and
//!   CATCHES the old midpoint pole rule at `Buggy=1` (counterexample).
//! * **Tier-1 (concrete, this file)** — the bound is checked against an
//!   INDEPENDENT WCAG oracle (contrast re-derived here from the sRGB EOTF,
//!   not shared with the shipping code) exhaustively over all grayscale
//!   `fg × bg` pairs and over a dense RGB lattice, at ratios across the whole
//!   `1..=21` domain. The old pole rule (`L(bg) > 0.5` picks black) is kept
//!   as the NEGATIVE CONTROL: for mid-luminance backgrounds it falls back to
//!   the WEAKER pole and violates the bound.

use aterm_render::{floor_fg_contrast, floor_min_contrast_fg};

// ---------------------------------------------------------------------------
// Independent WCAG oracle (not the shipping implementation).
// ---------------------------------------------------------------------------

fn srgb_to_linear(c: u32) -> f32 {
    let n = (c & 0xff) as f32 / 255.0;
    if n <= 0.04045 {
        n / 12.92
    } else {
        ((n + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(rgb: u32) -> f32 {
    0.2126 * srgb_to_linear(rgb >> 16)
        + 0.7152 * srgb_to_linear(rgb >> 8)
        + 0.0722 * srgb_to_linear(rgb)
}

fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The highest ratio any color can reach against `bg`: luminance is bounded
/// by [0, 1], whose witnesses are the black/white poles.
fn max_achievable(bg: u32) -> f32 {
    contrast(0x0000_0000, bg).max(contrast(0x00ff_ffff, bg))
}

fn gray(v: u32) -> u32 {
    (v << 16) | (v << 8) | v
}

/// Small float slack for cross-implementation f32 rounding (the oracle and the
/// shipping LUT compute the same math along different code paths).
const EPS: f32 = 1e-3;

const RATIOS: [f32; 7] = [1.0, 1.5, 3.0, 4.5, 7.0, 10.0, 21.0];

// ---------------------------------------------------------------------------
// The bound, exhaustively on grayscale.
// ---------------------------------------------------------------------------

/// All 256×256 grayscale pairs × 7 ratios spanning the knob's domain: the
/// floored fg meets `min(r, max_achievable(bg))` on every single input.
#[test]
fn floor_meets_bound_grayscale_exhaustive() {
    let mut floored = 0u32; // non-vacuity: the floor must actually engage
    for f in 0..=255u32 {
        for b in 0..=255u32 {
            let (fg, bg) = (gray(f), gray(b));
            let cap = max_achievable(bg);
            for r in RATIOS {
                let out = floor_fg_contrast(fg, bg, r);
                let bound = r.min(cap);
                assert!(
                    contrast(out, bg) >= bound - EPS,
                    "bound violated: fg={fg:#08x} bg={bg:#08x} r={r} -> out={out:#08x} \
                     contrast={} < {bound}",
                    contrast(out, bg)
                );
                if out != fg {
                    floored += 1;
                }
            }
        }
    }
    assert!(
        floored > 50_000,
        "non-vacuity: the floor must engage, got {floored}"
    );
}

// ---------------------------------------------------------------------------
// The bound on a dense full-color lattice.
// ---------------------------------------------------------------------------

/// RGB lattice (5 values/channel for fg = 125 colors, 8/channel for bg = 512
/// backgrounds) × the extreme + AA ratios: the bound holds for cross-hue
/// pairs too — including every MID-LUMINANCE background, the regime the old
/// pole rule got wrong.
#[test]
fn floor_meets_bound_rgb_lattice() {
    let fg_vals: Vec<u32> = (0..5u32).map(|i| i * 255 / 4).collect();
    let bg_vals: Vec<u32> = (0..8u32).map(|i| i * 255 / 7).collect();
    let mut fgs = Vec::new();
    for &r in &fg_vals {
        for &g in &fg_vals {
            for &b in &fg_vals {
                fgs.push((r << 16) | (g << 8) | b);
            }
        }
    }
    let mut bgs = Vec::new();
    for &r in &bg_vals {
        for &g in &bg_vals {
            for &b in &bg_vals {
                bgs.push((r << 16) | (g << 8) | b);
            }
        }
    }
    let mut mid_luminance_bgs = 0u32;
    for &bg in &bgs {
        let cap = max_achievable(bg);
        let l = luminance(bg);
        if l > 0.1791 && l <= 0.5 {
            mid_luminance_bgs += 1; // the pole-rule regression regime
        }
        for &fg in &fgs {
            for r in [4.5f32, 21.0] {
                let out = floor_fg_contrast(fg, bg, r);
                let bound = r.min(cap);
                assert!(
                    contrast(out, bg) >= bound - EPS,
                    "bound violated: fg={fg:#08x} bg={bg:#08x} r={r} -> out={out:#08x}"
                );
            }
        }
    }
    assert!(
        mid_luminance_bgs > 0,
        "non-vacuity: the mid-luminance regime is covered"
    );
}

// ---------------------------------------------------------------------------
// Negative control: the OLD pole rule violates the bound.
// ---------------------------------------------------------------------------

/// Reproduce the pre-W5 fallback pole choice (`L(bg) > 0.5` → black, else
/// white — the LUMINANCE MIDPOINT, not the contrast argmax) and exhibit a
/// concrete violation: on a mid-gray background the old rule falls back to
/// WHITE although BLACK contrasts strictly more, so it cannot deliver
/// `min(21, max_achievable)`. The shipping argmax rule delivers it on the
/// same input — the law genuinely separates fix from bug.
#[test]
fn old_midpoint_pole_rule_violates_the_bound() {
    fn old_floor(fg: u32, bg: u32, min: f32) -> u32 {
        if contrast(fg, bg) >= min {
            return fg;
        }
        let target: u32 = if luminance(bg) > 0.5 { 0 } else { 0x00ff_ffff };
        let mix = |fg: u32, shift: u32, t: f32| -> u32 {
            let f = ((fg >> shift) & 0xff) as f32;
            let g = ((target >> shift) & 0xff) as f32;
            (((f + (g - f) * t).round() as u32) & 0xff) << shift
        };
        for step in 1..=10u32 {
            let t = step as f32 / 10.0;
            let cand = mix(fg, 16, t) | mix(fg, 8, t) | mix(fg, 0, t);
            if contrast(cand, bg) >= min {
                return cand;
            }
        }
        target
    }

    // Mid-gray: L ≈ 0.216 ∈ (0.179, 0.5] — black is the stronger pole.
    let bg = gray(128);
    let fg = gray(120); // low-contrast fg so the floor must engage
    let bound = 21.0f32.min(max_achievable(bg));
    let old = old_floor(fg, bg, 21.0);
    assert!(
        contrast(old, bg) < bound - 0.5,
        "control: the old rule must fall short of the bound (got {}, bound {bound})",
        contrast(old, bg)
    );
    let new = floor_fg_contrast(fg, bg, 21.0);
    assert!(
        contrast(new, bg) >= bound - EPS,
        "the argmax rule delivers the bound on the same input"
    );
}

// ---------------------------------------------------------------------------
// The wrapper's documented exemptions stay pinned.
// ---------------------------------------------------------------------------

/// `floor_min_contrast_fg` deliberately exempts `min <= 1` (knob off) and
/// `fg == bg` (SGR 8 conceal must stay concealed) — the ONLY inputs where the
/// bound above does not apply. Everything else delegates to the proven floor.
#[test]
fn wrapper_exemptions_are_exactly_off_and_conceal() {
    let (fg, bg) = (gray(120), gray(128));
    assert_eq!(
        floor_min_contrast_fg(fg, bg, 1.0),
        fg,
        "knob off is identity"
    );
    assert_eq!(
        floor_min_contrast_fg(bg, bg, 21.0),
        bg,
        "conceal stays concealed"
    );
    assert_eq!(
        floor_min_contrast_fg(fg, bg, 4.5),
        floor_fg_contrast(fg, bg, 4.5),
        "everything else is the proven floor"
    );
    // And a floored fg is genuinely different (the wrapper is not a no-op).
    assert_ne!(floor_min_contrast_fg(fg, bg, 4.5), fg);
}
