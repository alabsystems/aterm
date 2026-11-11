// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typing momentum spine and display follower for Rainbow Kitty.

use aterm_time::{Duration, Instant};

use super::timing::smoothstep01;
use crate::typing_momentum::TypingMomentum;

#[inline]
const fn dur_from_secs_f32(secs: f32) -> Duration {
    Duration::from_nanos((secs as f64 * 1_000_000_000.0).round() as u64)
}

/// The gain the follower puts on the canonical metric.
pub const METRIC_GAIN: f32 = 1.23;

/// Ribbon-momentum attack ease, seconds to ~63 % of a rise.
pub const DISP_ATTACK_S: f32 = 0.05;

/// The slam ease: a big rise (a fling, a celebration drive, a warm resume) lands within a couple of
/// frames.
pub const DISP_SLAM_S: f32 = 0.04;

/// How big a rise counts as a slam.
pub const DISP_SLAM_RISE: f32 = 0.5;

/// Ribbon-momentum release time constant, in seconds.
pub const DISP_RELEASE_TAU: f32 = 0.85;

/// Half-life of the resume memory, in seconds.
pub const DISP_PEAK_HALF_LIFE_S: f32 = 7.0;

/// Share of the decayed peak that floors birth pricing ([`Spine::birth_disp`]).
pub const DISP_PEAK_FLOOR_SHARE: f32 = 0.8;

/// Below this the follower and peak memory snap to exactly zero, so an idle engine disarms on a
/// clean zero (T6; `Spine::at_rest` is an exact comparison).
pub const DISP_SNAP_ZERO: f32 = 0.005;

/// The lay/flow clock's rate, sweeps per second at full spine.
pub const PHASE_RATE: f32 = 0.85;

/// The phase ring: the clock is kept modulo this so a long session cannot bleed `f32` precision.
pub const PHASE_RING: f32 = 1024.0;

/// The mend window, seconds: a typed key this soon after the last delete of a run is a typo fix,
/// born at the momentum the delete interrupted ([`Spine::mend`]).
pub const MEND_WINDOW_S: f32 = 1.2;

/// The most deletes a run may hold and still be a typo: one or two.
pub const MEND_MAX_DELETES: u8 = 2;

/// The flow floor: a typed key counts toward the run only when its own birth price
/// ([`Spine::birth_disp`]) is at least this; below it the key breaks the run.
pub const FLOW_KEY_DISP: f32 = 0.8;

/// Hot resume after a green command, seconds.
pub const VERDICT_WINDOW_S: f32 = 5.0;

/// The birth price [`Spine::note_verdict`] floors that one key at: the top of the band.
pub const VERDICT_BIRTH_DISP: f32 = 1.0;

/// Run length at which [`Flow::heat`] reaches full flow.
pub const FLOW_ENTRY_KEYS: u32 = 24;

/// Keys over which [`Flow::heat`] eases up to full flow at [`FLOW_ENTRY_KEYS`].
pub const FLOW_OPEN_EASE_KEYS: u32 = 4;

/// How long one [`Spine::drive`] freezes the combo, seconds.
pub const FLOW_FREEZE_S: f32 = 0.25;

const FLOW_FREEZE_DUR: Duration = dur_from_secs_f32(FLOW_FREEZE_S);
const VERDICT_WINDOW_DUR: Duration = dur_from_secs_f32(VERDICT_WINDOW_S);
const MEND_WINDOW_DUR: Duration = dur_from_secs_f32(MEND_WINDOW_S);

/// Minimum birth-price rise over the previous key required for [`Spine::surge`].
pub const SURGE_MIN_RISE: f32 = 0.15;

/// Birth-price rise that reaches full [`Spine::surge`].
pub const SURGE_FULL_RISE: f32 = 0.25;

/// The flow state: the value on the `trail status` row an agent can hold its turn on, and the
/// number every flowing consumer lerps against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flow {
    /// Unit flow heat from zero below the ease to full flow at [`FLOW_ENTRY_KEYS`].
    pub heat: f32,
    /// The run: keys typed at [`FLOW_KEY_DISP`] or above with no delete and no kill since it began.
    pub combo: u32,
    /// The longest run held since the last [`Spine::reset`]; an exit never lowers it.
    pub best: u32,
}

