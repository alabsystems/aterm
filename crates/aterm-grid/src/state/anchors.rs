// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Span anchors tracked across grid row motion for the holder fence.

/// The most anchors a grid keeps: eight marks of four spans, two ends each.
pub const MAX_ANCHORS: usize = 64;

/// Slot identifier in an [`AnchorSet`], reused after [`AnchorSet::free`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnchorId(u16);

impl AnchorId {
    /// The slot index (a checkpoint carries it).
    #[must_use]
    #[inline]
    pub const fn index(self) -> u16 {
        self.0
    }

    /// The anchor in slot `index`, as a checkpoint names it.
    #[must_use]
    #[inline]
    pub const fn from_index(index: u16) -> Self {
        Self(index)
    }
}

/// Which boundary of a row span an anchor represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorRole {
    /// The span's first row (inclusive).
    Start,
    /// The row after the span's last (exclusive).
    End,
}

/// One anchor: an absolute row and the boundary it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    /// The absolute row (`Grid::visible_to_absolute` numbering).
    pub row: u64,
    /// Which boundary of its span this is.
    pub role: AnchorRole,
    /// Whether the slot holds an anchor (a freed slot keeps its last row).
    pub live: bool,
}

/// Span anchors tracked in [`GridPresentationState`](super::GridPresentationState).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnchorSet {
    anchors: Vec<Anchor>,
    live: u16,
    lost: bool,
    seal_epoch: u64,
}

impl AnchorSet {
    /// No anchor lives: every hook returns at once.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// How many anchors live.
    #[must_use]
    #[inline]
    pub const fn live_count(&self) -> usize {
        self.live as usize
    }

    /// Returns whether an untracked row motion occurred since [`Self::clear_lost`].
    #[must_use]
    #[inline]
    pub const fn lost(&self) -> bool {
        self.lost
    }

    /// The fence absorbed the loss (it tainted every mark holding an anchor).
    #[inline]
    pub const fn clear_lost(&mut self) {
        self.lost = false;
    }

    /// The seal epoch (see the type doc).
    #[must_use]
    #[inline]
    pub const fn seal_epoch(&self) -> u64 {
        self.seal_epoch
    }

    /// Places an anchor at absolute `row`, or returns `None` when [`MAX_ANCHORS`] live.
    pub fn place(&mut self, row: u64, role: AnchorRole) -> Option<AnchorId> {
        let anchor = Anchor {
            row,
            role,
            live: true,
        };
        let slot = match self.anchors.iter().position(|a| !a.live) {
            Some(free) => {
                self.anchors[free] = anchor;
                free
            }
            None if self.anchors.len() < MAX_ANCHORS => {
                self.anchors.push(anchor);
                self.anchors.len() - 1
            }
            None => return None,
        };
        self.live += 1;
        u16::try_from(slot).ok().map(AnchorId)
    }

    /// Frees `id`, resetting the set when the last live anchor is removed.
    pub fn free(&mut self, id: AnchorId) {
        if let Some(a) = self.anchors.get_mut(usize::from(id.0))
            && a.live
        {
            a.live = false;
            self.live -= 1;
            if self.live == 0 {
                *self = Self::default();
            }
        }
    }

    /// The live anchor in `id`, if any.
    #[must_use]
    #[inline]
    pub fn get(&self, id: AnchorId) -> Option<Anchor> {
        self.anchors
            .get(usize::from(id.0))
            .copied()
            .filter(|a| a.live)
    }

    /// Returns the live anchor's row, or `None` for a freed or unknown slot.
    #[must_use]
    #[inline]
    pub fn row(&self, id: AnchorId) -> Option<u64> {
        self.get(id).map(|a| a.row)
    }

    /// Moves a live anchor to `row`, returning `false` for a freed slot.
    #[inline]
    pub fn set_row(&mut self, id: AnchorId, row: u64) -> bool {
        match self.anchors.get_mut(usize::from(id.0)) {
            Some(a) if a.live => {
                a.row = row;
                true
            }
            _ => false,
        }
    }

    /// Every slot, live or not, in slot order (a checkpoint's projection).
    #[must_use]
    #[inline]
    pub fn slots(&self) -> &[Anchor] {
        &self.anchors
    }

    /// Rebuild a set from a checkpoint's slots, `None` past [`MAX_ANCHORS`].
    #[must_use]
    pub fn from_slots(slots: Vec<Anchor>, lost: bool, seal_epoch: u64) -> Option<Self> {
        if slots.len() > MAX_ANCHORS {
            return None;
        }
        let live = u16::try_from(slots.iter().filter(|a| a.live).count()).ok()?;
        Some(Self {
            anchors: slots,
            live,
            lost: lost && live > 0,
            seal_epoch,
        })
    }

