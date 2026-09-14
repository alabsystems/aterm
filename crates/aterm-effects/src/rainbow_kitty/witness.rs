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
//! * glyph → a DIFFERENT glyph, or glyph → blank, RETIRES the cell on the
//!   fast melt (`Ribbon::retire_cells`, [`super::ribbon::RETIRE_MELT_S`]) —
//!   both cells of a wide glyph as one unit, whichever half changed;
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
//!   stray. A typed run is one object, so when a walk retires any cell of a
//!   cohort, every never-armed cell of that cohort on that row is retired
//!   with it. A redraw of the same text retires nothing, so the spaces
//!   stay with their words (D2).
//!
//! D2 holds by construction: the host samples AFTER the PTY batch is
//! applied, so an erase followed by the same text at the same cells inside
//! one batch (every zsh prompt redraw, every Ctrl-L) is seen as the same
//! glyph and touches nothing. The witness is a fixed-capacity, sorted,
//! resident list ([`WITNESS_CAP`]); the walk is `O(cells · log entries)`,
//! allocates nothing past warm-up, and runs only while rainbow kitty owns
//! the frame.
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
}

/// The witness: the sorted `(row, col, born)` list of cells seen holding a
/// glyph.
#[derive(Clone, Debug, Default)]
pub struct Witness {
    /// Sorted by `(row, col, born)`, one record per cell, resident and
    /// reused.
    seen: Vec<Seen>,
    /// `(row, cohort)` of every run a walk retired a cell of — the runs
    /// whose never-armed blanks go with them. Resident scratch, cleared per
    /// walk.
    retired_runs: Vec<(u16, u32)>,
}

