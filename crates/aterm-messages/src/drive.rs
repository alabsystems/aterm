// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BAND'S ONE DRIVER (design ruling 336) — the per-frame SEQUENCING
//! every host ran for itself until round 27: retire what expired, commit the
//! row count, drain the log, take the wire's paced paint, lay the band out,
//! read one motion frame, key and paint the rows, and fold the deadlines the
//! host's timer arms. The owner's ask (2026-09-21): *"design the logic in
//! aterm core and then keep the osx layer lightweight so that we can make
//! this cross platform."*
//!
//! Nothing here reads a clock, owns a timer or knows a window system: every
//! instant is the host's (`now`, injected), every input a plain value — the
//! grids' heights the band may take a row from ([`afford`]), whether a
//! handoff freezes the band ([`Commit::frozen`]), whether a view is on
//! screen and may move ([`LookIn`]), its width and link posture ([`Lay`]),
//! its hover, geometry and inks ([`View::paint`]). The host keeps what is
//! its own: the re-grid a committed count asks for ([`Committed::regrid`]),
//! the log file the drained lines go to, the timer, the compose of the
//! painted rows onto its frame, and what a repaint request means there.
//!
//! No wake is added: every instant a fold returns comes from the center's
//! own deadlines ([`MessageCenter::deadline`],
//! [`MessageCenter::motion_deadline`]) and the wire's paced paint
//! ([`WireGate::due`]); an idle band returns `None` from each (FL-1).
//!
//! # One view per window
//!
//! A [`View`] is one surface's band state: the width law's layout (keyed by
//! the center's fingerprint and the width), the motion frame the next
//! present reads (and the look it was drawn in), the memoised next change
//! from that frame, and the key the painted rows were built from. The macOS
//! host keeps one per window; the web module one per terminal.

use std::borrow::Cow;
use std::cell::Cell;

use crate::animate::{BandMotion, Look, Pace};
use crate::glass::{Links, Presentation};
use crate::ink::{self, BandInks, Resolved};
use crate::log::{LogLine, Shelf};
use crate::paint::{self, BandKey, Geometry, Hover};
use crate::wire::WireGate;
use crate::{Instant, MAX_ROWS, MessageCenter};

// ---- the center-wide step ---------------------------------------------------

/// What one step of the band commits against: the instant, the handoff
/// freeze, and the rows the grids can spare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commit {
    /// The step's instant (the host's clock, read once).
    pub now: Instant,
    /// A seamless handoff freezes the band: holds are suspended and the row
    /// count stays where it is until the handoff finishes.
    pub frozen: bool,
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
    /// The row count the host re-grids to: `Some` when the center's count
    /// moved, or when it disagrees with the host's reserved count (a
    /// handoff's successor converging once its freeze lifts); `None` under a
    /// freeze.
    pub regrid: Option<u16>,
}

/// Commit the row count (never under a freeze): the center's hysteresis
/// over what the grids afford, and the count the host re-grids to — `Some`
/// when it moved or disagrees with [`Commit::reserved`].
pub fn commit(center: &mut MessageCenter, c: Commit) -> Option<u16> {
    if c.frozen {
        return None;
    }
    let moved = center.commit_rows(c.now, c.afford);
    let rows = center.committed_rows();
    (moved.is_some() || rows != c.reserved).then_some(rows)
}

/// The step's first half: retire what expired (holds suspended under a
/// freeze), then [`commit`]. The host re-grids between this and
/// [`drain_and_pace`], so a row is reserved in the same step as its resize.
pub fn settle_and_commit(center: &mut MessageCenter, c: Commit) -> Committed {
    let settled = center.settle(c.now, !c.frozen);
    Committed {
        glass_changed: settled.glass_changed,
        regrid: commit(center, c),
    }
}

/// The step's second half: the log lines the center shelved (only when
/// `persist`; a handoff's successor writes nothing before its commit), and
/// whether the wire's one paced paint fell due (ruling 170).
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

/// The most rows the band may take: every grid keeps its last terminal row
/// — each grid's rows plus the band rows already `reserved`, less one; the
/// smallest decides. [`MAX_ROWS`] with no grid to spare a row from.
#[must_use]
pub fn afford(grids: impl IntoIterator<Item = u16>, reserved: u16) -> u16 {
    grids
        .into_iter()
        .map(|g| g.saturating_add(reserved).saturating_sub(1))
        .min()
        .unwrap_or(MAX_ROWS)
}