    /// Translates and clamps anchors within screen rows `top..=bottom` at `base` by `delta`.
    /// On an insertion every anchor on the band's top holds its row: the blank rows opened
    /// there join the span that starts there and stay out of the one that ends there. Rows
    /// opened at a stopped mark's start are some other writer's, and its reselect's seal
    /// check cuts them, as it does rows opened anywhere inside its spans.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "BandMove",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "Reserve",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    pub(crate) fn band_move(&mut self, base: u64, top: u16, bottom: u16, delta: i64) {
        if self.live == 0 || delta == 0 || top > bottom || knob(FenceKnob::NoBandTransform) {
            return;
        }
        let lo = base.saturating_add(u64::from(top));
        let hi = base.saturating_add(u64::from(bottom));
        let (top, end) = (i64::from(top), i64::from(bottom) + 1);
        let starts_follow = knob(FenceKnob::StartFollows);
        for a in self.anchors.iter_mut().filter(|a| a.live) {
            if (lo..=hi).contains(&a.row) {
                let holds = a.role == AnchorRole::End || !starts_follow;
                if holds && a.row == lo && delta > 0 {
                    continue;
                }
                let screen = i64::try_from(a.row - base).unwrap_or(end);
                let moved = screen.saturating_add(delta).clamp(top, end);
                a.row = base.saturating_add(moved.unsigned_abs());
            }
        }
    }

    /// Shifts anchors at or past `at` by `inserted` rows for an archival splice.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "Splice",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    pub(crate) fn splice(&mut self, at: u64, inserted: u64) {
        if self.live == 0 || inserted == 0 {
            return;
        }
        let end_as_start = knob(FenceKnob::SpliceEndAsStart);
        for a in self.anchors.iter_mut().filter(|a| a.live) {
            let shifts = match a.role {
                AnchorRole::Start => a.row >= at,
                AnchorRole::End => a.row > at || (end_as_start && a.row == at),
            };
            if shifts {
                a.row = a.row.saturating_add(inserted);
            }
        }
    }

    /// Clamps anchors below `oldest` up to `oldest` after history erasure.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "Ed3",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    pub(crate) fn clamp_below(&mut self, oldest: u64) {
        for a in self.anchors.iter_mut().filter(|a| a.live) {
            a.row = a.row.max(oldest);
        }
    }

    /// Marks the anchor set lost after an untracked row motion.
    #[inline]
    pub(crate) const fn lose(&mut self) {
        if self.live > 0 {
            self.lost = true;
            self.seal_epoch = self.seal_epoch.wrapping_add(1);
        }
    }

    /// Marks the anchor set lost when rows are renumbered outside a lend.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "Renumber",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    #[inline]
    pub(crate) fn lose_renumbered(&mut self) {
        if !knob(FenceKnob::UnlentRenumberKeeps) {
            self.lose();
        }
    }

    /// Advances the seal epoch when rows are re-keyed without moving anchors.
    #[inline]
    pub(crate) const fn note_rekeyed(&mut self) {
        if self.live > 0 {
            self.seal_epoch = self.seal_epoch.wrapping_add(1);
        }
    }

    /// Takes all anchors for a lend, leaving an empty set at the current epoch.
    #[inline]
    pub(crate) fn take_for_lend(&mut self) -> Self {
        let epoch = self.seal_epoch;
        let taken = std::mem::take(self);
        self.seal_epoch = epoch;
        taken
    }

    /// Mutable access to every live anchor, for the lend's return.
    #[inline]
    pub(crate) fn live_mut(&mut self) -> impl Iterator<Item = (usize, &mut Anchor)> {
        self.anchors.iter_mut().enumerate().filter(|(_, a)| a.live)
    }

    /// Marks the set lost when a lent rewrap could not be verified.
    #[inline]
    pub(crate) const fn lose_lent(&mut self) {
        self.lose();
    }

    /// Bump the seal epoch on the lend's return.
    #[inline]
    pub(crate) const fn rekey_lent(&mut self) {
        self.note_rekeyed();
    }
}

