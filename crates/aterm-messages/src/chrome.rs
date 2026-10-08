// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW'S CHROME REGROW FLOOR (design ruling 414) — after a
//! window's chrome rows ROSE (its grid shrank), no chrome source lets them
//! FALL (the grid grows back) inside [`CHROME_REGROW_SETTLE`].
//!
//! A window's chrome rows — the tab strip, the message band and the presence
//! row — are reserved out of its terminal grid, so every change of their
//! total re-grids the window: an engine resize and a `SIGWINCH` to every
//! session it shows. Each source already spaces its OWN fall from its own
//! rise: the band's D1 hysteresis shrinks only after [`crate::SHRINK_QUIET`]
//! (1.5 s) of wanting less, and the presence row folds only after
//! [`crate::presence::FOLD_QUIET`] (1.5 s, ruling 394) — so no source makes a
//! shrink-then-grow pair by itself. Two sources still did, because neither
//! law reads the other's rise (measured 2026-09-29 on main `8b24e99a4`, in
//! real windows, against a stand-in that reads its size 20 ms after the
//! signal and writes a spinner tick every 5 ms): the presence row BORN 4–6 ms
//! before the band's own quiet shrink; a band row RISING 11–13 ms before the
//! presence row's timed fold; a band row rising 3–6 ms before a person's
//! keypress folds a presence row that has already stood its quiet. On the
//! alternate screen that net-zero pair shifts every row up one, and a program
//! that repaints only when its `SIGWINCH` handler reads a CHANGED size (Claude
//! Code) reads the old size after both and never repaints. The grid's resize
//! undo (aterm-grid `grid/resize_undo.rs`) makes such a pair an exact identity
//! only while nothing is written between its halves; the spinner tick between
//! them drops it, and the screen stayed shifted (3 of 3, 3 of 3 and 2 of 3
//! runs).
//!
//! THE FLOOR. The host notes every RISE of a window's chrome rows
//! ([`RegrowFloor::rose`]): the presence row's birth on that window, a band
//! row reaching the glass and a tab-strip rise on every window. A source's
//! own quiet FALL then also waits the floor, [`RegrowFloor::floor`] —
//! [`CHROME_REGROW_SETTLE`] past the last rise — through [`fall_allowed`];
//! the fall's deadline is the later of its own and the floor ([`at_floor`]).
//! A RISE is never delayed (the grow-then-shrink order is benign: the shrink
//! trims the blank row the grow appended, so a quiet pair is an exact
//! identity), and two falls are exempt by name, as they already are from the
//! sources' own quiet: the WINDOW's (a real window that can no longer afford
//! a row gets it back at once) and the person's View-menu hide of the
//! presence band. The tab strip's fall (a person's edit of Settings) is not
//! floored either: the strip is a host-only source this engine never sees,
//! so the host notes its rise on every window's floor and re-grids its fall
//! at once. The band's fall is global — it re-grids every window — so it
//! waits the LATEST floor of every window with glass ([`latest`]). A window
//! without glass never re-grids for its chrome, so it has no floor.
//!
//! Clockless like the rest of the engine: every instant is the host's `now`.
//! The derived model is aterm-spec's `ChromeRegrowFloor`
//! (`models_chrome_regrow.rs`): `NoQuickRegrow` (no floored fall inside the
//! settle of the last rise, whichever source rose), `TheAnchorIsTheLastRise`
//! (the floor counts from the last rise, never re-anchored by another step),
//! and the liveness `TheGridRegrows` (a waiting fall is applied once its own
//! quiet and the floor have run).

use crate::{Duration, Instant};

/// The least span between a RISE of a window's chrome rows (its grid shrank)
/// and the next unforced FALL (the grid grows back), whichever sources moved.
///
/// 400 ms, because:
///
/// * the cross-source pairs came 3–13 ms apart, and a program's `SIGWINCH`
///   handler reads the size 5–30 ms after the signal (Claude Code): 400 ms is
///   more than ten times the slowest read, so the program has read the shrunk
///   size, and repainted for it, before the grid grows back — each size the
///   grid takes is a size it reads;
/// * it is wider than the resize ledger's run gap (250 ms, `RUN_GAP_MS` in
///   aterm-gui), so a spaced rise and fall the program drew between are two
///   runs, each judged for itself; a pair with nothing drawn between still
///   joins, however late, and the grid's resize undo makes that quiet pair an
///   identity (`none`);
/// * it is shorter than every source's own quiet ([`crate::SHRINK_QUIET`],
///   [`crate::presence::FOLD_QUIET`], both 1.5 s), so a source's own rise is
///   never the binding floor: the floor binds only across sources, and adds
///   at most 400 ms to a fall that another source's rise just preceded.
pub const CHROME_REGROW_SETTLE: Duration = Duration::from_millis(400);

/// One window's regrow floor: when its chrome rows last ROSE. The host keeps
/// it in that window's own state and notes each rise there; the engine's fall
/// laws read the floor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegrowFloor {
    last_rise: Option<Instant>,
}

