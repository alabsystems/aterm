// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The ring's out-of-line extras, and the byte budget on the OSC 8 payload that
//! uncompressed history holds.
//!
//! A row scrolling off the screen takes its extras with it
//! (`Grid::extract_row_extras_into`): every OSC 8 span carries an `Arc<str>`
//! of up to 8 KiB (`MAX_HYPERLINK_URL_BYTES`). On the live screen the extras
//! map caps links at `MAX_HYPERLINK_ENTRIES`; in history the tiered store
//! weighs them against its memory budget. Between the two sit the ring and the
//! lazy buffer's staged rows, which hold rows uncompressed and were sized in
//! lines on the assumption that a row costs its cells. A program printing a
//! distinct 8 KiB link per cell made that 0.63 MiB a row, ~4 GiB a tab, that no
//! byte count saw and no shed could reclaim.
//!
//! So both holders keep running sums of what their extras retain, which
//! `Grid::memory_used` reports, and the OSC 8 share of it is held to
//! [`Grid::history_link_budget`]: past it the oldest rows lose their links and
//! keep their text, the policy `CellExtras::enforce_hyperlink_limit` applies to
//! the live map. The sums live in the containers ([`RingExtras`] here,
//! `LazyBuffer` for the staged rows) so no push or pop site can forget them.

use std::collections::VecDeque;

use super::{Grid, ScrolledRowExtras};

/// One ring slot: `None` for a row with no extras (the common plain-text row),
/// never `Some` of an empty box.
pub(crate) type RingSlot = Option<Box<ScrolledRowExtras>>;

/// The fraction of the scrollback memory budget that OSC 8 payload in
/// uncompressed history (ring + staged rows) may use: a quarter.
///
/// The memory budget is the one statement of what a tab's history may cost (it
/// is what the GUI's pressure shed and aggregate cap lower), and these rows are
/// headed for the store it governs, so the allowance follows it, including
/// while a shed has it lowered. A quarter keeps the total within 1.25 × the
/// budget plus the ring's fixed cells, where it was ~40 × the 100 MB default,
/// and at that default (25 MiB) the GUI's largest ring (10,000 lines) with a
/// 2 KiB link on every row still keeps every link. Not the store's headroom:
/// a store at its budget (the steady state of a long session) would then
/// strip the newest history's links while the oldest compressed rows kept
/// theirs.
const HISTORY_LINK_BUDGET_DIVISOR: usize = 4;

/// [`RingSlot`]'s out-of-line bytes: `(retained, links)`, as
/// [`ScrolledRowExtras::retained_memory_used`] and
/// [`ScrolledRowExtras::link_bytes`] count them. Fixed while the slot sits in
/// the ring: nothing mutates a held box except [`RingExtras::strip_oldest_links`].
fn slot_bytes(slot: &RingSlot) -> (usize, usize) {
    slot.as_deref()
        .map_or((0, 0), |e| (e.retained_memory_used(), e.link_bytes()))
}

/// The ring's per-row extras (index 0 = oldest ring history row), with running
/// sums of what they retain.
///
/// A `VecDeque<RingSlot>` whose every mutation goes through a method that keeps
/// [`retained_bytes`](Self::retained_bytes) and
/// [`link_bytes`](Self::link_bytes) exact, so the accounting cannot drift from
/// the rows at any of the scroll, reflow, unscroll or erase sites.
#[derive(Debug, Default)]
pub(crate) struct RingExtras {
    slots: VecDeque<RingSlot>,
    /// Sum of [`slot_bytes`]' retained bytes over `slots`.
    retained: usize,
    /// Sum of [`slot_bytes`]' link bytes over `slots`.
    links: usize,
    /// `slots[..linkless_front]` hold no link bytes: where the next
    /// [`strip_oldest_links`](Self::strip_oldest_links) resumes, so a flood
    /// visits each row once. Every removal from the front shifts it down with
    /// the rows; an insertion at or before it pulls it back to the insertion.
    linkless_front: usize,
}

impl RingExtras {
    /// An empty ring.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Number of slots.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether no slot is held.
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The slot deque's capacity, in slots (its spine is heap too).
    #[inline]
    pub(crate) fn capacity(&self) -> usize {
        self.slots.capacity()
    }

    /// The extras of ring history row `idx`, `None` for a row without any.
    #[inline]
    pub(crate) fn get(&self, idx: usize) -> Option<&ScrolledRowExtras> {
        self.slots.get(idx).and_then(|slot| slot.as_deref())
    }

    /// Out-of-line bytes the slots retain (boxes, vectors, strings, image
    /// shares), O(1).
    #[inline]
    pub(crate) fn retained_bytes(&self) -> usize {
        self.retained
    }

