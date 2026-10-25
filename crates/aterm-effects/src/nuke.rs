// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Baked vector tiles and pure keyframe curves for the f-bomb nuke cloud.

use aterm_scene::{PathCmd, PathTransform, Tile, fill_path};

/// Independently baked and animated cloud layers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NukePart {
    /// The billowing head.
    Cap,
    /// The rising column.
    Stem,
    /// The ground-level base surge.
    Skirt,
}

/// Initial white flash end in milliseconds.
pub const FLASH_END_MS: u64 = 260;
/// Stem climb end in milliseconds.
pub const STEM_RISE_END_MS: u64 = 1100;
/// Cap bloom start in milliseconds.
pub const CAP_BLOOM_START_MS: u64 = 900;
pub const CAP_BLOOM_END_MS: u64 = 1900;
pub const SKIRT_START_MS: u64 = 1300;
pub const SKIRT_END_MS: u64 = 2400;
/// Total visible window in milliseconds.
pub const NUKE_TOTAL_MS: u64 = 3600;

const NUKE_A: f32 = 0.86;

const NUKE_CAP_DOME: [PathCmd; 7] = [
    PathCmd::Move(0.04, 0.62),
    PathCmd::Cubic(0.02, 0.30, 0.18, 0.05, 0.40, 0.06),
    PathCmd::Cubic(0.52, 0.00, 0.70, 0.02, 0.76, 0.14),
    PathCmd::Cubic(0.94, 0.16, 1.00, 0.38, 0.96, 0.62),
    PathCmd::Cubic(0.88, 0.78, 0.66, 0.86, 0.50, 0.80),
    PathCmd::Cubic(0.34, 0.88, 0.12, 0.80, 0.04, 0.62),
    PathCmd::Close,
];

const NUKE_CAP_CURL: [PathCmd; 5] = [
    PathCmd::Move(0.14, 0.66),
    PathCmd::Cubic(0.28, 0.92, 0.72, 0.92, 0.86, 0.66),
    PathCmd::Cubic(0.72, 0.80, 0.60, 0.84, 0.50, 0.83),
    PathCmd::Cubic(0.40, 0.84, 0.28, 0.80, 0.14, 0.66),
    PathCmd::Close,
];

const NUKE_STEM: [PathCmd; 5] = [
    PathCmd::Move(0.38, 0.00),
    PathCmd::Cubic(0.34, 0.30, 0.24, 0.62, 0.12, 1.00),
    PathCmd::Line(0.88, 1.00),
    PathCmd::Cubic(0.76, 0.62, 0.66, 0.30, 0.62, 0.00),
    PathCmd::Close,
];

const NUKE_SKIRT: [PathCmd; 4] = [
    PathCmd::Move(0.02, 0.62),
    PathCmd::Cubic(0.18, 0.06, 0.82, 0.06, 0.98, 0.62),
    PathCmd::Cubic(0.82, 1.00, 0.18, 1.00, 0.02, 0.62),
    PathCmd::Close,
];

const NUKE_HOST_SALT: u64 = 0xB0FF_1E5E_D0D0_CA57;

