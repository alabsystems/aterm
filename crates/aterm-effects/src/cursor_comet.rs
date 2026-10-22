// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The comet cursor (`comet` trail style): the block cursor is a frosted fill
//! wrapped in a round additive coma, with a few glints twinkling on its rim.
//! It breathes off the aurora's blaze ([`crate::cursor_glow::CursorGlow::blaze`]):
//! dim and slow at rest, swelling and whitening under the keys, white-hot on a
//! jump flare.
//!
//! Text safety: the host floors the fill against the page
//! ([`crate::cursor_fx::floor_block_fill`]), and the renderer's
//! `floor_cursor_fill` adds the user's `minimum_contrast` on top (off by
//! default). On a dark page the coma's stacked heart stays inside the caret
//! cell, under the opaque block; outside it, non-overlapping rings and glints
//! together stay within the over-ink budget
//! ([`crate::cursor_glow::OVER_INK_COV_CAP`]). Pure in the injected `now`; it
//! settles to inactive when the blaze dies, then rides the blink cadence.
//!
//! On a LIGHT GROUND (additive light cannot show on white) the comet has a
//! light mode of its own: the nucleus concentrates its hue with the blaze
//! ([`crate::color_math::light_body_fill`]), held to
//! [`LIGHT_NUCLEUS_CONTRAST`] against the page, instead of frosting; at rest
//! the coma is a round head of ink UNDER the letters, inside the caret's row,
//! its ink solved against the theme's text (`cursor_glow::light_body_ink`),
//! and it fades out as the blaze rises, when the streak's own round nucleus
//! (`CursorGlow::emit_comet_light`) is the head, so no second head stands
//! ahead of the caret while typing. The glints, dust over the letters, are
//! not drawn there.

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::{Geom, OVER_INK_COV_CAP};
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
/// Per-disc additive coverage ceiling, the light rod's and flame curtain's
/// band: the stacked heart inside the caret cell saturates here per disc.
const COV_CAP: f32 = 92.0;

/// The coma's concentric discs, outermost first: (radius ×, coverage ×, the
/// `ice_ramp` position at rest, its climb with the blaze). Accent fringe, hue
/// body, sunlit ice, then the near-white heart.
const COMA_DISCS: [(f32, f32, f32, f32); 4] = [
    (1.00, 0.30, 0.18, 0.06),
    (0.80, 0.58, 0.44, 0.12),
    (0.60, 0.85, 0.66, 0.18),
    (0.38, 1.00, 0.86, 0.14),
];

const fn cov_capped(cov: f32) -> f32 {
    if cov < COV_CAP { cov } else { COV_CAP }
}

/// The most light the stacked discs put on one pixel: every disc at its
/// capped full-blaze coverage, where the breathing and the dial both peak at
/// 1. The worst case the outside budget is solved against.
const COMA_STACK_MAX: f32 = cov_capped(COV_MAX * COMA_DISCS[0].1)
    + cov_capped(COV_MAX * COMA_DISCS[1].1)
    + cov_capped(COV_MAX * COMA_DISCS[2].1)
    + cov_capped(COV_MAX * COMA_DISCS[3].1);

/// The over-ink budget's share the rim glints claim outside the caret cell;
/// the coma's rings take the rest, so rings and a glint landing on them sum
/// to at most [`OVER_INK_COV_CAP`] over a neighbouring glyph.
const GLINT_SHARE: f32 = 0.4;
const RING_BUDGET: f32 = OVER_INK_COV_CAP * (1.0 - GLINT_SHARE);
const GLINT_BUDGET: f32 = OVER_INK_COV_CAP * GLINT_SHARE;

/// Outside the caret cell the stacked coverage is scaled by one constant, so
/// the worst case lands exactly on [`RING_BUDGET`] and a resting coma stays
/// proportionally dimmer: the rings keep their radial falloff and still
/// brighten with the blaze, but can no longer bury the letter just typed.
const COMA_OUTSIDE_SCALE: f32 = RING_BUDGET / COMA_STACK_MAX;
const _: () = assert!(
    COMA_OUTSIDE_SCALE < 1.0 && RING_BUDGET + GLINT_BUDGET <= OVER_INK_COV_CAP,
    "the coma outside the caret must fit the over-ink budget"
);

