// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A THIN CARET READS AGAINST ITS CELL AND ITS RING (composed r1's judges, O2(a); the caret's
//! legibility ruling: the caret reads 4.5:1 or more against what is beside it, and light is not
//! colour).
//!
//! A bar or underline caret is two pixels or so of the caret's colour at the edge of a cell
//! the trail lights: the head glow, the halos, the momentum halo and the Return's flash are
//! anchored on it (`GlowConfig::head_dx`), so the light it is read against is the light of its
//! own cell and the light right beside its line (`trail-contrast`'s thin-caret reader: its
//! line's median against the closer of the page and the median of the cell's other pixels, and
//! each pixel of its line against the median of the page pixels on its nearest Chebyshev ring,
//! the lower median of the worst slice across the line: a bar's pixel row, an underline's pixel
//! column). On GitHub Light the theme's `#0969da` bar, 5.19:1 on the
//! paper, read 3.14:1 in a ribbon's pink and 1.94 in Comet's disc; on the dark page Fire's ember
//! rod read 2.87 in its brown halo and the green bar 1.08 in Laser's flash; and a band head laid
//! beside the bar (`#918766` on Solarized-dark's olive) read white at 3.58 on the ring alone.
//!
//! The renderer lays the caret's neighbourhood as the frame does ([`CursorGlow::thin_caret_hood`]:
//! each cell's trail blend, the under stream, the fire, the text, the over stream and the halos)
//! and reads it as the reader does. The caret's colour is moved in lightness alone ([`lifted`]):
//! each channel by one amount toward white on a dark page and toward black on paper until the
//! first meets its end, then on toward white or black, to the least lift whose line reads
//! [`THIN_CARET_MIN`] against the page, its cell and its ring. A lift is taken at once and given
//! back no faster than [`RELEASE_PER_S`] of relative luminance a second ([`ThinLift`]), so a ring
//! that flickers about a lift's threshold turns the bar a few times a second at most, never at
//! the trail's frame rate (the old lift-first swung dark jump's bar 8 times a second). The theme's
//! caret colour is no pin: its hue is kept while it can be, and its light is the caret's to
//! choose. A caret colour the user or a program pinned (config, OSC 12) goes along the first leg
//! alone, which keeps its hue and chroma.
//!
//! Where white (dark) or black (paper), or a pinned colour at its first leg's end, still reads
//! under the bar there, lightness cannot do it and the light is held, only where it fails: the
//! caret's cell to the largest share of itself the end colour reads its cell against, easing
//! back over the guard's feather, as the letter guard holds the light on a letter; and each run
//! of ring pixels the end colour fails, with a hard edge, to the largest share of its light the
//! end reads against (a light-owned light included, inside that strip alone; a foot rail never).
//! Nothing is moved: the head stays anchored on the bar and keeps its shape. Classic's spectrum
//! face on a dark theme keeps its bytes (the v0.28 ruling): its caret is lifted, never its light.
//! Controls: [`CursorGlow::set_thin_caret_read`], [`CursorGlow::set_thin_caret_ring`],
//! [`CursorGlow::set_thin_caret_release`].

use super::*;

/// The upper median of byte samples, without ordering the scratch they occupy.
/// A byte's closed value range needs at most eight cumulative-count decisions.
fn upper_channel_median(values: &[u8]) -> u8 {
    let Some((&first, rest)) = values.split_first() else {
        return 0;
    };
    let (mut lo, mut hi) = (u16::from(first), u16::from(first));
    for &value in rest {
        lo = lo.min(u16::from(value));
        hi = hi.max(u16::from(value));
    }
    let rank = values.len() / 2;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let threshold = mid as u8;
        let count = values.iter().filter(|&&value| value <= threshold).count();
        if count > rank {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo as u8
}

/// Composite a stream into bounded RGB pixels in its original draw order.
/// Bounds limit iteration; gradient coordinates remain those of the whole quad.
fn paint_hood_lights(
    pixels: &mut [u32],
    quads: &[GlowQuad],
    ramps: &aterm_core::render::GlowRamps,
    bounds: (i32, i32, i32, i32),
) {
    let (_, y, w, h) = bounds;
    debug_assert_eq!(pixels.len(), (w * h) as usize);
    ramps.in_order(quads.len(), |laid| match laid {
        aterm_core::render::Laid::Quads(range) => {
            for quad in &quads[range] {
                paint_hood_quad(pixels, quad, bounds);
            }
        }
        aterm_core::render::Laid::Ramp(k) => {
            for quad in ramps.items[k].quads_within(&ramps.ends, (y, y + h)) {
                paint_hood_quad(pixels, &quad, bounds);
            }
        }
    });
}

#[inline]
fn paint_hood_quad(pixels: &mut [u32], q: &GlowQuad, (bx, by, bw, bh): (i32, i32, i32, i32)) {
    let (x, y, w, h) = (
        i32::from(q.x),
        i32::from(q.y),
        i32::from(q.w),
        i32::from(q.h),
    );
    if w == 0 || h == 0 || x >= bx + bw || x + w <= bx || y >= by + bh || y + h <= by {
        return;
    }
    for xx in x.max(bx)..(x + w).min(bx + bw) {
        let (i, n) = ((xx - x) as u32, w as u32);
        let color = aterm_render::glow_lerp_rgb(q.color, q.color2, i, n);
        let alpha = aterm_render::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, n) as u8;
        for yy in y.max(by)..(y + h).min(by + bh) {
            let at = ((yy - by) * bw + xx - bx) as usize;
            pixels[at] = aterm_render::over_premul(pixels[at], color, alpha) & 0x00FF_FFFF;
        }
    }
}

/// The least a thin caret's line reads against the page, the light of its cell and its ring
/// ([`CursorGlow::hold_light_for_thin_caret`]): the legibility bar and a guard for the post
/// passes (the bloom and the crown light the cell past what the frame's streams lay), as the
/// chosen bolt's ([`crate::cursor_fx::CHOSEN_BOLT_HALO_MIN`]).
pub(crate) const THIN_CARET_MIN: f32 = crate::cursor_fx::CARET_PAGE_MIN_CONTRAST + 0.6;

/// How fast a thin caret's lift is given back, in relative luminance a second
/// ([`CursorGlow::thin_caret_fill`]): a bar lifted from the Dracula green to white eases back
/// over about 1.3 s, and a ring flickering about the lift's threshold turns it about 4 times a
/// second at most (the flash law's bound is 6).
pub(crate) const RELEASE_PER_S: f32 = 0.20;

/// How many steps along its lightness path a thin caret's colour is tried at ([`lifted_to`]):
/// the path's two legs at a sixty-fourth of each.
const PATH_STEPS: u32 = 128;

/// Bisection steps for the share of the cell's light a held caret keeps.
const SHARE_STEPS: u32 = 8;

/// How many times a held caret's cell is held again on what the frame then lays there
/// ([`CursorGlow::hold_light_for_thin_caret`]).
const HOLD_ROUNDS: u32 = 3;

