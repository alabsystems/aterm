// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The engine's resize journal and its screen-replacement counters.
//!
//! Only the engine knows what a resize did to the rows an application painted.
//! A rows-only shrink on the alternate screen, with a non-blank bottom row and
//! the cursor below row 0, demotes the TOP row. The alt grid keeps no history
//! (`max_scrollback = 0`), so the row is dropped, and the following grow appends
//! a blank row at the bottom. A 64→63→64 flap therefore leaves the whole screen
//! one row higher than the app painted it. The app repaints only when its
//! SIGWINCH handler reads a CHANGED size. When both resizes land before the
//! handler runs, it reads 64 == 64, skips the repaint, and its later diff frames
//! land on the shifted content for good (measured 2026-09-28 against a live
//! Claude Code session, one flap in 52).
//!
//! The grid now undoes such a flap when nothing was drawn between its halves
//! (`aterm_grid`'s resize undo: the shrink keeps the rows it took off, the grow
//! hands them back), and the report says so (`stashed`, `restored_top`,
//! `restored_bottom`). A flap the app drew into keeps the shift, and the app,
//! having drawn at the changed size, repaints.
//!
//! A host cannot see this from the geometry, which is unchanged across the
//! flap, nor from a per-read watermark, which misses a flap that lands between
//! two PTY reads. So the engine records each resize itself, in order, including
//! both halves of such a flap:
//!
//! * [`ResizeReport`]: the from/to geometry, the active screen, whether a DEC
//!   2026 frame was open, and every arm of the grid's row accounting
//!   ([`aterm_grid::ResizeShape`]).
//! * The journal: the newest [`RESIZE_JOURNAL_CAP`] reports, keyed by a
//!   1-based ordinal that never resets for this engine's lifetime. A reader
//!   holds its last ordinal and asks for what came after it through
//!   [`Terminal::resize_journal_since`]. An overrun is COUNTED, never silent.
//! * The engine's LIFETIME token ([`Terminal::resize_lifetime`]): unique per
//!   engine built in this process, so a reader that holds an ordinal can tell a
//!   copy it took late (a lower ordinal of the SAME lifetime: stale) from a
//!   restored engine (another lifetime, whose ordinals restart at 0).
//! * Two monotonic counters of the operations that repaint a displaced screen
//!   from scratch: [`Terminal::full_clear_count`] (ED 2, and ED 0 issued with
//!   the cursor home) and [`Terminal::screen_replaced_count`] (alternate screen
//!   enter and exit, RIS). A reader compares each against its value in a report
//!   to learn whether the app healed the screen afterwards
//!   ([`ResizeReport::healed_by`]). ED 3 is not one: it erases the history
//!   behind the screen and leaves every visible row where it is.
//!
//! PURE, like the rest of the engine. No clock is read here (`grep_guard` C3):
//! the ordinal and `content_seq` order a resize against output, and the host
//! stamps wall time. Nothing here is checkpointed. A restored engine starts at
//! ordinal 0 under a new lifetime token, and a reader keys on ordinal deltas
//! within one lifetime; a watermark past the current ordinal is read as 0 (see
//! [`Terminal::resize_journal_since`]).
//! No hot-path cost: a resize is already O(viewport), and the counters sit in
//! the rare ED and screen-switch handlers.

use super::Terminal;

#[cfg(test)]
#[path = "resize_journal_tests.rs"]
mod tests;

/// How many resizes the journal keeps.
///
/// Sixteen: a net-zero chrome-row flap is two resizes, and a reader that drains
/// once per event-loop wake sees a handful at most. A window drag makes about
/// 60 resizes a second, so a reader that falls further behind loses the oldest
/// ones, and [`Terminal::resize_journal_since`] reports how many it lost.
pub const RESIZE_JOURNAL_CAP: usize = 16;

/// [`RESIZE_JOURNAL_CAP`] in ordinal arithmetic (a lossless widening).
const CAP_U64: u64 = RESIZE_JOURNAL_CAP as u64;

