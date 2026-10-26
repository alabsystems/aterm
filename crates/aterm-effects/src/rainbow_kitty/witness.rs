// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The content witness: the ribbon's one content-aware retirement, and the engine half of a seam the host feeds from its terminal lock.

use aterm_time::{Duration, Instant};

use crate::cursor_glow::band_row;

use super::ribbon::Cell;

/// Witness records kept: one per resident ribbon cell, at most this many.
pub const WITNESS_CAP: usize = 1024;

/// Rows the bands request per frame for the witness ([`super::Engine::ribbon_rows_for`]).
pub const WITNESS_ROWS: usize = 8;

/// The slots a waiting key's arming rows take after the bands' [`WITNESS_ROWS`] ([`super::Engine::ribbon_rows_for`]).
pub const ARMING_ROWS: usize = 3;

/// One row of the live grid as the host sampled it this frame.
#[derive(Clone, Copy, Debug)]
pub struct RowSample<'a> {
    /// The grid row.
    pub row: u16,
    /// Per-column chars: the lead glyph at its column, `'\0'` at a wide continuation, `' '` for a blank.
    pub cols: &'a [char],
}

/// What one column of a sampled row holds, resolved to its glyph unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unit {
    /// The glyph — the lead char for either half of a wide glyph; `' '` for a blank.
    pub ch: char,
    /// The glyph occupies two cells.
    pub wide: bool,
    /// This column is the continuation half of a wide glyph (its lead is one column left).
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
    #[inline]
    #[must_use]
    pub const fn is_blank(self) -> bool {
        self.ch == ' '
    }

    /// True when two glyph units match in char and wide-cell role.
    #[inline]
    #[must_use]
    pub const fn eq(self, other: Self) -> bool {
        self.ch == other.ch && self.wide == other.wide && self.cont == other.cont
    }
}

/// The glyph unit at `col` of a sampled row.
#[inline]
#[must_use]
pub const fn unit_at(cols: &[char], col: u16) -> Unit {
    let i = col as usize;
    if i >= cols.len() {
        return Unit::BLANK;
    }
    let c = cols[i];
    if c == '\0' {
        let lead = if i > 0 { cols[i - 1] } else { ' ' };
        if lead == '\0' || lead == ' ' {
            Unit::BLANK
        } else {
            Unit {
                ch: lead,
                wide: true,
                cont: true,
            }
        }
    } else if c == ' ' {
        Unit::BLANK
    } else {
        Unit {
            ch: c,
            wide: i + 1 < cols.len() && cols[i + 1] == '\0',
            cont: false,
        }
    }
}

type ShiftCell = (u16, Option<Unit>, bool);

const fn find_shift(
    cells: &[ShiftCell],
    cols: &[char],
    caret: Option<u16>,
) -> Option<(u16, u16, i16)> {
    let mut s_opt = None;
    let mut idx = 0;
    while idx < cells.len() {
        if cells[idx].2 {
            s_opt = Some(idx);
            break;
        }
        idx += 1;
    }
    let Some(s) = s_opt else {
        return None;
    };
    let (scol, sunit, _) = cells[s];
    let Some(sunit) = sunit else {
        return None;
    };
    let width = if cols.len() <= u16::MAX as usize {
        cols.len() as u16
    } else {
        u16::MAX
    };
    let max_reach = scol.saturating_add(SHIFT_MAX_COLS);
    let reach = if width < max_reach { width } else { max_reach };
    let mut x = scol.saturating_add(1);
    while x < reach {
        if !unit_at(cols, x).eq(sunit) {
            x += 1;
            continue;
        }
        let span = x - scol;
        if span > i16::MAX as u16 {
            break;
        }
        let dc = span as i16;
        if let Some(v) = shift_block(cells, cols, s, dc, caret) {
            return Some(v);
        }
        x += 1;
    }
    None
}

const fn shift_block(
    cells: &[ShiftCell],
    cols: &[char],
    b: usize,
    dc: i16,
    caret: Option<u16>,
) -> Option<(u16, u16, i16)> {
    #[inline]
    const fn at(cols: &[char], dc: i16, col: u16) -> Unit {
        match col.checked_add_signed(dc) {
            Some(c) => unit_at(cols, c),
            None => Unit::BLANK,
        }
    }

    let mut found = 0usize;
    let mut tail = false;
    let mut hi = None;
    let mut overwritten = false;
    let mut i = b;
    while i < cells.len() {
        let (col, rec, _) = cells[i];
        let there = at(cols, dc, col);
        match rec {
            Some(unit) => {
                if !tail && there.eq(unit) {
                    found += 1;
                    hi = Some(i);
                    overwritten |= !unit_at(cols, col).is_blank();
                } else if there.is_blank() {
                    tail = true;
                } else {
                    return None;
                }
            }
            None => {
                let on_row =
                    matches!(col.checked_add_signed(dc), Some(c) if (c as usize) < cols.len());
                if !tail && on_row && there.is_blank() {
                    hi = Some(i);
                } else {
                    tail = true;
                }
            }
        }
        i += 1;
    }
    let Some(hi) = hi else {
        return None;
    };
    if found == 0 || !overwritten {
        return None;
    }
    let mut lo = b;
    while lo > 0 {
        let (pcol, prec, _) = cells[lo - 1];
        if !matches!(pcol.checked_add(1), Some(next) if next == cells[lo].0) {
            break;
        }
        let there = at(cols, dc, pcol);
        let explained = match prec {
            Some(unit) => there.eq(unit),
            None => there.is_blank(),
        };
        if !explained {
            break;
        }
        lo -= 1;
    }
    let mut keyed = None;
    if let Some(c) = caret {
        let mut k = lo;
        while k <= b {
            if cells[k].0 == c {
                keyed = Some(k);
                break;
            }
            k += 1;
        }
    }
    let lo = match keyed {
        Some(k) => k,
        None => lo,
    };
    if found < 2 {
        let end = match cells[hi].0.checked_add_signed(dc) {
            Some(c) => c as usize,
            None => usize::MAX,
        };
        let mut text_past = false;
        let mut idx = end.saturating_add(1);
        while idx < cols.len() {
            let c = cols[idx];
            if c != ' ' && c != '\0' {
                text_past = true;
                break;
            }
            idx += 1;
        }
        if keyed.is_none() || tail || text_past {
            return None;
        }
    }
    Some((cells[lo].0, cells[hi].0, dc))
}

#[derive(Clone, Copy, Debug)]
struct Seen {
    row: u16,
    col: u16,
    born: Instant,
    unit: Unit,
    live: bool,
    released: bool,
    restorable: bool,
    counted: bool,
    clear: u8,
    twin: u8,
    pending: u8,
    pend_ms: [u32; 4],
    deferred: bool,
}

/// How long a copy must stand beside a line, seen on every sampled frame between, to be its twin ([`Seen::pending`]).
pub const TWIN_MIN: Duration = Duration::from_millis(40);
const TWIN_MIN_MS: u32 = TWIN_MIN.as_millis() as u32;

/// The follow pass's row offsets, nearest first: one row up, one row down, then two.
pub(super) const FOLLOW_DRS: [i16; 4] = [-1, 1, -2, 2];

#[inline]
const fn row_sample_at<'a>(rows: &[RowSample<'a>], row: u16) -> Option<RowSample<'a>> {
    let mut j = 0;
    while j < rows.len() {
        if rows[j].row == row {
            return Some(rows[j]);
        }
        j += 1;
    }
    None
}

#[inline]
const fn neighbours_of<'a>(rows: &[RowSample<'a>], row: u16) -> [Option<&'a [char]>; 4] {
    let mut out = [None; 4];
    let mut k = 0;
    while k < FOLLOW_DRS.len() {
        if let Some(r) = row.checked_add_signed(FOLLOW_DRS[k])
            && let Some(s) = row_sample_at(rows, r)
        {
            out[k] = Some(s.cols);
        }
        k += 1;
    }
    out
}

#[inline]
const fn band_keeps(old: u16, new: u16, top: u16, bottom: u16, delta: i16) -> u8 {
    let mut keep = 0u8;
    let mut k = 0;
    while k < FOLLOW_DRS.len() {
        let dr = FOLLOW_DRS[k];
        if let (Some(was), Some(now)) = (old.checked_add_signed(dr), new.checked_add_signed(dr))
            && matches!(band_row(was, top, bottom, delta), Some(r) if r == now)
        {
            keep |= 1 << k;
        }
        k += 1;
    }
    keep
}

impl Seen {
    #[inline]
    fn observe(&mut self, nb: &[Option<&[char]>; 4], at_arm: bool, now: Instant) {
        if self.unit.is_blank() {
            return;
        }
        let age =
            u32::try_from(now.saturating_duration_since(self.born).as_millis()).unwrap_or(u32::MAX);
        let mut k = 0;
        while k < nb.len() {
            let Some(cols) = nb[k] else {
                k += 1;
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
            } else if age.saturating_sub(self.pend_ms[k]) >= TWIN_MIN_MS {
                self.twin |= bit;
            }
            k += 1;
        }
    }

    #[inline]
    const fn forget_evidence(&mut self) {
        self.clear = ALL_OFFSETS;
        self.twin = 0;
        self.pending = 0;
    }

    #[inline]
    const fn arrivals(&self) -> u8 {
        self.clear & !self.twin & ALL_OFFSETS
    }
}

const ALL_OFFSETS: u8 = 0x0F;

const RETURN_K: usize = FOLLOW_DRS.len();

const RETURN_BIT: u8 = 1 << RETURN_K;

const CARRY_K: usize = RETURN_K + 1;

const CARRY_BIT: u8 = 1 << CARRY_K;

#[derive(Clone, Copy, Debug)]
struct Blanked {
    row: u16,
    col: u16,
    born: Instant,
    cohort: u32,
    unit: Unit,
    held: bool,
    counted: bool,
}

#[derive(Clone, Copy, Debug)]
struct BlindRecord {
    row: u16,
    cohort: u32,
    col: u16,
    changed: bool,
    deferred: bool,
}

