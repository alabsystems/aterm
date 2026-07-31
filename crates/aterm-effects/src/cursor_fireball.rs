// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The burning FIREBALL cursor — while the `fire` trail style is active the block
//! cursor IS the fire: a molten block fill plus an additive round ball of flame
//! hugging the cell, crowned by flickering tongues licking off its top. The whole
//! thing breathes off the fire's single BLAZE number (typing heat / jump flare,
//! injected by the host from [`crate::cursor_glow::CursorGlow::blaze`]), so the
//! fireball, the flame curtain, and the trail all belong to one burn:
//!
//! * **at rest** — a small smoldering ember: a dim deep-orange ball, a couple of
//!   lazy licks, the block fill a quiet molten orange;
//! * **under the keys** — the ball SWELLS and whitens at the core, the crown
//!   leaps taller and faster, the fill heats toward yellow-white;
//! * **on a jump** — the flare slams the blaze to full, so the landing cursor
//!   erupts white-hot and visibly cools back down through orange to ember.
//!
//! Text-safe by construction, mirroring [`crate::cursor_rainbow`]: the block
//! FILL is returned for the renderer's `floor_cursor_fill` contrast floor (the
//! cut-out glyph stays razor-sharp), and the ball/crown are purely additive
//! [`GlowQuad`] light with capped coverage. Like its siblings it is a CLOCKLESS
//! pure function of an injected `now`, settles to inactive when the blaze dies
//! (the resting ember then rides the blink cadence — no perpetual wakeups), and
//! emits identical premultiplied quads on the CPU and Metal backends.

use web_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::Geom;
use crate::effect_util::{fire_ramp, push_grid_rect as push_rect};

/// Flicker rate in turns/second: a live coal at rest, a roaring shimmer at full
/// blaze. The phase only advances when frames render, so a settled ember
/// flickers at the blink cadence for free.
const FLICK_IDLE: f32 = 1.6;
const FLICK_ACTIVE: f32 = 6.5;

/// Ball radius in cell-heights: the ball must SWALLOW the block (its diameter at
/// least the cell height), or the block's square corners poke out and the whole
/// thing reads as a coloured rectangle, not a fireball. Still under a cell of
/// reach past the cell edge so the light hugs the cursor.
const RADIUS_IDLE: f32 = 0.52;
const RADIUS_MAX: f32 = 0.80;

/// Innermost-core coverage (pre-intensity): dim ember → hot heart. The concentric
/// discs below scale down from this, and every push is additionally clamped by
/// [`COV_CAP`] so the stacked additive light can never bury a neighbouring glyph.
const COV_IDLE: f32 = 64.0;
const COV_MAX: f32 = 150.0;
/// Per-quad additive coverage ceiling — matches the flame curtain's text-safety
/// band (the readable-at-full-blaze live-review tuning). Inner discs saturate
/// here (they sit over the cursor cell itself); the wide rim quads that actually
/// overlap neighbouring glyphs run at a fraction of the core and stay a tint.
const COV_CAP: f32 = 92.0;

/// Crown (flame-tongue) height in cell-heights over the ball's top arc: lazy
/// idle licks, a full row of leaping flame at max blaze.
const CROWN_IDLE: f32 = 0.50;
const CROWN_MAX: f32 = 1.55;

/// The blaze below which the fireball is SETTLED — the animator reports itself
/// inactive so the host stops arming the 60 fps tick and the resting ember rides
/// the blink cadence.
const SETTLED_BLAZE: f32 = 0.02;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct FireballConfig {
    /// Master on/off (the `fire` style opted in AND the cursor is a focused,
    /// visible block).
    pub enabled: bool,
    /// Overall scale `0..1` — the reduced-motion / load-shed amplitude, folded in
    /// by the host exactly like the aurora. 0 ⇒ fully inert (plain themed cursor).
    pub intensity: f32,
}

