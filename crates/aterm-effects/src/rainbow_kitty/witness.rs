// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The content witness: the ribbon's one content-aware retirement, and the
//! engine half of a seam the host feeds from its terminal lock.
//!
//! The ribbon records keys as absolute cells `(row, col)` with clocks and
//! knows no glyphs, and an erase invalidates no host coordinate (D2: a prompt
//! redraw must not wipe the ribbon), so a TUI that re-lays its input box would
//! otherwise leave the band where the text was. After each glow tick the host
//! hands the engine the live grid rows the ribbon occupies ([`RowSample`]:
//! lead glyph at its column, `'\0'` at a wide continuation, `' '` for blank).
//! Per cell:
//!
//! * First seen over a non-blank glyph, the cell is armed with a record
//!   (char, width, wide half). Blank → glyph only arms, so a frame-late echo is
//!   never retired (except a glyph landing on an old blank in the batch that
//!   took its run, `Witness::landed_goes_with_its_run`).
//! * Glyph → different glyph retires the cell on the fast melt
//!   (`Ribbon::retire_cells`, [`super::ribbon::RETIRE_MELT_S`]); both halves
//!   of a wide glyph go as one unit.
//! * Glyph → blank releases the cell to the swoosh (`Ribbon::release_cells`),
//!   unless the run's text was replaced or is found in order and spacing on
//!   another sampled row (it moved), in which case the whole run melts.
//! * A released record stays while the cell is resident, so a glyph landing
//!   under it later is replaced text and melts it; it is counted once.
//! * Records are per identity `(row, col, born)`; a re-laid cell has its own.
//! * Never-armed (blank) cells go with their run by the span-plus-tail rule
//!   ([`Witness::released_span_takes`]). A redraw of the same text retires
//!   nothing (D2).
//! * A torn read is not a cleared line: a blanked fragment of a run with a
//!   glyph still standing in place is released, never searched for as moved
//!   text; one glyph is never moved text; and a partial release is lifted as
//!   soon as one of its glyphs is back (`Witness::restored_runs`;
//!   `tests/torn_read_gaps.rs`, `tests/torn_edit_gaps.rs`).
//!
//! The band follows its text. A bottom-anchored composer grows by repainting
//! one row higher without a scroll; the host samples the rows above and below
//! every ribbon row (`Engine::ribbon_rows_for`), and a follow pass
//! ([`Witness::follow_runs`], `Engine::follow_rows`) translates a run whose
//! armed glyphs left their row and stand at their columns one or two rows
//! away (nearest first: `−1, +1, −2, +2`), clocks intact
//! (`Ribbon::translate_run`, [`Witness::translate_cells`]). A shift pass
//! ([`Witness::shift_runs`]) does the same along the row for a mid-line
//! insert (owner, 2026-09-22).
//!
//! A glyph at a destination counts as an arrival only if it was not already a
//! copy there: every record accumulates per offset whether a copy of its glyph
//! stands on that row ([`Seen::observe`]); a copy present at arming, or one
//! that stood [`TWIN_MIN`], is a twin ([`Seen::twin`]) and is neutral
//! ([`Seen::arrivals`]). Unseen rows are no evidence either way, except that a
//! row a band move carried across an offset is not clear until sampled
//! ([`Witness::translate_band`]), and a run beside a row the host was asked for
//! and did not deliver waits one walk ([`Witness::find_deferred_runs`]).
//!
//! Known limits: a copy that stood beside the line less than [`TWIN_MIN`], or
//! was written in the same batch as the erase, is followed as a move; a
//! carried record starts over; a twin is sticky for the record's life; a torn
//! repaint that stands [`TWIN_MIN`] is a copy (synchronized-output apps are
//! presented whole and never tear); a line identical to the one above that
//! moves onto it melts in place. Each limit is pinned by a named test.
//!
//! D2 holds by construction: the host samples after the PTY batch is applied,
//! so an erase and the same text in one batch read as the same glyph. Records
//! are a fixed-capacity sorted list ([`WITNESS_CAP`]), lookups
//! `O(cells · log entries)`, no allocation past warm-up, run only while the
//! theme owns the frame. The witness never touches the echo ledger; a payout
//! may lay over a retired cell ([`super::ribbon::Cell::leaving`]) but never
//! revives it.

use aterm_time::{Duration, Instant};

use crate::cursor_glow::band_row;

use super::ribbon::Cell;

/// Witness records kept: one per resident ribbon cell, at most this many. Past
/// the cap a cell is not witnessed and keeps its own clocks.
pub const WITNESS_CAP: usize = 1024;

/// Rows the bands request per frame for the witness
/// ([`super::Engine::ribbon_rows_for`] names them): the rows the resident
/// ribbon occupies and their possible follow destinations. A wrapped
/// paragraph is three; eight band rows are headroom, not a target. A waiting
/// key's arming rows have [`ARMING_ROWS`] slots of their own after these, and
/// the host captures the caret's row in one more
/// ([`crate::cursor_glow::CURSOR_WITNESS_ROWS`]).
pub const WITNESS_ROWS: usize = 8;

/// The slots a waiting key's arming rows take after the bands'
/// [`WITNESS_ROWS`] ([`super::Engine::ribbon_rows_for`]): the row its cell will
/// be laid on and the rows above and below it, where not already named.
pub const ARMING_ROWS: usize = 3;

/// One row of the live grid as the host sampled it this frame — the row
/// probe's own per-column convention (`Terminal::row_cols_into`).
#[derive(Clone, Copy, Debug)]
pub struct RowSample<'a> {
    /// The grid row.
    pub row: u16,
    /// Per-column chars: the lead glyph at its column, `'\0'` at a wide
    /// continuation, `' '` for a blank. Shorter than the grid means the
    /// missing tail is blank.
    pub cols: &'a [char],
}

/// What one column of a sampled row holds, resolved to its glyph unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unit {
    /// The glyph — the lead char for either half of a wide glyph; `' '` for
    /// a blank.
    pub ch: char,
    /// The glyph occupies two cells.
    pub wide: bool,
    /// This column is the continuation half of a wide glyph (its lead is one
    /// column left).
    pub cont: bool,
}

impl Unit {
    /// A blank column.
    pub const BLANK: Self = Self {
        ch: ' ',
        wide: false,
        cont: false,
    };

    /// True for a column holding no glyph.
    #[must_use]
    pub fn is_blank(self) -> bool {
        self.ch == ' '
    }
}

/// The glyph unit at `col` of a sampled row. A column past the sample's end
/// is blank; a `'\0'` continuation resolves to the lead one column left (a
/// `'\0'` at column 0, or one whose lead is itself blank or `'\0'`, is a
/// degenerate sample and reads as blank).
#[must_use]
pub fn unit_at(cols: &[char], col: u16) -> Unit {
    let i = usize::from(col);
    match cols.get(i) {
        None => Unit::BLANK,
        Some('\0') => {
            let lead = i
                .checked_sub(1)
                .and_then(|j| cols.get(j))
                .copied()
                .unwrap_or(' ');
            if lead == '\0' || lead == ' ' {
                Unit::BLANK
            } else {
                Unit {
                    ch: lead,
                    wide: true,
                    cont: true,
                }
            }
        }
        Some(&c) => Unit {
            ch: c,
            wide: cols.get(i + 1) == Some(&'\0'),
            cont: false,
        },
    }
}

/// One cell of a run as the shift pass reads it: `(col, recorded, gone)`
/// ([`Witness::shift_runs`]).
type ShiftCell = (u16, Option<Unit>, bool);

/// [`Witness::shift_runs`]'s verdict for one run, in column order: the
/// `(lo, hi, dc)` of the block an insert pushed right along the row, or `None`.
fn find_shift(cells: &[ShiftCell], cols: &[char], caret: Option<u16>) -> Option<(u16, u16, i16)> {
    let s = cells.iter().position(|c| c.2)?;
    let (scol, sunit, _) = cells[s];
    let sunit = sunit?;
    let width = u16::try_from(cols.len()).unwrap_or(u16::MAX);
    // AN insert: `s`'s own glyph stands `dc` columns right of it, nearest
    // first — the narrowest insert that explains the row.
    let reach = width.min(scol.saturating_add(SHIFT_MAX_COLS));
    for x in scol.saturating_add(1)..reach {
        if unit_at(cols, x) != sunit {
            continue;
        }
        let Ok(dc) = i16::try_from(x - scol) else {
            break;
        };
        if let Some(v) = shift_block(cells, cols, s, dc, caret) {
            return Some(v);
        }
    }
    None
}

/// Verify one candidate shift for [`find_shift`]: every cell from index `b`
/// on stands at `col + dc` under its own glyph (a never-armed cell under a
/// blank) as one run from `b`, and past the first cell that does not, every
/// recorded glyph has left the row. Then extend the block left over the
/// cells the shift also explains, snap the edit point to the caret when it
/// stands inside that extension, and weigh the evidence. Returns
/// `(lo, hi, dc)` in columns.
fn shift_block(
    cells: &[ShiftCell],
    cols: &[char],
    b: usize,
    dc: i16,
    caret: Option<u16>,
) -> Option<(u16, u16, i16)> {
    let at = |col: u16| {
        col.checked_add_signed(dc)
            .map_or(Unit::BLANK, |c| unit_at(cols, c))
    };
    let mut found = 0usize;
    let mut tail = false;
    let mut hi = None;
    // Whether any found glyph's own column holds other text now. An insert
    // overwrites every column from its edit point on; a block whose columns all
    // went blank was cleared, and a copy further along proves nothing.
    let mut overwritten = false;
    for (i, &(col, rec, _)) in cells.iter().enumerate().skip(b) {
        let there = at(col);
        match rec {
            Some(unit) => {
                if !tail && there == unit {
                    found += 1;
                    hi = Some(i);
                    overwritten |= !unit_at(cols, col).is_blank();
                } else if there.is_blank() {
                    tail = true;
                } else {
                    // A glyph of the tail standing on other text: a
                    // rewrite, not a move.
                    return None;
                }
            }
            None => {
                let on_row = col
                    .checked_add_signed(dc)
                    .is_some_and(|c| usize::from(c) < cols.len());
                if !tail && on_row && there.is_blank() {
                    hi = Some(i);
                } else {
                    tail = true;
                }
            }
        }
    }
    let hi = hi?;
    if found == 0 || !overwritten {
        return None;
    }
    // The edit point: left over every contiguous cell the shift explains.
    let mut lo = b;
    while lo > 0 {
        let (pcol, prec, _) = cells[lo - 1];
        if pcol.checked_add(1) != Some(cells[lo].0) {
            break;
        }
        let there = at(pcol);
        let explained = match prec {
            Some(unit) => there == unit,
            None => there.is_blank(),
        };
        if !explained {
            break;
        }
        lo -= 1;
    }
    // …and a hand typing at the caret inserts there, whatever letters
    // repeat around it.
    let keyed = caret.and_then(|c| (lo..=b).find(|&i| cells[i].0 == c));
    let lo = keyed.unwrap_or(lo);
    // Two found glyphs are a block. One is a block only at the caret, whole,
    // and at the end of the line: a single letter found with the rest of the
    // run gone is a rewrite sharing it, and one with more text past it is a
    // letter the line already held further on.
    if found < 2 {
        let end = cells[hi]
            .0
            .checked_add_signed(dc)
            .map_or(usize::MAX, usize::from);
        let text_past = cols
            .iter()
            .skip(end.saturating_add(1))
            .any(|&c| c != ' ' && c != '\0');
        if keyed.is_none() || tail || text_past {
            return None;
        }
    }
    Some((cells[lo].0, cells[hi].0, dc))
}

/// One witnessed cell, keyed by the cell `(row, col, born)`, not the position:
/// two cells can be resident at one position with different births.
#[derive(Clone, Copy, Debug)]
struct Seen {
    row: u16,
    col: u16,
    /// The birth of the cell, the key's third part: a cell re-laid at the same
    /// column is a different cell, and a stale cell under a fresh one is named
    /// for retirement by this, so the fresh one is never taken with it
    /// (`retire_cells` stamps by identity).
    born: Instant,
    /// The glyph unit the cell was first seen holding.
    unit: Unit,
    /// Set by the walk for every record whose cell is still resident; a
    /// record the walk did not reach is a cell the ribbon dropped.
    live: bool,
    /// Set once the cell's run was released (its glyph went, nothing replaced
    /// it), and kept while the cell is resident: it is unstamped and owns its
    /// column through the retract, so without a record the next walk would arm
    /// it over whatever lands there. A glyph landing under it is replaced text
    /// (melt, with its run); a blank is nothing; the same glyph back is D2.
    /// Never released, searched for or counted again.
    released: bool,
    /// The original identity of a released run, eligible for exact redraw
    /// restoration. A freshly armed/retyped identity never inherits this.
    restorable: bool,
    /// Retirement accounting survives a successful redraw restoration.
    counted: bool,
    /// Nothing says a glyph found there did not arrive: one bit per follow
    /// offset of [`FOLLOW_DRS`], the positive half of an arrival (a glyph found
    /// at that offset counts as arrived only where this is set and
    /// [`Seen::twin`] is not, [`Seen::arrivals`]). Set for every offset when
    /// armed, since the unseen is not evidence against a move. A band move
    /// clears the offsets that reach across the band's edge
    /// ([`Witness::translate_band`]; vim's `yyp`), and a sample showing the row
    /// holding something else sets the bit again.
    clear: u8,
    /// A copy stood beside it: one sticky bit per follow offset, set once the
    /// row that far away was sampled holding this unit at this column while
    /// the record's own glyph stood; at once on the arming frame (`$ cd ..`
    /// typed under `$ cd ..`), otherwise after the copy stood for [`TWIN_MIN`]
    /// ([`Seen::pending`]), counted on a scrolled-in row from the last walk
    /// before the scroll ([`Witness::translate`]). A twin is evidence neither
    /// for nor against a move.
    twin: u8,
    /// A copy seen beside the standing record after arming, not yet for
    /// [`TWIN_MIN`]: a twin in the making, first seen [`Seen::pend_ms`] into
    /// the cell's life. [`Witness::translate`] sets it for a scrolled-in row,
    /// with the clock at [`Witness::last_walk`]. A sampled frame with anything
    /// else there resets it. A short stand is not enough, since a torn repaint
    /// (new row drawn before the old is erased) must still follow.
    pending: u8,
    /// When each [`Seen::pending`] copy was first seen, in ms since the
    /// record's cell was born (saturating).
    pend_ms: [u32; 4],
    /// A verdict deferred once: the glyph changed on a walk where a row beside
    /// its run was asked for but not delivered, and the walk passed no verdict
    /// ([`Witness::walk`]'s `withheld`). A second change while set is judged,
    /// so a host that never delivers cannot keep light under wrong text.
    /// Cleared when the glyph returns or the record is carried.
    deferred: bool,
}

/// How long a copy must stand beside a line to be its twin, seen on every
/// sampled frame between ([`Seen::pending`]). Wall time, not frames: frame
/// spacing ranges from 8 ms while light is live to over 100 ms when idle.
/// The floor is a torn repaint presented on up to three 60 Hz frames (32 ms,
/// `tests/moved_again_without_a_key.rs`), which must follow; the ceiling is a
/// copy that stood three frames before the erase (48 ms,
/// `tests/copy_beside_the_line.rs`), which must not. A scrolled-in copy is
/// clocked from the last walk before the scroll ([`Witness::translate`]).
/// Pinned by
/// `a_copy_that_appears_beside_a_standing_line_is_a_twin_once_it_has_stood_forty_ms`,
/// `tests/moves_are_followed.rs`'s `the_limit_…` laws and
/// `a_line_relocated_by_a_scroll_follows_until_its_new_row_has_stood_twin_min`.
pub const TWIN_MIN: Duration = Duration::from_millis(40);

/// The follow pass's row offsets, nearest first: one row up, one row down,
/// then two. Bit `k` of [`Seen::clear`], [`Seen::twin`] and
/// [`Seen::pending`] is `FOLLOW_DRS[k]`, and the rows one away from a
/// waiting key's row are among the ones the host samples first
/// (`Engine::ribbon_rows_for`).
pub(super) const FOLLOW_DRS: [i16; 4] = [-1, 1, -2, 2];

/// The sampled rows at every follow offset from `row`, in [`FOLLOW_DRS`]
/// order, `None` where not sampled this frame. The walk reuses the answer for
/// every cell on the same row.
fn neighbours_of<'a>(rows: &[RowSample<'a>], row: u16) -> [Option<&'a [char]>; 4] {
    let mut out = [None; 4];
    for (slot, dr) in out.iter_mut().zip(FOLLOW_DRS) {
        *slot = row
            .checked_add_signed(dr)
            .and_then(|r| rows.iter().find(|s| s.row == r))
            .map(|s| s.cols);
    }
    out
}

/// The follow offsets whose evidence survives a row-band move
/// ([`Witness::translate_band`]) for a record carried from `old` to `new`:
/// offset `dr` keeps its bits only where the neighbour moved with the record
/// (the row at `old + dr` is at `new + dr`). Other offsets start over, not
/// clear ([`Seen::clear`]) until a sample shows them.
fn band_keeps(old: u16, new: u16, top: u16, bottom: u16, delta: i16) -> u8 {
    let mut keep = 0u8;
    for (k, dr) in FOLLOW_DRS.into_iter().enumerate() {
        if let (Some(was), Some(now)) = (old.checked_add_signed(dr), new.checked_add_signed(dr))
            && band_row(was, top, bottom, delta) == Some(now)
        {
            keep |= 1 << k;
        }
    }
    keep
}

impl Seen {
    /// One frame's evidence: the record's glyph stands on its row at `now`,
    /// and `nb` are the rows at its follow offsets ([`neighbours_of`]). For
    /// each sampled one, a different unit at the record's column sets
    /// [`Seen::clear`] and resets [`Seen::pending`]; the same unit sets
    /// [`Seen::twin`] at once when arming (`at_arm`), otherwise once it has
    /// stood [`TWIN_MIN`] ([`Seen::pending`], dated from the last pre-scroll
    /// walk on a scrolled-in row, [`Witness::translate`]). An unsampled row
    /// changes nothing; a blank record testifies nothing.
    fn observe(&mut self, nb: &[Option<&[char]>; 4], at_arm: bool, now: Instant) {
        if self.unit.is_blank() {
            return;
        }
        let age =
            u32::try_from(now.saturating_duration_since(self.born).as_millis()).unwrap_or(u32::MAX);
        for (k, cols) in nb.iter().enumerate() {
            let Some(cols) = cols else {
                continue;
            };
            let bit = 1u8 << k;
            if unit_at(cols, self.col) != self.unit {
                self.clear |= bit;
                self.pending &= !bit;
            } else if at_arm {
                self.twin |= bit;
            } else if self.pending & bit == 0 {
                self.pending |= bit;
                self.pend_ms[k] = age;
            } else if u128::from(age.saturating_sub(self.pend_ms[k])) >= TWIN_MIN.as_millis() {
                self.twin |= bit;
            }
        }
    }

    /// Start over as if just armed where nothing was seen: every offset clear,
    /// no twin. The record now stands somewhere its earlier observations were
    /// not about, and later frames learn its new neighbours.
    fn forget_evidence(&mut self) {
        self.clear = ALL_OFFSETS;
        self.twin = 0;
        self.pending = 0;
    }

    /// The follow offsets at which a glyph found would have arrived: not known
    /// to have been there before ([`Seen::clear`]) and never a twin there. A
    /// twin testifies only about its own row
    /// (`the_evidence_is_what_each_follow_row_was_seen_holding`,
    /// `a_twin_testifies_only_about_its_own_row`).
    fn arrivals(&self) -> u8 {
        self.clear & !self.twin & ALL_OFFSETS
    }
}

/// Every follow offset's bit ([`FOLLOW_DRS`]).
const ALL_OFFSETS: u8 = 0x0F;

/// A cell whose recorded glyph went BLANK this walk — provisional until its
/// run has decided the clock ([`Witness::walk`]).
#[derive(Clone, Copy, Debug)]
struct Blanked {
    row: u16,
    col: u16,
    born: Instant,
    cohort: u32,
    /// The glyph unit the cell was recorded holding — what the moved-text
    /// search looks for elsewhere.
    unit: Unit,
    /// Released by an earlier walk and still under a blank: held until its run
    /// decides. It melts with a run-mate replaced this walk (not counted again)
    /// and is otherwise left to its retract; never searched for.
    held: bool,
    counted: bool,
}

/// One armed record on a row beside a withheld one
/// ([`Witness::find_deferred_runs`]).
#[derive(Clone, Copy, Debug)]
struct BlindRecord {
    row: u16,
    cohort: u32,
    col: u16,
    /// Its glyph no longer stands under it.
    changed: bool,
    /// …and its verdict was deferred once already ([`Seen::deferred`]).
    deferred: bool,
}

#[derive(Clone, Copy, Debug)]
struct RestoreRun {
    row: u16,
    cohort: u32,
    /// The release the run's cells carry ([`Cell::released_at`]): one
    /// partial release's stamp, or `None` for the cohort's unstamped cells.
    stamp: Option<Instant>,
    exact: bool,
    returned_ink: bool,
}

