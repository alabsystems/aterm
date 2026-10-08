// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The light-rod cursor: every trail style's shape-completion seam. The host
//! tints it with the active style's resolved colour and a per-style haze, so
//! each trail has a coherent cursor in both shapes (styles with bespoke block
//! bodies get their bar from the rod; styles without one also get the
//! charged-emitter block). The treatment is keyed by the live cursor shape,
//! resolved per frame:
//!
//! * **Bar** (DECSCUSR bar / `cursor_style beam`): a vertical rod of light, a
//!   white-hot axis inside a haze sleeve ([`BEAM_SPACE_HAZE`] for beam, shared
//!   with the trail), overshooting the cell slightly. It breathes at rest and
//!   flares with the aurora's blaze. The bar keeps its shape; the light is
//!   purely additive.
//! * **Block**: a charged emitter. The fill locks to the style's hue
//!   (whitening with blaze, contrast-floored by the renderer) inside a soft
//!   aura that fades into the haze at its rim.
//!
//! Text-safe like [`crate::cursor_rainbow`]: the block fill rides the
//! renderer's `floor_cursor_fill` contrast floor, and all rod/aura light is
//! additive [`GlowQuad`]s with capped coverage. A clockless pure function of an
//! injected `now`; it goes inactive when the blaze dies (the resting rod rides
//! the blink cadence) and emits identical premultiplied quads on CPU and Metal.

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::Geom;
use crate::effect_util::push_fx_rect as push_rect;

/// Breathing rate in turns/second: a slow, calm shimmer. The phase advances
/// only when frames render, so a settled rod breathes at the blink cadence.
const BREATHE_IDLE: f32 = 0.55;
const BREATHE_ACTIVE: f32 = 1.6;

/// Rod overshoot past the cell's top/bottom edges, in cell-heights: a resting
/// filament hugs the cell; a charged one extends like a powered rod.
const OVERSHOOT_IDLE: f32 = 0.06;
const OVERSHOOT_MAX: f32 = 0.22;

/// Axis coverage (pre-intensity): a quiet glow at rest → bright under the
/// keys. The sleeve layers scale down from this, and every push is clamped by
/// [`COV_CAP`] so the stacked additive light never buries a neighbour glyph.
const COV_IDLE: f32 = 70.0;
const COV_MAX: f32 = 150.0;
/// Per-quad additive coverage ceiling (the fire curtain's text-safety band).
/// The thin axis saturates here over the cursor's own column; wider sleeve
/// quads that reach a neighbouring glyph run at a fraction of it.
const COV_CAP: f32 = 92.0;

/// The blaze below which the rod is settled: the animator reports inactive so
/// the host stops the 60 fps tick and the resting glow rides the blink cadence.
const SETTLED_BLAZE: f32 = 0.02;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct BeamRodConfig {
    /// Master on/off (the `beam` style opted in AND the cursor is a focused,
    /// visible bar or block).
    pub enabled: bool,
    /// Overall scale `0..1` — the reduced-motion / load-shed amplitude, folded
    /// in by the host exactly like the aurora. 0 ⇒ fully inert.
    pub intensity: f32,
    /// The style's resolved hue `0x00RRGGBB` (`GlowConfig::color`), so the rod,
    /// the fill and the trail are one light.
    pub color: u32,
    /// The sleeve colour `0x00RRGGBB` the outer rod layers and aura rim mix
    /// toward: beam's indigo `BEAM_SPACE_HAZE`, otherwise a deepened shade of
    /// the style's own hue.
    pub haze: u32,
    /// Whether the live cursor is a thin BAR (rod treatment) rather than a
    /// block (emitter treatment).
    pub bar: bool,
    /// Sparkle's twinkle: the breathing runs faster and deeper so the
    /// emitter glitters instead of breathing. Everyone else passes `false`.
    pub shimmer: bool,
}

/// What a tick produced: the emitter block fill for the renderer (`None` for
/// the bar, which has no fill-override channel) and a fingerprint that changes
/// on every visible step (0 when off).
#[derive(Clone, Copy, Debug)]
pub struct BeamRodFrame {
    /// The emitter block-fill colour `0x00RRGGBB`, or `None` when off / bar.
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + rod light (0 ⇒ nothing this frame).
    pub fp: u64,
}

/// Per-window light-rod animation state — one breathing phase plus the last
/// clock reading and a latched blaze. Tiny + Copy-cheap.
#[derive(Default)]
pub struct CursorBeamRod {
    /// Rolling breathing phase in turns `0..1`.
    breathe: f32,
    last: Option<Instant>,
    /// Latched blaze at the last tick (so [`is_active`] answers without a clock).
    blaze: f32,
}

