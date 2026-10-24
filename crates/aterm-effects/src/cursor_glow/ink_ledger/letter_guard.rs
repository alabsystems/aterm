// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LETTER GUARD: whatever a frame's lights add up to, every letter they
//! lie on keeps the legibility bar.
//!
//! Each style keeps its own lights off the letters as it emits them (the ink
//! ledger's tiers, the bed, the fire's cap, the ribbon's hold), each against
//! its own account and floor, so the frame's sum can still wash a letter out:
//! unguarded, the trail gallery measured the newest letters beside a dark
//! caret at 2.65 to 4.17:1 (Beam, Comet, Laser, Lumen, Phaser, Sparkle),
//! Water's wave through its letters at 2.3, a ribbon's Enter flare on the echo
//! at 2.84 and a fire spark on `好` at 1.79. The guard is the one law over all
//! of them, run once on the frame's finished streams before they are
//! published: for each lettered cell of the frame's own text (the caret's own
//! cell is the caret's law), it composites every light that reaches the cell
//! exactly as the renderer will (the under-text quads on the ground, then the
//! over-text quads and the halos on ground and ink alike), reads each pixel of
//! the cell as solid ink ([`SOLID_SHARE`] of the clean ratio, as `aterm-lab
//! trail-contrast` counts ink) against the worst ground within [`RING`] px of
//! it, and where the cell would fall under [`LETTER_FLOOR`] it finds the
//! largest share of those lights that keeps the floor. A letter whose solid
//! ink reads under the floor on the bare page (a clean ratio under about
//! 5.4:1: Solarized Dark's text, a 5:1 grey, many a coloured output) keeps
//! [`OWN_SHARE`] of its own bare reading instead: the floor it cannot reach
//! never leaves it to the whole light. Every light is then cut to that share
//! over the cell (and its ring), easing back to its whole strength over the
//! ledger's feather off the cell, so a bloom hugs its caret and a flare dims
//! behind the word it crosses with no hard edge. A cell the lights leave at
//! the floor is untouched, and a frame with no such cell is byte-identical to
//! the one it would have drawn.
//!
//! A row's cuts are one eased ramp, not a cell-by-cell comb ([`RAMP_SLOPE`]):
//! between its first and last cut cell, every cell (a word gap, a letter the
//! whole light left at the floor) keeps no more than the least of each cut
//! cell's share plus the slope per column between them. Cut letter by letter,
//! Phaser's neon bar kept its whole light in each word gap between dimmed
//! letters and its spectrum underline went dashed (2026-10-23, looks6 N1).
//! The ramp only ever lowers a share, so every letter keeps its bar.

use super::*;

/// The bar every letter a light lies on keeps (WCAG AA for text, with a
/// hair of room for the judge's rounding).
pub(crate) const LETTER_FLOOR: f32 = 4.6;
/// The share of a letter's clean ratio its solid ink keeps (`aterm-lab
/// trail-contrast`'s `SOLID_SHARE`): the guard judges the dimmest ink the
/// judge counts, not the face's full colour.
const SOLID_SHARE: f32 = 0.85;
/// The share of its own bare reading a letter whose solid ink reads under
/// [`LETTER_FLOOR`] on the bare page keeps under the lights ([`letter_bar`]).
/// Given up on, such letters would lie unguarded: a 5.0:1 grey under
/// Sparkle's caret bloom measured 1.01:1 and Solarized Dark's text under
/// Laser's Enter bolt 1.34:1, the guard on or off.
const OWN_SHARE: f32 = 0.97;
/// How far from an ink pixel its ground is read, px (the judge's ring),
/// and how far round a lettered cell the guard judges a light: a light
/// kept that far off a lettered row is never cut (the dark meteor train).
pub(crate) const RING: i32 = 2;
/// Bisection steps for a cell's share.
const STEPS: usize = 7;
/// How much more of its light a cell of a cut row keeps than a cut cell one
/// column away (a share per column): the row's cuts ease as one ramp, and a
/// word gap between two cut letters is no brighter than they are beyond it.
const RAMP_SLOPE: f32 = 0.08;

/// Which stream a light rides, in the renderer's draw order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stream {
    /// `glow_under`: on the ground, beneath the ink.
    Under,
    /// `cursor_glow_add`: over ground and ink.
    Over,
    /// `glow_halo`, [`HaloMode::Add`] (drawn before every `Over` halo).
    HaloAdd,
    /// `glow_halo`, [`HaloMode::Over`].
    HaloOver,
}

#[derive(Clone, Copy)]
struct RowLight {
    stream: Stream,
    idx: u32,
    x0: i32,
    x1: i32,
}

/// The guard's resident scratch ([`CursorGlow::guard_letters`]).
#[derive(Default)]
pub(crate) struct LetterGuard {
    /// The lights that reach the row being read.
    row: Vec<RowLight>,
    /// Of those, the lights that reach the cell being read.
    cell: Vec<(Stream, u32)>,
    /// [`Self::cell`] bucketed by the ring box pixel each light covers, each
    /// bucket in `cell`'s order: pixel `k`'s lights are
    /// `by_px[px_at[k]..px_at[k + 1]]`. A ribbon's quads are 1 px tall and a
    /// few wide, so a pixel meets a handful of the cell's hundreds of lights.
    by_px: Vec<(Stream, u32)>,
    px_at: Vec<u32>,
    /// [`Self::index_pixels`]'s fill cursor per pixel.
    px_fill: Vec<u32>,
    /// The ring box's ground luminance per pixel, and per pixel row the
    /// least and greatest of it within [`RING`] columns.
    lum: Vec<f32>,
    ring_lo: Vec<f32>,
    ring_hi: Vec<f32>,
    /// An additive cell's whole light per ring-box pixel: under the ink
    /// (ground only) and over it (ground and ink), per channel.
    lift_under: Vec<[u16; 3]>,
    lift_over: Vec<[u16; 3]>,
    /// The row being read's cuts, `(column, share)`, for its ramp.
    cuts: Vec<(i32, f32)>,
    /// A test's negative control: the guard off.
    #[cfg(any(test, debug_assertions))]
    pub(crate) off: bool,
    /// A test's negative control: each cell cut alone, no ramp along the row.
    #[cfg(any(test, debug_assertions))]
    pub(crate) comb: bool,
}

