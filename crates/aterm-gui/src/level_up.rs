// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE UPGRADE SURGE: the border of the terminal comes alive while aterm replaces
//! itself in place, and bursts when the new build lands. Two phases of ONE
//! effect, both accent-themed off the live cursor colour and both painted
//! through the SAME drop-target overlay pass the drag-and-drop highlight uses
//! (CPU `apply_overlay_at`, GPU `DropOverlay`, and the SACRED `image`/`snapshot`
//! introspection) so it reads on glass AND to an AI:
//!
//!   * [`Phase::Charging`] — from the instant the outgoing process parks its
//!     readers until the successor takes over: an inset rim that ramps up fast,
//!     thickens, and PULSES on a quick period while its colour is pulled from the
//!     theme accent toward an electric white-cyan — energy building. Open-ended
//!     (the freeze it explains has no fixed length) and capped at
//!     [`CHARGE_TTL`], the same ceiling the handoff's own readiness deadline has.
//!     The successor restarts it the moment it can paint, so the rim is
//!     continuous across the swap.
//!   * [`Phase::Landing`] — the moment the handoff commits: a BURST (the rim
//!     jumps to full, thick, electric; the wash flares) that decays within half a
//!     second into the breathing glow the celebration always had, then fades.
//!     The RISING UP-ARROW (↑) — a single bold accent glyph that rises through
//!     the window centre and fades, rasterized as a paint-only [`DrawPrim`] card
//!     (the `level_up_card` tray-quad slot) — rides the landing only.
//!
//! GLOBAL (App-level), like `notice`/`config_notice`, and it borrows the SAME timed
//! lifecycle shape (`is_expired` + `deadline` + a quantized `fingerprint`). It
//! animates for its WHOLE life, so `deadline` steps every [`FRAME`] — except under
//! reduced motion, where the rim holds still (information kept, movement removed:
//! the notice pill's rule) and the deadline is the next instant the pixels change
//! (a still charging rim: its cap; a still landing: the held arrow's fade-out,
//! then the closing fade).
//!
//! WHY THE BORDER, AND NOT A CARD. The AUTOMATIC lane cannot add a status-bar row
//! at apply time (its own re-check reads the re-grid's SIGWINCH as activity), any
//! row the explicit lane adds must be raised before the handoff carry is built
//! (`App::begin_update_installing`), and the owner's brief forbids a floating card
//! over the terminal. The rim covers no cell and takes no row: it is the one
//! surface that can say "upgrading" on a frozen frame without lying about the
//! frame.
//!
//! It stays TASTEFUL by construction: the charging rim never exceeds the drop
//! overlay's crisp 235 except in the landing burst, the wash stays low enough to
//! keep the terminal readable at every instant, and the arrow bursts once (≈1.1s)
//! then clears. Serious mode drops the whole thing
//! (`SeriousEffect::LevelUp`); reduced motion keeps a still rim
//! (`MotionEffect::UpgradeSurge`, amplitude 0 ⇒ no pulse, no burst, no hue
//! travel, no thickening — and the arrow holds its place while it fades).

use std::time::{Duration, Instant};

use crate::settings::{SettingsGeom, text_w};
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// The landing's whole lifetime (burst + breathing + fade-out).
const TTL: Duration = Duration::from_millis(2400);
/// The charging phase's CAP: it explains a freeze that ends on its own (the
/// successor takes over, or the attempt rolls back and the caller clears it),
/// and this is the ceiling the handoff's readiness deadline has plus five
/// seconds — the same reasoning as the old installing card's lifetime.
const CHARGE_TTL: Duration = Duration::from_secs(125);
/// Animation cadence — the surge animates the WHOLE time, so this is the
/// deadline granularity and the fingerprint quantum (~30 fps).
const FRAME: Duration = Duration::from_millis(33);
/// The charging rim ramps 0→1 over this opening stretch.
const CHARGE_RAMP_IN: Duration = Duration::from_millis(160);
/// One pulse of the charging rim (fast: energy building, not a breath).
const CHARGE_PULSE_PERIOD: Duration = Duration::from_millis(420);
/// The charging rim's base intensity climbs over this stretch — the longer the
/// freeze, the more charged the rim looks.
const CHARGE_CLIMB: Duration = Duration::from_millis(3000);
/// The landing's opening BURST decays over this stretch into the breath.
const BURST: Duration = Duration::from_millis(520);
/// The landing envelope ramps 1→0 over this closing stretch of [`TTL`].
const FADE_OUT: Duration = Duration::from_millis(650);
/// One full breath of the landed glow (its alpha oscillates on this period).
const BREATH_PERIOD: Duration = Duration::from_millis(850);
/// The up-arrow's rise + fade lifetime (starts with the landing; ends well
/// before [`TTL`] so the border keeps glowing after the arrow has cleared).
const ARROW_DUR: Duration = Duration::from_millis(1150);
/// The arrow's alpha ramps 0→1 over this opening stretch of [`ARROW_DUR`].
const ARROW_RAMP: Duration = Duration::from_millis(130);
/// The arrow's alpha ramps 1→0 over this closing stretch of [`ARROW_DUR`].
const ARROW_FADE: Duration = Duration::from_millis(480);

