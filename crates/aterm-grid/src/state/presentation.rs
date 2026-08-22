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
        self.damage.reset(visible_rows);
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
