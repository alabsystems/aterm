// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Presentation-oriented grid state.

use crate::damage::Damage;
use crate::extra::{CellCoord, CellExtra};
use crate::extra_collection::CellExtras;

use super::AnchorSet;

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

/// One translation of a band of screen rows: rows `top..=bottom` (0-based, visible
/// coordinates) moved by `delta`; a row whose new index `r + delta` leaves the band is gone,
/// with nothing landing in its place. Rows outside the band did not move.
///
/// Covers every row-moving VT operation but a whole-screen scroll: a DECSTBM region scroll
/// (up: `delta < 0`, the bottom rows vacated blank; down or reverse index at the region top:
/// `delta > 0`), IL/DL (band `cursor_row..=region.bottom`) and the top-anchored archival
/// scroll (band `0..=bottom`, `delta = -n`, the leaving rows entering history). A host
/// caching screen coordinates applies it as a transform. [`AbsoluteRowUpdate::Splice`]
/// carries only the history insertion point, so the grid records the band itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowBandMove {
    /// First screen row of the band that moved (inclusive).
    pub top: u16,
    /// Last screen row of the band that moved (inclusive).
    pub bottom: u16,
    /// Signed row displacement: positive is down the screen (a reverse index or IL),
    /// negative is up (a region scroll up, DL, or the archival top-anchored scroll).
    pub delta: i16,
}

/// How many row-band moves one parser batch records before the record degrades to
/// [`RowBandMoves::inexact`]. Streamed inline-TUI output records one per batch (reverse
/// indices at a region top compose); a tmux repaint scrolling two panes and a status row
/// records three. More than eight is a full-screen re-layout, which the host handles as an
/// invalidation.
pub const ROW_BAND_MOVES_PER_BATCH: usize = 8;

/// The row-band moves one parser batch recorded, in the order they happened.
///
/// Fixed-arity and `Copy`, like [`BandSet`], because it is written from the
/// scroll paths and must never allocate. `inexact` is the fail-closed bit:
/// once set, the listed moves do not fully describe the batch's row motion (a
/// margined rectangle scroll, a column op, an alt-screen flip, RIS, a ninth
/// band, a displacement past `i16`) and a consumer must treat the batch as a
/// plain invalidation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowBandMoves {
    /// The recorded moves; only `moves[..len]` is meaningful.
    pub moves: [RowBandMove; ROW_BAND_MOVES_PER_BATCH],
    /// Number of live entries in `moves`.
    pub len: u8,
    /// The batch moved rows in a way these entries do not fully describe.
    pub inexact: bool,
}

impl RowBandMoves {
    /// The recorded moves, oldest first.
    #[must_use]
    #[inline]
    pub fn as_slice(&self) -> &[RowBandMove] {
        &self.moves[..usize::from(self.len)]
    }
}

/// How many disjoint damage bands one parser batch keeps before the closest are merged.
///
/// The main-screen shapes that produce disjoint bands are small (an inline TUI repainting
/// its title row and composer box; a spinner and a summary line). Contiguous and adjacent
/// rows coalesce (see [`merge_bands`]), so a 40-row repaint is one band.
pub const MAX_SELECTION_DAMAGE_BANDS: usize = 8;

/// Where a print is about to land, captured before it runs.
///
/// `overwrites` lets ordinary output be marked without clearing a selection on every frame:
/// text appended past a row's content (a history splice filling the blank row it created,
/// too) replaces nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputOrigin {
    /// Absolute row the cursor sat on before the print.
    pub abs_row: u64,
    /// The cursor sat on existing content, so the print replaces cells.
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

/// Sort `bands` ascending by low edge and merge every overlapping or adjacent
/// pair, in place. Returns the surviving count.
///
/// Adjacent (`hi + 1 == lo`) bands merge too, so a row-by-row repaint does not
/// use one slot per row: rows 3, 4, 5 collapse to 3..=5.
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