/// Peak charging rim alpha (0..255) — the drop overlay's crisp 235: a rim that
/// means it, without the burst's full 255.
const CHARGE_PEAK_BORDER: f32 = 235.0;
/// Where the charging rim's base starts (0..255) before it climbs.
const CHARGE_FLOOR_BORDER: f32 = 150.0;
/// Peak charging wash alpha (0..255) — low enough that a frozen screen stays
/// readable for as long as the freeze lasts.
const CHARGE_PEAK_WASH: f32 = 24.0;
/// The landed glow's steady peak border alpha (0..255) — a touch below the drop
/// overlay's 235 so the celebration reads as a warm pulse, not an alarm.
const PEAK_BORDER: f32 = 215.0;
/// The landed glow's steady peak interior-wash alpha (0..255).
const PEAK_WASH: f32 = 20.0;
/// The landing burst's wash alpha (0..255) at its very first frame.
const BURST_WASH: f32 = 72.0;
/// Under reduced motion the rim holds at this alpha (0..255) — present, not loud.
const STILL_BORDER: f32 = 200.0;
/// Under reduced motion the wash holds here (0..255).
const STILL_WASH: f32 = 16.0;
/// The "electric" tint the accent is pulled toward while charging and in the
/// burst: a light cyan-white, so a dark accent brightens and a light one cools.
const ELECTRIC: u32 = 0x00b8_f0ff;
/// Total vertical travel of the arrow, as a fraction of the window height.
const ARROW_TRAVEL_FRAC: f32 = 0.16;
/// The arrow glyph size is the [`TypeStep::Display`] step applied to the terminal
/// `font_px` scaled by this — a deliberately large decorative pictogram.
const ARROW_SCALE: f32 = 2.0;
/// Rim thickness as a multiple of the drop-target law (`drop_border_px`), in
/// 1/16 units: the charging rim thickens up to this at a pulse's crest.
const CHARGE_THICK_Q4: f32 = 40.0; // 2.5×
/// …and rests at this in a pulse's trough.
const CHARGE_THIN_Q4: f32 = 24.0; // 1.5×
/// The landing burst's rim thickness at its first frame, in 1/16 units.
const BURST_THICK_Q4: f32 = 44.0; // 2.75×
/// The still rim's thickness under reduced motion, in 1/16 units.
const STILL_THICK_Q4: f32 = 24.0; // 1.5×

/// Which half of the surge this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Energy building while the update applies — open-ended, capped.
    Charging,
    /// The new build took over — a burst, a glow, an arrow.
    Landing,
}

/// A live surge: its phase, its spawn instant, the build it marks (folded into
/// the fingerprint so a second one re-animates rather than aliasing) and the
/// motion amplitude it was spawned under (`MotionPolicy::amplitude(
/// MotionEffect::UpgradeSurge)`: `1.0` animates, `0.0` holds still).
pub(crate) struct LevelUp {
    phase: Phase,
    build: u64,
    spawned: Instant,
    motion: f32,
}

impl LevelUp {
    /// Begin the CHARGING phase for `build` at `now`.
    pub(crate) fn charging(build: u64, now: Instant, motion: f32) -> Self {
        Self {
            phase: Phase::Charging,
            build,
            spawned: now,
            motion: motion.clamp(0.0, 1.0),
        }
    }