/// The next instant the band's STATE needs the host back — a hold's
/// expiry, an echo's end, a queued row's patience (holds suspended under a
/// freeze), the wire's paced paint. `None` when idle.
#[must_use]
pub fn state_deadline(center: &MessageCenter, gate: &WireGate, frozen: bool) -> Option<Instant> {
    [center.deadline(!frozen), gate.due()]
        .into_iter()
        .flatten()
        .min()
}

// ---- the look ----------------------------------------------------------------

/// What decides a view's look, each a host's own reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent host readings, each a yes or no"
)]
pub struct LookIn {
    /// The host's motion gates allow the band to move (its effect policy,
    /// reduced motion, a serious mode — the page's `set_band_motion`).
    pub motion_allowed: bool,
    /// The view is on screen (not occluded, minimized or headless).
    pub on_screen: bool,
    /// A handoff freezes the band.
    pub frozen: bool,
    /// An OS-forced palette owns the chrome (High Contrast).
    pub forced: bool,
}

/// The look a view draws in (design ruling 140): MOVING only on screen,
/// unfrozen and allowed; GRADED unless a forced palette owns the chrome.
#[must_use]
pub fn look(i: LookIn) -> Look {
    Look {
        pace: if i.on_screen && !i.frozen && i.motion_allowed {
            Pace::Moving
        } else {
            Pace::Still
        },
        graded: !i.forced,
    }
}

// ---- one view ------------------------------------------------------------------

/// How a view lays the band out: its width, whether each row ends in its
/// link, and where `$HOME` is (read only when a layout is built).
#[derive(Clone, Copy, Debug)]
pub struct Lay {
    /// The view's column count.
    pub cols: usize,
    /// Whether rows end in their links ([`Links`]).
    pub links: Links,
    /// The home paths under which are printed `~/…` (`None`: none).
    pub home: fn() -> Option<String>,
}

