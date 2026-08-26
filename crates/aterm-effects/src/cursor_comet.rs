// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The COMET NUCLEUS cursor — while the `comet` trail style is active the block
//! cursor IS the comet: a frosted block fill wrapped in a round additive COMA of
//! icy light, with a handful of facet GLINTS twinkling on the coma's rim as it
//! slowly turns. The whole thing breathes off the aurora's single BLAZE number
//! (typing heat / jump flare, injected by the host from
//! [`crate::cursor_glow::CursorGlow::blaze`]), so the nucleus, the coma-halo
//! crown, and the icy dust tail all belong to one comet:
//!
//! * **at rest** — a cold drifting nucleus: a dim blue-white coma, slow facet
//!   glints, the block fill a quiet frosted tint of the trail hue;
//! * **under the keys** — the coma SWELLS and whitens at the core (fresh ice in
//!   sunlight), the glints spin faster and flash brighter, the fill frosts
//!   toward white;
//! * **on a jump** — the flare slams the blaze to full, so the landing nucleus
//!   ignites white-hot and visibly refreezes back through the trail hue.
//!
//! Text-safe by construction, mirroring [`crate::cursor_fireball`]: the block
//! FILL is returned for the renderer's `floor_cursor_fill` contrast floor (the
//! cut-out glyph stays razor-sharp), and the coma/glints are purely additive
//! [`GlowQuad`] light with capped coverage. Like its siblings it is a CLOCKLESS
//! pure function of an injected `now`, settles to inactive when the blaze dies
//! (the resting nucleus then rides the blink cadence — no perpetual wakeups),
//! and emits identical premultiplied quads on the CPU and Metal backends.

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::Geom;
use crate::effect_util::{lerp_rgb, push_grid_rect as push_rect};

/// Shimmer rate in turns/second: a slow glacial drift at rest, a lively glitter
/// at full blaze — deliberately calmer than the fireball's flicker (ice glints,
/// it doesn't roar). The phase only advances when frames render, so a settled
/// nucleus shimmers at the blink cadence for free.
const SHIMMER_IDLE: f32 = 0.7;
const SHIMMER_ACTIVE: f32 = 3.2;

/// Coma radius in cell-heights: the coma must SWALLOW the block (diameter at
/// least the cell height), or the block's square corners poke out and the whole
/// thing reads as a coloured rectangle, not a comet head. Still under a cell of
/// reach past the cell edge so the light hugs the cursor.
const RADIUS_IDLE: f32 = 0.54;
const RADIUS_MAX: f32 = 0.82;

/// Innermost-core coverage (pre-intensity): cold drift → sunlit ice. The
/// concentric discs below scale down from this, and every push is additionally
/// clamped by [`COV_CAP`] so the stacked additive light can never bury a
/// neighbouring glyph.
const COV_IDLE: f32 = 58.0;
const COV_MAX: f32 = 148.0;
/// Per-quad additive coverage ceiling — the same text-safety band as the
/// fireball and the flame curtain (the readable-at-full-blaze tuning). Inner
/// discs saturate here (they sit over the cursor cell itself); the wide fringe
/// quads that actually overlap neighbouring glyphs run at a fraction and stay
/// a tint.
const COV_CAP: f32 = 92.0;

/// Facet glints twinkling on the coma rim — tiny 4-point sparks that read as
/// sunlight catching ice crystals as the nucleus turns.
const GLINTS: u32 = 4;

/// The blaze below which the nucleus is SETTLED — the animator reports itself
/// inactive so the host stops arming the 60 fps tick and the resting nucleus
/// rides the blink cadence.
const SETTLED_BLAZE: f32 = 0.02;

/// The near-white the ice freezes toward — deliberately short of pure white so
/// the coma's stacked core (additive) stays the brightest thing on screen.
const ICE_WHITE: u32 = 0x00F2_FAFF;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct CometConfig {
    /// Master on/off (the `comet` style opted in AND the cursor is a focused,
    /// visible block).
    pub enabled: bool,
    /// Overall scale `0..1` — the reduced-motion / load-shed amplitude, folded in
    /// by the host exactly like the aurora. 0 ⇒ fully inert (plain themed cursor).
    pub intensity: f32,
    /// The trail's base hue (`GlowConfig::color`) — the coma body freezes off
    /// this, so nucleus and tail always read as one comet.
    pub color: u32,
    /// The trail's accent hue (`GlowConfig::accent`) — the coma's outer fringe.
    pub accent: u32,
}