/// One resize as the engine applied it to the ACTIVE grid.
///
/// `from`/`to` are `(rows, cols)`. `to` is after the grid's ingress clamp, so
/// it is the geometry the grid really took. Every call is recorded, including
/// one that repeats the current size (the grid still re-lays it out); `from ==
/// to` names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "three independent facts about one resize, not a state machine"
)]
pub struct ResizeReport {
    /// 1-based position in this engine's resize sequence. Never reset.
    pub ordinal: u64,
    /// Geometry before the resize, `(rows, cols)`.
    pub from: (u16, u16),
    /// Geometry after the resize, `(rows, cols)`, after the ingress clamp.
    pub to: (u16, u16),
    /// The active grid was the alternate screen, which keeps no history.
    pub alt: bool,
    /// A DEC 2026 synchronized-output frame was open when the resize landed.
    pub in_sync: bool,
    /// Trailing blank rows below the cursor dropped by a shrink (nothing moved).
    pub trimmed: u16,
    /// Top rows demoted into history by a shrink: every surviving row moved UP
    /// by this count.
    pub demoted: u16,
    /// Bottom rows pushed off the screen with nothing above them moving.
    pub pushed: u16,
    /// History lines a grow revealed at the top: every row moved DOWN by this
    /// count.
    pub revealed: u16,
    /// Blank rows a grow appended at the bottom (nothing moved).
    pub appended: u16,
    /// Of `demoted + pushed`, the rows the grid's resize undo kept (the
    /// alternate screen, which has no history to put them in): a grow back
    /// with nothing drawn in between hands them back. All of them or none.
    pub stashed: u16,
    /// Of `revealed`, the rows the resize undo handed back to the top: an
    /// earlier shrink demoted them and nothing drew since, so the grow put them
    /// back where the app painted them.
    pub restored_top: u16,
    /// Rows the resize undo put back at the bottom, where an earlier shrink
    /// pushed them off (nothing moved; not counted in `appended`).
    pub restored_bottom: u16,
    /// A width change rewrapped the grid (a primary-screen reflow), which
    /// renumbers rows wholesale, so the counts above are not a displacement.
    pub reflowed: bool,
    /// The active grid's content sequence (`Terminal::content_seq`) AFTER the
    /// resize. A resize re-lays the grid out, so it advances this by itself
    /// (once, or more when a width change pulls history back): compare a live
    /// `content_seq` against it to learn whether the app drew since.
    pub content_seq: u64,
    /// The active grid's content sequence BEFORE the resize. Equal to the
    /// previous report's `content_seq` exactly when nothing changed the screen
    /// between the two resizes (see [`ResizeReport::adjoins`]).
    pub content_seq_before: u64,
    /// [`Terminal::full_clear_count`] when the resize landed.
    pub full_clears: u64,
    /// [`Terminal::screen_replaced_count`] when the resize landed.
    pub screen_replaced: u64,
}

impl ResizeReport {
    /// Rows this resize moved the painted content by: `revealed - demoted`.
    /// Negative means the content moved UP. Meaningful when `reflowed` is
    /// false; a rewrap moves rows on its own. Rows the resize undo handed back
    /// to the top are in `revealed`, so a quiet alt-screen flap sums to 0 just
    /// as a primary-screen one does.
    #[must_use]
    pub fn shift(&self) -> i32 {
        i32::from(self.revealed) - i32::from(self.demoted)
    }

    /// Rows this resize pushed off the bottom, net of the ones it put back
    /// there from the resize undo: positive for a shrink's bottom-push,
    /// negative for the grow that undid one.
    #[must_use]
    pub fn net_pushed(&self) -> i32 {
        i32::from(self.pushed) - i32::from(self.restored_bottom)
    }

    /// The resize moved content that no later resize can move back by itself:
    /// the alternate screen keeps no history, so a demoted row is dropped, and
    /// a later grow appends blanks rather than revealing it. Not lossy when the
    /// resize undo kept the rows (`stashed`): a grow back with nothing drawn in
    /// between hands them back. Decided when THIS resize ran, so it is a
    /// promise, not an outcome: any output before the grow back (even a
    /// spinner frame from an app that never saw the smaller size), an alt
    /// switch, a reset or a checkpoint restore drops the undo and the rows are
    /// then lost after all. Whether a later grow really handed them back is
    /// that grow's `restored_top`/`restored_bottom`, which is what `cast drift`
    /// folds.
    #[must_use]
    pub fn lossy(&self) -> bool {
        self.alt
            && self.demoted > 0
            && u32::from(self.stashed) < u32::from(self.demoted) + u32::from(self.pushed)
    }

    /// Nothing changed the screen between `prev` and this resize: the same
    /// screen was active and its content sequence did not move. Both halves of
    /// a flap that landed between two PTY reads adjoin; a resize the app drew
    /// in front of does not. Cursor moves are not content and do not count.
    #[must_use]
    pub fn adjoins(&self, prev: &ResizeReport) -> bool {
        self.alt == prev.alt && self.content_seq_before == prev.content_seq
    }

    /// The screen this resize displaced has since been repainted from scratch:
    /// `full_clears`/`screen_replaced` are the engine's
    /// [`Terminal::full_clear_count`] and [`Terminal::screen_replaced_count`]
    /// read later, and either one advanced PAST this report's. Both counters
    /// only grow, so a reading taken before this resize (a stale one, lower)
    /// can never pass for a heal; compare readings of one engine lifetime only
    /// ([`Terminal::resize_lifetime`]).
    #[must_use]
    pub fn healed_by(&self, full_clears: u64, screen_replaced: u64) -> bool {
        full_clears > self.full_clears || screen_replaced > self.screen_replaced
    }
}

