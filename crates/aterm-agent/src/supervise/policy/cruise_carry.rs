// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CRUISE NEVER PARKS AT A BACKGROUND-WORK BREAK (owner ruling 2026-10-26).
//!
//! A turn that ends while the agent's own background work runs (a dynamic workflow, a background
//! agent, a shell or a monitor) draws Claude Code's `Waiting for 1 dynamic workflow` row or a
//! footer pill that counts the work, which the phase reader reads busy: the turn-end policy never
//! sees a point there ([`super::turn_end::decide_turn_end`] judges idle points alone), and the
//! occupancy door under its default `observe` types nothing. Under Cruise that break is a turn
//! end like any other: [`BACKGROUND_CARRY`] tells the agent to check on that work and keep going,
//! paced by the policy's own short-turn ladder ([`BackgroundCarries`]) so an agent that has
//! nothing else to do is asked again at 2, 4, 8 … and at most every 60 minutes, never left.

use std::time::{Duration, Instant};

use super::turn_end::TurnEndTiming;
use crate::harness::harness_mark;

/// The ledger's and the journal's name for the carry typed at a break of the agent's own
/// background work under Cruise ([`BACKGROUND_CARRY`]).
pub const RULE_BACKGROUND: &str = "continue-background@v1";

/// The carry's words: the work runs on, so check on it and go on with everything else.
pub const BACKGROUND_CARRY: &str = concat!(
    harness_mark!(),
    " Autopilot is Cruise: your turn ended while your own background work runs. Check on that \
     work, then keep going with everything else in hand; never wait idle for it. Put what only \
     a person can give on a line starting NEEDS YOU: and carry on."
);

/// [`BACKGROUND_CARRY`] with the policy's standing `rules`, fitted as every line the turn-end
/// policy types is.
#[must_use]
pub fn carry_words(rules: Option<&str>) -> String {
    super::turn_end::act_words(BACKGROUND_CARRY, &[], rules)
}

/// CRUISE'S CARRIES AT BACKGROUND BREAKS ([`RULE_BACKGROUND`]): when the last went and how many in a
/// row the agent answered short, which the next one waits on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackgroundCarries {
    /// When the last carry was typed.
    last: Option<Instant>,
    /// Carries in a row whose answers ran short of [`TurnEndTiming::min_work`].
    streak: u32,
}

impl BackgroundCarries {
    /// Whether the break that began at `began` answers the last carry with real work: the agent
    /// worked at least [`TurnEndTiming::min_work`] between that carry and this break, or no carry
    /// went since the streak was last reset.
    fn fresh(&self, timing: &TurnEndTiming, began: Instant) -> bool {
        self.last
            .is_none_or(|last| began.saturating_duration_since(last) >= timing.min_work)
    }

    /// When the next carry is due at the break that began at `began`: at once (the break's settle
    /// is the caller's) after real work or none typed yet, else the last carry plus the short-turn
    /// back-off of the streak ([`TurnEndTiming::short_wait_at`]: 2 min, doubled each time, to 60).
    #[must_use]
    pub fn due(&self, timing: &TurnEndTiming, began: Instant) -> Instant {
        match self.last {
            Some(last) if !self.fresh(timing, began) => {
                let wait = timing.short_wait_at(self.streak).unwrap_or(Duration::ZERO);
                last.checked_add(wait).unwrap_or(last)
            }
            _ => began,
        }
    }

    /// A carry was typed at `now` at the break that began at `began`.
    pub fn typed(&mut self, timing: &TurnEndTiming, began: Instant, now: Instant) {
        self.streak = if self.fresh(timing, began) {
            1
        } else {
            self.streak.saturating_add(1)
        };
        self.last = Some(now);
    }

    /// Someone else's turn (a person's, the program's own wake) or a judged point: the next break
    /// is carried at its settle again.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Carries in a row the agent answered short.
    #[must_use]
    pub const fn streak(&self) -> u32 {
        self.streak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn short_answers_back_off_on_the_policys_ladder_and_real_work_resets_it() {
        let timing = TurnEndTiming::default();
        let t0 = Instant::now();
        let mut c = BackgroundCarries::default();
        assert_eq!(c.due(&timing, t0), t0, "the first break is carried at once");
        c.typed(&timing, t0, t0);
        let mut last = t0;
        for (n, wait) in [2, 4, 8, 16, 32, 60, 60].into_iter().enumerate() {
            // The agent answered in 20 s and its turn ended on the work again.
            let began = last + Duration::from_secs(20);
            let due = c.due(&timing, began);
            assert_eq!(due, last + wait * MIN, "carry {} waits {wait} min", n + 2);
            c.typed(&timing, began, due);
            last = due;
        }
        assert_eq!(c.streak(), 8);
        // The agent worked past min_work on the last carry: the next break goes at once.
        let began = last + timing.min_work;
        assert_eq!(c.due(&timing, began), began);
        c.typed(&timing, began, began);
        assert_eq!(c.streak(), 1, "real work starts the ladder again");
        c.reset();
        let later = began + Duration::from_secs(5);
        assert_eq!(
            c.due(&timing, later),
            later,
            "someone else's turn resets it"
        );
    }
}
