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
//!   stray. When a run is released whole or every witnessed glyph in it is
//!   retired, its never-armed cells go with it on the same clock. A run
//!   released in PART — recorded glyphs still standing — takes a
//!   never-armed cell by the span-plus-tail rule (2026-09-21,
//!   [`Witness::released_span_takes`]): the cell goes when it lies inside
//!   the span of the cells the run lost, or past that span with no standing
//!   glyph of the run beyond it on its side; it stands when a glyph of the
//!   run still stands beyond it. So the interior space of `hello world`
//!   with one glyph blanked stands (glyphs on both sides), the typed
//!   trailing space after a cleared `world` goes with the suffix, and the
//!   space before a cleared suffix stands because `hello` does. If some
//!   letters survive a partial repaint, unchanged spaces stay between them;
//!   changing one glyph must not split the ribbon at every word boundary.
//!   A redraw of the same text retires nothing (D2).
//! * **a torn read is not a cleared line** (2026-09-22 — the owner, on 0.89:
//!   *"There is a rainbow trail gap in 0.89. I'm not sure what is causing it
//!   but it seems like some kind of back cursor movement bug."*, three dark
//!   cells under `TO ` of `INTO `). D2's premise below — one batch, one
//!   sample — does not hold for a frame larger than a PTY read: a
//!   full-region composer redraw over 1 KiB, which the macOS PTY hands over
//!   in 1024-byte reads, presented between two of them, shows the composer
//!   row `CSI 2K`-cleared and written only up to the boundary, the caret
//!   hidden. Every cell right of it reads as glyph → BLANK, and the
//!   identical text comes back 2 to 30 ms later. WHAT WAS AND WAS NOT
//!   WITNESSED: the shape is driven byte for byte at the host seam
//!   (`tests/torn_read_gaps.rs`), which is where every measurement below
//!   comes from; the four real Claude Code recordings beside it
//!   (`tests/fixtures/claude-composer-2026-09-21*.ptylog`) carry no
//!   `CSI 2K` at all and only their startup paint reaches 1 KiB, because
//!   Ink's diff renderer writes changed cells, so they replay as a guard and
//!   not as the reproducer. The owner's own frame — a composer under a
//!   running agent, with a spinner and rules repainting above it — was not
//!   recorded, so the cause of his screenshot is this shape reproduced, not
//!   this shape caught. Three verdicts made that permanent, each
//!   measured at the host seam (`tests/torn_read_gaps.rs`) and each now
//!   answered: a blanked fragment of a run with a recorded glyph still
//!   STANDING in place is released, never searched for — the moved-text
//!   search has no length floor, and `TO` stands in `NEED TO` and `STOP`, so
//!   the owner's `TO` was ruled moved text and melted, which nothing
//!   restores ([`Witness::walk`]); one blanked glyph is never moved text at
//!   all (`Witness::moved`); a restore judges each partial release on its
//!   own cells, lifts it as soon as one of its glyphs is exactly back — the
//!   walk that follows gives the complete frame the verdict it would have
//!   had without the torn one — and a record armed after the release is no
//!   evidence either way (`Witness::restored_runs`); and a released run
//!   whose text still stands takes only the never-armed cells the loss is
//!   around — the span-plus-tail rule of the entry above
//!   ([`Witness::released_span_takes`], which landed first for the same
//!   defect class read from the owner's next report) — so an unrestored
//!   tail no longer combs the text left of it.
//! * **a column still blank when a part is lifted is text that really went**
//!   (2026-09-22, the review round — the owner's class reached through the
//!   back cursor movement he actually named). The lift also demanded that
//!   NO cell of the part be under a blank, and a Backspace or ⌃W erases one
//!   for real: its column is blank on the complete present too, so the part
//!   was never lifted and every standing letter right of the boundary
//!   retracted while its glyph stood — 38 cells dark within 160 ms, then a
//!   29-cell hole for about a second once the hand typed on (85 frames with
//!   real 1024-byte reads under the spinner). The part is lifted on its
//!   returned ink alone now. A column still blank is not evidence against
//!   the rest of the part, and it needs no special case: the walk runs on
//!   that same sample straight after and names it, as a one-glyph run
//!   released to the retract, which is what it is. Measured in
//!   `tests/torn_edit_gaps.rs`; one glyph is never moved text, so this
//!   cannot melt.
//! * **one glyph means one GLYPH** (2026-09-22, the review round). The
//!   length floor counted CELLS, and a wide glyph owns two of them — so one
//!   CJK character cleared the floor and took the melt on the very
//!   coincidence the floor was written against, in the languages where one
//!   glyph really is a word. The continuation half is not a glyph
//!   (`Witness::moved`).
//!
//! **THE BAND FOLLOWS ITS TEXT** (2026-09-21, the owner on v0.90.0 with
//! Claude Code in the window: *"when typing wraps to a new line the
//! previous row's rainbow vanishes suddenly while the new row populates"*).
//! Claude Code's bottom-anchored composer grows by repainting ONE ROW
//! HIGHER without a scroll (measured byte for byte:
//! `docs/measured/claude-code-composer-wrap-bytes-2026-09-21.md`): the
//! text row is rewritten a row up minus the word the wrap moved, the
//! continuation row holds that word beside the caret, and the caret makes
//! a same-row backward move. The witness used to read the old row's
//! glyphs as replaced and melt the whole run in `RETIRE_MELT_S` — the
//! vanish — because the row the text went TO was never sampled and
//! nothing translated cells. So the host now also samples the row above
//! and below every ribbon row (`Engine::ribbon_rows`), and a FOLLOW PASS
//! runs at the START of the tick, before the tick's events are replayed
//! ([`Witness::follow_runs`], `Engine::follow_rows`): a run whose armed
//! glyphs are gone from their own row and stand, at their own columns,
//! one row away (nearest first: `−1, +1, −2, +2`) as a block is
//! TRANSLATED there with every clock intact (`Ribbon::translate_run`),
//! and its records re-keyed ([`Witness::translate_cells`]). The walk after
//! the tick then finds the cells under their own glyphs and retires
//! nothing; `ribbon_followed=` counts them beside `ribbon_retired=`. The
//! pass is a pure function of the records and the samples; it allocates
//! nothing past warm-up and is bounded by the runs' own widths.
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
    /// **THE TWINS IT WAS BORN BESIDE** (2026-09-22): one bit per follow
    /// offset of [`FOLLOW_DRS`], set when the record was ARMED and the row
    /// that far away was sampled holding THIS unit at THIS column. Such a
    /// glyph was already standing there before the record's own glyph was
    /// witnessed, so finding it there later is not evidence that the text
    /// MOVED — it never arrived ([`Witness::follow_runs`]). Cleared when
    /// the record is carried to another row: its neighbours there are new.
    twins: u8,
}

