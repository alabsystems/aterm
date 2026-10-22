// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The over-ink ledger: one budget per text cell, spent once by every
//! additive light that reaches it.
//!
//! Each of fire's lights over the typed line (the streak, the crown's two
//! halos, the forge core and its crawling embers, the ember shower, the
//! steam) was capped on its own, but the renderer adds them all with
//! `add_sat` onto the same letters, so individually legal layers summed past
//! the point where the newest glyph washed to white. The ledger holds the
//! light already laid on each text cell and admits a new light only at the
//! strength that still leaves the reference stroke (the shipped dark theme's
//! foreground on its background, as the streak budgets use) its contrast
//! there.
//!
//! The account is kept per horizontal band of a cell ([`InkLedger::BANDS`]
//! of them), conservatively: every light is charged its strength at the
//! band's nearest point, as though all of them peaked on one pixel of it. So
//! an ember hanging over the tops of the letters spends the top band, which
//! the streak along the middle of the row barely touches. The caret's own
//! cell is free (the block caret covers it), and so is every cell off the
//! text lines ([`CursorGlow::begin_ink_ledger`]).
//!
//! The beam family, water and the classic wake's mono face
//! keep the same account on a dark ground. Their streak lies UNDER the
//! letters there (the bed, [`CursorGlow::emit_dark_bed`]; water's wake), so a
//! cell's ground is what lies under it rather than the page
//! ([`InkLedger::lay_under`]), and every additive light
//! that still lands on a letter spends from what the bed left: the crown and
//! the laser's aura ([`InkLedger::HEAD_FLOOR`]), the flying grains and the
//! landing ring ([`InkLedger::FLYING_FLOOR`]), the caret's body
//! ([`InkLedger::BODY_FLOOR`]) and, last, the momentum glow
//! ([`InkLedger::MOMENTUM_FLOOR`]), each down to its own floor, so the later
//! lights keep a share and their sum cannot pass the last one. A light made of
//! quads (the bodies, the glow, the grains) is charged by its own summed
//! pixels ([`InkLedger::admit_quads`]). A head light (the crown, the ring,
//! the bodies, the glow) is admitted cell by cell ([`InkLedger::admit_masked`],
//! [`InkLedger::admit_halos_masked`]): each letter it reaches keeps the share
//! that letter can take and the light is cut to it there, recovering over a
//! smooth shoulder, while over the blank cells around the caret it lands
//! whole. The tier floors are the caret's line's: a letter on any other
//! line the account holds keeps the text bar ([`InkLedger::NEIGHBOUR_FLOOR`]).

use super::*;

/// A streak sample's lift per unit coverage on each band of its cell, top
/// first ([`InkLedger::admit_axis`]).
pub(super) type BandLifts = [[f32; 3]; InkLedger::BANDS];

/// One frame's over-ink account (see the module docs), reset by
/// [`CursorGlow::begin_ink_ledger`] before the frame's first emitter.
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
/// the cell's pixel box and the share of the light it keeps. The light
/// recovers its whole strength around it over [`InkLedger::FEATHER_X`] and
/// [`InkLedger::FEATHER_Y`] ([`InkLedger::mask_at`]).
#[derive(Clone, Copy, Debug)]
struct Dim {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    share: f32,
}

