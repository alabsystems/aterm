// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE NUKE CLOUD — the rarest tier of the f-bomb detonation ladder (owner,
//! 2026-07-24: "3 degrees of f-bomb detonations … and one of the f-bombs should
//! be a nuke-cloud").
//!
//! Three STATIC baked tiles (cap / stem / base surge) animated purely by
//! dest-rect transform, alpha and tint — exactly the Nyan cursor's pose trick,
//! where `aw/ah` stay the natural source size and the renderer NEAREST-scales a
//! fixed tile. The tiles are cache-stable per `(part, w, h)`, so an animating
//! cloud NEVER rebakes and never spends the shared two-bakes-per-frame budget
//! after warmup. Three atlas slots out of thirty-two.
//!
//! Authoring lane is the proven one: const [`PathCmd`] drawlists in the 0..1
//! frame → [`fill_path`] at WHITE → a per-sprite tint. That keeps the art a
//! pure function of `(part, w, h)` and the animation a pure function of
//! `t_ms`, which is what the CPU/GPU parity suite depends on: no clock, no RNG,
//! no per-frame allocation.

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
/// …and the cap starts blooming slightly BEFORE it does, so the two overlap and
/// the head reads as being pushed up by the column rather than appearing on it.
pub const CAP_BLOOM_START_MS: u64 = 900;
pub const CAP_BLOOM_END_MS: u64 = 1900;
pub const SKIRT_START_MS: u64 = 1300;
pub const SKIRT_END_MS: u64 = 2400;
/// Total visible window.
pub const NUKE_TOTAL_MS: u64 = 3600;

/// Peak ink alpha of the baked art. The cloud floats OVER live text, so the
/// ART carries the legibility budget — the same reasoning as the over-ink
/// coverage cap in `cursor_glow`: a detonation may be spectacular, but the line
/// underneath it still has to be readable a moment later.
const NUKE_A: f32 = 0.86;

/// The billowing cap, authored in the 0..1 glyph frame (y down). Lobes are
/// discs drawn on top so the silhouette stays round at small cell sizes, where
/// the Bézier hull alone would flatten into a blob.
const NUKE_CAP_DOME: [PathCmd; 7] = [
    PathCmd::Move(0.04, 0.62),
    PathCmd::Cubic(0.02, 0.30, 0.18, 0.05, 0.40, 0.06),
    PathCmd::Cubic(0.52, 0.00, 0.70, 0.02, 0.76, 0.14),
    PathCmd::Cubic(0.94, 0.16, 1.00, 0.38, 0.96, 0.62),
    PathCmd::Cubic(0.88, 0.78, 0.66, 0.86, 0.50, 0.80),
    PathCmd::Cubic(0.34, 0.88, 0.12, 0.80, 0.04, 0.62),
    PathCmd::Close,
];

/// The torus roll UNDER the cap — the read that makes it a mushroom and not
/// just a cloud.
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

/// The ground-level base surge — a flat dust lens that rolls outward.
const NUKE_SKIRT: [PathCmd; 4] = [
    PathCmd::Move(0.02, 0.62),
    PathCmd::Cubic(0.18, 0.06, 0.82, 0.06, 0.98, 0.62),
    PathCmd::Cubic(0.82, 1.00, 0.18, 1.00, 0.02, 0.62),
    PathCmd::Close,
];

/// Salt for the nuke tiles' host-id space — its own family, scrambled away from
/// every other baked-sprite id by the splitmix finalizer.
const NUKE_HOST_SALT: u64 = 0xB0FF_1E5E_D0D0_CA57;

/// Cache key for one baked part at one size.
#[must_use]
pub fn nuke_host_id(part: NukePart, w: u16, h: u16) -> u64 {
    let k = match part {
        NukePart::Cap => 1u64,
        NukePart::Stem => 2,
        NukePart::Skirt => 3,
    };
    let mut x = NUKE_HOST_SALT ^ (k << 32) ^ (u64::from(w) << 16) ^ u64::from(h);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Natural tile size for a cell.
///
/// Width rides `cell_w` (the cloud is measured in COLUMNS) but is CLAMPED to
/// the baker's slot bounds — `4 * cell_h` wide, `2 * cell_h` tall — because
/// `CatBaker::host_tile` rejects anything larger. On a very wide cell the
/// column-derived width would exceed that, so the clamp is load-bearing, not
/// defensive.
#[must_use]
pub fn nuke_nat_size(part: NukePart, cell_w: u16, cell_h: u16) -> (u16, u16) {
    let (cw, ch) = (f32::from(cell_w.max(1)), f32::from(cell_h.max(1)));
    let (cols, rows) = match part {
        NukePart::Cap => (6.0, 1.50),
        NukePart::Stem => (2.0, 1.90),
        NukePart::Skirt => (8.0, 0.55),
    };
    let w = (cols * cw).round().clamp(4.0, 4.0 * ch) as u16;
    let h = (rows * ch).round().clamp(4.0, 2.0 * ch) as u16;
    (w, h)
}

/// Bake one cloud part at its natural `w x h`, WHITE (tinted per sprite).
///
/// Deterministic: const drawlists through the fixed scanline filler, so one
/// tile per `(part, w, h)` and byte-identical across bakes.
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
            fill_path(&mut tile, &[&NUKE_CAP_CURL], white, NUKE_A * 0.82, fit);
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
        }
        NukePart::Skirt => {
            fill_path(&mut tile, &[&NUKE_SKIRT], white, NUKE_A * 0.62, fit);
        }
    }
    tile
}

/// One part's resolved draw state at `t_ms`: dest scale about the anchor, a
/// vertical offset in CELLS, alpha, and the cooled tint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NukeDraw {
    pub sx: f32,
    pub sy: f32,
    pub dy_cells: f32,
    pub alpha: f32,
    pub tint: u32,
}

