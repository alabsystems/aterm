// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Memo of materialized history rows for a scrolled-back viewport.
//!
//! [`Grid::visible_row_view`](crate::Grid::visible_row_view) rebuilds a whole
//! [`MaterializedRow`](super::MaterializedRow) from the 3-tier store for every history row,
//! and the frame path (`Terminal::cell_frame_into`) walks every row each frame. This
//! direct-mapped memo, keyed by absolute row number, turns frames where history did not
//! change (cursor blinks, effects, selection drags) and overlapping scrolls into hits.
//!
//! The whole cache is stamped with a [`HistoryEpoch`]; a mismatch drops every slot:
//!
//! * `content_gen`: bumped once per content mutation, including paths that assign
//!   `Damage::Full` directly (width reflow); a pure viewport change (`scroll_display`) leaves
//!   it, so a wheel scrub keeps its hits.
//! * `history_renumber_epoch`: bumped when history rows' absolute numbers shift (width
//!   reflow, Kitty CSI +T unscroll; see `GridStorage::history_renumber_epoch`).
//! * `cols` / `visible_rows`: materialization depends on width, and the slot count on height.
//!
//! Retention eviction needs no key: survivors keep their absolute numbers, so an evicted
//! row's slot is unreachable. Debug builds re-materialize every hit and compare
//! (`Grid::materialized_history_row`), so a stale hit fails loudly.

use std::cell::RefCell;
use std::sync::Arc;

use super::MaterializedRow;

/// Fewest slots the memo ever holds. Keeps a tiny grid (a 1-row test harness,
/// a strip) from thrashing a 2-slot cache.
const MIN_SLOTS: usize = 8;

/// Most slots the memo ever holds (the memory bound; one slot is one
/// materialized row). A pathologically tall grid trades hit rate for a bounded
/// footprint.
const MAX_SLOTS: usize = 512;

/// The identity of the history a set of memo slots was filled from. Any change
/// drops every slot — see the module docs for why each term is here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grid) struct HistoryEpoch {
    /// `GridStorage::content_gen` — the authority on "content changed".
    pub(in crate::grid) content_gen: u64,
    /// `GridStorage::history_renumber_epoch` — absolute keys shifted wholesale.
    pub(in crate::grid) renumber: u64,
    /// Width the rows were materialized at.
    pub(in crate::grid) cols: u16,
    /// Viewport height, which sizes the slot table.
    pub(in crate::grid) visible_rows: u16,
}

/// One filled slot: the absolute row it holds and the shared row. `Arc`, not a
/// `Ref` guard: `visible_row_view` takes `&self`, and two live row views must
/// not panic.
#[derive(Debug)]
struct Slot {
    abs_row: u64,
    row: Arc<MaterializedRow>,
}

#[derive(Debug, Default)]
struct Inner {
    /// `None` until the first fill; `Some` stamps what the slots describe.
    epoch: Option<HistoryEpoch>,
    /// Direct-mapped table indexed by `abs_row % slots.len()`.
    slots: Vec<Option<Slot>>,
}

impl Inner {
    /// Bring the table in line with `epoch`, dropping every slot if the history
    /// it describes is not the history being read.
    fn sync(&mut self, epoch: HistoryEpoch) {
        if self.epoch == Some(epoch) {
            return;
        }
        self.epoch = Some(epoch);
        let capacity = capacity_for(epoch.visible_rows);
        self.slots.clear();
        self.slots.resize_with(capacity, || None);
    }

    /// The slot an absolute row maps to, or `None` for an empty table.
    fn index_of(&self, abs_row: u64) -> Option<usize> {
        let len = u64::try_from(self.slots.len()).ok()?;
        if len == 0 {
            return None;
        }
        usize::try_from(abs_row % len).ok()
    }
}

/// Slots for a viewport: two screens' worth, so a reversed wheel scrub still
/// hits and no two rows of one viewport collide.
fn capacity_for(visible_rows: u16) -> usize {
    usize::from(visible_rows)
        .saturating_mul(2)
        .clamp(MIN_SLOTS, MAX_SLOTS)
}

/// The per-`Grid` memo. Interior-mutable because `Grid::visible_row_view` takes `&self`;
/// `Grid` is already `!Sync` (its `StyleTable` is).
#[derive(Debug, Default)]
pub(in crate::grid) struct ViewportRowCache {
    inner: RefCell<Inner>,
}

impl ViewportRowCache {
    /// The memoized row for `abs_row` under `epoch`, or `None` on a miss.
    /// Syncing to `epoch` is part of the lookup, so a caller cannot forget it.
    pub(in crate::grid) fn lookup(
        &self,
        epoch: HistoryEpoch,
        abs_row: u64,
    ) -> Option<Arc<MaterializedRow>> {
        let mut inner = self.inner.borrow_mut();
        inner.sync(epoch);
        let idx = inner.index_of(abs_row)?;
        match &inner.slots[idx] {
            Some(slot) if slot.abs_row == abs_row => Some(Arc::clone(&slot.row)),
            _ => None,
        }
    }

    /// Memoize `row` for `abs_row` under `epoch`, replacing the slot's occupant. The borrow
    /// lives only inside this call, never across the materialize that produced `row`, so the
    /// `RefCell` cannot panic.
    pub(in crate::grid) fn store(
        &self,
        epoch: HistoryEpoch,
        abs_row: u64,
        row: &Arc<MaterializedRow>,
    ) {
        let mut inner = self.inner.borrow_mut();
        inner.sync(epoch);
        let Some(idx) = inner.index_of(abs_row) else {
            return;
        };
        inner.slots[idx] = Some(Slot {
            abs_row,
            row: Arc::clone(row),
        });
    }
}
