// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-frame message-band driver for the web host. The native host has no band: it calls only
//! [`drain_and_pace`], for the drain.

use std::borrow::Cow;
use std::cell::Cell;

use crate::animate::{BandMotion, Look, Pace};
use crate::glass::{Links, Presentation};
use crate::ink::{self, BandInks, Resolved};
use crate::log::{LogLine, Shelf};
use crate::paint::{BandKey, Geometry, Hover};
use crate::wire::WireGate;
use crate::{Instant, MAX_ROWS, MessageCenter};

/// Inputs for one step of message-band row-count commitment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commit {
    /// The step's instant (the host's clock, read once).
    pub now: Instant,
    /// The most rows the band may take ([`afford`]).
    pub afford: u16,
    /// The row count the host's geometry reserves now.
    pub reserved: u16,
}

/// What one step changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Committed {
    /// Something painted changed ([`crate::Settled::glass_changed`]).
    pub glass_changed: bool,
    /// Row count the host should re-grid to, or `None` when unchanged.
    pub regrid: Option<u16>,
}

/// Commits the row count and returns `Some(rows)` when the host must re-grid.
fn commit(center: &mut MessageCenter, c: Commit) -> Option<u16> {
    let moved = center.commit_rows(c.now, c.afford);
    let rows = center.committed_rows();
    (moved.is_some() || rows != c.reserved).then_some(rows)
}

/// Retires expired entries, holds included, and commits the row count for `c`.
pub fn settle_and_commit(center: &mut MessageCenter, c: Commit) -> Committed {
    let settled = center.settle(c.now, true);
    Committed {
        glass_changed: settled.glass_changed,
        regrid: commit(center, c),
    }
}

/// Drains shelved log lines when `persist` is set and checks whether a paced paint is due.
pub fn drain_and_pace(
    center: &mut MessageCenter,
    gate: &mut WireGate,
    now: Instant,
    persist: bool,
) -> (Vec<(LogLine, Shelf)>, bool) {
    let lines = if persist {
        center.drain_shelved_for_persist()
    } else {
        Vec::new()
    };
    (lines, gate.take_due(now))
}

/// Returns the maximum rows the band may take given `grids` and `reserved`.
#[must_use]
pub fn afford(grids: impl IntoIterator<Item = u16>, reserved: u16) -> u16 {
    grids
        .into_iter()
        .map(|g| g.saturating_add(reserved).saturating_sub(1))
        .min()
        .unwrap_or(MAX_ROWS)
}

/// Returns the next instant the center's state or the wire gate needs a wake.
#[must_use]
pub fn state_deadline(center: &MessageCenter, gate: &WireGate, frozen: bool) -> Option<Instant> {
    [center.deadline(!frozen), gate.due()]
        .into_iter()
        .flatten()
        .min()
}

/// What decides a view's look, each a host's own reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LookIn {
    /// Whether the host's motion gates allow the band to animate.
    pub motion_allowed: bool,
    /// The view is on screen (not occluded, minimized or headless).
    pub on_screen: bool,
    /// An OS-forced palette owns the chrome (High Contrast).
    pub forced: bool,
}

/// Computes the [`Look`] for a view from `i`.
#[must_use]
pub const fn look(i: LookIn) -> Look {
    Look {
        pace: if i.on_screen && i.motion_allowed {
            Pace::Moving
        } else {
            Pace::Still
        },
        graded: !i.forced,
    }
}

/// Layout parameters for a view.
#[derive(Clone, Copy, Debug)]
pub struct Lay {
    /// The view's column count.
    pub cols: usize,
    /// Whether rows end in their links ([`Links`]).
    pub links: Links,
}

impl Lay {
    /// Returns the layout configuration for a web terminal view.
    #[must_use]
    pub fn web(cols: usize, details: bool) -> Self {
        Self {
            cols,
            links: if details {
                Links::Painted
            } else {
                Links::Withheld
            },
        }
    }