/// The witness: the sorted `(row, col, born)` list of cells seen holding a
/// glyph.
#[derive(Clone, Debug, Default)]
pub struct Witness {
    /// Sorted by `(row, col, born)`, one record per cell, resident and
    /// reused.
    seen: Vec<Seen>,
    /// `(row, cohort)` of every run a walk retired a cell of (a glyph replaced,
    /// or its blanked text found elsewhere): runs whose blanked and never-armed
    /// cells take the fast melt. Resident scratch.
    retired_runs: Vec<(u16, u32)>,
    /// Sorted `(row, cohort)` identities of retired runs that still have an
    /// armed, non-retired cell. Such runs keep their unchanged spaces;
    /// retired runs absent from this list take their spaces with them.
    /// Resident scratch, cleared per walk.
    standing_runs: Vec<(u16, u32)>,
    /// Sorted copy of this walk's retirement identities. The public verdict
    /// keeps its original order; this scratch makes membership logarithmic
    /// while finding surviving runs. Capacity is reused after warm-up.
    retired_cells: Vec<(u16, u16, Instant)>,
    /// `(row, cohort)` of every run a walk released (blanked, text nowhere
    /// visible): runs whose never-armed cells go to the swoosh with them when
    /// the text went whole. Resident scratch.
    released_runs: Vec<(u16, u32)>,
    /// Sorted `(row, cohort, col)` of every recorded glyph still standing this
    /// walk (not released, glyph under it). A released run with a cell here
    /// lost only part of its text and keeps never-armed cells outside the lost
    /// span and short of a standing glyph on their side
    /// ([`Witness::released_span_takes`]). Resident scratch.
    standing_glyphs: Vec<(u16, u32, u16)>,
    /// The runs the shape pass reads this walk: every run something was
    /// named in, retired or released, each once.
    shape_runs: Vec<(u16, u32)>,
    /// The ids of this walk's fresh releases, marked on their records only
    /// once the shape pass has let them stand.
    fresh_released: Vec<(u16, u16, Instant)>,
    /// The ids this walk named whose records were already counted — the
    /// re-verdicts, tallied against the names that survive the shape pass.
    counted_names: Vec<(u16, u16, Instant)>,
    /// One run's named ids, for the shape pass's retains.
    run_ids: Vec<(u16, u16, Instant)>,
    /// The cells whose glyph went blank this walk, sorted by `(row, cohort,
    /// col)` once the pass is over. Resident scratch, cleared per walk.
    blanked: Vec<Blanked>,
    restore_runs: Vec<RestoreRun>,
    /// The follow pass's runs — every `(row, cohort)` with a resident,
    /// non-leaving cell — sorted and deduped. Resident scratch.
    follow_runs: Vec<(u16, u32)>,
    /// One run's armed cells for the follow pass: `(col, unit, gone, arrivals)`
    /// in column order; `gone` when the recorded glyph no longer stands there,
    /// `arrivals` per [`Seen::arrivals`]. Resident scratch.
    follow_armed: Vec<(u16, Unit, bool, u8)>,
    /// The cells the last walk read over a blank, sorted `(row, col, born)`:
    /// resident, not leaving, never armed (a typed Space). Tells a glyph
    /// landing on an old blank this walk from a key's own cell first read this
    /// walk ([`Witness::walk`]'s landed arm). Rebuilt every walk; carried with
    /// its cells by scroll, band move and follow. Resident.
    blank_seen: Vec<(u16, u16, Instant)>,
    /// The next walk's [`Witness::blank_seen`], built by this one.
    /// Resident scratch.
    blank_next: Vec<(u16, u16, Instant)>,
    /// This walk's cells from [`Witness::blank_seen`] with a glyph under
    /// them now, `(row, col, born, cohort)`: armed on the walk's first pass
    /// as any blank → glyph is, and judged with their run once its verdict
    /// is in. Resident scratch.
    landed: Vec<(u16, u16, Instant, u32)>,
    /// Every armed record on a row beside a withheld one this walk
    /// ([`Witness::find_deferred_runs`]) — at most one per record. Resident
    /// scratch, reserved at [`WITNESS_CAP`] by [`Witness::new`].
    blind_changed: Vec<BlindRecord>,
    /// The runs this walk passes no verdict on, sorted: a row beside them
    /// was withheld ([`Witness::find_deferred_runs`]). Resident scratch,
    /// reserved at [`WITNESS_CAP`] by [`Witness::new`].
    deferred_runs: Vec<(u16, u32)>,
    /// When the last walk ran: a scroll arriving before the next walk dates the
    /// copies on rows it brings in from here, starting their [`Seen::pending`]
    /// clocks ([`Witness::translate`]). `None` until the first walk.
    last_walk: Option<Instant>,
    /// The shift pass's view of one run ([`Witness::shift_runs`]): every
    /// resident, non-leaving cell as `(col, recorded, gone)` in column order;
    /// `recorded` is `None` for a never-armed cell, `gone` when the recorded
    /// glyph no longer stands there. Resident scratch.
    shift_cells: Vec<(u16, Option<Unit>, bool)>,
}

/// A run the shift pass found along its own row ([`Witness::shift_runs`]): the
/// cells of `cohort` on `row` within `lo..=hi` stand under their own glyphs
/// `dc` columns right, the tail a mid-line insert of `dc` cells pushed along.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShiftRun {
    /// The run's row.
    pub row: u16,
    /// The run's cohort.
    pub cohort: u32,
    /// The leftmost column that moves — the insert's edit point: the new
    /// text stands on `lo..lo + dc`.
    pub lo: u16,
    /// The rightmost column that moves.
    pub hi: u16,
    /// Columns to the text: the inserted width, always positive.
    pub dc: i16,
}

/// The widest same-row shift searched for (a paste of a full wide line). Past
/// it the tail is left to the walk's fast melt. A bound on the search, not the
/// evidence.
pub const SHIFT_MAX_COLS: u16 = 512;

/// A run the follow pass found elsewhere ([`Witness::follow_runs`]): the cells
/// of `cohort` on `row` within `lo..=hi` stand under their own glyphs `dr` rows
/// away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FollowRun {
    /// The run's row now.
    pub row: u16,
    /// The run's cohort.
    pub cohort: u32,
    /// The leftmost column carried: the found block's, grown over its
    /// neutral and never-armed border ([`Witness::follow_runs`]).
    pub lo: u16,
    /// The rightmost column carried, grown the same way.
    pub hi: u16,
    /// Rows to the text: `−1` one row up, `+1` one row down, then `±2`.
    pub dr: i16,
}

/// What a follow pass saw beside what it named ([`Witness::follow_runs`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FollowScore {
    /// Missed follows: armed cells whose glyphs are gone from their row, in a
    /// run whose text arrived on a neighbouring sampled row (passing the
    /// half-gate of [`Witness::follow_runs`]) that the pass did not name
    /// because the arrivals were not one block. The witness judges such a run
    /// where it stands, so it melts or is released; this counter
    /// (`ribbon_follow_missed=`) makes that visible. A true re-layout, an
    /// erase under an identical line, and a kill under a line sharing only a
    /// prefix arrive nothing and count nothing.
    pub missed: u32,
}

/// One offset's reading of a run ([`Witness::follow_verdict`]).
#[derive(Clone, Copy, Debug)]
struct FollowVerdict {
    /// Records found: arrived at their own column.
    found: usize,
    /// Records neutral: their twin stands there, as it did at arm time.
    neutral: usize,
    /// Holes among the run's relaid suffix ([`Witness::relaid_suffix`]):
    /// records the wrap took elsewhere, out of the half-gate's denominator.
    relaid: usize,
    /// The block's extent, riders included.
    lo: u16,
    hi: u16,
    /// No found record after a hole that followed a found one.
    block: bool,
}