/// Holder-fence negative-control mutation knobs for tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum FenceKnob {
    /// Band moves leave anchors where they were.
    NoBandTransform = 1 << 0,
    /// A splice shifts an End at `at` as it does a Start.
    SpliceEndAsStart = 1 << 1,
    /// A width reflow lends nothing: anchors keep their pre-rewrap rows.
    NoReflowLend = 1 << 2,
    /// A renumbering outside a lend leaves anchors unmarked.
    UnlentRenumberKeeps = 1 << 3,
    /// The alt-screen swap marks anchors lost (a liveness control).
    SwapLoses = 1 << 4,
    /// A reselect keeps every span without reading its seal.
    NoSealCheck = 1 << 5,
    /// A new mark is never tainted by what lies below the cursor.
    NoLayTaint = 1 << 6,
    /// The lay taint scan starts below the cursor row.
    LayTaintSkipsCursorRow = 1 << 7,
    /// An exec into a drawer inherits the outgoing mark's spans.
    ExecInherits = 1 << 8,
    /// Evicts suspended marks by count regardless of liveness.
    CountEviction = 1 << 9,
    /// Preserves `alt_fresh` on stop so reselect admits external alternate-screen writes.
    StopKeepsAltFresh = 1 << 10,
    /// A Start on an insertion's top row follows its row down (a liveness control: the rows a
    /// drawer opens at its own start, and the card it draws in them, are refused).
    StartFollows = 1 << 11,
    /// Rows the current drawer erases above its open span's start stay out of it (a liveness
    /// control: the card it redraws there, as Claude Code does from home after a resize, is
    /// refused).
    ErasedRowsStayOut = 1 << 12,
}

#[cfg(any(test, debug_assertions))]
mod knob_state {
    use std::cell::Cell;

    use super::FenceKnob;

    thread_local! {
        static ARMED: Cell<u16> = const { Cell::new(0) };
    }

    /// Whether `k` is switched off on this thread.
    #[must_use]
    #[inline]
    pub fn knob(k: FenceKnob) -> bool {
        ARMED.with(|a| a.get() & k as u16 != 0)
    }

    /// Runs `f` with `k` armed on this thread and restores the previous state on exit.
    pub fn with_knob<T>(k: FenceKnob, f: impl FnOnce() -> T) -> T {
        struct Restore(u16);
        impl Drop for Restore {
            fn drop(&mut self) {
                ARMED.with(|a| a.set(self.0));
            }
        }
        let before = ARMED.with(|a| {
            let before = a.get();
            a.set(before | k as u16);
            before
        });
        let _restore = Restore(before);
        f()
    }
}

#[cfg(not(any(test, debug_assertions)))]
mod knob_state {
    use super::FenceKnob;

    /// Release: no knob is ever switched off.
    #[must_use]
    #[inline]
    pub const fn knob(_k: FenceKnob) -> bool {
        false
    }
}

pub use knob_state::knob;
#[cfg(any(test, debug_assertions))]
pub use knob_state::with_knob;

#[cfg(test)]
mod tests {
    use super::*;

    fn set_with(rows: &[(u64, AnchorRole)]) -> (AnchorSet, Vec<AnchorId>) {
        let mut set = AnchorSet::default();
        let ids = rows
            .iter()
            .map(|&(row, role)| set.place(row, role).unwrap())
            .collect();
        (set, ids)
    }

    #[test]
    fn an_empty_set_allocates_nothing_and_every_hook_is_a_no_op() {
        let mut set = AnchorSet::default();
        set.band_move(0, 0, 10, 3);
        set.splice(5, 2);
        set.lose();
        set.note_rekeyed();
        assert!(set.is_empty());
        assert!(!set.lost(), "a set with no anchor has nothing to lose");
        assert_eq!(set.seal_epoch(), 0);
        assert_eq!(set.slots().len(), 0);
    }

    #[test]
    fn a_band_move_carries_anchors_inside_the_band_and_clamps_at_its_edges() {
        let (mut set, ids) = set_with(&[
            (100, AnchorRole::Start), // above the band
            (103, AnchorRole::Start), // in the band, survives
            (101, AnchorRole::End),   // in the band, pushed off its top
            (108, AnchorRole::End),   // in the band, pushed off its bottom by IL
            (111, AnchorRole::Start), // below the band
        ]);
        set.band_move(100, 1, 8, -2);
        assert_eq!(set.row(ids[0]), Some(100));
        assert_eq!(set.row(ids[1]), Some(101));
        assert_eq!(set.row(ids[2]), Some(101), "clamped to the band's top");
        assert_eq!(set.row(ids[3]), Some(106));
        assert_eq!(set.row(ids[4]), Some(111));
        set.band_move(100, 1, 8, 3);
        assert_eq!(set.row(ids[3]), Some(109), "clamped to bottom + 1");
        assert_eq!(
            set.row(ids[1]),
            Some(101),
            "a start on the insertion's top holds"
        );
    }

