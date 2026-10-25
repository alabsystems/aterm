// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed input custody: stamps, press-credit ledger, insert seam, and `note_*` host input entry points.

use super::*;

/// Whether a one-shot hint stamped at `hint` is still inside its `window` (seconds) at `now`.
#[inline]
pub(crate) fn hint_fresh(hint: Option<Instant>, now: Instant, window: f32) -> bool {
    hint.is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() <= window)
}

/// Spend a one-shot hint: take it only while fresh ([`hint_fresh`]).
#[inline]
pub(crate) fn take_hint_fresh(
    hint: &mut Option<Instant>,
    now: Instant,
    window: f32,
) -> Option<Instant> {
    hint.take_if(|t| now.saturating_duration_since(*t).as_secs_f32() <= window)
}

/// Cap of the exact content witness for the last adjacent typed echo run.
pub(super) const RECENT_TYPED_RUN_CAP: usize = 256;

/// The farthest, in rows, a cross-row park may stand from the row it left.
pub(super) const CROSS_ROW_PARK_ROWS_MAX: u16 = 3;

#[derive(Clone, Copy)]
pub(super) struct RecentTypedRun {
    pub(super) row: u16,
    pub(super) col0: u16,
    pub(super) len: u16,
    pub(super) glyphs: [char; RECENT_TYPED_RUN_CAP],
    pub(super) at: Instant,
    /// The walk the run was laid on.
    pub(super) origin: Option<rk::ribbon::RunOrigin>,
}

impl RecentTypedRun {
    #[inline]
    pub(super) const fn end(&self) -> u16 {
        self.col0.saturating_add(self.len)
    }
}

/// Bounded FIFO of typed-press licence stamps, packed oldest-first.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TypedStamps {
    /// Packed oldest-first: every `Some` precedes every `None`.
    pub(super) slots: [Option<Instant>; TYPED_STAMP_DEPTH],
}

impl Default for TypedStamps {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Depth of [`TypedStamps`] and [`PressCredits`], equal to the coalesced-sweep cell cap.
pub const TYPED_STAMP_DEPTH: usize = 128;
const STAMP_RING_MASK: usize = TYPED_STAMP_DEPTH - 1;
const _: () = assert!(TYPED_STAMP_DEPTH.is_power_of_two());
const _: () = assert!(TYPED_STAMP_DEPTH == u128::BITS as usize);

/// The in-flight patience (seconds).
pub(crate) const IN_FLIGHT_PATIENCE_S: f32 = 10.0;

/// Capacity of [`CursorGlow::anchor_rows`].
pub(super) const ANCHOR_ROWS: usize = 16;

/// One entry of [`CursorGlow::anchor_rows`].
#[derive(Clone, Copy)]
pub(super) struct AnchorRow {
    pub(super) row: u16,
    pub(super) end: u16,
    pub(super) tainted: bool,
    /// [`CursorGlow::anchor_rows_clock`] at this entry's last write.
    pub(super) touched: u64,
}

/// The hand's row ([`CursorGlow::hand_row`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HandRow {
    /// The row the last licensed typed echo landed.
    pub(super) row: u16,
    /// The caret's screen row across a content move the hand typed.
    pub(super) caret_row: Option<u16>,
}

/// A generic input boundary may move input away from the drawn caret.
#[derive(Clone, Copy, Debug)]
pub(super) struct UnownedCaret {
    pub(super) row: u16,
    pub(super) parked: bool,
}

impl HandRow {
    /// The hand's row after a licensed typed echo landed on `row`.
    #[inline]
    pub(super) const fn at(row: u16) -> Self {
        Self {
            row,
            caret_row: None,
        }
    }

    /// Carried across a scroll or band move.
    #[inline]
    pub(super) fn carried(self, map: &impl Fn(u16) -> Option<u16>, at_caret: bool) -> Option<Self> {
        let caret_row = self.caret_row.or(at_caret.then_some(self.row));
        match map(self.row) {
            Some(row) => Some(Self { row, caret_row }),
            None => caret_row.map(Self::at),
        }
    }

    /// The caret was seen on `caret_row`.
    #[inline]
    pub(super) const fn settle(&mut self, caret_row: u16) {
        if let Some(cr) = self.caret_row
            && cr == caret_row
        {
            *self = Self::at(caret_row);
        } else if self.row == caret_row {
            self.caret_row = None;
        }
    }
}

impl TypedStamps {
    pub(crate) const EMPTY: Self = Self {
        slots: [None; TYPED_STAMP_DEPTH],
    };

    /// Bank one press.
    #[inline]
    pub(crate) const fn stamp(&mut self, now: Instant) {
        let mut i = 0;
        while i < TYPED_STAMP_DEPTH {
            if self.slots[i].is_none() {
                self.slots[i] = Some(now);
                return;
            }
            i += 1;
        }
        let mut j = 0;
        while j + 1 < TYPED_STAMP_DEPTH {
            self.slots[j] = self.slots[j + 1];
            j += 1;
        }
        self.slots[TYPED_STAMP_DEPTH - 1] = Some(now);
    }

    /// Wipe every banked stamp (the class-changing supersede / teardown).
    #[inline]
    pub(crate) const fn clear(&mut self) {
        self.slots = [None; TYPED_STAMP_DEPTH];
    }

    /// Take every stamp banked after `at` out of the bank, keeping the rest packed.
    pub(crate) fn split_off_after(&mut self, at: Instant) -> Self {
        let mut kept = Self::EMPTY;
        let mut later = Self::EMPTY;
        let (mut nk, mut nl) = (0, 0);
        for t in self.slots.into_iter().map_while(|s| s) {
            if t <= at {
                kept.slots[nk] = Some(t);
                nk += 1;
            } else {
                later.slots[nl] = Some(t);
                nl += 1;
            }
        }
        *self = kept;
        later
    }

    /// Put back what [`Self::split_off_after`] took, behind the remaining earlier stamps.
    pub(crate) const fn merge(&mut self, later: Self) {
        let mut n_later = 0;
        while n_later < TYPED_STAMP_DEPTH && later.slots[n_later].is_some() {
            n_later += 1;
        }
        if n_later == 0 {
            return;
        }
        let mut n_kept = 0;
        while n_kept < TYPED_STAMP_DEPTH && self.slots[n_kept].is_some() {
            n_kept += 1;
        }
        let max_keep = TYPED_STAMP_DEPTH - n_later;
        let keep = if n_kept < max_keep { n_kept } else { max_keep };
        let shift = n_kept - keep;
        let mut i = 0;
        while i < keep {
            self.slots[i] = self.slots[shift + i];
            i += 1;
        }
        let mut j = 0;
        while j < n_later {
            self.slots[keep + j] = later.slots[j];
            j += 1;
        }
        let mut k = keep + n_later;
        while k < TYPED_STAMP_DEPTH {
            self.slots[k] = None;
            k += 1;
        }
    }

    /// Keep only the stamps `keep` admits, packed oldest-first in their banked order.
    pub(super) fn retain(&mut self, keep: impl Fn(Instant) -> bool) {
        let mut write = 0;
        let mut read = 0;
        while read < TYPED_STAMP_DEPTH {
            let Some(t) = self.slots[read] else {
                break;
            };
            if keep(t) {
                if write != read {
                    self.slots[write] = Some(t);
                }
                write += 1;
            }
            read += 1;
        }
        if write < read {
            self.slots[write..read].fill(None);
        }
    }

    /// Remove only the stamp(s) written at `at`.
    pub(crate) fn revoke_at(&mut self, at: Instant) {
        self.retain(|t| t != at);
    }

    /// Whether any stamp is banked, fresh or stale.
    #[inline]
    pub(crate) const fn armed(&self) -> bool {
        self.slots[0].is_some()
    }

    /// Whether any banked stamp is still within `window` of `now`.
    #[inline]
    pub(crate) fn any_fresh(&self, now: Instant, window: f32) -> bool {
        self.slots
            .iter()
            .map_while(|s| *s)
            .any(|t| now.saturating_duration_since(t).as_secs_f32() <= window)
    }

    /// Drop only stamps older than `window`.
    #[inline]
    pub(crate) fn retire_stale(&mut self, now: Instant, window: f32) {
        self.retain(|t| now.saturating_duration_since(t).as_secs_f32() <= window);
    }

    /// The oldest still-fresh stamp, left in place.
    #[inline]
    pub(crate) fn peek_fresh(&self, now: Instant, window: f32) -> Option<Instant> {
        self.slots
            .iter()
            .map_while(|s| *s)
            .find(|t| now.saturating_duration_since(*t).as_secs_f32() <= window)
    }

    /// Consume the oldest still-fresh stamp.
    pub(crate) fn take_fresh(&mut self, now: Instant, window: f32) -> Option<Instant> {
        let idx = self
            .slots
            .iter()
            .map_while(|s| *s)
            .position(|t| now.saturating_duration_since(t).as_secs_f32() <= window)?;
        let taken = self.slots[idx];
        self.slots.copy_within(idx + 1.., idx);
        self.slots[TYPED_STAMP_DEPTH - 1] = None;
        taken
    }
}

/// One live unpaid press as the exact-run proofs collect it.
pub(super) type LiveUnpaidPress = (usize, Instant, u8, Option<char>);

/// Bound on the exact-glyph proof for a loaded host that missed a whole composer row.
pub(super) const EXACT_COALESCED_PREFIX_MAX: usize = TYPED_STAMP_DEPTH;
pub(super) const EXACT_COALESCED_STALE_PREFIX_MAX: usize = 8;
/// At most two queued keys can be proved after an unknown insert.
pub(super) const ORPHAN_EXACT_KEYS_MAX: usize = 2;
/// Fixed, press-ordered escrow candidates recovered after an unknown insert.
pub(super) type OrphanExactPressRun = [Option<(Instant, char)>; ORPHAN_EXACT_KEYS_MAX];