/// The pre-resize facts a [`ResizeReport`] needs. Captured by each resize entry
/// point before it touches a grid, and consumed by `finalize_resize`.
#[derive(Clone, Copy, Debug)]
pub(super) struct ResizeOrigin {
    from: (u16, u16),
    alt: bool,
    in_sync: bool,
    content_seq: u64,
}

/// The active selection a grid resize undo will put back
/// (`Terminal::follow_resize_undo_selection`). Session-only, like the
/// selection it copies.
#[derive(Clone, Debug)]
pub(super) struct ResizeUndoSelection {
    /// The grid undo this record belongs to (`Grid::resize_undo_serial`).
    serial: u64,
    /// The selection before that undo's first shrink; `None` once something
    /// moved the selection between two of its resizes (nothing to put back).
    pre: Option<crate::selection::TextSelection>,
    /// The selection as the newest of those resizes left it.
    after: crate::selection::TextSelection,
}

/// The newest [`RESIZE_JOURNAL_CAP`] resize reports, the lifetime ordinal and
/// the lifetime token.
///
/// A fixed ring indexed by `(ordinal - 1) % CAP`, like
/// `ContentScrollState::bands`: a push never allocates. Session-only: never
/// checkpointed and never forwarded to the VT handler.
#[derive(Clone, Debug)]
pub(super) struct ResizeJournal {
    /// This engine's lifetime token ([`Terminal::resize_lifetime`]).
    lifetime: u64,
    /// Resizes recorded for this engine's lifetime; the newest report's ordinal.
    ordinal: u64,
    /// `ring[(o - 1) % CAP]` holds the report with ordinal `o`.
    ring: [ResizeReport; RESIZE_JOURNAL_CAP],
}

/// The next lifetime token. A counter, not a clock: it only tells engines apart
/// (`grep_guard` C3 holds), and it starts at 1 so a reader's 0 is never one.
static NEXT_LIFETIME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Default for ResizeJournal {
    /// An empty journal with a FRESH lifetime token: every engine built (new,
    /// restored from a checkpoint) gets its own.
    fn default() -> Self {
        Self {
            lifetime: NEXT_LIFETIME.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ordinal: 0,
            ring: [ResizeReport::default(); RESIZE_JOURNAL_CAP],
        }
    }
}

/// Ring slot of the report with ordinal `ordinal` (1-based).
fn slot(ordinal: u64) -> usize {
    usize::try_from(ordinal.wrapping_sub(1) % CAP_U64).unwrap_or_default()
}

impl ResizeJournal {
    /// Record `report` as the next resize and stamp its ordinal.
    fn push(&mut self, report: ResizeReport) {
        self.ordinal = self.ordinal.wrapping_add(1);
        if let Some(entry) = self.ring.get_mut(slot(self.ordinal)) {
            *entry = ResizeReport {
                ordinal: self.ordinal,
                ..report
            };
        }
    }

    /// The reports after ordinal `since`, oldest first, and how many of them the
    /// ring no longer holds. See [`Terminal::resize_journal_since`].
    fn since(&self, since: u64) -> (Vec<ResizeReport>, u64) {
        let since = if since > self.ordinal { 0 } else { since };
        let pending = self.ordinal - since;
        let kept = pending.min(CAP_U64);
        let reports = (self.ordinal - kept + 1..=self.ordinal)
            .filter_map(|o| self.ring.get(slot(o)).copied())
            .collect();
        (reports, pending - kept)
    }
}

/// Is an ED (`CSI Ps J`) with parameter `mode`, issued with the cursor at
/// `(row, col)`, a FULL clear of the visible screen?
///
/// ED 2 erases the whole screen. ED 0 erases from the cursor to the end, which
/// is the whole screen only from the home position. ED 3 is NOT one: it erases
/// the scrollback and leaves every visible row where it is (on the alternate
/// screen, which has none, it changes nothing a displaced screen shows); a
/// clearing app sends it together with ED 2, which counts. EL, ED 1 and DECSED
/// are never counted.
pub(super) fn is_full_clear(mode: u16, row: u16, col: u16) -> bool {
    match mode {
        2 => true,
        0 => row == 0 && col == 0,
        _ => false,
    }
}

impl Terminal {
    /// Capture the pre-resize facts for this resize's [`ResizeReport`]. Called
    /// by both resize entry points before either grid is resized.
    pub(super) fn resize_origin(&self) -> ResizeOrigin {
        ResizeOrigin {
            from: (self.grid.rows(), self.grid.cols()),
            alt: self.modes.alternate_screen,
            in_sync: self.modes.synchronized_output,
            content_seq: self.content_seq(),
        }
    }

