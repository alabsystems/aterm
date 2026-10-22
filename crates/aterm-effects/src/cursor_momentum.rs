// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing-momentum glow: the cursor glows with how fast you type and cools when
//! you stop. While warm it does not blink.
//!
//! # Not [`crate::typing_momentum::TypingMomentum`]
//!
//! That metric is rate-normalised (time spent typing, not key count), so it cannot glow brighter
//! for faster typing; its τ = 2 s arc gates cat drama; and it advances only on echo-correlated
//! typing inside an enabled `CursorGlow`. This glow is a cursor feature for any style or shape,
//! trail on or off. It reuses the integrator pattern (a `Copy` state, an `Option<Instant>` stamp,
//! lazy decay, snap-to-zero), which keeps it pure in the injected `now`.
//!
//! # The law
//!
//! * One key adds [`MOMENTUM_GLOW_KEY_CREDIT`] (0.10) to the decayed value,
//!   clamped to 1. A steady rate `r` keys/s settles at `K / (1 − exp(−1/(r·τ)))`:
//!   with τ = 1.2 s that is 0.18 at 1/s, 0.29 at 2/s, 0.53 at 4/s, and 1.0 from
//!   ≈ 8/s, monotone in rate.
//! * Between keys it decays on [`MOMENTUM_GLOW_TAU_S`]: half in 0.83 s, and from
//!   1.0 to the floor ([`MOMENTUM_GLOW_HOT`], 0.02) in `τ·ln 50 ≈ 4.7 s`.
//! * Only printable typing feeds it; deletes, kills and navigation neither build
//!   nor drain it.
//! * At or above the floor the cursor is hot: the host pins a Blinking* style to
//!   its Steady* twin (the rainbow twinkle's precedence) and the halo is drawn.
//!   Fully cooled, the cursor is exactly the configured one.
//!
//! # What it draws
//!
//! A warm tint on the block body and a soft halo around the cursor cell, both
//! ramping amber → gold → white-hot with momentum. On a light page the body
//! concentrates in its own hue instead (`color_math::light_charge_in_hue`)
//! and the halo wears the style's hue (below). The halo is one radial profile, `(1 − ρ²)²` over an ellipse that
//! reaches [`MOMENTUM_GLOW_RADIUS_CELLS`] past the cell at full momentum, laid
//! as per-row horizontal ramps ([`GlowQuad::color2`]) that never overlap, so it
//! falls off smoothly instead of in stepped rings. Its peak is at most
//! [`HALO_PEAK_COV`] (the over-ink budget, [`OVER_INK_COV_CAP`]), rising with
//! the momentum, so no pixel it lights carries more than the budget: not the
//! letter just typed beside the caret, not the rows above and below, and not
//! the caret's own cell under a bar or underline caret, where a glyph shows.
//! At full momentum it is as large as it ever was, and dimmer.
//!
//! On a dark page the halo is additive light. On a light page additive light
//! is invisible and would only lift the ink, so the glow is ink beneath the
//! letters instead (the under-ink stream, where Lumen's light glow and the
//! beam family's light bodies are): one round mark
//! (`cursor_glow::push_ink_ellipse`) in the style's own hue
//! ([`MomentumGlowConfig::hue`]), as wide as the dark halo and held inside
//! the caret's row, so it melts to nothing at the row's edges and the rows
//! above and below stay clean paper. Its ink is solved against the theme's
//! text (`cursor_glow::light_body_ink`), so the letters it lies under keep
//! the light bar even where it stacks on a light body; it peaks at 0.85 of
//! the body ceiling. Over the caret row's letters it keeps only the opacity
//! that leaves them [`LIGHT_GLOW_TEXT_KEEP`] of their reading
//! ([`MomentumGlowConfig::glyph_span`]), so the glow shows ahead of the caret
//! and around it while the letter just typed reads as it would without it.
//!
//! # When it asks for frames
//!
//! Everything drawn is a function of the amplitude's `u8` step, so the
//! fingerprint changes exactly when the glass does. While it changes frame to
//! frame the glow is [`MomentumGlow::is_active`] (frame cadence). Once two
//! frames match it is [`MomentumGlow::settling`]: the cool-down still owes
//! steps, a few frames apart and then further, and the host wakes it at the
//! analytic instant of the next one ([`MomentumGlow::next_change_deadline`]),
//! down to the crossing below the hot floor, whose frame clears the halo, the
//! tint and the blink pin. Neither arm is a poll.
//!
//! The halo is in window px (`Geom::cell_center` + `push_fx_ramp`), the
//! `cursor_glow_add` stream's contract
//! (`halo_surrounds_the_caret_in_a_padded_headed_window`); at origin (0,0) it
//! matches the grid form.
//!
//! Off (disabled, intensity 0, degenerate geometry, no cursor, or cold) equals
//! no effect: `fill: None`, `hot: false`, `fp: 0`, scratch untouched.
//!
//! # Hosted as an Aos task
//!
//! Two pure additions let the glow run as a checkpointed, replayable task. The
//! engine above does not change.
//! * [`snapshot`]: [`MomentumGlow::snapshot`] and [`MomentumGlow::restore`]
//!   capture the semantic state fieldwise, with instants as signed offsets from
//!   a declared epoch. [`MomentumGlowSnapshot`] has a fixed canonical encoding
//!   with a schema, and restore refuses every state that breaks an invariant
//!   the engine's transitions keep, or a bound of the hosted profile. The
//!   light-ink memo is a cache, so it is left out.
//! * [`admission`]: [`MOMENTUM_PROFILE`] and the `admit_*` checks refuse, before
//!   the call, any input outside the range where every engine call is total
//!   and bounded. [`admit_tick`] returns the worst-case [`ScratchBudget`] the host
//!   reserves so that a tick never reallocates.

pub mod admission;
pub mod snapshot;
#[cfg(test)]
mod test_fixtures;

pub use admission::{
    AdmitRefusal, GeometryFault, MOMENTUM_ENVELOPE, MOMENTUM_PROFILE, MomentumProfile,
    ScratchBudget, admit_frame_interval, admit_tau, admit_tick, admit_time,
};
pub use snapshot::{
    MOMENTUM_GLOW_SNAPSHOT_SCHEMA, MomentumGlowSnapshot, SnapshotField, SnapshotRefusal,
};

use std::time::Duration;

use aterm_render::{GlowQuad, premul_rgb};
use aterm_time::Instant;

use crate::cursor_glow::{Geom, OVER_INK_COV_CAP};
use crate::effect_util::mix_rgb;
use crate::effect_util::push_fx_ramp;

/// What one printable key adds, from wherever the decayed value is.
pub const MOMENTUM_GLOW_KEY_CREDIT: f32 = 0.10;

/// The cool-down time constant in seconds (half-life ≈ 0.83 s).
pub const MOMENTUM_GLOW_TAU_S: f32 = 1.2;

/// At or above this the cursor is hot: blink suppressed, halo drawn. Below it the cursor is
/// exactly the configured one. Matches the rainbow twinkle's settled-energy floor.
///
/// [`MomentumGlow::tick`] compares the rate scaled by `intensity` (what is
/// drawn), so a dimmed glow cools sooner than the hand does; the latched,
/// post-intensity value is what [`MomentumGlow::is_active`],
/// [`MomentumGlow::settling`] and [`MomentumGlow::cold_at`] read.
pub const MOMENTUM_GLOW_HOT: f32 = 0.02;

/// Snap-to-exact-zero threshold: below this the value reads 0 and the state
/// unstamps, so a cold cursor stays cold on every frame with no drift.
const SNAP_ZERO: f32 = 0.005;

/// Halo reach in cell widths at full momentum, beyond the cell's own half.
pub const MOMENTUM_GLOW_RADIUS_CELLS: f32 = 1.6;

/// The halo's peak premultiplied coverage at full momentum: the over-ink
/// budget, spent once. The halo is one profile of non-overlapping quads
/// peaking at the caret, so this bounds every pixel it lights at any momentum
/// (`no_pixel_carries_more_than_the_over_ink_budget`).
const HALO_PEAK_COV: f32 = OVER_INK_COV_CAP;

/// Linear ramp pieces per side of each halo row: the `(1 − ρ²)²` profile to
/// within about 2 % of its peak.
const HALO_SEGMENTS: usize = 4;

/// Halo row height is `ch / HALO_ROW_DIV`, at least 2 px: a few levels of
/// light from one row to the next at most, so the vertical falloff is smooth.
const HALO_ROW_DIV: f32 = 12.0;

/// The halo's colour climbs the ramp toward its core by up to this much
/// momentum, full inside [`HALO_WARM_CORE`] of the radius and none at the rim,
/// so the centre burns hotter than the edge.
const HALO_WARMTH: f32 = 0.7;
const HALO_WARM_CORE: f32 = 0.42;

/// The light-page glow's peak opacity at full momentum, beneath the letters:
/// 0.85 of the body ceiling ([`crate::cursor_glow::LIGHT_BODY_ALPHA_CAP`]).
/// Its ink ([`light_glow_ink`]) is solved so that at any opacity, over the
/// paper or over a style's light body of any hue, the theme's text keeps the
/// light bar (`the_light_glow_stacked_on_a_light_body_keeps_the_light_bar`).
pub(crate) const LIGHT_GLOW_PEAK: f32 = crate::cursor_glow::LIGHT_BODY_ALPHA_CAP * 0.85;

/// A settle wake lands this long after the predicted step, so a float ulp
/// between the forecast and the tick never wakes a frame just short of the
/// step it was armed for.
const SETTLE_MARGIN: Duration = Duration::from_millis(1);

/// The typing-rate integrator — pure in the injected `now`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TypingRate {
    /// Value at the `at` stamp (reads decay it lazily from there).
    value: f32,
    /// The last key stamp; `None` ⇒ at rest (0).
    at: Option<Instant>,
}

