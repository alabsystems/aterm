// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Over-ink contrast ledger for additive cursor glow emitters.

use super::*;

mod letter_guard;
pub(crate) use letter_guard::LetterGuard;

/// Per-band RGB lift per unit coverage for a streak sample ([`InkLedger::admit_axis`]).
pub(super) type BandLifts = [[f32; 3]; InkLedger::BANDS];

/// Per-frame over-ink contrast account reset by [`CursorGlow::begin_ink_ledger`].
#[derive(Default)]
pub(crate) struct InkLedger {
    /// Whether this frame keeps the account; an idle ledger admits everything.
    on: bool,
    /// The frame's cell grid: origin and cell size, window px.
    grid: (f32, f32, f32, f32),
    /// The text lines held, `(row, first, end)` per slot.
    lines: [Option<(i32, i32, i32)>; 3],
    /// The caret cell, whose light the block caret covers.
    caret: Option<(i32, i32)>,
    /// The light laid on each band of each text cell so far, per channel in
    /// `add_sat` levels: `lines` slot-major, `stride` cells a slot,
    /// [`Self::BANDS`] bands a cell, top first.
    laid: Vec<[f32; 3]>,
    stride: usize,
    /// The pair the account keeps legible: the stroke's ink and the page.
    pair: (u32, u32),
    /// What lies under each text cell's letters before any light (the page,
    /// or the bed's composite over it), `lines` slot-major as `laid` is, one
    /// entry a cell.
    under: Vec<u32>,
    /// Each text cell's letter ink, as `under` (the pair's ink, or the
    /// frame's own cell's).
    ink: Vec<u32>,
    /// [`Self::admit_quads`]'s scratch: the text cells one light reaches
    /// (indices into `under`), and the summed light on each of their pixels,
    /// cell-major.
    reached: Vec<usize>,
    px: Vec<[f32; 3]>,
    /// Each reached cell's brightest pixel per band and channel, cell-major
    /// ([`Self::quad_peaks`]).
    peaks: Vec<[f32; 3]>,
    /// The text cells the light being masked keeps less than its whole
    /// strength over ([`Self::admit_masked`]).
    dims: Vec<Dim>,
    /// The masked light's pieces, spliced back over the light they cut.
    pieces: Vec<GlowQuad>,
    halo_pieces: Vec<RainHalo>,
    /// One piece's slice knots, px.
    knots_x: Vec<i32>,
    knots_y: Vec<i32>,
    /// Test-only: admit a masked light as one ([`CursorGlow::over_ink_uniform`]).
    #[cfg(test)]
    uniform: bool,
    /// A print of every cut this frame's masked lights took (each dimmed
    /// cell and the share it kept), so a frame whose cuts differ prints
    /// differently ([`CursorGlow::over_ink_print`]).
    print: u64,
}

/// One text cell a light was admitted over at less than its whole strength:
/// the cell's pixel box and the share of the light it keeps.
#[derive(Clone, Copy, Debug)]
struct Dim {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    share: f32,
}

impl InkLedger {
    /// Minimum contrast floor for the reference stroke ([`CursorGlow::STREAK_INK_FLOOR`]).
    pub(super) const FLOOR: f32 = CursorGlow::STREAK_INK_FLOOR;
    /// Contrast floor for steady lights (streak, crown, forge).
    pub(super) const STEADY_FLOOR: f32 = 6.0;
    /// Number of horizontal bands tracked per text cell.
    pub(super) const BANDS: usize = 4;
    const QUAD_CELLS: usize = 32;
    const MASK_PIECES: usize = 4096;
    /// Contrast floor for the cursor head light over a dark streak bed.
    pub(crate) const HEAD_FLOOR: f32 = 3.6;
    /// Contrast floor for flying lights (embers, sparks) over a dark streak bed.
    pub(crate) const FLYING_FLOOR: f32 = 3.4;
    /// Contrast floor for the caret body ([`OverInkTier::Body`]).
    pub(super) const BODY_FLOOR: f32 = 3.25;
    /// Contrast floor for the typing-momentum glow ([`OverInkTier::Momentum`]).
    pub(super) const MOMENTUM_FLOOR: f32 = 3.1;
    /// Minimum contrast floor on neighbouring text lines above and below the caret row.
    pub(super) const NEIGHBOUR_FLOOR: f32 = 4.6;
    /// Horizontal feather width for masked lights as a fraction of cell width.
    pub(super) const FEATHER_X: f32 = 0.45;
    /// Vertical feather height for masked lights as a fraction of cell height.
    pub(super) const FEATHER_Y: f32 = 0.35;
    const FEATHER_STEP: i32 = 2;

    /// The account index of band 0 of text cell `(row, col)`, or `None` for a
    /// cell the account does not hold (off every line, past the grid, or the
    /// caret's own).
    #[inline]
    const fn slot(&self, row: i32, col: i32) -> Option<usize> {
        if matches!(self.caret, Some((cr, cc)) if cr == row && cc == col)
            || col < 0
            || col as usize >= self.stride
        {
            return None;
        }
        let mut i = 0;
        while i < self.lines.len() {
            if let Some((r, first, end)) = self.lines[i]
                && r == row
                && col >= first
                && col < end
            {
                return Some((i * self.stride + col as usize) * Self::BANDS);
            }
            i += 1;
        }
        None
    }

    /// The text cell at `(row, col)`'s index into `under`, as
    /// [`Self::slot`] finds it.
    #[inline]
    const fn cell(&self, row: i32, col: i32) -> Option<usize> {
        match self.slot(row, col) {
            Some(at) => Some(at / Self::BANDS),
            None => None,
        }
    }