/// The follow pass's row offsets, nearest first: one row up, one row down,
/// then two. Bit `k` of [`Seen::twins`] is `FOLLOW_DRS[k]`.
const FOLLOW_DRS: [i16; 4] = [-1, 1, -2, 2];

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
    /// the swoosh with them when the run's text went WHOLE. Resident
    /// scratch, cleared per walk.
    released_runs: Vec<(u16, u32)>,
    /// Sorted `(row, cohort, col)` of every recorded glyph STILL STANDING
    /// this walk — a record that is not released and whose glyph is under
    /// it now. A released run with a cell on this list lost only PART of
    /// its text, and keeps its never-armed cells outside the span it lost
    /// and short of a glyph still standing on their side
    /// ([`Witness::released_span_takes`], 2026-09-21). Resident scratch,
    /// cleared per walk.
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
    /// One run's ARMED cells for the follow pass: `(col, unit, gone,
    /// twins)` in column order, `gone` when the recorded glyph no longer
    /// stands at the cell, `twins` the record's [`Seen::twins`]. Resident
    /// scratch.
    follow_armed: Vec<(u16, Unit, bool, u8)>,
}

/// **A RUN THE FOLLOW PASS FOUND ELSEWHERE** ([`Witness::follow_runs`]):
/// the cells of `cohort` on `row` within `lo..=hi` stand under their own
/// glyphs `dr` rows away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FollowRun {
    /// The run's row now.
    pub row: u16,
    /// The run's cohort.
    pub cohort: u32,
    /// The leftmost column found.
    pub lo: u16,
    /// The rightmost column found.
    pub hi: u16,
    /// Rows to the text: `−1` one row up, `+1` one row down, then `±2`.
    pub dr: i16,
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
            standing_glyphs: Vec::new(),
            shape_runs: Vec::new(),
            fresh_released: Vec::new(),
            counted_names: Vec::new(),
            run_ids: Vec::new(),
            blanked: Vec::new(),
            restore_runs: Vec::new(),
            follow_runs: Vec::new(),
            follow_armed: Vec::new(),
        }
    }

    /// **THE FOLLOW PASS'S SEARCH** (2026-09-21, the band follows its text;
    /// see the module doc). For every run `(row, cohort)` with a resident
    /// cell on a row sampled this frame, read its ARMED records — live,
    /// not released, on cells that are not leaving — in column order, and
    /// name the run on `out` when the text moved vertically AS A BLOCK:
    ///
    /// * at least two armed records, and at least two of them — and no
    ///   fewer than half — are GONE from their own row (replaced or blank).
    ///   A run whose glyphs still stand where they were is not followed,
    ///   however many copies of its text the screen holds (identical text
    ///   on two rows translates nothing); a run with a single armed glyph
    ///   is too little evidence for a move;
    /// * on the nearest sampled row `row + dr`, `dr ∈ {−1, +1, −2, +2}`,
    ///   at least two and no fewer than half of the armed records stand at
    ///   THEIR OWN COLUMN with THEIR OWN unit — and ARRIVED there: a glyph
    ///   that already stood at that offset when the record was armed
    ///   ([`Seen::twins`]) is not found. An erase is not a move: a line
    ///   killed under an identical line (`$ cd ..` typed below `$ cd ..`,
    ///   then Ctrl-U) has its glyphs gone and a block of them standing one
    ///   row up, but that block was there before any of them was typed —
    ///   carrying the band onto it lit a previous command no key wrote for
    ///   1.3 s (2026-09-22 review). The records that do not
    ///   are all to one side of those that do — a prefix or a suffix (the
    ///   word a composer's wrap moved down off the row's end), never a hole
    ///   in the middle: a rewrite that keeps some letters by coincidence is
    ///   not a block that moved. The first `dr` that qualifies wins.
    ///
    /// `lo..=hi` is the found block's extent; the run's never-armed blanks
    /// inside it go with it (`Ribbon::translate_run`). Pure in the records
    /// and the samples, `O(runs × cells × 4)`, and allocation-free past
    /// warm-up: the scratch is resident and `out` is the caller's.
    pub fn follow_runs(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        out: &mut Vec<FollowRun>,
    ) {
        out.clear();
        self.follow_runs.clear();
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
                    .push((cell.col, seen.unit, gone, seen.twins));
            }
            self.follow_armed.sort_unstable_by_key(|a| a.0);
            self.follow_armed.dedup_by_key(|a| a.0);
            let armed = self.follow_armed.len();
            let gone = self.follow_armed.iter().filter(|a| a.2).count();
            if armed < 2 || gone < 2 || gone * 2 < armed {
                continue;
            }
            for (k, dr) in FOLLOW_DRS.into_iter().enumerate() {
                let Some(target) = row.checked_add_signed(dr) else {
                    continue;
                };
                let Some(there) = rows.iter().find(|s| s.row == target) else {
                    continue;
                };
                let mut found = 0usize;
                let mut lo = u16::MAX;
                let mut hi = 0u16;
                // The found records must be one block: no not-found record
                // between the first and the last found.
                let mut block = true;
                let mut after = false;
                for &(col, unit, _, twins) in &self.follow_armed {
                    // A glyph that was already standing there when this
                    // record was armed did not ARRIVE: it is not found.
                    if twins & (1 << k) == 0 && unit_at(there.cols, col) == unit {
                        if after {
                            block = false;
                            break;
                        }
                        found += 1;
                        lo = lo.min(col);
                        hi = hi.max(col);
                    } else if found > 0 {
                        after = true;
                    }
                }
                if !block || found < 2 || found * 2 < armed {
                    continue;
                }
                out.push(FollowRun {
                    row,
                    cohort,
                    lo,
                    hi,
                    dr,
                });
                break;
            }
        }
    }

    /// **THE RECORDS FOLLOW THEIR CELLS** ([`Witness::follow_runs`] →
    /// `Ribbon::translate_run`): each identity in `moved` — a cell's OLD
    /// `(row, col, born)` — is re-keyed to `row + dr`; a cell without a
    /// record (never armed) needs nothing. The list is re-sorted in place
    /// afterwards: an event, not a frame, and `sort_unstable` allocates
    /// nothing.
    pub fn translate_cells(&mut self, moved: &mut [(u16, u16, Instant)], dr: i16) {
        if moved.is_empty() || dr == 0 {
            return;
        }
        // The records are read against a SORTED copy of the identities, so
        // a record already moved cannot break the search for the next.
        moved.sort_unstable();
        let mut any = false;
        for s in &mut self.seen {
            if moved.binary_search(&(s.row, s.col, s.born)).is_ok()
                && let Some(target) = s.row.checked_add_signed(dr)
            {
                s.row = target;
                // Its neighbours on the new row are not the ones it was
                // armed beside.
                s.twins = 0;
                any = true;
            }
        }
        if any {
            self.seen.sort_unstable_by_key(|s| (s.row, s.col, s.born));
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
    /// (blank → glyph only arms, later) or the witness is full — and note
    /// the TWINS it was born beside ([`Seen::twins`]): every sampled row
    /// at a follow offset that already holds the same unit at the same
    /// column.
    fn arm(&mut self, cell: &Cell, unit: Unit, rows: &[RowSample<'_>]) {
        if unit.is_blank() {
            return;
        }
        self.insert(cell, unit, false);
        let mut twins = 0u8;
        for (k, &dr) in FOLLOW_DRS.iter().enumerate() {
            let twin = cell
                .row
                .checked_add_signed(dr)
                .and_then(|r| rows.iter().find(|s| s.row == r))
                .is_some_and(|s| unit_at(s.cols, cell.col) == unit);
            if twin {
                twins |= 1 << k;
            }
        }
        if twins != 0
            && let Some(i) = self.find(cell.row, cell.col, cell.born)
        {
            self.seen[i].twins = twins;
        }
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
                twins: 0,
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
    /// REPLACED this walk, or — for a run with no recorded glyph still
    /// standing in place (2026-09-22: a run that stands lost a tail, it did
    /// not move) — the run's blanked glyphs are found in order at
    /// their own spacing on any sampled row other than where they were
    /// ([`Witness::moved`] — the text moved, the light did not), the run's
    /// blanked cells go onto `retire` with it; otherwise the run's text is
    /// gone and they go onto `release`. Never-armed cells follow their run
    /// when it is released — only those the loss is around while its text
    /// still stands ([`Witness::released_span_takes`]) — or has no
    /// witnessed glyph left standing; unchanged blanks between surviving
    /// letters stay lit. A
    /// released run's never-armed cells get a released record of their own.
    /// Records whose cells the walk never
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
        self.standing_glyphs.clear();
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
                        // A recorded glyph standing where it was recorded:
                        // its run still has text.
                        if !seen.released && !unit.is_blank() {
                            self.standing_glyphs.push((cell.row, cell.cohort, cell.col));
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
                None => self.arm(cell, unit, rows),
            }
        }
        self.standing_glyphs.sort_unstable();
        self.standing_glyphs.dedup();
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
            // **A RUN THAT STILL STANDS HAS NOT MOVED** (2026-09-22 — the
            // owner, on 0.89: *"There is a rainbow trail gap in 0.89. I'm
            // not sure what is causing it but it seems like some kind of
            // back cursor movement bug."*). The moved-text search has no
            // length floor, so a one- or two-glyph fragment (`TO`, `N`,
            // `IN`) is found somewhere on almost any line of English
            // (`NEED TO`, `STOP`). A present that lands between two PTY
            // reads of one Claude Code frame — the frame is over 1 KiB, the
            // macOS PTY hands it over in 1024-byte reads — sees the composer
            // row cleared by `CSI 2K` and rewritten only up to the read
            // boundary, the caret hidden: every cell right of the boundary
            // reads BLANK, and the newest letters of the hand's own run were
            // ruled MOVED TEXT and retired. No restore reaches a
            // retirement and nothing re-lays an interior column, so the
            // identical text back 2 to 30 ms later stood over a permanent
            // black hole inside the live band — the owner's three cells
            // under `TO ` of `INTO `, and every later key only widened it.
            // Text that moved took its WHOLE run with it: a run with a
            // recorded glyph still standing in place
            // ([`Witness::standing_glyphs`], the same record the partial
            // release's span law reads) lost a tail, it did not go
            // elsewhere, so its blanked glyphs are RELEASED — restorable —
            // and never searched for. A replaced glyph still retires its
            // run, and a run with nothing standing is still searched as
            // before.
            let fast = self.retired_runs.contains(&run)
                || (self.standing_glyphs_of(run).is_empty()
                    && Self::moved(&self.blanked[i..fresh_end], rows));
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
                    // blanks, and so does a released run whose text went
                    // WHOLE: there the run's text is gone and a space with
                    // nothing around it is the one-cell stray the law was
                    // written for. What is refused is only a hole punched
                    // in a run whose letters are still lit either side of
                    // it.
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
                    // **A PARTIAL RELEASE TAKES NO SPACE OUTSIDE WHAT IT
                    // LOST** (2026-09-21 — the owner, in Claude Code's
                    // composer: the spaces between typed phrases dark, every
                    // glyph lit). A row sampled mid-rewrite reads ONE
                    // recorded glyph blank for a walk; the run is released
                    // for that cell, and this arm took every never-armed
                    // cell of the run with it — all its spaces — onto the
                    // retract (`Ribbon::release_cells`' partial stamp). A
                    // key typed inside the melt, or the glyph back a walk
                    // later than the melt, then left the letters standing
                    // and the spaces dark: the owner's exact shape. A
                    // released run whose recorded glyphs STILL STAND lost
                    // only part of its text, and its never-armed cells go
                    // only where the loss is around them: inside the span
                    // of the cells it lost, as the retire span law reads
                    // names, or PAST the span with no standing glyph of the
                    // run beyond them on their side — the tail closure
                    // (`released_span_takes`: the review's probe typed
                    // `hello world ` and the program cleared from `w`; the
                    // span alone left the trailing space lit five cells past
                    // `hello `, the one-cell stray this law forbids). So a
                    // suffix an app cleared takes the space inside it and
                    // the space after it (`tests/partial_release.rs`), a
                    // single blanked letter takes none, and the space before
                    // a cleared suffix stands while the letters left of it
                    // do. A run with no recorded glyph standing lost its
                    // text whole, and its spaces go with it as they always
                    // did.
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

    /// Whether the never-armed cell at `col` of the released `run` goes with
    /// the release. A run with NO recorded glyph standing lost its text
    /// whole: yes, as always. A run with glyphs still standing lost only
    /// PART, and takes the cell in two shapes — **the span**: `col` lies
    /// strictly inside the span the run is losing, between two of its cells
    /// this walk named on `release` or that an earlier partial release
    /// stamped onto the retract (`Cell::released_at`); and **the tail**
    /// (2026-09-21, the review's host-seam probe): `col` lies past the span
    /// with no standing recorded glyph of the run beyond it on its side —
    /// right of the span's `hi` with none standing right of `hi`, left of
    /// its `lo` with none standing left of `lo`. Under that rule the
    /// interior space of `hello world` with one glyph blanked stands
    /// (glyphs stand on both sides), the typed trailing space after a
    /// cleared `world` goes with the suffix (nothing stands right of it),
    /// and the space before a cleared suffix stands because `hello` stands
    /// left of it. An event's cost, not a frame's.
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

    /// Releases whose complete original cell identities and glyphs are
    /// back, as `(cohort, stamp)`, sorted: `stamp` is the
    /// [`Cell::released_at`] one PARTIAL release laid on its cells, `None`
    /// for the cohort's unstamped cells — a WHOLE release. Each partial
    /// release is judged on its own cells (2026-09-22): one whose text
    /// never came back as it was — an insert's shifted tail — no longer
    /// vetoes a later one whose glyphs are back. The first pass indexes
    /// distinct candidates; the second visits each resident cell once and
    /// checks its record. No per-cell full-pool scan. Missing
    /// samples/records and partial restores cannot authorize recovery.
    /// Whether every released CELL is still resident is the ribbon's to
    /// count (`Ribbon::restore_releases`): a cell laid after the release is
    /// no part of what was released, here or there. A record armed after
    /// the release carries `released_at: None`, so it is read with the
    /// cohort's UNSTAMPED cells, and there it is neutral unless it sits
    /// over a column a released record holds (2026-09-21, the arm below) —
    /// a retype over released text, whose new light the old must not come
    /// back under.
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
            // **A KEY TYPED INSIDE THE MELT IS NOT A VETO** (2026-09-21 —
            // the owner's dark cells at phrase boundaries). A record armed
            // AFTER the release — the cell the hand laid past the stamped
            // ones while the composer's rewrite was still owed, tagged by
            // no release walk — makes no claim on the run's OLD text: it
            // neither authorizes nor refuses, whether its own glyph stands
            // or was rewritten since (a changed glyph is the walk's to
            // retire on its own — the walk runs right after this restore,
            // `Engine::witness_rows` — so it is NEUTRAL here, not a veto;
            // 2026-09-21, the review). Read as a refusal (the tag was
            // required of every record) it made a partial release
            // UNRESTORABLE the moment the next key landed, and the stamped
            // glyph melted under text that was back. What still refuses is
            // a record armed over a column a RELEASED record holds: that is
            // the retype over released text the tag loop names ("a later
            // retype arms an untagged new birth"), and the old light must
            // not come back under the new.
            let over_released = seen.is_some_and(|s| !s.restorable)
                && self
                    .seen
                    .iter()
                    .any(|s| s.row == cell.row && s.col == cell.col && s.released);
            let run = &mut self.restore_runs[i];
            match (seen, sample) {
                // **A PART COMES BACK WITH ITS ROW** (2026-09-22). A PARTIAL
                // release was a verdict on a BLANK — the tail a torn read
                // cut off — and nothing else. Once one glyph it released is
                // back exactly, the torn present is over, and the part is
                // lifted whole:
                // the walk that follows on this same sample then reads its
                // cells as the live cells they were, and gives exactly the
                // verdict the complete frame would have had without the
                // torn one — the same glyph stands (D2), a different one is
                // named and goes through the shape law like any other.
                // Demanding every glyph exactly back held the part hostage
                // to text that comes back MOVED: a torn read under a
                // mid-line insert releases the letters just typed with the
                // tail the insert pushes right, the tail returns one column
                // on, and the letters — back under their own light — were
                // never lifted and retracted out of the middle of the band
                // (the owner's hole under `INTO`, measured at the host seam
                // for 40 frames from ONE torn present). A never-armed cell
                // (a released blank, the part's spaces) constrains nothing.
                //
                // **AND A COLUMN STILL BLANK IS TEXT THAT REALLY WENT**
                // (2026-09-22, the review round). The lift also demanded
                // that no released glyph be under a blank. A Backspace or
                // ⌃W erases one for real, and its column is blank on the
                // COMPLETE present too — so a torn edit's part was never
                // lifted, and the standing text right of the boundary
                // retracted while its glyph stood: 38 cells dark within
                // 160 ms, a 29-cell hole for ~1 s once the hand typed on.
                // The clause is gone. Nothing is lost by dropping it,
                // because the walk runs on this very sample straight after
                // the lift and names a column that is still blank, as a run
                // of its own on its own clock — which is precisely what
                // lifting the part is FOR. A part lifted over a frame that
                // is still incomplete is simply re-released the same frame.
                // Measured in `tests/torn_edit_gaps.rs`.
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
                .map(|r| (r.cohort, r.stamp)),
        );
    }

    /// Clear release custody only after the ribbon accepted restoration.
    /// Already-counted identities stay counted if they are cleared again.
    /// A cell still LEAVING in an accepted cohort belongs to another
    /// partial release the ribbon did not lift (2026-09-22: each part is
    /// restored on its own) and keeps its custody record for that one.
    pub(super) fn restored(&mut self, cohorts: &[u32], cells: &[Cell]) {
        if cohorts.is_empty() {
            return;
        }
        for cell in cells
            .iter()
            .filter(|c| !c.leaving() && cohorts.binary_search(&c.cohort).is_ok())
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
    ///
    /// **ONE GLYPH IS NOT A WORD** (2026-09-22). A run of ONE blanked glyph
    /// is never ruled moved: a single letter stands somewhere on almost any
    /// sampled row, so its "match" is coincidence, not the text going
    /// elsewhere. Measured at the host seam: the first key of a line, its
    /// run blanked whole by a spinner repaint whose first 1024-byte read
    /// ended before the composer row was written, matched the `z` on the
    /// row the frame was writing and took the melt — the band started one
    /// cell late for the rest of the line. A one-glyph run whose text went
    /// is released, and leaves through the retract.
    ///
    /// GLYPHS, NOT CELLS (2026-09-22, the review round). The floor counted
    /// `run.len()`, which is CELLS, and a wide glyph owns two of them — so
    /// one CJK character cleared the floor and took the melt on the very
    /// coincidence the floor was written against, in the languages where
    /// one glyph really is a word. The continuation half of a wide cell is
    /// not a glyph and is not counted.
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
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty());
        assert_eq!(w.len(), 6, "every glyph armed");
        (w, cells)
    }

    /// The follow pass's verdict once row 5 is blank and row 4 reads `now`.
    fn follow_onto(w: &mut Witness, cells: &[Cell], now: &str) -> Vec<FollowRun> {
        let mut out = Vec::new();
        w.follow_runs(
            cells,
            &[
                RowSample {
                    row: 5,
                    cols: &row("      "),
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

    /// **THE ONE-BLOCK LAW** ([`Witness::follow_runs`]): the records a
    /// neighbour row matches must be one block — a prefix or a suffix of the
    /// run, never letters either side of a hole. `abc  f` a row up keeps
    /// four of six letters, a block of three of them first — enough by
    /// count — but the `f` past the hole says it is a coincidence: nothing
    /// is named. The positive controls: the prefix `abcd` (the moved-word
    /// shape) and the whole run are named, with their extents.
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

    /// **AN ERASE IS NOT A MOVE** (2026-09-22 review): the same run, armed
    /// while the row above ALREADY held `abcdef` — the previous command
    /// typed again below itself. When the run's own row is blanked (Ctrl-U)
    /// its glyphs are gone and a block of them stands a row up, but that
    /// block never ARRIVED: nothing is named. The control is the run armed
    /// over a blank row above, where the same final screen IS a move.
    #[test]
    fn a_block_already_standing_beside_the_run_when_it_was_armed_is_not_followed() {
        let (mut w, cells) = armed_run("abcdef");
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
        let (mut w, cells) = armed_run("      ");
        assert_eq!(follow_onto(&mut w, &cells, "abcdef").len(), 1);
        // A twin on part of the run only: the rest may still be a block.
        let (mut w, cells) = armed_run("abc   ");
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            vec![FollowRun {
                row: 5,
                cohort: 7,
                lo: 3,
                hi: 5,
                dr: -1,
            }],
            "only the glyphs that arrived are found"
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

    /// **A RELEASE TAKES NO SPACE FROM BETWEEN STANDING LETTERS**
    /// (2026-09-21, `wrapped_composer_band`'s `c2_bs`). `ab cd ef` with the
    /// last letter blanked: the blanked `f` is released, and so is nothing
    /// between the letters still standing — the spaces at 2 and 5 stay lit
    /// with them. Clear the tail from 4 instead and the space at 5, past
    /// the standing `ab c`, goes with the release (no speck); clear the
    /// whole row and every blank goes (the stray law, as before).
    ///
    /// RED before 2026-09-21: the first walk released columns 2, 5 and 7 —
    /// a comb over a line whose letters were all still on glass.
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

    /// **ONE GLYPH READ BLANK FOR A WALK TAKES NO SPACE WITH IT**
    /// (2026-09-21 — the owner, in Claude Code's composer: the spaces
    /// between typed phrases dark, every glyph lit). `hello world` as one
    /// run; a row sampled mid-rewrite reads the last letter blank for one
    /// walk — the run's LAST recorded cell, a one-cell suffix, which the
    /// shape pass lets stand as named (a lone INTERIOR blank it already
    /// refuses as evidence). The run's other glyphs still stand, so the
    /// release names that one cell and NOT the never-armed space at 5 — which keeps its light
    /// and its clock (it is never named, so `Ribbon::release_cells` never
    /// stamps it) and takes no released record. The glyph back on the next
    /// walk restores the run by its recorded identities. The control is a
    /// suffix cleared from `w` on: the space inside the lost span still goes
    /// with it (`tests/partial_release.rs`'s law).
    ///
    /// RED before the span rule: `rel=[(5, 10), (5, 5)]`, `w.len() == 11`.
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
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty());
        assert_eq!(w.len(), 11, "the new key's cell is armed");
        // The glyph back: the run's recorded identities are all standing,
        // and the released record's ink returned — restorable, the untagged
        // new cell notwithstanding.
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
        // The control: a RETYPE over the released column — a new cell born
        // later at col 10, its glyph the same `d` — is the untagged new
        // birth, and refuses.
        let mut w2 = w.clone();
        let mut retyped: Vec<Cell> = word.clone();
        retyped.push(cell_of(5, 10, t1, 7));
        w2.walk(
            &retyped,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
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
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty() && rel.is_empty(), "the row stands whole");
        assert_eq!(n, 0);
        assert_eq!(w.len(), 11);
        // The control: the suffix cleared from `w` on — the space at 5 is
        // outside the lost span and stands; a suffix cleared from `o` on
        // takes the space inside it.
        let mut w = Witness::new();
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello world"),
            }],
            &mut out,
            &mut rel,
        );
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello      "),
            }],
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
            &mut out,
            &mut rel,
        );
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hell       "),
            }],
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

    /// **THE TAIL GOES WITH THE SUFFIX** (2026-09-21, the review's host-seam
    /// probe). `hello world ` — twelve keys, the last a typed trailing
    /// space at 11, never armed — and the program clears from `w` on: the
    /// span rule alone released 6..10 and left the space at 11 lit, five
    /// dark cells past `hello ` — the one-cell stray the blank law was
    /// written for. A never-armed cell of a released run with glyphs still
    /// standing goes with the release when it is inside the lost span OR
    /// when no standing recorded glyph of the run lies beyond it on its
    /// side: 11 is right of `hi = 10` with nothing standing right of 10, so
    /// it goes; 5 is left of `lo = 6` with `hello` standing left of 6, so it
    /// stands. The mirror: ` hello world` with `hello` cleared takes the
    /// leading space at 0 (nothing stands left of 1) and leaves the space at
    /// 6 (`world` stands right of 5).
    ///
    /// RED before the tail closure: `rel=[6, 7, 8, 9, 10]` (11 lit), and
    /// `rel=[1, 2, 3, 4, 5]` (0 lit).
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

    /// **A RECORD ARMED AFTER THE RELEASE WHOSE GLYPH CHANGED IS NEUTRAL**
    /// (2026-09-21, the review). `hello world`, one glyph released for a
    /// walk; a key lands on the never-armed column 11 and is armed
    /// (`x`); its glyph is then rewritten differently (`y`) as the old
    /// text returns. The record at 11 was armed after the release and
    /// makes no claim on the run's OLD text — a changed glyph is the
    /// walk's to retire on its own — so it neither authorizes nor refuses:
    /// the restore is ACCEPTED, and the walk after it retires exactly the
    /// rewritten cell. Read as a veto it left the stamped `d` melting under
    /// text that was back.
    ///
    /// RED before: `restore=[]`.
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
            &mut out,
            &mut rel,
        );
        w.walk(
            &word,
            &[RowSample {
                row: 5,
                cols: &row("hello worl "),
            }],
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

    /// **A RUN THAT STILL STANDS HAS NOT MOVED** (2026-09-22). The owner's
    /// hole: a torn read blanks `TO` at the end of a line whose `TO` also
    /// stands earlier on it. The blanked fragment of a run with glyphs
    /// still in place is RELEASED — restorable — never searched for and
    /// melted as moved text.
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
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab TO cd   "),
            }],
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "nothing melts: {:?}", pos(&out));
        let mut got = pos(&rel);
        got.sort_unstable();
        assert_eq!(got, vec![(5, 9), (5, 10)], "the blanked `TO` is released");
    }

    /// **ONE GLYPH IS NOT A WORD** (2026-09-22). A run of one blanked glyph
    /// is never ruled moved text, wherever its letter stands.
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
            &mut out,
            &mut rel,
        );
        assert!(out.is_empty(), "a lone `z` elsewhere moves nothing");
        assert_eq!(pos(&rel), vec![(5, 0)]);
    }

    /// **ONE WIDE GLYPH IS ONE GLYPH** (2026-09-22, the review round). The
    /// floor counted cells, and a wide glyph owns two — so one CJK
    /// character standing anywhere else took the melt. It is released.
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

    /// **A PARTIAL RELEASE TAKES ITS OWN SPACES, NOT THE RUN'S** (2026-09-22).
    /// A released tail of a run whose text still stands takes the spaces on
    /// its released side and none inside the standing text; a run with
    /// nothing standing still takes all of them.
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
            &mut out,
            &mut rel,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("ab cd      "),
            }],
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
                &[],
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
