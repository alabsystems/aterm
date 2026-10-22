// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's chrome regrow floor: after a window's chrome rows rise (its
//! grid shrinks), no chrome source lets them fall (the grid grows back) inside
//! [`CHROME_REGROW_SETTLE`].
//!
//! The tab strip and the message band are reserved out of the window's grid,
//! so every change of their total re-grids it. The band spaces its own fall
//! from its own rise ([`crate::SHRINK_QUIET`]), but not from the strip's, and a
//! person's Settings edit takes the strip down whenever it lands, so two
//! sources can make a shrink-then-grow pair milliseconds apart. (The
//! presence row was the third source until the owner surfaces, 2026-10-16,
//! took it out: presence re-grids nothing now.) On the
//! alternate screen that pair shifts every row up one, and a program that
//! repaints only when its `SIGWINCH` handler reads a changed size (Claude
//! Code) never repaints; the grid's resize undo (aterm-grid
//! `grid/resize_undo.rs`) restores the screen only when nothing is written
//! between the halves.
//!
//! The host notes every rise ([`RegrowFloor::rose`]): a band row reaching the
//! glass and a tab-strip rise, on every window. The band's quiet fall and the
//! tab strip's fall then wait the floor ([`fall_allowed`], [`at_floor`]); the
//! strip's re-grids at once when no row rose inside the settle, so a Settings
//! edit is delayed only after a rise, by at most the settle. A rise is never
//! delayed: grow-then-shrink trims the blank row the grow appended, an exact
//! identity. One fall is not floored: the window's own (it can no longer
//! afford a row). The band's and the strip's falls re-grid every window, so
//! they wait the [`latest`] floor of every window with glass; a window without
//! glass has no floor. (The strip's fall was immediate until 2026-10-19, when
//! the windowed lane measured a band rise and a Settings edit's strip fall
//! re-gridding 38 ms apart.)
//!
//! Every instant is the host's `now`. The derived model is aterm-spec's
//! `ChromeRegrowFloor` (`models_chrome_regrow.rs`: `NoQuickRegrow`,
//! `TheAnchorIsTheLastRise`, and the liveness `TheGridRegrows`).

use crate::{Duration, Instant};

/// The least span between a rise of a window's chrome rows (its grid shrank)
/// and the next unforced fall, whichever sources moved. 400 ms:
///
/// * over ten times the slowest `SIGWINCH` size read (5–30 ms, Claude Code),
///   so the program reads and repaints each size the grid takes;
/// * wider than the resize ledger's run gap (`RUN_GAP_MS` in aterm-gui,
///   250 ms), so a spaced rise and fall the program drew between are two runs;
/// * shorter than the band's own quiet ([`crate::SHRINK_QUIET`]), so the
///   floor binds only across sources.
pub const CHROME_REGROW_SETTLE: Duration = Duration::from_millis(400);

/// One window's regrow floor: when its chrome rows last rose. The host keeps
/// it in that window's state and notes each rise there; the engine's fall laws
/// read the floor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegrowFloor {
    last_rise: Option<Instant>,
}

impl RegrowFloor {
    /// The window's chrome rows rose at `now`. The floor keeps the latest rise:
    /// a stale `now` moves nothing, so it can never hasten a fall.
    #[inline]
    pub fn rose(&mut self, now: Instant) {
        self.last_rise = Some(self.last_rise.map_or(now, |x| x.max(now)));
    }

    /// When the window's chrome rows last rose; `None` if they never have.
    #[inline]
    #[must_use]
    pub const fn last_rise(self) -> Option<Instant> {
        self.last_rise
    }

    /// The earliest instant an unforced fall may re-grid the window:
    /// [`CHROME_REGROW_SETTLE`] past the last rise. `None` with no rise.
    #[inline]
    #[must_use]
    pub fn floor(self) -> Option<Instant> {
        self.last_rise
            .map(|r| r.checked_add(CHROME_REGROW_SETTLE).unwrap_or(r))
    }
}

/// Whether an unforced fall may re-grid at `now` against `floor` (a window's
/// [`RegrowFloor::floor`], or the [`latest`] of several): at or past it, or
/// with no floor at all.
#[inline]
#[must_use]
pub fn fall_allowed(floor: Option<Instant>, now: Instant) -> bool {
    floor.is_none_or(|f| now >= f)
}

/// When a fall due at `due` by its own law may commit: the later of that and
/// `floor`. A deadline built on it never names an instant the fall's commit
/// would decline, so the host's timer never spins on it.
#[inline]
#[must_use]
pub fn at_floor(due: Instant, floor: Option<Instant>) -> Instant {
    floor.map_or(due, |f| due.max(f))
}

/// The latest of several windows' floors — what a fall that re-grids all of
/// them waits (the band's). `None` when none has one.
#[inline]
#[must_use]
pub fn latest(floors: impl IntoIterator<Item = Option<Instant>>) -> Option<Instant> {
    floors.into_iter().flatten().max()
}

/// Whether a re-grid of chrome rows from `reserved` to `rows` is a rise (the
/// grid shrinks), which the host notes on every window it re-grids.
#[inline]
#[must_use]
pub const fn is_rise(reserved: u16, rows: u16) -> bool {
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
}
