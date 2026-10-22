// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The droplet cursor (`water` trail style): the block cursor is a cool aqua
//! fill under an additive bead with a specular glint, drips beading off its
//! belly and ripple rings spreading below. It breathes off the aurora's blaze
//! ([`crate::cursor_glow::CursorGlow::blaze`]): calm at rest, sloshing and
//! dripping under the keys, splashing on a jump flare.
//!
//! On a light page the water runs the other way: the fill deepens toward the
//! abyss with the surge, and bead, glint, rings and drips are not drawn
//! (additive light cannot brighten a white ground, and over a glyph it only
//! lifts the ink toward the page).
//!
//! Text safety: given its page ([`DropletConfig::ground`]) the fill's ramp
//! position is solved against it, so the caret and the glyph cut out of it
//! keep [`CARET_PAGE_MIN_CONTRAST`] on any ground while the colour stays on
//! the water ramp; the host's page floor
//! ([`crate::cursor_fx::floor_block_fill`]) and the renderer's
//! `minimum_contrast` floor stand behind it. Bead, drips and rings are
//! additive [`GlowQuad`] light with capped coverage. Pure in the injected
//! `now`; it settles to inactive when the surge dies, then rides the blink
//! cadence.

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_fx::CARET_PAGE_MIN_CONTRAST;
use crate::cursor_glow::{Geom, push_ripple_ring};
use crate::effect_util::{push_soft_disc, water_ramp};

/// Slosh rate in turns/second, at rest and at full surge. The phase advances
/// only on rendered frames, so a settled bead sways at the blink cadence.
const SLOSH_IDLE: f32 = 0.9;
const SLOSH_ACTIVE: f32 = 4.2;

/// Bead radius in cell-heights: the diameter must cover the cell height, or
/// the block's corners poke out and it reads as a rectangle.
const RADIUS_IDLE: f32 = 0.52;
const RADIUS_MAX: f32 = 0.78;

/// Innermost-disc coverage before intensity, at rest and at full surge. The
/// outer discs scale down from it, each clamped by [`COV_CAP`].
const COV_IDLE: f32 = 60.0;
const COV_MAX: f32 = 148.0;
/// Per-quad additive coverage ceiling: inner discs saturate here over the
/// cursor cell; rim quads and rings over neighbouring glyphs stay a tint.
const COV_CAP: f32 = 92.0;

/// A soft disc carries its full coverage only at its heart and falls off to
/// nothing at its rim, where the slab discs it replaced held theirs to the
/// edge: this much more coverage keeps the bead as bright to the eye.
const DISC_SOFT_GAIN: f32 = 1.35;

/// Ripple cadence in rings/second, at rest and at full surge. Two rings run
/// half a cycle apart.
const RIPPLE_IDLE: f32 = 0.35;
const RIPPLE_ACTIVE: f32 = 1.15;
/// How far a ring rolls out, in bead-radii, over its life (`u` 0→1).
const RIPPLE_REACH: f32 = 2.2;

/// Drip cadence in drops/second/lane, at rest and at full surge.
const DRIP_IDLE: f32 = 0.45;
const DRIP_ACTIVE: f32 = 1.8;
/// Drip lanes: x offset from the bead centre (× cell width), phase offset, and
/// the surge at which the lane wakes.
const DRIP_LANES: [(f32, f32, f32); 3] =
    [(-0.26, 0.00, 0.0), (0.06, 0.37, 0.30), (0.30, 0.71, 0.62)];
/// How far a drop falls, in cell-heights, before it has fully faded.
const DRIP_FALL: f32 = 1.05;

/// Below this surge the droplet reports inactive, so the host stops the tick.
const SETTLED_SURGE: f32 = 0.02;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct DropletConfig {
    /// On when the `water` style is chosen and the cursor is a focused, visible
    /// block.
    pub enabled: bool,
    /// Overall scale `0..1` (reduced motion / load shed); 0 is fully inert.
    pub intensity: f32,
    /// The page the caret sits on (the live default background,
    /// `0x00RRGGBB`): the fill's ramp position is solved against it and its
    /// polarity picks the arm (a light page deepens the water and draws no
    /// additive light). `None` is the dark arm with the ramp unsolved, for
    /// callers with no page.
    pub ground: Option<u32>,
}

