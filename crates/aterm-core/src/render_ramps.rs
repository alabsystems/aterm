// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A glow stream's ramps (`docs/DESIGN-card-engine-2026-10-19.md` §4.2's law
//! F, lane B5).
//!
//! A ribbon slab lays its light one pixel row at a time, each row a
//! [`GlowQuad`] of its own premultiplied ends. A [`GlowRamp`] carries a
//! slab's rows within one row band as one item: its row `r` is exactly the
//! quad `(x, y + r, w, 1)` whose left and right ends are `ends[ink + 2r]` and
//! `ends[ink + 2r + 1]` ([`GlowEnd`]), drawn by the glow gradient law, and a
//! row whose two ends are zero is no light at all (the quad never laid). A
//! stream keeps its ramps beside its quads ([`GlowRamps`]), each drawn just
//! before the quad its `at` names, so every reader meets the stream's lights
//! in the order they were laid: a source-over light over another is still
//! drawn after it.
//!
//! A ramp may also carry one taller light, a uniform piece several rows high
//! (a comet slab's core): its run of rows, from `join` through
//! `join + joined`, every one of them row `join`'s ends, which the ramp keeps
//! once ([`GlowRamp::end_row`]). A reader that reads the ramp a pixel row at
//! a time reads each run row as row `join`, and the run is the one quad
//! `(x, y + join, w, joined + 1)` to every reader that counts or lays the
//! stream's lights ([`GlowRamp::row_quad`]). The GPU's `fx_ramp_row` folds a
//! row onto its ends by the same law.

use crate::render::GlowQuad;

/// One end of a ramp row: the premultiplied colour `0x00RRGGBB` with the
/// source-over opacity in the top byte (`0` adds), as a [`GlowQuad`] end.
pub type GlowEnd = u32;

/// The end of premultiplied `color` at opacity `alpha`.
#[must_use]
#[inline]
pub const fn glow_end(color: u32, alpha: u8) -> GlowEnd {
    (color & 0x00FF_FFFF) | ((alpha as u32) << 24)
}

/// An end's premultiplied colour `0x00RRGGBB`.
#[must_use]
#[inline]
pub const fn end_color(end: GlowEnd) -> u32 {
    end & 0x00FF_FFFF
}

/// An end's source-over opacity (`0` adds).
#[must_use]
#[inline]
pub const fn end_alpha(end: GlowEnd) -> u8 {
    (end >> 24) as u8
}

/// `n`, a place among a stream's quads or ends, as a ramp keeps it.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a stream holds far fewer than 2^32 quads or ends: its budgets cap it at thousands"
)]
#[inline]
const fn place(n: usize) -> u32 {
    n as u32
}

/// A slab of one-pixel glow rows in one row band ([`self`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GlowRamp {
    /// The index of the stream's quad this ramp is drawn just before (the
    /// quad count when it was laid).
    pub at: u32,
    /// The row band its rows live in: a damage hint, as [`GlowQuad::row`].
    pub row: u16,
    /// Pixel X of every row.
    pub x: u16,
    /// Pixel Y of row 0.
    pub y: u16,
    /// Every row's width in pixels.
    pub w: u16,
    /// Rows (within the one `row` band).
    pub h: u16,
    /// The first row of the ramp's run, the one light its rows through
    /// `join + joined` lay ([`self`]).
    pub join: u16,
    /// The rows after `join` that repeat its ends as the same light (`0`:
    /// the ramp has no run).
    pub joined: u16,
    /// Row 0's left end in [`GlowRamps::ends`]: row `r`'s ends are
    /// `ends[ink + 2s]` and `ends[ink + 2s + 1]`, `s` its
    /// [`Self::end_row`].
    pub ink: u32,
}

impl GlowRamp {
    /// The row of kept ends row `r` reads: `r` itself before the run, the
    /// run's first row through it, `joined` fewer after it (the shaders'
    /// `fx_ramp_row`).
    #[must_use]
    #[inline]
    pub const fn end_row(&self, r: u16) -> u16 {
        // Most ramps have no run: the reader's per-pixel path stays a load.
        if self.joined == 0 {
            return r;
        }
        let past = r.saturating_sub(self.join);
        r - if past < self.joined {
            past
        } else {
            self.joined
        }
    }

