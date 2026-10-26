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
use aterm_render::{GlowEnd, GlowRamp, Laid, glow_end};

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

/// Which stream a light rides, in the renderer's draw order. The four
/// streams of quads come first, each its index in [`Streams::quads`], the
/// two on the ground before the two over the ink.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Stream {
    /// `glow_under`: on the ground, beneath the ink.
    Under = 0,
    /// A `glow_under` ramp's light within the row being read: a quad of
    /// [`RampRows::under`].
    UnderRow = 1,
    /// `cursor_glow_add`: over ground and ink.
    Over = 2,
    /// A `cursor_glow_add` ramp's light within the row: a quad of
    /// [`RampRows::over`].
    OverRow = 3,
    /// `glow_halo`, [`HaloMode::Add`] (drawn before every `Over` halo).
    HaloAdd = 4,
    /// `glow_halo`, [`HaloMode::Over`].
    HaloOver = 5,
}

impl Stream {
    /// Whether the light lies on the ground alone, beneath the ink.
    #[inline]
    const fn on_ground(self) -> bool {
        (self as u8) <= Self::UnderRow as u8
    }
}

#[derive(Clone, Copy)]
struct RowLight {
    stream: Stream,
    idx: u32,
    x0: i32,
    x1: i32,
    /// A ramp's lights within the row, `idx .. idx + rows` of its
    /// [`RampRows`] stream (`0`: one light, `idx` itself).
    rows: u32,
}

/// The lights a text row's ramps ([`GlowRamps`]) lay within its band, each
/// the quad it stands for (a run one quad, read off the ramp's kept ends):
/// the guard reads a ramp's rows exactly as the legacy stream's quads, from
/// one row light a ramp ([`RowLight::rows`]).
#[derive(Default)]
pub(crate) struct RampRows {
    under: Vec<GlowQuad>,
    over: Vec<GlowQuad>,
}

/// The frame's lights a text row reads: the under-text quads, the over-text
/// quads and the row's ramp lights of each, indexed by [`Stream`], and the
/// halos.
#[derive(Clone, Copy)]
struct Streams<'a> {
    quads: [&'a [GlowQuad]; 4],
    halos: &'a [RainHalo],
}

