// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CRUISE NEVER PARKS AT A BACKGROUND-WORK BREAK.
//!
//! A turn that ends while the agent's own background work runs (a dynamic workflow, a background
//! agent, a shell or a monitor) draws Claude Code's `Waiting for 1 dynamic workflow` row or a
//! footer pill that counts the work, which the phase reader reads busy: the turn-end policy never
//! sees a point there ([`super::turn_end::decide_turn_end`] judges idle points alone), and the
//! occupancy door under its default `observe` types nothing. Under Cruise that break is a turn
//! end like any other: [`BACKGROUND_CARRY`] tells the agent to check on that work and keep going,
//! paced by the policy's own short-turn ladder ([`BackgroundCarries`]) so an agent that has
//! nothing else to do is asked again at 2, 4, 8 … and at most every 60 minutes, never left. Real
//! work is the busy time of the turn a break ends, judged once at that break, never wall time
//! (since the last carry, or a break stood), so a short turn of the program's own wake (a
//! background task's notification) keeps the ladder.

use std::time::{Duration, Instant};

use super::turn_end::TurnEndTiming;
use crate::harness::harness_mark;

/// The ledger's and the journal's name for the carry typed at a break of the agent's own
/// background work under Cruise ([`BACKGROUND_CARRY`]).
pub const RULE_BACKGROUND: &str = "continue-background@v1";

/// The ledger's name for the books of [`RULE_BACKGROUND`]'s carries started again in memory where
/// they held a carry ([`BackgroundCarries`]: a turn of real work, someone else's message, a turn
/// whose work went unread, a person's gesture after a loop before this one's last carry): a later
/// loop's replay of the ladder starts after it, as after a judged point.
pub const RULE_BACKGROUND_RESET: &str = "continue-background-reset@v1";

/// The carry's words: the work runs on, so check on it and go on with everything else.
pub const BACKGROUND_CARRY: &str = concat!(
    harness_mark!(),
    " Autopilot is Cruise: your turn ended while your own background work runs. Check on that \
     work, then keep going with everything else in hand; never wait idle for it. Make the next \
     safe decision, record it for later review, and carry on. Defer unavailable work with its \
     reason and advance another task; do not turn routine failures into requests to the person. ",
    super::go_ahead::never_a_need!()
);

/// [`BACKGROUND_CARRY`] with the policy's standing `rules`, fitted as every line the turn-end
/// policy types is.
#[must_use]
pub fn carry_words(rules: Option<&str>) -> String {
    super::turn_end::act_words(BACKGROUND_CARRY, &[], rules)
}

/// The forward-recovery instruction fitted to the same limits as a normal carry.
pub(crate) fn recovery_words(rules: Option<&str>) -> String {
    super::turn_end::act_words(super::turn_end::FORWARD_RECOVERY, &[], rules)
}

/// CRUISE'S CARRIES AT BACKGROUND BREAKS ([`RULE_BACKGROUND`]): when the last went and how many in a
/// row the agent answered short, which the next one waits on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackgroundCarries {
    /// When the last carry was typed: the ladder's back-off runs from it.
    last: Option<Instant>,
    /// Carries in a row whose answers ran short of [`TurnEndTiming::min_work`].
    streak: u32,
}

impl BackgroundCarries {
    /// When the next carry is due at the break that began at `began`: at once (the break's settle
    /// is the caller's) with none typed since the books were last started, else the last carry
    /// plus the short-turn back-off of the streak ([`TurnEndTiming::short_wait_at`]: 2 min,
    /// doubled each time, to 60). A TURN IS JUDGED ONCE, at the settled break that ends it
    /// ([`Self::ended`]): when this break began, read again or read afresh after the loop came
    /// back to it, is no work of any turn.
    #[must_use]
    pub fn due(&self, timing: &TurnEndTiming, began: Instant) -> Instant {
        self.due_within(timing, began, Duration::MAX)
    }

    /// [`Self::due`] with the back-off at most `cap`: the owner's standing go-ahead in the
    /// carry's place waits no longer from the last carry than a point's waits from the last act
    /// ([`super::turn_end::GO_AHEAD_WAIT`]).
    #[must_use]
    pub fn due_within(&self, timing: &TurnEndTiming, began: Instant, cap: Duration) -> Instant {
        self.last.map_or(began, |last| {
            let wait = timing
                .short_wait_at(self.streak)
                .unwrap_or(Duration::ZERO)
                .min(cap);
            last.checked_add(wait).unwrap_or(last)
        })
    }

