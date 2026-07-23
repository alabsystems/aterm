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