impl<'a> Streams<'a> {
    /// The quad an `Under`, `Over`, `UnderRow` or `OverRow` light names.
    #[inline]
    const fn quad(self, stream: Stream, i: usize) -> &'a GlowQuad {
        &self.quads[stream as usize][i]
    }
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
    /// The row being read's ramp lights.
    ramp_rows: RampRows,
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
        ramp_rows: RampRows {
            under: Vec::new(),
            over: Vec::new(),
        },
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
    let mut col = (lo - reach).max(0);
    let col_end = hi + reach;
    while col <= col_end {
        if skip == Some(col) {
            col += 1;
            continue;
        }
        let own = share_of(col);
        let mut eased = own;
        if col >= lo && col <= hi {
            let mut ci = 0;
            while ci < cuts.len() {
                let (c, s) = cuts[ci];
                eased = eased.min(s + RAMP_SLOPE * (c - col).abs() as f32);
                ci += 1;
            }
        }
        if !climb && !lettered(col) {
            let mut left = None;
            let mut d = 1;
            while d <= reach {
                let c = col - d;
                if c < 0 {
                    break;
                }
                if lettered(c) {
                    left = Some(c);
                    break;
                }
                d += 1;
            }
            if let Some(a) = left {
                let max_b = (col + reach).min(a + 1 + WORD_GAP_COLS);
                let mut right = None;
                let mut c = col + 1;
                while c <= max_b {
                    if lettered(c) {
                        right = Some(c);
                        break;
                    }
                    c += 1;
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
        col += 1;
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

/// Lay a run of `ramp`'s cut rows from `y` ([`InkLedger::guard_mask_ramps`]):
/// one ramp a slice between `knots`, each row's ends `ends[row][slice]`,
/// its dark rows at either end left off and a slice dark throughout none.
/// `held` names the rows that are one light, `(first row, rows after it)`:
/// each slice's ramp keeps them its run where they are lit.
fn lay_run(
    out: &mut GlowRamps,
    ramp: &GlowRamp,
    y: i32,
    knots: &[i32],
    ends: &[GlowEnd],
    held: Option<(i32, u16)>,
) {
    let n = knots.len() - 1;
    let rows = ends.len() / (2 * n);
    let mut s = 0;
    while s < n {
        let row = |i: usize| (ends[2 * (i * n + s)], ends[2 * (i * n + s) + 1]);
        let mut first_opt = None;
        let mut i = 0;
        while i < rows {
            if row(i) != (0, 0) {
                first_opt = Some(i);
                break;
            }
            i += 1;
        }
        let Some(first) = first_opt else {
            s += 1;
            continue;
        };
        let mut last = first;
        let mut i = rows;
        while i > first {
            i -= 1;
            if row(i) != (0, 0) {
                last = i;
                break;
            }
        }
        let (join, joined) = if let Some((hy, after)) = held
            && let Ok(offset) = usize::try_from(hy - y)
            && let Some(k) = offset.checked_sub(first)
            && k + usize::from(after) <= last - first
            && row(first + k) != (0, 0)
        {
            (k as u16, after)
        } else {
            (0, 0)
        };
        let laid = GlowRamp {
            at: ramp.at,
            row: ramp.row,
            x: knots[s] as u16,
            y: (y + first as i32) as u16,
            w: (knots[s + 1] - knots[s]) as u16,
            h: (last + 1 - first) as u16,
            join,
            joined,
            ink: out.ends.len() as u32,
        };
        out.items.push(laid);
        let mut i = first;
        while i <= last {
            if !laid.continues((i - first) as u16) {
                let (a, b) = row(i);
                out.ends.extend([a, b]);
            }
            i += 1;
        }
        s += 1;
    }
}

/// Whether a quad lays light (a lit row light of the guard's).
#[inline]
const fn lit(q: &GlowQuad) -> bool {
    q.w > 0 && q.h > 0 && (q.color | q.color2) & 0x00FF_FFFF != 0
}

/// A text row's lights of a stream with ramps, in its draw order: each lit
/// quad meeting the band `ry0 .. ry1` a row light (`push(stream, index,
/// x, w, 0)`), and each ramp meeting it one row light of its lights there,
/// laid into `laid` (`push(row stream, first, x, w, lights)`).
fn row_lights(
    (quads, ramps): (&[GlowQuad], &GlowRamps),
    (sq, sr): (Stream, Stream),
    (ry0, ry1): (i32, i32),
    laid: &mut Vec<GlowQuad>,
    push: &mut impl FnMut(Stream, usize, u16, u16, usize),
) {
    let meets_y = |y: u16, h: u16| i32::from(y) < ry1 && i32::from(y) + i32::from(h) > ry0;
    ramps.in_order(quads.len(), |at| match at {
        Laid::Quads(r) => {
            let mut k = r.start;
            while k < r.end {
                let q = &quads[k];
                if lit(q) && meets_y(q.y, q.h) {
                    push(sq, k, q.x, q.w, 0);
                }
                k += 1;
            }
        }
        Laid::Ramp(k) => {
            let r = &ramps.items[k];
            if r.w == 0 || !meets_y(r.y, r.h) {
                return;
            }
            let from = laid.len();
            for q in r.quads_within(&ramps.ends, (ry0, ry1)) {
                if lit(&q) {
                    laid.push(q);
                }
            }
            if laid.len() > from {
                push(sr, from, r.x, r.w, laid.len() - from);
            }
        }
    });
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
    /// the over-text quads and the halos. `streams` comes by reference: a
    /// call a pixel copies no slices.
    #[inline]
    fn composite(
        &self,
        mut px: u32,
        (x, y, k): (i32, i32, usize),
        ground: bool,
        s: f32,
        streams: &Streams<'_>,
    ) -> u32 {
        let halos = streams.halos;
        let lights = &self.by_px[self.px_at[k] as usize..self.px_at[k + 1] as usize];
        let mut li = 0;
        while li < lights.len() {
            let (stream, i) = lights[li];
            li += 1;
            let i = i as usize;
            match stream {
                Stream::Under | Stream::Over | Stream::UnderRow | Stream::OverRow => {
                    if stream.on_ground() && !ground {
                        continue;
                    }
                    let (c, a) = quad_at(streams.quad(stream, i), x, s);
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
    fn prepare(&mut self, j: Judge, streams: Streams<'_>, has_over: bool) -> bool {
        #[inline]
        const fn unpack_rgb(c: u32) -> [u16; 3] {
            [
                ((c >> 16) & 0xff) as u16,
                ((c >> 8) & 0xff) as u16,
                (c & 0xff) as u16,
            ]
        }
        #[inline]
        const fn add_rgb(acc: &mut [u16; 3], rgb: [u16; 3]) {
            acc[0] = acc[0].saturating_add(rgb[0]);
            acc[1] = acc[1].saturating_add(rgb[1]);
            acc[2] = acc[2].saturating_add(rgb[2]);
        }
        let halos = streams.halos;
        let cell_slice = self.cell.as_slice();
        let mut additive = true;
        let mut ci = 0;
        while ci < cell_slice.len() {
            let (stream, i) = cell_slice[ci];
            ci += 1;
            let ok = match stream {
                Stream::Under | Stream::Over | Stream::UnderRow | Stream::OverRow => {
                    let q = streams.quad(stream, i as usize);
                    q.alpha | q.alpha2 == 0
                }
                Stream::HaloAdd => true,
                Stream::HaloOver => false,
            };
            if !ok {
                additive = false;
                break;
            }
        }
        if !additive {
            return false;
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
        let mut ci = 0;
        while ci < cell_slice.len() {
            let (stream, i) = cell_slice[ci];
            ci += 1;
            let i = i as usize;
            let (bx0, by0, bw, bh) = match stream {
                Stream::HaloAdd | Stream::HaloOver => {
                    (halos[i].x, halos[i].y, halos[i].w, halos[i].h)
                }
                _ => {
                    let q = streams.quad(stream, i);
                    (q.x, q.y, q.w, q.h)
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
                Stream::Under | Stream::Over | Stream::UnderRow | Stream::OverRow => {
                    let q = streams.quad(stream, i);
                    let dst = if stream.on_ground() { &mut lu } else { &mut lo };
                    if (q.color ^ q.color2) & 0x00FF_FFFF == 0 {
                        let c = q.color & 0x00FF_FFFF;
                        if c != 0 {
                            let rgb = unpack_rgb(c);
                            let (rx0, rx1) = ((x0 - j.x0) as usize, (x1 - j.x0) as usize);
                            let mut y = y0;
                            while y < y1 {
                                let row_off = (y - j.y0) as usize * w;
                                let row_slice = &mut dst[row_off + rx0..row_off + rx1];
                                let mut ri = 0;
                                while ri < row_slice.len() {
                                    add_rgb(&mut row_slice[ri], rgb);
                                    ri += 1;
                                }
                                y += 1;
                            }
                        }
                    } else {
                        let mut x = x0;
                        while x < x1 {
                            let c = quad_at(q, x, 1.0).0;
                            if c != 0 {
                                let rgb = unpack_rgb(c);
                                let col_off = (x - j.x0) as usize;
                                let mut y = y0;
                                while y < y1 {
                                    add_rgb(&mut dst[(y - j.y0) as usize * w + col_off], rgb);
                                    y += 1;
                                }
                            }
                            x += 1;
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
                    let mut y = y0;
                    while y < y1 {
                        let ny = aterm_render::halo_row_ny(y - cy, ry2);
                        if ny < 256 {
                            let span = ((((256 - ny) * rx2) >> 8) as u32).isqrt() as i32 + 1;
                            let hx0 = x0.max(cx - span);
                            let hx1 = x1.min(cx + span + 1);
                            let row_off = (y - j.y0) as usize * w;
                            let mut x = hx0;
                            while x < hx1 {
                                let wt =
                                    aterm_render::halo_weight(x - cx, ny, rx2).clamp(0, 255) as u8;
                                if wt != 0 {
                                    add_rgb(
                                        &mut lo[row_off + (x - j.x0) as usize],
                                        unpack_rgb(aterm_render::premul_rgb(color, wt)),
                                    );
                                }
                                x += 1;
                            }
                        }
                        y += 1;
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
    fn index_pixels(&mut self, j: Judge, streams: Streams<'_>) {
        #[inline]
        fn span(at: u16, len: u16, o: i32, n: i32) -> (usize, usize) {
            let lo = (i32::from(at) - o).clamp(0, n) as usize;
            let hi = (i32::from(at) + i32::from(len) - o).clamp(0, n) as usize;
            (lo, hi)
        }
        let halos = streams.halos;
        let (w, h) = ((j.x1 - j.x0).max(0), (j.y1 - j.y0).max(0));
        let cover = |(stream, i): (Stream, u32)| {
            let i = i as usize;
            let (x, y, qw, qh) = match stream {
                Stream::HaloAdd | Stream::HaloOver => {
                    (halos[i].x, halos[i].y, halos[i].w, halos[i].h)
                }
                _ => {
                    let q = streams.quad(stream, i);
                    (q.x, q.y, q.w, q.h)
                }
            };
            (span(x, qw, j.x0, w), span(y, qh, j.y0, h))
        };
        let (w, n) = (w as usize, (w * h) as usize);
        self.px_at.clear();
        self.px_at.resize(n + 1, 0);
        let cell_slice = self.cell.as_slice();
        let mut ci = 0;
        while ci < cell_slice.len() {
            let ((x0, x1), (y0, y1)) = cover(cell_slice[ci]);
            ci += 1;
            let mut y = y0;
            while y < y1 {
                let row_slice = &mut self.px_at[y * w + x0 + 1..y * w + x1 + 1];
                let mut ri = 0;
                while ri < row_slice.len() {
                    row_slice[ri] += 1;
                    ri += 1;
                }
                y += 1;
            }
        }
        let mut k = 0;
        while k < n {
            self.px_at[k + 1] += self.px_at[k];
            k += 1;
        }
        self.by_px.clear();
        self.by_px
            .resize(self.px_at[n] as usize, (Stream::Under, 0));
        let mut next = std::mem::take(&mut self.px_fill);
        next.clear();
        next.extend_from_slice(&self.px_at[..n]);
        let mut ci = 0;
        while ci < cell_slice.len() {
            let light = cell_slice[ci];
            ci += 1;
            let ((x0, x1), (y0, y1)) = cover(light);
            let mut y = y0;
            while y < y1 {
                let row_slice = &mut next[y * w + x0..y * w + x1];
                let mut ri = 0;
                while ri < row_slice.len() {
                    let at = row_slice[ri];
                    self.by_px[at as usize] = light;
                    row_slice[ri] = at + 1;
                    ri += 1;
                }
                y += 1;
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
        streams: Streams<'_>,
        (additive, has_over): (bool, bool),
    ) -> bool {
        #[inline]
        const fn lift_rgb(c: u32, a: [u16; 3], s: f32) -> u32 {
            #[inline]
            const fn ch(c: u32, sh: u32, ak: u16, s: f32) -> u32 {
                let l = (ak as f32 * s) as u32;
                let v = ((c >> sh) & 0xff) + l;
                if v < 255 { v } else { 255 }
            }
            (ch(c, 16, a[0], s) << 16) | (ch(c, 8, a[1], s) << 8) | ch(c, 0, a[2], s)
        }
        #[inline]
        const fn ratio(a: f32, b: f32) -> f32 {
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        }
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
        let mut holds = true;
        let mut last_g: Option<(u32, f32)> = None;
        let mut last_g_lift: Option<([u16; 3], f32)> = None;
        let mut last_ink: Option<(u32, f32)> = None;
        let mut last_ink_lift: Option<([u16; 3], f32)> = None;
        let mut gy = 0;
        'rows: while gy < h {
            let row = gy * w;
            let mut x = 0;
            while x < w {
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
                        let g = lift_rgb(j.ground, ab, s);
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
                    let g = self.composite(j.ground, at, true, s, &streams);
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
                x += 1;
            }
            let mut x = r;
            let x_end = w - r;
            while x < x_end {
                let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
                let slice = &lum[row + x - r..=row + x + r];
                let mut si = 0;
                while si < slice.len() {
                    let l = slice[si];
                    lo = lo.min(l);
                    hi = hi.max(l);
                    si += 1;
                }
                ring_lo[row + x] = lo;
                ring_hi[row + x] = hi;
                x += 1;
            }
            let Some(y) = gy.checked_sub(2 * r) else {
                gy += 1;
                continue;
            };
            let y = y + r;
            let irow = y * w;
            let mut x = r;
            while x < x_end {
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
                        let ink = lift_rgb(j.ink, b, s);
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
                    let ink = self.composite(j.ink, at, false, s, &streams);
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
                let mut yy = y - r;
                let yy_end = y + r;
                while yy <= yy_end {
                    let idx = yy * w + x;
                    lo = lo.min(ring_lo[idx]);
                    hi = hi.max(ring_hi[idx]);
                    yy += 1;
                }
                if ratio(li, lo) < bar || ratio(li, hi) < bar {
                    holds = false;
                    break 'rows;
                }
                x += 1;
            }
            gy += 1;
        }
        self.lum = lum;
        self.ring_lo = ring_lo;
        self.ring_hi = ring_hi;
        holds
    }

    /// The largest share of the cell's lights that keeps the letter's `bar`
    /// ([`letter_bar`]; `1.0` when the whole light does).
    fn share(&mut self, j: Judge, bar: f32, streams: Streams<'_>) -> f32 {
        let mut has_over = false;
        let cell_slice = self.cell.as_slice();
        let mut ci = 0;
        while ci < cell_slice.len() {
            if !cell_slice[ci].0.on_ground() {
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
        let mut step = 0;
        while step < STEPS {
            let mid = 0.5 * (lo + hi);
            if self.holds(j, mid, bar, streams, flags) {
                lo = mid;
            } else {
                hi = mid;
            }
            step += 1;
        }
        lo
    }

    /// The upper bound of the light the cell's lights can add to a pixel of
    /// the ring box, per channel, over the ink (`over`) and the ground
    /// (`all`); `None` when a source-over light reaches it (no bound).
    fn bound(&self, j: Judge, streams: Streams<'_>) -> Option<([u32; 3], [u32; 3])> {
        #[inline]
        const fn add(acc: &mut [u32; 3], c: u32) {
            acc[0] += (c >> 16) & 0xff;
            acc[1] += (c >> 8) & 0xff;
            acc[2] += c & 0xff;
        }
        #[inline]
        const fn max_ch(a: u32, b: u32, sh: u32) -> u32 {
            let ca = (a >> sh) & 0xff;
            let cb = (b >> sh) & 0xff;
            if ca > cb { ca } else { cb }
        }
        let halos = streams.halos;
        let (mut on_ink, mut all) = ([0u32; 3], [0u32; 3]);
        let cell_slice = self.cell.as_slice();
        let mut ci = 0;
        while ci < cell_slice.len() {
            let (stream, i) = cell_slice[ci];
            ci += 1;
            let i = i as usize;
            let c = match stream {
                Stream::Under | Stream::Over | Stream::UnderRow | Stream::OverRow => {
                    let q = streams.quad(stream, i);
                    if q.alpha != 0 || q.alpha2 != 0 {
                        return None;
                    }
                    let (a, b) = (q.color, q.color2);
                    (max_ch(a, b, 16) << 16) | (max_ch(a, b, 8) << 8) | max_ch(a, b, 0)
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
            if !stream.on_ground() {
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
    /// dims ([`Self::mask_at_with`]), easing back to the whole light off them,
    /// and the stream's `ramps` with them ([`Self::guard_mask_ramps`]). With
    /// `rails` (the under stream), a light of a rail laid clear of its row's
    /// ring ([`Self::note_rail`]) is cut only where it meets a dimmed cell's
    /// box.
    fn guard_mask_stream(&mut self, quads: &mut Vec<GlowQuad>, ramps: &mut GlowRamps, rails: bool) {
        if ramps.is_empty() {
            self.guard_mask_quads::<false>(quads, &mut [], rails);
            if rails {
                self.rail_ramps.clear();
            }
            return;
        }
        self.guard_mask_quads::<true>(quads, &mut ramps.items, rails);
        self.guard_mask_ramps(ramps, rails);
    }

    /// Cut every quad of `quads` to the guard's dims ([`Self::guard_mask_stream`]),
    /// each of `ramps` (in draw order, where `RAMPS`) drawn before the first
    /// piece of the quad it was drawn before (after the last where it was
    /// drawn after every quad). With `rails`, a quad of a rail laid clear of
    /// its row's ring ([`Self::note_rail`]) is cut only where it meets a
    /// dimmed cell's box.
    fn guard_mask_quads<const RAMPS: bool>(
        &mut self,
        quads: &mut Vec<GlowQuad>,
        ramps: &mut [GlowRamp],
        rails: bool,
    ) {
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
        let mut next = 0;
        let mut i = 0;
        while i < quads.len() {
            let q = quads[i];
            if RAMPS {
                while next < ramps.len() && ramps[next].at as usize <= i {
                    ramps[next].at = pieces.len() as u32;
                    next += 1;
                }
            }
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
                i += 1;
                continue;
            }
            cut = true;
            let inv_w = 1.0 / f32::from(q.w);
            #[inline]
            const fn at_ch(c: u32, sh: u32) -> f32 {
                ((c >> sh) & 0xff) as f32
            }
            let (r0, g0, b0) = (at_ch(q.color, 16), at_ch(q.color, 8), at_ch(q.color, 0));
            let (dr, dg, db) = (
                at_ch(q.color2, 16) - r0,
                at_ch(q.color2, 8) - g0,
                at_ch(q.color2, 0) - b0,
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
                    let u = if m < 1.0 && !round_cuts {
                        v.trunc()
                    } else {
                        v.round()
                    } as u32;
                    if u < 255 { u } else { 255 }
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
            let mut yi = 0;
            while yi < ny {
                let (ya, yb) = (self.knots_y[yi], self.knots_y[yi + 1]);
                let ym = 0.5 * (ya + yb) as f32;
                let ya_u = ya as u16;
                let h_u = (yb - ya) as u16;
                let mut xa = x0_knot;
                let mut ca = end(xa, self.mask_at_with(xa as f32, ym, feather));
                let mut xi = 0;
                while xi < nx {
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
                    xi += 1;
                }
                yi += 1;
            }
            i += 1;
        }
        if RAMPS {
            while next < ramps.len() {
                ramps[next].at = pieces.len() as u32;
                next += 1;
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

    /// Cut every ramp of `ramps` to the guard's dims as
    /// [`Self::guard_mask_quads`] cuts the quads its rows stand for, piece
    /// for piece: a run of rows the same knots slice lays each slice
    /// between them as one ramp, its rows the pieces' ends (a piece the cut
    /// darkens whole a dark row). A row's pieces lie side by side, so the
    /// slices of a ramp may be laid in any order. A ramp's run is the one
    /// quad its rows stand for: its knots are the run's, each of its rows
    /// takes the cut of the knot interval it lies in, and each interval's
    /// pieces are one light apiece (a run of each slice's ramp), as the
    /// quad's are. With `rails`, a ramp of a rail laid clear of its row's
    /// ring ([`Self::note_rail_laid`]) has a row (a run, its one quad) cut
    /// only where it meets a dimmed cell's box, as its quad would be.
    fn guard_mask_ramps(&mut self, ramps: &mut GlowRamps, rails: bool) {
        let (bx0, by0, bx1, by1) = self.dims_bounds();
        let feather = self.feather();
        #[cfg(any(test, debug_assertions))]
        let round_cuts = self.round_cuts;
        #[cfg(not(any(test, debug_assertions)))]
        let round_cuts = false;
        #[cfg(any(test, debug_assertions))]
        let rails = rails && !self.rails_feathered;
        let spans = if rails {
            let mut spans = std::mem::take(&mut self.rail_ramps);
            spans.sort_unstable();
            spans
        } else {
            Vec::new()
        };
        let mut span = 0usize;
        let mut out = std::mem::take(&mut self.ramp_pieces);
        out.clear();
        let mut cut = false;
        let mut k = 0;
        while k < ramps.items.len() {
            let r = ramps.items[k];
            let (x0, y0) = (i32::from(r.x), i32::from(r.y));
            let (x1, y1) = (x0 + i32::from(r.w), y0 + i32::from(r.h));
            while span < spans.len() && spans[span].1 as usize <= k {
                span += 1;
            }
            let rail = span < spans.len() && spans[span].0 as usize <= k;
            #[inline]
            fn keep(out: &mut GlowRamps, ends: &[u32], r: &GlowRamp) {
                out.items.push(GlowRamp {
                    ink: out.ends.len() as u32,
                    ..*r
                });
                out.ends.extend_from_slice(r.kept_ends(ends));
            }
            if r.w == 0 || x1 <= bx0 || x0 >= bx1 || y1 <= by0 || y0 >= by1 {
                // Whole: laid again only once the stream is rebuilt.
                if cut {
                    keep(&mut out, &ramps.ends, &r);
                }
                k += 1;
                continue;
            }
            if !cut {
                let mut bi = 0;
                while bi < k {
                    keep(&mut out, &ramps.ends, &ramps.items[bi]);
                    bi += 1;
                }
            }
            cut = true;
            // The run of rows being gathered: its first row, its slices'
            // knots, and each row's ends per slice, row-major.
            let mut run_y = y0;
            let mut run_ends = std::mem::take(&mut self.run_ends);
            run_ends.clear();
            let mut run_knots = std::mem::take(&mut self.run_knots);
            run_knots.clear();
            run_knots.extend([x0, x1]);
            let inv_w = 1.0 / f32::from(r.w);
            let held = (
                y0 + i32::from(r.join),
                y0 + i32::from(r.join) + i32::from(r.joined) + 1,
            );
            let mut held_knotted = false;
            // The gathered rows' one light, if they hold one (`lay_run`).
            let mut run_held = None;
            let mut y = y0;
            while y < y1 {
                let (a, b) = r.row_ends(&ramps.ends, (y - y0) as u16);
                let in_held = r.joined > 0 && y >= held.0 && y < held.1;
                if in_held && y == held.0 {
                    held_knotted = a | b != 0
                        && held.0 < by1
                        && held.1 > by0
                        && (!rail || self.meets_dim_box((x0, held.0, x1, held.1), feather.0))
                        && self.knots((x0, held.0, x1, held.1));
                }
                let knotted = if in_held {
                    held_knotted
                } else {
                    a | b != 0
                        && y >= by0
                        && y < by1
                        && (!rail || self.meets_dim_box((x0, y, x1, y + 1), feather.0))
                        && self.knots((x0, y, x1, y + 1))
                };
                // The held rows' light from `y`: the whole run uncut, or the
                // knot interval `y` begins.
                let interval = if in_held && knotted {
                    let kp = self.knots_y.partition_point(|&ky| ky <= y);
                    Some((self.knots_y[kp - 1], self.knots_y[kp]))
                } else {
                    None
                };
                let opens = match interval {
                    Some((ya, yb)) => {
                        if y == ya {
                            Some((ya, (yb - ya - 1) as u16))
                        } else {
                            None
                        }
                    }
                    None => {
                        if in_held && y == held.0 {
                            Some((held.0, r.joined))
                        } else {
                            None
                        }
                    }
                };
                let same = if knotted {
                    self.knots_x == run_knots
                } else {
                    a | b == 0 || run_knots == [x0, x1]
                };
                if !same || (interval.is_some() && opens.is_some() && y > run_y) {
                    lay_run(&mut out, &r, run_y, &run_knots, &run_ends, run_held);
                    run_y = y;
                    run_held = None;
                    run_ends.clear();
                    run_knots.clear();
                    if knotted {
                        run_knots.extend_from_slice(&self.knots_x);
                    } else {
                        run_knots.extend([x0, x1]);
                    }
                }
                if opens.is_some() {
                    run_held = opens;
                }
                if !knotted {
                    if run_knots.len() == 2 {
                        run_ends.extend([a, b]);
                    } else {
                        run_ends.resize(run_ends.len() + 2 * (run_knots.len() - 1), 0);
                    }
                    y += 1;
                    continue;
                }
                if interval.is_some() && opens.is_none() {
                    // A held row past its interval's first: the row above's
                    // ends, its pieces cut at the same `ym`.
                    let n = run_ends.len();
                    run_ends.extend_from_within(n - 2 * (run_knots.len() - 1)..n);
                    y += 1;
                    continue;
                }
                #[inline]
                const fn at_ch(c: u32, sh: u32) -> f32 {
                    ((c >> sh) & 0xff) as f32
                }
                // A cut piece truncated as `guard_mask_quads` truncates it.
                let end = |x: i32, m: f32| {
                    let t = (x - x0) as f32 * inv_w;
                    let fit = |v: f32| {
                        let u = if m < 1.0 && !round_cuts {
                            v.trunc()
                        } else {
                            v.round()
                        } as u32;
                        if u < 255 { u } else { 255 }
                    };
                    let ch = |sh: u32| fit((at_ch(a, sh) + (at_ch(b, sh) - at_ch(a, sh)) * t) * m);
                    let al = fit((at_ch(a, 24) + (at_ch(b, 24) - at_ch(a, 24)) * t) * m) as u8;
                    glow_end((ch(16) << 16) | (ch(8) << 8) | ch(0), al)
                };
                let ym = if in_held {
                    let kp = self.knots_y.partition_point(|&ky| ky <= y);
                    0.5 * (self.knots_y[kp - 1] + self.knots_y[kp]) as f32
                } else {
                    0.5 * (2 * y + 1) as f32
                };
                let mut ca = end(
                    run_knots[0],
                    self.mask_at_with(run_knots[0] as f32, ym, feather),
                );
                let mut xi = 1;
                while xi < run_knots.len() {
                    let xb = run_knots[xi];
                    let cb = end(xb, self.mask_at_with(xb as f32, ym, feather));
                    // A piece of no light is never laid: a dark row (two
                    // zero ends already).
                    run_ends.extend([ca, cb]);
                    ca = cb;
                    xi += 1;
                }
                y += 1;
            }
            lay_run(&mut out, &r, run_y, &run_knots, &run_ends, run_held);
            self.run_ends = run_ends;
            self.run_knots = run_knots;
            k += 1;
        }
        if cut {
            out.lit = out.count_lit();
            std::mem::swap(ramps, &mut out);
        }
        out.clear();
        self.ramp_pieces = out;
        if rails {
            // The ramps' indices moved with the cuts: the spans are spent.
            self.rail_ramps = spans;
            self.rail_ramps.clear();
        }
    }

    /// Cut every halo of `halos` to the guard's dims: an additive halo's
    /// colour, a source-over halo's opacity ceiling.
    fn guard_mask_halos(&mut self, halos: &mut Vec<RainHalo>) {
        let (bx0, by0, bx1, by1) = self.dims_bounds();
        let feather = self.feather();
        let mut pieces = std::mem::take(&mut self.halo_pieces);
        pieces.clear();
        let mut cut = false;
        let mut hi = 0;
        while hi < halos.len() {
            let h = halos[hi];
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
                hi += 1;
                continue;
            }
            cut = true;
            let ny = self.knots_y.len() - 1;
            let nx = self.knots_x.len() - 1;
            let mut yi = 0;
            while yi < ny {
                let (ya, yb) = (self.knots_y[yi], self.knots_y[yi + 1]);
                let ym = 0.5 * (ya + yb) as f32;
                let ya_u = ya as u16;
                let h_u = (yb - ya) as u16;
                let mut xi = 0;
                while xi < nx {
                    let (xa, xb) = (self.knots_x[xi], self.knots_x[xi + 1]);
                    let m = self.mask_at_with(0.5 * (xa + xb) as f32, ym, feather);
                    let color = match h.mode {
                        HaloMode::Add => scale_rgb(h.color & 0x00FF_FFFF, m),
                        HaloMode::Over => match over_cap(h.color, m) {
                            Some(cap) if m < 1.0 => {
                                (h.color & 0x00FF_FFFF) | (u32::from(cap) << 24)
                            }
                            Some(_) => h.color,
                            None => {
                                xi += 1;
                                continue;
                            }
                        },
                    };
                    if h.mode == HaloMode::Add && color == 0 {
                        xi += 1;
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
                    xi += 1;
                }
                yi += 1;
            }
            hi += 1;
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
    pub(crate) const fn holds_bloom(&mut self, ink: u32, ground: u32) -> bool {
        let key = (ink & 0x00FF_FFFF, ground & 0x00FF_FFFF);
        if let Some((at, held)) = self.letter_guard.bloom_memo
            && at.0 == key.0
            && at.1 == key.1
        {
            return held;
        }
        let li = crate::color_math::relative_luminance(solid_ink(key.0, key.1));
        let lg = crate::color_math::relative_luminance(key.1);
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
            || (over.is_empty()
                && self.under_out.is_empty()
                && self.halo_out.is_empty()
                && self.under_ramps.is_empty()
                && self.out_ramps.is_empty())
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
        let text = &self.frame_text;
        let (under, halos) = (self.under_out.as_slice(), self.halo_out.as_slice());
        let over_q = over.as_slice();
        // Each stream's lit extent down the page, its ramps' boxes counted:
        // a text row none of its lights reach skips that stream.
        #[inline]
        const fn y_bounds(qs: &[GlowQuad], ramps: &GlowRamps) -> (i32, i32) {
            let (mut y0, mut y1) = (i32::MAX, i32::MIN);
            let mut i = 0;
            while i < qs.len() {
                let q = &qs[i];
                if lit(q) {
                    let qy = q.y as i32;
                    let qy1 = qy + q.h as i32;
                    if qy < y0 {
                        y0 = qy;
                    }
                    if qy1 > y1 {
                        y1 = qy1;
                    }
                }
                i += 1;
            }
            let items = ramps.items.as_slice();
            let mut j = 0;
            while j < items.len() {
                let r = &items[j];
                if r.w > 0 && r.h > 0 {
                    let ry = r.y as i32;
                    let ry1 = ry + r.h as i32;
                    if ry < y0 {
                        y0 = ry;
                    }
                    if ry1 > y1 {
                        y1 = ry1;
                    }
                }
                j += 1;
            }
            (y0, y1)
        }
        let (u_y0, u_y1) = y_bounds(under, &self.under_ramps);
        let (o_y0, o_y1) = y_bounds(over_q, &self.out_ramps);
        let (mut ha_y0, mut ha_y1) = (i32::MAX, i32::MIN);
        let (mut ho_y0, mut ho_y1) = (i32::MAX, i32::MIN);
        let mut hi = 0;
        while hi < halos.len() {
            let h = &halos[hi];
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
            hi += 1;
        }
        let all_y0 = u_y0.min(o_y0).min(ha_y0).min(ho_y0);
        let all_y1 = u_y1.max(o_y1).max(ha_y1).max(ho_y1);
        if all_y0 >= all_y1 {
            self.letter_guard = guard;
            return 0;
        }
        let mut ramp_rows = std::mem::take(&mut guard.ramp_rows);
        let mut print = 0u64;
        let mut solid = None;
        let lum = crate::color_math::relative_luminance;
        #[inline]
        const fn lum_ratio(la: f32, lb: f32) -> f32 {
            (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
        }
        #[inline]
        const fn meets_y(y: u16, h: u16, ry0: i32, ry1: i32) -> bool {
            let y0 = y as i32;
            y0 < ry1 && y0 + h as i32 > ry0
        }
        let mut i = 0;
        while i < text.rows.len() {
            let Some((first, end)) = text.rows[i] else {
                i += 1;
                continue;
            };
            let row = text.lo + i as i32;
            let (ry0, ry1) = (oy + row * ch - RING, oy + (row + 1) * ch + RING);
            if ry1 <= all_y0 || ry0 >= all_y1 {
                i += 1;
                continue;
            }
            guard.row.clear();
            ramp_rows.under.clear();
            ramp_rows.over.clear();
            let (mut min_x, mut max_x) = (i32::MAX, i32::MIN);
            let mut push_row = |stream: Stream, k: usize, x: u16, w: u16, rows: usize| {
                let x0 = i32::from(x);
                let x1 = x0 + i32::from(w);
                min_x = min_x.min(x0);
                max_x = max_x.max(x1);
                guard.row.push(RowLight {
                    stream,
                    idx: k as u32,
                    x0,
                    x1,
                    rows: rows as u32,
                });
            };
            // Each stream's quads, then where it has ramps its quads and
            // ramps in draw order ([`row_lights`]); a stream of quads alone
            // reads as the guard read it before ramps.
            if ry1 > u_y0 && ry0 < u_y1 {
                if self.under_ramps.is_empty() {
                    let mut k = 0;
                    while k < under.len() {
                        let q = &under[k];
                        if lit(q) && meets_y(q.y, q.h, ry0, ry1) {
                            push_row(Stream::Under, k, q.x, q.w, 0);
                        }
                        k += 1;
                    }
                } else {
                    row_lights(
                        (under, &self.under_ramps),
                        (Stream::Under, Stream::UnderRow),
                        (ry0, ry1),
                        &mut ramp_rows.under,
                        &mut push_row,
                    );
                }
            }
            if ry1 > o_y0 && ry0 < o_y1 {
                if self.out_ramps.is_empty() {
                    let mut k = 0;
                    while k < over_q.len() {
                        let q = &over_q[k];
                        if lit(q) && meets_y(q.y, q.h, ry0, ry1) {
                            push_row(Stream::Over, k, q.x, q.w, 0);
                        }
                        k += 1;
                    }
                } else {
                    row_lights(
                        (over_q, &self.out_ramps),
                        (Stream::Over, Stream::OverRow),
                        (ry0, ry1),
                        &mut ramp_rows.over,
                        &mut push_row,
                    );
                }
            }
            if ry1 > ha_y0 && ry0 < ha_y1 {
                let mut k = 0;
                while k < halos.len() {
                    let h = &halos[k];
                    if h.mode == HaloMode::Add
                        && h.w > 0
                        && h.h > 0
                        && h.color & 0x00FF_FFFF != 0
                        && meets_y(h.y, h.h, ry0, ry1)
                    {
                        push_row(Stream::HaloAdd, k, h.x, h.w, 0);
                    }
                    k += 1;
                }
            }
            if ry1 > ho_y0 && ry0 < ho_y1 {
                let mut k = 0;
                while k < halos.len() {
                    let h = &halos[k];
                    if h.mode == HaloMode::Over
                        && h.w > 0
                        && h.h > 0
                        && h.color & 0x00FF_FFFF != 0
                        && meets_y(h.y, h.h, ry0, ry1)
                    {
                        push_row(Stream::HaloOver, k, h.x, h.w, 0);
                    }
                    k += 1;
                }
            }
            if guard.row.is_empty() {
                i += 1;
                continue;
            }
            let skip = match caret {
                Some((r, c)) if i32::from(r) == row => Some(i32::from(c)),
                _ => None,
            };
            let row_from = ledger.dims.len();
            guard.cuts.clear();
            let mut col = first;
            while col < end {
                let (cx0, cx1) = (ox + col * cw - RING, ox + (col + 1) * cw + RING);
                if max_x <= cx0 {
                    break;
                }
                if min_x >= cx1 || skip == Some(col) {
                    col += 1;
                    continue;
                }
                let Some((Some(ink), ground)) = text.pair(row, col) else {
                    col += 1;
                    continue;
                };
                let (ink, ground) = (ink & 0x00FF_FFFF, ground & 0x00FF_FFFF);
                guard.cell.clear();
                let mut li = 0;
                while li < guard.row.len() {
                    let light = guard.row[li];
                    if light.x0 < cx1 && light.x1 > cx0 {
                        if light.rows == 0 {
                            guard.cell.push((light.stream, light.idx));
                        } else {
                            let mut k = light.idx;
                            let k_end = light.idx + light.rows;
                            while k < k_end {
                                guard.cell.push((light.stream, k));
                                k += 1;
                            }
                        }
                    }
                    li += 1;
                }
                if guard.cell.is_empty() {
                    col += 1;
                    continue;
                }
                let (ink, l_ink, l_ground, bar) = match solid {
                    Some((pair, cached)) if pair == (ink, ground) => cached,
                    _ => {
                        let s = solid_ink(ink, ground);
                        let l_i = lum(s);
                        let l_g = lum(ground);
                        // The bare page's reading: every light at share 0 composes
                        // to the ground and the ink themselves.
                        let b = letter_bar(lum_ratio(l_i, l_g), no_room);
                        let cached = (s, l_i, l_g, b);
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
                let streams = Streams {
                    quads: [under, &ramp_rows.under, over_q, &ramp_rows.over],
                    halos,
                };
                if let Some((on_ink, all)) = guard.bound(j, streams) {
                    // Light ink on a dark page reads worst against the
                    // brightest ground the lights can make; dark ink on
                    // paper, lifted the most, against the page itself.
                    let clear = if l_ink >= l_ground {
                        let lg = lum(lifted(ground, all));
                        lg < l_ink && lum_ratio(l_ink, lg) >= bar
                    } else {
                        let l_i = lum(lifted(ink, on_ink));
                        l_i < l_ground && lum_ratio(l_i, l_ground) >= bar
                    };
                    if clear {
                        col += 1;
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
                col += 1;
            }
            #[cfg(any(test, debug_assertions))]
            let comb = guard.comb;
            #[cfg(not(any(test, debug_assertions)))]
            let comb = false;
            if comb || guard.cuts.is_empty() {
                i += 1;
                continue;
            }
            #[cfg(any(test, debug_assertions))]
            let climb = guard.gaps_climb;
            #[cfg(not(any(test, debug_assertions)))]
            let climb = false;
            let cuts = std::mem::take(&mut guard.cuts);
            let lettered = |col: i32| matches!(text.pair(row, col), Some((Some(_), _)));
            ramp_row(&cuts, skip, climb, lettered, |col, share| {
                match cuts.binary_search_by_key(&col, |c| c.0) {
                    Ok(idx) => ledger.dims[row_from + idx].share = share,
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
            i += 1;
        }
        guard.ramp_rows = ramp_rows;
        if !ledger.dims.is_empty() {
            ledger.guard_mask_stream(&mut self.under_out, &mut self.under_ramps, true);
            ledger.guard_mask_stream(over, &mut self.out_ramps, false);
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
        (out, ramps): (&mut Vec<GlowQuad>, &mut GlowRamps),
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
        let mut bands = std::mem::take(&mut self.icon_scratch);
        bands.clear();
        match sprite {
            Some(s) if !self.pet_veil_boxed() => {
                super::super::icon_shade::sprite_rows(
                    s,
                    (bx0, bx1, by0, by1),
                    (ox, oy),
                    &mut bands,
                );
                let mut bi = 0;
                while bi < bands.len() {
                    let b = &mut bands[bi];
                    (b.0, b.1) = ((b.0 - pad).max(bx0), (b.1 + pad).min(bx1));
                    bi += 1;
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
                let mut a = 1u8;
                loop {
                    let g = aterm_render::over_premul(coat, premul_rgb(ground, a), a) & 0x00FF_FFFF;
                    if g != prev_g {
                        prev_g = g;
                        if ratio_ink(g) >= bar {
                            found = a;
                            break;
                        }
                    }
                    if a == u8::MAX {
                        break;
                    }
                    a += 1;
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
                let mut bi = 0;
                while bi < bands.len() {
                    let (cx0, cx1, cy0, cy1) = bands[bi];
                    let (c0, c1) = (x0.max(cx0).max(k0), x1.min(cx1).min(k1));
                    let (t0, t1) = (y0.max(cy0), y1.min(cy1));
                    if c1 <= c0 || t1 <= t0 {
                        bi += 1;
                        continue;
                    }
                    let dx = (x1 - x0).max(1);
                    let (ia0, da) = (i32::from(a0), i32::from(a1) - i32::from(a0));
                    let s = (ia0 + da * (c0 - x0) / dx).clamp(0, 255) as u8;
                    let e = (ia0 + da * (c1 - x0) / dx).clamp(0, 255) as u8;
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
                    bi += 1;
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
                let mut bi = 0;
                while bi < bands.len() {
                    let b = bands[bi];
                    if b.2 <= y && y < b.3 {
                        let (a0, b0) = (b.0.max(l0), b.1.min(r1));
                        if b0 > a0 {
                            span = Some(match span {
                                Some((sa, sb)) => (sa.min(a0), sb.max(b0)),
                                None => (a0, b0),
                            });
                        }
                    }
                    bi += 1;
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
            let mut fi = 0;
            while fi < feathers.len() {
                let (open, edge, inward, outward, first) = feathers[fi];
                fi += 1;
                if !open {
                    continue;
                }
                let Some(clip) = edge_span(edge) else {
                    continue;
                };
                let r = if deep { rise } else { depth(edge, inward) };
                let inv_r1 = 1.0 / (r + 1) as f32;
                let mut k = 1;
                while k <= r {
                    let u = (r + 1 - k) as f32 * inv_r1;
                    let ak = (f32::from(a) * u * u * (3.0 - 2.0 * u)).round() as u8;
                    let y = first + outward * (k - 1);
                    if ak != 0 && y >= by0 && y < by1 {
                        lay(under, (y, y + 1), ak, clip);
                    }
                    k += 1;
                }
            }
            print = aterm_hash::fnv_mix_u64(
                print | 1,
                ((l0 as u64) << 48) ^ ((y0 as u64) << 32) ^ ((r1 as u64) << 16) ^ u64::from(a),
            );
        };
        let mut i = 0;
        while i < text.rows.len() {
            let Some((first, end)) = text.rows[i] else {
                i += 1;
                continue;
            };
            if ox + end * cw <= bx0 || ox + first * cw >= bx1 {
                i += 1;
                continue;
            }
            let row = text.lo + i as i32;
            let (top, bottom) = (oy + row * ch, oy + (row + 1) * ch);
            let (y0, y1) = (top.max(by0), bottom.min(by1));
            if y1 <= y0 {
                i += 1;
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
            let mut col = first;
            while col <= end {
                let at = if col < end
                    && caret_col != Some(col)
                    && ox + (col + 1) * cw > bx0
                    && ox + col * cw < bx1
                    && let Some((Some(ink), ground)) = text.pair(row, col)
                    && let Some(b) = veil(ink & 0x00FF_FFFF, ground & 0x00FF_FFFF)
                {
                    Some((col, ground & 0x00FF_FFFF, b))
                } else {
                    None
                };
                match (run, at) {
                    (Some((c0, page, a)), Some((_, g, b))) if g == page => {
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
                        run = next;
                    }
                }
                col += 1;
            }
            i += 1;
        }
        self.icon_scratch = bands;
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
            // The stream's ramps keep their places among the quads they
            // were drawn before, the veil beneath them all.
            let mut ri = 0;
            while ri < ramps.items.len() {
                ramps.items[ri].at += laid as u32;
                ri += 1;
            }
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
    /// pet's tick drew this frame ([`Self::note_pet_body`]), laid in the
    /// frame's published under stream `out` (its quads and their ramps),
    /// kept to the silhouette of the body's sprite among the frame's
    /// `sprites` (grid pixels; the one whose box is the body's). The host calls it after the pet's tick, its
    /// word pass and the trail's publish; the print of what it drew. What it
    /// drew is kept ([`Self::pet_veil`]) for the frames that project the
    /// trail's streams again before the next pet tick.
    #[inline]
    pub fn veil_pet_letters(
        &mut self,
        (out, ramps): (&mut Vec<GlowQuad>, &mut GlowRamps),
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
                self.veil_letters_on_pet((&mut *out, ramps), caret, geom, sprite)
            }
            None => 0,
        };
        self.pet_veil.clear();
        if let Some(slice) = out.get(from..) {
            self.pet_veil.extend_from_slice(slice);
        }
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

    /// Lane B5: the guard cuts a stream's ramps as it cuts the quads their
    /// rows stand for, a rail laid clear of the ring included
    /// ([`InkLedger::note_rail_laid`]): a rail's row, or its run's one
    /// quad, is cut only where it meets a dimmed cell's box, and every cut
    /// piece is truncated as the quad's is. A light across a dimmed row and
    /// a rail below it, laid once as quads and once as ramps (every run a
    /// ramp), are cut alike, light for light. Premise: the rail noted, its
    /// quads keep light the feather would cut. Negative controls: the
    /// rail's ramps not noted (its quads alone, [`InkLedger::note_rail`]),
    /// or the ramps' cut pieces rounded, the ramps are cut otherwise.
    #[test]
    fn a_stream_s_ramps_are_cut_as_their_quads_rails_included() {
        use aterm_render::{BeamRows, RampCost, RampSink};
        // Cells 10 x 20 px; columns 4..7 of the row at y 40..60 dimmed.
        let dimmed = || {
            let mut ledger = InkLedger::EMPTY;
            ledger.grid = (0.0, 0.0, 10.0, 20.0);
            for (col, share) in [(4, 0.3), (5, 0.25), (6, 0.4)] {
                ledger.dims.push(Dim {
                    x0: col as f32 * 10.0,
                    y0: 40.0,
                    x1: (col + 1) as f32 * 10.0,
                    y1: 60.0,
                    share,
                });
            }
            ledger
        };
        // A row's ends: additive, every third source-over.
        fn ends(y: u16) -> ((u32, u8), (u32, u8)) {
            let k = u32::from(y % 5 + 1);
            if y.is_multiple_of(3) {
                ((k * 0x000E_0804, 0x50), (k * 0x0004_080C, 0x48))
            } else {
                ((k * 0x0018_1008, 0), (k * 0x0006_0C14, 0))
            }
        }
        // The light across the row (y 30..56), then the rail below it: rows
        // 56..60 in the dimmed box, a run's tall light at 60..64 and rows
        // to 70, in the feather's reach and past it. The rail's marks.
        fn lay<S: BeamRows>(out: &mut S) -> (S::Mark, S::Mark) {
            out.slab();
            for y in 30..56 {
                let (a, b) = ends(y);
                out.row(i32::from(y / 20), y / 20, (30, y, 50), a, b);
            }
            let from = out.mark();
            out.slab();
            for y in 56..60 {
                let (a, b) = ends(y);
                out.row(3, 3, (30, y, 50), a, b);
            }
            out.rect(3, 3, (30, 60, 50, 4), (0x0020_1810, 0));
            for y in 64..70 {
                let (a, b) = ends(y);
                out.row(3, 3, (30, y, 50), a, b);
            }
            (from, out.mark())
        }
        let lead = GlowQuad::flat(1, 0, 10, 4, 4, 0x0010_1010, 0);
        let legacy = |rails: bool| {
            let mut quads = vec![lead];
            let (from, to) = lay(&mut quads);
            let mut ledger = dimmed();
            if rails {
                ledger.note_rail(from, to);
            }
            ledger.guard_mask_stream(&mut quads, &mut GlowRamps::default(), true);
            quads
        };
        let ramped = |(rails, rounded): (bool, bool)| {
            let (mut quads, mut ramps) = (vec![lead], GlowRamps::default());
            let (from, to) = {
                let mut sink = RampSink::new(&mut quads, &mut ramps).costed(RampCost::ALWAYS);
                lay(&mut sink)
            };
            assert!(
                to.1.items() > from.1.items(),
                "premise: the rail rides ramps"
            );
            let mut ledger = dimmed();
            if rails {
                ledger.note_rail_laid(from, to);
            } else {
                ledger.note_rail(from.0, to.0);
            }
            ledger.round_cuts = rounded;
            ledger.guard_mask_stream(&mut quads, &mut ramps, true);
            let mut got = Vec::new();
            ramps.expand_into(&quads, &mut got);
            (got, ramps.laid(quads.len()))
        };
        let key = |q: &GlowQuad| (q.y, q.x, q.w, q.h, q.color, q.alpha, q.color2, q.alpha2);
        let sorted = |mut v: Vec<GlowQuad>| {
            v.sort_by_key(key);
            v
        };
        let want = legacy(true);
        assert_ne!(
            sorted(want.clone()),
            sorted(legacy(false)),
            "premise: the rail keeps light the feather would cut"
        );
        let (got, laid) = ramped((true, false));
        assert_eq!(
            sorted(got),
            sorted(want.clone()),
            "the ramps are cut as their quads"
        );
        assert_eq!(laid, want.len(), "the stream counts alike");
        assert_ne!(
            sorted(ramped((false, false)).0),
            sorted(want.clone()),
            "control: the rail's ramps not noted, they are cut as the rail's quads are"
        );
        assert_ne!(
            sorted(ramped((true, true)).0),
            sorted(want),
            "control: rounded, the ramps' cut pieces are their quads' truncated"
        );
    }
}