    /// Whether this frame keeps the over-ink contrast account.
    #[inline]
    pub(super) const fn is_on(&self) -> bool {
        self.on
    }

    /// Close the ledger for the frame so an inactive ledger admits all lights.
    #[inline]
    pub(super) const fn close(&mut self) {
        self.on = false;
        self.print = 0;
    }

    /// Fold one cut into the frame's print.
    #[inline]
    const fn note_cut(&mut self, cell: usize, share: f32) {
        self.print = aterm_hash::fnv_mix_u64(
            self.print | 1,
            ((cell as u64) << 32) | (share.to_bits() as u64),
        );
    }

    /// Record composite bed colour `rgb` under text cell `(row, col)`.
    pub(super) fn lay_under(&mut self, (row, col): (i32, i32), rgb: u32) {
        if !self.on {
            return;
        }
        let Some(cell) = self.cell(row, col) else {
            return;
        };
        let table = crate::color_math::relative_luminance_table();
        let lum = |c: u32| crate::color_math::relative_luminance_with(table, c);
        if lum(rgb) > lum(self.under[cell]) {
            self.under[cell] = rgb;
        }
    }

    /// The brightest light laid so far on any band of text cell `(row, col)`,
    /// per channel in `add_sat` levels: `None` for an idle ledger or a cell
    /// it does not hold (off every line, or the caret's own).
    pub(super) fn laid_peak(&self, (row, col): (i32, i32)) -> Option<[f32; 3]> {
        if !self.on {
            return None;
        }
        let at = self.slot(row, col)?;
        Some(
            self.laid[at..at + Self::BANDS]
                .iter()
                .fold([0.0f32; 3], |m, b| {
                    [m[0].max(b[0]), m[1].max(b[1]), m[2].max(b[2])]
                }),
        )
    }

    /// Add `light` (premultiplied, `add_sat` levels) under the letters of
    /// text cell `(row, col)`, over what already lies there: a light that
    /// burns beneath the ink (the dark flame kept in its row), so every
    /// later light over that letter spends from what it left.
    pub(super) fn add_under(&mut self, (row, col): (i32, i32), light: u32) {
        if !self.on {
            return;
        }
        let Some(cell) = self.cell(row, col) else {
            return;
        };
        let g = self.under[cell];
        let ch = |sh: u32| (((g >> sh) & 0xff) + ((light >> sh) & 0xff)).min(255);
        self.under[cell] = (ch(16) << 16) | (ch(8) << 8) | ch(0);
    }

    /// How far band `band` of a cell `ch` px tall lies from the cell's middle
    /// row of pixels (a streak sample's axis), at its nearest point.
    #[inline]
    pub(super) const fn band_reach(ch: f32, band: usize) -> f32 {
        let h = ch / Self::BANDS as f32;
        let (y0, y1) = (band as f32 * h, (band + 1) as f32 * h);
        let mid = 0.5 * ch;
        (y0 - mid).max(mid - y1).max(0.0)
    }

    /// Whether the account's ink keeps `floor` over `ground` under `lift`,
    /// with ink and ground lifted together and clipped at 255 as `add_sat`
    /// does.
    #[inline]
    fn reads_with(
        table: &crate::color_math::RelativeLuminanceTable,
        ink: [f32; 3],
        ground: [f32; 3],
        lift: [f32; 3],
        floor: f32,
    ) -> bool {
        let lum = |base: [f32; 3]| {
            let ch = |c: usize| (base[c] + lift[c]).min(255.0) as usize;
            crate::color_math::relative_luminance_rgb(table, ch(0), ch(1), ch(2))
        };
        let (ink, ground) = (lum(ink), lum(ground));
        ink.max(ground) + 0.05 >= floor * (ink.min(ground) + 0.05)
    }