/// What a tick produced: the liquid block fill and a fingerprint that changes
/// on every visible step (0 when off).
#[derive(Clone, Copy, Debug)]
pub struct DropletFrame {
    /// The liquid block-fill colour `0x00RRGGBB`, or `None` when the droplet is
    /// off (the renderer then keeps the ordinary themed cursor fill).
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + bead + rings + drips (0 ⇒ nothing this frame).
    pub fp: u64,
}

/// The band of water-ramp positions whose colour keeps
/// [`CARET_PAGE_MIN_CONTRAST`] against `ground`, as `(lo, hi)`. The ramp's
/// luminance climbs with `t` (abyss to foam), so on a dark page the band is
/// `[lo, 1]` and on a light one `[0, hi]`; each edge is bisected on the
/// ramp's own bytes. An unreachable floor collapses the band onto the
/// ramp's end that contrasts most.
fn ramp_band_on(ground: u32) -> (f32, f32) {
    let meets = |t: f32| {
        crate::color_math::contrast_ratio(water_ramp(t), ground) >= CARET_PAGE_MIN_CONTRAST
    };
    let dark = aterm_render::theme_is_dark(ground);
    // The end of the ramp on the far side of the page from the ground.
    let far = if dark { 1.0 } else { 0.0 };
    if !meets(far) {
        return (far, far);
    }
    let near = 1.0 - far;
    if meets(near) {
        return (0.0, 1.0);
    }
    // `pass` always meets, `fail` never: the edge lies between.
    let (mut pass, mut fail) = (far, near);
    for _ in 0..20 {
        let mid = 0.5 * (pass + fail);
        if meets(mid) {
            pass = mid;
        } else {
            fail = mid;
        }
    }
    if dark { (pass, 1.0) } else { (0.0, pass) }
}

/// Per-window droplet state: slosh, ripple and drip phases, the last clock
/// reading, and a latched surge.
#[derive(Default)]
pub struct CursorDroplet {
    /// Rolling slosh (caustic shimmer) phase in turns `0..1`.
    slosh: f32,
    /// Rolling ripple-ring phase in ring-lives `0..1` (two rings ride it, half a
    /// cycle apart).
    ripple: f32,
    /// Rolling drip phase in drop-lives `0..1` (each lane adds its own offset).
    drip: f32,
    last: Option<Instant>,
    /// Latched surge at the last tick, so [`Self::is_active`] needs no clock.
    surge: f32,
    /// Cached `(ground, (lo, hi))` from [`ramp_band_on`].
    band_memo: Option<(u32, (f32, f32))>,
}

impl CursorDroplet {
    /// Whether the host must keep arming the animation tick: only while the
    /// water is surging (typing, or a splash calming).
    #[must_use]
    #[inline]
    pub fn is_active(&self) -> bool {
        self.surge > SETTLED_SURGE
    }