/// Ease a row's cuts (`(column, share)`, columns ascending) into one ramp
/// ([`RAMP_SLOPE`]): from the first cut column to the last, each column but
/// `skip` (the caret's) keeps the least of every cut's share plus the slope
/// per column between them, where that is under its own (`1.0` uncut). Calls
/// `cut(column, share)` for each column whose share falls, in column order.
fn ramp_row(cuts: &[(i32, f32)], skip: Option<i32>, mut cut: impl FnMut(i32, f32)) {
    let (Some(&(lo, _)), Some(&(hi, _))) = (cuts.first(), cuts.last()) else {
        return;
    };
    let mut k = 0usize;
    for col in lo..=hi {
        while k < cuts.len() && cuts[k].0 < col {
            k += 1;
        }
        let own = cuts.get(k).filter(|c| c.0 == col).map_or(1.0, |c| c.1);
        if skip == Some(col) {
            continue;
        }
        let eased = cuts
            .iter()
            .map(|&(c, s)| s + RAMP_SLOPE * (c - col).abs() as f32)
            .fold(own, f32::min);
        if eased < own {
            cut(col, eased);
        }
    }
}

/// One cell being judged: its ring box (`[x0, x1) × [y0, y1)`, window px),
/// its solid ink and its ground.
#[derive(Clone, Copy)]
struct Judge {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    ink: u32,
    ground: u32,
    l_ink: f32,
    l_ground: f32,
}

#[inline]
const fn scale_u8(v: u32, s: f32) -> u32 {
    let v = (v as f32 * s) as u32;
    if v > 255 { 255 } else { v }
}

/// A quad's light at window column `x`, scaled to `s`: its premultiplied
/// colour and opacity, as the renderer's gradient law reads them.
#[inline]
fn quad_at(q: &GlowQuad, x: i32, s: f32) -> (u32, u8) {
    let (w, i) = (u32::from(q.w).max(1), (x - i32::from(q.x)).max(0) as u32);
    let c = aterm_render::glow_lerp_rgb(q.color & 0x00FF_FFFF, q.color2 & 0x00FF_FFFF, i, w);
    let a = aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, w);
    if s >= 1.0 {
        return (c, a.min(255) as u8);
    }
    (scale_rgb(c, s), scale_u8(a, s) as u8)
}

/// A halo's weight at window pixel `(x, y)`, as the renderer draws it.
#[inline]
fn halo_wt(h: &RainHalo, x: i32, y: i32) -> u8 {
    let ry2 = (i32::from(h.ry) * i32::from(h.ry)).max(1);
    let rx2 = (i32::from(h.rx) * i32::from(h.rx)).max(1);
    let ny = aterm_render::halo_row_ny(y - i32::from(h.cy), ry2);
    if ny >= 256 {
        return 0;
    }
    let span = ((((256 - ny) * rx2) >> 8) as u32).isqrt() as i32 + 1;
    let dx = x - i32::from(h.cx);
    if dx < -span || dx > span {
        return 0;
    }
    aterm_render::halo_weight(dx, ny, rx2).clamp(0, 255) as u8
}

/// An `Over` halo's opacity ceiling scaled to `s`; `None` once it is gone.
#[inline]
fn over_cap(color: u32, s: f32) -> Option<u8> {
    let cap = u32::from(aterm_render::halo_over_cap(color));
    let cap = if s >= 1.0 { cap } else { scale_u8(cap, s) };
    (cap > 0).then_some(cap as u8)
}

impl LetterGuard {
    /// Composite `px` (window pixel `(x, y)`, ring box pixel `k`) under the
    /// cell's lights at share `s`: the under-text lights when `ground`, then
    /// the over-text quads and the halos.
    #[inline]
    fn composite(
        &self,
        mut px: u32,
        (x, y, k): (i32, i32, usize),
        ground: bool,
        s: f32,
        streams: (&[GlowQuad], &[GlowQuad], &[RainHalo]),
    ) -> u32 {
        let (under, over, halos) = streams;
        let lights = &self.by_px[self.px_at[k] as usize..self.px_at[k + 1] as usize];
        for &(stream, i) in lights {
            let i = i as usize;
            match stream {
                Stream::Under | Stream::Over => {
                    if stream == Stream::Under && !ground {
                        continue;
                    }
                    let q = if stream == Stream::Under {
                        &under[i]
                    } else {
                        &over[i]
                    };
                    let (c, a) = quad_at(q, x, s);
                    px = aterm_render::over_premul(px, c, a);
                }
                Stream::HaloAdd => {
                    let h = &halos[i];
                    let wt = halo_wt(h, x, y);
                    if wt > 0 {
                        let c = if s >= 1.0 {
                            h.color & 0x00FF_FFFF
                        } else {
                            scale_rgb(h.color & 0x00FF_FFFF, s)
                        };
                        px = aterm_render::add_sat(px, aterm_render::premul_rgb(c, wt));
                    }
                }
                Stream::HaloOver => {
                    let h = &halos[i];
                    let wt = halo_wt(h, x, y);
                    if let Some(cap) = over_cap(h.color, s)
                        && wt > 0
                    {
                        px = aterm_render::over_rgb(px, h.color & 0x00FF_FFFF, wt.min(cap));
                    }
                }
            }
        }
        px
    }

