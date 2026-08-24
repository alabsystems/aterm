// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Presentation-oriented grid state.

use crate::damage::Damage;
use crate::extra::{CellCoord, CellExtra};
use crate::extra_collection::CellExtras;
use crate::style::StyleTable;

/// Change to the grid's monotonic absolute-row coordinate space.
///
/// Top-anchored partial scrolling inserts new logical rows immediately before
/// the protected footer. Consumers with durable row-attached metadata must
/// apply this update in order, or discard that metadata when exact composition
/// is no longer available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbsoluteRowUpdate {
    /// Insert `inserted` logical rows at `at`.
    Splice {
        /// First old absolute row shifted by the insertion.
        at: u64,
        /// Number of logical rows inserted.
        inserted: u64,
    },
    /// Multiple non-composable splices occurred before the consumer drained
    /// them, so durable absolute-row metadata must be invalidated.
    Invalidate,
}

/// SELECTION CUSTODY Phase 4 — WHERE this batch damaged content, in ABSOLUTE rows.
///
/// The thing it replaces: `content_scroll_delta = i32::MAX`, a sentinel meaning
/// "some region op happened, kill the selection". It was applied backwards. A status
/// bar scrolling rows 18-23 destroyed a highlight anchored at row -40 in scrollback,
/// content it never touched — while a `\r` + EL progress bar rewrote the row UNDER a
/// live highlight and left it in place, so a copy returned text the user never
/// selected. Damage is a QUESTION ABOUT OVERLAP, and the sentinel could not express
/// one.
///
/// Absolute rows are frame-invariant, so bands recorded at different points in one
/// parser batch compose by hull-union with no ordering reasoning — that is what makes
/// the composition sound. The lattice is `None ⊑ Band ⊑ All`, and `union` is monotone.
///
/// KNOWN IMPRECISION, deliberate: two disjoint bands in one batch hull-union into a
/// band covering the gap between them, over-clearing a selection sitting strictly
/// inside it. A set of bands is unbounded state on a hot path. This fails SAFE —
/// over-clear, never a stale highlight over changed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionDamage {
    /// Nothing in this batch replaced content a selection could be sitting on.
    #[default]
    None,
    /// Absolute rows `lo_abs..=hi_abs` were moved or rewritten.
    Band { lo_abs: u64, hi_abs: u64 },
    /// The whole coordinate space is gone (ED 3, `clear_scrollback`, RIS, a Kitty
    /// unscroll that renumbers history wholesale). No band can describe it.
    All,
}