/// One live unpaid press as the exact-run proofs collect it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PressCredits {
    pub(super) slots: [Option<(Instant, u8, Option<char>)>; TYPED_STAMP_DEPTH],
    /// Bitmask of occupied slots (`bit i == 1` iff `slots[i].is_some()`).
    pub(super) occupied: u128,
    /// The next slot to write.
    pub(super) head: usize,
    /// The held presses: every unpaid press banked at or before this instant.
    pub(super) held_through: Option<Instant>,
    /// Whether the move being judged stays on the hand's row, so held presses may pay for it.
    pub(super) admit_held: bool,
}

impl Default for PressCredits {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl PressCredits {
    pub(super) const EMPTY: Self = Self {
        slots: [None; TYPED_STAMP_DEPTH],
        occupied: 0,
        head: 0,
        held_through: None,
        admit_held: false,
    };

    /// Bank one press worth `credits` cells.
    #[inline]
    pub(super) const fn bank(&mut self, now: Instant, credits: u8, glyph: Option<char>) {
        self.slots[self.head] = Some((now, credits, glyph));
        self.occupied |= 1u128 << self.head;
        self.head = (self.head + 1) & STAMP_RING_MASK;
    }

    /// Hold every press banked through `at` ([`Self::held_through`]).
    #[inline]
    pub(super) fn hold_through(&mut self, at: Instant) {
        self.held_through = self.held_through.max(Some(at));
    }

    /// Whether the press banked at `at` is held ([`Self::held_through`]).
    #[inline]
    pub(super) fn held(&self, at: Instant) -> bool {
        self.held_through.is_some_and(|through| at <= through)
    }

    /// Whether the press banked at `at` with `credits` unpaid cells may pay for the move.
    #[inline]
    pub(super) fn payable(&self, at: Instant, credits: u8, now: Instant) -> bool {
        credits > 0
            && now.saturating_duration_since(at).as_secs_f32() <= IN_FLIGHT_PATIENCE_S
            && (self.admit_held || !self.held(at))
    }

    /// The live presses: unpaid, younger than [`IN_FLIGHT_PATIENCE_S`].
    #[inline]
    pub(super) fn within(
        &self,
        now: Instant,
    ) -> impl Iterator<Item = (Instant, u8, Option<char>)> + '_ {
        let mut mask = self.occupied;
        std::iter::from_fn(move || {
            while mask != 0 {
                let i = mask.trailing_zeros() as usize;
                mask &= mask - 1;
                if let Some((t, c, g)) = self.slots[i]
                    && self.payable(t, c, now)
                {
                    return Some((t, c, g));
                }
            }
            None
        })
    }

    /// Unpaid cells of the live presses whose instant `keep` admits.
    #[inline]
    pub(super) fn cells_where(&self, now: Instant, keep: impl Fn(Instant) -> bool) -> usize {
        self.within(now)
            .filter(|(t, _, _)| keep(*t))
            .map(|(_, c, _)| usize::from(c))
            .sum()
    }

    /// Unpaid cells of every live press.
    #[inline]
    pub(super) fn cells_within(&self, now: Instant) -> usize {
        self.cells_where(now, |_| true)
    }

    /// Unpaid presses — the same pool counted in keys.
    #[inline]
    pub(super) fn presses_within(&self, now: Instant) -> usize {
        self.within(now).count()
    }

    /// Whether any unpaid press is live at `now`.
    #[inline]
    pub(super) fn any_within(&self, now: Instant) -> bool {
        self.within(now).next().is_some()
    }

    /// The oldest unpaid press, or `None`.
    #[inline]
    pub(super) fn oldest_unpaid(&self, now: Instant) -> Option<Instant> {
        let (mut free, mut held) = (None::<Instant>, None::<Instant>);
        for (t, _, _) in self.within(now) {
            let slot = if self.held(t) { &mut held } else { &mut free };
            *slot = Some(slot.map_or(t, |prev| prev.min(t)));
        }
        free.or(held)
    }

    /// The last press still funding a foreign-row park.
    #[inline]
    pub(super) fn newest_unpaid(&self, now: Instant) -> Option<Instant> {
        self.within(now).map(|(t, _, _)| t).max()
    }

    /// Whether an unpaid press stamped exactly `glyph` within `freshness_s`.
    #[inline]
    pub(super) fn fresh_glyph(&self, now: Instant, freshness_s: f32, glyph: char) -> bool {
        self.within(now).any(|(at, _, g)| {
            g == Some(glyph) && now.saturating_duration_since(at).as_secs_f32() <= freshness_s
        })
    }

    /// The oldest fresh press's exact glyph, in ring order.
    pub(super) fn oldest_fresh_glyph(
        &self,
        now: Instant,
        freshness_s: f32,
    ) -> Option<(usize, char)> {
        let mut mask = self.occupied.rotate_right(self.head as u32);
        while mask != 0 {
            let k = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            let i = (self.head + k) & STAMP_RING_MASK;
            if let Some((at, credits, glyph)) = self.slots[i]
                && self.payable(at, credits, now)
                && now.saturating_duration_since(at).as_secs_f32() <= freshness_s
            {
                return glyph.map(|glyph| (i, glyph));
            }
        }
        None
    }

    /// Whether `printed` is foreign to every press in flight.
    pub(super) fn foreign_to_live(&self, now: Instant, printed: char) -> Option<bool> {
        let mut any = false;
        for (_, _, glyph) in self.within(now) {
            match glyph {
                None => return None,
                Some(g) if g == printed => return Some(false),
                Some(_) => any = true,
            }
        }
        any.then_some(true)
    }

    /// The chronological unpaid one-cell keys must spell this whole small observed run.
    pub(super) fn exact_unpaid_run(
        &self,
        now: Instant,
        previous_probe_at: Instant,
        glyphs: &[char],
    ) -> Option<usize> {
        self.exact_unpaid_run_with_tail_window(
            now,
            previous_probe_at,
            glyphs,
            CursorGlow::TYPE_HINT_FRESH,
        )
    }

    /// The same proof with a caller-selected tail age.
    pub(super) fn exact_unpaid_run_with_tail_window(
        &self,
        now: Instant,
        previous_probe_at: Instant,
        glyphs: &[char],
        tail_window_s: f32,
    ) -> Option<usize> {
        if self.occupied == 0 || glyphs.is_empty() || glyphs.len() > EXACT_COALESCED_PREFIX_MAX {
            return None;
        }
        let mut live: [Option<LiveUnpaidPress>;
            EXACT_COALESCED_PREFIX_MAX + EXACT_COALESCED_STALE_PREFIX_MAX] =
            [None; EXACT_COALESCED_PREFIX_MAX + EXACT_COALESCED_STALE_PREFIX_MAX];
        let mut n = 0;
        let mut mask = self.occupied.rotate_right(self.head as u32);
        while mask != 0 {
            let k = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            let i = (self.head + k) & STAMP_RING_MASK;
            let Some((at, credits, glyph)) = self.slots[i] else {
                continue;
            };
            if !self.payable(at, credits, now) {
                continue;
            }
            if n == live.len() {
                return None;
            }
            live[n] = Some((i, at, credits, glyph));
            n += 1;
        }
        for start in 0..n {
            if start + glyphs.len() <= n {
                let exact = glyphs.iter().enumerate().all(|(offset, &expected)| {
                    live[start + offset].is_some_and(|(_, _, credits, glyph)| {
                        credits == 1 && glyph == Some(expected)
                    })
                });
                if exact {
                    let (_, last_at, _, _) = live[start + glyphs.len() - 1]?;
                    if now.saturating_duration_since(last_at).as_secs_f32() <= tail_window_s {
                        return live[start].map(|(slot, _, _, _)| slot);
                    }
                }
            }
            let (_, at, _, _) = live[start]?;
            if start >= EXACT_COALESCED_STALE_PREFIX_MAX
                || at > previous_probe_at
                || now.saturating_duration_since(at).as_secs_f32() <= CursorGlow::TYPE_HINT_FRESH
            {
                return None;
            }
        }
        None
    }

    /// The slot of the oldest payable press when the payable presses begin, oldest-first.
    pub(super) fn exact_run_after(
        &self,
        now: Instant,
        after: Instant,
        glyphs: &[char],
    ) -> Option<usize> {
        let mut first = None;
        let mut n = 0;
        let mut mask = self.occupied.rotate_right(self.head as u32);
        while mask != 0 {
            if n == glyphs.len() {
                break;
            }
            let k = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            let i = (self.head + k) & STAMP_RING_MASK;
            let Some((at, credits, glyph)) = self.slots[i] else {
                continue;
            };
            if !self.payable(at, credits, now) {
                continue;
            }
            if at <= after || credits != 1 || glyph != Some(glyphs[n]) {
                return None;
            }
            first.get_or_insert(i);
            n += 1;
        }
        first.filter(|_| n == glyphs.len() && n > 0)
    }

    /// A later exact content echo proves earlier same-stream keys already appeared.
    pub(super) const fn retire_before_slot(&mut self, selected: usize) {
        let mut k = 0;
        while k < TYPED_STAMP_DEPTH {
            let i = (self.head + k) & STAMP_RING_MASK;
            if i == selected {
                return;
            }
            let bit = 1u128 << i;
            if self.occupied & bit != 0 {
                self.slots[i] = None;
                self.occupied &= !bit;
            }
            k += 1;
        }
    }

