// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The comet cursor (`comet` trail style): the block cursor is a frosted fill
//! wrapped in a round additive coma, with a few glints twinkling on its rim.
//! It breathes off the aurora's blaze ([`crate::cursor_glow::CursorGlow::blaze`]):
//! dim and slow at rest, swelling and whitening under the keys, white-hot on a
//! jump flare.
//!
//! Text-safe: the fill goes through the renderer's `floor_cursor_fill` contrast
//! floor, and coma and glints are additive [`GlowQuad`] light with capped
//! coverage. Pure in the injected `now`; it settles to inactive when the blaze
//! dies, then rides the blink cadence.

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::Geom;
use crate::effect_util::lerp_rgb;
// Window px: this light lands in `cursor_glow_add` (see `push_fx_rect`).
use crate::effect_util::push_fx_rect as push_rect;

/// Shimmer rate in turns/second, at rest and at full blaze. The phase advances
/// only on rendered frames, so a settled nucleus shimmers at the blink cadence.
const SHIMMER_IDLE: f32 = 0.7;
const SHIMMER_ACTIVE: f32 = 3.2;

/// Coma radius in cell-heights: the diameter must cover the cell height, or
/// the block's corners poke out and it reads as a rectangle.
const RADIUS_IDLE: f32 = 0.54;
const RADIUS_MAX: f32 = 0.82;

/// Innermost-disc coverage before intensity, at rest and at full blaze. The
/// outer discs scale down from it, each clamped by [`COV_CAP`].
const COV_IDLE: f32 = 58.0;
const COV_MAX: f32 = 148.0;
/// Per-quad additive coverage ceiling: inner discs saturate here over the
/// cursor cell; fringe quads over neighbouring glyphs stay a tint.
const COV_CAP: f32 = 92.0;

/// Facet glints (tiny 4-point sparks) on the coma rim.
const GLINTS: u32 = 4;

/// Below this blaze the nucleus reports inactive, so the host stops the tick.
const SETTLED_BLAZE: f32 = 0.02;

/// The near-white the ice freezes toward — deliberately short of pure white so
/// the coma's stacked core (additive) stays the brightest thing on screen.
const ICE_WHITE: u32 = 0x00F2_FAFF;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct CometConfig {
    /// On when the `comet` style is chosen and the cursor is a focused, visible
    /// block.
    pub enabled: bool,
    /// Overall scale `0..1` (reduced motion / load shed); 0 is fully inert.
    pub intensity: f32,
    /// The trail's base hue (`GlowConfig::color`); the coma freezes off it.
    pub color: u32,
    /// The trail's accent hue (`GlowConfig::accent`): the coma's outer fringe.
    pub accent: u32,
}

/// What a tick produced: the frosted block fill for the renderer (which floors
/// it for contrast) and a fingerprint that changes on every visible step (0 when
/// off).
#[derive(Clone, Copy, Debug)]
pub struct CometFrame {
    /// The frosted block-fill colour `0x00RRGGBB`, or `None` when the nucleus is
    /// off (the renderer then keeps the ordinary themed cursor fill).
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + coma + glints (0 ⇒ nothing this frame).
    pub fp: u64,
}

/// Per-window nucleus state: shimmer phase, last clock reading, latched blaze.
#[derive(Default)]
pub struct CursorComet {
    /// Rolling shimmer phase in turns `0..1`.
    shimmer: f32,
    last: Option<Instant>,
    /// Latched blaze at the last tick, so [`Self::is_active`] needs no clock.
    blaze: f32,
}