/// What a tick produced: the molten block FILL to hand the renderer (it floors it
/// for contrast) and a fingerprint that changes on every visible step (0 when off).
#[derive(Clone, Copy, Debug)]
pub struct FireballFrame {
    /// The molten block-fill colour `0x00RRGGBB`, or `None` when the fireball is
    /// off (the renderer then keeps the ordinary themed cursor fill).
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + ball + crown (0 ⇒ nothing this frame).
    pub fp: u64,
}

/// Per-window fireball animation state — one flicker phase plus the last clock
/// reading and a latched blaze. Tiny + Copy-cheap.
#[derive(Default)]
pub struct CursorFireball {
    /// Rolling flicker phase in turns `0..1`.
    flick: f32,
    last: Option<Instant>,
    /// Latched blaze at the last tick (so [`is_active`] answers without a clock).
    blaze: f32,
}

impl CursorFireball {
    /// Whether the host must keep arming the animation tick: only while the fire
    /// is still CHARGED (typing or a jump flare cooling). Once the blaze settles
    /// the resting ember rides the blink cadence at no extra idle cost.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.blaze > SETTLED_BLAZE
    }

    /// Advance one frame at `now` with the fire's current `blaze` (`0..1`, the
    /// host reads [`crate::cursor_glow::CursorGlow::blaze`] after the aurora
    /// tick), the block cursor cell `cur` (`None` ⇒ hidden), grid `geom`, and the
    /// resolved `cfg`. Appends the additive fireball to `out` and returns the
    /// molten block FILL + a fingerprint. Pure: no wall-clock, unit-testable by
    /// injecting `now`/`blaze`.
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        blaze: f32,
        geom: Geom,
        cfg: &FireballConfig,
        out: &mut Vec<GlowQuad>,
    ) -> FireballFrame {
        let e = (blaze.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        // Fully inert — byte-identical to the plain themed cursor — when off, when
        // the geometry is degenerate, or when the amplitude is zero (reduced
        // motion / load-shed), mirroring the rainbow cursor's "0 ⇒ off" contract.
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.blaze = 0.0; // inert: report settled so the host disarms the tick
            self.last = Some(now);
            return FireballFrame { fill: None, fp: 0 };
        }
        self.blaze = e;

        // Advance the flicker by real dt (clamped so a long stall — e.g. the
        // window slept — doesn't fling the phase). Faster the hotter it burns.
        let dt = self
            .last
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            .unwrap_or(0.0)
            .min(0.25);
        self.last = Some(now);
        self.flick = (self.flick + dt * (FLICK_IDLE + FLICK_ACTIVE * e)).fract();
        let phase = self.flick * std::f32::consts::TAU;
        // The shared flicker envelope 0..1 — two incommensurate sines so the
        // shimmer never reads as a metronome pulse.
        let flicker = 0.5 + 0.35 * phase.sin() + 0.15 * (phase * 2.7 + 1.3).sin();

        // The molten BLOCK FILL: deep ember at rest, bright orange at full blaze,
        // shimmering slightly with the flame. Deliberately capped BELOW the
        // additive core's white — if the rectangular fill outshines the round
        // ball around it, the eye reads a bright BLOCK with a halo instead of a
        // fireball; keeping the fill in the ball body's hue band lets the round
        // white-hot core (additive, below) be the brightest thing on screen. The
        // renderer floors this against the cell bg, so the glyph stays sharp.
        let fill = fire_ramp(0.40 + 0.30 * e + 0.05 * (flicker - 0.5));

        // The additive fireball, drawn only over a visible in-grid cursor cell.
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
        {
            self.emit_ball(cr, cc, e, flicker, phase, geom, cfg, out);
        }

        // Fingerprint: quantized phase + blaze + fill, so a settled ember
        // early-outs the present but any visible step forces a repaint.
        let fp = ((self.flick * 512.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add((e * 255.0) as u64)
            .wrapping_add((fill as u64) << 12)
            | 1; // never 0 while enabled — the ember is always drawable

        FireballFrame {
            fill: Some(fill),
            fp,
        }
    }

    /// Emit the ball + crown as premultiplied additive quads. Bounded by
    /// construction: ≤ ~4 discs × (2·radius / slab) slabs + one column pass.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + blaze/flicker/phase + geometry/config; one internal call site"
    )]
    fn emit_ball(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        flicker: f32,
        phase: f32,
        geom: Geom,
        cfg: &FireballConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        // Ball centre: mid-cell, sunk slightly low so the crown has headroom
        // above the glyph row.
        let cx = (cc as f32 + 0.5) * cw;
        let cy = (cr as f32 + 0.58) * ch;
        let radius = (RADIUS_IDLE + (RADIUS_MAX - RADIUS_IDLE) * e) * ch;
        let cov_core =
            (COV_IDLE + (COV_MAX - COV_IDLE) * e) * cfg.intensity * (0.82 + 0.18 * flicker);

        // Concentric discs, outermost first: deep-red rim → orange body →
        // yellow flame → the white-hot heart (present only as the blaze climbs).
        // Additive overlap builds the radial gradient without per-pixel math.
        let discs: [(f32, f32, f32); 4] = [
            // (radius ×, coverage ×, fire_ramp t)
            (1.00, 0.32, 0.16 + 0.06 * e),
            (0.82, 0.60, 0.40 + 0.14 * e),
            (0.62, 0.85, 0.62 + 0.20 * e),
            (0.40, 1.00, 0.84 + 0.16 * e),
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
            let premul = premul_rgb(fire_ramp(ramp_t), cov);
            let r_i = r as i32;
            let mut dy = -r_i;
            while dy < r_i {
                let h = slab.min(r_i - dy);
                // Sample the disc's half-width at the slab's vertical centre; a
                // small travelling wobble on the edge keeps the silhouette alive
                // (a perfect circle reads as a sticker, not a flame).
                let ym = dy as f32 + h as f32 * 0.5;
                let base = (r * r - ym * ym).max(0.0).sqrt();
                let wobble = 1.0 + 0.08 * (ym * 0.55 / ch.max(1.0) * 12.0 + phase * 1.7).sin();
                // TEARDROP taper: the upper half narrows toward the crown so the
                // ball merges into the tongues — a flame's egg, not a billiard ball.
                let taper = if ym < 0.0 { 1.0 + 0.22 * ym / r } else { 1.0 };
                let hw = (base * wobble * taper) as i32;
                if hw >= 1 {
                    push_rect(out, geom, cx as i32 - hw, cy as i32 + dy, 2 * hw, h, premul);
                }
                dy += h;
            }
        }

        // The CROWN: flame tongues licking up off the ball's upper arc — narrow
        // columns sampling a travelling multi-frequency silhouette in absolute
        // pixel x (the same trick as the flame curtain, so the two read as one
        // fire). Height and heat ride the blaze.
        let crown_peak = (CROWN_IDLE + (CROWN_MAX - CROWN_IDLE) * e) * ch * (0.75 + 0.25 * flicker);
        let stride = ((cw * 0.25) as i32).max(2);
        let span = (radius * 0.85) as i32;
        // Tongue roots run as hot as the ball's flame disc so the crown grows
        // OUT of the ball's body (a dim root reads as smoke hovering over it);
        // only the tips die down into deep red.
        let lower = premul_rgb(
            fire_ramp(0.64 + 0.16 * e),
            (cov_core * 0.90).min(COV_CAP) as u8,
        );
        let upper = premul_rgb(fire_ramp(0.38), (cov_core * 0.50).min(COV_CAP) as u8);
        let mut dx = -span;
        while dx < span {
            let w = stride.min(span - dx);
            let x = cx as i32 + dx;
            let xm = dx as f32 + w as f32 * 0.5;
            // Root each tongue on the ball's upper arc so the crown grows out of
            // the ball, not off a flat line floating above it.
            let arc = (radius * radius - xm * xm).max(0.0).sqrt() * 0.85;
            let root_y = cy as i32 - arc as i32;
            // Travelling silhouette: the big rolling lick, a counter-wave, and a
            // fine flicker — power-curved so the peaks sharpen into points.
            let fx = (x as f32 + w as f32 * 0.5) / cw.max(1.0);
            let t1 = (fx * 0.9 - phase * 1.9).sin();
            let t2 = (fx * 2.1 + phase * 1.2 + 1.7).sin();
            let t3 = (fx * 4.3 - phase * 3.1 + 4.2).sin();
            let ridge = (0.60 + 0.30 * t1 + 0.18 * t2 + 0.08 * t3).clamp(0.05, 1.15);
            // Tongues shrink toward the ball's shoulders so the crown is a peak,
            // not a flat-topped wall.
            let shoulder = 1.0 - (xm / span.max(1) as f32).powi(2) * 0.55;
            let h = (crown_peak * ridge.powf(1.4) * shoulder) as i32;
            if h >= 1 {
                let h_low = (h as f32 * 0.55) as i32;
                if h_low > 0 {
                    push_rect(out, geom, x, root_y - h_low, w, h_low, lower);
                }
                if h > h_low {
                    push_rect(out, geom, x, root_y - h, w, h - h_low, upper);
                }
            }
            dx += w;
        }
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
    fn cfg() -> FireballConfig {
        FireballConfig {
            enabled: true,
            intensity: 1.0,
        }
    }

    /// Disabled ⇒ no fill, no ball, fp 0 (byte-identical to the plain cursor).
    #[test]
    fn disabled_is_inert() {
        let mut fb = CursorFireball::default();
        let mut out = Vec::new();
        let f = fb.tick(
            Some((1, 1)),
            Instant::now(),
            1.0,
            geom(),
            &FireballConfig {
                enabled: false,
                intensity: 1.0,
            },
            &mut out,
        );
        assert!(f.fill.is_none());
        assert_eq!(f.fp, 0);
        assert!(out.is_empty());
        assert!(!fb.is_active());
    }

    /// Reduced motion / load-shed (`intensity == 0`) ⇒ fully inert even at full blaze.
    #[test]
    fn zero_intensity_is_inert() {
        let mut fb = CursorFireball::default();
        let mut out = Vec::new();
        let f = fb.tick(
            Some((1, 1)),
            Instant::now(),
            1.0,
            geom(),
            &FireballConfig {
                enabled: true,
                intensity: 0.0,
            },
            &mut out,
        );
        assert!(f.fill.is_none(), "reduced motion keeps the plain cursor");
        assert_eq!(f.fp, 0);
        assert!(out.is_empty());
        assert!(!fb.is_active());
    }

    /// A hidden cursor draws no ball, but the fill stays resolved (harmless — the
    /// renderer draws no cursor to fill) and the ember still reports its state.
    #[test]
    fn hidden_cursor_draws_no_ball() {
        let mut fb = CursorFireball::default();
        let mut out = Vec::new();
        let f = fb.tick(None, Instant::now(), 0.5, geom(), &cfg(), &mut out);
        assert!(out.is_empty(), "no additive light without a cursor cell");
        assert!(f.fill.is_some());
    }

    /// Full blaze swells the ball (taller extent) and burns far brighter than the
    /// resting ember — the progressive-ignition read.
    #[test]
    fn blaze_swells_and_brightens() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let run = |blaze: f32| -> (u64, i32, i32) {
            let mut fb = CursorFireball::default();
            let mut out = Vec::new();
            fb.tick(Some((3, 20)), t, blaze, g, &c, &mut out);
            out.clear();
            fb.tick(
                Some((3, 20)),
                t + Duration::from_millis(16),
                blaze,
                g,
                &c,
                &mut out,
            );
            let y0 = out.iter().map(|q| q.y as i32).min().unwrap_or(0);
            let y1 = out.iter().map(|q| (q.y + q.h) as i32).max().unwrap_or(0);
            (ink(&out), y0, y1)
        };
        let (ember_ink, ember_y0, _) = run(0.0);
        let (blaze_ink, blaze_y0, _) = run(1.0);
        assert!(ember_ink > 0, "the resting ember is still visibly lit");
        assert!(
            blaze_ink > ember_ink * 2,
            "full blaze far outshines the ember ({blaze_ink} vs {ember_ink})"
        );
        assert!(
            blaze_y0 < ember_y0,
            "the hot crown licks higher ({blaze_y0} vs {ember_y0})"
        );
    }

    /// The molten fill heats from ember-orange toward white-hot with the blaze.
    #[test]
    fn fill_heats_with_blaze() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let mut out = Vec::new();
        let mut ember = CursorFireball::default();
        let f0 = ember
            .tick(Some((1, 1)), t, 0.0, g, &c, &mut out)
            .fill
            .unwrap();
        let mut hot = CursorFireball::default();
        let f1 = hot
            .tick(Some((1, 1)), t, 1.0, g, &c, &mut out)
            .fill
            .unwrap();
        let bright = |c: u32| ((c >> 16) & 0xff) + ((c >> 8) & 0xff) + (c & 0xff);
        assert!(
            bright(f1) > bright(f0) + 100,
            "blaze whitens the fill ({f1:#08x} vs {f0:#08x})"
        );
        // Both stay in fire hues: red channel dominant.
        for f in [f0, f1] {
            let (r, gg, b) = ((f >> 16) & 0xff, (f >> 8) & 0xff, f & 0xff);
            assert!(r >= gg && gg >= b, "fire hue ordering, got {f:#08x}");
        }
    }

    /// Every emitted quad is single-row, grid-interior, and fire-hued with capped
    /// coverage (the renderer row-gate / parity / legibility invariants).
    #[test]
    fn quads_respect_grid_hue_and_cap() {
        let g = geom();
        let c = cfg();
        let mut fb = CursorFireball::default();
        let mut out = Vec::new();
        let t = Instant::now();
        // A corner cell so clamping is exercised.
        fb.tick(Some((0, 0)), t, 1.0, g, &c, &mut out);
        fb.tick(
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
                r + 1 >= gg && gg + 1 >= b,
                "premultiplied fire hue ordering: {q:?}"
            );
            for sh in [16u32, 8, 0] {
                assert!(
                    (q.color >> sh) & 0xff <= COV_CAP as u32 + 1,
                    "coverage capped: {q:?}"
                );
            }
        }
    }

    /// Charged ⇒ active (host keeps the tick armed); settled blaze ⇒ inactive so
    /// the resting ember rides the blink cadence at no extra wakeup cost.
    #[test]
    fn settles_to_inactive_when_blaze_dies() {
        let g = geom();
        let c = cfg();
        let mut fb = CursorFireball::default();
        let mut out = Vec::new();
        let t = Instant::now();
        fb.tick(Some((1, 1)), t, 0.8, g, &c, &mut out);
        assert!(fb.is_active(), "a charged fireball keeps the tick armed");
        fb.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.0,
            g,
            &c,
            &mut out,
        );
        assert!(
            !fb.is_active(),
            "the settled ember idles on the blink cadence"
        );
    }

    /// The per-frame quad budget is bounded by construction (discs + one column
    /// pass), far under the aurora's MAX_QUADS defence.
    #[test]
    fn quad_count_is_bounded() {
        let g = Geom {
            cw: 20,
            ch: 40,
            rows: 50,
            cols: 200,
            origin_x: 0,
            origin_y: 0,
            win_w: 4000,
            win_h: 2000,
            head: 0,
        };
        let mut fb = CursorFireball::default();
        let mut out = Vec::new();
        fb.tick(Some((25, 100)), Instant::now(), 1.0, g, &cfg(), &mut out);
        assert!(
            out.len() < 400,
            "bounded fireball geometry, got {}",
            out.len()
        );
    }
}
