// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Row probes: the host's reads of the rows the light lands on (the witness rows, the ribbon rows, the print anchor) and the trust each one carries.

use super::*;

/// Host metadata for a row flanking the probed cursor row ([`CursorGlow::observe_neighbor_rows`]).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NbrProbe {
    /// No neighbour capture with this probe (a host wiring only `observe_row`).
    Unprobed,
    /// No row handed.
    OffGrid,
    /// Captured: the chars ride the matching `row_above_*`/`row_below_*` double buffer, rotated with `row_cur`/`row_prev`.
    Probed,
}

/// Which detector families may consume one row probe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProbeTrust {
    /// Primary screen, or an alt-screen app inside the repaint-blink window (a DECTCEM hide inside a DEC-2026 update).
    Full,
    /// Alt screen without repaint-blink evidence.
    ContentOnly,
}

/// Metadata for one cursor-row content probe ([`CursorGlow::observe_row`]).
#[derive(Clone, Copy)]
pub(super) struct RowProbe {
    pub(super) row: u16,
    /// The caret column at capture (row identity and diagnostics; the vanished span comes from the prefix/suffix diff, which needs no caret).
    pub(super) caret: u16,
    pub(super) fill: u16,
    pub(super) at: Instant,
    /// What the host said about the row above `row` ([`NbrProbe`]).
    pub(super) above: NbrProbe,
    /// What the host said about the row below `row`.
    pub(super) below: NbrProbe,
    /// Which detector families may read this probe.
    pub(super) trust: ProbeTrust,
}

/// One sampled grid row for Rainbow Kitty's content witness, per-column chars in the row probe's convention.
pub(super) struct WitnessRowBuf {
    pub(super) row: u16,
    pub(super) cols: Vec<char>,
}

impl Default for WitnessRowBuf {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl WitnessRowBuf {
    pub(super) const EMPTY: Self = Self {
        row: 0,
        cols: Vec::new(),
    };
}

impl CursorGlow {
    /// Clear row probes and poof proofs when the terminal content stream changes (tab/pane switch or scroll fence).
    #[inline]
    pub fn drop_row_probe(&mut self) {
        self.recent_typed_run = None;
        self.park_source_intact = None;
        self.row_cur.clear();
        self.row_prev.clear();
        self.row_cur_meta = None;
        self.row_prev_meta = None;
        self.row_shown_meta = None;
        self.clear_neighbor_rows();
        self.kill_hint = None;
        self.bs_poof_hint = None;
        self.bs_baseline = None;
    }

    /// Clear the neighbour captures (both generations, [`NbrProbe`]) wherever the row probe is dropped, retaining capacity.
    #[inline]
    pub(super) fn clear_neighbor_rows(&mut self) {
        self.row_above_cur.clear();
        self.row_above_prev.clear();
        self.row_below_cur.clear();
        self.row_below_prev.clear();
        self.probed_spans = TextSpans::EMPTY;
    }

    /// Host observation.
    #[inline]
    pub const fn observe_print_anchor(&mut self, anchor: Option<(u16, u16, u64)>) {
        if let Some(sample) = anchor {
            self.print_anchor = Some(sample);
            self.print_anchor_glyph = None;
        }
    }

    /// [`Self::observe_print_anchor`] with the glyph at the run's last cell (`Terminal::print_anchor_glyph`, same lock hold, row probe convention).
    #[inline]
    pub const fn observe_print_anchor_glyph(
        &mut self,
        anchor: Option<(u16, u16, u64)>,
        glyph: Option<char>,
    ) {
        self.observe_print_anchor(anchor);
        if anchor.is_some() {
            self.print_anchor_glyph = glyph;
        }
    }

    /// Host observation: the cells of the frame about to be presented, the
    /// window's own ([`Self::observe_frame_text_at`] at `(0, 0)`).
    #[inline]
    pub fn observe_frame_text(
        &mut self,
        cur: Option<(u16, u16)>,
        cells: &[Vec<aterm_core::terminal::RenderCell>],
        now: Instant,
    ) {
        self.observe_frame_text_at(cur, cells, (0, 0), now);
    }