impl SelectionDamage {
    /// Join on the lattice: hull for two bands, `All` absorbs, `None` is the unit.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        match (self, other) {
            (Self::All, _) | (_, Self::All) => Self::All,
            (Self::None, x) | (x, Self::None) => x,
            (
                Self::Band {
                    lo_abs: a_lo,
                    hi_abs: a_hi,
                },
                Self::Band {
                    lo_abs: b_lo,
                    hi_abs: b_hi,
                },
            ) => Self::Band {
                lo_abs: a_lo.min(b_lo),
                hi_abs: a_hi.max(b_hi),
            },
        }
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub struct GridPresentationState {
    /// Damage tracking.
    pub damage: Damage,
    /// Cell extras (hyperlinks, combining chars, underline colors).
    /// Stored separately from cells to keep the common case fast.
    pub extras: CellExtras,
    /// Style deduplication table (Ghostty pattern).
    /// Interns unique styles and provides IDs for memory-efficient storage.
    /// Typical terminals have 50-200 unique styles, providing ~67% memory savings.
    pub styles: StyleTable,
    /// Accumulated content scroll delta since last `take_content_scroll_delta()`.
    /// Used by Terminal to adjust selection coordinates after processing.
    /// Positive = content scrolled up by this many lines.
    /// `i32::MAX` = region scroll (forces selection clear).
    pub content_scroll_delta: i32,
    /// SELECTION CUSTODY Phase 4: where this batch damaged content, in absolute
    /// rows. Accumulated by `union` at each grid site, drained once per batch by
    /// `take_selection_damage()`.
    pub selection_damage: SelectionDamage,
    /// Rows of downward shift the most recent resize applied to the viewport by
    /// REVEALING retained history (a rows-grow). One-shot; drained by
    /// `Terminal::finalize_resize`.
    ///
    /// `Grid::resize_with_reflow_mode` already follows this shift for the cursor and
    /// the saved cursor — "every pre-resize viewport row now sits `revealed` rows
    /// further down". The SELECTION needs the same compensation, and until Phase 3
    /// narrowed `finalize_resize` it did not notice: the unconditional clear masked
    /// it. With the clear gone, uncompensated anchors sit `revealed` rows above their
    /// content, which is a WRONG-COPY path, not a cosmetic drift.
    pub last_resize_row_shift: u16,
    /// SELECTION CUSTODY Phase 4: whether this batch moved rows in a way that makes a
    /// HOST's cached grid coordinates untranslatable.
    ///
    /// This was the `content_scroll_delta = i32::MAX` sentinel's SECOND job, and it is
    /// a different question from the selection lattice above. The epoch asks "did
    /// coordinates MOVE?"; the lattice asks "was CONTENT replaced?". A region scroll
    /// is both. An EL or a DECERA is only the latter — it rewrites cells without
    /// moving any row — which is why the two cannot be derived from one another.
    pub coordinates_invalidated: bool,
    /// Pending logical-row insertion for durable absolute-row metadata.
    pub pending_absolute_row_update: Option<AbsoluteRowUpdate>,
    /// Independent copy retained until terminal post-processing remaps the
    /// active text selection. Metadata may drain its copy mid-parser to
    /// preserve OSC ordering; selection is adjusted once per complete batch.
    pub pending_selection_row_update: Option<AbsoluteRowUpdate>,
    /// D-2 PER-ROW CONTENT REVISION: `row_rev[r]` is the value of
    /// [`row_rev_clock`](Self::row_rev_clock) at the most recent fold that saw
    /// visible row `r` damaged. One `u64` per VISIBLE row (never per scrollback
    /// row) — the damage tracker's own coordinate space.
    ///
    /// The contract consumers rely on, and the ONLY one:
    ///
    /// > for two folds F1 before F2, `row_rev[r]` differs between them IF row
    /// > `r`'s content changed between F1 and F2.
    ///
    /// The converse does NOT hold and must never be assumed: an unchanged row
    /// may still be re-stamped (a redundant write, a `Damage::Full` session,
    /// a foreign consumer's take). Over-reporting costs a repaint;
    /// under-reporting is a stale frame, so every doubt resolves toward a
    /// fresh stamp.
    ///
    /// This is the row-identity-STABLE half of the damage fact only. Anything
    /// that MOVES content between row indices without marking every moved row
    /// (`mark_scroll_damage` marks only the exposed strip) is invisible here —
    /// which is why consumers must additionally pin `base_y`,
    /// `absolute_row_revision`, the alt bit and a zero `display_offset` before
    /// comparing two snapshots' stamps. See
    /// `aterm_render::compute_dirty_rows`.
    pub row_rev: Vec<u64>,
    /// Monotone clock the fold stamps [`row_rev`](Self::row_rev) with. Advanced
    /// once per fold that observes NEW damage, so two folds with nothing between
    /// them leave every row's revision untouched (an idle frame stays an exact
    /// gate hit rather than degrading into a whole-screen repaint).
    pub row_rev_clock: u64,
    /// The tracker mark-clock ([`crate::damage::DamageTracker::mark_seq`]) as of
    /// the last fold. The fold is a no-op while this is unchanged, which is what
    /// makes back-to-back folds (the extract's and the following
    /// `take_damage`'s) idempotent instead of double-stamping every damaged row.
    pub row_rev_folded_seq: u64,
}