    /// Fill [`Self::lift_under`]/[`Self::lift_over`] with the cell's whole
    /// light, when every light that reaches it adds (no source-over light):
    /// then a share's composite is the ground or ink plus that share of it,
    /// saturating, as `add_sat` composes. Returns whether it is additive.
    fn prepare(
        &mut self,
        j: Judge,
        (under, over, halos): (&[GlowQuad], &[GlowQuad], &[RainHalo]),
    ) -> bool {
        let additive = self.cell.iter().all(|&(stream, i)| match stream {
            Stream::Under => under[i as usize].alpha | under[i as usize].alpha2 == 0,
            Stream::Over => over[i as usize].alpha | over[i as usize].alpha2 == 0,
            Stream::HaloAdd => true,
            Stream::HaloOver => false,
        });
        if !additive {
            return false;
        }
        let (w, h) = ((j.x1 - j.x0) as usize, (j.y1 - j.y0) as usize);
        let mut lu = std::mem::take(&mut self.lift_under);
        let mut lo = std::mem::take(&mut self.lift_over);
        lu.clear();
        lo.clear();
        lu.resize(w * h, [0; 3]);
        lo.resize(w * h, [0; 3]);
        let add = |acc: &mut [u16; 3], c: u32| {
            for (k, sh) in [16u32, 8, 0].into_iter().enumerate() {
                acc[k] = acc[k].saturating_add(((c >> sh) & 0xff) as u16);
            }
        };
        for &(stream, i) in &self.cell {
            let i = i as usize;
            let (bx0, by0, bw, bh) = match stream {
                Stream::Under => (under[i].x, under[i].y, under[i].w, under[i].h),
                Stream::Over => (over[i].x, over[i].y, over[i].w, over[i].h),
                Stream::HaloAdd | Stream::HaloOver => {
                    (halos[i].x, halos[i].y, halos[i].w, halos[i].h)
                }
            };
            let (x0, x1) = (
                i32::from(bx0).max(j.x0),
                (i32::from(bx0) + i32::from(bw)).min(j.x1),
            );
            let (y0, y1) = (
                i32::from(by0).max(j.y0),
                (i32::from(by0) + i32::from(bh)).min(j.y1),
            );
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            match stream {
                Stream::Under | Stream::Over => {
                    let q = if stream == Stream::Under {
                        &under[i]
                    } else {
                        &over[i]
                    };
                    let dst = if stream == Stream::Under {
                        &mut lu
                    } else {
                        &mut lo
                    };
                    if (q.color ^ q.color2) & 0x00FF_FFFF == 0 {
                        let c = q.color & 0x00FF_FFFF;
                        if c != 0 {
                            let (rx0, rx1) = ((x0 - j.x0) as usize, (x1 - j.x0) as usize);
                            for y in y0..y1 {
                                let row_off = (y - j.y0) as usize * w;
                                for cell in &mut dst[row_off + rx0..row_off + rx1] {
                                    add(cell, c);
                                }
                            }
                        }
                    } else {
                        for x in x0..x1 {
                            let c = quad_at(q, x, 1.0).0;
                            if c != 0 {
                                let col_off = (x - j.x0) as usize;
                                for y in y0..y1 {
                                    add(&mut dst[(y - j.y0) as usize * w + col_off], c);
                                }
                            }
                        }
                    }
                }
                Stream::HaloAdd | Stream::HaloOver => {
                    let h = &halos[i];
                    let color = h.color & 0x00FF_FFFF;
                    if color == 0 {
                        continue;
                    }
                    let ry2 = (i32::from(h.ry) * i32::from(h.ry)).max(1);
                    let rx2 = (i32::from(h.rx) * i32::from(h.rx)).max(1);
                    let cx = i32::from(h.cx);
                    let cy = i32::from(h.cy);
                    for y in y0..y1 {
                        let ny = aterm_render::halo_row_ny(y - cy, ry2);
                        if ny >= 256 {
                            continue;
                        }
                        let span = ((((256 - ny) * rx2) >> 8) as u32).isqrt() as i32 + 1;
                        let hx0 = x0.max(cx - span);
                        let hx1 = x1.min(cx + span + 1);
                        let row_off = (y - j.y0) as usize * w;
                        for x in hx0..hx1 {
                            let wt = aterm_render::halo_weight(x - cx, ny, rx2).clamp(0, 255) as u8;
                            if wt != 0 {
                                add(
                                    &mut lo[row_off + (x - j.x0) as usize],
                                    aterm_render::premul_rgb(color, wt),
                                );
                            }
                        }
                    }
                }
            }
        }
        self.lift_under = lu;
        self.lift_over = lo;
        true
    }

    /// Fill [`Self::by_px`] for `j`'s ring box: a stable counting sort of
    /// [`Self::cell`] by the pixels each light covers, so
    /// [`Self::composite`] visits exactly a pixel's own lights, in the order
    /// it would meet them in `cell`.
    fn index_pixels(
        &mut self,
        j: Judge,
        (under, over, halos): (&[GlowQuad], &[GlowQuad], &[RainHalo]),
    ) {
        let (w, h) = ((j.x1 - j.x0).max(0), (j.y1 - j.y0).max(0));
        let cover = |&(stream, i): &(Stream, u32)| {
            let i = i as usize;
            let (x, y, qw, qh) = match stream {
                Stream::Under => (under[i].x, under[i].y, under[i].w, under[i].h),
                Stream::Over => (over[i].x, over[i].y, over[i].w, over[i].h),
                Stream::HaloAdd | Stream::HaloOver => {
                    (halos[i].x, halos[i].y, halos[i].w, halos[i].h)
                }
            };
            let span = |at: u16, len: u16, o: i32, n: i32| {
                let lo = (i32::from(at) - o).clamp(0, n);
                let hi = (i32::from(at) + i32::from(len) - o).clamp(0, n);
                lo as usize..hi as usize
            };
            (span(x, qw, j.x0, w), span(y, qh, j.y0, h))
        };
        let (w, n) = (w as usize, (w * h) as usize);
        self.px_at.clear();
        self.px_at.resize(n + 1, 0);
        for light in &self.cell {
            let (xs, ys) = cover(light);
            for y in ys {
                for n in &mut self.px_at[y * w + xs.start + 1..y * w + xs.end + 1] {
                    *n += 1;
                }
            }
        }
        for k in 0..n {
            self.px_at[k + 1] += self.px_at[k];
        }
        self.by_px.clear();
        self.by_px
            .resize(self.px_at[n] as usize, (Stream::Under, 0));
        let mut next = std::mem::take(&mut self.px_fill);
        next.clear();
        next.extend_from_slice(&self.px_at[..n]);
        for &light in &self.cell {
            let (xs, ys) = cover(&light);
            for y in ys {
                for at in &mut next[y * w + xs.start..y * w + xs.end] {
                    self.by_px[*at as usize] = light;
                    *at += 1;
                }
            }
        }
        self.px_fill = next;
    }