    /// The ends the ramp keeps in [`GlowRamps::ends`] from `ink`: two a row,
    /// its run's rows two in all.
    #[must_use]
    #[inline]
    pub const fn end_count(&self) -> usize {
        2 * (self.h - self.joined) as usize
    }

    /// The ends the ramp keeps: [`Self::end_count`] of them from `ink`.
    #[must_use]
    #[inline]
    pub fn kept_ends<'e>(&self, ends: &'e [GlowEnd]) -> &'e [GlowEnd] {
        let k = self.ink as usize;
        &ends[k..k + self.end_count()]
    }

    /// Canonical lossless words of the ramp's band, box, run and place among
    /// its stream's quads, for cursor-effect fingerprints: with its kept ends
    /// ([`Self::kept_ends`]) they fix every pixel it draws. The second word's
    /// top 16 bits are zero.
    #[doc(hidden)]
    #[must_use]
    #[inline]
    pub const fn damage_words(&self) -> [u64; 3] {
        [
            self.row as u64 | (self.x as u64) << 16 | (self.y as u64) << 32 | (self.w as u64) << 48,
            self.h as u64 | (self.join as u64) << 16 | (self.joined as u64) << 32,
            self.at as u64,
        ]
    }

    /// Row `r`'s ends, `(left, right)`.
    #[must_use]
    #[inline]
    pub fn row_ends(&self, ends: &[GlowEnd], r: u16) -> (GlowEnd, GlowEnd) {
        let k = self.ink as usize + 2 * usize::from(self.end_row(r));
        (ends[k], ends[k + 1])
    }

    /// Whether row `r` continues the ramp's run: a row of its light, not a
    /// light of its own.
    #[must_use]
    #[inline]
    pub const fn continues(&self, r: u16) -> bool {
        r > self.join && r - self.join <= self.joined
    }

    /// The lights the ramp lays: its lit rows, its run one of them (the
    /// rows of ends it keeps that are lit).
    #[must_use]
    #[inline]
    pub fn lights(&self, ends: &[GlowEnd]) -> usize {
        let k = self.ink as usize;
        ends[k..k + self.end_count()]
            .as_chunks::<2>()
            .0
            .iter()
            .filter(|e| e[0] | e[1] != 0)
            .count()
    }

    /// Keep rows `top .. top + rows` of the ramp (`rows > 0`), its run cut
    /// with them as a quad of it would be.
    #[inline]
    pub const fn keep_rows(&mut self, top: u16, rows: u16) {
        self.ink += 2 * self.end_row(top) as u32;
        if self.joined > 0 {
            let first = if self.join > top { self.join } else { top };
            let end = self.join + self.joined;
            let last = if end < top + rows - 1 {
                end
            } else {
                top + rows - 1
            };
            (self.join, self.joined) = if first <= last {
                (first - top, last - first)
            } else {
                (0, 0)
            };
        }
        self.y += top;
        self.h = rows;
    }

    /// Narrow the ramp to the box `[x0, x1) × [y0, y1)` as clipping each of
    /// its rows' quads to it would (a row keeps its ends over the narrower
    /// span); `false` when nothing of it is left.
    #[must_use]
    #[inline]
    pub fn clip_to(&mut self, (x0, y0, x1, y1): (u16, u16, u16, u16)) -> bool {
        let (nx0, nx1) = (self.x.max(x0), self.x.saturating_add(self.w).min(x1));
        let (ny0, ny1) = (self.y.max(y0), self.y.saturating_add(self.h).min(y1));
        if nx1 <= nx0 || ny1 <= ny0 {
            return false;
        }
        self.keep_rows(ny0 - self.y, ny1 - ny0);
        (self.x, self.w) = (nx0, nx1 - nx0);
        true
    }

    /// Row `r` as the light it lays: the quad it stands for (the run's
    /// first row its whole run), `None` where it draws nothing or continues
    /// the run.
    #[must_use]
    #[inline]
    pub fn row_quad(&self, ends: &[GlowEnd], r: u16) -> Option<GlowQuad> {
        if self.continues(r) {
            return None;
        }
        let (a, b) = self.row_ends(ends, r);
        (a | b != 0).then_some(GlowQuad {
            row: self.row,
            x: self.x,
            y: self.y + r,
            w: self.w,
            h: if r == self.join { self.joined + 1 } else { 1 },
            color: end_color(a),
            alpha: end_alpha(a),
            color2: end_color(b),
            alpha2: end_alpha(b),
        })
    }

    /// The lights the ramp lays, in row order: each lit row's quad, its
    /// run's one quad ([`Self::row_quad`]), read off the ends it keeps.
    #[inline]
    pub fn quads<'e>(&self, ends: &'e [GlowEnd]) -> impl Iterator<Item = GlowQuad> + 'e {
        self.slot_quads(ends, 0, self.h - self.joined)
    }

    /// [`Self::quads`] of the rows that meet window rows `y0 .. y1` (its run
    /// whole where any of the run's rows does), in row order: a reader of
    /// one band of rows reads only the ends that lie in it.
    #[inline]
    pub fn quads_within<'e>(
        &self,
        ends: &'e [GlowEnd],
        (y0, y1): (i32, i32),
    ) -> impl Iterator<Item = GlowQuad> + 'e {
        let (top, h) = (i32::from(self.y), i32::from(self.h));
        // A window row as a row of the ramp, held to `0 ..= h`, so it fits.
        let row = |y: i32| u16::try_from((y - top).clamp(0, h)).unwrap_or(self.h);
        let (a, b) = (row(y0), row(y1));
        let (s0, s1) = if a < b {
            (self.end_row(a), self.end_row(b - 1) + 1)
        } else {
            (0, 0)
        };
        self.slot_quads(ends, s0, s1)
    }

    /// The lights of the ramp's rows of kept ends `s0 .. s1`
    /// ([`Self::end_row`]), each lit one's quad.
    #[inline]
    fn slot_quads<'e>(
        &self,
        ends: &'e [GlowEnd],
        s0: u16,
        s1: u16,
    ) -> impl Iterator<Item = GlowQuad> + 'e {
        let (r, k) = (*self, self.ink as usize);
        ends[k + 2 * usize::from(s0)..k + 2 * usize::from(s1)]
            .as_chunks::<2>()
            .0
            .iter()
            .zip(s0..)
            .filter(|(e, _)| e[0] | e[1] != 0)
            .map(move |(e, s)| GlowQuad {
                row: r.row,
                x: r.x,
                y: r.y + s + if s > r.join { r.joined } else { 0 },
                w: r.w,
                h: if s == r.join { r.joined + 1 } else { 1 },
                color: end_color(e[0]),
                alpha: end_alpha(e[0]),
                color2: end_color(e[1]),
                alpha2: end_alpha(e[1]),
            })
    }
}

