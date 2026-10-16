// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AUTOPILOT'S TIMER: Cruise until a time, then Land
//! (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.3). A timer only ever
//! takes power away.
//!
//! The stored pair — the word `cruise` and its `land_at`
//! (`SessionMeta::land_at`) — is the truth; this index only says WHEN to
//! look, never WHAT to write (§9.3.6, item 3): the App folds its earliest
//! instant as ONE deadline (`metrics::DeadlineOwner::AutopilotTimer`), and
//! the fire is a compare-and-set on the stored pair.
//!
//! THE SEAM (design §9.6.0): the index is empty and nothing arms it. The
//! `until=` parser, the index, the fire and the re-derivation on a moved
//! wall clock are slice B's (§9.6.2); until it lands no session is timed, no
//! deadline is folded and no clock is read here.

use std::time::Instant;

use crate::App;
use crate::claude_lights::LandAtGlass;

/// The App's index of armed timers, by session local id: empty.
#[derive(Debug, Default)]
pub(crate) struct AutopilotTimers {}

impl AutopilotTimers {
    /// Whether no timer is armed: the frame then looks nothing up here
    /// (`App::chip_glass`).
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        true
    }

    /// Session `local_id`'s armed timer as the chip shows it, or `None`.
    #[must_use]
    pub(crate) fn glass(&self, local_id: u64) -> Option<LandAtGlass> {
        let _ = local_id;
        None
    }

    /// Session `local_id` is retired: its entry goes with it, so no past
    /// instant is folded again.
    pub(crate) fn forget(&mut self, local_id: u64) {
        let _ = local_id;
    }
}

impl App {
    /// The earliest instant an armed timer is due, or `None` with none
    /// armed: then nothing is folded.
    #[must_use]
    pub(crate) fn autopilot_timer_deadline(&self) -> Option<Instant> {
        None
    }

    /// Fire every due timer, each a compare-and-set on its session's stored
    /// pair (§9.3.6, item 5). With none armed, nothing is read.
    pub(crate) fn expire_autopilot_timers(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing is armed, so nothing is due, shown or kept.
    #[test]
    fn no_timer_is_armed_and_nothing_is_due() {
        let mut timers = AutopilotTimers::default();
        assert!(timers.is_empty());
        assert_eq!(timers.glass(1), None);
        timers.forget(1);
        let mut app = App::headless_for_test();
        assert_eq!(app.autopilot_timer_deadline(), None);
        app.expire_autopilot_timers();
        assert_eq!(app.autopilot_timer_deadline(), None);
    }
}