#[derive(Clone, Copy, Debug)]
struct RestoreRun {
    row: u16,
    cohort: u32,
    stamp: Option<Instant>,
    exact: bool,
    returned_ink: bool,
}

/// The witness: the sorted `(row, col, born)` list of cells seen holding a glyph.
#[derive(Clone, Debug, Default)]
pub struct Witness {
    seen: Vec<Seen>,
    retired_runs: Vec<(u16, u32)>,
    standing_runs: Vec<(u16, u32)>,
    retired_cells: Vec<(u16, u16, Instant)>,
    released_runs: Vec<(u16, u32)>,
    standing_glyphs: Vec<(u16, u32, u16)>,
    shape_runs: Vec<(u16, u32)>,
    fresh_released: Vec<(u16, u16, Instant)>,
    counted_names: Vec<(u16, u16, Instant)>,
    run_ids: Vec<(u16, u16, Instant)>,
    blanked: Vec<Blanked>,
    restore_runs: Vec<RestoreRun>,
    follow_runs: Vec<(u16, u32)>,
    follow_armed: Vec<(u16, Unit, bool, u8)>,
    blank_seen: Vec<(u16, u16, Instant)>,
    blank_next: Vec<(u16, u16, Instant)>,
    landed: Vec<(u16, u16, Instant, u32)>,
    blind_changed: Vec<BlindRecord>,
    deferred_runs: Vec<(u16, u32)>,
    awaiting: Option<(u16, u32)>,
    last_walk: Option<Instant>,
    scroll_carry: u16,
    caret_carry: i16,
    shift_cells: Vec<(u16, Option<Unit>, bool)>,
}

/// A run the shift pass found along its own row ([`Witness::shift_runs`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShiftRun {
    /// The run's row.
    pub row: u16,
    /// The run's cohort.
    pub cohort: u32,
    /// The leftmost column that moves — the insert's edit point: the new text stands on `lo..lo + dc`.
    pub lo: u16,
    /// The rightmost column that moves.
    pub hi: u16,
    /// Columns to the text: the inserted width, always positive.
    pub dc: i16,
}

/// The widest same-row shift searched for (a paste of a full wide line).
pub const SHIFT_MAX_COLS: u16 = 512;

/// A run the follow pass found elsewhere ([`Witness::follow_runs`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FollowRun {
    /// The run's row now.
    pub row: u16,
    /// The run's cohort.
    pub cohort: u32,
    /// The leftmost column carried: the found block's, grown over its neutral and never-armed border ([`Witness::follow_runs`]).
    pub lo: u16,
    /// The rightmost column carried, grown the same way.
    pub hi: u16,
    /// Rows to the text: `−1` one row up, `+1` one row down, then `±2`.
    pub dr: i16,
}

/// What a follow pass saw beside what it named ([`Witness::follow_runs`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FollowScore {
    /// Missed follows: armed cells gone from their row whose text arrived on a neighboring row without forming a single block (`ribbon_follow_missed=`).
    pub missed: u32,
}