/// What a tick produced: the frosted block FILL to hand the renderer (it floors
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

/// Per-window nucleus animation state — one shimmer phase plus the last clock
/// reading and a latched blaze. Tiny + Copy-cheap.
#[derive(Default)]
pub struct CursorComet {
    /// Rolling shimmer phase in turns `0..1`.
    shimmer: f32,
    last: Option<Instant>,
    /// Latched blaze at the last tick (so [`is_active`] answers without a clock).
    blaze: f32,
}

impl CursorComet {
    /// Whether the host must keep arming the animation tick: only while the
    /// comet is still CHARGED (typing or a jump flare refreezing). Once the
    /// blaze settles the resting nucleus rides the blink cadence at no extra
    /// idle cost.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.blaze > SETTLED_BLAZE
    }

    /// Advance one frame at `now` with the aurora's current `blaze` (`0..1`, the
    /// host reads [`crate::cursor_glow::CursorGlow::blaze`] after the aurora
    /// tick), the block cursor cell `cur` (`None` ⇒ hidden), grid `geom`, and the
    /// resolved `cfg`. Appends the additive coma to `out` and returns the
    /// frosted block FILL + a fingerprint. Pure: no wall-clock, unit-testable by
    /// injecting `now`/`blaze`.
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
        // Fully inert — byte-identical to the plain themed cursor — when off,
        // when the geometry is degenerate, or when the amplitude is zero
        // (reduced motion / load-shed), mirroring the fireball's "0 ⇒ off"
        // contract.
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.blaze = 0.0; // inert: report settled so the host disarms the tick
            self.last = Some(now);
            return CometFrame { fill: None, fp: 0 };
        }
        self.blaze = e;

        // Advance the shimmer by real dt (clamped so a long stall — e.g. the
        // window slept — doesn't fling the phase). Livelier the hotter it burns.
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

        // The frosted BLOCK FILL: a cool tint of the trail hue at rest, frosting
        // toward white as the blaze climbs. Deliberately capped BELOW the
        // additive core's near-white — if the rectangular fill outshines the
        // round coma around it, the eye reads a bright BLOCK with a halo instead
        // of a comet head; keeping the fill inside the coma's hue band lets the
        // round sunlit core (additive, below) be the brightest thing on screen.
        // The renderer floors this against the cell bg, so the glyph stays sharp.
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
        // Coma centre: mid-cell. A comet's coma is round — the DIRECTION lives
        // in the dust tail behind it, not in the head's silhouette.
        let cx = (cc as f32 + 0.5) * cw;
        let cy = (cr as f32 + 0.5) * ch;
        let radius = (RADIUS_IDLE + (RADIUS_MAX - RADIUS_IDLE) * e) * ch * (0.96 + 0.06 * breathe);
        let cov_core =
            (COV_IDLE + (COV_MAX - COV_IDLE) * e) * cfg.intensity * (0.86 + 0.14 * breathe);

        // Concentric discs, outermost first: accent fringe → hue body → sunlit
        // ice → the near-white heart (present only as the blaze climbs).
        // Additive overlap builds the radial gradient without per-pixel math.
        // (radius ×, coverage ×, ramp t) — ramp t maps through `ice_ramp`.
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
                // The outermost fringe carries the ACCENT — the gas envelope a
                // shade apart from the dust, so the head reads layered.
                lerp_rgb(cfg.accent, ice_ramp(cfg.color, ramp_t), 0.45)
            } else {
                ice_ramp(cfg.color, ramp_t)
            };
            let premul = premul_rgb(color, cov);
            let r_i = r as i32;
            let mut dy = -r_i;
            while dy < r_i {
                let h = slab.min(r_i - dy);
                // Sample the disc's half-width at the slab's vertical centre; a
                // small travelling wobble keeps the silhouette alive (a perfect
                // circle reads as a sticker, not a glowing gas envelope).
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

        // FACET GLINTS: a handful of tiny 4-point sparks riding the coma rim,
        // slowly orbiting with the shimmer phase, each twinkling on its own
        // offset — sunlight catching ice crystals as the nucleus turns. Kept
        // OFF the glyph row's centre band by riding at ~0.72·radius, and each
        // spark is a couple of hairline rects, so they never bury a letter.
        let glint_r = radius * 0.72;
        for i in 0..GLINTS {
            let fi = i as f32;
            let ang = fi / GLINTS as f32 * std::f32::consts::TAU + phase * 0.55;
            let tw = 0.5 + 0.5 * (phase * 2.0 + fi * 2.4).sin();
            // Only the bright half of each twinkle draws — glints FLASH, they
            // don't hover.
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

/// The ICE ramp off the trail's base hue, `t` 0 (cold, dim) → 1 (sunlit,
/// near-white): dimmed hue → pure hue → whitened hue → [`ICE_WHITE`]. Derived
/// from the config colour (never a fixed palette) so an explicit
/// `cursor_trail_color` re-tints the whole nucleus with the tail.
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
    /// Disabled ⇒ no fill, no coma, fp 0 (byte-identical to the plain cursor).
    #[test]
    fn disabled_is_inert() {
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        let f = nc.tick(
            Some((1, 1)),
            Instant::now(),
            1.0,
            geom(),
            &CometConfig {
                enabled: false,
                ..cfg()
            },
            &mut out,
        );
        assert!(f.fill.is_none());
        assert_eq!(f.fp, 0);
        assert!(out.is_empty());
        assert!(!nc.is_active());
    }

    /// Reduced motion / load-shed (`intensity == 0`) ⇒ fully inert even at full blaze.
    #[test]
    fn zero_intensity_is_inert() {
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        let f = nc.tick(
            Some((1, 1)),
            Instant::now(),
            1.0,
            geom(),
            &CometConfig {
                intensity: 0.0,
                ..cfg()
            },
            &mut out,
        );
        assert!(f.fill.is_none(), "reduced motion keeps the plain cursor");
        assert_eq!(f.fp, 0);
        assert!(out.is_empty());
        assert!(!nc.is_active());
    }

    /// A hidden cursor draws no coma, but the fill stays resolved (harmless —
    /// the renderer draws no cursor to fill) and the state still reports.
    #[test]
    fn hidden_cursor_draws_no_coma() {
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        let f = nc.tick(None, Instant::now(), 0.5, geom(), &cfg(), &mut out);
        assert!(out.is_empty(), "no additive light without a cursor cell");
        assert!(f.fill.is_some());
    }

    /// Full blaze swells the coma (wider extent) and shines far brighter than
    /// the cold drift — the sunlit-ice read.
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

    /// The frosted fill whitens from the cold hue toward sunlit ice with the blaze.
    #[test]
    fn fill_frosts_with_blaze() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let mut out = Vec::new();
        let mut cold = CursorComet::default();
        let f0 = cold
            .tick(Some((1, 1)), t, 0.0, g, &c, &mut out)
            .fill
            .unwrap();
        let mut hot = CursorComet::default();
        let f1 = hot
            .tick(Some((1, 1)), t, 1.0, g, &c, &mut out)
            .fill
            .unwrap();
        let bright = |c: u32| ((c >> 16) & 0xff) + ((c >> 8) & 0xff) + (c & 0xff);
        assert!(
            bright(f1) > bright(f0) + 60,
            "blaze frosts the fill toward white ({f1:#08x} vs {f0:#08x})"
        );
        // Both stay in the icy hue band: blue channel dominant.
        for f in [f0, f1] {
            let (r, gg, b) = ((f >> 16) & 0xff, (f >> 8) & 0xff, f & 0xff);
            assert!(b >= gg && gg >= r, "icy hue ordering, got {f:#08x}");
        }
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

    /// Charged ⇒ active (host keeps the tick armed); settled blaze ⇒ inactive so
    /// the resting nucleus rides the blink cadence at no extra wakeup cost.
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

    /// The per-frame quad budget is bounded by construction (discs + a handful
    /// of glint crosses), far under the aurora's MAX_QUADS defence.
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
        let mut nc = CursorComet::default();
        let mut out = Vec::new();
        nc.tick(Some((25, 100)), Instant::now(), 1.0, g, &cfg(), &mut out);
        assert!(out.len() < 400, "bounded coma geometry, got {}", out.len());
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
}