impl RegrowFloor {
    /// The window's chrome rows rose at `now`. The floor keeps the LATEST
    /// rise: a stale `now` (an earlier instant than one already noted) moves
    /// nothing, so it can never hasten a fall.
    pub fn rose(&mut self, now: Instant) {
        self.last_rise = Some(self.last_rise.map_or(now, |x| x.max(now)));
    }

    /// When the window's chrome rows last rose; `None` if they never have.
    #[must_use]
    pub fn last_rise(self) -> Option<Instant> {
        self.last_rise
    }

    /// The earliest instant an unforced fall may re-grid the window:
    /// [`CHROME_REGROW_SETTLE`] past the last rise. `None` with no rise.
    #[must_use]
    pub fn floor(self) -> Option<Instant> {
        self.last_rise
            .map(|r| r.checked_add(CHROME_REGROW_SETTLE).unwrap_or(r))
    }
}

/// Whether an unforced fall may re-grid at `now` against `floor` (a window's
/// [`RegrowFloor::floor`], or the [`latest`] of several): at or past it, or
/// with no floor at all.
#[must_use]
pub fn fall_allowed(floor: Option<Instant>, now: Instant) -> bool {
    floor.is_none_or(|f| now >= f)
}

/// When a fall due at `due` by its own law may commit: the later of that and
/// `floor`. A deadline built on it never names an instant the fall's commit
/// would decline, so the host's timer never spins on it.
#[must_use]
pub fn at_floor(due: Instant, floor: Option<Instant>) -> Instant {
    floor.map_or(due, |f| due.max(f))
}

/// The latest of several windows' floors — what a fall that re-grids all of
/// them waits (the band's). `None` when none has one.
#[must_use]
pub fn latest(floors: impl IntoIterator<Item = Option<Instant>>) -> Option<Instant> {
    floors.into_iter().flatten().max()
}

/// Whether a re-grid of chrome rows from `reserved` to `rows` is a RISE (the
/// grid shrinks), which the host notes on every window it re-grids.
#[must_use]
pub fn is_rise(reserved: u16, rows: u16) -> bool {
    rows > reserved
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn no_rise_is_no_floor_and_every_fall_is_allowed() {
        let now = Instant::now();
        let f = RegrowFloor::default();
        assert_eq!(f.last_rise(), None);
        assert_eq!(f.floor(), None);
        assert!(fall_allowed(f.floor(), now));
        assert_eq!(at_floor(now, f.floor()), now);
    }

    #[test]
    fn a_fall_waits_the_settle_past_the_last_rise() {
        let t0 = Instant::now();
        let mut f = RegrowFloor::default();
        f.rose(t0);
        assert_eq!(f.floor(), Some(t0 + CHROME_REGROW_SETTLE));
        assert!(!fall_allowed(f.floor(), t0));
        assert!(!fall_allowed(f.floor(), t0 + ms(5)));
        assert!(!fall_allowed(f.floor(), t0 + CHROME_REGROW_SETTLE - ms(1)));
        assert!(fall_allowed(f.floor(), t0 + CHROME_REGROW_SETTLE));
        // A fall already due by its own law waits the floor; one due later
        // keeps its own instant.
        assert_eq!(at_floor(t0 + ms(6), f.floor()), t0 + CHROME_REGROW_SETTLE);
        assert_eq!(at_floor(t0 + ms(900), f.floor()), t0 + ms(900));
    }

    /// THE ANCHOR IS THE LAST RISE: a later rise moves the floor on, and a
    /// stale instant (earlier than one noted) never moves it back.
    #[test]
    fn the_floor_counts_from_the_latest_rise_and_never_moves_back() {
        let t0 = Instant::now();
        let mut f = RegrowFloor::default();
        f.rose(t0);
        f.rose(t0 + ms(300));
        assert_eq!(f.last_rise(), Some(t0 + ms(300)));
        assert!(!fall_allowed(f.floor(), t0 + CHROME_REGROW_SETTLE));
        f.rose(t0 + ms(100));
        assert_eq!(f.last_rise(), Some(t0 + ms(300)), "a stale rise");
        assert!(fall_allowed(f.floor(), t0 + ms(700)));
    }

    #[test]
    fn the_band_waits_the_latest_floor_of_its_windows() {
        let t0 = Instant::now();
        assert_eq!(latest([None, None]), None);
        assert_eq!(
            latest([Some(t0 + ms(10)), None, Some(t0 + ms(20))]),
            Some(t0 + ms(20))
        );
    }

    #[test]
    fn only_a_growing_row_count_is_a_rise() {
        assert!(is_rise(0, 1));
        assert!(is_rise(1, 3));
        assert!(!is_rise(1, 1));
        assert!(!is_rise(2, 1));
    }

    /// The span's reasons, pinned: wider than the ledger's run gap (250 ms)
    /// and shorter than both sources' own quiet.
    #[test]
    fn the_settle_sits_between_the_run_gap_and_the_sources_quiet() {
        assert_eq!(CHROME_REGROW_SETTLE, ms(400));
        assert!(CHROME_REGROW_SETTLE > ms(250));
        assert!(CHROME_REGROW_SETTLE < crate::SHRINK_QUIET);
        assert!(CHROME_REGROW_SETTLE < crate::presence::FOLD_QUIET);
    }
}