fn coalesce_row_splice(
    pending: Option<AbsoluteRowUpdate>,
    at: u64,
    inserted: u64,
) -> AbsoluteRowUpdate {
    match pending {
        None => AbsoluteRowUpdate::Splice { at, inserted },
        Some(AbsoluteRowUpdate::Splice {
            at: previous_at,
            inserted: previous_inserted,
        }) if at == previous_at || at == previous_at.saturating_add(previous_inserted) => {
            AbsoluteRowUpdate::Splice {
                at: previous_at,
                inserted: previous_inserted.saturating_add(inserted),
            }
        }
        Some(AbsoluteRowUpdate::Splice { .. } | AbsoluteRowUpdate::Invalidate) => {
            AbsoluteRowUpdate::Invalidate
        }
    }
}

/// Advance the D-2 row-revision clock, SKIPPING zero.
///
/// Zero is the "no stamp / do not trust" sentinel a snapshot carries for a row
/// no engine fold has ever vouched for, so the clock must never mint it — a
/// wrapped clock handing out 0 would turn a real revision into "unknown",
/// which the consumer answers with the brute-force compare (safe), but a
/// SECOND wrap could then hand 0 to two snapshots and make them compare equal.
#[inline]
const fn next_row_rev_clock(clock: u64) -> u64 {
    match clock.wrapping_add(1) {
        0 => 1,
        next => next,
    }
}

impl GridPresentationState {
    #[cfg(kani)]
    pub(crate) fn kani_stub() -> Self {
        Self {
            damage: Damage::Full,
            extras: CellExtras::new(),
            styles: StyleTable::kani_stub(),
            content_scroll_delta: 0,
            selection_damage: SelectionDamage::None,
            last_resize_row_shift: 0,
            coordinates_invalidated: false,
            pending_absolute_row_update: None,
            pending_selection_row_update: None,
            row_rev: Vec::new(),
            row_rev_clock: 0,
            row_rev_folded_seq: 0,
        }
    }

    /// D-2: fold the CURRENT damage session into the per-row revisions.
    ///
    /// Idempotent while nothing new has been marked, so the two folds a frame
    /// performs — the extract's and the following `take_damage`'s — stamp a
    /// damaged row exactly ONCE between them. Both folds are required:
    ///
    /// * the EXTRACT's fold publishes this session's damage to the snapshot
    ///   being filled, and
    /// * the RESET's fold captures damage that a FOREIGN consumer is about to
    ///   discard (`take_damage` clears the bits for everyone), which no later
    ///   fold could recover.
    ///
    /// `Damage::Full` re-stamps unconditionally: it keeps no mark clock (see
    /// [`Damage::mark_seq`]), so "nothing new since the last fold" is not a
    /// question it can answer, and answering it wrongly is a stale frame. A
    /// full-damage frame repaints everything anyway, so the cost of the
    /// unconditional re-stamp lands only on frames that were never cheap.
    pub(crate) fn fold_row_revisions(&mut self, visible_rows: u16) {
        let rows = usize::from(visible_rows);
        // A row-count change is a full re-resolve for every consumer (resize
        // marks full damage), so rebuild the lane rather than carrying stamps
        // across a geometry the old indices no longer describe.
        if self.row_rev.len() != rows {
            self.row_rev.clear();
            self.row_rev.resize(rows, 0);
            self.row_rev_folded_seq = 0;
        }
        match self.damage.mark_seq() {
            // Partial: skip when no mark has been made since the last fold.
            Some(seq) => {
                if seq == self.row_rev_folded_seq {
                    return;
                }
                self.row_rev_folded_seq = seq;
                self.row_rev_clock = next_row_rev_clock(self.row_rev_clock);
                let clock = self.row_rev_clock;
                for row in self.damage.damaged_rows(visible_rows) {
                    if let Some(slot) = self.row_rev.get_mut(usize::from(row)) {
                        *slot = clock;
                    }
                }
            }
            // Full: no clock to consult, so re-stamp every row.
            None => {
                self.row_rev_clock = next_row_rev_clock(self.row_rev_clock);
                self.row_rev.fill(self.row_rev_clock);
            }
        }
    }

    #[inline]
    pub(crate) fn take_content_scroll_delta(&mut self) -> i32 {
        let delta = self.content_scroll_delta;
        self.content_scroll_delta = 0;
        delta
    }