    /// Test seam: how many lettered cells this frame's text reads.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn frame_text_letters(&self) -> usize {
        let pairs = self.frame_text.pairs.as_slice();
        let mut n = 0;
        let mut i = 0;
        while i < pairs.len() {
            if matches!(pairs[i], Some((Some(_), _))) {
                n += 1;
            }
            i += 1;
        }
        n
    }

    /// Whether the frame's cell under the caret at `(row, col)` holds a
    /// letter: a block caret draws it cut out of its fill in the cell's own
    /// ground. `false` for a cell the frame did not read.
    #[inline]
    #[must_use]
    pub(crate) const fn caret_lettered(&self, (row, col): (u16, u16)) -> bool {
        matches!(
            self.frame_text.pair(row as i32, col as i32),
            Some((Some(_), _))
        )
    }

    /// Host observation: the cells of the frame about to be presented,
    /// whose first cell is window cell `at` (a split's focused pane hands
    /// its own cells at its offset, the window's composite being drawn only
    /// after the cursor's step). `cur` is in window cells.
    #[inline]
    pub fn observe_frame_text_at(
        &mut self,
        cur: Option<(u16, u16)>,
        cells: &[Vec<aterm_core::terminal::RenderCell>],
        at: (u16, u16),
        now: Instant,
    ) {
        /// The most rows one frame reads: a leap longer than this keeps its
        /// far rows unknown.
        const MAX_ROWS: i32 = 256;
        #[inline]
        const fn extend_bounds(lo: &mut i32, hi: &mut i32, row: u16) {
            let r = row as i32;
            if r < *lo {
                *lo = r;
            }
            if r > *hi {
                *hi = r;
            }
        }
        let mut lo = i32::MAX;
        let mut hi = i32::MIN;
        let sparks = self.sparks.as_slice();
        let mut si = 0;
        while si < sparks.len() {
            extend_bounds(&mut lo, &mut hi, sparks[si].row);
            si += 1;
        }
        if let Some((row, _)) = cur {
            extend_bounds(&mut lo, &mut hi, row);
        }
        if let Some((row, _)) = self.last {
            extend_bounds(&mut lo, &mut hi, row);
        }
        if let Some((a, b)) = self.classic.row_range() {
            extend_bounds(&mut lo, &mut hi, a);
            extend_bounds(&mut lo, &mut hi, b);
        }
        // The rows a landing ring reaches about its caret: 1.7 rows at twice
        // a hop's radius ([`super::raster::classic_ring_radius_factor`]), and
        // its rim.
        const RING_ROWS: u16 = 4;
        if let Some(Ring {
            caret: Some((row, _)),
            ..
        }) = self.ring
        {
            extend_bounds(&mut lo, &mut hi, row.saturating_sub(RING_ROWS));
            extend_bounds(&mut lo, &mut hi, row.saturating_add(RING_ROWS));
        }
        let (r0, c0) = (i32::from(at.0), i32::from(at.1));
        // The rows the pet's body lay on last frame, padded for its flight:
        // its page veil reads the letters under the body wherever it lies,
        // not only near the caret (`[vim] NORMAL` printed under the cat five
        // rows below a caret that moved up went bare, looks8 round 2).
        const PET_TEXT_PAD: i32 = 3;
        let pet_rows = match (self.pet_body, self.veil_frame) {
            (Some((_, _, y0, y1)), Some((_, g))) if g.ch > 0 => {
                let (oy, ch) = (i32::from(g.origin_y), g.ch as i32);
                Some((
                    (y0 - oy).div_euclid(ch) - PET_TEXT_PAD,
                    (y1 - 1 - oy).div_euclid(ch) + PET_TEXT_PAD,
                ))
            }
            _ => None,
        };
        #[inline]
        const fn clamp_row_span(a: i32, b: i32, r0: i32, len: usize) -> (i32, i32) {
            let hi = r0 + len as i32 - 1;
            (if a > r0 { a } else { r0 }, if b < hi { b } else { hi })
        }
        match pet_rows {
            Some((a, b)) => {
                let (a, b) = clamp_row_span(a, b.min(a + MAX_ROWS - 1), r0, cells.len());
                read_text(&mut self.pet_text, cells, (a, b), c0, r0, now);
            }
            None => read_text(&mut self.pet_text, cells, (1, 0), c0, r0, now),
        }
        if lo > hi {
            read_text(&mut self.frame_text, cells, (1, 0), c0, r0, now);
            return;
        }
        let lo = (lo - 1 - Self::FRAME_TEXT_RISE).max(r0);
        let hi = (hi + 1)
            .min(r0 + cells.len() as i32 - 1)
            .min(lo + MAX_ROWS - 1);
        read_text(&mut self.frame_text, cells, (lo, hi), c0, r0, now);
    }

