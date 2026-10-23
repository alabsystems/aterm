// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Engine resize journal ([`ResizeReport`], [`aterm_grid::ResizeShape`]) and heal counters.

use super::Terminal;
use crate::grid::{ResizeLaw, ResizePolicy};

#[cfg(test)]
#[path = "resize_journal_tests.rs"]
mod tests;

/// How many resizes the journal keeps ([`Terminal::resize_journal_since`]).
pub const RESIZE_JOURNAL_CAP: usize = 16;

const CAP_U64: u64 = RESIZE_JOURNAL_CAP as u64;

/// One resize as the engine applied it to the active grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "three independent facts about one resize, not a state machine"
)]
pub struct ResizeReport {
    /// 1-based position in this engine's resize sequence.
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
    /// Top rows demoted into history by a shrink.
    pub demoted: u16,
    /// Bottom rows pushed off the screen with nothing above them moving.
    pub pushed: u16,
    /// History lines a grow revealed at the top.
    pub revealed: u16,
    /// Blank rows a grow appended at the bottom (nothing moved).
    pub appended: u16,
    /// Of `demoted + pushed`, the rows the resize undo kept (alternate screen only).
    pub stashed: u16,
    /// Of `revealed`, the rows the resize undo handed back to the top.
    pub restored_top: u16,
    /// Rows put back at the bottom, where an earlier shrink pushed them off.
    pub restored_bottom: u16,
    /// A width change rewrapped the grid (a primary-screen reflow).
    pub reflowed: bool,
    /// Drawn columns cut off the right edge by a narrowing ([`aterm_grid::ResizeShape::cut`]).
    pub cut: u16,
    /// The active grid's content sequence (`Terminal::content_seq`) after the resize.
    pub content_seq: u64,
    /// The active grid's content sequence before the resize ([`ResizeReport::adjoins`]).
    pub content_seq_before: u64,
    /// [`Terminal::full_clear_count`] when the resize landed.
    pub full_clears: u64,
    /// [`Terminal::screen_replaced_count`] when the resize landed.
    pub screen_replaced: u64,
}

impl ResizeReport {
    /// Rows this resize moved the painted content by (`revealed - demoted`; negative is up).
    #[inline]
    #[must_use]
    pub fn shift(&self) -> i32 {
        i32::from(self.revealed) - i32::from(self.demoted)
    }

    /// Rows this resize pushed off the bottom.
    #[inline]
    #[must_use]
    pub fn net_pushed(&self) -> i32 {
        i32::from(self.pushed) - i32::from(self.restored_bottom)
    }

    /// The resize moved content no later resize can move back by itself.
    #[inline]
    #[must_use]
    pub fn lossy(&self) -> bool {
        self.alt
            && self.demoted > 0
            && u32::from(self.stashed) < u32::from(self.demoted) + u32::from(self.pushed)
    }

    /// Nothing changed the screen between `prev` and this resize.
    #[inline]
    #[must_use]
    pub const fn adjoins(&self, prev: &ResizeReport) -> bool {
        self.alt == prev.alt && self.content_seq_before == prev.content_seq
    }

    /// Whether [`Terminal::full_clear_count`] or [`Terminal::screen_replaced_count`] advanced.
    #[inline]
    #[must_use]
    pub const fn healed_by(&self, full_clears: u64, screen_replaced: u64) -> bool {
        full_clears > self.full_clears || screen_replaced > self.screen_replaced
    }
}

/// Alternate-screen resize debt ([`Terminal::render_debt`], [`owed`](Self::owed)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderDebt {
    /// Rows the painted content moved (the sum of [`ResizeReport::shift`]).
    pub shift: i32,
    /// Rows pushed off the bottom and not put back (the sum of [`ResizeReport::net_pushed`]).
    pub pushed: i32,
    /// Columns of drawn cells narrowings cut off the right edge ([`ResizeReport::cut`]).
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

    /// The rows owed, `(shift, pushed)`.
    #[inline]
    #[must_use]
    pub const fn rows(self) -> (i32, i32) {
        (self.shift, self.pushed)
    }

    /// Anything is owed.
    #[inline]
    #[must_use]
    pub const fn owed(self) -> bool {
        self.shift != 0 || self.pushed != 0 || self.cut != 0
    }

    /// The debt after `report`, one more non-reflowed alternate-screen resize.
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
    /// The resize ran under [`ResizePolicy::Native`].
    native: bool,
}

