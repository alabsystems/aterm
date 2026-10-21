// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The upgrade rim: the terminal's border animates while aterm replaces itself
//! in place, from the reader park until the successor takes over. An inset rim
//! ramps up, thickens and pulses while its colour moves from the theme accent
//! toward a white-cyan. It paints through the drop-target overlay pass (CPU
//! `apply_overlay_at`, GPU `DropOverlay`, and `image`/`snapshot` introspection),
//! so it shows on glass and to an agent. It is open-ended up to [`CHARGE_TTL`];
//! the successor restarts it already charged ([`LevelUp::charging_continued`]) so
//! the rim is continuous across the swap, and ends it at Commit.
//!
//! It has the timed lifecycle shape (`is_expired` + `deadline` + a quantized
//! `fingerprint`): `deadline` steps every [`FRAME`], except under reduced motion
//! or serious mode, where the rim holds still and the deadline is its cap.
//!
//! A border, not a card or a row: the automatic lane cannot add a status-bar row
//! at apply time (its re-check reads the re-grid's SIGWINCH as activity), a row the
//! explicit lane adds must exist before the handoff carry is built
//! (`App::begin_update_installing`), and no floating card may cover the terminal.
//! The rim covers no cell and takes no row, so it also paints over an in-app
//! overlay.

use std::time::{Duration, Instant};

/// The rim's cap, counted from the reader park where each lane starts it
/// (`App::charge_upgrade_rim_at_park`). What follows the park is bounded well
/// inside it: the post-park proof deadline (at most 30 s, 120 s through its
/// development seam), the capture and the grant. The rim normally ends earlier,
/// at takeover or when a rolled-back attempt clears it.
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
/// Rim thickness as a multiple of the drop-target law (`drop_border_px`), in
/// 1/16 units: the rim thickens up to this at a pulse's crest.
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

impl LevelUp {
    /// Begin the rim for `build` at `now`.
    #[cfg(any(unix, test))]
    pub(crate) fn charging(build: u64, now: Instant, motion: f32) -> Self {
        Self {
            build,
            spawned: now,
            motion: motion.clamp(0.0, 1.0),
        }
    }

    /// Stop the movement and keep the rim (serious mode turned on mid-update):
    /// the reduced-motion still rim, from now to its cap.
    pub(crate) fn hold_still(&mut self) {
        self.motion = 0.0;
    }

    /// Past its cap: the caller drops it and the rim vanishes on the next
    /// present. Its owner normally ends it well before.
    pub(crate) fn is_expired(&self, now: Instant) -> bool {
        now.duration_since(self.spawned) >= CHARGE_TTL
    }

    /// Whether the pixels change between frames right now: always while
    /// animating; under reduced motion only through the opening ramp.
    fn animates(&self, now: Instant) -> bool {
        self.motion > 0.0 || now.duration_since(self.spawned) < CHARGE_RAMP_IN
    }

    /// Begin the rim already charged, for the successor's half of the swap: no
    /// ramp-in and the base already climbed, so the rim does not dip. The cap
    /// counts from the back-dated instant, which only shortens it.
    pub(crate) fn charging_continued(build: u64, now: Instant, motion: f32) -> Self {
        // Back-dated by a whole number of pulse periods, at least the climb, so
        // the pulse starts at its midpoint. The outgoing process's phase is
        // unknown; a midpoint start bounds the visible step to half the swing.
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

    /// The same rim at a RE-PARK of its attempt (round eight of the update audit,
    /// the review of leads U60 and U40): its cap counts again from `now`, as a
    /// rim [`Self::charging_continued`] starts there would — already charged, so
    /// nothing dips — keeping its build and motion. A rim younger than that keeps
    /// its own instant: its cap is further off already, and its ramp runs on. A
    /// park the launched lane admitted late after an early miss would otherwise
    /// freeze the terminal on a cap counted from the first park.
    #[cfg(any(unix, test))]
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

    /// A repaint fingerprint folded into `RepaintKey::level_up_fp`, quantized to
    /// the [`FRAME`] step so the animation re-presents ~30×/s. `0` is the no-rim
    /// sentinel (the caller's `map_or(0, …)`), so a live rim is forced non-zero.
    /// A still rim (reduced motion) hashes stable through its hold, so a settled
    /// window with the rim up re-presents nothing.
    pub(crate) fn fingerprint(&self, now: Instant) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.build.hash(&mut h);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The rim ramps in, pulses, climbs as the freeze goes on, thickens and
    /// cools toward the electric tint, and is open-ended up to its cap.
    #[test]
    fn the_charging_rim_ramps_pulses_climbs_and_is_capped() {
        let now = Instant::now();
        let l = LevelUp::charging(831, now, 1.0);
        assert!(l.border_alpha(now) < l.border_alpha(now + CHARGE_RAMP_IN));
        // Pulses on its period…
        let a = l.border_alpha(now + Duration::from_millis(600));
        let b = l.border_alpha(now + Duration::from_millis(600) + CHARGE_PULSE_PERIOD / 2);
        assert_ne!(a, b, "the rim pulses over a half-period");
        // …climbs as the freeze goes on (compare crest to crest)…
        let early = l.border_alpha(now + CHARGE_PULSE_PERIOD / 4 + CHARGE_PULSE_PERIOD);
        let late = l.border_alpha(now + CHARGE_PULSE_PERIOD / 4 + CHARGE_PULSE_PERIOD * 12);
        assert!(late >= early, "charged: {late} vs {early}");
        // …never past the drop overlay's rim alpha, but thick and electric…
        for ms in (0..4000).step_by(37) {
            let t = now + Duration::from_millis(ms);
            assert!(l.border_alpha(t) <= CHARGE_PEAK_BORDER as u8 + 1);
            assert!(l.wash_alpha(t) <= CHARGE_PEAK_WASH as u8 + 1, "readable");
            assert!(l.border_scale_q4(t) >= CHARGE_THIN_Q4 as u8 - 1);
        }
        assert_ne!(
            l.accent(0x0000_00ff, now + Duration::from_secs(1)),
            0x0000_00ff
        );
        // …and open-ended until the cap.
        assert!(!l.is_expired(now + Duration::from_secs(60)));
        assert!(l.is_expired(now + CHARGE_TTL));
        assert_eq!(l.border_alpha(now + CHARGE_TTL), 0);
        assert_eq!(l.deadline(now), now + FRAME, "animating every frame");
    }

    /// Under reduced motion the rim holds still (no pulse, hue travel or extra
    /// thickening) and its deadline is its cap. The successor's continued rim
    /// starts charged.
    #[test]
    fn reduced_motion_holds_a_still_rim_and_the_successor_continues_it_charged() {
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

        // Continued (the successor's half): no ramp-in dip and the base already
        // climbed, at full motion and still.
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
    fn fingerprint_is_nonzero_stable_per_frame_and_steps() {
        let now = Instant::now();
        let l = LevelUp::charging(830, now, 1.0);
        assert_ne!(l.fingerprint(now), 0, "never the no-rim sentinel");
        // Stable within one frame quantum, changes across a frame.
        assert_eq!(
            l.fingerprint(now),
            l.fingerprint(now + Duration::from_millis(10))
        );
        assert_ne!(
            l.fingerprint(now),
            l.fingerprint(now + FRAME + Duration::from_millis(1))
        );
        // Two builds never alias.
        assert_ne!(
            LevelUp::charging(831, now, 1.0).fingerprint(now),
            l.fingerprint(now)
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
