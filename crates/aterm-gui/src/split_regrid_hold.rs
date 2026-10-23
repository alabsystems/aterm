// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Split take-back hold: a pre-existing pane shrunk by a split does not grow
//! back to its pre-split grid until the shrink has stood [`SPLIT_REGRID_QUIET`],
//! ensuring the collapse sends a distinct `SIGWINCH` after the shrink is read.
//! Newborn split panes are spawned at full-window size before drawing, so
//! closing the original pane grows the newborn immediately.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::{App, WindowId};

/// Quiet window before a split collapse may grow a pane back to its pre-split grid.
pub(crate) const SPLIT_REGRID_QUIET: Duration = Duration::from_millis(250);

/// One pane's take-back hold, keyed by its session id in
/// [`SplitRegridHolds`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SplitRegridHold {
    before: (u16, u16),
    until: Instant,
    withheld: bool,
}

/// The live take-back holds, by session id.
#[derive(Debug, Default)]
pub(crate) struct SplitRegridHolds {
    holds: HashMap<u64, SplitRegridHold>,
}

/// What a resize pass does with one pane's target grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HoldVerdict {
    /// Resize as usual.
    Apply,
    /// Withhold: the target is the pane's pre-split grid inside the quiet.
    Withhold,
}

/// Why a resize pass is running, as far as the hold is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelayCause {
    /// A pane of the tab collapsed (`apply_close_outcome`): the one pass whose
    /// grow back to the pre-split grid is the take-back.
    Collapse,
    /// Anything else: a drag, a re-grid, a zoom, a divider, a redraw's pass.
    Other,
}

impl SplitRegridHolds {
    /// Records that a split just re-gridded `session` away from `before`.
    pub(crate) fn arm(&mut self, session: u64, before: (u16, u16), now: Instant) {
        self.holds.insert(
            session,
            SplitRegridHold {
                before,
                until: now + SPLIT_REGRID_QUIET,
                withheld: false,
            },
        );
    }

    /// Judges whether sizing `session` to `target` at `now` for `cause` should
    /// be withheld (collapse back to `before` inside the quiet) or applied.
    pub(crate) fn judge(
        &mut self,
        session: u64,
        target: (u16, u16),
        cause: RelayCause,
        now: Instant,
    ) -> HoldVerdict {
        let Some(hold) = self.holds.get_mut(&session) else {
            return HoldVerdict::Apply;
        };
        let take_back = cause == RelayCause::Collapse || hold.withheld;
        if take_back && target == hold.before && now < hold.until {
            hold.withheld = true;
            return HoldVerdict::Withhold;
        }
        self.holds.remove(&session);
        HoldVerdict::Apply
    }

    /// No hold stands (the resize pass then reads no clock).
    pub(crate) fn is_empty(&self) -> bool {
        self.holds.is_empty()
    }

    /// The session left: its hold goes with it.
    pub(crate) fn forget(&mut self, session: u64) {
        self.holds.remove(&session);
    }

    /// Earliest expiry among holds that withheld a grow.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.holds
            .values()
            .filter(|h| h.withheld)
            .map(|h| h.until)
            .min()
    }

    /// Drops expired holds at `now` and returns session ids with an owed grow.
    pub(crate) fn take_due(&mut self, now: Instant) -> Vec<u64> {
        let mut owed: Vec<u64> = Vec::new();
        self.holds.retain(|session, h| {
            if now < h.until {
                return true;
            }
            if h.withheld {
                owed.push(*session);
            }
            false
        });
        owed.sort_unstable();
        owed
    }

    #[cfg(test)]
    pub(crate) fn get(&self, session: u64) -> Option<SplitRegridHold> {
        self.holds.get(&session).copied()
    }

    /// Test seam: move every hold's quiet `by` into the past.
    #[cfg(test)]
    pub(crate) fn age(&mut self, by: Duration) {
        for h in self.holds.values_mut() {
            h.until = h.until.checked_sub(by).unwrap_or(h.until);
        }
    }
}