/// Active selection preserved across a grid resize undo.
#[derive(Clone, Debug)]
pub(super) struct ResizeUndoSelection {
    /// The grid undo this record belongs to (`Grid::resize_undo_serial`).
    serial: u64,
    /// The selection before that undo's first shrink.
    pre: Option<crate::selection::TextSelection>,
    /// The selection as the newest of those resizes left it.
    after: crate::selection::TextSelection,
}

/// The newest [`RESIZE_JOURNAL_CAP`] resize reports, the ordinal and the lifetime token.
#[derive(Clone, Debug)]
pub(super) struct ResizeJournal {
    /// This engine's lifetime token ([`Terminal::resize_lifetime`]).
    lifetime: u64,
    /// Resizes recorded for this engine's lifetime.
    ordinal: u64,
    /// `ring[(o - 1) % CAP]` holds the report with ordinal `o`.
    ring: [ResizeReport; RESIZE_JOURNAL_CAP],
}

static NEXT_LIFETIME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Default for ResizeJournal {
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

    /// Reports after `since` and the overrun count ([`Terminal::resize_journal_since`]).
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

/// Whether `CSI Ps J` with `mode` at `(row, col)` is a full clear of the visible screen.
#[inline]
pub(super) const fn is_full_clear(mode: u16, row: u16, col: u16) -> bool {
    match mode {
        2 => true,
        0 => row == 0 && col == 0,
        _ => false,
    }
}

impl Terminal {
    /// The pre-resize facts for this resize's [`ResizeReport`].
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

    /// Keep the selection whole across a flap the grid undoes.
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
            Some(k) if k.serial == serial => k.pre.filter(|_| k.after == before),
            _ => Some(before),
        };
        self.resize_undo_selection = Some(ResizeUndoSelection {
            serial,
            pre,
            after: self.text_selection.clone(),
        });
    }

    /// Journal the resize that just ran from the active grid's row accounting.
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
        if origin.native && report.alt && !report.reflowed {
            self.transient.render_debt = self.transient.render_debt.after(&report);
        }
        self.resize_journal.push(report);
    }

    /// Unpaid alternate-screen resize displacement and column cuts ([`RenderDebt`]).
    #[inline]
    #[must_use]
    pub const fn render_debt(&self) -> RenderDebt {
        self.transient.render_debt
    }

    /// Carry a predecessor's render debt across a handoff.
    #[inline]
    pub const fn seed_render_debt(&mut self, debt: RenderDebt) {
        if self.modes.alternate_screen {
            self.transient.render_debt = debt;
        }
    }

    /// Clear pending rows-only resize hand-back state on both grids.
    #[inline]
    pub fn forget_handback(&mut self) {
        self.grid.forget_handback();
        if let Some(parked) = self.alt_grid.as_mut() {
            parked.forget_handback();
        }
    }

    /// Set the [`ResizeLaw`] for future resizes on both grids.
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

    /// How many resizes this engine has applied ([`resize_lifetime`]).
    #[inline]
    #[must_use]
    pub const fn resize_ordinal(&self) -> u64 {
        self.resize_journal.ordinal
    }

    /// This engine's lifetime token.
    #[inline]
    #[must_use]
    pub const fn resize_lifetime(&self) -> u64 {
        self.resize_journal.lifetime
    }

    /// Resizes after `ordinal` ([`RESIZE_JOURNAL_CAP`], [`resize_ordinal`](Self::resize_ordinal)).
    #[inline]
    #[must_use]
    pub fn resize_journal_since(&self, ordinal: u64) -> (Vec<ResizeReport>, u64) {
        self.resize_journal.since(ordinal)
    }

    /// Monotonic count of full clears processed.
    #[inline]
    #[must_use]
    pub const fn full_clear_count(&self) -> u64 {
        self.transient.full_clears
    }

    /// Monotonic count of alternate-screen switches and full resets ([`Terminal::reset`]).
    #[inline]
    #[must_use]
    pub const fn screen_replaced_count(&self) -> u64 {
        self.transient.screen_replaced
    }
}