impl TypingRate {
    /// The value at `now`: decayed over the elapsed gap on `tau`, snapped to
    /// exactly 0 once negligible. NaN/garbage in `tau` reads as 0 — inert.
    #[must_use]
    #[inline]
    pub fn value(&self, now: Instant, tau: f32) -> f32 {
        if self.value < SNAP_ZERO {
            return 0.0;
        }
        let Some(at) = self.at else {
            return 0.0;
        };
        if !tau.is_finite() || tau <= 0.0 {
            return 0.0;
        }
        let dt = now.saturating_duration_since(at).as_secs_f32();
        let v = if dt == 0.0 {
            self.value
        } else {
            self.value * (-dt / tau).exp()
        };
        if !v.is_finite() || v < SNAP_ZERO {
            0.0
        } else {
            v.min(1.0)
        }
    }

    /// One printable key at `now`: decay to `now`, credit, clamp to 1, stamp.
    #[inline]
    pub fn on_key(&mut self, now: Instant, tau: f32) {
        self.value = (self.value(now, tau) + MOMENTUM_GLOW_KEY_CREDIT).min(1.0);
        self.at = Some(now);
    }
}

/// Host-side inputs for one tick.
#[derive(Clone, Copy, Debug)]
pub struct MomentumGlowConfig {
    /// The user's switch (default on) combined with the host's gates: serious mode, focus, live
    /// viewport.
    pub enabled: bool,
    /// `0..=1` — the aurora's folded intensity (the brightness dial × the
    /// motion amplitude × the load-shed envelope), as every body takes it.
    pub intensity: f32,
    /// The cool-down τ in seconds ([`MOMENTUM_GLOW_TAU_S`] unless configured).
    pub tau_s: f32,
    /// Halo reach at full momentum, in cell widths.
    pub radius_cells: f32,
    /// The resolved cursor colour `0x00RRGGBB` — what the tint is built from.
    pub base: u32,
    /// The page the light lands on is dark: additive halo. `false`: ink
    /// beneath the letters in [`Self::hue`].
    pub dark_theme: bool,
    /// The hue a LIGHT page's glow wears, `0x00RRGGBB`: the style's own
    /// caret/glow colour (its trail colour, or the signature shade its caret
    /// wears), so the glow reads as the style's, never as a fixed warm halo.
    /// The dark page's additive halo keeps its temperature ramp.
    pub hue: u32,
    /// The theme's text and paper, which a light ground's glow solves its
    /// ink against.
    pub theme_fg: u32,
    pub theme_bg: u32,
    /// Whether the cursor is a block (the body tint applies); bar/underline get the halo only.
    pub block: bool,
    /// The caret row's letters as columns `first..end` (a blank between two
    /// glyphs counts, as it reads as part of the line), or `None` for a
    /// blank row. A host that cannot read the row passes every column left
    /// of the caret. A LIGHT page's glow keeps only
    /// [`LIGHT_GLOW_TEXT_KEEP`]'s worth of ink over them; a dark page's halo
    /// spends the over-ink account instead.
    pub glyph_span: Option<(i32, i32)>,
}

/// One tick's outputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MomentumGlowFrame {
    /// The block body tint, `None` when cold, off, or not a block.
    pub fill: Option<u32>,
    /// Hot: the host pins a Blinking* style to its Steady* twin.
    pub hot: bool,
    /// Fingerprint of everything visible; 0 when nothing is.
    pub fp: u64,
}

/// Cached light-page glow ink and text-alpha ceiling for `(hue, theme_fg, theme_bg)`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LightInkMemo {
    key: (u32, u32, u32),
    ink: u32,
    text_alpha: f32,
}

/// The engine: one integrator plus the fingerprint law that tells frame
/// cadence from the cool-down's sparse steps.
#[derive(Clone, Copy, Debug, Default)]
pub struct MomentumGlow {
    rate: TypingRate,
    fp_last: u64,
    fp_prev: u64,
    /// The last real tick's post-intensity amplitude; 0 once inert.
    latched: f32,
    /// That tick's instant and τ, for the settle schedule.
    stamp: Option<Instant>,
    tau: f32,
    /// Cached `(ink, text_alpha)` for the light-page glow.
    light_ink_memo: Option<LightInkMemo>,
}

impl MomentumGlow {
    /// One printable key. The host calls this for glyph-producing keys only (never a bare
    /// modifier, never delete/kill/navigation).
    #[inline]
    pub fn on_key(&mut self, now: Instant, tau_s: f32) {
        self.rate.on_key(now, tau_s);
    }

    /// The decayed value at `now` — for diagnostics and the host's cadence.
    #[must_use]
    #[inline]
    pub fn value(&self, now: Instant, tau_s: f32) -> f32 {
        self.rate.value(now, tau_s)
    }

    /// Back to rest (layout/space resets only).
    #[inline]
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Frame cadence: the light is hot and its last two frames differed (the
    /// rainbow body's fingerprint law).
    #[must_use]
    #[inline]
    pub fn is_active(&self) -> bool {
        self.latched >= MOMENTUM_GLOW_HOT && self.fp_last != self.fp_prev
    }

    /// Hot, but its last two frames were identical: not settled. A cooling
    /// amplitude moves one `u8` step every few frames, then every few
    /// hundred milliseconds, so two identical frames come long before the
    /// glow is cold. The host keeps the lane and wakes it at
    /// [`Self::next_change_deadline`] instead of at frame cadence.
    ///
    /// Disjoint from [`Self::is_active`]; their OR is "hot".
    #[must_use]
    #[inline]
    pub fn settling(&self) -> bool {
        self.latched >= MOMENTUM_GLOW_HOT && self.fp_last == self.fp_prev
    }

    /// The analytic instant the latched amplitude decays below
    /// [`MOMENTUM_GLOW_HOT`] (`stamp + τ·ln(a/floor)`): the frame that clears
    /// the halo, the tint and the blink pin. `None` once cold.
    #[must_use]
    #[inline]
    pub fn cold_at(&self) -> Option<Instant> {
        self.decays_to(MOMENTUM_GLOW_HOT)
    }

    /// When the glass next changes. `Some(now + frame_interval)` while
    /// [`Self::is_active`]; `None` once cold; while [`Self::settling`], the
    /// analytic instant of the latched amplitude's next `u8` step down its
    /// exponential or its crossing below the hot floor, whichever comes first,
    /// never sooner than `now + frame_interval`. A tick at that instant
    /// re-latches, so a changed intensity is re-measured, not extrapolated.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant, frame_interval: Duration) -> Option<Instant> {
        let floor = now + frame_interval;
        if self.is_active() {
            return Some(floor);
        }
        if !self.settling() {
            return None;
        }
        let step = (self.latched * 255.0).floor();
        let next = [
            (step >= 1.0)
                .then(|| self.decays_to(step / 255.0))
                .flatten(),
            self.cold_at(),
        ]
        .into_iter()
        .flatten()
        .min();
        // A latched amplitude with no forecast fails onto the cadence.
        Some(next.map_or(floor, |at| (at + SETTLE_MARGIN).max(floor)))
    }

    /// The instant the latched amplitude decays to `level`, from the last real
    /// tick: `stamp + τ·ln(latched / level)`.
    #[inline]
    fn decays_to(&self, level: f32) -> Option<Instant> {
        if self.latched < MOMENTUM_GLOW_HOT || level <= 0.0 {
            return None;
        }
        let secs = self.tau * (self.latched / level).ln();
        let after = Duration::try_from_secs_f32(secs.max(0.0)).ok()?;
        self.stamp.map(|stamp| stamp + after)
    }