const FLASH_CORE: u32 = 0x00FF_F6E0;
const CAP_HOT: u32 = 0x00FF_C466;
const CAP_DUST: u32 = 0x009A_8272;
const STEM_HOT: u32 = 0x00FF_D79A;
const STEM_DUST: u32 = 0x00B0_8D74;
const SKIRT_TONE: u32 = 0x00C8_B49C;

/// Heat → dust. The whole cloud cools on ONE clock so the cap and the stem can
/// never disagree about how old the blast is.
fn cool(t_ms: u64) -> f32 {
    aterm_scene::smoothstep((t_ms.saturating_sub(900) as f32 / 1700.0).clamp(0.0, 1.0))
}

/// The tier's flash-core tint, for the host's `0..FLASH_END_MS` crown.
#[must_use]
pub fn flash_core() -> u32 {
    FLASH_CORE
}

/// Resolve one part at `t_ms`, or `None` when that part is not on screen yet
/// (or any more). Pure: same input, same bytes.
#[must_use]
pub fn nuke_draw(t_ms: u64, part: NukePart) -> Option<NukeDraw> {
    let c = cool(t_ms);
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
            let q = ((t_ms - FLASH_END_MS) as f32 / (STEM_RISE_END_MS - FLASH_END_MS) as f32)
                .clamp(0.0, 1.0);
            // Ease-out cubic: the column leaves the ground fast and settles.
            let rise = 1.0 - (1.0 - q) * (1.0 - q) * (1.0 - q);
            Some(NukeDraw {
                sx: 0.70 + 0.30 * rise,
                sy: 0.15 + 0.85 * rise,
                dy_cells: -0.35 * (t_ms.saturating_sub(CAP_BLOOM_END_MS) as f32 / 1700.0),
                alpha: (q * 3.0).min(1.0)
                    * fade(CAP_BLOOM_END_MS, NUKE_TOTAL_MS - CAP_BLOOM_END_MS),
                tint: aterm_scene::mix_rgb(STEM_HOT, STEM_DUST, c),
            })
        }
        NukePart::Cap => {
            if !(CAP_BLOOM_START_MS..NUKE_TOTAL_MS).contains(&t_ms) {
                return None;
            }
            let q = ((t_ms - CAP_BLOOM_START_MS) as f32
                / (CAP_BLOOM_END_MS - CAP_BLOOM_START_MS) as f32)
                .clamp(0.0, 1.0);
            // Bloom with a ~6% overshoot near q = 0.75, then a slow spread —
            // the head keeps growing after it stops rising, which is what makes
            // it read as billowing rather than inflating.
            let bloom = (1.0 - (1.0 - q) * (1.0 - q) * (1.0 - q))
                + 0.06 * (core::f32::consts::PI * q).sin();
            let spread = 1.0
                + 0.25 * (t_ms.saturating_sub(CAP_BLOOM_END_MS) as f32 / 1700.0).clamp(0.0, 1.0);
            Some(NukeDraw {
                sx: (0.35 + 0.65 * bloom) * spread,
                sy: (0.40 + 0.60 * bloom) * (1.0 + 0.10 * (spread - 1.0)),
                dy_cells: -0.60 * (t_ms.saturating_sub(CAP_BLOOM_START_MS) as f32 / 2700.0),
                alpha: (q * 4.0).min(1.0)
                    * fade(CAP_BLOOM_END_MS, NUKE_TOTAL_MS - CAP_BLOOM_END_MS),
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
                sx: 0.40 + 1.50 * q,
                sy: 1.0,
                dy_cells: 0.10 * q,
                alpha: 0.70 * (core::f32::consts::PI * q).sin().max(0.0),
                tint: SKIRT_TONE,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARTS: [NukePart; 3] = [NukePart::Cap, NukePart::Stem, NukePart::Skirt];

    /// The art is a PURE function of `(part, w, h)` — the property the parity
    /// suite depends on, and the reason the cloud can be baked once and then
    /// animated by transform alone.
    #[test]
    fn baking_is_deterministic() {
        for part in PARTS {
            let (w, h) = nuke_nat_size(part, 14, 32);
            let a = bake_nuke(w, h, part);
            let b = bake_nuke(w, h, part);
            assert_eq!(a.pixels(), b.pixels(), "{part:?} baked two different tiles");
            assert!(
                a.pixels().iter().any(|p| *p != 0),
                "{part:?} baked an empty tile"
            );
        }
    }

    /// `CatBaker::host_tile` rejects a tile wider than `4 * cell_h` or taller
    /// than `2 * cell_h`. The cloud is measured in COLUMNS, so on a wide cell
    /// the natural width would breach that — the clamp has to hold for every
    /// plausible cell geometry, not just the owner's.
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
    }

    /// Each part is on screen for exactly its own window, and the whole cloud
    /// is gone by `NUKE_TOTAL_MS` — nothing may outlive the detonation.
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
        // The cap starts BEFORE the stem finishes: the head is pushed up by the
        // column, it does not appear on a finished one. Both are constants, so
        // this is checked at build time — a retune that inverts them never
        // compiles.
        const {
            assert!(CAP_BLOOM_START_MS < STEM_RISE_END_MS);
        }
        for part in PARTS {
            assert!(
                nuke_draw(NUKE_TOTAL_MS, part).is_none(),
                "{part:?} outlived the detonation"
            );
        }
    }

    /// The cloud COOLS: its tint must actually change from hot to dust, and
    /// every part must stay finite and inside its alpha range for every frame
    /// of the whole window (a NaN here would be an invisible or a stuck sprite).
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
