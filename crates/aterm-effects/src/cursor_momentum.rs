// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TYPING-MOMENTUM GLOW — the cursor glows with how fast you are typing, and
//! cools down when you stop. While it is warm it does not blink.
//!
//! Owner, 2026-09-08, verbatim: *"the blinking cursor is annoying, I want some
//! momentum glow for typing faster that cools down."*
//!
//! # Why this is not [`crate::typing_momentum::TypingMomentum`]
//!
//! That metric is RATE-NORMALISED by law — a key-repeat flood and a 30 ms
//! sprint build at the same 0.65/s (its module doc, and the pin
//! `flood_and_sprint_build_at_the_same_pace`) — so 3 keys/s and 30 keys/s
//! would glow identically, the opposite of "for typing faster". Its τ = 2 s
//! and 2.9 s-to-full arc gate cat drama, not a light. And it advances only on
//! echo-correlated spawns inside an enabled `CursorGlow`, so it is off when
//! `cursor_trail` is off. This glow is a CURSOR feature: any style, any shape,
//! trail on or off. What IS reused is the integrator pattern (a `Copy` state,
//! an `Option<Instant>` stamp, lazy decay, snap-to-zero), because that pattern
//! is what makes a metric pure in the injected `now` and free of the wall
//! clock.
//!
//! # The law
//!
//! * One key credits [`MOMENTUM_GLOW_KEY_CREDIT`] (0.10) from wherever the
//!   decayed value is, clamped to 1. A steady rate `r` keys/s therefore
//!   equilibrates at `K / (1 − exp(−1/(r·τ)))`: with τ = 1.2 s that is 0.18 at
//!   1/s, 0.29 at 2/s, 0.53 at 4/s, and 1.0 (clamped) from ≈ 8/s — momentum IS
//!   monotone in rate, which is the whole ask.
//! * Between keys the value decays on [`MOMENTUM_GLOW_TAU_S`]: half in 0.83 s,
//!   and from 1.0 down to the blink-suppression floor
//!   ([`MOMENTUM_GLOW_HOT`], 0.02) in `τ·ln 50 ≈ 4.7 s` — the "cools down".
//! * Only PRINTABLE typing feeds it. Deletes, kills, navigation never build,
//!   and they need no drain either: silence already cools it.
//! * While the value is at or above the floor the cursor is HOT: the host pins
//!   a Blinking* style to its Steady* twin (the same precedence the rainbow
//!   twinkle already uses) and the halo is on glass. Fully cooled, the cursor
//!   is exactly what it was — the configured style, blinking or not.
//!
//! # What it draws
//!
//! A soft additive halo around the cursor cell (three stacked discs, premul
//! coverage capped at [`COV_CAP`] like every sibling body effect) and a warm
//! tint on the block body, both ramping amber → gold → white-hot with
//! momentum. On a light theme the halo is a source-over veil ending at a deep
//! orange instead of white, because white sinks into paper. Everything is
//! quantised to `u8` so the fingerprint SETTLES and the effect lane releases
//! once the light stops visibly changing, even while the value is still > 0.
//! The halo is emitted in WINDOW px (`Geom::cell_center` + `push_fx_rect`),
//! the contract of the `cursor_glow_add` stream it joins: at a real window's
//! origin it surrounds the caret cell (`halo_surrounds_the_caret_in_a_padded_
//! headed_window`), and at origin (0,0) it is byte-identical to the grid form.
//!
//! Off (disabled, intensity 0, degenerate geometry, no cursor, or cold) is
//! BYTE-IDENTICAL to no effect: `fill: None`, `hot: false`, `fp: 0`, and the
//! scratch untouched — the contract every cursor body effect keeps.

use aterm_render::{GlowQuad, premul_rgb};
use aterm_time::Instant;

use crate::cursor_glow::Geom;
// WINDOW-ABSOLUTE on purpose. The halo lands in `cursor_glow_add`, whose
// quads are window px (`GlowQuad`'s INVARIANTS: the producer folds in the
// grid origin, the renderer adds NO offset). The first cut went through
// `push_grid_rect` with a grid-relative centre, which is byte-identical at
// origin (0,0) — every unit test's geometry — and displaced by exactly
// (origin_x, origin_y) in a real window: the halo floated in the chrome head
// band, 84 px above and 24 px left of the caret (capture, 2026-09-08). The
// Fire arm in `cursor_glow.rs` documents the same trap.
use crate::effect_util::push_fx_rect as push_rect;