/// Where this batch damaged content, in absolute rows.
///
/// Damage is a question about overlap: a status bar scrolling rows 18-23 must not clear a
/// highlight in scrollback, while a `\r` + EL progress bar rewriting the row under a
/// highlight must.
///
/// Absolute rows are frame-invariant, so bands recorded at different points in one batch
/// compose in any order. The lattice is `None ⊑ Band ⊑ Bands ⊑ All`, and `union` is
/// monotone in coverage (`damage_selection_output`'s one-compare guard relies on this).
///
/// Up to [`MAX_SELECTION_DAMAGE_BANDS`] disjoint bands are kept exactly; past that the
/// closest pair merges, which may over-clear a selection in that gap but never leaves a
/// stale highlight over changed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionDamage {
    /// Nothing in this batch replaced content a selection could be sitting on.
    #[default]
    None,
    /// Absolute rows `lo_abs..=hi_abs` were moved or rewritten.
    Band { lo_abs: u64, hi_abs: u64 },
    /// Two or more disjoint bands were moved or rewritten and the rows between
    /// them were not. Never fewer than two: one band canonicalises to
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
            // Past the arity bound, absorb the closest pair rather than collapsing to the
            // hull: hulling on transient arity would be order-dependent and lose every gap
            // (a box border drawn as two overlapping pieces, then filled, exceeds the bound
            // momentarily). `bands` is ascending and disjoint, so the closest pair is
            // adjacent; merging only widens, so it still fails safe.
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

    /// Join on the lattice: `All` absorbs, `None` is the unit, and bands merge into a disjoint
    /// set that absorbs its closest pair past [`MAX_SELECTION_DAMAGE_BANDS`].
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
    /// selection overlap absolute rows `lo_abs..=hi_abs`?" for one band.
    ///
    /// The entry point consumers should use: matching `Band` by hand cannot
    /// express the gap between two disjoint bands and would clear across it.
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