    /// The OSC 8 share of [`retained_bytes`](Self::retained_bytes), O(1).
    #[inline]
    pub(crate) fn link_bytes(&self) -> usize {
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

    /// Append the newest ring row's slot. A `Some` box must not be empty (the
    /// `None ⟺ empty` encoding the materializers rely on).
    #[inline]
    pub(crate) fn push_back(&mut self, slot: RingSlot) {
        debug_assert!(
            slot.as_deref().is_none_or(|e| !e.is_empty()),
            "an empty ring slot must be None"
        );
        self.charge(&slot);
        self.slots.push_back(slot);
    }

    /// Append freshly extracted extras, boxing them only when non-empty.
    #[inline]
    pub(crate) fn push_extracted(&mut self, extracted: ScrolledRowExtras) {
        self.push_back((!extracted.is_empty()).then(|| Box::new(extracted)));
    }

    /// Remove the oldest slot; `None` when the ring is empty.
    #[inline]
    pub(crate) fn pop_front(&mut self) -> Option<RingSlot> {
        let slot = self.slots.pop_front()?;
        self.discharge(&slot);
        self.linkless_front = self.linkless_front.saturating_sub(1);
        Some(slot)
    }

    /// Remove the newest slot; `None` when the ring is empty.
    #[inline]
    pub(crate) fn pop_back(&mut self) -> Option<RingSlot> {
        let slot = self.slots.pop_back()?;
        self.discharge(&slot);
        self.linkless_front = self.linkless_front.min(self.slots.len());
        Some(slot)
    }

    /// Insert `slot` at `idx` (0 = oldest).
    pub(crate) fn insert(&mut self, idx: usize, slot: RingSlot) {
        self.charge(&slot);
        self.slots.insert(idx, slot);
        self.linkless_front = self.linkless_front.min(idx);
    }

    /// Append `slots` in order, newest last.
    pub(crate) fn extend(&mut self, slots: impl IntoIterator<Item = RingSlot>) {
        for slot in slots {
            self.push_back(slot);
        }
    }

    /// Drop every slot.
    pub(crate) fn clear(&mut self) {
        self.slots.clear();
        self.retained = 0;
        self.links = 0;
        self.linkless_front = 0;
    }

    /// Strip the links of the oldest rows that hold any until at least `want`
    /// link bytes are freed or none are left; returns the bytes freed. Text,
    /// colours and images stay. A slot left empty becomes `None`.
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

    /// Test-only: the running sums equal a fresh walk of the slots, and the
    /// strip cursor's prefix holds no link.
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
    /// OSC 8 payload held in uncompressed history: the ring's rows plus the
    /// lazy buffer's staged rows, as [`ScrolledRowExtras::link_bytes`] counts
    /// it (each span's URL and id, an upper bound for a link that covers
    /// several rows). O(1). Bounded by
    /// [`history_link_budget`](Self::history_link_budget) after every scroll.
    ///
    /// The tiered store's own rows are not included: it weighs their links in
    /// its budgeted bytes.
    #[must_use]
    #[inline]
    pub fn history_link_bytes(&self) -> usize {
        // Saturating, as in `Grid::memory_used`.
        self.storage
            .ring_extras
            .link_bytes()
            .saturating_add(self.storage.lazy_buffer.link_bytes())
    }

    /// The byte allowance for [`history_link_bytes`](Self::history_link_bytes):
    /// a quarter of the effective scrollback memory budget (a deferred one
    /// while the store is detached for a reflow), or of the default store's
    /// budget on a ring-only grid. See [`HISTORY_LINK_BUDGET_DIVISOR`].
    #[must_use]
    #[inline]
    pub fn history_link_budget(&self) -> usize {
        self.storage
            .scrollback_memory_budget()
            .unwrap_or(aterm_scrollback::DEFAULT_MEMORY_BUDGET)
            / HISTORY_LINK_BUDGET_DIVISOR
    }

    /// Hold [`history_link_bytes`](Self::history_link_bytes) to its budget:
    /// O(1) while under it. Called after every scroll that moves rows into
    /// history (the only path that grows it without bound: every other entry
    /// re-homes rows already counted, or at most a screen the live map's cap
    /// bounds) and whenever the budget is set, which is how the pressure shed
    /// reaches it.
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
    /// older than ring rows) down to a low-water mark of three quarters of the
    /// budget, so a flood re-enters this once per quarter-budget of new links
    /// rather than on every row (the same reason `enforce_hyperlink_limit`
    /// trims the live map below its ceiling).
    ///
    /// History content changed, so `content_gen` moves (the viewport memo must
    /// not serve a row with the links it lost). The screen is damaged only
    /// while a scrolled-back viewport can show history: at the live bottom it
    /// shows none, and the scroll paths that call this mark their own rows
    /// (the top-anchored one must not mark its protected footer).
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