/// What one printable key adds, from wherever the decayed value is.
pub const MOMENTUM_GLOW_KEY_CREDIT: f32 = 0.10;

/// The cool-down time constant in seconds (half-life ≈ 0.83 s).
pub const MOMENTUM_GLOW_TAU_S: f32 = 1.2;

/// At or above this the cursor is HOT: blink is suppressed and the halo is
/// on glass. Below it the cursor is exactly the configured cursor. Chosen to
/// match the rainbow twinkle's settled-energy floor.
pub const MOMENTUM_GLOW_HOT: f32 = 0.02;

/// Snap-to-exact-zero threshold: below this the value reads 0 and the state
/// unstamps, so a cold cursor stays cold on every frame with no drift.
const SNAP_ZERO: f32 = 0.005;

/// Per-quad premultiplied coverage cap, the sibling body effects' number.
const COV_CAP: f32 = 92.0;

/// Halo reach in cell widths at full momentum, beyond the cell's own half.
pub const MOMENTUM_GLOW_RADIUS_CELLS: f32 = 1.6;

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
    pub fn value(&self, now: Instant, tau: f32) -> f32 {
        let Some(at) = self.at else {
            return 0.0;
        };
        if !tau.is_finite() || tau <= 0.0 {
            return 0.0;
        }
        let dt = now.saturating_duration_since(at).as_secs_f32();
        let v = self.value * (-dt / tau).exp();
        if !v.is_finite() || v < SNAP_ZERO {
            0.0
        } else {
            v.min(1.0)
        }
    }

    /// One PRINTABLE key at `now`: decay to `now`, credit, clamp to 1, stamp.
    pub fn on_key(&mut self, now: Instant, tau: f32) {
        self.value = (self.value(now, tau) + MOMENTUM_GLOW_KEY_CREDIT).min(1.0);
        self.at = Some(now);
    }

    /// Back to rest.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// HOT at `now`: at or above [`MOMENTUM_GLOW_HOT`].
    #[must_use]
    pub fn hot(&self, now: Instant, tau: f32) -> bool {
        self.value(now, tau) >= MOMENTUM_GLOW_HOT
    }

    /// The analytic instant the decaying value crosses the HOT floor —
    /// `stamp + τ·ln(v/floor)` — or `None` while unstamped / already cold.
    /// Lets a frame-rate-independent host schedule the exact release.
    #[must_use]
    pub fn cold_at(&self, tau: f32) -> Option<Instant> {
        let at = self.at?;
        if !tau.is_finite() || tau <= 0.0 || self.value <= MOMENTUM_GLOW_HOT {
            return Some(at);
        }
        let secs = tau * (self.value / MOMENTUM_GLOW_HOT).ln();
        Some(at + std::time::Duration::from_secs_f32(secs.max(0.0)))
    }
}

/// Host-side inputs for one tick.
#[derive(Clone, Copy, Debug)]
pub struct MomentumGlowConfig {
    /// The user's switch (default ON) ANDed with the host's gates: serious
    /// mode, focus, live viewport.
    pub enabled: bool,
    /// `0..=1` — the motion amplitude × load-shed envelope, folded by the host.
    pub intensity: f32,
    /// The cool-down τ in seconds ([`MOMENTUM_GLOW_TAU_S`] unless configured).
    pub tau_s: f32,
    /// Halo reach at full momentum, in cell widths.
    pub radius_cells: f32,
    /// The resolved cursor colour `0x00RRGGBB` — what the tint is built from.
    pub base: u32,
    pub dark_theme: bool,
    /// Whether the cursor is a BLOCK (the body tint applies) — bar/underline
    /// get the halo only.
    pub block: bool,
}

impl MomentumGlowConfig {
    #[must_use]
    pub const fn off() -> Self {
        Self {
            enabled: false,
            intensity: 0.0,
            tau_s: MOMENTUM_GLOW_TAU_S,
            radius_cells: MOMENTUM_GLOW_RADIUS_CELLS,
            base: 0x00FF_FFFF,
            dark_theme: true,
            block: true,
        }
    }
}

/// One tick's outputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MomentumGlowFrame {
    /// The block body tint, `None` when cold, off, or not a block.
    pub fill: Option<u32>,
    /// HOT: the host pins a Blinking* style to its Steady* twin.
    pub hot: bool,
    /// Fingerprint of everything visible; 0 when nothing is.
    pub fp: u64,
}