    /// One frame. Pushes the halo into `out` (the host's additive
    /// `cursor_glow_add` scratch) on a dark page, or into `under` (the
    /// under-ink stream, beneath the letters) on a light one, and returns the
    /// fill / blink verdict / fingerprint.
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        geom: Geom,
        cfg: &MomentumGlowConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> MomentumGlowFrame {
        let raw = self.rate.value(now, cfg.tau_s);
        let a = if cfg.intensity.is_finite() {
            (raw * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        // Inert — byte-identical to the plain cursor — when off, when the
        // geometry is degenerate, when there is no cursor, or when cold.
        let Some((cr, cc)) = cur else {
            return self.inert();
        };
        if !cfg.enabled
            || cfg.intensity <= 0.0
            || geom.cw == 0
            || geom.ch == 0
            || a < MOMENTUM_GLOW_HOT
            || (cr as usize) >= geom.rows
            || (cc as usize) >= geom.cols
        {
            return self.inert();
        }
        self.latched = a;
        self.stamp = Some(now);
        self.tau = cfg.tau_s;

        // Everything below is drawn from the `u8` step, so the fingerprint
        // (the step, the fill, the cell) changes exactly when the glass does
        // and the settle schedule can name the next change.
        let q = (a * 255.0) as u8;
        let a = f32::from(q) / 255.0;
        let s = smoothstep(a);
        let tint = ramp(a, cfg.dark_theme);
        // On a dark page the body warms toward the ramp; on a light one it
        // concentrates in its own hue instead, since the ramp's orange mixed
        // into a blue or pink caret reads as mud.
        let fill = cfg.block.then(|| {
            if cfg.dark_theme {
                mix_rgb(cfg.base, tint, 0.55 * s)
            } else {
                crate::color_math::light_charge_in_hue(cfg.base, s)
            }
        });
        if cfg.dark_theme {
            emit_halo((cr, cc), a, s, tint, geom, cfg, out);
        } else {
            let key = (
                cfg.hue & 0x00FF_FFFF,
                cfg.theme_fg & 0x00FF_FFFF,
                cfg.theme_bg & 0x00FF_FFFF,
            );
            let light_ink = match self.light_ink_memo {
                Some(memo) if memo.key == key => (memo.ink, memo.text_alpha),
                _ => {
                    let ink = light_glow_ink(cfg.hue, cfg.theme_fg, cfg.theme_bg);
                    let text_alpha = light_glow_text_alpha(ink, cfg.theme_fg, cfg.theme_bg);
                    self.light_ink_memo = Some(LightInkMemo {
                        key,
                        ink,
                        text_alpha,
                    });
                    (ink, text_alpha)
                }
            };
            emit_light_halo((cr, cc), a, s, light_ink, geom, cfg, under);
        }

        // The light glow's hue is drawn too, so a hue that moves at one
        // amplitude step (a phaser's rolling beam) still repaints.
        let fp = u64::from(q)
            .wrapping_mul(1_000_003)
            .wrapping_add(u64::from(fill.unwrap_or(0)) << 8)
            .wrapping_add((u64::from(cr) << 40) ^ (u64::from(cc) << 52))
            .wrapping_add(if cfg.dark_theme {
                0
            } else {
                // …and so are the letters it keeps its ink off.
                let span = cfg.glyph_span.map_or(0, |(first, end)| {
                    (u64::from(first as u32) << 20) ^ u64::from(end as u32)
                });
                (u64::from(cfg.hue & 0x00FF_FFFF) << 16 | 1 << 63) ^ span.rotate_left(37)
            })
            | 1;
        self.fp_prev = self.fp_last;
        self.fp_last = fp;
        MomentumGlowFrame {
            fill,
            hot: true,
            fp,
        }
    }

    #[inline]
    fn inert(&mut self) -> MomentumGlowFrame {
        self.latched = 0.0;
        self.stamp = None;
        self.fp_prev = self.fp_last;
        self.fp_last = 0;
        MomentumGlowFrame::default()
    }
}

/// The dark page's halo around the caret at amplitude `a` (its smoothstep
/// `s`, its ramp `tint`): one `(1 − ρ²)²` profile over the ellipse, laid row
/// by row as [`HALO_SEGMENTS`] linear ramps a side that meet but never
/// overlap, so each pixel carries one quad's light, at most the peak at the
/// caret's centre, which is at most [`HALO_PEAK_COV`].
fn emit_halo(
    (cr, cc): (u16, u16),
    a: f32,
    s: f32,
    tint: u32,
    geom: Geom,
    cfg: &MomentumGlowConfig,
    out: &mut Vec<GlowQuad>,
) {
    let (cw, ch) = (geom.cw as f32, geom.ch as f32);
    let (cx, cy) = geom.cell_center(cr, cc);
    let rx = cw * (0.55 + cfg.radius_cells * s);
    let ry = ch * (0.45 + 0.6 * cfg.radius_cells * s);
    // Eased out (`a·(2 − a)`): the budget is the ceiling, and an ordinary
    // pace (5 keys/s, `a` ≈ 0.6) spends 84 % of it, so the glow still reads
    // at a typing speed and not only at a sprint. Linear at the floor, so
    // the frame that goes cold steps by about two levels.
    let peak = HALO_PEAK_COV * a * (2.0 - a);
    if !(rx >= 1.0 && ry >= 1.0 && peak >= 0.5) {
        return;
    }
    let row_h = ((ch / HALO_ROW_DIV) as i32).max(2);
    let half_row_h = row_h as f32 * 0.5;
    // Rows on a grid anchored at the caret row's top edge, from the first
    // row reaching the ellipse's top.
    let cell_y0 = i32::from(geom.origin_y) + i32::from(cr) * geom.ch as i32;
    let top = (cy - ry).floor() as i32;
    let bot = (cy + ry).ceil() as i32;
    let mut y = cell_y0 - (cell_y0 - top + row_h - 1).div_euclid(row_h) * row_h;
    while y < bot {
        let v = (y as f32 + half_row_h - cy) / ry;
        let v2 = v * v;
        let w2 = 1.0 - v2;
        if w2 > 0.0 {
            let half = rx * w2.sqrt();
            // Knot `k` of the row: its rounded edges and its ramp end
            // (premultiplied light, opacity 0: additive).
            let knot = |k: usize| {
                let t = k as f32 / HALO_SEGMENTS as f32;
                let dx = half * t;
                let (rx_i, lx_i) = ((cx + dx).round() as i32, (cx - dx).round() as i32);
                let rho2 = v2 + w2 * t * t;
                let f = (1.0 - rho2).max(0.0);
                let cov = (peak * f * f).round().min(HALO_PEAK_COV) as u8;
                if cov == 0 {
                    return (rx_i, lx_i, 0, (0, 0));
                }
                let colour = halo_colour(tint, a, rho2.sqrt());
                (rx_i, lx_i, cov, (premul_rgb(colour, cov), 0u8))
            };
            let knots: [_; HALO_SEGMENTS + 1] = std::array::from_fn(knot);
            for pair in knots.windows(2) {
                let ((r0, l1, cov_in, inner), (r1, l0, cov_out, outer)) = (pair[0], pair[1]);
                if cov_in == 0 && cov_out == 0 {
                    continue;
                }
                push_fx_ramp(out, geom, r0, y, r1 - r0, row_h, inner, outer);
                push_fx_ramp(out, geom, l0, y, l1 - l0, row_h, outer, inner);
            }
        }
        y += row_h;
    }
}

/// The halo's colour at normalised radius `rho`: the tint mixed with the ramp
/// up to [`HALO_WARMTH`] hotter toward the core.
#[inline]
fn halo_colour(tint: u32, a: f32, rho: f32) -> u32 {
    let warm = (HALO_WARMTH * (1.0 - rho) / (1.0 - HALO_WARM_CORE)).clamp(0.0, HALO_WARMTH);
    mix_rgb(tint, ramp((a + warm).min(1.0), true), 0.5)
}

/// The light glow's ink for `hue`: its vivid ink walked toward white until,
/// laid opaque on the page, the theme's text keeps [`LIGHT_GLOW_INK_BAR`]
/// over it (`cursor_glow::light_ink_over_floor`).
///
/// Stricter than a body's own law (`cursor_glow::light_body_ink`, 5.25:1)
/// because the glow lies over the style's light body in the same stream, and
/// two inks solved to the same luminance composite DARKER than either where
/// their hues differ (sRGB mixing under a convex luminance): over an opaque
/// body of the opposite hue a glow solved at the body law leaves the text
/// about 4.45:1. Solved at this bar, any glow over any body keeps 5.25:1
/// (`the_light_glow_stacked_on_a_light_body_keeps_the_light_bar`).
#[inline]
fn light_glow_ink(hue: u32, theme_fg: u32, theme_bg: u32) -> u32 {
    crate::cursor_glow::light_ink_over_floor(
        hue & 0x00FF_FFFF,
        theme_bg,
        u8::MAX,
        crate::cursor_glow::light_floor_at(theme_fg, LIGHT_GLOW_INK_BAR),
    )
}

/// The share of the theme's text contrast on its paper a light glow keeps
/// where it lies under a letter of the caret's row
/// ([`MomentumGlowConfig::glyph_span`]): the glow stands well clear of the
/// letters' reading (the newest letter typed reads within about half a point
/// of the glow-off frame, on the paper and over a style's light body), and
/// lands whole over the blank cells around the caret, where it is seen.
pub const LIGHT_GLOW_TEXT_KEEP: f32 = 0.97;

/// The opacity `0..=255` at which the light glow's `ink` leaves the theme's
/// text [`LIGHT_GLOW_TEXT_KEEP`] of its contrast on its paper, composited as
/// the renderer composites a source-over quad.
#[inline]
fn light_glow_text_alpha(ink: u32, theme_fg: u32, theme_bg: u32) -> f32 {
    crate::cursor_glow::light_alpha_keeping(ink, theme_fg, theme_bg, LIGHT_GLOW_TEXT_KEEP)
}

/// The contrast the theme's text keeps over the light glow's ink laid opaque
/// ([`light_glow_ink`]): the least at which a glow at [`LIGHT_GLOW_PEAK`]
/// over a light body of any hue still leaves the 5.25:1 light bar.
const LIGHT_GLOW_INK_BAR: f32 = 7.5;

/// The light page's glow at amplitude `a` (its smoothstep `s`): one round
/// ink mark beneath the letters in [`MomentumGlowConfig::hue`], as wide as
/// the dark halo and held inside the caret's row
/// (`cursor_glow::push_ink_ellipse`), in [`light_glow_ink`], at most
/// [`LIGHT_GLOW_PEAK`].
fn emit_light_halo(
    (cr, cc): (u16, u16),
    a: f32,
    s: f32,
    (ink, text_alpha): (u32, f32),
    geom: Geom,
    cfg: &MomentumGlowConfig,
    under: &mut Vec<GlowQuad>,
) {
    use crate::cursor_glow::{LIGHT_MARK_QUADS, push_ink_ellipse_shaped};
    let (cw, ch) = (geom.cw as f32, geom.ch as f32);
    let (cx, cy) = geom.cell_center(cr, cc);
    let rx_full = cw * (0.55 + cfg.radius_cells * s);
    let ry_full = ch * (0.45 + 0.6 * cfg.radius_cells * s);
    let peak = LIGHT_GLOW_PEAK * a;
    let budget = under.len().saturating_add(LIGHT_MARK_QUADS);
    // Over the caret row's letters the glow keeps only the opacity that
    // leaves them their reading; a block caret's own cell is covered by the
    // block, so the glow lands whole under it.
    let on_letters = (text_alpha / peak.max(1.0)).min(1.0);
    let ox = f32::from(geom.origin_x);
    let caret = i32::from(cc);
    let letters = cfg.glyph_span.map(|(first, end)| {
        let x = |col: i32| ox + col as f32 * cw;
        let caret_free = cfg.block && (first..end).contains(&caret);
        (
            x(first),
            x(end),
            caret_free.then(|| (x(caret), x(caret + 1))),
        )
    });
    let share = |x0: f32, x1: f32| match letters {
        Some((l0, l1, caret_cell)) if x1 > l0 && x0 < l1 => {
            // A sample whose reach lies inside the covered caret cell alone
            // keeps the whole glow.
            if caret_cell.is_some_and(|(c0, c1)| x0 >= c0 && x1 <= c1) {
                1.0
            } else {
                on_letters
            }
        }
        _ => 1.0,
    };
    push_ink_ellipse_shaped(
        under,
        geom,
        (cx, cy),
        (rx_full, rx_full),
        ry_full,
        ink,
        peak,
        budget,
        share,
    );
}

#[inline]
fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Colour temperature with momentum: amber → gold → warm white → white-hot on a
/// dark theme; on a light theme the top is a deep orange.
#[inline]
#[must_use]
pub fn ramp(a: f32, dark_theme: bool) -> u32 {
    const AMBER: u32 = 0x00FF_9A3C;
    const GOLD: u32 = 0x00FF_C46B;
    const WARM_WHITE: u32 = 0x00FF_E8C8;
    const WHITE: u32 = 0x00FF_FFFF;
    const DEEP_ORANGE: u32 = 0x00E0_5A00;
    let a = a.clamp(0.0, 1.0);
    let top = if dark_theme { WHITE } else { DEEP_ORANGE };
    if a < 0.35 {
        mix_rgb(AMBER, GOLD, a / 0.35)
    } else if a < 0.75 {
        mix_rgb(GOLD, WARM_WHITE, (a - 0.35) / 0.40)
    } else {
        mix_rgb(WARM_WHITE, top, (a - 0.75) / 0.25)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color_math::contrast_ratio;

    const TAU: f32 = MOMENTUM_GLOW_TAU_S;

    fn geom() -> Geom {
        Geom {
            cw: 10,
            ch: 20,
            rows: 24,
            cols: 80,
            origin_x: 0,
            origin_y: 0,
            win_w: 800,
            win_h: 480,
            head: 0,
        }
    }

    fn on(base: u32, dark: bool) -> MomentumGlowConfig {
        MomentumGlowConfig {
            enabled: true,
            intensity: 1.0,
            tau_s: TAU,
            radius_cells: MOMENTUM_GLOW_RADIUS_CELLS,
            base,
            dark_theme: dark,
            hue: base,
            theme_fg: if dark { 0x00C8_C8D2 } else { 0x0024_292F },
            theme_bg: if dark { 0x0010_1016 } else { 0x00FF_FFFF },
            block: true,
            glyph_span: None,
        }
    }

    /// The halo a glow draws on the frame its `keys`-th key is struck (all at
    /// one instant, so the amplitude is `keys × 0.1`, clamped).
    fn charged(keys: u32, cell: (u16, u16), g0: Geom, cfg: &MomentumGlowConfig) -> Vec<GlowQuad> {
        let t0 = Instant::now();
        let mut g = MomentumGlow::default();
        for _ in 0..keys {
            g.on_key(t0, TAU);
        }
        let (mut out, mut under) = (Vec::new(), Vec::new());
        g.tick(Some(cell), t0, g0, cfg, &mut out, &mut under);
        out.extend(under);
        out
    }

    /// Every quad of `quads` composited over a plane of `ground` exactly as
    /// the CPU rasterizer does it (the gradient law per column, then
    /// `over_premul`, which is `add_sat` at opacity 0).
    fn composite(quads: &[GlowQuad], ground: u32, w: usize, h: usize) -> Vec<u32> {
        let mut px = vec![ground; w * h];
        for q in quads {
            let qw = u32::from(q.w);
            for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                for i in 0..qw {
                    let x = usize::from(q.x) + i as usize;
                    if x >= w {
                        break;
                    }
                    let c = aterm_render::glow_lerp_rgb(q.color, q.color2, i, qw);
                    let a = aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, qw)
                        as u8;
                    px[y * w + x] = aterm_render::over_premul(px[y * w + x], c, a) & 0x00FF_FFFF;
                }
            }
        }
        px
    }

    /// The light (additive) or veil opacity (source-over) every quad lays on
    /// each pixel, summed: the brightest channel of the light, or the opacity.
    fn spent_per_pixel(quads: &[GlowQuad], w: usize, h: usize) -> Vec<u32> {
        let mut px = vec![0u32; w * h];
        for q in quads {
            let qw = u32::from(q.w);
            for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                for i in 0..qw {
                    let x = usize::from(q.x) + i as usize;
                    if x >= w {
                        break;
                    }
                    px[y * w + x] += if q.alpha == 0 {
                        let c = aterm_render::glow_lerp_rgb(q.color, q.color2, i, qw);
                        ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff)
                    } else {
                        aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, qw)
                    };
                }
            }
        }
        px
    }

    /// The wake deadline agrees with the glass: at low intensity (reduced motion
    /// times the load-shed envelope) where `raw * intensity` is below the hot
    /// floor, `tick` draws nothing and no deadline is asked for.
    #[test]
    fn the_wake_deadline_never_outlives_what_the_glass_draws() {
        let t0 = Instant::now();
        let lane = Duration::from_millis(16);
        let mut glow = MomentumGlow::default();
        let mut cfg = on(0x0000_0000, true);
        let mut out: Vec<GlowQuad> = Vec::new();

        // A real run of typing, read at the last key: hot on the raw rate.
        let last = script(&mut glow.rate, t0, 12.0, 2.0);
        assert!(
            glow.rate.value(last, cfg.tau_s) >= MOMENTUM_GLOW_HOT,
            "fixture: the raw rate is hot"
        );

        // Positive control at full intensity: drawn, and a deadline asked for.
        out.clear();
        let full = glow.tick(Some((1, 1)), last, geom(), &cfg, &mut out, &mut Vec::new());
        assert!(full.hot, "fixture: at intensity 1 it is on glass");
        assert!(
            glow.next_change_deadline(last, lane).is_some(),
            "and the host is asked for the next frame"
        );

        // Now scale it below the floor — the state a low motion amplitude or a
        // load-shed envelope produces.
        cfg.intensity = MOMENTUM_GLOW_HOT / glow.rate.value(last, cfg.tau_s) * 0.5;
        out.clear();
        let dim = glow.tick(Some((1, 1)), last, geom(), &cfg, &mut out, &mut Vec::new());
        assert!(
            !dim.hot && out.is_empty(),
            "fixture: scaled below the floor the effect is inert"
        );
        assert!(
            glow.next_change_deadline(last, lane).is_none() && !glow.settling(),
            "an inert effect asks for no frame"
        );
    }

    /// Feed keys at `rate` keys/s from `t0` for `secs`; returns the instant of
    /// the last key (the value is read there, since silence is the cool-down
    /// the other test measures).
    fn script(r: &mut TypingRate, t0: Instant, rate: f32, secs: f32) -> Instant {
        let gap = Duration::from_secs_f32(1.0 / rate);
        let mut t = t0;
        let n = (rate * secs) as usize;
        for k in 0..n {
            if k > 0 {
                t += gap;
            }
            r.on_key(t, TAU);
        }
        t
    }

    /// The value is strictly increasing in the key rate (which the
    /// rate-normalised family metric deliberately is not).
    #[test]
    fn rises_with_key_rate() {
        let t0 = Instant::now();
        let mut vals = Vec::new();
        for rate in [1.0f32, 2.0, 4.0, 8.0] {
            let mut r = TypingRate::default();
            let end = script(&mut r, t0, rate, 3.0);
            vals.push(r.value(end, TAU));
        }
        for w in vals.windows(2) {
            assert!(w[1] > w[0] + 0.05, "faster typing must glow more: {vals:?}");
        }
        // 24 keys at 8/s climb the 1 − e^{−n/(r·τ)} arc to ≈ 0.93; the clamp is
        // reached asymptotically. White-hot on the ramp starts at 0.75.
        assert!(vals[3] >= 0.9, "8 keys/s is white-hot: {vals:?}");
        assert!(vals[0] <= 0.25, "1 key/s is a warm ember: {vals:?}");
    }

    /// Cooling: monotone non-increasing in silence, half-life at τ·ln 2, exactly
    /// zero at the end, and the engine's analytic cold crossing matches the
    /// tick that first draws nothing.
    #[test]
    fn cooldown_is_monotone_and_snaps_to_exact_zero() {
        let t0 = Instant::now();
        let mut g = MomentumGlow::default();
        for _ in 0..40 {
            g.on_key(t0, TAU); // clamps to 1.0
        }
        assert!((g.value(t0, TAU) - 1.0).abs() < 1e-6);
        let mut out = Vec::new();
        assert!(
            g.tick(
                Some((1, 1)),
                t0,
                geom(),
                &on(0, true),
                &mut out,
                &mut Vec::new()
            )
            .hot
        );
        let analytic = g.cold_at().expect("a hot glow names its cold instant");
        let mut last = 1.0f32;
        let mut sampled_cold: Option<Instant> = None;
        for k in 1..=500 {
            let t = t0 + Duration::from_millis(16 * k);
            let v = g.value(t, TAU);
            assert!(
                v <= last + 1e-6,
                "cooling must be monotone at {k}: {v} > {last}"
            );
            if sampled_cold.is_none() && v < MOMENTUM_GLOW_HOT {
                sampled_cold = Some(t);
            }
            last = v;
        }
        assert_eq!(last, 0.0, "fully cooled is EXACTLY zero, not a drift");
        let half = g.value(t0 + Duration::from_secs_f32(TAU * 2f32.ln()), TAU);
        assert!((half - 0.5).abs() < 0.01, "half-life at τ·ln2: {half}");
        let sampled = sampled_cold.expect("it cools");
        let gap = sampled.saturating_duration_since(analytic).as_secs_f32();
        assert!(
            sampled >= analytic && gap < 0.02,
            "analytic crossing agrees with the sampled one ({gap} s)"
        );
    }

    /// Hot after one key, and cold once the value drops below the floor: the host reads this to
    /// pin a Blinking* style to its Steady* twin.
    #[test]
    fn blink_is_suppressed_while_hot_and_returns_when_cold() {
        let t0 = Instant::now();
        let mut g = MomentumGlow::default();
        let mut out = Vec::new();
        assert!(
            !g.tick(
                Some((3, 3)),
                t0,
                geom(),
                &on(0x00A0_A0A0, true),
                &mut out,
                &mut Vec::new()
            )
            .hot
        );
        g.on_key(t0, TAU);
        let f = g.tick(
            Some((3, 3)),
            t0,
            geom(),
            &on(0x00A0_A0A0, true),
            &mut out,
            &mut Vec::new(),
        );
        assert!(f.hot, "one key makes the cursor hot");
        assert!(f.fill.is_some(), "a hot block carries the tint");
        assert!(!out.is_empty(), "and a halo is on glass");
        let later = t0 + Duration::from_secs(6);
        out.clear();
        let f = g.tick(
            Some((3, 3)),
            later,
            geom(),
            &on(0x00A0_A0A0, true),
            &mut out,
            &mut Vec::new(),
        );
        assert!(
            !f.hot && f.fill.is_none() && f.fp == 0,
            "cold is exactly the configured cursor"
        );
        assert!(out.is_empty(), "and nothing is drawn");
    }

    /// A key-repeat flood, a zero gap, a 10-year silence: finite, in [0, 1],
    /// never a panic. A NaN intensity is inert.
    #[test]
    fn absurd_rates_never_nan_or_exceed_one() {
        let t0 = Instant::now();
        let mut r = TypingRate::default();
        for _ in 0..100_000 {
            r.on_key(t0, TAU);
        }
        let v = r.value(t0, TAU);
        assert!(v.is_finite() && (0.0..=1.0).contains(&v));
        let mut t = t0;
        for _ in 0..10_000 {
            t += Duration::from_nanos(1);
            r.on_key(t, TAU);
        }
        assert!((r.value(t, TAU) - 1.0).abs() < 1e-6);
        let decade = t + Duration::from_secs(10 * 365 * 24 * 3600);
        assert_eq!(r.value(decade, TAU), 0.0);
        assert_eq!(r.value(t, f32::NAN), 0.0, "garbage τ is inert");
        assert_eq!(r.value(t, 0.0), 0.0);
        let mut g = MomentumGlow::default();
        g.on_key(t, TAU);
        let mut cfg = on(0x00FF_FFFF, true);
        cfg.intensity = f32::NAN;
        let mut out = Vec::new();
        let f = g.tick(Some((1, 1)), t, geom(), &cfg, &mut out, &mut Vec::new());
        assert_eq!(f, MomentumGlowFrame::default());
        assert!(out.is_empty());
    }

    /// Pure in the injected `now`: two engines fed one script agree bit for bit,
    /// and 60 Hz and 7 Hz sampling agree at shared instants (lazy decay trusts
    /// no dt).
    #[test]
    fn deterministic_given_timestamps() {
        let t0 = Instant::now();
        let (mut a, mut b) = (MomentumGlow::default(), MomentumGlow::default());
        let keys: Vec<Instant> = (0..30)
            .map(|k| t0 + Duration::from_millis(90 * k))
            .collect();
        for &k in &keys {
            a.on_key(k, TAU);
            b.on_key(k, TAU);
        }
        let mut out_a = Vec::new();
        let mut out_b = Vec::new();
        let end = keys[keys.len() - 1];
        // 60 Hz samples for a; only every 8th (≈ 7 Hz) for b.
        let mut fa = MomentumGlowFrame::default();
        let mut fb = MomentumGlowFrame::default();
        for k in 0..240u64 {
            let t = end + Duration::from_millis(16 * k);
            out_a.clear();
            fa = a.tick(
                Some((2, 2)),
                t,
                geom(),
                &on(0x0080_80FF, true),
                &mut out_a,
                &mut Vec::new(),
            );
            if k % 8 == 0 {
                out_b.clear();
                fb = b.tick(
                    Some((2, 2)),
                    t,
                    geom(),
                    &on(0x0080_80FF, true),
                    &mut out_b,
                    &mut Vec::new(),
                );
                assert_eq!(
                    fa, fb,
                    "sampling cadence must not change the value at a shared instant"
                );
                assert_eq!(out_a, out_b);
            }
        }
        // Both engines were live for the whole script.
        assert!(fa.fp != 0 || fb.fp == 0);
    }

    /// Off is byte-identical to no effect: disabled, intensity 0, no cursor,
    /// or a cursor outside the grid — fill None, not hot, fp 0, scratch untouched.
    #[test]
    fn off_is_byte_identical() {
        let t0 = Instant::now();
        let mut g = MomentumGlow::default();
        g.on_key(t0, TAU);
        let mut out = vec![GlowQuad {
            row: 0,
            x: 1,
            y: 1,
            w: 1,
            h: 1,
            color: 7,
            alpha: 0,
            color2: 7,
            alpha2: 0,
        }];
        let sentinel = out.clone();
        let mut off = on(0x00FF_FFFF, true);
        off.enabled = false;
        assert_eq!(
            g.tick(Some((1, 1)), t0, geom(), &off, &mut out, &mut Vec::new()),
            MomentumGlowFrame::default()
        );
        let mut zero = on(0x00FF_FFFF, true);
        zero.intensity = 0.0;
        assert_eq!(
            g.tick(Some((1, 1)), t0, geom(), &zero, &mut out, &mut Vec::new()),
            MomentumGlowFrame::default()
        );
        assert_eq!(
            g.tick(
                None,
                t0,
                geom(),
                &on(0x00FF_FFFF, true),
                &mut out,
                &mut Vec::new()
            ),
            MomentumGlowFrame::default()
        );
        assert_eq!(
            g.tick(
                Some((99, 1)),
                t0,
                geom(),
                &on(0x00FF_FFFF, true),
                &mut out,
                &mut Vec::new()
            ),
            MomentumGlowFrame::default()
        );
        assert_eq!(out, sentinel, "an inert tick touches nothing");
        assert!(!g.is_active() && !g.settling());
    }

    /// The halo hugs the cell — every quad inside the frame, its reach bounded
    /// by the radius law — in a bounded number of quads, and the ramp tops out
    /// white-hot on a dark page and orange on a light one.
    #[test]
    fn halo_hugs_the_cell_in_bounded_quads() {
        let g0 = geom();
        let out = charged(40, (10, 40), g0, &on(0x00FF_FFFF, true));
        assert!(!out.is_empty());
        let cx = (40.0 + 0.5) * g0.cw as f32;
        let reach = g0.cw as f32 * (0.55 + MOMENTUM_GLOW_RADIUS_CELLS) + 1.0;
        for q in &out {
            assert!(u32::from(q.x) + u32::from(q.w) <= u32::from(g0.win_w));
            assert!(u32::from(q.y) + u32::from(q.h) <= u32::from(g0.win_h));
            assert!(
                (f32::from(q.x) - cx).abs() <= reach + 1.0,
                "quad at {} strays past {reach}",
                q.x
            );
            assert!((f32::from(q.x + q.w) - cx).abs() <= reach + 1.0);
            assert_eq!((q.alpha, q.alpha2), (0, 0), "dark: additive light");
        }
        // A tall cell lays more rows; still a bounded handful per row.
        let big = Geom {
            cw: 20,
            ch: 40,
            rows: 50,
            cols: 200,
            win_w: 4000,
            win_h: 2000,
            ..g0
        };
        let n = charged(40, (25, 100), big, &on(0x00FF_FFFF, true)).len();
        assert!(n < 400, "bounded halo geometry, got {n} quads");
        assert_ne!(ramp(1.0, false), 0x00FF_FFFF);
        assert_eq!(ramp(1.0, true), 0x00FF_FFFF);
    }

    /// The over-ink budget, solved per pixel: at every momentum, on every
    /// geometry and caret shape, the light the halo lays on any one pixel
    /// (summed over every quad, by the renderer's gradient law) stays within
    /// [`OVER_INK_COV_CAP`], so the letter just typed beside the caret, the
    /// rows above and below, and the glyph in a bar or underline caret's own
    /// cell keep the over-ink bar. At full momentum it is spent (the halo is
    /// dimmer, not starved) and it still reaches more than a cell and a half
    /// past the caret (dimmer, not smaller). Negative control: two halos
    /// stacked on one caret overrun it, so the raster sees overlap.
    #[test]
    fn no_pixel_carries_more_than_the_over_ink_budget() {
        let padded = Geom {
            cw: 15,
            ch: 28,
            rows: 24,
            cols: 60,
            origin_x: 24,
            origin_y: 84,
            win_w: 948,
            win_h: 780,
            head: 80,
        };
        let odd = Geom {
            cw: 9,
            ch: 19,
            rows: 30,
            cols: 90,
            win_w: 810,
            win_h: 570,
            ..geom()
        };
        let cap = OVER_INK_COV_CAP as u32;
        for g0 in [geom(), padded, odd] {
            let (w, h) = (usize::from(g0.win_w), usize::from(g0.win_h));
            let cell = (6u16, 20u16);
            for keys in [1u32, 3, 5, 8, 40] {
                for block in [true, false] {
                    let cfg = MomentumGlowConfig {
                        block,
                        ..on(0x0050_FA7B, true)
                    };
                    let out = charged(keys, cell, g0, &cfg);
                    let light = spent_per_pixel(&out, w, h);
                    let max = light.iter().copied().max().unwrap_or(0);
                    assert!(
                        max <= cap,
                        "{keys} keys, block {block}, cell {}x{}: {max} levels on one pixel",
                        g0.cw,
                        g0.ch
                    );
                }
            }
            // Full momentum: the budget is spent beside the caret, and the
            // light still reaches past a cell and a half on each side.
            let out = charged(40, cell, g0, &on(0x0050_FA7B, true));
            let light = spent_per_pixel(&out, w, h);
            let row = (i32::from(g0.origin_y) + i32::from(cell.0) * g0.ch as i32 + g0.ch as i32 / 2)
                as usize;
            let cell_x0 = i32::from(g0.origin_x) + i32::from(cell.1) * g0.cw as i32;
            let beside = light[row * w + (cell_x0 - 1) as usize];
            assert!(
                beside * 10 >= cap * 7,
                "the budget is spent beside the caret: {beside} of {cap}"
            );
            let far = cell_x0 - (g0.cw as i32 * 3) / 2;
            assert!(
                light[row * w + far as usize] > 0,
                "the halo reaches a cell and a half past the caret"
            );
            let mut twice = out.clone();
            twice.extend_from_slice(&out);
            let max = spent_per_pixel(&twice, w, h).into_iter().max().unwrap_or(0);
            assert!(max > cap, "control: two halos stacked overrun it ({max})");
        }
    }

    /// The letter left of the caret keeps the over-ink bar (5.25:1) under a
    /// full-momentum halo on the harness theme (`#C8D3F5` on `#16171D`): every
    /// pixel of the column beside the caret, composited as the renderer does
    /// it, ink against ground. Negative control: the light the pre-budget
    /// halo stacked there (about four budgets' worth) drops it under the bar.
    #[test]
    fn the_letter_beside_the_caret_keeps_the_over_ink_bar() {
        const INK: u32 = 0x00C8_D3F5;
        const GROUND: u32 = 0x0016_171D;
        let g0 = geom();
        let (w, h) = (usize::from(g0.win_w), usize::from(g0.win_h));
        let cell = (6u16, 20u16);
        let out = charged(40, cell, g0, &on(0x0050_FA7B, true));
        let x = usize::from(cell.1) * g0.cw - 2;
        let rows = usize::from(cell.0) * g0.ch..(usize::from(cell.0) + 1) * g0.ch;
        let worst = |quads: &[GlowQuad]| {
            let (ink, ground) = (composite(quads, INK, w, h), composite(quads, GROUND, w, h));
            rows.clone()
                .map(|y| contrast_ratio(ink[y * w + x], ground[y * w + x]))
                .fold(f32::MAX, f32::min)
        };
        let lit = worst(&out);
        assert!(lit >= 5.25, "the letter beside the caret reads {lit:.2}:1");
        let stacked: Vec<GlowQuad> = (0..4).flat_map(|_| out.iter().copied()).collect();
        let old = worst(&stacked);
        assert!(old < 5.25, "control: four budgets of light read {old:.2}:1");
    }

    /// A light page gets ink beneath the letters: every quad is source-over
    /// (no additive light, which white would swallow), its opacity summed per
    /// pixel within [`LIGHT_GLOW_PEAK`], deepening the ground around the
    /// caret; GitHub Light's ink drawn over it keeps the 5.25:1 light bar.
    /// Negative control: the dark arm on the same caret lays additive light.
    #[test]
    fn the_light_page_halo_is_a_source_over_veil_under_the_budget() {
        const INK: u32 = 0x0024_292F;
        const PAPER: u32 = 0x00FF_FFFF;
        let g0 = geom();
        let (w, h) = (usize::from(g0.win_w), usize::from(g0.win_h));
        let cell = (6u16, 20u16);
        let veil = charged(40, cell, g0, &on(0x0009_69DA, false));
        assert!(!veil.is_empty(), "a hot light-page caret still glows");
        assert!(
            veil.iter().all(|q| q.alpha > 0 && q.alpha2 > 0),
            "every light-page quad is source-over"
        );
        let spent = spent_per_pixel(&veil, w, h);
        assert!(spent.iter().all(|&a| a <= LIGHT_GLOW_PEAK.ceil() as u32));
        let ground = composite(&veil, PAPER, w, h);
        let deepened = ground.iter().filter(|&&px| px != PAPER).count();
        assert!(
            deepened > g0.cw * g0.ch,
            "the veil deepens the page around the caret"
        );
        let worst = ground
            .iter()
            .map(|&g| contrast_ratio(INK, g))
            .fold(f32::MAX, f32::min);
        assert!(worst >= 5.25, "ink over the glow reads {worst:.2}:1");
        let dark_arm = charged(40, cell, g0, &on(0x0009_69DA, true));
        assert!(
            dark_arm.iter().all(|q| q.alpha == 0),
            "control: the dark arm is additive light"
        );
    }

    /// On a light page the glow is one round glow inside the caret's row, in
    /// the style's hue. Composited over GitHub Light's paper from three keys
    /// of momentum on (a single key's glow is still inside the block caret),
    /// no pixel of the rows above or below changes (a selection row under the
    /// prompt keeps its letters), the glow deepens the row on both sides of
    /// the caret, and its opacity melts to nothing at the
    /// row's top and bottom edges as it does sideways, with no straight cut.
    /// Its deepest pixel wears the configured hue. Negative controls: the
    /// dark arm's taller profile cut at the row's edges (the halves the
    /// previous light veil drew) leaves a hard edge there, and a red style's
    /// glow is not a blue one's.
    #[test]
    fn the_light_page_glow_is_one_round_glow_inside_the_caret_row() {
        use crate::color_math::rgb2hsv;
        const PAPER: u32 = 0x00FF_FFFF;
        let g0 = geom();
        let (w, h) = (usize::from(g0.win_w), usize::from(g0.win_h));
        let cell = (6u16, 20u16);
        let band = usize::from(cell.0) * g0.ch..(usize::from(cell.0) + 1) * g0.ch;
        let cell_x = usize::from(cell.1) * g0.cw..(usize::from(cell.1) + 1) * g0.cw;
        // The deepest opacity in the band's top and bottom pixel rows,
        // against the deepest anywhere.
        let edge_and_peak = |spent: &[u32]| {
            let row_max = |y: usize| (0..w).map(|x| spent[y * w + x]).max().unwrap_or(0);
            let edge = row_max(band.start).max(row_max(band.end - 1));
            (edge, spent.iter().copied().max().unwrap_or(0))
        };
        // From three keys on: a single key's glow is still inside the block
        // caret.
        for keys in [3u32, 6, 40] {
            let glow = charged(keys, cell, g0, &on(0x0009_69DA, false));
            assert!(!glow.is_empty(), "{keys} keys: a hot caret glows");
            let ground = composite(&glow, PAPER, w, h);
            for y in (0..h).filter(|y| !band.contains(y)) {
                for x in 0..w {
                    assert_eq!(
                        ground[y * w + x],
                        PAPER,
                        "{keys} keys: the glow left the caret row at ({x}, {y})"
                    );
                }
            }
            let mid = band.start + g0.ch / 2;
            let deepened =
                |xs: std::ops::Range<usize>| xs.into_iter().any(|x| ground[mid * w + x] != PAPER);
            assert!(
                deepened(0..cell_x.start) && deepened(cell_x.end..w),
                "{keys} keys: the glow shows on both sides of the caret"
            );
            let (edge, peak) = edge_and_peak(&spent_per_pixel(&glow, w, h));
            assert!(
                edge * 6 <= peak,
                "{keys} keys: a cut edge at the row's rim ({edge} of {peak})"
            );
        }
        let dark_arm = charged(40, cell, g0, &on(0x0009_69DA, true));
        let cut: Vec<GlowQuad> = dark_arm
            .iter()
            .filter(|q| band.contains(&usize::from(q.y)))
            .copied()
            .collect();
        let (edge, peak) = edge_and_peak(&spent_per_pixel(&cut, w, h));
        assert!(
            edge * 6 > peak,
            "control: a taller profile cut at the row leaves an edge ({edge} of {peak})"
        );
        let deepest_hue = |hue: u32| {
            let ground = composite(&charged(40, cell, g0, &on(hue, false)), PAPER, w, h);
            let deepest = ground
                .iter()
                .copied()
                .min_by_key(|&px| (crate::color_math::relative_luminance(px) * 1e6) as u32)
                .unwrap_or(PAPER);
            rgb2hsv(deepest).0
        };
        let blue = deepest_hue(0x0009_69DA);
        let (want, _, _) = rgb2hsv(0x0009_69DA);
        assert!(
            (blue - want).abs() < 12.0,
            "the glow wears the style's hue: {blue:.0} vs {want:.0}"
        );
        let red = deepest_hue(0x00CF_222E);
        assert!(
            (red - blue).abs() > 90.0,
            "control: a red style's glow ({red:.0}) is not a blue one's ({blue:.0})"
        );
    }

    /// ON A LIGHT PAGE A HOT CARET STAYS IN ITS OWN HUE: for a blue, a pink
    /// and a green caret, at every momentum the block's fill keeps the
    /// caret's hue within a few degrees, is no greyer than it, and is the
    /// caret's colour exactly at the cold edge. Negative control: the dark
    /// page's warm ramp mixed into the same carets swings their hue far off
    /// (blue toward grey-mauve, pink toward brown): the mud the judges saw.
    #[test]
    fn a_hot_light_caret_stays_in_its_own_hue() {
        use crate::color_math::rgb2hsv;
        let gap = |a: f32, b: f32| {
            let d = (a - b).abs().rem_euclid(360.0);
            d.min(360.0 - d)
        };
        for base in [0x0009_69DA, 0x00D0_3A8C, 0x001A_7F37] {
            let (h0, s0, _) = rgb2hsv(base);
            let mut worst_mix = 0.0f32;
            for keys in 1..=12u32 {
                let cfg = on(base, false);
                let t0 = Instant::now();
                let mut g = MomentumGlow::default();
                for _ in 0..keys {
                    g.on_key(t0, TAU);
                }
                let frame = g.tick(
                    Some((6, 20)),
                    t0,
                    geom(),
                    &cfg,
                    &mut Vec::new(),
                    &mut Vec::new(),
                );
                let fill = frame.fill.expect("a hot block wears a fill");
                let (h, sat, _) = rgb2hsv(fill);
                assert!(
                    gap(h, h0) <= 4.0 && sat >= s0 - 0.02,
                    "{base:06x} at {keys} keys: {fill:06x} left its hue"
                );
                let a = (keys as f32 * MOMENTUM_GLOW_KEY_CREDIT).min(1.0);
                let s = smoothstep(a);
                let mixed = mix_rgb(base, ramp(a, false), 0.55 * s);
                worst_mix = worst_mix.max(gap(rgb2hsv(mixed).0, h0));
            }
            assert_eq!(
                crate::color_math::light_charge_in_hue(base, 0.0),
                base,
                "cold, the caret is its own colour"
            );
            assert!(
                worst_mix > 20.0,
                "control: the warm ramp moves {base:06x} only {worst_mix:.0} degrees"
            );
        }
    }

    /// THE LIGHT GLOW KEEPS THE NEWEST LETTER WITHIN HALF A POINT: at full
    /// momentum behind a typed run on GitHub Light and Default Light, over
    /// the bare paper and over a style's warm body under the letters, the
    /// letter just typed (one cell, or the two of a wide glyph) reads within
    /// half a point of the same frame with the glow off, while the glow still
    /// deepens the blank cells ahead of the caret to most of its peak and
    /// lands whole under the block. Negative control: the same glow with no
    /// letters named lies its whole ink on them and costs the newest letter
    /// well over half a point.
    #[test]
    fn the_light_glow_keeps_the_newest_letter_within_half_a_point() {
        let g0 = geom();
        let (w, h) = (usize::from(g0.win_w), usize::from(g0.win_h));
        let cell = (6u16, 20u16);
        let px = |row: u16, col: u16| {
            let (x0, y0) = (usize::from(col) * g0.cw, usize::from(row) * g0.ch);
            (y0..y0 + g0.ch).flat_map(move |y| (x0..x0 + g0.cw).map(move |x| y * w + x))
        };
        // The worst letter reading in `cols` of the caret's row: the theme's
        // ink against the deepest ground pixel under it.
        let reads = |ground: &[u32], ink: u32, cols: std::ops::Range<u16>| {
            cols.flat_map(|c| px(cell.0, c))
                .map(|i| contrast_ratio(ink, ground[i]))
                .fold(f32::MAX, f32::min)
        };
        for (paper, ink) in [(0x00FF_FFFF, 0x001F_2328), (0x00F6_F7F9, 0x002B_2D33)] {
            for bed in [paper, 0x00F3_E2C2] {
                for hue in [0x00FF_D9A0, 0x00E0_6CE8, 0x0009_69DA] {
                    let lit = |span: Option<(i32, i32)>| {
                        let cfg = MomentumGlowConfig {
                            hue,
                            theme_fg: ink,
                            theme_bg: paper,
                            glyph_span: span,
                            ..on(hue, false)
                        };
                        composite(&charged(40, cell, g0, &cfg), bed, w, h)
                    };
                    let off = vec![bed; w * h];
                    let bound = lit(Some((2, i32::from(cell.1))));
                    for newest in [19..20u16, 18..20] {
                        let (was, now) = (
                            reads(&off, ink, newest.clone()),
                            reads(&bound, ink, newest.clone()),
                        );
                        assert!(
                            was - now <= 0.5,
                            "{hue:06x} over {bed:06x}: the newest letter {newest:?} reads {now:.2}, \
                             {was:.2} with the glow off"
                        );
                    }
                    let spent = spent_per_pixel(
                        &charged(
                            40,
                            cell,
                            g0,
                            &MomentumGlowConfig {
                                hue,
                                theme_fg: ink,
                                theme_bg: paper,
                                glyph_span: Some((2, i32::from(cell.1))),
                                ..on(hue, false)
                            },
                        ),
                        w,
                        h,
                    );
                    let ahead = px(cell.0, cell.1 + 1).map(|i| spent[i]).max().unwrap_or(0);
                    let under = px(cell.0, cell.1).map(|i| spent[i]).max().unwrap_or(0);
                    assert!(
                        ahead as f32 >= LIGHT_GLOW_PEAK * 0.6
                            && under as f32 >= LIGHT_GLOW_PEAK * 0.9,
                        "{hue:06x}: the glow must still show ahead of the caret ({ahead}) and \
                         under it ({under})"
                    );
                    let whole = lit(None);
                    let cost = reads(&off, ink, 19..20) - reads(&whole, ink, 19..20);
                    assert!(
                        cost > 0.5,
                        "control: {hue:06x} laid whole on the letters costs only {cost:.2}"
                    );
                }
            }
        }
    }

    /// The light glow, stacked over a style's light body at its cap (both are
    /// ink beneath the letters of the caret's row, the body in its own ink
    /// law, `light_body_ink`), keeps Default Light's and GitHub Light's ink at
    /// the 5.25:1 light bar for every pair of hues and every glow opacity up
    /// to its peak, composited as the renderer composites source-over quads.
    /// Negative control: the glow in the body's own ink law sinks under the
    /// bar where the two hues differ, which is why the glow's ink is solved
    /// stricter.
    #[test]
    fn the_light_glow_stacked_on_a_light_body_keeps_the_light_bar() {
        use crate::cursor_glow::{LIGHT_BODY_ALPHA_CAP, light_body_ink};
        let hues: Vec<u32> = (0..36)
            .map(|k| crate::color_math::hsv2rgb(k as f32 * 10.0, 1.0, 1.0))
            .collect();
        let worst = |glow_ink: &dyn Fn(u32) -> u32, ink: u32, page: u32| {
            let lay = |ground: u32, rgb: u32, alpha: f32| {
                let alpha = alpha as u8;
                aterm_render::over_premul(ground, premul_rgb(rgb, alpha), alpha) & 0x00FF_FFFF
            };
            let mut worst = f32::MAX;
            for &body in &hues {
                let body_ink = light_body_ink(body, ink, page, LIGHT_BODY_ALPHA_CAP as u8);
                let under = lay(page, body_ink, LIGHT_BODY_ALPHA_CAP);
                for &glow in &hues {
                    let glow_ink = glow_ink(glow);
                    for k in 1..=17 {
                        let alpha = (LIGHT_GLOW_PEAK * k as f32 / 17.0).min(LIGHT_GLOW_PEAK);
                        worst = worst.min(contrast_ratio(ink, lay(under, glow_ink, alpha)));
                    }
                }
            }
            worst
        };
        for (ink, page) in [(0x0024_292F, 0x00FF_FFFF), (0x002B_2D33, 0x00F6_F7F9)] {
            let ratio = worst(&|hue| light_glow_ink(hue, ink, page), ink, page);
            assert!(
                ratio >= 5.25,
                "{ink:06x} on {page:06x} under a body and the glow reads {ratio:.2}:1"
            );
        }
        let (ink, page) = (0x0024_292F, 0x00FF_FFFF);
        let body_law = worst(
            &|hue| light_body_ink(hue, ink, page, LIGHT_BODY_ALPHA_CAP as u8),
            ink,
            page,
        );
        assert!(
            body_law < 5.25,
            "control: a glow in the body's own ink law stacks to {body_law:.2}:1"
        );
    }

    /// Siting: the halo joins the window-absolute `cursor_glow_add` stream, so in
    /// a real window (cell 15×28, pad 24, head 80, origin (24, 84)) its bounding
    /// box surrounds the caret cell on every side with its centre of light at
    /// the cell's centre.
    #[test]
    fn halo_surrounds_the_caret_in_a_padded_headed_window() {
        let t0 = Instant::now();
        let real = Geom {
            cw: 15,
            ch: 28,
            rows: 24,
            cols: 100,
            origin_x: 24,
            origin_y: 84,
            win_w: 1548,
            win_h: 780,
            head: 80,
        };
        let (cr, cc) = (4u16, 10u16);
        let mut g = MomentumGlow::default();
        for _ in 0..40 {
            g.on_key(t0, TAU);
        }
        let mut out = Vec::new();
        let f = g.tick(
            Some((cr, cc)),
            t0,
            real,
            &on(0x00FF_FFFF, true),
            &mut out,
            &mut Vec::new(),
        );
        assert!(f.hot && !out.is_empty());
        // The caret cell in window px, from the one anchor every emitter shares.
        let (cell_x0, cell_y0) = (
            i32::from(real.origin_x) + i32::from(cc) * real.cw as i32,
            i32::from(real.origin_y) + i32::from(cr) * real.ch as i32,
        );
        let (cell_x1, cell_y1) = (cell_x0 + real.cw as i32, cell_y0 + real.ch as i32);
        let x0 = out.iter().map(|q| i32::from(q.x)).min().unwrap();
        let x1 = out.iter().map(|q| i32::from(q.x + q.w)).max().unwrap();
        let y0 = out.iter().map(|q| i32::from(q.y)).min().unwrap();
        let y1 = out.iter().map(|q| i32::from(q.y + q.h)).max().unwrap();
        assert!(
            x0 < cell_x0 && x1 > cell_x1 && y0 < cell_y0 && y1 > cell_y1,
            "halo bbox x {x0}..{x1} y {y0}..{y1} does not surround the caret cell \
             x {cell_x0}..{cell_x1} y {cell_y0}..{cell_y1}"
        );
        // Centre of light = the cell centre (the profile is symmetric about
        // it, so the bbox midpoint is within a row of it on each axis).
        let (ecx, ecy) = real.cell_center(cr, cc);
        assert!(
            ((x0 + x1) as f32 * 0.5 - ecx).abs() <= 2.0,
            "x centre {} vs {ecx}",
            (x0 + x1) as f32 * 0.5
        );
        assert!(
            ((y0 + y1) as f32 * 0.5 - ecy).abs() <= real.ch as f32 * 0.25,
            "y centre {} vs {ecy}",
            (y0 + y1) as f32 * 0.5
        );
        // Every quad inside the effects box (the grid plus the head band).
        for q in &out {
            assert!(i32::from(q.x) >= real.fx_left() && i32::from(q.x + q.w) <= real.fx_right());
            assert!(i32::from(q.y) >= real.fx_top() && i32::from(q.y + q.h) <= real.fx_bot());
        }
    }

    /// The frame cadence releases when the light stops changing, not when the
    /// value reaches zero: once the quantised output settles, `is_active` is
    /// false while `value` is still > 0, and the glow is settling instead.
    #[test]
    fn fingerprint_settles_so_the_lane_releases() {
        let t0 = Instant::now();
        let mut g = MomentumGlow::default();
        g.on_key(t0, TAU);
        let mut out = Vec::new();
        let cfg = on(0x0010_2030, true);
        let f1 = g.tick(Some((1, 1)), t0, geom(), &cfg, &mut out, &mut Vec::new());
        assert!(f1.hot);
        // Same instant twice: the fingerprint repeats, so the lane releases…
        let f2 = g.tick(Some((1, 1)), t0, geom(), &cfg, &mut out, &mut Vec::new());
        assert_eq!(f1.fp, f2.fp);
        assert!(!g.is_active(), "an unchanged frame must not hold the lane");
        assert!(
            g.value(t0, TAU) > 0.0,
            "…even though the value is still warm"
        );
        assert!(g.settling(), "…so it owes its next step instead");
        // …and a later, dimmer frame re-arms it.
        let f3 = g.tick(
            Some((1, 1)),
            t0 + Duration::from_millis(400),
            geom(),
            &cfg,
            &mut out,
            &mut Vec::new(),
        );
        assert_ne!(f3.fp, f2.fp);
        assert!(g.is_active());
    }

    /// A burst, then silence, through the host's scheduler predicate: armed
    /// while the glow is active (next frame at the lane interval) or
    /// settling (at its named step), parked otherwise. At a 120 Hz (16.7 ms)
    /// and an Apple-silicon 60 Hz (33.3 ms) lane, with a steady cursor (no
    /// blink clock to repaint it), every armed deadline is in the future and
    /// the lane parks only on a frame that drew nothing: no halo, no tint, not
    /// hot. Negative control: the frame-cadence predicate alone parks with
    /// the halo still on glass, the freeze this arm closes.
    ///
    /// Prints the frames rendered and presented per burst (the cost of the
    /// honest release); nothing pins those counts.
    #[test]
    fn a_cooling_glow_keeps_the_lane_until_its_last_frame_clears_the_halo() {
        for lane_ms in [16.7f32, 33.3] {
            let lane = Duration::from_secs_f32(lane_ms / 1000.0);
            for honest in [true, false] {
                let t0 = Instant::now();
                let mut g = MomentumGlow::default();
                let cfg = on(0x0050_FA7B, true);
                let mut out = Vec::new();
                let (mut frames, mut presents, mut last_fp) = (0u32, 0u32, 0u64);
                let mut drawn = (0usize, MomentumGlowFrame::default());
                let mut frame = |g: &mut MomentumGlow, now: Instant, col: u16| {
                    out.clear();
                    let f = g.tick(Some((4, col)), now, geom(), &cfg, &mut out, &mut Vec::new());
                    frames += 1;
                    if f.fp != last_fp {
                        presents += 1;
                        last_fp = f.fp;
                    }
                    drawn = (out.len(), f);
                };
                // 24 keys at 60 ms, each moving the caret, a frame after each.
                let (mut now, mut col) = (t0, 10u16);
                for k in 0..24u16 {
                    now = t0 + Duration::from_millis(60 * u64::from(k));
                    col = 10 + k;
                    g.on_key(now, TAU);
                    frame(&mut g, now, col);
                }
                let burst_end = now;
                let parked_at = loop {
                    let next = if g.is_active() {
                        Some(now + lane)
                    } else if honest && g.settling() {
                        g.next_change_deadline(now, lane)
                    } else {
                        None
                    };
                    let Some(next) = next else { break now };
                    assert!(next > now, "a deadline at or before now is a busy re-arm");
                    assert!(
                        next.saturating_duration_since(burst_end) < Duration::from_secs(60),
                        "the lane never parked"
                    );
                    now = next;
                    frame(&mut g, now, col);
                };
                let tail = parked_at.saturating_duration_since(burst_end);
                println!(
                    "momentum lane {lane_ms} ms, {}: {frames} frames, {presents} presents, \
                     parked {tail:?} after the last key with {} halo quads",
                    if honest {
                        "settle arm"
                    } else {
                        "frame cadence only"
                    },
                    drawn.0
                );
                if honest {
                    assert!(
                        drawn.0 == 0 && drawn.1.fill.is_none() && !drawn.1.hot,
                        "{lane_ms} ms: parked on a frame still drawing {} quads",
                        drawn.0
                    );
                    assert!(
                        g.value(parked_at, TAU) < MOMENTUM_GLOW_HOT,
                        "{lane_ms} ms: parked while still hot"
                    );
                } else {
                    assert!(
                        drawn.0 > 0 && drawn.1.hot,
                        "control at {lane_ms} ms: the cadence alone parks with light on glass"
                    );
                }
            }
        }
    }

    /// While the glow settles, each wake it names is its next visible step,
    /// read against the engine's own decay: until the wake the step the glass
    /// last drew is still the true one (no step slept through), and the
    /// wake's frame draws a different one (no wasted wake: not a poll). A
    /// step nearer than a frame is woken at the frame floor, which lands only
    /// after it. Negative controls on the same burst: a fixed 33 ms poll
    /// wastes wakes, and waking only at the cold crossing sleeps through
    /// steps.
    #[test]
    fn each_settle_wake_is_the_glows_next_visible_step() {
        #[derive(Clone, Copy)]
        enum Schedule {
            Analytic,
            Poll,
            ColdOnly,
        }
        let cfg = on(0x0050_FA7B, true);
        // The `u8` step `tick` draws at `t` (intensity 1), 0 once cold.
        let shown = |g: &MomentumGlow, t: Instant| {
            let a = g.value(t, TAU);
            if a < MOMENTUM_GLOW_HOT {
                0
            } else {
                (a * 255.0) as u8
            }
        };
        // (settle wakes, wasted wakes, wakes that slept through a step).
        let run = |schedule: Schedule, lane: Duration| -> (u32, u32, u32) {
            let t0 = Instant::now();
            let mut g = MomentumGlow::default();
            let mut out = Vec::new();
            let mut now = t0;
            for k in 0..24u64 {
                now = t0 + Duration::from_millis(60 * k);
                g.on_key(now, TAU);
                out.clear();
                g.tick(Some((4, 10)), now, geom(), &cfg, &mut out, &mut Vec::new());
            }
            let mut drawn = shown(&g, now);
            let (mut wakes, mut wasted, mut slept) = (0u32, 0u32, 0u32);
            loop {
                let floor = now + lane;
                let settle = g.settling();
                let next = if g.is_active() {
                    floor
                } else if settle {
                    match schedule {
                        Schedule::Analytic => g
                            .next_change_deadline(now, lane)
                            .expect("a settling glow names its next step"),
                        Schedule::Poll => now + Duration::from_millis(33),
                        Schedule::ColdOnly => {
                            (g.cold_at().expect("still hot") + SETTLE_MARGIN).max(floor)
                        }
                    }
                } else {
                    break;
                };
                assert!(
                    next.saturating_duration_since(t0) < Duration::from_secs(60),
                    "fixture: the glow never went cold"
                );
                if settle {
                    wakes += 1;
                    if next > floor && shown(&g, next - Duration::from_millis(2)) != drawn {
                        slept += 1;
                    }
                }
                now = next;
                out.clear();
                g.tick(Some((4, 10)), now, geom(), &cfg, &mut out, &mut Vec::new());
                let step = shown(&g, now);
                if settle && step == drawn {
                    wasted += 1;
                }
                drawn = step;
            }
            (wakes, wasted, slept)
        };
        for lane in [Duration::from_micros(16_667), Duration::from_micros(33_333)] {
            let (wakes, wasted, slept) = run(Schedule::Analytic, lane);
            assert!(wakes > 0, "fixture at {lane:?}: the cool-down settles");
            assert_eq!(
                (wasted, slept),
                (0, 0),
                "at {lane:?}, of {wakes} settle wakes: wasted, slept through a step"
            );
            let (_, wasted, _) = run(Schedule::Poll, lane);
            assert!(wasted > 0, "control at {lane:?}: a 33 ms poll wastes wakes");
            let (_, _, slept) = run(Schedule::ColdOnly, lane);
            assert!(
                slept > 0,
                "control at {lane:?}: waking only at the cold crossing sleeps through steps"
            );
        }
    }

    /// The temperature ramp is continuous and monotone in luminance on a dark
    /// theme, so a rising momentum never flickers darker.
    #[test]
    fn ramp_is_monotone_in_luminance_on_dark() {
        let lum = |c: u32| {
            0.299 * ((c >> 16) & 0xff) as f32
                + 0.587 * ((c >> 8) & 0xff) as f32
                + 0.114 * (c & 0xff) as f32
        };
        let mut last = 0.0;
        for k in 0..=100 {
            let l = lum(ramp(k as f32 / 100.0, true));
            assert!(l + 0.5 >= last, "ramp dips at {k}: {l} < {last}");
            last = l;
        }
    }
}
