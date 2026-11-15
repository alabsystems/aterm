// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE POLICY KEYS a harness decides an agent's turn ends under: how far Autopilot carries a
//! session's work ([`Autopilot`]), what Cruise's continuation types ([`PolicyConfig`],
//! [`CONTINUE_TEXT`]), and the timings the decider waits by ([`TurnEndTiming`]). A harness reads
//! them from its own configuration; nothing here reads a file, the environment or a clock.

use std::time::Duration;

use crate::cruise_carry::CarryTiming;

/// CRUISE'S CONTINUATION, the default [`PolicyConfig::continue_text`] (Autopilot Cruise).
pub const CONTINUE_TEXT: &str = concat!(
    crate::mark!(),
    " Autopilot is Cruise: keep going. Finish, verify and land the work, then find the next \
     improvement. Decide and record why for later AI review; defer unavailable work and \
     advance what can proceed. ",
    crate::never_a_need!(),
    " ",
    crate::restart_hint!(),
);

/// How far the harness carries a session's work at its turn ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Autopilot {
    /// Keep going: every turn end with work left is continued, and done is never the end.
    #[default]
    Cruise,
    /// Carry the work until it is landed, then stop.
    Land,
    /// Type nothing at a turn end.
    Off,
}

impl Autopilot {
    /// Every mode, in the order the chip cycles them.
    pub const ALL: [Autopilot; 3] = [Autopilot::Cruise, Autopilot::Land, Autopilot::Off];

    /// The mode's word as the table, the session's `meta` word and the chip spell it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Autopilot::Cruise => "cruise",
            Autopilot::Land => "land",
            Autopilot::Off => "off",
        }
    }

    /// A mode from its word ([`Self::word`]) or another name for it (`forever`, `infinity`:
    /// [`Autopilot::Cruise`]; `wrap-up`, `wrapup`, `wrap`: [`Autopilot::Land`]), trimmed, in any
    /// case.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "cruise" | "forever" | "infinity" => Some(Autopilot::Cruise),
            "land" | "wrap-up" | "wrapup" | "wrap" => Some(Autopilot::Land),
            "off" => Some(Autopilot::Off),
            _ => None,
        }
    }

    /// How much power the mode gives, least first: [`Autopilot::Off`] `0`, [`Autopilot::Land`]
    /// `1`, [`Autopilot::Cruise`] `2`. A configuration that only limits never raises it.
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Autopilot::Off => 0,
            Autopilot::Land => 1,
            Autopilot::Cruise => 2,
        }
    }
}

/// The policy keys of a harness's configuration that the turn-end decider reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyConfig {
    /// The [`Autopilot`] of a session with no `autopilot` word of its own: [`Autopilot::Cruise`]
    /// by default.
    pub autopilot: Autopilot,
    /// What a continuation types when the agent offers no suggestion of its own:
    /// [`CONTINUE_TEXT`] by default.
    pub continue_text: String,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            autopilot: Autopilot::Cruise,
            continue_text: CONTINUE_TEXT.to_string(),
        }
    }
}

/// The busy work that makes a turn real work by default ([`TurnEndTiming::min_work`]).
pub const MIN_WORK: Duration = Duration::from_secs(2 * 60);

/// A minute, as the shipped harness runs the time to a wall's reset: the reset sits where its
/// words name it ([`TurnEndTiming::reset_minute`]).
pub const RESET_MINUTE: Duration = Duration::from_secs(60);

/// The decider's timings; tests shorten them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEndTiming {
    /// The busy time that makes a turn real work ([`MIN_WORK`]).
    pub min_work: Duration,
    /// The wait before the next act after the first short turn in a row.
    pub short_backoff: Duration,
    /// The longest wait the short-turn ladder doubles to.
    pub short_backoff_max: Duration,
    /// The ladder an overload's, a server error's and an over-long reply's retries wait on, a
    /// rung per try.
    pub retry_backoff: Vec<Duration>,
    /// The ladder a network error's and a cut-off reply's retries wait on, a rung per try.
    pub net_backoff: Vec<Duration>,
    /// An outage wall's first blind try comes this long after the wall, each later one twice as
    /// long after its own wall's re-read, up to [`Self::outage_cap`].
    pub outage_first: Duration,
    /// The longest gap between an outage wall's blind tries, the period a wait the host
    /// measures down decides again at, and how long a measured outage holds what the harness
    /// would type where no wall stands, or the try of a wall only the measure makes the
    /// connection's.
    pub outage_cap: Duration,
    /// The busy time after an act that counts as the agent working past the wall it was typed
    /// at.
    pub progress: Duration,
    /// The grace a resume waits past a wall's reset.
    pub reset_grace: Duration,
    /// The most a session's resume is spread past [`Self::reset_grace`], so sessions at one
    /// reset do not all resume at once.
    pub reset_jitter: Duration,
    /// How long a minute of the time from a wall's read to the reset its words name lasts on the
    /// harness's clock ([`Self::reset_placed`]): [`RESET_MINUTE`], unless a development build
    /// shortens it ([`Self::reset_span_scaled`]).
    pub reset_minute: Duration,
    /// The grace past its reset for a limit the program continues by itself, before the harness
    /// resumes it.
    pub auto_resume_grace: Duration,
    /// The retry floor a usage limit's resumes wait on, a rung per try.
    pub limit_backoff: Vec<Duration>,
    /// The ladder a wall that no reset ends and none names (an entitlement) waits on, a rung per
    /// try.
    pub entitlement_backoff: Vec<Duration>,
    /// The ladder a refused request's retries wait on, a rung per try.
    pub refused_backoff: Vec<Duration>,
    /// The window a rule's and an account move's caps count their acts in.
    pub window: Duration,
    /// How long an act the harness typed may take to be taken up before it is read as not
    /// taken.
    pub take_within: Duration,
    /// The ladder an agent's restarts wait on, a rung per restart.
    pub restart_backoff: Vec<Duration>,
    /// How long a hold on usage credits stands where no reset could be placed.
    pub credits_bound: Duration,
}

