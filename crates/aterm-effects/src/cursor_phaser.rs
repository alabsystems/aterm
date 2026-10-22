// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The phaser emitter cursor: under the `phaser` trail style the block cursor is
//! the emitter the beam fires from.
//!
//! * **The block fill follows the beam's rolling hue**, the sweep phase the
//!   streak is laid in (from [`crate::cursor_glow::CursorGlow::beam_hue`]), so
//!   the beam reads as light leaving the cursor. At rest the fill is a whisper
//!   of the beam hue over the cursor's own colour ([`PhaserConfig::base`]: OSC
//!   12 / `cursor_color`); under the keys it saturates to the vivid beam colour.
//!   On a light page with no pinned colour the block IS the beam's hue, clean
//!   and saturated, deepened by lightness to read clearly on the page, on a
//!   wheel that steps over the hues paper can only show as khaki
//!   (`color_math::light_spectrum_hue`); the bar it fires does the same.
//! * **Beam-axis energy wings**: an additive pair of lens-shaped side lobes on
//!   the horizontal axis, spanning the cell height and tapering sideways, at a
//!   dim static floor while idle and swelling with the typing cadence.
//! * **A prism fringe**: the outer wing layers split slightly off the core hue
//!   (trailing spectrum left, leading right), unlike the rainbow kitty's banded
//!   ring.
//!
//! Text-safe: the host floors the fill against the page (`crate::cursor_fx::floor_block_fill`,
//! unless it wears a pinned cursor colour) and the renderer's `floor_cursor_fill` adds the user's
//! `minimum_contrast` on top (off by default). The wings are additive [`GlowQuad`] light with
//! capped coverage, drawn on dark pages only; only the dim outer tint reaches over neighbouring
//! glyphs. Pure in the injected `now`, it settles to inactive when the charge dies (then rides
//! the blink cadence).

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::{Geom, hsv2rgb};
use crate::effect_util::push_fx_rect as push_rect;
use crate::effect_util::{lerp, mix_rgb};

/// The block base the beam hue tints from on a dark theme when the host names
/// none: white. A light theme's block is the beam's hue itself
/// ([`light_emitter_fill`]). A host that knows a pinned cursor colour passes
/// it as [`PhaserConfig::base`] instead.
const BASE_DARK_THEME: u32 = 0x00FF_FFFF;

/// The emitter block on a light page with no pinned cursor colour: the beam's
/// own hue, clean and fully saturated (the wheel stepping over the hues that
/// can only read on paper as khaki or olive,
/// [`crate::color_math::light_spectrum_hue`]), deepening with the charge
/// ([`crate::color_math::light_body_fill`]) and held by lightness alone to
/// the light emitter's contrast on white
/// ([`crate::cursor_beam::LIGHT_EMITTER_CONTRAST`]), so it reads as the
/// emitter rather than a drab near-black. The host still floors it against
/// the cell it sits on.
#[inline]
fn light_emitter_fill(hue: f32, e: f32) -> u32 {
    let clean = hsv2rgb(crate::color_math::light_spectrum_hue(hue), 1.0, 1.0);
    crate::color_math::floor_contrast_keeping_hue(
        crate::color_math::light_body_fill(clean, e),
        0x00FF_FFFF,
        crate::cursor_beam::LIGHT_EMITTER_CONTRAST,
    )
}

/// Charged breath (turns/sec of the wing pulse) and its depth. The phase freezes
/// below [`SETTLED_ENERGY`], so an unrelated present after a long idle never
/// advances a hidden clock and changes brightness.
const PULSE_HZ: f32 = 0.30;
const PULSE_DEPTH: f32 = 0.55;

/// Saturation / value / base-mix ramps from idle whisper to vivid charged fill.
/// Idle saturation is above the rainbow cursor's (0.32) so the fill reads as the
/// beam's hue even at rest.
const SAT_IDLE: f32 = 0.45;
const SAT_MAX: f32 = 1.0;
const VAL_IDLE: f32 = 0.85;
const VAL_MAX: f32 = 1.0;
const MIX_IDLE: f32 = 0.22;
const MIX_MAX: f32 = 0.88;