/// Cache key for one baked part at one size.
#[inline]
#[must_use]
pub const fn nuke_host_id(part: NukePart, w: u16, h: u16) -> u64 {
    let k = match part {
        NukePart::Cap => 1u64,
        NukePart::Stem => 2,
        NukePart::Skirt => 3,
    };
    let mut x = NUKE_HOST_SALT ^ (k << 32) ^ ((w as u64) << 16) ^ (h as u64);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Natural tile size for `part` at `(cell_w, cell_h)`, clamped to the baker's `4*cell_h x 2*cell_h` slot.
#[inline]
#[must_use]
pub const fn nuke_nat_size(part: NukePart, cell_w: u16, cell_h: u16) -> (u16, u16) {
    let cw = (if cell_w > 1 { cell_w } else { 1 }) as f32;
    let ch = (if cell_h > 1 { cell_h } else { 1 }) as f32;
    let (cols, rows) = match part {
        NukePart::Cap => (8.0, 2.00),
        NukePart::Stem => (2.6, 2.00),
        NukePart::Skirt => (10.0, 0.70),
    };
    let max_w = 4.0 * ch;
    let max_h = 2.0 * ch;
    let w = (cols * cw).round().clamp(4.0_f32.min(max_w), max_w) as u16;
    let h = (rows * ch).round().clamp(4.0_f32.min(max_h), max_h) as u16;
    (w, h)
}

/// Bake `part` at `w x h` in white for per-sprite tinting.
#[must_use]
pub fn bake_nuke(w: u16, h: u16, part: NukePart) -> Tile {
    let mut tile = Tile::new(u32::from(w), u32::from(h));
    paint_nuke(&mut tile, part);
    tile
}

/// Paint `part` onto a cleared `tile` at its current dimensions in white for
/// per-sprite tinting.
pub fn paint_nuke(tile: &mut Tile, part: NukePart) {
    let (w, h) = (tile.width(), tile.height());
    if w == 0 || h == 0 {
        return;
    }
    let white = (1.0, 1.0, 1.0);
    let fit = PathTransform::fit(w, h);
    let (wf, hf) = (w as f32, h as f32);
    match part {
        NukePart::Cap => {
            fill_path(tile, &[&NUKE_CAP_DOME], white, NUKE_A, fit);
            fill_path(tile, &[&NUKE_CAP_CURL], white, NUKE_A * 0.66, fit);
            tile.disc(
                0.26 * wf,
                0.30 * hf,
                (0.15 * hf).max(1.5),
                white,
                NUKE_A * 0.90,
            );
            tile.disc(
                0.52 * wf,
                0.18 * hf,
                (0.17 * hf).max(1.5),
                white,
                NUKE_A * 0.90,
            );
            tile.disc(
                0.76 * wf,
                0.32 * hf,
                (0.14 * hf).max(1.5),
                white,
                NUKE_A * 0.90,
            );
            tile.disc(0.40 * wf, 0.12 * hf, (0.11 * hf).max(1.0), white, 1.0);
            tile.disc(0.62 * wf, 0.14 * hf, (0.09 * hf).max(1.0), white, 1.0);
            for &(sx, sy, sr, sa) in &[
                (0.035f32, 0.44f32, 0.045f32, 0.38f32),
                (0.075, 0.28, 0.035, 0.30),
                (0.955, 0.42, 0.045, 0.38),
                (0.925, 0.26, 0.035, 0.30),
                (0.30, 0.02, 0.030, 0.34),
                (0.72, 0.03, 0.030, 0.34),
            ] {
                tile.disc(
                    sx * wf,
                    sy * hf,
                    (sr * hf.max(wf * 0.4)).max(1.0),
                    white,
                    NUKE_A * sa,
                );
            }
        }
        NukePart::Stem => {
            fill_path(tile, &[&NUKE_STEM], white, NUKE_A, fit);
            tile.ellipse(
                0.50 * wf,
                0.94 * hf,
                0.46 * wf,
                (0.08 * hf).max(1.0),
                white,
                NUKE_A * 0.70,
            );
            let ramp_top = (0.70 * hf) as u32;
            let inv_span = 1.0 / (h.saturating_sub(ramp_top)).max(1) as f32;
            let row_bytes = w as usize * 4;
            let px = tile.pixels_mut();
            for y in ramp_top..h {
                let scale = 1.0 - 0.58 * ((y - ramp_top) as f32 * inv_span);
                let row_start = y as usize * row_bytes;
                for p in px[row_start..row_start + row_bytes].as_chunks_mut::<4>().0 {
                    if p[3] != 0 {
                        p[3] = (f32::from(p[3]) * scale) as u8;
                    }
                }
            }
        }
        NukePart::Skirt => {
            fill_path(tile, &[&NUKE_SKIRT], white, NUKE_A * 0.62, fit);
        }
    }
}

/// One part's draw state at `t_ms`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NukeDraw {
    pub sx: f32,
    pub sy: f32,
    pub dy_cells: f32,
    pub alpha: f32,
    pub tint: u32,
}

const CAP_HOT: u32 = 0x00FF_A94A;
const CAP_DUST: u32 = 0x008F_7668;
const STEM_HOT: u32 = 0x00FF_C070;
const STEM_DUST: u32 = 0x00B0_8D74;
const SKIRT_TONE: u32 = 0x00C8_B49C;
const CORE_HOT: u32 = 0x00FF_E9B0;

const INV_COOL_SPAN_MS: f32 = 1.0 / 1600.0;
const INV_STEM_RISE_SPAN_MS: f32 = 1.0 / (STEM_RISE_END_MS - FLASH_END_MS) as f32;
const INV_TAIL_SPAN_MS: f32 = 1.0 / 1700.0;
const INV_CAP_BLOOM_SPAN_MS: f32 = 1.0 / (CAP_BLOOM_END_MS - CAP_BLOOM_START_MS) as f32;
const INV_SKIRT_SPAN_MS: f32 = 1.0 / (SKIRT_END_MS - SKIRT_START_MS) as f32;
const INV_FADE_SPAN_MS: f32 = 1.0 / (NUKE_TOTAL_MS - SKIRT_END_MS) as f32;