impl Witness {
    /// An empty witness with its capacity reserved once ([`WITNESS_CAP`]).
    #[must_use]
    pub fn new() -> Self {
        Self {
            seen: Vec::with_capacity(WITNESS_CAP),
            retired_runs: Vec::new(),
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

    /// The record for the cell `(row, col, born)`, if any.
    fn find(&self, row: u16, col: u16, born: Instant) -> Option<usize> {
        self.seen
            .binary_search_by(|s| (s.row, s.col, s.born).cmp(&(row, col, born)))
            .ok()
    }

    /// Arm a record for `cell` holding `unit` — unless the unit is blank
    /// (blank → glyph only arms, later) or the witness is full.
    fn arm(&mut self, cell: &Cell, unit: Unit) {
        if unit.is_blank() || self.seen.len() >= WITNESS_CAP {
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
    /// whose recorded glyph has changed or gone has its unit pushed onto
    /// `retire` BY IDENTITY, `(row, col, born)` (cleared here, so the caller
    /// reads exactly this walk's verdicts) and its record dropped; a cell
    /// re-laid since (a different `born`) is a different cell and is armed
    /// on its own. Then every never-armed cell of a run one of whose cells
    /// was just retired goes with it (the blank in a word). Records whose
    /// cells the walk never reached — dropped by the ribbon — are
    /// forgotten. A cell may be pushed twice (a wide unit's two halves both
    /// resident); `Ribbon::retire_cells` stamps it once.
    pub fn walk(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        retire: &mut Vec<(u16, u16, Instant)>,
    ) {
        retire.clear();
        self.retired_runs.clear();
        for s in &mut self.seen {
            s.live = false;
        }
        for cell in cells {
            if cell.leaving() {
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
                    } else {
                        Self::push_unit(retire, cell.row, cell.col, cell.born, seen.unit);
                        let run = (cell.row, cell.cohort);
                        if !self.retired_runs.contains(&run) {
                            self.retired_runs.push(run);
                        }
                    }
                }
                None => self.arm(cell, unit),
            }
        }
        // THE BLANKS GO WITH THEIR RUN: a cell of a retired run that holds no
        // record at all — never armed, because nothing was ever under it —
        // is retired with its run-mates. A cell WITH a record was read above:
        // matched and kept, or changed and already named.
        if !self.retired_runs.is_empty() {
            for cell in cells {
                if cell.leaving() || !self.retired_runs.contains(&(cell.row, cell.cohort)) {
                    continue;
                }
                if self.find(cell.row, cell.col, cell.born).is_none() {
                    retire.push((cell.row, cell.col, cell.born));
                }
            }
        }
        self.seen.retain(|s| s.live);
    }
}

#[cfg(test)]
mod tests {
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
    fn blank_to_glyph_only_arms_and_glyph_to_blank_retires() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let cells = [cell(5, 0, t0), cell(5, 1, t0)];
        // The echo landed a frame late: nothing under the cells yet.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("  "),
            }],
            &mut out,
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
        );
        assert!(
            out.is_empty() && w.len() == 2,
            "the same glyph touches nothing"
        );
        // Then the row is blank: both go.
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("  "),
            }],
            &mut out,
        );
        assert_eq!(pos(&out), vec![(5, 0), (5, 1)]);
        assert!(
            out.iter().all(|&(_, _, born)| born == t0),
            "named by identity: the cells' own births"
        );
        assert!(w.is_empty(), "retired records are dropped");
    }

    #[test]
    fn a_different_glyph_retires_and_a_relaid_cell_arms_afresh() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let cells = [cell(5, 0, t0)];
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("h"),
            }],
            &mut out,
        );
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("H"),
            }],
            &mut out,
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
        );
        assert!(out.is_empty());
    }

    #[test]
    fn a_wide_glyph_s_two_cells_are_retired_as_one_unit() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
        let cells = [cell(5, 3, t0), cell(5, 4, t0)];
        w.walk(
            &cells,
            &[RowSample {
                row: 5,
                cols: &row("   \u{4f60}\0"),
            }],
            &mut out,
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
        );
        w.walk(
            &cont,
            &[RowSample {
                row: 5,
                cols: &row("     "),
            }],
            &mut out,
        );
        assert_eq!(pos(&out), vec![(5, 4), (5, 3)]);
        assert!(
            out.iter().all(|&(_, _, born)| born == t0),
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
            );
            assert!(out.is_empty(), "stale first = {stale_first}");
        }
    }

    #[test]
    fn a_blank_in_a_word_goes_with_its_run_and_stays_with_it_on_a_redraw() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
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
        );
        let mut got = pos(&out);
        got.sort_unstable();
        got.dedup();
        assert_eq!(got, vec![(5, 0), (5, 1), (5, 2), (5, 3), (5, 4)]);
        assert_eq!(w.len(), 2, "the other run's records stand");
    }

    #[test]
    fn an_unsampled_row_keeps_its_records_and_a_dropped_cell_is_forgotten() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let mut out = Vec::new();
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
        );
        assert_eq!(pos(&out), vec![(0, 0)]);
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
        let cells = [cell(2, 5, t0), cell(3, 1, t0), cell(5, 0, t0)];
        let glyphs = row("abcdef");
        let samples: Vec<RowSample<'_>> = [2u16, 3, 5]
            .iter()
            .map(|&r| RowSample {
                row: r,
                cols: &glyphs,
            })
            .collect();
        w.walk(&cells, &samples, &mut out);
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
        w.walk(&moved, &samples, &mut out);
        assert!(
            out.is_empty() && w.len() == 3,
            "every record followed its cell and none was re-armed"
        );
        // The text under the moved row-3 cell is now blank: found by the
        // binary search on the re-sorted list, and retired.
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
        );
        assert_eq!(pos(&out), vec![(2, 5), (2, 1)]);
        // A band move that carries a record past the top edge drops it.
        let mut w = Witness::new();
        w.walk(
            &[cell(3, 0, t0)],
            &[RowSample {
                row: 3,
                cols: &glyphs,
            }],
            &mut out,
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
        w.walk(&cells, &samples, &mut out);
        assert_eq!(w.len(), WITNESS_CAP);
        assert_eq!(w.seen.capacity(), cap, "no growth past the reserved cap");
    }
}