    /// The width law's layout of `center` at this view's width.
    #[must_use]
    pub fn presentation(&self, center: &MessageCenter) -> Presentation {
        center.presentation(self.cols, &crate::text::char_width, self.links)
    }
}

/// A next-frame answer and what it was computed from.
#[derive(Clone, Copy, Debug)]
struct Memo {
    layout_fp: u64,
    motion_input_epoch: u64,
    cols: usize,
    look: Look,
    frame_at: Instant,
    next: Option<Instant>,
}

/// Per-surface band layout, motion frame, memo, and paint key.
#[derive(Debug, Default)]
pub struct View {
    /// `(center fingerprint, cols, layout)`; `None` with no committed row.
    layout: Option<(u64, usize, Presentation)>,
    /// The motion frame the next present reads; `None` with no row.
    motion: Option<BandMotion>,
    /// Monotone generation bumped whenever prepared `motion.rows` structurally changes.
    motion_gen: u64,
    /// The look [`Self::motion`] was drawn in.
    look: Option<Look>,
    /// The next painted change from that frame (a visible bar can take
    /// dozens of cell-surface probes to find it), kept across unrelated wakes
    /// until the source, the width, the look or the frame moves.
    memo: Cell<Option<Memo>>,
    /// What the painted rows were built from; `None` with nothing painted.
    key: Option<BandKey>,
    /// [`Self::invalidate`] ran: the next paint paints, even the empty band.
    stale: bool,
    /// How many next-change scans ran — the memo's cost law, read by tests.
    computations: Cell<u64>,
    /// The origin the last scan read from.
    last_from: Cell<Option<Instant>>,
}

impl View {
    /// Prepares the layout and motion frame at `now` in `look`, returning the motion fingerprint.
    pub fn prepare(&mut self, center: &MessageCenter, lay: &Lay, now: Instant, look: Look) -> u64 {
        let cols = lay.cols;
        let fp = center.fingerprint(cols);
        if fp == 0 {
            self.layout = None;
            self.motion = None;
            self.look = None;
            self.memo.set(None);
            return 0;
        }
        if !matches!(&self.layout, Some((f, c, _)) if *f == fp && *c == cols) {
            self.layout = Some((fp, cols, lay.presentation(center)));
        }
        let Some((_, _, p)) = &self.layout else {
            return 0;
        };
        let motion = center.motion(p, now, look);
        let mfp = motion.fingerprint();
        if self.motion.as_ref().map(|m| &m.rows) != Some(&motion.rows) {
            self.motion_gen = self.motion_gen.wrapping_add(1);
        }
        self.motion = Some(motion);
        self.look = Some(look);
        mfp
    }

    /// Returns whether a frame is prepared for `center` at `cols`.
    #[must_use]
    pub fn is_prepared(&self, center: &MessageCenter, cols: usize) -> bool {
        let fp = center.fingerprint(cols);
        fp == 0
            || (self.motion.is_some()
                && matches!(&self.layout, Some((f, c, _)) if *f == fp && *c == cols))
    }