/// Facet glints (tiny 4-point sparks) on the coma rim.
const GLINTS: u32 = 4;

/// Below this blaze the nucleus reports inactive, so the host stops the tick.
const SETTLED_BLAZE: f32 = 0.02;

/// The near-white the ice freezes toward — deliberately short of pure white so
/// the coma's stacked core (additive) stays the brightest thing on screen. The
/// cell bed's ignition heats toward it too (`cursor_trail::heat_color`), so
/// head and body of one comet share one temperature.
pub(crate) const ICE_WHITE: u32 = 0x00F2_FAFF;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct CometConfig {
    /// On when the `comet` style is chosen and the cursor is a focused, visible
    /// block.
    pub enabled: bool,
    /// Overall scale `0..1`: the aurora's folded intensity (brightness dial ×
    /// reduced motion × load shed), handed over by
    /// [`crate::cursor_fx::CursorFx::tick`] unchanged; 0 is fully inert.
    pub intensity: f32,
    /// The trail's base hue (`GlowConfig::color`); the coma freezes off it.
    pub color: u32,
    /// The trail's accent hue (`GlowConfig::accent`): the coma's outer fringe.
    pub accent: u32,
    /// The ground is dark (the coma is light) or light (the coma is ink
    /// under the letters).
    pub dark_theme: bool,
    /// The theme's text and paper: a light ground's coma solves its ink
    /// against them, and its resting nucleus is held to
    /// [`LIGHT_NUCLEUS_CONTRAST`] against the paper.
    pub theme_fg: u32,
    pub theme_bg: u32,
    /// [`Self::color`] is a PINNED cursor colour (a configured `cursor_color`
    /// or a program's OSC 12, which the trail colour follows): the settled
    /// nucleus is exactly that colour, and only the charge moves it
    /// ([`crate::color_math::wear_pinned`]).
    pub pinned: bool,
}

/// The least WCAG contrast the comet's nucleus keeps against a LIGHT page at
/// rest: a deep, saturated ice that reads as the comet's head, clearly above
/// the caret's page floor ([`crate::cursor_fx::CARET_PAGE_MIN_CONTRAST`])
/// rather than sitting on it. Lightness alone moves
/// ([`crate::color_math::floor_contrast_keeping_hue`]), so the hue holds and
/// nothing whitens; the heat deepens it further from there.
pub const LIGHT_NUCLEUS_CONTRAST: f32 = 6.0;