    /// Spend `cells` oldest-first, so one pool of typing never funds a second echo.
    pub(super) fn spend_counting(&mut self, now: Instant, mut cells: usize) -> usize {
        if cells == 0 || self.occupied == 0 {
            return 0;
        }
        let mut drained = 0;
        for held in [false, true] {
            let mut mask = self.occupied.rotate_right(self.head as u32);
            while mask != 0 {
                if cells == 0 {
                    return drained;
                }
                let k = mask.trailing_zeros() as usize;
                mask &= mask - 1;
                let i = (self.head + k) & STAMP_RING_MASK;
                let Some((t, c, _)) = self.slots[i] else {
                    continue;
                };
                if self.held(t) != held || !self.payable(t, c, now) {
                    continue;
                }
                let take = usize::from(c).min(cells);
                cells -= take;
                if usize::from(c) == take {
                    self.slots[i] = None;
                    self.occupied &= !(1u128 << i);
                    drained += 1;
                } else if let Some((_, left, _)) = &mut self.slots[i] {
                    *left -= take as u8;
                }
            }
        }
        drained
    }

    /// Blank every slot whose press instant `pred` admits, in place.
    pub(super) fn blank_where(&mut self, pred: impl Fn(Instant) -> bool) {
        let mut mask = self.occupied;
        while mask != 0 {
            let i = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            if self.slots[i].is_some_and(|(t, _, _)| pred(t)) {
                self.slots[i] = None;
                self.occupied &= !(1u128 << i);
            }
        }
    }

    /// Drop every press past [`IN_FLIGHT_PATIENCE_S`].
    pub(super) fn retire_stale(&mut self, now: Instant) {
        self.blank_where(|t| now.saturating_duration_since(t).as_secs_f32() > IN_FLIGHT_PATIENCE_S);
    }

    /// Revoke the press banked at `at`, for a dispatch that never reached the child.
    pub(super) fn revoke_at(&mut self, at: Instant) {
        self.blank_where(|t| t == at);
    }

    /// Forget every press.
    #[inline]
    pub(super) const fn forget(&mut self) {
        if self.occupied != 0 {
            self.slots = [None; TYPED_STAMP_DEPTH];
            self.occupied = 0;
        }
        self.head = 0;
        self.held_through = None;
    }

    /// Retire every press banked at or before `at`.
    pub(super) fn retire_through(&mut self, at: Instant) {
        self.blank_where(|t| t <= at);
    }

    /// One or two exact one-cell keys dispatched after an insert but banked by its delivery.
    pub(super) fn exact_run_between(
        &self,
        now: Instant,
        dispatched_at: Instant,
        delivered_at: Instant,
    ) -> Option<(OrphanExactPressRun, usize)> {
        let mut keys = [None; ORPHAN_EXACT_KEYS_MAX];
        let mut n = 0;
        let mut mask = self.occupied.rotate_right(self.head as u32);
        while mask != 0 {
            let k = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            let i = (self.head + k) & STAMP_RING_MASK;
            let Some((at, credits, glyph)) = self.slots[i] else {
                continue;
            };
            if at <= dispatched_at || at > delivered_at || !self.payable(at, credits, now) {
                continue;
            }
            if n == keys.len() || credits != 1 {
                return None;
            }
            let glyph = glyph?;
            if matches!(glyph, ' ' | '\0') {
                return None;
            }
            keys[n] = Some((at, glyph));
            n += 1;
        }
        (n > 0).then_some((keys, n))
    }

    /// Take every press banked after `at` out of the ring at its own index.
    pub(super) fn split_off_after(&mut self, at: Instant) -> Self {
        let mut later = Self {
            head: self.head,
            ..Self::default()
        };
        let mut mask = self.occupied;
        while mask != 0 {
            let i = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            if self.slots[i].is_some_and(|(t, _, _)| t > at) {
                later.slots[i] = self.slots[i].take();
                let bit = 1u128 << i;
                self.occupied &= !bit;
                later.occupied |= bit;
            }
        }
        later
    }

    /// Put back what [`Self::split_off_after`] took.
    pub(super) fn merge(&mut self, later: Self) {
        if later.occupied == 0 {
            return;
        }
        if self.head == 0 && self.occupied == 0 {
            let mut mask = later.occupied.rotate_right(later.head as u32);
            while mask != 0 {
                let k = mask.trailing_zeros() as usize;
                mask &= mask - 1;
                let i = (later.head + k) & STAMP_RING_MASK;
                if let Some((t, c, glyph)) = later.slots[i] {
                    self.bank(t, c, glyph);
                }
            }
            return;
        }
        let mut mask = later.occupied;
        while mask != 0 {
            let i = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            if later.slots[i].is_some() {
                self.slots[i] = later.slots[i];
                self.occupied |= 1u128 << i;
            }
        }
    }

    /// The slot of the newest press.
    #[inline]
    pub(super) fn newest(&self) -> Option<usize> {
        let mut mask = self.occupied;
        let mut best: Option<(usize, Instant)> = None;
        while mask != 0 {
            let i = mask.trailing_zeros() as usize;
            mask &= mask - 1;
            if let Some((t, _, _)) = self.slots[i]
                && best.is_none_or(|(_, bt)| t >= bt)
            {
                best = Some((i, t));
            }
        }
        best.map(|(i, _)| i)
    }

    /// Retire the newest press (the key a Backspace erases), whole.
    #[inline]
    pub(super) fn retire_newest(&mut self) {
        if let Some(i) = self.newest() {
            self.slots[i] = None;
            self.occupied &= !(1u128 << i);
        }
    }

    /// Retire `cells` of the newest press.
    #[inline]
    pub(super) fn retire_newest_cells(&mut self, cells: u8) {
        if cells == 0 {
            return;
        }
        if let Some(i) = self.newest()
            && let Some((_, c, _)) = &mut self.slots[i]
        {
            *c = c.saturating_sub(cells);
            if *c == 0 {
                self.slots[i] = None;
                self.occupied &= !(1u128 << i);
            }
        }
    }

    /// Whether no press is banked at all.
    #[inline]
    pub(super) const fn is_empty(&self) -> bool {
        self.occupied == 0
    }
}

/// A queued key's licence class, re-stamped at delivery ([`CursorGlow::note_delivered`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveredClass {
    /// A plain typed glyph: its stamp (the credit and v2 press were never revoked).
    Typed,
    /// A plain Enter: the Return one-shot.
    Return,
    /// A composer newline (Shift+Enter on the alternate screen).
    Newline,
    /// A navigation key: the nav one-shot.
    Nav,
    /// A Backspace: the quench and poof stamps and the pre-erase row fill.
    Erase,
    /// A kill chord: its poof witness.
    Kill { moves_cursor: bool, word: bool },
    /// A Tab (any modifier) or a bare ⌃V.
    Gesture,
}

/// How the host priced a delivered insert ([`CursorGlow::note_insert_delivered_from`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertWidth {
    /// Priced in cells from the very text put on the wire (`Terminal::paste_insert_cells`).
    Cells(u16),
    /// One line priced in cells, then the one line break that ends it.
    Line(u16),
    /// The text cannot say: a Tab's completion, a raw ⌃V, a multi-line or tabbed body.
    Unknown,
}

/// The delivered-insert licence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InsertLicence {
    /// The delivery instant.
    pub(super) at: Instant,
    /// When the insert's first byte was dispatched.
    pub(super) dispatched_at: Instant,
    /// The insert's cell width as the host priced it.
    pub(super) cells: u16,
    /// Whether `cells` is a priced width ([`InsertWidth::Cells`]).
    pub(super) known: bool,
    /// Whether the priced line ends in its line break ([`InsertWidth::Line`]).
    pub(super) breaks: bool,
    /// The row witness: the row the hand was on when armed ([`CursorGlow::insert_row`]).
    pub(super) row: Option<u16>,
}

/// Where a delivered insert's sweep was laid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InsertSpan {
    pub(super) row: u16,
    pub(super) col0: u16,
    pub(super) col1: u16,
    pub(super) laid_at: Instant,
}

/// A same-row forward hop the seam refused for want of a licence, remembered for one delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PendingHop {
    pub(super) row: u16,
    pub(super) col0: u16,
    pub(super) col1: u16,
    pub(super) at: Instant,
}

/// A visible-lane typed lay whose caret has not rested.
#[derive(Clone, Copy, Debug)]
pub(super) struct TornLay {
    /// The row the typed echo laid on (a scroll drops the lay).
    pub(super) row: u16,
    /// The lay's first cell.
    pub(super) col0: u16,
    /// The caret the echo left the row at (exclusive end of the lay).
    pub(super) col1: u16,
    /// The caret last seen while armed (`None`: hidden).
    pub(super) seen: Option<(u16, u16)>,
    /// When the caret last moved (the lay itself included).
    pub(super) moved_at: Instant,
    /// The hand's row before the lay put the hand on `row`.
    pub(super) hand_before: Option<HandRow>,
    /// The generic boundary's doubt before the lay settled it ([`CursorGlow::hand_lands_on`]).
    pub(super) unowned_before: Option<UnownedCaret>,
    /// The print anchor's `seq` at the lay.
    pub(super) anchor_seq: Option<u64>,
    /// A print since the lay ended on another row.
    pub(super) printed_off_row: bool,
}

/// The most presses an [`EraseWitness`] remembers retiring.
pub(super) const ERASE_RETIRED_MAX: usize = 4;

/// The last Backspace ([`CursorGlow::note_backspace_erasing`]) and what its echo still owes.
#[derive(Clone, Copy, Debug)]
pub(super) struct EraseWitness {
    /// The latest Backspace's press.
    pub(super) at: Instant,
    /// The glyphs of the presses Backspaces retired before they echoed, in typing order.
    pub(super) retired: [char; ERASE_RETIRED_MAX],
    pub(super) retired_n: u8,
    /// The retreat cells the Backspaces since the last shown erase still owe.
    pub(super) owed: u16,
}

impl EraseWitness {
    /// Whether a retreat is still owed at `now`, within the press patience.
    #[inline]
    pub(super) fn owed_at(&self, now: Instant) -> bool {
        self.owed > 0
            && now.saturating_duration_since(self.at).as_secs_f32() <= IN_FLIGHT_PATIENCE_S
    }