#[derive(Clone, Copy, Debug)]
struct FollowVerdict {
    found: usize,
    neutral: usize,
    relaid: usize,
    lo: u16,
    hi: u16,
    block: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SoleArrival {
    Refused,
    ShortPark,
    Hand { glyph: u16, to: u16 },
}

impl Witness {
    /// An empty witness with capacity reserved once ([`WITNESS_CAP`]).
    #[inline]
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
            awaiting: None,
            last_walk: None,
            scroll_carry: 0,
            caret_carry: 0,
            shift_cells: Vec::new(),
        }
    }

    /// Rows full-grid scrolls carried the records up since the last walk, while past the follow offsets (`> 2`), else `0`.
    #[inline]
    #[must_use]
    pub const fn scroll_carry(&self) -> u16 {
        if self.scroll_carry > 2 {
            self.scroll_carry
        } else {
            0
        }
    }

    /// THE CARET'S CARRY: this frame's caret stands `dr` rows from the caret mirror, the hand's row, with no key to say why (a relocation the licence gate declined, `Engine::observe_caret`).
    #[inline]
    pub(crate) const fn carry_caret(&mut self, dr: i16) {
        let scroll = self.scroll_carry();
        let ret = if scroll > i16::MAX as u16 {
            i16::MAX
        } else {
            scroll as i16
        };
        self.caret_carry = if dr.unsigned_abs() > 2 && dr != ret {
            dr
        } else {
            0
        };
    }

    /// The caret's carry pending for this frame's follow pass ([`Witness::carry_caret`]), `0` for none.
    #[inline]
    #[must_use]
    pub(crate) const fn caret_carry(&self) -> i16 {
        self.caret_carry
    }

    /// The follow pass searched the carries ([`Witness::scroll_carry`], [`Witness::carry_caret`]) on this frame's rows.
    #[inline]
    pub(crate) const fn end_scroll_carry(&mut self) {
        self.scroll_carry = 0;
        self.caret_carry = 0;
    }

    /// The follow pass: name on `out` every run whose text moved vertically as a block.
    #[inline]
    pub fn follow_runs(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        out: &mut Vec<FollowRun>,
    ) -> FollowScore {
        self.follow_runs_with_short_park(cells, rows, None, out)
    }

    /// [`Witness::follow_runs`], except that a sole arriving glyph is evidence for the exact short held park whose source row the host is judging.
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
        let short_owner = if let Some((from, to)) = short_park
            && from.0 == to.0
            && {
                let d = from.1.saturating_sub(to.1);
                d == 1 || d == 2
            }
            && let Some(last_col) = from.1.checked_sub(1)
        {
            let mut owner = None;
            let mut ci = 0;
            while ci < cells.len() {
                let cell = &cells[ci];
                if !cell.leaving() && cell.row == from.0 && cell.col == last_col {
                    owner = Some((from.0, cell.cohort));
                    break;
                }
                ci += 1;
            }
            owner
        } else {
            None
        };
        let mut sample_row: Option<(u16, bool)> = None;
        let mut ci = 0;
        while ci < cells.len() {
            let cell = &cells[ci];
            ci += 1;
            if !cell.leaving() {
                let run = (cell.row, cell.cohort);
                if self.follow_runs.last() == Some(&run) {
                    continue;
                }
                let sampled = match sample_row {
                    Some((r, ok)) if r == cell.row => ok,
                    _ => {
                        let ok = row_sample_at(rows, cell.row).is_some();
                        sample_row = Some((cell.row, ok));
                        ok
                    }
                };
                if sampled {
                    self.follow_runs.push(run);
                }
            }
        }
        self.follow_runs.sort_unstable();
        self.follow_runs.dedup();
        let mut k = 0;
        while k < self.follow_runs.len() {
            let (row, cohort) = self.follow_runs[k];
            let sole = if short_owner == Some((row, cohort)) {
                SoleArrival::ShortPark
            } else {
                SoleArrival::Refused
            };
            let (run, missed) = self.judge_follow(cells, rows, (row, cohort), false, sole);
            out.extend(run);
            score.missed = score
                .missed
                .saturating_add(u32::try_from(missed).unwrap_or(u32::MAX));
            k += 1;
        }
        score
    }

    /// THE HAND'S OWN LINE, judged on the hand's typed records.
    pub(crate) fn follow_hand(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        hand: (u16, u16),
        rode_to: Option<u16>,
    ) -> Option<FollowRun> {
        let last = hand.1.checked_sub(1)?;
        let mut best_cell: Option<(Instant, u32)> = None;
        let mut ci = 0;
        while ci < cells.len() {
            let c = &cells[ci];
            if !c.leaving()
                && c.typing
                && c.row == hand.0
                && c.col == last
                && !matches!(best_cell, Some((b, _)) if c.born < b)
            {
                best_cell = Some((c.born, c.cohort));
            }
            ci += 1;
        }
        let cohort = best_cell?.1;
        let mut glyph: Option<u16> = None;
        let mut cj = 0;
        while cj < cells.len() {
            let c = &cells[cj];
            if !c.leaving()
                && c.typing
                && c.row == hand.0
                && c.cohort == cohort
                && c.col <= last
                && !matches!(glyph, Some(g) if c.col <= g)
                && matches!(
                    self.find(c.row, c.col, c.born),
                    Some(i) if !self.seen[i].released
                )
            {
                glyph = Some(c.col);
            }
            cj += 1;
        }
        let glyph = glyph?;
        let sole = match rode_to {
            Some(to) => SoleArrival::Hand { glyph, to },
            None => SoleArrival::Refused,
        };
        self.judge_follow(cells, rows, (hand.0, cohort), true, sole)
            .0
    }

    fn judge_follow(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        (row, cohort): (u16, u32),
        typed_only: bool,
        sole: SoleArrival,
    ) -> (Option<FollowRun>, usize) {
        let Some(own) = row_sample_at(rows, row) else {
            return (None, 0);
        };
        let carry = self.scroll_carry();
        let caret_carry = self.caret_carry;
        let ret =
            if carry > 0 { RETURN_BIT } else { 0 } | if caret_carry != 0 { CARRY_BIT } else { 0 };
        self.follow_armed.clear();
        let mut ci = 0;
        while ci < cells.len() {
            let cell = &cells[ci];
            ci += 1;
            if cell.leaving()
                || cell.row != row
                || cell.cohort != cohort
                || (typed_only && !cell.typing)
            {
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
                .push((cell.col, seen.unit, gone, seen.arrivals() | ret));
        }
        self.follow_armed.sort_unstable_by_key(|a| a.0);
        self.follow_armed.dedup_by_key(|a| a.0);
        let armed = self.follow_armed.len();
        let mut gone = 0usize;
        let mut ai = 0;
        while ai < armed {
            if self.follow_armed[ai].2 {
                gone += 1;
            }
            ai += 1;
        }
        let floor = if matches!(sole, SoleArrival::Hand { .. }) {
            1
        } else {
            2
        };
        if armed < floor || gone < floor || gone * 2 < armed {
            return (None, 0);
        }
        let relaid = Self::relaid_suffix(&self.follow_armed, own.cols);
        let mut evidence = false;
        let mut offsets = [(0usize, 0i16); 6];
        let mut n_off = 0usize;
        if let Ok(dr) = i16::try_from(carry)
            && dr > 0
        {
            offsets[n_off] = (RETURN_K, dr);
            n_off += 1;
        }
        if caret_carry != 0 {
            offsets[n_off] = (CARRY_K, caret_carry);
            n_off += 1;
        }
        let mut fi = 0usize;
        while fi < FOLLOW_DRS.len() {
            offsets[n_off] = (fi, FOLLOW_DRS[fi]);
            n_off += 1;
            fi += 1;
        }
        let mut oi = 0;
        while oi < n_off {
            let (k, dr) = offsets[oi];
            oi += 1;
            let Some(target) = row.checked_add_signed(dr) else {
                continue;
            };
            let Some(there) = row_sample_at(rows, target) else {
                continue;
            };
            let v = Self::follow_verdict(&self.follow_armed, there.cols, k, relaid);
            let bit = 1u8 << k;
            let counted = armed.saturating_sub(v.neutral + v.relaid);
            let sole_stands = v.found == 1
                && counted == 1
                && match sole {
                    SoleArrival::Refused => false,
                    SoleArrival::ShortPark => v.relaid >= 3,
                    SoleArrival::Hand { glyph, to } => {
                        target == to
                            && if let Ok(i) =
                                self.follow_armed.binary_search_by_key(&glyph, |a| a.0)
                            {
                                let (col, unit, _, arrivals) = self.follow_armed[i];
                                arrivals & bit != 0 && unit_at(there.cols, col) == unit
                            } else {
                                false
                            }
                    }
                };
            let enough = (v.found >= 2 || sole_stands) && v.found * 2 >= counted;
            evidence |= enough;
            if !v.block || !enough {
                continue;
            }
            let (mut lo, mut hi) = (v.lo, v.hi);
            #[inline]
            const fn is_neutral(
                there_cols: &[char],
                bit: u8,
                col: u16,
                unit: Unit,
                arrivals: u8,
            ) -> bool {
                arrivals & bit == 0 && unit_at(there_cols, col).eq(unit)
            }
            #[inline]
            const fn has_run_cell(
                cells: &[Cell],
                k: usize,
                row: u16,
                cohort: u32,
                col: u16,
            ) -> bool {
                let mut i = 0;
                while i < cells.len() {
                    let c = &cells[i];
                    if (!c.leaving() || k >= RETURN_K)
                        && c.row == row
                        && c.cohort == cohort
                        && c.col == col
                    {
                        return true;
                    }
                    i += 1;
                }
                false
            }
            #[inline]
            fn armed_at(
                follow_armed: &[(u16, Unit, bool, u8)],
                col: u16,
            ) -> Option<(u16, Unit, bool, u8)> {
                match follow_armed.binary_search_by_key(&col, |a| a.0) {
                    Ok(i) => Some(follow_armed[i]),
                    Err(_) => None,
                }
            }
            let mut beyond_hi = false;
            let mut idx_hi = self.follow_armed.len();
            while idx_hi > 0 {
                idx_hi -= 1;
                let a = self.follow_armed[idx_hi];
                if a.0 <= hi {
                    break;
                }
                if !is_neutral(there.cols, bit, a.0, a.1, a.3) {
                    beyond_hi = true;
                    break;
                }
            }
            let mut beyond_lo = false;
            let mut idx_lo = 0;
            while idx_lo < self.follow_armed.len() {
                let a = self.follow_armed[idx_lo];
                if a.0 >= lo {
                    break;
                }
                if !is_neutral(there.cols, bit, a.0, a.1, a.3) {
                    beyond_lo = true;
                    break;
                }
                idx_lo += 1;
            }
            while let Some(next) = hi.checked_add(1) {
                let grows = match armed_at(&self.follow_armed, next) {
                    Some(a) => is_neutral(there.cols, bit, a.0, a.1, a.3),
                    None => !beyond_hi && has_run_cell(cells, k, row, cohort, next),
                };
                if !grows {
                    break;
                }
                hi = next;
            }
            while let Some(prev) = lo.checked_sub(1) {
                let grows = match armed_at(&self.follow_armed, prev) {
                    Some(a) => is_neutral(there.cols, bit, a.0, a.1, a.3),
                    None => !beyond_lo && has_run_cell(cells, k, row, cohort, prev),
                };
                if !grows {
                    break;
                }
                lo = prev;
            }
            let run = FollowRun {
                row,
                cohort,
                lo,
                hi,
                dr,
            };
            return (Some(run), 0);
        }
        (None, if evidence { gone } else { 0 })
    }

    const fn follow_verdict(
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
        let bit = 1u8 << k;
        let mut after = false;
        let mut lead: Option<u16> = None;
        let mut ext_hi = 0u16;
        let mut i = 0;
        while i < armed.len() {
            let (col, unit, _, arrivals) = armed[i];
            let here = unit_at(there, col).eq(unit);
            let twin = arrivals & bit == 0;
            if here && !twin {
                v.found += 1;
                if after {
                    v.block = false;
                    i += 1;
                    continue;
                }
                let candidate = match lead {
                    Some(l) => l,
                    None => col,
                };
                if candidate < v.lo {
                    v.lo = candidate;
                }
                lead = None;
                ext_hi = col;
            } else if here {
                v.neutral += 1;
                if v.found == 0 {
                    if lead.is_none() {
                        lead = Some(col);
                    }
                } else if !after {
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
            i += 1;
        }
        v.hi = ext_hi;
        v
    }

    const fn relaid_suffix(armed: &[(u16, Unit, bool, u8)], own: &[char]) -> usize {
        let mut first = None;
        let mut idx = 0;
        while idx < own.len() {
            let c = own[idx];
            if c != ' ' && c != '\0' {
                if idx <= u16::MAX as usize {
                    first = Some(idx as u16);
                }
                break;
            }
            idx += 1;
        }
        let Some(first) = first else {
            return 0;
        };
        let mut start = 0;
        while start < armed.len() {
            let c0 = armed[start].0;
            if c0 > first {
                let shift = c0 - first;
                let mut all_match = true;
                let mut j = start;
                while j < armed.len() {
                    let (col, unit, ..) = armed[j];
                    if !unit_at(own, col - shift).eq(unit) {
                        all_match = false;
                        break;
                    }
                    j += 1;
                }
                if all_match {
                    return armed.len() - start;
                }
            }
            start += 1;
        }
        0
    }

    /// The shift pass: the band follows its text along the row.
    pub fn shift_runs(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        caret: Option<(u16, u16)>,
        now: Instant,
        out: &mut Vec<ShiftRun>,
    ) {
        out.clear();
        self.follow_runs.clear();
        let mut sample_row: Option<(u16, Option<RowSample<'_>>)> = None;
        let mut ci = 0;
        while ci < cells.len() {
            let cell = &cells[ci];
            ci += 1;
            if cell.leaving() {
                continue;
            }
            let run = (cell.row, cell.cohort);
            if self.follow_runs.last() == Some(&run) {
                continue;
            }
            let own = match sample_row {
                Some((r, s)) if r == cell.row => s,
                _ => {
                    let s = row_sample_at(rows, cell.row);
                    sample_row = Some((cell.row, s));
                    s
                }
            };
            let Some(own) = own else {
                continue;
            };
            let gone = matches!(
                self.find(cell.row, cell.col, cell.born),
                Some(i) if unit_at(own.cols, cell.col) != self.seen[i].unit
            );
            if gone {
                self.follow_runs.push(run);
            }
        }
        if self.follow_runs.is_empty() {
            return;
        }
        self.follow_runs.sort_unstable();
        self.follow_runs.dedup();
        let mut k = 0;
        while k < self.follow_runs.len() {
            let (row, cohort) = self.follow_runs[k];
            k += 1;
            let Some(own) = row_sample_at(rows, row) else {
                continue;
            };
            self.shift_cells.clear();
            let mut released = false;
            let mut leaving_max: Option<u16> = None;
            let mut cj = 0;
            while cj < cells.len() {
                let cell = &cells[cj];
                cj += 1;
                if cell.row != row || cell.cohort != cohort {
                    continue;
                }
                if cell.leaving() {
                    let dark_release = cell.released_at.is_some()
                        && matches!(
                            cell.retract_at,
                            Some(at) if now.saturating_duration_since(at).as_secs_f32()
                                >= super::ribbon::RETRACT_FADE_S
                        );
                    if !dark_release {
                        leaving_max = Some(match leaving_max {
                            Some(m) => m.max(cell.col),
                            None => cell.col,
                        });
                    }
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
                let gone = matches!(rec, Some(u) if unit_at(own.cols, cell.col) != u);
                self.shift_cells.push((cell.col, rec, gone));
            }
            if released {
                continue;
            }
            self.shift_cells.sort_unstable_by_key(|c| c.0);
            self.shift_cells.dedup_by_key(|c| c.0);
            let caret_col = match caret {
                Some((r, col)) if r == row => Some(col),
                _ => None,
            };
            if let Some((lo, hi, dc)) = find_shift(&self.shift_cells, own.cols, caret_col)
                && !matches!(leaving_max, Some(m) if m >= lo)
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

    /// The records follow their cells along the row ([`Witness::shift_runs`] → `Ribbon::shift_run`).
    pub fn shift_cells(&mut self, moved: &mut [(u16, u16, Instant)], dc: i16) {
        if moved.is_empty() || dc == 0 {
            return;
        }
        moved.sort_unstable();
        let mut any = false;
        self.seen.retain_mut(|s| {
            if moved.binary_search(&(s.row, s.col, s.born)).is_ok() {
                if let Some(target) = s.col.checked_add_signed(dc) {
                    s.col = target;
                    s.forget_evidence();
                    s.deferred = false;
                    any = true;
                }
                true
            } else {
                !matches!(
                    s.col.checked_add_signed(-dc),
                    Some(src) if moved.binary_search(&(s.row, src, s.born)).is_ok()
                )
            }
        });
        if any {
            self.seen.sort_unstable_by_key(|s| (s.row, s.col, s.born));
        }
        let mut blanks = false;
        let mut bi = 0;
        while bi < self.blank_seen.len() {
            let b = &mut self.blank_seen[bi];
            if moved.binary_search(b).is_ok()
                && let Some(target) = b.1.checked_add_signed(dc)
            {
                b.1 = target;
                blanks = true;
            }
            bi += 1;
        }
        if blanks {
            self.blank_seen.sort_unstable();
            self.blank_seen.dedup();
        }
    }

    /// The records follow their cells ([`Witness::follow_runs`] → `Ribbon::translate_run`).
    pub fn translate_cells(&mut self, moved: &mut [(u16, u16, Instant)], dr: i16) {
        if moved.is_empty() || dr == 0 {
            return;
        }
        moved.sort_unstable();
        let mut any = false;
        let mut si = 0;
        while si < self.seen.len() {
            let s = &mut self.seen[si];
            if moved.binary_search(&(s.row, s.col, s.born)).is_ok()
                && let Some(target) = s.row.checked_add_signed(dr)
            {
                s.row = target;
                s.forget_evidence();
                s.deferred = false;
                any = true;
            }
            si += 1;
        }
        if any {
            self.seen.sort_unstable_by_key(|s| (s.row, s.col, s.born));
        }
        let mut blanks = false;
        let mut bi = 0;
        while bi < self.blank_seen.len() {
            let b = &mut self.blank_seen[bi];
            if moved.binary_search(b).is_ok()
                && let Some(target) = b.0.checked_add_signed(dr)
            {
                b.0 = target;
                blanks = true;
            }
            bi += 1;
        }
        if blanks {
            self.blank_seen.sort_unstable();
        }
    }

    /// Records held.
    #[must_use]
    #[cfg(test)]
    pub const fn len(&self) -> usize {
        self.seen.len()
    }

    /// True with nothing witnessed.
    #[must_use]
    #[cfg(test)]
    pub const fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Forget everything (a reset: the cells are gone with the coordinate space).
    pub fn clear(&mut self) {
        self.seen.clear();
        self.retired_runs.clear();
        self.standing_runs.clear();
        self.retired_cells.clear();
        self.released_runs.clear();
        self.standing_glyphs.clear();
        self.shape_runs.clear();
        self.fresh_released.clear();
        self.counted_names.clear();
        self.run_ids.clear();
        self.blanked.clear();
        self.restore_runs.clear();
        self.follow_runs.clear();
        self.follow_armed.clear();
        self.blank_seen.clear();
        self.blank_next.clear();
        self.landed.clear();
        self.blind_changed.clear();
        self.deferred_runs.clear();
        self.awaiting = None;
        self.last_walk = None;
        self.scroll_carry = 0;
        self.caret_carry = 0;
        self.shift_cells.clear();
    }

    /// THE HAND'S LONE GLYPH WAITS FOR ITS CARET.
    #[inline]
    pub(crate) const fn await_caret(&mut self, run: Option<(u16, u32)>) {
        self.awaiting = run;
    }

    /// A scroll moved every cell up by `rows` (`Ribbon::translate_scroll`).
    pub fn translate(&mut self, rows: u16, bottom: u16) {
        if rows == 0 {
            return;
        }
        self.scroll_carry = self.scroll_carry.saturating_add(rows);
        self.awaiting = match self.awaiting {
            Some((row, cohort)) if row >= rows => Some((row - rows, cohort)),
            _ => None,
        };
        let first_new = bottom.saturating_sub(rows);
        let last_walk = self.last_walk;
        self.seen.retain_mut(|s| {
            if s.row < rows {
                return false;
            }
            s.row -= rows;
            if bottom > 0 && s.row.saturating_add(2) >= first_new {
                let pend_ms = match last_walk {
                    Some(t) => u32::try_from(t.saturating_duration_since(s.born).as_millis())
                        .unwrap_or(u32::MAX),
                    None => 0,
                };
                let mut k = 0;
                while k < FOLLOW_DRS.len() {
                    let dr = FOLLOW_DRS[k];
                    if let Some(r) = s.row.checked_add_signed(dr)
                        && r >= first_new
                        && r < bottom
                    {
                        s.pending |= 1 << k;
                        s.pend_ms[k] = pend_ms;
                    }
                    k += 1;
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

    /// A row band moved (`Ribbon::translate_band`, the [`crate::cursor_glow::band_row`] law).
    pub fn translate_band(&mut self, top: u16, bottom: u16, delta: i16) {
        if delta == 0 || top > bottom {
            return;
        }
        self.scroll_carry = 0;
        self.awaiting = if let Some((row, cohort)) = self.awaiting
            && let Some(row) = band_row(row, top, bottom, delta)
        {
            Some((row, cohort))
        } else {
            None
        };
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

    /// The insert's rewrite re-laid `row` left of `col` ([`super::Event::Rewrite`]).
    #[inline]
    pub fn forget_left_of(&mut self, row: u16, col: u16) {
        self.seen.retain(|s| !(s.row == row && s.col < col));
        self.blank_seen.retain(|b| !(b.0 == row && b.1 < col));
    }

    #[inline]
    fn find(&self, row: u16, col: u16, born: Instant) -> Option<usize> {
        self.seen
            .binary_search_by(|s| (s.row, s.col, s.born).cmp(&(row, col, born)))
            .ok()
    }

    #[inline]
    fn arm(&mut self, cell: &Cell, unit: Unit, nb: &[Option<&[char]>; 4], now: Instant) {
        if unit.is_blank() {
            return;
        }
        if let Some(i) = self.insert(cell, unit, false) {
            self.seen[i].observe(nb, true, now);
        }
    }

    #[inline]
    fn hold_released(&mut self, cell: &Cell) {
        let _ = self.insert(cell, Unit::BLANK, true);
    }

    fn insert(&mut self, cell: &Cell, unit: Unit, released: bool) -> Option<usize> {
        if self.seen.len() >= WITNESS_CAP {
            return None;
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
        Some(at)
    }

    #[inline]
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

    /// The walk: every resident, non-leaving cell is read against this frame's samples.
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
        let mut si = 0;
        while si < self.seen.len() {
            self.seen[si].live = false;
            si += 1;
        }
        self.last_walk = Some(now);
        self.scroll_carry = 0;
        let mut sample_row: Option<(u16, Option<RowSample<'_>>)> = None;
        let mut nb_row = None;
        let mut nb = [None; 4];
        self.find_deferred_runs(cells, rows, withheld);
        if let Some(run) = self.awaiting.take()
            && let Err(k) = self.deferred_runs.binary_search(&run)
        {
            self.deferred_runs.insert(k, run);
        }
        let mut deferred_run: Option<((u16, u32), bool)> = None;
        let mut ci = 0;
        while ci < cells.len() {
            let cell = &cells[ci];
            ci += 1;
            let row_sample = match sample_row {
                Some((r, s)) if r == cell.row => s,
                _ => {
                    let s = row_sample_at(rows, cell.row);
                    sample_row = Some((cell.row, s));
                    s
                }
            };
            let is_deferred = if self.deferred_runs.is_empty() {
                false
            } else {
                let run = (cell.row, cell.cohort);
                match deferred_run {
                    Some((r, ok)) if r == run => ok,
                    _ => {
                        let ok = self.deferred_runs.binary_search(&run).is_ok();
                        deferred_run = Some((run, ok));
                        ok
                    }
                }
            };
            if is_deferred {
                if let Some(i) = self.find(cell.row, cell.col, cell.born) {
                    self.seen[i].live = true;
                    if !self.seen[i].released
                        && matches!(
                            row_sample,
                            Some(s) if unit_at(s.cols, cell.col) != self.seen[i].unit
                        )
                    {
                        self.seen[i].deferred = true;
                    }
                } else if self.was_blank(cell) {
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
                    let replaced = if let Some(sample) = row_sample {
                        let unit = unit_at(sample.cols, cell.col);
                        !unit.is_blank() && unit != self.seen[i].unit
                    } else {
                        false
                    };
                    if replaced {
                        Self::push_unit(retire, cell.row, cell.col, cell.born, self.seen[i].unit);
                    } else {
                        self.seen[i].live = true;
                    }
                }
                continue;
            }
            let Some(sample) = row_sample else {
                if let Some(i) = self.find(cell.row, cell.col, cell.born) {
                    self.seen[i].live = true;
                } else if self.was_blank(cell) {
                    self.blank_next.push((cell.row, cell.col, cell.born));
                }
                continue;
            };
            let unit = unit_at(sample.cols, cell.col);
            if !unit.is_blank() && nb_row != Some(cell.row) {
                nb_row = Some(cell.row);
                nb = neighbours_of(rows, cell.row);
            }
            match self.find(cell.row, cell.col, cell.born) {
                Some(i) => {
                    let seen = self.seen[i];
                    if seen.unit == unit {
                        self.seen[i].live = true;
                        self.seen[i].deferred = false;
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
                    } else {
                        if self.was_blank(cell) {
                            self.landed
                                .push((cell.row, cell.col, cell.born, cell.cohort));
                        }
                        self.arm(cell, unit, &nb, now);
                    }
                }
            }
        }
        self.standing_glyphs.sort_unstable();
        self.standing_glyphs.dedup();
        self.blanked
            .sort_unstable_by_key(|b| (b.row, b.cohort, b.held, b.col));
        let mut i = 0;
        while i < self.blanked.len() {
            let run = (self.blanked[i].row, self.blanked[i].cohort);
            let mut end = i + 1;
            while end < self.blanked.len()
                && (self.blanked[end].row, self.blanked[end].cohort) == run
            {
                end += 1;
            }
            let mut fresh_end = i;
            while fresh_end < end && !self.blanked[fresh_end].held {
                fresh_end += 1;
            }
            let fast = self.retired_runs.contains(&run)
                || (self.standing_glyphs_of(run).is_empty()
                    && Self::moved(&self.blanked[i..fresh_end], rows));
            if fast {
                let mut bi = i;
                while bi < end {
                    let b = &self.blanked[bi];
                    Self::push_unit(retire, b.row, b.col, b.born, b.unit);
                    if b.counted {
                        self.counted_names.push((b.row, b.col, b.born));
                    }
                    bi += 1;
                }
                if !self.retired_runs.contains(&run) {
                    self.retired_runs.push(run);
                }
            } else {
                let mut bi = i;
                while bi < end {
                    let b = self.blanked[bi];
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
                    bi += 1;
                }
                if fresh_end > i {
                    self.released_runs.push(run);
                }
            }
            i = end;
        }
        self.shape_verdicts(cells, rows, now, retire, release);
        self.landed_goes_with_its_run(cells, retire, release);
        let mut fk = 0;
        while fk < self.fresh_released.len() {
            let (row, col, born) = self.fresh_released[fk];
            if let Some(i) = self.find(row, col, born) {
                self.seen[i].released = true;
                self.seen[i].counted = true;
            }
            fk += 1;
        }
        let mut recounted = 0usize;
        let mut cn = 0;
        while cn < self.counted_names.len() {
            let id = &self.counted_names[cn];
            if retire.contains(id) || release.contains(id) {
                recounted += 1;
            }
            cn += 1;
        }
        self.find_standing_runs(cells, retire);
        self.released_runs.sort_unstable();
        if !self.retired_runs.is_empty() || !self.released_runs.is_empty() {
            let mut sample_row: Option<(u16, Option<RowSample<'_>>)> = None;
            let mut run_verdict: Option<((u16, u32), bool, bool)> = None;
            let mut ci = 0;
            while ci < cells.len() {
                let cell = &cells[ci];
                ci += 1;
                if cell.leaving() {
                    continue;
                }
                let run = (cell.row, cell.cohort);
                let (retired, released) = match run_verdict {
                    Some((r, ret, rel)) if r == run => (ret, rel),
                    _ => {
                        let ret = self.retired_runs.binary_search(&run).is_ok();
                        let rel = !ret && self.released_runs.binary_search(&run).is_ok();
                        run_verdict = Some((run, ret, rel));
                        (ret, rel)
                    }
                };
                if (!retired && !released) || self.find(cell.row, cell.col, cell.born).is_some() {
                    continue;
                }
                if retired {
                    let still_blank = matches!(
                        match sample_row {
                            Some((r, s)) if r == cell.row => s,
                            _ => {
                                let s = row_sample_at(rows, cell.row);
                                sample_row = Some((cell.row, s));
                                s
                            }
                        },
                        Some(s) if unit_at(s.cols, cell.col).is_blank()
                    );
                    if !still_blank || self.standing_runs.binary_search(&run).is_err() {
                        retire.push((cell.row, cell.col, cell.born));
                    }
                } else if self.released_span_takes(cells, release, run, cell.col) {
                    release.push((cell.row, cell.col, cell.born));
                    self.hold_released(cell);
                }
            }
        }
        if !retire.is_empty() && !self.retired_runs.is_empty() {
            self.retired_cells.clear();
            self.retired_cells.extend_from_slice(retire);
            self.retired_cells.sort_unstable();
            let mut ri = 0;
            while ri < self.retired_runs.len() {
                let (row, cohort) = self.retired_runs[ri];
                ri += 1;
                let mut lo = u16::MAX;
                let mut hi = 0u16;
                let mut named = false;
                let mut ci = 0;
                while ci < cells.len() {
                    let cell = &cells[ci];
                    if cell.row == row
                        && cell.cohort == cohort
                        && self
                            .retired_cells
                            .binary_search(&(cell.row, cell.col, cell.born))
                            .is_ok()
                    {
                        named = true;
                        lo = lo.min(cell.col);
                        hi = hi.max(cell.col);
                    }
                    ci += 1;
                }
                if !named || lo >= hi {
                    continue;
                }
                let mut cj = 0;
                while cj < cells.len() {
                    let cell = &cells[cj];
                    cj += 1;
                    if cell.leaving()
                        || cell.row != row
                        || cell.cohort != cohort
                        || cell.col < lo
                        || cell.col > hi
                    {
                        continue;
                    }
                    let id = (cell.row, cell.col, cell.born);
                    if self.retired_cells.binary_search(&id).is_err()
                        && !retire.contains(&id)
                        && !release.contains(&id)
                    {
                        retire.push(id);
                    }
                }
            }
        }
        if !self.released_runs.is_empty() {
            let mut released_run: Option<((u16, u32), bool)> = None;
            let mut ci = 0;
            while ci < cells.len() {
                let cell = &cells[ci];
                let run = (cell.row, cell.cohort);
                let is_rel = match released_run {
                    Some((r, ok)) if r == run => ok,
                    _ => {
                        let ok = self.released_runs.binary_search(&run).is_ok();
                        released_run = Some((run, ok));
                        ok
                    }
                };
                if is_rel && let Some(i) = self.find(cell.row, cell.col, cell.born) {
                    self.seen[i].restorable = true;
                }
                ci += 1;
            }
        }
        self.seen.retain(|s| s.live);
        std::mem::swap(&mut self.blank_seen, &mut self.blank_next);
        self.blank_seen.sort_unstable();
        self.blank_seen.dedup();
        recounted
    }

    #[inline]
    fn was_blank(&self, cell: &Cell) -> bool {
        self.blank_seen
            .binary_search(&(cell.row, cell.col, cell.born))
            .is_ok()
    }

    fn landed_goes_with_its_run(
        &mut self,
        cells: &[Cell],
        retire: &mut Vec<(u16, u16, Instant)>,
        release: &mut Vec<(u16, u16, Instant)>,
    ) {
        let mut k = 0;
        while k < self.landed.len() {
            let (row, col, born, cohort) = self.landed[k];
            k += 1;
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
                release.push((row, col, born));
                self.fresh_released.push((row, col, born));
            }
        }
    }

    fn retired_far_side_clear(
        &self,
        cells: &[Cell],
        retire: &[(u16, u16, Instant)],
        run: (u16, u32),
        col: u16,
    ) -> bool {
        let mut lo = u16::MAX;
        let mut hi = 0u16;
        let mut ci = 0;
        while ci < cells.len() {
            let cell = &cells[ci];
            ci += 1;
            if (cell.row, cell.cohort) != run || !retire.contains(&(cell.row, cell.col, cell.born))
            {
                continue;
            }
            let mut in_landed = false;
            let mut li = 0;
            while li < self.landed.len() {
                let l = &self.landed[li];
                if (l.0, l.1, l.2) == (cell.row, cell.col, cell.born) {
                    in_landed = true;
                    break;
                }
                li += 1;
            }
            if in_landed {
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
            !matches!(standing.first(), Some(s) if s.2 <= col)
        } else {
            !matches!(standing.last(), Some(s) if s.2 >= col)
        }
    }

    fn find_deferred_runs(&mut self, cells: &[Cell], rows: &[RowSample<'_>], withheld: &[u16]) {
        self.blind_changed.clear();
        self.deferred_runs.clear();
        if withheld.is_empty() {
            return;
        }
        let mut row_state: Option<(u16, Option<RowSample<'_>>)> = None;
        let mut ci = 0;
        while ci < cells.len() {
            let cell = &cells[ci];
            ci += 1;
            if cell.leaving() {
                continue;
            }
            let sample = match row_state {
                Some((r, s)) if r == cell.row => s,
                _ => {
                    let blind = (cell.row > 0 && withheld.contains(&(cell.row - 1)))
                        || (cell.row < u16::MAX && withheld.contains(&(cell.row + 1)));
                    let s = if blind {
                        row_sample_at(rows, cell.row)
                    } else {
                        None
                    };
                    row_state = Some((cell.row, s));
                    s
                }
            };
            let Some(sample) = sample else {
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
            let mut end = k + 1;
            while end < self.blind_changed.len()
                && (self.blind_changed[end].row, self.blind_changed[end].cohort) == run
            {
                end += 1;
            }
            let records = &self.blind_changed[k..end];
            let mut changed = 0usize;
            let mut any_deferred = false;
            let mut ri = 0;
            while ri < records.len() {
                let b = &records[ri];
                if b.changed {
                    changed += 1;
                }
                if b.deferred {
                    any_deferred = true;
                }
                ri += 1;
            }
            if !any_deferred && changed >= 2 && changed * 2 >= records.len() {
                self.deferred_runs.push(run);
            }
            k = end;
        }
    }

    fn shape_verdicts(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        now: Instant,
        retire: &mut Vec<(u16, u16, Instant)>,
        release: &mut Vec<(u16, u16, Instant)>,
    ) {
        self.shape_runs.clear();
        if self.retired_runs.is_empty() && self.released_runs.is_empty() {
            return;
        }
        self.shape_runs.extend_from_slice(&self.retired_runs);
        self.shape_runs.extend_from_slice(&self.released_runs);
        self.shape_runs.sort_unstable();
        self.shape_runs.dedup();
        let mut k = 0;
        while k < self.shape_runs.len() {
            let run = self.shape_runs[k];
            k += 1;
            let (row, cohort) = run;
            let mut lo_r = u16::MAX;
            let mut hi_r = 0u16;
            let mut lo_n = u16::MAX;
            let mut hi_n = 0u16;
            let mut standing = false;
            self.run_ids.clear();
            self.retired_cells.clear();
            let mut ci = 0;
            while ci < cells.len() {
                let cell = &cells[ci];
                ci += 1;
                if cell.leaving() || cell.row != row || cell.cohort != cohort {
                    continue;
                }
                let id = (cell.row, cell.col, cell.born);
                let Some(i) = self.find(cell.row, cell.col, cell.born) else {
                    continue;
                };
                self.retired_cells.push(id);
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
            let recorded = self.retired_cells.len();
            if self.run_ids.is_empty() || !standing || lo_n <= lo_r || hi_n >= hi_r {
                continue;
            }
            let mut inside = 0usize;
            let mut ri = 0;
            while ri < self.retired_cells.len() {
                let col = self.retired_cells[ri].1;
                if col >= lo_n && col <= hi_n {
                    inside += 1;
                }
                ri += 1;
            }
            if inside * 2 > recorded {
                let (a, b) = if lo_n - lo_r <= hi_r - hi_n {
                    (lo_r, hi_n)
                } else {
                    (lo_n, hi_r)
                };
                let releasing = !self.retired_runs.contains(&run);
                let mut cj = 0;
                while cj < cells.len() {
                    let cell = &cells[cj];
                    cj += 1;
                    if cell.leaving()
                        || cell.row != row
                        || cell.cohort != cohort
                        || cell.col < a
                        || cell.col > b
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
            retire.retain(|id| id.0 != row || !self.run_ids.contains(id));
            release.retain(|id| id.0 != row || !self.run_ids.contains(id));
            self.fresh_released
                .retain(|id| id.0 != row || !self.run_ids.contains(id));
            self.retired_runs.retain(|r| *r != run);
            self.released_runs.retain(|r| *r != run);
            let sample = row_sample_at(rows, row);
            let nb = neighbours_of(rows, row);
            let mut j = 0;
            while j < self.run_ids.len() {
                let (r, c, born) = self.run_ids[j];
                if let Some(i) = self.find(r, c, born) {
                    self.seen[i].unit = match sample {
                        Some(s) => unit_at(s.cols, c),
                        None => Unit::BLANK,
                    };
                    self.seen[i].live = true;
                    self.seen[i].forget_evidence();
                    self.seen[i].observe(&nb, true, now);
                }
                j += 1;
            }
        }
    }

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
        let mut ci = 0;
        while ci < cells.len() {
            let cell = &cells[ci];
            ci += 1;
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
        let tail =
            (col > hi && standing[standing.len() - 1].2 <= hi) || (col < lo && standing[0].2 >= lo);
        inside || tail
    }

    #[inline]
    const fn standing_glyphs_of(&self, run: (u16, u32)) -> &[(u16, u32, u16)] {
        let slice = self.standing_glyphs.as_slice();
        let mut lo = 0;
        let mut hi = slice.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let (r, c, _) = slice[mid];
            if r < run.0 || (r == run.0 && c < run.1) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let mut end = lo;
        while end < slice.len() {
            let (r, c, _) = slice[end];
            if r == run.0 && c == run.1 {
                end += 1;
            } else {
                break;
            }
        }
        slice.split_at(end).0.split_at(lo).1
    }

    fn find_standing_runs(&mut self, cells: &[Cell], retire: &[(u16, u16, Instant)]) {
        self.standing_runs.clear();
        self.retired_cells.clear();
        if self.retired_runs.is_empty() {
            return;
        }
        self.retired_runs.sort_unstable();
        self.retired_cells.extend_from_slice(retire);
        self.retired_cells.sort_unstable();
        let mut retired_run: Option<((u16, u32), bool)> = None;
        let mut ci = 0;
        while ci < cells.len() {
            let cell = &cells[ci];
            ci += 1;
            if cell.leaving() {
                continue;
            }
            let run = (cell.row, cell.cohort);
            let is_retired = match retired_run {
                Some((r, ok)) if r == run => ok,
                _ => {
                    let ok = self.retired_runs.binary_search(&run).is_ok();
                    retired_run = Some((run, ok));
                    ok
                }
            };
            if is_retired
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

    /// The sampled glyph is the exact identity held by a released cell.
    pub(super) fn released_cell_matches(&self, cell: &Cell, cols: &[char]) -> bool {
        if let Some(index) = self.find(cell.row, cell.col, cell.born) {
            let seen = self.seen[index];
            seen.released && seen.restorable && seen.unit == unit_at(cols, cell.col)
        } else {
            false
        }
    }

    /// Releases whose original cells and glyphs are back, as sorted `(cohort, stamp)`.
    pub(super) fn restored_runs(
        &mut self,
        cells: &[Cell],
        rows: &[RowSample<'_>],
        out: &mut Vec<(u32, Option<Instant>)>,
    ) {
        out.clear();
        self.restore_runs.clear();
        let mut has_restorable = false;
        let mut si = 0;
        while si < self.seen.len() {
            if self.seen[si].restorable {
                has_restorable = true;
                break;
            }
            si += 1;
        }
        if !has_restorable {
            return;
        }
        let mut ci = 0;
        while ci < cells.len() {
            let cell = &cells[ci];
            ci += 1;
            let mut in_restore = false;
            let mut ri = 0;
            while ri < self.restore_runs.len() {
                let r = &self.restore_runs[ri];
                if r.cohort == cell.cohort && r.stamp == cell.released_at {
                    in_restore = true;
                    break;
                }
                ri += 1;
            }
            if in_restore {
                continue;
            }
            if !matches!(
                self.find(cell.row, cell.col, cell.born),
                Some(i) if self.seen[i].restorable
            ) {
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
        let mut sample_row: Option<(u16, Option<RowSample<'_>>)> = None;
        let mut cj = 0;
        while cj < cells.len() {
            let cell = &cells[cj];
            cj += 1;
            let Ok(i) = self
                .restore_runs
                .binary_search_by_key(&(cell.cohort, cell.released_at), |r| (r.cohort, r.stamp))
            else {
                continue;
            };
            let seen_idx = self.find(cell.row, cell.col, cell.born);
            let sample = match sample_row {
                Some((r, s)) if r == cell.row => s,
                _ => {
                    let s = row_sample_at(rows, cell.row);
                    sample_row = Some((cell.row, s));
                    s
                }
            };
            let run = &mut self.restore_runs[i];
            match (seen_idx, sample) {
                (Some(j), Some(sample)) if run.stamp.is_some() => {
                    let seen = self.seen[j];
                    let now = unit_at(sample.cols, cell.col);
                    run.exact &= seen.restorable && run.row == cell.row;
                    run.returned_ink |= seen.released && !seen.unit.is_blank() && seen.unit == now;
                }
                (Some(j), Some(sample)) => {
                    let seen = self.seen[j];
                    run.exact &= if seen.restorable {
                        let standing = seen.unit == unit_at(sample.cols, cell.col);
                        (!cell.leaving() || cell.released_at.is_some())
                            && run.row == cell.row
                            && standing
                    } else {
                        let start = self
                            .seen
                            .partition_point(|s| (s.row, s.col) < (cell.row, cell.col));
                        let mut any_rel = false;
                        let mut k = start;
                        while k < self.seen.len() {
                            let s = &self.seen[k];
                            if (s.row, s.col) != (cell.row, cell.col) {
                                break;
                            }
                            if s.released {
                                any_rel = true;
                                break;
                            }
                            k += 1;
                        }
                        !any_rel
                    };
                    run.returned_ink |= seen.released && !seen.unit.is_blank();
                }
                (None, Some(_)) => {}
                _ => run.exact = false,
            }
        }
        let mut rk = 0;
        while rk < self.restore_runs.len() {
            let r = &self.restore_runs[rk];
            if r.exact && r.returned_ink {
                out.push((r.cohort, r.stamp));
            }
            rk += 1;
        }
    }

    /// Clear release custody once the ribbon accepted restoration.
    pub(super) fn restored(&mut self, cohorts: &[u32], cells: &[Cell]) {
        if cohorts.is_empty() {
            return;
        }
        let mut cohort_hit: Option<(u32, bool)> = None;
        let mut ci = 0;
        while ci < cells.len() {
            let cell = &cells[ci];
            ci += 1;
            if cell.leaving() {
                continue;
            }
            let in_cohorts = match cohort_hit {
                Some((c, ok)) if c == cell.cohort => ok,
                _ => {
                    let ok = cohorts.binary_search(&cell.cohort).is_ok();
                    cohort_hit = Some((cell.cohort, ok));
                    ok
                }
            };
            if !in_cohorts {
                continue;
            }
            if let Some(i) = self.find(cell.row, cell.col, cell.born) {
                // A held blank goes back to never-armed: un-released, it would
                // be a blank record no walk can name (blank over blank is a
                // redraw), and a later whole-line clear would read as partial.
                if self.seen[i].unit.is_blank() {
                    self.seen.remove(i);
                } else {
                    self.seen[i].released = false;
                    self.seen[i].restorable = false;
                }
            }
        }
    }

    const fn moved(run: &[Blanked], rows: &[RowSample<'_>]) -> bool {
        let mut lead_count = 0;
        let mut i = 0;
        while i < run.len() {
            if !run[i].unit.cont {
                lead_count += 1;
                if lead_count >= 2 {
                    break;
                }
            }
            i += 1;
        }
        if lead_count < 2 {
            return false;
        }
        let first = run[0];
        let last = run[run.len() - 1];
        let (row, c0) = (first.row, first.col as usize);
        let span = (last.col as usize).saturating_sub(c0);
        let mut r = 0;
        while r < rows.len() {
            let s = rows[r];
            let max_d = s.cols.len().saturating_sub(span);
            let mut d = 0;
            while d < max_d {
                if !(s.row == row && d == c0) {
                    let mut all_match = true;
                    let mut k = 0;
                    while k < run.len() {
                        let b = run[k];
                        let at = b.col as usize - c0 + d;
                        if at > u16::MAX as usize {
                            all_match = false;
                            break;
                        }
                        if !unit_at(s.cols, at as u16).eq(b.unit) {
                            all_match = false;
                            break;
                        }
                        k += 1;
                    }
                    if all_match {
                        return true;
                    }
                }
                d += 1;
            }
            r += 1;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::super::ribbon::Layer;
    use super::*;

    const fn cell(row: u16, col: u16, born: Instant) -> Cell {
        cell_of(row, col, born, 0)
    }

    const fn cell_of(row: u16, col: u16, born: Instant, cohort: u32) -> Cell {
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

    fn pos(out: &[(u16, u16, Instant)]) -> Vec<(u16, u16)> {
        out.iter().map(|&(r, c, _)| (r, c)).collect()
    }

    #[test]
    fn a_malformed_or_short_sample_reads_blank() {
        let cols = row("a\u{4f60}\0 ");
        assert!(unit_at(&cols, 3).is_blank(), "a space is blank");
        assert!(unit_at(&cols, 9).is_blank(), "past the sample is blank");
        assert!(
            unit_at(&row("\0x"), 0).is_blank(),
            "a stray continuation is blank"
        );
    }

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

    const fn up(lo: u16, hi: u16) -> FollowRun {
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

    #[test]
    fn a_twin_inside_the_block_is_neutral_and_one_off_the_target_is_a_hole() {
        const B: &str = "      ";
        type Case = (
            Option<&'static str>,
            &'static str,
            &'static str,
            Option<(u16, u16)>,
            Option<u32>,
        );
        let cases: [Case; 25] = [
            (Some(B), B, "abcdef", Some((0, 5)), Some(0)),
            (Some(B), B, "abcd  ", Some((0, 3)), None),
            (Some(B), B, "  cdef", Some((2, 5)), None),
            (Some(B), B, "abc  f", None, Some(6)),
            (Some(B), B, "ab  ef", None, None),
            (Some(B), B, "a cdef", None, None),
            (Some(B), B, "uvwxyz", None, Some(0)),
            (Some(B), "abcdef", "abcdef", None, Some(0)),
            (None, B, "abcdef", Some((0, 5)), None),
            (Some("abcdef"), B, "abcdef", None, Some(0)),
            (Some("abc   "), B, "abcxyz", None, Some(0)),
            (Some("  c   "), B, "abcdef", Some((0, 5)), Some(0)),
            (Some("a    f"), B, "abcdef", Some((0, 5)), None),
            (Some(" bcde "), B, "abcdef", Some((0, 5)), None),
            (Some("  c   "), B, "ab def", None, None),
            (Some("  c   "), B, "ab  ef", None, None),
            (Some("a     "), "a     ", "abcdef", Some((0, 5)), None),
            (Some("     f"), "     f", "abcdef", Some((0, 5)), None),
            (Some("a     "), "a     ", "a cdef", Some((2, 5)), None),
            (Some("a     "), B, "a cdef", Some((2, 5)), None),
            (Some("     f"), "     f", "abcd f", Some((0, 3)), None),
            (Some("     f"), B, "abcd f", Some((0, 3)), None),
            (Some("abcd  "), B, "abcdef", Some((0, 5)), None),
            (Some("abcdxy"), B, "abcdxy", None, None),
            (Some("abcd  "), B, "abcde ", None, None),
        ];
        let own_run = row("abcdef");
        for (above, own, now, named, missed) in cases {
            let ctx = format!("armed under {above:?}, then `{own}` / `{now}`");
            let above = above.map(row);
            let mut arming = vec![RowSample {
                row: 5,
                cols: &own_run,
            }];
            if let Some(cols) = &above {
                arming.push(RowSample { row: 4, cols });
            }
            let (mut w, cells) = armed_on(5, &arming);
            let (own, now) = (row(own), row(now));
            let mut out = Vec::new();
            let score = w.follow_runs(
                &cells,
                &[
                    RowSample { row: 5, cols: &own },
                    RowSample { row: 4, cols: &now },
                ],
                &mut out,
            );
            let want: Vec<FollowRun> = named.map(|(lo, hi)| up(lo, hi)).into_iter().collect();
            assert_eq!(out, want, "{ctx}");
            if let Some(missed) = missed {
                assert_eq!(score.missed, missed, "{ctx}: missed");
            }
        }
    }

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

    #[test]
    fn a_word_the_wrap_relaid_at_its_row_s_start_does_not_count_against_the_line() {
        let line = "  this confirmatio";
        let blank = "";
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

    fn armed_on(row: u16, rows: &[RowSample<'_>]) -> (Witness, Vec<Cell>) {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let cells: Vec<Cell> = (0..6).map(|c| cell_of(row, c, t0, 7)).collect();
        walk_quiet(&mut w, &cells, rows);
        assert_eq!(w.len(), 6, "every glyph armed");
        (w, cells)
    }

    fn walk_quiet(w: &mut Witness, cells: &[Cell], rows: &[RowSample<'_>]) {
        walk_quiet_at(w, cells, rows, 0);
    }

    fn walk_quiet_at(w: &mut Witness, cells: &[Cell], rows: &[RowSample<'_>], ms: u64) {
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        let now = cells[0].born + Duration::from_millis(ms);
        w.walk(cells, rows, &[], now, &mut out, &mut rel);
        assert!(out.is_empty() && rel.is_empty(), "the glyphs stand");
    }

    fn whole_up() -> Vec<FollowRun> {
        vec![FollowRun {
            row: 5,
            cohort: 7,
            lo: 0,
            hi: 5,
            dr: -1,
        }]
    }

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
        let (mut w, cells) = armed();
        for ms in [16, 32, 48] {
            frame(&mut w, &cells, &copy, ms);
        }
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            whole_up(),
            "a copy that stood 32 ms: the new row of a torn repaint"
        );
        let (mut w, cells) = armed();
        for ms in [16, 32, 48, 64] {
            frame(&mut w, &cells, &copy, ms);
        }
        assert_eq!(
            follow_onto(&mut w, &cells, "abcdef"),
            vec![],
            "the copy stood beside the line: its erase is not a move"
        );
        let (mut w, cells) = armed();
        frame(&mut w, &cells, &copy, 120);
        frame(&mut w, &cells, &copy, 240);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
        let (mut w, cells) = armed();
        frame(&mut w, &cells, &copy, 16);
        frame(&mut w, &cells, &other, 48);
        frame(&mut w, &cells, &copy, 80);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), whole_up());
        let (mut w, cells) = armed();
        frame(&mut w, &cells, &copy, 16);
        walk_quiet_at(&mut w, &cells, &[RowSample { row: 5, cols: &own }], 32);
        frame(&mut w, &cells, &copy, 64);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
        let (mut w, cells) = armed();
        frame(&mut w, &cells, &copy, 16);
        frame(&mut w, &cells, &copy, 64);
        frame(&mut w, &cells, &blank, 80);
        assert_eq!(follow_onto(&mut w, &cells, "abcdef"), vec![]);
    }

    #[test]
    fn a_carried_record_starts_over_on_its_new_row_as_if_just_armed() {
        let text = row("abcdef");
        let blank = row("      ");
        let dests = |out: &[FollowRun]| out.iter().map(|f| (f.row, f.dr)).collect::<Vec<_>>();
        let carried_down = || {
            let (mut w, cells) = armed_run("abcdef");
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
            (w, carried)
        };
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
    }

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

    #[test]
    fn a_twin_testifies_only_about_its_own_row() {
        let own = row("abcdef");
        let blank = row("      ");
        let near = row("abcdvw");
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

    fn shift_onto(w: &mut Witness, cells: &[Cell], now: &str, caret: Option<u16>) -> Vec<ShiftRun> {
        let mut out = Vec::new();
        w.shift_runs(
            cells,
            &[RowSample {
                row: 5,
                cols: &row(now),
            }],
            caret.map(|c| (5, c)),
            Instant::now(),
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

    #[test]
    fn an_insert_names_the_tail_it_pushed_with_its_width() {
        type Case = (
            &'static str,
            &'static str,
            Option<u16>,
            Option<(u16, u16, i16)>,
        );
        let cases: [Case; 15] = [
            ("hello world", "hello Xworld", Some(6), Some((6, 10, 1))),
            ("hello world", "helloX world", Some(5), Some((5, 10, 1))),
            ("hello world", "hello XYZworld", Some(6), Some((6, 10, 3))),
            ("hello world", "hello world", Some(11), None),
            ("hello", "helllo", Some(2), Some((2, 4, 1))),
            ("hello", "helllo", Some(3), Some((3, 4, 1))),
            ("hello", "helllo", None, None),
            ("abcd", "abbcd", Some(2), Some((2, 3, 1))),
            ("type", "typXe", Some(3), Some((3, 3, 1))),
            ("type", "typXe", None, None),
            ("type", "typXe", Some(1), None),
            ("I als", "I alee see", Some(4), None),
            ("hello world", "hello xyzqw", Some(6), None),
            ("hello world", "hello Xwoqqq", Some(6), None),
            ("ab cd ab", "ab cd   ", Some(8), None),
        ];
        for (armed, now, caret, want) in cases {
            let (mut w, cells) = armed_line(armed);
            let want = want.map_or_else(Vec::new, |(lo, hi, dc)| shift(lo, hi, dc));
            assert_eq!(
                shift_onto(&mut w, &cells, now, caret),
                want,
                "`{armed}` -> `{now}`, caret {caret:?}"
            );
        }
    }

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
        assert_eq!(cols, vec![0, 1, 6, 7, 8, 10, 11]);
        let units: Vec<Unit> = w.seen.iter().map(|s| s.unit).collect();
        let at = |c: u16| units[cols.iter().position(|&x| x == c).expect("a record")];
        assert_eq!(at(6), unit_at(&row("hi you there"), 3), "`y` moved to 6");
        assert_eq!(at(8), unit_at(&row("hi you there"), 5), "`u` moved to 8");
        assert_eq!(at(10), unit_at(&row("hi you there"), 10), "`r` stayed");
    }

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
    }

    type Named = (Vec<(u16, u16)>, Vec<(u16, u16)>);

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

    #[test]
    fn returning_partial_ink_requires_the_released_identity_and_exact_unit() {
        for text in ["abcdef", "ab你\0ef"] {
            let (mut w, cells) = armed_line(text);
            let held = cells[2];
            let original = row(text);
            assert!(!w.released_cell_matches(&held, &original));
            let (retired, mut released) = walk_row(&mut w, &cells, 5, "ab    ");
            released.sort_unstable();
            released.dedup();
            assert!(retired.is_empty());
            assert_eq!(released, vec![(5, 2), (5, 3), (5, 4), (5, 5)]);
            assert!(w.released_cell_matches(&held, &original));
            assert!(!w.released_cell_matches(&cells[0], &original));
            assert!(!w.released_cell_matches(&held, &row("abxdef")));
            assert!(!w.released_cell_matches(&held, &row("ab    ")));
            let mut changed = held;
            changed.born += std::time::Duration::from_millis(1);
            assert!(!w.released_cell_matches(&changed, &original));
            changed = held;
            changed.row += 1;
            assert!(!w.released_cell_matches(&changed, &original));
            if text.contains('\0') {
                assert!(w.released_cell_matches(&cells[3], &original));
                assert!(!w.released_cell_matches(&held, &row("ab你def")));
                assert!(!w.released_cell_matches(&cells[3], &row("abc\0ef")));
            }
            let mut restored = w.clone();
            restored.restored(&[7], &cells);
            assert!(!restored.released_cell_matches(&held, &original));
            walk_row(&mut w, &cells, 5, "abxxxx");
            assert!(!w.released_cell_matches(&held, &original));
        }
    }

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

        let t1 = t0 + std::time::Duration::from_millis(60);
        let mut w = Witness::new();
        let word = [cell(5, 3, t0), cell(5, 4, t0)];
        assert_eq!(walk_row(&mut w, &word, 5, "xxxcd"), (vec![], vec![]));
        let keyed = [cell(5, 2, t1), cell(5, 3, t0), cell(5, 4, t0)];
        let (out, _) = walk_row(&mut w, &keyed, 5, "xxzqw");
        assert_eq!(out, vec![(5, 3), (5, 4)], "the key's cell is not named");
        assert_eq!(w.len(), 1, "…and it is armed");

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

    #[test]
    fn a_released_record_is_kept_so_a_glyph_landing_under_it_later_melts_fast_and_counts_once() {
        let t0 = Instant::now();
        let cells = [cell(5, 0, t0), cell(5, 1, t0)];
        let release = |w: &mut Witness, out: &mut Vec<_>, rel: &mut Vec<_>| {
            for text in ["hi", "  "] {
                w.walk(
                    &cells,
                    &[RowSample {
                        row: 5,
                        cols: &row(text),
                    }],
                    &[],
                    Instant::now(),
                    out,
                    rel,
                );
            }
        };
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

    #[test]
    fn a_partial_release_takes_no_space_from_between_standing_letters() {
        let cols = |r: std::ops::Range<u16>| r.collect::<Vec<u16>>();
        for (armed, then, released) in [
            ("ab cd ef", "ab cd e ", vec![7]),
            ("ab cd ef", "ab c    ", cols(4..8)),
            ("ab cd ef", "        ", cols(0..8)),
            ("hello world ", "hello       ", cols(6..12)),
            (" hello world", "       world", cols(0..6)),
        ] {
            let (mut w, cells) = armed_line(armed);
            let (mut out, mut rel) = (Vec::new(), Vec::new());
            let then_row = row(then);
            let sample = [RowSample {
                row: 5,
                cols: &then_row,
            }];
            w.walk(&cells, &sample, &[], Instant::now(), &mut out, &mut rel);
            assert!(
                out.is_empty(),
                "`{then}`: nothing replaced: {:?}",
                pos(&out)
            );
            let mut got: Vec<u16> = pos(&rel).into_iter().map(|(_, c)| c).collect();
            got.sort_unstable();
            got.dedup();
            assert_eq!(got, released, "`{armed}` -> `{then}`");
        }
    }

    #[test]
    fn a_partial_release_of_one_glyph_leaves_the_run_s_spaces_standing() {
        let t0 = Instant::now();
        let t1 = t0 + std::time::Duration::from_millis(50);
        let word: Vec<Cell> = (0..11u16).map(|c| cell_of(5, c, t0, 7)).collect();
        let mut typed = word.clone();
        typed.push(cell_of(5, 11, t1, 7));
        let released = || {
            let mut w = Witness::new();
            walk_row(&mut w, &word, 5, "hello world");
            assert_eq!(w.len(), 10, "ten letters armed, not the space");
            assert_eq!(
                walk_row(&mut w, &word, 5, "hello worl "),
                (vec![], vec![(5, 10)]),
                "the one blanked glyph is released, and no space with it"
            );
            assert_eq!(w.len(), 10, "the space took no released record");
            assert_eq!(
                walk_row(&mut w, &typed, 5, "hello worl x"),
                (vec![], vec![])
            );
            assert_eq!(w.len(), 11, "the new key's cell is armed");
            w
        };
        let restore = |w: &Witness, cells: &[Cell], text: &str| {
            let mut runs = Vec::new();
            w.clone().restored_runs(
                cells,
                &[RowSample {
                    row: 5,
                    cols: &row(text),
                }],
                &mut runs,
            );
            runs
        };
        for (back, retired) in [("hello worldx", vec![]), ("hello worldy", vec![(5, 11)])] {
            let mut w = released();
            assert_eq!(
                restore(&w, &typed, back),
                vec![(7, None)],
                "`{back}`: the run is restorable with a key typed inside the melt"
            );
            w.restored(&[7], &typed);
            assert_eq!(
                walk_row(&mut w, &typed, 5, back),
                (retired, vec![]),
                "`{back}`: the row stands, bar a rewritten key's own cell"
            );
        }
        let mut w = released();
        let mut retyped = word.clone();
        retyped.push(cell_of(5, 10, t1, 7));
        walk_row(&mut w, &retyped, 5, "hello world");
        assert!(restore(&w, &retyped, "hello world").is_empty());
    }

    #[test]
    fn a_relaid_run_melts_fast_and_a_vanished_or_unmatched_one_is_released() {
        type Case = (
            &'static str,
            &'static [(u16, &'static str)],
            &'static [u16],
            &'static [u16],
        );
        let cases: [Case; 11] = [
            ("hi", &[(5, "H ")], &[0, 1], &[]),
            ("hi", &[(5, "  "), (7, "hi")], &[0, 1], &[]),
            ("hi", &[(5, "    hi")], &[0, 1], &[]),
            ("hi", &[(5, "  "), (7, "> ok")], &[], &[0, 1]),
            ("hi", &[(5, "  "), (7, "h ho")], &[], &[0, 1]),
            ("z", &[(5, " "), (7, "zoom")], &[], &[0]),
            ("\u{4f60}\0", &[(5, "  "), (7, "\u{4f60}\0")], &[], &[0, 1]),
            ("ab TO cd TO", &[(5, "ab TO cd   ")], &[], &[9, 10]),
            ("ab cd", &[(5, "  x  ")], &[], &[0, 1, 2, 3, 4]),
            ("ab cd ef", &[(5, "ab*cd eX")], &[7], &[]),
            ("ab cd efgh", &[(5, "ab*cd egh ")], &[7, 8, 9], &[]),
        ];
        let t0 = Instant::now();
        for (armed, then, want_retire, want_release) in cases {
            let what = format!("`{armed}` -> {then:?}");
            let n = u16::try_from(armed.chars().count()).expect("a short line");
            let cells: Vec<Cell> = (0..n).map(|c| cell(5, c, t0)).collect();
            let mut w = Witness::new();
            assert_eq!(
                walk_row(&mut w, &cells, 5, armed),
                (vec![], vec![]),
                "{what}"
            );
            let rows: Vec<Vec<char>> = then.iter().map(|&(_, s)| row(s)).collect();
            let samples: Vec<RowSample<'_>> = then
                .iter()
                .zip(&rows)
                .map(|(&(r, _), cols)| RowSample { row: r, cols })
                .collect();
            let (mut out, mut rel) = (Vec::new(), Vec::new());
            w.walk(&cells, &samples, &[], Instant::now(), &mut out, &mut rel);
            let named = |v: &[(u16, u16, Instant)]| {
                let mut got = pos(v);
                got.sort_unstable();
                got.dedup();
                got
            };
            let cols = |c: &[u16]| c.iter().map(|&c| (5, c)).collect::<Vec<_>>();
            assert_eq!(named(&out), cols(want_retire), "{what}: retired");
            assert_eq!(named(&rel), cols(want_release), "{what}: released");
        }
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
            cell_of(2, 1, t0, 7),
            cell_of(2, 2, t0, 7),
            cell_of(3, 0, t0, 7),
            cell_of(3, 1, t0, 7),
            cell_of(3, 2, t0, 7),
            cell_of(5, 0, t0, 9),
            cell_of(5, 1, t0, 9),
            cell_of(6, 0, t0, 10),
            cell_of(2, 0, t1, 11),
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
    }

    #[test]
    fn a_band_move_carries_the_records_inside_it_drops_what_leaves_and_keeps_the_list_sorted() {
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

    #[test]
    fn awaiting_and_last_walk_follow_scroll_band_and_clear() {
        let t0 = Instant::now();
        let mut w = Witness::new();
        let (mut out, mut rel) = (Vec::new(), Vec::new());
        w.walk(
            &[cell(5, 0, t0)],
            &[RowSample {
                row: 5,
                cols: &['a'],
            }],
            &[],
            t0,
            &mut out,
            &mut rel,
        );
        assert_eq!(w.last_walk, Some(t0));

        w.await_caret(Some((5, 7)));
        w.translate(2, 10);
        assert_eq!(w.awaiting, Some((3, 7)));

        w.translate_band(2, 6, 1);
        assert_eq!(w.awaiting, Some((4, 7)));

        w.translate_band(2, 6, -3);
        assert_eq!(w.awaiting, None);

        w.clear();
        assert_eq!(w.last_walk, None);
        assert_eq!(w.awaiting, None);
    }
}