    /// Muse Code's launch: three rows opened at the top of a region that
    /// starts on a span's start are that span's, every anchor on the top
    /// holding its row; a start below the top follows its row, as does one
    /// the band starts above.
    #[test]
    fn an_insertion_at_a_start_holds_it_and_moves_the_starts_below() {
        let (mut set, ids) = set_with(&[
            (102, AnchorRole::Start), // the open span's
            (102, AnchorRole::Start), // a closed span's
            (102, AnchorRole::End),
            (105, AnchorRole::Start), // in the band, below its top
        ]);
        set.band_move(100, 2, 8, 3);
        assert_eq!(set.row(ids[0]), Some(102), "a start holds");
        assert_eq!(set.row(ids[1]), Some(102), "every start on the top holds");
        assert_eq!(set.row(ids[2]), Some(102), "an end holds");
        assert_eq!(set.row(ids[3]), Some(108));
        set.band_move(100, 1, 8, 2);
        assert_eq!(
            set.row(ids[0]),
            Some(104),
            "rows opened above a start are not its"
        );
        set.band_move(100, 4, 8, -1);
        assert_eq!(
            set.row(ids[0]),
            Some(104),
            "a deletion clamps it to the band's top like any start"
        );
    }

    #[test]
    fn the_start_knob_moves_a_start_on_the_top() {
        let (mut set, ids) = set_with(&[(102, AnchorRole::Start), (102, AnchorRole::End)]);
        with_knob(FenceKnob::StartFollows, || set.band_move(100, 2, 8, 3));
        assert_eq!(set.row(ids[0]), Some(105));
        assert_eq!(set.row(ids[1]), Some(102), "an end holds under the knob");
        assert!(!knob(FenceKnob::StartFollows));
    }

    #[test]
    fn a_splice_whose_end_equals_at_stays_put() {
        let (mut set, ids) = set_with(&[
            (10, AnchorRole::Start),
            (20, AnchorRole::End),
            (20, AnchorRole::Start),
            (25, AnchorRole::End),
            (19, AnchorRole::End),
        ]);
        set.splice(20, 3);
        assert_eq!(set.row(ids[0]), Some(10));
        assert_eq!(
            set.row(ids[1]),
            Some(20),
            "an End at the insertion point keeps its end"
        );
        assert_eq!(set.row(ids[2]), Some(23), "a Start at it shifts");
        assert_eq!(set.row(ids[3]), Some(28));
        assert_eq!(set.row(ids[4]), Some(19));
    }

    #[test]
    fn the_splice_end_knob_moves_an_end_at_the_insertion_point() {
        let (mut set, ids) = set_with(&[(20, AnchorRole::End)]);
        with_knob(FenceKnob::SpliceEndAsStart, || set.splice(20, 3));
        assert_eq!(set.row(ids[0]), Some(23));
        assert!(!knob(FenceKnob::SpliceEndAsStart));
    }

    #[test]
    fn ed3_clamps_anchors_below_the_oldest_row() {
        let (mut set, ids) = set_with(&[(3, AnchorRole::Start), (8, AnchorRole::End)]);
        set.clamp_below(10);
        assert_eq!(set.row(ids[0]), Some(10));
        assert_eq!(set.row(ids[1]), Some(10), "a span wholly erased is empty");
    }

    #[test]
    fn slots_are_reused_and_bounded() {
        let mut set = AnchorSet::default();
        let ids: Vec<_> = (0..MAX_ANCHORS as u64)
            .map(|r| set.place(r, AnchorRole::Start).unwrap())
            .collect();
        assert!(
            set.place(99, AnchorRole::End).is_none(),
            "the cap fails closed"
        );
        set.free(ids[7]);
        assert_eq!(set.place(99, AnchorRole::End), Some(ids[7]));
        assert_eq!(set.live_count(), MAX_ANCHORS);
    }

    #[test]
    fn a_loss_marks_lost_and_moves_the_seal_epoch() {
        let (mut set, _) = set_with(&[(1, AnchorRole::Start)]);
        set.lose();
        assert!(set.lost());
        assert_eq!(set.seal_epoch(), 1);
        set.clear_lost();
        with_knob(FenceKnob::UnlentRenumberKeeps, || set.lose_renumbered());
        assert!(!set.lost(), "the knob keeps a renumbered set");
        set.lose_renumbered();
        assert!(set.lost());
    }
}