/// What a tick produced: the nucleus block fill for the renderer and a
/// fingerprint that changes on every visible step (0 when off).
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
    /// `geom` and `cfg`. Appends the additive coma to `out` on a dark ground,
    /// the ink coma to `under` (the under-ink stream) on a light one; returns
    /// the fill and a fingerprint.
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
        cfg: &CometConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
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

        // The block fill. Dark page: a cool tint of the trail hue at rest,
        // frosting toward white as the blaze climbs, capped below the additive
        // core's near-white so the round coma, not the rectangle, is the
        // brightest thing. Light page: the hue concentrating as it heats. The
        // host floors it against the page either way.
        let styled = if cfg.dark_theme {
            ice_ramp(cfg.color, 0.34 + 0.40 * e + 0.05 * (breathe - 0.5))
        } else {
            crate::color_math::floor_contrast_keeping_hue(
                crate::color_math::light_body_fill(cfg.color, e),
                cfg.theme_bg & 0x00FF_FFFF,
                LIGHT_NUCLEUS_CONTRAST,
            )
        };
        // A pinned cursor colour is the settled nucleus itself; the frost
        // (or the deepening) comes in only with the charge.
        let fill = if cfg.pinned {
            crate::color_math::wear_pinned(cfg.color, styled, e, SETTLED_BLAZE)
        } else {
            styled
        };

        // The coma, drawn only over a visible in-grid cursor cell: light on a
        // dark ground, ink under the letters on a light one.
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
        {
            if cfg.dark_theme {
                self.emit_coma(cr, cc, e, breathe, phase, geom, cfg, out);
            } else {
                Self::emit_coma_light(cr, cc, e, breathe, geom, cfg, under);
            }
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

    /// The coma on a LIGHT GROUND: a round head of ink under the letters,
    /// centred on the caret and held inside its row, a cell either side, its
    /// ink solved against the theme's text; it breathes with the shimmer as
    /// the dark coma does. The resting head: dim at rest and gone at full
    /// blaze, where the streak's own nucleus (`CursorGlow::emit_comet_light`)
    /// is the head, so typing never stands a second head of ink ahead of the
    /// caret. No glints: dust inked over the letters is noise there
    /// (8570e153e).
    fn emit_coma_light(
        cr: u16,
        cc: u16,
        e: f32,
        breathe: f32,
        geom: Geom,
        cfg: &CometConfig,
        under: &mut Vec<GlowQuad>,
    ) {
        use crate::cursor_glow::{LIGHT_BODY_ALPHA_CAP, light_body_ink, push_ink_ellipse};
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let centre = geom.cell_center(cr, cc);
        let ink = light_body_ink(
            cfg.color,
            cfg.theme_fg,
            cfg.theme_bg,
            LIGHT_BODY_ALPHA_CAP as u8,
        );
        let peak = LIGHT_BODY_ALPHA_CAP
            * (0.35 + 0.65 * e)
            * (1.0 - e)
            * (0.86 + 0.14 * breathe)
            * cfg.intensity.clamp(0.0, 1.0);
        let budget = under
            .len()
            .saturating_add(crate::cursor_glow::LIGHT_MARK_QUADS);
        push_ink_ellipse(
            under,
            geom,
            centre,
            (cw * (0.9 + 0.3 * e), cw * (0.9 + 0.3 * e)),
            ch * 0.46,
            ink,
            peak,
            budget,
        );
    }

    /// Emit the coma + rim glints as premultiplied additive quads (dark page).
    ///
    /// Inside the caret cell the concentric discs stack as before, each
    /// clipped to the cell (the opaque block covers them; a translucent
    /// cursor shows the heart). Outside it the same profile is laid as
    /// non-overlapping rings on one shared slab grid, each ring carrying the
    /// summed light of the discs over it scaled by [`COMA_OUTSIDE_SCALE`],
    /// and the glints take at most [`GLINT_BUDGET`], so no pixel over a
    /// neighbouring glyph carries more than [`OVER_INK_COV_CAP`]. Bounded by
    /// construction: a few slab rows × at most fourteen spans, plus the
    /// clipped discs and [`GLINTS`] crosses.
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
        let cell_x0 = i32::from(geom.origin_x) + i32::from(cc) * geom.cw as i32;
        let cell_y0 = i32::from(geom.origin_y) + i32::from(cr) * geom.ch as i32;
        let (cell_x1, cell_y1) = (cell_x0 + geom.cw as i32, cell_y0 + geom.ch as i32);
        let radius = (RADIUS_IDLE + (RADIUS_MAX - RADIUS_IDLE) * e) * ch * (0.96 + 0.06 * breathe);
        let cov_core =
            (COV_IDLE + (COV_MAX - COV_IDLE) * e) * cfg.intensity * (0.86 + 0.14 * breathe);

        // Each disc's radius, coverage and colour (the outermost fringe
        // carries the ACCENT, the gas envelope a shade apart from the dust).
        let discs = COMA_DISCS.map(|(rx, cov_x, t0, dt)| {
            let ramp = ice_ramp(cfg.color, t0 + dt * e);
            let color = if rx >= 1.0 {
                lerp_rgb(cfg.accent, ramp, 0.45)
            } else {
                ramp
            };
            (radius * rx, (cov_core * cov_x).min(COV_CAP), color)
        });
        // Slab height ~1/8 cell (min 2px): fine enough to read as round.
        let slab = ((ch * 0.125) as i32).max(2);
        // A small travelling wobble keeps the silhouette from reading as a
        // sticker; one factor per row, so the discs stay nested.
        let wobble = |ym: f32| 1.0 + 0.05 * (ym * 0.55 / ch.max(1.0) * 10.0 + phase * 1.3).sin();

        // The heart, inside the caret cell: the stacked discs, clipped.
        let (cxi, cyi) = (cx as i32, cy as i32);
        for &(r, cov, color) in &discs {
            let cov = cov as u8;
            if r < 1.0 || cov == 0 {
                continue;
            }
            let premul = premul_rgb(color, cov);
            let r2 = r * r;
            let r_i = r as i32;
            let mut dy = -r_i;
            while dy < r_i {
                let h = slab.min(r_i - dy);
                let (y0, y1) = ((cyi + dy).max(cell_y0), (cyi + dy + h).min(cell_y1));
                if y1 > y0 {
                    let ym = dy as f32 + h as f32 * 0.5;
                    let hw = ((r2 - ym * ym).max(0.0).sqrt() * wobble(ym)) as i32;
                    let (x0, x1) = ((cxi - hw).max(cell_x0), (cxi + hw).min(cell_x1));
                    if hw >= 1 && x1 > x0 {
                        push_rect(out, geom, x0, y0, x1 - x0, y1 - y0, premul);
                    }
                }
                dy += h;
            }
        }

        // The rings, outside the caret cell, on one slab grid anchored at the
        // cell's top edge so no slab straddles the cell.
        let outer = discs[0].0;
        if outer >= 1.0 {
            let mut ring_premul = [0u32; 4];
            let (mut sum, mut acc) = (0.0f32, [0.0f32; 3]);
            for i in 0..discs.len() {
                let (_, cov, color) = discs[i];
                sum += cov;
                for (k, sh) in [16u32, 8, 0].into_iter().enumerate() {
                    acc[k] += ((color >> sh) & 0xff) as f32 * cov;
                }
                if sum <= 0.0 {
                    continue;
                }
                let cov = (sum * COMA_OUTSIDE_SCALE) as u8;
                if cov == 0 {
                    continue;
                }
                let avg = |k: usize| ((acc[k] / sum).round() as u32).min(255);
                ring_premul[i] = premul_rgb((avg(0) << 16) | (avg(1) << 8) | avg(2), cov);
            }
            let top = (cy - outer).floor() as i32;
            let bot = (cy + outer).ceil() as i32;
            let mut y = cell_y0 - (cell_y0 - top).div_euclid(slab) * slab - slab;
            while y < bot {
                let mut h = slab;
                if y < cell_y1 && y + h > cell_y1 {
                    h = cell_y1 - y;
                }
                if y + h > top {
                    let in_band = y >= cell_y0 && y < cell_y1;
                    ring_row(
                        &discs,
                        &ring_premul,
                        (cx, cy),
                        (y, h),
                        wobble,
                        in_band,
                        (cell_x0, cell_x1),
                        geom,
                        out,
                    );
                }
                y += h;
            }
        }

        // Facet glints: a few tiny 4-point sparks orbiting on the coma rim with
        // the shimmer phase, each twinkling on its own offset. They ride at
        // ~0.72·radius, and each is a non-overlapping hairline cross held to
        // [`GLINT_BUDGET`], so with the rings under it no neighbouring letter
        // takes more than the over-ink budget.
        let glint_r = radius * 0.72;
        let arm = ((ch * 0.11) as i32).max(1);
        let cov_base = cov_core * (0.35 + 0.65 * e);
        for i in 0..GLINTS {
            let fi = i as f32;
            let ang = fi / GLINTS as f32 * std::f32::consts::TAU + phase * 0.55;
            let tw = 0.5 + 0.5 * (phase * 2.0 + fi * 2.4).sin();
            // Only the bright half of each twinkle draws: glints flash.
            if tw < 0.55 {
                continue;
            }
            let cov = (cov_base * (tw - 0.55) / 0.45).min(GLINT_BUDGET) as u8;
            if cov == 0 {
                continue;
            }
            let gx = (cx + ang.cos() * glint_r) as i32;
            let gy = (cy + ang.sin() * glint_r * 0.9) as i32;
            let spark = premul_rgb(ICE_WHITE, cov);
            push_rect(out, geom, gx - arm, gy, 2 * arm + 1, 1, spark);
            push_rect(out, geom, gx, gy - arm, 1, arm, spark);
            push_rect(out, geom, gx, gy + 1, 1, arm, spark);
        }
    }
}

