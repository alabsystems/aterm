// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The split's take-back hold: a pane a split shrank does not grow back to its
//! pre-split grid until the shrink has stood [`SPLIT_REGRID_QUIET`].
//!
//! A split that closes at once (a probe that opens a scratch pane and tears it
//! down, a shell that exits at once) would re-grid its sibling N -> ~N/2 -> N
//! within milliseconds. Claude Code repaints only when its `SIGWINCH` handler
//! reads a changed size, so after the net-zero pair it never repaints, while the
//! alternate screen's shrink has already demoted about half its rows (the
//! engine's resize undo repairs the pair only when no output lands between its
//! halves).
//!
//! The split's shrink stays immediate (the new pane needs the room at once);
//! only the take-back waits: the collapse of a pane the split made
//! (`apply_close_outcome`: Cmd-W, a control `close`, an exited shell) growing a
//! pane back to exactly its pre-split grid, until the shrink has stood the
//! quiet, so the grow is a second, distinct `SIGWINCH`. Only panes that existed
//! before the split are held: the newborn is spawned at a size its program never
//! drew, so closing the original pane grows it at once.
//!
//! Who is held: only the panes that existed BEFORE the split. The newborn
//! pane is spawned at the window's full grid, a size its program never drew
//! (`App::open_off_loop`: "its landing sizes every pane"), so its resize away
//! from it is no shrink worth holding: closing the ORIGINAL pane at once grows
//! the newborn to the full rect straight away.
//!
//! Meanwhile a control `close` answers, and a `size`, `screen` or `status` read
//! reports, the split grid; an agent that needs the grown grid waits
//! [`SPLIT_REGRID_QUIET`] (or `await`s the resize).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::{App, WindowId};

/// How long a split's shrink stands before a collapse may grow the same pane
/// back to its pre-split grid: long enough for a program's event loop to read
/// the shrink's `SIGWINCH` (the measured flap is 8 ms), short enough that a
/// person's mistaken Cmd-D, Cmd-W barely sees the held half.
pub(crate) const SPLIT_REGRID_QUIET: Duration = Duration::from_millis(250);

/// One pane's take-back hold, keyed by its session id in
/// [`SplitRegridHolds`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SplitRegridHold {
    /// The pane's `(rows, cols)` before the split: the one target withheld.
    before: (u16, u16),
    /// When the shrink has stood the quiet.
    until: Instant,
    /// A collapse's grow back to `before` was withheld, so `until` owes a wake.
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
    /// Record that a split just re-grid `session` away from `before`.
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

    /// The verdict for sizing `session` to `target` at `now` in a pass run
    /// for `cause`. Only a collapse starts withholding; a withheld take-back
    /// stays withheld against the same target. Anything applied ends the hold.
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

    /// The earliest end of a hold that withheld a grow; `None` when none did.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.holds
            .values()
            .filter(|h| h.withheld)
            .map(|h| h.until)
            .min()
    }

    /// Drop every hold whose quiet ended by `now` and name the sessions whose
    /// withheld grow is now owed (in id order).
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
    /// The `(session, (rows, cols))` of every terminal pane in `wid`'s active
    /// tab, read from the engines — what a split is about to re-grid.
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

    /// Arm a take-back hold on every pane of `before` whose grid the split
    /// just changed, except `newborn` — the split's own new pane, whose
    /// "before" is its transient spawn size, a grid its program never drew.
    /// `before` is [`Self::active_tab_pane_grids`] read ahead of the split's
    /// resize.
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

    /// Pay every withheld take-back whose quiet has ended by `now`: re-grid
    /// every window that shows its session now, and ask for its repaint.
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
        // The in-place grid (11, 80) was a different target: the hold ended.
        assert_eq!(holds.get(7), None);

        holds.arm(7, (24, 80), t0);
        assert_eq!(holds.judge(7, (24, 80), C, t0), HoldVerdict::Withhold);
        // A later re-lay (a redraw's pass) to the same grid keeps it withheld.
        assert_eq!(holds.judge(7, (24, 80), O, t0), HoldVerdict::Withhold);
        assert_eq!(holds.deadline(), Some(t0 + SPLIT_REGRID_QUIET));
        assert!(holds.take_due(t0).is_empty(), "not due before the quiet");
        assert_eq!(holds.take_due(t0 + SPLIT_REGRID_QUIET), vec![7]);
        assert_eq!(holds.deadline(), None);
    }

    #[test]
    fn no_collapse_no_hold_even_on_the_pre_split_grid() {
        // A zoom inside the quiet targets exactly the pre-split grid: it is no
        // take-back, so it applies and ends the hold.
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
