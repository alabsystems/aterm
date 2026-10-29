// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Ring-history out-of-line row extras and uncompressed OSC 8 hyperlink byte budget.

use std::collections::VecDeque;

use super::{Grid, ScrolledRowExtras};

/// Ring history slot (`None` for rows without out-of-line extras).
pub(crate) type RingSlot = Option<Box<ScrolledRowExtras>>;

/// The fraction of the scrollback memory budget that OSC 8 payload in
/// uncompressed history (ring + staged rows) may use: a quarter.
const HISTORY_LINK_BUDGET_DIVISOR: usize = 4;

/// [`RingSlot`]'s out-of-line bytes: `(retained, links)`, as
/// [`ScrolledRowExtras::retained_memory_used`] and
/// [`ScrolledRowExtras::link_bytes`] count them.
#[inline]
fn slot_bytes(slot: &RingSlot) -> (usize, usize) {
    slot.as_deref()
        .map_or((0, 0), |e| (e.retained_memory_used(), e.link_bytes()))
}

/// Per-row out-of-line extras for the scrollback ring with running byte totals.
#[derive(Debug, Default)]
pub(crate) struct RingExtras {
    slots: VecDeque<RingSlot>,
    /// Sum of [`slot_bytes`]' retained bytes over `slots`.
    retained: usize,
    /// Sum of [`slot_bytes`]' link bytes over `slots`.
    links: usize,
    /// `slots[..linkless_front]` hold no link bytes: where the next
    /// [`strip_oldest_links`](Self::strip_oldest_links) resumes, so a flood
    /// visits each row once.
    linkless_front: usize,
}

impl RingExtras {
    /// Whether a ring-history row holds an image accepted by `selected`.
    pub(crate) fn any_image(
        &self,
        selected: &dyn Fn(&aterm_scrollback::ImageSpan) -> bool,
    ) -> bool {
        self.slots.iter().any(|slot| {
            slot.as_deref()
                .is_some_and(|extras| extras.images.iter().any(selected))
        })
    }

    /// Retains accepted image spans, preserving the running retained-byte count.
    pub(crate) fn retain_images(
        &mut self,
        keep: &mut dyn FnMut(&aterm_scrollback::ImageSpan) -> bool,
    ) -> bool {
        let mut changed = false;
        for slot in &mut self.slots {
            let Some(extras) = slot.as_deref_mut() else {
                continue;
            };
            let before = extras.retained_memory_used();
            let count = extras.images.len();
            extras.images.retain(|span| keep(span));
            if count == extras.images.len() {
                continue;
            }
            let after = if extras.is_empty() {
                *slot = None;
                0
            } else {
                extras.retained_memory_used()
            };
            self.retained = self.retained.saturating_sub(before).saturating_add(after);
            changed = true;
        }
        changed
    }

    /// Replaces inline-image spans on ring history slot `idx` (`0` = oldest), reconciling retained bytes.
    pub(crate) fn set_images(
        &mut self,
        idx: usize,
        spans: Vec<aterm_scrollback::ImageSpan>,
    ) -> bool {
        let Some(slot) = self.slots.get_mut(idx) else {
            return false;
        };
        if spans.is_empty() {
            let Some(extras) = slot.as_deref_mut() else {
                return false;
            };
            if extras.images.is_empty() {
                return false;
            }
            let before = extras.retained_memory_used();
            extras.images.clear();
            let after = if extras.is_empty() {
                *slot = None;
                0
            } else {
                extras.retained_memory_used()
            };
            self.retained = self.retained.saturating_sub(before).saturating_add(after);
            return true;
        }
        if let Some(extras) = slot.as_deref_mut() {
            let before = extras.retained_memory_used();
            extras.images = spans;
            let after = extras.retained_memory_used();
            self.retained = self.retained.saturating_sub(before).saturating_add(after);
        } else {
            let new_slot = Some(Box::new(ScrolledRowExtras {
                images: spans,
                ..ScrolledRowExtras::default()
            }));
            let (retained, _) = slot_bytes(&new_slot);
            self.retained += retained;
            *slot = new_slot;
        }
        true
    }

    /// Creates an empty [`RingExtras`].
    #[inline]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns the number of slots.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// Returns whether the ring holds no slots.
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Returns the underlying deque capacity in slots.
    #[inline]
    pub(crate) fn capacity(&self) -> usize {
        self.slots.capacity()
    }

    /// Returns the extras for ring history row `idx`, if any.
    #[inline]
    pub(crate) fn get(&self, idx: usize) -> Option<&ScrolledRowExtras> {
        self.slots.get(idx).and_then(|slot| slot.as_deref())
    }

    /// Returns total out-of-line bytes retained by all slots.
    #[inline]
    pub(crate) const fn retained_bytes(&self) -> usize {
        self.retained
    }

    /// Returns total OSC 8 hyperlink bytes retained by all slots.
    #[inline]
    pub(crate) const fn link_bytes(&self) -> usize {
        self.links
    }

    #[inline]
    fn charge(&mut self, slot: &RingSlot) {
        let (retained, links) = slot_bytes(slot);
        self.retained += retained;
        self.links += links;
    }

    #[inline]
    fn discharge(&mut self, slot: &RingSlot) {
        let (retained, links) = slot_bytes(slot);
        self.retained -= retained;
        self.links -= links;
    }

