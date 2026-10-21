// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The light-rod cursor: the cursor-shape treatment every trail style shares,
//! tinted by the host with the style's colour and haze (a style with a bespoke
//! block body takes only the bar). It follows the live cursor shape:
//!
//! * **Bar** (DECSCUSR bar / `cursor_style beam`): a vertical rod of light, a
//!   white-hot axis inside a haze sleeve ([`BEAM_SPACE_HAZE`] for beam, shared
//!   with the trail), overshooting the cell slightly. It breathes at rest and
//!   flares with the aurora's blaze. The bar keeps its shape; the light is
//!   additive on a dark page and ink under the letters on a light one (see
//!   below).
//! * **Block**: a charged emitter. The fill locks to the style's hue beside a
//!   soft round aura, inside the caret's row, that fades into the haze at its
//!   rim (the rows above and below take none of it). On a dark page the fill
//!   whitens with the blaze; on a light page it deepens instead
//!   ([`crate::color_math::light_body_fill`]) and the aura is not drawn, since
//!   additive light cannot brighten a white ground and over a glyph it only
//!   lifts the ink toward the page.
//!
//! Text safety: the host floors the fill against the page
//! ([`crate::cursor_fx::floor_block_fill`]), so the caret and the glyph cut
//! out of it in the page colour stay legible; the renderer's
//! `floor_cursor_fill` adds the user's `minimum_contrast` on top (off by
//! default). Rod and aura light is additive [`GlowQuad`]s with capped coverage.
//! A clockless pure function of an injected `now`; it goes inactive when the
//! blaze dies (the resting rod rides the blink cadence) and emits identical
//! premultiplied quads on CPU and Metal.
//!
//! On a LIGHT GROUND (additive light cannot show on white) the bar's rod has
//! a light mode of its own, ink UNDER the letters inside the cursor's row,
//! solved against the theme's text (`cursor_glow::light_body_ink`): a narrow
//! sleeve of the style's ink around a deeper core. The emitter block lays no
//! aura there; its fill concentrates the hue as it charges instead of
//! whitening ([`crate::color_math::light_body_fill`]).

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::{Geom, OVER_INK_COV_CAP};
use crate::effect_util::TWINKLE_FLASH_BOUND_HZ;
use crate::effect_util::push_fx_rect as push_rect;
use crate::effect_util::{lerp_rgb, push_fx_ramp};

/// Breathing rate in turns/second: a slow, calm shimmer. The phase advances
/// only when frames render, so a settled rod breathes at the blink cadence.
const BREATHE_IDLE: f32 = 0.55;
const BREATHE_ACTIVE: f32 = 1.6;

/// Sparkle's shimmer runs the same sine this many times faster: a glitter
/// tremble rather than a breath.
const SHIMMER_RATE_MUL: f32 = 4.5;
/// The shimmer's ceiling in turns/second. The breath is one unrectified sine,
/// one peak per turn, so turns/second is its flash rate, and it stays inside
/// the crate's general-flash bound however hard the keys charge the rod.
const SHIMMER_MAX_TURNS_PER_S: f32 = TWINKLE_FLASH_BOUND_HZ;
const _: () = assert!(
    BREATHE_IDLE * SHIMMER_RATE_MUL <= SHIMMER_MAX_TURNS_PER_S
        && SHIMMER_MAX_TURNS_PER_S <= TWINKLE_FLASH_BOUND_HZ,
    "the sparkle shimmer must breathe inside the general-flash bound, idle included"
);

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

/// The least WCAG contrast the emitter block keeps against a LIGHT page at
/// rest, as the comet's nucleus does
/// ([`crate::cursor_comet::LIGHT_NUCLEUS_CONTRAST`]): the style's hue,
/// deepened by lightness alone ([`crate::color_math::floor_contrast_keeping_hue`],
/// so Sparkle's gold stays gold and Beam's ice stays ice, never greyed and
/// never whitened) until it reads clearly above the caret's page floor
/// ([`crate::cursor_fx::CARET_PAGE_MIN_CONTRAST`]) rather than resting on it.
/// The heat deepens it further from there. A pinned cursor colour is not
/// moved ([`BeamRodConfig::pinned`]).
pub const LIGHT_EMITTER_CONTRAST: f32 = crate::cursor_comet::LIGHT_NUCLEUS_CONTRAST;