impl Lay {
    /// The width law's layout of `center` at this view's width.
    #[must_use]
    pub fn presentation(&self, center: &MessageCenter) -> Presentation {
        let home = (self.home)();
        center.presentation(
            self.cols,
            &crate::text::char_width,
            home.as_deref(),
            self.links,
        )
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

/// One surface's band: its layout, its motion frame, the memoised next
/// change from that frame, and its paint key (module doc).
#[derive(Debug, Default)]
pub struct View {
    /// `(center fingerprint, cols, layout)`; `None` with no committed row.
    layout: Option<(u64, usize, Presentation)>,
    /// The motion frame the next present reads; `None` with no row.
    motion: Option<BandMotion>,
    /// [`Self::motion`]'s fingerprint (0 when nothing moves).
    motion_fp: u64,
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
    /// Prepare the frame a present at `now` reads, in `look`: the layout
    /// (re-laid only when the center's fingerprint or the width moved) and
    /// one motion frame. Returns the frame's fingerprint — a repaint key's
    /// motion term — `0` when nothing moves. With no committed row the view
    /// forgets its layout, frame, look and memo.
    pub fn prepare(&mut self, center: &MessageCenter, lay: &Lay, now: Instant, look: Look) -> u64 {
        let cols = lay.cols;
        let fp = center.fingerprint(cols);
        if fp == 0 {
            self.layout = None;
            self.motion = None;
            self.motion_fp = 0;
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
        self.motion = Some(motion);
        self.motion_fp = mfp;
        self.look = Some(look);
        // A content-only present can prepare the SAME frame again: the memo's
        // key keeps its answer valid, so it stays; a new frame misses the key
        // on the next query and computes its own.
        mfp
    }

    /// Whether a frame is prepared for the band at `cols` (nothing to
    /// prepare with no committed row).
    #[must_use]
    pub fn is_prepared(&self, center: &MessageCenter, cols: usize) -> bool {
        let fp = center.fingerprint(cols);
        fp == 0
            || (self.motion.is_some()
                && matches!(&self.layout, Some((f, c, _)) if *f == fp && *c == cols))
    }

    /// The layout at the view's width: the kept one when current, else a
    /// fresh one; `None` with no committed row.
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

    /// One next-change scan from `from`, counted.
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

    /// The next visible change from the frame on glass: its origin is the
    /// prepared frame's instant when that frame was drawn in `look` at the
    /// current layout, else `now` (a newly posted row, a frame in another
    /// look); the answer is memoised only for a prepared frame. `None` with
    /// no committed row or nothing that will change.
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

    /// [`Self::next_frame`], never in the past: a due frame may still be
    /// queued in the host, so a past answer is re-read from `now` — the next
    /// grid instant is the safe deadline until that frame is prepared.
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

    /// Whether the view is due a new frame at `now`: the frame on glass was
    /// drawn in another look (focus, reduced motion, a forced palette moved
    /// it), or its next change has come, or nothing is prepared yet while
    /// something will change.
    #[must_use]
    pub(crate) fn due(&self, center: &MessageCenter, lay: &Lay, now: Instant, look: Look) -> bool {
        if self.motion.is_some() && self.look != Some(look) {
            return center.fingerprint(lay.cols) != 0;
        }
        self.next_frame(center, lay, now, look)
            .is_some_and(|d| d <= now || self.motion.is_none())
    }

    /// The next change read from `from` over the KEPT layout — no memo, no
    /// past guard: the web module's rule (ruling 337, F4). `None` with no
    /// layout kept.
    #[must_use]
    pub fn deadline_from(
        &self,
        center: &MessageCenter,
        from: Instant,
        look: Look,
    ) -> Option<Instant> {
        let (_, _, p) = self.layout.as_ref()?;
        self.scan(center, p, from, look)
    }

    /// The band's repaint term for this view ([`paint::band_fp`]): the
    /// center at `cols`, the hover, the geometry and this view's motion
    /// frame; `0` with no committed row.
    #[must_use]
    pub fn band_fp(
        &self,
        center: &MessageCenter,
        cols: usize,
        hover: Option<Hover>,
        geom: Geometry,
    ) -> u64 {
        paint::band_fp(center.fingerprint(cols), hover, geom, self.motion_fp)
    }

    /// Paint the view's rows when what they are built from moved — the
    /// center at `cols`, the inks and the forced palette, the hover, the
    /// geometry, the motion frame — and `None` when the rows already painted
    /// are still these. `inks` runs only with a committed row, so an idle
    /// frame derives nothing; with none, the rows are empty (`Some` of
    /// nothing once, when rows were painted before or the view was
    /// [invalidated](Self::invalidate)).
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
        let ink_key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            c.hash(&mut h);
            forced.hash(&mut h);
            h.finish()
        };
        let key: BandKey = (fp, cols, ink_key, hover, geom, self.motion_fp);
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

    /// Forget the paint key, so the next [`Self::paint`] paints — the empty
    /// band too (a capture that repaints every frame it shoots).
    pub fn invalidate(&mut self) {
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
    pub fn motion(&self) -> Option<&BandMotion> {
        self.motion.as_ref()
    }

    /// The look the prepared frame was drawn in.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn look(&self) -> Option<Look> {
        self.look
    }

    /// The prepared frame's fingerprint (0 when nothing moves).
    #[must_use]
    pub fn motion_fp(&self) -> u64 {
        self.motion_fp
    }

    /// How many next-change scans this view has run.
    #[must_use]
    pub fn deadline_computations(&self) -> u64 {
        self.computations.get()
    }

    /// The origin the last scan read from.
    #[must_use]
    pub fn last_from(&self) -> Option<Instant> {
        self.last_from.get()
    }
}

// ---- many views -----------------------------------------------------------------

/// The next instant any of `views` — the ON-SCREEN ones, each with its lay
/// and look — needs a motion frame or a time word's tick
/// (`View::motion_deadline`). `None` with no row reserved or under a
/// freeze (the frame on glass stays where it is).
pub fn motion_deadline_over<'a>(
    center: &MessageCenter,
    reserved: u16,
    frozen: bool,
    now: Instant,
    views: impl IntoIterator<Item = (&'a View, Lay, Look)>,
) -> Option<Instant> {
    if reserved == 0 || frozen {
        return None;
    }
    views
        .into_iter()
        .filter_map(|(v, lay, look)| v.motion_deadline(center, &lay, now, look))
        .min()
}

/// Which of `views` — the ON-SCREEN ones, each named by `K` — are due a new
/// frame at `now` (`View::due`). Nothing with no row reserved or under a
/// freeze.
pub fn due_views<'a, K>(
    center: &MessageCenter,
    reserved: u16,
    frozen: bool,
    now: Instant,
    views: impl IntoIterator<Item = (K, &'a View, Lay, Look)>,
) -> Vec<K> {
    if reserved == 0 || frozen {
        return Vec::new();
    }
    views
        .into_iter()
        .filter(|(_, v, lay, look)| v.due(center, lay, now, *look))
        .map(|(k, ..)| k)
        .collect()
}