    /// The retired glyphs still to echo.
    #[inline]
    pub(super) const fn retired(&self) -> &[char] {
        self.retired.split_at(self.retired_n as usize).0
    }
}

/// What an observed move is to an owed [`EraseWitness`] ([`CursorGlow::erase_hop`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EraseHop {
    /// A same-row retreat no wider than the erases owe.
    Retreat(u16),
    /// A same-row forward hop the presses typed before the erase pay.
    Ahead,
    /// A same-row forward hop whose last `n` cells, past what those presses pay.
    Late(u16),
    /// Any other move: the erase.
    Past,
}

/// The park: Ink repaints the input row by parking the caret left (a same-row backward move).
#[derive(Clone, Copy)]
pub(super) struct HeldPark {
    /// The source row.
    pub(super) row: u16,
    pub(super) landing_row: u16,
    /// Content proved that a foreign-row landing left the source intact.
    pub(super) cross_row: bool,
    pub(super) origin: u16,
    pub(super) landing: u16,
    pub(super) at: Instant,
    pub(super) cfg: GlowConfig,
    pub(super) geom: Geom,
    /// Whether a glyph sat at the landing cell (`landing - 1`) when the park was held.
    pub(super) landing_glyph: bool,
    /// Which glyph sat at the landing cell when the park was held.
    pub(super) landing_cell: Option<char>,
    /// The content witness carried this exact park's source row away.
    pub(super) content_followed: bool,
}

impl HeldPark {
    #[inline]
    pub(super) fn patience(&self) -> Duration {
        if self.cross_row {
            Duration::from_secs_f32(IN_FLIGHT_PATIENCE_S)
        } else {
            Duration::from_secs_f32(CursorGlow::TYPE_HINT_FRESH)
        }
    }

    #[inline]
    pub(super) fn fresh(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.at) <= self.patience()
    }
}

/// `trail status`'s delivered-insert rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InsertTally {
    /// Inserts the host reported delivered.
    pub delivered: u64,
    /// Inserts whose echo the seam licensed and laid as one sweep.
    pub lit: u64,
    /// Program rewrites of a lit span the seam retracted.
    pub retracted: u64,
    /// The last delivered insert's cell width, of the last arm that stood.
    pub last_cells: u16,
}

impl InsertTally {
    /// Zeroed delivered-insert tally.
    pub const EMPTY: Self = Self {
        delivered: 0,
        lit: 0,
        retracted: 0,
        last_cells: 0,
    };
}

