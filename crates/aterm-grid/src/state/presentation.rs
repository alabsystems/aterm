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

/// How many DISJOINT damage bands one parser batch keeps before they degrade to
/// their hull.
///
/// FOUR, and the number is a coverage claim rather than a taste: the shapes that
/// actually produce disjoint bands on the main screen are an inline TUI repainting
/// its title row and its composer box (two), and a build tool rewriting a spinner
/// line and a summary line ten rows above it (two). A batch with five or more
/// genuinely non-adjacent regions is a full-screen repaint, where the hull IS the
/// honest answer. Contiguous and adjacent rows coalesce (see [`merge_bands`]), so a
/// 40-row repaint is ONE band, not forty — the set only grows on real gaps.
pub const MAX_SELECTION_DAMAGE_BANDS: usize = 8;

/// SELECTION CUSTODY: where a print is about to land, captured before it runs.
///
/// `overwrites` is the whole reason ordinary output can be marked at all without
/// clearing a selection on every frame of live output: text APPENDED past a row's
/// existing content replaces nothing a selection could be sitting on, and a
/// history-splice's fill of the blank row it just created is exactly that shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputOrigin {
    /// Absolute row the cursor sat on before the print.
    pub abs_row: u64,
    /// The cursor sat ON existing content, so the print replaces cells.
    pub overwrites: bool,
}

/// A bounded, ascending set of pairwise-disjoint, non-adjacent absolute-row bands.
///
/// Fixed-arity and `Copy` on purpose: this lives in the grid's per-batch state and
/// is unioned from grid write paths, so it must never allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandSet {
    bands: [(u64, u64); MAX_SELECTION_DAMAGE_BANDS],
    len: u8,
}

impl BandSet {
    /// The bands, ascending, pairwise disjoint and non-adjacent.
    #[must_use]
    #[inline]
    pub fn as_slice(&self) -> &[(u64, u64)] {
        &self.bands[..usize::from(self.len)]
    }
}

/// Sort `bands` ascending by low edge and merge every overlapping or ADJACENT pair,
/// in place. Returns the surviving count.
///
/// ADJACENT (`hi + 1 == lo`) merges as well as overlapping, and that is what keeps a
/// row-by-row repaint from eating one set slot per row: rows 3, 4, 5 recorded
/// separately collapse to 3..=5, exactly the band a single 3..=5 record would give.
fn merge_bands(bands: &mut [(u64, u64)]) -> usize {
    // Insertion sort: `bands` is at most `2 * MAX_SELECTION_DAMAGE_BANDS` long, so a
    // quadratic sort with no call overhead is the cheap choice at this size.
    for i in 1..bands.len() {
        let mut j = i;
        while j > 0 && bands[j - 1].0 > bands[j].0 {
            bands.swap(j - 1, j);
            j -= 1;
        }
    }
    let mut out = 0;
    for i in 0..bands.len() {
        let band = bands[i];
        if out > 0 && band.0 <= bands[out - 1].1.saturating_add(1) {
            bands[out - 1].1 = bands[out - 1].1.max(band.1);
        } else {
            bands[out] = band;
            out += 1;
        }
    }
    out
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
/// parser batch compose with no ordering reasoning — that is what makes the
/// composition sound. The lattice is `None ⊑ Band ⊑ Bands ⊑ All`, and `union` is
/// monotone in COVERAGE: no union ever un-covers a row that was already covered.
/// (`damage_selection_output`'s one-compare guard depends on exactly that — a
/// row recorded once stays covered for the rest of the batch.)
///
/// RESIDUAL IMPRECISION, now BOUNDED rather than unconditional: up to
/// [`MAX_SELECTION_DAMAGE_BANDS`] disjoint bands are kept exactly; a fifth
/// non-adjacent region degrades the whole set to its hull, over-clearing a selection
/// sitting in a gap. It hulled from the FIRST disjoint pair until this fix, and that
/// was reachable: an inline TUI repainting a title row and a composer box in one
/// batch cleared a selection between them that nothing had rewritten. Degrading
/// fails SAFE — over-clear, never a stale highlight over changed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionDamage {
    /// Nothing in this batch replaced content a selection could be sitting on.
    #[default]
    None,
    /// Absolute rows `lo_abs..=hi_abs` were moved or rewritten.
    Band { lo_abs: u64, hi_abs: u64 },
    /// Two or more DISJOINT bands were moved or rewritten and the rows between them
    /// were not. Never holds fewer than two: one band canonicalises to
    /// [`SelectionDamage::Band`], so equality has a single spelling.
    Bands(BandSet),
    /// The whole coordinate space is gone (ED 3, `clear_scrollback`, RIS, a Kitty
    /// unscroll that renumbers history wholesale). No band can describe it.
    All,
}