/// What the most recent resize did to this grid's rows, one count per arm of
/// `Grid::adjust_row_count`. Every resize overwrites the whole record; drained through
/// `Grid::take_last_resize_shape`.
///
/// The counts say what left or entered the viewport, not what was kept. On a grid with no
/// retention (the alternate screen, `max_scrollback = 0`) demoted and pushed rows are
/// evicted, but the resize undo keeps them (`stashed`) while nothing draws, so a grow back
/// restores them (`restored_top`, counted in `revealed`, and `restored_bottom`) and the flap
/// is an identity. On a width change `reflowed` says whether the column rewrap ran and
/// `pushed`/`revealed`/`appended` are counted after it, so `revealed - demoted` is a
/// displacement only when `reflowed` is false. A resize that moves both axes of a reflowing
/// grid that keeps history runs as its rows-only resize, then the width change
/// (`Grid::split_rows_step`): the record holds the rows step's counts, with the width
/// change's `reflowed` and `cut`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResizeShape {
    /// Trailing blank rows below the cursor dropped off the bottom by a shrink.
    /// No row moved.
    pub trimmed: u16,
    /// Top viewport rows relabelled as the newest history by a shrink. Every surviving row,
    /// and the cursor, moved up by this count.
    pub demoted: u16,
    /// Bottom viewport rows that left the screen with nothing above them moving:
    /// the rows-only bottom-push corner (rotated into history), or the rows a
    /// width-change shrink drained off the bottom.
    pub pushed: u16,
    /// Retained history lines relabelled as the top of the viewport by a grow. Every
    /// pre-resize row, and the cursor, moved down by this count.
    pub revealed: u16,
    /// Fresh blank rows added at the bottom by a grow. No row moved.
    pub appended: u16,
    /// Of `demoted + pushed`, the rows a shrink on a grid with no retention kept in its
    /// resize undo for a grow back with nothing drawn in between (all of them or none).
    pub stashed: u16,
    /// Of `revealed`, the rows that came back from the resize undo: demoted by
    /// an earlier shrink with nothing drawn since, now back where they were.
    pub restored_top: u16,
    /// Rows an earlier shrink pushed off the bottom, put back there by this
    /// grow: from the resize undo, or, on a grid that keeps history, from the
    /// newest history lines the rows hand-back owes (`grid::resize_handback`).
    /// Not counted in `appended` or `revealed`; no row moved.
    pub restored_bottom: u16,
    /// This grow handed back every row the resize undo held and landed on the height its
    /// first shrink started from: the run of resizes since is an identity and the grid's
    /// cursor picture was put back. The host restores what it keeps outside the grid (the
    /// selection).
    pub undone: bool,
    /// A width change ran the column rewrap (primary-screen reflow), which
    /// renumbers rows wholesale.
    pub reflowed: bool,
    /// Columns of drawn cells a narrowing with no rewrap (the alternate
    /// screen) cut off the right edge: one past the rightmost visible cell
    /// at or beyond the new width that was not blank, less the new width. A
    /// grow back brings blank columns there, so until the program draws them
    /// again the screen is missing what it painted. 0 on a widening, on a
    /// rewrap and when every cut cell was blank.
    pub cut: u16,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct GridPresentationState {
    /// Damage tracking.
    pub damage: Damage,
    /// Cell extras (hyperlinks, combining chars, underline colors), kept beside the cells.
    pub extras: CellExtras,
    /// Accumulated content scroll delta since last `take_content_scroll_delta()`.
    /// Used by Terminal to adjust selection coordinates after processing.
    /// Positive = content scrolled up by this many lines.
    /// `i32::MAX` = region scroll (forces selection clear).
    pub content_scroll_delta: i32,
    /// Where this batch damaged content, in absolute rows. Accumulated by
    /// `union` at each grid site, drained once per batch by
    /// `take_selection_damage()`.
    pub selection_damage: SelectionDamage,
    /// Absolute row most recently recorded through `damage_selection_output` (the
    /// ordinary-output path, run per printed run), so the union is skipped while output
    /// stays on one row. Sound because `SelectionDamage::union` is monotone in coverage;
    /// cleared with the damage in `take_selection_damage`.
    pub last_output_damage_abs: Option<u64>,
    /// Rows of downward shift the most recent resize applied to the viewport by revealing
    /// retained history (a rows-grow). One-shot; drained by `Terminal::finalize_resize`.
    ///
    /// `Grid::resize_with_reflow_mode` already shifts the cursor and saved cursor by this;
    /// the selection needs the same compensation or a copy returns the wrong text.
    pub last_resize_row_shift: u16,
    /// What the most recent resize did to this grid's rows, every arm counted
    /// (see [`ResizeShape`]). Overwritten by each resize; drained by
    /// `Terminal`'s resize journal. Separate from `last_resize_row_shift`,
    /// which the selection consumes.
    pub last_resize_shape: ResizeShape,
    /// Whether this batch moved rows so a host's cached grid coordinates cannot be
    /// translated. Independent of the selection lattice ("was content replaced?"): a
    /// region scroll is both, an EL or DECERA only the latter.
    pub coordinates_invalidated: bool,
    /// The row-band moves behind this batch's `coordinates_invalidated` (see
    /// [`RowBandMove`]). Recorded beside the flag, never instead of it, so a host that can
    /// translate bands reads them as an exact explanation of that epoch step. Drained once
    /// per batch by `take_row_band_moves()`.
    pub row_band_moves: RowBandMoves,
    /// The holder fence's span anchors, moved by the same choke points that
    /// write `row_band_moves` and the splice (see [`AnchorSet`]). Empty, and
    /// every hook a single branch, until a drawer runs.
    pub anchors: AnchorSet,
    /// Pending logical-row insertion for durable absolute-row metadata.
    pub pending_absolute_row_update: Option<AbsoluteRowUpdate>,
    /// Independent copy retained until terminal post-processing remaps the
    /// active text selection. Metadata may drain its copy mid-parser to
    /// preserve OSC ordering; selection is adjusted once per complete batch.
    pub pending_selection_row_update: Option<AbsoluteRowUpdate>,
    /// Per-row content revision: `row_rev[r]` is the [`row_rev_clock`](Self::row_rev_clock)
    /// value at the most recent fold that saw visible row `r` damaged. One `u64` per visible
    /// row, in the damage tracker's coordinates.
    ///
    /// Contract: for folds F1 before F2, `row_rev[r]` differs between them if row `r`'s
    /// content changed in between. The converse does not hold (a redundant write, a
    /// `Damage::Full` session or a foreign consumer's take re-stamps an unchanged row):
    /// over-reporting costs a repaint, under-reporting is a stale frame.
    ///
    /// Content moved between rows without marking every moved row (`mark_scroll_damage`
    /// marks only the exposed strip) is invisible here, so consumers also pin `base_y`,
    /// `absolute_row_revision`, the alt bit and a zero `display_offset` before comparing
    /// stamps (`aterm_render::compute_dirty_rows`).
    pub row_rev: Vec<u64>,
    /// Monotone clock the fold stamps [`row_rev`](Self::row_rev) with, advanced once per fold
    /// that observes new damage, so an idle frame leaves every revision untouched.
    pub row_rev_clock: u64,
    /// The tracker mark-clock ([`crate::damage::DamageTracker::mark_seq`]) as of the last
    /// fold. The fold is a no-op while it is unchanged, which keeps back-to-back folds (the
    /// extract's and the following `take_damage`'s) from double-stamping.
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

/// Advance the row-revision clock, skipping zero.
///
/// Zero is the "no stamp, do not trust" sentinel for a row no fold has
/// vouched for, so the clock never mints it; otherwise a wrapped clock could
/// hand 0 to two snapshots and make them compare equal.
#[inline]
const fn next_row_rev_clock(clock: u64) -> u64 {
    match clock.wrapping_add(1) {
        0 => 1,
        next => next,
    }
}

impl GridPresentationState {
    /// Fold the current damage session into the per-row revisions.
    ///
    /// Idempotent while nothing new has been marked, so a frame's two folds stamp a damaged
    /// row once. Both are needed: the extract's fold publishes this session's damage to the
    /// snapshot being filled, and the reset's fold captures damage a foreign consumer is
    /// about to discard (`take_damage` clears the bits for everyone).
    ///
    /// `Damage::Full` re-stamps unconditionally: it keeps no mark clock (see
    /// [`Damage::mark_seq`]) and repaints everything anyway.
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
        // The output guard describes this accumulator, so it is cleared with
        // it; otherwise the next batch's first print on the same row would
        // record nothing.
        self.last_output_damage_abs = None;
        std::mem::take(&mut self.selection_damage)
    }

    /// Drain this batch's host-coordinate invalidation flag.
    #[inline]
    pub(crate) fn take_coordinates_invalidated(&mut self) -> bool {
        std::mem::take(&mut self.coordinates_invalidated)
    }

    /// Record that screen rows `top..=bottom` moved by `delta` rows (see [`RowBandMove`]),
    /// composing with the newest entry when the two moves are provably one translation.
    ///
    /// The move folds into the newest entry `(t1, b1, d1)` iff `b1 == bottom`, `d1` and
    /// `delta` have the same sign, and either `top == t1` or (downward only) `top == t1 + d1`;
    /// the composite is `(t1, b1, d1 + delta)`:
    ///
    /// * Same band twice, either sign: each move shifts the survivors or kills them, and the
    ///   rows the first vacated are blank (five reverse indices on one region give `+5`).
    /// * Downward, band narrowed from above by exactly `d1`: the excluded rows are the ones
    ///   the first move vacated (an inline viewport growing a row per line).
    /// * Upward with `top == t1 + d1` is refused: the wider second band destroys rows
    ///   `top..t1 - 1` the composite would report as untouched.
    ///
    /// Anything else is a separate entry, replayed in order. A full record or a composed
    /// displacement past `i16` marks the batch `inexact`; a zero delta or an inverted band
    /// records nothing.
    pub(crate) fn record_row_band_move(&mut self, top: u16, bottom: u16, delta: i16) {
        if delta == 0 || top > bottom {
            return;
        }
        let record = &mut self.row_band_moves;
        let len = usize::from(record.len);
        if let Some(newest) = len.checked_sub(1).and_then(|i| record.moves.get_mut(i)) {
            let same_bottom = newest.bottom == bottom;
            let same_sign = newest.delta.signum() == delta.signum();
            let same_top = newest.top == top;
            let narrowed_by_first_shift = newest.delta > 0
                && i32::from(top) == i32::from(newest.top) + i32::from(newest.delta);
            if same_bottom && same_sign && (same_top || narrowed_by_first_shift) {
                match newest.delta.checked_add(delta) {
                    Some(composed) => newest.delta = composed,
                    None => record.inexact = true,
                }
                return;
            }
        }
        match record.moves.get_mut(len) {
            Some(slot) => {
                *slot = RowBandMove { top, bottom, delta };
                record.len += 1;
            }
            None => record.inexact = true,
        }
    }

    /// Mark this batch's row motion as not fully described by its band record:
    /// the fail-closed bit every non-band coordinate move sets (see
    /// [`RowBandMoves::inexact`]).
    #[inline]
    pub(crate) fn poison_row_band_moves(&mut self) {
        self.row_band_moves.inexact = true;
    }

    /// Drain this batch's row-band move record.
    #[inline]
    pub(crate) fn take_row_band_moves(&mut self) -> RowBandMoves {
        std::mem::take(&mut self.row_band_moves)
    }

    /// Drain the most recent resize's revealed-history row shift.
    #[inline]
    pub(crate) fn take_last_resize_row_shift(&mut self) -> u16 {
        std::mem::take(&mut self.last_resize_row_shift)
    }

    /// Drain the most recent resize's full row accounting (see [`ResizeShape`]).
    #[inline]
    pub(crate) fn take_last_resize_shape(&mut self) -> ResizeShape {
        std::mem::take(&mut self.last_resize_shape)
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
    pub(crate) fn cell_extra(&self, row: u16, col: u16) -> Option<&CellExtra> {
        self.extras.get(CellCoord::new(row, col))
    }

    #[inline]
    pub(crate) fn clear_damage(&mut self, visible_rows: u16) {
        // Capture this session's damage before discarding it. `reset` is the
        // choke point every consumer goes through, so a foreign consumer taking
        // the damage between two of our extracts cannot hide a row change.
        self.fold_row_revisions(visible_rows);
        let was_full = self.damage.is_full();
        self.damage.reset(visible_rows);
        if was_full {
            // `reset` from `Full` installs a fresh tracker whose mark clock
            // restarts at 0; rewind the fold's watermark in step, or the next
            // real mark would compare equal and skip the fold.
            self.row_rev_folded_seq = 0;
        }
    }

    pub(crate) fn mark_scroll_damage(&mut self, visible_rows: u16, n: usize) {
        let rows = usize::from(visible_rows);
        if n >= rows {
            self.damage.mark_full();
        } else {
            // One range op marks the exposed bottom strip.
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

    /// Collect the bands a value names, read through `clears_selection` (the
    /// consumer-facing spelling), so a test can state the whole answer.
    fn bands_of(damage: SelectionDamage) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let cleared = damage.clears_selection(|lo_abs, hi_abs| {
            out.push((lo_abs, hi_abs));
            false
        });
        assert!(!cleared, "a predicate that never overlaps cannot clear");
        out
    }

    /// The gap between two disjoint bands is not damage.
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

    /// Order independence holds across the arity bound: a sequence that
    /// momentarily exceeds the bound (a box border drawn as two overlapping
    /// pieces and then filled) keeps its gaps when the final cover fits, folded
    /// in either order.
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
        // Seven inputs merge to four disjoint covers, within the bound: nothing is given up.
        assert_eq!(bands_of(forward), vec![(0, 2), (4, 7), (21, 24), (32, 42)]);
    }

    /// Past the bound the set absorbs a pair, never collapsing to the hull, and covers every
    /// damaged row. Which pair absorbs can depend on arrival order once more bands are recorded
    /// than the bound; order independence holds while the merged count fits.
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

    /// At the bound the set is exact; one past it gives up a gap, not the whole set. Which of
    /// several equally small gaps absorbs is unspecified, so this checks the bound, coverage,
    /// and that a row in a large gap survives.
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

        // Row 100 is far past the others, so the widest gap is the one below it;
        // the rule absorbs the smallest gap, so that one never goes.
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
