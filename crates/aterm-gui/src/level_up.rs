// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The upgrade rim: the terminal's border animates while aterm replaces itself
//! in place, from the reader park until the successor takes over.

use std::time::{Duration, Instant};

/// The rim's cap, counted from the reader park where each lane starts it
/// (`App::charge_upgrade_rim_at_park`).
const CHARGE_TTL: Duration = Duration::from_secs(125);
/// Animation cadence (~30 fps): the deadline granularity and fingerprint quantum.
const FRAME: Duration = Duration::from_millis(33);
/// The rim ramps 0→1 over this opening stretch.
const CHARGE_RAMP_IN: Duration = Duration::from_millis(160);
/// One pulse of the rim.
const CHARGE_PULSE_PERIOD: Duration = Duration::from_millis(420);
/// The rim's base intensity climbs over this stretch, so a longer freeze looks
/// more charged.
const CHARGE_CLIMB: Duration = Duration::from_millis(3000);

/// Peak rim alpha (0..255), the drop overlay's rim alpha.
const CHARGE_PEAK_BORDER: f32 = 235.0;
/// Where the rim's base starts (0..255) before it climbs.
const CHARGE_FLOOR_BORDER: f32 = 150.0;
/// Peak wash alpha (0..255) — low enough that a frozen screen stays readable
/// for as long as the freeze lasts.
const CHARGE_PEAK_WASH: f32 = 24.0;
/// Under reduced motion the rim holds at this alpha (0..255).
const STILL_BORDER: f32 = 200.0;
/// Under reduced motion the wash holds here (0..255).
const STILL_WASH: f32 = 16.0;
/// The "electric" tint the accent is pulled toward: a light cyan-white, so a
/// dark accent brightens and a light one cools.
const ELECTRIC: u32 = 0x00b8_f0ff;
/// Rim thickness as a multiple of the drop-target law
/// (`aterm_render::drop_overlay_border_px`), in 1/16 units: the rim thickens
/// up to this at a pulse's crest.
const CHARGE_THICK_Q4: f32 = 40.0; // 2.5×
/// …and rests at this in a pulse's trough.
const CHARGE_THIN_Q4: f32 = 24.0; // 1.5×
/// The still rim's thickness under reduced motion, in 1/16 units.
const STILL_THICK_Q4: f32 = 24.0; // 1.5×

/// A live rim: the build it marks (folded into the fingerprint so a second one
/// re-animates rather than aliasing), its spawn instant and the motion
/// amplitude it was spawned under (`MotionPolicy::amplitude(
/// MotionEffect::UpgradeSurge)`: `1.0` animates, `0.0` holds still).
pub(crate) struct LevelUp {
    build: u64,
    spawned: Instant,
    motion: f32,
}

/// Structural repaint key for [`LevelUp`], quantized to the [`FRAME`] step while animating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LevelUpRepaintKey {
    /// Staged build number driving this rim.
    pub(crate) build: u64,
    /// Quantized [`FRAME`] step while [`LevelUp::animates`] holds, or `None` when still.
    pub(crate) frame_step: Option<u64>,
}

impl LevelUp {
    /// Begin the rim for `build` at `now`.
    #[cfg(any(unix, test))]
    pub(crate) const fn charging(build: u64, now: Instant, motion: f32) -> Self {
        Self {
            build,
            spawned: now,
            motion: motion.clamp(0.0, 1.0),
        }
    }

    /// Stop the movement and keep the rim (serious mode turned on mid-update):
    /// the reduced-motion still rim, from now to its cap.
    pub(crate) const fn hold_still(&mut self) {
        self.motion = 0.0;
    }