/// The delivered-insert seam.
#[derive(Clone, Copy, Debug)]
pub(super) struct InsertSeam {
    /// The licence armed by the host when an insert's bytes provably landed.
    pub(super) armed: Option<InsertLicence>,
    /// What the newest arm overwrote, for the revoke of that arm.
    pub(super) undo: InsertArmUndo,
    /// Where the last delivered insert was laid.
    pub(super) span: Option<InsertSpan>,
    /// The last refused same-row forward hop.
    pub(super) pending_hop: Option<PendingHop>,
    /// The presses an unknown-width insert's hop echoed.
    pub(super) orphans: Option<UnknownInsertOrphans>,
    /// Up to two ambiguous post-insert keys, removed from the press pool at orphan cleanup.
    pub(super) orphan_exact: Option<OrphanExactRun>,
    /// `trail status`'s insert rows ([`InsertTally`]).
    pub(super) tally: InsertTally,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct UnknownInsertOrphans {
    pub(super) dispatched_at: Instant,
    pub(super) delivered_at: Instant,
    pub(super) laid_at: Instant,
    /// Absent when the insert lacked a sampled row/print generation or its coordinate space.
    pub(super) site: Option<(u16, u16, u64)>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OrphanExactKey {
    pub(super) pressed_at: Instant,
    pub(super) row: u16,
    pub(super) col: u16,
    pub(super) glyph: char,
    pub(super) print_seq: u64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OrphanExactRun {
    pub(super) keys: [Option<OrphanExactKey>; ORPHAN_EXACT_KEYS_MAX],
    pub(super) next: usize,
    pub(super) len: usize,
    /// The inserted span's landing, fixed even after one key is proved.
    pub(super) insert_col: u16,
}

impl OrphanExactRun {
    #[inline]
    pub(super) const fn current(self) -> Option<OrphanExactKey> {
        if self.next < self.keys.len() {
            self.keys[self.next]
        } else {
            None
        }
    }
}

/// What an arm overwrote, restored when that arm is revoked at its exact instant.
#[derive(Clone, Copy, Debug)]
pub(super) struct InsertArmUndo {
    /// The still-fresh, unspent licence the newest arm accumulated.
    pub(super) licence: Option<InsertLicence>,
    /// `last_insert_cells=` before the newest arm.
    pub(super) last_cells: u16,
}

impl InsertArmUndo {
    pub(super) const EMPTY: Self = Self {
        licence: None,
        last_cells: 0,
    };
}

impl Default for InsertArmUndo {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl Default for InsertSeam {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl InsertSeam {
    pub(super) const EMPTY: Self = Self {
        armed: None,
        undo: InsertArmUndo::EMPTY,
        span: None,
        pending_hop: None,
        orphans: None,
        orphan_exact: None,
        tally: InsertTally::EMPTY,
    };
    /// Arm a delivered insert of `cells` at `at` on `row`.
    pub(super) fn arm(
        &mut self,
        at: Instant,
        dispatched_at: Instant,
        cells: u16,
        known: bool,
        breaks: bool,
        row: Option<u16>,
    ) -> InsertLicence {
        self.tally.delivered += 1;
        self.undo.last_cells = self.tally.last_cells;
        self.tally.last_cells = cells;
        let prior = self.armed.filter(|prev| {
            at.saturating_duration_since(prev.at).as_secs_f32() <= CursorGlow::INSERT_HINT_FRESH
        });
        let armed = match prior {
            Some(prev) if prev.breaks || breaks => InsertLicence {
                at,
                dispatched_at: prev.dispatched_at.min(dispatched_at),
                cells: CursorGlow::INSERT_GESTURE_CELLS,
                known: false,
                breaks: false,
                row: row.or(prev.row),
            },
            Some(prev) => InsertLicence {
                at,
                dispatched_at: prev.dispatched_at.min(dispatched_at),
                cells: if known || prev.known {
                    prev.cells.saturating_add(cells)
                } else {
                    prev.cells
                },
                known: prev.known && known,
                breaks: false,
                row: row.or(prev.row),
            },
            None => InsertLicence {
                at,
                dispatched_at,
                cells,
                known,
                breaks,
                row,
            },
        };
        self.undo.licence = prior;
        self.armed = Some(armed);
        armed
    }

    /// Revoke the arm stamped at exactly `at` (a queued or failed dispatch).
    pub(super) fn revoke_arm_at(&mut self, at: Instant) {
        if self.armed.is_some_and(|i| i.at == at) {
            self.armed = self.undo.licence.take();
            self.tally.delivered = self.tally.delivered.saturating_sub(1);
            self.tally.last_cells = self.undo.last_cells;
        }
    }

    /// The armed licence while fresh.
    #[inline]
    pub(super) fn fresh(&self, now: Instant) -> Option<InsertLicence> {
        self.armed.filter(|i| {
            let window = if i.known {
                CursorGlow::INSERT_HINT_FRESH
            } else {
                CursorGlow::INSERT_GESTURE_HINT_FRESH
            };
            now.saturating_duration_since(i.at).as_secs_f32() <= window
        })
    }

    /// Drop the licence and the credit it accumulated.
    #[inline]
    pub(super) const fn clear(&mut self) {
        self.armed = None;
        self.undo.licence = None;
    }

    /// A coordinate-space teardown.
    #[inline]
    pub(super) const fn teardown(&mut self) {
        self.clear();
        self.span = None;
        self.pending_hop = None;
        self.orphans = None;
        self.orphan_exact = None;
    }

    /// Move the row-addressed members with a scroll or band move under `map`.
    pub(super) fn translate_rows(&mut self, map: impl Fn(u16) -> Option<u16>) {
        self.span = None;
        self.pending_hop = None;
        if let Some(orphans) = self.orphans.as_mut() {
            orphans.site = None;
        }
        self.orphan_exact = None;
        for ins in [&mut self.armed, &mut self.undo.licence]
            .into_iter()
            .flatten()
        {
            ins.row = ins.row.and_then(&map);
        }
    }

    /// Take the refused hop ([`PendingHop`]) if it is the insert `armed` now delivers.
    pub(super) fn take_retro_hop(
        &mut self,
        armed: InsertLicence,
        at: Instant,
        dispatched_at: Option<Instant>,
    ) -> Option<PendingHop> {
        let hop = self.pending_hop.take()?;
        (at.saturating_duration_since(hop.at).as_secs_f32() <= CursorGlow::INSERT_RETRO_FRESH
            && dispatched_at.is_none_or(|dispatched| hop.at >= dispatched)
            && Self::retro_hop_is_the_inserts(hop, armed.cells, armed.known, armed.row))
        .then_some(hop)
    }

    /// Whether a refused hop is the insert now delivered.
    #[inline]
    pub(super) const fn retro_hop_is_the_inserts(
        hop: PendingHop,
        cells: u16,
        known: bool,
        row: Option<u16>,
    ) -> bool {
        let width = hop.col1.saturating_sub(hop.col0);
        if known {
            width == cells
        } else {
            width > 0
                && width <= CursorGlow::INSERT_GESTURE_CELLS
                && matches!(row, Some(r) if r == hop.row)
        }
    }

    /// Whether a retreat to `col` on `row` lands inside the span a delivered insert laid.
    #[inline]
    pub(super) fn span_admits_retreat(&self, row: u16, col: u16, now: Instant) -> bool {
        self.span.is_some_and(|span| {
            span.row == row
                && col >= span.col0
                && col <= span.col1
                && now.saturating_duration_since(span.laid_at).as_secs_f32()
                    <= CursorGlow::INSERT_REWRITE_FRESH
        })
    }

    /// The licence was laid as `span`.
    #[inline]
    pub(super) const fn laid(&mut self, span: InsertSpan) {
        self.clear();
        self.pending_hop = None;
        self.span = Some(span);
        self.orphan_exact = None;
        self.tally.lit += 1;
    }

    /// The laid span was retracted to the program's new caret `col`.
    #[inline]
    pub(super) const fn retracted(&mut self, col: u16) {
        if let Some(span) = self.span.as_mut() {
            span.col1 = col;
        }
        if let Some(orphans) = self.orphans.as_mut() {
            orphans.site = None;
        }
        self.orphan_exact = None;
        self.tally.retracted += 1;
    }

    /// Once an unknown insert's hop is old enough for any next-frame echo to have spent its own.
    pub(super) fn retire_orphans(&mut self, now: Instant) -> Option<UnknownInsertOrphans> {
        let orphans = self.orphans?;
        (now.saturating_duration_since(orphans.laid_at).as_secs_f32()
            > CursorGlow::INSERT_REWRITE_FRESH)
            .then(|| {
                self.orphans = None;
                orphans
            })
    }
}

/// `trail status`'s in-flight rows (the stall).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InFlightTally {
    /// Moves licensed by the in-flight pool with no stamp fresh.
    pub licensed: u64,
    /// Times an observed edge forgot a pool with a live press in it.
    pub forgotten: u64,
    /// The live pool: unpaid cells in flight at the last tick.
    pub credits: usize,
    /// Presses typed into a tty that will never echo them.
    pub swallowed_no_echo: u64,
    /// Held parks whose return came within the window and was judged as one `origin -> target`.
    pub park_returns: u64,
    /// Held parks flushed through the ordinary body at their own clock.
    pub park_flushed: u64,
}

impl InFlightTally {
    /// Zeroed in-flight tally.
    pub const EMPTY: Self = Self {
        licensed: 0,
        forgotten: 0,
        credits: 0,
        swallowed_no_echo: 0,
        park_returns: 0,
        park_flushed: 0,
    };
}

impl CursorGlow {
    /// Host key hint: one Backspace keypress.
    pub fn note_backspace(&mut self, now: Instant) {
        self.note_backspace_erasing(now, None);
    }

    /// [`Self::note_backspace`] with the erased glyph's cell width, priced by the host.
    pub fn note_backspace_erasing(&mut self, now: Instant, cells: Option<u16>) {
        self.clear_typed_keeping_hand(now);
        let newest = self.type_press_ring.newest();
        let newest_press = newest.and_then(|i| self.type_press_ring.slots[i]);
        match cells {
            Some(cells) => self
                .type_press_ring
                .retire_newest_cells(u8::try_from(cells).unwrap_or(u8::MAX)),
            None => self.type_press_ring.retire_newest(),
        }
        let retired = newest
            .filter(|&i| self.type_press_ring.slots[i].is_none())
            .and(newest_press)
            .and_then(|(_, _, glyph)| glyph);
        let mut owed = self
            .last_erase
            .filter(|erase| erase.owed_at(now))
            .map_or(([' '; ERASE_RETIRED_MAX], 0, 0), |erase| {
                (erase.retired, erase.retired_n, erase.owed)
            });
        if let Some(glyph) = retired
            && usize::from(owed.1) < ERASE_RETIRED_MAX
        {
            owed.0.copy_within(..usize::from(owed.1), 1);
            owed.0[0] = glyph;
            owed.1 += 1;
        }
        self.kill_hint = None;
        self.unsettle();
        self.quench = (self.quench + Self::QUENCH_GAIN).min(1.0);
        self.heat *= Self::QUENCH_COOL;
        self.coal *= Self::QUENCH_COOL;
        self.momentum.delete(now);
        self.erase_mom.advance(now);
        self.quench_hint = Some(now);
        self.last_erase = Some(EraseWitness {
            at: now,
            retired: owed.0,
            retired_n: owed.1,
            owed: owed.2.saturating_add(cells.unwrap_or(1)),
        });
        self.bs_poof_hint = Some(now);
        self.bs_baseline = self.row_fill_at_key();
        if self.v2.engaged() {
            self.v2.on_event(rk::Event::Erase, now);
        }
    }

    /// Host key hint: a navigation keypress (Ctrl-a/Ctrl-E, Home/End, arrows, word/line motions).
    pub fn note_navigation(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.unowned_caret = None;
        self.flush_held_park();
        if self.newline_hint.take().is_some() && self.v2.engaged() {
            self.v2.on_event(rk::Event::CancelComposerNewline, now);
        }
        self.unsettle();
        self.user_gesture_hint = None;
        self.nav_hint = Some(now);
    }

    /// Host key hint: one plain typed-glyph keypress.
    #[inline]
    pub fn note_typed(&mut self, now: Instant) {
        self.note_typed_cells(now, 1);
    }

    /// [`Self::note_typed`] with the committed text's cell width.
    #[inline]
    pub fn note_typed_cells(&mut self, now: Instant, cells: u16) {
        self.note_typed_glyph(now, cells, false, rk::TypedClass::Glyph);
    }

    /// [`Self::note_typed_cells`] with the glyph class spelled by the host.
    #[inline]
    pub fn note_typed_glyph(
        &mut self,
        now: Instant,
        cells: u16,
        shifted: bool,
        class: rk::TypedClass,
    ) {
        self.note_typed_glyph_with_expected(now, cells, shifted, class, None);
    }

    pub(super) fn note_typed_glyph_with_expected(
        &mut self,
        now: Instant,
        cells: u16,
        shifted: bool,
        class: rk::TypedClass,
        expected: Option<char>,
    ) {
        self.unsettle();
        self.supersede_typed_press();
        self.type_hint.stamp(now);
        let credits = cells.clamp(1, Self::RAINBOW_TYPED_SWEEP_MAX as u16) as u8;
        self.type_press_ring.bank(now, credits, expected);
        self.last_committed_type = Some(now);
        if self.v2.engaged() {
            self.v2.on_event(
                rk::Event::Typed {
                    cells: cells.min(Self::RAINBOW_TYPED_SWEEP_MAX as u16),
                    shifted,
                    class,
                },
                now,
            );
        }
    }

    /// The one-cell typed hint with the input glyph's exact identity.
    #[inline]
    pub fn note_typed_expected(
        &mut self,
        now: Instant,
        cells: u16,
        shifted: bool,
        class: rk::TypedClass,
        glyph: char,
    ) {
        self.note_typed_glyph_with_expected(
            now,
            cells,
            shifted,
            class,
            (cells == 1).then_some(glyph),
        );
    }

    /// Record a main-screen Enter classifier stamp, the bare-Enter licence for the jump arm.
    #[inline]
    pub fn note_return(&mut self, now: Instant) {
        self.note_return_keeping_hand(now);
        self.hand_row = None;
        self.unowned_caret = None;
    }

    /// [`Self::note_return`] for an Enter into a foreground program's composer.
    pub fn note_return_keeping_hand(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.flush_held_park();
        self.unsettle();
        self.user_gesture_hint = None;
        self.return_hint = Some(now);
        if self.v2.engaged() {
            self.v2.on_event(rk::Event::Return, now);
        }
    }

    /// Record a Tab / scripted-gesture classifier boundary.
    pub fn note_user_gesture(&mut self, now: Instant) {
        self.flush_held_park();
        self.supersede_typed_press();
        self.unsettle();
        self.user_gesture_hint = Some(now);
    }

    /// Host delivery edge for the key classes.
    #[inline]
    pub const fn note_delivered(&mut self, at: Instant, class: DeliveredClass) {
        self.unsettle();
        match class {
            DeliveredClass::Typed => self.type_hint.stamp(at),
            DeliveredClass::Return => self.return_hint = Some(at),
            DeliveredClass::Newline => self.newline_hint = Some(at),
            DeliveredClass::Nav => self.nav_hint = Some(at),
            DeliveredClass::Gesture => self.user_gesture_hint = Some(at),
            DeliveredClass::Erase => {
                self.quench_hint = Some(at);
                self.bs_poof_hint = Some(at);
                self.bs_baseline = self.row_fill_at_delivery();
            }
            DeliveredClass::Kill { moves_cursor, word } => {
                self.kill_hint = Some(at);
                self.kill_hint_word = word;
                self.bs_baseline = self.row_fill_at_delivery();
                if moves_cursor {
                    self.nav_hint = Some(at);
                    self.kill_retreat_pending = Some(at);
                }
            }
        }
    }

    /// The row fill an erase key stamps as its pre-erase baseline ([`Self::bs_baseline`]).
    #[inline]
    pub(super) const fn row_fill_at_key(&self) -> Option<(u16, u16)> {
        match self.row_cur_meta {
            Some(m) => Some((m.row, m.fill)),
            None => match self.row_prev_meta {
                Some(m) => Some((m.row, m.fill)),
                None => None,
            },
        }
    }

    /// The row fill an erase delivery stamps as its baseline.
    #[inline]
    pub(super) const fn row_fill_at_delivery(&self) -> Option<(u16, u16)> {
        match self.row_prev_meta {
            Some(m) => Some((m.row, m.fill)),
            None => match self.row_cur_meta {
                Some(m) => Some((m.row, m.fill)),
                None => None,
            },
        }
    }

    /// [`Self::note_insert_delivered_from`] for a test that has no dispatch instant to hand.
    #[cfg(test)]
    pub fn note_insert_delivered(&mut self, at: Instant, width: InsertWidth) {
        self.deliver_insert(at, None, width);
    }

    /// Host delivery edge: an insert's bytes.
    #[inline]
    pub fn note_insert_delivered_from(
        &mut self,
        dispatched_at: Instant,
        at: Instant,
        width: InsertWidth,
    ) {
        self.deliver_insert(at, Some(dispatched_at), width);
    }

    pub(super) fn deliver_insert(
        &mut self,
        at: Instant,
        dispatched_at: Option<Instant>,
        width: InsertWidth,
    ) {
        let Some(armed) = self.arm_insert(at, dispatched_at.unwrap_or(at), width) else {
            return;
        };
        if let Some(hop) = self.insert.take_retro_hop(armed, at, dispatched_at) {
            let ins = InsertLicence {
                row: Some(hop.row),
                ..armed
            };
            for entry in self.anchor_rows.iter_mut().flatten() {
                if entry.row == hop.row {
                    entry.tainted = false;
                }
            }
            self.lay_insert(hop.row, hop.col0, hop.row, hop.col1, ins, at);
        }
    }

    /// The dispatch-time arm: a bare Tab / ⌃V written inline arms the insert licence at dispatch.
    #[inline]
    pub fn note_insert_armed(&mut self, at: Instant, width: InsertWidth) {
        let _ = self.arm_insert(at, at, width);
    }

    /// The arm shared by [`Self::deliver_insert`] and [`Self::note_insert_armed`].
    pub(super) fn arm_insert(
        &mut self,
        at: Instant,
        dispatched_at: Instant,
        width: InsertWidth,
    ) -> Option<InsertLicence> {
        let (cells, known, breaks) = match width {
            InsertWidth::Cells(0) => return None,
            InsertWidth::Cells(cells) => (cells, true, false),
            InsertWidth::Line(0) | InsertWidth::Unknown => {
                (Self::INSERT_GESTURE_CELLS, false, false)
            }
            InsertWidth::Line(cells) => (cells, true, true),
        };
        self.recent_typed_run = None;
        self.unsettle();
        let row = self.insert_row(at);
        Some(
            self.insert
                .arm(at, dispatched_at, cells, known, breaks, row),
        )
    }

    /// The insert's row at `now`, read once as [`InsertLicence::row`].
    #[inline]
    pub(super) fn insert_row(&self, now: Instant) -> Option<u16> {
        let established = self.last_anchor_sweep.map(|(row, _)| row);
        if let Some((row, _)) = self.last
            && established.is_none_or(|er| er == row)
        {
            return Some(row);
        }
        self.last_licensed_row
            .filter(|(_, at)| {
                now.saturating_duration_since(*at).as_secs_f32() <= rk::ribbon::CHAIN_GAP_MAX
            })
            .map(|(row, _)| row)
            .or(established)
    }

    /// Remember a refused same-row forward hop `col0 → col1` on `row` for one in-flight delivery.
    #[inline]
    pub(super) const fn remember_refused_hop(
        &mut self,
        row: u16,
        col0: u16,
        col1: u16,
        now: Instant,
        cfg: &GlowConfig,
    ) {
        if matches!(cfg.style, GlowStyle::RainbowKitty) {
            self.insert.pending_hop = Some(PendingHop {
                row,
                col0,
                col1,
                at: now,
            });
        }
    }

    /// Move the insert's row-addressed members with a scroll or band move.
    #[inline]
    pub(super) fn translate_insert_rows(&mut self, map: &impl Fn(u16) -> Option<u16>) {
        self.insert.translate_rows(map);
        self.last_licensed_row = None;
    }

    /// How many cells the fresh delivered insert `ins` may buy on a same-row forward echo.
    #[inline]
    pub(super) fn insert_reach(&self, ins: InsertLicence, now: Instant) -> usize {
        usize::from(ins.cells) + self.typed_credits_within(now)
    }

    /// The insert echo shape: a same-row forward move no wider than the fresh insert's reach.
    #[inline]
    pub(super) fn insert_echo(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &GlowConfig,
    ) -> Option<InsertLicence> {
        if !matches!(cfg.style, GlowStyle::RainbowKitty) || cr != pr || cc <= pc {
            return None;
        }
        let ins = self.insert.fresh(now)?;
        let hop = usize::from(cc - pc);
        (hop <= self.insert_reach(ins, now)).then_some(ins)
    }

    /// Row identity for the insert.
    #[inline]
    pub(super) const fn insert_row_identity(row: u16, hop: usize, ins: InsertLicence) -> bool {
        let own_row = matches!(ins.row, Some(r) if r == row);
        own_row || (ins.known && hop >= ins.cells as usize)
    }

    /// The insert's echo on the visible lane, shared by `spawn_judged` and the hide bridge.
    #[inline]
    pub(super) fn visible_insert_echo(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &GlowConfig,
    ) -> Option<InsertLicence> {
        let ins = self.insert_echo(pr, pc, cr, cc, now, cfg)?;
        let hop = usize::from(cc - pc);
        (Self::insert_row_identity(cr, hop, ins) && !self.fresher_class_owns_hop(hop, ins, now))
            .then_some(ins)
    }

    /// The line insert's echo across its break ([`InsertWidth::Line`]).
    pub(super) fn line_insert_echo(
        &self,
        (pr, pc): (u16, u16),
        (cr, cc): (u16, u16),
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) -> Option<(InsertLicence, u16)> {
        if !matches!(cfg.style, GlowStyle::RainbowKitty) || pr.checked_add(1) != Some(cr) || cc > pc
        {
            return None;
        }
        let ins = self
            .insert
            .fresh(now)
            .filter(|ins| ins.known && ins.breaks && ins.row == Some(pr))?;
        let end = pc
            .checked_add(ins.cells)
            .filter(|&end| usize::from(end) < geom.cols)?;
        let ahead = self
            .type_press_ring
            .cells_where(now, |key_at| key_at <= ins.dispatched_at);
        (ahead == 0 && !self.fresher_class_owns_hop(usize::from(ins.cells), ins, now))
            .then_some((ins, end))
    }

    /// Whether a fresher press class owns this same-row forward hop.
    pub(super) fn fresher_class_owns_hop(
        &self,
        hop: usize,
        ins: InsertLicence,
        now: Instant,
    ) -> bool {
        if hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
            || hint_fresh(self.reflow_hint, now, Self::REFLOW_HINT_FRESH)
            || (hint_fresh(self.user_gesture_hint, now, Self::USER_GESTURE_HINT_FRESH)
                && self.user_gesture_hint != Some(ins.at))
        {
            return true;
        }
        let credits = self
            .type_press_ring
            .cells_where(now, |key_at| key_at <= ins.dispatched_at);
        credits > 0 && credits >= hop
    }

    /// Whether a keyed erase class is fresh (Backspace, kill chord, navigation).
    #[inline]
    pub(super) fn erase_key_fresh(&self, now: Instant) -> bool {
        hint_fresh(self.quench_hint, now, Self::QUENCH_HINT_FRESH)
            || hint_fresh(self.kill_hint, now, Self::KILL_HINT_FRESH)
            || hint_fresh(self.bs_poof_hint, now, Self::KILL_HINT_FRESH)
            || hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH)
    }

    /// Whether the row probe, when it speaks for `row` this frame.
    #[inline]
    pub(super) fn probe_blank_from(&self, row: u16, col: u16, now: Instant) -> bool {
        let Some(meta) = self.row_cur_meta else {
            return true;
        };
        if meta.row != row
            || now.saturating_duration_since(meta.at).as_secs_f32() > Self::POOF_PROBE_STALE
        {
            return true;
        }
        meta.fill <= col
    }

    /// The insert's rewrite shape.
    #[inline]
    pub(super) fn insert_rewrite(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &GlowConfig,
    ) -> Option<u16> {
        if !matches!(cfg.style, GlowStyle::RainbowKitty) || cr != pr || cc >= pc {
            return None;
        }
        if !self.insert.span_admits_retreat(cr, cc, now)
            || self.erase_key_fresh(now)
            || !self.probe_blank_from(cr, cc, now)
        {
            return None;
        }
        Some(pc - cc)
    }

    /// Lay a delivered insert's echo `pc → cc` on `row`.
    pub(super) fn lay_insert(
        &mut self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        ins: InsertLicence,
        now: Instant,
    ) {
        self.unsettle();
        if let Some(orphans) = self.insert.orphans.as_mut() {
            orphans.site = None;
        }
        let hop = usize::from(cc.saturating_sub(pc));
        let surplus = hop.saturating_sub(usize::from(ins.cells));
        if surplus > 0 {
            let credits = self.typed_credits_within(now);
            let presses = self
                .type_press_ring
                .spend_counting(now, surplus.min(credits));
            for _ in 0..presses {
                let _ = self.type_hint.take_fresh(now, Self::TYPE_HINT_FRESH);
            }
        }
        if !ins.known && self.type_press_ring.cells_where(now, |t| t <= ins.at) > 0 {
            let site = self
                .row_cur_meta
                .filter(|m| m.row == cr && m.caret == cc && m.at == now)
                .and_then(|_| {
                    self.print_anchor
                        .filter(|&(row, col, _)| row == cr && col == cc)
                        .map(|(_, _, seq)| (cr, cc, seq))
                });
            self.insert.orphans = Some(UnknownInsertOrphans {
                dispatched_at: ins.dispatched_at,
                delivered_at: ins.at,
                laid_at: now,
                site,
            });
        }
        self.insert.laid(InsertSpan {
            row: cr,
            col0: pc,
            col1: cc,
            laid_at: now,
        });
        if self.user_gesture_hint == Some(ins.at) {
            self.user_gesture_hint = None;
        }
        self.last_licensed_row = Some((cr, now));
        self.hand_lands_on(cr);
        if self.v2.engaged() {
            self.v2.on_event(
                rk::Event::Sweep {
                    row: cr,
                    col0: pc,
                    col1: cc,
                },
                Self::sweep_born(ins.at, now),
            );
            self.v2.on_event(
                rk::Event::Move {
                    from: (pr, pc),
                    to: (cr, cc),
                    licence: rk::Licence::Insert,
                    dir: rk::Dir::of(i32::from(cc) - i32::from(pc), i32::from(cr) - i32::from(pr)),
                },
                now,
            );
        }
        self.spawns += 1;
        self.log_licensed_as(now, (pr, pc), (cr, cc), AdmissionRecord::LICENCE_INSERT);
    }

    /// Retract a delivered insert's span to the program's new caret.
    pub(super) fn retract_insert(&mut self, row: u16, col: u16, cells: u16, now: Instant) {
        self.unsettle();
        if self.v2.engaged() {
            self.v2
                .on_event(rk::Event::Rewrite { row, col, cells }, now);
        }
        self.insert.retracted(col);
        self.last_licensed_row = Some((row, now));
        self.spawns += 1;
        self.log_licensed_as(
            now,
            (row, col.saturating_add(cells)),
            (row, col),
            AdmissionRecord::LICENCE_REWRITE,
        );
    }

    /// `trail status`'s insert rows.
    #[inline]
    #[must_use]
    pub const fn insert_tally(&self) -> InsertTally {
        self.insert.tally
    }

    /// `trail status`'s in-flight rows ([`InFlightTally`]), read at the last tick's clock.
    #[inline]
    #[must_use]
    pub fn in_flight_tally(&self) -> InFlightTally {
        InFlightTally {
            credits: self.heat_at.map_or(0, |now| self.typed_credits_within(now)),
            ..self.in_flight_tally
        }
    }

    /// A composer newline landed.
    pub fn note_newline_break(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.unsettle();
        self.newline_hint = Some(now);
        if self.v2.engaged() {
            self.v2.on_event(rk::Event::ComposerNewline, now);
        }
    }

    /// Record a grid-geometry classifier boundary.
    pub fn note_reflow(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.clear_typed(now);
        self.unsettle();
        self.reflow_hint = Some(now);
    }

    /// A user motion press: a key whose purpose is moving the cursor.
    pub fn note_motion(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.unowned_caret = None;
        self.clear_typed_keeping_hand(now);
        self.unsettle();
        self.nav_hint = Some(now);
        self.kill_retreat_pending = None;
    }

    /// Explicit scripted preview/test provenance.
    #[doc(hidden)]
    pub fn note_synthetic_move(&mut self, now: Instant) {
        self.recent_typed_run = None;
        self.clear_typed(now);
        self.unsettle();
        self.user_gesture_hint = Some(now);
    }

    /// Scripted typed-move twin for previews.
    #[inline]
    #[doc(hidden)]
    pub fn note_synthetic_typed(&mut self, now: Instant, cells: u16) {
        self.note_typed_cells(now, cells);
    }

    /// Whether the bounded reflow classifier stamp is currently armed.
    #[inline]
    #[must_use]
    pub const fn reflow_hint_armed(&self) -> bool {
        self.reflow_hint.is_some()
    }

    /// Committed cell credits in flight (unpaid, inside [`IN_FLIGHT_PATIENCE_S`]).
    #[inline]
    pub(super) fn typed_credits_within(&self, now: Instant) -> usize {
        self.type_press_ring.cells_within(now)
    }

    /// The share rule: `paid` cells from `credits` unpaid presses buy a `cells`-wide typed hop.
    #[inline]
    pub(super) const fn share_rule(credits: usize, paid: usize, cells: usize) -> bool {
        credits >= 2 && paid * 4 >= cells * 3
    }

    /// Whether the unpaid presses pay for the same-row forward hop `pc → cc` on `row`.
    pub(super) fn typed_share_paid(&self, row: u16, pc: u16, cc: u16, now: Instant) -> bool {
        let credits = self.typed_credits_within(now);
        if credits < 2 {
            return false;
        }
        let cells = usize::from(cc.saturating_sub(pc));
        if Self::share_rule(credits, credits, cells) {
            return true;
        }
        let paid = credits.saturating_add(
            self.swept_wide_continuations(row, pc, cc, now)
                .min(self.type_press_ring.presses_within(now)),
        );
        Self::share_rule(credits, paid, cells)
    }

    /// How many columns of the same-row run `[from_col, to_col)` are wide continuations (`'\0'`
    /// in the row probe, as `Terminal::row_cols_into` spells them).
    pub(super) fn swept_wide_continuations(
        &self,
        row: u16,
        from_col: u16,
        to_col: u16,
        now: Instant,
    ) -> usize {
        let Some(meta) = self.row_cur_meta else {
            return 0;
        };
        if meta.row != row
            || now.saturating_duration_since(meta.at).as_secs_f32() > Self::POOF_PROBE_STALE
        {
            return 0;
        }
        let lo = usize::from(from_col);
        let hi = usize::from(to_col).min(self.row_cur.len());
        if lo >= hi {
            return 0;
        }
        self.row_cur[lo..hi]
            .iter()
            .filter(|&&ch| ch == '\0')
            .count()
    }

    /// Host key hint withheld.
    #[inline]
    pub const fn note_typed_swallowed_no_echo(&mut self) {
        self.in_flight_tally.swallowed_no_echo += 1;
    }

    /// Forget every in-flight press on an observed edge the presses cannot explain.
    pub(super) fn forget_typed_credits(&mut self, now: Instant) {
        self.insert.orphans = None;
        self.insert.orphan_exact = None;
        if self.type_press_ring.is_empty() {
            return;
        }
        let live = self.type_press_ring.any_within(now);
        self.type_press_ring.forget();
        if live {
            self.in_flight_tally.forgotten += 1;
        }
    }

    /// Whether this move is a same-row forward echo with an unpaid press behind it.
    #[allow(
        clippy::too_many_arguments,
        reason = "from/to cursor cells + clock + config + geometry; the seam's one predicate, called from four sites"
    )]
    pub(super) fn unpaid_typed_echo(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
    ) -> bool {
        if !matches!(cfg.style, GlowStyle::RainbowKitty) {
            return false;
        }
        let credits = self.typed_credits_within(now);
        if cr == pr && cc > pc {
            return credits >= usize::from(cc - pc).min(2);
        }
        if credits >= 1 && self.carried_key_move == Some(((pr, pc), (cr, cc))) {
            return true;
        }
        if let Some(cells) = self.fold_shape(pr, pc, cr, cc, geom) {
            return cells >= 1 && credits >= cells;
        }
        self.coalesced_fold_shape(pr, pc, cr, cc, now, geom)
            .is_some_and(|cells| credits == cells)
    }

    /// The alt-screen blink discriminator.
    #[inline]
    pub(super) fn blink_fresh(&self, now: Instant) -> bool {
        hint_fresh(self.blink_hint, now, Self::BLINK_HINT_FRESH)
    }

    /// The focused pane's `(first column, width)` under the host's pane columns.
    #[inline]
    pub(super) const fn pane_span(&self, geom: Geom) -> (usize, usize) {
        match self.pane_columns {
            Some((col0, cols)) => (col0 as usize, cols as usize),
            None => (0, geom.cols),
        }
    }

    /// A move's pane-local columns for the fold shapes.
    #[inline]
    pub(super) const fn pane_local(
        &self,
        pc: u16,
        cc: u16,
        geom: Geom,
    ) -> (usize, usize, usize, bool) {
        let (pane_col0, pane_cols) = self.pane_span(geom);
        let pane_col1 = pane_col0.saturating_add(pane_cols);
        let pc_window = pc as usize;
        let cc_window = cc as usize;
        let pane_contains_move = pane_cols > 0
            && pc_window >= pane_col0
            && pc_window < pane_col1
            && cc_window >= pane_col0
            && cc_window < pane_col1;
        (
            pane_cols,
            pc_window.saturating_sub(pane_col0),
            cc_window.saturating_sub(pane_col0),
            pane_contains_move,
        )
    }

    /// The soft-wrapped caret.
    pub(super) fn soft_wrapped_caret(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        pane_col0: usize,
        now: Instant,
    ) -> Option<u16> {
        let cur = self.row_cur_meta?;
        let prev = self.row_prev_meta?;
        if cr != pr.checked_add(1)?
            || usize::from(cc) <= pane_col0
            || cur.row != cr
            || cur.above != NbrProbe::Probed
            || prev.row != pr
            || prev.caret != pc
        {
            return None;
        }
        let blank = |row: &[char], col: usize| row.get(col).is_none_or(|&g| g == ' ');
        let origin = usize::from(pc);
        if !(pane_col0..usize::from(cc)).all(|col| blank(&self.row_cur, col))
            || blank(&self.row_above_cur, origin)
        {
            return None;
        }
        let glyph = self.row_above_cur[origin];
        if !blank(&self.row_prev, origin)
            && (self.row_prev[origin] == glyph
                || !self
                    .type_press_ring
                    .fresh_glyph(now, Self::TYPE_HINT_FRESH, glyph))
        {
            return None;
        }
        let at = |row: &[char], col: usize| row.get(col).copied().unwrap_or(' ');
        if (pane_col0..origin).any(|col| at(&self.row_prev, col) != at(&self.row_above_cur, col)) {
            return None;
        }
        let word_col0 = (pane_col0..origin)
            .rev()
            .take_while(|&col| !blank(&self.row_above_cur, col))
            .last()
            .unwrap_or(origin);
        u16::try_from(word_col0).ok()
    }

    /// Whether the forward echo from a soft-wrapped caret's landing carried the wrap key's word.
    pub(super) fn carried_word_left(&self, sw: rk::ribbon::SoftWrap, cr: u16, cc: u16) -> bool {
        let Some(cur) = self.row_cur_meta else {
            return false;
        };
        if cur.row != cr
            || cur.caret != cc
            || cur.above != NbrProbe::Probed
            || sw.origin.0.checked_add(1) != Some(cr)
        {
            return false;
        }
        (sw.word_col0..=sw.origin.1).all(|col| {
            self.row_above_cur
                .get(usize::from(col))
                .is_none_or(|&g| g == ' ')
        })
    }

    /// The lifted word (the reverse re-wrap).
    pub(super) fn lifted_word(&self, pr: u16, pc: u16, cr: u16, cc: u16) -> Option<u16> {
        let cur = self.row_cur_meta?;
        let prev = self.row_prev_meta?;
        if cr != pr
            || cc >= pc
            || cur.row != cr
            || cur.caret != cc
            || cur.above != NbrProbe::Probed
            || prev.row != pr
            || prev.caret != pc
            || prev.above != NbrProbe::Probed
        {
            return None;
        }
        let unit = rk::witness::unit_at;
        let mut any_changed = false;
        for c in cc..pc {
            let prev_u = unit(&self.row_prev, c);
            if prev_u.is_blank() {
                return None;
            }
            any_changed |= unit(&self.row_cur, c) != prev_u;
        }
        if !any_changed {
            return None;
        }
        let end = u16::try_from(self.ink_end_in_pane(&self.row_above_cur)).ok()?;
        let dst = end.checked_sub(pc - cc)?;
        let before = dst.checked_sub(1)?;
        if !unit(&self.row_above_cur, before).is_blank() {
            return None;
        }
        let arrived = (cc..pc).zip(dst..end).all(|(src, now)| {
            unit(&self.row_above_cur, now) == unit(&self.row_prev, src)
                && unit(&self.row_above_prev, now).is_blank()
        });
        arrived.then_some(dst)
    }

    /// The tight fold shape.
    #[inline]
    pub(super) const fn fold_shape(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        geom: Geom,
    ) -> Option<usize> {
        let (pane_cols, pc_local, cc_local, pane_contains_move) = self.pane_local(pc, cc, geom);
        if cr == pr.saturating_add(1)
            && pane_contains_move
            && cc_local <= 1
            && pc_local.saturating_add(2) >= pane_cols
        {
            Some(
                self.fold_origin_tail(pr, pc_local, pane_cols)
                    .saturating_add(cc_local),
            )
        } else {
            None
        }
    }

    /// The cells a fold lays on its origin row.
    #[inline]
    pub(super) const fn fold_origin_tail(
        &self,
        pr: u16,
        pc_local: usize,
        pane_cols: usize,
    ) -> usize {
        if pc_local.saturating_add(1) == pane_cols
            && matches!(self.wrap_paid_row, Some(r) if r == pr)
        {
            0
        } else {
            pane_cols.saturating_sub(pc_local)
        }
    }

    /// The coalesced fold shape (`shape_wrap`'s second arm, shared with the in-flight seam).
    #[inline]
    pub(super) fn coalesced_fold_shape(
        &self,
        pr: u16,
        pc: u16,
        cr: u16,
        cc: u16,
        now: Instant,
        geom: Geom,
    ) -> Option<usize> {
        let (pane_cols, pc_local, cc_local, pane_contains_move) = self.pane_local(pc, cc, geom);
        (cr == pr.saturating_add(1)
            && pane_contains_move
            && (1..=3).contains(&cc_local)
            && pc_local.saturating_add(4) >= pane_cols
            && (!self.ctx_alt || self.blink_fresh(now)))
        .then_some(
            self.fold_origin_tail(pr, pc_local, pane_cols)
                .saturating_add(cc_local),
        )
    }

    /// Host key hint: one kill keypress (Ctrl-K/U/W, Alt-D, word-delete Backspace, forward Delete).
    pub fn note_kill(&mut self, now: Instant, moves_cursor: bool) {
        self.recent_typed_run = None;
        self.flush_held_park();
        if self.newline_hint.take().is_some() && self.v2.engaged() {
            self.v2.on_event(rk::Event::CancelComposerNewline, now);
        }
        self.unsettle();
        self.forget_typed_credits(now);
        self.bs_poof_hint = None;
        self.kill_hint = Some(now);
        self.quench = (self.quench + Self::KILL_QUENCH_GAIN).min(1.0);
        self.heat *= Self::QUENCH_COOL;
        self.coal *= Self::QUENCH_COOL;
        self.momentum.kill(now);
        self.erase_mom.advance(now);
        self.erase_mom.advance(now);
        self.bs_baseline = self.row_fill_at_key();
        if moves_cursor {
            self.nav_hint = Some(now);
        }
        self.kill_retreat_pending = moves_cursor.then_some(now);
        self.kill_hint_word = false;
    }

    /// [`Self::note_kill`] for a word-scale kill chord (^W, Alt-D, Alt/Ctrl- Backspace).
    #[inline]
    pub fn note_word_kill(&mut self, now: Instant, moves_cursor: bool) {
        self.note_kill(now, moves_cursor);
        self.kill_hint_word = true;
    }

    /// Disarm dangling typed and backspace hints when a key with its own move semantics arrives.
    #[inline]
    pub fn clear_typed(&mut self, now: Instant) {
        self.clear_typed_keeping_hand(now);
        self.hand_row = None;
        self.unowned_caret = self
            .last
            .map(|(row, _)| UnownedCaret { row, parked: false });
    }

    /// A licensed echo of the hand's own input landed on `row`.
    #[inline]
    pub(super) const fn hand_lands_on(&mut self, row: u16) {
        self.hand_row = Some(HandRow::at(row));
        if let Some(caret) = self.unowned_caret
            && caret.row == row
        {
            self.unowned_caret = None;
        }
    }

    /// [`Self::clear_typed`] for a press that cannot take the hand off its row.
    pub fn clear_typed_keeping_hand(&mut self, now: Instant) {
        self.flush_held_park();
        if self.newline_hint.is_some() && self.v2.engaged() {
            self.v2.on_event(rk::Event::CancelComposerNewline, now);
        }
        self.type_hint.clear();
        self.clear_non_typed_hints(now);
    }

    /// The non-typed half of [`Self::clear_typed`].
    pub(super) fn clear_non_typed_hints(&mut self, now: Instant) {
        self.user_gesture_hint = None;
        self.nav_hint = None;
        self.return_hint = None;
        self.reflow_hint = None;
        self.newline_hint = None;
        if self
            .quench_hint
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() > 0.03)
        {
            self.quench_hint = None;
        }
    }