/// Heat -> dust, on one clock for the whole cloud. Cooling starts once the bloom is well
/// underway, so the cap first appears fire-lit.
#[inline]
const fn cool(t_ms: u64) -> f32 {
    aterm_scene::smoothstep((t_ms.saturating_sub(1500) as f32 * INV_COOL_SPAN_MS).clamp(0.0, 1.0))
}

#[inline]
const fn fade_after_skirt(t_ms: u64) -> f32 {
    if t_ms <= SKIRT_END_MS {
        1.0
    } else {
        1.0 - ((t_ms - SKIRT_END_MS) as f32 * INV_FADE_SPAN_MS).clamp(0.0, 1.0)
    }
}

/// Resolve `part`'s draw state at `t_ms`, or `None` outside its active window.
#[inline]
#[must_use]
pub const fn nuke_draw(t_ms: u64, part: NukePart) -> Option<NukeDraw> {
    match part {
        NukePart::Stem => {
            if t_ms < FLASH_END_MS || t_ms >= NUKE_TOTAL_MS {
                return None;
            }
            let c = cool(t_ms);
            let q = ((t_ms - FLASH_END_MS) as f32 * INV_STEM_RISE_SPAN_MS).clamp(0.0, 1.0);
            let omq = 1.0 - q;
            let rise = 1.0 - omq * omq * omq;
            Some(NukeDraw {
                sx: 0.85 + 0.35 * rise,
                sy: 0.15 + 0.85 * rise,
                dy_cells: -0.35 * (t_ms.saturating_sub(CAP_BLOOM_END_MS) as f32 * INV_TAIL_SPAN_MS),
                alpha: (q * 3.0).min(1.0) * fade_after_skirt(t_ms),
                tint: crate::effect_util::lerp_rgb(STEM_HOT, STEM_DUST, c),
            })
        }
        NukePart::Cap => {
            if t_ms < CAP_BLOOM_START_MS || t_ms >= NUKE_TOTAL_MS {
                return None;
            }
            let c = cool(t_ms);
            let q = ((t_ms - CAP_BLOOM_START_MS) as f32 * INV_CAP_BLOOM_SPAN_MS).clamp(0.0, 1.0);
            let omq = 1.0 - q;
            let (sin_q, _) = crate::supernova::sin_cos_f32(core::f32::consts::PI * q);
            let bloom = (1.0 - omq * omq * omq) + 0.06 * sin_q;
            let tail =
                (t_ms.saturating_sub(CAP_BLOOM_END_MS) as f32 * INV_TAIL_SPAN_MS).clamp(0.0, 1.0);
            let spread = 1.0 + 0.55 * tail;
            Some(NukeDraw {
                sx: (0.45 + 0.75 * bloom) * spread,
                sy: (0.50 + 0.62 * bloom) * (1.0 + 0.15 * (spread - 1.0)),
                dy_cells: -(0.90 + 0.60 * q) - 0.70 * tail,
                alpha: (q * 4.0).min(1.0) * fade_after_skirt(t_ms),
                tint: crate::effect_util::lerp_rgb(CAP_HOT, CAP_DUST, c),
            })
        }
        NukePart::Skirt => {
            if t_ms < SKIRT_START_MS || t_ms >= SKIRT_END_MS {
                return None;
            }
            let q = ((t_ms - SKIRT_START_MS) as f32 * INV_SKIRT_SPAN_MS).clamp(0.0, 1.0);
            let (sin_q, _) = crate::supernova::sin_cos_f32(core::f32::consts::PI * q);
            Some(NukeDraw {
                sx: 0.60 + 2.60 * q,
                sy: 1.0,
                dy_cells: 0.10 * q,
                alpha: 0.70 * sin_q.max(0.0),
                tint: SKIRT_TONE,
            })
        }
    }
}