impl Witness {
    /// An empty witness with capacity reserved once ([`WITNESS_CAP`]) for the
    /// records and the deferral's scratch (at most one entry per record), so a
    /// frame with a withheld row never allocates.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seen: Vec::with_capacity(WITNESS_CAP),
            retired_runs: Vec::new(),
            standing_runs: Vec::new(),
            retired_cells: Vec::new(),
            released_runs: Vec::new(),
            standing_glyphs: Vec::new(),
            shape_runs: Vec::new(),
            fresh_released: Vec::new(),
            counted_names: Vec::new(),
            run_ids: Vec::new(),
            blanked: Vec::new(),
            restore_runs: Vec::new(),
            follow_runs: Vec::new(),
            follow_armed: Vec::new(),
            blank_seen: Vec::new(),
            blank_next: Vec::new(),
            landed: Vec::new(),
            blind_changed: Vec::with_capacity(WITNESS_CAP),
            deferred_runs: Vec::with_capacity(WITNESS_CAP),
            last_walk: None,
            shift_cells: Vec::new(),
        }
    }

    /// The follow pass's search (see the module doc). For every run
    /// `(row, cohort)` with a resident cell on a sampled row, read its armed
    /// records (live, not released, on non-leaving cells) in column order and
    /// name the run on `out` when its text moved vertically as a block:
    ///
    /// * At least two armed records, and at least two (no fewer than half) are
    ///   gone from their own row. A run whose glyphs still stand is never
    ///   followed; a single armed glyph is too little evidence.
    /// * On the nearest sampled row `row + dr`, `dr ∈ {−1, +1, −2, +2}`, at
    ///   least two records are found (own unit at own column, and arrived,
    ///   [`Seen::arrivals`]: not a twin, [`Seen::twin`], nor on an unseen
    ///   band-carried row, [`Seen::clear`]), no fewer than half of the records
    ///   that are neither neutral nor relaid by a wrap
    ///   ([`Witness::follow_verdict`], [`Witness::relaid_suffix`]). An erase
    ///   under an identical line (`$ cd ..` under `$ cd ..`, then Ctrl-U) and a
    ///   copy that appeared beside the line and stood [`TWIN_MIN`] (fzf's
    ///   list, a completion popup) name nothing. The found records form one
    ///   block, a prefix or suffix of the run, never letters either side of a
    ///   hole. The first qualifying `dr` wins; a twin row is refused and the
    ///   search continues (`a_twin_testifies_only_about_its_own_row`).
    ///
    /// A glyph found without evidence of arrival is neutral: in a composer the
    /// row above is the previous line of the paragraph, and a glyph under the
    /// same glyph at the same column is ordinary text
    /// (`tests/composer_growth_follows.rs`). A neutral record is neither found
    /// nor a hole, leaves the half-gate's denominator, and rides with the block
    /// (inside it, and flush at its edges whatever its own row reads; see
    /// [`Witness::follow_verdict`]) (owner, 2026-09-23).
    ///
    /// The word a wrap relaid is not a hole: a composer's wrap carries the
    /// line up minus its last word, laid again at the start of the caret row. A
    /// trailing suffix standing on the run's own row in order and spacing,
    /// starting at the row's first glyph column ([`Witness::relaid_suffix`]),
    /// leaves the denominator: the half-gate is
    /// `2·found ≥ armed − neutral − relaid`. Relaid holes are only subtracted,
    /// so a run of twins or holes still names nothing.
    ///
    /// `lo..=hi` is the found block's extent with riders, grown over a
    /// bordering never-armed cell of the run while no record beyond it on that
    /// side stayed behind (the Space typed at a full line's end before the
    /// wrapping glyph). The run's never-armed blanks inside go with it
    /// (`Ribbon::translate_run`). Returns what the pass saw ([`FollowScore`]).
    /// Pure in records and samples, `O(runs × cells × 4)` to search plus
    /// `O(width × cells)` once per qualifying run; allocation-free past
    /// warm-up.
    pub fn follow_runs(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        out: &mut Vec<FollowRun>,
    ) -> FollowScore {
        self.follow_runs_with_short_park(cells, rows, None, out)
    }

    /// A sole arriving glyph is evidence only for the exact short held park
    /// whose source row the host is judging; other searches keep the
    /// two-arrival floor.
    pub(crate) fn follow_runs_with_short_park(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        short_park: Option<((u16, u16), (u16, u16))>,
        out: &mut Vec<FollowRun>,
    ) -> FollowScore {
        let mut score = FollowScore::default();
        out.clear();
        self.follow_runs.clear();
        // The short-park exception belongs to the run right behind that park;
        // resolve its cohort once so unrelated runs keep the two-arrival floor.
        let short_owner = short_park.and_then(|(from, to)| {
            if from.0 != to.0 || !(1..=2).contains(&from.1.saturating_sub(to.1)) {
                return None;
            }
            let last_col = from.1.checked_sub(1)?;
            cells
                .iter()
                .find(|cell| !cell.leaving() && cell.row == from.0 && cell.col == last_col)
                .map(|cell| (from.0, cell.cohort))
        });
        for cell in cells {
            if !cell.leaving() && rows.iter().any(|s| s.row == cell.row) {
                self.follow_runs.push((cell.row, cell.cohort));
            }
        }
        self.follow_runs.sort_unstable();
        self.follow_runs.dedup();
        for k in 0..self.follow_runs.len() {
            let (row, cohort) = self.follow_runs[k];
            let Some(own) = rows.iter().find(|s| s.row == row) else {
                continue;
            };
            self.follow_armed.clear();
            for cell in cells {
                if cell.leaving() || cell.row != row || cell.cohort != cohort {
                    continue;
                }
                let Some(i) = self.find(cell.row, cell.col, cell.born) else {
                    continue;
                };
                let seen = self.seen[i];
                if seen.released {
                    continue;
                }
                let gone = unit_at(own.cols, cell.col) != seen.unit;
                self.follow_armed
                    .push((cell.col, seen.unit, gone, seen.arrivals()));
            }
            self.follow_armed.sort_unstable_by_key(|a| a.0);
            self.follow_armed.dedup_by_key(|a| a.0);
            let armed = self.follow_armed.len();
            let gone = self.follow_armed.iter().filter(|a| a.2).count();
            if armed < 2 || gone < 2 || gone * 2 < armed {
                continue;
            }
            let relaid = Self::relaid_suffix(&self.follow_armed, own.cols);
            let mut named = false;
            let mut evidence = false;
            for (k, dr) in FOLLOW_DRS.into_iter().enumerate() {
                let Some(target) = row.checked_add_signed(dr) else {
                    continue;
                };
                let Some(there) = rows.iter().find(|s| s.row == target) else {
                    continue;
                };
                let v = Self::follow_verdict(&self.follow_armed, there.cols, k, relaid);
                // The half-gate: two or more arrived, and no fewer than half of
                // the records neither neutral nor relaid. A single arrival is
                // allowed only with a substantial exact relaid suffix and no
                // competing counted cell.
                let counted = armed.saturating_sub(v.neutral + v.relaid);
                // A narrow wrap can leave one glyph on the row carried up. One
                // arrival is ambiguous alone, but the same frame relaying at
                // least three exact trailing glyphs to the old row's start
                // identifies the wrap, provided no other counted hole competes.
                let sole_kept = short_owner == Some((row, cohort))
                    && v.found == 1
                    && v.relaid >= 3
                    && counted == 1;
                let enough = (v.found >= 2 || sole_kept) && v.found * 2 >= counted;
                // Evidence without the block: enough records arrived here to be
                // a move, block or not ([`FollowScore::missed`] counts these
                // when no offset names the run).
                evidence |= enough;
                if !v.block || !enough {
                    continue;
                }
                let (mut lo, mut hi) = (v.lo, v.hi);
                // A glyph standing here that did not arrive — a twin, or a
                // row a band move carried beside it unseen: neutral.
                let neutral = |col: u16, unit: Unit, arrivals: u8| {
                    arrivals & (1 << k) == 0 && unit_at(there.cols, col) == unit
                };
                // The extent, riders included, grows over a bordering
                // never-armed cell while no record past it on that side stayed
                // behind.
                let is_cell = |col: u16| {
                    cells
                        .iter()
                        .any(|c| !c.leaving() && c.row == row && c.cohort == cohort && c.col == col)
                };
                let armed_at = |col: u16| {
                    self.follow_armed
                        .binary_search_by_key(&col, |a| a.0)
                        .ok()
                        .map(|i| self.follow_armed[i])
                };
                let beyond_hi = self
                    .follow_armed
                    .iter()
                    .any(|a| a.0 > hi && !neutral(a.0, a.1, a.3));
                let beyond_lo = self
                    .follow_armed
                    .iter()
                    .any(|a| a.0 < lo && !neutral(a.0, a.1, a.3));
                while let Some(next) = hi.checked_add(1) {
                    let grows = match armed_at(next) {
                        Some(a) => neutral(a.0, a.1, a.3),
                        None => !beyond_hi && is_cell(next),
                    };
                    if !grows {
                        break;
                    }
                    hi = next;
                }
                while let Some(prev) = lo.checked_sub(1) {
                    let grows = match armed_at(prev) {
                        Some(a) => neutral(a.0, a.1, a.3),
                        None => !beyond_lo && is_cell(prev),
                    };
                    if !grows {
                        break;
                    }
                    lo = prev;
                }
                out.push(FollowRun {
                    row,
                    cohort,
                    lo,
                    hi,
                    dr,
                });
                named = true;
                break;
            }
            if !named && evidence {
                score.missed = score
                    .missed
                    .saturating_add(u32::try_from(gone).unwrap_or(u32::MAX));
            }
        }
        score
    }

    /// One offset's reading of a run's armed records ([`Witness::follow_runs`]),
    /// bit `k` of each record's [`Seen::arrivals`] being that offset's. Each
    /// record, in column order, is:
    ///
    /// * found: its unit stands at its column on the target row with evidence
    ///   of arrival (the row was seen holding something else while the record
    ///   stood, [`Seen::clear`], and no copy stood there, [`Seen::twin`]);
    /// * neutral: its unit stands there without that evidence (a copy present
    ///   at arming, one that appeared and stood [`TWIN_MIN`], or an unseen
    ///   band-carried row);
    /// * a hole: its unit is not there. A hole among the last `relaid` records
    ///   ([`Witness::relaid_suffix`]) is counted apart for the half-gate.
    ///
    /// Found records must form one block (`found` still counts every arrival).
    /// Neutral records ride: inside the block, and flush at its edges (a lead
    /// directly before the first found record, a trail directly after the
    /// last), whatever their own row reads, because a composer re-lays the
    /// caret row with the moved word and an edge letter can read as standing.
    /// Leaving it behind would split the run and capture the new line's keys
    /// (`Ribbon::join_cohort`); carrying a glyph that truly stayed costs one
    /// cell. A neutral cut off by a hole stays.
    fn follow_verdict(
        armed: &[(u16, Unit, bool, u8)],
        there: &[char],
        k: usize,
        relaid: usize,
    ) -> FollowVerdict {
        let mut v = FollowVerdict {
            found: 0,
            neutral: 0,
            relaid: 0,
            lo: u16::MAX,
            hi: 0,
            block: true,
        };
        let relaid_from = armed.len().saturating_sub(relaid);
        let mut after = false;
        let mut lead: Option<u16> = None;
        let mut ext_hi = 0u16;
        for (i, &(col, unit, _, arrivals)) in armed.iter().enumerate() {
            let here = unit_at(there, col) == unit;
            // No evidence the glyph there arrived ([`Seen::arrivals`]): a
            // twin, or a row a band move carried beside it unseen.
            let twin = arrivals & (1 << k) == 0;
            if here && !twin {
                v.found += 1;
                if after {
                    // Not one block — but every arrival still counts for
                    // the evidence [`FollowScore::missed`] reads.
                    v.block = false;
                    continue;
                }
                v.lo = v.lo.min(lead.unwrap_or(col));
                lead = None;
                ext_hi = col;
            } else if here {
                v.neutral += 1;
                if v.found == 0 {
                    lead.get_or_insert(col);
                } else if !after {
                    // Inside the block, or trailing it flush: it rides.
                    ext_hi = col;
                }
            } else {
                if i >= relaid_from {
                    v.relaid += 1;
                }
                if v.found > 0 {
                    after = true;
                } else {
                    lead = None;
                }
            }
        }
        v.hi = ext_hi;
        v
    }

    /// The word a wrap relaid ([`Witness::follow_runs`]): how many of `armed`'s
    /// last records (column order) stand on the run's own row `own` in order
    /// and spacing, shifted left as one block to start at the row's first
    /// glyph column; the longest such suffix, `0` if none. That is a
    /// composer's wrap (`  this confirmatio` → `  confirmation`). Anchored at
    /// the first glyph, so a row rewritten with other text relays nothing.
    /// Read only for runs past the gone gate (an event, not a frame).
    fn relaid_suffix(armed: &[(u16, Unit, bool, u8)], own: &[char]) -> usize {
        let Some(first) = own
            .iter()
            .position(|&c| c != ' ' && c != '\0')
            .and_then(|i| u16::try_from(i).ok())
        else {
            return 0;
        };
        for (start, &(c0, ..)) in armed.iter().enumerate() {
            let Some(shift) = c0.checked_sub(first).filter(|&s| s > 0) else {
                continue;
            };
            if armed[start..]
                .iter()
                .all(|&(col, unit, ..)| unit_at(own, col - shift) == unit)
            {
                return armed.len() - start;
            }
        }
        0
    }

    /// The shift pass's search: the band follows its text along the row. A
    /// mid-line insert pushes every glyph from the edit point to the line's
    /// end right by the inserted width; read per cell each column is replaced
    /// text, and the walk would melt the whole tail. The text moved `dc`
    /// columns, and so does its light (`Ribbon::shift_run`), clocks and price
    /// intact, colour re-read at the new column (C2).
    ///
    /// For every run `(row, cohort)` on a sampled row whose records are all
    /// live (a released run is left to its retract) and none of whose cells
    /// right of the edit point is already leaving, in column order:
    ///
    /// * `s` is the run's first armed record whose glyph left its column;
    ///   without one nothing moved;
    /// * `dc` comes from the evidence: the nearest column right of `s` holding
    ///   `s`'s glyph (the narrowest insert that explains the row);
    /// * the block (every cell from `dc`'s first moved cell on) must stand at
    ///   `col + dc` under its own glyph (a never-armed cell under a blank) as
    ///   one run; the first cell that does not starts a tail, and every
    ///   recorded glyph of the tail must have left the row (blank at
    ///   `col + dc`: a wrapped word, the line's end past the grid). A tail
    ///   glyph standing on other text is a rewrite and refuses the shift;
    /// * the block extends left over cells whose glyph (or blank) also stands
    ///   at `col + dc` (repeated letters, spaces), and when the caret is inside
    ///   that extension the caret is the edit point;
    /// * evidence: at least two of the block's glyphs found, or one when the
    ///   edit point is the caret's column, nothing of the block left the row
    ///   and no text stands past it; and the found glyphs' own columns hold
    ///   text now (an insert overwrites from its edit point on; a copy of a
    ///   cleared tail proves nothing). A tail that left the row does not count
    ///   against the block.
    ///
    /// A delete is not followed: [`super::ribbon::Ribbon`]'s erase law takes
    /// the row's suffix from the caret, replayed at the key before the echo
    /// moves the text, so the tail is already leaving by the frame a shift
    /// could be read. This pass is the insert's.
    ///
    /// Pure in records, samples and caret; allocation-free past warm-up. Every
    /// engaged frame pays one record lookup per live cell on a sampled row
    /// (`O(cells × log records)`) and stops there when no armed glyph moved;
    /// only runs that lost a glyph are gathered and searched
    /// (`O(cells × width)` each).
    pub fn shift_runs(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        caret: Option<(u16, u16)>,
        out: &mut Vec<ShiftRun>,
    ) {
        out.clear();
        self.follow_runs.clear();
        // Only a run with a cell whose recorded glyph left its column can have
        // been pushed ([`find_shift`] starts there), so only those are gathered;
        // when every armed glyph stands, this loop is the whole pass.
        for cell in cells {
            if cell.leaving() {
                continue;
            }
            let Some(own) = rows.iter().find(|s| s.row == cell.row) else {
                continue;
            };
            let gone = self
                .find(cell.row, cell.col, cell.born)
                .is_some_and(|i| unit_at(own.cols, cell.col) != self.seen[i].unit);
            if gone {
                self.follow_runs.push((cell.row, cell.cohort));
            }
        }
        if self.follow_runs.is_empty() {
            return;
        }
        self.follow_runs.sort_unstable();
        self.follow_runs.dedup();
        for k in 0..self.follow_runs.len() {
            let (row, cohort) = self.follow_runs[k];
            let Some(own) = rows.iter().find(|s| s.row == row).copied() else {
                continue;
            };
            self.shift_cells.clear();
            let mut released = false;
            // The rightmost cell of the run already leaving (an erase's suffix
            // retract, a melt): no shift is read over light going out.
            let mut leaving_max: Option<u16> = None;
            for cell in cells {
                if cell.row != row || cell.cohort != cohort {
                    continue;
                }
                if cell.leaving() {
                    leaving_max = Some(leaving_max.map_or(cell.col, |m| m.max(cell.col)));
                    continue;
                }
                let rec = match self.find(cell.row, cell.col, cell.born) {
                    Some(i) if self.seen[i].released => {
                        released = true;
                        break;
                    }
                    Some(i) => Some(self.seen[i].unit),
                    None => None,
                };
                let gone = rec.is_some_and(|u| unit_at(own.cols, cell.col) != u);
                self.shift_cells.push((cell.col, rec, gone));
            }
            if released {
                continue;
            }
            self.shift_cells.sort_unstable_by_key(|c| c.0);
            self.shift_cells.dedup_by_key(|c| c.0);
            let caret_col = caret.filter(|c| c.0 == row).map(|c| c.1);
            if let Some((lo, hi, dc)) = find_shift(&self.shift_cells, own.cols, caret_col)
                && leaving_max.is_none_or(|m| m < lo)
            {
                out.push(ShiftRun {
                    row,
                    cohort,
                    lo,
                    hi,
                    dc,
                });
            }
        }
    }

    /// The records follow their cells along the row ([`Witness::shift_runs`] →
    /// `Ribbon::shift_run`): each identity in `moved` (a cell's old
    /// `(row, col, born)`) is re-keyed to `col + dc`, then re-sorted in place.
    ///
    /// A shifted record starts over ([`Seen::forget_evidence`]), as a carried
    /// one does ([`Witness::translate_cells`]): its neighbour evidence
    /// ([`Seen::clear`], [`Seen::twin`], [`Seen::pending`]) and any deferred
    /// verdict ([`Seen::deferred`]) were about its old column.
    pub fn shift_cells(&mut self, moved: &mut [(u16, u16, Instant)], dc: i16) {
        if moved.is_empty() || dc == 0 {
            return;
        }
        moved.sort_unstable();
        // A record standing where a moved one lands with the same birth is the
        // left-behind tail's (`Ribbon::shift_run` drops its cell); drop it so no
        // two records share a key.
        self.seen.retain(|s| {
            moved.binary_search(&(s.row, s.col, s.born)).is_ok()
                || !s
                    .col
                    .checked_add_signed(-dc)
                    .is_some_and(|src| moved.binary_search(&(s.row, src, s.born)).is_ok())
        });
        let mut any = false;
        for s in &mut self.seen {
            if moved.binary_search(&(s.row, s.col, s.born)).is_ok()
                && let Some(target) = s.col.checked_add_signed(dc)
            {
                s.col = target;
                s.forget_evidence();
                s.deferred = false;
                any = true;
            }
        }
        if any {
            self.seen.sort_unstable_by_key(|s| (s.row, s.col, s.born));
        }
        // A never-armed blank the shift carried with its run is still one
        // ([`Witness::blank_seen`], as the vertical carry keeps it).
        let mut blanks = false;
        for b in &mut self.blank_seen {
            if moved.binary_search(b).is_ok()
                && let Some(target) = b.1.checked_add_signed(dc)
            {
                b.1 = target;
                blanks = true;
            }
        }
        if blanks {
            self.blank_seen.sort_unstable();
            self.blank_seen.dedup();
        }
    }

    /// The records follow their cells ([`Witness::follow_runs`] →
    /// `Ribbon::translate_run`): each identity in `moved` (a cell's old
    /// `(row, col, born)`) is re-keyed to `row + dr`; never-armed cells carry
    /// only their place on [`Witness::blank_seen`]. Re-sorted in place (an
    /// event; `sort_unstable` allocates nothing).
    ///
    /// A carried record starts over ([`Seen::forget_evidence`]): the run moved
    /// alone, so its new neighbours are not the ones it was seen beside, and
    /// later walks learn them. A second move with no key between (a box
    /// relocated twice, a chat box pushed down twice) follows again
    /// (`tests/moved_again_without_a_key.rs`); starting with nothing clear
    /// would melt it. A copy beside the new row becomes a twin only after
    /// [`TWIN_MIN`] (the module doc's limits).
    pub fn translate_cells(&mut self, moved: &mut [(u16, u16, Instant)], dr: i16) {
        if moved.is_empty() || dr == 0 {
            return;
        }
        // The records are read against a sorted copy of the identities, so
        // a record already moved cannot break the search for the next.
        moved.sort_unstable();
        let mut any = false;
        for s in &mut self.seen {
            if moved.binary_search(&(s.row, s.col, s.born)).is_ok()
                && let Some(target) = s.row.checked_add_signed(dr)
            {
                s.row = target;
                s.forget_evidence();
                s.deferred = false;
                any = true;
            }
        }
        if any {
            self.seen.sort_unstable_by_key(|s| (s.row, s.col, s.born));
        }
        // A never-armed blank the pass carried with its run is still one.
        let mut blanks = false;
        for b in &mut self.blank_seen {
            if moved.binary_search(b).is_ok()
                && let Some(target) = b.0.checked_add_signed(dr)
            {
                b.0 = target;
                blanks = true;
            }
        }
        if blanks {
            self.blank_seen.sort_unstable();
        }
    }

    /// Records held.
    #[must_use]
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// True with nothing witnessed.
    #[must_use]
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Forget everything (a reset: the cells are gone with the coordinate
    /// space). Capacity is kept.
    pub fn clear(&mut self) {
        self.seen.clear();
        self.blank_seen.clear();
    }

    /// A scroll moved every cell up by `rows` (`Ribbon::translate_scroll`): the
    /// records move with them and those off the grid are dropped, as are the
    /// blanks the last walk read ([`Witness::blank_seen`]). Order is kept.
    ///
    /// The evidence moves with them: a scroll moves the whole screen, so each
    /// record's neighbours still hold what they held ([`Seen::clear`],
    /// [`Seen::twin`]). Forgetting it would lose a move right after the scroll
    /// (`tests/moved_again_without_a_key.rs`).
    ///
    /// Except about the rows it brought in: the last `rows` above `bottom`,
    /// the row past the region that scrolled (the grid's last row, or the
    /// focused pane's, `Engine::translate_scroll`). Whatever stands there came
    /// in with the scroll, so every offset reaching one gets its
    /// [`Seen::pending`] bit with the clock at [`Witness::last_walk`]: a copy
    /// there is a twin once [`TWIN_MIN`] has passed since
    /// (`tests/copy_beside_the_line.rs`); sooner, it is a torn relocation and
    /// follows (`tests/moves_are_followed.rs`). An offset never looked at stays
    /// clear. Rows below `bottom` (another pane's) are not marked; `bottom`
    /// is `0` while the grid is unmeasured, marking nothing.
    pub fn translate(&mut self, rows: u16, bottom: u16) {
        if rows == 0 {
            return;
        }
        let first_new = bottom.saturating_sub(rows);
        let last_walk = self.last_walk;
        self.seen.retain_mut(|s| {
            if s.row < rows {
                return false;
            }
            s.row -= rows;
            if bottom > 0 {
                for (k, dr) in FOLLOW_DRS.into_iter().enumerate() {
                    if s.row
                        .checked_add_signed(dr)
                        .is_some_and(|r| r >= first_new && r < bottom)
                    {
                        s.pending |= 1 << k;
                        s.pend_ms[k] = last_walk.map_or(0, |t| {
                            u32::try_from(t.saturating_duration_since(s.born).as_millis())
                                .unwrap_or(u32::MAX)
                        });
                    }
                }
            }
            true
        });
        self.blank_seen.retain_mut(|b| {
            if b.0 < rows {
                return false;
            }
            b.0 -= rows;
            true
        });
    }

    /// A row band moved (`Ribbon::translate_band`, the
    /// [`crate::cursor_glow::band_row`] law): a record outside `top..=bottom`
    /// stands, one inside moves by `delta`, one carried past the band's edge is
    /// dropped, and likewise the last walk's blanks ([`Witness::blank_seen`]).
    /// Rows can cross, so the list is re-sorted in place.
    ///
    /// A band move is not uniform, so neighbour evidence survives only where
    /// the neighbour moved with the record ([`band_keeps`]); other offsets are
    /// forgotten and not clear ([`Seen::clear`]): a glyph found there is no
    /// arrival until a sample shows the row without it. So vim `yyp`'s copy,
    /// written into the opened line in the same batch, is not where the
    /// original went when `S` clears it (`tests/copies_are_not_moves.rs`).
    pub fn translate_band(&mut self, top: u16, bottom: u16, delta: i16) {
        if delta == 0 || top > bottom {
            return;
        }
        self.seen
            .retain_mut(|s| match band_row(s.row, top, bottom, delta) {
                Some(row) => {
                    let keep = band_keeps(s.row, row, top, bottom, delta);
                    s.clear &= keep;
                    s.twin &= keep;
                    s.pending &= keep;
                    s.row = row;
                    true
                }
                None => false,
            });
        self.seen.sort_unstable_by_key(|s| (s.row, s.col, s.born));
        self.blank_seen
            .retain_mut(|b| match band_row(b.0, top, bottom, delta) {
                Some(row) => {
                    b.0 = row;
                    true
                }
                None => false,
            });
        self.blank_seen.sort_unstable();
    }

    /// The insert's rewrite re-laid `row` left of `col` ([`super::Event::Rewrite`]):
    /// drop the records and the last walk's blanks there
    /// ([`Witness::blank_seen`]), so the next walk arms the cells under the
    /// placeholder's glyphs (Claude Code's `[Image #1] `) instead of retiring
    /// them, keeping one continuous ribbon (§27). The suffix's records fall
    /// out on their own (its cells are leaving); a later re-lay with other text
    /// still retires the re-armed cells. Order is kept.
    pub fn forget_left_of(&mut self, row: u16, col: u16) {
        self.seen.retain(|s| !(s.row == row && s.col < col));
        self.blank_seen.retain(|b| !(b.0 == row && b.1 < col));
    }

    /// The records at `(row, col)`, appended to `out` — for the engine's
    /// hole tripwire (`super::holes`); a record's cost, and only when armed.
    pub(super) fn describe_cell(&self, row: u16, col: u16, now: Instant, out: &mut String) {
        use std::fmt::Write as _;
        for s in self.seen.iter().filter(|s| s.row == row && s.col == col) {
            let _ = write!(
                out,
                "{{born_age_ms={} unit={:?} live={} released={} restorable={} counted={} \
                 twin={:#06b} pending={:#06b} deferred={}}}",
                now.saturating_duration_since(s.born).as_millis(),
                s.unit,
                s.live,
                s.released,
                s.restorable,
                s.counted,
                s.twin,
                s.pending,
                s.deferred
            );
        }
    }

    /// The record for the cell `(row, col, born)`, if any.
    fn find(&self, row: u16, col: u16, born: Instant) -> Option<usize> {
        self.seen
            .binary_search_by(|s| (s.row, s.col, s.born).cmp(&(row, col, born)))
            .ok()
    }

    /// Arm a record for `cell` holding `unit` (unless blank, or the witness is
    /// full) and take its first frame of neighbour evidence from `nb`
    /// ([`Seen::observe`] with `at_arm`): a copy already standing at an offset
    /// is a twin at once; every other offset is clear.
    ///
    /// A fresh line's first keys have no band yet to name their rows, so the
    /// host names a waiting key's row and its `±1` on that frame
    /// (`Engine::ribbon_rows_for`), and the copy that was there first is seen
    /// as a twin (`tests/erased_under_its_twin.rs`). What was not seen is no
    /// evidence either way.
    fn arm(&mut self, cell: &Cell, unit: Unit, nb: &[Option<&[char]>; 4], now: Instant) {
        if unit.is_blank() {
            return;
        }
        self.insert(cell, unit, false);
        if let Some(i) = self.find(cell.row, cell.col, cell.born) {
            self.seen[i].observe(nb, true, now);
        }
    }

    /// A released record for a never-armed cell of a run the walk just
    /// released (the space in `hello world`): a glyph landing on it reads as
    /// replaced text, like its run-mates. Skipped when the witness is full.
    fn hold_released(&mut self, cell: &Cell) {
        self.insert(cell, Unit::BLANK, true);
    }

    /// Insert the record for `cell` at its sorted place, live; a full
    /// witness ([`WITNESS_CAP`]) takes nothing.
    fn insert(&mut self, cell: &Cell, unit: Unit, released: bool) {
        if self.seen.len() >= WITNESS_CAP {
            return;
        }
        let key = (cell.row, cell.col, cell.born);
        let at = self.seen.partition_point(|s| (s.row, s.col, s.born) < key);
        self.seen.insert(
            at,
            Seen {
                row: cell.row,
                col: cell.col,
                born: cell.born,
                unit,
                live: true,
                released,
                restorable: released,
                counted: released,
                clear: ALL_OFFSETS,
                twin: 0,
                pending: 0,
                pend_ms: [0; 4],
                deferred: false,
            },
        );
    }

    /// Both cells of the unit a record stands for, pushed onto `retire` by
    /// identity. The partner half has the same `born` (one keystroke lays both,
    /// `Ribbon::lay`); a cell there with another birth is judged on its own.
    fn push_unit(
        retire: &mut Vec<(u16, u16, Instant)>,
        row: u16,
        col: u16,
        born: Instant,
        unit: Unit,
    ) {
        retire.push((row, col, born));
        if unit.wide {
            if unit.cont {
                if let Some(lead) = col.checked_sub(1) {
                    retire.push((row, lead, born));
                }
            } else {
                retire.push((row, col.saturating_add(1), born));
            }
        }
    }

    /// The walk. Every resident, non-leaving cell is read against this frame's
    /// samples: on an unsampled row it keeps what the witness knew; first seen
    /// over a glyph, it is armed; if its recorded glyph changed, its unit goes
    /// onto `retire` by identity `(row, col, born)` and its record is dropped;
    /// if its recorded glyph went blank, it goes onto `release` (unless its run
    /// decides otherwise) and its record is kept, marked released; a cell
    /// re-laid since (another `born`) is armed on its own. Both lists are
    /// cleared here.
    ///
    /// The run decides the clock. If any cell of the run was replaced this
    /// walk, or (for a run with no recorded glyph still standing in place) its
    /// blanked glyphs are found in order and spacing on another sampled row
    /// ([`Witness::moved`]), its blanked cells go onto `retire`; otherwise onto
    /// `release`. Never-armed cells follow their run when it is released whole
    /// or has no witnessed glyph left, and by
    /// [`Witness::released_span_takes`] while its text still stands; a
    /// released run's never-armed cells get released records. Records whose
    /// cells the walk never reached are forgotten. A cell may be pushed twice
    /// (both halves resident); `Ribbon::retire_cells` and
    /// `Ribbon::release_cells` act once.
    ///
    /// A released record is read again every walk while resident: the same
    /// glyph back is D2 and a blank is nothing, but a glyph landing under it
    /// retires it and every held run-mate under a blank. The walk returns how
    /// many already-released cells it named on `retire`, so the caller keeps
    /// `ribbon_retired=` at one count per cell.
    ///
    /// Every standing record takes this frame's neighbour evidence
    /// ([`Seen::observe`]), as does a record armed this walk; the next tick's
    /// follow pass judges arrivals by it.
    ///
    /// A run beside a withheld row waits one walk: `withheld` are rows asked
    /// for and not delivered (`Engine::follow_rows`), and a run with a changed
    /// record beside one gets no verdict, arming or evidence, at most once in a
    /// row ([`Witness::find_deferred_runs`]).
    pub fn walk(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        withheld: &[u16],
        now: Instant,
        retire: &mut Vec<(u16, u16, Instant)>,
        release: &mut Vec<(u16, u16, Instant)>,
    ) -> usize {
        retire.clear();
        release.clear();
        self.retired_runs.clear();
        self.released_runs.clear();
        self.standing_glyphs.clear();
        self.blanked.clear();
        self.fresh_released.clear();
        self.counted_names.clear();
        self.blank_next.clear();
        self.landed.clear();
        for s in &mut self.seen {
            s.live = false;
        }
        // The frame a scroll before the next walk dates its rows from.
        self.last_walk = Some(now);
        // The rows sampled at every follow offset from the row last looked up,
        // recomputed only when the walk changes row.
        let mut nb_row = None;
        let mut nb = [None; 4];
        self.find_deferred_runs(cells, rows, withheld);
        for cell in cells {
            if !self.deferred_runs.is_empty()
                && self
                    .deferred_runs
                    .binary_search(&(cell.row, cell.cohort))
                    .is_ok()
            {
                if let Some(i) = self.find(cell.row, cell.col, cell.born) {
                    self.seen[i].live = true;
                    if !self.seen[i].released
                        && rows
                            .iter()
                            .find(|s| s.row == cell.row)
                            .is_some_and(|s| unit_at(s.cols, cell.col) != self.seen[i].unit)
                    {
                        self.seen[i].deferred = true;
                    }
                } else if self.was_blank(cell) {
                    // No verdict this walk: it keeps what the witness knew
                    // ([`Witness::blank_seen`]).
                    self.blank_next.push((cell.row, cell.col, cell.born));
                }
                continue;
            }
            if cell.leaving() {
                // A cell a partial release stamped onto the retract keeps its
                // released record while resident, so an identical redraw can
                // still restore it ([`Witness::restored_runs`],
                // `Ribbon::restore_releases`).
                if cell.released_at.is_some()
                    && let Some(i) = self.find(cell.row, cell.col, cell.born)
                    && self.seen[i].released
                {
                    self.seen[i].live = true;
                }
                continue;
            }
            let Some(sample) = rows.iter().find(|s| s.row == cell.row) else {
                if let Some(i) = self.find(cell.row, cell.col, cell.born) {
                    self.seen[i].live = true;
                } else if self.was_blank(cell) {
                    // Unsampled, it keeps what the witness knew.
                    self.blank_next.push((cell.row, cell.col, cell.born));
                }
                continue;
            };
            let unit = unit_at(sample.cols, cell.col);
            if nb_row != Some(cell.row) {
                nb_row = Some(cell.row);
                nb = neighbours_of(rows, cell.row);
            }
            match self.find(cell.row, cell.col, cell.born) {
                Some(i) => {
                    let seen = self.seen[i];
                    if seen.unit == unit {
                        self.seen[i].live = true;
                        self.seen[i].deferred = false;
                        // A recorded glyph standing in place: its run still has
                        // text, and its neighbours this frame are evidence.
                        if !seen.released && !unit.is_blank() {
                            self.standing_glyphs.push((cell.row, cell.cohort, cell.col));
                            self.seen[i].observe(&nb, false, now);
                        }
                    } else if unit.is_blank() {
                        self.blanked.push(Blanked {
                            row: cell.row,
                            col: cell.col,
                            born: cell.born,
                            cohort: cell.cohort,
                            unit: seen.unit,
                            held: seen.released,
                            counted: seen.counted,
                        });
                    } else {
                        // A different glyph — under a live cell, or under a
                        // released one still retracting: the wrong letters.
                        Self::push_unit(retire, cell.row, cell.col, cell.born, seen.unit);
                        let run = (cell.row, cell.cohort);
                        if !self.retired_runs.contains(&run) {
                            self.retired_runs.push(run);
                        }
                        if seen.counted {
                            self.counted_names.push((cell.row, cell.col, cell.born));
                        }
                    }
                }
                None => {
                    if unit.is_blank() {
                        self.blank_next.push((cell.row, cell.col, cell.born));
                    } else if self.was_blank(cell) {
                        // A glyph landed on an old blank: armed as ever,
                        // and judged with its run below.
                        self.landed
                            .push((cell.row, cell.col, cell.born, cell.cohort));
                    }
                    self.arm(cell, unit, &nb, now);
                }
            }
        }
        self.standing_glyphs.sort_unstable();
        self.standing_glyphs.dedup();
        // The run decides: each run with a blanked cell is retired if one of
        // its glyphs was replaced or its blanked text is found elsewhere, and
        // released otherwise. Sorted by run, fresh blanks before held ones, so
        // the moved-text search reads each run's new blanks in column order,
        // once, and never the held cells'.
        self.blanked
            .sort_unstable_by_key(|b| (b.row, b.cohort, b.held, b.col));
        let mut i = 0;
        while i < self.blanked.len() {
            let run = (self.blanked[i].row, self.blanked[i].cohort);
            let end = i + self.blanked[i..]
                .iter()
                .take_while(|b| (b.row, b.cohort) == run)
                .count();
            let fresh_end = i + self.blanked[i..end].iter().take_while(|b| !b.held).count();
            // A run that still stands has not moved. The moved-text search has
            // no length floor, so a short fragment (`TO`) is found on almost any
            // line. A present between two PTY reads of one large frame shows
            // the row cleared and rewritten only to the read boundary, and the
            // newest letters would be ruled moved text and retired permanently
            // (owner, 2026-09-22).
            // Text that moved takes its whole run, so a run with a recorded
            // glyph still standing in place ([`Witness::standing_glyphs`]) lost
            // a tail: its blanked glyphs are released (restorable) and never
            // searched for. A replaced glyph still retires its run, and a run
            // with nothing standing is still searched.
            let fast = self.retired_runs.contains(&run)
                || (self.standing_glyphs_of(run).is_empty()
                    && Self::moved(&self.blanked[i..fresh_end], rows));
            if fast {
                // The whole run melts: fresh blanks with their replaced or moved
                // run-mates, and held cells too (already counted).
                for b in &self.blanked[i..end] {
                    Self::push_unit(retire, b.row, b.col, b.born, b.unit);
                    if b.counted {
                        self.counted_names.push((b.row, b.col, b.born));
                    }
                }
                if !self.retired_runs.contains(&run) {
                    self.retired_runs.push(run);
                }
            } else {
                // The run's text is gone: the fresh blanks are released and
                // their records kept, marked; the held cells stay on the
                // retract they are on.
                for b in &self.blanked[i..end] {
                    if !b.held {
                        Self::push_unit(release, b.row, b.col, b.born, b.unit);
                        self.fresh_released.push((b.row, b.col, b.born));
                        if b.counted {
                            self.counted_names.push((b.row, b.col, b.born));
                        }
                    }
                    if let Some(k) = self.find(b.row, b.col, b.born) {
                        self.seen[k].live = true;
                    }
                }
                if fresh_end > i {
                    self.released_runs.push(run);
                }
            }
            i = end;
        }
        self.shape_verdicts(cells, rows, now, retire, release);
        self.landed_goes_with_its_run(cells, retire, release);
        // The releases that stood: records kept and marked (a glyph landing
        // later is replaced text, a blank is nothing, the same glyph is D2),
        // counted once.
        for k in 0..self.fresh_released.len() {
            let (row, col, born) = self.fresh_released[k];
            if let Some(i) = self.find(row, col, born) {
                self.seen[i].released = true;
                self.seen[i].counted = true;
            }
        }
        let recounted = self
            .counted_names
            .iter()
            .filter(|id| retire.contains(id) || release.contains(id))
            .count();
        self.find_standing_runs(cells, retire);
        // Blanks go with their run: a named run's cell with no record (never
        // armed) goes with its run-mates on the run's clock, and a released
        // run's gets a released record. Cells with records were read above.
        self.released_runs.sort_unstable();
        if !self.retired_runs.is_empty() || !self.released_runs.is_empty() {
            for cell in cells {
                if cell.leaving() || self.find(cell.row, cell.col, cell.born).is_some() {
                    continue;
                }
                let run = (cell.row, cell.cohort);
                if self.retired_runs.binary_search(&run).is_ok() {
                    // A blank still blank in a run still standing is not
                    // evidence. A never-armed cell is a space; it goes with a
                    // retired run because the run's letters moved, but where the
                    // space is still a space nothing moved under it. Retiring it
                    // would punch word-shaped gaps into a band after a one-char
                    // mid-line insert. A run with nothing standing, and a
                    // released run whose text went whole, still take their
                    // blanks: there the space is a one-cell stray
                    // (owner, 2026-09-15).
                    let still_blank = rows
                        .iter()
                        .find(|s| s.row == cell.row)
                        .is_some_and(|s| unit_at(s.cols, cell.col).is_blank());
                    if !still_blank || self.standing_runs.binary_search(&run).is_err() {
                        retire.push((cell.row, cell.col, cell.born));
                    }
                } else if self.released_runs.binary_search(&run).is_ok()
                    && self.released_span_takes(cells, release, run, cell.col)
                {
                    // A partial release takes no space outside what it lost. A
                    // row sampled mid-rewrite can read one recorded glyph blank
                    // for a walk; taking every space of the run with it would
                    // leave the spaces dark when the glyph returns. A released
                    // run whose recorded glyphs still stand takes its
                    // never-armed cells only inside the span it lost, or past
                    // the span with no standing glyph beyond them on their side
                    // (`released_span_takes`, `tests/partial_release.rs`): a
                    // cleared suffix takes its inner and trailing spaces, a
                    // single blanked letter takes none, and the space before a
                    // cleared suffix stands while letters left of it do. A run
                    // with nothing standing takes all its spaces
                    // (owner, 2026-09-21).
                    release.push((cell.row, cell.col, cell.born));
                    self.hold_released(cell);
                }
            }
        }
        // What a run loses, it loses in one piece. Every verdict above is per
        // cell. A text shift (mid-line Backspace or insert) changes every
        // column from the edit to the line's end, but natural text repeats, so
        // some shifted columns hold the same glyph and compare unchanged,
        // leaving isolated specks between named cells.
        //
        // `Ribbon::retract_suffix` states that light leaves a row contiguously
        // and never from the middle; the Backspace, kill and insert rewrite
        // obey it, but a per-cell verdict can name any subset. So within one
        // run (one row, one cohort) every cell between the leftmost and
        // rightmost named column goes with them: a cell inside that span
        // compared equal by coincidence.
        //
        // The span is the tightest closure that removes holes: a real interior
        // change stays interior (a wide glyph's two cells,
        // [`a_wide_glyph_s_two_cells_are_retired_as_one_unit`]; a single stale
        // cell,
        // [`a_key_typed_over_an_abandoned_cohorts_cell_keeps_its_own_light`]).
        // Runs with nothing named are untouched, and released cells keep their
        // own clock and restore path ([`Witness::restored_runs`]).
        if !retire.is_empty() {
            for i in 0..self.retired_runs.len() {
                let (row, cohort) = self.retired_runs[i];
                let mut lo = u16::MAX;
                let mut hi = 0u16;
                let mut named = false;
                for cell in cells {
                    if cell.row == row
                        && cell.cohort == cohort
                        && retire.contains(&(cell.row, cell.col, cell.born))
                    {
                        named = true;
                        lo = lo.min(cell.col);
                        hi = hi.max(cell.col);
                    }
                }
                if !named || lo >= hi {
                    continue;
                }
                for cell in cells {
                    if cell.leaving()
                        || cell.row != row
                        || cell.cohort != cohort
                        || cell.col < lo
                        || cell.col > hi
                    {
                        continue;
                    }
                    let id = (cell.row, cell.col, cell.born);
                    if !retire.contains(&id) && !release.contains(&id) {
                        retire.push(id);
                    }
                }
            }
        }
        // Tag every original run identity, including its still-visible
        // letters and spaces. A later retype arms an untagged new birth.
        for cell in cells {
            if self
                .released_runs
                .binary_search(&(cell.row, cell.cohort))
                .is_ok()
                && let Some(i) = self.find(cell.row, cell.col, cell.born)
            {
                self.seen[i].restorable = true;
            }
        }
        self.seen.retain(|s| s.live);
        std::mem::swap(&mut self.blank_seen, &mut self.blank_next);
        self.blank_seen.sort_unstable();
        self.blank_seen.dedup();
        recounted
    }

    /// Whether the last walk read `cell` over a blank with no record
    /// ([`Witness::blank_seen`]).
    fn was_blank(&self, cell: &Cell) -> bool {
        self.blank_seen
            .binary_search(&(cell.row, cell.col, cell.born))
            .is_ok()
    }

    /// A glyph that landed on a blank with the rewrite that took its run goes
    /// with the run. A cell the last walk read over a blank
    /// ([`Witness::blank_seen`]) that now holds a glyph was armed on the walk's
    /// first pass (a late echo is never retired). But if its run was retired
    /// this walk, or released with this cell inside what the release takes
    /// ([`Witness::released_span_takes`]), the glyph came with the rewrite, not
    /// a key: the cell leaves on the run's verdict, on the melt (`retire`, its
    /// fresh record dropped) when no standing glyph of the run lies beyond it
    /// ([`Witness::retired_far_side_clear`]), or on the swoosh (`release`,
    /// record kept and marked) (owner, 2026-09-24).
    ///
    /// Case (`tests/composer_growth_follows.rs`): a composer wrap relays a
    /// word over the column of the Space the wrap ate; that cell, armed under
    /// the relaid glyph, would stand while its run melted, and
    /// `Ribbon::join_cohort` would lay the new line into the old run's walk. A
    /// key's own cell is never on [`Witness::blank_seen`] on its first walk,
    /// so it is armed as before, as is a landed glyph whose run keeps its text.
    fn landed_goes_with_its_run(
        &mut self,
        cells: &[Cell],
        retire: &mut Vec<(u16, u16, Instant)>,
        release: &mut Vec<(u16, u16, Instant)>,
    ) {
        for k in 0..self.landed.len() {
            let (row, col, born, cohort) = self.landed[k];
            let run = (row, cohort);
            if self.retired_runs.contains(&run) {
                if !self.retired_far_side_clear(cells, retire, run, col) {
                    continue;
                }
                retire.push((row, col, born));
                if let Some(i) = self.find(row, col, born) {
                    self.seen[i].live = false;
                }
            } else if self.released_runs.contains(&run)
                && self.released_span_takes(cells, release, run, col)
            {
                // The run's own verdict: a released run leaves on the swoosh,
                // and its landed blank rides it, released and marked.
                release.push((row, col, born));
                self.fresh_released.push((row, col, born));
            }
        }
    }

    /// Whether a landed cell at `col` of the retired `run` is the run's loose
    /// blank rather than a cell between standing text: inside the named span,
    /// or outside it with no standing glyph of the run beyond it (the rule
    /// [`Witness::released_span_takes`] reads for a released run). `ab cd ef`
    /// → `ab*cd eX` names only `f`'s cell; the `*` has `ab` beyond it and stays.
    /// The span is read off the cells named so far, so one landed cell cannot
    /// widen it for another.
    fn retired_far_side_clear(
        &self,
        cells: &[Cell],
        retire: &[(u16, u16, Instant)],
        run: (u16, u32),
        col: u16,
    ) -> bool {
        let mut lo = u16::MAX;
        let mut hi = 0u16;
        for cell in cells {
            if (cell.row, cell.cohort) != run
                || !retire.contains(&(cell.row, cell.col, cell.born))
                || self
                    .landed
                    .iter()
                    .any(|l| (l.0, l.1, l.2) == (cell.row, cell.col, cell.born))
            {
                continue;
            }
            lo = lo.min(cell.col);
            hi = hi.max(cell.col);
        }
        if lo > hi || (lo <= col && col <= hi) {
            return true;
        }
        let standing = self.standing_glyphs_of(run);
        if col < lo {
            standing.first().is_none_or(|s| s.2 > col)
        } else {
            standing.last().is_none_or(|s| s.2 < col)
        }
    }

    /// A run whose neighbour was withheld is not judged this walk. `withheld`
    /// are rows the host was asked for and did not deliver, inside the grid and
    /// the focused pane (`Engine::follow_rows`): the split or zoomed host reads
    /// rows past the caret's `±1` under a second, generation-checked lock and
    /// drops them if a PTY chunk landed between. A run beside one may have
    /// moved there (`tests/moved_again_without_a_key.rs`).
    ///
    /// So a run beside a withheld row that the follow pass could carry (two of
    /// its armed glyphs gone, and at least half, [`Witness::follow_runs`]) goes
    /// on [`Witness::deferred_runs`] and gets no verdict this walk. A change the
    /// follow pass could never carry is judged at once (a lone interior rewrite
    /// is the shape pass's, `Witness::shape_verdicts`). At most once: a run
    /// with a record already deferred ([`Seen::deferred`]) is judged. Nothing is
    /// scanned when nothing was withheld; otherwise `O(cells × log records)`
    /// into scratch reserved by [`Witness::new`].
    fn find_deferred_runs(&mut self, cells: &[Cell], rows: &[RowSample<'_>], withheld: &[u16]) {
        self.blind_changed.clear();
        self.deferred_runs.clear();
        if withheld.is_empty() {
            return;
        }
        for cell in cells {
            if cell.leaving() {
                continue;
            }
            let blind = [cell.row.checked_sub(1), cell.row.checked_add(1)]
                .into_iter()
                .flatten()
                .any(|r| withheld.contains(&r));
            if !blind {
                continue;
            }
            let Some(sample) = rows.iter().find(|s| s.row == cell.row) else {
                continue;
            };
            let Some(i) = self.find(cell.row, cell.col, cell.born) else {
                continue;
            };
            let seen = self.seen[i];
            if seen.released {
                continue;
            }
            let changed = unit_at(sample.cols, cell.col) != seen.unit;
            self.blind_changed.push(BlindRecord {
                row: cell.row,
                cohort: cell.cohort,
                col: cell.col,
                changed,
                deferred: changed && seen.deferred,
            });
        }
        self.blind_changed
            .sort_unstable_by_key(|b| (b.row, b.cohort, b.col));
        self.blind_changed
            .dedup_by_key(|b| (b.row, b.cohort, b.col));
        let mut k = 0;
        while k < self.blind_changed.len() {
            let run = (self.blind_changed[k].row, self.blind_changed[k].cohort);
            let end = k + self.blind_changed[k..]
                .iter()
                .take_while(|b| (b.row, b.cohort) == run)
                .count();
            let records = &self.blind_changed[k..end];
            let changed = records.iter().filter(|b| b.changed).count();
            // Only a run the follow pass could carry waits: two glyphs gone,
            // and at least half.
            if changed >= 2 && changed * 2 >= records.len() && !records.iter().any(|b| b.deferred) {
                self.deferred_runs.push(run);
            }
            k = end;
        }
    }

    /// A run loses a prefix, a suffix or all of itself, never its middle
    /// (owner, 2026-09-16).
    ///
    /// [`Ribbon::retract_suffix`] states that light leaves a row contiguously,
    /// and every geometric path obeys it; the witness removes light by name.
    /// An app painting a decoration into a blank cell and moving it on (Codex's
    /// ambient particles, `tests/codex_particle_replay.rs`), a spinner glyph, a
    /// spellcheck rewriting one letter or a TUI-drawn cursor glyph would each
    /// punch a one-cell hole, or release the whole run.
    ///
    /// For a standing run (some record neither released nor named this walk)
    /// the names are read as one shape against the run's recorded extent (cells
    /// with records; never-armed spaces are not ends):
    ///
    /// * names reaching the first or last recorded cell are a prefix, suffix or
    ///   the whole, and stand (a mid-line shift, a cleared tail, a rewritten
    ///   line);
    /// * interior names covering most of the recorded cells are a rewrite whose
    ///   end cell compared equal by coincidence, and are extended to the nearer
    ///   end;
    /// * interior names covering less are not evidence: the light stays and
    ///   the records take the glyphs standing now (a blank records a blank). A
    ///   record caught up this way is armed again: its evidence is forgotten
    ///   ([`Seen::forget_evidence`]) and this walk is the new glyph's arming
    ///   frame ([`Seen::observe`] with `at_arm`), so a copy of the new glyph
    ///   beside the line now is a twin at once
    ///   (`a_record_the_shape_pass_caught_up_is_armed_again`,
    ///   `tests/copies_are_not_moves.rs`).
    ///
    /// A run with nothing standing is not shaped: a glyph landing under it is
    /// replaced text and the run melts.
    fn shape_verdicts(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        now: Instant,
        retire: &mut Vec<(u16, u16, Instant)>,
        release: &mut Vec<(u16, u16, Instant)>,
    ) {
        self.shape_runs.clear();
        self.shape_runs.extend_from_slice(&self.retired_runs);
        self.shape_runs.extend_from_slice(&self.released_runs);
        self.shape_runs.sort_unstable();
        self.shape_runs.dedup();
        for k in 0..self.shape_runs.len() {
            let run = self.shape_runs[k];
            let (row, cohort) = run;
            let mut lo_r = u16::MAX;
            let mut hi_r = 0u16;
            let mut recorded = 0usize;
            let mut lo_n = u16::MAX;
            let mut hi_n = 0u16;
            let mut standing = false;
            self.run_ids.clear();
            for cell in cells {
                if cell.leaving() || cell.row != row || cell.cohort != cohort {
                    continue;
                }
                let id = (cell.row, cell.col, cell.born);
                let Some(i) = self.find(cell.row, cell.col, cell.born) else {
                    continue;
                };
                recorded += 1;
                lo_r = lo_r.min(cell.col);
                hi_r = hi_r.max(cell.col);
                if retire.contains(&id) || release.contains(&id) {
                    self.run_ids.push(id);
                    lo_n = lo_n.min(cell.col);
                    hi_n = hi_n.max(cell.col);
                } else if !self.seen[i].released {
                    standing = true;
                }
            }
            if self.run_ids.is_empty() || !standing || lo_n <= lo_r || hi_n >= hi_r {
                continue;
            }
            let inside = cells
                .iter()
                .filter(|c| {
                    !c.leaving()
                        && c.row == row
                        && c.cohort == cohort
                        && (lo_n..=hi_n).contains(&c.col)
                        && self.find(c.row, c.col, c.born).is_some()
                })
                .count();
            if inside * 2 > recorded {
                // A rewrite of most of the run: extend to the nearer end.
                let (a, b) = if lo_n - lo_r <= hi_r - hi_n {
                    (lo_r, hi_n)
                } else {
                    (lo_n, hi_r)
                };
                let releasing = !self.retired_runs.contains(&run);
                for cell in cells {
                    if cell.leaving()
                        || cell.row != row
                        || cell.cohort != cohort
                        || !(a..=b).contains(&cell.col)
                    {
                        continue;
                    }
                    let id = (cell.row, cell.col, cell.born);
                    if retire.contains(&id) || release.contains(&id) {
                        continue;
                    }
                    if releasing {
                        release.push(id);
                        self.fresh_released.push(id);
                    } else {
                        retire.push(id);
                    }
                }
                continue;
            }
            // Not evidence: the names come off, the records catch up.
            retire.retain(|id| !self.run_ids.contains(id));
            release.retain(|id| !self.run_ids.contains(id));
            self.fresh_released.retain(|id| !self.run_ids.contains(id));
            self.retired_runs.retain(|r| *r != run);
            self.released_runs.retain(|r| *r != run);
            let sample = rows.iter().find(|s| s.row == row);
            let nb = neighbours_of(rows, row);
            for j in 0..self.run_ids.len() {
                let (r, c, born) = self.run_ids[j];
                if let Some(i) = self.find(r, c, born) {
                    self.seen[i].unit = sample.map_or(Unit::BLANK, |s| unit_at(s.cols, c));
                    self.seen[i].live = true;
                    // The record's evidence was about the glyph it no longer
                    // claims: it is armed again on the new glyph, and this is
                    // its arming frame, so a copy standing beside it now is a
                    // twin at once.
                    self.seen[i].forget_evidence();
                    self.seen[i].observe(&nb, true, now);
                }
            }
        }
    }

    /// Whether the never-armed cell at `col` of the released `run` goes with
    /// the release. A run with no recorded glyph standing lost its text whole:
    /// yes. Otherwise it goes in two shapes: the span (`col` strictly inside
    /// the span being lost, between cells named on `release` this walk or
    /// stamped by an earlier partial release, `Cell::released_at`), and the
    /// tail (`col` past the span with no standing recorded glyph beyond it on
    /// its side). So `hello world` with one interior glyph blanked keeps its
    /// space, a cleared `world` takes the typed trailing space, and the space
    /// before a cleared suffix stands because `hello` does.
    fn released_span_takes(
        &self,
        cells: &[Cell],
        release: &[(u16, u16, Instant)],
        run: (u16, u32),
        col: u16,
    ) -> bool {
        let standing = self.standing_glyphs_of(run);
        if standing.is_empty() {
            return true;
        }
        let mut lo = u16::MAX;
        let mut hi = 0u16;
        for cell in cells {
            if (cell.row, cell.cohort) != run {
                continue;
            }
            let lost = (cell.leaving() && cell.released_at.is_some())
                || release.contains(&(cell.row, cell.col, cell.born));
            if lost {
                lo = lo.min(cell.col);
                hi = hi.max(cell.col);
            }
        }
        if lo > hi {
            return false;
        }
        let inside = lo < col && col < hi;
        // `standing` is column-sorted: its last entry is the rightmost
        // standing glyph, its first the leftmost.
        let tail =
            (col > hi && standing[standing.len() - 1].2 <= hi) || (col < lo && standing[0].2 >= lo);
        inside || tail
    }

    /// The standing recorded glyphs of `run` this walk, column-sorted — a
    /// contiguous slice of the sorted [`Witness::standing_glyphs`].
    fn standing_glyphs_of(&self, run: (u16, u32)) -> &[(u16, u32, u16)] {
        let lo = self
            .standing_glyphs
            .partition_point(|&(r, c, _)| (r, c) < run);
        let hi = self
            .standing_glyphs
            .partition_point(|&(r, c, _)| (r, c) <= run);
        &self.standing_glyphs[lo..hi]
    }

    /// Find the retired runs with something left standing, in one pool walk.
    /// Only runs with no surviving armed cell take their unchanged spaces.
    /// Membership checks use sorted resident scratch; the verdict's order and
    /// identities are untouched.
    fn find_standing_runs(&mut self, cells: &[Cell], retire: &[(u16, u16, Instant)]) {
        self.standing_runs.clear();
        self.retired_cells.clear();
        if self.retired_runs.is_empty() {
            return;
        }
        self.retired_runs.sort_unstable();
        self.retired_cells.extend_from_slice(retire);
        self.retired_cells.sort_unstable();
        for cell in cells {
            let run = (cell.row, cell.cohort);
            if !cell.leaving()
                && self.retired_runs.binary_search(&run).is_ok()
                && self.find(cell.row, cell.col, cell.born).is_some()
                && self
                    .retired_cells
                    .binary_search(&(cell.row, cell.col, cell.born))
                    .is_err()
            {
                self.standing_runs.push(run);
            }
        }
        self.standing_runs.sort_unstable();
        self.standing_runs.dedup();
    }

    /// Releases whose complete original cell identities and glyphs are back, as
    /// sorted `(cohort, stamp)`: `stamp` is the [`Cell::released_at`] a partial
    /// release laid on its cells, `None` for the cohort's unstamped cells (a
    /// whole release). Each partial release is judged on its own cells, so one
    /// whose text came back moved does not veto another. Two passes, no
    /// per-cell pool scan; missing samples or records and partial restores
    /// cannot authorize recovery. Whether every released cell is still
    /// resident is `Ribbon::restore_releases`'s to count. A record armed after
    /// the release (`released_at: None`) is read with the unstamped cells and
    /// is neutral unless it sits over a column a released record holds (a
    /// retype over released text, which the old light must not return under).
    pub(super) fn restored_runs(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        out: &mut Vec<(u32, Option<Instant>)>,
    ) {
        out.clear();
        self.restore_runs.clear();
        if !self.seen.iter().any(|s| s.restorable) {
            return;
        }
        for cell in cells {
            if !self
                .find(cell.row, cell.col, cell.born)
                .is_some_and(|i| self.seen[i].restorable)
            {
                continue;
            }
            self.restore_runs.push(RestoreRun {
                row: cell.row,
                cohort: cell.cohort,
                stamp: cell.released_at,
                exact: true,
                returned_ink: false,
            });
        }
        self.restore_runs
            .sort_unstable_by_key(|r| (r.cohort, r.stamp));
        self.restore_runs.dedup_by_key(|r| (r.cohort, r.stamp));
        for cell in cells {
            let Ok(i) = self
                .restore_runs
                .binary_search_by_key(&(cell.cohort, cell.released_at), |r| (r.cohort, r.stamp))
            else {
                continue;
            };
            let seen = self
                .find(cell.row, cell.col, cell.born)
                .map(|j| self.seen[j]);
            let sample = rows.iter().find(|r| r.row == cell.row);
            // A key typed inside the melt is not a veto: a record armed after
            // the release makes no claim on the run's old text, whether its
            // glyph stands or changed (the walk right after this retires it on
            // its own, `Engine::witness_rows`). Treating it as a refusal would
            // make a partial release unrestorable as soon as the next key
            // landed. A record armed over a column a released record holds
            // still refuses: that is a retype over released text.
            let over_released = seen.is_some_and(|s| !s.restorable)
                && self
                    .seen
                    .iter()
                    .any(|s| s.row == cell.row && s.col == cell.col && s.released);
            let run = &mut self.restore_runs[i];
            match (seen, sample) {
                // A part comes back with its row. A partial release was a
                // verdict on a blank (a tail a torn read cut off); once one
                // glyph it released is exactly back, the torn present is over
                // and the part is lifted whole. The walk that follows on this
                // same sample then gives the verdict the complete frame would
                // have had (same glyph: D2; different: named and shaped).
                // Requiring every glyph back would hold the part hostage to text
                // that returns moved (a torn read under a mid-line insert). A
                // never-armed cell constrains nothing.
                //
                // A column still blank is text that really went (a Backspace or
                // ⌃W), and needs no clause here: the walk right after the lift
                // names it as its own run on its own clock. A part lifted over a
                // still-incomplete frame is re-released the same frame
                // (`tests/torn_edit_gaps.rs`).
                (Some(seen), Some(sample)) if run.stamp.is_some() => {
                    let now = unit_at(sample.cols, cell.col);
                    run.exact &= seen.restorable && run.row == cell.row;
                    run.returned_ink |= seen.released && !seen.unit.is_blank() && seen.unit == now;
                }
                (Some(seen), Some(sample)) => {
                    let standing = seen.unit == unit_at(sample.cols, cell.col);
                    run.exact &= if seen.restorable {
                        (!cell.leaving() || cell.released_at.is_some())
                            && run.row == cell.row
                            && standing
                    } else {
                        !over_released
                    };
                    run.returned_ink |= seen.released && !seen.unit.is_blank();
                }
                // A cell with no record is not evidence: a never-armed cell (a
                // space, or a cell laid while the app's repaint still had the
                // column blank) makes no claim about the run's text, so it
                // neither authorizes nor refuses a restore. As a refusal it
                // would make any run with a space unrestorable, and every glyph
                // landing back would then retire one cell at a time.
                //
                // A cell whose row was not sampled still refuses: the witness
                // does not know.
                (None, Some(_)) => {}
                _ => run.exact = false,
            }
        }
        out.extend(
            self.restore_runs
                .iter()
                .filter(|r| r.exact && r.returned_ink)
                .map(|r| (r.cohort, r.stamp)),
        );
    }

    /// Clear release custody only after the ribbon accepted restoration.
    /// Already-counted identities stay counted if cleared again. A cell still
    /// leaving in an accepted cohort belongs to another partial release the
    /// ribbon did not lift, and keeps its custody record.
    pub(super) fn restored(&mut self, cohorts: &[u32], cells: &[Cell]) {
        if cohorts.is_empty() {
            return;
        }
        for cell in cells
            .iter()
            .filter(|c| !c.leaving() && cohorts.binary_search(&c.cohort).is_ok())
        {
            if let Some(i) = self.find(cell.row, cell.col, cell.born) {
                // A held blank goes back to never-armed. The release gave the
                // run's spaces released blank records (`hold_released`);
                // un-releasing would leave a blank record no walk can name
                // (blank over blank is D2), so a later whole-line clear would
                // read as partial in `Ribbon::release_cells`. The record goes.
                if self.seen[i].unit.is_blank() {
                    self.seen.remove(i);
                } else {
                    self.seen[i].released = false;
                    self.seen[i].restorable = false;
                }
            }
        }
    }

    /// The moved-text search: true when a run's blanked glyphs (sorted by
    /// column, `run`) stand in the same order and spacing elsewhere on the
    /// sampled rows, on another row at any offset or on their own row at
    /// another offset. Never-armed cells are not in `run` and constrain nothing
    /// (so `hello world` re-laid two rows down matches). The original place is
    /// excluded. `O(rows × cols × glyphs)` per event; allocates nothing.
    ///
    /// One glyph is not a word: a run of one blanked glyph is never ruled moved,
    /// since a single letter stands on almost any row by coincidence; it is
    /// released instead. Glyphs are counted, not cells: a wide glyph's
    /// continuation half does not count.
    fn moved(run: &[Blanked], rows: &[RowSample<'_>]) -> bool {
        if run.iter().filter(|b| !b.unit.cont).count() < 2 {
            return false;
        }
        let Some(first) = run.first() else {
            return false;
        };
        let (row, c0) = (first.row, usize::from(first.col));
        rows.iter().any(|s| {
            (0..s.cols.len()).any(|d| {
                if s.row == row && d == c0 {
                    return false;
                }
                run.iter().all(|b| {
                    let at = usize::from(b.col) - c0 + d;
                    u16::try_from(at).is_ok_and(|col| unit_at(s.cols, col) == b.unit)
                })
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::ribbon::Layer;
    use super::*;

    fn cell(row: u16, col: u16, born: Instant) -> Cell {
        cell_of(row, col, born, 0)
    }

    fn cell_of(row: u16, col: u16, born: Instant, cohort: u32) -> Cell {
        Cell {
            row,
            col,
            cohort,
            t: 0.0,
            born,
            attack_at: born,
            life_s: 1.0,
            cov0: 1.0,
            typing: true,
            retract_at: None,
            retire_at: None,
            birth_disp: 0.5,
            edge_cells: 3.0,
            layer: Layer::Base,
            rearm: None,
            released_at: None,
        }
    }

    fn row(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    /// The positions a walk named, in the order it named them.
    fn pos(out: &[(u16, u16, Instant)]) -> Vec<(u16, u16)> {
        out.iter().map(|&(r, c, _)| (r, c)).collect()
    }

    #[test]
    fn a_unit_is_the_lead_glyph_for_either_half_of_a_wide_one() {
        let cols = row("a\u{4f60}\0 ");
        assert_eq!(
            unit_at(&cols, 0),
            Unit {
                ch: 'a',
                wide: false,
                cont: false
            }
        );
        assert_eq!(
            unit_at(&cols, 1),
            Unit {
                ch: '\u{4f60}',
                wide: true,
                cont: false
            }
        );
        assert_eq!(
            unit_at(&cols, 2),
            Unit {
                ch: '\u{4f60}',
                wide: true,
                cont: true
            }
        );
        assert!(unit_at(&cols, 3).is_blank(), "a space is blank");
        assert!(unit_at(&cols, 9).is_blank(), "past the sample is blank");
        assert!(
            unit_at(&row("\0x"), 0).is_blank(),
            "a stray continuation is blank"
        );
    }

    /// Six cells of one run on row 5, armed over `abcdef` with row 4
    /// sampled holding `above` at the arming walk.
    fn armed_run(above: &str) -> (Witness, Vec<Cell>) {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let cells: Vec<Cell> = (0..6).map(|c| cell_of(5, c, t0, 7)).collect();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("abcdef"),
                },
                RowSample {
                    row: 4,
                    cols: &row(above),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty());
        assert_eq!(w.len(), 6, "every glyph armed");
        (w, cells)
    }

    /// The follow pass's verdict once row 5 is blank and row 4 reads `now`.
    fn follow_onto(w: &mut Witness, cells: &[Cell], now: &str) -> Vec<FollowRun> {
        follow_onto_with(w, cells, "      ", now)
    }

    /// The follow pass's verdict once row 5 reads `own` and row 4 reads
    /// `now`.
    fn follow_onto_with(w: &mut Witness, cells: &[Cell], own: &str, now: &str) -> Vec<FollowRun> {
        let mut out = Vec::new();
        w.follow_runs(
            cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row(own),
                },
                RowSample {
                    row: 4,
                    cols: &row(now),
                },
            ],
            &mut out,
        );
        out
    }

    /// Row 5's run followed one row up, over `lo..=hi`.
    fn up(lo: u16, hi: u16) -> FollowRun {
        FollowRun {
            row: 5,
            cohort: 7,
            lo,
            hi,
            dr: -1,
        }
    }

    #[test]
    fn a_short_park_cannot_relax_an_unrelated_run() {
        let sample = [
            RowSample {
                row: 5,
                cols: &row("bcdef       "),
            },
            RowSample {
                row: 4,
                cols: &row("a           "),
            },
        ];
        let (mut ordinary, cells) = armed_run("            ");
        let mut out = Vec::new();
        ordinary.follow_runs(&cells, &sample, &mut out);
        assert!(out.is_empty(), "one arrival cannot move an ordinary run");

        let (mut unrelated, cells) = armed_run("            ");
        out.clear();
        unrelated.follow_runs_with_short_park(&cells, &sample, Some(((5, 12), (5, 11))), &mut out);
        assert!(
            out.is_empty(),
            "an unrelated park at col 12 must not relax the run at cols 0..5: {out:?}"
        );

        let (mut owned, cells) = armed_run("            ");
        out.clear();
        owned.follow_runs_with_short_park(&cells, &sample, Some(((5, 6), (5, 5))), &mut out);
        assert_eq!(out, vec![up(0, 0)], "the exact run keeps the narrow wrap");
    }

    /// A twin is neutral ([`Witness::follow_runs`]): a glyph that already stood
    /// a row up when its record was armed neither counts as found nor breaks
    /// the block. `  c   ` a row up at arm time and the whole run arriving there
    /// is one block that moved; so are twins at both ends and a block of four
    /// twins between two arrivals.
    #[test]
    fn a_twin_inside_the_block_is_neutral_and_the_block_follows() {
        for above in ["  c   ", "a    f", " bcde "] {
            let (mut w, cells) = armed_run(above);
            assert_eq!(
                follow_onto(&mut w, &cells, "abcdef"),
                vec![up(0, 5)],
                "armed under `{above}`: the whole run arrived a row up and follows as one block"
            );
        }
    }

    /// But a twin whose glyph is not at the target is a hole: `ab def` and
    /// `ab  ef` a row up are letters either side of a hole, not a move.
    #[test]
    fn a_twin_whose_glyph_is_not_at_the_target_is_a_hole() {
        for now in ["ab def", "ab  ef"] {
            let (mut w, cells) = armed_run("  c   ");
            assert_eq!(
                follow_onto(&mut w, &cells, now),
                vec![],
                "`{now}` a row up: a hole in the middle is not a block that moved"
            );
        }
    }

    /// A missed follow is counted ([`FollowScore::missed`],
    /// `ribbon_follow_missed=`): row 5's six glyphs gone and `abc  f` a row up
    /// arrive four glyphs but not as one block, so nothing is named and six
    /// cells count as missed. A named run misses nothing, and neither does a
    /// run whose text arrived nowhere (an erase under an identical line, a
    /// kill under a line sharing a prefix, other text a row up).
    #[test]
    fn a_run_whose_text_arrived_but_not_as_one_block_is_counted_missed() {
        let score = |above: &str, own: &str, now: &str| {
            let (mut w, cells) = armed_run(above);
            let mut out = Vec::new();
            let score = w.follow_runs(
                &cells,
                &[
                    RowSample {
                        row: 5,
                        cols: &row(own),
                    },
                    RowSample {
                        row: 4,
                        cols: &row(now),
                    },
                ],
                &mut out,
            );
            (out.len(), score.missed)
        };
        let blank = "      ";
        for (above, own, now, want, what) in [
            (blank, blank, "abc  f", (0, 6), "arrived, not a block"),
            (blank, blank, "abcdef", (1, 0), "named: nothing missed"),
            ("  c   ", blank, "abcdef", (1, 0), "the twin is neutral"),
            ("abcdef", blank, "abcdef", (0, 0), "an erase is not a move"),
            ("abc   ", blank, "abcxyz", (0, 0), "a prefix twin and holes"),
            (blank, blank, "uvwxyz", (0, 0), "other text arrived nothing"),
            (blank, "abcdef", "abcdef", (0, 0), "nothing left its row"),
        ] {
            assert_eq!(score(above, own, now), want, "{what}");
        }
    }

    /// An edge twin rides with its block, gone or standing. A neutral record
    /// flush against the found block (a lead directly before its first found
    /// record, a trail directly after its last) rides whether or not its own
    /// glyph still reads as standing on the run's row: in a bottom-anchored
    /// composer a kept line's first cell can read as standing because the new
    /// line starts with the same letter, and leaving it behind lets the old
    /// run capture the new line's keys (`tests/composer_growth_follows.rs`).
    /// Only a flush edge rides; a neutral cut off by a hole stays.
    #[test]
    fn an_edge_twin_rides_with_its_block_whether_or_not_its_own_glyph_is_gone() {
        for above in ["a     ", "     f"] {
            for own in [above, "      "] {
                let (mut w, cells) = armed_run(above);
                assert_eq!(
                    follow_onto_with(&mut w, &cells, own, "abcdef"),
                    vec![up(0, 5)],
                    "armed under `{above}`, its row reading `{own}`: the edge twin rides"
                );
            }
        }
        // …only flush: a hole between the twin and the block leaves it.
        for (above, now, want) in [
            ("a     ", "a cdef", up(2, 5)),
            ("     f", "abcd f", up(0, 3)),
        ] {
            for own in [above, "      "] {
                let (mut w, cells) = armed_run(above);
                assert_eq!(
                    follow_onto_with(&mut w, &cells, own, now),
                    vec![want],
                    "armed under `{above}`, `{now}` a row up: a twin past a hole is not the block's"
                );
            }
        }
    }

    /// Six-plus cells of one run on row 5, one per column of `text` (its
    /// blanks laid and never armed, as a typed Space is), armed with row 4
    /// sampled holding `above`.
    fn armed_line_under(text: &str, above: &str) -> (Witness, Vec<Cell>) {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let n = u16::try_from(text.chars().count()).expect("a short line");
        let cells: Vec<Cell> = (0..n).map(|c| cell_of(5, c, t0, 7)).collect();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row(text),
                },
                RowSample {
                    row: 4,
                    cols: &row(above),
                },
            ],
            &[],
            t0,
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty());
        (w, cells)
    }

    /// The follow pass's names and its missed count once row 5 reads `own`
    /// and row 4 reads `now`.
    fn follow_line(w: &mut Witness, cells: &[Cell], own: &str, now: &str) -> (Vec<FollowRun>, u32) {
        let mut out = Vec::new();
        let score = w.follow_runs(
            cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row(own),
                },
                RowSample {
                    row: 4,
                    cols: &row(now),
                },
            ],
            &mut out,
        );
        (out, score.missed)
    }

    /// The word the wrap relaid is not a hole in the line. A bottom-anchored
    /// composer's wrap carries the line up minus its last word, laid again at
    /// the start of the caret row (`  this confirmatio` → `  this` a row up and
    /// `  confirmation` on its own row). A trailing suffix standing in order and
    /// spacing at the start of the run's own row leaves the half-gate's
    /// denominator, so the four arrivals of `this` follow.
    ///
    /// Controls: a rewrite of the row and the same word not leading its row
    /// relay nothing, so the four arrivals are refused; and the relaid word
    /// does not excuse a hole in the kept line (`t is` a row up is counted
    /// missed).
    #[test]
    fn a_word_the_wrap_relaid_at_its_row_s_start_does_not_count_against_the_line() {
        let line = "  this confirmatio";
        let blank = "";
        // `lo` 0, not 2: the line's two leading cells are typed Spaces of the
        // run, never armed, with no record left behind on that side, so they go
        // up with the line (the extent's never-armed growth).
        let run = FollowRun {
            row: 5,
            cohort: 7,
            lo: 0,
            hi: 5,
            dr: -1,
        };
        let (mut w, cells) = armed_line_under(line, blank);
        assert_eq!(
            follow_line(&mut w, &cells, "  confirmation", "  this"),
            (vec![run], 0),
            "`this` arrived a row up and `confirmatio` leads the caret row: the line follows"
        );
        for (own, now, want, what) in [
            (
                "  xyzwvutsrqpon",
                "  this",
                (vec![], 0),
                "a rewrite relays nothing",
            ),
            (
                "  xyz confirmatio",
                "  this",
                (vec![], 0),
                "a word not leading its row",
            ),
            (
                "  confirmation",
                "  t is",
                (vec![], 14),
                "a hole in the kept line",
            ),
        ] {
            let (mut w, cells) = armed_line_under(line, blank);
            assert_eq!(follow_line(&mut w, &cells, own, now), want, "{what}");
        }
    }

    /// The one-block law ([`Witness::follow_runs`]): matched records must be
    /// one block, a prefix or suffix of the run. `abc  f` a row up keeps four
    /// of six letters but the `f` past the hole marks coincidence: nothing is
    /// named. Controls: the prefix `abcd` and the whole run are named with
    /// their extents.
    #[test]
    fn the_follow_pass_names_only_a_block_never_letters_around_a_hole() {
        let (mut w, cells) = armed_run("      ");
        for holed in ["ab  ef", "abc  f", "a cdef"] {
            assert_eq!(
                follow_onto(&mut w, &cells, holed),
                vec![],
                "`{holed}`: a hole in the middle is not a block that moved"
            );
        }
        let run = |lo, hi| FollowRun {
            row: 5,
            cohort: 7,
            lo,
            hi,
            dr: -1,
        };
        assert_eq!(follow_onto(&mut w, &cells, "abcd  "), vec![run(0, 3)]);
        assert_eq!(follow_onto(&mut w, &cells, "  cdef"), vec![run(2, 5)]);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![run(0, 5)]);
    }

    /// An erase is not a move: the run armed while the row above already held
    /// `abcdef` (a command typed again below itself), then its row blanked
    /// (Ctrl-U). The block standing a row up never arrived, so nothing is
    /// named. Control: armed over a blank row above, the same final screen is
    /// a move.
    ///
    /// With a twin on part of the run (`abc` armed under `abc`), the arrived
    /// block `def` is found and the twin prefix rides with it (`0..=5`); the
    /// all-twin run still names nothing, and twins ride only flush against the
    /// block
    /// ([`an_edge_twin_rides_with_its_block_whether_or_not_its_own_glyph_is_gone`]).
    #[test]
    fn a_block_already_standing_beside_the_run_when_it_was_armed_is_not_followed() {
        let (mut w, cells) = armed_run("abcdef");
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
        let (mut w, cells) = armed_run("      ");
        assert_eq!(follow_onto(&mut w, &cells, "abcdef").len(), 1);
        // A twin on part of the run: the arrived glyphs are the evidence, and
        // the bordering twins go with the block, so the whole run is carried.
        let (mut w, cells) = armed_run("abc   ");
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            vec![up(0, 5)],
            "the arrived block is the evidence; the twins bordering it ride along"
        );
    }

    /// A twin inside an arrived block is neutral, not a hole: the run was armed
    /// under a line holding `c` at the same column; that record did not arrive
    /// but stands where the moved text puts it, and the run is carried whole
    /// (owner, 2026-09-24).
    /// Negative control: the `$ cd ..` erase, every record a twin, names
    /// nothing.
    #[test]
    fn a_twin_inside_an_arrived_block_is_neutral_not_a_hole() {
        let (mut w, cells) = armed_run("  c   ");
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            vec![FollowRun {
                row: 5,
                cohort: 7,
                lo: 0,
                hi: 5,
                dr: -1,
            }]
        );
        let (mut w, cells) = armed_run("abcdef");
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
    }

    /// The half is of the records that can testify: a run armed under a line it
    /// mostly repeats (`abcd` above) moved when its two differing letters
    /// arrive a row up (two of the two non-twin records), and is carried whole
    /// (`tests/composer_multiline_wrap.rs`). Controls: the row above unchanged
    /// (an erase under a near-identical line) and only one letter arriving;
    /// nothing is named.
    #[test]
    fn a_run_under_a_near_copy_of_itself_follows_on_the_letters_that_arrived() {
        let (mut w, cells) = armed_run("abcd  ");
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            vec![FollowRun {
                row: 5,
                cohort: 7,
                lo: 0,
                hi: 5,
                dr: -1,
            }]
        );
        let (mut w, cells) = armed_run("abcdxy");
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdxy"),
            vec![],
            "the row above never changed: nothing arrived"
        );
        let (mut w, cells) = armed_run("abcd  ");
        assert_eq!(
            follow_onto(&mut w, &cells, "abcde "),
            vec![],
            "one arrived letter is too little evidence"
        );
    }

    /// Six cells of one run on `row`, born at `t0`, armed over `abcdef` on
    /// the samples `rows` alone, at `t0`.
    fn armed_on(row: u16, rows: &[RowSample<'_>]) -> (Witness, Vec<Cell>) {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let cells: Vec<Cell> = (0..6).map(|c| cell_of(row, c, t0, 7)).collect();
        walk_quiet(&mut w, &cells, rows);
        assert_eq!(w.len(), 6, "every glyph armed");
        (w, cells)
    }

    /// One walk that names nothing — the run's glyphs stand — so the only
    /// thing it does is take this frame's evidence, at the cells' birth.
    fn walk_quiet(w: &mut Witness, cells: &[Cell], rows: &[RowSample<'_>]) {
        walk_quiet_at(w, cells, rows, 0);
    }

    /// [`walk_quiet`] `ms` after the cells' birth.
    fn walk_quiet_at(w: &mut Witness, cells: &[Cell], rows: &[RowSample<'_>], ms: u64) {
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let now = cells[0].born + Duration::from_millis(ms);
        w.walk(cells, rows, &[], now, &mut out, &mut rel);
        assert!(out.is_empty() && rel.is_empty(), "the glyphs stand");
    }

    /// Every record's evidence, `(clear, twin, pending)`, in column order.
    fn evidence(w: &Witness) -> Vec<(u8, u8, u8)> {
        w.seen
            .iter()
            .map(|s| (s.clear, s.twin, s.pending))
            .collect()
    }

    /// The run on row 5, cohort 7, carried whole one row up.
    fn whole_up() -> Vec<FollowRun> {
        vec![FollowRun {
            row: 5,
            cohort: 7,
            lo: 0,
            hi: 5,
            dr: -1,
        }]
    }

    /// What the witness did not see is no evidence either way. The run is armed
    /// while row 4 (inside the grid) is not sampled; when row 5 is blanked and
    /// row 4 reads `abcdef`, the run is carried there, as a move to an unseen
    /// destination (a paste then a wrap, a band moved twice) must be. The
    /// host's safeguard is the control: the arming row's `±1` is sampled on the
    /// arming frame (`Engine::ribbon_rows_for`), so a copy seen then is a twin
    /// and the same erase names nothing (`tests/erased_under_its_twin.rs`).
    #[test]
    fn an_unsampled_neighbour_is_no_evidence_either_way() {
        let own = row("abcdef");
        let (mut w, cells) = armed_on(5, &[RowSample { row: 5, cols: &own }]);
        assert_eq!(
            evidence(&w),
            vec![(0b1111, 0, 0); 6],
            "nothing seen: every offset clear, no twin"
        );
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            whole_up(),
            "row 4 was never seen: the glyphs found there may have arrived"
        );
        let copy = row("abcdef");
        let (mut w, cells) = armed_on(
            5,
            &[
                RowSample { row: 5, cols: &own },
                RowSample {
                    row: 4,
                    cols: &copy,
                },
            ],
        );
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            vec![],
            "row 4 held the line when it was armed: its erase is no move"
        );
    }

    /// The evidence offset by offset ([`Seen::observe`]; bit `k` is
    /// `FOLLOW_DRS[k]` = `−1, +1, −2, +2`). On the arming frame a sampled row
    /// holding the glyph at its column is a twin at once; every other offset,
    /// sampled or not, in the grid or past it, is clear. Arrivals
    /// ([`Seen::arrivals`]) are clear and not twin. A twin testifies only about
    /// its own row: the row two away past it may still arrive.
    #[test]
    fn the_evidence_is_what_each_follow_row_was_seen_holding() {
        let own = row("abcdef");
        let blank = row("      ");
        let twin = row("abcdef");
        // −1 seen blank, +1 a twin, −2 and +2 not sampled.
        let (w, _) = armed_on(
            5,
            &[
                RowSample { row: 5, cols: &own },
                RowSample {
                    row: 4,
                    cols: &blank,
                },
                RowSample {
                    row: 6,
                    cols: &twin,
                },
            ],
        );
        assert_eq!(evidence(&w), vec![(0b1111, 0b0010, 0); 6]);
        assert!(
            w.seen.iter().all(|s| s.arrivals() == 0b1101),
            "−1, −2 and +2 may arrive; +1 is a twin"
        );
        // Every follow row seen, one of them holding the run shifted by a
        // column (not a twin at any column): all four clear.
        let (w, _) = armed_on(
            2,
            &[
                RowSample { row: 2, cols: &own },
                RowSample {
                    row: 0,
                    cols: &blank,
                },
                RowSample {
                    row: 1,
                    cols: &blank,
                },
                RowSample {
                    row: 3,
                    cols: &row("bcdefa"),
                },
                RowSample {
                    row: 4,
                    cols: &blank,
                },
            ],
        );
        assert_eq!(
            evidence(&w),
            vec![(0b1111, 0, 0); 6],
            "all four seen, no twin"
        );
        // Row 0: nothing above it, nothing sampled below it.
        let (w, _) = armed_on(0, &[RowSample { row: 0, cols: &own }]);
        assert_eq!(evidence(&w), vec![(0b1111, 0, 0); 6]);
    }

    /// A copy that appears beside a standing line is a twin once it has stood
    /// there [`TWIN_MIN`] (fzf's list, a completion popup, a copy drawn above).
    /// The run is armed with row 4 blank, then `abcdef` is drawn on row 4. Seen
    /// on every sampled frame for 40 ms, every record is a twin and erasing row
    /// 5 names nothing; less is a torn repaint and the move is named. The unit
    /// is time: two frames 16 ms apart are a tear, two frames 120 ms apart a
    /// copy.
    #[test]
    fn a_copy_that_appears_beside_a_standing_line_is_a_twin_once_it_has_stood_forty_ms() {
        let own = row("abcdef");
        let blank = row("      ");
        let copy = row("abcdef");
        let other = row("xyzxyz");
        let frame = |w: &mut Witness, cells: &[Cell], four: &[char], ms: u64| {
            walk_quiet_at(
                w,
                cells,
                &[
                    RowSample { row: 5, cols: &own },
                    RowSample { row: 4, cols: four },
                ],
                ms,
            );
        };
        let armed = || {
            armed_on(
                5,
                &[
                    RowSample { row: 5, cols: &own },
                    RowSample {
                        row: 4,
                        cols: &blank,
                    },
                ],
            )
        };
        // Three 60 Hz frames of the copy — 32 ms — then the old row erased:
        // the torn repaint, a move.
        let (mut w, cells) = armed();
        for ms in [16, 32, 48] {
            frame(&mut w, &cells, &copy, ms);
        }
        assert_eq!(evidence(&w), vec![(0b1111, 0, 0b0001); 6]);
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            whole_up(),
            "a copy that stood 32 ms: the new row of a torn repaint"
        );
        // A fourth frame, 48 ms after the first: a twin.
        let (mut w, cells) = armed();
        for ms in [16, 32, 48, 64] {
            frame(&mut w, &cells, &copy, ms);
        }
        assert_eq!(evidence(&w), vec![(0b1111, 0b0001, 0b0001); 6]);
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            vec![],
            "the copy stood beside the line: its erase is not a move"
        );
        // Two frames 120 ms apart: a twin.
        let (mut w, cells) = armed();
        frame(&mut w, &cells, &copy, 120);
        frame(&mut w, &cells, &copy, 240);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
        // Copy, something else, copy: the stand starts over.
        let (mut w, cells) = armed();
        frame(&mut w, &cells, &copy, 16);
        frame(&mut w, &cells, &other, 48);
        frame(&mut w, &cells, &copy, 80);
        assert_eq!(evidence(&w), vec![(0b1111, 0, 0b0001); 6]);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), whole_up());
        // A frame on which row 4 is not sampled does not break the stand:
        // the copy stood on both frames the witness saw.
        let (mut w, cells) = armed();
        frame(&mut w, &cells, &copy, 16);
        walk_quiet_at(&mut w, &cells, &[RowSample { row: 5, cols: &own }], 32);
        frame(&mut w, &cells, &copy, 64);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
        // A twin is sticky: the copy gone again does not undo it.
        let (mut w, cells) = armed();
        frame(&mut w, &cells, &copy, 16);
        frame(&mut w, &cells, &copy, 64);
        frame(&mut w, &cells, &blank, 80);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
    }

    /// A carried record starts over on its new row as if just armed
    /// ([`Witness::translate_cells`]). Armed on row 5 under a copy on row 4, the
    /// text moves down to row 6 and is carried (the copy one row up is
    /// neutral); its evidence is forgotten. When the text goes back to row 5 on
    /// the next frame with the copy still on row 4, row 5 is a row the record
    /// was never seen beside, so the band follows. It also learns its new row:
    /// a copy that stood on row 7 for [`TWIN_MIN`] makes the next erase no
    /// move.
    #[test]
    fn a_carried_record_starts_over_on_its_new_row_as_if_just_armed() {
        let text = row("abcdef");
        let blank = row("      ");
        let dests = |out: &[FollowRun]| out.iter().map(|f| (f.row, f.dr)).collect::<Vec<_>>();
        let carried_down = || {
            let (mut w, cells) = armed_run("abcdef");
            assert_eq!(
                evidence(&w),
                vec![(0b1111, 0b0001, 0); 6],
                "fixture: a twin one row up"
            );
            let mut out = Vec::new();
            w.follow_runs(
                &cells,
                &[
                    RowSample {
                        row: 5,
                        cols: &blank,
                    },
                    RowSample {
                        row: 4,
                        cols: &text,
                    },
                    RowSample {
                        row: 6,
                        cols: &text,
                    },
                ],
                &mut out,
            );
            assert_eq!(
                dests(&out),
                vec![(5, 1)],
                "fixture: the run follows its text down, not onto its twin"
            );
            let mut moved: Vec<(u16, u16, Instant)> =
                cells.iter().map(|c| (c.row, c.col, c.born)).collect();
            w.translate_cells(&mut moved, 1);
            let carried: Vec<Cell> = cells
                .iter()
                .map(|c| cell_of(6, c.col, c.born, c.cohort))
                .collect();
            assert_eq!(
                evidence(&w),
                vec![(0b1111, 0, 0); 6],
                "carried: as if just armed"
            );
            (w, carried)
        };
        // Back up on the next frame, the copy still on row 4: it follows.
        let (mut w, carried) = carried_down();
        let mut out = Vec::new();
        w.follow_runs(
            &carried,
            &[
                RowSample {
                    row: 6,
                    cols: &blank,
                },
                RowSample {
                    row: 5,
                    cols: &text,
                },
                RowSample {
                    row: 4,
                    cols: &text,
                },
            ],
            &mut out,
        );
        assert_eq!(dests(&out), vec![(6, -1)], "back up one row");
        // A copy that stood on row 7 for 48 ms: its erase is not a move.
        let (mut w, carried) = carried_down();
        for ms in [16, 64] {
            walk_quiet_at(
                &mut w,
                &carried,
                &[
                    RowSample {
                        row: 6,
                        cols: &text,
                    },
                    RowSample {
                        row: 7,
                        cols: &text,
                    },
                ],
                ms,
            );
        }
        let mut out = Vec::new();
        w.follow_runs(
            &carried,
            &[
                RowSample {
                    row: 6,
                    cols: &blank,
                },
                RowSample {
                    row: 7,
                    cols: &text,
                },
            ],
            &mut out,
        );
        assert_eq!(dests(&out), vec![], "erased beside a twin it learned");
    }

    /// A scroll keeps the evidence; a band move keeps only what moved with the
    /// record ([`Witness::translate`], [`Witness::translate_band`]). A scroll
    /// moves every row, so the bits ride along (an inline chat box pushed by a
    /// streamed row keeps its band, `tests/moved_again_without_a_key.rs`). A
    /// record inside a moved band forgets offsets reaching across its edge, and
    /// one outside forgets offsets reaching in (vim's `yyp`).
    #[test]
    fn a_scroll_keeps_the_evidence_and_a_band_move_keeps_what_moved_with_the_record() {
        let own = row("abcdef");
        let blank = row("      ");
        let everything = |row_: u16| {
            let mut w = Witness::new();
            let t0 = Instant::now();
            let cells: Vec<Cell> = (0..6).map(|c| cell_of(row_, c, t0, 7)).collect();
            walk_quiet(
                &mut w,
                &cells,
                &[
                    RowSample {
                        row: row_,
                        cols: &own,
                    },
                    RowSample {
                        row: row_ - 2,
                        cols: &blank,
                    },
                    RowSample {
                        row: row_ - 1,
                        cols: &blank,
                    },
                    RowSample {
                        row: row_ + 1,
                        cols: &blank,
                    },
                    RowSample {
                        row: row_ + 2,
                        cols: &blank,
                    },
                ],
            );
            assert_eq!(evidence(&w), vec![(0b1111, 0, 0); 6]);
            w
        };
        let mut w = everything(10);
        w.translate(3, 0);
        assert!(w.seen.iter().all(|s| s.row == 7));
        assert_eq!(
            evidence(&w),
            vec![(0b1111, 0, 0); 6],
            "a scroll keeps it all"
        );
        // The band 10..=20 moves down one: the record on row 10 moves to 11
        // with rows 11 and 12 (its +1, +2), while rows 9 and 8 stay put.
        let mut w = everything(10);
        w.translate_band(10, 20, 1);
        assert!(w.seen.iter().all(|s| s.row == 11));
        assert_eq!(
            evidence(&w),
            vec![(0b1010, 0, 0); 6],
            "+1 and +2 moved with it; −1 and −2 are a new line and a row that stood"
        );
        // The band 11..=20 moves down one — vim's `yyp` under row 10: the
        // record on row 10 stands, its +1 and +2 do not.
        let mut w = everything(10);
        w.translate_band(11, 20, 1);
        assert!(w.seen.iter().all(|s| s.row == 10));
        assert_eq!(
            evidence(&w),
            vec![(0b0101, 0, 0); 6],
            "−1 and −2 stood with it"
        );
    }

    /// A band move that opens a row beside a line leaves that row unseen until
    /// a frame shows it ([`Witness::translate_band`]). vim's `yyp` on a line on
    /// row 10: band 11..=20 moves down one and the copy is written into the
    /// opened row 11 in the same batch. The record's `+1` and `+2` are not
    /// clear, so the copy is no arrival when `S` clears the original. A frame
    /// showing row 11 without the glyph clears it again, and a later move there
    /// follows.
    #[test]
    fn a_band_move_that_opens_a_row_beside_a_line_leaves_it_unseen_until_a_frame_shows_it() {
        let own = row("abcdef");
        let blank = row("      ");
        let t0 = Instant::now();
        let cells: Vec<Cell> = (0..6).map(|c| cell_of(10, c, t0, 7)).collect();
        let armed = || {
            let mut w = Witness::new();
            walk_quiet(
                &mut w,
                &cells,
                &[
                    RowSample {
                        row: 10,
                        cols: &own,
                    },
                    RowSample {
                        row: 11,
                        cols: &blank,
                    },
                ],
            );
            w.translate_band(11, 20, 1);
            w
        };
        let erased_onto_eleven = |w: &mut Witness| {
            let mut out = Vec::new();
            w.follow_runs(
                &cells,
                &[
                    RowSample {
                        row: 10,
                        cols: &blank,
                    },
                    RowSample {
                        row: 11,
                        cols: &own,
                    },
                ],
                &mut out,
            );
            out.iter().map(|f| f.dr).collect::<Vec<_>>()
        };
        let mut w = armed();
        assert_eq!(evidence(&w), vec![(0b0101, 0, 0); 6]);
        assert_eq!(
            erased_onto_eleven(&mut w),
            Vec::<i16>::new(),
            "`S` after `yyp`"
        );
        let mut w = armed();
        walk_quiet_at(
            &mut w,
            &cells,
            &[
                RowSample {
                    row: 10,
                    cols: &own,
                },
                RowSample {
                    row: 11,
                    cols: &row("}     "),
                },
            ],
            16,
        );
        assert_eq!(
            erased_onto_eleven(&mut w),
            vec![1],
            "seen without it: a move"
        );
    }

    /// A row a scroll brings in dates its copy from the last frame before the
    /// scroll ([`Witness::translate`], [`Witness::last_walk`]). A line on the
    /// grid's last row (23 of 24) is scrolled to row 22, bringing row 23 in;
    /// its [`Seen::pending`] clock starts at the arming walk (0 ms). A copy
    /// there on a look 48 ms later has stood [`TWIN_MIN`]: a twin, and the
    /// original's erase names nothing (`tests/copies_are_not_moves.rs`). On a
    /// look 16 ms later it has not: a torn relocation, which follows
    /// (`tests/moves_are_followed.rs`). Row 23 blank on first look, or never
    /// looked at, is clear.
    #[test]
    fn a_row_a_scroll_brings_in_dates_its_copy_from_the_last_frame_before_the_scroll() {
        let own = row("abcdef");
        let blank = row("      ");
        let t0 = Instant::now();
        let at = |r: u16| -> Vec<Cell> { (0..6).map(|c| cell_of(r, c, t0, 7)).collect() };
        let scrolled = || {
            let mut w = Witness::new();
            walk_quiet(
                &mut w,
                &at(23),
                &[RowSample {
                    row: 23,
                    cols: &own,
                }],
            );
            w.translate(1, 24);
            // `+1` reaches row 23, the row the scroll brought in; `+2`
            // reaches row 24, past the grid, which no scroll brings in.
            assert!(
                w.seen
                    .iter()
                    .all(|s| s.row == 22 && s.pending == 0b0010 && s.pend_ms == [0; 4])
            );
            w
        };
        let erased = |w: &mut Witness| {
            let mut out = Vec::new();
            w.follow_runs(
                &at(22),
                &[
                    RowSample {
                        row: 22,
                        cols: &blank,
                    },
                    RowSample {
                        row: 23,
                        cols: &own,
                    },
                ],
                &mut out,
            );
            out.iter().map(|f| f.dr).collect::<Vec<_>>()
        };
        let look = |w: &mut Witness, twenty_three: &[char], ms: u64| {
            walk_quiet_at(
                w,
                &at(22),
                &[
                    RowSample {
                        row: 22,
                        cols: &own,
                    },
                    RowSample {
                        row: 23,
                        cols: twenty_three,
                    },
                ],
                ms,
            );
        };
        let mut w = scrolled();
        look(&mut w, &own, 48);
        assert_eq!(
            erased(&mut w),
            Vec::<i16>::new(),
            "the copy came in with the row, 48 ms ago at the latest"
        );
        let mut w = scrolled();
        look(&mut w, &own, 16);
        assert_eq!(erased(&mut w), vec![1], "16 ms: the line relocated, torn");
        let mut w = scrolled();
        look(&mut w, &own, 16);
        look(&mut w, &own, 48);
        assert_eq!(erased(&mut w), Vec::<i16>::new(), "seen again at 48 ms");
        let mut w = scrolled();
        look(&mut w, &blank, 48);
        assert_eq!(erased(&mut w), vec![1], "seen blank first: a move");
        let mut w = scrolled();
        assert_eq!(erased(&mut w), vec![1], "never looked at: a move");
        // An unmeasured grid marks nothing.
        let mut w = Witness::new();
        walk_quiet(
            &mut w,
            &at(23),
            &[RowSample {
                row: 23,
                cols: &own,
            }],
        );
        w.translate(1, 0);
        assert!(w.seen.iter().all(|s| s.pending == 0));
    }

    /// A run beside a withheld row waits one walk
    /// ([`Witness::find_deferred_runs`]). Row 5 reads other text on a walk
    /// where row 4 was asked for and not delivered: no verdict, records kept.
    /// Next walk row 4 is delivered and the follow pass carries the run. A
    /// second withheld walk judges it. With nothing withheld the verdict is
    /// immediate (`tests/moved_again_without_a_key.rs`).
    #[test]
    fn a_run_beside_a_withheld_row_waits_one_walk() {
        let own = row("abcdef");
        let other = row("uvwxyz");
        let blank = row("      ");
        let t0 = Instant::now();
        let cells: Vec<Cell> = (0..6).map(|c| cell_of(5, c, t0, 7)).collect();
        let armed = || {
            let mut w = Witness::new();
            walk_quiet(
                &mut w,
                &cells,
                &[
                    RowSample { row: 5, cols: &own },
                    RowSample {
                        row: 4,
                        cols: &blank,
                    },
                ],
            );
            w
        };
        let replaced = |w: &mut Witness, withheld: &[u16]| {
            let (mut out, mut rel) = (Vec::new(), Vec::new());
            w.walk(
                &cells,
                &[RowSample {
                    row: 5,
                    cols: &other,
                }],
                withheld,
                t0 + Duration::from_millis(16),
                &mut out,
                &mut rel,
            );
            out.len()
        };
        let mut w = armed();
        assert_eq!(replaced(&mut w, &[4]), 0, "row 4 withheld: no verdict");
        assert_eq!(w.len(), 6, "the records are kept");
        let mut out = Vec::new();
        w.follow_runs(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &other,
                },
                RowSample { row: 4, cols: &own },
            ],
            &mut out,
        );
        assert_eq!(out.iter().map(|f| f.dr).collect::<Vec<_>>(), vec![-1]);
        let mut w = armed();
        assert_eq!(replaced(&mut w, &[4]), 0);
        assert_eq!(replaced(&mut w, &[4]), 6, "withheld again: judged");
        let mut w = armed();
        assert_eq!(replaced(&mut w, &[]), 6, "nothing withheld: judged at once");
        let mut w = armed();
        assert_eq!(replaced(&mut w, &[9]), 6, "a row not beside it: judged");
    }

    /// A twin testifies only about its own row ([`Witness::follow_runs`],
    /// [`Seen::arrivals`]). Armed under its twin on row 4, with rows 3 and 6
    /// sampled: its row blanked and `abcdef` arriving two rows up (row 3) or
    /// down (row 7) is a move; the twin on row 4 is refused and says nothing
    /// about rows beyond it, nor does a near copy on row 6 about row 7
    /// (`tests/moves_are_followed.rs`). A copy two rows away on a sampled row
    /// that did not stand `TWIN_MIN` gets the band (the module doc's limit).
    /// Negative control: row 3 also held `abcdef` at arming; an erase under two
    /// copies names nothing.
    #[test]
    fn a_twin_testifies_only_about_its_own_row() {
        let own = row("abcdef");
        let blank = row("      ");
        let near = row("abcdvw");
        // (row 4, row 3, row 6) when armed and after; the landing row is blank
        // when armed and holds `abcdef` after, unless it is row 3 and held it
        // already; the dr named.
        for (four, three, six, lands, named) in [
            (&own, &blank, &blank, 3u16, vec![-2]),
            (&blank, &blank, &blank, 3, vec![-2]),
            (&own, &blank, &blank, 7, vec![2]),
            (&own, &blank, &near, 7, vec![2]),
            (&own, &own, &blank, 3, vec![]),
        ] {
            let (mut w, cells) = armed_on(
                5,
                &[
                    RowSample { row: 5, cols: &own },
                    RowSample { row: 4, cols: four },
                    RowSample {
                        row: 3,
                        cols: three,
                    },
                    RowSample { row: 6, cols: six },
                    RowSample {
                        row: 7,
                        cols: &blank,
                    },
                ],
            );
            let at = |r: u16, was: &[char]| -> Vec<char> {
                if r == lands {
                    own.clone()
                } else {
                    was.to_vec()
                }
            };
            let (r3, r7) = (at(3, three), at(7, &blank));
            let mut out = Vec::new();
            w.follow_runs(
                &cells,
                &[
                    RowSample {
                        row: 5,
                        cols: &blank,
                    },
                    RowSample { row: 4, cols: four },
                    RowSample { row: 3, cols: &r3 },
                    RowSample { row: 6, cols: six },
                    RowSample { row: 7, cols: &r7 },
                ],
                &mut out,
            );
            assert_eq!(
                out.iter().map(|f| f.dr).collect::<Vec<_>>(),
                named,
                "row 4 {:?}, row 3 {:?}, row 6 {:?}, lands on {lands}",
                four.iter().collect::<String>(),
                three.iter().collect::<String>(),
                six.iter().collect::<String>()
            );
        }
    }

    /// A record the shape pass caught up is armed again
    /// (`Witness::shape_verdicts`). `abcdef` typed under an identical line;
    /// two interior glyphs overwritten (`abXYef`, too few to be evidence, so
    /// the records catch up) and then restored. That walk is their new arming
    /// frame, so the `c`, `d` one row up are twins at once, and the erase under
    /// the twin names nothing (`tests/copies_are_not_moves.rs`). Control: the
    /// line moved down a row after the same catch-up follows.
    #[test]
    fn a_record_the_shape_pass_caught_up_is_armed_again() {
        let own = row("abcdef");
        let blank = row("      ");
        let t0 = Instant::now();
        let cells: Vec<Cell> = (0..6).map(|c| cell_of(5, c, t0, 7)).collect();
        let caught_up = || {
            let (mut w, _) = armed_on(
                5,
                &[
                    RowSample { row: 5, cols: &own },
                    RowSample { row: 4, cols: &own },
                    RowSample {
                        row: 6,
                        cols: &blank,
                    },
                ],
            );
            for (text, ms) in [("abXYef", 100), ("abcdef", 190)] {
                let (mut out, mut rel) = (Vec::new(), Vec::new());
                w.walk(
                    &cells,
                    &[
                        RowSample {
                            row: 5,
                            cols: &row(text),
                        },
                        RowSample { row: 4, cols: &own },
                        RowSample {
                            row: 6,
                            cols: &blank,
                        },
                    ],
                    &[],
                    t0 + Duration::from_millis(ms),
                    &mut out,
                    &mut rel,
                );
                assert!(
                    out.is_empty() && rel.is_empty(),
                    "an interior rewrite is not evidence: {text}"
                );
            }
            assert!(
                w.seen.iter().all(|s| s.twin & 1 == 1),
                "every record a twin one row up again"
            );
            w
        };
        let follow = |w: &mut Witness, six: &[char]| {
            let mut out = Vec::new();
            w.follow_runs(
                &cells,
                &[
                    RowSample {
                        row: 5,
                        cols: &blank,
                    },
                    RowSample { row: 4, cols: &own },
                    RowSample { row: 6, cols: six },
                ],
                &mut out,
            );
            out.iter().map(|f| f.dr).collect::<Vec<_>>()
        };
        let mut w = caught_up();
        assert_eq!(
            follow(&mut w, &blank),
            Vec::<i16>::new(),
            "erased under its twin"
        );
        let mut w = caught_up();
        assert_eq!(follow(&mut w, &own), vec![1], "moved down a row");
    }

    /// One run on row 5 laid under every column of `text` (its spaces
    /// never-armed) and armed by one walk.
    fn armed_line(text: &str) -> (Witness, Vec<Cell>) {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let n = u16::try_from(text.chars().count()).expect("a short line");
        let cells: Vec<Cell> = (0..n).map(|c| cell_of(5, c, t0, 7)).collect();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row(text),
            }],
            &[],
            t0,
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty());
        (w, cells)
    }

    /// The shift pass's verdict once row 5 reads `now`, the caret last seen
    /// at `caret` on it.
    fn shift_onto(w: &mut Witness, cells: &[Cell], now: &str, caret: Option<u16>) -> Vec<ShiftRun> {
        let mut out = Vec::new();
        w.shift_runs(
            cells,
            &[RowSample {
                row: 5,
                cols: &row(now),
            }],
            caret.map(|c| (5, c)),
            &mut out,
        );
        out
    }

    fn shift(lo: u16, hi: u16, dc: i16) -> Vec<ShiftRun> {
        vec![ShiftRun {
            row: 5,
            cohort: 7,
            lo,
            hi,
            dc,
        }]
    }

    /// An insert pushes its tail, and the tail is found where it went
    /// ([`Witness::shift_runs`]). One key inside `hello world`, at the caret and
    /// one column on, and a three-cell paste: the block from the edit point to
    /// the line's end is named with its width, including the space before
    /// `world` when the key went in at it.
    #[test]
    fn an_insert_names_the_tail_it_pushed_with_its_width() {
        let (mut w, cells) = armed_line("hello world");
        assert_eq!(
            shift_onto(&mut w, &cells, "hello Xworld", Some(6)),
            shift(6, 10, 1),
            "a key typed at the `w`"
        );
        assert_eq!(
            shift_onto(&mut w, &cells, "helloX world", Some(5)),
            shift(5, 10, 1),
            "a key typed at the space takes the space along"
        );
        assert_eq!(
            shift_onto(&mut w, &cells, "hello XYZworld", Some(6)),
            shift(6, 10, 3),
            "a three-cell insert"
        );
        assert_eq!(
            shift_onto(&mut w, &cells, "hello world", Some(11)),
            vec![],
            "nothing moved, nothing named"
        );
    }

    /// The caret is the edit point where letters repeat. `hello` with an `l`
    /// typed at the first `l`: only the `o` reads gone, but the extension over
    /// the repeated letters finds the block, and the caret picks its start.
    /// Without the caret the one moved glyph is too little.
    #[test]
    fn a_key_typed_among_repeated_letters_inserts_at_the_caret() {
        let (mut w, cells) = armed_line("hello");
        assert_eq!(
            shift_onto(&mut w, &cells, "helllo", Some(2)),
            shift(2, 4, 1)
        );
        assert_eq!(
            shift_onto(&mut w, &cells, "helllo", Some(3)),
            shift(3, 4, 1)
        );
        assert_eq!(
            shift_onto(&mut w, &cells, "helllo", None),
            vec![],
            "one moved glyph and no caret is not evidence"
        );
        // `ab|cd` + `b`: the greedy block would start at the `b` left of
        // the caret; the caret says the key went in at `c`.
        let (mut w, cells) = armed_line("abcd");
        assert_eq!(shift_onto(&mut w, &cells, "abbcd", Some(2)), shift(2, 3, 1));
    }

    /// One glyph is evidence only at the caret: a key typed before the line's
    /// last letter pushes a one-glyph tail, named only when the caret was at
    /// the edit point.
    #[test]
    fn a_one_glyph_tail_moves_only_under_the_caret() {
        let (mut w, cells) = armed_line("type");
        assert_eq!(shift_onto(&mut w, &cells, "typXe", Some(3)), shift(3, 3, 1));
        assert_eq!(shift_onto(&mut w, &cells, "typXe", None), vec![]);
        assert_eq!(shift_onto(&mut w, &cells, "typXe", Some(1)), vec![]);
        // A Backspace case (`wrapped_composer_band`'s `c8_bs_mid`): the caret's
        // `s` is the band's last cell, the erase pulled `also see` together, and
        // `see`'s `s` stands three columns on with more text past it, so it is
        // not an insert.
        let (mut w, cells) = armed_line("I als");
        assert_eq!(shift_onto(&mut w, &cells, "I alee see", Some(4)), vec![]);
    }

    /// A rewrite is not a shift. Negative controls: other text over the tail
    /// with a coincidental copy of its first letter further along; a found tail
    /// whose next glyph stands on other text; a cleared tail with a copy
    /// further left (⌃W over a repeated word). Nothing is named.
    #[test]
    fn a_rewrite_or_a_clear_is_never_read_as_an_insert() {
        let (mut w, cells) = armed_line("hello world");
        assert_eq!(
            shift_onto(&mut w, &cells, "hello xyzqw", Some(6)),
            vec![],
            "other text with a `w` in it"
        );
        assert_eq!(
            shift_onto(&mut w, &cells, "hello Xwoqqq", Some(6)),
            vec![],
            "`wo` found, then other text"
        );
        let (mut w, cells) = armed_line("ab cd ab");
        assert_eq!(
            shift_onto(&mut w, &cells, "ab cd   ", Some(8)),
            vec![],
            "a cleared tail is not text that moved"
        );
    }

    /// A tail the row's end took is left behind, not a veto: an insert pushing
    /// the line's last word past the edge names the glyphs still on the row;
    /// those whose `col + dc` is blank stay for the walk to release.
    #[test]
    fn an_insert_whose_tail_left_the_row_moves_what_stayed() {
        let (mut w, cells) = armed_line("hi you there");
        assert_eq!(
            shift_onto(&mut w, &cells, "hi XYZyou      ", Some(3)),
            shift(3, 6, 3),
            "`you ` pushed on, `there` wrapped away"
        );
    }

    /// A block landing on its own left-behind tail leaves one record per
    /// `(row, col, born)`: the stale tail's records under the landing columns
    /// go, and those past the landing stay for the walk to release.
    #[test]
    fn a_shift_onto_its_own_left_behind_tail_leaves_one_record_per_key() {
        let (mut w, cells) = armed_line("hi you there");
        assert_eq!(
            shift_onto(&mut w, &cells, "hi XYZyou      ", Some(3)),
            shift(3, 6, 3)
        );
        let mut moved: Vec<_> = cells
            .iter()
            .filter(|c| (3..=6).contains(&c.col))
            .map(|c| (c.row, c.col, c.born))
            .collect();
        w.shift_cells(&mut moved, 3);
        let keys: Vec<_> = w.seen.iter().map(|s| (s.row, s.col, s.born)).collect();
        let mut dedup = keys.clone();
        dedup.dedup();
        assert_eq!(keys, dedup, "two records share a key");
        let cols: Vec<u16> = w.seen.iter().map(|s| s.col).collect();
        // Spaces were never armed: `h i`, then `you` at 6..=8, then the
        // wrapped word's `re` past the landing at 10..=11.
        assert_eq!(cols, vec![0, 1, 6, 7, 8, 10, 11]);
        let units: Vec<Unit> = w.seen.iter().map(|s| s.unit).collect();
        let at = |c: u16| units[cols.iter().position(|&x| x == c).expect("a record")];
        assert_eq!(at(6), unit_at(&row("hi you there"), 3), "`y` moved to 6");
        assert_eq!(at(8), unit_at(&row("hi you there"), 5), "`u` moved to 8");
        assert_eq!(at(10), unit_at(&row("hi you there"), 10), "`r` stayed");
    }

    /// The records follow their cells: re-keyed to the new column, so the
    /// next walk finds each cell under its own glyph and names nothing.
    #[test]
    fn shifted_records_find_their_glyphs_on_the_next_walk() {
        let (mut w, mut cells) = armed_line("hello world");
        let found = shift_onto(&mut w, &cells, "hello Xworld", Some(6));
        assert_eq!(found, shift(6, 10, 1));
        let mut moved = Vec::new();
        for c in &mut cells {
            if (6..=10).contains(&c.col) {
                moved.push((c.row, c.col, c.born));
                c.col += 1;
            }
        }
        w.shift_cells(&mut moved, 1);
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("hello Xworld"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && rel.is_empty(),
            "retired {:?} released {:?}",
            pos(&out),
            pos(&rel)
        );
    }

    #[test]
    fn blank_to_glyph_only_arms_and_glyph_to_blank_releases() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        let cells = [cell(5, 0, t0), cell(5, 1, t0)];
        // The echo landed a frame late: nothing under the cells yet.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("  "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && w.is_empty(),
            "a blank arms nothing and retires nothing"
        );
        // Then the glyphs arrive.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("hi"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && w.len() == 2, "seen: armed, not retired");
        // The same text again — a prompt redraw that put it back (D2).
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("hi"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && w.len() == 2,
            "the same glyph touches nothing"
        );
        // Then the row is blank: both are released to the swoosh, not melted,
        // since nothing sits under them and `hi` is nowhere else visible.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("  "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "a blank is not the fast melt's verdict");
        assert_eq!(pos(&rel), vec![(5, 0), (5, 1)]);
        assert!(
            rel.iter().all(|&(_, _, born)| born == t0),
            "named by identity: the cells' own births"
        );
        assert_eq!(
            w.len(),
            2,
            "released records are KEPT: the cells are resident and unstamped for the retract"
        );
        // The next frame, still blank: nothing to say, and nothing named
        // again.
        let n = w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("  "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty() && n == 0);
        assert_eq!(w.len(), 2, "…and the records stand");
    }

    /// What one walk named: the positions on `retire`, then on `release`.
    type Named = (Vec<(u16, u16)>, Vec<(u16, u16)>);

    /// One walk of `cells` against one sampled row.
    fn walk_row(w: &mut Witness, cells: &[Cell], r: u16, now: &str) -> Named {
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(
            cells,
            &[RowSample {
                row: r,
                cols: &row(now),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        (pos(&out), pos(&rel))
    }

    /// A glyph landing on a blank with the rewrite that took its run goes with
    /// the run ([`Witness::landed_goes_with_its_run`]). A composer follow split
    /// leaves on the caret row the Space the wrap ate (never armed) and the
    /// moved word's old cells; the relay lays a long word over all of them in
    /// one batch. The Space's cell goes with its run, fresh record and all.
    #[test]
    fn a_glyph_landing_on_a_blank_with_the_rewrite_that_took_its_run_goes_with_the_run() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let cells = [cell(5, 2, t0), cell(5, 3, t0), cell(5, 4, t0)];
        assert_eq!(walk_row(&mut w, &cells, 5, "xx cd"), (vec![], vec![]));
        assert_eq!(w.len(), 2, "premise: the Space is never armed");
        let (out, rel) = walk_row(&mut w, &cells, 5, "xxzqw");
        assert_eq!(out, vec![(5, 3), (5, 4), (5, 2)]);
        assert!(rel.is_empty());
        assert!(w.is_empty(), "no record is left for the melting cells");

        // Control — a key's own cell, first read on the walk that retires
        // its run, is armed as ever: it was never read over a blank.
        let t1 = t0 + std::time::Duration::from_millis(60);
        let mut w = Witness::new();
        let word = [cell(5, 3, t0), cell(5, 4, t0)];
        assert_eq!(walk_row(&mut w, &word, 5, "xxxcd"), (vec![], vec![]));
        let keyed = [cell(5, 2, t1), cell(5, 3, t0), cell(5, 4, t0)];
        let (out, _) = walk_row(&mut w, &keyed, 5, "xxzqw");
        assert_eq!(out, vec![(5, 3), (5, 4)], "the key's cell is not named");
        assert_eq!(w.len(), 1, "…and it is armed");

        // Control — a glyph landing on a blank of a run that keeps its text
        // is armed as ever (a late echo, an app filling the space).
        let mut w = Witness::new();
        let line = [
            cell(5, 0, t0),
            cell(5, 1, t0),
            cell(5, 2, t0),
            cell(5, 3, t0),
            cell(5, 4, t0),
        ];
        assert_eq!(walk_row(&mut w, &line, 5, "ab cd"), (vec![], vec![]));
        assert_eq!(walk_row(&mut w, &line, 5, "abxcd"), (vec![], vec![]));
        assert_eq!(w.len(), 5, "the landed glyph is armed with its run");
    }

    /// And with the clear that released it: `ab cd` cleared in the batch that
    /// writes one glyph on the Space's column (a spinner over the cleared
    /// composer). The letters are released, and the Space's cell goes with
    /// them on the swoosh (`release`), the run's own verdict, never the melt,
    /// which would punch a hole in the draining band (`tests/new_line_fade.rs`).
    #[test]
    fn a_glyph_landing_on_a_released_run_s_blank_goes_with_the_run() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let line = [
            cell(5, 0, t0),
            cell(5, 1, t0),
            cell(5, 2, t0),
            cell(5, 3, t0),
            cell(5, 4, t0),
        ];
        assert_eq!(walk_row(&mut w, &line, 5, "ab cd"), (vec![], vec![]));
        let (out, rel) = walk_row(&mut w, &line, 5, "  x  ");
        assert_eq!(rel, vec![(5, 0), (5, 1), (5, 3), (5, 4), (5, 2)]);
        assert!(out.is_empty(), "nothing melts: {out:?}");
    }

    /// A glyph landing on a blank does not pull its run's standing text into
    /// the melt ([`Witness::landed_goes_with_its_run`]). `ab cd ef` →
    /// `ab*cd eX`: the landed cell has `ab` standing on its far side, so it is
    /// not the run's loose blank and the span closure does not reach back
    /// through `cd e`. Likewise a mid-line Backspace, `ab cd efgh` →
    /// `ab*cd egh `.
    #[test]
    fn a_glyph_landing_on_a_blank_between_standing_words_stays_out_of_its_run_s_melt() {
        let t0 = Instant::now();
        for (before, after, want) in [
            ("ab cd ef", "ab*cd eX", vec![(5, 7)]),
            ("ab cd efgh", "ab*cd egh ", vec![(5, 7), (5, 8), (5, 9)]),
        ] {
            let mut w = Witness::new();
            let n = u16::try_from(before.len()).expect("short");
            let line: Vec<Cell> = (0..n).map(|c| cell(5, c, t0)).collect();
            assert_eq!(walk_row(&mut w, &line, 5, before), (vec![], vec![]));
            let (mut out, rel) = walk_row(&mut w, &line, 5, after);
            out.sort_unstable();
            assert_eq!(out, want, "{before:?} -> {after:?}");
            assert!(rel.is_empty(), "{before:?} -> {after:?}: {rel:?}");
        }
    }

    /// The last walk's blanks ride with their cells ([`Witness::blank_seen`]):
    /// a follow pass, scroll and band move each carry a never-armed blank, and
    /// the next walk still knows it was blank, so a relay glyph landing on it
    /// with its run retired takes it with the run.
    #[test]
    fn the_last_walk_s_blanks_ride_with_their_cells() {
        let t0 = Instant::now();
        for mover in 0..3 {
            let mut w = Witness::new();
            let mut cells = [cell(5, 2, t0), cell(5, 3, t0), cell(5, 4, t0)];
            assert_eq!(walk_row(&mut w, &cells, 5, "xx cd"), (vec![], vec![]));
            match mover {
                0 => {
                    let mut moved: Vec<_> = cells.iter().map(|c| (c.row, c.col, c.born)).collect();
                    w.translate_cells(&mut moved, -1);
                }
                1 => w.translate(1, 0),
                _ => w.translate_band(0, 10, -1),
            }
            for c in &mut cells {
                c.row = 4;
            }
            let (out, _) = walk_row(&mut w, &cells, 4, "xxzqw");
            assert_eq!(out, vec![(4, 3), (4, 4), (4, 2)], "mover {mover}");
        }
    }

    /// A released record is kept and read again. `hi` released; the cells stay
    /// resident on the retract. The spinner then lands `Th` under them
    /// (replaced text): both go onto `retire` by birth, reported as re-verdicts
    /// so the engine does not count them twice. The same glyphs back is D2:
    /// nothing.
    #[test]
    fn a_released_record_is_kept_so_a_glyph_landing_under_it_later_melts_fast_and_counts_once() {
        let t0 = Instant::now();
        let cells = [cell(5, 0, t0), cell(5, 1, t0)];
        let release = |w: &mut Witness, out: &mut Vec<_>, rel: &mut Vec<_>| {
            let n = w.walk(
                &cells,
                &[RowSample {
                    row: 5,
                    cols: &row("hi"),
                }],
                &[],
                Instant::now(),
                out,
                rel,
            );
            assert!(out.is_empty() && rel.is_empty() && n == 0 && w.len() == 2);
            let n = w.walk(
                &cells,
                &[RowSample {
                    row: 5,
                    cols: &row("  "),
                }],
                &[],
                Instant::now(),
                out,
                rel,
            );
            assert_eq!(pos(rel), vec![(5, 0), (5, 1)], "released");
            assert!(out.is_empty() && n == 0);
            assert_eq!(w.len(), 2, "the released records are kept");
        };
        // (1) The spinner lands under the released cells.
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        release(&mut w, &mut out, &mut rel);
        let n = w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("Th"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(
            pos(&out),
            vec![(5, 0), (5, 1)],
            "text under a released cell is REPLACED text: the melt"
        );
        assert!(
            out.iter().all(|&(_, _, born)| born == t0),
            "named by identity"
        );
        assert!(rel.is_empty(), "never released twice");
        assert_eq!(
            n, 2,
            "both are re-verdicts of cells already released and counted"
        );
        assert!(
            w.is_empty(),
            "a retired cell's record goes: it is stamped and leaving"
        );
        // (2) One glyph lands and the other stays blank: the run is one object,
        // so the held cell melts with its replaced run-mate (a re-verdict).
        let mut w = Witness::new();
        release(&mut w, &mut out, &mut rel);
        let n = w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row(" x"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = pos(&out);
        got.sort_unstable();
        assert_eq!(got, vec![(5, 0), (5, 1)], "the held cell goes with its run");
        assert!(rel.is_empty());
        assert_eq!(n, 2);
        // (3) The same glyphs back where they were: D2, nothing.
        let mut w = Witness::new();
        release(&mut w, &mut out, &mut rel);
        let n = w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("hi"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && rel.is_empty() && n == 0,
            "the same glyph touches nothing"
        );
        assert_eq!(w.len(), 2);
        // And blank again: still nothing; a held cell is never released or
        // counted again, and never searched for.
        let n = w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("  "),
                },
                RowSample {
                    row: 7,
                    cols: &row("hi"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty() && n == 0);
        assert_eq!(w.len(), 2);
    }

    /// The never-armed cell of a released run (the space in `ab cd`) takes a
    /// released record, so a glyph landing on it is replaced text and the run
    /// melts as one.
    #[test]
    fn a_released_run_s_never_armed_cell_is_held_with_it_and_a_glyph_on_it_melts_the_run() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let word: Vec<Cell> = (0..5u16).map(|c| cell_of(5, c, t0, 7)).collect();
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("ab cd"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(w.len(), 4, "the four letters, not the blank");
        let n = w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("     "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(rel.len(), 5, "the run released, the blank with it");
        assert_eq!(n, 0);
        assert_eq!(
            w.len(),
            5,
            "…and the blank now holds a released record of its own"
        );
        // A single glyph lands on the space: the whole run melts, five
        // re-verdicts.
        let n = w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("  *  "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = pos(&out);
        got.sort_unstable();
        got.dedup();
        assert_eq!(got, vec![(5, 0), (5, 1), (5, 2), (5, 3), (5, 4)]);
        assert!(rel.is_empty());
        assert_eq!(n, 5, "every cell was counted when released: none again");
        assert!(w.is_empty());
    }

    /// A release takes no space from between standing letters
    /// (`wrapped_composer_band`'s `c2_bs`). `ab cd ef` with the last letter
    /// blanked releases `f` only; the spaces at 2 and 5 stay. Clearing the tail
    /// from 4 takes the space at 5 with it; clearing the whole row takes every
    /// blank.
    #[test]
    fn a_partial_release_takes_no_space_from_between_standing_letters() {
        let t0 = Instant::now();
        let run: Vec<Cell> = (0..8u16).map(|c| cell_of(5, c, t0, 7)).collect();
        let arm = |w: &mut Witness, out: &mut Vec<_>, rel: &mut Vec<_>| {
            w.walk(
                &run,
                &[RowSample {
                    row: 5,
                    cols: &row("ab cd ef"),
                }],
                &[],
                Instant::now(),
                out,
                rel,
            );
        };
        let released = |text: &str| {
            let mut w = Witness::new();
            let (mut out, mut rel) = (Vec::new(), Vec::new());
            arm(&mut w, &mut out, &mut rel);
            w.walk(
                &run,
                &[RowSample {
                    row: 5,
                    cols: &row(text),
                }],
                &[],
                Instant::now(),
                &mut out,
                &mut rel,
            );
            assert!(
                out.is_empty(),
                "{text:?}: nothing replaced: {:?}",
                pos(&out)
            );
            let mut got: Vec<u16> = pos(&rel).into_iter().map(|(_, c)| c).collect();
            got.sort_unstable();
            got.dedup();
            got
        };
        assert_eq!(
            released("ab cd e "),
            vec![7],
            "the blanked letter alone: the spaces between standing letters stay"
        );
        assert_eq!(
            released("ab c    "),
            vec![4, 5, 6, 7],
            "a cleared tail takes its own space with it"
        );
        assert_eq!(
            released("        "),
            (0..8).collect::<Vec<u16>>(),
            "a run with nothing standing takes every blank"
        );
    }

    /// One glyph read blank for a walk takes no space with it. `hello world`;
    /// a mid-rewrite sample reads the last letter blank for one walk (a
    /// one-cell suffix, which the shape pass lets stand). The other glyphs
    /// stand, so the release names that cell and not the never-armed space at
    /// 5, which keeps its light and clock and takes no released record. The
    /// glyph back restores the run. Control: a suffix cleared from `w` takes
    /// the space inside the lost span (`tests/partial_release.rs`)
    /// (owner, 2026-09-21).
    #[test]
    fn a_partial_release_of_one_glyph_leaves_the_run_s_spaces_standing() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let word: Vec<Cell> = (0..11u16).map(|c| cell_of(5, c, t0, 7)).collect();
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(w.len(), 10, "ten letters armed, not the space");
        let n = w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello worl "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "nothing replaced: {:?}", pos(&out));
        assert_eq!(
            pos(&rel),
            vec![(5, 10)],
            "the one blanked glyph is released, and no space with it"
        );
        assert_eq!(n, 0);
        assert_eq!(w.len(), 10, "the space took no released record");
        // A key typed inside the melt, past the stamped cell: its cell is
        // armed on the next walk with no release tag. It is not a veto.
        let t1 = t0 + std::time::Duration::from_millis(50);
        let mut typed: Vec<Cell> = word.clone();
        typed.push(cell_of(5, 11, t1, 7));
        w.walk(
            &typed,
            &[RowSample {
                row: 5,
                cols: &row("hello worl x"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty());
        assert_eq!(w.len(), 11, "the new key's cell is armed");
        // The glyph back: the recorded identities all stand and the released
        // record's ink returned, so the run is restorable despite the new cell.
        let mut restore = Vec::new();
        w.restored_runs(
            &typed,
            &[RowSample {
                row: 5,
                cols: &row("hello worldx"),
            }],
            &mut restore,
        );
        assert_eq!(
            restore,
            vec![(7, None)],
            "the run is restorable with a key typed inside the melt"
        );
        // Control: a retype over the released column (a later-born cell at col
        // 10 with the same `d`) refuses.
        let mut w2 = w.clone();
        let mut retyped: Vec<Cell> = word.clone();
        retyped.push(cell_of(5, 10, t1, 7));
        w2.walk(
            &retyped,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut refused = Vec::new();
        w2.restored_runs(
            &retyped,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &mut refused,
        );
        assert!(
            refused.is_empty(),
            "a retype over the released column does not bring the old light back: {refused:?}"
        );
        w.restored(&[7], &typed);
        let n = w.walk(
            &typed,
            &[RowSample {
                row: 5,
                cols: &row("hello worldx"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty(), "the row stands whole");
        assert_eq!(n, 0);
        assert_eq!(w.len(), 11);
        // Control: cleared from `w` on, the space at 5 is outside the lost span
        // and stands; cleared from `o` on, the space inside it goes.
        let mut w = Witness::new();
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello      "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = pos(&rel);
        got.sort_unstable();
        assert_eq!(
            got,
            vec![(5, 6), (5, 7), (5, 8), (5, 9), (5, 10)],
            "a cleared suffix takes its letters; the space before it stands"
        );
        let mut w = Witness::new();
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hell       "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = pos(&rel);
        got.sort_unstable();
        assert_eq!(
            got,
            (4..11u16).map(|c| (5, c)).collect::<Vec<_>>(),
            "a suffix cleared across the space takes the space inside it"
        );
    }

    /// The tail goes with the suffix. `hello world ` (a typed trailing space at
    /// 11, never armed), cleared from `w` on: 11 is right of `hi = 10` with
    /// nothing standing beyond, so it goes; 5 is left of `lo = 6` with `hello`
    /// standing left of it, so it stands. Mirror: ` hello world` with `hello`
    /// cleared takes the leading space at 0 and keeps the space at 6.
    #[test]
    fn a_never_armed_cell_past_a_cleared_suffix_goes_with_it() {
        let t0 = Instant::now();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let mut w = Witness::new();
        let line: Vec<Cell> = (0..12u16).map(|c| cell_of(5, c, t0, 7)).collect();
        w.walk(
            &line,
            &[RowSample {
                row: 5,
                cols: &row("hello world "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(w.len(), 10, "ten letters armed, neither space");
        w.walk(
            &line,
            &[RowSample {
                row: 5,
                cols: &row("hello       "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "nothing replaced: {:?}", pos(&out));
        let mut got = pos(&rel);
        got.sort_unstable();
        assert_eq!(
            got,
            (6..12u16).map(|c| (5, c)).collect::<Vec<_>>(),
            "the cleared suffix takes the trailing space after it; the space before it stands"
        );
        // The mirror: a leading never-armed space before a cleared prefix.
        let mut w = Witness::new();
        w.walk(
            &line,
            &[RowSample {
                row: 5,
                cols: &row(" hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(w.len(), 10);
        w.walk(
            &line,
            &[RowSample {
                row: 5,
                cols: &row("       world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "nothing replaced: {:?}", pos(&out));
        let mut got = pos(&rel);
        got.sort_unstable();
        assert_eq!(
            got,
            (0..6u16).map(|c| (5, c)).collect::<Vec<_>>(),
            "the cleared prefix takes the leading space before it; the space after it stands"
        );
    }

    /// A record armed after the release whose glyph changed is neutral.
    /// `hello world` with one glyph released for a walk; a key lands on the
    /// never-armed column 11 (`x`) and is then rewritten (`y`) as the old text
    /// returns. The restore is accepted, and the walk after it retires exactly
    /// the rewritten cell.
    #[test]
    fn a_record_armed_after_the_release_whose_glyph_changed_is_neutral_to_the_restore() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let word: Vec<Cell> = (0..11u16).map(|c| cell_of(5, c, t0, 7)).collect();
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello worl "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(
            pos(&rel),
            vec![(5, 10)],
            "the one blanked glyph is released"
        );
        let t1 = t0 + std::time::Duration::from_millis(50);
        let mut typed: Vec<Cell> = word.clone();
        typed.push(cell_of(5, 11, t1, 7));
        w.walk(
            &typed,
            &[RowSample {
                row: 5,
                cols: &row("hello worl x"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty());
        assert_eq!(w.len(), 11, "the new key's cell is armed as `x`");
        // The glyph at 11 rewritten to `y`, the `d` back at 10.
        let mut restore = Vec::new();
        w.restored_runs(
            &typed,
            &[RowSample {
                row: 5,
                cols: &row("hello worldy"),
            }],
            &mut restore,
        );
        assert_eq!(
            restore,
            vec![(7, None)],
            "a record armed after the release, its glyph changed, is no veto"
        );
        w.restored(&[7], &typed);
        w.walk(
            &typed,
            &[RowSample {
                row: 5,
                cols: &row("hello worldy"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(
            pos(&out),
            vec![(5, 11)],
            "the rewritten cell is retired by the walk, on its own"
        );
        assert!(rel.is_empty(), "nothing else leaves: {:?}", pos(&rel));
        assert_eq!(
            w.len(),
            10,
            "the retired cell's record goes with its verdict; the restored `d` keeps its own, the space still has none"
        );
    }

    /// The run decides the clock. `hi` three ways: one glyph replaced and one
    /// blanked is a re-laid box (the whole run melts); both blanked with `hi`
    /// on another sampled row is a box re-laid elsewhere (moved, melts); both
    /// blanked and `hi` nowhere is a submit (released).
    #[test]
    fn a_replaced_glyph_or_a_moved_run_melts_fast_and_only_a_vanished_run_is_released() {
        let t0 = Instant::now();
        let cells = [cell(5, 0, t0), cell(5, 1, t0)];
        let arm = |w: &mut Witness, out: &mut Vec<_>, rel: &mut Vec<_>| {
            w.walk(
                &cells,
                &[RowSample {
                    row: 5,
                    cols: &row("hi"),
                }],
                &[],
                Instant::now(),
                out,
                rel,
            );
            assert!(out.is_empty() && rel.is_empty() && w.len() == 2);
        };
        // (1) `h` → `H`, `i` → blank: the run was overwritten.
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        arm(&mut w, &mut out, &mut rel);
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("H "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = pos(&out);
        got.sort_unstable();
        assert_eq!(
            got,
            vec![(5, 0), (5, 1)],
            "the blanked `i` melts with its replaced run-mate"
        );
        assert!(rel.is_empty());
        // (2) both blank here, `hi` on the caret's row: the text moved.
        let mut w = Witness::new();
        arm(&mut w, &mut out, &mut rel);
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("  "),
                },
                RowSample {
                    row: 7,
                    cols: &row("hi"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(
            pos(&out),
            vec![(5, 0), (5, 1)],
            "a run found elsewhere moved: fast"
        );
        assert!(rel.is_empty());
        // (3) both blank, the caret's row holds other text: the text went.
        let mut w = Witness::new();
        arm(&mut w, &mut out, &mut rel);
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("  "),
                },
                RowSample {
                    row: 7,
                    cols: &row("> ok"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty(),
            "nothing replaced it and it is nowhere: not the melt"
        );
        assert_eq!(pos(&rel), vec![(5, 0), (5, 1)], "released to the swoosh");
        // A partial match is not the text: `h` alone elsewhere moves nothing.
        let mut w = Witness::new();
        arm(&mut w, &mut out, &mut rel);
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("  "),
                },
                RowSample {
                    row: 7,
                    cols: &row("h ho"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty(),
            "`h` and `i` must stand together at their spacing"
        );
        assert_eq!(pos(&rel), vec![(5, 0), (5, 1)]);
    }

    /// A run that still stands has not moved: a torn read blanks `TO` at the
    /// end of a line whose `TO` also stands earlier. The blanked fragment of a
    /// run with glyphs still in place is released (restorable), never melted
    /// as moved text.
    #[test]
    fn a_blanked_tail_of_a_standing_run_is_released_though_its_glyphs_stand_elsewhere() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let cells: Vec<Cell> = (0..11u16).map(|c| cell(5, c, t0)).collect();
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab TO cd TO"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab TO cd   "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "nothing melts: {:?}", pos(&out));
        let mut got = pos(&rel);
        got.sort_unstable();
        assert_eq!(got, vec![(5, 9), (5, 10)], "the blanked `TO` is released");
    }

    /// One glyph is not a word: a run of one blanked glyph is never ruled moved
    /// text, wherever its letter stands.
    #[test]
    fn one_blanked_glyph_found_elsewhere_is_released_not_moved() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let cells = [cell(5, 0, t0)];
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("z"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row(" "),
                },
                RowSample {
                    row: 7,
                    cols: &row("zoom"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "a lone `z` elsewhere moves nothing");
        assert_eq!(pos(&rel), vec![(5, 0)]);
    }

    /// One wide glyph is one glyph: a single CJK character standing elsewhere
    /// is not moved text; it is released.
    #[test]
    fn one_blanked_wide_glyph_found_elsewhere_is_released_not_moved() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let cells = [cell(5, 0, t0), cell(5, 1, t0)];
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("\u{4f60}\0"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("  "),
                },
                RowSample {
                    row: 7,
                    cols: &row("\u{4f60}\0"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "one wide glyph elsewhere moves nothing");
        // Both halves are released. A wide cell is named once per half by
        // each of the run's two records, so the list is deduped here.
        let mut got = pos(&rel);
        got.sort_unstable();
        got.dedup();
        assert_eq!(got, vec![(5, 0), (5, 1)]);
    }

    /// A partial release takes its own spaces, not the run's: a released tail
    /// of a run whose text still stands takes the spaces on its side and none
    /// inside the standing text; a run with nothing standing takes all.
    #[test]
    fn a_released_tail_of_a_standing_run_takes_only_its_own_spaces() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        // "ab cd ef gh": spaces at 2, 5 and 8.
        let cells: Vec<Cell> = (0..11u16).map(|c| cell(5, c, t0)).collect();
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab cd ef gh"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab cd      "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty());
        let mut got = pos(&rel);
        got.sort_unstable();
        got.dedup();
        assert_eq!(
            got,
            vec![(5, 6), (5, 7), (5, 8), (5, 9), (5, 10)],
            "`ef gh` and the space between them go; the spaces at 2 and 5 stand"
        );
    }

    /// The moved-text search reads the run's own row too, at any other offset
    /// (a box re-laid four columns right is re-laid), never the original place.
    #[test]
    fn a_run_re_laid_elsewhere_on_its_own_row_moved_and_melts_fast() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let cells = [cell(5, 0, t0), cell(5, 1, t0)];
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("hi"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("    hi"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(pos(&out), vec![(5, 0), (5, 1)], "moved along its row: fast");
        assert!(rel.is_empty());
    }

    #[test]
    fn a_different_glyph_retires_and_a_relaid_cell_arms_afresh() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        let cells = [cell(5, 0, t0)];
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("h"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("H"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(pos(&out), vec![(5, 0)], "h -> H is an overwrite");
        // Re-laid at the same column (a new birth) over the new glyph: not
        // an overwrite of the old record — a fresh arm.
        let later = t0 + std::time::Duration::from_millis(90);
        let relaid = [cell(5, 0, later)];
        w.walk(
            &relaid,
            &[RowSample {
                row: 5,
                cols: &row("x"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty(),
            "the old record must not retire a re-laid cell"
        );
        assert_eq!(w.len(), 1);
        w.walk(
            &relaid,
            &[RowSample {
                row: 5,
                cols: &row("x"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_wide_glyph_s_two_cells_are_retired_as_one_unit() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        let cells = [cell(5, 3, t0), cell(5, 4, t0)];
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("   \u{4f60}\0"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && w.len() == 2);
        // Overwritten by two narrow glyphs: the lead changes and the
        // continuation changes; each pushes its unit, so both cells are
        // named (twice — `retire_cells` stamps a cell once).
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("   ab"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        let mut got = pos(&out);
        got.sort_unstable();
        got.dedup();
        assert_eq!(got, vec![(5, 3), (5, 4)]);
        // And when only the continuation cell is resident, its change still
        // names the lead too.
        let mut w = Witness::new();
        let cont = [cell(5, 4, t0)];
        w.walk(
            &cont,
            &[RowSample {
                row: 5,
                cols: &row("   \u{4f60}\0"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.walk(
            &cont,
            &[RowSample {
                row: 5,
                cols: &row("     "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "blanked, and nowhere else: not the melt");
        assert_eq!(pos(&rel), vec![(5, 4), (5, 3)], "released as one unit");
        assert!(
            rel.iter().all(|&(_, _, born)| born == t0),
            "the partner is named with the SAME birth: one keystroke laid both"
        );
    }

    #[test]
    fn a_stale_cell_under_a_fresh_one_is_named_by_its_own_birth_and_the_fresh_one_never() {
        // One owner per cell holds per cohort, so a key typed back over a cell
        // an abandoned cohort still owns leaves two cells at one position: the
        // stale one (cohort 7, laid under `r`) is named by its birth and the
        // fresh one (cohort 9, under the `X`) is armed, in either pool order.
        let t0 = Instant::now();
        let t1 = t0 + std::time::Duration::from_millis(540);
        let mut out = Vec::new();
        let mut rel = Vec::new();
        for stale_first in [true, false] {
            let mut w = Witness::new();
            let stale = cell_of(5, 8, t0, 7);
            let blank = cell_of(5, 5, t0, 7);
            w.walk(
                &[blank, stale],
                &[RowSample {
                    row: 5,
                    cols: &row("hello world"),
                }],
                &[],
                Instant::now(),
                &mut out,
                &mut rel,
            );
            assert!(
                out.is_empty() && w.len() == 1,
                "the `r` is armed, the blank not"
            );
            let fresh = cell_of(5, 8, t1, 9);
            let pool = if stale_first {
                [blank, stale, fresh]
            } else {
                [fresh, blank, stale]
            };
            w.walk(
                &pool,
                &[RowSample {
                    row: 5,
                    cols: &row("hello woXld"),
                }],
                &[],
                Instant::now(),
                &mut out,
                &mut rel,
            );
            let mut got = out.clone();
            got.sort_unstable();
            assert_eq!(
                got,
                vec![(5, 5, t0), (5, 8, t0)],
                "stale first = {stale_first}: the stale cell and its run's blank, by birth — never the fresh cell"
            );
            assert_eq!(
                w.len(),
                1,
                "stale first = {stale_first}: the fresh cell's record stands alone"
            );
            // The next frame, the same text: the fresh cell is at peace.
            w.walk(
                &[fresh],
                &[RowSample {
                    row: 5,
                    cols: &row("hello woXld"),
                }],
                &[],
                Instant::now(),
                &mut out,
                &mut rel,
            );
            assert!(out.is_empty(), "stale first = {stale_first}");
        }
    }

    #[test]
    fn standing_run_index_matches_the_retirement_oracle() {
        let t0 = Instant::now();
        let t1 = t0 + std::time::Duration::from_millis(1);
        let mut cells = [
            cell_of(2, 0, t0, 7),
            cell_of(2, 1, t0, 7), // never-armed space
            cell_of(2, 2, t0, 7),
            cell_of(3, 0, t0, 7),  // same cohort ID, different row
            cell_of(3, 1, t0, 7),  // wide continuation
            cell_of(3, 2, t0, 7),  // never-armed space
            cell_of(5, 0, t0, 9),  // released record
            cell_of(5, 1, t0, 9),  // leaving cell cannot keep its run standing
            cell_of(6, 0, t0, 10), // no record
            cell_of(2, 0, t1, 11), // fresh owner at an old position
        ];
        cells[7].retire_at = Some(t0);
        let mut witness = Witness::new();
        for i in [0, 2, 3, 4, 7, 9] {
            witness.arm(
                &cells[i],
                Unit {
                    ch: if i == 3 || i == 4 { '你' } else { 'a' },
                    wide: i == 3 || i == 4,
                    cont: i == 4,
                },
                &[None; 4],
                t0,
            );
        }
        witness.hold_released(&cells[6]);
        let runs = [(5, 9), (2, 7), (6, 10), (3, 7), (2, 11)];

        // Every subset of the witnessed identities; order is deliberately
        // reversed and duplicates emulate both halves naming a wide unit.
        let armed = [0, 2, 3, 4, 6, 7, 9];
        for mask in 0..(1usize << armed.len()) {
            let mut retire = Vec::new();
            for (bit, &i) in armed.iter().enumerate().rev() {
                if mask & (1 << bit) != 0 {
                    let c = cells[i];
                    retire.push((c.row, c.col, c.born));
                    retire.push((c.row, c.col, c.born));
                }
            }
            let mut expected: Vec<_> = runs
                .iter()
                .copied()
                .filter(|&run| {
                    cells.iter().any(|c| {
                        !c.leaving()
                            && (c.row, c.cohort) == run
                            && witness.find(c.row, c.col, c.born).is_some()
                            && !retire.contains(&(c.row, c.col, c.born))
                    })
                })
                .collect();
            expected.sort_unstable();
            let original_order = retire.clone();
            witness.retired_runs.clear();
            witness.retired_runs.extend_from_slice(&runs);
            witness.find_standing_runs(&cells, &retire);
            assert_eq!(witness.standing_runs, expected, "mask {mask}");
            assert_eq!(retire, original_order, "verdict order must stay unchanged");
        }
        witness.retired_runs.clear();
        witness.find_standing_runs(&cells, &[]);
        assert!(
            witness.standing_runs.is_empty(),
            "no stale run state on an idle walk"
        );
    }

    #[test]
    fn a_blank_in_a_word_goes_with_its_run_and_stays_with_it_on_a_redraw() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        // "ab cd" as one run (cohort 7), and an unrelated run on the same
        // row (cohort 9) three columns on.
        let word: Vec<Cell> = (0..5u16).map(|c| cell_of(5, c, t0, 7)).collect();
        let other: Vec<Cell> = (8..10u16).map(|c| cell_of(5, c, t0, 9)).collect();
        let cells: Vec<Cell> = word.iter().chain(other.iter()).copied().collect();
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab cd   xy"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty());
        assert_eq!(w.len(), 6, "the four letters and the two, not the blank");
        // The same text again: nothing moves, the blank stays with its word.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab cd   xy"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty());
        // The word's row cleared under it, the other run's text still there:
        // the blank goes with its run; the other run is untouched.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("        xy"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty(),
            "`ab cd` went and is nowhere: released, not melted"
        );
        let mut got = pos(&rel);
        got.sort_unstable();
        got.dedup();
        assert_eq!(
            got,
            vec![(5, 0), (5, 1), (5, 2), (5, 3), (5, 4)],
            "the blank goes with its run, on the run's own clock"
        );
        assert_eq!(
            w.len(),
            7,
            "the other run's records stand, and the released run's are kept — the blank's too"
        );
    }

    #[test]
    fn an_unsampled_row_keeps_its_records_and_a_dropped_cell_is_forgotten() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        let cells = [cell(5, 0, t0), cell(7, 0, t0)];
        w.walk(
            &cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("a"),
                },
                RowSample {
                    row: 7,
                    cols: &row("b"),
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(w.len(), 2);
        // Row 7 not sampled this frame: its record stays.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("a"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && w.len() == 2);
        // The ribbon dropped row 7's cell: its record goes with it.
        w.walk(
            &cells[..1],
            &[RowSample {
                row: 5,
                cols: &row("a"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && w.len() == 1);
        // A scroll carries the record with the cell.
        w.translate(5, 0);
        let moved = [cell(0, 0, t0)];
        w.walk(
            &moved,
            &[RowSample {
                row: 0,
                cols: &row("a"),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && w.len() == 1,
            "the record followed the scroll"
        );
        w.walk(
            &moved,
            &[RowSample {
                row: 0,
                cols: &row(" "),
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty());
        assert_eq!(pos(&rel), vec![(0, 0)], "blanked and nowhere: released");
    }

    #[test]
    fn a_band_move_carries_the_records_inside_it_drops_what_leaves_and_keeps_the_list_sorted() {
        // A composer slides rows 3..=5 up by one under a band that stands: the
        // row-2 record stays, the row-3 record lands beside it on row 2 (the
        // list must re-sort for the walk's binary search), row 5's moves to 4,
        // and a row-3 record carried past the band's top is dropped.
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        let cells = [cell(2, 5, t0), cell(3, 1, t0), cell(5, 0, t0)];
        let glyphs = row("abcdef");
        let samples: Vec<RowSample<'_>> = [2u16, 3, 5]
            .iter()
            .map(|&r| RowSample {
                row: r,
                cols: &glyphs,
            })
            .collect();
        w.walk(&cells, &samples, &[], Instant::now(), &mut out, &mut rel);
        assert_eq!(w.len(), 3);
        w.translate_band(3, 5, -1);
        let moved = [cell(2, 5, t0), cell(2, 1, t0), cell(4, 0, t0)];
        let samples: Vec<RowSample<'_>> = [2u16, 4]
            .iter()
            .map(|&r| RowSample {
                row: r,
                cols: &glyphs,
            })
            .collect();
        w.walk(&moved, &samples, &[], Instant::now(), &mut out, &mut rel);
        assert!(
            out.is_empty() && w.len() == 3,
            "every record followed its cell and none was re-armed"
        );
        // The moved row-3 cell's text is now blank, found by the binary search
        // on the re-sorted list. Its run's blanked glyphs (`b` at 1, `f` four
        // on) stand at that spacing on row 4 (`abcdef`, offset 1), so the run
        // moved and is retired, in column order.
        let blank = row("a     ");
        w.walk(
            &moved,
            &[
                RowSample {
                    row: 2,
                    cols: &blank,
                },
                RowSample {
                    row: 4,
                    cols: &glyphs,
                },
            ],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        assert_eq!(pos(&out), vec![(2, 1), (2, 5)]);
        assert!(rel.is_empty());
        // A band move that carries a record past the top edge drops it.
        let mut w = Witness::new();
        w.walk(
            &[cell(3, 0, t0)],
            &[RowSample {
                row: 3,
                cols: &glyphs,
            }],
            &[],
            Instant::now(),
            &mut out,
            &mut rel,
        );
        w.translate_band(3, 5, -1);
        assert!(w.is_empty(), "carried past the band's edge: gone");
    }

    #[test]
    fn the_witness_is_bounded_and_allocates_nothing_past_its_cap() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let cap = w.seen.capacity();
        let mut out = Vec::new();
        let mut rel = Vec::new();
        let cells: Vec<Cell> = (0..(WITNESS_CAP as u16 + 40))
            .map(|i| cell(i / 200, i % 200, t0))
            .collect();
        let glyphs: Vec<char> = vec!['x'; 200];
        let samples: Vec<RowSample<'_>> = (0..6)
            .map(|r| RowSample {
                row: r,
                cols: &glyphs,
            })
            .collect();
        w.walk(&cells, &samples, &[], Instant::now(), &mut out, &mut rel);
        assert_eq!(w.len(), WITNESS_CAP);
        assert_eq!(w.seen.capacity(), cap, "no growth past the reserved cap");
    }

    /// The deferral's scratch is reserved ([`Witness::find_deferred_runs`],
    /// [`Witness::new`]): a full witness whose every record is beside a
    /// withheld row and whose every glyph changed fills `blind_changed` and
    /// defers every run without growing past what `new` reserved.
    #[test]
    fn the_deferral_s_scratch_is_reserved_and_never_grows() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let caps = (w.blind_changed.capacity(), w.deferred_runs.capacity());
        assert!(
            caps.0 >= WITNESS_CAP && caps.1 >= WITNESS_CAP,
            "reserved: {caps:?}"
        );
        let cap = u16::try_from(WITNESS_CAP).expect("a small cap");
        let cells: Vec<Cell> = (0..cap)
            .map(|i| cell_of(2 * (i / 200), i % 200, t0, u32::from(i / 2)))
            .collect();
        fn even(cols: &[char]) -> Vec<RowSample<'_>> {
            (0..6).map(|k| RowSample { row: 2 * k, cols }).collect()
        }
        let was: Vec<char> = vec!['x'; 200];
        let changed: Vec<char> = vec!['y'; 200];
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(&cells, &even(&was), &[], t0, &mut out, &mut rel);
        assert_eq!(w.len(), WITNESS_CAP, "fixture: a full witness");
        let odd: Vec<u16> = (0..6).map(|k| 2 * k + 1).collect();
        let later = t0 + Duration::from_millis(16);
        w.walk(&cells, &even(&changed), &odd, later, &mut out, &mut rel);
        assert!(out.is_empty() && rel.is_empty(), "every run waits a walk");
        assert_eq!(w.blind_changed.len(), WITNESS_CAP, "every record read");
        assert_eq!(w.deferred_runs.len(), WITNESS_CAP / 2, "every run deferred");
        assert_eq!(
            (w.blind_changed.capacity(), w.deferred_runs.capacity()),
            caps,
            "no growth past the reserved capacity"
        );
    }
}