    /// The typed-press supersede, [`Self::clear_typed`]'s sibling for a typed glyph.
    #[inline]
    pub const fn supersede_typed_press(&mut self) {
        self.user_gesture_hint = None;
        self.reflow_hint = None;
    }

    /// Revoke the classifier cohort armed by the input dispatch at `at`, licence terms included.
    pub fn revoke_input_hints_at(&mut self, at: Instant) {
        let clear = |slot: &mut Option<Instant>| {
            if *slot == Some(at) {
                *slot = None;
            }
        };
        self.type_hint.revoke_at(at);
        clear(&mut self.quench_hint);
        clear(&mut self.nav_hint);
        clear(&mut self.return_hint);
        clear(&mut self.user_gesture_hint);
        clear(&mut self.newline_hint);
        clear(&mut self.reflow_hint);
        clear(&mut self.kill_hint);
        self.insert.revoke_arm_at(at);
        let revoke_bs = self.bs_poof_hint == Some(at);
        clear(&mut self.bs_poof_hint);
        if revoke_bs {
            self.bs_baseline = None;
        }
        if self.last_committed_type == Some(at) {
            self.last_committed_type = None;
        }
    }

    /// Revoke the cohort of a dispatch whose inline write failed (the child never saw the key).
    #[inline]
    pub fn revoke_failed_input_at(&mut self, at: Instant) {
        self.revoke_input_hints_at(at);
        self.type_press_ring.revoke_at(at);
    }

