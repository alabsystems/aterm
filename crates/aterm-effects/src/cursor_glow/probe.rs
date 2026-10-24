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
#[derive(Default)]
pub(super) struct WitnessRowBuf {
    pub(super) row: u16,
    pub(super) cols: Vec<char>,
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
        self.probed_spans = TextSpans::default();
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
    #[must_use]
    pub fn frame_text_letters(&self) -> usize {
        self.frame_text
            .pairs
            .iter()
            .filter(|p| matches!(p, Some((Some(_), _))))
            .count()
    }

    /// Host observation: the cells of the frame about to be presented,
    /// whose first cell is window cell `at` (a split's focused pane hands
    /// its own cells at its offset, the window's composite being drawn only
    /// after the cursor's step). `cur` is in window cells.
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
        let mut lo = i32::MAX;
        let mut hi = i32::MIN;
        let mut take = |row: u16| {
            lo = lo.min(i32::from(row));
            hi = hi.max(i32::from(row));
        };
        for s in &self.sparks {
            take(s.row);
        }
        for (row, _) in cur.into_iter().chain(self.last) {
            take(row);
        }
        if let Some((a, b)) = self.classic.row_range() {
            take(a);
            take(b);
        }
        let (r0, c0) = (i32::from(at.0), i32::from(at.1));
        let ft = &mut self.frame_text;
        ft.rows.clear();
        ft.pairs.clear();
        ft.cols = 0;
        ft.col0 = c0;
        ft.at = Some(now);
        if lo > hi {
            return;
        }
        let lo = (lo - 1 - Self::FRAME_TEXT_RISE).max(r0);
        let hi = (hi + 1)
            .min(r0 + cells.len() as i32 - 1)
            .min(lo + MAX_ROWS - 1);
        ft.lo = lo;
        if lo > hi {
            return;
        }
        let line = |row: i32| &cells[(row - r0) as usize];
        ft.cols = (lo..=hi).map(|r| line(r).len()).max().unwrap_or(0);
        let rgb = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        let n_rows = (hi - lo + 1) as usize;
        ft.rows.reserve(n_rows);
        ft.pairs.reserve(n_rows * ft.cols);
        for row in lo..=hi {
            let line = line(row);
            let glyph = |c: &aterm_core::terminal::RenderCell| c.ch != ' ' || c.wide;
            let mut first_glyph: Option<i32> = None;
            let mut last_glyph = 0i32;
            let n_cols = line.len().min(ft.cols);
            for (c, cell) in line[..n_cols].iter().enumerate() {
                let is_glyph = glyph(cell);
                if is_glyph {
                    let ci = c0 + c as i32;
                    if first_glyph.is_none() {
                        first_glyph = Some(ci);
                    }
                    last_glyph = ci + 1;
                }
                ft.pairs
                    .push(Some((is_glyph.then(|| rgb(cell.fg)), rgb(cell.bg))));
            }
            if n_cols < ft.cols {
                ft.pairs.resize(ft.pairs.len() + (ft.cols - n_cols), None);
            }
            ft.rows.push(first_glyph.map(|f| (f, last_glyph)));
        }
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
        let fill = cols
            .iter()
            .rposition(|&c| c != ' ')
            .map_or(0, |i| i + 1)
            .min(u16::MAX as usize) as u16;
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
        let source = self
            .held_park
            .filter(|p| p.cross_row)
            .map(|p| p.row)
            .or_else(|| {
                (self.type_hint.armed() || !self.type_press_ring.is_empty())
                    .then_some(self.last)
                    .flatten()
                    .map(|(row, _)| row)
            })
            .or_else(|| self.insert.armed.and_then(|licence| licence.row));
        let caret_move = self
            .last
            .or_else(|| self.last_visible.map(|(cell, _)| cell))
            .zip(self.witness_rows[..self.witness_rows_n.min(self.witness_rows.len())].first())
            .map(|((from, _), to)| (from, to.row));
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
        let slot = match self.witness_rows[..n.min(self.witness_rows.len())]
            .iter()
            .position(|s| s.row == row)
        {
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
    pub fn v2_ribbon(&self) -> Option<&rk::ribbon::Ribbon> {
        self.v2.engaged().then(|| self.v2.ribbon())
    }

    /// Neighbour probe.
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
