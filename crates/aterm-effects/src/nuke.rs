// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The nuke cloud: the rarest tier of the f-bomb detonation ladder.
//!
//! Three static baked tiles (cap, stem, base surge) animated only by dest-rect transform, alpha
//! and tint: `aw/ah` stay the natural source size and the renderer nearest-scales a fixed tile.
//! Tiles are cache-stable per `(part, w, h)`, so an animating cloud never rebakes after warmup;
//! the cloud holds three of the 32 atlas slots.
//!
//! The art is const [`PathCmd`] drawlists filled in white with [`fill_path`] and tinted per
//! sprite. It is a pure function of `(part, w, h)` and the animation a pure function of `t_ms`
//! (no clock, no RNG, no per-frame allocation), which the CPU/GPU parity suite relies on.

use aterm_scene::{PathCmd, PathTransform, Tile, fill_path};

/// The three pieces of the cloud, baked and animated independently so the stem
/// can rise before the cap blooms and the base surge can roll out under both.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NukePart {
    /// The billowing head.
    Cap,
    /// The rising column.
    Stem,
    /// The ground-level base surge.
    Skirt,
}

// Phase edges in ms. The host adds the shared ember fade after `NUKE_TOTAL_MS`.
/// The initial white flash owns the screen before the column is visible.
pub const FLASH_END_MS: u64 = 260;
/// The stem finishes climbing here…
pub const STEM_RISE_END_MS: u64 = 1100;
/// …and the cap starts blooming slightly before it does, so the head reads as
/// pushed up by the column rather than appearing on it.
pub const CAP_BLOOM_START_MS: u64 = 900;
pub const CAP_BLOOM_END_MS: u64 = 1900;
pub const SKIRT_START_MS: u64 = 1300;
pub const SKIRT_END_MS: u64 = 2400;
/// Total visible window.
pub const NUKE_TOTAL_MS: u64 = 3600;

/// Peak ink alpha of the baked art, below 1 so the line under the cloud stays readable.
const NUKE_A: f32 = 0.86;

/// The cap's hull, in the 0..1 glyph frame (y down). `bake_nuke` adds lobe discs on top so the
/// silhouette stays round at small cell sizes.
const NUKE_CAP_DOME: [PathCmd; 7] = [
    PathCmd::Move(0.04, 0.62),
    PathCmd::Cubic(0.02, 0.30, 0.18, 0.05, 0.40, 0.06),
    PathCmd::Cubic(0.52, 0.00, 0.70, 0.02, 0.76, 0.14),
    PathCmd::Cubic(0.94, 0.16, 1.00, 0.38, 0.96, 0.62),
    PathCmd::Cubic(0.88, 0.78, 0.66, 0.86, 0.50, 0.80),
    PathCmd::Cubic(0.34, 0.88, 0.12, 0.80, 0.04, 0.62),
    PathCmd::Close,
];

/// The torus roll under the cap, which makes it read as a mushroom.
const NUKE_CAP_CURL: [PathCmd; 5] = [
    PathCmd::Move(0.14, 0.66),
    PathCmd::Cubic(0.28, 0.92, 0.72, 0.92, 0.86, 0.66),
    PathCmd::Cubic(0.72, 0.80, 0.60, 0.84, 0.50, 0.83),
    PathCmd::Cubic(0.40, 0.84, 0.28, 0.80, 0.14, 0.66),
    PathCmd::Close,
];

/// The column: narrow at the cap, flaring to the ground, with a slight S lean
/// so it never reads as a drawn rectangle.
const NUKE_STEM: [PathCmd; 5] = [
    PathCmd::Move(0.38, 0.00),
    PathCmd::Cubic(0.34, 0.30, 0.24, 0.62, 0.12, 1.00),
    PathCmd::Line(0.88, 1.00),
    PathCmd::Cubic(0.76, 0.62, 0.66, 0.30, 0.62, 0.00),
    PathCmd::Close,
];

/// The base surge: a flat dust lens that rolls outward.
const NUKE_SKIRT: [PathCmd; 4] = [
    PathCmd::Move(0.02, 0.62),
    PathCmd::Cubic(0.18, 0.06, 0.82, 0.06, 0.98, 0.62),
    PathCmd::Cubic(0.82, 1.00, 0.18, 1.00, 0.02, 0.62),
    PathCmd::Close,
];

/// Salt that keeps the nuke tiles' host ids apart from every other baked sprite's.
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