/// The reach of a thin caret's ring ([`CursorGlow::thin_caret_hood`]): the reader's
/// (`trail-contrast`'s `RING_REACH`), its nearest Chebyshev ring of page pixels, 1 then 2.
const HOOD: i32 = 2;

/// The fire's field (`aterm_render::fire_field`, as the renderer's `draw_fire_patch` lays it:
/// every adding patch, then every veiling one) over `px`, the light of the window box
/// `(x0, y0, w, h)` row-major; `pad` is the frame's padding, where the field's top fade starts.
fn fire_over(
    px: &mut [u32],
    patches: &[FirePatch],
    (x0, y0, w, h): (i32, i32, i32, i32),
    pad: i32,
) {
    use aterm_core::render::FireMode;
    use aterm_render::fire_field as ff;
    for mode in [FireMode::Add, FireMode::Over] {
        for q in patches.iter().filter(|q| q.mode == mode) {
            let (qx, qy) = (i32::from(q.x), i32::from(q.y));
            let (qx1, qy1) = (qx + i32::from(q.w), qy + i32::from(q.h));
            if qx >= x0 + w || qx1 <= x0 || qy >= y0 + h || qy1 <= y0 {
                continue;
            }
            let fp = ff::FireFieldParams {
                base_y: i32::from(q.base_y),
                peak_h: i32::from(q.peak_h),
                phase: q.phase,
                temp: i32::from(q.temp),
                strength: i32::from(q.strength),
                lean: i32::from(q.lean),
                cov_cap: i32::from(q.cov_cap),
                cell_h: i32::from(q.cell_h),
                top_fade_y: pad,
            };
            let pc = ff::fire_precomp(&fp);
            for y in qy.max(y0)..qy1.min(y0 + h) {
                let tf = ff::fire_top_fade(y, &fp);
                let mut row = ff::FireRow::new(y, qx, &fp, &pc);
                for x in qx..qx1.min(x0 + w) {
                    let c = row.core(x);
                    if x < x0 {
                        continue;
                    }
                    let at = &mut px[((y - y0) * w + (x - x0)) as usize];
                    match mode {
                        FireMode::Add => {
                            let pm = ff::fire_shade_add(&c, &fp, tf);
                            if pm != 0 {
                                *at = aterm_render::add_sat(*at, pm) & 0x00FF_FFFF;
                            }
                        }
                        FireMode::Over => {
                            let (rgb, a) = ff::fire_shade_over(&c, &fp, tf);
                            if a != 0 {
                                *at = aterm_render::over_rgb(*at, rgb, a) & 0x00FF_FFFF;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A bar or underline caret: its cell, its shape and the ground the renderer lays it on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThinCaret {
    /// `(row, col)`.
    pub(crate) cell: (u16, u16),
    /// A bar (its left columns); else an underline (its bottom rows).
    pub(crate) bar: bool,
    /// The caret cell's background.
    pub(crate) page: u32,
    /// The caret's colour where the user or a program pinned it (config, OSC 12): moved along
    /// its path's first leg alone, which keeps its hue and chroma (the pin ruling).
    pub(crate) pin: Option<u32>,
}

impl ThinCaret {
    /// How far along its lightness path ([`lifted`]) this caret's colour may go: the whole
    /// path, or its first leg for a pinned colour.
    #[inline]
    const fn reach(self) -> f32 {
        if self.pin.is_some() { 1.0 } else { 2.0 }
    }

    /// What this caret's line reads against the page, its cell and its ring
    /// ([`CursorGlow::hold_light_for_thin_caret`]): [`THIN_CARET_MIN`], or for a pinned colour
    /// no more than the pin reads on its own page. A pin is the person's: it is moved only where
    /// the light round it would read it under its own reading or that bar, so with nothing lit it
    /// keeps its bytes (GitHub's `#8250df` pinned on white reads 5.05 and was lifted on every
    /// focused frame), and the end of its first leg always reads what is asked of it on the page.
    #[inline]
    pub(crate) fn need(self, pin_need: bool) -> f32 {
        match self.pin.filter(|_| pin_need) {
            Some(pin) => {
                crate::color_math::contrast_ratio(pin & 0x00FF_FFFF, self.page & 0x00FF_FFFF)
                    .min(THIN_CARET_MIN)
            }
            None => THIN_CARET_MIN,
        }
    }
}

/// A bar caret's width in a cell `cw` px wide, as the renderer draws it
/// (`aterm_render::for_each_cursor_rect`): an eighth of the cell, two px at least.
#[inline]
#[must_use]
pub(crate) const fn bar_width(cw: i32) -> i32 {
    clamp_i32(cw / 8, 2, cw)
}

impl ThinCaret {
    /// The caret's line in its cell, `(x0, y0, x1, y1)` from the cell's top left, end exclusive,
    /// as the renderer draws it (`aterm_render::for_each_cursor_rect`).
    #[inline]
    pub(crate) const fn line(self, cw: i32, ch: i32) -> (i32, i32, i32, i32) {
        if self.bar {
            (0, 0, bar_width(cw), ch)
        } else {
            let t = clamp_i32(ch / 8, 2, ch);
            (0, ch - t, cw, ch)
        }
    }
}

#[inline]
const fn clamp_i32(v: i32, lo: i32, hi: i32) -> i32 {
    let v = if v < lo { lo } else { v };
    if v > hi { hi } else { v }
}

/// `fill` along its lightness path at `m` (`0..=2`): over the first leg one shift on every
/// channel (toward white on a dark page, toward black on paper) up to where the first channel
/// meets its end, which keeps its hue and chroma; over the second, from there on toward white
/// or black, which keeps its hue.
#[inline]
pub(crate) fn lifted(fill: u32, dark: bool, m: f32) -> u32 {
    let fill = fill & 0x00FF_FFFF;
    let ch = [(fill >> 16) & 0xff, (fill >> 8) & 0xff, fill & 0xff];
    let room = if dark {
        255 - ch[0].max(ch[1]).max(ch[2])
    } else {
        ch[0].min(ch[1]).min(ch[2])
    } as f32;
    let shift = |d: f32| -> u32 {
        let c = |v: u32| {
            let v = v as f32;
            (if dark { v + d } else { v - d }).round().clamp(0.0, 255.0) as u32
        };
        (c(ch[0]) << 16) | (c(ch[1]) << 8) | c(ch[2])
    };
    let m = m.clamp(0.0, 2.0);
    if m <= 1.0 {
        return shift(room * m);
    }
    let end = if dark { 0x00FF_FFFF } else { 0 };
    crate::effect_util::lerp_rgb(shift(room), end, m - 1.0)
}

/// Whether `c` reads `need` ([`ThinCaret::need`]) against `page` and against `ground`.
#[inline]
fn reads(c: u32, page: u32, ground: u32, need: f32) -> bool {
    use crate::color_math::contrast_ratio;
    contrast_ratio(c, page) >= need && contrast_ratio(c, ground) >= need
}

/// The least lift of `fill` along its path ([`lifted`]) up to `reach` that `ok` holds for,
/// as `(m, colour)`, or `None` where none does. A fill `ok` holds for already is `fill` itself.
#[inline]
fn least_lift(fill: u32, dark: bool, reach: f32, ok: impl Fn(u32) -> bool) -> Option<(f32, u32)> {
    let fill = fill & 0x00FF_FFFF;
    if ok(fill) {
        return Some((0.0, fill));
    }
    // Not one bisection: on its way toward white a bar under a brighter ground reads it less
    // before it reads it more.
    let mut k = 1;
    while k <= PATH_STEPS {
        let m = reach * k as f32 / PATH_STEPS as f32;
        let c = lifted(fill, dark, m);
        if ok(c) {
            return Some((m, c));
        }
        k += 1;
    }
    None
}

/// The least lift of `fill` along its path ([`lifted`]) up to `reach` that reads against
/// `page` and `ground` ([`reads`]), or `None` where none does. A fill that reads already is
/// `fill` itself (the tests' reading of a caret's path).
#[cfg(test)]
#[inline]
pub(crate) fn lifted_to(fill: u32, page: u32, ground: u32, reach: f32) -> Option<u32> {
    let dark = aterm_render::theme_is_dark(page & 0x00FF_FFFF);
    least_lift(fill, dark, reach, |c| {
        reads(c, page & 0x00FF_FFFF, ground, THIN_CARET_MIN)
    })
    .map(|(_, c)| c)
}

/// `page` lit by `ground`'s light at `share` of it, channel by channel.
#[inline]
fn held(page: u32, ground: u32, share: f32) -> u32 {
    crate::effect_util::lerp_rgb(page, ground, share)
}

/// `c`'s relative luminance.
#[inline]
fn lum(c: u32) -> f32 {
    crate::color_math::relative_luminance_with(crate::color_math::relative_luminance_table(), c)
}

/// What a pixel of a thin caret's neighbourhood is ([`HoodPx::kind`]).
const GROUND: u8 = 0;
/// The caret's own line.
const LINE: u8 = 1;
/// A letter's pixel or a cell of another ground: never ring.
const OTHER: u8 = 2;

/// One pixel of a thin caret's neighbourhood ([`CursorGlow::thin_caret_hood`]).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HoodPx {
    /// What the frame lays there: the cell's trail blend lit by every stream, its text between.
    lit: u32,
    /// The cell's trail blend over its ground, unlit.
    base: u32,
    /// The cell's ground and its letter's colour.
    ground: u32,
    fg: u32,
    /// The letter's coverage there.
    cov: u8,
    /// [`GROUND`], [`LINE`] or [`OTHER`].
    kind: u8,
    /// A foot rail (the trail's underline) or a page shade lies here: held only by cutting the
    /// other light laid there, which the guard's cut leaves the rail and the shade out of, never
    /// by laying the page over it, which would dim the trail.
    keep: bool,
    /// A pixel of a cell whose ink the frame cannot raster: held by cutting its light, never by
    /// laying the page over it, which would fade a letter there.
    inked: bool,
}

impl HoodPx {
    /// Whether a hold here cuts the streams ([`Self::keep`], [`Self::inked`]) rather than laying
    /// the page over the frame's light.
    const fn cut(self) -> bool {
        self.keep || self.inked
    }
}

/// A thin caret's neighbourhood as the frame lays it: the box (the caret's cell [`HOOD`] px
/// round, window px), its pixels row-major, and for each pixel of its line (slice by slice across
/// it), the slice it lies in and its ring's pixels ([`CursorGlow::thin_caret_hood`]).
#[derive(Debug)]
pub(crate) struct ThinHood {
    at: HoodBox,
    px: Vec<HoodPx>,
    /// Each line pixel, slice by slice across the line: its box index, its slice (its window row
    /// on a bar, its window column on an underline), and its ring's range of `ring`.
    line: Vec<(u32, i32, u32, u32)>,
    ring: Vec<u32>,
    /// Each line pixel's ring median, as [`Self::line`].
    meds: Vec<u32>,
    /// Their relative luminances.
    meds_l: Vec<f32>,
    /// The cell's own ground: the median of its lit pixels off the line, its letter's among them.
    cell: u32,
    /// A bar's (its ring runs down the columns); else an underline's (along the rows).
    bar: bool,
    /// What its caret's line reads against all of it ([`ThinCaret::need`]).
    need: f32,
}

impl Default for ThinHood {
    fn default() -> Self {
        Self {
            at: (0, 0, 0, 0),
            px: Vec::new(),
            line: Vec::new(),
            ring: Vec::new(),
            meds: Vec::new(),
            meds_l: Vec::new(),
            cell: 0,
            bar: false,
            need: THIN_CARET_MIN,
        }
    }
}

impl ThinHood {
    /// Whether `c` drawn on the line reads its caret's need ([`ThinCaret::need`]) against `page`, the cell and, slice
    /// by slice across the line (a bar's pixel rows, an underline's pixel columns), the ring: the
    /// lower median of each slice's readings, as the reader takes it.
    fn reads_all(&self, c: u32, page: u32, ring: bool) -> bool {
        use crate::color_math::{contrast_ratio_from_luminance as ratio, relative_luminance};
        if !reads(c, page, self.cell, self.need) {
            return false;
        }
        if !ring {
            return true;
        }
        let lc = relative_luminance(c);
        let mut i = 0;
        while i < self.line.len() {
            let y = self.line[i].1;
            let (mut n, mut under) = (0usize, 0usize);
            while i < self.line.len() && self.line[i].1 == y {
                if self.line[i].2 < self.line[i].3 {
                    n += 1;
                    under += usize::from(ratio(lc, self.meds_l[i]) < self.need);
                }
                i += 1;
            }
            if n > 0 && under > (n - 1) / 2 {
                return false;
            }
        }
        true
    }

    /// Whether the hard dim `d`'s run ([`hard_runs`], [`CursorGlow::thin_ring_dims`], each of one
    /// kind) is held by cutting the streams ([`HoodPx::cut`]).
    fn cut_at(&self, d: &Dim) -> bool {
        let (bx0, by0, bw, bh) = self.at;
        let (x, y) = (d.x0 as i32 - bx0, d.y0 as i32 - by0);
        (0..bw).contains(&x) && (0..bh).contains(&y) && self.px[(y * bw + x) as usize].cut()
    }

    /// Lay the hard dim `d` over the frame's light (`over`, the stream drawn over the text): each
    /// run of its pixels on one ground is held to `d.share` of its light by its cell's unlit
    /// ground laid over it at the rest, `lit` eased toward `base` as [`held`] reads it, with no
    /// other pixel of the frame touched.
    fn lay_over(&self, d: &Dim, (geom, over): (Geom, &mut Vec<GlowQuad>)) {
        let (bx0, by0, bw, bh) = self.at;
        let alpha = ((1.0 - d.share) * 255.0).ceil().clamp(0.0, 255.0) as u8;
        if alpha == 0 {
            return;
        }
        let (x0, y0) = ((d.x0 as i32 - bx0).max(0), (d.y0 as i32 - by0).max(0));
        let (x1, y1) = ((d.x1 as i32 - bx0).min(bw), (d.y1 as i32 - by0).min(bh));
        // A bar's runs go down a column, an underline's along a row.
        let (along, n) = if x1 - x0 == 1 {
            (false, y1 - y0)
        } else {
            (true, x1 - x0)
        };
        let at = |k: i32| {
            if along {
                (y0 * bw + x0 + k) as usize
            } else {
                ((y0 + k) * bw + x0) as usize
            }
        };
        let mut k = 0;
        while k < n {
            let base = self.px[at(k)].base;
            let from = k;
            while k < n && self.px[at(k)].base == base {
                k += 1;
            }
            let (w, h) = if along { (k - from, 1) } else { (1, k - from) };
            let (x, y) = if along {
                (bx0 + x0 + from, by0 + y0)
            } else {
                (bx0 + x0, by0 + y0 + from)
            };
            crate::effect_util::push_fx_quad(
                over,
                geom,
                x,
                y,
                w,
                h,
                aterm_render::premul_rgb(base, alpha),
                alpha,
            );
        }
    }

    /// Each line pixel's ring median of what is lit there ([`Self::meds`]).
    fn read_rings(&mut self, chans: &mut [Vec<u8>; 3]) {
        self.meds.clear();
        self.meds_l.clear();
        for &(_, _, from, to) in &self.line {
            if from == to {
                self.meds.push(0);
                self.meds_l.push(0.0);
                continue;
            }
            for c in chans.iter_mut() {
                c.clear();
            }
            for &j in &self.ring[from as usize..to as usize] {
                let lit = self.px[j as usize].lit;
                chans[0].push(((lit >> 16) & 0xff) as u8);
                chans[1].push(((lit >> 8) & 0xff) as u8);
                chans[2].push((lit & 0xff) as u8);
            }
            let mut med = 0u32;
            for (k, c) in chans.iter_mut().enumerate() {
                med |= u32::from(upper_channel_median(c)) << (16 - 8 * k as u32);
            }
            self.meds.push(med);
            self.meds_l.push(lum(med));
        }
    }
}

/// The hard dims of `share`, a share of its light for each pixel of `hood` (`1` where it keeps
/// it whole): each run of pixels sharing one share, and one kind ([`HoodPx::cut`]), down a column
/// (a bar's ring) or along a row (an underline's), one dim apiece
/// ([`CursorGlow::hold_light_for_thin_caret`]'s carried hold).
fn hard_runs(hood: &ThinHood, share: &[f32], dims: &mut Vec<Dim>) {
    let (bx0, by0, bw, bh) = hood.at;
    let (outer, inner) = if hood.bar { (bw, bh) } else { (bh, bw) };
    let at = |a: i32, b: i32| {
        if hood.bar {
            (b * bw + a) as usize
        } else {
            (a * bw + b) as usize
        }
    };
    for a in 0..outer {
        let mut b = 0;
        while b < inner {
            let s = share[at(a, b)];
            if s >= 1.0 {
                b += 1;
                continue;
            }
            let from = b;
            let cut = hood.px[at(a, b)].cut();
            while b < inner && share[at(a, b)] == s && hood.px[at(a, b)].cut() == cut {
                b += 1;
            }
            let (f, l) = (at(a, from) as i32, at(a, b - 1) as i32);
            dims.push(Dim {
                x0: (bx0 + f % bw) as f32,
                y0: (by0 + f / bw) as f32,
                x1: (bx0 + l % bw + 1) as f32,
                y1: (by0 + l / bw + 1) as f32,
                share: s,
                hard: true,
            });
        }
    }
}

/// A thin caret's drawn lift ([`CursorGlow::thin_caret_fill`]): its luminance when drawn, then,
/// and on which side of the page it lifts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ThinLift {
    lum: f32,
    at: Instant,
    dark: bool,
    /// Whether it was drawn short of giving its lift back.
    releasing: bool,
}

/// A hood's box in the frame's pixels: left, top, width, height.
type HoodBox = (i32, i32, i32, i32);

/// What a thin caret's hold kept of the light round it ([`CursorGlow::hold_light_for_thin_caret`]):
/// for which caret and hood, when, the share of its cell's light the cell's dims kept, and each
/// hood pixel's share of its own the ring's hard dims kept. A hold is taken at once and given back
/// as the lift is, no faster than [`RELEASE_PER_S`] of each pixel's light a second: a ring
/// flickering about the end colour's threshold held and let go on alternate frames turned its
/// pixels at the trail's rate (dark backspace's seam above the bar, 8 turns a second).
#[derive(Debug, Default)]
pub(crate) struct ThinHeld {
    at: Option<(Instant, ThinCaret, HoodBox)>,
    cell: f32,
    px: Vec<f32>,
    /// Whether the last frame held more than it needed, giving its hold back.
    releasing: bool,
}

impl ThinHeld {
    #[inline]
    fn forget(&mut self) {
        self.at = None;
        self.px.clear();
        self.releasing = false;
    }
}

impl CursorGlow {
    /// A test's negative control: `on` (as shipped), a bar or underline caret reads against the
    /// light of its cell and its ring ([`Self::hold_light_for_thin_caret`]); off, it is drawn in
    /// its own colour on whatever light the trail lays there.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_thin_caret_read(&mut self, on: bool) {
        self.thin_caret_free = !on;
    }

    /// A test's negative control: `on` (as shipped), a thin caret reads against its ring as well
    /// as its cell; off, against its cell alone, as composed r2's did.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_thin_caret_ring(&mut self, on: bool) {
        self.thin_ring_off = !on;
    }

    /// A test's negative control: `on` (as shipped), a thin caret's lift is given back no faster
    /// than [`RELEASE_PER_S`]; off, each frame draws the least lift it needs.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_thin_caret_release(&mut self, on: bool) {
        self.thin_release_off = !on;
    }

    /// A test's negative control: `on` (as shipped), a thin caret's cell is read with its
    /// letter laid in it, as the frame lays it; off, without, as composed r2's was.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_thin_caret_text(&mut self, on: bool) {
        self.thin_text_off = !on;
    }

    /// A test's negative control: `on` (as shipped), the pixels of a cell whose ink the frame
    /// cannot raster (a wide Han letter, a decorated cell) on the page's ground are a thin
    /// caret's ring, read as the light over that ground, as the reader takes them where its twin
    /// shows the ground; off, such a cell is never ring, as composed r2's was.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_thin_caret_unrastered(&mut self, on: bool) {
        self.thin_unrastered_off = !on;
    }

    /// A test's negative control: `on` (as shipped), a thin caret's ring is read slice by slice
    /// across its line (a bar's pixel rows, an underline's pixel columns), as the reader reads
    /// it; off, by pixel rows, as a letter's strokes are, where an underline's row of 14 px kept
    /// its lower median with 6 of them at 2.4:1.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_thin_caret_across(&mut self, on: bool) {
        self.thin_rows_off = !on;
    }

    /// A test's negative control: `on` (as shipped), a pinned thin caret is asked no more than it
    /// reads on its own page ([`ThinCaret::need`]); off, [`THIN_CARET_MIN`] as the theme's is,
    /// which lifted a pin that read 4.5 on its page with nothing lit and, for a pin whose first
    /// leg's end could not read it, held the light round it to nothing.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_thin_caret_pin_need(&mut self, on: bool) {
        self.thin_pin_need_off = !on;
    }

    #[inline]
    const fn thin_pin_need_on(&self) -> bool {
        #[cfg(any(test, debug_assertions))]
        {
            !self.thin_pin_need_off
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            true
        }
    }

    #[inline]
    const fn thin_ring_on(&self) -> bool {
        #[cfg(any(test, debug_assertions))]
        {
            !self.thin_ring_off
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            true
        }
    }

    /// Whether a thin caret is drawn short of giving its lift back, so the frame cadence runs
    /// on until it has ([`Self::thin_caret_fill`]).
    #[inline]
    #[must_use]
    pub(crate) fn thin_caret_releasing(&self) -> bool {
        self.thin_lift.is_some_and(|l| l.releasing) || self.thin_held.releasing
    }

    /// Lay `caret`'s neighbourhood as the frame lays it ([`ThinHood`]): each cell's trail blend
    /// over its ground (`trail`, as the renderer's `draw_trail` blends it), the under stream, the
    /// fire's field ([`fire_over`]), the cell's text as the renderer blends it (the face's raster,
    /// [`aterm_render::TextLevel`]), the over stream (`over`) and the halos
    /// ([`crate::cursor_fx::light_at`]). With `kinds`, read which pixels are the ring's first:
    /// a pixel of the page's ground no letter covers, nearest each line pixel (Chebyshev 1, else
    /// 2), as the reader's ring takes the clean frame's ground. Returns the cell's ground.
    fn thin_caret_hood(
        &mut self,
        over: &[GlowQuad],
        caret: ThinCaret,
        (geom, trail): (Geom, Trail<'_>),
        kinds: bool,
    ) -> u32 {
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let cx0 = ox + i32::from(caret.cell.1) * cw;
        let cy0 = oy + i32::from(caret.cell.0) * ch;
        let (bx0, by0, bw, bh) = (cx0 - HOOD, cy0 - HOOD, cw + 2 * HOOD, ch + 2 * HOOD);
        let page = caret.page & 0x00FF_FFFF;
        let mut hood = std::mem::take(&mut self.thin_hood);
        #[cfg(any(test, debug_assertions))]
        let unrastered = !self.thin_unrastered_off;
        #[cfg(not(any(test, debug_assertions)))]
        let unrastered = true;
        if kinds {
            hood.at = (bx0, by0, bw, bh);
            hood.bar = caret.bar;
            hood.need = caret.need(self.thin_pin_need_on());
            hood.px.clear();
            let inks = self.glyph_ink.as_deref();
            let text = &self.frame_text;
            let (tr, tc) = trail;
            let (lx0, ly0, lx1, ly1) = caret.line(cw, ch);
            let (row, col) = (i32::from(caret.cell.0), i32::from(caret.cell.1));
            let (win_w, win_h) = (i32::from(geom.win_w), i32::from(geom.win_h));
            // The 3 by 3 cells round the caret's (the hood reaches [`HOOD`] px past its cell):
            // each one's ground and trail blend, read once.
            let mut cells = [(page, page); 9];
            for (k, cell) in cells.iter_mut().enumerate() {
                let (r, c) = (row + k as i32 / 3 - 1, col + k as i32 % 3 - 1);
                let ground = text.pair(r, c).map_or(page, |(_, g)| g & 0x00FF_FFFF);
                let alpha = tr
                    .iter()
                    .find(|t| (t.row as i64, t.col as i64) == (i64::from(r), i64::from(c)))
                    .map_or(0, |t| t.alpha);
                *cell = (
                    ground,
                    aterm_render::blend_rgb(ground, tc & 0x00FF_FFFF, alpha) & 0x00FF_FFFF,
                );
            }
            for y in by0..by0 + bh {
                for x in bx0..bx0 + bw {
                    let (r, c) = ((y - oy).div_euclid(ch), (x - ox).div_euclid(cw));
                    let (fx, fy) = (x - ox - c * cw, y - oy - r * ch);
                    let pair = text.pair(r, c);
                    let (dr, dc) = (r - row + 1, c - col + 1);
                    let (ground, base) = if (0..3).contains(&dr) && (0..3).contains(&dc) {
                        cells[(dr * 3 + dc) as usize]
                    } else {
                        let g = pair.map_or(page, |(_, g)| g & 0x00FF_FFFF);
                        (g, g)
                    };
                    let caret_cell = (r, c) == (i32::from(caret.cell.0), i32::from(caret.cell.1));
                    let mut p = HoodPx {
                        lit: base,
                        base,
                        ground,
                        fg: 0,
                        cov: 0,
                        kind: if ground == page { GROUND } else { OTHER },
                        keep: false,
                        inked: false,
                    };
                    if caret_cell && fx >= lx0 && fx < lx1 && fy >= ly0 && fy < ly1 {
                        p.kind = LINE;
                    } else if x < 0 || y < 0 || x >= win_w || y >= win_h {
                        // Off the window: no reader sees it (a caret in the first column).
                        p.kind = OTHER;
                    } else if let Some((Some(fg), _)) = pair {
                        let raster = inks
                            .filter(|_| text.plain(r, c))
                            .and_then(|i| i.raster(text.glyph(r, c)));
                        match raster {
                            Some(raster) => {
                                let cov = raster.at(fx, fy);
                                if cov > 0 {
                                    p.fg = fg & 0x00FF_FFFF;
                                    p.cov = cov;
                                    p.kind = OTHER;
                                }
                            }
                            // Its ink unknown: the reader's ring takes the twin's ground about
                            // it, which the light lies over as here (a bar beside a wide Han
                            // letter on GitHub Light read 4.45 on a ring the hood never saw).
                            None if unrastered => p.inked = true,
                            None => p.kind = OTHER,
                        }
                    }
                    hood.px.push(p);
                }
            }
            // The foot rails and page shades over the hood: the trail is never dimmed to buy the
            // caret's reading, and a shade, which darkens, is never lightened.
            let l = &self.ink_ledger;
            #[cfg(test)]
            let feet_whole = l.foot_rails_whole() && !self.thin_keep_off;
            #[cfg(not(test))]
            let feet_whole = l.foot_rails_whole();
            let mut keep = |x: u16, y: u16, w: u16, h: u16| {
                let (x0, y0) = ((i32::from(x) - bx0).max(0), (i32::from(y) - by0).max(0));
                let x1 = (i32::from(x) + i32::from(w) - bx0).min(bw);
                let y1 = (i32::from(y) + i32::from(h) - by0).min(bh);
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        hood.px[(yy * bw + xx) as usize].keep = true;
                    }
                }
            };
            let quads = self.under_out.as_slice();
            let ramps = self.under_ramps.items.as_slice();
            let rails = l
                .rails
                .iter()
                .filter(|r| r.2 && feet_whole)
                .map(|r| (r.0, r.1));
            for (a, b) in rails.chain(l.page_shades.iter().copied()) {
                for q in quads.get(a as usize..b as usize).unwrap_or(&[]) {
                    keep(q.x, q.y, q.w, q.h);
                }
            }
            let rails = l
                .rail_ramps
                .iter()
                .filter(|r| r.2 && feet_whole)
                .map(|r| (r.0, r.1));
            for (a, b) in rails.chain(l.page_shade_ramps.iter().copied()) {
                for r in ramps.get(a as usize..b as usize).unwrap_or(&[]) {
                    keep(r.x, r.y, r.w, r.h);
                }
            }
            // Each line pixel's ring: the nearest Chebyshev ring of ground pixels, slice by
            // slice across the line (a bar's pixel rows, an underline's pixel columns), each
            // keyed by its slice, as the reader reads a break in it.
            #[cfg(any(test, debug_assertions))]
            let across = !self.thin_rows_off;
            #[cfg(not(any(test, debug_assertions)))]
            let across = true;
            let by_columns = !caret.bar && across;
            let (outer, inner) = if by_columns { (bw, bh) } else { (bh, bw) };
            hood.line.clear();
            hood.ring.clear();
            for a in 0..outer {
                for b in 0..inner {
                    let (x, y) = if by_columns { (a, b) } else { (b, a) };
                    let i = (y * bw + x) as usize;
                    if hood.px[i].kind != LINE {
                        continue;
                    }
                    let from = hood.ring.len() as u32;
                    for d in 1..=HOOD {
                        for dy in -d..=d {
                            for dx in -d..=d {
                                if dx.abs().max(dy.abs()) != d {
                                    continue;
                                }
                                let (xx, yy) = (x + dx, y + dy);
                                if xx < 0 || yy < 0 || xx >= bw || yy >= bh {
                                    continue;
                                }
                                let j = (yy * bw + xx) as u32;
                                if hood.px[j as usize].kind == GROUND {
                                    hood.ring.push(j);
                                }
                            }
                        }
                        if hood.ring.len() as u32 > from {
                            break;
                        }
                    }
                    let slice = if by_columns { bx0 + x } else { by0 + y };
                    hood.line
                        .push((i as u32, slice, from, hood.ring.len() as u32));
                }
            }
        }
        // The lights, in the renderer's order.
        let meets = |x: u16, y: u16, w: u16, h: u16| {
            let (x, y, w, h) = (i32::from(x), i32::from(y), i32::from(w), i32::from(h));
            w > 0 && h > 0 && x < bx0 + bw && x + w > bx0 && y < by0 + bh && y + h > by0
        };
        let halos = &self.halo_out;
        let patches = &self.patch_out;
        let any_fire = patches.iter().any(|q| meets(q.x, q.y, q.w, q.h));
        let mut px = std::mem::take(&mut self.thin_caret_px);
        px.clear();
        for p in &hood.px {
            px.push(p.base);
        }
        paint_hood_lights(
            &mut px,
            &self.under_out,
            &self.under_ramps,
            (bx0, by0, bw, bh),
        );
        if any_fire {
            fire_over(&mut px, patches, (bx0, by0, bw, bh), ox);
        }
        let corrected = self
            .glyph_ink
            .as_deref()
            .is_some_and(aterm_render::GlyphInk::corrected);
        #[cfg(any(test, debug_assertions))]
        let text_on = !self.thin_text_off;
        #[cfg(not(any(test, debug_assertions)))]
        let text_on = true;
        for (i, p) in hood.px.iter().enumerate() {
            let mut at = px[i];
            if p.cov > 0 && text_on {
                at = aterm_render::TextLevel::new(p.fg, p.ground, p.cov, corrected).over(at)
                    & 0x00FF_FFFF;
            }
            px[i] = at;
        }
        paint_hood_lights(&mut px, over, &self.out_ramps, (bx0, by0, bw, bh));
        for (i, p) in hood.px.iter_mut().enumerate() {
            let point = (bx0 + (i as i32) % bw, by0 + (i as i32) / bw);
            p.lit = crate::cursor_fx::light_at(px[i], (&[], halos), point);
        }
        // The cell's ground: the median of its pixels off the line, its letter's among them, as
        // the reader's cell median takes it.
        let mut chans: [Vec<u8>; 3] = std::mem::take(&mut self.thin_caret_chans);
        for c in &mut chans {
            c.clear();
        }
        for fy in HOOD..HOOD + ch {
            for fx in HOOD..HOOD + cw {
                let p = hood.px[(fy * bw + fx) as usize];
                if p.kind == LINE {
                    continue;
                }
                chans[0].push(((p.lit >> 16) & 0xff) as u8);
                chans[1].push(((p.lit >> 8) & 0xff) as u8);
                chans[2].push((p.lit & 0xff) as u8);
            }
        }
        let mut cell = page;
        if chans.iter().all(|c| !c.is_empty()) {
            cell = 0;
            for (k, c) in chans.iter_mut().enumerate() {
                cell |= u32::from(upper_channel_median(c)) << (16 - 8 * k as u32);
            }
        }
        hood.cell = cell;
        hood.read_rings(&mut chans);
        self.thin_caret_px = px;
        self.thin_caret_chans = chans;
        self.thin_hood = hood;
        cell
    }

    /// The hard dims of the ring pixels `end` fails ([`Self::hold_light_for_thin_caret`]): each
    /// line pixel whose ring reads under [`THIN_CARET_MIN`] marks its ring's pixels that fail
    /// (all of them where none does alone: a median of channels), and each run of marked pixels
    /// along the line keeps the largest share of its light at which `end` reads every pixel.
    fn thin_ring_dims(&mut self, end: u32, dims: &mut Vec<Dim>) {
        use crate::color_math::contrast_ratio;
        let hood = &self.thin_hood;
        let need = hood.need;
        let (bx0, by0, bw, bh) = hood.at;
        let mut marked = std::mem::take(&mut self.thin_marked);
        marked.clear();
        marked.resize(hood.px.len(), false);
        for (i, &(_, _, from, to)) in hood.line.iter().enumerate() {
            if from == to || contrast_ratio(end, hood.meds[i]) >= need {
                continue;
            }
            let ring = &hood.ring[from as usize..to as usize];
            let mut any = false;
            for &j in ring {
                if contrast_ratio(end, hood.px[j as usize].lit) < need {
                    marked[j as usize] = true;
                    any = true;
                }
            }
            if !any {
                for &j in ring {
                    marked[j as usize] = true;
                }
            }
        }
        // Runs down each column (a bar's ring) or along each row (an underline's).
        let share_of = |run: &[usize]| {
            let ok = |s: f32| {
                run.iter().all(|&j| {
                    let p = hood.px[j];
                    contrast_ratio(end, held(p.base, p.lit, s)) >= need
                })
            };
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            let mut k = 0;
            while k < SHARE_STEPS {
                let mid = 0.5 * (lo + hi);
                if ok(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
                k += 1;
            }
            lo
        };
        let bar = hood.bar;
        let mut run: Vec<usize> = Vec::new();
        let (outer, inner) = if bar { (bw, bh) } else { (bh, bw) };
        for a in 0..outer {
            run.clear();
            for b in 0..=inner {
                let j = if b < inner {
                    Some(if bar {
                        (b * bw + a) as usize
                    } else {
                        (a * bw + b) as usize
                    })
                } else {
                    None
                };
                // A run is all of one kind: laid over, or cut where a letter's ink is unknown or
                // the trail's rail or a page shade lies.
                let joins = |j: usize| {
                    run.first()
                        .is_none_or(|&f: &usize| hood.px[f].cut() == hood.px[j].cut())
                };
                let next = j.filter(|&j| marked[j]);
                if let Some(j) = next.filter(|&j| joins(j)) {
                    run.push(j);
                    continue;
                }
                if !run.is_empty() {
                    let s = share_of(&run);
                    let (f, l) = (run[0] as i32, run[run.len() - 1] as i32);
                    let (x0, y0) = (bx0 + f % bw, by0 + f / bw);
                    let (x1, y1) = (bx0 + l % bw + 1, by0 + l / bw + 1);
                    dims.push(Dim {
                        x0: x0 as f32,
                        y0: y0 as f32,
                        x1: x1 as f32,
                        y1: y1 as f32,
                        share: s,
                        hard: true,
                    });
                    run.clear();
                }
                run.extend(next);
            }
        }
        self.thin_marked = marked;
    }

    /// A THIN CARET READS AGAINST ITS CELL AND ITS RING ([`self`]): `caret` and its ground (its
    /// page, or a trail cell's blend over it, `trail` each cell's). Where white (dark) or black
    /// (paper) reads under [`THIN_CARET_MIN`] against what the frame lays there, the light that
    /// fails is held: the cell to the largest share of itself the end colour reads the bar
    /// against (easing back over the guard's feather), and each run of failing ring pixels, hard
    /// edged, to the largest share of its light the end reads it against. A hold is given back no
    /// faster than [`RELEASE_PER_S`] ([`ThinHeld`]): what the last frame held is held first, each
    /// share risen by what the release allows over the time since. The neighbourhood is then read
    /// again and kept for [`Self::thin_caret_fill`]. Run on the frame's finished streams, after
    /// the letter guard. The frame's print of what it held.
    pub(crate) fn hold_light_for_thin_caret(
        &mut self,
        over: &mut Vec<GlowQuad>,
        caret: Option<(ThinCaret, u32)>,
        (geom, style, now): (Geom, GlowStyle, Instant),
        trail: Trail<'_>,
    ) -> u64 {
        self.thin_caret_read = None;
        #[cfg(any(test, debug_assertions))]
        if self.thin_caret_free {
            self.thin_held.forget();
            return 0;
        }
        let Some((caret, under)) = caret else {
            self.thin_held.forget();
            return 0;
        };
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        if cw <= 0 || ch <= 0 {
            self.thin_held.forget();
            return 0;
        }
        let under = under & 0x00FF_FFFF;
        let ring = self.thin_ring_on();
        let mut cell = self.thin_caret_hood(over, caret, (geom, trail), true);
        self.thin_caret_read = Some((caret, cell));
        let page = caret.page & 0x00FF_FFFF;
        let dark = aterm_render::theme_is_dark(page);
        let end = match caret.pin {
            Some(pin) => lifted(pin, dark, 1.0),
            None if dark => 0x00FF_FFFF,
            None => 0,
        };
        let need = caret.need(self.thin_pin_need_on());
        // An end that cannot read its need on the page itself buys nothing by a hold: the
        // light is kept.
        if self.classic_keeps_its_bytes(style)
            || crate::color_math::contrast_ratio(end, page) < need
        {
            self.thin_held.forget();
            return 0;
        }
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let (cx0, cy0) = (
            ox + i32::from(caret.cell.1) * cw,
            oy + i32::from(caret.cell.0) * ch,
        );
        let cell_dim = |share: f32| Dim {
            x0: (cx0 - RING) as f32,
            y0: (cy0 - RING) as f32,
            x1: (cx0 + cw + RING) as f32,
            y1: (cy0 + ch + RING) as f32,
            share,
            hard: false,
        };
        // What this frame holds: the cell's share and each hood pixel's, as the dims multiply.
        let n = self.thin_hood.px.len();
        let mut kept = std::mem::take(&mut self.thin_held);
        let mut px_kept = std::mem::take(&mut self.thin_px_kept);
        px_kept.clear();
        px_kept.resize(n, 1.0);
        let mut cell_kept = 1.0f32;
        // Which of them the carried hold set and no round cut further: the hold given back.
        let mut carried = std::mem::take(&mut self.thin_carried);
        carried.clear();
        carried.resize(n, false);
        let mut cell_carried = false;
        let mut print = 0u64;
        let mut dims: Vec<Dim> = Vec::new();
        #[cfg(any(test, debug_assertions))]
        let release = !self.thin_release_off;
        #[cfg(not(any(test, debug_assertions)))]
        let release = true;
        if release
            && let Some((at, was, hood_at)) = kept.at
            && was == caret
            && hood_at == self.thin_hood.at
            && kept.px.len() == n
        {
            let dt = now.saturating_duration_since(at).as_secs_f32();
            let risen = |share: f32, reach: f32| {
                if reach <= 1.0e-3 {
                    1.0
                } else {
                    share + RELEASE_PER_S * dt / reach
                }
            };
            let hood = &self.thin_hood;
            if kept.cell < 1.0 {
                let (_, _, bw, _) = hood.at;
                let mut reach = 0.0f32;
                for fy in HOOD..HOOD + ch {
                    for fx in HOOD..HOOD + cw {
                        let p = hood.px[(fy * bw + fx) as usize];
                        reach = reach.max((lum(p.lit) - lum(p.base)).abs());
                    }
                }
                let share = risen(kept.cell, reach);
                if share < 1.0 {
                    dims.push(cell_dim(share));
                    cell_kept = share;
                    cell_carried = true;
                }
            }
            let mut floor = std::mem::take(&mut self.thin_floor);
            floor.clear();
            floor.resize(n, 1.0);
            let mut any = false;
            for (j, &was) in kept.px.iter().enumerate() {
                if was >= 1.0 {
                    continue;
                }
                let p = hood.px[j];
                let share = risen(was, (lum(p.lit) - lum(p.base)).abs());
                if share < 1.0 {
                    // Kept as it rose, and laid at the sixty-fourth above, so a run of pixels
                    // shares one dim and the release is never slowed by the step.
                    px_kept[j] = share;
                    floor[j] = (share * 64.0).ceil() / 64.0;
                    carried[j] = true;
                    any = true;
                }
            }
            if any {
                hard_runs(hood, &floor, &mut dims);
            }
            self.thin_floor = floor;
        }
        let mut round = 0;
        let mut carrying = !dims.is_empty();
        while carrying || (round < HOLD_ROUNDS && !self.thin_hood.reads_all(end, page, ring)) {
            if !carrying {
                round += 1;
                dims.clear();
                if !reads(end, page, cell, need) {
                    let (mut lo, mut hi) = (0.0f32, 1.0f32);
                    let mut k = 0;
                    while k < SHARE_STEPS {
                        let mid = 0.5 * (lo + hi);
                        if reads(end, page, held(under, cell, mid), need) {
                            lo = mid;
                        } else {
                            hi = mid;
                        }
                        k += 1;
                    }
                    dims.push(cell_dim(lo));
                    cell_carried = false;
                }
                if ring {
                    self.thin_ring_dims(end, &mut dims);
                }
                if dims.is_empty() {
                    break;
                }
            }
            for d in &dims {
                print = aterm_hash::fnv_mix_u64(
                    print ^ 0x7469_6e63_6172_6574,
                    u64::from(d.share.to_bits())
                        ^ (u64::from(d.x0.to_bits()) << 32)
                        ^ u64::from(d.y0.to_bits()).rotate_left(17),
                );
                // The carried hold's shares are kept as they rose.
                if carrying {
                    continue;
                }
                if !d.hard {
                    cell_kept *= d.share;
                    continue;
                }
                let (bx0, by0, bw, bh) = self.thin_hood.at;
                let (x0, y0) = ((d.x0 as i32 - bx0).max(0), (d.y0 as i32 - by0).max(0));
                let (x1, y1) = ((d.x1 as i32 - bx0).min(bw), (d.y1 as i32 - by0).min(bh));
                for y in y0..y1 {
                    for x in x0..x1 {
                        let j = (y * bw + x) as usize;
                        px_kept[j] *= d.share;
                        carried[j] = false;
                    }
                }
            }
            carrying = false;
            // The cell's soft dim, and a ring run over a cell whose ink the frame cannot raster or
            // over the trail's rail or a page shade, cut the streams (whose cut leaves the rail
            // and the shade whole: dark bar steam's caret read 3.58 where the light over the rail
            // was left whole too, 4.35 cut); every other ring run is laid over the frame's finished light as
            // the page, so no light beside it is cut and laid again (a whole piece's ends fitted
            // anew lowered the `w` beside glass dark typing's bar from 4.60 to 4.58).
            let hood = &self.thin_hood;
            #[cfg(test)]
            let cut = self.thin_ring_cut;
            #[cfg(not(test))]
            let cut = false;
            let laid_over = |d: &Dim| d.hard && !cut && !hood.cut_at(d);
            let ledger = &mut self.ink_ledger;
            ledger.grid = (ox as f32, oy as f32, cw as f32, ch as f32);
            ledger.dims.clear();
            ledger.dims.extend(dims.iter().filter(|d| !laid_over(d)));
            if !ledger.dims.is_empty() {
                ledger.owned_in_strip = true;
                ledger.guard_mask_stream(&mut self.under_out, &mut self.under_ramps, true);
                ledger.guard_mask_stream(over, &mut self.out_ramps, false);
                ledger.guard_mask_halos(&mut self.halo_out);
                ledger.owned_in_strip = false;
            }
            ledger.dims.clear();
            ledger.dims.extend(dims.iter().filter(|d| laid_over(d)));
            if !ledger.dims.is_empty() {
                ledger.guard_mask_halos(&mut self.halo_out);
                for d in &ledger.dims {
                    hood.lay_over(d, (geom, over));
                }
            }
            ledger.dims.clear();
            cell = self.thin_caret_hood(over, caret, (geom, trail), false);
        }
        self.thin_caret_read = Some((caret, cell));
        let held_any = cell_kept < 1.0 || px_kept.iter().any(|&k| k < 1.0);
        if held_any {
            kept.at = Some((now, caret, self.thin_hood.at));
            kept.cell = cell_kept;
            std::mem::swap(&mut kept.px, &mut px_kept);
            kept.releasing = cell_carried || carried.iter().any(|&c| c);
        } else {
            kept.forget();
        }
        self.thin_held = kept;
        self.thin_px_kept = px_kept;
        self.thin_carried = carried;
        if print == 0 {
            return 0;
        }
        aterm_hash::fnv_mix_u64(
            print,
            (u64::from(caret.cell.0) << 48) ^ (u64::from(caret.cell.1) << 32) ^ u64::from(cell),
        )
    }

    /// The colour a thin caret `fill` is drawn in at `now` on the light
    /// [`Self::hold_light_for_thin_caret`] read round it this frame: the least lift along its
    /// lightness path that reads [`THIN_CARET_MIN`] against its page, its cell and its ring, or
    /// its path's end where none does (Classic's spectrum face, whose light is never held). A
    /// lift is taken at once and given back no faster than [`RELEASE_PER_S`]: where the last
    /// frame's was more, the bar is drawn on its path at the luminance the release allows, if
    /// that reads too. `None` where the caret is drawn as it is.
    #[must_use]
    pub(crate) fn thin_caret_fill(&mut self, fill: u32, now: Instant) -> Option<u32> {
        let Some((caret, _)) = self.thin_caret_read else {
            self.thin_lift = None;
            return None;
        };
        let page = caret.page & 0x00FF_FFFF;
        let fill = fill & 0x00FF_FFFF;
        let dark = aterm_render::theme_is_dark(page);
        let reach = caret.reach();
        let ring = self.thin_ring_on();
        let hood = &self.thin_hood;
        let (m, target) = least_lift(fill, dark, reach, |c| hood.reads_all(c, page, ring))
            .unwrap_or_else(|| (reach, lifted(fill, dark, reach)));
        let mut drawn = target;
        // The luminance the release allows, kept as it falls (a colour's steps along its path
        // can be coarser than a frame's release).
        let mut held_lum = None;
        #[cfg(any(test, debug_assertions))]
        let release = !self.thin_release_off;
        #[cfg(not(any(test, debug_assertions)))]
        let release = true;
        let lt = lum(target);
        if release && let Some(prev) = self.thin_lift.filter(|l| l.dark == dark) {
            let dt = now.saturating_duration_since(prev.at).as_secs_f32();
            let bound = if dark {
                prev.lum - RELEASE_PER_S * dt
            } else {
                prev.lum + RELEASE_PER_S * dt
            };
            if (dark && lt < bound) || (!dark && lt > bound) {
                // The least lift past the target whose luminance the release allows.
                let (mut lo, mut hi) = (m, reach);
                let past = |c: u32| {
                    if dark {
                        lum(c) >= bound
                    } else {
                        lum(c) <= bound
                    }
                };
                if past(lifted(fill, dark, hi)) {
                    let mut k = 0;
                    while k < 12 {
                        let mid = 0.5 * (lo + hi);
                        if past(lifted(fill, dark, mid)) {
                            hi = mid;
                        } else {
                            lo = mid;
                        }
                        k += 1;
                    }
                    let c = lifted(fill, dark, hi);
                    if hood.reads_all(c, page, ring) {
                        drawn = c;
                        held_lum = Some(bound);
                    }
                }
            }
        }
        self.thin_lift = Some(ThinLift {
            lum: held_lum.unwrap_or_else(|| lum(drawn)),
            at: now,
            dark,
            releasing: drawn != target,
        });
        (drawn != fill).then_some(drawn)
    }
}

#[cfg(test)]
#[path = "thin_caret_tests.rs"]
mod tests;