    /// Drain this batch's accumulated selection damage.
    #[inline]
    pub(crate) fn take_selection_damage(&mut self) -> SelectionDamage {
        std::mem::take(&mut self.selection_damage)
    }

    /// Drain this batch's host-coordinate invalidation flag.
    #[inline]
    pub(crate) fn take_coordinates_invalidated(&mut self) -> bool {
        std::mem::take(&mut self.coordinates_invalidated)
    }

    /// Drain the most recent resize's revealed-history row shift.
    #[inline]
    pub(crate) fn take_last_resize_row_shift(&mut self) -> u16 {
        std::mem::take(&mut self.last_resize_row_shift)
    }

    /// Record a logical-row insertion, coalescing the consecutive form emitted
    /// by repeated scrolls through one top-anchored region.
    pub(crate) fn record_absolute_row_splice(&mut self, at: u64, inserted: u64) {
        debug_assert!(inserted > 0);
        self.pending_absolute_row_update = Some(coalesce_row_splice(
            self.pending_absolute_row_update,
            at,
            inserted,
        ));
        self.pending_selection_row_update = Some(coalesce_row_splice(
            self.pending_selection_row_update,
            at,
            inserted,
        ));
    }

    #[inline]
    pub(crate) fn take_absolute_row_update(&mut self) -> Option<AbsoluteRowUpdate> {
        self.pending_absolute_row_update.take()
    }

    #[inline]
    pub(crate) fn take_selection_row_update(&mut self) -> Option<AbsoluteRowUpdate> {
        self.pending_selection_row_update.take()
    }

    #[must_use]
    #[inline]
    pub(crate) fn damage(&self) -> &Damage {
        &self.damage
    }

    #[inline]
    pub(crate) fn damage_mut(&mut self) -> &mut Damage {
        &mut self.damage
    }

    #[must_use]
    #[inline]
    pub(crate) fn extras(&self) -> &CellExtras {
        &self.extras
    }

    #[inline]
    pub(crate) fn extras_mut(&mut self) -> &mut CellExtras {
        &mut self.extras
    }

    #[must_use]
    #[inline]
    pub(crate) fn styles(&self) -> &StyleTable {
        &self.styles
    }

    #[inline]
    pub(crate) fn styles_mut(&mut self) -> &mut StyleTable {
        &mut self.styles
    }

    #[must_use]
    #[inline]
    pub(crate) fn cell_extra(&self, row: u16, col: u16) -> Option<&CellExtra> {
        self.extras.get(CellCoord::new(row, col))
    }

    #[inline]
    pub(crate) fn clear_damage(&mut self, visible_rows: u16) {
        // D-2: capture this session's damage BEFORE discarding it. `reset` is
        // the one choke point every consumer funnels through, so a foreign
        // consumer that takes the damage between two of OUR extracts cannot
        // erase a row change from the revision lane.
        self.fold_row_revisions(visible_rows);
        let was_full = self.damage.is_full();
        self.damage.reset(visible_rows);
        if was_full {
            // `reset` from `Full` installs a FRESH tracker whose mark clock
            // restarts at 0. Rewinding the fold's watermark in lockstep keeps
            // "no mark since the last fold" honest; leaving it high would make
            // the next real mark compare equal and skip the fold.
            self.row_rev_folded_seq = 0;
        }
    }

    pub(crate) fn mark_scroll_damage(&mut self, visible_rows: u16, n: usize) {
        let rows = usize::from(visible_rows);
        if n >= rows {
            self.damage.mark_full();
        } else {
            // n < rows <= u16::MAX, so (rows - n) and rows both fit in u16.
            // Equivalent to the previous per-row `mark_row` loop, but a single
            // range op (the previous `u16::try_from(...).unwrap_or(MAX)` could
            // never saturate here since i < rows <= u16::MAX).
            #[allow(
                clippy::cast_possible_truncation,
                reason = "rows = u16::from(visible_rows) and n < rows, so both bounds fit in u16"
            )]
            self.damage.mark_rows((rows - n) as u16, rows as u16);
        }
    }
}