    /// Begin the LANDING phase for `build` at `now`.
    pub(crate) fn landing(build: u64, now: Instant, motion: f32) -> Self {
        Self {
            phase: Phase::Landing,
            build,
            spawned: now,
            motion: motion.clamp(0.0, 1.0),
        }
    }

    pub(crate) const fn phase(&self) -> Phase {
        self.phase
    }

    fn ttl(&self) -> Duration {
        match self.phase {
            Phase::Charging => CHARGE_TTL,
            Phase::Landing => TTL,
        }
    }

    /// Fully gone (past its whole lifetime) — the caller drops it and the border/arrow
    /// vanish on the next present. A charging surge is normally ended by its owner
    /// (the successor takes over, or the attempt rolls back) well before its cap.
    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.spawned) >= self.ttl()
    }

    /// Whether the pixels change between frames right now: always while animating;
    /// under reduced motion only through a phase's opening ramp, the arrow's
    /// two fades in place (it still fades in and out, it just does not travel
    /// — its hold between them is still), and the closing fade.
    fn animates(&self, now: Instant) -> bool {
        if self.motion > 0.0 {
            return true;
        }
        let e = now.duration_since(self.spawned);
        match self.phase {
            Phase::Charging => e < CHARGE_RAMP_IN,
            Phase::Landing => {
                e < ARROW_RAMP
                    || (e >= ARROW_DUR - ARROW_FADE && e < ARROW_DUR)
                    || e + FADE_OUT >= TTL
            }
        }
    }

    /// Begin the CHARGING phase ALREADY CHARGED — the successor's half of a rim
    /// the outgoing process has been charging for seconds: no ramp-in and the
    /// base already climbed, so the swap does not dip the rim to nothing and
    /// build it back. The cap counts from the back-dated instant, which only
    /// shortens it.
    pub(crate) fn charging_continued(build: u64, now: Instant, motion: f32) -> Self {
        // Back-dated by a whole number of pulse periods, at least the climb: the
        // base is up, and the pulse starts at its MIDPOINT. The outgoing
        // process's pulse phase at the swap is unknown, so a midpoint start
        // bounds the visible step to half the pulse's swing instead of betting
        // on a trough.
        let periods = CHARGE_CLIMB
            .as_millis()
            .div_ceil(CHARGE_PULSE_PERIOD.as_millis());
        let back = CHARGE_PULSE_PERIOD * u32::try_from(periods).unwrap_or(u32::MAX);
        Self {
            phase: Phase::Charging,
            build,
            spawned: now.checked_sub(back).unwrap_or(now),
            motion: motion.clamp(0.0, 1.0),
        }
    }

    /// The next wake time: one [`FRAME`] ahead while the surge is moving; a still
    /// rim (reduced motion, past its ramp) wakes only when something next
    /// changes — the held arrow's fade-out, then the rim's closing fade.
    pub(crate) fn deadline(&self, now: Instant) -> Instant {
        if self.animates(now) {
            return now + FRAME;
        }
        match self.phase {
            Phase::Charging => self.spawned + CHARGE_TTL,
            Phase::Landing => {
                let e = now.duration_since(self.spawned);
                if e < ARROW_DUR - ARROW_FADE {
                    self.spawned + (ARROW_DUR - ARROW_FADE)
                } else {
                    self.spawned + (TTL - FADE_OUT)
                }
            }
        }
    }

    /// The pulse of the charging rim: a sine on [`CHARGE_PULSE_PERIOD`] mapped to
    /// `[0, 1]`, scaled by the motion amplitude (`0` ⇒ a steady `0.5`).
    fn pulse(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned).as_secs_f32();
        let phase = std::f32::consts::TAU * e / CHARGE_PULSE_PERIOD.as_secs_f32();
        let wave = 0.5 + 0.5 * phase.sin();
        0.5 + (wave - 0.5) * self.motion
    }

    /// The charging envelope: a fast ramp-in, then `1` until the cap.
    fn charge_envelope(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned);
        if e >= CHARGE_TTL {
            return 0.0;
        }
        (e.as_secs_f32() / CHARGE_RAMP_IN.as_secs_f32()).min(1.0)
    }

    /// How charged the rim's BASE is: climbs `0 → 1` over [`CHARGE_CLIMB`]
    /// (motion-scaled: a still rim stays at its floor's midpoint).
    fn charge_climb(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned).as_secs_f32();
        let climb = (e / CHARGE_CLIMB.as_secs_f32()).clamp(0.0, 1.0);
        0.5 + (climb - 0.5) * self.motion
    }

    /// The landing envelope: `1` from the first frame (the burst IS the entrance),
    /// then a fade-out over the closing stretch (`0` at/after [`TTL`]).
    fn land_envelope(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned).as_secs_f32();
        let ttl = TTL.as_secs_f32();
        if e >= ttl {
            return 0.0;
        }
        ((ttl - e) / FADE_OUT.as_secs_f32()).clamp(0.0, 1.0)
    }

    /// The landing burst's strength: `1` at the first frame decaying to `0` over
    /// [`BURST`] (quadratic ease-out), scaled by the motion amplitude.
    fn burst(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned).as_secs_f32();
        let t = (e / BURST.as_secs_f32()).clamp(0.0, 1.0);
        (1.0 - t) * (1.0 - t) * self.motion
    }

    /// The "breathing" multiplier the landed glow pulses on — a sine in
    /// `[0.55, 1.0]`, motion-scaled to a steady `0.775` when still.
    fn breath(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned).as_secs_f32();
        let phase = std::f32::consts::TAU * e / BREATH_PERIOD.as_secs_f32();
        let wave = 0.55 + 0.45 * (0.5 + 0.5 * phase.sin());
        0.775 + (wave - 0.775) * self.motion
    }

    /// The inset-border alpha (0..255) at `now`.
    pub(crate) fn border_alpha(&self, now: Instant) -> u8 {
        let a = match self.phase {
            Phase::Charging => {
                let env = self.charge_envelope(now);
                if self.motion <= 0.0 {
                    STILL_BORDER * env
                } else {
                    let base = CHARGE_FLOOR_BORDER
                        + (CHARGE_PEAK_BORDER - CHARGE_FLOOR_BORDER) * self.charge_climb(now);
                    let peak = base.min(CHARGE_PEAK_BORDER);
                    env * peak * (0.78 + 0.22 * self.pulse(now))
                }
            }
            Phase::Landing => {
                let env = self.land_envelope(now);
                if self.motion <= 0.0 {
                    STILL_BORDER * env
                } else {
                    let glow = PEAK_BORDER * self.breath(now);
                    let burst = self.burst(now);
                    env * (glow + (255.0 - glow) * burst)
                }
            }
        };
        a.round().clamp(0.0, 255.0) as u8
    }

    /// The interior-wash alpha (0..255) at `now` — capped low so content stays
    /// readable; the landing burst flares it for a few frames.
    pub(crate) fn wash_alpha(&self, now: Instant) -> u8 {
        let a = match self.phase {
            Phase::Charging => {
                let env = self.charge_envelope(now);
                if self.motion <= 0.0 {
                    STILL_WASH * env
                } else {
                    env * CHARGE_PEAK_WASH * (0.6 + 0.4 * self.pulse(now))
                }
            }
            Phase::Landing => {
                let env = self.land_envelope(now);
                if self.motion <= 0.0 {
                    STILL_WASH * env
                } else {
                    let glow = PEAK_WASH * self.breath(now);
                    env * (glow + (BURST_WASH - glow) * self.burst(now))
                }
            }
        };
        a.round().clamp(0.0, 255.0) as u8
    }

    /// The rim's thickness as a multiple of the drop-target law, in 1/16 units
    /// (`16` = the plain drop border). Thick while charged, thickest in the burst,
    /// settling to the plain rim as the landed glow breathes out.
    pub(crate) fn border_scale_q4(&self, now: Instant) -> u8 {
        let q = match self.phase {
            Phase::Charging => {
                if self.motion <= 0.0 {
                    STILL_THICK_Q4
                } else {
                    CHARGE_THIN_Q4 + (CHARGE_THICK_Q4 - CHARGE_THIN_Q4) * self.pulse(now)
                }
            }
            Phase::Landing => {
                if self.motion <= 0.0 {
                    16.0
                } else {
                    16.0 + (BURST_THICK_Q4 - 16.0) * self.burst(now)
                }
            }
        };
        q.round().clamp(1.0, 255.0) as u8
    }

    /// How far the accent is pulled toward [`ELECTRIC`] at `now` (`0..1`).
    fn electric(&self, now: Instant) -> f32 {
        match self.phase {
            Phase::Charging => {
                if self.motion <= 0.0 {
                    0.4
                } else {
                    (0.3 + 0.4 * self.pulse(now)) * self.charge_envelope(now).max(0.35)
                }
            }
            Phase::Landing => {
                if self.motion <= 0.0 {
                    0.0
                } else {
                    0.75 * self.burst(now)
                }
            }
        }
    }

    /// The rim's colour at `now`: the theme `accent` (packed `0x00RRGGBB`) pulled
    /// toward the electric tint by [`Self::electric`].
    pub(crate) fn accent(&self, accent: u32, now: Instant) -> u32 {
        mix_rgb(accent, ELECTRIC, self.electric(now))
    }

    /// The rising arrow's alpha (0..1) at `now` — landing only: a fast ramp-in, a
    /// hold, then a fade to `0` at [`ARROW_DUR`] (and `0` thereafter, so the card
    /// clears and whatever is beneath shows).
    pub(crate) fn arrow_alpha(&self, now: Instant) -> f32 {
        if self.phase != Phase::Landing {
            return 0.0;
        }
        let e = now.duration_since(self.spawned).as_secs_f32();
        let dur = ARROW_DUR.as_secs_f32();
        if e >= dur {
            return 0.0;
        }
        let rin = (e / ARROW_RAMP.as_secs_f32()).min(1.0);
        let rout = ((dur - e) / ARROW_FADE.as_secs_f32()).min(1.0);
        rin.min(rout).clamp(0.0, 1.0)
    }

    /// The arrow's rise fraction (0→1, ease-out) over [`ARROW_DUR`] — `0` at the
    /// bottom of its travel, `1` at the top. Under reduced motion it holds the
    /// middle of its travel: the glyph still says "up", it just does not move.
    pub(crate) fn arrow_rise(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned).as_secs_f32();
        let t = (e / ARROW_DUR.as_secs_f32()).clamp(0.0, 1.0);
        let rise = 1.0 - (1.0 - t) * (1.0 - t);
        0.5 + (rise - 0.5) * self.motion
    }

    /// A repaint fingerprint folded into `RepaintKey::level_up_fp`, quantized to the
    /// [`FRAME`] step so the animation re-presents ~30×/s. `0` is the no-surge
    /// sentinel (the caller's `map_or(0, …)`), so a live surge is forced non-zero.
    /// A still rim (reduced motion) hashes stable through its hold, so a settled
    /// window with the rim up re-presents nothing.
    pub(crate) fn fingerprint(&self, now: Instant) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.build.hash(&mut h);
        (self.phase == Phase::Landing).hash(&mut h);
        if self.animates(now) {
            let step = (now.duration_since(self.spawned).as_millis() / FRAME.as_millis()) as u64;
            step.hash(&mut h);
        }
        h.finish() | 1
    }
}

