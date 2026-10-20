// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The light-rod cursor: the cursor-shape treatment every trail style shares,
//! tinted by the host with the style's colour and haze (a style with a bespoke
//! block body takes only the bar). It follows the live cursor shape:
//!
//! * **Bar**: a vertical rod of light, a white-hot axis inside a haze sleeve,
//!   overshooting the cell slightly. It breathes at rest and flares with the
//!   blaze; the light is purely additive and the bar keeps its body.
//! * **Block**: the fill locks to the style's hue (whitening with blaze,
//!   contrast-floored by the renderer's `floor_cursor_fill`) inside a soft aura
//!   that fades into the haze.
//!
//! All light is additive [`GlowQuad`]s with per-quad coverage capped at `COV_CAP`.
//! A pure function of an injected `now`; inactive once the blaze dies.
//!
//! On a LIGHT GROUND (additive light cannot show on white) the rod has a
//! light mode of its own, ink UNDER the letters inside the cursor's row,
//! solved against the theme's text (`cursor_glow::light_body_ink`): the bar
//! becomes a narrow sleeve of the style's ink around a deeper core, the
//! emitter block's aura the same slabs hugging the cell in ink; the block
//! fill deepens toward its ink as it charges instead of whitening.

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

/// Axis coverage before intensity, rest → full blaze. The sleeve layers scale
/// down from it and every quad is clamped to [`COV_CAP`].
const COV_IDLE: f32 = 70.0;
const COV_MAX: f32 = 150.0;
/// Per-quad additive coverage ceiling (the comet and droplet cursors use the
/// same value): the thin axis saturates here; sleeve quads that reach a
/// neighbour glyph run below it.
const COV_CAP: f32 = 92.0;

/// The blaze below which the rod is settled: the animator reports inactive so
/// the host stops the 60 fps tick and the resting glow rides the blink cadence.
const SETTLED_BLAZE: f32 = 0.02;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct BeamRodConfig {
    /// The trail is on and the cursor is a focused bar, or a block under a style
    /// with no bespoke block body.
    pub enabled: bool,
    /// Reduced-motion / load-shed amplitude `0..1`; 0 ⇒ fully inert.
    pub intensity: f32,
    /// The rod's hue `0x00RRGGBB` (`GlowConfig::color`, or the style's
    /// signature shade), so the rod, the fill and the trail are one light.
    pub color: u32,
    /// The sleeve colour `0x00RRGGBB` the outer layers and aura rim mix toward:
    /// beam's `BEAM_SPACE_HAZE`, otherwise a deepened shade of the style's hue.
    pub haze: u32,
    /// The live cursor is a bar (rod treatment) rather than a block (emitter).
    pub bar: bool,
    /// Sparkle's twinkle: faster, deeper breathing so the emitter glitters.
    pub shimmer: bool,
    /// The ground is dark (the rod is light) or light (the rod is ink under
    /// the letters).
    pub dark_theme: bool,
    /// The theme's text and paper, which a light ground's ink is solved
    /// against.
    pub theme_fg: u32,
    pub theme_bg: u32,
}

/// What a tick produced: the emitter block fill (`None` for the bar, whose body
/// stays the themed cursor) and a fingerprint that changes on every visible step.
#[derive(Clone, Copy, Debug)]
pub struct BeamRodFrame {
    /// The emitter block-fill colour `0x00RRGGBB`, or `None` when off / bar.
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + rod light (0 ⇒ nothing this frame).
    pub fp: u64,
}

/// Per-window light-rod animation state: a breathing phase, the last clock
/// reading and a latched blaze.
#[derive(Default)]
pub struct CursorBeamRod {
    /// Rolling breathing phase in turns `0..1`.
    breathe: f32,
    last: Option<Instant>,
    /// Latched blaze at the last tick (so [`Self::is_active`] needs no clock).
    blaze: f32,
}