impl Default for TurnEndTiming {
    fn default() -> Self {
        let min = |m: u64| Duration::from_secs(m * 60);
        Self {
            min_work: MIN_WORK,
            short_backoff: min(2),
            short_backoff_max: min(60),
            retry_backoff: vec![min(1), min(5), min(15), min(30), min(60)],
            net_backoff: vec![min(1), min(2), min(5)],
            outage_first: min(2),
            outage_cap: min(60),
            progress: min(5),
            reset_grace: Duration::from_secs(150),
            reset_jitter: Duration::from_secs(60),
            reset_minute: RESET_MINUTE,
            auto_resume_grace: min(10),
            limit_backoff: vec![min(10), min(30)],
            entitlement_backoff: vec![min(30), min(120), min(360)],
            refused_backoff: vec![min(1), min(30), min(120), min(360)],
            window: min(60),
            take_within: Duration::from_secs(30),
            restart_backoff: vec![Duration::from_secs(10), min(1), min(5), min(10)],
            credits_bound: min(300),
        }
    }
}

impl TurnEndTiming {
    /// The same timing, its short back-off ladder and an outage wall's ladder scaled by `factor`.
    #[must_use]
    pub fn ladder_scaled(mut self, factor: f64) -> Self {
        self.short_backoff = self.short_backoff.mul_f64(factor);
        self.short_backoff_max = self.short_backoff_max.mul_f64(factor);
        self.outage_first = self.outage_first.mul_f64(factor);
        self.outage_cap = self.outage_cap.mul_f64(factor);
        self
    }

    /// The back-off before the next act after a streak of `streak` short turns:
    /// [`Self::short_backoff`] after the first, doubled at each turn past it up to
    /// [`Self::short_backoff_max`]; `None` before any ([`CarryTiming::short_wait_at`]).
    #[must_use]
    pub fn short_wait_at(&self, streak: u32) -> Option<Duration> {
        self.carry().short_wait_at(streak)
    }

    /// The timings Cruise's carry ladder runs on: this policy's short-turn ladder and what makes a
    /// turn real work.
    #[must_use]
    pub const fn carry(&self) -> CarryTiming {
        CarryTiming {
            min_work: self.min_work,
            short_backoff: self.short_backoff,
            short_backoff_max: self.short_backoff_max,
        }
    }

    /// The same timing, the wait past a wall's reset ([`Self::reset_grace`] and
    /// [`Self::reset_jitter`]) scaled by `factor`.
    #[must_use]
    pub fn reset_scaled(mut self, factor: f64) -> Self {
        self.reset_grace = self.reset_grace.mul_f64(factor);
        self.reset_jitter = self.reset_jitter.mul_f64(factor);
        self
    }

    /// The same timing, the time from a wall's read to the reset its words name
    /// ([`Self::reset_minute`]) scaled by `factor`.
    #[must_use]
    pub fn reset_span_scaled(mut self, factor: f64) -> Self {
        self.reset_minute = self.reset_minute.mul_f64(factor);
        self
    }

    /// Whether a wall's reset sits where its words name it ([`Self::reset_minute`] is
    /// [`RESET_MINUTE`]), as in every shipped build.
    #[must_use]
    pub fn reset_as_named(&self) -> bool {
        self.reset_minute == RESET_MINUTE
    }

    /// `at`, the reset a wall's words name in unix seconds, read at `now_ms` in unix
    /// milliseconds, where the harness waits to it: the time from the read to `at` lasts
    /// [`Self::reset_minute`] a minute, and the reset falls on the first whole second at or after
    /// that, so it never comes before the scaled time. A reset as named
    /// ([`Self::reset_as_named`]), one at or before the read, and one too far off to scale stay
    /// at `at`.
    #[must_use]
    pub fn reset_placed(&self, at: i64, now_ms: i64) -> i64 {
        if self.reset_as_named() {
            return at;
        }
        scaled_reset(at, now_ms, self.reset_minute).unwrap_or(at)
    }
}

/// [`TurnEndTiming::reset_placed`]'s arithmetic: the first whole second at or after `now_ms` plus
/// the span from it to `at` at `minute` a minute; `None` for a reset at or before the read, or one
/// too far off to scale.
fn scaled_reset(at: i64, now_ms: i64, minute: Duration) -> Option<i64> {
    let span_ms = i128::from(at)
        .checked_mul(1000)?
        .checked_sub(i128::from(now_ms))?;
    if span_ms <= 0 {
        return None;
    }
    // The read and the scaled span, both in milliseconds times a minute's nanoseconds, so one
    // division by a second's worth, rounded up, places the reset.
    let named = i128::try_from(RESET_MINUTE.as_nanos()).ok()?;
    let rate = i128::try_from(minute.as_nanos()).ok()?;
    let placed = i128::from(now_ms)
        .checked_mul(named)?
        .checked_add(span_ms.checked_mul(rate)?)?;
    let second = named.checked_mul(1000)?;
    i64::try_from(placed.div_euclid(second) + i128::from(placed.rem_euclid(second) != 0)).ok()
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