/// Blend packed `0x00RRGGBB` colours: `a` toward `b` by `t` in `0..1`.
fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let chan = |shift: u32| {
        let x = ((a >> shift) & 0xff) as f32;
        let y = ((b >> shift) & 0xff) as f32;
        ((x + (y - x) * t).round().clamp(0.0, 255.0) as u32) << shift
    };
    chan(16) | chan(8) | chan(0)
}

/// Build the rising up-arrow as a single bold accent [`DrawPrim::Text`] glyph, with the
/// glyph's alpha and vertical position taken from the surge's arrow curves. The
/// returned `card` rect tightly bounds the glyph so [`crate::App::splice_level_up`] crops
/// the raster to it (a small, cheap card that moves up frame by frame).
pub(crate) fn arrow_tray(
    l: &LevelUp,
    g: &SettingsGeom,
    accent: [u8; 3],
    now: Instant,
) -> TrayInput {
    let win_w = g.cols as f32 * g.cw;
    let win_h = g.panel_rows as f32 * g.ch;
    // A deliberately large decorative pictogram off the Display step (see `ARROW_SCALE`).
    let size = TypeStep::Display.px(g.font_px * ARROW_SCALE);
    let s = size.get();
    let glyph = "\u{2191}"; // ↑
    let gw = text_w(glyph, s);
    // The glyph's visual centre rises from `centre + travel/2` (below) to `centre -
    // travel/2` (above) as the rise fraction goes 0→1.
    let travel = win_h * ARROW_TRAVEL_FRAC;
    let cy = win_h * 0.5 + travel * (0.5 - l.arrow_rise(now));
    // Approximate mono cap-centre → baseline (cap height ≈ 0.7·size, centred).
    let baseline = cy + s * 0.34;
    let x = (win_w - gw) * 0.5;
    let alpha = (l.arrow_alpha(now) * 255.0).round().clamp(0.0, 255.0) as u8;

    let prims: Vec<DrawPrim> = vec![text_prim(
        x,
        baseline,
        glyph.to_string(),
        size,
        TextWeight::Bold,
        TextFace::Mono,
        rgba(accent, alpha),
    )];
    // Card bounds: the glyph sits within [baseline − ascent, baseline + descent].
    let card = (x, baseline - s, gw, s * 1.34);
    TrayInput { prims, card }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom() -> SettingsGeom {
        SettingsGeom {
            cw: 9.0,
            ch: 20.0,
            font_px: 14.0,
            cols: 120,
            panel_rows: 40,
        }
    }

    /// The landing: a full-strength burst on the first frame that decays into
    /// the breathing glow, then fades to zero by the end of its life; the wash
    /// flares with the burst and stays readable afterwards; the rim is thickest
    /// in the burst and plain once it has settled; the colour starts electric
    /// and settles to the theme accent.
    #[test]
    fn the_landing_bursts_then_breathes_then_fades_to_zero() {
        let now = Instant::now();
        let l = LevelUp::landing(830, now, 1.0);
        assert_eq!(l.phase(), Phase::Landing);
        // The burst: full rim, flared wash, thick, electric — on the first frame.
        assert_eq!(l.border_alpha(now), 255);
        assert!(l.wash_alpha(now) >= 60, "{}", l.wash_alpha(now));
        assert!(l.border_scale_q4(now) > 32, "{}", l.border_scale_q4(now));
        assert_ne!(
            l.accent(0x0000_ff00, now),
            0x0000_ff00,
            "electric at the burst"
        );
        // Settled: the breathing glow, a plain rim, the theme accent.
        let mid = now + Duration::from_millis(900);
        assert!(l.border_alpha(mid) > 0 && l.border_alpha(mid) < 255);
        assert!(l.wash_alpha(mid) <= PEAK_WASH as u8 + 1);
        assert_eq!(l.border_scale_q4(mid), 16);
        assert_eq!(
            l.accent(0x0000_ff00, mid),
            0x0000_ff00,
            "settled to the theme"
        );
        // The breathing pulse actually moves the alpha across a breath.
        let a = l.border_alpha(now + Duration::from_millis(700));
        let b = l.border_alpha(now + Duration::from_millis(700) + BREATH_PERIOD / 2);
        assert_ne!(a, b, "border alpha breathes over a half-period");
        // Fully faded at/after the whole lifetime.
        assert_eq!(l.border_alpha(now + TTL), 0);
        assert_eq!(l.wash_alpha(now + TTL), 0);
        assert!(l.is_expired(now + TTL));
        assert!(!l.is_expired(now));
        assert_eq!(l.deadline(now), now + FRAME, "animating every frame");
    }

    /// The charging rim: ramps in fast, pulses on a quick period, climbs as the
    /// freeze goes on, thickens and cools toward the electric tint — and it is
    /// open-ended up to its cap, never lighting the burst or the arrow.
    #[test]
    fn the_charging_rim_ramps_pulses_climbs_and_is_capped() {
        let now = Instant::now();
        let l = LevelUp::charging(831, now, 1.0);
        assert_eq!(l.phase(), Phase::Charging);
        assert!(l.border_alpha(now) < l.border_alpha(now + CHARGE_RAMP_IN));
        // Pulses on its period…
        let a = l.border_alpha(now + Duration::from_millis(600));
        let b = l.border_alpha(now + Duration::from_millis(600) + CHARGE_PULSE_PERIOD / 2);
        assert_ne!(a, b, "the rim pulses over a half-period");
        // …climbs as the freeze goes on (compare crest to crest)…
        let early = l.border_alpha(now + CHARGE_PULSE_PERIOD / 4 + CHARGE_PULSE_PERIOD);
        let late = l.border_alpha(now + CHARGE_PULSE_PERIOD / 4 + CHARGE_PULSE_PERIOD * 12);
        assert!(late >= early, "charged: {late} vs {early}");
        // …never past the drop overlay's crisp rim, but thick and electric…
        for ms in (0..4000).step_by(37) {
            let t = now + Duration::from_millis(ms);
            assert!(l.border_alpha(t) <= CHARGE_PEAK_BORDER as u8 + 1);
            assert!(l.wash_alpha(t) <= CHARGE_PEAK_WASH as u8 + 1, "readable");
            assert!(l.border_scale_q4(t) >= CHARGE_THIN_Q4 as u8 - 1);
            assert_eq!(l.arrow_alpha(t), 0.0, "no arrow while charging");
        }
        assert_ne!(
            l.accent(0x0000_00ff, now + Duration::from_secs(1)),
            0x0000_00ff
        );
        // …and open-ended until the cap.
        assert!(!l.is_expired(now + Duration::from_secs(60)));
        assert!(l.is_expired(now + CHARGE_TTL));
        assert_eq!(l.border_alpha(now + CHARGE_TTL), 0);
    }

    /// Under reduced motion both phases hold STILL: no pulse, no burst, no hue
    /// travel, no thickening beyond the fixed rim — the information (an update is
    /// applying; it landed) stays, the movement goes — and the deadline is the
    /// next instant the pixels change (the arrow's fade-out, the closing fade,
    /// the charging cap) rather than a per-frame wake.
    #[test]
    fn reduced_motion_holds_a_still_rim_and_wakes_only_at_its_next_change() {
        let now = Instant::now();
        let c = LevelUp::charging(1, now, 0.0);
        let held = now + Duration::from_secs(2);
        assert_eq!(c.border_alpha(held), STILL_BORDER as u8);
        assert_eq!(
            c.border_alpha(held + CHARGE_PULSE_PERIOD / 2),
            STILL_BORDER as u8
        );
        assert_eq!(c.wash_alpha(held), STILL_WASH as u8);
        assert_eq!(c.border_scale_q4(held), STILL_THICK_Q4 as u8);
        assert_eq!(
            c.accent(0x0000_00ff, held),
            c.accent(0x0000_00ff, held + FRAME * 7)
        );
        assert_eq!(
            c.fingerprint(held),
            c.fingerprint(held + Duration::from_secs(20))
        );
        assert_eq!(c.deadline(held), now + CHARGE_TTL);
        assert_eq!(c.deadline(now), now + FRAME, "the ramp-in still animates");

        let l = LevelUp::landing(2, now, 0.0);
        assert_eq!(l.border_alpha(now), STILL_BORDER as u8, "no burst");
        assert_eq!(l.wash_alpha(now), STILL_WASH as u8);
        assert_eq!(l.border_scale_q4(now), 16);
        assert_eq!(l.accent(0x0000_00ff, now), 0x0000_00ff, "no hue travel");
        let mid = now + Duration::from_millis(800);
        assert_eq!(
            l.border_alpha(mid),
            l.border_alpha(mid + BREATH_PERIOD / 2),
            "no breath"
        );
        assert_eq!(
            l.deadline(mid),
            mid + FRAME,
            "the arrow is still fading in place: per-frame until it has cleared"
        );
        let hold = now + Duration::from_millis(400);
        assert_eq!(
            l.deadline(hold),
            now + (ARROW_DUR - ARROW_FADE),
            "a held arrow wakes only when its fade-out begins"
        );
        assert_eq!(l.fingerprint(hold), l.fingerprint(hold + FRAME * 3));
        let after_arrow = now + ARROW_DUR + Duration::from_millis(50);
        assert_eq!(l.deadline(after_arrow), now + (TTL - FADE_OUT));
        assert_eq!(l.arrow_rise(now), 0.5, "the arrow holds its place…");
        assert_eq!(l.arrow_rise(now + ARROW_DUR), 0.5);
        assert!(
            l.arrow_alpha(now + ARROW_RAMP) > 0.0,
            "…and still fades in and out"
        );
        assert_eq!(l.arrow_alpha(now + ARROW_DUR), 0.0);
        assert_eq!(l.border_alpha(now + TTL), 0, "and it still ends");

        // CONTINUED CHARGING (the successor's half): no ramp-in dip and the
        // base already climbed, at full motion and still.
        let c = LevelUp::charging_continued(3, now, 1.0);
        assert!(
            c.border_alpha(now) >= CHARGE_FLOOR_BORDER as u8,
            "no dip to nothing at the swap: {}",
            c.border_alpha(now)
        );
        assert!(c.border_alpha(now) > LevelUp::charging(3, now, 1.0).border_alpha(now));
        assert!(
            (c.pulse(now) - 0.5).abs() < 1e-3,
            "the pulse starts at its midpoint: {}",
            c.pulse(now)
        );
        assert_eq!(c.charge_climb(now), 1.0, "the base has climbed");
        assert!(!c.is_expired(now + Duration::from_secs(60)));
        assert!(c.is_expired(now + CHARGE_TTL));
        let cs = LevelUp::charging_continued(3, now, 0.0);
        assert_eq!(cs.border_alpha(now), STILL_BORDER as u8);
        assert_eq!(cs.deadline(now), cs.deadline(now + Duration::from_secs(5)));
    }

    #[test]
    fn arrow_rises_monotonically_and_alpha_fades_out() {
        let now = Instant::now();
        let l = LevelUp::landing(830, now, 1.0);
        // Rise is monotonic 0→1 across the arrow's life.
        let r0 = l.arrow_rise(now);
        let r1 = l.arrow_rise(now + ARROW_DUR / 2);
        let r2 = l.arrow_rise(now + ARROW_DUR);
        assert!(r0 < r1 && r1 < r2, "arrow rises: {r0} < {r1} < {r2}");
        assert!((r0 - 0.0).abs() < 1e-3 && (r2 - 1.0).abs() < 1e-3);
        // Alpha ramps in, holds, then reaches 0 by ARROW_DUR (and stays 0 after).
        assert!(l.arrow_alpha(now) < l.arrow_alpha(now + ARROW_RAMP));
        assert_eq!(l.arrow_alpha(now + ARROW_DUR), 0.0);
        assert_eq!(
            l.arrow_alpha(now + ARROW_DUR + Duration::from_millis(200)),
            0.0
        );
        // The arrow clears well before the border stops glowing.
        assert!(l.border_alpha(now + ARROW_DUR + Duration::from_millis(100)) > 0);
    }

    #[test]
    fn fingerprint_is_nonzero_stable_per_frame_and_steps() {
        let now = Instant::now();
        let l = LevelUp::landing(830, now, 1.0);
        assert_ne!(l.fingerprint(now), 0, "never the no-surge sentinel");
        // Stable within one frame quantum, changes across a frame.
        assert_eq!(
            l.fingerprint(now),
            l.fingerprint(now + Duration::from_millis(10))
        );
        assert_ne!(
            l.fingerprint(now),
            l.fingerprint(now + FRAME + Duration::from_millis(1))
        );
        // The two phases of one build never alias.
        let c = LevelUp::charging(830, now, 1.0);
        assert_ne!(c.fingerprint(now), l.fingerprint(now));
    }

    #[test]
    fn arrow_tray_paints_the_glyph_centred_within_the_window() {
        let now = Instant::now();
        let l = LevelUp::landing(830, now, 1.0);
        let g = geom();
        let t = arrow_tray(&l, &g, [0, 255, 0], now);
        assert!(
            t.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s == "\u{2191}")),
            "an up-arrow glyph is emitted"
        );
        let (x, _, w, _) = t.card;
        let win_w = g.cols as f32 * g.cw;
        // Roughly horizontally centred.
        let mid = x + w * 0.5;
        assert!(
            (mid - win_w * 0.5).abs() < 1.0,
            "arrow centred: {mid} vs {}",
            win_w * 0.5
        );
    }

    #[test]
    fn mix_rgb_interpolates_per_channel_and_clamps() {
        assert_eq!(mix_rgb(0x0000_0000, 0x00ff_ffff, 0.0), 0x0000_0000);
        assert_eq!(mix_rgb(0x0000_0000, 0x00ff_ffff, 1.0), 0x00ff_ffff);
        assert_eq!(mix_rgb(0x0000_0000, 0x00ff_ffff, 0.5), 0x0080_8080);
        assert_eq!(mix_rgb(0x0010_2030, 0x0010_2030, 0.7), 0x0010_2030);
        assert_eq!(
            mix_rgb(0x0000_0000, 0x00ff_ffff, 7.0),
            0x00ff_ffff,
            "clamped"
        );
    }
}
