// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE CONTENT WITNESS** (2026-09-12, the abandoned band) — the ribbon's
//! one content-aware retirement, and the engine half of a seam the host
//! feeds from its terminal lock.
//!
//! The ribbon is an ABSOLUTE-CELL record of the keys: a cell is `(row, col)`
//! and its clocks, and nothing in the engine knows what glyph is under it.
//! Since `1e3e4a31b` an erase invalidates no host coordinate (D2 — the
//! owner: *"when the screen is refreshed the rainbow disappears"* — a prompt
//! redraw must not wipe the ribbon), so the only things that could take a
//! cell off the glass were its own clocks. A TUI that re-lays its input box
//! (Claude Code, Codex) moves the TEXT and leaves the band where the text
//! was: the owner's *"stray rainbows … cursor trails that sometimes get
//! 'lost' or abandoned in complex CLI tools like codex"*.
//!
//! The witness closes exactly that, and nothing else:
//!
//! * after each glow tick the host hands the engine the live grid rows the
//!   resident ribbon occupies ([`RowSample`], per-column chars in the row
//!   probe's own convention — the lead glyph at its column, `'\0'` at a wide
//!   continuation, `' '` for a blank);
//! * the FIRST time a cell is seen holding a NON-BLANK glyph, the witness
//!   records it — the char, whether it is wide, and which half of a wide
//!   glyph the cell is. Blank → glyph only ARMS: a key whose echo lands a
//!   frame late is never retired, and a cell laid over a blank is left to
//!   its own clocks;
//! * glyph → a DIFFERENT glyph RETIRES the cell on the fast melt
//!   (`Ribbon::retire_cells`, [`super::ribbon::RETIRE_MELT_S`]) — both cells
//!   of a wide glyph as one unit, whichever half changed: light is sitting
//!   under the WRONG letters, the stray the melt was built for;
//! * glyph → BLANK is a different verdict (2026-09-14, the new line's fade —
//!   the owner: *"when I went to a new line the contrail disappeared versus
//!   nicely fading"*). Nothing sits under the light, so it has nothing to
//!   be wrong over; the cell is RELEASED (`Ribbon::release_cells`) and its
//!   run leaves through the swoosh's own retract — drawn into the hand
//!   farthest-first over `RETRACT_DUR_S + RETRACT_FADE_S`, the head last —
//!   exactly as a row the hand left leaves everywhere else. That is Claude
//!   Code's Enter (the composer cleared), a Ctrl-U, a cleared line. **THE
//!   RUN DECIDES THE SPEED**: a typed run is one object, so if any of its
//!   glyphs was REPLACED this walk, every blanked cell of the run melts
//!   with it (a re-laid box that overwrote one letter and blanked the rest
//!   is a re-laid box); and if the run's blanked glyphs are found, in the
//!   same order at the same spacing, on another sampled row or elsewhere on
//!   its own — the text MOVED, an input box re-laid elsewhere with the
//!   light left where the text was — the run melts fast too, the mark
//!   following its text away. Only a run whose text is GONE from every row
//!   the witness can see is released;
//! * **a released record is KEPT** (the fix-up of 2026-09-14). A released
//!   cell is not stamped — it owns its column, unstamped, for the whole
//!   retract — so its record must stand for as long as it is resident, or
//!   the next walk would ARM it over whatever lands there: an Ink frame
//!   split across PTY reads paints the transcript's spinner on the row the
//!   composer left one frame after the clear, and the light would have sat
//!   under those wrong letters for the rest of the 0.64 s. A released
//!   record has exactly one thing left to say: a glyph landing under the
//!   cell is REPLACED text and the cell melts fast — the melt multiplies
//!   into the retract's envelope, monotone (`Ribbon::env_of`) — with its
//!   run (a run-mate still under a blank goes with it, the run being one
//!   object); a blank is still nothing; the same glyph back is D2. It is
//!   never released again, never searched for again, and never counted
//!   again: the walk reports every cell it named on `retire` that an
//!   earlier walk had already released, and `Engine::witness_rows` keeps
//!   `ribbon_retired=` at one count per cell. The never-armed cells of a
//!   released run (its spaces) get a released record of their own, so a
//!   glyph landing on one of them is read the same way;
//! * a record is about ONE CELL — `(row, col, born)`, not a position. A
//!   cell re-laid at the same column is a new birth with a record of its
//!   own, so a retype is never mistaken for an overwrite; and where TWO
//!   cells are resident at one position (one owner per cell holds per
//!   cohort — a key typed back over a cell an abandoned cohort still owns
//!   mints a second) each is judged on its own record and retired by its
//!   own identity: the stale one for the glyph it was laid under, the key's
//!   own never, on the frame it is born or after. A cell the ribbon drops
//!   is forgotten with it;
//! * **a blank goes with its run.** A cell laid over a blank — the space in
//!   `hello world` — has no glyph of its own to witness: blank before, blank
//!   after the row is cleared, it could never be retired by content and
//!   lingered ALONE on the abandoned row for its whole life, a one-cell
//!   stray. When a run is released or every witnessed glyph in it is
//!   retired, its never-armed cells go with it on the same clock. If some
//!   letters survive a partial repaint, unchanged spaces stay between them;
//!   changing one glyph must not split the ribbon at every word boundary.
//!   A redraw of the same text retires nothing (D2).
//!
//! D2 holds by construction: the host samples AFTER the PTY batch is
//! applied, so an erase followed by the same text at the same cells inside
//! one batch (every zsh prompt redraw, every Ctrl-L) is seen as the same
//! glyph and touches nothing. Witness records form a fixed-capacity, sorted,
//! resident list ([`WITNESS_CAP`]); ordinary record lookups cost
//! `O(cells · log entries)`. Retirement membership uses sorted scratch, and
//! moved-text checks have their own bound below. The walk allocates nothing
//! past warm-up and runs only while rainbow kitty owns the frame.
//!
//! **It composes with the echo ledger** (`Engine::echo_bridge`, 2026-09-10)
//! by never touching it: a retirement is a clock on a cell already laid,
//! and the ledger is about presses whose cells are not laid yet. A ledger
//! payout lays fresh cells (a new birth) only where no live cell owns the
//! column — a retired cell is [`super::ribbon::Cell::leaving`] and owns
//! nothing — so a payout can lay over a retired cell but never revive it,
//! and the fresh cell arms its own record on the next walk.