impl InkLedger {
    /// The least contrast the reference stroke keeps under the account's sum:
    /// the beam family's streak floor ([`CursorGlow::STREAK_INK_FLOOR`]).
    pub(super) const FLOOR: f32 = CursorGlow::STREAK_INK_FLOOR;
    /// The contrast the steady lights (the streak, the crown and the forge,
    /// which burn in place frame after frame) may spend a cell down to, so
    /// the flying ones (embers and steam) keep the rest of the budget, down
    /// to [`Self::FLOOR`], around the head.
    pub(super) const STEADY_FLOOR: f32 = 6.0;
    /// The horizontal bands a cell's account is kept in.
    pub(super) const BANDS: usize = 4;
    /// The text cells [`Self::admit_quads`]' scratch is sized for up front.
    const QUAD_CELLS: usize = 32;
    /// The pieces [`Self::admit_masked`]'s scratch is sized for up front.
    const MASK_PIECES: usize = 4096;
    /// The floors of a dark streak's lights over the bed, one tier each, in
    /// the order the frame lays them: the style's own head light (the crown,
    /// the laser's aura over the caret's neighbour) first, then the flying
    /// grains and the landing ring, then the caret's body, and the momentum
    /// glow last. Each spends only down to its own floor, so every later
    /// tier keeps a share, and the last is the least any letter keeps with
    /// every light on: [`Self::MOMENTUM_FLOOR`], over the 3:1 the captures
    /// are judged at by the measure's own margin (a glyph's antialiased core
    /// reads darker than its ink, and the ledger charges every light at one
    /// pixel).
    pub(crate) const HEAD_FLOOR: f32 = 3.6;
    /// See [`Self::HEAD_FLOOR`].
    pub(crate) const FLYING_FLOOR: f32 = 3.4;
    /// See [`Self::HEAD_FLOOR`].
    pub(super) const BODY_FLOOR: f32 = 3.25;
    /// See [`Self::HEAD_FLOOR`].
    pub(super) const MOMENTUM_FLOOR: f32 = 3.1;
    /// The least contrast every light together leaves a letter on a line
    /// other than the caret's (the output above, a selection below): WCAG's
    /// 4.5:1 text bar plus the account's own rounding (a share is bisected
    /// to 1/256 and each cut piece rounds its colour), since the caret's
    /// light there is decoration over letters nobody is typing. The tier
    /// floors above are the caret's line's.
    pub(super) const NEIGHBOUR_FLOOR: f32 = 4.6;
    /// How far a masked light ([`Self::admit_masked`]) takes to recover its
    /// whole strength off a dimmed text cell, as a share of the cell's width
    /// and height: a smooth shoulder, so the light never shows a cell's
    /// hard edge, laid in [`Self::FEATHER_STEP`]-px slices.
    pub(super) const FEATHER_X: f32 = 0.45;
    /// See [`Self::FEATHER_X`].
    pub(super) const FEATHER_Y: f32 = 0.35;
    /// See [`Self::FEATHER_X`].
    const FEATHER_STEP: i32 = 2;

    /// The account index of band 0 of text cell `(row, col)`, or `None` for a
    /// cell the account does not hold (off every line, past the grid, or the
    /// caret's own).
    #[inline]
    fn slot(&self, row: i32, col: i32) -> Option<usize> {
        if self.caret == Some((row, col)) || col < 0 || col as usize >= self.stride {
            return None;
        }
        self.lines.iter().enumerate().find_map(|(i, line)| {
            line.filter(|&(r, first, end)| r == row && (first..end).contains(&col))
                .map(|_| (i * self.stride + col as usize) * Self::BANDS)
        })
    }

    /// The text cell at `(row, col)`'s index into `under`, as
    /// [`Self::slot`] finds it.
    #[inline]
    fn cell(&self, row: i32, col: i32) -> Option<usize> {
        self.slot(row, col).map(|at| at / Self::BANDS)
    }

    /// Close the account: an idle ledger admits everything. Every tick does
    /// this first, so a frame that never opens one (a light ground, a dark
    /// tick) leaves no stale account for the caret's bodies to spend.
    #[inline]
    pub(super) fn close(&mut self) {
        self.on = false;
        self.print = 0;
    }

    /// Fold one cut into the frame's print.
    #[inline]
    fn note_cut(&mut self, cell: usize, share: f32) {
        self.print = aterm_hash::fnv_mix_u64(
            self.print | 1,
            ((cell as u64) << 32) | u64::from(share.to_bits()),
        );
    }

    /// Lay `rgb` under the letters of text cell `(row, col)`: the streak's
    /// bed composited over the page there. The brighter of two beds on one
    /// cell stands (by luminance), so a cell two samples share is charged
    /// the worse of them.
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