    /// Advance one frame at `now` with the water's `surge` (`0..1`, the aurora's
    /// blaze read after its tick), the block cursor cell `cur` (`None`: hidden),
    /// grid `geom` and `cfg`. Appends the additive droplet to `out`; returns the
    /// block fill and a fingerprint.
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        surge: f32,
        geom: Geom,
        cfg: &DropletConfig,
        out: &mut Vec<GlowQuad>,
    ) -> DropletFrame {
        let e = (surge.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        // Inert (the plain themed cursor) when off, with degenerate geometry,
        // or at zero intensity.
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.surge = 0.0; // inert: report settled so the host disarms the tick
            self.last = Some(now);
            return DropletFrame { fill: None, fp: 0 };
        }
        self.surge = e;

        // Advance by real dt, clamped so a long stall doesn't fling the phases.
        let dt = self
            .last
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            .unwrap_or(0.0)
            .min(0.25);
        self.last = Some(now);
        self.slosh = (self.slosh + dt * (SLOSH_IDLE + SLOSH_ACTIVE * e)).fract();
        self.ripple = (self.ripple + dt * (RIPPLE_IDLE + RIPPLE_ACTIVE * e)).fract();
        self.drip = (self.drip + dt * (DRIP_IDLE + DRIP_ACTIVE * e)).fract();
        let phase = self.slosh * std::f32::consts::TAU;
        // The shared caustic envelope 0..1 — two incommensurate sines so the
        // shimmer never reads as a metronome pulse.
        let shimmer = 0.5 + 0.35 * phase.sin() + 0.15 * (phase * 2.7 + 1.3).sin();

        // The block fill, shimmering with the caustics. On a dark page: deep
        // still water at rest, bright azure at full surge, capped below the
        // additive heart's foam so the round bead, not the rectangle, is the
        // brightest thing. On a light page the surge deepens it toward the
        // abyss instead. Either way the ramp position stays inside the band
        // that keeps the page floor against the actual ground.
        let dark = cfg.ground.is_none_or(aterm_render::theme_is_dark);
        let caustic = 0.05 * (shimmer - 0.5);
        let t = if dark {
            0.36 + 0.42 * e + caustic
        } else {
            0.36 - 0.16 * e + caustic
        };
        let t = match cfg.ground {
            Some(ground) => {
                let (lo, hi) = match self.band_memo {
                    Some((g, band)) if g == ground => band,
                    _ => {
                        let band = ramp_band_on(ground);
                        self.band_memo = Some((ground, band));
                        band
                    }
                };
                t.clamp(lo, hi)
            }
            None => t,
        };
        let fill = water_ramp(t);

        // The additive droplet, drawn only over a visible in-grid cursor cell
        // on a dark page.
        if dark
            && let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
        {
            self.emit_bead(cr, cc, e, shimmer, phase, geom, cfg, out);
        }

        // Fingerprint: quantized phases + surge + fill, so a settled bead
        // early-outs the present but any visible step forces a repaint.
        let fp = ((self.slosh * 512.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add(((self.ripple * 512.0) as u64) << 20)
            .wrapping_add(((self.drip * 512.0) as u64) << 40)
            .wrapping_add((e * 255.0) as u64)
            .wrapping_add((fill as u64) << 12)
            | 1; // never 0 while enabled — still water is always drawable

        DropletFrame {
            fill: Some(fill),
            fp,
        }
    }

    /// Emit the bead, glint, ripple rings and drips as premultiplied additive
    /// quads: ≤ ~4 discs × (2·radius / slab) slabs, two rings, three drips,
    /// two glints.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + surge/shimmer/phase + geometry/config; one internal call site"
    )]
    fn emit_bead(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        shimmer: f32,
        phase: f32,
        geom: Geom,
        cfg: &DropletConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        // Bead centre in window px (the grid origin is folded in here, once),
        // floated to 0.46 of the cell so drips and waterline have headroom.
        let cx = f32::from(geom.origin_x) + (cc as f32 + 0.5) * cw;
        let cy = f32::from(geom.origin_y) + (cr as f32 + 0.46) * ch;
        let radius = (RADIUS_IDLE + (RADIUS_MAX - RADIUS_IDLE) * e) * ch;
        let cov_core =
            (COV_IDLE + (COV_MAX - COV_IDLE) * e) * cfg.intensity * (0.84 + 0.16 * shimmer);

        // Concentric discs, outermost first (misty rim to foam-white heart);
        // additive overlap builds the radial gradient. Each disc is a soft
        // round mark ([`push_soft_disc`]): it falls off to nothing at its
        // rim, so the bead is round and wet, never a stack of hard slabs.
        // The bead hangs a little heavy and sloshes sideways, gently at rest,
        // churning at full surge.
        let discs: [(f32, f32, f32); 4] = [
            // (radius ×, coverage ×, water_ramp t)
            (1.00, 0.30, 0.24 + 0.06 * e),
            (0.82, 0.58, 0.46 + 0.12 * e),
            (0.62, 0.85, 0.66 + 0.16 * e),
            (0.40, 1.00, 0.86 + 0.14 * e),
        ];
        for (k, &(rx, cov_x, ramp_t)) in discs.iter().enumerate() {
            let r = radius * rx;
            if r < 1.0 {
                continue;
            }
            let cov = (cov_core * cov_x * DISC_SOFT_GAIN).min(COV_CAP);
            let shear = (phase + k as f32 * 0.6).sin() * r * 0.10 * (0.30 + 0.70 * e);
            push_soft_disc(
                out,
                geom,
                (cx + shear, cy + r * 0.06),
                (r * 0.94, r),
                water_ramp(ramp_t),
                cov,
            );
        }

        // Specular glint: an off-centre highlight high on the bead's shoulder
        // plus a small echo below, flickering with the caustics; soft points
        // of light, not chips.
        let glint_cov = (cov_core * (0.80 + 0.20 * shimmer)).min(COV_CAP);
        let gx = cx - radius * 0.30 + (phase.sin() * radius * 0.05);
        let gr = (radius * 0.14).max(1.5);
        // Aqua-tinged, not glacial white: a wet gleam.
        push_soft_disc(
            out,
            geom,
            (gx, cy - radius * 0.30),
            (gr, gr * 0.7),
            0x00B4_EAF0,
            glint_cov,
        );
        // The echo: a pinprick lower-right, half the size, half the light.
        push_soft_disc(
            out,
            geom,
            (cx + radius * 0.20, cy + radius * 0.14),
            (gr * 0.6, gr * 0.5),
            0x00B4_EAF0,
            glint_cov * 0.5,
        );

        // Ripple rings: two flattened ellipses half a life apart, scaled by the
        // surge (invisible on still water).
        if e > 0.0 {
            let ring_rgb = water_ramp(0.74 + 0.10 * e);
            let y0 = cy + radius * 0.88;
            for half in [0.0f32, 0.5] {
                let u = (self.ripple + half).fract();
                let one_minus_u = 1.0 - u;
                let env = one_minus_u * one_minus_u.sqrt() * e;
                let cov = (cov_core * 0.55 * env).min(COV_CAP) as u8;
                if cov == 0 {
                    continue;
                }
                let rx = radius * (0.9 + RIPPLE_REACH * u);
                let ry = (rx * 0.22).max(2.0);
                let thick = (rx * 0.12).max(1.5);
                let ring = premul_rgb(ring_rgb, cov);
                push_ripple_ring(out, geom, (cx, y0), (rx, ry), thick, ring);
            }
        }

        // Drips: one lazy drip at rest, rain as the surge wakes the outer lanes.
        // A drop grows on the belly for the first 30% of its life, then
        // detaches and accelerates down, fading: a soft round bead, drawn
        // out a little as it falls.
        let belly = cy + radius * 0.80;
        let body = water_ramp(0.68);
        for &(dx, off, wake) in &DRIP_LANES {
            if e < wake && wake > 0.0 {
                continue;
            }
            let u = (self.drip + off).fract();
            let x = cx + dx * cw + (phase + off * 7.0).sin() * 1.5;
            let (y, r, stretch, cov) = if u < 0.30 {
                // Beading: the drop grows on the belly.
                let s = u / 0.30;
                (belly, 1.2 + radius * 0.08 * s, 1.0, cov_core * 0.70 * s)
            } else {
                // Falling: detach, accelerate, fade.
                let v = (u - 0.30) / 0.70;
                (
                    belly + v * v * DRIP_FALL * ch,
                    1.2 + radius * 0.05,
                    1.0 + 0.8 * v,
                    cov_core * 0.70 * (1.0 - v),
                )
            };
            push_soft_disc(out, geom, (x, y), (r, r * stretch), body, cov.min(COV_CAP));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_util::ink;
    use crate::effect_util::push_fx_rect;
    use std::time::Duration;

    fn geom() -> Geom {
        Geom {
            cw: 8,
            ch: 16,
            rows: 8,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 320,
            win_h: 128,
            head: 0,
        }
    }
    fn cfg() -> DropletConfig {
        DropletConfig {
            enabled: true,
            intensity: 1.0,
            ground: None,
        }
    }

    /// Disabled or zero intensity: fully inert (no fill, no bead, fp 0, settled).
    #[test]
    fn disabled_or_zero_intensity_is_inert() {
        for (why, config) in [
            (
                "disabled",
                DropletConfig {
                    enabled: false,
                    intensity: 1.0,
                    ground: None,
                },
            ),
            (
                "zero intensity",
                DropletConfig {
                    enabled: true,
                    intensity: 0.0,
                    ground: None,
                },
            ),
        ] {
            let mut d = CursorDroplet::default();
            let mut out = Vec::new();
            let f = d.tick(Some((1, 1)), Instant::now(), 1.0, geom(), &config, &mut out);
            assert!(f.fill.is_none(), "{why}: keeps the plain cursor");
            assert_eq!(f.fp, 0, "{why}");
            assert!(out.is_empty(), "{why}: no additive light");
            assert!(!d.is_active(), "{why}: settled");
        }
    }

    /// A hidden cursor draws no bead; the fill stays resolved.
    #[test]
    fn hidden_cursor_draws_no_bead() {
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        let f = d.tick(None, Instant::now(), 0.5, geom(), &cfg(), &mut out);
        assert!(out.is_empty(), "no additive light without a cursor cell");
        assert!(f.fill.is_some());
    }

    /// Full surge swells the bead and shines far brighter than still water.
    #[test]
    fn surge_swells_and_brightens() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let run = |surge: f32| -> u64 {
            let mut d = CursorDroplet::default();
            let mut out = Vec::new();
            d.tick(Some((3, 20)), t, surge, g, &c, &mut out);
            out.clear();
            d.tick(
                Some((3, 20)),
                t + Duration::from_millis(16),
                surge,
                g,
                &c,
                &mut out,
            );
            ink(&out)
        };
        let still_ink = run(0.0);
        let surge_ink = run(1.0);
        assert!(still_ink > 0, "still water is still visibly lit");
        assert!(
            surge_ink > still_ink * 2,
            "full surge far outshines still water ({surge_ink} vs {still_ink})"
        );
    }

    /// Every emitted quad is single-row, grid-interior, and water-hued with capped
    /// coverage (the renderer row-gate / parity / legibility invariants).
    #[test]
    fn quads_respect_grid_hue_and_cap() {
        let g = geom();
        let c = cfg();
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        let t = Instant::now();
        // A corner cell so clamping is exercised.
        for i in 0..8 {
            if i == 7 {
                out.clear();
            }
            d.tick(
                Some((0, 0)),
                t + Duration::from_millis(16 * i),
                1.0,
                g,
                &c,
                &mut out,
            );
        }
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
                "premultiplied water hue ordering: {q:?}"
            );
            for sh in [16u32, 8, 0] {
                assert!(
                    (q.color >> sh) & 0xff <= COV_CAP as u32 + 1,
                    "coverage capped: {q:?}"
                );
            }
        }
    }

    /// Surging keeps the tick armed; a dead surge settles to inactive.
    #[test]
    fn settles_to_inactive_when_surge_dies() {
        let g = geom();
        let c = cfg();
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        let t = Instant::now();
        d.tick(Some((1, 1)), t, 0.8, g, &c, &mut out);
        assert!(d.is_active(), "surging water keeps the tick armed");
        d.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.0,
            g,
            &c,
            &mut out,
        );
        assert!(!d.is_active(), "still water idles on the blink cadence");
    }

    /// The bead surrounds the caret in a padded, headed window: the producer
    /// folds in the grid origin, which the origin (0,0) tests cannot tell.
    #[test]
    fn bead_surrounds_the_caret_in_a_padded_headed_window() {
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
        let mut e = CursorDroplet::default();
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
        let mut f = CursorDroplet::default();
        f.tick(Some((3, 10)), t0, 1.0, geom(), &cfg(), &mut at_origin);
        let mut shifted = Vec::new();
        let mut g = CursorDroplet::default();
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

    /// The caret cell at `(1, 1)` of [`geom`], composited with everything one
    /// full-surge tick emits over a page, and the fill the tick returned.
    fn surged_caret(ground: Option<u32>, page: u32) -> (f32, u32, usize) {
        let g = geom();
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        let c = DropletConfig { ground, ..cfg() };
        let fill = d
            .tick(Some((1, 1)), Instant::now(), 1.0, g, &c, &mut out)
            .fill
            .expect("an enabled droplet fills the block");
        let cell = (g.cw as i32, g.ch as i32, g.cw as i32, g.ch as i32);
        (
            crate::effect_util::caret_cell_worst_contrast(fill, &out, cell, page),
            fill,
            out.len(),
        )
    }

    /// On a light page at full surge the droplet keeps its caret: the fill is
    /// solved against the actual ground (white and Solarized's off-white) and
    /// no additive bead lands on it. Negative control: the dark arm (no page)
    /// over white foams the caret under 3:1.
    #[test]
    fn the_light_droplet_keeps_its_caret_on_a_light_page() {
        for page in [0x00FF_FFFF, 0x00FD_F6E3] {
            let (worst, fill, quads) = surged_caret(Some(page), page);
            assert_eq!(quads, 0, "no additive light on a light page");
            assert!(
                worst >= 3.0,
                "#{page:06x}: the caret must keep 3:1, got {worst:.2}:1"
            );
            assert!(
                crate::color_math::contrast_ratio(fill, page) >= CARET_PAGE_MIN_CONTRAST,
                "#{page:06x}: the fill {fill:06x} must clear the page floor"
            );
            let (r, gg, b) = ((fill >> 16) & 0xff, (fill >> 8) & 0xff, fill & 0xff);
            assert!(b >= gg && gg >= r, "still a water hue, got {fill:06x}");
        }
        let (control, _, _) = surged_caret(None, 0x00FF_FFFF);
        assert!(
            control < 3.0,
            "control: the dark arm sinks into white ({control:.2}:1)"
        );
    }

    /// On the dark default page still water is solved up the ramp until the
    /// fill clears the page floor, so the glyph cut out of it in the page
    /// colour stays readable. Negative control: the same tick with no page is
    /// the unsolved deep water under it.
    #[test]
    fn still_water_is_solved_against_a_dark_page() {
        const PAGE: u32 = 0x0011_1318;
        let still = |ground| {
            let mut d = CursorDroplet::default();
            let mut out = Vec::new();
            let c = DropletConfig { ground, ..cfg() };
            d.tick(Some((1, 1)), Instant::now(), 0.0, geom(), &c, &mut out)
                .fill
                .expect("an enabled droplet fills the block")
        };
        let solved = still(Some(PAGE));
        assert!(
            crate::color_math::contrast_ratio(solved, PAGE) >= CARET_PAGE_MIN_CONTRAST,
            "still water {solved:06x} must clear the page floor"
        );
        let unsolved = still(None);
        assert!(
            crate::color_math::contrast_ratio(unsolved, PAGE) < CARET_PAGE_MIN_CONTRAST,
            "control: unsolved still water {unsolved:06x} sits under it"
        );
    }

    /// THE BEAD IS ROUND AND SOFT: at full surge the light across the bead's
    /// middle row climbs to its heart and falls away a little at a time (no
    /// pixel steps by more than a quarter of the peak), and down its middle
    /// column too, where hard-edged slabs once stood it in stairs. Negative
    /// control: a hard slab of the same light steps its whole peak at once.
    #[test]
    fn the_bead_is_round_and_soft() {
        let g = Geom {
            cw: 14,
            ch: 26,
            rows: 8,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 560,
            win_h: 208,
            head: 0,
        };
        let mut d = CursorDroplet::default();
        let mut out = Vec::new();
        d.tick(Some((3, 20)), Instant::now(), 1.0, g, &cfg(), &mut out);
        let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
        let light = |quads: &[GlowQuad]| {
            let mut px = vec![0u32; w * h];
            for q in quads {
                let qw = u32::from(q.w);
                for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                    for i in 0..qw {
                        let x = usize::from(q.x) + i as usize;
                        if x < w {
                            let c = aterm_render::glow_lerp_rgb(q.color, q.color2, i, qw);
                            px[y * w + x] = aterm_render::add_sat(px[y * w + x], c);
                        }
                    }
                }
            }
            px.iter()
                .map(|&p| ((p >> 8) & 0xff) as i32)
                .collect::<Vec<i32>>()
        };
        let lit = light(&out);
        let (cx, cy) = (20 * 14 + 7, 3 * 26 + 12);
        let across: Vec<i32> = (cx - 20..cx + 20).map(|x| lit[cy * w + x]).collect();
        let down: Vec<i32> = (cy - 16..cy + 14).map(|y| lit[y * w + cx]).collect();
        let peak = across.iter().chain(&down).copied().max().unwrap_or(0);
        assert!(peak > 40, "the bead is lit ({peak})");
        let steepest = |l: &[i32]| l.windows(2).map(|p| (p[1] - p[0]).abs()).max().unwrap_or(0);
        assert!(
            steepest(&across) * 4 <= peak && steepest(&down) * 4 <= peak,
            "a hard edge in the bead: {} across, {} down, of {peak}",
            steepest(&across),
            steepest(&down)
        );
        let mut slab = Vec::new();
        push_fx_rect(
            &mut slab,
            g,
            cx as i32 - 8,
            cy as i32 - 8,
            16,
            16,
            premul_rgb(0x0032_DCDE, 92),
        );
        let hard = light(&slab);
        let row: Vec<i32> = (cx - 20..cx + 20).map(|x| hard[cy * w + x]).collect();
        assert!(
            steepest(&row) * 4 > *row.iter().max().unwrap_or(&0),
            "control: a slab's edge is a step"
        );
    }
}