    /// Journal the resize that just ran: drain the ACTIVE grid's row accounting
    /// and push one report. The saved grid records its own shape too, but only
    /// the screen the app is painting is journalled.
    /// Keep the active selection whole across a flap the grid undoes: record
    /// it as it stood before the first shrink of a resize undo, keep that record
    /// only while the same undo stays open and nothing moved the selection
    /// between resizes, and put it back when the grid reports the undo handed
    /// back whole (`undone`). `before` is the selection as this resize found
    /// it, before the rows-only transform ran.
    pub(super) fn follow_resize_undo_selection(
        &mut self,
        before: crate::selection::TextSelection,
        undone: bool,
    ) {
        let kept = self.resize_undo_selection.take();
        if undone {
            if let Some(ResizeUndoSelection {
                pre: Some(pre),
                after,
                ..
            }) = kept
                && after == before
            {
                self.text_selection = pre;
            }
            return;
        }
        let Some(serial) = self.grid.resize_undo_serial() else {
            return;
        };
        let pre = match kept {
            // The same undo, unbroken: keep the first shrink's selection, unless
            // something moved the selection since the last resize (then the
            // grid can hand the rows back but the selection has no pre-image).
            Some(k) if k.serial == serial => k.pre.filter(|_| k.after == before),
            // A fresh undo: this resize is its first shrink.
            _ => Some(before),
        };
        self.resize_undo_selection = Some(ResizeUndoSelection {
            serial,
            pre,
            after: self.text_selection.clone(),
        });
    }

    pub(super) fn journal_resize(&mut self, origin: ResizeOrigin) {
        let shape = self.grid.take_last_resize_shape();
        let report = ResizeReport {
            ordinal: 0,
            from: origin.from,
            to: (self.grid.rows(), self.grid.cols()),
            alt: origin.alt,
            in_sync: origin.in_sync,
            trimmed: shape.trimmed,
            demoted: shape.demoted,
            pushed: shape.pushed,
            revealed: shape.revealed,
            appended: shape.appended,
            stashed: shape.stashed,
            restored_top: shape.restored_top,
            restored_bottom: shape.restored_bottom,
            reflowed: shape.reflowed,
            content_seq: self.content_seq(),
            content_seq_before: origin.content_seq,
            full_clears: self.transient.full_clears,
            screen_replaced: self.transient.screen_replaced,
        };
        self.resize_journal.push(report);
    }

    /// How many resizes this engine has applied: the newest journal ordinal, 0
    /// before the first. Never reset (RIS included); a restored engine starts
    /// again at 0, under a new [`resize_lifetime`](Self::resize_lifetime).
    #[must_use]
    pub fn resize_ordinal(&self) -> u64 {
        self.resize_journal.ordinal
    }

    /// This engine's LIFETIME token: unique among the engines this process
    /// built (a counter, never 0) and fixed for the engine's life. Ordinals, `content_seq` and the two heal counters compare only
    /// within one lifetime: a reader that holds a reading of another lifetime
    /// (a restored engine) starts over rather than read a lower ordinal as a
    /// stale copy of this one.
    #[must_use]
    pub fn resize_lifetime(&self) -> u64 {
        self.resize_journal.lifetime
    }

    /// The resizes applied after ordinal `ordinal`, oldest first, plus how many
    /// of those the journal no longer holds (`lost`, more than
    /// [`RESIZE_JOURNAL_CAP`] behind).
    ///
    /// Pass 0 for everything retained, or the last ordinal already read. A
    /// watermark PAST [`resize_ordinal`](Self::resize_ordinal) was taken from
    /// another engine lifetime (a restored engine restarts at 0), so it is read
    /// as 0 instead of hiding the new engine's resizes.
    #[must_use]
    pub fn resize_journal_since(&self, ordinal: u64) -> (Vec<ResizeReport>, u64) {
        self.resize_journal.since(ordinal)
    }

    /// Monotonic count of full clears processed: ED 2, and ED 0 issued with the
    /// cursor at the home position (ED 3, a scrollback erase, is not one).
    /// Never reset (RIS included).
    #[must_use]
    pub fn full_clear_count(&self) -> u64 {
        self.transient.full_clears
    }

    /// Monotonic count of whole-screen replacements: every alternate-screen
    /// enter and exit (modes 47, 1047 and 1049, including a repeated 1049 set,
    /// which clears) and every full reset (RIS and [`Terminal::reset`]). Never
    /// reset.
    #[must_use]
    pub fn screen_replaced_count(&self) -> u64 {
        self.transient.screen_replaced
    }
}