    /// How far band `band` of a cell `ch` px tall lies from the cell's middle
    /// row of pixels (a streak sample's axis), at its nearest point.
    #[inline]
    pub(super) fn band_reach(ch: f32, band: usize) -> f32 {
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
            let ch = |c: usize| (base[c] + lift[c]).min(255.0) as u32;
            crate::color_math::relative_luminance_with(table, (ch(0) << 16) | (ch(1) << 8) | ch(2))
        };
        let (ink, ground) = (lum(ink), lum(ground));
        ink.max(ground) + 0.05 >= floor * (ink.min(ground) + 0.05)
    }

    /// The largest share `0..=1` of `add` account `at` can still take while
    /// the account's ink keeps `floor` over what lies under that cell.
    fn room(&self, at: usize, add: [f32; 3], floor: f32) -> f32 {
        if add == [0.0; 3] {
            return 1.0;
        }
        let table = crate::color_math::relative_luminance_table();
        let laid = self.laid[at];
        let unpack = |rgb: u32| [16u32, 8, 0].map(|sh| ((rgb >> sh) & 0xff) as f32);
        let cell = at / Self::BANDS;
        let (ink, ground) = (unpack(self.ink[cell]), unpack(self.under[cell]));
        // A line other than the caret's (earlier output, a selection, the
        // prompt above) holds the text bar: the lights only wash the letters
        // being typed down to their tier's floor.
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
        // Eight halvings resolve the share well inside one coverage level.
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

    /// Admit a streak sample of coverage `cov` on cell `(row, col)`, its axis
    /// on the cell's middle row, lifting band `b` by `lifts[b]` per unit
    /// coverage (the layers of its stack that reach that band): the coverage
    /// the cell can take, charged. The streak claims first, and only down to
    /// [`Self::STEADY_FLOOR`].
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
        let share = (0..Self::BANDS)
            .filter(|&b| lifts[b] != [0.0; 3])
            .map(|b| self.room(base + b, lifts[b].map(|l| l * cov), Self::STEADY_FLOOR))
            .fold(1.0f32, f32::min);
        for (b, lift) in lifts.iter().enumerate() {
            if *lift != [0.0; 3] {
                for (laid, l) in self.laid[base + b].iter_mut().zip(lift) {
                    *laid += share * l * cov;
                }
            }
        }
        cov * share
    }

    /// Admit a radial halo at `(cx, cy)` with radii `(rx, ry)` and
    /// premultiplied colour `premul` (its centre's light): the share `0..=1`
    /// of its peak every text band it reaches can take down to `floor`
    /// ([`Self::STEADY_FLOOR`] or [`Self::FLOOR`]), charged to each of them
    /// at that share.
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
        // The halo's falloff at the nearest point of band `band` of a cell,
        // as the renderer's `halo_weight` (`(1 − d²)²`), or `None` where it
        // does not reach.
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
        // The cells of one line the halo's box reaches.
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
                        share = share.min(self.room(base + band, peak.map(|p| p * w), floor));
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

    /// The light `quads` (its additive quads; source-over ones pass
    /// uncharged) lays on each band of each text cell it reaches, summed as
    /// the renderer adds it (a gradient quad at each column's own value):
    /// fills `reached` (indices into `under`) and `peaks` (each band's
    /// brightest pixel per channel, [`Self::BANDS`] a cell), and says whether
    /// it reaches any.
    fn quad_peaks(&mut self, quads: &[GlowQuad]) -> bool {
        let (ox, oy, cw, ch) = self.grid;
        let (cwi, chi) = (cw as i32, ch as i32);
        self.reached.clear();
        self.peaks.clear();
        if cwi <= 0 || chi <= 0 {
            return false;
        }
        let lit = |q: &&GlowQuad| q.alpha == 0 && (q.color | q.color2) & 0x00FF_FFFF != 0;
        // The text cells the light reaches, and each one's pixel box.
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
                    // The renderer's column law for a gradient quad.
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
        // Each band's brightest pixel, per channel.
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
    fn cell_origin(&self, cell: usize) -> (i32, i32) {
        let (ox, oy, cw, ch) = self.grid;
        let stride = self.stride.max(1);
        let (slot, col) = (cell / stride, (cell % stride) as i32);
        let row = self.lines[slot].map_or(0, |l| l.0);
        (ox as i32 + col * cw as i32, oy as i32 + row * ch as i32)
    }

    /// Admit one light made of additive quads (`alpha == 0`; source-over
    /// quads pass uncharged) down to `floor`, as one: the share `0..=1` of
    /// it every text band it reaches can take, charged at that share. A band
    /// is charged the light's own brightest pixel in it, per channel
    /// ([`Self::quad_peaks`]), so a body whose quads tile its halo side by
    /// side is charged its halo, not the sum of its tiles.
    pub(super) fn admit_quads(&mut self, quads: &[GlowQuad], floor: f32) -> f32 {
        if !self.on || !self.quad_peaks(quads) {
            return 1.0;
        }
        let mut share = 1.0f32;
        for (k, &cell) in self.reached.iter().enumerate() {
            for band in 0..Self::BANDS {
                let add = self.peaks[k * Self::BANDS + band];
                if add != [0.0; 3] {
                    share = share.min(self.room(cell * Self::BANDS + band, add, floor));
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

    /// Admit one light made of the additive quads `quads[range]` down to
    /// `floor`, each text cell it reaches on its own: a cell keeps the share
    /// of the light its own bands can take ([`Self::room`]) and is charged
    /// that, and the light is cut to it there and kept whole everywhere
    /// else, recovering off each dimmed cell over a smooth shoulder
    /// ([`Self::mask_at`]). So a caret's bloom stays at full strength over
    /// the blank cells around it and over the caret's own cell, and only the
    /// letters it reaches hold it back. The pieces are spliced over
    /// `quads[range]` in order (source-over quads pass through whole).
    /// Returns the least share any cell kept (`1.0` for the whole light).
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
                }
            }
            for band in 0..Self::BANDS {
                let add = self.peaks[k * Self::BANDS + band];
                for (laid, a) in self.laid[cell * Self::BANDS + band].iter_mut().zip(add) {
                    *laid += share * a;
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
    fn dim_of(&self, cell: usize, share: f32) -> Dim {
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

    /// Fill `knots_x`/`knots_y` with the slice edges of the box
    /// `[x0, x1) × [y0, y1)` against the dimmed cells: its own edges, each
    /// dimmed cell's edges, and the feather's slices off them, inside the
    /// box. Returns whether any dimmed cell's feathered box meets it.
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

    /// One quad of a masked light, cut into the pieces [`Self::mask_at`]
    /// asks for, into `out`: a source-over quad, or one no dimmed cell's
    /// feather reaches, whole; otherwise each slice a gradient piece whose
    /// ends carry the quad's own colour there times the mask there (the
    /// column law makes a piece's ends meet its neighbours').
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

    /// Admit the radial halos `halos[from..]` (additive ones; a veil passes
    /// through) down to `floor`, each halo once (the per-row pieces one
    /// [`push_halo`] call splits a halo into share its centre, radii and
    /// colour) and each text cell it reaches on its own, as
    /// [`Self::admit_masked`] admits quads: a cell keeps the share its bands
    /// can take at the halo's falloff there, and the halo is cut to it there
    /// and kept whole elsewhere, each slice at the mask at its middle.
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

    /// One radial halo's account, cell by cell ([`Self::admit_halos_masked`]):
    /// each text cell it reaches is charged the share of the halo its bands
    /// can take down to `floor` at the halo's falloff there, and a cell that
    /// keeps less than the whole halo joins `dims`.
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
                    }
                }
                for (band, w) in weights.into_iter().enumerate() {
                    if let Some(w) = w {
                        for (laid, p) in self.laid[base + band].iter_mut().zip(peak) {
                            *laid += share * p * w;
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
    /// Admit quads already laid one by one down to `floor` (each a grain of
    /// its own), scaling each to its share.
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
fn scale_rgb(c: u32, share: f32) -> u32 {
    let share = share.clamp(0.0, 1.0);
    let ch = |sh: u32| (((c >> sh) & 0xff) as f32 * share) as u32;
    (c & 0xFF00_0000) | (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Scale every additive quad of `quads` (`alpha == 0`) by `share`, colour
/// and gradient end alike: the light [`InkLedger::admit_quads`] admitted.
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
    /// Open this frame's [`InkLedger`]: on a dark ground under Fire, a style
    /// whose streak lies under the letters there ([`Self::streak_lies_under`])
    /// or the classic wake's mono face; idle otherwise. The caret's cell is
    /// free. Fire keeps
    /// its account on the reference pair, as its caps were solved; the others
    /// on the live theme's, the pair their bed is solved for
    /// ([`Self::bed_pair`]). Allocates only when the grid widens.
    ///
    /// The account holds every line of text the row probe shows. Where the
    /// probe is silent it assumes text rather than air, since fire's light
    /// rises off the typed run onto whatever line sits above it: a caret row
    /// with no probe holds the typed sparks' span (the letters just typed),
    /// and a neighbouring grid row the host did not capture (the GUI captures
    /// them only for the rainbow kitty) holds the typed span widened by
    /// [`Self::INK_LEDGER_REACH`] cells, where the shower and the steam rise.
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
            // The neighbours the probe did not capture, over the typed run
            // (or around the caret before one is laid).
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
            // A neighbour the frame's own cells read holds what they show
            // (nothing, if blank); one nobody read holds the typed span.
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
        // A letter the frame's own cells show is held against the colours
        // it is drawn in (a selection's ground, a coloured prompt's ink),
        // not the page's pair. Fire keeps the reference pair its caps were
        // solved on.
        if !fire {
            let frame = &self.frame_text;
            for (slot, line) in ledger.lines.iter().enumerate() {
                let Some((row, first, end)) = *line else {
                    continue;
                };
                for col in first.max(0)..end.min(geom.cols as i32) {
                    if let Some((fg, bg)) = frame.pair(row, col) {
                        // A blank cell inside the line keeps the pair's ink
                        // as its stand-in letter, on its own ground.
                        let i = slot * geom.cols + col as usize;
                        ledger.ink[i] = fg.unwrap_or(ledger.pair.0);
                        ledger.under[i] = bg;
                    }
                }
            }
        }
        // `admit_quads`' scratch, sized once for the widest light it meets
        // (a landing ring at its largest reaches about this many text cells),
        // so a frame whose grain straddles one more cell than the last does
        // not allocate.
        let want = InkLedger::QUAD_CELLS * geom.cw * geom.ch;
        if ledger.px.capacity() < want {
            ledger.px.reserve(want - ledger.px.len());
        }
        if ledger.reached.capacity() < InkLedger::QUAD_CELLS {
            ledger
                .reached
                .reserve(InkLedger::QUAD_CELLS - ledger.reached.len());
        }
        // The masked lights' scratch ([`InkLedger::admit_masked`]), sized
        // once for the most a frame's head lights cut into, so a frame that
        // cuts one more piece than the last does not allocate.
        reserve_to(&mut ledger.peaks, InkLedger::QUAD_CELLS * InkLedger::BANDS);
        reserve_to(&mut ledger.dims, InkLedger::QUAD_CELLS);
        reserve_to(&mut ledger.knots_x, InkLedger::QUAD_CELLS * 8);
        reserve_to(&mut ledger.knots_y, InkLedger::QUAD_CELLS * 8);
        reserve_to(&mut ledger.pieces, InkLedger::MASK_PIECES);
        reserve_to(&mut ledger.halo_pieces, InkLedger::MASK_PIECES);
    }

    /// The pair a dark bed is solved for and its account kept on: the live
    /// theme's ink and page, or the reference pair when the host handed no
    /// coherent one (`fg == bg` reads as unknown).
    #[inline]
    pub(crate) fn bed_pair(cfg: &GlowConfig) -> (u32, u32) {
        let (fg, bg) = (cfg.theme_fg & 0x00FF_FFFF, cfg.theme_bg & 0x00FF_FFFF);
        if fg == bg {
            (Self::STREAK_REF_INK, Self::STREAK_REF_GROUND)
        } else {
            (fg, bg)
        }
    }

    /// Whether `style`'s streak lies under the letters on a dark ground: the
    /// beam family's bed ([`Self::emit_comet`]) and water's wake. Their
    /// over-text lights spend the over-ink account ([`InkLedger`]).
    #[inline]
    pub(super) fn streak_lies_under(style: GlowStyle) -> bool {
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

    /// A print of every cut this frame's over-ink account made to a masked
    /// light ([`InkLedger::admit_masked`]): which letters held a light back
    /// and by how much, so a host folding it into its frame fingerprint
    /// repaints when only the cuts change. `0` with no cut.
    #[inline]
    #[must_use]
    pub fn over_ink_print(&self) -> u64 {
        self.ink_ledger.print
    }

    /// Admit one of the caret's bodies, or the momentum glow, into this
    /// frame's over-ink account cell by cell ([`InkLedger::admit_masked`]):
    /// `quads[range]` is the light it just laid, cut where it reaches a
    /// letter to the share that letter's cell can take and kept whole over
    /// the blank cells and the caret's own, the pieces spliced in its place.
    /// The host calls this after [`Self::tick`], in the order the lights are
    /// laid; without an open account (a light ground, a style whose light
    /// does not reach the letters) it changes nothing. Returns the least
    /// share any letter kept (`1.0` for the whole light), which the host
    /// folds into its frame fingerprint when it bound.
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
}

/// Which of the lights laid after the aurora a host admits
/// ([`CursorGlow::admit_over_ink`]): each spends the over-ink account only
/// down to its own floor, so the later one keeps a share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverInkTier {
    /// The caret's own body (the comet nucleus, the phaser emitter, the
    /// light rod, the droplet).
    Body,
    /// The typing-momentum glow, laid last.
    Momentum,
}