/// The engine: one integrator plus the fingerprint law that releases the
/// effect lane once the light stops changing.
#[derive(Clone, Copy, Debug, Default)]
pub struct MomentumGlow {
    rate: TypingRate,
    fp_last: u64,
    fp_prev: u64,
    latched: f32,
}

impl MomentumGlow {
    /// One PRINTABLE key. The host calls this for glyph-producing keys only
    /// (never a bare modifier, never delete/kill/navigation).
    pub fn on_key(&mut self, now: Instant, tau_s: f32) {
        self.rate.on_key(now, tau_s);
    }

    /// The decayed value at `now` — for diagnostics and the host's cadence.
    #[must_use]
    pub fn value(&self, now: Instant, tau_s: f32) -> f32 {
        self.rate.value(now, tau_s)
    }

    /// Back to rest (layout/space resets only).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The effect lane is live while the light is HOT and still CHANGING —
    /// the same fingerprint law the rainbow body keeps, so a settled glow
    /// early-outs the present even while the value is still > 0.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.latched >= MOMENTUM_GLOW_HOT && self.fp_last != self.fp_prev
    }

    /// While hot the host needs a frame every ~33 ms (the present gate dedups
    /// identical u8 frames); cold needs nothing.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant, tau_s: f32) -> Option<Instant> {
        self.rate
            .hot(now, tau_s)
            .then(|| now + std::time::Duration::from_millis(33))
    }

    /// One frame. Pushes additive quads into `out` (the host's aurora
    /// scratch) and returns the fill / blink verdict / fingerprint.
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        geom: Geom,
        cfg: &MomentumGlowConfig,
        out: &mut Vec<GlowQuad>,
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

        let s = smoothstep(a);
        let tint = ramp(a, cfg.dark_theme);
        let fill = cfg.block.then(|| mix_rgb(cfg.base, tint, 0.55 * s));

        // The halo: three stacked discs around the cell centre, reaching
        // `radius_cells` beyond the cell at full momentum. The centre is the
        // cell's WINDOW-px centre (`origin + (index + 0.5)·cell`, the anchor
        // every emitter shares) and the discs go through `push_fx_rect`, so
        // the light sits on the caret in a padded, headed window and not at
        // the same numbers measured from the window corner.
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let (cx, cy) = geom.cell_center(cr, cc);
        let rx_full = cw * (0.55 + cfg.radius_cells * s);
        let ry_full = ch * (0.45 + 0.6 * cfg.radius_cells * s);
        let discs: [(f32, f32, f32); 3] =
            [(1.00, 0.40, 0.0), (0.70, 0.70, 0.35), (0.42, 1.00, 0.7)];
        let cov_core = (COV_CAP * a).min(COV_CAP);
        let slab = ((ch * 0.125) as i32).max(2);
        for &(scale, cov_x, warm) in &discs {
            let (rx, ry) = (rx_full * scale, ry_full * scale);
            if rx < 1.0 || ry < 1.0 {
                continue;
            }
            let cov = (cov_core * cov_x).min(COV_CAP) as u8;
            if cov == 0 {
                continue;
            }
            let colour = mix_rgb(tint, ramp((a + warm).min(1.0), cfg.dark_theme), 0.5);
            let premul = premul_rgb(colour, cov);
            let ry_i = ry as i32;
            let mut dy = -ry_i;
            while dy < ry_i {
                let h = slab.min(ry_i - dy);
                let ym = (dy as f32 + h as f32 * 0.5) / ry;
                let half = (rx * (1.0 - ym * ym).max(0.0).sqrt()) as i32;
                if half >= 1 {
                    push_rect(
                        out,
                        geom,
                        cx as i32 - half,
                        cy as i32 + dy,
                        2 * half,
                        h,
                        premul,
                    );
                }
                dy += h;
            }
        }

        // Fingerprint: the quantised amplitude, the fill, and the cell — so a
        // settled glow early-outs and any visible step forces a present.
        let q = (a * 255.0) as u64;
        let fp = q
            .wrapping_mul(1_000_003)
            .wrapping_add(u64::from(fill.unwrap_or(0)) << 8)
            .wrapping_add((u64::from(cr) << 40) ^ (u64::from(cc) << 52))
            | 1;
        self.fp_prev = self.fp_last;
        self.fp_last = fp;
        MomentumGlowFrame {
            fill,
            hot: true,
            fp,
        }
    }

    fn inert(&mut self) -> MomentumGlowFrame {
        self.latched = 0.0;
        self.fp_prev = self.fp_last;
        self.fp_last = 0;
        MomentumGlowFrame::default()
    }
}