/// The block aura's brightest light outside the cell at full blaze: the
/// over-ink budget, spent once, at the cell's edge.
const AURA_PEAK_COV: f32 = OVER_INK_COV_CAP;
/// Linear ramp pieces a side of each aura row: the profile to within a few
/// per cent.
const AURA_SEGMENTS: usize = 4;
/// Aura row height is `ch / AURA_ROW_DIV`, at least 2 px, so the falloff
/// toward the row's edges is a few levels a step.
const AURA_ROW_DIV: f32 = 12.0;

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
    /// Sparkle's twinkle: the breathing runs [`SHIMMER_RATE_MUL`] times
    /// faster (capped at [`SHIMMER_MAX_TURNS_PER_S`]) so the emitter glitters
    /// instead of breathing; its depth is unchanged. Everyone else passes
    /// `false`.
    pub shimmer: bool,
    /// The ground is dark (the rod is light) or light (the bar's rod is ink
    /// under the letters, the emitter block deepens and lays no aura).
    pub dark_theme: bool,
    /// The theme's text and paper, which a light ground's ink is solved
    /// against.
    pub theme_fg: u32,
    pub theme_bg: u32,
    /// [`Self::color`] is a PINNED cursor colour (a configured `cursor_color`
    /// or a program's OSC 12, which the style's colour follows): the settled
    /// emitter block is exactly that colour, and only the blaze moves it
    /// ([`crate::color_math::wear_pinned`]).
    pub pinned: bool,
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
        let rate = BREATHE_IDLE + BREATHE_ACTIVE * e;
        let rate = if cfg.shimmer {
            (rate * SHIMMER_RATE_MUL).min(SHIMMER_MAX_TURNS_PER_S)
        } else {
            rate
        };
        self.breathe = (self.breathe + dt * rate).fract();
        let phase = self.breathe * std::f32::consts::TAU;
        // A single sine: steady light breathes, and sparkle's faster tremble is
        // still that sine under the flash bound, never a strobe.
        let breathe = 0.5 + 0.5 * phase.sin();

        // Block fill: the style's hue. On a dark page it whitens as the blaze
        // climbs, capped below pure white so the additive axis stays
        // brightest; on a light page it deepens instead. The host floors it
        // against the page either way.
        // A pinned cursor colour is the settled block itself; the lift (or
        // the deepening) comes in only with the blaze.
        let fill = (!cfg.bar).then(|| {
            let styled = if cfg.dark_theme {
                let k = 0.10 + 0.35 * e;
                let mix = |sh: u32| {
                    let c = ((cfg.color >> sh) & 0xff) as f32;
                    (c + (255.0 - c) * k).min(255.0) as u32
                };
                (mix(16) << 16) | (mix(8) << 8) | mix(0)
            } else {
                crate::color_math::floor_contrast_keeping_hue(
                    crate::color_math::light_body_fill(cfg.color, e),
                    cfg.theme_bg & 0x00FF_FFFF,
                    LIGHT_EMITTER_CONTRAST,
                )
            };
            if cfg.pinned {
                crate::color_math::wear_pinned(cfg.color, styled, e, SETTLED_BLAZE)
            } else {
                styled
            }
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
                // The opaque block carries the caret on paper; an aura of ink
                // beside it would only stand over the letter just typed.
                (false, false) => {}
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

    /// The block treatment: a soft round aura beside the emitter cell, inside
    /// the caret's row.
    ///
    /// One `(1 − ρ²)²` profile over an ellipse as wide as the cell plus the
    /// reach (a quarter of a cell-height at rest, over half of one at full
    /// blaze, breathing) and exactly
    /// as tall as the row, so it melts to nothing at the row's top and bottom
    /// edges and the rows above and below take no light. Only the part
    /// outside the cell is laid (the opaque block covers the rest), scaled so
    /// its brightest pixel, at the cell's edge, carries at most
    /// [`OVER_INK_COV_CAP`] (the letter just typed beside the caret keeps the
    /// over-ink bar), as per-row ramps of [`AURA_SEGMENTS`] pieces a side that
    /// meet but never overlap: one smooth falloff, not nested steps.
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
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let (cx, cy) = geom.cell_center(cr, cc);
        let reach = (0.28 + 0.30 * e) * (0.85 + 0.15 * breathe) * ch;
        let (rx, ry) = (cw * 0.5 + reach, ch * 0.5);
        // The profile at the cell's edge on the row's centre line: the
        // brightest point outside the block, which the budget scales to.
        let rho_edge = (cw * 0.5) / rx;
        let f_edge = (1.0 - rho_edge * rho_edge).powi(2);
        let peak = AURA_PEAK_COV * (0.55 + 0.45 * e) * (0.9 + 0.1 * breathe) * cfg.intensity;
        if !(reach >= 1.0 && f_edge > 0.0 && peak >= 0.5) {
            return;
        }
        let cell_x0 = (cx - cw * 0.5).round() as i32;
        let cell_x1 = cell_x0 + geom.cw as i32;
        let band_y0 = i32::from(geom.origin_y) + i32::from(cr) * geom.ch as i32;
        let band_y1 = band_y0 + geom.ch as i32;
        let row_h = ((ch / AURA_ROW_DIV) as i32).max(2);
        let mut y = band_y0;
        while y < band_y1 {
            let h = row_h.min(band_y1 - y);
            let v = (y as f32 + h as f32 * 0.5 - cy) / ry;
            let w2 = 1.0 - v * v;
            let half = rx * w2.max(0.0).sqrt();
            if w2 > 0.0 && half > cw * 0.5 {
                // Knot `k`: from the cell's edge (`k = 0`) out to the
                // ellipse's rim at this row, its offset and premultiplied
                // light (the hue sliding into the haze toward the rim).
                let knot = |k: usize| {
                    let t = k as f32 / AURA_SEGMENTS as f32;
                    let dx = cw * 0.5 + (half - cw * 0.5) * t;
                    let rho2 = (dx / rx).powi(2) + v * v;
                    let f = (1.0 - rho2).max(0.0).powi(2) / f_edge;
                    let cov = (peak * f).round().min(OVER_INK_COV_CAP) as u8;
                    let color = lerp_rgb(cfg.color, cfg.haze, 0.75 * rho2.sqrt());
                    (dx, cov, premul_rgb(color, cov))
                };
                let knots: [_; AURA_SEGMENTS + 1] = std::array::from_fn(knot);
                for pair in knots.windows(2) {
                    let ((d_in, c_in, inner), (d_out, c_out, outer)) = (pair[0], pair[1]);
                    if c_in == 0 && c_out == 0 {
                        continue;
                    }
                    let (r0, r1) = (
                        ((cx + d_in).round() as i32).max(cell_x1),
                        (cx + d_out).round() as i32,
                    );
                    push_fx_ramp(out, geom, r0, y, r1 - r0, h, (inner, 0), (outer, 0));
                    let (l0, l1) = (
                        (cx - d_out).round() as i32,
                        ((cx - d_in).round() as i32).min(cell_x0),
                    );
                    push_fx_ramp(out, geom, l0, y, l1 - l0, h, (outer, 0), (inner, 0));
                }
            }
            y += h;
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
            pinned: false,
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

    /// On a white page at full blaze the emitter block (GitHub Light's cursor
    /// `#0969DA` as its colour) keeps 3:1 with everything it emits composited
    /// over the caret cell, and the bar draws no additive rod. Negative
    /// control: the dark arm on the same page whitens the block under 3:1,
    /// the defect the light arm exists for.
    #[test]
    fn the_light_emitter_keeps_its_caret_on_a_white_page() {
        const WHITE: u32 = 0x00FF_FFFF;
        let g = geom();
        let cell = (3 * g.cw as i32, 2 * g.ch as i32, g.cw as i32, g.ch as i32);
        let caret = |dark: bool| {
            let mut rod = CursorBeamRod::default();
            let mut out = Vec::new();
            let c = BeamRodConfig {
                color: 0x0009_69DA,
                haze: 0x0004_3466,
                dark_theme: dark,
                theme_fg: if dark { 0x00C8_C8D2 } else { 0x001F_2328 },
                theme_bg: if dark { 0x0010_1016 } else { WHITE },
                ..cfg(false)
            };
            let mut under = Vec::new();
            let fill = rod
                .tick(
                    Some((2, 3)),
                    Instant::now(),
                    1.0,
                    g,
                    &c,
                    &mut out,
                    &mut under,
                )
                .fill
                .expect("block gets a fill");
            assert!(dark || under.is_empty(), "no aura of ink beside the block");
            crate::effect_util::caret_cell_worst_contrast(fill, &out, cell, WHITE)
        };
        let light = caret(false);
        assert!(
            light >= 3.0,
            "light arm: the caret must keep 3:1 on white, got {light:.2}:1"
        );
        let dark_arm = caret(true);
        assert!(
            dark_arm < 3.0,
            "control: the whitening dark arm sinks into white ({dark_arm:.2}:1)"
        );
        let mut rod = CursorBeamRod::default();
        let mut out = Vec::new();
        let bar = BeamRodConfig {
            dark_theme: false,
            theme_fg: 0x001F_2328,
            theme_bg: WHITE,
            ..cfg(true)
        };
        let mut under = Vec::new();
        rod.tick(
            Some((2, 3)),
            Instant::now(),
            1.0,
            g,
            &bar,
            &mut out,
            &mut under,
        );
        assert!(out.is_empty(), "no additive rod on a light page");
        assert!(!under.is_empty(), "the bar's rod is ink under the letters");
    }

    /// ON PAPER THE RESTING EMITTER READS CLEARLY ABOVE THE CARET FLOOR, IN
    /// ITS OWN HUE: Sparkle's gold and Beam's ice at rest (and charged) keep
    /// [`LIGHT_EMITTER_CONTRAST`] on GitHub Light and Default Light, within a
    /// few degrees of their hue and no greyer than the hue itself. A pinned
    /// cursor colour still rests exactly as pinned. Negative control: the
    /// deepening alone (the arm before the floor) leaves the gold under 4.5:1
    /// and the ice on the floor, which the host then lifted to exactly 4.5.
    #[test]
    fn the_light_emitter_rests_clearly_above_the_floor_in_its_hue() {
        use crate::color_math::{contrast_ratio, light_body_fill, rgb2hsv};
        use crate::cursor_glow::{BEAM_DEFAULT_COLOR, SPARKLE_DEFAULT_COLOR};
        let hue_gap = |a: f32, b: f32| {
            let d = (a - b).abs().rem_euclid(360.0);
            d.min(360.0 - d)
        };
        for (paper, ink) in [(0x00FF_FFFF, 0x001F_2328), (0x00F6_F7F9, 0x002B_2D33)] {
            for (color, shimmer) in [(SPARKLE_DEFAULT_COLOR, true), (BEAM_DEFAULT_COLOR, false)] {
                for blaze in [0.0, 1.0] {
                    let mut rod = CursorBeamRod::default();
                    let (mut out, mut under) = (Vec::new(), Vec::new());
                    let c = BeamRodConfig {
                        color,
                        shimmer,
                        dark_theme: false,
                        theme_fg: ink,
                        theme_bg: paper,
                        ..cfg(false)
                    };
                    let fill = rod
                        .tick(
                            Some((2, 3)),
                            Instant::now(),
                            blaze,
                            geom(),
                            &c,
                            &mut out,
                            &mut under,
                        )
                        .fill
                        .expect("a block gets a fill");
                    let ratio = contrast_ratio(fill, paper);
                    assert!(
                        ratio >= LIGHT_EMITTER_CONTRAST - 0.05,
                        "{color:06x} at blaze {blaze} on {paper:06x}: {fill:06x} reads {ratio:.2}:1"
                    );
                    let (h0, s0, _) = rgb2hsv(color);
                    let (h, s, _) = rgb2hsv(fill);
                    assert!(
                        hue_gap(h, h0) <= 6.0 && s >= s0 - 0.02,
                        "{color:06x} rests as {fill:06x}: off its hue or toward grey"
                    );
                }
                let control = contrast_ratio(light_body_fill(color, 0.0), paper);
                assert!(
                    control < 4.75,
                    "control: the deepening alone of {color:06x} reads {control:.2}:1"
                );
            }
        }
        let pinned = 0x00FF_3399;
        let mut rod = CursorBeamRod::default();
        let (mut out, mut under) = (Vec::new(), Vec::new());
        let fill = rod
            .tick(
                Some((2, 3)),
                Instant::now(),
                0.0,
                geom(),
                &BeamRodConfig {
                    color: pinned,
                    pinned: true,
                    dark_theme: false,
                    theme_fg: 0x001F_2328,
                    theme_bg: 0x00FF_FFFF,
                    ..cfg(false)
                },
                &mut out,
                &mut under,
            )
            .fill;
        assert_eq!(fill, Some(pinned), "a pinned colour rests as pinned");
    }

    /// Sparkle's shimmer: at full blaze its breath advances no faster than
    /// [`SHIMMER_MAX_TURNS_PER_S`], while at rest it still trembles faster
    /// than the plain breath (the glitter is kept, only its ceiling is new).
    #[test]
    fn the_sparkle_shimmer_stays_inside_the_flash_bound() {
        let advance = |shimmer: bool, blaze: f32| {
            let mut rod = CursorBeamRod::default();
            let mut out = Vec::new();
            let t0 = Instant::now();
            let c = BeamRodConfig {
                shimmer,
                ..cfg(false)
            };
            rod.tick(
                Some((2, 3)),
                t0,
                blaze,
                geom(),
                &c,
                &mut out,
                &mut Vec::new(),
            );
            let before = rod.breathe;
            // 0.2 s: under the 0.25 s stall clamp and under one turn.
            rod.tick(
                Some((2, 3)),
                t0 + std::time::Duration::from_millis(200),
                blaze,
                geom(),
                &c,
                &mut out,
                &mut Vec::new(),
            );
            (rod.breathe - before).rem_euclid(1.0) / 0.2
        };
        let hot = advance(true, 1.0);
        assert!(
            hot <= SHIMMER_MAX_TURNS_PER_S + 1e-3,
            "charged shimmer at {hot:.2} turns/s exceeds the bound"
        );
        assert!(
            advance(true, 0.0) > advance(false, 0.0),
            "the idle shimmer still trembles faster than the plain breath"
        );
    }

    /// The light every quad lays on each pixel of a `w`×`h` window, summed
    /// by the renderer's gradient law (the brightest channel).
    fn light_per_pixel(quads: &[GlowQuad], w: usize, h: usize) -> Vec<u32> {
        let mut px = vec![0u32; w * h];
        for q in quads {
            let qw = u32::from(q.w);
            for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                for i in 0..qw {
                    let x = usize::from(q.x) + i as usize;
                    if x >= w {
                        break;
                    }
                    let c = aterm_render::glow_lerp_rgb(q.color, q.color2, i, qw);
                    px[y * w + x] += (c >> 16 & 0xff).max(c >> 8 & 0xff).max(c & 0xff);
                }
            }
        }
        px
    }

    /// The emitter block's aura on a dark page is one round glow beside the
    /// block, inside its row: at rest and at full blaze, the rows above and
    /// below take no light (a selection row under the prompt keeps its
    /// letters), no pixel outside the cell carries more than the over-ink
    /// budget, the light shows on both sides of the block, it melts toward
    /// the row's top and bottom edges instead of stopping at them, and along
    /// the row it falls away from the block a few levels a pixel. Negative
    /// control: the nested box the aura used to stack (three rectangles
    /// around the cell) lights the rows above and below and steps in jumps
    /// along the row.
    #[test]
    fn the_block_aura_is_a_round_glow_inside_the_caret_row() {
        let g = geom();
        let (w, h) = (usize::from(g.win_w), usize::from(g.win_h));
        let (cr, cc) = (4u16, 20u16);
        let band = usize::from(cr) * g.ch..(usize::from(cr) + 1) * g.ch;
        let cell = usize::from(cc) * g.cw..(usize::from(cc) + 1) * g.cw;
        let mid = band.start + g.ch / 2;
        // The largest jump between neighbouring pixels along the centre line
        // left of the block, and the light outside the row.
        let shape = |quads: &[GlowQuad]| {
            let light = light_per_pixel(quads, w, h);
            let outside_row: u32 = (0..h)
                .filter(|y| !band.contains(y))
                .flat_map(|y| (0..w).map(move |x| (x, y)))
                .map(|(x, y)| light[y * w + x])
                .sum();
            let step = (1..cell.start)
                .map(|x| light[mid * w + x].abs_diff(light[mid * w + x - 1]))
                .max()
                .unwrap_or(0);
            (light, outside_row, step)
        };
        for blaze in [0.0f32, 0.5, 1.0] {
            let mut rod = CursorBeamRod::default();
            let mut out = Vec::new();
            rod.tick(
                Some((cr, cc)),
                Instant::now(),
                blaze,
                g,
                &cfg(false),
                &mut out,
                &mut Vec::new(),
            );
            assert!(!out.is_empty(), "blaze {blaze}: the emitter glows");
            assert!(
                out.iter().all(|q| q.alpha == 0 && q.alpha2 == 0),
                "additive light on a dark page"
            );
            let (light, outside_row, step) = shape(&out);
            assert_eq!(outside_row, 0, "blaze {blaze}: the aura left its row");
            let outside_cell_max = band
                .clone()
                .flat_map(|y| (0..w).filter(|x| !cell.contains(x)).map(move |x| (x, y)))
                .map(|(x, y)| light[y * w + x])
                .max()
                .unwrap_or(0);
            assert!(
                outside_cell_max <= OVER_INK_COV_CAP as u32,
                "blaze {blaze}: {outside_cell_max} levels beside the block"
            );
            let lit = |x: usize| light[mid * w + x] > 0;
            assert!(
                lit(cell.start - 1) && lit(cell.end),
                "blaze {blaze}: the glow shows on both sides of the block"
            );
            let rim = (0..w)
                .map(|x| light[band.start * w + x].max(light[(band.end - 1) * w + x]))
                .max()
                .unwrap_or(0);
            assert!(
                rim * 4 <= outside_cell_max,
                "blaze {blaze}: an edge at the row's rim ({rim} of {outside_cell_max})"
            );
            assert!(
                step <= 12,
                "blaze {blaze}: a {step}-level step along the row"
            );
        }
        // The nested box the aura used to stack, at full blaze.
        let mut boxed = Vec::new();
        let (x, y) = (
            (usize::from(cc) * g.cw) as i32,
            (usize::from(cr) * g.ch) as i32,
        );
        for (reach, cov) in [(9, 23u8), (5, 44), (2, 74)] {
            push_rect(
                &mut boxed,
                g,
                x - reach,
                y - reach,
                g.cw as i32 + 2 * reach,
                g.ch as i32 + 2 * reach,
                premul_rgb(BEAM_DEFAULT_COLOR, cov),
            );
        }
        let (_, outside_row, step) = shape(&boxed);
        assert!(
            outside_row > 0 && step > 12,
            "control: the nested box lights other rows ({outside_row}) and steps by {step}"
        );
    }
}