/// Wing reach past the cell edge in cell widths: a tight idle glow, up to over a
/// neighbour cell at full charge. Only the dim outer layer runs the full reach
/// ([`WING_LAYERS`]), so the far tint never buries a glyph.
const REACH_IDLE: f32 = 0.30;
const REACH_MAX: f32 = 1.60;

/// The nested lens layers, innermost last: (reach ×, coverage ×, hue fringe in
/// turns, height ×). The bright core hugs the cell and spans its height; outer
/// layers are the faint prism fringe, flatter the further they reach, so the far
/// light is a slim flare on the beam axis that stays off neighbouring ascenders
/// and descenders. Additive overlap builds the sideways falloff.
const WING_LAYERS: [(f32, f32, f32, f32); 4] = [
    (1.00, 0.18, 0.10, 0.45),
    (0.74, 0.38, 0.06, 0.62),
    (0.50, 0.68, 0.025, 0.82),
    (0.28, 1.00, 0.0, 1.00),
];

/// Innermost-layer peak coverage (× charge, pre-cap): a dim idle port, a bright
/// charged core. Every push is also clamped by [`COV_CAP`] (the light rod's
/// text-safety band), since the wings overlap neighbouring glyph cells.
const COV_IDLE: f32 = 46.0;
const COV_MAX: f32 = 132.0;
const COV_CAP: f32 = 96.0;

/// Stack budget for the strip where all four [`WING_LAYERS`] overlap (hugging the cursor's cell
/// edges, where the newest typed glyphs sit). The per-layer [`COV_CAP`] alone lets the sum reach
/// ~280 at full charge and saturate those glyphs, so the shared core is capped so
/// `Σ(layer × core) ≤ budget`, the rainbow ribbon's text-safety ceiling.
const WING_STACK_BUDGET: f32 = 150.0;
/// Σ of the [`WING_LAYERS`] coverage multipliers (the worst-case overlap).
const WING_STACK_OVERLAP: f32 = 2.24;

/// Below this charge the emitter is settled: it reports inactive so the host
/// stops the 60 fps tick (the idle glow rides the blink cadence).
const SETTLED_ENERGY: f32 = 0.02;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct PhaserConfig {
    /// Master on/off (the `phaser` style opted in and the cursor is a focused, visible block).
    pub enabled: bool,
    /// Overall scale `0..1` — the reduced-motion / load-shed amplitude, folded in
    /// by the host exactly like the aurora. 0 ⇒ fully inert (plain themed cursor).
    pub intensity: f32,
    /// The block's colour at rest, `0x00RRGGBB`: a PINNED cursor colour (a configured
    /// `cursor_color` or a program's OSC 12). The beam hue is a tint over it that comes in with
    /// the charge, so a settled caret is exactly the user's cursor colour. The fill leaves the tick as
    /// [`aterm_render::RenderInput::cursor_fill_override`], which replaces the frame cursor
    /// colour, so it must carry the host's colour, as the comet and beam rod do. `None` uses
    /// white on a dark theme ([`BASE_DARK_THEME`]) and the beam's own hue on a light one
    /// ([`light_emitter_fill`]).
    pub base: Option<u32>,
}

/// What a tick produced: the beam-hued block fill to hand the renderer and a fingerprint that
/// changes on every visible step (0 when off).
#[derive(Clone, Copy, Debug)]
pub struct PhaserFrame {
    /// The beam-hued block-fill colour `0x00RRGGBB`, or `None` when the emitter
    /// is off (the renderer then keeps the ordinary themed cursor fill).
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + wings (0 ⇒ nothing this frame).
    pub fp: u64,
}

/// Per-window emitter animation state — one breath phase plus the last clock
/// reading and a latched charge. Tiny + Copy-cheap.
#[derive(Default)]
pub struct CursorPhaser {
    /// Idle-breath phase in turns `0..1`.
    pulse: f32,
    last: Option<Instant>,
    /// Latched charge at the last tick (so [`Self::is_active`] answers without a clock).
    energy: f32,
}