/// Where a stream's ramps stood, to truncate them back to ([`GlowRamps::mark`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RampMark {
    items: usize,
    ends: usize,
    lit: usize,
}

impl RampMark {
    /// The lit rows the stream's ramps held then ([`GlowRamps::lit`]).
    #[must_use]
    #[inline]
    pub const fn lit(self) -> usize {
        self.lit
    }

    /// The ramps the stream held then: the index of the first ramp laid since.
    #[must_use]
    #[inline]
    pub const fn items(self) -> usize {
        self.items
    }
}

/// One run of a stream in draw order ([`GlowRamps::in_order`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Laid {
    /// The stream's quads in this range.
    Quads(std::ops::Range<usize>),
    /// The ramp at this index of [`GlowRamps::items`].
    Ramp(usize),
}

/// A glow stream's ramps, beside its quads ([`self`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GlowRamps {
    /// The ramps, in the order they were laid (`at` never falls).
    pub items: Vec<GlowRamp>,
    /// Every ramp's row ends, a run's once ([`GlowRamp::end_row`]).
    pub ends: Vec<GlowEnd>,
    /// How many rows of the ramps are lit: the quads they stand for, which
    /// every budget and count over the stream counts.
    pub lit: usize,
}

impl GlowRamps {
    /// A stream with no ramp.
    pub const EMPTY: Self = Self {
        items: Vec::new(),
        ends: Vec::new(),
        lit: 0,
    };

    /// Whether the stream has no ramp.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Drop every ramp, keeping the capacity.
    #[inline]
    pub fn clear(&mut self) {
        self.items.clear();
        self.ends.clear();
        self.lit = 0;
    }