    /// Host observation.
    #[inline]
    pub const fn observe_caret_drawn(&mut self, drawn: bool) {
        self.caret_hidden = !drawn;
    }

    /// Per-frame row probe.
    #[inline]
    pub fn observe_row(&mut self, row: u16, caret: u16, cols: &[char], now: Instant) {
        self.observe_row_with_trust(row, caret, cols, now, ProbeTrust::Full);
    }

    /// [`Self::observe_row`] with an explicit [`ProbeTrust`] class.
    pub fn observe_row_with_trust(
        &mut self,
        row: u16,
        caret: u16,
        cols: &[char],
        now: Instant,
        trust: ProbeTrust,
    ) {
        self.unsettle();
        self.row_cur.clear();
        self.row_cur.extend_from_slice(cols);
        let mut fill = 0u16;
        let mut idx = cols.len();
        while idx > 0 {
            idx -= 1;
            if cols[idx] != ' ' {
                fill = (idx + 1).min(u16::MAX as usize) as u16;
                break;
            }
        }
        self.row_cur_meta = Some(RowProbe {
            row,
            caret,
            fill,
            at: now,
            above: NbrProbe::Unprobed,
            below: NbrProbe::Unprobed,
            trust,
        });
        if self.v2.engaged() {
            self.v2_probe_row(i32::from(row), cols);
        }
    }

    /// The row probe to v2's glyph probe (§5.4).
    #[inline]
    pub(super) fn v2_probe_row(&mut self, row: i32, cols: &[char]) {
        self.v2.probe_mut().probe_row_chars(row, cols);
    }

    /// The rows the content witness wants (`rk::witness`).
    pub fn ribbon_rows(&self, out: &mut [u16]) -> usize {
        if !self.v2.engaged() || out.is_empty() {
            return 0;
        }
        let source = if let Some(p) = self.held_park
            && p.cross_row
        {
            Some(p.row)
        } else if (self.type_hint.armed() || !self.type_press_ring.is_empty())
            && let Some((row, _)) = self.last
        {
            Some(row)
        } else {
            match self.insert.armed {
                Some(licence) => licence.row,
                None => None,
            }
        };
        let caret_move = if let Some((from, _)) = self.cursor_anchor()
            && let Some(to) =
                self.witness_rows[..self.witness_rows_n.min(self.witness_rows.len())].first()
        {
            Some((from, to.row))
        } else {
            None
        };
        let n = self.v2.ribbon_rows_at_caret(source, caret_move, out);
        let mut asked = [0u16; RIBBON_LIST_ROWS];
        let kept = n.min(asked.len());
        asked[..kept].copy_from_slice(&out[..kept]);
        self.witness_asked.set((asked, kept));
        n
    }

    /// One sampled row for the content witness.
    #[inline]
    pub fn observe_ribbon_row(&mut self, row: u16, cols: &[char]) {
        let Some(slot) = self.witness_row_slot(row) else {
            return;
        };
        slot.cols.clear();
        slot.cols.extend_from_slice(cols);
    }

    /// [`Self::observe_ribbon_row`] filled in place.
    #[inline]
    pub fn capture_ribbon_row(&mut self, row: u16, fill: impl FnOnce(&mut Vec<char>)) {
        let Some(slot) = self.witness_row_slot(row) else {
            return;
        };
        slot.cols.clear();
        fill(&mut slot.cols);
    }