impl SelectionDamage {
    /// Copy this value's bands into `out`, returning how many were written. `None`
    /// writes none; `All` is absorbing and is handled by the caller before this runs.
    fn spill(self, out: &mut [(u64, u64)]) -> usize {
        match self {
            Self::None | Self::All => 0,
            Self::Band { lo_abs, hi_abs } => {
                out[0] = (lo_abs, hi_abs);
                1
            }
            Self::Bands(set) => {
                let slice = set.as_slice();
                out[..slice.len()].copy_from_slice(slice);
                slice.len()
            }
        }
    }

    /// Canonical value for an already-merged, ascending band list.
    fn from_merged(bands: &[(u64, u64)]) -> Self {
        match bands {
            [] => Self::None,
            [(lo_abs, hi_abs)] => Self::Band {
                lo_abs: *lo_abs,
                hi_abs: *hi_abs,
            },
            _ if bands.len() <= MAX_SELECTION_DAMAGE_BANDS => {
                let mut set = BandSet {
                    bands: [(0, 0); MAX_SELECTION_DAMAGE_BANDS],
                    // Guarded by the arm above: at most MAX_SELECTION_DAMAGE_BANDS.
                    len: u8::try_from(bands.len()).unwrap_or(0),
                };
                set.bands[..bands.len()].copy_from_slice(bands);
                Self::Bands(set)
            }
            // Past the arity bound, ABSORB THE CLOSEST PAIR rather than collapsing to
            // the hull.
            //
            // Hulling here degraded on TRANSIENT arity, not final arity, which made
            // the result order-dependent and needlessly coarse: a box border drawn as
            // two overlapping pieces and then filled — the commonest TUI shape —
            // momentarily exceeds the bound and permanently loses every gap, even
            // when the FINAL cover fits. Folding random band sequences, one in eighty
            // hulled despite fitting.
            //
            // Repeatedly merging the adjacent pair with the SMALLEST GAP keeps the
            // arity bound while giving up the least possible precision, and depends
            // only on the sorted set — so the same batch yields the same answer
            // whatever order its ops recorded in. `bands` is ascending and disjoint,
            // so the closest pair is always adjacent. Still fails safe: merging only
            // ever widens, never leaves a stale highlight.
            _ => {
                let mut buf = [(0u64, 0u64); MAX_SELECTION_DAMAGE_BANDS * 2];
                let mut len = bands.len().min(buf.len());
                buf[..len].copy_from_slice(&bands[..len]);
                while len > MAX_SELECTION_DAMAGE_BANDS {
                    let mut best = 0usize;
                    let mut best_gap = u64::MAX;
                    for i in 0..len - 1 {
                        let gap = buf[i + 1].0.saturating_sub(buf[i].1);
                        if gap < best_gap {
                            best_gap = gap;
                            best = i;
                        }
                    }
                    buf[best].1 = buf[best].1.max(buf[best + 1].1);
                    buf.copy_within(best + 2..len, best + 1);
                    len -= 1;
                }
                let mut set = BandSet {
                    bands: [(0, 0); MAX_SELECTION_DAMAGE_BANDS],
                    // The loop above drove `len` down to the bound.
                    len: u8::try_from(len).unwrap_or(0),
                };
                set.bands[..len].copy_from_slice(&buf[..len]);
                Self::Bands(set)
            }
        }
    }