    /// The stream's light count with `quads` quads: what it would hold had
    /// every lit ramp row been laid as its quad.
    #[must_use]
    #[inline]
    pub const fn laid(&self, quads: usize) -> usize {
        quads + self.lit
    }

    /// Where the ramps stand now.
    #[must_use]
    #[inline]
    pub const fn mark(&self) -> RampMark {
        RampMark {
            items: self.items.len(),
            ends: self.ends.len(),
            lit: self.lit,
        }
    }

    /// Drop every ramp laid since `mark`.
    #[inline]
    pub fn truncate(&mut self, mark: RampMark) {
        self.items.truncate(mark.items);
        self.ends.truncate(mark.ends);
        self.lit = mark.lit;
    }

    /// The ramps laid since `mark`.
    #[must_use]
    #[inline]
    pub fn since(&self, mark: RampMark) -> &[GlowRamp] {
        &self.items[mark.items.min(self.items.len())..]
    }

    /// Open a ramp of one row, `(x, y, w)` with its two `ends`, drawn before
    /// quad `at` and tagged `row`; the row counts lit unless both ends are 0.
    /// A test fixture's ([`Self::open_run`] is what the producers fold with).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn open(
        &mut self,
        at: usize,
        row: u16,
        (x, y, w): (u16, u16, u16),
        ends: (GlowEnd, GlowEnd),
    ) {
        self.items.push(GlowRamp {
            at: place(at),
            row,
            x,
            y,
            w,
            h: 1,
            join: 0,
            joined: 0,
            ink: place(self.ends.len()),
        });
        self.push_rows(0, ends);
    }

    /// Open a ramp of one light `h` rows tall (`h > 0`), `(x, y, w)` with
    /// its two `ends` on every row (kept once), drawn before quad `at` and
    /// tagged `row`: the quad `(x, y, w, h)` of those ends.
    #[inline]
    pub fn open_run(
        &mut self,
        at: usize,
        row: u16,
        (x, y, w): (u16, u16, u16),
        h: u16,
        ends: (GlowEnd, GlowEnd),
    ) {
        self.items.push(GlowRamp {
            at: place(at),
            row,
            x,
            y,
            w,
            h,
            join: 0,
            joined: h - 1,
            ink: place(self.ends.len()),
        });
        self.push_rows(0, ends);
    }

    /// Lay the next light of the last ramp, `h` rows tall (`h > 0`) after
    /// `gap` dark rows, as [`Self::open_run`] lays one; `false`, laying
    /// nothing, where it is taller than a row and the ramp has its run.
    #[inline]
    pub fn extend_run(&mut self, gap: u16, h: u16, (a, b): (GlowEnd, GlowEnd)) -> bool {
        let Some(last) = self.items.last_mut() else {
            return false;
        };
        if h > 1 {
            if last.joined > 0 {
                return false;
            }
            (last.join, last.joined) = (last.h + gap, h - 1);
        }
        last.h += gap + h;
        self.push_rows(gap, (a, b));
        true
    }

    /// Lay the next row of the last ramp ([`Self::open`]), after `gap` dark
    /// rows; the row counts lit unless both ends are 0. A test fixture's
    /// ([`Self::extend_run`] is what the producers fold with).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn extend_last(&mut self, gap: u16, (a, b): (GlowEnd, GlowEnd)) {
        let Some(last) = self.items.last_mut() else {
            return;
        };
        last.h += gap + 1;
        self.push_rows(gap, (a, b));
    }

    /// Keep `gap` dark rows of ends, then the row `(a, b)`, lit unless both
    /// ends are 0.
    #[inline]
    fn push_rows(&mut self, gap: u16, (a, b): (GlowEnd, GlowEnd)) {
        if gap > 0 {
            self.ends.resize(self.ends.len() + 2 * usize::from(gap), 0);
        }
        self.ends.extend_from_slice(&[a, b]);
        self.lit += usize::from(a | b != 0);
    }

    /// Keep the stream's quads `quad` keeps and the ramps `ramp` keeps, each
    /// free to narrow its light ([`GlowRamp::clip_to`]); every ramp keeps
    /// its place among the quads kept.
    pub fn retain_mut(
        &mut self,
        quads: &mut Vec<GlowQuad>,
        mut quad: impl FnMut(&mut GlowQuad) -> bool,
        mut ramp: impl FnMut(&mut GlowRamp) -> bool,
    ) {
        if self.items.is_empty() {
            quads.retain_mut(quad);
            return;
        }
        let (mut next, mut kept) = (0usize, 0usize);
        for k in 0..=self.items.len() {
            let to = self
                .items
                .get(k)
                .map_or(quads.len(), |r| (r.at as usize).min(quads.len()));
            while next < to {
                if quad(&mut quads[next]) {
                    quads.swap(kept, next);
                    kept += 1;
                }
                next += 1;
            }
            if let Some(r) = self.items.get_mut(k) {
                r.at = place(kept);
            }
        }
        quads.truncate(kept);
        self.items.retain_mut(|r| ramp(r));
        self.lit = self.count_lit();
    }

    /// [`Self::retain_mut`] over what was laid since `from` (the stream's
    /// quad count and [`Self::mark`] then), each ramp kept free to rewrite
    /// its rows' ends in `ends` (its own [`GlowRamp::end_count`] from `ink`)
    /// and to narrow to fewer of them; the lit count follows the ends.
    pub fn retain_since(
        &mut self,
        quads: &mut Vec<GlowQuad>,
        (q0, mark): (usize, RampMark),
        mut quad: impl FnMut(&mut GlowQuad) -> bool,
        mut ramp: impl FnMut(&mut GlowRamp, &mut [GlowEnd]) -> bool,
    ) {
        let q0 = q0.min(quads.len());
        let k0 = mark.items.min(self.items.len());
        let (mut next, mut kept) = (q0, q0);
        for k in k0..=self.items.len() {
            let to = self
                .items
                .get(k)
                .map_or(quads.len(), |r| (r.at as usize).clamp(q0, quads.len()));
            while next < to {
                if quad(&mut quads[next]) {
                    quads.swap(kept, next);
                    kept += 1;
                }
                next += 1;
            }
            if let Some(r) = self.items.get_mut(k) {
                r.at = place(kept);
            }
        }
        quads.truncate(kept);
        let mut keep = k0;
        // The ramps before `mark` are untouched: their lights are the mark's
        // (counted afresh where the stream has since lost some of them).
        let mut lit = if k0 == mark.items {
            mark.lit
        } else {
            self.items[..k0].iter().map(|r| r.lights(&self.ends)).sum()
        };
        for k in k0..self.items.len() {
            let mut r = self.items[k];
            if ramp(&mut r, &mut self.ends) {
                lit += r.lights(&self.ends);
                self.items[keep] = r;
                keep += 1;
            }
        }
        self.items.truncate(keep);
        self.lit = lit;
    }

    /// The lights of the ramps held ([`Self::lit`]): each ramp's lit rows,
    /// its run one of them.
    #[must_use]
    pub fn count_lit(&self) -> usize {
        self.items.iter().map(|r| r.lights(&self.ends)).sum()
    }

    /// Walk the stream (`quads` quads and these ramps) in draw order.
    #[inline]
    pub fn in_order(&self, quads: usize, mut each: impl FnMut(Laid)) {
        let mut next = 0usize;
        for (k, ramp) in self.items.iter().enumerate() {
            let at = (ramp.at as usize).min(quads);
            if at > next {
                each(Laid::Quads(next..at));
                next = at;
            }
            each(Laid::Ramp(k));
        }
        if quads > next {
            each(Laid::Quads(next..quads));
        }
    }

    /// The stream in draw order as quads alone, every lit ramp row as the
    /// quad it stands for (the legacy stream, for the readers that read it).
    pub fn expand_into(&self, quads: &[GlowQuad], out: &mut Vec<GlowQuad>) {
        out.reserve(self.laid(quads.len()));
        self.in_order(quads.len(), |laid| match laid {
            Laid::Quads(r) => out.extend_from_slice(&quads[r]),
            Laid::Ramp(k) => {
                out.extend(self.items[k].quads(&self.ends));
            }
        });
    }

    /// Fold the ramps into `quads` ([`Self::expand_into`]) and drop them.
    pub fn flatten_into(&mut self, quads: &mut Vec<GlowQuad>) {
        if self.items.is_empty() {
            return;
        }
        let mut all = Vec::with_capacity(self.laid(quads.len()));
        self.expand_into(quads, &mut all);
        *quads = all;
        self.clear();
    }

    /// Lay rows `top .. top + rows` of `ramp` (`rows > 0`, its ends read
    /// from `ends`, another stream's), its run cut with them as
    /// [`GlowRamp::keep_rows`] cuts it, drawn before this stream's quad `at`;
    /// rows with no light among them lay nothing.
    pub fn copy_rows(
        &mut self,
        ramp: &GlowRamp,
        ends: &[GlowEnd],
        at: usize,
        (top, rows): (u16, u16),
    ) {
        let mut r = *ramp;
        r.keep_rows(top, rows);
        let lights = r.lights(ends);
        if lights == 0 {
            return;
        }
        let k = r.ink as usize;
        (r.at, r.ink) = (place(at), place(self.ends.len()));
        self.ends.extend_from_slice(&ends[k..k + r.end_count()]);
        self.lit += lights;
        self.items.push(r);
    }

    /// Keep the stream's quads `keep` keeps, every ramp in its place among
    /// them ([`Self::retain_mut`] keeping every ramp whole).
    pub fn retain_quads(
        &mut self,
        quads: &mut Vec<GlowQuad>,
        mut keep: impl FnMut(&GlowQuad) -> bool,
    ) {
        if self.items.is_empty() {
            quads.retain(keep);
            return;
        }
        let (mut next, mut kept) = (0usize, 0usize);
        for k in 0..=self.items.len() {
            let to = self
                .items
                .get(k)
                .map_or(quads.len(), |r| (r.at as usize).min(quads.len()));
            while next < to {
                if keep(&quads[next]) {
                    quads.swap(kept, next);
                    kept += 1;
                }
                next += 1;
            }
            if let Some(r) = self.items.get_mut(k) {
                r.at = place(kept);
            }
        }
        quads.truncate(kept);
    }

    /// Append `other`'s ramps, its stream's quads appended after this
    /// stream's first `base`.
    pub fn append_from(&mut self, other: &Self, base: usize) {
        let ink = place(self.ends.len());
        self.items.extend(other.items.iter().map(|r| GlowRamp {
            at: r.at + place(base),
            ink: r.ink + ink,
            ..*r
        }));
        self.ends.extend_from_slice(&other.ends);
        self.lit += other.lit;
    }

    /// Keep the stream's first `n` lights in draw order (`quads` truncated
    /// with it), as truncating its legacy quads to `n` would: a ramp the cut
    /// falls in keeps its rows through its `n`-th light.
    pub fn truncate_laid(&mut self, quads: &mut Vec<GlowQuad>, n: usize) {
        if self.laid(quads.len()) <= n {
            return;
        }
        let mut left = n;
        let mut quads_kept = quads.len();
        let mut items_kept = self.items.len();
        let mut next = 0usize;
        let mut cut = false;
        for k in 0..self.items.len() {
            let at = (self.items[k].at as usize).min(quads.len());
            let run = at - next;
            if run >= left {
                quads_kept = next + left;
                items_kept = k;
                cut = true;
                break;
            }
            left -= run;
            next = at;
            let ramp = self.items[k];
            let lit = ramp.lights(&self.ends);
            if lit >= left {
                quads_kept = at;
                items_kept = k + usize::from(left > 0);
                if left > 0 {
                    // Through the `left`-th light's last row: a run whole.
                    let q = ramp
                        .quads(&self.ends)
                        .nth(left - 1)
                        .expect("the ramp lays `left` lights");
                    self.items[k].keep_rows(0, q.y + q.h - ramp.y);
                }
                cut = true;
                break;
            }
            left -= lit;
        }
        if !cut {
            quads_kept = next + left;
        }
        quads.truncate(quads_kept);
        self.items.truncate(items_kept);
        let ends = self
            .items
            .last()
            .map_or(0, |r| r.ink as usize + r.end_count());
        self.ends.truncate(ends);
        self.lit = self.count_lit();
    }
}

#[cfg(test)]
#[path = "render_ramps_tests.rs"]
mod tests;