impl CursorComet {
    /// Whether the host must keep arming the animation tick: only while the
    /// comet is charged (typing, or a jump flare cooling).
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.blaze > SETTLED_BLAZE
    }

    /// Advance one frame at `now` with the aurora's `blaze` (`0..1`, read after
    /// the aurora tick), the block cursor cell `cur` (`None`: hidden), grid
    /// `geom` and `cfg`. Appends the additive coma to `out`; returns the frosted
    /// fill and a fingerprint.
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        blaze: f32,
        geom: Geom,
        cfg: &CometConfig,
        out: &mut Vec<GlowQuad>,
    ) -> CometFrame {
        let e = (blaze.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        // Inert (the plain themed cursor) when off, with degenerate geometry,
        // or at zero intensity.
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.blaze = 0.0; // inert: report settled so the host disarms the tick
            self.last = Some(now);
            return CometFrame { fill: None, fp: 0 };
        }
        self.blaze = e;

        // Advance by real dt, clamped so a long stall doesn't fling the phase.
        let dt = self
            .last
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            .unwrap_or(0.0)
            .min(0.25);
        self.last = Some(now);
        self.shimmer = (self.shimmer + dt * (SHIMMER_IDLE + SHIMMER_ACTIVE * e)).fract();
        let phase = self.shimmer * std::f32::consts::TAU;
        // The breathing envelope 0..1 — two incommensurate sines so the coma
        // never reads as a metronome pulse.
        let breathe = 0.5 + 0.35 * phase.sin() + 0.15 * (phase * 2.3 + 0.9).sin();

        // Frosted fill: a cool tint of the trail hue frosting toward white with
        // the blaze, kept below the coma core's near-white. The renderer floors
        // it against the cell bg, so the glyph stays sharp.
        let fill = ice_ramp(cfg.color, 0.34 + 0.40 * e + 0.05 * (breathe - 0.5));

        // The additive coma, drawn only over a visible in-grid cursor cell.
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
        {
            self.emit_coma(cr, cc, e, breathe, phase, geom, cfg, out);
        }

        // Fingerprint: quantized phase + blaze + fill, so a settled nucleus
        // early-outs the present but any visible step forces a repaint.
        let fp = ((self.shimmer * 512.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add((e * 255.0) as u64)
            .wrapping_add((fill as u64) << 12)
            | 1; // never 0 while enabled — the nucleus is always drawable

        CometFrame {
            fill: Some(fill),
            fp,
        }
    }

    /// Emit the coma + rim glints as premultiplied additive quads. Bounded by
    /// construction: ≤ ~4 discs × (2·radius / slab) slabs + [`GLINTS`] crosses.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + blaze/breathe/phase + geometry/config; one internal call site"
    )]
    fn emit_coma(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        breathe: f32,
        phase: f32,
        geom: Geom,
        cfg: &CometConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        // Coma centre: mid-cell, in window px (the grid origin is folded in
        // here, once; every quad below derives from it).
        let cx = f32::from(geom.origin_x) + (cc as f32 + 0.5) * cw;
        let cy = f32::from(geom.origin_y) + (cr as f32 + 0.5) * ch;
        let radius = (RADIUS_IDLE + (RADIUS_MAX - RADIUS_IDLE) * e) * ch * (0.96 + 0.06 * breathe);
        let cov_core =
            (COV_IDLE + (COV_MAX - COV_IDLE) * e) * cfg.intensity * (0.86 + 0.14 * breathe);

        // Concentric discs, outermost first: accent fringe, hue body, sunlit ice,
        // then the near-white heart as the blaze climbs. Additive overlap builds
        // the radial gradient. (radius ×, coverage ×, ramp t through `ice_ramp`.)
        let discs: [(f32, f32, f32); 4] = [
            (1.00, 0.30, 0.18 + 0.06 * e),
            (0.80, 0.58, 0.44 + 0.12 * e),
            (0.60, 0.85, 0.66 + 0.18 * e),
            (0.38, 1.00, 0.86 + 0.14 * e),
        ];
        // Slab height ~1/8 cell (min 2px): fine enough to read as round.
        let slab = ((ch * 0.125) as i32).max(2);
        for &(rx, cx_cov, ramp_t) in &discs {
            let r = radius * rx;
            if r < 1.0 {
                continue;
            }
            let cov = (cov_core * cx_cov).min(COV_CAP) as u8;
            if cov == 0 {
                continue;
            }
            let color = if rx >= 1.0 {
                // The outer fringe carries the accent, so the head reads layered.
                lerp_rgb(cfg.accent, ice_ramp(cfg.color, ramp_t), 0.45)
            } else {
                ice_ramp(cfg.color, ramp_t)
            };
            let premul = premul_rgb(color, cov);
            let r_i = r as i32;
            let mut dy = -r_i;
            while dy < r_i {
                let h = slab.min(r_i - dy);
                // Half-width at the slab's vertical centre, with a small
                // travelling wobble so the silhouette is not a sticker.
                let ym = dy as f32 + h as f32 * 0.5;
                let base = (r * r - ym * ym).max(0.0).sqrt();
                let wobble = 1.0 + 0.05 * (ym * 0.55 / ch.max(1.0) * 10.0 + phase * 1.3).sin();
                let hw = (base * wobble) as i32;
                if hw >= 1 {
                    push_rect(out, geom, cx as i32 - hw, cy as i32 + dy, 2 * hw, h, premul);
                }
                dy += h;
            }
        }

        // Rim glints orbit with the shimmer phase at ~0.72·radius, off the glyph
        // row's centre band; each is two hairline rects, so no letter is buried.
        let glint_r = radius * 0.72;
        for i in 0..GLINTS {
            let fi = i as f32;
            let ang = fi / GLINTS as f32 * std::f32::consts::TAU + phase * 0.55;
            let tw = 0.5 + 0.5 * (phase * 2.0 + fi * 2.4).sin();
            // Only the bright half of each twinkle draws: glints flash.
            if tw < 0.55 {
                continue;
            }
            let cov = (cov_core * (0.35 + 0.65 * e) * (tw - 0.55) / 0.45).min(COV_CAP) as u8;
            if cov == 0 {
                continue;
            }
            let gx = (cx + ang.cos() * glint_r) as i32;
            let gy = (cy + ang.sin() * glint_r * 0.9) as i32;
            let arm = ((ch * 0.11) as i32).max(1);
            let spark = premul_rgb(ICE_WHITE, cov);
            push_rect(out, geom, gx - arm, gy, 2 * arm + 1, 1, spark);
            push_rect(out, geom, gx, gy - arm, 1, 2 * arm + 1, spark);
        }
    }
}