    /// Whether every solid ink pixel of the cell reads at least `bar`
    /// against the ground within [`RING`] of it, with the cell's lights at
    /// share `s`: the ground's rows composite in order, each ink row is read
    /// once the ring's last ground row is in, and the first pixel under the
    /// bar settles it.
    fn holds(
        &mut self,
        j: Judge,
        s: f32,
        bar: f32,
        streams: (&[GlowQuad], &[GlowQuad], &[RainHalo]),
        additive: bool,
        table: &crate::color_math::RelativeLuminanceTable,
    ) -> bool {
        let (w, h) = ((j.x1 - j.x0) as usize, (j.y1 - j.y0) as usize);
        let r = RING as usize;
        if w <= 2 * r || h <= 2 * r {
            return true;
        }
        let mut lum = std::mem::take(&mut self.lum);
        let mut ring_lo = std::mem::take(&mut self.ring_lo);
        let mut ring_hi = std::mem::take(&mut self.ring_hi);
        lum.clear();
        ring_lo.clear();
        ring_hi.clear();
        lum.resize(w * h, 0.0);
        ring_lo.resize(w * h, 0.0);
        ring_hi.resize(w * h, 0.0);
        let lift = |c: u32, a: [u16; 3], b: [u16; 3]| {
            let ch = |sh: u32, k: usize| {
                let l = (f32::from(a[k]) + f32::from(b[k])) * s;
                (((c >> sh) & 0xff) + l as u32).min(255)
            };
            (ch(16, 0) << 16) | (ch(8, 1) << 8) | ch(0, 2)
        };
        let ratio = |a: f32, b: f32| (a.max(b) + 0.05) / (a.min(b) + 0.05);
        let mut holds = true;
        let mut last_g: Option<(u32, f32)> = None;
        let mut last_ink: Option<(u32, f32)> = None;
        'rows: for gy in 0..h {
            let row = gy * w;
            for x in 0..w {
                let lg = if additive {
                    let (a, b) = (self.lift_under[row + x], self.lift_over[row + x]);
                    if a == [0; 3] && b == [0; 3] {
                        j.l_ground
                    } else {
                        let g = lift(j.ground, a, b);
                        match last_g {
                            Some((prev_g, prev_lum)) if prev_g == g => prev_lum,
                            _ => {
                                let l = crate::color_math::relative_luminance_with(table, g);
                                last_g = Some((g, l));
                                l
                            }
                        }
                    }
                } else {
                    let at = (j.x0 + x as i32, j.y0 + gy as i32, row + x);
                    let g = self.composite(j.ground, at, true, s, streams);
                    match last_g {
                        Some((prev_g, prev_lum)) if prev_g == g => prev_lum,
                        _ => {
                            let l = crate::color_math::relative_luminance_with(table, g);
                            last_g = Some((g, l));
                            l
                        }
                    }
                };
                lum[row + x] = lg;
            }
            for x in r..w - r {
                let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
                for &l in &lum[row + x - r..=row + x + r] {
                    lo = lo.min(l);
                    hi = hi.max(l);
                }
                ring_lo[row + x] = lo;
                ring_hi[row + x] = hi;
            }
            let Some(y) = gy.checked_sub(2 * r) else {
                continue;
            };
            let y = y + r;
            for x in r..w - r {
                let li = if additive {
                    let b = self.lift_over[y * w + x];
                    if b == [0; 3] {
                        j.l_ink
                    } else {
                        let ink = lift(j.ink, [0; 3], b);
                        match last_ink {
                            Some((prev_ink, prev_lum)) if prev_ink == ink => prev_lum,
                            _ => {
                                let l = crate::color_math::relative_luminance_with(table, ink);
                                last_ink = Some((ink, l));
                                l
                            }
                        }
                    }
                } else {
                    let at = (j.x0 + x as i32, j.y0 + y as i32, y * w + x);
                    let ink = self.composite(j.ink, at, false, s, streams);
                    match last_ink {
                        Some((prev_ink, prev_lum)) if prev_ink == ink => prev_lum,
                        _ => {
                            let l = crate::color_math::relative_luminance_with(table, ink);
                            last_ink = Some((ink, l));
                            l
                        }
                    }
                };
                let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
                for yy in y - r..=y + r {
                    lo = lo.min(ring_lo[yy * w + x]);
                    hi = hi.max(ring_hi[yy * w + x]);
                }
                if ratio(li, lo) < bar || ratio(li, hi) < bar {
                    holds = false;
                    break 'rows;
                }
            }
        }
        self.lum = lum;
        self.ring_lo = ring_lo;
        self.ring_hi = ring_hi;
        holds
    }

    /// The largest share of the cell's lights that keeps the letter's `bar`
    /// ([`letter_bar`]; `1.0` when the whole light does).
    fn share(
        &mut self,
        j: Judge,
        bar: f32,
        streams: (&[GlowQuad], &[GlowQuad], &[RainHalo]),
        table: &crate::color_math::RelativeLuminanceTable,
    ) -> f32 {
        let additive = self.prepare(j, streams);
        if !additive {
            self.index_pixels(j, streams);
        }
        if self.holds(j, 1.0, bar, streams, additive, table) {
            return 1.0;
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..STEPS {
            let mid = 0.5 * (lo + hi);
            if self.holds(j, mid, bar, streams, additive, table) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The upper bound of the light the cell's lights can add to a pixel of
    /// the ring box, per channel, over the ink (`over`) and the ground
    /// (`all`); `None` when a source-over light reaches it (no bound).
    fn bound(
        &self,
        j: Judge,
        (under, over, halos): (&[GlowQuad], &[GlowQuad], &[RainHalo]),
    ) -> Option<([u32; 3], [u32; 3])> {
        let (mut on_ink, mut all) = ([0u32; 3], [0u32; 3]);
        let add = |acc: &mut [u32; 3], c: u32| {
            for (k, sh) in [16u32, 8, 0].into_iter().enumerate() {
                acc[k] += (c >> sh) & 0xff;
            }
        };
        for &(stream, i) in &self.cell {
            let i = i as usize;
            let c = match stream {
                Stream::Under | Stream::Over => {
                    let q = if stream == Stream::Under {
                        &under[i]
                    } else {
                        &over[i]
                    };
                    if q.alpha != 0 || q.alpha2 != 0 {
                        return None;
                    }
                    let (a, b) = (q.color, q.color2);
                    let ch = |sh: u32| ((a >> sh) & 0xff).max((b >> sh) & 0xff);
                    (ch(16) << 16) | (ch(8) << 8) | ch(0)
                }
                Stream::HaloAdd => {
                    let h = &halos[i];
                    let (cx, cy) = (i32::from(h.cx), i32::from(h.cy));
                    let x = cx.clamp(j.x0.max(i32::from(h.x)), (j.x1 - 1).max(j.x0));
                    let y = cy.clamp(j.y0.max(i32::from(h.y)), (j.y1 - 1).max(j.y0));
                    aterm_render::premul_rgb(h.color & 0x00FF_FFFF, halo_wt(h, x, y).max(1))
                }
                Stream::HaloOver => return None,
            };
            if stream != Stream::Under {
                add(&mut on_ink, c);
            }
            add(&mut all, c);
        }
        Some((on_ink, all))
    }
}

/// The text reading under which a letter the pet lies beneath is veiled
/// ([`CursorGlow::veil_letters_on_pet`]): a dim theme's.
const PET_VEIL_THEME_BARE: f32 = 7.0;
/// The margin past a letter's bar the veil keeps for the renderer's rounding.
const PET_VEIL_GUARD: f32 = 0.08;
/// How far past the pet's noted box the veil reaches, px (the box is noted
/// once a frame and the body moves within it).
const PET_VEIL_PAD_PX: i32 = 3;
/// The run over which a veil's ends ease in, px.
const PET_VEIL_EASE_PX: i32 = 3;

/// The bar a letter keeps under the lights, from its solid ink's reading
/// on the bare page, `bare`: [`LETTER_FLOOR`], or [`OWN_SHARE`] of `bare`
/// where the bare page itself reads under the floor.
#[inline]
fn letter_bar(bare: f32) -> f32 {
    if bare >= LETTER_FLOOR {
        LETTER_FLOOR
    } else {
        bare * OWN_SHARE
    }
}

/// `c` lifted by `lift` per channel, saturating.
#[inline]
fn lifted(c: u32, lift: [u32; 3]) -> u32 {
    let ch = |sh: u32, l: u32| (((c >> sh) & 0xff) + l).min(255);
    (ch(16, lift[0]) << 16) | (ch(8, lift[1]) << 8) | ch(0, lift[2])
}

/// The solid ink of a letter drawn in `ink` on `ground`: the colour on the
/// way from `ink` to `ground` that reads [`SOLID_SHARE`] of the clean ratio.
fn solid_ink(ink: u32, ground: u32, table: &crate::color_math::RelativeLuminanceTable) -> u32 {
    let lum = |c: u32| crate::color_math::relative_luminance_with(table, c);
    let lg = lum(ground);
    let ratio = |a: u32| {
        let la = lum(a);
        (la.max(lg) + 0.05) / (la.min(lg) + 0.05)
    };
    let want = SOLID_SHARE * ratio(ink);
    let mix = |t: f32| {
        let ch = |sh: u32| {
            let (a, b) = (((ink >> sh) & 0xff) as f32, ((ground >> sh) & 0xff) as f32);
            ((a + (b - a) * t).round() as u32).min(255)
        };
        (ch(16) << 16) | (ch(8) << 8) | ch(0)
    };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (ink & 0x00FF_FFFF, ground & 0x00FF_FFFF);
    for _ in 0..10 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = mix(mid);
        if ratio(mid_rgb) >= want {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
        if lo_rgb == hi_rgb {
            break;
        }
    }
    lo_rgb
}

impl InkLedger {
    /// Cut every quad of `quads` (additive or source-over) to the guard's
    /// dims ([`Self::mask_at`]), easing back to the whole light off them.
    fn guard_mask_quads(&mut self, quads: &mut Vec<GlowQuad>) {
        let (bx0, by0, bx1, by1) = self.dims_bounds();
        let mut pieces = std::mem::take(&mut self.pieces);
        pieces.clear();
        let mut cut = false;
        for &q in quads.iter() {
            let (x0, y0) = (i32::from(q.x), i32::from(q.y));
            let (x1, y1) = (x0 + i32::from(q.w), y0 + i32::from(q.h));
            if q.w == 0
                || q.h == 0
                || x1 <= bx0
                || x0 >= bx1
                || y1 <= by0
                || y0 >= by1
                || !self.knots((x0, y0, x1, y1))
            {
                pieces.push(q);
                continue;
            }
            cut = true;
            let w = f32::from(q.w);
            let at = |c: u32, sh: u32| ((c >> sh) & 0xff) as f32;
            let end = |x: i32, m: f32| {
                let t = (x - x0) as f32 / w;
                let ch = |sh: u32| {
                    let v = at(q.color, sh) + (at(q.color2, sh) - at(q.color, sh)) * t;
                    ((v * m).round() as u32).min(255)
                };
                let a = f32::from(q.alpha) + (f32::from(q.alpha2) - f32::from(q.alpha)) * t;
                (
                    (ch(16) << 16) | (ch(8) << 8) | ch(0),
                    ((a * m).round() as u32).min(255) as u8,
                )
            };
            for yi in 0..self.knots_y.len() - 1 {
                let (ya, yb) = (self.knots_y[yi], self.knots_y[yi + 1]);
                let ym = 0.5 * (ya + yb) as f32;
                let mut xa = self.knots_x[0];
                let mut ca = end(xa, self.mask_at(xa as f32, ym));
                for xi in 0..self.knots_x.len() - 1 {
                    let xb = self.knots_x[xi + 1];
                    let cb = end(xb, self.mask_at(xb as f32, ym));
                    if ca.0 | cb.0 != 0 || ca.1 | cb.1 != 0 {
                        pieces.push(GlowQuad {
                            row: q.row,
                            x: xa as u16,
                            y: ya as u16,
                            w: (xb - xa) as u16,
                            h: (yb - ya) as u16,
                            color: ca.0,
                            alpha: ca.1,
                            color2: cb.0,
                            alpha2: cb.1,
                        });
                    }
                    xa = xb;
                    ca = cb;
                }
            }
        }
        if cut {
            std::mem::swap(quads, &mut pieces);
        }
        pieces.clear();
        self.pieces = pieces;
    }

    /// Cut every halo of `halos` to the guard's dims: an additive halo's
    /// colour, a source-over halo's opacity ceiling.
    fn guard_mask_halos(&mut self, halos: &mut Vec<RainHalo>) {
        let (bx0, by0, bx1, by1) = self.dims_bounds();
        let mut pieces = std::mem::take(&mut self.halo_pieces);
        pieces.clear();
        let mut cut = false;
        for &h in halos.iter() {
            let (x0, y0) = (i32::from(h.x), i32::from(h.y));
            let (x1, y1) = (x0 + i32::from(h.w), y0 + i32::from(h.h));
            if h.w == 0
                || h.h == 0
                || x1 <= bx0
                || x0 >= bx1
                || y1 <= by0
                || y0 >= by1
                || !self.knots((x0, y0, x1, y1))
            {
                pieces.push(h);
                continue;
            }
            cut = true;
            for yi in 0..self.knots_y.len() - 1 {
                let (ya, yb) = (self.knots_y[yi], self.knots_y[yi + 1]);
                for xi in 0..self.knots_x.len() - 1 {
                    let (xa, xb) = (self.knots_x[xi], self.knots_x[xi + 1]);
                    let m = self.mask_at(0.5 * (xa + xb) as f32, 0.5 * (ya + yb) as f32);
                    let color = match h.mode {
                        HaloMode::Add => scale_rgb(h.color & 0x00FF_FFFF, m),
                        HaloMode::Over => match over_cap(h.color, m) {
                            Some(cap) if m < 1.0 => {
                                (h.color & 0x00FF_FFFF) | (u32::from(cap) << 24)
                            }
                            Some(_) => h.color,
                            None => continue,
                        },
                    };
                    if h.mode == HaloMode::Add && color == 0 {
                        continue;
                    }
                    pieces.push(RainHalo {
                        x: xa as u16,
                        y: ya as u16,
                        w: (xb - xa) as u16,
                        h: (yb - ya) as u16,
                        color,
                        ..h
                    });
                }
            }
        }
        if cut {
            std::mem::swap(halos, &mut pieces);
        }
        pieces.clear();
        self.halo_pieces = pieces;
    }
}

impl CursorGlow {
    /// THE LETTER GUARD ([`self`]): cut this frame's lights (`over`, the
    /// frame's `cursor_glow_add`, and the glow's own under-text quads and
    /// halos) so every lettered cell of the frame's text but the caret's
    /// keeps [`LETTER_FLOOR`]. Classic's spectrum wake is the v0.28
    /// restoration and keeps its bytes (owner ruling); its one-colour face is
    /// guarded. Returns a print of the cuts.
    pub(crate) fn guard_letters(
        &mut self,
        over: &mut Vec<GlowQuad>,
        caret: Option<(u16, u16)>,
        geom: Geom,
        style: GlowStyle,
    ) -> u64 {
        #[cfg(any(test, debug_assertions))]
        if self.letter_guard.off {
            return 0;
        }
        // Classic's spectrum face keeps its v0.28 bytes; its one-colour face
        // (a colour the user chose) is guarded like every other light, in
        // intensity only, so the chosen hue stays (looks6 N10).
        if (matches!(style, GlowStyle::Classic) && !self.last_classic_mono)
            || self.frame_text.rows.is_empty()
            || (over.is_empty() && self.under_out.is_empty() && self.halo_out.is_empty())
        {
            return 0;
        }
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        if cw <= 0 || ch <= 0 {
            return 0;
        }
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let table = crate::color_math::relative_luminance_table();
        let mut guard = std::mem::take(&mut self.letter_guard);
        let ledger = &mut self.ink_ledger;
        ledger.grid = (ox as f32, oy as f32, cw as f32, ch as f32);
        ledger.dims.clear();
        let caret = caret.map(|(r, c)| (i32::from(r), i32::from(c)));
        let text = &self.frame_text;
        let streams = (
            self.under_out.as_slice(),
            over.as_slice(),
            self.halo_out.as_slice(),
        );
        let mut print = 0u64;
        let mut solid = None;
        let lum = |c: u32| crate::color_math::relative_luminance_with(table, c);
        let lum_ratio = |la: f32, lb: f32| (la.max(lb) + 0.05) / (la.min(lb) + 0.05);
        for (i, span) in text.rows.iter().enumerate() {
            let Some((first, end)) = *span else {
                continue;
            };
            let row = text.lo + i as i32;
            let (ry0, ry1) = (oy + row * ch - RING, oy + (row + 1) * ch + RING);
            guard.row.clear();
            let (mut min_x, mut max_x) = (i32::MAX, i32::MIN);
            let mut push_row = |stream: Stream, k: usize, x: u16, w: u16| {
                let x0 = i32::from(x);
                let x1 = x0 + i32::from(w);
                min_x = min_x.min(x0);
                max_x = max_x.max(x1);
                guard.row.push(RowLight {
                    stream,
                    idx: k as u32,
                    x0,
                    x1,
                });
            };
            let meets_y = |y: u16, h: u16| i32::from(y) < ry1 && i32::from(y) + i32::from(h) > ry0;
            let lit = |q: &GlowQuad| q.w > 0 && q.h > 0 && (q.color | q.color2) & 0x00FF_FFFF != 0;
            for (k, q) in streams.0.iter().enumerate() {
                if lit(q) && meets_y(q.y, q.h) {
                    push_row(Stream::Under, k, q.x, q.w);
                }
            }
            for (k, q) in streams.1.iter().enumerate() {
                if lit(q) && meets_y(q.y, q.h) {
                    push_row(Stream::Over, k, q.x, q.w);
                }
            }
            for mode in [HaloMode::Add, HaloMode::Over] {
                for (k, h) in streams.2.iter().enumerate() {
                    if h.mode == mode
                        && h.w > 0
                        && h.h > 0
                        && h.color & 0x00FF_FFFF != 0
                        && meets_y(h.y, h.h)
                    {
                        let s = if mode == HaloMode::Add {
                            Stream::HaloAdd
                        } else {
                            Stream::HaloOver
                        };
                        push_row(s, k, h.x, h.w);
                    }
                }
            }
            if guard.row.is_empty() {
                continue;
            }
            let row_from = ledger.dims.len();
            guard.cuts.clear();
            for col in first..end {
                let (cx0, cx1) = (ox + col * cw - RING, ox + (col + 1) * cw + RING);
                if max_x <= cx0 {
                    break;
                }
                if min_x >= cx1 || caret == Some((row, col)) {
                    continue;
                }
                let Some((Some(ink), ground)) = text.pair(row, col) else {
                    continue;
                };
                let (ink, ground) = (ink & 0x00FF_FFFF, ground & 0x00FF_FFFF);
                guard.cell.clear();
                for &light in &guard.row {
                    if light.x0 < cx1 && light.x1 > cx0 {
                        guard.cell.push((light.stream, light.idx));
                    }
                }
                if guard.cell.is_empty() {
                    continue;
                }
                let (ink, l_ink, l_ground, bar) = match solid {
                    Some((pair, cached)) if pair == (ink, ground) => cached,
                    _ => {
                        let s = solid_ink(ink, ground, table);
                        let li = lum(s);
                        let lg = lum(ground);
                        // The bare page's reading: every light at share 0 composes
                        // to the ground and the ink themselves.
                        let b = letter_bar(lum_ratio(li, lg));
                        let cached = (s, li, lg, b);
                        solid = Some(((ink, ground), cached));
                        cached
                    }
                };
                let j = Judge {
                    x0: cx0,
                    y0: ry0,
                    x1: cx1,
                    y1: ry1,
                    ink,
                    ground,
                    l_ink,
                    l_ground,
                };
                if let Some((on_ink, all)) = guard.bound(j, streams) {
                    // Light ink on a dark page reads worst against the
                    // brightest ground the lights can make; dark ink on
                    // paper, lifted the most, against the page itself.
                    let clear = if l_ink >= l_ground {
                        let lg = lum(lifted(ground, all));
                        lg < l_ink && lum_ratio(l_ink, lg) >= bar
                    } else {
                        let li = lum(lifted(ink, on_ink));
                        li < l_ground && lum_ratio(li, l_ground) >= bar
                    };
                    if clear {
                        continue;
                    }
                }
                let share = guard.share(j, bar, streams, table);
                if share < 1.0 {
                    ledger.dims.push(Dim {
                        x0: cx0 as f32,
                        y0: ry0 as f32,
                        x1: cx1 as f32,
                        y1: ry1 as f32,
                        share,
                    });
                    guard.cuts.push((col, share));
                    print = aterm_hash::fnv_mix_u64(
                        print | 1,
                        ((row as u64) << 48) ^ ((col as u64) << 32) ^ u64::from(share.to_bits()),
                    );
                }
            }
            #[cfg(any(test, debug_assertions))]
            let comb = guard.comb;
            #[cfg(not(any(test, debug_assertions)))]
            let comb = false;
            if comb || guard.cuts.len() < 2 {
                continue;
            }
            let skip = caret.filter(|&(r, _)| r == row).map(|(_, c)| c);
            let cuts = std::mem::take(&mut guard.cuts);
            ramp_row(&cuts, skip, |col, share| {
                match cuts.binary_search_by_key(&col, |c| c.0) {
                    Ok(i) => ledger.dims[row_from + i].share = share,
                    Err(_) => ledger.dims.push(Dim {
                        x0: (ox + col * cw - RING) as f32,
                        y0: ry0 as f32,
                        x1: (ox + (col + 1) * cw + RING) as f32,
                        y1: ry1 as f32,
                        share,
                    }),
                }
                print = aterm_hash::fnv_mix_u64(
                    print | 1,
                    ((row as u64) << 48) ^ ((col as u64) << 32) ^ u64::from(share.to_bits()) ^ 1,
                );
            });
            guard.cuts = cuts;
        }
        if !ledger.dims.is_empty() {
            ledger.guard_mask_quads(&mut self.under_out);
            ledger.guard_mask_quads(over);
            ledger.guard_mask_halos(&mut self.halo_out);
        }
        ledger.dims.clear();
        self.letter_guard = guard;
        print
    }

    /// On a dim theme, veil the pet's body with the page under every letter
    /// it lies beneath (looks6 N11). The renderer's knock-out rings each
    /// glyph in a pixel of page, which keeps a bright theme's letters; a dim
    /// theme's text over the coat read at its cell well under its own
    /// reading (`dim-grey`'s `o` of `echo` at 3.94:1 under the body, its
    /// bar [`letter_bar`] 4.6:1). Each lettered cell the body's box
    /// ([`Self::note_pet_body`], padded [`PET_VEIL_PAD_PX`]) reaches, but
    /// the caret's, takes the page at the least opacity that keeps the
    /// letter at its bar over the worst coat the pet may wear (the letter's
    /// own colour), as one run per row of lettered
    /// cells, its ends eased over [`PET_VEIL_EASE_PX`] outside the lettered
    /// cells (inside them, a word's first and last letters' outer pixels
    /// read under the bar, looks6 review): the letters' row
    /// reads as lying in front of the pet, which keeps its outline above
    /// and below the row. A cell whose text reads past
    /// [`PET_VEIL_THEME_BARE`] draws none. It runs after the guard (whose
    /// cuts would thin it) into the under stream, under the glyphs and over
    /// the coat, and is the guard's: the guard's negative control drops it.
    /// The frame's print of what it drew.
    pub(crate) fn veil_letters_on_pet(&mut self, caret: Option<(u16, u16)>, geom: Geom) -> u64 {
        #[cfg(any(test, debug_assertions))]
        if self.letter_guard.off || self.pet_veil_off {
            return 0;
        }
        let Some((bx0, bx1, by0, by1)) = self.pet_body else {
            return 0;
        };
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        if cw <= 0 || ch <= 0 || self.frame_text.rows.is_empty() {
            return 0;
        }
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let pad = PET_VEIL_PAD_PX;
        let (bx0, bx1, by0, by1) = (bx0 - pad, bx1 + pad, by0 - pad, by1 + pad);
        let table = crate::color_math::relative_luminance_table();
        let lum = |c: u32| crate::color_math::relative_luminance_with(table, c);
        let ratio = |a: u32, b: u32| {
            let (la, lb) = (lum(a), lum(b));
            (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
        };
        // The least page opacity over the worst coat that keeps `ink` at
        // its bar on `ground`; `None` where the text needs no veil.
        let mut veil_memo: Option<((u32, u32), Option<u8>)> = None;
        let mut veil = |ink: u32, ground: u32| -> Option<u8> {
            if let Some((prev, ans)) = veil_memo
                && prev == (ink, ground)
            {
                return ans;
            }
            let ans = (|| {
                let ink = solid_ink(ink, ground, table);
                let bare = ratio(ink, ground);
                if bare >= PET_VEIL_THEME_BARE {
                    return None;
                }
                let bar = (letter_bar(bare) + PET_VEIL_GUARD).min(bare);
                // The worst coat is the letter's own colour (a coat may wear any).
                let coat = ink;
                let holds = |a: u8| {
                    let g = aterm_render::over_premul(coat, premul_rgb(ground, a), a) & 0x00FF_FFFF;
                    ratio(ink, g) >= bar
                };
                Some((1..=u8::MAX).find(|&a| holds(a)).unwrap_or(u8::MAX))
            })();
            veil_memo = Some(((ink, ground), ans));
            ans
        };
        let ease = PET_VEIL_EASE_PX;
        let text = &self.frame_text;
        let under = &mut self.under_out;
        let mut print = 0u64;
        // One run: the lettered span `s0..s1` at full opacity, its eases
        // laid outside it (thin to full over the `ease` px before its first
        // lettered cell, full to thin over the `ease` px past its last), so
        // every lettered pixel takes the whole veil. Where the body's box
        // cuts the span, the ease lies in the box's pad, off the coat; beside
        // the caret's cell (`wall`) the span ends full, the caret's cell
        // taking none.
        let mut push = |under: &mut Vec<GlowQuad>,
                        (s0, wall0): (i32, bool),
                        (s1, wall1): (i32, bool),
                        (y0, y1): (i32, i32),
                        page: u32,
                        a: u8| {
            let (l0, mut f0) = if s0 <= bx0 {
                (bx0, bx0 + ease)
            } else if wall0 {
                (s0, s0)
            } else {
                ((s0 - ease).max(bx0), s0)
            };
            let (mut f1, r1) = if s1 >= bx1 {
                (bx1 - ease, bx1)
            } else if wall1 {
                (s1, s1)
            } else {
                (s1, (s1 + ease).min(bx1))
            };
            if f1 < f0 {
                let mid = (f0 + f1) / 2;
                (f0, f1) = (mid.max(l0), mid.min(r1));
            }
            let (full, thin) = ((premul_rgb(page, a), a), (premul_rgb(page, 1), 1));
            let h = y1 - y0;
            crate::effect_util::push_fx_ramp(under, geom, l0, y0, f0 - l0, h, thin, full);
            crate::effect_util::push_fx_ramp(under, geom, f0, y0, f1 - f0, h, full, full);
            crate::effect_util::push_fx_ramp(under, geom, f1, y0, r1 - f1, h, full, thin);
            print = aterm_hash::fnv_mix_u64(
                print | 1,
                ((l0 as u64) << 48) ^ ((y0 as u64) << 32) ^ ((r1 as u64) << 16) ^ u64::from(a),
            );
        };
        for (i, span) in text.rows.iter().enumerate() {
            let Some((first, end)) = *span else {
                continue;
            };
            if ox + end * cw <= bx0 || ox + first * cw >= bx1 {
                continue;
            }
            let row = text.lo + i as i32;
            let (y0, y1) = ((oy + row * ch).max(by0), (oy + (row + 1) * ch).min(by1));
            if y1 <= y0 {
                continue;
            }
            let caret_col = caret
                .filter(|&(r, _)| i32::from(r) == row)
                .map(|(_, c)| i32::from(c));
            // `(first column, page, opacity)` of the open run.
            let mut run: Option<(i32, u32, u8)> = None;
            for col in first..=end {
                let at = (col < end
                    && caret_col != Some(col)
                    && ox + (col + 1) * cw > bx0
                    && ox + col * cw < bx1)
                    .then(|| text.pair(row, col))
                    .flatten()
                    .and_then(|(ink, ground)| {
                        let ground = ground & 0x00FF_FFFF;
                        Some((ground, veil(ink? & 0x00FF_FFFF, ground)?))
                    });
                match (run, at) {
                    (Some((c0, page, a)), Some((g, b))) if g == page => {
                        run = Some((c0, page, a.max(b)));
                    }
                    (open, next) => {
                        if let Some((c0, page, a)) = open {
                            let (s0, s1) = (ox + c0 * cw, ox + col * cw);
                            if s1.min(bx1) > s0.max(bx0) {
                                push(
                                    under,
                                    (s0, caret_col == Some(c0 - 1)),
                                    (s1, caret_col == Some(col)),
                                    (y0, y1),
                                    page,
                                    a,
                                );
                            }
                        }
                        run = next.map(|(g, b)| (col, g, b));
                    }
                }
            }
        }
        print
    }

    /// A test's negative control: the guard on, but no page veil under the
    /// letters the pet lies beneath ([`Self::veil_letters_on_pet`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil(&mut self, on: bool) {
        self.pet_veil_off = !on;
    }

    /// A test's negative control: lights drawn without the letter guard.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn set_letter_guard(&mut self, on: bool) {
        self.letter_guard.off = !on;
    }

    /// A test's negative control: the guard cuts each cell alone, with no
    /// ramp along the row ([`RAMP_SLOPE`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn set_letter_guard_ramp(&mut self, on: bool) {
        self.letter_guard.comb = !on;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramped(cuts: &[(i32, f32)], skip: Option<i32>) -> Vec<(i32, f32)> {
        let mut out = cuts.to_vec();
        ramp_row(cuts, skip, |col, share| {
            match out.iter_mut().find(|c| c.0 == col) {
                Some(c) => c.1 = share,
                None => out.push((col, share)),
            }
        });
        out.sort_by_key(|c| c.0);
        out
    }

    /// N1 (looks6): a word gap between two cut letters keeps no more light
    /// than they do beyond one slope step, and every share only falls.
    #[test]
    fn a_row_s_cuts_ease_as_one_ramp_through_its_word_gaps() {
        // "as the": columns 0-1 cut, 2 a gap, 3-5 cut; 4 left whole by its own cut.
        let cuts = [(0, 0.20), (1, 0.30), (3, 0.25), (5, 0.60)];
        let out = ramped(&cuts, None);
        let at = |col: i32| out.iter().find(|c| c.0 == col).map_or(1.0, |c| c.1);
        assert!(
            at(2) <= at(1).min(at(3)) + RAMP_SLOPE + 1e-6,
            "the gap keeps {} against its letters' {} and {}",
            at(2),
            at(1),
            at(3)
        );
        for w in out.windows(2) {
            assert!(
                (w[1].1 - w[0].1).abs() <= RAMP_SLOPE * (w[1].0 - w[0].0) as f32 + 1e-6,
                "a step of {} between columns {} and {}",
                (w[1].1 - w[0].1).abs(),
                w[0].0,
                w[1].0
            );
        }
        for &(col, s) in &cuts {
            assert!(at(col) <= s, "column {col} rose from {s} to {}", at(col));
        }
        assert!(at(4) < 1.0, "the letter between two cuts eases with them");
        // The caret's cell is the caret's law, and nothing past the run is touched.
        let skipped = ramped(&cuts, Some(2));
        assert!(skipped.iter().all(|c| c.0 != 2));
        assert!(out.iter().all(|c| (0..=5).contains(&c.0)));
    }
}