impl CursorBeamRod {
    /// Whether the host must keep arming the animation tick: only while the
    /// beam is charged (typing or a jump flare cooling).
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.blaze > SETTLED_BLAZE
    }

    /// Advance one frame at `now` with the current `blaze` (`0..1`, read from
    /// [`crate::cursor_glow::CursorGlow::blaze`] after the aurora tick), cursor
    /// cell `cur` (`None` ⇒ hidden), grid `geom`, and resolved `cfg`. Appends
    /// the additive rod/aura to `out` and returns the emitter fill and a
    /// fingerprint. Pure.
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        blaze: f32,
        geom: Geom,
        cfg: &BeamRodConfig,
        out: &mut Vec<GlowQuad>,
    ) -> BeamRodFrame {
        let e = (blaze.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        // Inert (identical to the plain themed cursor) when off, when the
        // geometry is degenerate, or at zero amplitude (reduced motion / shed).
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.blaze = 0.0; // inert: report settled so the host disarms the tick
            self.last = Some(now);
            return BeamRodFrame { fill: None, fp: 0 };
        }
        self.blaze = e;

        // Advance the breathing by real dt (clamped so a long stall — e.g. the
        // window slept — doesn't fling the phase). Faster while charged.
        let dt = self
            .last
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            .unwrap_or(0.0)
            .min(0.25);
        self.last = Some(now);
        let rate_mul = if cfg.shimmer { 4.5 } else { 1.0 };
        self.breathe = (self.breathe + dt * (BREATHE_IDLE + BREATHE_ACTIVE * e) * rate_mul).fract();
        let phase = self.breathe * std::f32::consts::TAU;
        // A single slow sine: steady light breathes, never strobes.
        let breathe = 0.5 + 0.5 * phase.sin();

        // Block fill: the style's hue, whitening as the blaze climbs, capped
        // below pure white so the additive axis stays brightest. The renderer
        // floors it against the cell background.
        let fill = (!cfg.bar).then(|| {
            let k = 0.10 + 0.35 * e;
            let mix = |sh: u32| {
                let c = ((cfg.color >> sh) & 0xff) as f32;
                (c + (255.0 - c) * k).min(255.0) as u32
            };
            (mix(16) << 16) | (mix(8) << 8) | mix(0)
        });

        // The additive rod/aura, drawn only over a visible in-grid cursor cell.
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
        {
            if cfg.bar {
                self.emit_rod(cr, cc, e, breathe, geom, cfg, out);
            } else {
                self.emit_aura(cr, cc, e, breathe, geom, cfg, out);
            }
        }

        // Fingerprint: quantized phase + blaze + fill, so a settled rod
        // early-outs the present but any visible step forces a repaint.
        let fp = ((self.breathe * 512.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add((e * 255.0) as u64)
            .wrapping_add((fill.unwrap_or(cfg.color) as u64) << 12)
            | 1; // never 0 while enabled — the resting glow is always drawable

        BeamRodFrame { fill, fp }
    }

    /// The bar treatment: a vertical rod of light on the bar's own x, a
    /// white-hot axis inside a cool sleeve, overshooting the cell slightly.
    /// Bounded: 4 layers × ≤3 vertical segments each.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + blaze/breathe + geometry/config; one internal call site"
    )]
    fn emit_rod(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        breathe: f32,
        geom: Geom,
        cfg: &BeamRodConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        // WINDOW px: everything below is grid-relative + origin.
        let (ox, oy) = (geom.origin_x as f32, geom.origin_y as f32);
        // The rod's axis sits ON the bar: the renderer draws the bar hugging
        // the cell's left edge, so centre the light just inside it.
        let ax = ox + (cc as f32) * cw + cw * 0.10;
        let over =
            (OVERSHOOT_IDLE + (OVERSHOOT_MAX - OVERSHOOT_IDLE) * e) * ch * (0.85 + 0.15 * breathe);
        let y0 = oy + (cr as f32) * ch - over;
        let h = ch + 2.0 * over;
        let cov_axis =
            (COV_IDLE + (COV_MAX - COV_IDLE) * e) * cfg.intensity * (0.90 + 0.10 * breathe);
        const WHITE: u32 = 0x00FF_FFFF;
        let lerp = |a: u32, b: u32, t: f32| -> u32 {
            let m = |sh: u32| {
                let x = ((a >> sh) & 0xff) as f32;
                let y = ((b >> sh) & 0xff) as f32;
                (x + (y - x) * t).min(255.0) as u32
            };
            (m(16) << 16) | (m(8) << 8) | m(0)
        };
        // (half-width in cell-widths, coverage ×, mix target, mix amount),
        // axis first so a saturated quad budget sheds sleeve, never filament.
        // Inner layers mix toward white, outer toward the haze. The end caps
        // (the overshoot) run only the two inner layers, narrower.
        let layers: [(f32, f32, u32, f32); 4] = [
            (0.045, 1.00, WHITE, 0.55),   // white-hot axis
            (0.13, 0.55, WHITE, 0.12),    // inner sleeve
            (0.30, 0.24, cfg.haze, 0.55), // haze glow (beam: nebula; others: deep self-shade)
            (0.55, 0.10, cfg.haze, 0.85), // outer wash (a tint at the neighbour)
        ];
        for (i, &(hw, cmul, target, mix)) in layers.iter().enumerate() {
            let cov = (cov_axis * cmul).min(COV_CAP) as u8;
            if cov == 0 {
                continue;
            }
            let w = ((cw * hw * 2.0) as i32).max(1);
            let x = (ax - cw * hw) as i32;
            let color = if mix > 0.0 {
                lerp(cfg.color, target, mix)
            } else {
                cfg.color
            };
            let premul = premul_rgb(color, cov);
            if i < 2 {
                // Full rod including the overshoot caps.
                push_rect(out, geom, x, y0 as i32, w, h as i32, premul);
            } else {
                // Sleeve layers stay inside the cell band.
                push_rect(
                    out,
                    geom,
                    x,
                    oy as i32 + (cr as i32) * ch as i32,
                    w,
                    ch as i32,
                    premul,
                );
            }
        }
    }

    /// The block treatment: a soft aura of concentric rounded slabs hugging
    /// the emitter cell, reaching at most ~0.45 cell-heights.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + blaze/breathe + geometry/config; one internal call site"
    )]
    fn emit_aura(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        breathe: f32,
        geom: Geom,
        cfg: &BeamRodConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        // WINDOW px: cell position + origin.
        let x = i32::from(geom.origin_x) + cc as i32 * cw;
        let y = i32::from(geom.origin_y) + cr as i32 * ch;
        let reach = ((0.16 + 0.29 * e) * (0.85 + 0.15 * breathe) * geom.ch as f32) as i32;
        let cov_core = (COV_IDLE * 0.7 + (COV_MAX - COV_IDLE) * 0.7 * e) * cfg.intensity;
        // (reach ×, coverage ×, haze mix), outermost first: additive overlap
        // builds the radial falloff, sliding into `cfg.haze` with distance.
        let rings: [(f32, f32, f32); 3] =
            [(1.0, 0.22, 0.75), (0.62, 0.42, 0.40), (0.30, 0.70, 0.0)];
        let lerp = |a: u32, b: u32, t: f32| -> u32 {
            let m = |sh: u32| {
                let xx = ((a >> sh) & 0xff) as f32;
                let yy = ((b >> sh) & 0xff) as f32;
                (xx + (yy - xx) * t).min(255.0) as u32
            };
            (m(16) << 16) | (m(8) << 8) | m(0)
        };
        for &(rx, cmul, haze) in &rings {
            let g = (reach as f32 * rx) as i32;
            if g < 1 {
                continue;
            }
            let cov = (cov_core * cmul).min(COV_CAP) as u8;
            if cov == 0 {
                continue;
            }
            push_rect(
                out,
                geom,
                x - g,
                y - g,
                cw + 2 * g,
                ch + 2 * g,
                premul_rgb(lerp(cfg.color, cfg.haze, haze), cov),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor_glow::BEAM_DEFAULT_COLOR;

    fn geom() -> Geom {
        Geom {
            cw: 10,
            ch: 20,
            rows: 10,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 400,
            win_h: 200,
            head: 0,
        }
    }

    fn cfg(bar: bool) -> BeamRodConfig {
        BeamRodConfig {
            enabled: true,
            intensity: 1.0,
            color: BEAM_DEFAULT_COLOR,
            haze: aterm_render::BEAM_SPACE_HAZE,
            bar,
            shimmer: false,
        }
    }

    /// Disabled / zero-amplitude ⇒ byte-identical off: no quads, no fill,
    /// fingerprint 0, and the animator reports settled.
    #[test]
    fn disabled_or_reduced_motion_is_inert() {
        let mut rod = CursorBeamRod::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        let mut c = cfg(true);
        c.enabled = false;
        let f = rod.tick(Some((2, 3)), t0, 1.0, geom(), &c, &mut out);
        assert!(out.is_empty() && f.fill.is_none() && f.fp == 0);
        assert!(!rod.is_active());
        let mut c = cfg(true);
        c.intensity = 0.0;
        let f = rod.tick(Some((2, 3)), t0, 1.0, geom(), &c, &mut out);
        assert!(out.is_empty() && f.fill.is_none() && f.fp == 0);
        assert!(!rod.is_active());
    }

    /// The BAR rod: additive light hugging the bar's x (the cell's left edge),
    /// no fill override (bars keep their themed paint), quads in-grid, and the
    /// wide wash capped to a tint where it could cross the neighbour glyph.
    #[test]
    fn bar_rod_hugs_the_bar_and_stays_text_safe() {
        let mut rod = CursorBeamRod::default();
        let mut out = Vec::new();
        let g = geom();
        let f = rod.tick(Some((2, 3)), Instant::now(), 1.0, g, &cfg(true), &mut out);
        assert!(f.fill.is_none(), "bars have no fill-override channel");
        assert!(f.fp != 0, "a charged rod is drawable");
        assert!(rod.is_active(), "full blaze ⇒ still animating");
        assert!(!out.is_empty(), "the rod emits light");
        let cell_x0 = 3 * g.cw as i32;
        for q in &out {
            assert!((q.x as usize) < g.cols * g.cw && (q.y as usize) < g.rows * g.ch);
            // Everything stays within one neighbour of the bar's own column.
            assert!(
                (q.x as i32) >= cell_x0 - g.cw as i32 && (q.x as i32) <= cell_x0 + g.cw as i32,
                "rod light strays from the bar: x={}",
                q.x
            );
            // Any quad reaching past the cursor cell's own column is a tint.
            let a = (q.color >> 24) & 0xff;
            if (q.x as i32) < cell_x0 {
                assert!(a <= COV_CAP as u32, "neighbour-crossing wash too hot: {a}");
            }
        }
    }

    /// The BLOCK emitter: the fill locks to the beam hue (whitening with the
    /// blaze but never pure white) and the aura hugs the cell.
    #[test]
    fn block_emitter_fill_rides_the_blaze() {
        let mut rod = CursorBeamRod::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        let cold = rod
            .tick(Some((2, 3)), t0, 0.0, geom(), &cfg(false), &mut out)
            .fill
            .expect("block gets a fill");
        out.clear();
        let mut rod2 = CursorBeamRod::default();
        let hot = rod2
            .tick(Some((2, 3)), t0, 1.0, geom(), &cfg(false), &mut out)
            .fill
            .expect("block gets a fill");
        assert_ne!(cold, hot, "the fill whitens as the blaze climbs");
        assert!(
            hot != 0x00FF_FFFF,
            "never pure white — the axis stays the star"
        );
        assert!(!out.is_empty(), "the charged emitter glows");
    }

    /// Non-identity geometry: the rod lands inside the padded window's grid
    /// box (origin applied), not at the window's top-left.
    #[test]
    fn rod_lands_at_the_window_origin() {
        for bar in [true, false] {
            let mut rod = CursorBeamRod::default();
            let mut out = Vec::new();
            let mut g = geom();
            g.origin_x = 30;
            g.origin_y = 120;
            g.win_w = 460;
            g.win_h = 440;
            let f = rod.tick(Some((2, 3)), Instant::now(), 1.0, g, &cfg(bar), &mut out);
            assert!(f.fp != 0 && !out.is_empty(), "charged light must draw");
            for q in &out {
                assert!(
                    q.y >= g.origin_y,
                    "bar={bar}: quad above the grid origin: y={}",
                    q.y
                );
                assert!(
                    q.x >= g.origin_x.saturating_sub(g.cw as u16),
                    "bar={bar}: quad left of the grid origin: x={}",
                    q.x
                );
            }
        }
    }

    /// Settles: once the blaze dies the animator reports inactive so the host
    /// disarms the tick and the resting rod rides the blink cadence.
    #[test]
    fn settles_when_the_blaze_dies() {
        let mut rod = CursorBeamRod::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        rod.tick(Some((2, 3)), t0, 1.0, geom(), &cfg(true), &mut out);
        assert!(rod.is_active());
        rod.tick(Some((2, 3)), t0, 0.0, geom(), &cfg(true), &mut out);
        assert!(!rod.is_active(), "a dead blaze settles the animator");
    }
}