impl CursorBeamRod {
    /// Whether the host must keep arming the animation tick: only while the
    /// beam is charged (typing or a jump flare cooling).
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.blaze > SETTLED_BLAZE
    }

    /// Advance one frame: `blaze` is [`crate::cursor_glow::CursorGlow::blaze`]
    /// read after the aurora tick, `cur` the cursor cell (`None` ⇒ hidden). Appends
    /// the additive rod/aura to `out` on a dark ground, its ink to `under` (the
    /// under-ink stream) on a light one; returns the emitter fill and a
    /// fingerprint.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + clock + blaze + geometry + config + the two streams it draws into"
    )]
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        blaze: f32,
        geom: Geom,
        cfg: &BeamRodConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
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
            if !cfg.dark_theme {
                // Paper: the block deepens toward its ink as it charges.
                let deep = crate::cursor_glow::InkRole::OverText.ink(cfg.color);
                return crate::effect_util::lerp_rgb(cfg.color, deep, k);
            }
            let mix = |sh: u32| {
                let c = ((cfg.color >> sh) & 0xff) as f32;
                (c + (255.0 - c) * k).min(255.0) as u32
            };
            (mix(16) << 16) | (mix(8) << 8) | mix(0)
        });

        // The rod/aura, drawn only over a visible in-grid cursor cell: light
        // on a dark ground, ink under the letters on a light one.
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
        {
            match (cfg.dark_theme, cfg.bar) {
                (true, true) => self.emit_rod(cr, cc, e, breathe, geom, cfg, out),
                (true, false) => self.emit_aura(cr, cc, e, breathe, geom, cfg, out),
                (false, true) => Self::emit_rod_light(cr, cc, e, breathe, geom, cfg, under),
                (false, false) => Self::emit_aura_light(cr, cc, e, breathe, geom, cfg, under),
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
        // Window px: everything below is grid-relative + origin.
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

    /// The bar on a LIGHT GROUND: a narrow sleeve of the style's ink around a
    /// deeper core on the bar's own x, under the letters and held inside the
    /// cursor's row, charging with the blaze and breathing as the light rod
    /// does. Bounded: two rects.
    fn emit_rod_light(
        cr: u16,
        cc: u16,
        e: f32,
        breathe: f32,
        geom: Geom,
        cfg: &BeamRodConfig,
        under: &mut Vec<GlowQuad>,
    ) {
        use crate::cursor_glow::{LIGHT_BODY_ALPHA_CAP, light_body_ink, reink_quads};
        let cw = geom.cw as f32;
        let ax = f32::from(geom.origin_x) + (cc as f32) * cw + cw * 0.10;
        let y = i32::from(geom.origin_y) + cr as i32 * geom.ch as i32;
        let strength = (COV_IDLE + (COV_MAX - COV_IDLE) * e) / COV_MAX
            * cfg.intensity.clamp(0.0, 1.0)
            * (0.90 + 0.10 * breathe);
        let ink = light_body_ink(
            cfg.color,
            cfg.theme_fg,
            cfg.theme_bg,
            LIGHT_BODY_ALPHA_CAP as u8,
        );
        // (half-width in cell-widths, share of the body ceiling), sleeve first
        // so the core composites over it.
        for (hw, share) in [(0.30, 0.45), (0.11, 1.0)] {
            let a = (LIGHT_BODY_ALPHA_CAP * share * strength) as u8;
            if a == 0 {
                continue;
            }
            let w = ((cw * hw * 2.0) as i32).max(1);
            let from = under.len();
            push_rect(
                under,
                geom,
                (ax - cw * hw) as i32,
                y,
                w,
                geom.ch as i32,
                premul_rgb(0x00FF_FFFF, a),
            );
            reink_quads(&mut under[from..], LIGHT_BODY_ALPHA_CAP as u8, |_| ink);
        }
    }

    /// The emitter block on a LIGHT GROUND: its aura as the dark one's
    /// concentric slabs hugging the cell, in the style's ink under the
    /// letters and held inside the cursor's row (so it reaches out sideways,
    /// a square glow around the block, never Lumen's or the comet's round
    /// one), swelling with the blaze; the slabs' compound darkness is the
    /// ink's to bound (`light_body_ink`, solved opaque).
    fn emit_aura_light(
        cr: u16,
        cc: u16,
        e: f32,
        breathe: f32,
        geom: Geom,
        cfg: &BeamRodConfig,
        under: &mut Vec<GlowQuad>,
    ) {
        use crate::cursor_glow::{LIGHT_BODY_ALPHA_CAP, light_body_ink, reink_quads};
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        let x = i32::from(geom.origin_x) + cc as i32 * cw;
        let y = i32::from(geom.origin_y) + cr as i32 * ch;
        let reach = ((0.16 + 0.29 * e) * (0.85 + 0.15 * breathe) * geom.ch as f32) as i32;
        let strength = (0.30 + 0.45 * e) * cfg.intensity.clamp(0.0, 1.0);
        let ink = light_body_ink(
            cfg.color,
            cfg.theme_fg,
            cfg.theme_bg,
            LIGHT_BODY_ALPHA_CAP as u8,
        );
        // (reach ×, share of the body ceiling), outermost first, as on dark.
        for (rx, share) in [(1.0, 0.25), (0.62, 0.35), (0.30, 0.45)] {
            let g = (reach as f32 * rx) as i32;
            let a = (LIGHT_BODY_ALPHA_CAP * share * strength) as u8;
            if g < 1 || a == 0 {
                continue;
            }
            let from = under.len();
            push_rect(
                under,
                geom,
                x - g,
                y,
                cw + 2 * g,
                ch,
                premul_rgb(0x00FF_FFFF, a),
            );
            reink_quads(&mut under[from..], LIGHT_BODY_ALPHA_CAP as u8, |_| ink);
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
        // Window px: cell position + origin.
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
            dark_theme: true,
            theme_fg: 0x00C8_C8D2,
            theme_bg: 0x0010_1016,
        }
    }

    /// Disabled or zero amplitude: no quads, no fill, fingerprint 0, settled.
    #[test]
    fn disabled_or_reduced_motion_is_inert() {
        let mut rod = CursorBeamRod::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        let mut c = cfg(true);
        c.enabled = false;
        let f = rod.tick(Some((2, 3)), t0, 1.0, geom(), &c, &mut out, &mut Vec::new());
        assert!(out.is_empty() && f.fill.is_none() && f.fp == 0);
        assert!(!rod.is_active());
        let mut c = cfg(true);
        c.intensity = 0.0;
        let f = rod.tick(Some((2, 3)), t0, 1.0, geom(), &c, &mut out, &mut Vec::new());
        assert!(out.is_empty() && f.fill.is_none() && f.fp == 0);
        assert!(!rod.is_active());
    }

    /// The bar rod hugs the bar's x, overrides no fill, stays in-grid, and caps
    /// any quad crossing into the neighbour column to a tint.
    #[test]
    fn bar_rod_hugs_the_bar_and_stays_text_safe() {
        let mut rod = CursorBeamRod::default();
        let mut out = Vec::new();
        let g = geom();
        let f = rod.tick(
            Some((2, 3)),
            Instant::now(),
            1.0,
            g,
            &cfg(true),
            &mut out,
            &mut Vec::new(),
        );
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

    /// The rod lands inside the padded window's grid box (origin applied).
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
            let f = rod.tick(
                Some((2, 3)),
                Instant::now(),
                1.0,
                g,
                &cfg(bar),
                &mut out,
                &mut Vec::new(),
            );
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

    /// A dead blaze reports inactive, so the host disarms the tick.
    #[test]
    fn settles_when_the_blaze_dies() {
        let mut rod = CursorBeamRod::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        rod.tick(
            Some((2, 3)),
            t0,
            1.0,
            geom(),
            &cfg(true),
            &mut out,
            &mut Vec::new(),
        );
        assert!(rod.is_active());
        rod.tick(
            Some((2, 3)),
            t0,
            0.0,
            geom(),
            &cfg(true),
            &mut out,
            &mut Vec::new(),
        );
        assert!(!rod.is_active(), "a dead blaze settles the animator");
    }
}