/// Molten core sprite drawn over the cap tile at `t_ms`.
#[inline]
#[must_use]
pub const fn nuke_core_draw(t_ms: u64) -> Option<NukeDraw> {
    const CORE_END_MS: u64 = 2_900;
    const INV_CORE_DIE_SPAN_MS: f32 = 1.0 / (CORE_END_MS - CAP_BLOOM_END_MS) as f32;
    if t_ms < CAP_BLOOM_START_MS || t_ms >= CORE_END_MS {
        return None;
    }
    let Some(cap) = nuke_draw(t_ms, NukePart::Cap) else {
        return None;
    };
    let q = ((t_ms - CAP_BLOOM_START_MS) as f32 * INV_CAP_BLOOM_SPAN_MS).clamp(0.0, 1.0);
    let die = 1.0
        - ((t_ms.saturating_sub(CAP_BLOOM_END_MS)) as f32 * INV_CORE_DIE_SPAN_MS).clamp(0.0, 1.0);
    Some(NukeDraw {
        sx: cap.sx * 0.58,
        sy: cap.sy * 0.52,
        dy_cells: cap.dy_cells,
        alpha: (q * 4.0).min(1.0) * die * 0.92,
        tint: crate::effect_util::lerp_rgb(CORE_HOT, CAP_HOT, 0.35 + 0.45 * q),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARTS: [NukePart; 3] = [NukePart::Cap, NukePart::Stem, NukePart::Skirt];

    #[test]
    fn natural_sizes_stay_inside_the_baker_slot() {
        for (cw, ch) in [(7u16, 14u16), (14, 32), (8, 16), (30, 20), (40, 12)] {
            for part in PARTS {
                let (w, h) = nuke_nat_size(part, cw, ch);
                assert!(
                    u32::from(w) <= 4 * u32::from(ch) && u32::from(h) <= 2 * u32::from(ch),
                    "{part:?} at cell {cw}x{ch} baked {w}x{h}, outside the slot"
                );
                assert!(w >= 4 && h >= 4, "{part:?} degenerate at cell {cw}x{ch}");
            }
        }
        for (cw, ch) in [(0u16, 0u16), (1, 1)] {
            for part in PARTS {
                let (w, h) = nuke_nat_size(part, cw, ch);
                let eff_h = u32::from(ch.max(1));
                assert!(u32::from(w) <= 4 * eff_h && u32::from(h) <= 2 * eff_h);
                assert!(w >= 1 && h >= 1);
            }
        }
    }

    #[test]
    fn parts_live_only_in_their_phase() {
        assert!(
            nuke_draw(0, NukePart::Stem).is_none(),
            "stem before the flash"
        );
        assert!(
            nuke_draw(0, NukePart::Cap).is_none(),
            "cap before the flash"
        );
        assert!(
            nuke_draw(FLASH_END_MS, NukePart::Stem).is_some(),
            "the stem must rise once the flash ends"
        );
        assert!(
            nuke_draw(CAP_BLOOM_START_MS, NukePart::Cap).is_some(),
            "the cap must bloom at its edge"
        );
        const {
            assert!(CAP_BLOOM_START_MS < STEM_RISE_END_MS);
        }
        for part in PARTS {
            assert!(
                nuke_draw(NUKE_TOTAL_MS, part).is_none(),
                "{part:?} outlived the detonation"
            );
        }
        assert!(nuke_core_draw(CAP_BLOOM_START_MS - 1).is_none());
        assert!(nuke_core_draw(CAP_BLOOM_START_MS).is_some());
        assert!(
            nuke_core_draw(NUKE_TOTAL_MS - 1).is_none(),
            "core must die before the dust"
        );
        let core = nuke_core_draw(1_400).expect("core mid-bloom");
        let cap = nuke_draw(1_400, NukePart::Cap).expect("cap mid-bloom");
        assert!(
            core.sx < cap.sx && core.sy < cap.sy,
            "the core sits INSIDE the cap"
        );
        assert_ne!(core.tint, cap.tint, "the core must be hotter than the dust");
        for t in (0..NUKE_TOTAL_MS + 200).step_by(10) {
            if let Some(d) = nuke_core_draw(t) {
                assert!(d.sx.is_finite() && d.sy.is_finite() && d.dy_cells.is_finite());
                assert!(
                    (0.0..=1.0).contains(&d.alpha),
                    "core alpha {} at {t}ms",
                    d.alpha
                );
                assert!(d.sx > 0.0 && d.sy > 0.0);
            }
        }
    }

    #[test]
    fn the_whole_window_is_finite_and_cools() {
        let hot = nuke_draw(CAP_BLOOM_START_MS, NukePart::Cap).expect("cap at bloom");
        let cold = nuke_draw(NUKE_TOTAL_MS - 1, NukePart::Cap).expect("cap at the end");
        assert_ne!(hot.tint, cold.tint, "the cap never cooled");

        for t in (0..NUKE_TOTAL_MS + 200).step_by(10) {
            for part in PARTS {
                let Some(d) = nuke_draw(t, part) else {
                    continue;
                };
                assert!(
                    d.sx.is_finite() && d.sy.is_finite() && d.dy_cells.is_finite(),
                    "{part:?} at {t}ms produced a non-finite transform"
                );
                assert!(
                    (0.0..=1.0).contains(&d.alpha),
                    "{part:?} at {t}ms had alpha {} outside 0..=1",
                    d.alpha
                );
                assert!(
                    d.sx > 0.0 && d.sy > 0.0,
                    "{part:?} at {t}ms scaled to nothing"
                );
            }
        }
    }
}
