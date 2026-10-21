// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Span anchors: the absolute rows the engine's holder fence keeps
//! (`aterm_core::terminal::holder_fence`), moved by the grid's own row-motion
//! record instead of by hooks in each VT handler.
//!
//! An anchor is one boundary of a span of rows a foreground program wrote: a
//! [`AnchorRole::Start`] is the span's first row, an [`AnchorRole::End`] the
//! row after its last. Each rides the choke point that already records the
//! motion for the band record and the selection:
//!
//! | Grid event | Anchor rule |
//! |---|---|
//! | a band move (IL/DL, a region scroll, RI at a region top) | translate and clamp inside the band; an End at its start excludes inserted rows |
//! | the top-anchored archival scroll's splice at `at` | a Start at or after `at` shifts; an End only past `at` |
//! | a rectangle move, a reset, a rotation, a renumbering | [`AnchorSet::lost`] |
//! | an alt-screen swap | nothing: the outgoing grid's rows did not move |
//! | ED3 / `clear_scrollback` | rows below the oldest retained row clamp up to it |
//! | a width reflow | lent ([`AnchorLend`], `grid::anchor_lend`) |
//!
//! The set is empty, and every hook a branch on `live == 0`, until a drawer
//! runs: zero cost at rest.

/// The most anchors a grid keeps: eight marks of four spans, two ends each.
pub const MAX_ANCHORS: usize = 64;

/// A slot in an [`AnchorSet`]. Stable while the anchor lives; a freed slot is
/// reused by the next [`AnchorSet::place`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnchorId(u16);

impl AnchorId {
    /// The slot index (a checkpoint carries it).
    #[must_use]
    pub const fn index(self) -> u16 {
        self.0
    }

    /// The anchor in slot `index`, as a checkpoint names it.
    #[must_use]
    pub const fn from_index(index: u16) -> Self {
        Self(index)
    }
}

/// Which boundary of a span an anchor is. The two differ only where rows are
/// inserted at the anchor (a band insertion or [`AnchorSet::splice`]) and where a rewrap lands it
/// inside a row (`grid::anchor_lend`): both exclude what they cannot vouch for.
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

