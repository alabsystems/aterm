// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The engine's resize journal and its screen-replacement counters.
//!
//! Only the engine knows what a resize did to the rows an application painted. A rows-only
//! shrink on the alternate screen (no history) drops the top row and the following grow
//! appends a blank one, so a 64→63→64 flap leaves the screen a row higher than painted; if
//! both halves land before the app's SIGWINCH handler runs, it sees no change and never
//! repaints. The grid undoes such a flap when nothing was drawn between its halves
//! (`aterm_grid`'s resize undo, reported as `stashed`/`restored_*`).
//!
//! A host cannot see a flap from geometry or a per-read watermark, so the engine records:
//!
//! * [`ResizeReport`]: geometry, the active screen, an open DEC 2026 frame, and the grid's
//!   row accounting ([`aterm_grid::ResizeShape`]).
//! * The journal: the newest [`RESIZE_JOURNAL_CAP`] reports by a 1-based ordinal that
//!   never resets, read through [`Terminal::resize_journal_since`]; an overrun is counted.
//! * The lifetime token ([`Terminal::resize_lifetime`]), unique per engine in this process,
//!   so a reader tells a stale copy from a restored engine.
//! * Two monotonic heal counters, [`Terminal::full_clear_count`] and
//!   [`Terminal::screen_replaced_count`], compared with a report's by
//!   [`ResizeReport::healed_by`].
//!
//! No clock is read (aterm-core's clippy.toml): the ordinal and `content_seq` order a
//! resize against output, and the host stamps wall time. Nothing is checkpointed.

use super::Terminal;
use crate::grid::{ResizeLaw, ResizePolicy};

#[cfg(test)]
#[path = "resize_journal_tests.rs"]
mod tests;

/// How many resizes the journal keeps. A net-zero chrome-row flap is two resizes and a
/// reader draining once per wake sees a handful; a window drag makes about 60 a second, so
/// a reader further behind loses the oldest, and [`Terminal::resize_journal_since`] counts
/// them.
pub const RESIZE_JOURNAL_CAP: usize = 16;

/// [`RESIZE_JOURNAL_CAP`] in ordinal arithmetic (a lossless widening).
const CAP_U64: u64 = RESIZE_JOURNAL_CAP as u64;

/// One resize as the engine applied it to the active grid. `from`/`to` are `(rows, cols)`,
/// `to` after the grid's ingress clamp. A call that repeats the current size is recorded
/// too (the grid still re-lays it out).
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
    /// Top rows demoted into history by a shrink: every surviving row moved up
    /// by this count.
    pub demoted: u16,
    /// Bottom rows pushed off the screen with nothing above them moving.
    pub pushed: u16,
    /// History lines a grow revealed at the top: every row moved down by this
    /// count.
    pub revealed: u16,
    /// Blank rows a grow appended at the bottom (nothing moved).
    pub appended: u16,
    /// Of `demoted + pushed`, the rows the resize undo kept (alternate screen only): a grow
    /// back with nothing drawn in between hands them back. All of them or none.
    pub stashed: u16,
    /// Of `revealed`, the rows the resize undo handed back to the top: an
    /// earlier shrink demoted them and nothing drew since, so the grow put them
    /// back where the app painted them.
    pub restored_top: u16,
    /// Rows put back at the bottom, where an earlier shrink pushed them off:
    /// by the resize undo, or from history by the rows hand-back (nothing
    /// moved; not counted in `appended`).
    pub restored_bottom: u16,
    /// A width change rewrapped the grid (a primary-screen reflow), which
    /// renumbers rows wholesale, so the counts above are not a displacement.
    pub reflowed: bool,
    /// Columns of drawn cells a narrowing with no rewrap cut off the right edge
    /// ([`aterm_grid::ResizeShape::cut`]): a grow back brings blanks there.
    pub cut: u16,
    /// The active grid's content sequence (`Terminal::content_seq`) after the resize, which
    /// advances it by itself: compare a live `content_seq` to learn whether the app drew
    /// since.
    pub content_seq: u64,
    /// The active grid's content sequence before the resize. Equal to the
    /// previous report's `content_seq` exactly when nothing changed the screen
    /// between the two resizes (see [`ResizeReport::adjoins`]).
    pub content_seq_before: u64,
    /// [`Terminal::full_clear_count`] when the resize landed.
    pub full_clears: u64,
    /// [`Terminal::screen_replaced_count`] when the resize landed.
    pub screen_replaced: u64,
}