    /// THE LADDER A LOOP BEFORE THIS ONE LEFT: its last carry typed at `last`, the `streak`-th in a
    /// row ([`Self::replayed`]), so a restart's next carry waits as that loop's would have.
    #[must_use]
    pub const fn carried(last: Instant, streak: u32) -> Self {
        Self {
            last: Some(last),
            streak,
        }
    }

    /// THE BOOKS' LADDER REPLAYED AS THE LIVE LADDER TYPED IT, at `now_ms`, from the carries
    /// `typed` (unix ms and the streak each row names, oldest first, none before a row that
    /// started the books again: [`crate::supervise::approvals::background_carries_in`]): the
    /// newest carry's time and its streak, which [`Self::carried`] takes up. A carry whose row
    /// names its streak is at that streak, the live ladder's, books started again in memory alone
    /// included. One written before rows named it is one more in the run unless it came on books
    /// started again where no row says so ([`Self::next_in_run`]). `None` with no carry, or where
    /// the newest has lapsed ([`Self::lapsed`]): a restart there takes no ladder.
    #[must_use]
    pub fn replayed(
        timing: &TurnEndTiming,
        typed: &[(i64, Option<u32>)],
        now_ms: i64,
    ) -> Option<(i64, u32)> {
        let mut run: Option<(i64, u32)> = None;
        for &(at, named) in typed {
            run = Some(match (named, run) {
                (Some(streak), _) => (at, streak),
                (None, Some((last, streak))) if Self::next_in_run(timing, streak, at - last) => {
                    (at, streak.saturating_add(1))
                }
                (None, _) => (at, 1),
            });
        }
        run.filter(|&(last, streak)| !Self::lapsed(timing, streak, now_ms - last))
    }

    /// Whether a carry `gap_ms` after the one before it, the `streak`-th in a row, is the next of
    /// their run, for a row that names no streak. THE LIVE LADDER TYPES NONE SOONER THAN THE
    /// STREAK'S RUNG, so one closer came on books started again in memory alone (a turn of real
    /// work, someone else's message at a break); an eighth of the rung is the stamps' slack (a row
    /// is stamped after its typing, on a wall clock that slews). And one past the rung by the lapse
    /// ([`Self::lapsed`]) came where a restart takes no ladder. Books started again after a turn
    /// of real work longer than the rung show in no gap, which is why rows name their streak.
    fn next_in_run(timing: &TurnEndTiming, streak: u32, gap_ms: i64) -> bool {
        let rung = Self::rung(timing, streak);
        let gap = Duration::from_millis(u64::try_from(gap_ms).unwrap_or(0));
        gap >= rung.saturating_sub(rung / 8) && !Self::lapsed(timing, streak, gap_ms)
    }

    /// Whether the ladder lapsed `since_ms` after a carry, the `streak`-th in a row: THE LADDER'S
    /// CAP PAST THE RUNG that streak waits. A session at its break is carried at the rung, so what
    /// kept it that long (real work, a person, no loop running, a hold) is read as its books
    /// started again. Measured from the rung, a ladder on its cap never lapses at the carry due.
    fn lapsed(timing: &TurnEndTiming, streak: u32, since_ms: i64) -> bool {
        let since = Duration::from_millis(u64::try_from(since_ms).unwrap_or(0));
        since >= Self::rung(timing, streak).saturating_add(timing.short_backoff_max)
    }

    /// The wait after `streak` carries in a row ([`TurnEndTiming::short_wait_at`]).
    fn rung(timing: &TurnEndTiming, streak: u32) -> Duration {
        timing.short_wait_at(streak).unwrap_or(Duration::ZERO)
    }

    /// A carry was typed at `now`: one more short answer in a row, or the first since the books
    /// were last started.
    pub fn typed(&mut self, now: Instant) {
        self.streak = if self.last.is_some() {
            self.streak.saturating_add(1)
        } else {
            1
        };
        self.last = Some(now);
    }

    /// REAL WORK IS STICKY: a turn that was busy `worked` ended at a settled break, and a turn of
    /// real work ([`TurnEndTiming::min_work`]) starts the books again there, so a short wake or a
    /// held carry after it takes nothing back. A short one leaves them as they stand.
    pub fn ended(&mut self, timing: &TurnEndTiming, worked: Duration) {
        if worked >= timing.min_work {
            self.reset();
        }
    }

    /// A person's turn, someone else's message, a turn whose work went unread or a judged point:
    /// the next break is carried at its settle again.
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
#[path = "cruise_carry_tests.rs"]
mod tests;