    fn room(&self, at: usize, add: [f32; 3], floor: f32) -> f32 {
        if add == [0.0; 3] {
            return 1.0;
        }
        let table = crate::color_math::relative_luminance_table();
        let laid = self.laid[at];
        let unpack = |rgb: u32| [16u32, 8, 0].map(|sh| ((rgb >> sh) & 0xff) as f32);
        let cell = at / Self::BANDS;
        let (ink, ground) = (unpack(self.ink[cell]), unpack(self.under[cell]));
        let slot = cell / self.stride.max(1);
        let off_caret = self.lines[slot]
            .zip(self.caret)
            .is_some_and(|((row, _, _), (caret_row, _))| row != caret_row);
        let floor = if off_caret {
            floor.max(Self::NEIGHBOUR_FLOOR)
        } else {
            floor
        };
        let with = |s: f32| [0, 1, 2].map(|c| laid[c] + s * add[c]);
        if Self::reads_with(table, ink, ground, with(1.0), floor) {
            return 1.0;
        }
        if !Self::reads_with(table, ink, ground, with(0.0), floor) {
            return 0.0;
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..8 {
            let mid = 0.5 * (lo + hi);
            if Self::reads_with(table, ink, ground, with(mid), floor) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// Admit a streak sample of coverage `cov` on `(row, col)` with per-band `lifts` down to `floor`.
    pub(super) fn admit_axis(
        &mut self,
        (row, col): (u16, u16),
        lifts: &BandLifts,
        cov: f32,
    ) -> f32 {
        if !self.on {
            return cov;
        }
        let Some(base) = self.slot(i32::from(row), i32::from(col)) else {
            return cov;
        };
        let mut share = 1.0f32;
        for (b, lift) in lifts.iter().enumerate() {
            if *lift != [0.0; 3] {
                share = share.min(self.room(base + b, lift.map(|l| l * cov), Self::STEADY_FLOOR));
                if share <= 0.0 {
                    return 0.0;
                }
            }
        }
        for (b, lift) in lifts.iter().enumerate() {
            if *lift != [0.0; 3] {
                for (laid, l) in self.laid[base + b].iter_mut().zip(lift) {
                    *laid += share * l * cov;
                }
            }
        }
        cov * share
    }

    /// Admit an additive radial halo at `(cx, cy)` with radii `(rx, ry)` down to `floor`.
    pub(super) fn admit_halo(
        &mut self,
        (cx, cy): (f32, f32),
        (rx, ry): (f32, f32),
        premul: u32,
        floor: f32,
    ) -> f32 {
        if !self.on || rx <= 0.0 || ry <= 0.0 || (premul & 0x00FF_FFFF) == 0 {
            return 1.0;
        }
        let peak = [16u32, 8, 0].map(|sh| ((premul >> sh) & 0xff) as f32);
        let (ox, oy, cw, ch) = self.grid;
        let bh = ch / Self::BANDS as f32;
        let row_dy2 = |row: i32| -> [f32; Self::BANDS] {
            let ry0 = oy + row as f32 * ch;
            std::array::from_fn(|band| {
                let y0 = ry0 + band as f32 * bh;
                let dy = (cy.clamp(y0, y0 + bh) - cy) / ry;
                dy * dy
            })
        };
        let weights = |col: i32, dy2: &[f32; Self::BANDS]| -> [Option<f32>; Self::BANDS] {
            let x0 = ox + col as f32 * cw;
            let dx = (cx.clamp(x0, x0 + cw) - cx) / rx;
            let dx2 = dx * dx;
            std::array::from_fn(|band| {
                let d2 = dx2 + dy2[band];
                (d2 < 1.0).then_some((1.0 - d2) * (1.0 - d2))
            })
        };
        let reached = |(row, first, end): (i32, i32, i32)| {
            let (y0, y1) = (oy + row as f32 * ch, oy + (row + 1) as f32 * ch);
            let lo = (((cx - rx - ox) / cw).floor() as i32).max(first);
            let hi = (((cx + rx - ox) / cw).floor() as i32 + 1).min(end);
            let cols = if cy + ry > y0 && cy - ry < y1 {
                lo..hi
            } else {
                0..0
            };
            (row, cols)
        };
        let mut share = 1.0f32;
        'probe: for line in self.lines.iter().flatten().copied() {
            let (row, cols) = reached(line);
            if cols.is_empty() {
                continue;
            }
            let dy2 = row_dy2(row);
            for col in cols {
                let Some(base) = self.slot(row, col) else {
                    continue;
                };
                for (band, w) in weights(col, &dy2).into_iter().enumerate() {
                    if let Some(w) = w {
                        share = share.min(self.room(base + band, peak.map(|p| p * w), floor));
                        if share <= 0.0 {
                            break 'probe;
                        }
                    }
                }
            }
        }
        if share <= 0.0 {
            return 0.0;
        }
        for line in self.lines.iter().flatten().copied() {
            let (row, cols) = reached(line);
            if cols.is_empty() {
                continue;
            }
            let dy2 = row_dy2(row);
            for col in cols {
                let Some(base) = self.slot(row, col) else {
                    continue;
                };
                for (band, w) in weights(col, &dy2).into_iter().enumerate() {
                    if let Some(w) = w {
                        for (laid, p) in self.laid[base + band].iter_mut().zip(peak) {
                            *laid += share * p * w;
                        }
                    }
                }
            }
        }
        share
    }

    fn quad_peaks(&mut self, quads: &[GlowQuad]) -> bool {
        let (ox, oy, cw, ch) = self.grid;
        let (cwi, chi) = (cw as i32, ch as i32);
        self.reached.clear();
        self.peaks.clear();
        if cwi <= 0 || chi <= 0 {
            return false;
        }
        let lit = |q: &&GlowQuad| q.alpha == 0 && (q.color | q.color2) & 0x00FF_FFFF != 0;
        let mut reached = std::mem::take(&mut self.reached);
        let span = |lo: f32, len: u16, o: f32, size: i32| {
            let first = ((lo - o) / size as f32).floor() as i32;
            let last = ((lo + f32::from(len) - 1.0 - o) / size as f32).floor() as i32;
            first..=last
        };
        for q in quads.iter().filter(lit) {
            for row in span(f32::from(q.y), q.h.max(1), oy, chi) {
                for col in span(f32::from(q.x), q.w.max(1), ox, cwi) {
                    if let Some(cell) = self.cell(row, col)
                        && !reached.contains(&cell)
                    {
                        reached.push(cell);
                    }
                }
            }
        }
        if reached.is_empty() {
            self.reached = reached;
            return false;
        }
        let area = (cwi * chi) as usize;
        let mut px = std::mem::take(&mut self.px);
        px.clear();
        px.resize(reached.len() * area, [0.0; 3]);
        for q in quads.iter().filter(lit) {
            let (qx0, qy0) = (i32::from(q.x), i32::from(q.y));
            let (qx1, qy1) = (qx0 + i32::from(q.w), qy0 + i32::from(q.h));
            let w = i32::from(q.w).max(1);
            let denom = (2 * w) as f32;
            let ends = [16u32, 8, 0].map(|sh| {
                let a = ((q.color >> sh) & 0xff) as f32;
                let b = ((q.color2 >> sh) & 0xff) as f32;
                (a, b - a)
            });
            let cwi_u = cwi as usize;
            for (k, &cell) in reached.iter().enumerate() {
                let (cx0, cy0) = self.cell_origin(cell);
                let (x0, x1) = (qx0.max(cx0), qx1.min(cx0 + cwi));
                let (y0, y1) = (qy0.max(cy0), qy1.min(cy0 + chi));
                if x0 >= x1 || y0 >= y1 {
                    continue;
                }
                let k_base = k * area + ((y0 - cy0) * cwi) as usize;
                let y_span = (y1 - y0) as usize;
                for x in x0..x1 {
                    let m = (2 * (x - qx0) + 1) as f32;
                    let t = m / denom;
                    let c = ends.map(|(a, da)| a + da * t);
                    let mut at = k_base + (x - cx0) as usize;
                    for _ in 0..y_span {
                        for (p, v) in px[at].iter_mut().zip(c) {
                            *p += v;
                        }
                        at += cwi_u;
                    }
                }
            }
        }
        let cwi_u = cwi as usize;
        for k in 0..reached.len() {
            let cell_base = k * area;
            for band in 0..Self::BANDS {
                let (y0, y1) = (
                    band * chi as usize / Self::BANDS,
                    (band + 1) * chi as usize / Self::BANDS,
                );
                let mut m = [0.0f32; 3];
                for y in y0..y1 {
                    let row_base = cell_base + y * cwi_u;
                    for p in &px[row_base..row_base + cwi_u] {
                        for c in 0..3 {
                            m[c] = m[c].max(p[c]);
                        }
                    }
                }
                self.peaks.push(m);
            }
        }
        self.reached = reached;
        self.px = px;
        true
    }

    /// Window px of the top-left corner of text cell `cell` (an index into
    /// `under`).
    #[inline]
    const fn cell_origin(&self, cell: usize) -> (i32, i32) {
        let (ox, oy, cw, ch) = self.grid;
        let stride = if self.stride > 1 { self.stride } else { 1 };
        let (slot, col) = (cell / stride, (cell % stride) as i32);
        let row = match self.lines[slot] {
            Some(l) => l.0,
            None => 0,
        };
        (ox as i32 + col * cw as i32, oy as i32 + row * ch as i32)
    }

    /// Admit a group of additive quads uniformly down to `floor`.
    pub(super) fn admit_quads(&mut self, quads: &[GlowQuad], floor: f32) -> f32 {
        if !self.on || !self.quad_peaks(quads) {
            return 1.0;
        }
        let mut share = 1.0f32;
        'probe: for (k, &cell) in self.reached.iter().enumerate() {
            for band in 0..Self::BANDS {
                let add = self.peaks[k * Self::BANDS + band];
                if add != [0.0; 3] {
                    share = share.min(self.room(cell * Self::BANDS + band, add, floor));
                    if share <= 0.0 {
                        break 'probe;
                    }
                }
            }
        }
        if share > 0.0 {
            for (k, &cell) in self.reached.iter().enumerate() {
                for band in 0..Self::BANDS {
                    let add = self.peaks[k * Self::BANDS + band];
                    for (laid, a) in self.laid[cell * Self::BANDS + band].iter_mut().zip(add) {
                        *laid += share * a;
                    }
                }
            }
        }
        share
    }

    /// Admit additive quads in `quads[range]` cell by cell down to `floor`, slicing around dimmed cells.
    pub(crate) fn admit_masked(
        &mut self,
        quads: &mut Vec<GlowQuad>,
        range: std::ops::Range<usize>,
        floor: f32,
    ) -> f32 {
        if !self.on || range.is_empty() || !self.quad_peaks(&quads[range.clone()]) {
            return 1.0;
        }
        #[cfg(test)]
        if self.uniform {
            let share = self.admit_quads(&quads[range.clone()], floor);
            scale_additive(&mut quads[range], share);
            return share;
        }
        self.dims.clear();
        let mut least = 1.0f32;
        for k in 0..self.reached.len() {
            let cell = self.reached[k];
            let mut share = 1.0f32;
            for band in 0..Self::BANDS {
                let add = self.peaks[k * Self::BANDS + band];
                if add != [0.0; 3] {
                    share = share.min(self.room(cell * Self::BANDS + band, add, floor));
                    if share <= 0.0 {
                        break;
                    }
                }
            }
            if share > 0.0 {
                for band in 0..Self::BANDS {
                    let add = self.peaks[k * Self::BANDS + band];
                    for (laid, a) in self.laid[cell * Self::BANDS + band].iter_mut().zip(add) {
                        *laid += share * a;
                    }
                }
            }
            if share < 1.0 {
                self.dims.push(self.dim_of(cell, share));
                self.note_cut(cell, share);
                least = least.min(share);
            }
        }
        if self.dims.is_empty() {
            return 1.0;
        }
        let mut pieces = std::mem::take(&mut self.pieces);
        pieces.clear();
        for i in range.clone() {
            self.mask_quad(quads[i], &mut pieces);
        }
        quads.splice(range, pieces.drain(..));
        self.pieces = pieces;
        least
    }

    /// The pixel box of text cell `cell` and the share it keeps.
    #[inline]
    const fn dim_of(&self, cell: usize, share: f32) -> Dim {
        let (x0, y0) = self.cell_origin(cell);
        let (_, _, cw, ch) = self.grid;
        Dim {
            x0: x0 as f32,
            y0: y0 as f32,
            x1: x0 as f32 + cw,
            y1: y0 as f32 + ch,
            share: share.clamp(0.0, 1.0),
        }
    }

    /// The share of a masked light kept at window px `(x, y)`: each dimmed
    /// cell's share inside it, rising to the whole light over the feather's
    /// reach off its box (a smoothstep of the normalised distance), the
    /// least of every dimmed cell's.
    #[inline]
    fn mask_at(&self, x: f32, y: f32) -> f32 {
        let (_, _, cw, ch) = self.grid;
        let (fx, fy) = (
            (cw * Self::FEATHER_X).max(1.0),
            (ch * Self::FEATHER_Y).max(1.0),
        );
        self.dims.iter().fold(1.0f32, |m, d| {
            if d.share >= m {
                return m;
            }
            let dx = (d.x0 - x).max(x - d.x1).max(0.0) / fx;
            let dy = (d.y0 - y).max(y - d.y1).max(0.0) / fy;
            if dx >= 1.0 || dy >= 1.0 {
                return m;
            }
            let e = (dx * dx + dy * dy).sqrt().min(1.0);
            let rise = e * e * (3.0 - 2.0 * e);
            m.min(d.share + (1.0 - d.share) * rise)
        })
    }

    fn knots(&mut self, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> bool {
        let (_, _, cw, ch) = self.grid;
        let (fx, fy) = (
            (cw * Self::FEATHER_X).ceil() as i32,
            (ch * Self::FEATHER_Y).ceil() as i32,
        );
        let (kx, ky) = (&mut self.knots_x, &mut self.knots_y);
        kx.clear();
        ky.clear();
        kx.extend([x0, x1]);
        ky.extend([y0, y1]);
        let mut meets = false;
        for d in &self.dims {
            let (dx0, dy0, dx1, dy1) = (d.x0 as i32, d.y0 as i32, d.x1 as i32, d.y1 as i32);
            if dx1 + fx <= x0 || dx0 - fx >= x1 || dy1 + fy <= y0 || dy0 - fy >= y1 {
                continue;
            }
            meets = true;
            let edge = |k: &mut Vec<i32>, a: i32, b: i32, f: i32, lo: i32, hi: i32| {
                let mut push = |v: i32| {
                    if v > lo && v < hi {
                        k.push(v);
                    }
                };
                push(a);
                push(b);
                let mut s = Self::FEATHER_STEP;
                while s < f {
                    push(a - s);
                    push(b + s);
                    s += Self::FEATHER_STEP;
                }
                push(a - f);
                push(b + f);
            };
            edge(kx, dx0, dx1, fx, x0, x1);
            edge(ky, dy0, dy1, fy, y0, y1);
        }
        kx.sort_unstable();
        kx.dedup();
        ky.sort_unstable();
        ky.dedup();
        meets
    }

    fn mask_quad(&mut self, q: GlowQuad, out: &mut Vec<GlowQuad>) {
        let (x0, y0) = (i32::from(q.x), i32::from(q.y));
        let (x1, y1) = (x0 + i32::from(q.w), y0 + i32::from(q.h));
        if q.alpha != 0 || q.w == 0 || q.h == 0 || !self.knots((x0, y0, x1, y1)) {
            out.push(q);
            return;
        }
        let w = f32::from(q.w);
        let at = |c: u32, sh: u32| ((c >> sh) & 0xff) as f32;
        let colour = |x: i32, m: f32| {
            let t = (x - x0) as f32 / w;
            let ch = |sh: u32| {
                let v = at(q.color, sh) + (at(q.color2, sh) - at(q.color, sh)) * t;
                ((v * m).round() as u32).min(255)
            };
            (ch(16) << 16) | (ch(8) << 8) | ch(0)
        };
        for yi in 0..self.knots_y.len() - 1 {
            let (ya, yb) = (self.knots_y[yi], self.knots_y[yi + 1]);
            let ym = 0.5 * (ya + yb) as f32;
            let mut xa = self.knots_x[0];
            let mut ca = colour(xa, self.mask_at(xa as f32, ym));
            for xi in 0..self.knots_x.len() - 1 {
                let xb = self.knots_x[xi + 1];
                let cb = colour(xb, self.mask_at(xb as f32, ym));
                if ca | cb != 0 {
                    out.push(GlowQuad {
                        row: q.row,
                        x: xa as u16,
                        y: ya as u16,
                        w: (xb - xa) as u16,
                        h: (yb - ya) as u16,
                        color: ca,
                        alpha: 0,
                        color2: cb,
                        alpha2: 0,
                    });
                }
                xa = xb;
                ca = cb;
            }
        }
    }

    /// Admit additive radial halos in `halos[from..]` down to `floor`, slicing around dimmed cells.
    pub(crate) fn admit_halos_masked(
        &mut self,
        halos: &mut Vec<RainHalo>,
        from: usize,
        floor: f32,
    ) {
        if !self.on || from >= halos.len() {
            return;
        }
        let key = |h: &RainHalo| (h.cx, h.cy, h.rx, h.ry, h.color);
        let mut pieces = std::mem::take(&mut self.halo_pieces);
        pieces.clear();
        let mut i = from;
        let mut cut = false;
        while i < halos.len() {
            let k = key(&halos[i]);
            let mut j = i + 1;
            while j < halos.len() && key(&halos[j]) == k {
                j += 1;
            }
            self.dims.clear();
            if halos[i].mode == HaloMode::Add {
                self.halo_dims(
                    (f32::from(k.0), f32::from(k.1)),
                    (f32::from(k.2), f32::from(k.3)),
                    k.4,
                    floor,
                );
            }
            for h in &halos[i..j] {
                let (x0, y0) = (i32::from(h.x), i32::from(h.y));
                let (x1, y1) = (x0 + i32::from(h.w), y0 + i32::from(h.h));
                if self.dims.is_empty() || h.w == 0 || h.h == 0 || !self.knots((x0, y0, x1, y1)) {
                    pieces.push(*h);
                    continue;
                }
                cut = true;
                for yi in 0..self.knots_y.len() - 1 {
                    let (ya, yb) = (self.knots_y[yi], self.knots_y[yi + 1]);
                    for xi in 0..self.knots_x.len() - 1 {
                        let (xa, xb) = (self.knots_x[xi], self.knots_x[xi + 1]);
                        let m = self.mask_at(0.5 * (xa + xb) as f32, 0.5 * (ya + yb) as f32);
                        let color = scale_rgb(h.color, m);
                        if color & 0x00FF_FFFF == 0 {
                            continue;
                        }
                        pieces.push(RainHalo {
                            x: xa as u16,
                            y: ya as u16,
                            w: (xb - xa) as u16,
                            h: (yb - ya) as u16,
                            color,
                            ..*h
                        });
                    }
                }
            }
            i = j;
        }
        if cut {
            halos.splice(from.., pieces.drain(..));
        }
        self.halo_pieces = pieces;
    }

    fn halo_dims(&mut self, (cx, cy): (f32, f32), (rx, ry): (f32, f32), premul: u32, floor: f32) {
        if rx <= 0.0 || ry <= 0.0 {
            return;
        }
        let peak = [16u32, 8, 0].map(|sh| ((premul >> sh) & 0xff) as f32);
        let (ox, oy, cw, ch) = self.grid;
        let bh = ch / Self::BANDS as f32;
        for line in self.lines.into_iter().flatten() {
            let (row, first, end) = line;
            let (y0, y1) = (oy + row as f32 * ch, oy + (row + 1) as f32 * ch);
            if !(cy + ry > y0 && cy - ry < y1) {
                continue;
            }
            let lo = (((cx - rx - ox) / cw).floor() as i32).max(first);
            let hi = (((cx + rx - ox) / cw).floor() as i32 + 1).min(end);
            if lo >= hi {
                continue;
            }
            let dy2: [f32; Self::BANDS] = std::array::from_fn(|band| {
                let by0 = y0 + band as f32 * bh;
                let dy = (cy.clamp(by0, by0 + bh) - cy) / ry;
                dy * dy
            });
            for col in lo..hi {
                let Some(base) = self.slot(row, col) else {
                    continue;
                };
                let x0 = ox + col as f32 * cw;
                let dx = (cx.clamp(x0, x0 + cw) - cx) / rx;
                let dx2 = dx * dx;
                let weights: [Option<f32>; Self::BANDS] = std::array::from_fn(|band| {
                    let d2 = dx2 + dy2[band];
                    (d2 < 1.0).then_some((1.0 - d2) * (1.0 - d2))
                });
                let mut share = 1.0f32;
                for (band, w) in weights.into_iter().enumerate() {
                    if let Some(w) = w {
                        share = share.min(self.room(base + band, peak.map(|p| p * w), floor));
                        if share <= 0.0 {
                            break;
                        }
                    }
                }
                if share > 0.0 {
                    for (band, w) in weights.into_iter().enumerate() {
                        if let Some(w) = w {
                            for (laid, p) in self.laid[base + band].iter_mut().zip(peak) {
                                *laid += share * p * w;
                            }
                        }
                    }
                }
                if share < 1.0 {
                    let cell = base / Self::BANDS;
                    self.dims.push(self.dim_of(cell, share));
                    self.note_cut(cell, share);
                }
            }
        }
    }
}

impl InkLedger {
    /// Admit each additive quad in `quads` independently down to `floor`.
    pub(super) fn admit_each_quad(&mut self, quads: &mut [GlowQuad], floor: f32) {
        if !self.on {
            return;
        }
        for q in quads {
            let share = self.admit_quads(std::slice::from_ref(q), floor);
            scale_additive(std::slice::from_mut(q), share);
        }
    }
}

/// Grow `v`'s capacity to at least `want`, once.
#[inline]
fn reserve_to<T>(v: &mut Vec<T>, want: usize) {
    if v.capacity() < want {
        v.reserve(want - v.len());
    }
}

/// `rgb` scaled by `share`, per channel (truncating), its top byte kept.
#[inline]
const fn scale_rgb(c: u32, share: f32) -> u32 {
    let share = share.clamp(0.0, 1.0);
    let r = (((c >> 16) & 0xff) as f32 * share) as u32;
    let g = (((c >> 8) & 0xff) as f32 * share) as u32;
    let b = ((c & 0xff) as f32 * share) as u32;
    (c & 0xFF00_0000) | (r << 16) | (g << 8) | b
}

/// Scale every additive quad (`alpha == 0`) in `quads` by `share`.
#[inline]
pub(super) fn scale_additive(quads: &mut [GlowQuad], share: f32) {
    if share >= 1.0 {
        return;
    }
    for q in quads.iter_mut().filter(|q| q.alpha == 0) {
        q.color = scale_rgb(q.color, share);
        q.color2 = scale_rgb(q.color2, share);
    }
}

impl CursorGlow {
    /// Initialize this frame's [`InkLedger`] from the active style, theme, and text lines.
    pub(super) fn begin_ink_ledger(
        &mut self,
        cfg: &GlowConfig,
        geom: Geom,
        cur: Option<(u16, u16)>,
    ) {
        let fire = matches!(cfg.style, GlowStyle::Fire);
        let mono = matches!(cfg.style, GlowStyle::Classic) && cfg.classic_mono;
        let on = cfg.dark_theme
            && !self.streak_unbudgeted()
            && !self.over_ink_idle()
            && (fire || mono || Self::streak_lies_under(cfg.style));
        self.ink_ledger.on = on;
        if !on {
            return;
        }
        let spans = self.probed_text_spans();
        let mut lines = [None; 3];
        for (slot, line) in lines.iter_mut().zip(spans.lines()) {
            *slot = Some(line);
        }
        if let Some((row, col)) = cur.map(|(r, c)| (i32::from(r), i32::from(c))) {
            let typed = self
                .sparks
                .iter()
                .filter(|s| s.typing && i32::from(s.row) == row)
                .map(|s| i32::from(s.col))
                .fold(None, |acc: Option<(i32, i32)>, c| {
                    Some(acc.map_or((c, c), |(lo, hi)| (lo.min(c), hi.max(c))))
                });
            let mut add = |line: (i32, i32, i32)| {
                if !lines.iter().flatten().any(|l| l.0 == line.0)
                    && let Some(free) = lines.iter_mut().find(|l| l.is_none())
                {
                    *free = Some(line);
                }
            };
            if let Some((first, last)) = typed {
                add((row, first, last + 1));
            }
            let (first, last) = typed.unwrap_or((col, col));
            let reach = Self::INK_LEDGER_REACH;
            let (lo, hi) = (
                (first - reach).max(0),
                (last + 1 + reach).min(geom.cols as i32),
            );
            let meta = self
                .row_prev_meta
                .as_ref()
                .filter(|m| i32::from(m.row) == row);
            let captured =
                |nbr: fn(&RowProbe) -> NbrProbe| meta.is_some_and(|m| nbr(m) == NbrProbe::Probed);
            let text = self.text_view();
            let mut neighbour = |nbr: i32| match text.frame_line(nbr) {
                Some(Some((first, end))) => add((nbr, first, end)),
                Some(None) => {}
                None => add((nbr, lo, hi)),
            };
            if row > 0 && !captured(|m| m.above) {
                neighbour(row - 1);
            }
            if row + 1 < geom.rows as i32 && !captured(|m| m.below) {
                neighbour(row + 1);
            }
        }
        #[cfg(test)]
        {
            self.ink_ledger.uniform = self.over_ink_uniform;
        }
        let ledger = &mut self.ink_ledger;
        ledger.lines = lines;
        ledger.grid = (
            f32::from(geom.origin_x),
            f32::from(geom.origin_y),
            geom.cw as f32,
            geom.ch as f32,
        );
        ledger.caret = cur.map(|(r, c)| (i32::from(r), i32::from(c)));
        ledger.stride = geom.cols;
        ledger.laid.clear();
        ledger
            .laid
            .resize(3 * geom.cols * InkLedger::BANDS, [0.0; 3]);
        ledger.pair = if fire {
            (Self::STREAK_REF_INK, Self::STREAK_REF_GROUND)
        } else {
            Self::bed_pair(cfg)
        };
        ledger.under.clear();
        ledger.under.resize(3 * geom.cols, ledger.pair.1);
        ledger.ink.clear();
        ledger.ink.resize(3 * geom.cols, ledger.pair.0);
        if !fire {
            let frame = &self.frame_text;
            for (slot, line) in ledger.lines.iter().enumerate() {
                let Some((row, first, end)) = *line else {
                    continue;
                };
                for col in first.max(0)..end.min(geom.cols as i32) {
                    if let Some((fg, bg)) = frame.pair(row, col) {
                        let i = slot * geom.cols + col as usize;
                        ledger.ink[i] = fg.unwrap_or(ledger.pair.0);
                        ledger.under[i] = bg;
                    }
                }
            }
        }
        let want = InkLedger::QUAD_CELLS * geom.cw * geom.ch;
        if ledger.px.capacity() < want {
            ledger.px.reserve(want - ledger.px.len());
        }
        if ledger.reached.capacity() < InkLedger::QUAD_CELLS {
            ledger
                .reached
                .reserve(InkLedger::QUAD_CELLS - ledger.reached.len());
        }
        reserve_to(&mut ledger.peaks, InkLedger::QUAD_CELLS * InkLedger::BANDS);
        reserve_to(&mut ledger.dims, InkLedger::QUAD_CELLS);
        reserve_to(&mut ledger.knots_x, InkLedger::QUAD_CELLS * 8);
        reserve_to(&mut ledger.knots_y, InkLedger::QUAD_CELLS * 8);
        reserve_to(&mut ledger.pieces, InkLedger::MASK_PIECES);
        reserve_to(&mut ledger.halo_pieces, InkLedger::MASK_PIECES);
    }

    /// Return the `(ink, page)` RGB pair used to solve dark-bed contrast.
    #[inline]
    pub(crate) const fn bed_pair(cfg: &GlowConfig) -> (u32, u32) {
        let (fg, bg) = (cfg.theme_fg & 0x00FF_FFFF, cfg.theme_bg & 0x00FF_FFFF);
        if fg == bg {
            (Self::STREAK_REF_INK, Self::STREAK_REF_GROUND)
        } else {
            (fg, bg)
        }
    }

    /// Whether `style` draws its streak underneath letters on a dark ground.
    #[inline]
    pub(super) const fn streak_lies_under(style: GlowStyle) -> bool {
        matches!(
            style,
            GlowStyle::Lumen
                | GlowStyle::Comet
                | GlowStyle::Laser
                | GlowStyle::Phaser
                | GlowStyle::Sparkle
                | GlowStyle::Beam
                | GlowStyle::Water
        )
    }

    /// Fingerprint of all masked-light cuts performed during this frame.
    #[inline]
    #[must_use]
    pub const fn over_ink_print(&self) -> u64 {
        self.ink_ledger.print
    }

    /// Admit `quads[range]` at `tier` into this frame's over-ink ledger.
    #[inline]
    pub fn admit_over_ink(
        &mut self,
        quads: &mut Vec<GlowQuad>,
        range: std::ops::Range<usize>,
        tier: OverInkTier,
    ) -> f32 {
        if range.is_empty() {
            return 1.0;
        }
        let floor = match tier {
            OverInkTier::Body => InkLedger::BODY_FLOOR,
            OverInkTier::Momentum => InkLedger::MOMENTUM_FLOOR,
        };
        self.ink_ledger.admit_masked(quads, range, floor)
    }

    /// Attenuate additive caret quads near inked neighbour rows and return the cut print.
    pub fn hold_off_neighbour_ink(
        &self,
        quads: &mut [GlowQuad],
        caret: Option<(u16, u16)>,
        geom: Geom,
        page: u32,
    ) -> u64 {
        if quads.is_empty() {
            return 0;
        }
        let Some((cr, _)) = caret else {
            return 0;
        };
        let view = self.text_view();
        let page = page & 0x00FF_FFFF;
        let (cw, ch) = (geom.cw as f32, geom.ch as f32);
        let (ox, oy) = (f32::from(geom.origin_x), f32::from(geom.origin_y));
        let (top, bot) = (oy + f32::from(cr) * ch, oy + f32::from(cr + 1) * ch);
        let (fx, fy, fi) = (
            (cw * Self::NEIGHBOUR_HOLD_X).max(1.0),
            (ch * Self::NEIGHBOUR_HOLD_Y).max(1.0),
            (ch * Self::NEIGHBOUR_EASE_IN).max(1.0),
        );
        #[cfg(test)]
        let fi = if self.neighbour_hold_hard { 1e-3 } else { fi };
        let smooth = |k: f32| {
            let k = k.clamp(0.0, 1.0);
            k * k * (3.0 - 2.0 * k)
        };
        let inked = |row: i32, col: i32| {
            view.covers_cell(row, col)
                || self.frame_text.pair(row, col).is_some_and(|(ink, ground)| {
                    ink.is_some() || !super::emit::reads_as_page(ground, page)
                })
        };
        let cr = i32::from(cr);
        let mut print = 0u64;
        for q in quads.iter_mut().filter(|q| q.alpha == 0 && q.h > 0) {
            let ym = f32::from(q.y) + 0.5 * f32::from(q.h);
            let (inside, across) = if ym < 0.5 * (top + bot) {
                (ym - top, cr - 1)
            } else {
                (bot - ym, cr + 1)
            };
            let row = if inside < 0.0 {
                ((ym - oy) / ch).floor() as i32
            } else {
                across
            };
            let hold = smooth((inside + fy) / (fi + fy));
            if hold >= 1.0 || row < 0 || !view.knows_row(row) {
                continue;
            }
            let (x0, x1) = (f32::from(q.x), f32::from(q.x) + f32::from(q.w));
            let (c_lo, c_hi) = (
                ((x0 - fx - ox) / cw).floor() as i32,
                ((x1 + fx - ox) / cw).floor() as i32,
            );
            let mut near = f32::INFINITY;
            for c in c_lo.max(0)..=c_hi {
                if !inked(row, c) {
                    continue;
                }
                let (a, b) = (ox + c as f32 * cw, ox + (c + 1) as f32 * cw);
                near = near.min((a - x1).max(x0 - b).max(0.0));
                if near == 0.0 {
                    break;
                }
            }
            if !near.is_finite() {
                continue;
            }
            let share = hold + (1.0 - hold) * smooth(near / fx);
            if share >= 1.0 {
                continue;
            }
            q.color = scale_rgb(q.color, share);
            q.color2 = scale_rgb(q.color2, share);
            print = print.rotate_left(7)
                ^ ((u64::from(q.x) << 32) | (u64::from(q.y) << 16) | (share * 255.0) as u64);
        }
        print
    }

    /// How far into an inked row next to the caret's a caret light keeps
    /// any of its strength, as a share of the row's height, and how far off
    /// an inked cell it takes to recover, as a share of a cell's width
    /// ([`Self::hold_off_neighbour_ink`]).
    pub(super) const NEIGHBOUR_HOLD_Y: f32 = 0.12;
    /// See [`Self::NEIGHBOUR_HOLD_Y`].
    pub(super) const NEIGHBOUR_HOLD_X: f32 = 0.45;
    /// How far inside the caret row's edge a caret light begins easing
    /// down toward an inked row across it, as a share of the row's height
    /// ([`Self::hold_off_neighbour_ink`]).
    pub(super) const NEIGHBOUR_EASE_IN: f32 = 0.35;
}

/// Over-ink contrast tier for lights emitted after the aurora ([`CursorGlow::admit_over_ink`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverInkTier {
    /// Caret body light.
    Body,
    /// Typing-momentum glow.
    Momentum,
}