use aterm_time::Instant;

use crate::cursor_glow::band_row;

use super::ribbon::Cell;

/// Witness records the engine keeps: one per resident ribbon cell, at most
/// this many. Past the cap a cell is simply not witnessed (it keeps its own
/// clocks); the host's widest band at 177 columns across three rows is
/// under it.
pub const WITNESS_CAP: usize = 1024;

/// Rows a host samples per frame for the witness ([`super::Engine::ribbon_rows`]
/// names them): the rows the resident ribbon occupies, plus the caret's.
/// A wrapped paragraph is three; eight is headroom, not a target.
pub const WITNESS_ROWS: usize = 8;

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

/// What one column of a sampled row holds, resolved to the GLYPH UNIT it
/// belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unit {
    /// The glyph — the lead char for either half of a wide glyph; `' '` for
    /// a blank.
    pub ch: char,
    /// The glyph occupies two cells.
    pub wide: bool,
    /// This column is the CONTINUATION half of a wide glyph (its lead is one
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

/// One witnessed cell — keyed by the CELL, `(row, col, born)`, not by the
/// position: two cells can be resident at one position with different
/// births, and each carries its own record.
#[derive(Clone, Copy, Debug)]
struct Seen {
    row: u16,
    col: u16,
    /// The birth of the cell this record is about, the third part of the
    /// key: a cell re-laid at the same column is a different cell and the
    /// record does not carry over; a stale cell still resident under a
    /// fresh one is named for retirement by this, so the fresh one is
    /// never taken with it (`retire_cells` stamps by identity — a stamp by
    /// position took a key's own cell with the stale one on its birth
    /// frame).
    born: Instant,
    /// The glyph unit the cell was first seen holding.
    unit: Unit,
    /// Set by the walk for every record whose cell is still resident; a
    /// record the walk did not reach is a cell the ribbon dropped.
    live: bool,
    /// Set once the cell's run was RELEASED — its glyph went and nothing
    /// replaced it — and kept while the cell is resident: it is unstamped
    /// and owns its column for the retract's span, so without a record the
    /// next walk would arm it over whatever lands there. A released record
    /// says one thing more: a glyph landing under the cell is REPLACED text
    /// (the melt, with the cell's run); a blank is nothing; the same glyph
    /// back is D2. It is never released, searched for or counted again.
    released: bool,
    /// The original identity of a released run, eligible for exact redraw
    /// restoration. A freshly armed/retyped identity never inherits this.
    restorable: bool,
    /// Retirement accounting survives a successful redraw restoration.
    counted: bool,
}

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
    /// The record was already released by an earlier walk and the cell is
    /// still under a blank: HELD until its run decides — it melts with a
    /// run-mate that was replaced this walk (a re-verdict, not counted
    /// again) and is otherwise left to the retract it is on. Its glyph is
    /// long gone, so it is never searched for.
    held: bool,
    counted: bool,
}