#[inline]
fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Colour temperature with momentum: amber → gold → warm white → white-hot on
/// a dark theme; on a light theme the top is a deep orange, because white
/// sinks into paper.
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

#[inline]
fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |sh: u32| {
        let x = ((a >> sh) & 0xff) as f32;
        let y = ((b >> sh) & 0xff) as f32;
        ((x + (y - x) * t).round().clamp(0.0, 255.0)) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
            block: true,
        }
    }

    /// Feed keys at `rate` keys/s from `t0` for `secs`; returns the instant of
    /// the LAST key — the value is read there, not one gap later, because a
    /// gap of silence is exactly the cool-down the other test measures.
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

    /// The ask, verbatim: "momentum glow for typing FASTER". So the value
    /// must be strictly increasing in the key rate — the property the
    /// rate-normalised family metric deliberately does NOT have.
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

    /// "…that cools down": monotone non-increasing in silence, half-life at
    /// τ·ln 2, exactly zero at the end, and the analytic crossing agrees with
    /// the sampled one.
    #[test]
    fn cooldown_is_monotone_and_snaps_to_exact_zero() {
        let t0 = Instant::now();
        let mut r = TypingRate::default();
        for _ in 0..40 {
            r.on_key(t0, TAU); // clamps to 1.0
        }
        assert!((r.value(t0, TAU) - 1.0).abs() < 1e-6);
        let mut last = 1.0f32;
        let mut sampled_cold: Option<Instant> = None;
        for k in 1..=500 {
            let t = t0 + Duration::from_millis(16 * k);
            let v = r.value(t, TAU);
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
        let half = r.value(t0 + Duration::from_secs_f32(TAU * 2f32.ln()), TAU);
        assert!((half - 0.5).abs() < 0.01, "half-life at τ·ln2: {half}");
        let analytic = r.cold_at(TAU).expect("stamped");
        let sampled = sampled_cold.expect("it cools");
        let gap = sampled.saturating_duration_since(analytic).as_secs_f32();
        assert!(
            gap < 0.02,
            "analytic crossing agrees with the sampled one ({gap} s)"
        );
    }

    /// HOT after one key, and cold once the value drops below the floor —
    /// the host reads this to pin a Blinking* style to its Steady* twin.
    #[test]
    fn blink_is_suppressed_while_hot_and_returns_when_cold() {
        let t0 = Instant::now();
        let mut g = MomentumGlow::default();
        let mut out = Vec::new();
        assert!(
            !g.tick(Some((3, 3)), t0, geom(), &on(0x00A0_A0A0, true), &mut out)
                .hot
        );
        g.on_key(t0, TAU);
        let f = g.tick(Some((3, 3)), t0, geom(), &on(0x00A0_A0A0, true), &mut out);
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
        let f = g.tick(Some((1, 1)), t, geom(), &cfg, &mut out);
        assert_eq!(f, MomentumGlowFrame::default());
        assert!(out.is_empty());
    }

    /// Pure in the injected `now`: two engines fed one script agree bit for
    /// bit, and sampling at 60 Hz or 7 Hz agrees at shared instants (lazy
    /// decay trusts no dt).
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
            fa = a.tick(Some((2, 2)), t, geom(), &on(0x0080_80FF, true), &mut out_a);
            if k % 8 == 0 {
                out_b.clear();
                fb = b.tick(Some((2, 2)), t, geom(), &on(0x0080_80FF, true), &mut out_b);
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
        }];
        let sentinel = out.clone();
        let mut off = on(0x00FF_FFFF, true);
        off.enabled = false;
        assert_eq!(
            g.tick(Some((1, 1)), t0, geom(), &off, &mut out),
            MomentumGlowFrame::default()
        );
        let mut zero = on(0x00FF_FFFF, true);
        zero.intensity = 0.0;
        assert_eq!(
            g.tick(Some((1, 1)), t0, geom(), &zero, &mut out),
            MomentumGlowFrame::default()
        );
        assert_eq!(
            g.tick(None, t0, geom(), &on(0x00FF_FFFF, true), &mut out),
            MomentumGlowFrame::default()
        );
        assert_eq!(
            g.tick(Some((99, 1)), t0, geom(), &on(0x00FF_FFFF, true), &mut out),
            MomentumGlowFrame::default()
        );
        assert_eq!(out, sentinel, "an inert tick touches nothing");
        assert!(!g.is_active());
    }

    /// The halo hugs the cell — every quad inside the frame, its reach bounded
    /// by the radius law — and every quad's coverage sits under the cap.
    #[test]
    fn halo_hugs_the_cell_and_caps_coverage() {
        let t0 = Instant::now();
        let g0 = geom();
        let mut g = MomentumGlow::default();
        for _ in 0..40 {
            g.on_key(t0, TAU);
        }
        let mut out = Vec::new();
        let f = g.tick(Some((10, 40)), t0, g0, &on(0x00FF_FFFF, true), &mut out);
        assert!(f.hot && !out.is_empty());
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
            let max_ch = [
                (q.color >> 16) & 0xff,
                (q.color >> 8) & 0xff,
                q.color & 0xff,
            ]
            .into_iter()
            .max()
            .unwrap();
            assert!(
                max_ch <= COV_CAP as u32,
                "premul channel {max_ch} over the cap"
            );
        }
        // Light theme: still hugs, still capped, tint tops out orange not white.
        assert_ne!(ramp(1.0, false), 0x00FF_FFFF);
        assert_eq!(ramp(1.0, true), 0x00FF_FFFF);
    }

    /// THE SITING LAW. The halo joins `cursor_glow_add`, a WINDOW-ABSOLUTE
    /// stream, so in a real window — the captured one: cell 15×28, pad 24,
    /// head 80, origin (24, 84) — the halo's bounding box SURROUNDS the caret
    /// cell on every side and its centre of light is the cell's own centre.
    /// The grid-relative form this replaces put the blob at (157, 126) for a
    /// caret at (174..189, 196..224): 84 px up in the chrome band, 24 px left.
    /// And at origin (0,0), head 0, the window form is byte-identical to the
    /// grid form (the identity law every sibling emitter keeps).
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
        let f = g.tick(Some((cr, cc)), t0, real, &on(0x00FF_FFFF, true), &mut out);
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
        // Centre of light = the cell centre (the discs are symmetric about it,
        // so the bbox midpoint is within a slab of it on each axis).
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

        // The identity law: at origin (0,0) the window form IS the grid form —
        // the same bytes the grid-relative emitter produced, so nothing that
        // was measured at the origin moves.
        let g0 = geom();
        let mut grid_form = Vec::new();
        let mut h = MomentumGlow::default();
        for _ in 0..40 {
            h.on_key(t0, TAU);
        }
        h.tick(
            Some((10, 40)),
            t0,
            g0,
            &on(0x00FF_FFFF, true),
            &mut grid_form,
        );
        let cx = (40.0 + 0.5) * g0.cw as f32;
        let gx0 = grid_form
            .iter()
            .map(|q| f32::from(q.x))
            .fold(f32::MAX, f32::min);
        let gx1 = grid_form
            .iter()
            .map(|q| f32::from(q.x + q.w))
            .fold(f32::MIN, f32::max);
        assert!(((gx0 + gx1) * 0.5 - cx).abs() <= 2.0);
        assert!(
            grid_form
                .iter()
                .all(|q| u32::from(q.y) / g0.ch as u32 == u32::from(q.row)),
            "row tags are y / ch at the origin"
        );
    }

    /// The lane releases when the light stops CHANGING, not when the value
    /// reaches zero: after the quantised output settles, `is_active` is false
    /// while `value` is still > 0.
    #[test]
    fn fingerprint_settles_so_the_lane_releases() {
        let t0 = Instant::now();
        let mut g = MomentumGlow::default();
        g.on_key(t0, TAU);
        let mut out = Vec::new();
        let cfg = on(0x0010_2030, true);
        let f1 = g.tick(Some((1, 1)), t0, geom(), &cfg, &mut out);
        assert!(f1.hot);
        // Same instant twice: the fingerprint repeats, so the lane releases…
        let f2 = g.tick(Some((1, 1)), t0, geom(), &cfg, &mut out);
        assert_eq!(f1.fp, f2.fp);
        assert!(!g.is_active(), "an unchanged frame must not hold the lane");
        assert!(
            g.value(t0, TAU) > 0.0,
            "…even though the value is still warm"
        );
        // …and a later, dimmer frame re-arms it.
        let f3 = g.tick(
            Some((1, 1)),
            t0 + Duration::from_millis(400),
            geom(),
            &cfg,
            &mut out,
        );
        assert_ne!(f3.fp, f2.fp);
        assert!(g.is_active());
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
