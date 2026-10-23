// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Conformance for [`aterm_render::floor_fg_contrast`] against an independent WCAG oracle
//! (`contrast(floor_fg_contrast(fg, bg, r), bg) >= min(r, max_achievable(bg))`).

use aterm_render::{floor_fg_contrast, floor_min_contrast_fg};

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

fn max_achievable(bg: u32) -> f32 {
    contrast(0x0000_0000, bg).max(contrast(0x00ff_ffff, bg))
}

fn gray(v: u32) -> u32 {
    (v << 16) | (v << 8) | v
}

const EPS: f32 = 1e-3;

const RATIOS: [f32; 7] = [1.0, 1.5, 3.0, 4.5, 7.0, 10.0, 21.0];

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
    assert_ne!(floor_min_contrast_fg(fg, bg, 4.5), fg);
}
