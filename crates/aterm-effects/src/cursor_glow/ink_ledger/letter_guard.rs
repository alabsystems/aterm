// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The letter guard: whatever a frame's lights add up to, every letter they
//! lie on keeps the legibility bar.
//!
//! Each style keeps its own lights off the letters against its own account,
//! so the frame's sum can still wash a letter out. The guard is the one law
//! over all of them, run once on the frame's finished streams before they are
//! published. For each lettered cell of the frame's text but the caret's, it
//! composites every light that reaches the cell exactly as the renderer will
//! (the under-text quads on the ground, then the over-text quads and the halos
//! on ground and ink alike), reads each pixel as solid ink ([`SOLID_SHARE`] of
//! the clean ratio) against the worst ground within [`RING`] px, and where the
//! cell falls under its bar finds the largest share of those lights that keeps
//! it. The bar is [`LETTER_FLOOR`], or [`OWN_SHARE`] of the letter's own bare
//! reading where that is under the floor (a clean ratio under about 5.4:1),
//! with the floor's room over it ([`OWN_ROOM`]).
//! Every light is then cut to that share over the cell and its ring, easing
//! back to its whole strength over the ledger's feather. A frame with no cut
//! is byte-identical to the one it would have drawn.
//!
//! A row's cuts are one eased ramp, not a cell-by-cell comb ([`RAMP_SLOPE`]):
//! between its first and last cut cell, every cell (a word gap, a letter the
//! whole light left at the floor) keeps no more than the least of each cut
//! cell's share plus the slope per column between them. Cut letter by letter,
//! Phaser's neon bar kept its whole light in each word gap between dimmed
//! letters and its spectrum underline went dashed (2026-10-23, looks6 N1).
//! A word gap ([`WORD_GAP_COLS`]) between two letters, one of them cut at
//! least, keeps no more than the straight line between their shares (an
//! uncut letter's being whole): climbing the slope from each, a one-column
//! gap between letters cut to a fifth of their light kept a share and a half
//! of theirs, and past the run's last cut a gap kept its whole light beside
//! its cut letter, so the guard left Phaser's dark bar brighter in the word
//! gaps than over their letters (the replay's typing scene, looks7 M4; the
//! gallery's stripe was the bed's own knot, `bed::bridge_word_gaps`). The
//! ramp only ever lowers a share, so every letter keeps its bar.

use super::*;

/// The bar every letter a light lies on keeps (WCAG AA for text, with a
/// hair of room for the judge's rounding).
pub(crate) const LETTER_FLOOR: f32 = 4.6;
/// The share of a letter's clean ratio its solid ink keeps (`aterm-lab
/// trail-contrast`'s `SOLID_SHARE`): the guard judges the dimmest ink the
/// judge counts, not the face's full colour.
const SOLID_SHARE: f32 = 0.85;
/// The share of its own bare reading a letter whose solid ink reads under
/// [`LETTER_FLOOR`] on the bare page keeps under the lights ([`letter_bar`]),
/// so the floor it cannot reach never leaves it unguarded.
const OWN_SHARE: f32 = 0.97;
/// The room the guard keeps over [`OWN_SHARE`] for the judge's rounding, as
/// [`LETTER_FLOOR`] keeps over WCAG AA's 4.5: the guard reads solid ink, the
/// judge each glyph's ring, and at the bare share a dim theme's letter the
/// guard passed read under it by the judge (Solarized Dark, Water typing:
/// `u` at 4.39:1 of its own 4.57, 96%; looks7).
const OWN_ROOM: f32 = LETTER_FLOOR / 4.5;
/// How far from an ink pixel its ground is read, px (the judge's ring),
/// and how far round a lettered cell the guard judges a light: a light
/// kept that far off a lettered row is never cut.
pub(crate) const RING: i32 = 2;
/// Bisection steps for a cell's share.
const STEPS: usize = 7;
/// How much more of its light a cell of a cut row keeps than a cut cell one
/// column away (a share per column): the row's cuts ease as one ramp, and a
/// word gap between two cut letters is no brighter than they are beyond it.
const RAMP_SLOPE: f32 = 0.08;
/// The widest run of columns between two cut cells read as a word gap: it
/// keeps the straight line between their shares ([`ramp_row`]).
const WORD_GAP_COLS: i32 = 2;

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
    /// [`CursorGlow::holds_bloom`]'s last `(ink, ground)` and its answer.
    bloom_memo: Option<((u32, u32), bool)>,
    /// A test's negative control: the guard off.
    #[cfg(any(test, debug_assertions))]
    pub(crate) off: bool,
    /// A test's negative control: each cell cut alone, no ramp along the row.
    #[cfg(any(test, debug_assertions))]
    pub(crate) comb: bool,
    /// A test's negative control: the ramp's slopes alone, a word gap
    /// climbing them from each letter (looks7 M4).
    #[cfg(any(test, debug_assertions))]
    pub(crate) gaps_climb: bool,
    /// A test's negative control: a dim letter's bar at its bare share, no
    /// [`OWN_ROOM`].
    #[cfg(any(test, debug_assertions))]
    pub(crate) no_room: bool,
}

impl LetterGuard {
    pub(crate) const EMPTY: Self = Self {
        row: Vec::new(),
        cell: Vec::new(),
        by_px: Vec::new(),
        px_at: Vec::new(),
        px_fill: Vec::new(),
        lum: Vec::new(),
        ring_lo: Vec::new(),
        ring_hi: Vec::new(),
        lift_under: Vec::new(),
        lift_over: Vec::new(),
        cuts: Vec::new(),
        bloom_memo: None,
        #[cfg(any(test, debug_assertions))]
        off: false,
        #[cfg(any(test, debug_assertions))]
        comb: false,
        #[cfg(any(test, debug_assertions))]
        gaps_climb: false,
        #[cfg(any(test, debug_assertions))]
        no_room: false,
    };
}