/// One slab row `[y, y + h)` of the outside rings: band `i` spans
/// between disc `i`'s half-width and disc `i + 1`'s and carries the
/// coverage-weighted colour of discs `0..=i` at their summed coverage ×
/// [`COMA_OUTSIDE_SCALE`]. In the caret row's band the cell's own columns
/// are cut out (the heart lives there).
#[allow(
    clippy::too_many_arguments,
    reason = "the slab row, the shared disc table and the cell cut; one call site"
)]
fn ring_row(
    discs: &[(f32, f32, u32); 4],
    ring_premul: &[u32; 4],
    (cx, cy): (f32, f32),
    (y, h): (i32, i32),
    wobble: impl Fn(f32) -> f32,
    in_band: bool,
    (cell_x0, cell_x1): (i32, i32),
    geom: Geom,
    out: &mut Vec<GlowQuad>,
) {
    let ym = y as f32 + h as f32 * 0.5 - cy;
    let ym2 = ym * ym;
    let wob = wobble(ym);
    let hw = discs.map(|(r, _, _)| (r * r - ym2).max(0.0).sqrt() * wob);
    let x_edges = hw.map(|w| ((cx - w).round() as i32, (cx + w).round() as i32));
    let cx_round = cx.round() as i32;
    for i in 0..discs.len() {
        let premul = ring_premul[i];
        if hw[i] < 1.0 || premul == 0 {
            continue;
        }
        let (xo0, xo1) = x_edges[i];
        let (xi0, xi1) = x_edges.get(i + 1).copied().unwrap_or((cx_round, cx_round));
        let spans: [(i32, i32); 2] = if xi1 > xi0 {
            [(xo0, xi0), (xi1, xo1)]
        } else {
            [(xo0, xo1), (0, 0)]
        };
        for (a, b) in spans {
            let pieces: [(i32, i32); 2] = if in_band {
                [(a, b.min(cell_x0)), (a.max(cell_x1), b)]
            } else {
                [(a, b), (0, 0)]
            };
            for (x0, x1) in pieces {
                if x1 > x0 {
                    push_rect(out, geom, x0, y, x1 - x0, h, premul);
                }
            }
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
            dark_theme: true,
            theme_fg: 0x00C8_C8D2,
            theme_bg: 0x0010_1016,
            pinned: false,
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
            let f = nc.tick(
                Some((1, 1)),
                Instant::now(),
                1.0,
                geom(),
                &config,
                &mut out,
                &mut Vec::new(),
            );
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
        let f = nc.tick(
            None,
            Instant::now(),
            0.5,
            geom(),
            &cfg(),
            &mut out,
            &mut Vec::new(),
        );
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
            nc.tick(Some((3, 20)), t, blaze, g, &c, &mut out, &mut Vec::new());
            out.clear();
            nc.tick(
                Some((3, 20)),
                t + Duration::from_millis(16),
                blaze,
                g,
                &c,
                &mut out,
                &mut Vec::new(),
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

    /// A light config: GitHub Light's text and paper, its cursor blue.
    fn light_cfg() -> CometConfig {
        CometConfig {
            color: 0x0009_69DA,
            accent: 0x0040_90F0,
            dark_theme: false,
            theme_fg: 0x001F_2328,
            theme_bg: 0x00FF_FFFF,
            ..cfg()
        }
    }

    fn contrast(a: u32, b: u32) -> f32 {
        let (x, y) = (
            crate::color_math::relative_luminance(a),
            crate::color_math::relative_luminance(b),
        );
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// ON PAPER THE NUCLEUS INKS A ROUND HEAD UNDER THE LETTERS AT REST: the
    /// additive stream stays empty, and the resting coma is source-over ink
    /// in the under-ink stream, inside the caret's row, reaching past both
    /// edges of the caret's cell; at full blaze it is gone, the streak's own
    /// nucleus being the head then. Negative control: a dark ground draws the
    /// additive coma and no ink.
    #[test]
    fn light_nucleus_inks_a_round_head_under_the_letters() {
        let g = geom();
        let (row, col) = (3u16, 20u16);
        let inked = |blaze: f32| {
            let mut nc = CursorComet::default();
            let (mut out, mut under) = (Vec::new(), Vec::new());
            nc.tick(
                Some((row, col)),
                Instant::now(),
                blaze,
                g,
                &light_cfg(),
                &mut out,
                &mut under,
            );
            assert!(out.is_empty(), "no additive light on paper");
            under
        };
        assert!(
            inked(1.0).is_empty(),
            "at full blaze the streak's nucleus is the head"
        );
        let width = |blaze: f32| {
            let mut nc = CursorComet::default();
            let (mut out, mut under) = (Vec::new(), Vec::new());
            nc.tick(
                Some((row, col)),
                Instant::now(),
                blaze,
                g,
                &light_cfg(),
                &mut out,
                &mut under,
            );
            assert!(out.is_empty(), "no additive light on paper");
            assert!(!under.is_empty(), "the head is inked (blaze {blaze})");
            let top = row * g.ch as u16;
            assert!(
                under
                    .iter()
                    .all(|q| q.alpha > 0 && q.y >= top && q.y + q.h <= top + g.ch as u16),
                "source-over, inside its row"
            );
            let x0 = under.iter().map(|q| q.x).min().unwrap_or(0);
            let x1 = under.iter().map(|q| q.x + q.w).max().unwrap_or(0);
            let (left, right) = (col * g.cw as u16, (col + 1) * g.cw as u16);
            assert!(
                x0 + g.cw as u16 / 4 <= left && x1 >= right + g.cw as u16 / 4,
                "past both edges of the caret's cell: {x0}..{x1}"
            );
            x1 - x0
        };
        assert!(
            width(0.1) > g.cw as u16,
            "the resting head is round and wide"
        );
        let mut nc = CursorComet::default();
        let (mut out, mut under) = (Vec::new(), Vec::new());
        nc.tick(
            Some((row, col)),
            Instant::now(),
            0.5,
            g,
            &cfg(),
            &mut out,
            &mut under,
        );
        assert!(
            !out.is_empty() && under.is_empty(),
            "dark: the additive coma"
        );
    }

    /// ON PAPER THE NUCLEUS DEEPENS UNDER THE KEYS: hot, the block fill holds
    /// at least the contrast against the paper it has at rest, and at least
    /// the trail colour's own (white sinks into the page, so it never frosts
    /// there). Negative control: on a dark ground the hot fill is lighter.
    #[test]
    fn light_ground_nucleus_deepens_under_the_keys() {
        let g = geom();
        let fill = |c: &CometConfig, blaze: f32| {
            let mut nc = CursorComet::default();
            nc.tick(
                Some((1, 1)),
                Instant::now(),
                blaze,
                g,
                c,
                &mut Vec::new(),
                &mut Vec::new(),
            )
            .fill
            .expect("a fill")
        };
        let c = light_cfg();
        let (cold, hot) = (fill(&c, 0.0), fill(&c, 1.0));
        let paper = c.theme_bg;
        assert!(contrast(hot, paper) >= contrast(cold, paper));
        assert!(contrast(hot, paper) >= contrast(c.color, paper));
        let luma = crate::color_math::relative_luminance;
        let d = cfg();
        assert!(luma(fill(&d, 1.0)) > luma(fill(&d, 0.0)), "dark: it frosts");
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
        nc.tick(Some((0, 0)), t, 1.0, g, &c, &mut out, &mut Vec::new());
        nc.tick(
            Some((0, 0)),
            t + Duration::from_millis(16),
            1.0,
            g,
            &c,
            &mut out,
            &mut Vec::new(),
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
        nc.tick(Some((1, 1)), t, 0.8, g, &c, &mut out, &mut Vec::new());
        assert!(nc.is_active(), "a charged nucleus keeps the tick armed");
        nc.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.0,
            g,
            &c,
            &mut out,
            &mut Vec::new(),
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
        let f = nc
            .tick(Some((1, 1)), t, 0.2, g, &c, &mut out, &mut Vec::new())
            .fill
            .unwrap();
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
        e.tick(
            Some((cr, cc)),
            t0,
            1.0,
            real,
            &cfg(),
            &mut out,
            &mut Vec::new(),
        );
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
        f.tick(
            Some((3, 10)),
            t0,
            1.0,
            geom(),
            &cfg(),
            &mut at_origin,
            &mut Vec::new(),
        );
        let mut shifted = Vec::new();
        let mut g = CursorComet::default();
        let mut o = geom();
        o.origin_x = 24;
        o.origin_y = 84;
        o.win_w += 48;
        o.win_h += 84;
        o.head = 84;
        g.tick(
            Some((3, 10)),
            t0,
            1.0,
            o,
            &cfg(),
            &mut shifted,
            &mut Vec::new(),
        );
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

    /// Every additive quad of `out` summed per window pixel, as the
    /// brightest channel: the light that lands on one pixel.
    fn light_per_pixel(out: &[GlowQuad], w: usize, h: usize) -> Vec<u32> {
        let mut px = vec![[0u32; 3]; w * h];
        for q in out {
            assert_eq!(q.alpha, 0, "the dark coma is additive light");
            for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                    for (k, sh) in [16u32, 8, 0].into_iter().enumerate() {
                        px[y * w + x][k] += (q.color >> sh) & 0xff;
                    }
                }
            }
        }
        px.iter().map(|c| c[0].max(c[1]).max(c[2])).collect()
    }

    /// The letter just typed beside the caret is never buried: at full
    /// blaze, across a run of shimmer phases (so the rim glints turn
    /// through), no pixel outside the caret cell sums more additive light
    /// than the over-ink budget. Negative control: inside the cell, where the
    /// opaque block covers it, the stacked heart does exceed the budget, so
    /// the raster does see overlap.
    #[test]
    fn the_coma_outside_the_caret_stays_inside_the_over_ink_budget() {
        let g = geom();
        let (w, h) = (g.cols * g.cw, g.rows * g.ch);
        let (cr, cc) = (3u16, 20u16);
        let (x0, y0) = (usize::from(cc) * g.cw, usize::from(cr) * g.ch);
        let mut nc = CursorComet::default();
        let t = Instant::now();
        let mut heart_max = 0u32;
        for i in 0..40u64 {
            let mut out = Vec::new();
            nc.tick(
                Some((cr, cc)),
                t + Duration::from_millis(16 * i),
                1.0,
                g,
                &cfg(),
                &mut out,
                &mut Vec::new(),
            );
            let light = light_per_pixel(&out, w, h);
            for y in 0..h {
                for x in 0..w {
                    let inside = (x0..x0 + g.cw).contains(&x) && (y0..y0 + g.ch).contains(&y);
                    let l = light[y * w + x];
                    if inside {
                        heart_max = heart_max.max(l);
                    } else {
                        assert!(
                            l as f32 <= OVER_INK_COV_CAP + 3.0,
                            "frame {i}: {l} levels of light on ({x},{y}) outside the caret"
                        );
                    }
                }
            }
        }
        assert!(
            heart_max as f32 > OVER_INK_COV_CAP,
            "control: the stacked heart inside the caret ({heart_max}) exceeds the budget"
        );
    }

    /// On a light page the comet keeps its caret and its identity: the
    /// nucleus clears 3:1 on white at full blaze, and it lays no light over
    /// the letters (its coma is the streak's head under them). Negative
    /// control: the dark arm on the same page frosts the caret under 3:1 and
    /// lays its additive coma.
    #[test]
    fn the_light_comet_keeps_its_caret_and_lays_nothing_over_the_letters() {
        const WHITE: u32 = 0x00FF_FFFF;
        let g = geom();
        let (cr, cc) = (3u16, 20u16);
        let cell = (
            i32::from(cc) * g.cw as i32,
            i32::from(cr) * g.ch as i32,
            g.cw as i32,
            g.ch as i32,
        );
        let caret = |dark: bool| {
            let mut nc = CursorComet::default();
            let mut out = Vec::new();
            let c = CometConfig {
                dark_theme: dark,
                theme_fg: if dark { 0x00C8_C8D2 } else { 0x001F_2328 },
                theme_bg: if dark { 0x0010_1016 } else { WHITE },
                ..cfg()
            };
            let fill = nc
                .tick(
                    Some((cr, cc)),
                    Instant::now(),
                    1.0,
                    g,
                    &c,
                    &mut out,
                    &mut Vec::new(),
                )
                .fill
                .expect("an enabled nucleus fills the block");
            let worst = crate::effect_util::caret_cell_worst_contrast(fill, &out, cell, WHITE);
            (worst, out)
        };
        let (light, over) = caret(false);
        assert!(
            light >= 3.0,
            "light arm: the nucleus must keep 3:1 on white, got {light:.2}:1"
        );
        assert!(
            over.is_empty(),
            "the light nucleus lays nothing over the letters: {over:?}"
        );
        let (dark_arm, coma) = caret(true);
        assert!(
            dark_arm < 3.0,
            "control: the frosting dark arm sinks into white ({dark_arm:.2}:1)"
        );
        assert!(!coma.is_empty(), "control: the dark arm lays its coma");
    }

    /// At rest on a light page the nucleus reads clearly above the caret's
    /// page floor, not on it: on white, Default Light and Solarized Light the
    /// settled fill keeps [`LIGHT_NUCLEUS_CONTRAST`] against the page, with
    /// the hue of the comet's ice and nothing whitened, and the heat only
    /// deepens it from there. Negative control: the deepened hue alone, with
    /// no page in hand, sits far under the 4.5:1 floor on white, so the
    /// solve against the page is what lifts it.
    #[test]
    fn the_light_nucleus_rests_clearly_above_the_page_floor() {
        use crate::color_math::{contrast_ratio, light_body_fill, rgb2hsv};
        let base = cfg().color;
        for page in [0x00FF_FFFFu32, 0x00F6_F7F9, 0x00FD_F6E3] {
            let fill_at = |blaze: f32| {
                let c = CometConfig {
                    dark_theme: false,
                    theme_bg: page,
                    ..cfg()
                };
                CursorComet::default()
                    .tick(
                        Some((1, 1)),
                        Instant::now(),
                        blaze,
                        geom(),
                        &c,
                        &mut Vec::new(),
                        &mut Vec::new(),
                    )
                    .fill
                    .expect("an enabled nucleus fills the block")
            };
            let rest = fill_at(0.0);
            let ratio = contrast_ratio(rest, page);
            assert!(
                ratio >= LIGHT_NUCLEUS_CONTRAST,
                "on {page:06x} the resting nucleus {rest:06x} reads {ratio:.2}:1"
            );
            let (h0, _, _) = rgb2hsv(light_body_fill(base, 0.0));
            let (h1, _, v1) = rgb2hsv(rest);
            assert!((h0 - h1).abs() < 4.0, "the ice keeps its hue: {h0} vs {h1}");
            assert!(
                v1 < rgb2hsv(base).2,
                "on {page:06x} the nucleus deepens, never whitens: {rest:06x}"
            );
            let hot = fill_at(1.0);
            assert!(
                contrast_ratio(hot, page) >= ratio,
                "on {page:06x} the heat deepens the nucleus further"
            );
        }
        let unsolved = contrast_ratio(light_body_fill(base, 0.0), 0x00FF_FFFF);
        assert!(
            unsolved < 4.5,
            "control: the deepened ice alone reads {unsolved:.2}:1 on white"
        );
    }
}