/// The ice ramp off the trail's base hue, `t` 0 (cold, dim) to 1 (sunlit,
/// near-white): dimmed hue, pure hue, whitened hue, [`ICE_WHITE`]. Derived from
/// the config colour, so an explicit `cursor_trail_color` re-tints the nucleus
/// with the tail.
fn ice_ramp(color: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.45 {
        lerp_rgb(lerp_rgb(color, 0x0000_0000, 0.50), color, t / 0.45)
    } else if t < 0.78 {
        lerp_rgb(color, lerp_rgb(color, ICE_WHITE, 0.55), (t - 0.45) / 0.33)
    } else {
        lerp_rgb(
            lerp_rgb(color, ICE_WHITE, 0.55),
            ICE_WHITE,
            (t - 0.78) / 0.22,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_util::ink;
    use std::time::Duration;

    fn geom() -> Geom {
        Geom {
            cw: 8,
            ch: 16,
            rows: 6,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 320,
            win_h: 96,
            head: 0,
        }
    }
    fn cfg() -> CometConfig {
        CometConfig {
            enabled: true,
            intensity: 1.0,
            color: 0x009E_D6FF, // the default glacial blue: r ≤ g ≤ b
            accent: 0x00C8_EAFF,
        }
    }

    /// Disabled or zero intensity: fully inert (no fill, no coma, fp 0, settled).
    #[test]
    fn disabled_or_zero_intensity_is_inert() {
        for (why, config) in [
            (
                "disabled",
                CometConfig {
                    enabled: false,
                    ..cfg()
                },
            ),
            (
                "zero intensity",
                CometConfig {
                    intensity: 0.0,
                    ..cfg()
                },
            ),
        ] {
            let mut nc = CursorComet::default();
            let mut out = Vec::new();
            let f = nc.tick(Some((1, 1)), Instant::now(), 1.0, geom(), &config, &mut out);
            assert!(f.fill.is_none(), "{why}: keeps the plain cursor");
            assert_eq!(f.fp, 0, "{why}");
            assert!(out.is_empty(), "{why}: no additive light");
            assert!(!nc.is_active(), "{why}: settled");
        }
    }

    /// A hidden cursor draws no coma; the fill stays resolved.
    #[test]
    fn hidden_cursor_draws_no_coma() {
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        let f = nc.tick(None, Instant::now(), 0.5, geom(), &cfg(), &mut out);
        assert!(out.is_empty(), "no additive light without a cursor cell");
        assert!(f.fill.is_some());
    }

    /// Full blaze swells the coma and shines far brighter than at rest.
    #[test]
    fn blaze_swells_and_brightens() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let run = |blaze: f32| -> (u64, i32, i32) {
            let mut nc = CursorComet::default();
            let mut out = Vec::new();
            nc.tick(Some((3, 20)), t, blaze, g, &c, &mut out);
            out.clear();
            nc.tick(
                Some((3, 20)),
                t + Duration::from_millis(16),
                blaze,
                g,
                &c,
                &mut out,
            );
            let x0 = out.iter().map(|q| q.x as i32).min().unwrap_or(0);
            let x1 = out.iter().map(|q| (q.x + q.w) as i32).max().unwrap_or(0);
            (ink(&out), x0, x1)
        };
        let (cold_ink, cold_x0, cold_x1) = run(0.0);
        let (hot_ink, hot_x0, hot_x1) = run(1.0);
        assert!(cold_ink > 0, "the resting nucleus is still visibly lit");
        assert!(
            hot_ink > cold_ink * 2,
            "full blaze far outshines the cold drift ({hot_ink} vs {cold_ink})"
        );
        assert!(
            hot_x1 - hot_x0 > cold_x1 - cold_x0,
            "the sunlit coma swells wider ({} vs {})",
            hot_x1 - hot_x0,
            cold_x1 - cold_x0
        );
    }

    /// Every emitted quad is single-row, grid-interior, and ice-hued with capped
    /// coverage (the renderer row-gate / parity / legibility invariants).
    #[test]
    fn quads_respect_grid_hue_and_cap() {
        let g = geom();
        let c = cfg();
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        let t = Instant::now();
        // A corner cell so clamping is exercised.
        nc.tick(Some((0, 0)), t, 1.0, g, &c, &mut out);
        nc.tick(
            Some((0, 0)),
            t + Duration::from_millis(16),
            1.0,
            g,
            &c,
            &mut out,
        );
        assert!(!out.is_empty());
        let gw = (g.cols * g.cw) as u32;
        let gh = (g.rows * g.ch) as u32;
        for q in &out {
            let band = q.row as u32 * g.ch as u32;
            assert!(
                q.y as u32 >= band && q.y as u32 + q.h as u32 <= band + g.ch as u32,
                "single-row: {q:?}"
            );
            assert!(
                q.x as u32 + q.w as u32 <= gw && q.y as u32 + q.h as u32 <= gh,
                "in grid: {q:?}"
            );
            let (r, gg, b) = (
                (q.color >> 16) & 0xff,
                (q.color >> 8) & 0xff,
                q.color & 0xff,
            );
            assert!(
                b + 1 >= gg && gg + 1 >= r,
                "premultiplied icy hue ordering: {q:?}"
            );
            for sh in [16u32, 8, 0] {
                assert!(
                    (q.color >> sh) & 0xff <= COV_CAP as u32 + 1,
                    "coverage capped: {q:?}"
                );
            }
        }
    }

    /// Charged keeps the tick armed; a dead blaze settles to inactive.
    #[test]
    fn settles_to_inactive_when_blaze_dies() {
        let g = geom();
        let c = cfg();
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        let t = Instant::now();
        nc.tick(Some((1, 1)), t, 0.8, g, &c, &mut out);
        assert!(nc.is_active(), "a charged nucleus keeps the tick armed");
        nc.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.0,
            g,
            &c,
            &mut out,
        );
        assert!(
            !nc.is_active(),
            "the settled nucleus idles on the blink cadence"
        );
    }

    /// An explicit trail colour re-tints the whole nucleus: the ramp derives
    /// from the config hue, never a fixed palette.
    #[test]
    fn nucleus_follows_the_configured_hue() {
        let g = geom();
        let t = Instant::now();
        let mut out = Vec::new();
        let mut nc = CursorComet::default();
        let mut c = cfg();
        c.color = 0x00FF_4020; // a red comet, if that's what the user pins
        let f = nc.tick(Some((1, 1)), t, 0.2, g, &c, &mut out).fill.unwrap();
        let (r, gg, b) = ((f >> 16) & 0xff, (f >> 8) & 0xff, f & 0xff);
        assert!(
            r > b && r > gg,
            "the fill follows the pinned hue, got {f:#08x}"
        );
    }

    /// The coma surrounds the caret in a padded, headed window: the producer
    /// folds in the grid origin, which the origin (0,0) tests cannot tell.
    #[test]
    fn coma_surrounds_the_caret_in_a_padded_headed_window() {
        let t0 = Instant::now();
        let real = Geom {
            cw: 15,
            ch: 28,
            rows: 30,
            cols: 100,
            origin_x: 24,
            origin_y: 84,
            win_w: 1548,
            win_h: 948,
            head: 80,
        };
        let (cr, cc) = (12u16, 47u16);
        let mut e = CursorComet::default();
        let mut out = Vec::new();
        e.tick(Some((cr, cc)), t0, 1.0, real, &cfg(), &mut out);
        assert!(!out.is_empty(), "the emitter drew nothing to place");
        // The caret cell in window px, from the one anchor every emitter shares.
        let (x0c, y0c) = (
            i32::from(real.origin_x) + i32::from(cc) * real.cw as i32,
            i32::from(real.origin_y) + i32::from(cr) * real.ch as i32,
        );
        let (x1c, y1c) = (x0c + real.cw as i32, y0c + real.ch as i32);
        let x0 = out.iter().map(|q| i32::from(q.x)).min().unwrap();
        let x1 = out.iter().map(|q| i32::from(q.x + q.w)).max().unwrap();
        let y0 = out.iter().map(|q| i32::from(q.y)).min().unwrap();
        let y1 = out.iter().map(|q| i32::from(q.y + q.h)).max().unwrap();
        assert!(
            x0 < x0c && x1 > x1c && y0 < y0c && y1 > y1c,
            "bbox x {x0}..{x1} y {y0}..{y1} does not surround the caret cell \
             x {x0c}..{x1c} y {y0c}..{y1c} — a grid-relative emitter lands at \
             x {}..  y {}.., short by (origin_x, origin_y) = ({}, {})",
            x0 + i32::from(real.origin_x),
            y0 + i32::from(real.origin_y),
            real.origin_x,
            real.origin_y
        );
        // Every quad inside the effects box (the grid plus the head band).
        for q in &out {
            assert!(
                i32::from(q.x) >= real.fx_left() && i32::from(q.x + q.w) <= real.fx_right(),
                "quad x {}..{} outside the effects box",
                q.x,
                q.x + q.w
            );
            assert!(
                i32::from(q.y) >= real.fx_top() && i32::from(q.y + q.h) <= real.fx_bot(),
                "quad y {}..{} outside the effects box",
                q.y,
                q.y + q.h
            );
        }
        // Identity: at origin (0,0) the window form equals the grid form, so
        // the other tests here are unaffected.
        let mut at_origin = Vec::new();
        let mut f = CursorComet::default();
        f.tick(Some((3, 10)), t0, 1.0, geom(), &cfg(), &mut at_origin);
        let mut shifted = Vec::new();
        let mut g = CursorComet::default();
        let mut o = geom();
        o.origin_x = 24;
        o.origin_y = 84;
        o.win_w += 48;
        o.win_h += 84;
        o.head = 84;
        g.tick(Some((3, 10)), t0, 1.0, o, &cfg(), &mut shifted);
        assert_eq!(
            at_origin.len(),
            shifted.len(),
            "the origin changed the quad COUNT"
        );
        for (a, b) in at_origin.iter().zip(&shifted) {
            assert_eq!(
                (i32::from(a.x) + 24, i32::from(a.y) + 84, a.w, a.h, a.color),
                (i32::from(b.x), i32::from(b.y), b.w, b.h, b.color),
                "a quad is not the grid form translated by the origin"
            );
        }
    }
}