/// The grid's span anchors, beside the band record in
/// [`GridPresentationState`](super::GridPresentationState).
///
/// `lost` is the fail-closed bit: some row motion moved rows no anchor rule
/// describes, so every anchor's row may now name other content. The fence
/// reads it as taint on every mark that holds an anchor, and clears it.
///
/// `seal_epoch` counts the events that re-key rows under a fence's seal
/// (a rewrap's lend, history revealed back onto the screen, a loss): a seal
/// taken at one epoch speaks only for rows read at the same epoch.
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
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// How many anchors live.
    #[must_use]
    pub fn live_count(&self) -> usize {
        usize::from(self.live)
    }

    /// Some motion moved rows no anchor rule describes since the fence last
    /// cleared this ([`Self::clear_lost`]).
    #[must_use]
    #[inline]
    pub fn lost(&self) -> bool {
        self.lost
    }

    /// The fence absorbed the loss (it tainted every mark holding an anchor).
    pub fn clear_lost(&mut self) {
        self.lost = false;
    }

    /// The seal epoch (see the type doc).
    #[must_use]
    #[inline]
    pub fn seal_epoch(&self) -> u64 {
        self.seal_epoch
    }

    /// Place an anchor at absolute `row`. `None` when [`MAX_ANCHORS`] live:
    /// the caller fails closed (no span it cannot bound).
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

    /// Free `id` (a no-op for a slot already free). The last one going
    /// leaves the empty set: no anchor is left to be wrong (the loss goes),
    /// no seal names a row (the epoch restarts), and the slots are released,
    /// so a session whose drawers have all gone holds no allocation and
    /// checkpoints nothing.
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
    pub fn get(&self, id: AnchorId) -> Option<Anchor> {
        self.anchors
            .get(usize::from(id.0))
            .copied()
            .filter(|a| a.live)
    }

    /// The live anchor's row (`None` for a freed or unknown slot, which a
    /// reader treats as lost).
    #[must_use]
    pub fn row(&self, id: AnchorId) -> Option<u64> {
        self.get(id).map(|a| a.row)
    }

    /// Move a live anchor to `row` (the fence re-anchoring a span on a full
    /// clear). `false` for a freed slot.
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
    pub fn slots(&self) -> &[Anchor] {
        &self.anchors
    }

    /// Rebuild a set from a checkpoint's slots. `None` past [`MAX_ANCHORS`].
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

    /// Rows `top..=bottom` of the screen whose top row is absolute `base`
    /// moved by `delta` (a [`RowBandMove`](super::RowBandMove)): an anchor
    /// inside the band moves with its row, clamped to the band and the row
    /// after it, where vacated rows are blank.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "BandMove",
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
        for a in self.anchors.iter_mut().filter(|a| a.live) {
            if (lo..=hi).contains(&a.row) {
                // An exclusive end at the band start owns none of the rows
                // inserted there; a Start follows the old first row instead.
                if a.role == AnchorRole::End && a.row == lo && delta > 0 {
                    continue;
                }
                // Inside the band: a screen row, so it fits `i64`.
                let screen = i64::try_from(a.row - base).unwrap_or(end);
                let moved = screen.saturating_add(delta).clamp(top, end);
                a.row = base.saturating_add(moved.unsigned_abs());
            }
        }
    }

    /// `inserted` rows were inserted at absolute `at` (the archival scroll's
    /// [`AbsoluteRowUpdate::Splice`](super::AbsoluteRowUpdate::Splice)): a
    /// Start at or after `at` shifts, an End only past it, the rule
    /// `OutputBlock::end_row` keeps, so a span ending at the boundary never
    /// absorbs the inserted rows.
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

    /// History older than `oldest` was erased (ED3): an anchor below it names
    /// no row and clamps up to it, so a span wholly in the erased history is
    /// empty.
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

    /// Rows moved in a way no anchor rule describes (a rectangle copy or
    /// shift, a reset, a rotation of rows below history).
    pub(crate) fn lose(&mut self) {
        if self.live > 0 {
            self.lost = true;
            self.seal_epoch = self.seal_epoch.wrapping_add(1);
        }
    }

    /// Rows were renumbered outside a lend (a Kitty unscroll, a pullback, a
    /// renumbering carry or restore): [`Self::lose`].
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "holder_fence",
            action = "Renumber",
            project = "aterm_core::terminal::holder_fence_conformance::project"
        )
    )]
    pub(crate) fn lose_renumbered(&mut self) {
        if !knob(FenceKnob::UnlentRenumberKeeps) {
            self.lose();
        }
    }

    /// Rows were re-keyed under a seal without moving an anchor: history
    /// revealed back onto the screen, or a lend returned.
    pub(crate) fn note_rekeyed(&mut self) {
        if self.live > 0 {
            self.seal_epoch = self.seal_epoch.wrapping_add(1);
        }
    }

    /// Take every anchor out for a lend, leaving an empty set behind (so the
    /// hooks the lent operation runs touch nothing) that keeps the epoch.
    pub(crate) fn take_for_lend(&mut self) -> Self {
        let epoch = self.seal_epoch;
        let taken = std::mem::take(self);
        self.seal_epoch = epoch;
        taken
    }

    /// Mutable access to every live anchor, for the lend's return.
    pub(crate) fn live_mut(&mut self) -> impl Iterator<Item = (usize, &mut Anchor)> {
        self.anchors.iter_mut().enumerate().filter(|(_, a)| a.live)
    }

    /// Mark the set lost unconditionally on the lend's return (the rewrap
    /// could not be verified).
    pub(crate) fn lose_lent(&mut self) {
        self.lose();
    }

    /// Bump the seal epoch on the lend's return.
    pub(crate) fn rekey_lent(&mut self) {
        self.note_rekeyed();
    }
}

/// One law of the holder fence switched off, for the writer oracle's
/// negative controls (`aterm_core::terminal::holder_fence`). Each is a
/// mutant of the `HolderFence` model (`aterm_models_core`); the release build
/// reads every knob as off.
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
    /// Suspended marks are evicted by count, live or not: the fence keeps one
    /// (a liveness control).
    CountEviction = 1 << 9,
    /// A stop leaves the outgoing mark's `alt_fresh` set, so a reselect
    /// admits an alternate screen others wrote while it was stopped.
    StopKeepsAltFresh = 1 << 10,
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

    /// Run `f` with `k` switched off on this thread, restored afterwards even
    /// on unwind.
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
        // Screen base 100, band rows 1..=8, up by 2 (DL at row 1).
        set.band_move(100, 1, 8, -2);
        assert_eq!(set.row(ids[0]), Some(100));
        assert_eq!(set.row(ids[1]), Some(101));
        assert_eq!(set.row(ids[2]), Some(101), "clamped to the band's top");
        assert_eq!(set.row(ids[3]), Some(106));
        assert_eq!(set.row(ids[4]), Some(111));
        // Down by 3 (IL at row 1): an anchor near the bottom leaves to the row
        // after the band.
        set.band_move(100, 1, 8, 3);
        assert_eq!(set.row(ids[3]), Some(109), "clamped to bottom + 1");
        assert_eq!(set.row(ids[1]), Some(104));
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
        // Restored after the scope.
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