    /// Join on the lattice: `All` absorbs, `None` is the unit, and bands merge into a
    /// disjoint set that degrades to its hull past [`MAX_SELECTION_DAMAGE_BANDS`].
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        match (self, other) {
            (Self::All, _) | (_, Self::All) => Self::All,
            (Self::None, x) | (x, Self::None) => x,
            (a, b) => {
                let mut buf = [(0u64, 0u64); MAX_SELECTION_DAMAGE_BANDS * 2];
                let head = a.spill(&mut buf);
                let total = head + b.spill(&mut buf[head..]);
                let merged = merge_bands(&mut buf[..total]);
                Self::from_merged(&buf[..merged])
            }
        }
    }

    /// Must a selection be cleared, given `overlaps`, which answers "does the
    /// selection overlap absolute rows `lo_abs..=hi_abs`?" for ONE band.
    ///
    /// The entry point consumers should use. A caller matching `Band` by hand cannot
    /// express the gap between two disjoint bands, and would silently clear across
    /// it — which is precisely the bug this variant exists to fix.
    #[must_use]
    pub fn clears_selection(self, mut overlaps: impl FnMut(u64, u64) -> bool) -> bool {
        match self {
            Self::None => false,
            Self::All => true,
            Self::Band { lo_abs, hi_abs } => overlaps(lo_abs, hi_abs),
            Self::Bands(set) => set
                .as_slice()
                .iter()
                .any(|&(lo_abs, hi_abs)| overlaps(lo_abs, hi_abs)),
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
    /// Absolute row most recently recorded through `damage_selection_output`,
    /// the ORDINARY-OUTPUT path — the one recording site that runs per printed run
    /// rather than per control sequence.
    ///
    /// It exists only to skip the union while output stays on one row, which is the
    /// overwhelmingly common shape (a `\r`-redrawn progress bar, a prompt, any line
    /// of text). Sound because `SelectionDamage::union` is monotone in coverage: a
    /// row already recorded this batch cannot become un-recorded, so re-recording it
    /// is a no-op and skipping it changes nothing. Cleared with the damage itself in
    /// `take_selection_damage`, so it can never speak for a previous batch.
    pub last_output_damage_abs: Option<u64>,
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
    /// D-2 PER-ROW CONTENT REVISION: `row_rev[r]` is the value of
    /// [`row_rev_clock`](Self::row_rev_clock) at the most recent fold that saw
    /// visible row `r` damaged. One `u64` per VISIBLE row (never per scrollback
    /// row) — the damage tracker's own coordinate space.
    ///
    /// The contract consumers rely on, and the ONLY one:
    ///
    /// > for two folds F1 before F2, `row_rev[r]` differs between them IF row
    /// > `r`'s content changed between F1 and F2.
    ///
    /// The converse does NOT hold and must never be assumed: an unchanged row
    /// may still be re-stamped (a redundant write, a `Damage::Full` session,
    /// a foreign consumer's take). Over-reporting costs a repaint;
    /// under-reporting is a stale frame, so every doubt resolves toward a
    /// fresh stamp.
    ///
    /// This is the row-identity-STABLE half of the damage fact only. Anything
    /// that MOVES content between row indices without marking every moved row
    /// (`mark_scroll_damage` marks only the exposed strip) is invisible here —
    /// which is why consumers must additionally pin `base_y`,
    /// `absolute_row_revision`, the alt bit and a zero `display_offset` before
    /// comparing two snapshots' stamps. See
    /// `aterm_render::compute_dirty_rows`.
    pub row_rev: Vec<u64>,
    /// Monotone clock the fold stamps [`row_rev`](Self::row_rev) with. Advanced
    /// once per fold that observes NEW damage, so two folds with nothing between
    /// them leave every row's revision untouched (an idle frame stays an exact
    /// gate hit rather than degrading into a whole-screen repaint).
    pub row_rev_clock: u64,
    /// The tracker mark-clock ([`crate::damage::DamageTracker::mark_seq`]) as of
    /// the last fold. The fold is a no-op while this is unchanged, which is what
    /// makes back-to-back folds (the extract's and the following
    /// `take_damage`'s) idempotent instead of double-stamping every damaged row.
    pub row_rev_folded_seq: u64,
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

/// Advance the D-2 row-revision clock, SKIPPING zero.
///
/// Zero is the "no stamp / do not trust" sentinel a snapshot carries for a row
/// no engine fold has ever vouched for, so the clock must never mint it — a
/// wrapped clock handing out 0 would turn a real revision into "unknown",
/// which the consumer answers with the brute-force compare (safe), but a
/// SECOND wrap could then hand 0 to two snapshots and make them compare equal.
#[inline]
const fn next_row_rev_clock(clock: u64) -> u64 {
    match clock.wrapping_add(1) {
        0 => 1,
        next => next,
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
            last_output_damage_abs: None,
            last_resize_row_shift: 0,
            coordinates_invalidated: false,
            pending_absolute_row_update: None,
            pending_selection_row_update: None,
            row_rev: Vec::new(),
            row_rev_clock: 0,
            row_rev_folded_seq: 0,
        }
    }

    /// D-2: fold the CURRENT damage session into the per-row revisions.
    ///
    /// Idempotent while nothing new has been marked, so the two folds a frame
    /// performs — the extract's and the following `take_damage`'s — stamp a
    /// damaged row exactly ONCE between them. Both folds are required:
    ///
    /// * the EXTRACT's fold publishes this session's damage to the snapshot
    ///   being filled, and
    /// * the RESET's fold captures damage that a FOREIGN consumer is about to
    ///   discard (`take_damage` clears the bits for everyone), which no later
    ///   fold could recover.
    ///
    /// `Damage::Full` re-stamps unconditionally: it keeps no mark clock (see
    /// [`Damage::mark_seq`]), so "nothing new since the last fold" is not a
    /// question it can answer, and answering it wrongly is a stale frame. A
    /// full-damage frame repaints everything anyway, so the cost of the
    /// unconditional re-stamp lands only on frames that were never cheap.
    pub(crate) fn fold_row_revisions(&mut self, visible_rows: u16) {
        let rows = usize::from(visible_rows);
        // A row-count change is a full re-resolve for every consumer (resize
        // marks full damage), so rebuild the lane rather than carrying stamps
        // across a geometry the old indices no longer describe.
        if self.row_rev.len() != rows {
            self.row_rev.clear();
            self.row_rev.resize(rows, 0);
            self.row_rev_folded_seq = 0;
        }
        match self.damage.mark_seq() {
            // Partial: skip when no mark has been made since the last fold.
            Some(seq) => {
                if seq == self.row_rev_folded_seq {
                    return;
                }
                self.row_rev_folded_seq = seq;
                self.row_rev_clock = next_row_rev_clock(self.row_rev_clock);
                let clock = self.row_rev_clock;
                for row in self.damage.damaged_rows(visible_rows) {
                    if let Some(slot) = self.row_rev.get_mut(usize::from(row)) {
                        *slot = clock;
                    }
                }
            }
            // Full: no clock to consult, so re-stamp every row.
            None => {
                self.row_rev_clock = next_row_rev_clock(self.row_rev_clock);
                self.row_rev.fill(self.row_rev_clock);
            }
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
        // The output guard is an assertion about THIS accumulator's contents, so it
        // must die with it — otherwise the first print of the next batch, landing on
        // the same row, would record nothing.
        self.last_output_damage_abs = None;
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
        // D-2: capture this session's damage BEFORE discarding it. `reset` is
        // the one choke point every consumer funnels through, so a foreign
        // consumer that takes the damage between two of OUR extracts cannot
        // erase a row change from the revision lane.
        self.fold_row_revisions(visible_rows);
        let was_full = self.damage.is_full();
        self.damage.reset(visible_rows);
        if was_full {
            // `reset` from `Full` installs a FRESH tracker whose mark clock
            // restarts at 0. Rewinding the fold's watermark in lockstep keeps
            // "no mark since the last fold" honest; leaving it high would make
            // the next real mark compare equal and skip the fold.
            self.row_rev_folded_seq = 0;
        }
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

#[cfg(test)]
mod tests {
    use super::{MAX_SELECTION_DAMAGE_BANDS, SelectionDamage};

    const fn band(lo_abs: u64, hi_abs: u64) -> SelectionDamage {
        SelectionDamage::Band { lo_abs, hi_abs }
    }

    /// Collect the bands a value names, so a test can state the whole answer. Read
    /// through `clears_selection`, the consumer-facing spelling: a band the predicate
    /// is never asked about is a band that cannot clear anything.
    fn bands_of(damage: SelectionDamage) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let cleared = damage.clears_selection(|lo_abs, hi_abs| {
            out.push((lo_abs, hi_abs));
            false
        });
        assert!(!cleared, "a predicate that never overlaps cannot clear");
        out
    }

    /// The gap between two disjoint bands is NOT damage. This is the whole point of
    /// the set: the hull answer destroyed selections nothing had rewritten.
    #[test]
    fn disjoint_bands_stay_disjoint() {
        let damage = band(10, 10).union(band(30, 30));
        assert_eq!(bands_of(damage), vec![(10, 10), (30, 30)]);
        assert!(!damage.clears_selection(|lo_abs, hi_abs| lo_abs <= 20 && 20 <= hi_abs));
        assert!(damage.clears_selection(|lo_abs, hi_abs| lo_abs <= 30 && 30 <= hi_abs));
    }

    /// Adjacent and overlapping bands coalesce, which is what keeps a row-by-row
    /// repaint from consuming a set slot per row.
    #[test]
    fn adjacent_and_overlapping_bands_coalesce() {
        let rows = band(3, 3).union(band(4, 4)).union(band(5, 5));
        assert_eq!(rows, band(3, 5));
        assert_eq!(band(3, 7).union(band(5, 9)), band(3, 9));
    }

    /// Order must not matter: bands are recorded at arbitrary points in a batch, and
    /// the composition's soundness rests on that.
    #[test]
    fn union_is_order_independent() {
        let forward = band(50, 50).union(band(10, 10)).union(band(30, 30));
        let backward = band(30, 30).union(band(10, 10)).union(band(50, 50));
        assert_eq!(forward, backward);
        assert_eq!(bands_of(forward), vec![(10, 10), (30, 30), (50, 50)]);
    }

    /// Order independence must hold ACROSS THE ARITY BOUND, which is where the first
    /// implementation failed: it collapsed to the hull on TRANSIENT arity, so a
    /// sequence that momentarily exceeded the bound lost every gap permanently even
    /// when its final cover fit. A box border drawn as two overlapping pieces and
    /// then filled — the commonest TUI shape — does exactly that.
    ///
    /// The audit's counterexample, folded both ways. Under the hull degrade the
    /// forward fold gave `Band { 0, 42 }` — clearing rows 8-20 and 25-31 that nothing
    /// rewrote — while the reverse fold gave the exact four-band answer.
    #[test]
    fn union_is_order_independent_past_the_arity_bound() {
        let seq = [
            (21u64, 24u64),
            (0, 2),
            (4, 7),
            (39, 42),
            (5, 5),
            (32, 36),
            (35, 39),
        ];
        let fold = |order: &mut dyn Iterator<Item = &(u64, u64)>| {
            order.fold(SelectionDamage::None, |acc, &(lo, hi)| {
                acc.union(band(lo, hi))
            })
        };
        let forward = fold(&mut seq.iter());
        let backward = fold(&mut seq.iter().rev());
        assert_eq!(
            forward, backward,
            "the same batch must damage the same rows whatever order its ops recorded in"
        );
        // Seven inputs merge to four disjoint covers, which is exactly the bound —
        // no precision is given up here at all.
        assert_eq!(bands_of(forward), vec![(0, 2), (4, 7), (21, 24), (32, 42)]);
    }

    /// Past the bound the set degrades by absorbing a pair, never by collapsing to
    /// the hull — and whatever it gives up, it still COVERS every damaged row.
    ///
    /// What is guaranteed here, and what is not, stated exactly:
    ///
    /// * GUARANTEED — the result is sorted, disjoint, at most the bound, and covers
    ///   the union of every recorded band. Coverage is the safety property: the set
    ///   may over-clear, it can never leave a stale highlight over replaced text.
    /// * NOT GUARANTEED — WHICH pair absorbs, once the number of bands recorded in
    ///   one batch exceeds the bound. The accumulator is fixed-capacity, so a batch
    ///   that transiently overflows absorbs a pair before later bands arrive, and a
    ///   different arrival order can absorb a different pair. Order independence
    ///   holds only while the merged count stays within the bound — which is what
    ///   `union_is_order_independent_past_the_arity_bound` pins, and which covers
    ///   the real shapes: a box border drawn in pieces then filled, a title row plus
    ///   a composer box, a status bar plus a viewport repaint. More than eight
    ///   SEPARATED regions in one batch is a full-screen repaint, where a coarse
    ///   answer is the honest one.
    #[test]
    fn overflow_absorbs_a_pair_and_still_covers_every_damaged_row() {
        let ten = [
            (0u64, 1u64),
            (10, 11),
            (20, 21),
            (30, 31),
            (40, 41),
            (50, 51),
            (60, 61),
            (70, 71),
            (80, 81),
            (90, 91),
        ];
        let merged = ten.iter().fold(SelectionDamage::None, |acc, &(lo, hi)| {
            acc.union(band(lo, hi))
        });
        let got = bands_of(merged);
        assert!(
            got.len() <= MAX_SELECTION_DAMAGE_BANDS,
            "stays within the arity bound: {got:?}"
        );
        assert!(
            got.len() > 1,
            "degrades by absorbing, NOT to a single hull: {got:?}"
        );
        for pair in got.windows(2) {
            assert!(pair[0].1 < pair[1].0, "sorted and disjoint: {got:?}");
        }
        for (lo, hi) in ten {
            assert!(
                got.iter().any(|&(l, h)| l <= lo && hi <= h),
                "every recorded band stays covered — over-clear is safe, a stale \
                 highlight is not: {lo}..={hi} missing from {got:?}"
            );
        }
    }

    /// At the bound the set is exact; one past it it gives up a GAP, not the whole
    /// set. The distinction is the point of the closest-pair rule: a hull degrade
    /// swallows every gap at once, so one repaint too many cleared a highlight
    /// nowhere near anything that was rewritten.
    ///
    /// What this asserts is deliberately tie-break independent. WHICH pair absorbs
    /// when several gaps are equally small is not a guarantee the type makes, so
    /// asserting it would pin an implementation detail; the guarantees are the
    /// bound, total coverage, and that a row in a LARGE gap survives.
    #[test]
    fn past_the_arity_bound_one_gap_is_given_up_not_all_of_them() {
        let mut damage = SelectionDamage::None;
        for i in 0..MAX_SELECTION_DAMAGE_BANDS {
            let row = u64::try_from(i).unwrap_or(0) * 4;
            damage = damage.union(band(row, row));
        }
        assert_eq!(bands_of(damage).len(), MAX_SELECTION_DAMAGE_BANDS);
        assert!(
            !damage.clears_selection(|lo_abs, hi_abs| lo_abs <= 2 && 2 <= hi_abs),
            "at the bound the set is exact, so row 2 sits in a gap"
        );

        // Row 100 is far past the others, so the widest gap by a wide margin is the
        // one below it. The rule absorbs the SMALLEST gap, so that one never goes —
        // whatever it does among the equally-spaced bands below.
        let overflowed = damage.union(band(100, 100));
        let got = bands_of(overflowed);
        assert!(
            got.len() <= MAX_SELECTION_DAMAGE_BANDS,
            "the bound is what makes this state fixed-size: {got:?}"
        );
        for i in 0..MAX_SELECTION_DAMAGE_BANDS {
            let row = u64::try_from(i).unwrap_or(0) * 4;
            assert!(
                got.iter().any(|&(l, h)| l <= row && row <= h),
                "every recorded row stays covered — over-clear is safe, a stale \
                 highlight is not: {row} missing from {got:?}"
            );
        }
        assert!(
            got.iter().any(|&(l, h)| l <= 100 && 100 <= h),
            "including the one that caused the overflow: {got:?}"
        );
        assert!(
            !overflowed.clears_selection(|lo_abs, hi_abs| lo_abs <= 50 && 50 <= hi_abs),
            "row 50 is in the widest gap and nothing rewrote it; the hull degrade \
             this replaced cleared it, which is the regression: {got:?}"
        );
        assert_ne!(
            overflowed,
            band(0, 100),
            "…which is to say the set must not have collapsed to its hull"
        );
    }

    /// `None` is the unit and `All` absorbs, in both argument positions.
    #[test]
    fn none_is_the_unit_and_all_absorbs() {
        assert_eq!(SelectionDamage::None.union(band(1, 2)), band(1, 2));
        assert_eq!(band(1, 2).union(SelectionDamage::None), band(1, 2));
        assert_eq!(SelectionDamage::All.union(band(1, 2)), SelectionDamage::All);
        assert_eq!(
            band(1, 2).union(band(9, 9)).union(SelectionDamage::All),
            SelectionDamage::All
        );
        assert!(!SelectionDamage::None.clears_selection(|_, _| true));
        assert!(SelectionDamage::All.clears_selection(|_, _| false));
    }
}