/// Natural tile size for a cell. Width follows `cell_w` (the cloud is measured in columns) but
/// is clamped to the baker's slot (`4 * cell_h` wide, `2 * cell_h` tall), which
/// `CatBaker::host_tile` refuses to exceed.
#[inline]
#[must_use]
pub fn nuke_nat_size(part: NukePart, cell_w: u16, cell_h: u16) -> (u16, u16) {
    let (cw, ch) = (f32::from(cell_w.max(1)), f32::from(cell_h.max(1)));
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

/// Bake one cloud part at its natural `w x h`, in white (tinted per sprite). Byte-identical
/// across bakes of the same `(part, w, h)`.
#[must_use]
pub fn bake_nuke(w: u16, h: u16, part: NukePart) -> Tile {
    let mut tile = Tile::new(u32::from(w), u32::from(h));
    if w == 0 || h == 0 {
        return tile;
    }
    let white = (1.0, 1.0, 1.0);
    let fit = PathTransform::fit(u32::from(w), u32::from(h));
    let (wf, hf) = (f32::from(w), f32::from(h));
    match part {
        NukePart::Cap => {
            fill_path(&mut tile, &[&NUKE_CAP_DOME], white, NUKE_A, fit);
            // The torus at a lower alpha than the dome gives the head volume.
            fill_path(&mut tile, &[&NUKE_CAP_CURL], white, NUKE_A * 0.66, fit);
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
            // Crown highlights: a single-tint sprite can shade only through
            // alpha, so brighter lobes along the top keep the head from
            // reading flat.
            tile.disc(0.40 * wf, 0.12 * hf, (0.11 * hf).max(1.0), white, 1.0);
            tile.disc(0.62 * wf, 0.14 * hf, (0.09 * hf).max(1.0), white, 1.0);
            // Dust flecks just outside the shoulders, at fixed offsets.
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
            fill_path(&mut tile, &[&NUKE_STEM], white, NUKE_A, fit);
            tile.ellipse(
                0.50 * wf,
                0.94 * hf,
                0.46 * wf,
                (0.08 * hf).max(1.0),
                white,
                NUKE_A * 0.70,
            );
            // The bottom 30% ramps translucent so the word the column stands
            // on ghosts through, dimmed, and returns untouched when it ends.
            let ramp_top = (0.70 * hf) as u32;
            let span = (u32::from(h).saturating_sub(ramp_top)).max(1) as f32;
            let row_bytes = usize::from(w) * 4;
            let px = tile.pixels_mut();
            for y in ramp_top..u32::from(h) {
                let scale = 1.0 - 0.58 * ((y - ramp_top) as f32 / span);
                let row_start = y as usize * row_bytes;
                for p in px[row_start..row_start + row_bytes].as_chunks_mut::<4>().0 {
                    if p[3] != 0 {
                        p[3] = (f32::from(p[3]) * scale) as u8;
                    }
                }
            }
        }
        NukePart::Skirt => {
            fill_path(&mut tile, &[&NUKE_SKIRT], white, NUKE_A * 0.62, fit);
        }
    }
    tile
}

/// One part's draw state at `t_ms`: dest scale about the anchor, a vertical offset in cells,
/// alpha, and the cooled tint.
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
/// The molten heart under the cap.
const CORE_HOT: u32 = 0x00FF_E9B0;

/// Heat -> dust, on one clock for the whole cloud. Cooling starts once the bloom is well
/// underway, so the cap first appears fire-lit.
#[inline]
const fn cool(t_ms: u64) -> f32 {
    aterm_scene::smoothstep((t_ms.saturating_sub(1500) as f32 / 1600.0).clamp(0.0, 1.0))
}

/// Resolve one part at `t_ms`, or `None` when that part is not on screen yet
/// (or any more). Pure: same input, same bytes.
#[inline]
#[must_use]
pub fn nuke_draw(t_ms: u64, part: NukePart) -> Option<NukeDraw> {
    let fade = |from: u64, span: u64| -> f32 {
        if t_ms <= from || span == 0 {
            1.0
        } else {
            1.0 - ((t_ms - from) as f32 / span as f32).clamp(0.0, 1.0)
        }
    };
    match part {
        NukePart::Stem => {
            if !(FLASH_END_MS..NUKE_TOTAL_MS).contains(&t_ms) {
                return None;
            }
            let c = cool(t_ms);
            let q = ((t_ms - FLASH_END_MS) as f32 / (STEM_RISE_END_MS - FLASH_END_MS) as f32)
                .clamp(0.0, 1.0);
            // Ease-out cubic: the column leaves the ground fast and settles.
            let rise = 1.0 - (1.0 - q) * (1.0 - q) * (1.0 - q);
            Some(NukeDraw {
                sx: 0.85 + 0.35 * rise,
                sy: 0.15 + 0.85 * rise,
                dy_cells: -0.35 * (t_ms.saturating_sub(CAP_BLOOM_END_MS) as f32 / 1700.0),
                alpha: (q * 3.0).min(1.0) * fade(SKIRT_END_MS, NUKE_TOTAL_MS - SKIRT_END_MS),
                tint: aterm_scene::mix_rgb(STEM_HOT, STEM_DUST, c),
            })
        }
        NukePart::Cap => {
            if !(CAP_BLOOM_START_MS..NUKE_TOTAL_MS).contains(&t_ms) {
                return None;
            }
            let c = cool(t_ms);
            let q = ((t_ms - CAP_BLOOM_START_MS) as f32
                / (CAP_BLOOM_END_MS - CAP_BLOOM_START_MS) as f32)
                .clamp(0.0, 1.0);
            // Bloom with a ~3% overshoot near q = 0.78, then a slow spread:
            // the head keeps growing after it stops rising.
            let bloom = (1.0 - (1.0 - q) * (1.0 - q) * (1.0 - q))
                + 0.06 * (core::f32::consts::PI * q).sin();
            let tail = (t_ms.saturating_sub(CAP_BLOOM_END_MS) as f32 / 1700.0).clamp(0.0, 1.0);
            let spread = 1.0 + 0.55 * tail;
            Some(NukeDraw {
                sx: (0.45 + 0.75 * bloom) * spread,
                sy: (0.50 + 0.62 * bloom) * (1.0 + 0.15 * (spread - 1.0)),
                // Rides the top of the column (anchored at the baseline it would
                // swallow the stem), then climbs through the dust tail.
                dy_cells: -(0.90 + 0.60 * q) - 0.70 * tail,
                alpha: (q * 4.0).min(1.0) * fade(SKIRT_END_MS, NUKE_TOTAL_MS - SKIRT_END_MS),
                tint: aterm_scene::mix_rgb(CAP_HOT, CAP_DUST, c),
            })
        }
        NukePart::Skirt => {
            if !(SKIRT_START_MS..SKIRT_END_MS).contains(&t_ms) {
                return None;
            }
            let q = ((t_ms - SKIRT_START_MS) as f32 / (SKIRT_END_MS - SKIRT_START_MS) as f32)
                .clamp(0.0, 1.0);
            Some(NukeDraw {
                sx: 0.60 + 2.60 * q,
                sy: 1.0,
                dy_cells: 0.10 * q,
                alpha: 0.70 * (core::f32::consts::PI * q).sin().max(0.0),
                tint: SKIRT_TONE,
            })
        }
    }
}

/// The molten core: the baked cap tile drawn again, smaller and hotter, at the foot of the head.
/// One tint per sprite, so hue contrast inside the cloud takes a second sprite on the same tile.
/// It dies while the dust cap is still spreading.
#[inline]
#[must_use]
pub fn nuke_core_draw(t_ms: u64) -> Option<NukeDraw> {
    const CORE_END_MS: u64 = 2_900;
    if !(CAP_BLOOM_START_MS..CORE_END_MS).contains(&t_ms) {
        return None;
    }
    let cap = nuke_draw(t_ms, NukePart::Cap)?;
    let q = ((t_ms - CAP_BLOOM_START_MS) as f32 / (CAP_BLOOM_END_MS - CAP_BLOOM_START_MS) as f32)
        .clamp(0.0, 1.0);
    let die = 1.0
        - ((t_ms.saturating_sub(CAP_BLOOM_END_MS)) as f32
            / (CORE_END_MS - CAP_BLOOM_END_MS) as f32)
            .clamp(0.0, 1.0);
    Some(NukeDraw {
        sx: cap.sx * 0.58,
        sy: cap.sy * 0.52,
        dy_cells: cap.dy_cells,
        alpha: (q * 4.0).min(1.0) * die * 0.92,
        // Warm cream toward orange, never near-white: at small cell sizes a
        // white core is indistinguishable from the block cursor.
        tint: aterm_scene::mix_rgb(CORE_HOT, CAP_HOT, 0.35 + 0.45 * q),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARTS: [NukePart; 3] = [NukePart::Cap, NukePart::Stem, NukePart::Skirt];

    /// The natural tile stays within `CatBaker::host_tile`'s bounds
    /// (`4 * cell_h` by `2 * cell_h`) for every plausible cell geometry.
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
        // Degenerate 0x0 and 1x1 cells clamp into the 1-px slot ceiling without panicking.
        for (cw, ch) in [(0u16, 0u16), (1, 1)] {
            for part in PARTS {
                let (w, h) = nuke_nat_size(part, cw, ch);
                let eff_h = u32::from(ch.max(1));
                assert!(u32::from(w) <= 4 * eff_h && u32::from(h) <= 2 * eff_h);
                assert!(w >= 1 && h >= 1);
            }
        }
    }

    /// Each part is on screen for exactly its own window, and the whole cloud
    /// is gone by `NUKE_TOTAL_MS`.
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
        // The cap starts blooming before the stem finishes rising.
        const {
            assert!(CAP_BLOOM_START_MS < STEM_RISE_END_MS);
        }
        for part in PARTS {
            assert!(
                nuke_draw(NUKE_TOTAL_MS, part).is_none(),
                "{part:?} outlived the detonation"
            );
        }
        // The molten core lives inside the cap's window and dies before the dust.
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

    /// The tint changes from hot to dust, and every part stays finite and
    /// within its alpha range for every frame of the window.
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
