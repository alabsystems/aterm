// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Direct-mapped memo of materialized history rows for a scrolled-back viewport.

use std::cell::RefCell;
use std::sync::Arc;

use super::MaterializedRow;

const MIN_SLOTS: usize = 8;

const MAX_SLOTS: usize = 512;

/// The identity of the history a set of memo slots was filled from.
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
    epoch: Option<HistoryEpoch>,
    slots: Vec<Option<Slot>>,
}

impl Inner {
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
    #[inline]
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
#[inline]
fn capacity_for(visible_rows: u16) -> usize {
    usize::from(visible_rows)
        .saturating_mul(2)
        .clamp(MIN_SLOTS, MAX_SLOTS)
}

/// The per-`Grid` memo.
#[derive(Debug, Default)]
pub(in crate::grid) struct ViewportRowCache {
    inner: RefCell<Inner>,
}

impl ViewportRowCache {
    /// The memoized row for `abs_row` under `epoch`, or `None` on a miss.
    #[inline]
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

    /// Memoize `row` for `abs_row` under `epoch`, replacing the slot's occupant.
    #[inline]
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