impl ResizeReport {
    /// Rows this resize moved the painted content by (`revealed - demoted`; negative is up).
    /// Meaningful when `reflowed` is false. Rows the undo handed back are in `revealed`, so
    /// a quiet alt-screen flap sums to 0 as a primary-screen one does.
    #[inline]
    #[must_use]
    pub fn shift(&self) -> i32 {
        i32::from(self.revealed) - i32::from(self.demoted)
    }

    /// Rows this resize pushed off the bottom, net of the ones it put back
    /// there from the resize undo: positive for a shrink's bottom-push,
    /// negative for the grow that undid one.
    #[inline]
    #[must_use]
    pub fn net_pushed(&self) -> i32 {
        i32::from(self.pushed) - i32::from(self.restored_bottom)
    }

    /// The resize moved content no later resize can move back by itself: the alternate
    /// screen keeps no history, so a demoted row is dropped. Not lossy when the resize undo
    /// kept the rows (`stashed`). A promise made when this resize ran, not an outcome: any
    /// output before the grow back, an alt switch, a reset or a checkpoint restore drops
    /// the undo. Whether a later grow handed them back is its
    /// `restored_top`/`restored_bottom`, which `cast drift` folds.
    #[inline]
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
    #[inline]
    #[must_use]
    pub const fn adjoins(&self, prev: &ResizeReport) -> bool {
        self.alt == prev.alt && self.content_seq_before == prev.content_seq
    }

    /// The screen this resize displaced has since been repainted from scratch: either
    /// counter ([`Terminal::full_clear_count`], [`Terminal::screen_replaced_count`]) read
    /// later advanced past this report's. Both only grow, so a stale reading never passes
    /// for a heal; compare readings of one [`Terminal::resize_lifetime`] only.
    #[inline]
    #[must_use]
    pub const fn healed_by(&self, full_clears: u64, screen_replaced: u64) -> bool {
        full_clears > self.full_clears || screen_replaced > self.screen_replaced
    }
}

/// What the alternate screen owes its program ([`Terminal::render_debt`]): what its
/// resizes did to the cells the program drew that the program has not drawn again.
///
/// A program redraws on SIGWINCH, so a resize it sees is paid by its own repaint. A flap
/// whose halves both land before the handler runs shows it no change: whatever the
/// first half moved or cut stays as the second half left it. Rows and columns are the
/// same debt: [`owed`](Self::owed) is whether any of it is unpaid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderDebt {
    /// Rows the painted content moved (the sum of [`ResizeReport::shift`]): `-1` means
    /// every row sits one higher than drawn.
    pub shift: i32,
    /// Rows pushed off the bottom and not put back (the sum of
    /// [`ResizeReport::net_pushed`]).
    pub pushed: i32,
    /// Columns of drawn cells narrowings cut off the right edge (the sum of
    /// [`ResizeReport::cut`]): a grow back brought blanks where the program drew them.
    pub cut: u16,
}

impl RenderDebt {
    /// A debt of rows only, `(shift, pushed)`.
    #[inline]
    #[must_use]
    pub const fn of_rows(shift: i32, pushed: i32) -> Self {
        Self {
            shift,
            pushed,
            cut: 0,
        }
    }

    /// The rows owed, `(shift, pushed)`; `(0, 0)` when every row sits where it was drawn.
    #[inline]
    #[must_use]
    pub const fn rows(self) -> (i32, i32) {
        (self.shift, self.pushed)
    }

    /// Anything is owed: a row sits away from where it was drawn, or a drawn cell is gone.
    #[inline]
    #[must_use]
    pub const fn owed(self) -> bool {
        self.shift != 0 || self.pushed != 0 || self.cut != 0
    }

    /// The debt after `report`, one more non-reflowed alternate-screen resize: each count
    /// adds that resize's. A cut counts only the drawn cells still on the screen, so a
    /// column a former cut emptied is never counted twice.
    #[inline]
    #[must_use]
    pub fn after(self, report: &ResizeReport) -> Self {
        Self {
            shift: self.shift.saturating_add(report.shift()),
            pushed: self.pushed.saturating_add(report.net_pushed()),
            cut: self.cut.saturating_add(report.cut),
        }
    }
}