    /// Past its cap: the caller drops it and the rim vanishes on the next
    /// present.
    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.spawned) >= CHARGE_TTL
    }

    /// Whether the pixels change between frames right now: always while
    /// animating; under reduced motion only through the opening ramp.
    fn animates(&self, now: Instant) -> bool {
        self.motion > 0.0 || now.duration_since(self.spawned) < CHARGE_RAMP_IN
    }

    /// Begin the rim already charged, for the successor's half of the swap: no
    /// ramp-in and the base already climbed, so the rim does not dip.
    pub(crate) fn charging_continued(build: u64, now: Instant, motion: f32) -> Self {
        let periods = CHARGE_CLIMB
            .as_millis()
            .div_ceil(CHARGE_PULSE_PERIOD.as_millis());
        let back = CHARGE_PULSE_PERIOD * u32::try_from(periods).unwrap_or(u32::MAX);
        Self {
            build,
            spawned: now.checked_sub(back).unwrap_or(now),
            motion: motion.clamp(0.0, 1.0),
        }
    }

    /// The same rim at a re-park of its attempt: its cap counts again from `now`,
    /// as a rim [`Self::charging_continued`] started there would (already charged,
    /// so nothing dips), keeping its build and motion.
    #[cfg(unix)]
    pub(crate) fn re_anchor(&mut self, now: Instant) {
        let continued = Self::charging_continued(self.build, now, self.motion);
        self.spawned = self.spawned.max(continued.spawned);
    }

    /// The next wake time: one [`FRAME`] ahead while the rim is moving; a still
    /// rim (reduced motion, past its ramp) wakes only at its cap.
    pub(crate) fn deadline(&self, now: Instant) -> Instant {
        if self.animates(now) {
            now + FRAME
        } else {
            self.spawned + CHARGE_TTL
        }
    }

    /// The pulse: a sine on [`CHARGE_PULSE_PERIOD`] mapped to `[0, 1]`, scaled
    /// by the motion amplitude (`0` ⇒ a steady `0.5`).
    fn pulse(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned).as_secs_f32();
        let phase = std::f32::consts::TAU * e / CHARGE_PULSE_PERIOD.as_secs_f32();
        let wave = 0.5 + 0.5 * phase.sin();
        0.5 + (wave - 0.5) * self.motion
    }

    /// The envelope: a fast ramp-in, then `1` until the cap.
    fn charge_envelope(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned);
        if e >= CHARGE_TTL {
            return 0.0;
        }
        (e.as_secs_f32() / CHARGE_RAMP_IN.as_secs_f32()).min(1.0)
    }

    /// How charged the rim's base is: climbs `0 → 1` over [`CHARGE_CLIMB`]
    /// (motion-scaled: a still rim stays at its floor's midpoint).
    fn charge_climb(&self, now: Instant) -> f32 {
        let e = now.duration_since(self.spawned).as_secs_f32();
        let climb = (e / CHARGE_CLIMB.as_secs_f32()).clamp(0.0, 1.0);
        0.5 + (climb - 0.5) * self.motion
    }

    /// The inset-border alpha (0..255) at `now`.
    pub(crate) fn border_alpha(&self, now: Instant) -> u8 {
        let env = self.charge_envelope(now);
        let a = if self.motion <= 0.0 {
            STILL_BORDER * env
        } else {
            let base = CHARGE_FLOOR_BORDER
                + (CHARGE_PEAK_BORDER - CHARGE_FLOOR_BORDER) * self.charge_climb(now);
            let peak = base.min(CHARGE_PEAK_BORDER);
            env * peak * (0.78 + 0.22 * self.pulse(now))
        };
        a.round().clamp(0.0, 255.0) as u8
    }

    /// The interior-wash alpha (0..255) at `now` — capped low so content stays
    /// readable.
    pub(crate) fn wash_alpha(&self, now: Instant) -> u8 {
        let env = self.charge_envelope(now);
        let a = if self.motion <= 0.0 {
            STILL_WASH * env
        } else {
            env * CHARGE_PEAK_WASH * (0.6 + 0.4 * self.pulse(now))
        };
        a.round().clamp(0.0, 255.0) as u8
    }

    /// The rim's thickness as a multiple of the drop-target law, in 1/16 units
    /// (`16` = the plain drop border): thickest at a pulse's crest.
    pub(crate) fn border_scale_q4(&self, now: Instant) -> u8 {
        let q = if self.motion <= 0.0 {
            STILL_THICK_Q4
        } else {
            CHARGE_THIN_Q4 + (CHARGE_THICK_Q4 - CHARGE_THIN_Q4) * self.pulse(now)
        };
        q.round().clamp(1.0, 255.0) as u8
    }

    /// How far the accent is pulled toward [`ELECTRIC`] at `now` (`0..1`).
    fn electric(&self, now: Instant) -> f32 {
        if self.motion <= 0.0 {
            0.4
        } else {
            (0.3 + 0.4 * self.pulse(now)) * self.charge_envelope(now).max(0.35)
        }
    }

    /// The rim's colour at `now`: the theme `accent` (packed `0x00RRGGBB`) pulled
    /// toward the electric tint by [`Self::electric`].
    pub(crate) fn accent(&self, accent: u32, now: Instant) -> u32 {
        mix_rgb(accent, ELECTRIC, self.electric(now))
    }

    /// Structural repaint key folded into `RepaintKey::level_up_fp`, quantized to
    /// the [`FRAME`] step so the animation re-presents ~30×/s.
    pub(crate) fn fingerprint(&self, now: Instant) -> LevelUpRepaintKey {
        let frame_step = self
            .animates(now)
            .then(|| (now.duration_since(self.spawned).as_millis() / FRAME.as_millis()) as u64);
        LevelUpRepaintKey {
            build: self.build,
            frame_step,
        }
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

#[cfg(test)]
#[path = "level_up_tests.rs"]
mod tests;