#[derive(Clone, Copy, Debug)]
struct RestoreRun {
    row: u16,
    cohort: u32,
    cells: usize,
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
    /// `(row, cohort)` of every run a walk RETIRED a cell of — a glyph
    /// replaced, or the run's blanked text found elsewhere — the runs whose
    /// blanked and never-armed cells take the fast melt. Resident scratch,
    /// cleared per walk.
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
    /// `(row, cohort)` of every run a walk RELEASED — blanked, and its text
    /// nowhere the witness can see — the runs whose never-armed cells go to
    /// the swoosh with them. Resident scratch, cleared per walk.
    released_runs: Vec<(u16, u32)>,
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
}

impl Witness {
    /// An empty witness with its capacity reserved once ([`WITNESS_CAP`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            seen: Vec::with_capacity(WITNESS_CAP),
            retired_runs: Vec::new(),
            standing_runs: Vec::new(),
            retired_cells: Vec::new(),
            released_runs: Vec::new(),
            shape_runs: Vec::new(),
            fresh_released: Vec::new(),
            counted_names: Vec::new(),
            run_ids: Vec::new(),
            blanked: Vec::new(),
            restore_runs: Vec::new(),
        }
    }

    /// Records held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// True with nothing witnessed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Forget everything (a reset: the cells are gone with the coordinate
    /// space). Capacity is kept.
    pub fn clear(&mut self) {
        self.seen.clear();
    }

    /// A scroll moved every cell up by `rows` (`Ribbon::translate_scroll`):
    /// the records move with them, and those that left the grid are dropped.
    /// Order is preserved — every row shifts by the same amount.
    pub fn translate(&mut self, rows: u16) {
        if rows == 0 {
            return;
        }
        self.seen.retain_mut(|s| {
            if s.row < rows {
                return false;
            }
            s.row -= rows;
            true
        });
    }

    /// A ROW BAND moved (`Ribbon::translate_band`, the
    /// [`crate::cursor_glow::band_row`] law): a record outside
    /// `top..=bottom` stands, one inside moves by `delta` with its cell, and
    /// one carried past the band's edge is dropped with it. Rows inside and
    /// outside the band can cross, so the list is re-sorted in place — an
    /// event, not a frame, and `sort_unstable` allocates nothing.
    pub fn translate_band(&mut self, top: u16, bottom: u16, delta: i16) {
        if delta == 0 || top > bottom {
            return;
        }
        self.seen
            .retain_mut(|s| match band_row(s.row, top, bottom, delta) {
                Some(row) => {
                    s.row = row;
                    true
                }
                None => false,
            });
        self.seen.sort_unstable_by_key(|s| (s.row, s.col, s.born));
    }

    /// **THE INSERT'S REWRITE RE-LAID `row` LEFT OF `col`** ([`super::Event::Rewrite`],
    /// 2026-09-13): drop the records there so the next walk ARMS the cells
    /// under the placeholder's glyphs instead of retiring them. Claude
    /// Code's `[Image #1] ` is different text over the insert's first cells,
    /// and the seam promises those cells keep their light under one
    /// continuous ribbon (§27) — judged as an overwrite they melted, and the
    /// next key laid beside a retired cell in a new cohort: the rainbow
    /// severed by a hole the placeholder's width. The suffix's records fall
    /// out of the walk on their own (its cells are leaving); a later re-lay
    /// of the row with other text still retires the re-armed cells. Order
    /// is preserved — `retain` keeps it.
    pub fn forget_left_of(&mut self, row: u16, col: u16) {
        self.seen.retain(|s| !(s.row == row && s.col < col));
    }

    /// The record for the cell `(row, col, born)`, if any.
    fn find(&self, row: u16, col: u16, born: Instant) -> Option<usize> {
        self.seen
            .binary_search_by(|s| (s.row, s.col, s.born).cmp(&(row, col, born)))
            .ok()
    }

    /// Arm a record for `cell` holding `unit` — unless the unit is blank
    /// (blank → glyph only arms, later) or the witness is full.
    fn arm(&mut self, cell: &Cell, unit: Unit) {
        if unit.is_blank() {
            return;
        }
        self.insert(cell, unit, false);
    }

    /// A RELEASED record for a never-armed cell of a run the walk just
    /// released — the space in `hello world`, nothing ever under it: it
    /// went to the swoosh with its run, and a glyph landing on it is read
    /// as replaced text exactly as on its run-mates. Unless the witness is
    /// full, when the cell is simply not witnessed, as ever.
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
            },
        );
    }

    /// Both cells of the unit a record stands for, pushed onto `retire` by
    /// identity. The partner half carries the SAME `born`: a wide glyph's
    /// two cells are laid by one keystroke at one instant (`Ribbon::lay`),
    /// and a cell at the partner's column with another birth is a different
    /// cell, judged on its own record in the same walk.
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

    /// **THE WALK.** Every resident cell that is not already leaving is read
    /// against this frame's samples: a cell on an unsampled row keeps what
    /// the witness knew; a cell first seen over a glyph is armed; a cell
    /// whose recorded glyph has CHANGED has its unit pushed onto `retire`
    /// BY IDENTITY, `(row, col, born)`, and its record dropped (the cell is
    /// stamped and leaving); a cell whose recorded glyph has GONE goes onto
    /// `release` (both lists cleared here, so the caller reads exactly this
    /// walk's verdicts) — unless its run decides otherwise, below — and its
    /// record is KEPT, marked released; a cell re-laid since (a different
    /// `born`) is a different cell and is armed on its own.
    ///
    /// **THE RUN DECIDES THE CLOCK.** A blanked cell's verdict is provisional
    /// until every cell of its run has been read: if any cell of the run was
    /// REPLACED this walk, or the run's blanked glyphs are found in order at
    /// their own spacing on any sampled row other than where they were
    /// ([`Witness::moved`] — the text moved, the light did not), the run's
    /// blanked cells go onto `retire` with it; otherwise the run's text is
    /// gone and they go onto `release`. Never-armed cells follow their run
    /// when it is released or has no witnessed glyph left standing; unchanged
    /// blanks between surviving letters stay lit. A released run's never-armed
    /// cells get a released record of their own. Records whose cells the walk never
    /// reached — dropped by the ribbon — are forgotten. A cell may be pushed
    /// twice (a wide unit's two halves both resident); `Ribbon::retire_cells`
    /// and `Ribbon::release_cells` act on it once.
    ///
    /// **A RELEASED RECORD IS READ AGAIN ON EVERY WALK** while its cell is
    /// resident (unstamped, on the retract): the same glyph back is D2 and
    /// a blank is nothing, but a glyph landing under it is REPLACED text —
    /// the cell goes onto `retire`, and so does every held run-mate still
    /// under a blank. Those re-verdicts name cells an earlier walk already
    /// released and `Engine::witness_rows` already counted, so the walk
    /// RETURNS how many of them it named on `retire` this time — the caller
    /// subtracts them and `ribbon_retired=` stays one count per cell. A
    /// held cell is never released again and never searched for.
    pub fn walk(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        retire: &mut Vec<(u16, u16, Instant)>,
        release: &mut Vec<(u16, u16, Instant)>,
    ) -> usize {
        retire.clear();
        release.clear();
        self.retired_runs.clear();
        self.released_runs.clear();
        self.blanked.clear();
        self.fresh_released.clear();
        self.counted_names.clear();
        for s in &mut self.seen {
            s.live = false;
        }
        for cell in cells {
            if cell.leaving() {
                // A cell a PARTIAL release stamped onto the retract keeps its
                // released record while it is resident, so the identical
                // redraw a frame later can still restore it
                // ([`Witness::restored_runs`], `Ribbon::restore_releases`).
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
                }
                continue;
            };
            let unit = unit_at(sample.cols, cell.col);
            match self.find(cell.row, cell.col, cell.born) {
                Some(i) => {
                    let seen = self.seen[i];
                    if seen.unit == unit {
                        self.seen[i].live = true;
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
                None => self.arm(cell, unit),
            }
        }
        // THE RUN DECIDES: each run with a blanked cell is retired if one of
        // its glyphs was replaced or its blanked text is found elsewhere, and
        // released otherwise. Sorted by run, the FRESH blanks before the HELD
        // ones, so the moved-text search reads each run's newly blanked
        // glyphs in column order, once, and never the held cells' — theirs
        // went on an earlier walk.
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
            let fast =
                self.retired_runs.contains(&run) || Self::moved(&self.blanked[i..fresh_end], rows);
            if fast {
                // The whole run melts: the fresh blanks with their replaced
                // or moved run-mates, and the held cells with them — a
                // re-verdict on each, counted already.
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
        self.shape_verdicts(cells, rows, retire, release);
        // The releases that stood: their records are kept, marked — a glyph
        // landing under one later is REPLACED text, a blank is nothing, the
        // same glyph back is D2 — and counted once.
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
        // THE BLANKS GO WITH THEIR RUN: a cell of a named run that holds no
        // record at all — never armed, because nothing was ever under it —
        // goes with its run-mates, on the run's own clock, and a released
        // run's takes a released record so it is read like its run-mates
        // from here on. A cell WITH a record was read above: matched and
        // kept, or changed and named.
        self.released_runs.sort_unstable();
        if !self.retired_runs.is_empty() || !self.released_runs.is_empty() {
            for cell in cells {
                if cell.leaving() || self.find(cell.row, cell.col, cell.born).is_some() {
                    continue;
                }
                let run = (cell.row, cell.cohort);
                if self.retired_runs.binary_search(&run).is_ok() {
                    // **A BLANK THAT IS STILL BLANK, IN A RUN THAT IS STILL
                    // STANDING, IS NOT EVIDENCE**
                    // (2026-09-15, the owner: *"odd logic of drawing a part
                    // of the trail when moving the cursor and editing
                    // text"*). A never-armed cell has no record because
                    // nothing was ever under it — it is the SPACE in
                    // `hello world`. It goes with a RETIRED run because the
                    // run is one object and its letters moved; but where
                    // the space is STILL a space, nothing moved under this
                    // cell and there is nothing for its light to be wrong
                    // over. Retiring it anyway is what punched the owner's
                    // band into word-shaped blocks: one character inserted
                    // mid-line retired eleven cells, five of them spaces at
                    // columns 8, 14, 18, 20 and 25 whose glyphs had not
                    // changed, and the band read
                    // `..######.#####.###.#.-###.####....` — six blocks
                    // with one-cell gaps — for the next 640 ms
                    // (`rbt/c4_edit`, frames 0647 → 0655).
                    //
                    // A run with NOTHING LEFT STANDING still takes its
                    // blanks, and so does a released run: there the run's
                    // text is gone and a space with nothing around it is
                    // the one-cell stray the law was written for. What is
                    // refused is only a hole punched in a run whose letters
                    // are still lit either side of it.
                    let still_blank = rows
                        .iter()
                        .find(|s| s.row == cell.row)
                        .is_some_and(|s| unit_at(s.cols, cell.col).is_blank());
                    if !still_blank || self.standing_runs.binary_search(&run).is_err() {
                        retire.push((cell.row, cell.col, cell.born));
                    }
                } else if self.released_runs.binary_search(&run).is_ok() {
                    release.push((cell.row, cell.col, cell.born));
                    self.hold_released(cell);
                }
            }
        }
        // **WHAT A RUN LOSES, IT LOSES IN ONE PIECE** (2026-09-16, the
        // owner: *"I still am sometimes seeing bugs and gaps"*, and *"when
        // backspacing … the rainbow cursor trail breaks a part"*).
        //
        // Every verdict above is taken PER CELL, by comparing one column's
        // recorded glyph with the one standing there now. A text SHIFT — the
        // mid-line Backspace that pulls the tail left, the insert that pushes
        // it right — changes every column from the edit to the end of the
        // line, so the honest per-cell answer is "replaced" for all of them.
        // But natural text repeats: wherever `old[i + 1] == old[i]` the
        // shifted column holds the SAME glyph it recorded, the comparison
        // answers "unchanged", and that one cell is kept while both its
        // neighbours are named. Measured at the host seam on the owner's own
        // gesture (`c8_bs_mid`: the caret walked back three words into a
        // wrapped composer, then a Backspace run mid-word), the named set
        // came out a COMB — columns 26, 28, 30, 31, 32, 33 and 35 retired,
        // 27, 29 and 34 kept, over `…and I alsosee some a` — and 120 ms
        // later, when the fast melt had taken the named ones, the band on
        // glass was a solid head at 2..25 plus three DETACHED SPECKS: three
        // interior dark runs where the row had had none.
        //
        // `Ribbon::retract_suffix` states the shape light is allowed to
        // leave a row in, and states it as a law: *"CONTIGUOUS at every frame
        // … nothing is ever removed from the MIDDLE"*. The Backspace, the
        // kill and the insert's rewrite all obey it. The content witness is
        // the one path that removes light by NAME, and a name is not a shape:
        // a per-cell verdict can name any subset, and most subsets are holes.
        //
        // So the names are read as a SPAN and not as a set: within one run —
        // one row, one cohort, the object the retirement is already reasoned
        // about as ("the run is one object and its letters moved") — every
        // cell BETWEEN the leftmost and the rightmost named column goes with
        // them. A cell inside that span that compared equal did so by
        // coincidence: the shift moved text over it too, and what is under it
        // now is not what its light was laid for.
        //
        // The span is the tightest closure that removes holes, and that is
        // why it is a span and not a suffix. It swallows only columns the
        // walk has already condemned on BOTH sides, so a change that really
        // is interior stays interior: a program overwriting a wide glyph's
        // two cells still takes exactly those two and leaves its narrow
        // neighbours lit either side
        // ([`a_wide_glyph_s_two_cells_are_retired_as_one_unit`]), and a
        // single stale cell named alone is still named alone
        // ([`a_key_typed_over_an_abandoned_cohorts_cell_keeps_its_own_light`]).
        // A run nothing was named in is untouched. A cell the walk RELEASED
        // (its glyph WENT, rather than changed) is left on its own clock:
        // that verdict has a restore path ([`Witness::restored_runs`]) and
        // this is not the place to revoke it.
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
        recounted
    }

    /// **A RUN LOSES A PREFIX, A SUFFIX, OR ALL OF ITSELF — NEVER ITS MIDDLE**
    /// (2026-09-16 — the owner: *"there is still this gapping issue that
    /// arises in codex, it seems to happen when doing backword and
    /// editing"*, and, on the fix's first cut: *"I'm not convinced that this
    /// is a Codex specific issue … you need to be fixing in general"*).
    ///
    /// [`Ribbon::retract_suffix`] states how light may leave a row —
    /// *"CONTIGUOUS at every frame … nothing is ever removed from the
    /// MIDDLE"* — and every geometric path obeys it. The witness is the one
    /// path that removes light by NAME, and until this pass a name anywhere
    /// in a run was honoured: one interior cell whose glyph changed, or
    /// went, punched a one-cell hole in a band whose letters stood lit either
    /// side of it. The 2026-09-16 span closure below closed the holes
    /// BETWEEN names; a lone name stayed a hole "by design".
    ///
    /// What names a lone interior cell, measured: an app painting a
    /// decoration into a blank cell of its composer and moving it on — Codex
    /// 0.154's ambient particle field, a dot drifting through the space
    /// between two typed words, the cell under a parked caret cleared and
    /// painted again (`tests/codex_particle_replay.rs`, its real bytes: a
    /// one-cell hole exactly on each space a word hop had parked the caret
    /// on, and — the same dot found nowhere else that frame — the whole run
    /// RELEASED and the band falling from 33 lit cells to 9 while the hand
    /// was still typing). A spinner glyph, a spellcheck rewriting one letter,
    /// a TUI drawing its own cursor glyph, a line-number gutter ticking: the
    /// same shape, none of them Codex.
    ///
    /// The law. For a run that is STANDING — some cell of it holds a record
    /// that is neither released nor named this walk — the names are read as
    /// ONE SHAPE against the run's recorded extent (its cells with records;
    /// never-armed spaces are not ends, they go with their run):
    ///
    /// * names that reach the run's first or last recorded cell are a
    ///   prefix, a suffix or the whole, and stand as named — the shift a
    ///   mid-line Backspace or insert makes, the tail an app cleared, the
    ///   line it rewrote;
    /// * names strictly inside that cover MOST of the run's recorded cells
    ///   are a rewrite whose end cell happened to compare equal (`ll`, `ee`
    ///   — natural text repeats) and are EXTENDED to the nearer end, so no
    ///   speck is left standing past them;
    /// * names strictly inside that cover less are NOT EVIDENCE: the light
    ///   stays, and the records take the glyphs standing there now, so the
    ///   walk does not name them again next frame for the same reason. A
    ///   cell that went blank records a blank; a glyph landing on it later
    ///   is another interior change, and stays.
    ///
    /// A run with nothing standing — every record released, its text gone,
    /// its light on the retract — is not shaped: a glyph landing under it
    /// is replaced text and the run melts as it always did.
    fn shape_verdicts(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
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
            for j in 0..self.run_ids.len() {
                let (r, c, born) = self.run_ids[j];
                if let Some(i) = self.find(r, c, born) {
                    self.seen[i].unit = sample.map_or(Unit::BLANK, |s| unit_at(s.cols, c));
                    self.seen[i].live = true;
                }
            }
        }
    }

    /// Find the retired runs with something left standing in one pool walk.
    /// Only runs with no surviving armed cell take their unchanged spaces.
    /// Previously each retired run rescanned the pool and each candidate
    /// linearly searched all retirement identities, multiplying the work on
    /// a composer's partial repaint. Both membership checks now use sorted
    /// resident scratch; the verdict's order and cell identities are untouched.
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

    /// Runs whose complete original cell identities and glyphs are back.
    /// The first pass indexes distinct candidates; the second visits each
    /// resident cell once and checks its record. No per-cell full-pool scan.
    /// Missing samples/records and partial restores cannot authorize recovery.
    pub(super) fn restored_runs(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        out: &mut Vec<(u32, usize)>,
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
                cells: 0,
                exact: true,
                returned_ink: false,
            });
        }
        self.restore_runs.sort_unstable_by_key(|r| r.cohort);
        self.restore_runs.dedup_by_key(|r| r.cohort);
        for cell in cells {
            let Ok(i) = self
                .restore_runs
                .binary_search_by_key(&cell.cohort, |r| r.cohort)
            else {
                continue;
            };
            let seen = self
                .find(cell.row, cell.col, cell.born)
                .map(|j| self.seen[j]);
            let sample = rows.iter().find(|r| r.row == cell.row);
            let run = &mut self.restore_runs[i];
            run.cells += 1;
            match (seen, sample) {
                (Some(seen), Some(sample)) => {
                    run.exact &= (!cell.leaving() || cell.released_at.is_some())
                        && seen.restorable
                        && run.row == cell.row
                        && seen.unit == unit_at(sample.cols, cell.col);
                    run.returned_ink |= seen.released && !seen.unit.is_blank();
                }
                // **A CELL WITH NO RECORD IS NOT EVIDENCE** (2026-09-15) —
                // the restore path's half of the blank law. A never-armed
                // cell holds no record because nothing was ever under it:
                // the SPACE in `hello world`, and the cell a key laid while
                // the app's repaint still had the column blank. It makes no
                // claim about the run's text, so it can neither authorize a
                // restore nor refuse one — only recorded cells decide. Read
                // as a refusal it made a run with a space in it
                // UNRESTORABLE for good: a composer that clears to the end
                // of the screen and rewrites releases the row, the restore
                // is vetoed by the run's own spaces, and every glyph that
                // lands back under the released light is then read as
                // REPLACED text and retired — one cell at a time, for the
                // rest of the run. Measured 2026-09-15 (`c2_bs`): `witness
                // REPLACED (13,7) was=' ' now='s'`, a one-cell hole that
                // stood 1.42 s of a 16 s take.
                //
                // A cell whose ROW was not sampled is a different thing and
                // still refuses: there the witness genuinely does not know.
                (None, Some(_)) => {}
                _ => run.exact = false,
            }
        }
        out.extend(
            self.restore_runs
                .iter()
                .filter(|r| r.exact && r.returned_ink)
                .map(|r| (r.cohort, r.cells)),
        );
    }

    /// Clear release custody only after the ribbon accepted restoration.
    /// Already-counted identities stay counted if they are cleared again.
    pub(super) fn restored(&mut self, cohorts: &[u32], cells: &[Cell]) {
        if cohorts.is_empty() {
            return;
        }
        for cell in cells
            .iter()
            .filter(|c| cohorts.binary_search(&c.cohort).is_ok())
        {
            if let Some(i) = self.find(cell.row, cell.col, cell.born) {
                // A HELD BLANK GOES BACK TO NEVER-ARMED (2026-09-16). The
                // release gave the run's spaces a released blank record of
                // their own (`hold_released`); un-releasing that record
                // would leave a space with a BLANK record no walk can ever
                // name — blank over blank is D2 — so a later clear of the
                // whole line could never name every cell of the run and
                // `Ribbon::release_cells` would read the text gone whole as
                // gone in part. A blank arms nothing: the record goes.
                if self.seen[i].unit.is_blank() {
                    self.seen.remove(i);
                } else {
                    self.seen[i].released = false;
                    self.seen[i].restorable = false;
                }
            }
        }
    }

    /// **THE MOVED-TEXT SEARCH.** True when a run's blanked glyphs — sorted
    /// by column, `run` — stand in the same order at the same spacing
    /// somewhere else on the sampled rows: on another row at any offset, or
    /// on their own row at another offset. Never-armed cells (a run's
    /// spaces) are not in `run` and so constrain nothing, which is what lets
    /// `hello world` re-laid two rows down match. The one place excluded is
    /// where the glyphs were — blank now by definition. `O(rows × cols ×
    /// glyphs)`, and a run is at most a row wide: an event's cost, not a
    /// frame's, and it allocates nothing.
    fn moved(run: &[Blanked], rows: &[RowSample<'_>]) -> bool {
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
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && w.len() == 2,
            "the same glyph touches nothing"
        );
        // Then the row is blank: both go — RELEASED to the swoosh, not
        // retired on the melt: nothing sits under them, and `hi` is nowhere
        // else the witness can see.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("  "),
            }],
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
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty() && n == 0);
        assert_eq!(w.len(), 2, "…and the records stand");
    }

    /// **A RELEASED RECORD IS KEPT, AND READ AGAIN** (the fix-up of
    /// 2026-09-14). `hi` released on one walk; the cells stay resident and
    /// unstamped on the retract. Then the transcript's spinner lands on the
    /// row — `Th` under the cells: different glyphs, REPLACED text — and
    /// both go onto `retire` on that walk, named by their own births, with
    /// the walk reporting both as re-verdicts of cells already released
    /// (so the engine does not count them twice). And the same glyphs back
    /// (`hi` echoed where it was) is D2: nothing. Before the fix-up the
    /// released records were dropped and the next walk ARMED the cells
    /// over the spinner.
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
        // (2) One glyph lands and the other cell stays blank: the run is
        // one object, so the held cell melts with its replaced run-mate —
        // and is a re-verdict too.
        let mut w = Witness::new();
        release(&mut w, &mut out, &mut rel);
        let n = w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row(" x"),
            }],
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
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty() && rel.is_empty() && n == 0,
            "the same glyph touches nothing"
        );
        assert_eq!(w.len(), 2);
        // …and blank again after that: still nothing — a held cell is never
        // released or counted again, and its text is never searched for
        // (`hi` standing on the caret's row would have read as MOVED on a
        // fresh blank; a held one is past that).
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
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty() && n == 0);
        assert_eq!(w.len(), 2);
    }

    /// The never-armed cell of a released run — the space in `ab cd` —
    /// takes a released record with its run, so a glyph landing on IT is
    /// read as replaced text too, and the run melts as one.
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

    /// **THE RUN DECIDES THE CLOCK** (2026-09-14, the new line's fade). The
    /// same `hi`, three ways: one glyph replaced and one blanked is a
    /// re-laid box — the whole run melts fast, the blanked cell with it;
    /// both blanked with `hi` standing on another sampled row is a box
    /// re-laid elsewhere — moved, fast; both blanked and `hi` nowhere is a
    /// submit — released.
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
            &mut out,
            &mut rel,
        );
        assert!(
            out.is_empty(),
            "`h` and `i` must stand together at their spacing"
        );
        assert_eq!(pos(&rel), vec![(5, 0), (5, 1)]);
    }

    /// The moved-text search reads the run's OWN row too, at any other
    /// offset — a box re-laid four columns right on the same row is a
    /// re-laid box — and never the place the glyphs were.
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
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("    hi"),
            }],
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
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("H"),
            }],
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
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && w.len() == 2);
        // Overwritten by two narrow glyphs: the lead changes AND the
        // continuation changes; each pushes its unit, so both cells are
        // named (twice — `retire_cells` stamps a cell once).
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("   ab"),
            }],
            &mut out,
            &mut rel,
        );
        let mut got = pos(&out);
        got.sort_unstable();
        got.dedup();
        assert_eq!(got, vec![(5, 3), (5, 4)]);
        // And when only the CONTINUATION cell is resident, its change still
        // names the lead too.
        let mut w = Witness::new();
        let cont = [cell(5, 4, t0)];
        w.walk(
            &cont,
            &[RowSample {
                row: 5,
                cols: &row("   \u{4f60}\0"),
            }],
            &mut out,
            &mut rel,
        );
        w.walk(
            &cont,
            &[RowSample {
                row: 5,
                cols: &row("     "),
            }],
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
        // One owner per cell holds per COHORT, so a key typed back over a
        // cell an abandoned cohort still owns leaves two cells resident at
        // one position. The stale one (cohort 7, laid under `r`) and the
        // fresh one (cohort 9, laid under the `X` that replaced it) are two
        // records; the walk names the stale cell by its birth and the fresh
        // cell is armed, whichever order the pool holds them in.
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
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty());
        // The word's row cleared under it, the other run's text still there:
        // the blank goes with ITS run; the other run is untouched.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("        xy"),
            }],
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
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && w.len() == 1);
        // A scroll carries the record with the cell.
        w.translate(5);
        let moved = [cell(0, 0, t0)];
        w.walk(
            &moved,
            &[RowSample {
                row: 0,
                cols: &row("a"),
            }],
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
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty());
        assert_eq!(pos(&rel), vec![(0, 0)], "blanked and nowhere: released");
    }

    #[test]
    fn a_band_move_carries_the_records_inside_it_drops_what_leaves_and_keeps_the_list_sorted() {
        // Codex's composer slides rows 3..=5 UP by one under a band above it
        // that stands still: the row-2 record stays at row 2, the row-3
        // record lands beside it on row 2 (the list must re-sort — the walk's
        // binary search reads it), row 5's moves to row 4, and a record on
        // row 3 that would be carried past the band's top edge is dropped
        // with its cell.
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
        w.walk(&cells, &samples, &mut out, &mut rel);
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
        w.walk(&moved, &samples, &mut out, &mut rel);
        assert!(
            out.is_empty() && w.len() == 3,
            "every record followed its cell and none was re-armed"
        );
        // The text under the moved row-3 cell is now blank: found by the
        // binary search on the re-sorted list. Its run's blanked glyphs —
        // `b` at column 1 and `f` four columns on — stand at that spacing on
        // row 4 (`abcdef`, offset 1), so the run MOVED and is retired, in
        // column order.
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
        w.walk(&cells, &samples, &mut out, &mut rel);
        assert_eq!(w.len(), WITNESS_CAP);
        assert_eq!(w.seen.capacity(), cap, "no growth past the reserved cap");
    }
}