impl Default for Flow {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Flow {
    /// Idle flow state (`heat = 0.0`, `combo = 0`, `best = 0`).
    pub const EMPTY: Self = Self {
        heat: 0.0,
        combo: 0,
        best: 0,
    };

    /// Whether the run has reached [`FLOW_ENTRY_KEYS`] without breaking.
    #[inline]
    #[must_use]
    pub const fn open(&self) -> bool {
        self.combo >= FLOW_ENTRY_KEYS
    }
}

/// [`Flow::heat`] from a run length: the one site of the ease.
#[inline]
#[must_use]
const fn flow_heat(run: u32) -> f32 {
    let lo = FLOW_ENTRY_KEYS.saturating_sub(FLOW_OPEN_EASE_KEYS);
    smoothstep01(run.saturating_sub(lo) as f32 * (1.0 / FLOW_OPEN_EASE_KEYS as f32))
}

/// The erase mark a Backspace leaves for the next typed key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EraseMark {
    /// The last delete's edge; the window is measured from here.
    pub at: Instant,
    /// The birth spine ([`Spine::birth_disp`]) at the first delete of the run, kept across the run
    /// (a second delete has already drained the follower a little, and that dip is what the mend
    /// closes).
    pub disp: f32,
    /// Deletes in the run since the last typed key (saturating).
    pub count: u8,
}

/// The one momentum integrator and its display follower.
#[derive(Clone, Copy, Debug)]
pub struct Spine {
    /// The canonical metric (`crate::typing_momentum`), the raw 0..1 leaky
    /// integrator, gained only in [`Spine::target`]; consumers want
    /// [`Spine::disp`].
    momentum: TypingMomentum,
    /// The eased display value the art reads. Advanced only in
    /// [`Spine::update`].
    disp: f32,
    /// The resume memory: the follower's recent peak, forgetting on
    /// [`DISP_PEAK_HALF_LIFE_S`].
    disp_peak: f32,
    /// The lay/flow clock in dimensionless sweeps, modulo [`PHASE_RING`].
    phase: f32,
    /// The last [`Spine::update`] stamp; `None` means never ticked, so the first
    /// update only latches the clock.
    at: Option<Instant>,
    /// Latched on the tick a rise past [`DISP_SLAM_RISE`] is first seen and held
    /// until the follower is within [`DISP_SNAP_ZERO`] of its target (or the
    /// target falls below it).
    slamming: bool,
    /// The pending [`EraseMark`], if the last edge was a Backspace. Cleared by
    /// the next typed advance, a kill, and [`Spine::reset`].
    mark: Option<EraseMark>,
    /// The flow run: keys typed at [`FLOW_KEY_DISP`] or above with no delete
    /// since it began.
    run: u32,
    /// The run's high-water mark, kept across exits.
    best: u32,
    /// The birth price the previous typed key was born at, the other half of
    /// the surge's `Δ` ([`Spine::surge`]).
    prev_birth: f32,
    /// The last key's own rise over `prev_birth`, latched at its edge so the
    /// tick that births the key reads that key's number.
    key_rise: f32,
    /// The last [`Spine::drive`] with a live floor; the combo is frozen for
    /// [`FLOW_FREEZE_S`] after it.
    driven_at: Option<Instant>,
    /// The hot-resume door: a green long command's stamp, living
    /// [`VERDICT_WINDOW_S`], which floors one birth at [`VERDICT_BIRTH_DISP`]
    /// and is spent by it ([`Spine::note_keys`]).
    verdict: Option<Instant>,
}

