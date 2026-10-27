// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The strain gauge's steam: wisps rising off a hot dial on the live edge's frame grid
//! (`docs/DESIGN-stats-deck-owner-2026-10-26.md` §5).

use aterm_vitals::ink::{self, Rgb, VitalsInks};

use crate::stats_gauge::{Place, R_FACE};
use crate::widget::{DrawPrim, rgba};

/// The needle reading above which the dial steams: where its hot bloom, the orange glow, starts.
pub(crate) const STEAM_FROM: f32 = crate::stats_gauge::HOT_BLOOM_FROM;
/// The needle reading from which the steam is full, halfway into the redline.
const STEAM_FULL: f32 = 1.5;
/// Where the wisps rise from, degrees clockwise from 12 o'clock, the first to come first as the
/// strain grows: left of 12 o'clock first, away from a hot needle, which points right of 29°.
const SOURCES: [f64; 4] = [-10.0, 26.0, -40.0, 50.0];
/// Each wisp's own offset into its rise, so no two puff in step.
const SEEDS: [f64; 4] = [0.0, 0.41, 0.73, 0.19];
/// The puffs along one wisp, evenly spaced in its rise.
const PUFFS: u32 = 6;
/// How long a puff takes to rise its wisp, ms, whatever the strain: a change of strain from one
/// reading to the next then never jumps a puff along its rise.
const RISE_MS: i64 = 5_200;
/// Where a puff leaves, a share of the face's radius out from the hub: over the face, so it rises
/// across the scale and the bezel, and a Folded dial, whose slot leaves little room above it,
/// still shows it.
const R_START: f64 = 0.42;
/// A puff's radius as it leaves and as it fades at the top, dial pt.
const PUFF_R: (f64, f64) = (1.6, 5.6);
/// How far a puff sways either side of its wisp at the top, dial pt.
const SWAY_PT: f64 = 2.4;
/// How far a wisp leans away from 12 o'clock over its rise, a share of the rise at its source's
/// sine.
const LEAN: f64 = 0.35;
/// A puff's opacity at its fullest on a dark ground: at the first steam, and at full.
const ALPHA: (f32, f32) = (0.14, 0.36);
/// How much more opaque the white steam is on a light ground, where only the dial's bezel and
/// scale show it.
const LIGHT_GAIN: f32 = 1.4;
/// The share of its rise over which a puff fades in.
const FADE_IN: f64 = 0.22;
/// How far below the slot's top a puff fades out, pt, so the slot never cuts one; a puff's rise
/// ends half of it below the top.
const TOP_FADE_PT: f64 = 4.0;

/// How hard a dial whose needle reads `needle` (`0..=2`) steams, `0..=1`: 0 at or below
/// [`STEAM_FROM`], growing with the strain to 1 at [`STEAM_FULL`].
pub(crate) fn strength(needle: Option<f32>) -> f32 {
    let Some(v) = needle.filter(|v| v.is_finite()) else {
        return 0.0;
    };
    if v <= STEAM_FROM {
        return 0.0;
    }
    ((v - STEAM_FROM) / (STEAM_FULL - STEAM_FROM)).clamp(0.01, 1.0)
}

/// The steam's ink on `inks`' ground and its opacity's gain: a near-white vapour on a dark ground,
/// white and a little denser on a light one.
pub(crate) fn ink(inks: &VitalsInks) -> (Rgb, f32) {
    if inks.dark {
        (ink::mix(inks.words, [0xFF; 3], 0.5), 1.0)
    } else {
        ([0xFF; 3], LIGHT_GAIN)
    }
}

/// Draws the steam of a dial steaming at `k` ([`strength`]) at `t_ms` on the steam's clock,
/// over the dial `at` places, inside its slot, in `steam` ([`ink`]), into `out`; nothing at
/// `k = 0`.
pub(crate) fn draw(
    k: f32,
    t_ms: i64,
    at: Place,
    (steam, gain): (Rgb, f32),
    out: &mut Vec<DrawPrim>,
) {
    if k.is_nan() || k <= 0.0 {
        return;
    }
    let k = k.min(1.0);
    let kk = f64::from(k);
    // Device px per dial pt: a Folded dial is the same drawing, smaller.
    let sc = at.s * at.r / R_FACE;
    let (cx, cy) = (f64::from(at.centre.0), f64::from(at.centre.1));
    let [x0, y0, x1, y1] = at.slot;
    out.push(DrawPrim::ClipPush {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    });
    let wisps = (1 + (3.0 * kk).floor() as usize).min(SOURCES.len());
    let peak = f64::from((ALPHA.0 + (ALPHA.1 - ALPHA.0) * k) * gain);
    let t = t_ms.rem_euclid(RISE_MS) as f64 / RISE_MS as f64;
    let top = f64::from(y0) + TOP_FADE_PT * at.s / 2.0;
    let fade_px = (TOP_FADE_PT * at.s).max(1.0);
    for (&theta, &seed) in SOURCES.iter().zip(&SEEDS).take(wisps) {
        let rad = theta.to_radians();
        let (bx, by) = (rad.sin() * R_FACE * R_START, -rad.cos() * R_FACE * R_START);
        let start_y = cy + by * sc;
        let rise = (start_y - top).max(1.0);
        let lean = rad.sin() * LEAN;
        for j in 0..PUFFS {
            let a = (t + seed + f64::from(j) / f64::from(PUFFS)).rem_euclid(1.0);
            let fade = if a < FADE_IN {
                a / FADE_IN
            } else {
                (1.0 - a) / (1.0 - FADE_IN)
            };
            let sway = SWAY_PT * a * (std::f64::consts::TAU * (1.1 * a + seed)).sin();
            let x = cx + (bx + sway) * sc + lean * rise * a;
            let y = start_y - rise * a;
            let near_top = ((y - f64::from(y0)) / fade_px).clamp(0.0, 1.0);
            // Two concentric dots of the same opacity: a soft puff, denser at its heart.
            let alpha = (peak * fade.clamp(0.0, 1.0) * near_top * 0.6 * 255.0).round();
            if alpha < 1.0 {
                continue;
            }
            let color = rgba(steam, alpha.min(255.0) as u8);
            let r = (PUFF_R.0 + (PUFF_R.1 - PUFF_R.0) * a) * sc;
            for r in [r, r * 0.6] {
                out.push(DrawPrim::Dot {
                    cx: x as f32,
                    cy: y as f32,
                    r: r as f32,
                    color,
                });
            }
        }
    }
    out.push(DrawPrim::ClipPop);
}