/// The pre-resize facts a [`ResizeReport`] needs, captured before a resize touches a grid.
#[derive(Clone, Copy, Debug)]
pub(super) struct ResizeOrigin {
    from: (u16, u16),
    alt: bool,
    in_sync: bool,
    content_seq: u64,
    /// The resize ran under [`ResizePolicy::Native`]. Under ConPTY conhost repaints the
    /// viewport after every resize, so such a session never accrues render debt.
    native: bool,
}

/// The active selection a grid resize undo will put back
/// (`Terminal::follow_resize_undo_selection`). Session-only.
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

/// The newest [`RESIZE_JOURNAL_CAP`] resize reports, the ordinal and the lifetime token. A
/// fixed ring indexed by `(ordinal - 1) % CAP`, so a push never allocates. Session-only:
/// never checkpointed, never forwarded to the VT handler.
#[derive(Clone, Debug)]
pub(super) struct ResizeJournal {
    /// This engine's lifetime token ([`Terminal::resize_lifetime`]).
    lifetime: u64,
    /// Resizes recorded for this engine's lifetime; the newest report's ordinal.
    ordinal: u64,
    /// `ring[(o - 1) % CAP]` holds the report with ordinal `o`.
    ring: [ResizeReport; RESIZE_JOURNAL_CAP],
}

/// The next lifetime token: a counter, not a clock, starting at 1 so a reader's 0 is never
/// one.
static NEXT_LIFETIME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Default for ResizeJournal {
    /// An empty journal with a fresh lifetime token: every engine built (new or restored)
    /// gets its own.
    fn default() -> Self {
        Self {
            lifetime: NEXT_LIFETIME.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ordinal: 0,
            ring: [ResizeReport::default(); RESIZE_JOURNAL_CAP],
        }
    }
}

/// Ring slot of the report with ordinal `ordinal` (1-based).
#[inline]
fn slot(ordinal: u64) -> usize {
    usize::try_from(ordinal.wrapping_sub(1) % CAP_U64).unwrap_or_default()
}

impl ResizeJournal {
    /// Record `report` as the next resize and stamp its ordinal.
    #[inline]
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

/// Is an ED (`CSI Ps J`) with parameter `mode`, at cursor `(row, col)`, a full clear of the
/// visible screen? ED 2 is; ED 0 is from the home position only. ED 3 erases the scrollback
/// and leaves every visible row in place (a clearing app sends it with ED 2, which counts).
/// EL, ED 1 and DECSED never count.
#[inline]
pub(super) const fn is_full_clear(mode: u16, row: u16, col: u16) -> bool {
    match mode {
        2 => true,
        0 => row == 0 && col == 0,
        _ => false,
    }
}

impl Terminal {
    /// The pre-resize facts for this resize's [`ResizeReport`], taken by both resize entry
    /// points before either grid is resized.
    #[inline]
    pub(super) const fn resize_origin(&self, policy: ResizePolicy) -> ResizeOrigin {
        ResizeOrigin {
            native: matches!(policy, ResizePolicy::Native),
            from: (self.grid.rows(), self.grid.cols()),
            alt: self.modes.alternate_screen,
            in_sync: self.modes.synchronized_output,
            content_seq: self.content_seq(),
        }
    }

    /// Keep the selection whole across a flap the grid undoes: record it as it stood before
    /// the undo's first shrink, keep the record while the same undo stays open and nothing
    /// moved the selection, and put it back when the grid hands the rows back (`undone`).
    /// `before` is the selection as this resize found it.
    pub(super) fn follow_resize_undo_selection(
        &mut self,
        before: crate::selection::TextSelection,
        undone: bool,
    ) {
        let kept = self.resize_undo_selection.take();
        if undone {
            if let Some(k) = kept
                && let Some(pre) = k.pre
                && k.after == before
            {
                self.text_selection = pre;
            }
            return;
        }
        let Some(serial) = self.grid.resize_undo_serial() else {
            return;
        };
        let pre = match kept {
            // The same undo: keep the first shrink's selection unless something moved the
            // selection since the last resize (then it has no pre-image).
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

    /// Journal the resize that just ran from the active grid's row accounting. Only the
    /// screen the app is painting is journalled.
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
            cut: shape.cut,
            content_seq: self.content_seq(),
            content_seq_before: origin.content_seq,
            full_clears: self.transient.full_clears,
            screen_replaced: self.transient.screen_replaced,
        };
        // Render debt is the alternate screen's only (the primary is history-backed and
        // moves with its cursor); a rewrap renumbers rows on its own, and under ConPTY
        // conhost repaints the viewport itself.
        if origin.native && report.alt && !report.reflowed {
            self.transient.render_debt = self.transient.render_debt.after(&report);
        }
        self.resize_journal.push(report);
    }