impl App {
    /// Returns `(session, (rows, cols))` for every terminal pane in `wid`'s active tab.
    pub(crate) fn active_tab_pane_grids(&self, wid: WindowId) -> Vec<(u64, (u16, u16))> {
        let Some(tab) = self.windows.get(&wid).and_then(|ws| ws.tab_set.active()) else {
            return Vec::new();
        };
        tab.root
            .leaves()
            .into_iter()
            .filter_map(|view| {
                self.view_store
                    .get(view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
            })
            .filter_map(|session| {
                let s = self.pool.get(session)?;
                let term = crate::term_lock(&s.term);
                Some((session, (term.rows(), term.cols())))
            })
            .collect()
    }

    /// Arms a take-back hold on every pane in `before` (except `newborn`) whose grid changed.
    pub(crate) fn arm_split_regrid_holds(
        &mut self,
        wid: WindowId,
        before: &[(u64, (u16, u16))],
        newborn: u64,
        now: Instant,
    ) {
        let after: HashMap<u64, (u16, u16)> = self.active_tab_pane_grids(wid).into_iter().collect();
        for &(session, grid) in before {
            if session != newborn
                && after
                    .get(&session)
                    .is_some_and(|now_grid| *now_grid != grid)
            {
                self.split_regrid_holds.arm(session, grid, now);
            }
        }
    }

    /// The one wake a withheld take-back owes (`None` when nothing is held).
    pub(crate) fn split_regrid_hold_deadline(&self) -> Option<Instant> {
        self.split_regrid_holds.deadline()
    }

    /// Re-grids and requests redraw for every window with a withheld take-back due at `now`.
    pub(crate) fn service_split_regrid_holds(&mut self, now: Instant) {
        let sessions = self.split_regrid_holds.take_due(now);
        if sessions.is_empty() {
            return;
        }
        let mut owed: Vec<WindowId> = Vec::new();
        for session in sessions {
            for wid in self.windows.keys().copied() {
                if !owed.contains(&wid) && self.window_contains_session(wid, session) {
                    owed.push(wid);
                }
            }
        }
        owed.sort();
        for wid in owed {
            self.resize_panes(wid);
            if let Some(win) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                win.request_redraw();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: RelayCause = RelayCause::Collapse;
    const O: RelayCause = RelayCause::Other;

    #[test]
    fn a_collapse_take_back_inside_the_quiet_is_withheld_and_owes_one_wake() {
        let mut holds = SplitRegridHolds::default();
        let t0 = Instant::now();
        holds.arm(7, (24, 80), t0);
        assert_eq!(holds.deadline(), None, "an unconsulted hold owes no wake");
        assert_eq!(holds.judge(7, (11, 80), C, t0), HoldVerdict::Apply);
        assert_eq!(holds.get(7), None);

        holds.arm(7, (24, 80), t0);
        assert_eq!(holds.judge(7, (24, 80), C, t0), HoldVerdict::Withhold);
        assert_eq!(holds.judge(7, (24, 80), O, t0), HoldVerdict::Withhold);
        assert_eq!(holds.deadline(), Some(t0 + SPLIT_REGRID_QUIET));
        assert!(holds.take_due(t0).is_empty(), "not due before the quiet");
        assert_eq!(holds.take_due(t0 + SPLIT_REGRID_QUIET), vec![7]);
        assert_eq!(holds.deadline(), None);
    }

    #[test]
    fn no_collapse_no_hold_even_on_the_pre_split_grid() {
        let mut holds = SplitRegridHolds::default();
        let t0 = Instant::now();
        holds.arm(7, (24, 80), t0);
        assert_eq!(holds.judge(7, (24, 80), O, t0), HoldVerdict::Apply);
        assert_eq!(holds.get(7), None, "a non-collapse pass ends the hold");
        assert_eq!(holds.deadline(), None);
    }

    #[test]
    fn any_other_target_or_an_ended_quiet_applies_at_once() {
        let mut holds = SplitRegridHolds::default();
        let t0 = Instant::now();
        holds.arm(7, (24, 80), t0);
        assert_eq!(holds.judge(7, (30, 100), C, t0), HoldVerdict::Apply);
        assert_eq!(holds.get(7), None, "another target ends the hold");

        holds.arm(7, (24, 80), t0);
        assert_eq!(holds.judge(7, (24, 80), C, t0), HoldVerdict::Withhold);
        assert_eq!(
            holds.judge(7, (30, 100), O, t0),
            HoldVerdict::Apply,
            "a withheld take-back yields to any other target"
        );
        assert_eq!(holds.get(7), None);

        holds.arm(7, (24, 80), t0);
        assert_eq!(
            holds.judge(7, (24, 80), C, t0 + SPLIT_REGRID_QUIET),
            HoldVerdict::Apply
        );
        assert_eq!(holds.get(7), None);

        holds.arm(7, (24, 80), t0);
        assert_eq!(holds.judge(9, (24, 80), C, t0), HoldVerdict::Apply);
        holds.forget(7);
        assert_eq!(holds.get(7), None);
        assert!(holds.take_due(t0 + SPLIT_REGRID_QUIET).is_empty());
    }
}