    /// Consume every one-shot classifier at a completed hidden→visible boundary where `spawn`.
    pub(super) fn retire_hidden_movement_provenance(&mut self, now: Instant) {
        // A held park is spared while fresh, like the typed bank: Ink brackets
        // its park and rewrite in DECTCEM hides, so a boundary can complete
        // mid-park. A park past its window is judged now, as the tick would.
        if self.held_park.is_some_and(|p| !p.fresh(now)) {
            self.flush_held_park();
        }
        self.type_hint.retire_stale(now, Self::TYPE_HINT_FRESH);
        if self.insert.fresh(now).is_none() {
            self.insert.clear();
        }
        self.quench_hint = None;
        if !hint_fresh(self.nav_hint, now, Self::NAV_HINT_FRESH) {
            self.nav_hint = None;
        }
        self.return_hint = None;
        self.user_gesture_hint = None;
        self.newline_hint = None;
        self.reflow_hint = None;
        self.type_press_ring.retire_stale(now);
        if self
            .last_committed_type
            .is_some_and(|t| now.saturating_duration_since(t).as_secs_f32() > Self::TYPE_HINT_FRESH)
        {
            self.last_committed_type = None;
        }
    }

    /// The unconditional twin of [`Self::retire_hidden_movement_provenance`] for the unseeded.
    pub(super) fn retire_all_movement_provenance(&mut self) {
        self.flush_held_park();
        self.type_hint.clear();
        self.insert.clear();
        self.insert.pending_hop = None;
        self.insert.orphans = None;
        self.insert.orphan_exact = None;
        self.quench_hint = None;
        self.nav_hint = None;
        self.return_hint = None;
        self.user_gesture_hint = None;
        self.newline_hint = None;
        self.reflow_hint = None;
        self.type_press_ring.forget();
        self.last_committed_type = None;
    }

    /// A hidden→visible relocation the bounded ConPTY bridge declined never reaches `spawn`.
    pub(super) fn retire_declined_hidden_relocation(&mut self, cur: (u16, u16)) {
        self.retire_all_movement_provenance();
        self.drop_row_probe();
        self.crown_until = None;
        if self.v2.engaged() {
            self.v2.observe_caret(cur);
        }
    }
}
