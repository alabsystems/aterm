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
/// How far from an ink pixel its ground is read, px (the judge's ring).
const RING: i32 = 2;
/// Bisection steps for a cell's share.
const STEPS: usize = 7;

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

/// The guard's resident scratch ([`CursorGlow::guard_letters`]).
#[derive(Default)]
pub(crate) struct LetterGuard {
    /// The lights that reach the row being read.
    row: Vec<(Stream, u32)>,
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
    /// A test's negative control: the guard off.
    #[cfg(any(test, debug_assertions))]
    pub(crate) off: bool,
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
            for y in y0..y1 {
                for x in x0..x1 {
                    let at = (y - j.y0) as usize * w + (x - j.x0) as usize;
                    let c = match stream {
                        Stream::Under => quad_at(&under[i], x, 1.0).0,
                        Stream::Over => quad_at(&over[i], x, 1.0).0,
                        _ => {
                            let wt = halo_wt(&halos[i], x, y);
                            if wt == 0 {
                                continue;
                            }
                            aterm_render::premul_rgb(halos[i].color & 0x00FF_FFFF, wt)
                        }
                    };
                    if stream == Stream::Under {
                        add(&mut lu[at], c);
                    } else {
                        add(&mut lo[at], c);
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
        'rows: for gy in 0..h {
            let row = gy * w;
            for x in 0..w {
                let g = if additive {
                    lift(j.ground, self.lift_under[row + x], self.lift_over[row + x])
                } else {
                    let at = (j.x0 + x as i32, j.y0 + gy as i32, row + x);
                    self.composite(j.ground, at, true, s, streams)
                };
                lum[row + x] = crate::color_math::relative_luminance_with(table, g);
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
                let ink = if additive {
                    lift(j.ink, [0; 3], self.lift_over[y * w + x])
                } else {
                    let at = (j.x0 + x as i32, j.y0 + y as i32, y * w + x);
                    self.composite(j.ink, at, false, s, streams)
                };
                let li = crate::color_math::relative_luminance_with(table, ink);
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
    let ratio = |a: u32| {
        let (la, lg) = (lum(a), lum(ground));
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
    for _ in 0..10 {
        let mid = 0.5 * (lo + hi);
        if ratio(mix(mid)) >= want {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    mix(lo)
}

impl InkLedger {
    /// Cut every quad of `quads` (additive or source-over) to the guard's
    /// dims ([`Self::mask_at`]), easing back to the whole light off them.
    fn guard_mask_quads(&mut self, quads: &mut Vec<GlowQuad>) {
        let mut pieces = std::mem::take(&mut self.pieces);
        pieces.clear();
        let mut cut = false;
        for &q in quads.iter() {
            let (x0, y0) = (i32::from(q.x), i32::from(q.y));
            let (x1, y1) = (x0 + i32::from(q.w), y0 + i32::from(q.h));
            if q.w == 0 || q.h == 0 || !self.knots((x0, y0, x1, y1)) {
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
        let mut pieces = std::mem::take(&mut self.halo_pieces);
        pieces.clear();
        let mut cut = false;
        for &h in halos.iter() {
            let (x0, y0) = (i32::from(h.x), i32::from(h.y));
            let (x1, y1) = (x0 + i32::from(h.w), y0 + i32::from(h.h));
            if h.w == 0 || h.h == 0 || !self.knots((x0, y0, x1, y1)) {
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
    /// keeps [`LETTER_FLOOR`]. Classic's wake is the v0.28 restoration and
    /// keeps its bytes (owner ruling). Returns a print of the cuts.
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
        if matches!(style, GlowStyle::Classic)
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
        let mut solid: Option<((u32, u32), u32)> = None;
        for (i, span) in text.rows.iter().enumerate() {
            let Some((first, end)) = *span else {
                continue;
            };
            let row = text.lo + i as i32;
            let (ry0, ry1) = (oy + row * ch - RING, oy + (row + 1) * ch + RING);
            guard.row.clear();
            let meets_y = |y: u16, h: u16| i32::from(y) < ry1 && i32::from(y) + i32::from(h) > ry0;
            let lit = |q: &GlowQuad| q.w > 0 && q.h > 0 && (q.color | q.color2) & 0x00FF_FFFF != 0;
            for (k, q) in streams.0.iter().enumerate() {
                if lit(q) && meets_y(q.y, q.h) {
                    guard.row.push((Stream::Under, k as u32));
                }
            }
            for (k, q) in streams.1.iter().enumerate() {
                if lit(q) && meets_y(q.y, q.h) {
                    guard.row.push((Stream::Over, k as u32));
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
                        guard.row.push((s, k as u32));
                    }
                }
            }
            if guard.row.is_empty() {
                continue;
            }
            for col in first..end {
                if caret == Some((row, col)) {
                    continue;
                }
                let Some((Some(ink), ground)) = text.pair(row, col) else {
                    continue;
                };
                let (ink, ground) = (ink & 0x00FF_FFFF, ground & 0x00FF_FFFF);
                let (cx0, cx1) = (ox + col * cw - RING, ox + (col + 1) * cw + RING);
                guard.cell.clear();
                for &(stream, k) in &guard.row {
                    let (x, w) = match stream {
                        Stream::Under => (streams.0[k as usize].x, streams.0[k as usize].w),
                        Stream::Over => (streams.1[k as usize].x, streams.1[k as usize].w),
                        Stream::HaloAdd | Stream::HaloOver => {
                            (streams.2[k as usize].x, streams.2[k as usize].w)
                        }
                    };
                    if i32::from(x) < cx1 && i32::from(x) + i32::from(w) > cx0 {
                        guard.cell.push((stream, k));
                    }
                }
                if guard.cell.is_empty() {
                    continue;
                }
                let ink = match solid {
                    Some((pair, s)) if pair == (ink, ground) => s,
                    _ => {
                        let s = solid_ink(ink, ground, table);
                        solid = Some(((ink, ground), s));
                        s
                    }
                };
                let j = Judge {
                    x0: cx0,
                    y0: ry0,
                    x1: cx1,
                    y1: ry1,
                    ink,
                    ground,
                };
                let lum = |c: u32| crate::color_math::relative_luminance_with(table, c);
                let ratio = |a: u32, b: u32| {
                    let (la, lb) = (lum(a), lum(b));
                    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
                };
                // The bare page's reading: every light at share 0 composes
                // to the ground and the ink themselves.
                let bar = letter_bar(ratio(ink, ground));
                if let Some((on_ink, all)) = guard.bound(j, streams) {
                    // Light ink on a dark page reads worst against the
                    // brightest ground the lights can make; dark ink on
                    // paper, lifted the most, against the page itself.
                    let clear = if lum(ink) >= lum(ground) {
                        let g = lifted(ground, all);
                        lum(g) < lum(ink) && ratio(ink, g) >= bar
                    } else {
                        let i = lifted(ink, on_ink);
                        lum(i) < lum(ground) && ratio(i, ground) >= bar
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
                    print = aterm_hash::fnv_mix_u64(
                        print | 1,
                        ((row as u64) << 48) ^ ((col as u64) << 32) ^ u64::from(share.to_bits()),
                    );
                }
            }
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

    /// A test's negative control: lights drawn without the letter guard.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn set_letter_guard(&mut self, on: bool) {
        self.letter_guard.off = !on;
    }
}