    fn layout_now(&self, center: &MessageCenter, lay: &Lay) -> Option<Cow<'_, Presentation>> {
        let fp = center.fingerprint(lay.cols);
        if fp == 0 {
            return None;
        }
        Some(match &self.layout {
            Some((f, c, p)) if *f == fp && *c == lay.cols => Cow::Borrowed(p),
            _ => Cow::Owned(lay.presentation(center)),
        })
    }

    fn scan(
        &self,
        center: &MessageCenter,
        p: &Presentation,
        from: Instant,
        look: Look,
    ) -> Option<Instant> {
        self.computations.set(self.computations.get() + 1);
        self.last_from.set(Some(from));
        center.motion_deadline(p, from, look)
    }

    /// Returns the next visible change instant from the prepared frame or `now`.
    #[must_use]
    pub(crate) fn next_frame(
        &self,
        center: &MessageCenter,
        lay: &Lay,
        now: Instant,
        look: Look,
    ) -> Option<Instant> {
        let cols = lay.cols;
        let layout_fp = center.fingerprint(cols);
        if layout_fp == 0 {
            return None;
        }
        let motion_input_epoch = center.motion_input_epoch();
        let prepared = self.motion.is_some()
            && self.look == Some(look)
            && matches!(&self.layout, Some((f, c, _)) if *f == layout_fp && *c == cols);
        let frame_at = if prepared {
            self.motion.as_ref().map_or(now, |m| m.at)
        } else {
            now
        };
        if prepared
            && let Some(memo) = self.memo.get()
            && memo.layout_fp == layout_fp
            && memo.motion_input_epoch == motion_input_epoch
            && memo.cols == cols
            && memo.look == look
            && memo.frame_at == frame_at
        {
            return memo.next;
        }
        let p = self.layout_now(center, lay)?;
        let next = self.scan(center, &p, frame_at, look);
        if prepared {
            self.memo.set(Some(Memo {
                layout_fp,
                motion_input_epoch,
                cols,
                look,
                frame_at,
                next,
            }));
        }
        next
    }

    /// Returns the next motion deadline at or after `now`.
    #[must_use]
    pub(crate) fn motion_deadline(
        &self,
        center: &MessageCenter,
        lay: &Lay,
        now: Instant,
        look: Look,
    ) -> Option<Instant> {
        let next = self.next_frame(center, lay, now, look)?;
        if next > now {
            return Some(next);
        }
        let p = self.layout_now(center, lay)?;
        self.scan(center, &p, now, look)
    }

    /// Paints the view's rows when their inputs changed, or returns `None` if unchanged.
    pub fn paint(
        &mut self,
        center: &MessageCenter,
        cols: usize,
        hover: Option<Hover>,
        geom: Geometry,
        forced: bool,
        inks: &dyn Fn() -> BandInks,
    ) -> Option<Vec<Resolved>> {
        let fp = center.fingerprint(cols);
        if fp == 0 {
            let stale = std::mem::take(&mut self.stale);
            let painted = self.key.take().is_some();
            return (painted || stale).then(Vec::new);
        }
        let c = inks();
        let key: BandKey = (fp, cols, c, forced, hover, geom, self.motion_gen);
        if self.key == Some(key) && !self.stale {
            return None;
        }
        self.key = Some(key);
        self.stale = false;
        Some(match (&self.motion, &self.layout) {
            (Some(motion), Some((f, w, p))) if *f == fp && *w == cols => {
                ink::paint_band(p, hover, geom, motion, forced, &c)
            }
            _ => Vec::new(),
        })
    }

    /// Clears the paint key so the next [`Self::paint`] repaints.
    pub const fn invalidate(&mut self) {
        self.key = None;
        self.stale = true;
    }

    /// The kept layout, if any.
    #[must_use]
    pub fn layout(&self) -> Option<&Presentation> {
        self.layout.as_ref().map(|(_, _, p)| p)
    }

    /// The prepared motion frame, if any.
    #[must_use]
    pub const fn motion(&self) -> Option<&BandMotion> {
        self.motion.as_ref()
    }

    /// The look the prepared frame was drawn in.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn look(&self) -> Option<Look> {
        self.look
    }

    /// How many next-change scans this view has run.
    #[must_use]
    pub const fn deadline_computations(&self) -> u64 {
        self.computations.get()
    }

    /// The origin the last scan read from.
    #[must_use]
    pub const fn last_from(&self) -> Option<Instant> {
        self.last_from.get()
    }
}

/// Returns the earliest motion deadline across on-screen `views`.
pub fn motion_deadline_over<'a>(
    center: &MessageCenter,
    reserved: u16,
    now: Instant,
    views: impl IntoIterator<Item = (&'a View, Lay, Look)>,
) -> Option<Instant> {
    if reserved == 0 {
        return None;
    }
    views
        .into_iter()
        .filter_map(|(v, lay, look)| v.motion_deadline(center, &lay, now, look))
        .min()
}