    #[inline]
    pub(super) fn witness_row_slot(&mut self, row: u16) -> Option<&mut WitnessRowBuf> {
        if !self.v2.engaged() {
            return None;
        }
        let n = self.witness_rows_n;
        let cap = n.min(self.witness_rows.len());
        let mut found = None;
        let mut idx = 0;
        while idx < cap {
            if self.witness_rows[idx].row == row {
                found = Some(idx);
                break;
            }
            idx += 1;
        }
        let slot = match found {
            Some(i) => i,
            None => {
                if n >= CURSOR_WITNESS_ROWS {
                    return None;
                }
                if self.witness_rows.len() == n {
                    self.witness_rows.push(WitnessRowBuf::default());
                }
                self.witness_rows_n = n + 1;
                n
            }
        };
        let slot = &mut self.witness_rows[slot];
        slot.row = row;
        Some(slot)
    }

    /// Rainbow Kitty's laid ribbon, read-only, `Some` only while v2 owns the frame (for conformance tests).
    #[inline]
    #[must_use]
    pub const fn v2_ribbon(&self) -> Option<&rk::ribbon::Ribbon> {
        if self.v2.engaged() {
            Some(self.v2.ribbon())
        } else {
            None
        }
    }

    /// Neighbour probe.
    #[inline]
    pub fn observe_neighbor_rows(&mut self, above: Option<&[char]>, below: Option<&[char]>) {
        let Some(meta) = self.row_cur_meta.as_mut() else {
            return;
        };
        let probed_row = i32::from(meta.row);
        self.row_above_cur.clear();
        self.row_below_cur.clear();
        meta.above = match above {
            Some(cols) => {
                self.row_above_cur.extend_from_slice(cols);
                NbrProbe::Probed
            }
            None => NbrProbe::OffGrid,
        };
        meta.below = match below {
            Some(cols) => {
                self.row_below_cur.extend_from_slice(cols);
                NbrProbe::Probed
            }
            None => NbrProbe::OffGrid,
        };
        if self.v2.engaged() {
            if let Some(cols) = above {
                self.v2_probe_row(probed_row - 1, cols);
            }
            if let Some(cols) = below {
                self.v2_probe_row(probed_row + 1, cols);
            }
        }
    }
}

/// Read rows `lo..=hi` (window rows; none when `lo > hi`) of `cells`, whose
/// first cell is window cell `(r0, c0)`, into `ft`: each row's lettered span
/// and every cell's `(ink, ground)`.
#[inline]
fn read_text(
    ft: &mut FrameText,
    cells: &[Vec<aterm_core::terminal::RenderCell>],
    (lo, hi): (i32, i32),
    c0: i32,
    r0: i32,
    now: Instant,
) {
    ft.rows.clear();
    ft.pairs.clear();
    ft.cols = 0;
    ft.col0 = c0;
    ft.at = Some(now);
    ft.lo = lo;
    if lo > hi {
        return;
    }
    let mut max_cols = 0usize;
    let mut r = lo;
    while r <= hi {
        let len = cells[(r - r0) as usize].len();
        if len > max_cols {
            max_cols = len;
        }
        r += 1;
    }
    ft.cols = max_cols;
    #[inline]
    const fn pack_rgb(c: [u8; 3]) -> u32 {
        ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32
    }
    let n_rows = (hi - lo + 1) as usize;
    ft.rows.reserve(n_rows);
    ft.pairs.reserve(n_rows * ft.cols);
    let mut row = lo;
    while row <= hi {
        let line = cells[(row - r0) as usize].as_slice();
        let mut glyph_span: Option<(i32, i32)> = None;
        let n_cols = line.len().min(ft.cols);
        let mut c = 0;
        while c < n_cols {
            let cell = &line[c];
            let is_glyph = cell.ch != ' ' || cell.wide;
            if is_glyph {
                let ci = c0 + c as i32;
                glyph_span = Some(match glyph_span {
                    Some((first, _)) => (first, ci + 1),
                    None => (ci, ci + 1),
                });
            }
            ft.pairs.push(Some((
                if is_glyph {
                    Some(pack_rgb(cell.fg))
                } else {
                    None
                },
                pack_rgb(cell.bg),
            )));
            c += 1;
        }
        if n_cols < ft.cols {
            ft.pairs.resize(ft.pairs.len() + (ft.cols - n_cols), None);
        }
        ft.rows.push(glyph_span);
        row += 1;
    }
}