impl Default for Spine {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Spine {
    /// A fresh spine at rest.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            momentum: TypingMomentum::new(),
            disp: 0.0,
            disp_peak: 0.0,
            phase: 0.0,
            at: None,
            slamming: false,
            mark: None,
            run: 0,
            best: 0,
            prev_birth: 0.0,
            key_rise: 0.0,
            driven_at: None,
            verdict: None,
        }
    }

    /// One non-delete printable typing advance (a forward glyph echo, a wrap, a coalesced echo).
    #[inline]
    pub fn advance(&mut self, now: Instant) {
        self.momentum.advance(now);
        self.mark = None;
    }

    /// Record `keys` typed advances on this frame at birth price `price`.
    #[inline]
    pub fn note_keys(&mut self, now: Instant, keys: u32, price: f32) {
        if keys == 0 {
            return;
        }
        self.verdict = None;
        if let Some(t) = self.driven_at
            && now.saturating_duration_since(t) < FLOW_FREEZE_DUR
        {
            return;
        }
        if price >= FLOW_KEY_DISP {
            self.run = self.run.saturating_add(keys);
            self.best = self.best.max(self.run);
        } else {
            self.run = 0;
        }
        self.key_rise = price - self.prev_birth;
        self.prev_birth = price;
    }

    /// One Backspace.
    #[inline]
    pub fn drain_delete(&mut self, now: Instant) {
        self.momentum.delete(now);
        self.run = 0;
        let here = self.birth_disp();
        let (disp, count) = match self.mark {
            Some(m) if now.saturating_duration_since(m.at) <= MEND_WINDOW_DUR => {
                (m.disp.max(here), m.count.saturating_add(1))
            }
            _ => (here, 1),
        };
        self.mark = Some(EraseMark {
            at: now,
            disp,
            count,
        });
    }

    /// One kill chord (`^W` / `^U` / `^K`, Alt-D, word backspace): un-earns like about two deletes.
    #[inline]
    pub fn drain_kill(&mut self, now: Instant) {
        self.momentum.kill(now);
        self.mark = None;
        self.run = 0;
    }

    /// Pin the raw metric to at least `floor` at `now`.
    #[inline]
    pub fn drive(&mut self, now: Instant, floor: f32) {
        let floor = if floor.is_finite() {
            floor.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if floor > 0.0 {
            self.driven_at = Some(now);
            if floor > self.momentum.value(now) {
                self.momentum.set_value(now, floor);
            }
        }
    }

    /// The grief gate, [`Spine::drive`]'s inverse.
    #[inline]
    pub const fn hush(&mut self, now: Instant) {
        self.momentum.set_value(now, 0.0);
        self.disp = 0.0;
        self.disp_peak = 0.0;
        self.slamming = false;
        self.mark = None;
        self.run = 0;
        self.prev_birth = 0.0;
        self.key_rise = 0.0;
        self.driven_at = None;
        self.verdict = None;
    }

    /// The one site of the gain: what the follower chases at `now`, the raw
    /// metric times [`METRIC_GAIN`], clamped to 0..1.
    #[inline]
    fn target(&self, now: Instant) -> f32 {
        (self.momentum.value(now) * METRIC_GAIN).clamp(0.0, 1.0)
    }

    /// Advance the display follower, resume memory and lay/flow clock to `now`.
    #[inline]
    pub fn update(&mut self, now: Instant) {
        // The hot-resume door closes by itself on the follower's tick, so a
        // session that goes dark re-earns its price.
        if let Some(at) = self.verdict
            && now.saturating_duration_since(at) > VERDICT_WINDOW_DUR
        {
            self.verdict = None;
        }
        let Some(prev) = self.at.replace(now) else {
            self.disp = self.target(now);
            self.disp_peak = self.disp;
            return;
        };
        let dt = now.saturating_duration_since(prev).as_secs_f32();
        if dt <= 0.0 {
            return;
        }
        let target = self.target(now);
        if target == 0.0 && self.disp == 0.0 && self.disp_peak == 0.0 {
            self.slamming = false;
            self.run = 0;
            self.prev_birth = 0.0;
            self.key_rise = 0.0;
            return;
        }
        let rise = target - self.disp;
        if rise > DISP_SLAM_RISE {
            self.slamming = true;
        } else if rise < DISP_SNAP_ZERO {
            self.slamming = false;
        }
        if rise != 0.0 {
            let inv_ease = if rise < 0.0 {
                1.0 / DISP_RELEASE_TAU
            } else if self.slamming {
                1.0 / DISP_SLAM_S
            } else {
                1.0 / DISP_ATTACK_S
            };
            self.disp += rise * (1.0 - crate::color_math::exp_f32(-dt * inv_ease));
        }
        if self.disp < DISP_SNAP_ZERO {
            self.disp = 0.0;
        }
        self.disp_peak = if self.disp >= self.disp_peak {
            self.disp
        } else {
            (self.disp_peak
                * crate::color_math::exp_f32(
                    -dt * (std::f32::consts::LN_2 / DISP_PEAK_HALF_LIFE_S),
                ))
            .max(self.disp)
        };
        if self.disp_peak < DISP_SNAP_ZERO {
            self.disp_peak = 0.0;
        }
        if self.disp == 0.0 && self.disp_peak == 0.0 {
            self.run = 0;
            self.prev_birth = 0.0;
            self.key_rise = 0.0;
        }
        if self.disp > 0.0 {
            let next_phase = self.phase + dt * PHASE_RATE * self.disp;
            self.phase = if (0.0..PHASE_RING).contains(&next_phase) {
                next_phase
            } else {
                next_phase - (next_phase * (1.0 / PHASE_RING)).floor() * PHASE_RING
            };
        }
    }

    /// Eased unit display spine.
    #[inline]
    #[must_use]
    pub const fn disp(&self) -> f32 {
        self.disp
    }

    /// The resume memory: the follower's recent peak, decaying on [`DISP_PEAK_HALF_LIFE_S`].
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn disp_peak(&self) -> f32 {
        self.disp_peak
    }

    /// The spine that prices a birth: [`Spine::disp`] floored at [`DISP_PEAK_FLOOR_SHARE`] of the
    /// decaying peak.
    #[inline]
    #[must_use]
    pub const fn birth_disp(&self) -> f32 {
        let verdict = if self.verdict.is_some() {
            VERDICT_BIRTH_DISP
        } else {
            0.0
        };
        self.disp
            .max(DISP_PEAK_FLOOR_SHARE * self.disp_peak)
            .max(verdict)
    }

    /// A command succeeded after a while: open the door for [`VERDICT_WINDOW_S`].
    #[inline]
    pub const fn note_verdict(&mut self, now: Instant) {
        self.verdict = Some(now);
    }

    /// Whether the door is open at `now` (diagnostic).
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn verdict_open(&self, now: Instant) -> bool {
        matches!(
            self.verdict,
            Some(at) if now.saturating_duration_since(at) <= VERDICT_WINDOW_DUR
        )
    }

    /// The pending [`EraseMark`], live or stale (diagnostic; nothing is armed on it).
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn erase_mark(&self) -> Option<EraseMark> {
        self.mark
    }

    /// The mend: the [`EraseMark`] if a typed key at `now` is a typo fix (within [`MEND_WINDOW_S`]
    /// of the last delete, at most [`MEND_MAX_DELETES`] in the run).
    #[inline]
    #[must_use]
    pub fn mend(&self, now: Instant) -> Option<EraseMark> {
        if let Some(m) = self.mark
            && m.count <= MEND_MAX_DELETES
            && now.saturating_duration_since(m.at) <= MEND_WINDOW_DUR
        {
            Some(m)
        } else {
            None
        }
    }

    /// The flow state: the run, its high-water mark and the open ramp ([`Flow`]).
    #[inline]
    #[must_use]
    pub const fn flow(&self) -> Flow {
        Flow {
            heat: flow_heat(self.run),
            combo: self.run,
            best: self.best,
        }
    }

    /// Unit fraction of the crisp-edge stretch bought by the last typed key.
    #[inline]
    #[must_use]
    pub const fn surge(&self) -> f32 {
        if self.slamming {
            return 1.0;
        }
        if self.key_rise < SURGE_MIN_RISE {
            return 0.0;
        }
        (self.key_rise * (1.0 / SURGE_FULL_RISE)).clamp(0.0, 1.0)
    }

    /// The lay/flow clock in sweeps, modulo [`PHASE_RING`].
    #[inline]
    #[must_use]
    pub const fn phase(&self) -> f32 {
        self.phase
    }

    /// The raw canonical metric at `now`: un-eased and un-gained, the number the kitty cursor's
    /// twin instance reads.
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub fn momentum(&self, now: Instant) -> f32 {
        self.momentum.value(now)
    }

    /// Idle is exactly zero (T6): true when follower, resume memory and metric are all at rest at
    /// `now`, an exact comparison that [`DISP_SNAP_ZERO`] makes reachable.
    #[must_use]
    #[cfg(test)]
    pub fn at_rest(&self, now: Instant) -> bool {
        self.disp == 0.0 && self.disp_peak == 0.0 && self.momentum.value(now) == 0.0
    }

    /// Back to rest (layout changes, style switches, `Engine::reset`).
    #[inline]
    pub const fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
#[path = "spine_tests.rs"]
mod tests;