    /// What the alternate screen owes its program since it was last repainted from
    /// scratch ([`RenderDebt`]): the rows its non-reflowed `ResizePolicy::Native` resizes
    /// moved and the drawn columns they cut. Zeroed by a full clear, an alternate-screen
    /// switch and a reset; exact however far the journal was overrun.
    #[inline]
    #[must_use]
    pub const fn render_debt(&self) -> RenderDebt {
        self.transient.render_debt
    }

    /// Carry a predecessor's render debt across a handoff: once, right after a checkpoint
    /// restore, and only while the alternate screen is up (the debt is about its cells).
    #[inline]
    pub const fn seed_render_debt(&mut self, debt: RenderDebt) {
        if self.modes.alternate_screen {
            self.transient.render_debt = debt;
        }
    }

    /// Forget, on both grids, what earlier rows-only resizes owe back at one edge
    /// (`aterm_grid`'s rows hand-back): the blank rows trims took, which the next grow
    /// would append before revealing history, and the history lines grows revealed, which
    /// the next shrink would demote before trimming. The next resize then reveals or trims
    /// first, and runs as one pass over both axes, as an aterm before the hand-back did.
    #[inline]
    pub fn forget_handback(&mut self) {
        self.grid.forget_handback();
        if let Some(parked) = self.alt_grid.as_mut() {
            parked.forget_handback();
        }
    }

    /// Lay the rows out under `law` from the next resize on, on both grids and on every
    /// alternate buffer entered after (`aterm_grid`'s [`ResizeLaw`]): an older law for the
    /// replay of a recording made under it (`cast drift`, from the recording's
    /// `aterm_resize_undo`).
    #[inline]
    pub const fn set_resize_law(&mut self, law: ResizeLaw) {
        self.grid.set_resize_law(law);
        if let Some(parked) = self.alt_grid.as_mut() {
            parked.set_resize_law(law);
        }
    }

    /// The resize law the rows are laid out under ([`Self::set_resize_law`]).
    #[inline]
    #[must_use]
    pub const fn resize_law(&self) -> ResizeLaw {
        self.grid.resize_law()
    }

    /// How many resizes this engine has applied (the newest ordinal, 0 before the first).
    /// Never reset, RIS included; a restored engine starts at 0 under a new
    /// [`resize_lifetime`](Self::resize_lifetime).
    #[inline]
    #[must_use]
    pub const fn resize_ordinal(&self) -> u64 {
        self.resize_journal.ordinal
    }

    /// This engine's lifetime token: unique among the engines this process built (a counter,
    /// never 0) and fixed for the engine's life. Ordinals, `content_seq` and the two heal
    /// counters compare only within one lifetime.
    #[inline]
    #[must_use]
    pub const fn resize_lifetime(&self) -> u64 {
        self.resize_journal.lifetime
    }

    /// The resizes applied after ordinal `ordinal`, oldest first, and how many of those the
    /// journal no longer holds (more than [`RESIZE_JOURNAL_CAP`] behind). Pass 0 for
    /// everything retained, or the last ordinal read. A watermark past
    /// [`resize_ordinal`](Self::resize_ordinal) is from another engine lifetime, so it reads
    /// as 0 rather than hide the new engine's resizes.
    #[inline]
    #[must_use]
    pub fn resize_journal_since(&self, ordinal: u64) -> (Vec<ResizeReport>, u64) {
        self.resize_journal.since(ordinal)
    }

    /// Monotonic count of full clears processed: ED 2, and ED 0 issued with the
    /// cursor at the home position (ED 3, a scrollback erase, is not one).
    /// Never reset (RIS included).
    #[inline]
    #[must_use]
    pub const fn full_clear_count(&self) -> u64 {
        self.transient.full_clears
    }

    /// Monotonic count of whole-screen replacements: every alternate-screen enter and exit
    /// (modes 47, 1047 and 1049, including a repeated 1049 set, which clears) and every full
    /// reset (RIS and [`Terminal::reset`]). Never reset.
    #[inline]
    #[must_use]
    pub const fn screen_replaced_count(&self) -> u64 {
        self.transient.screen_replaced
    }
}