impl Default for LetterGuard {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Ease a row's cuts (`(column, share)`, columns ascending) into one ramp
/// ([`RAMP_SLOPE`]): from the first cut column to the last, each column but
/// `skip` (the caret's) keeps the least of every cut's share plus the slope
/// per column between them, where that is under its own (`1.0` uncut). A
/// word gap (up to [`WORD_GAP_COLS`] columns no letter covers, `lettered`
/// telling which do) between two letters at least one of which is cut keeps
/// no more than the straight line between their shares, an uncut letter's
/// whole (unless `climb`), within the run or at its ends. Calls
/// `cut(column, share)` for each column whose share falls, in column order.
fn ramp_row(
    cuts: &[(i32, f32)],
    skip: Option<i32>,
    climb: bool,
    lettered: impl Fn(i32) -> bool,
    mut cut: impl FnMut(i32, f32),
) {
    let (Some(&(lo, _)), Some(&(hi, _))) = (cuts.first(), cuts.last()) else {
        return;
    };
    let share_of = |col: i32| match cuts.binary_search_by_key(&col, |c| c.0) {
        Ok(i) => cuts[i].1,
        Err(_) => 1.0,
    };
    let reach = WORD_GAP_COLS + 1;
    for col in (lo - reach).max(0)..=hi + reach {
        if skip == Some(col) {
            continue;
        }
        let own = share_of(col);
        let mut eased = own;
        if col >= lo && col <= hi {
            for &(c, s) in cuts {
                eased = eased.min(s + RAMP_SLOPE * (c - col).abs() as f32);
            }
        }
        if !climb && !lettered(col) {
            let mut left = None;
            for d in 1..=reach {
                let c = col - d;
                if c < 0 {
                    break;
                }
                if lettered(c) {
                    left = Some(c);
                    break;
                }
            }
            if let Some(a) = left {
                let max_b = (col + reach).min(a + 1 + WORD_GAP_COLS);
                let mut right = None;
                for c in col + 1..=max_b {
                    if lettered(c) {
                        right = Some(c);
                        break;
                    }
                }
                if let Some(b) = right {
                    let (sa, sb) = (share_of(a), share_of(b));
                    if sa.min(sb) < 1.0 {
                        let u = (col - a) as f32 / (b - a) as f32;
                        eased = eased.min(sa + (sb - sa) * u);
                    }
                }
            }
        }
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
const fn over_cap(color: u32, s: f32) -> Option<u8> {
    let cap = aterm_render::halo_over_cap(color) as u32;
    let cap = if s >= 1.0 { cap } else { scale_u8(cap, s) };
    if cap > 0 { Some(cap as u8) } else { None }
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
        has_over: bool,
    ) -> bool {
        let mut ci = 0;
        while ci < self.cell.len() {
            let (stream, i) = self.cell[ci];
            let ok = match stream {
                Stream::Under => under[i as usize].alpha | under[i as usize].alpha2 == 0,
                Stream::Over => over[i as usize].alpha | over[i as usize].alpha2 == 0,
                Stream::HaloAdd => true,
                Stream::HaloOver => false,
            };
            if !ok {
                return false;
            }
            ci += 1;
        }
        let (w, h) = ((j.x1 - j.x0) as usize, (j.y1 - j.y0) as usize);
        let mut lu = std::mem::take(&mut self.lift_under);
        let mut lo = std::mem::take(&mut self.lift_over);
        lu.clear();
        lu.resize(w * h, [0; 3]);
        if has_over {
            lo.clear();
            lo.resize(w * h, [0; 3]);
        }
        let unpack_rgb = |c: u32| {
            [
                ((c >> 16) & 0xff) as u16,
                ((c >> 8) & 0xff) as u16,
                (c & 0xff) as u16,
            ]
        };
        let add_rgb = |acc: &mut [u16; 3], rgb: [u16; 3]| {
            acc[0] = acc[0].saturating_add(rgb[0]);
            acc[1] = acc[1].saturating_add(rgb[1]);
            acc[2] = acc[2].saturating_add(rgb[2]);
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
                            let rgb = unpack_rgb(c);
                            let (rx0, rx1) = ((x0 - j.x0) as usize, (x1 - j.x0) as usize);
                            for y in y0..y1 {
                                let row_off = (y - j.y0) as usize * w;
                                for cell in &mut dst[row_off + rx0..row_off + rx1] {
                                    add_rgb(cell, rgb);
                                }
                            }
                        }
                    } else {
                        for x in x0..x1 {
                            let c = quad_at(q, x, 1.0).0;
                            if c != 0 {
                                let rgb = unpack_rgb(c);
                                let col_off = (x - j.x0) as usize;
                                for y in y0..y1 {
                                    add_rgb(&mut dst[(y - j.y0) as usize * w + col_off], rgb);
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
                                add_rgb(
                                    &mut lo[row_off + (x - j.x0) as usize],
                                    unpack_rgb(aterm_render::premul_rgb(color, wt)),
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
        (additive, has_over): (bool, bool),
    ) -> bool {
        let (w, h) = ((j.x1 - j.x0) as usize, (j.y1 - j.y0) as usize);
        let r = RING as usize;
        if w <= 2 * r || h <= 2 * r {
            return true;
        }
        let mut lum = std::mem::take(&mut self.lum);
        let mut ring_lo = std::mem::take(&mut self.ring_lo);
        let mut ring_hi = std::mem::take(&mut self.ring_hi);
        lum.resize(w * h, 0.0);
        ring_lo.resize(w * h, 0.0);
        ring_hi.resize(w * h, 0.0);
        let lift = |c: u32, a: [u16; 3]| {
            let ch = |sh: u32, k: usize| {
                let l = f32::from(a[k]) * s;
                (((c >> sh) & 0xff) + l as u32).min(255)
            };
            (ch(16, 0) << 16) | (ch(8, 1) << 8) | ch(0, 2)
        };
        let ratio = |a: f32, b: f32| (a.max(b) + 0.05) / (a.min(b) + 0.05);
        let mut holds = true;
        let mut last_g: Option<(u32, f32)> = None;
        let mut last_g_lift: Option<([u16; 3], f32)> = None;
        let mut last_ink: Option<(u32, f32)> = None;
        let mut last_ink_lift: Option<([u16; 3], f32)> = None;
        'rows: for gy in 0..h {
            let row = gy * w;
            for x in 0..w {
                let k = row + x;
                let lg = if additive {
                    let a = self.lift_under[k];
                    let ab = if has_over {
                        let b = self.lift_over[k];
                        [
                            a[0].saturating_add(b[0]),
                            a[1].saturating_add(b[1]),
                            a[2].saturating_add(b[2]),
                        ]
                    } else {
                        a
                    };
                    if ab == [0; 3] {
                        j.l_ground
                    } else if let Some((prev_ab, prev_lum)) = last_g_lift
                        && prev_ab == ab
                    {
                        prev_lum
                    } else {
                        let g = lift(j.ground, ab);
                        let l = if g == j.ground {
                            j.l_ground
                        } else {
                            match last_g {
                                Some((prev_g, prev_lum)) if prev_g == g => prev_lum,
                                _ => {
                                    let l = crate::color_math::relative_luminance(g);
                                    last_g = Some((g, l));
                                    l
                                }
                            }
                        };
                        last_g_lift = Some((ab, l));
                        l
                    }
                } else if self.px_at[k] == self.px_at[k + 1] {
                    j.l_ground
                } else {
                    let at = (j.x0 + x as i32, j.y0 + gy as i32, k);
                    let g = self.composite(j.ground, at, true, s, streams);
                    if g == j.ground {
                        j.l_ground
                    } else {
                        match last_g {
                            Some((prev_g, prev_lum)) if prev_g == g => prev_lum,
                            _ => {
                                let l = crate::color_math::relative_luminance(g);
                                last_g = Some((g, l));
                                l
                            }
                        }
                    }
                };
                lum[k] = lg;
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
            let irow = y * w;
            for x in r..w - r {
                let k = irow + x;
                let li = if !has_over {
                    j.l_ink
                } else if additive {
                    let b = self.lift_over[k];
                    if b == [0; 3] {
                        j.l_ink
                    } else if let Some((prev_b, prev_lum)) = last_ink_lift
                        && prev_b == b
                    {
                        prev_lum
                    } else {
                        let ink = lift(j.ink, b);
                        let l = if ink == j.ink {
                            j.l_ink
                        } else {
                            match last_ink {
                                Some((prev_ink, prev_lum)) if prev_ink == ink => prev_lum,
                                _ => {
                                    let l = crate::color_math::relative_luminance(ink);
                                    last_ink = Some((ink, l));
                                    l
                                }
                            }
                        };
                        last_ink_lift = Some((b, l));
                        l
                    }
                } else if self.px_at[k] == self.px_at[k + 1] {
                    j.l_ink
                } else {
                    let at = (j.x0 + x as i32, j.y0 + y as i32, k);
                    let ink = self.composite(j.ink, at, false, s, streams);
                    if ink == j.ink {
                        j.l_ink
                    } else {
                        match last_ink {
                            Some((prev_ink, prev_lum)) if prev_ink == ink => prev_lum,
                            _ => {
                                let l = crate::color_math::relative_luminance(ink);
                                last_ink = Some((ink, l));
                                l
                            }
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
    ) -> f32 {
        let mut has_over = false;
        let mut ci = 0;
        while ci < self.cell.len() {
            if self.cell[ci].0 != Stream::Under {
                has_over = true;
                break;
            }
            ci += 1;
        }
        let additive = self.prepare(j, streams, has_over);
        if !additive {
            self.index_pixels(j, streams);
        }
        let flags = (additive, has_over);
        if self.holds(j, 1.0, bar, streams, flags) {
            return 1.0;
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..STEPS {
            let mid = 0.5 * (lo + hi);
            if self.holds(j, mid, bar, streams, flags) {
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
            acc[0] += (c >> 16) & 0xff;
            acc[1] += (c >> 8) & 0xff;
            acc[2] += c & 0xff;
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
/// The share of a cell's height over which a veil eases in above and below
/// its lettered row, where the row beside it holds no letter under the
/// body, at most as deep as the coat reaches into the row
/// ([`CursorGlow::veil_letters_on_pet`]): a feather at the cut, not a fade
/// (over a quarter of a cell it darkened the eyes of a face cut at the
/// chin, and a paw grazing the row's top wore a dark block).
const PET_VEIL_RISE_SHARE: f32 = 0.12;

/// The bar a letter keeps under the lights, from its solid ink's reading
/// on the bare page, `bare`: [`LETTER_FLOOR`], or [`OWN_SHARE`] of `bare`
/// where the bare page itself reads under the floor, with [`OWN_ROOM`]
/// over it (unless `bare_share`, a test's control), never past `bare`.
#[inline]
const fn letter_bar(bare: f32, bare_share: bool) -> f32 {
    if bare >= LETTER_FLOOR {
        LETTER_FLOOR
    } else if bare_share {
        bare * OWN_SHARE
    } else {
        (bare * OWN_SHARE * OWN_ROOM).min(bare)
    }
}

/// `c` lifted by `lift` per channel, saturating.
#[inline]
const fn lifted(c: u32, lift: [u32; 3]) -> u32 {
    #[inline]
    const fn ch(c: u32, sh: u32, l: u32) -> u32 {
        let sum = ((c >> sh) & 0xff) + l;
        if sum < 255 { sum } else { 255 }
    }
    (ch(c, 16, lift[0]) << 16) | (ch(c, 8, lift[1]) << 8) | ch(c, 0, lift[2])
}

/// The solid ink of a letter drawn in `ink` on `ground`: the colour on the
/// way from `ink` to `ground` that reads [`SOLID_SHARE`] of the clean ratio.
const fn solid_ink(ink: u32, ground: u32) -> u32 {
    let lg = crate::color_math::relative_luminance(ground);
    #[inline]
    const fn ratio(lg: f32, a: u32) -> f32 {
        let la = crate::color_math::relative_luminance(a);
        (la.max(lg) + 0.05) / (la.min(lg) + 0.05)
    }
    #[inline]
    const fn mix_ch(ink: u32, ground: u32, sh: u32, t: f32) -> u32 {
        let (a, b) = (((ink >> sh) & 0xff) as f32, ((ground >> sh) & 0xff) as f32);
        let u = (a + (b - a) * t).round() as u32;
        if u < 255 { u } else { 255 }
    }
    #[inline]
    const fn mix(ink: u32, ground: u32, t: f32) -> u32 {
        (mix_ch(ink, ground, 16, t) << 16)
            | (mix_ch(ink, ground, 8, t) << 8)
            | mix_ch(ink, ground, 0, t)
    }
    let want = SOLID_SHARE * ratio(lg, ink);
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (ink & 0x00FF_FFFF, ground & 0x00FF_FFFF);
    let mut hi_checked = false;
    let mut step = 0;
    while step < 10 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = mix(ink, ground, mid);
        if mid_rgb == lo_rgb || ((!hi_checked || mid_rgb != hi_rgb) && ratio(lg, mid_rgb) >= want) {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
            hi_checked = true;
        }
        if lo_rgb == hi_rgb {
            break;
        }
        step += 1;
    }
    lo_rgb
}

impl InkLedger {
    /// Cut every quad of `quads` (additive or source-over) to the guard's
    /// dims ([`Self::mask_at_with`]), easing back to the whole light off them.
    /// With `rails` (the under stream), a quad of a rail laid clear of its
    /// row's ring ([`Self::note_rail`]) is cut only where it meets a dimmed
    /// cell's box.
    fn guard_mask_quads(&mut self, quads: &mut Vec<GlowQuad>, rails: bool) {
        let (bx0, by0, bx1, by1) = self.dims_bounds();
        let feather = self.feather();
        #[cfg(any(test, debug_assertions))]
        let round_cuts = self.round_cuts;
        #[cfg(not(any(test, debug_assertions)))]
        let round_cuts = false;
        #[cfg(any(test, debug_assertions))]
        let rails = rails && !self.rails_feathered;
        let spans = if rails {
            let mut spans = std::mem::take(&mut self.rails);
            spans.sort_unstable();
            spans
        } else {
            Vec::new()
        };
        let mut span = 0usize;
        let mut pieces = std::mem::take(&mut self.pieces);
        pieces.clear();
        let mut cut = false;
        for (i, &q) in quads.iter().enumerate() {
            let (x0, y0) = (i32::from(q.x), i32::from(q.y));
            let (x1, y1) = (x0 + i32::from(q.w), y0 + i32::from(q.h));
            while span < spans.len() && spans[span].1 as usize <= i {
                span += 1;
            }
            let rail = span < spans.len() && spans[span].0 as usize <= i;
            if q.w == 0
                || q.h == 0
                || x1 <= bx0
                || x0 >= bx1
                || y1 <= by0
                || y0 >= by1
                || (rail && !self.meets_dim_box((x0, y0, x1, y1), feather.0))
                || !self.knots((x0, y0, x1, y1))
            {
                pieces.push(q);
                continue;
            }
            cut = true;
            let inv_w = 1.0 / f32::from(q.w);
            let at = |c: u32, sh: u32| ((c >> sh) & 0xff) as f32;
            let (r0, g0, b0) = (at(q.color, 16), at(q.color, 8), at(q.color, 0));
            let (dr, dg, db) = (
                at(q.color2, 16) - r0,
                at(q.color2, 8) - g0,
                at(q.color2, 0) - b0,
            );
            let a0 = f32::from(q.alpha);
            let da = f32::from(q.alpha2) - a0;
            // A piece keeps the quad's own gradient where it keeps its whole
            // light; a cut piece keeps no more than the share the guard
            // weighed, truncated as its model truncates (`quad_at`). Rounded
            // up, each cut slab of a pile laid a byte of light the guard never
            // weighed: six of a bed's slabs cut to an eighth lit Solarized
            // Dark's page by six (`#00313c` under `fox`, looks7 round 2).
            let end = |x: i32, m: f32| {
                let t = (x - x0) as f32 * inv_w;
                let fit = |v: f32| {
                    (if m < 1.0 && !round_cuts {
                        v.trunc()
                    } else {
                        v.round()
                    } as u32)
                        .min(255)
                };
                let ch = |c0: f32, dc: f32| fit((c0 + dc * t) * m);
                let a = a0 + da * t;
                (
                    (ch(r0, dr) << 16) | (ch(g0, dg) << 8) | ch(b0, db),
                    fit(a * m) as u8,
                )
            };
            let ny = self.knots_y.len() - 1;
            let nx = self.knots_x.len() - 1;
            let x0_knot = self.knots_x[0];
            for yi in 0..ny {
                let (ya, yb) = (self.knots_y[yi], self.knots_y[yi + 1]);
                let ym = 0.5 * (ya + yb) as f32;
                let ya_u = ya as u16;
                let h_u = (yb - ya) as u16;
                let mut xa = x0_knot;
                let mut ca = end(xa, self.mask_at_with(xa as f32, ym, feather));
                for xi in 0..nx {
                    let xb = self.knots_x[xi + 1];
                    let cb = end(xb, self.mask_at_with(xb as f32, ym, feather));
                    if ca.0 | cb.0 != 0 || ca.1 | cb.1 != 0 {
                        pieces.push(GlowQuad {
                            row: q.row,
                            x: xa as u16,
                            y: ya_u,
                            w: (xb - xa) as u16,
                            h: h_u,
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
        if rails {
            // The quads' indices moved with the cuts: the spans are spent.
            self.rails = spans;
            self.rails.clear();
        }
    }

    /// Whether the window px box `[x0, x1) × [y0, y1)` meets a dimmed
    /// cell's box itself (not only its feather) in its rows, within the
    /// feather's reach `fx` across.
    const fn meets_dim_box(&self, (x0, y0, x1, y1): (i32, i32, i32, i32), fx: f32) -> bool {
        let (x0f, y0f, x1f, y1f) = (x0 as f32, y0 as f32, x1 as f32, y1 as f32);
        let dims = self.dims.as_slice();
        let mut i = 0;
        while i < dims.len() {
            let d = &dims[i];
            if d.share < 1.0 && y0f < d.y1 && y1f > d.y0 && x0f < d.x1 + fx && x1f > d.x0 - fx {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Cut every halo of `halos` to the guard's dims: an additive halo's
    /// colour, a source-over halo's opacity ceiling.
    fn guard_mask_halos(&mut self, halos: &mut Vec<RainHalo>) {
        let (bx0, by0, bx1, by1) = self.dims_bounds();
        let feather = self.feather();
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
            let ny = self.knots_y.len() - 1;
            let nx = self.knots_x.len() - 1;
            for yi in 0..ny {
                let (ya, yb) = (self.knots_y[yi], self.knots_y[yi + 1]);
                let ym = 0.5 * (ya + yb) as f32;
                let ya_u = ya as u16;
                let h_u = (yb - ya) as u16;
                for xi in 0..nx {
                    let (xa, xb) = (self.knots_x[xi], self.knots_x[xi + 1]);
                    let m = self.mask_at_with(0.5 * (xa + xb) as f32, ym, feather);
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
                        y: ya_u,
                        w: (xb - xa) as u16,
                        h: h_u,
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
    /// Whether the page whose text is `ink` on `ground` (the theme's
    /// resolved default foreground and background) holds the GPU's bloom off
    /// its frames ([`aterm_core::render::RenderInput::bloom_gate`]): its
    /// letters' solid ink reads under [`LETTER_FLOOR`] on the bare page, so
    /// each keeps only [`OWN_SHARE`] of its own reading under the lights
    /// ([`letter_bar`]), and the bloom blurs the over-text light as far as
    /// [`aterm_render::bloom::reach_px`] (12 px, most of a cell) past every
    /// cut the guard made on a letter, where no cut follows it. Solarized
    /// Dark's newest letter read 85% of its own reading under Comet's bloom
    /// and 93% under the plain caret's typing glow; without the bloom every
    /// letter the gallery measured kept 97%. A page whose text clears the
    /// floor has the room and keeps its bloom.
    pub(crate) fn holds_bloom(&mut self, ink: u32, ground: u32) -> bool {
        let key = (ink & 0x00FF_FFFF, ground & 0x00FF_FFFF);
        if let Some((at, held)) = self.letter_guard.bloom_memo
            && at == key
        {
            return held;
        }
        let lum = crate::color_math::relative_luminance;
        let (li, lg) = (lum(solid_ink(key.0, key.1)), lum(key.1));
        let held = (li.max(lg) + 0.05) / (li.min(lg) + 0.05) < LETTER_FLOOR;
        self.letter_guard.bloom_memo = Some((key, held));
        held
    }

    /// The letter guard ([`self`]): cut this frame's lights (`over`, the
    /// frame's `cursor_glow_add`, and the glow's own under-text quads and
    /// halos) so every lettered cell of the frame's text but the caret's
    /// keeps its bar ([`letter_bar`]). Classic's spectrum face keeps its v0.28
    /// bytes and is not guarded; its one-colour face is. Returns a print of
    /// the cuts.
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
        let mut guard = std::mem::take(&mut self.letter_guard);
        #[cfg(any(test, debug_assertions))]
        let no_room = guard.no_room;
        #[cfg(not(any(test, debug_assertions)))]
        let no_room = false;
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
        let lit = |q: &GlowQuad| q.w > 0 && q.h > 0 && (q.color | q.color2) & 0x00FF_FFFF != 0;
        let quad_y_bounds = |qs: &[GlowQuad]| {
            let (mut y0, mut y1) = (i32::MAX, i32::MIN);
            for q in qs {
                if lit(q) {
                    let qy = i32::from(q.y);
                    y0 = y0.min(qy);
                    y1 = y1.max(qy + i32::from(q.h));
                }
            }
            (y0, y1)
        };
        let (u_y0, u_y1) = quad_y_bounds(streams.0);
        let (o_y0, o_y1) = quad_y_bounds(streams.1);
        let (mut ha_y0, mut ha_y1) = (i32::MAX, i32::MIN);
        let (mut ho_y0, mut ho_y1) = (i32::MAX, i32::MIN);
        for h in streams.2 {
            if h.w > 0 && h.h > 0 && h.color & 0x00FF_FFFF != 0 {
                let hy0 = i32::from(h.y);
                let hy1 = hy0 + i32::from(h.h);
                if h.mode == HaloMode::Add {
                    ha_y0 = ha_y0.min(hy0);
                    ha_y1 = ha_y1.max(hy1);
                } else {
                    ho_y0 = ho_y0.min(hy0);
                    ho_y1 = ho_y1.max(hy1);
                }
            }
        }
        let all_y0 = u_y0.min(o_y0).min(ha_y0).min(ho_y0);
        let all_y1 = u_y1.max(o_y1).max(ha_y1).max(ho_y1);
        if all_y0 >= all_y1 {
            return 0;
        }
        let mut print = 0u64;
        let mut solid = None;
        let lum = crate::color_math::relative_luminance;
        let lum_ratio = |la: f32, lb: f32| (la.max(lb) + 0.05) / (la.min(lb) + 0.05);
        for (i, span) in text.rows.iter().enumerate() {
            let Some((first, end)) = *span else {
                continue;
            };
            let row = text.lo + i as i32;
            let (ry0, ry1) = (oy + row * ch - RING, oy + (row + 1) * ch + RING);
            if ry1 <= all_y0 || ry0 >= all_y1 {
                continue;
            }
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
            if ry1 > u_y0 && ry0 < u_y1 {
                for (k, q) in streams.0.iter().enumerate() {
                    if lit(q) && meets_y(q.y, q.h) {
                        push_row(Stream::Under, k, q.x, q.w);
                    }
                }
            }
            if ry1 > o_y0 && ry0 < o_y1 {
                for (k, q) in streams.1.iter().enumerate() {
                    if lit(q) && meets_y(q.y, q.h) {
                        push_row(Stream::Over, k, q.x, q.w);
                    }
                }
            }
            if ry1 > ha_y0 && ry0 < ha_y1 {
                for (k, h) in streams.2.iter().enumerate() {
                    if h.mode == HaloMode::Add
                        && h.w > 0
                        && h.h > 0
                        && h.color & 0x00FF_FFFF != 0
                        && meets_y(h.y, h.h)
                    {
                        push_row(Stream::HaloAdd, k, h.x, h.w);
                    }
                }
            }
            if ry1 > ho_y0 && ry0 < ho_y1 {
                for (k, h) in streams.2.iter().enumerate() {
                    if h.mode == HaloMode::Over
                        && h.w > 0
                        && h.h > 0
                        && h.color & 0x00FF_FFFF != 0
                        && meets_y(h.y, h.h)
                    {
                        push_row(Stream::HaloOver, k, h.x, h.w);
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
                        let s = solid_ink(ink, ground);
                        let li = lum(s);
                        let lg = lum(ground);
                        // The bare page's reading: every light at share 0 composes
                        // to the ground and the ink themselves.
                        let b = letter_bar(lum_ratio(li, lg), no_room);
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
                let share = guard.share(j, bar, streams);
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
            if comb || guard.cuts.is_empty() {
                continue;
            }
            let skip = match caret {
                Some((r, c)) if r == row => Some(c),
                _ => None,
            };
            #[cfg(any(test, debug_assertions))]
            let climb = guard.gaps_climb;
            #[cfg(not(any(test, debug_assertions)))]
            let climb = false;
            let cuts = std::mem::take(&mut guard.cuts);
            let lettered = |col: i32| matches!(text.pair(row, col), Some((Some(_), _)));
            ramp_row(&cuts, skip, climb, lettered, |col, share| {
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
            ledger.guard_mask_quads(&mut self.under_out, true);
            ledger.guard_mask_quads(over, false);
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
    ///
    /// It is drawn by the body the pet's tick drew this frame
    /// ([`Self::veil_pet_letters`], after the host's pet tick), into the
    /// published under stream: drawn in the trail's step it read the body
    /// the pet drew a frame before, so a moving pet's veil lay a frame
    /// behind it, page over the letters it had left and its new letters
    /// bare (looks7 M10). Given the body's `sprite`, each run keeps to the
    /// rows its art draws, each to that row's drawn span padded
    /// [`PET_VEIL_PAD_PX`]: the box's transparent margin (the 16 px under a
    /// standing cat's feet, the air over its back) takes no page, which on
    /// a translucent window read as a box of opaque page around the coat and
    /// on any window cut the trail's light under the letters beside it.
    pub(crate) fn veil_letters_on_pet(
        &mut self,
        out: &mut Vec<GlowQuad>,
        caret: Option<(u16, u16)>,
        geom: Geom,
        sprite: Option<&aterm_render::sprites::FreeSprite>,
    ) -> u64 {
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
        // The silhouette's rows, each its span padded; the padded box whole
        // without the body's sprite.
        let mut bands = Vec::new();
        match sprite {
            Some(s) if !self.pet_veil_boxed() => {
                super::super::icon_shade::sprite_rows(
                    s,
                    (bx0, bx1, by0, by1),
                    (ox, oy),
                    &mut bands,
                );
                for b in &mut bands {
                    (b.0, b.1) = ((b.0 - pad).max(bx0), (b.1 + pad).min(bx1));
                }
            }
            _ => bands.push((bx0, bx1, by0, by1)),
        }
        let lum = crate::color_math::relative_luminance;
        // The least page opacity over the worst coat that keeps `ink` at
        // its bar on `ground`; `None` where the text needs no veil.
        let mut veil_memo: Option<((u32, u32), Option<u8>)> = None;
        let mut veil = |ink: u32, ground: u32| -> Option<u8> {
            if let Some((prev, ans)) = veil_memo
                && prev == (ink, ground)
            {
                return ans;
            }
            let solid = solid_ink(ink, ground);
            let la = lum(solid);
            let ratio_ink = |b: u32| {
                let lb = lum(b);
                (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
            };
            let bare = ratio_ink(ground);
            let ans = if bare >= PET_VEIL_THEME_BARE {
                None
            } else {
                let bar = (letter_bar(bare, false) + PET_VEIL_GUARD).min(bare);
                // The worst coat is the letter's own colour (a coat may wear any).
                let coat = solid;
                let mut prev_g = coat;
                let mut found = u8::MAX;
                for a in 1..=u8::MAX {
                    let g = aterm_render::over_premul(coat, premul_rgb(ground, a), a) & 0x00FF_FFFF;
                    if g != prev_g {
                        prev_g = g;
                        if ratio_ink(g) >= bar {
                            found = a;
                            break;
                        }
                    }
                }
                Some(found)
            };
            veil_memo = Some(((ink, ground), ans));
            ans
        };
        let ease = PET_VEIL_EASE_PX;
        #[cfg(any(test, debug_assertions))]
        let risen = !self.pet_veil_sliced;
        #[cfg(not(any(test, debug_assertions)))]
        let risen = true;
        let rise = if risen {
            ((ch as f32 * PET_VEIL_RISE_SHARE).round() as i32).max(1)
        } else {
            0
        };
        #[cfg(any(test, debug_assertions))]
        let deep = self.pet_veil_deep;
        #[cfg(not(any(test, debug_assertions)))]
        let deep = false;
        let text = &self.frame_text;
        let under = out;
        let from = under.len();
        let mut print = 0u64;
        // One run: the lettered span `s0..s1` at full opacity, its eases laid
        // outside it, so every lettered pixel takes the whole veil. Where the
        // body's box cuts the span, the ease lies in the box's pad, off the
        // coat; beside the caret's cell (`wall`) the span ends full, and the
        // caret's cell takes none.
        // Above and below the row (`edges`: where no letter of the row beside
        // it lies under the body) the run eases in over `rise` px, one pixel
        // row at a time, and no deeper than the coat reaches into the row
        // from that edge: cut at the row's edge, the page sliced the coat
        // across (the burst on Solarized Dark, looks7 round 2).
        let mut push = |under: &mut Vec<GlowQuad>,
                        (s0, wall0): (i32, bool),
                        (s1, wall1): (i32, bool),
                        (y0, y1): (i32, i32),
                        edges: (bool, bool),
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
            // Each ramp, clipped to each silhouette row's span the band
            // reaches, its opacity read at the cut.
            let ramp = |under: &mut Vec<GlowQuad>,
                        (x0, x1): (i32, i32),
                        (y0, y1): (i32, i32),
                        (a0, a1): (u8, u8),
                        (k0, k1): (i32, i32)| {
                for &(cx0, cx1, cy0, cy1) in &bands {
                    let (c0, c1) = (x0.max(cx0).max(k0), x1.min(cx1).min(k1));
                    let (t0, t1) = (y0.max(cy0), y1.min(cy1));
                    if c1 <= c0 || t1 <= t0 {
                        continue;
                    }
                    let at = |x: i32| -> u8 {
                        let (a0, a1) = (i32::from(a0), i32::from(a1));
                        (a0 + (a1 - a0) * (x - x0) / (x1 - x0).max(1)).clamp(0, 255) as u8
                    };
                    let (s, e) = (at(c0), at(c1));
                    crate::effect_util::push_fx_ramp(
                        under,
                        geom,
                        c0,
                        t0,
                        c1 - c0,
                        t1 - t0,
                        (premul_rgb(page, s), s),
                        (premul_rgb(page, e), e),
                    );
                }
            };
            let lay = |under: &mut Vec<GlowQuad>, ys: (i32, i32), a: u8, clip: (i32, i32)| {
                ramp(under, (l0, f0), ys, (1, a), clip);
                ramp(under, (f0, f1), ys, (a, a), clip);
                ramp(under, (f1, r1), ys, (a, 1), clip);
            };
            lay(under, (y0, y1), a, (i32::MIN, i32::MAX));
            // A rise keeps to the coat the row's own edge pixel row veils:
            // where the silhouette leaves the row bare, it lays nothing.
            let edge_span = |y: i32| {
                let mut span: Option<(i32, i32)> = None;
                for b in &bands {
                    if b.2 <= y && y < b.3 {
                        let (a0, b0) = (b.0.max(l0), b.1.min(r1));
                        if b0 > a0 {
                            span = Some(match span {
                                Some((sa, sb)) => (sa.min(a0), sb.max(b0)),
                                None => (a0, b0),
                            });
                        }
                    }
                }
                span
            };
            // The coat's depth into the row from an edge pixel row `y`,
            // stepping `dy`, up to `rise`.
            let depth = |y: i32, dy: i32| {
                let mut d = 0;
                while d < rise {
                    let yk = y + dy * d;
                    if yk < y0 || yk >= y1 || edge_span(yk).is_none() {
                        break;
                    }
                    d += 1;
                }
                d
            };
            let feathers = [(edges.0, y0, 1, -1, y0 - 1), (edges.1, y1 - 1, -1, 1, y1)];
            for (open, edge, inward, outward, first) in feathers {
                if !open {
                    continue;
                }
                let Some(clip) = edge_span(edge) else {
                    continue;
                };
                let r = if deep { rise } else { depth(edge, inward) };
                let inv_r1 = 1.0 / (r + 1) as f32;
                for k in 1..=r {
                    let u = (r + 1 - k) as f32 * inv_r1;
                    let ak = (f32::from(a) * u * u * (3.0 - 2.0 * u)).round() as u8;
                    let y = first + outward * (k - 1);
                    if ak == 0 || y < by0 || y >= by1 {
                        continue;
                    }
                    lay(under, (y, y + 1), ak, clip);
                }
            }
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
            let (top, bottom) = (oy + row * ch, oy + (row + 1) * ch);
            let (y0, y1) = (top.max(by0), bottom.min(by1));
            if y1 <= y0 {
                continue;
            }
            let lettered = |j: Option<usize>| {
                let Some(j) = j else {
                    return false;
                };
                let Some(Some((f, e))) = text.rows.get(j).copied() else {
                    return false;
                };
                ox + e * cw > bx0 && ox + f * cw < bx1
            };
            let edges = (
                y0 == top && !lettered(i.checked_sub(1)),
                y1 == bottom && !lettered(i.checked_add(1)),
            );
            let caret_col = match caret {
                Some((r, c)) if i32::from(r) == row => Some(i32::from(c)),
                _ => None,
            };
            // `(first column, page, opacity)` of the open run.
            let mut run: Option<(i32, u32, u8)> = None;
            for col in first..=end {
                let at = if col < end
                    && caret_col != Some(col)
                    && ox + (col + 1) * cw > bx0
                    && ox + col * cw < bx1
                    && let Some((Some(ink), ground)) = text.pair(row, col)
                {
                    let ground = ground & 0x00FF_FFFF;
                    veil(ink & 0x00FF_FFFF, ground).map(|b| (ground, b))
                } else {
                    None
                };
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
                                    edges,
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
        // The veil lies on the pet's coat, beneath every light: laid over
        // them, it cut a dim page's bed (its hue at the page's own
        // luminance) back to the page along the silhouette's rows, a hard
        // edge across the letters it veiled, where the judge's ring read
        // each channel's median of page and bed, brighter than either (the
        // flat rainbow's burst on Solarized Dark: `f` at 3.78:1 of its own
        // 4.06, `#1e2b55` of `#002b36` and `#1e1b55`).
        #[cfg(any(test, debug_assertions))]
        let beneath = !self.pet_veil_over;
        #[cfg(not(any(test, debug_assertions)))]
        let beneath = true;
        if beneath {
            let laid = under.len() - from;
            under.rotate_right(laid);
        }
        print
    }

    /// A test's negative control: the veil laid over the body's whole box,
    /// not its silhouette ([`Self::veil_letters_on_pet`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil_boxed(&mut self, boxed: bool) {
        self.pet_veil_boxed = boxed;
    }

    /// A test's negative control: the veil cut at its lettered row's edges,
    /// with no rise above or below it ([`Self::veil_letters_on_pet`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil_risen(&mut self, on: bool) {
        self.pet_veil_sliced = !on;
    }

    /// A test's negative control: the rise above or below a lettered row
    /// feathers its whole height, however shallow the coat's cut at that
    /// edge ([`Self::veil_letters_on_pet`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil_feather_deep(&mut self, on: bool) {
        self.pet_veil_deep = on;
    }

    #[inline]
    const fn pet_veil_boxed(&self) -> bool {
        #[cfg(any(test, debug_assertions))]
        {
            self.pet_veil_boxed
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            false
        }
    }

    /// The caret and geometry of the frame the step drew, for the pet's veil
    /// the host draws after its pet's tick ([`Self::veil_pet_letters`]).
    #[inline]
    pub(crate) const fn note_veil_frame(&mut self, caret: Option<(u16, u16)>, geom: Geom) {
        self.veil_frame = Some((caret, geom));
    }

    /// The pet's page veil ([`Self::veil_letters_on_pet`]) at the body the
    /// pet's tick drew this frame ([`Self::note_pet_body`]), appended to the
    /// frame's published under stream `out`, kept to the silhouette of the
    /// body's sprite among the frame's `sprites` (grid pixels; the one whose
    /// box is the body's). The host calls it after the pet's tick, its
    /// word pass and the trail's publish; the print of what it drew. What it
    /// drew is kept ([`Self::pet_veil`]) for the frames that project the
    /// trail's streams again before the next pet tick.
    #[inline]
    pub fn veil_pet_letters(
        &mut self,
        out: &mut Vec<GlowQuad>,
        sprites: &[aterm_render::sprites::FreeSprite],
    ) -> u64 {
        let from = out.len();
        let print = match self.veil_frame {
            Some((caret, geom)) => {
                let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
                let mut sprite = None;
                if let Some(body) = self.pet_body {
                    let mut si = 0;
                    while si < sprites.len() {
                        let s = &sprites[si];
                        let (x, y) = (s.x + ox, s.y + oy);
                        if (x, x + i32::from(s.w), y, y + i32::from(s.h)) == body {
                            sprite = Some(s);
                            break;
                        }
                        si += 1;
                    }
                }
                self.veil_letters_on_pet(out, caret, geom, sprite)
            }
            None => 0,
        };
        self.pet_veil.clear();
        self.pet_veil
            .extend_from_slice(out.get(from..).unwrap_or_default());
        print
    }

    /// The veil the last [`Self::veil_pet_letters`] laid, window pixels: a
    /// host that projects the trail's under stream again without a pet tick
    /// (a split pane's cursor-only frame, whose pet sprite stays published)
    /// lays it with the stream, or the letters under the pet lose their
    /// veil on that frame and get it back on the next full one (a blink).
    #[inline]
    #[must_use]
    pub const fn pet_veil(&self) -> &[GlowQuad] {
        self.pet_veil.as_slice()
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
    pub const fn set_letter_guard(&mut self, on: bool) {
        self.letter_guard.off = !on;
    }

    /// A test's negative control: the guard cuts each cell alone, with no
    /// ramp along the row ([`RAMP_SLOPE`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_ramp(&mut self, on: bool) {
        self.letter_guard.comb = !on;
    }

    /// A test's negative control: a word gap climbs the ramp's slope from
    /// each of its letters ([`WORD_GAP_COLS`]), as it did.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_word_gaps(&mut self, on: bool) {
        self.letter_guard.gaps_climb = !on;
    }

    /// A test's negative control: a dim letter's bar at its bare share
    /// ([`OWN_SHARE`]), the guard keeping no room over it ([`OWN_ROOM`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_room(&mut self, on: bool) {
        self.letter_guard.no_room = !on;
    }

    /// A test's negative control: the pet's veil laid over every light,
    /// not beneath them ([`Self::veil_letters_on_pet`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil_beneath(&mut self, on: bool) {
        self.pet_veil_over = !on;
    }

    /// A test's negative control: the guard's cut pieces rounded to the
    /// nearest byte, laying light it never weighed (`guard_mask_quads`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_cuts_rounded(&mut self, on: bool) {
        self.ink_ledger.round_cuts = on;
    }

    /// A test's negative control: the dark bed held to its budget alone,
    /// never lit at the page's luminance ([`crate::cursor_glow::page_lit_ink`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bed_page_lit(&mut self, on: bool) {
        self.bed_page_unlit = !on;
    }

    /// A test's negative control: the guard feathers its cuts into a rail
    /// laid clear of the ring as into any light (`on` false;
    /// [`InkLedger::note_rail`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_rails_clear(&mut self, on: bool) {
        self.ink_ledger.rails_feathered = !on;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramped(cuts: &[(i32, f32)], skip: Option<i32>) -> Vec<(i32, f32)> {
        climbed(cuts, skip, false)
    }

    fn climbed(cuts: &[(i32, f32)], skip: Option<i32>, climb: bool) -> Vec<(i32, f32)> {
        lettered_climbed(cuts, &[], skip, climb)
    }

    /// The ramp over `cuts`, the letters being the cut columns and `whole`
    /// (letters left uncut).
    fn lettered_climbed(
        cuts: &[(i32, f32)],
        whole: &[i32],
        skip: Option<i32>,
        climb: bool,
    ) -> Vec<(i32, f32)> {
        let mut out = cuts.to_vec();
        let lettered = |col: i32| whole.contains(&col) || cuts.iter().any(|c| c.0 == col);
        ramp_row(cuts, skip, climb, lettered, |col, share| {
            match out.iter_mut().find(|c| c.0 == col) {
                Some(c) => c.1 = share,
                None => out.push((col, share)),
            }
        });
        out.sort_by_key(|c| c.0);
        out
    }

    /// A word gap between two cut letters keeps no more light than they do
    /// beyond one slope step, and every share only falls.
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

    /// looks7 WIDEN: a page whose text has no room for the GPU's bloom holds
    /// it, and one whose text clears the floor keeps it. Solarized Dark's
    /// text (4.75:1, its solid ink 4.04:1), a 5.0:1 grey on the dark page
    /// and a 4.95:1 grey on paper hold it; the cold dark page's text and
    /// GitHub Light's ink keep it: the negative control, which a hold on
    /// every page (or on none) fails.
    #[test]
    fn a_dim_page_holds_the_bloom_and_a_bright_one_keeps_it() {
        let mut glow = CursorGlow::default();
        for (ink, ground) in [
            (0x0083_9496, 0x0000_2B36),
            (0x0085_8585, 0x0011_1318),
            (0x0070_7070, 0x00FF_FFFF),
        ] {
            assert!(
                glow.holds_bloom(ink, ground),
                "{ink:06x} on {ground:06x} keeps the bloom"
            );
        }
        for (ink, ground) in [(0x00C8_D3F5, 0x0011_1318), (0x001F_2328, 0x00FF_FFFF)] {
            assert!(
                !glow.holds_bloom(ink, ground),
                "{ink:06x} on {ground:06x} holds the bloom"
            );
        }
        // The memo answers for its own pair only.
        assert!(glow.holds_bloom(0x0083_9496, 0x0000_2B36));
        assert!(!glow.holds_bloom(0x00C8_D3F5, 0x0011_1318));
    }

    /// looks7 M4: a word gap between two letters cut deep keeps no more than
    /// the straight line between their shares: Phaser's dark bar stood a
    /// stripe in each gap, a share and a half of its letters' light. Past the
    /// run's last cut the gap kept its whole light beside a letter cut to
    /// two fifths (the gallery's `the quick`, `e` cut and `q` whole): it
    /// keeps the line to the uncut letter's whole. Climbing the slope from
    /// each letter (the negative control), it peaks, and past the run stays
    /// whole. A long run of blanks still climbs the slope back toward its
    /// whole light.
    #[test]
    fn a_word_gap_keeps_the_line_between_its_letters() {
        // "the quick": 'e' at 2, the gap at 3, 'q' at 4; a long blank run
        // from 6 to 17 between 'u' at 5 and a cut at 18.
        let cuts = [(1, 0.18), (2, 0.15), (4, 0.20), (5, 0.22), (18, 0.2)];
        let line =
            |out: &[(i32, f32)], col: i32| out.iter().find(|c| c.0 == col).map_or(1.0, |c| c.1);
        let out = ramped(&cuts, None);
        let gap = line(&out, 3);
        assert!(
            gap <= 0.5 * (0.15 + 0.20) + 1e-6,
            "the gap keeps {gap} between its letters' 0.15 and 0.20"
        );
        assert!(
            line(&out, 11) > 0.22 + 2.0 * RAMP_SLOPE,
            "the long blank run keeps only {} in its middle",
            line(&out, 11)
        );
        let climbed_gap = line(&climbed(&cuts, None, true), 3);
        assert!(
            climbed_gap > 0.20 + 0.01,
            "control: climbing the slope the gap keeps only {climbed_gap}, no more than its letters"
        );
        // "the quick", `e` cut at 2 and `q`, `u` at 4 and 5 left whole.
        let edge = [(1, 0.18), (2, 0.4)];
        let gap = line(&lettered_climbed(&edge, &[4, 5], None, false), 3);
        assert!(
            gap <= 0.5 * (0.4 + 1.0) + 1e-6,
            "past the run the gap keeps {gap} between its letters' 0.4 and whole"
        );
        let past = line(&lettered_climbed(&edge, &[4, 5], None, true), 3);
        assert!(
            past >= 1.0,
            "control: climbing, the gap past the run keeps {past}, under its whole light"
        );
    }
}