impl CursorPhaser {
    /// Whether the host must keep arming the animation tick: only while the
    /// emitter is charged (typing or cooling). Once settled the idle glow rides
    /// the blink cadence.
    #[inline]
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.energy > SETTLED_ENERGY
    }

    /// Advance one frame at `now` with the beam's rolling `hue` (turns `0..1`,
    /// read from [`crate::cursor_glow::CursorGlow::beam_hue`] after the aurora
    /// tick), typing `energy` (`0..1`), the block cursor cell `cur` (`None`:
    /// hidden), theme darkness, grid `geom` and resolved `cfg`. Appends the
    /// additive wings to `out` and returns the block fill and a fingerprint.
    /// Pure: no wall clock.
    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        hue: f32,
        energy: f32,
        dark_theme: bool,
        geom: Geom,
        cfg: &PhaserConfig,
        out: &mut Vec<GlowQuad>,
    ) -> PhaserFrame {
        let e = (energy.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        // Inert (equal to the plain themed cursor) when off, with degenerate
        // geometry, or at zero amplitude (reduced motion / load shed), like the
        // rainbow block.
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.energy = 0.0; // inert: report settled so the host disarms the tick
            self.last = Some(now);
            return PhaserFrame { fill: None, fp: 0 };
        }
        let was_active = self.energy > SETTLED_ENERGY;
        self.energy = e;

        // Advance the breath only across a continuously charged interval: an
        // idle emitter has no timer, so integrating time on the next unrelated
        // present would make settled pixels jump. A fresh charge starts from the
        // frozen phase.
        let active = e > SETTLED_ENERGY;
        let dt = if was_active && active {
            self.last
                .map(|t| now.saturating_duration_since(t).as_secs_f32())
                .unwrap_or(0.0)
                .min(0.25)
        } else {
            0.0
        };
        self.last = Some(now);
        if active {
            self.pulse = (self.pulse + dt * PULSE_HZ).fract();
        }
        let breath = 0.5 + 0.5 * (self.pulse * std::f32::consts::TAU).sin(); // 0..1

        // The block fill: the cursor's own colour tinted toward the beam's hue with charge. The
        // host floors it against the page, so the glyph stays sharp, unless it wears a pinned
        // cursor colour.
        let fill = match cfg.base {
            // A pinned cursor colour is the settled emitter itself: the beam's
            // hue tints it only with the charge
            // ([`crate::color_math::wear_pinned`]).
            Some(pinned) if e <= SETTLED_ENERGY => pinned & 0x00FF_FFFF,
            Some(pinned) => {
                let beam = hsv2rgb(hue, lerp(SAT_IDLE, SAT_MAX, e), lerp(VAL_IDLE, VAL_MAX, e));
                crate::color_math::wear_pinned(
                    pinned,
                    mix_rgb(pinned, beam, lerp(MIX_IDLE, MIX_MAX, e)),
                    e,
                    SETTLED_ENERGY,
                )
            }
            None if dark_theme => {
                let beam = hsv2rgb(hue, lerp(SAT_IDLE, SAT_MAX, e), lerp(VAL_IDLE, VAL_MAX, e));
                mix_rgb(BASE_DARK_THEME, beam, lerp(MIX_IDLE, MIX_MAX, e))
            }
            None => light_emitter_fill(hue, e),
        };

        // The additive wings, drawn only over a visible in-grid cursor cell,
        // with a small breathing idle floor. Dark themes only: additive lobes
        // cannot brighten a white ground and would only lift neighbouring
        // glyphs; the theme-aware fill is the emitter's tell on light themes.
        let charge = COV_IDLE / COV_MAX * (0.45 + PULSE_DEPTH * breath) + e;
        if dark_theme
            && let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && charge > 0.01
        {
            self.emit_wings(cr, cc, e, charge, hue, geom, cfg, out);
        }

        // Fingerprint: quantized hue, breath, charge and fill, so a settled
        // emitter skips the present but any visible step forces a repaint.
        // Never 0 while enabled (the idle glow is always drawable).
        let fp = ((hue * 1024.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add((charge * 255.0) as u64)
            .wrapping_add(((fill as u64) << 12) ^ ((self.pulse * 64.0) as u64))
            | 1;

        PhaserFrame {
            fill: Some(fill),
            fp,
        }
    }

    /// Emit the two lens-shaped wings as premultiplied additive quads. Bounded by construction:
    /// `WING_LAYERS.len()` layers × (cell_h / slab) slabs × 2 sides.
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor cell + charge/hue + geometry/config; one internal call site"
    )]
    fn emit_wings(
        &self,
        cr: u16,
        cc: u16,
        e: f32,
        charge: f32,
        hue: f32,
        geom: Geom,
        cfg: &PhaserConfig,
        out: &mut Vec<GlowQuad>,
    ) {
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        // Window-absolute cell edges (`Geom` carries the grid origin), so the
        // wings sit on the cursor and carry the rows their pixels overlap.
        let cell_l = i32::from(geom.origin_x) + cc as i32 * geom.cw as i32;
        let cell_r = cell_l + geom.cw as i32;
        let cy = f32::from(geom.origin_y) + (cr as f32 + 0.5) * ch;
        let reach = (REACH_IDLE + (REACH_MAX - REACH_IDLE) * e) * cw;
        // Core coverage, capped so the four-layer overlap strip at the cell
        // edge — over the newest typed glyphs — stays under the stack budget.
        let cov_core =
            (COV_MAX * charge * cfg.intensity).min(WING_STACK_BUDGET / WING_STACK_OVERLAP);

        // Slab height ~1/8 cell (min 2px): fine enough that the tapering lens
        // reads as a point, not a staircase.
        let slab = ((ch * 0.125) as i32).max(2);
        let cy_i32 = cy as i32;
        for &(rx, cov_x, fringe, hx) in &WING_LAYERS {
            let r = reach * rx;
            if r < 1.0 {
                continue;
            }
            let cov = (cov_core * cov_x).min(COV_CAP) as u8;
            if cov == 0 {
                continue;
            }
            // Prism fringe: the left lobe trails the sweep, the right leads it. The core layer
            // (fringe 0) is the pure beam hue on both sides.
            let left_premul = premul_rgb(hsv2rgb(hue - fringe, 0.95, 1.0), cov);
            let right_premul = if fringe == 0.0 {
                left_premul
            } else {
                premul_rgb(hsv2rgb(hue + fringe, 0.95, 1.0), cov)
            };
            let half_h = (ch * 0.5 * hx).max(1.0);
            let half_h_i32 = half_h as i32;
            let mut dy = -half_h_i32;
            while dy < half_h_i32 {
                let h = slab.min(half_h_i32 - dy).max(1);
                // Elliptical lens: half-width at the slab's vertical centre,
                // full reach on the beam axis, tapering to a point at the
                // layer's top and bottom.
                let ym = (dy as f32 + h as f32 * 0.5) / half_h;
                let hw = (r * (1.0 - ym * ym).max(0.0).sqrt()) as i32;
                if hw >= 1 {
                    let y = cy_i32 + dy;
                    push_rect(out, geom, cell_l - hw, y, hw, h, left_premul);
                    push_rect(out, geom, cell_r, y, hw, h, right_premul);
                }
                dy += h;
            }
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
    fn cfg() -> PhaserConfig {
        PhaserConfig {
            enabled: true,
            intensity: 1.0,
            // No host base: these fixtures pin the theme-polar bloom.
            base: None,
        }
    }

    /// Disabled, or zero intensity (reduced motion / load shed) at full
    /// strength: fully inert (no fill, no wings, fp 0, settled), equal to the
    /// plain cursor.
    #[test]
    fn disabled_or_zero_intensity_is_inert() {
        for (why, config) in [
            (
                "disabled",
                PhaserConfig {
                    enabled: false,
                    intensity: 1.0,
                    base: None,
                },
            ),
            (
                "zero intensity",
                PhaserConfig {
                    enabled: true,
                    intensity: 0.0,
                    base: None,
                },
            ),
        ] {
            let mut cp = CursorPhaser::default();
            let mut out = Vec::new();
            let f = cp.tick(
                Some((1, 1)),
                Instant::now(),
                0.3,
                1.0,
                true,
                geom(),
                &config,
                &mut out,
            );
            assert!(f.fill.is_none(), "{why}: keeps the plain cursor");
            assert_eq!(f.fp, 0, "{why}");
            assert!(out.is_empty(), "{why}: no additive light");
            assert!(!cp.is_active(), "{why}: settled");
        }
    }

    /// A hidden cursor draws no wings, but the fill stays resolved (harmless —
    /// the renderer draws no cursor to fill).
    #[test]
    fn hidden_cursor_draws_no_wings() {
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        let f = cp.tick(
            None,
            Instant::now(),
            0.3,
            0.5,
            true,
            geom(),
            &cfg(),
            &mut out,
        );
        assert!(out.is_empty(), "no additive light without a cursor cell");
        assert!(f.fill.is_some());
    }

    /// The block fill follows the beam hue: two well-separated hues at full
    /// charge give clearly different fills, each close to the beam colour.
    #[test]
    fn fill_locks_to_the_beam_hue() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let mut out = Vec::new();
        let mut fill_at = |hue: f32| -> u32 {
            let mut cp = CursorPhaser::default();
            cp.tick(Some((1, 1)), t, hue, 1.0, true, g, &c, &mut out)
                .fill
                .unwrap()
        };
        let red = fill_at(0.0);
        let cyan = fill_at(0.5);
        // Red-ish hue ⇒ red channel dominates; cyan-ish ⇒ red is the weakest.
        let (rr, rg, rb) = ((red >> 16) & 0xff, (red >> 8) & 0xff, red & 0xff);
        let (cr_, cg, cb) = ((cyan >> 16) & 0xff, (cyan >> 8) & 0xff, cyan & 0xff);
        assert!(
            rr > rg + 60 && rr > rb + 60,
            "hue 0 fill is red: {red:#08x}"
        );
        assert!(
            cg > cr_ + 60 && cb > cr_ + 60,
            "hue 0.5 fill is cyan: {cyan:#08x}"
        );
    }

    /// With no host cursor colour ([`PhaserConfig::base`] `None`), the fill
    /// blooms from the theme base (near-white on dark) toward the saturated beam
    /// hue under charge; on a light theme it is the beam's hue itself, clean
    /// and saturated, deep enough to read clearly on white at rest.
    #[test]
    fn fill_blooms_from_theme_base() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let mut out = Vec::new();
        let mut idle = CursorPhaser::default();
        let f_idle = idle
            .tick(Some((1, 1)), t, 0.0, 0.0, true, g, &c, &mut out)
            .fill
            .unwrap();
        let minch = |c: u32| {
            [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff]
                .into_iter()
                .min()
                .unwrap()
        };
        assert!(
            minch(f_idle) > 140,
            "idle block stays near white on dark, got {f_idle:#08x}"
        );
        let mut hot = CursorPhaser::default();
        let f_hot = hot
            .tick(Some((1, 1)), t, 0.0, 1.0, true, g, &c, &mut out)
            .fill
            .unwrap();
        let spread = |c: u32| {
            let (r, gg, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
            r.max(gg).max(b) - r.min(gg).min(b)
        };
        assert!(
            spread(f_hot) > spread(f_idle) + 60,
            "charge saturates the fill toward the beam hue"
        );
        for hue in [0.0, 0.33, 0.6, 0.8] {
            let mut light = CursorPhaser::default();
            let f_light = light
                .tick(Some((1, 1)), t, hue, 0.0, false, g, &c, &mut out)
                .fill
                .unwrap();
            let ratio = crate::color_math::contrast_ratio(f_light, 0x00FF_FFFF);
            let (_, sat, _) = crate::color_math::rgb2hsv(f_light);
            assert!(
                ratio >= crate::cursor_beam::LIGHT_EMITTER_CONTRAST - 0.05 && sat > 0.9,
                "hue {hue}: the idle light block is a clean deep hue, got {f_light:#08x} \
                 ({ratio:.2}:1, saturation {sat:.2})"
            );
        }
    }

    /// ON PAPER THE EMITTER AND ITS BAR STEP OVER THE MUDDY HUES: every beam
    /// phase gives a light block whose hue lies outside
    /// [`crate::color_math::LIGHT_MUDDY_HUES`], and every hue on the light
    /// wheel is one of the others, in order. Negative control: the whole
    /// wheel's yellow, deepened to the same contrast, is the khaki the
    /// judges saw.
    #[test]
    fn the_light_emitter_steps_over_the_muddy_hues() {
        use crate::color_math::{LIGHT_MUDDY_HUES, light_spectrum_hue, rgb2hsv};
        let (b0, b1) = LIGHT_MUDDY_HUES;
        let mut last = -1.0;
        for i in 0..360 {
            let h = light_spectrum_hue(i as f32 / 360.0);
            assert!(
                !(b0..b1).contains(&h),
                "{i} degrees lands in the band at {h}"
            );
            assert!(h > last, "the light wheel keeps the hues in order");
            last = h;
            let fill = light_emitter_fill(i as f32 / 360.0, 0.0);
            let (deg, _, _) = rgb2hsv(fill);
            assert!(
                !(b0 * 360.0 + 1.0..b1 * 360.0 - 1.0).contains(&deg),
                "{i} degrees: the block {fill:06x} is a muddy hue"
            );
        }
        let yellow = crate::color_math::floor_contrast_keeping_hue(
            hsv2rgb(60.0 / 360.0, 1.0, 1.0),
            0x00FF_FFFF,
            crate::cursor_beam::LIGHT_EMITTER_CONTRAST,
        );
        let (r, g, b) = ((yellow >> 16) & 0xff, (yellow >> 8) & 0xff, yellow & 0xff);
        assert!(
            r.max(g) < 0x80 && b == 0,
            "control: yellow deepened to read on white is khaki, got {yellow:06x}"
        );
    }

    /// The host's cursor colour is the emitter block on either theme polarity.
    /// The fill becomes `RenderInput::cursor_fill_override`, which the renderer
    /// applies instead of `frame_cursor(input)`, so a settled caret must be the
    /// host's colour and two colours must never collapse to one block.
    #[test]
    fn host_base_is_the_settled_block_on_either_polarity() {
        let g = geom();
        let mut out = Vec::new();
        let t = Instant::now();
        // A settled emitter (energy 0) with a fixed beam hue: only the base can move this fill
        // between two ticks.
        let settled = |base: u32, dark: bool, out: &mut Vec<GlowQuad>| {
            let c = PhaserConfig {
                base: Some(base),
                ..cfg()
            };
            out.clear();
            CursorPhaser::default()
                .tick(Some((1, 1)), t, 0.5, 0.0, dark, g, &c, out)
                .fill
                .unwrap()
        };
        let chans = |c: u32| ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
        for dark in [true, false] {
            let (rr, rg, rb) = chans(settled(0x00FF_0000, dark, &mut out));
            assert!(
                rr > 200 && rg < 130 && rb < 130,
                "a red cursor colour settles red (dark={dark})"
            );
            let (br, bg, bb) = chans(settled(0x0000_00FF, dark, &mut out));
            assert!(
                bb > 200 && br < 130 && bg < 130,
                "a blue cursor colour settles blue (dark={dark})"
            );
            assert_ne!(
                settled(0x00FF_0000, dark, &mut out),
                settled(0x0000_00FF, dark, &mut out),
                "two cursor colours must not paint one identical block (dark={dark})"
            );
        }
        // …and charge still blooms the beam hue over that base: the charged fill differs
        // markedly.
        let c = PhaserConfig {
            base: Some(0x00FF_0000),
            ..cfg()
        };
        out.clear();
        let hot = CursorPhaser::default()
            .tick(Some((1, 1)), t, 0.5, 1.0, true, g, &c, &mut out)
            .fill
            .unwrap();
        assert_ne!(hot, settled(0x00FF_0000, true, &mut out));
    }

    /// Full charge reaches further sideways and burns far brighter than the idle port glow.
    #[test]
    fn charge_widens_and_brightens_the_wings() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let run = |energy: f32| -> (u64, i32, i32) {
            let mut cp = CursorPhaser::default();
            let mut out = Vec::new();
            cp.tick(Some((3, 20)), t, 0.3, energy, true, g, &c, &mut out);
            out.clear();
            cp.tick(
                Some((3, 20)),
                t + Duration::from_millis(16),
                0.3,
                energy,
                true,
                g,
                &c,
                &mut out,
            );
            let x0 = out.iter().map(|q| q.x as i32).min().unwrap_or(0);
            let x1 = out.iter().map(|q| (q.x + q.w) as i32).max().unwrap_or(0);
            (ink(&out), x0, x1)
        };
        let (idle_ink, idle_x0, idle_x1) = run(0.0);
        let (hot_ink, hot_x0, hot_x1) = run(1.0);
        assert!(idle_ink > 0, "the idle port glow is still visibly lit");
        assert!(
            hot_ink > idle_ink * 2,
            "full charge far outshines idle ({hot_ink} vs {idle_ink})"
        );
        assert!(
            hot_x0 < idle_x0 && hot_x1 > idle_x1,
            "charged wings reach further ({hot_x0}..{hot_x1} vs {idle_x0}..{idle_x1})"
        );
    }

    /// The wings stay on the cursor's row (the beam axis), with no light above
    /// or below, and never touch the cell's own interior columns (the fill owns
    /// the cell).
    #[test]
    fn wings_hug_the_beam_axis() {
        let g = geom();
        let c = cfg();
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        let t = Instant::now();
        let (row, col) = (3u16, 20u16);
        cp.tick(Some((row, col)), t, 0.3, 1.0, true, g, &c, &mut out);
        assert!(!out.is_empty());
        let cell_l = col as u32 * g.cw as u32;
        let cell_r = cell_l + g.cw as u32;
        for q in &out {
            assert_eq!(q.row, row, "wings never leave the cursor row: {q:?}");
            assert!(
                (q.x as u32 + q.w as u32) <= cell_l || q.x as u32 >= cell_r,
                "side lobes only — the fill owns the cell: {q:?}"
            );
        }
    }

    /// Every emitted quad is single-row, inside the grid, with capped coverage, including at a
    /// grid-edge cell where clamping applies.
    #[test]
    fn quads_respect_grid_and_cap() {
        let g = geom();
        let c = cfg();
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        let t = Instant::now();
        for cell in [(0u16, 0u16), (5, 39)] {
            cp.tick(Some(cell), t, 0.7, 1.0, true, g, &c, &mut out);
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
            for sh in [16u32, 8, 0] {
                assert!(
                    (q.color >> sh) & 0xff <= COV_CAP as u32 + 1,
                    "coverage capped: {q:?}"
                );
            }
        }
    }

    /// Charged ⇒ active (host keeps the tick armed); settled charge ⇒ inactive
    /// so the idle port glow rides the blink cadence at no extra wakeup cost.
    #[test]
    fn settles_to_inactive_when_charge_dies() {
        let g = geom();
        let c = cfg();
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        let t = Instant::now();
        cp.tick(Some((1, 1)), t, 0.3, 0.8, true, g, &c, &mut out);
        assert!(cp.is_active(), "a charged emitter keeps the tick armed");
        cp.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.3,
            0.0,
            true,
            g,
            &c,
            &mut out,
        );
        assert!(
            !cp.is_active(),
            "the settled emitter idles on the blink cadence"
        );
    }

    /// The idle port is static: sparse captures or an unrelated repaint after
    /// seconds of silence must not integrate a hidden breath slice and flicker
    /// the cursor edge.
    #[test]
    fn settled_present_gaps_are_byte_identical() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();

        cp.tick(Some((1, 12)), t, 0.3, 0.8, true, g, &c, &mut out);
        out.clear();
        let first = cp.tick(
            Some((1, 12)),
            t + Duration::from_secs(5),
            0.3,
            0.0,
            true,
            g,
            &c,
            &mut out,
        );
        let first_quads = out.clone();
        out.clear();
        let late = cp.tick(
            Some((1, 12)),
            t + Duration::from_secs(30),
            0.3,
            0.0,
            true,
            g,
            &c,
            &mut out,
        );

        assert_eq!(late.fill, first.fill);
        assert_eq!(late.fp, first.fp);
        assert_eq!(out, first_quads);
        assert!(!cp.is_active());
    }

    /// The per-frame quad budget is bounded by construction (4 layers × slabs × 2 sides), far
    /// under the aurora's MAX_QUADS defence.
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
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        cp.tick(
            Some((25, 100)),
            Instant::now(),
            0.3,
            1.0,
            true,
            g,
            &cfg(),
            &mut out,
        );
        assert!(out.len() < 200, "bounded wing geometry, got {}", out.len());
    }
}
