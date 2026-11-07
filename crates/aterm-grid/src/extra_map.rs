// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Sparse extras table with retained allocation accounting across erasures.

use crate::{CellCoord, CellExtra};
use aterm_hash::FxHashMap;
use std::collections::hash_map::{Drain, Entry};
use std::ops::Deref;

#[derive(Debug, Default)]
pub(crate) struct ExtraMap {
    entries: FxHashMap<CellCoord, CellExtra>,
    // Erasure can consume capacity as tombstones without freeing table buckets.
    // Remember capacity before erasing; reset only when replacing the allocation.
    before_erase: usize,
}

impl Clone for ExtraMap {
    fn clone(&self) -> Self {
        // Rebuild from live entries: a raw table clone can retain tombstones and
        // spare buckets whose allocation its erased capacity no longer names.
        Self {
            entries: self
                .entries
                .iter()
                .map(|(key, extra)| (*key, extra.clone()))
                .collect(),
            before_erase: 0,
        }
    }
}

impl Deref for ExtraMap {
    type Target = FxHashMap<CellCoord, CellExtra>;
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl ExtraMap {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: FxHashMap::with_capacity_and_hasher(capacity, aterm_hash::FxBuildHasher),
            before_erase: 0,
        }
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        aterm_alloc::heap::table_bytes::<(CellCoord, CellExtra)>(self.retained_capacity())
    }

    #[inline]
    pub(crate) fn retained_capacity(&self) -> usize {
        self.capacity().max(self.before_erase)
    }

    #[inline]
    pub(crate) fn get_or_default(&mut self, key: CellCoord) -> &mut CellExtra {
        self.entries.entry(key).or_default()
    }

    /// Reset the writer's slot, returning whether its previous value held a link.
    #[inline]
    pub(crate) fn fresh(&mut self, key: CellCoord) -> (&mut CellExtra, bool) {
        match self.entries.entry(key) {
            Entry::Vacant(slot) => (slot.insert(CellExtra::default()), false),
            Entry::Occupied(slot) => {
                let extra = slot.into_mut();
                let had_link = extra.hyperlink().is_some();
                *extra = CellExtra::default();
                (extra, had_link)
            }
        }
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, key: &CellCoord) -> Option<&mut CellExtra> {
        self.entries.get_mut(key)
    }

    #[inline]
    pub(crate) fn insert(&mut self, key: CellCoord, extra: CellExtra) -> Option<CellExtra> {
        self.entries.insert(key, extra)
    }

    #[inline]
    fn remember_allocation(&mut self) {
        self.before_erase = self.before_erase.max(self.entries.capacity());
    }

    #[inline]
    pub(crate) fn remove(&mut self, key: &CellCoord) -> Option<CellExtra> {
        self.remember_allocation();
        self.entries.remove(key)
    }

    pub(crate) fn retain(&mut self, keep: impl FnMut(&CellCoord, &mut CellExtra) -> bool) {
        self.remember_allocation();
        self.entries.retain(keep);
    }

    pub(crate) fn drain(&mut self) -> Drain<'_, CellCoord, CellExtra> {
        self.remember_allocation();
        self.entries.drain()
    }

    pub(crate) fn clear(&mut self) {
        self.remember_allocation();
        self.entries.clear();
    }

    pub(crate) fn shrink_to_fit(&mut self) {
        // Build a known fresh table: erased capacity cannot tell whether std's
        // shrink compacted tombstones in place or replaced the allocation.
        let mut fresh =
            FxHashMap::with_capacity_and_hasher(self.entries.len(), aterm_hash::FxBuildHasher);
        fresh.extend(self.entries.drain());
        self.entries = fresh;
        self.before_erase = 0;
    }
}