    /// Appends the newest ring row's slot.
    #[inline]
    pub(crate) fn push_back(&mut self, slot: RingSlot) {
        debug_assert!(
            slot.as_deref().is_none_or(|e| !e.is_empty()),
            "an empty ring slot must be None"
        );
        self.charge(&slot);
        self.slots.push_back(slot);
    }

    /// Appends freshly extracted row extras, boxing only when non-empty.
    #[inline]
    pub(crate) fn push_extracted(&mut self, extracted: ScrolledRowExtras) {
        self.push_back((!extracted.is_empty()).then(|| Box::new(extracted)));
    }

    /// Removes and returns the oldest slot.
    #[inline]
    pub(crate) fn pop_front(&mut self) -> Option<RingSlot> {
        let slot = self.slots.pop_front()?;
        self.discharge(&slot);
        self.linkless_front = self.linkless_front.saturating_sub(1);
        Some(slot)
    }

    /// Removes and returns the newest slot.
    #[inline]
    pub(crate) fn pop_back(&mut self) -> Option<RingSlot> {
        let slot = self.slots.pop_back()?;
        self.discharge(&slot);
        self.linkless_front = self.linkless_front.min(self.slots.len());
        Some(slot)
    }

    /// Inserts `slot` at `idx` (`0` = oldest).
    pub(crate) fn insert(&mut self, idx: usize, slot: RingSlot) {
        self.charge(&slot);
        self.slots.insert(idx, slot);
        self.linkless_front = self.linkless_front.min(idx);
    }

    /// Appends `slots` in order, newest last.
    pub(crate) fn extend(&mut self, slots: impl IntoIterator<Item = RingSlot>) {
        for slot in slots {
            self.push_back(slot);
        }
    }

    /// Removes all slots and resets byte counters.
    #[inline]
    pub(crate) fn clear(&mut self) {
        self.slots.clear();
        self.retained = 0;
        self.links = 0;
        self.linkless_front = 0;
    }

    /// Strips hyperlinks from the oldest rows until at least `want` link bytes are freed.
    pub(crate) fn strip_oldest_links(&mut self, want: usize) -> usize {
        let mut freed = 0;
        while freed < want && self.linkless_front < self.slots.len() {
            let idx = self.linkless_front;
            self.linkless_front += 1;
            let slot = &mut self.slots[idx];
            let Some(extras) = slot.as_deref_mut() else {
                continue;
            };
            let links = extras.link_bytes();
            if links == 0 {
                continue;
            }
            let before = extras.retained_memory_used();
            extras.strip_hyperlinks();
            let after = if extras.is_empty() {
                *slot = None;
                0
            } else {
                extras.retained_memory_used()
            };
            self.retained -= before - after;
            self.links -= links;
            freed += links;
        }
        freed
    }

    /// Asserts that running byte sums match a fresh scan of all slots.
    #[cfg(test)]
    pub(crate) fn assert_running_sums(&self) {
        let (retained, links) = self
            .slots
            .iter()
            .map(slot_bytes)
            .fold((0, 0), |(r, l), (dr, dl)| (r + dr, l + dl));
        assert_eq!(self.retained, retained, "ring retained-byte sum drifted");
        assert_eq!(self.links, links, "ring link-byte sum drifted");
        assert!(
            self.slots
                .range(..self.linkless_front.min(self.slots.len()))
                .all(|slot| slot_bytes(slot).1 == 0),
            "a row before the strip cursor still holds a link"
        );
        assert!(
            self.slots
                .iter()
                .all(|slot| slot.as_deref().is_none_or(|e| !e.is_empty())),
            "an empty ring slot must be None"
        );
    }
}

impl Grid {
    /// Returns total OSC 8 hyperlink bytes held in uncompressed history (ring + staged rows).
    #[must_use]
    #[inline]
    pub const fn history_link_bytes(&self) -> usize {
        self.storage
            .ring_extras
            .link_bytes()
            .saturating_add(self.storage.lazy_buffer.link_bytes())
    }

    /// Returns the byte budget for [`Self::history_link_bytes`].
    #[must_use]
    #[inline]
    pub fn history_link_budget(&self) -> usize {
        self.storage
            .scrollback_memory_budget()
            .unwrap_or(aterm_scrollback::DEFAULT_MEMORY_BUDGET)
            / HISTORY_LINK_BUDGET_DIVISOR
    }

    /// Strips oldest history hyperlinks when [`Self::history_link_bytes`] exceeds its budget.
    #[inline]
    pub(crate) fn enforce_history_link_budget(&mut self) {
        let used = self.history_link_bytes();
        if used == 0 {
            return;
        }
        let budget = self.history_link_budget();
        if used > budget {
            self.strip_history_links(used, budget);
        }
    }

    /// The over-budget path: strip links oldest row first (staged rows are
    /// older than ring rows) down to three quarters of the budget, so a flood
    /// re-enters this once per quarter-budget of new links, not on every row.
    #[cold]
    #[inline(never)]
    fn strip_history_links(&mut self, used: usize, budget: usize) {
        let low_water = budget - budget / 4;
        let want = used - low_water;
        let staged = self.storage.lazy_buffer.strip_oldest_links(want);
        let ringed = if staged < want {
            self.storage.ring_extras.strip_oldest_links(want - staged)
        } else {
            0
        };
        if staged + ringed > 0 {
            if self.storage.display_offset > 0 {
                self.storage.mark_content_full();
            } else {
                self.storage.content_gen += 1;
            }
        }
    }
}
