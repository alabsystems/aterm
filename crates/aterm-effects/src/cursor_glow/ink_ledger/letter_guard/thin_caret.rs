// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A THIN CARET READS AGAINST ITS CELL (composed r1's judges, O2(a); the caret's legibility
//! ruling: the caret reads 4.5:1 or more against its cell, and light is not colour).
//!
//! A bar or underline caret is two pixels or so of the caret's colour at the edge of a cell
//! the trail lights: the head glow, the halos, the momentum halo and the Return's flash are
//! anchored on it (`GlowConfig::head_dx`), so the light it is read against is the light of its
//! own cell (`trail-contrast`'s thin-caret reader: its line's median against the closer of the
//! page and the median of the cell's other pixels). On GitHub Light the theme's `#0969da` bar,
//! 5.19:1 on the paper, read 3.14:1 in a ribbon's pink and 1.94 in Comet's disc; on the dark
//! page Fire's ember rod read 2.87 in its brown halo and the green bar 1.08 in Laser's flash.
//!
//! The caret's colour is moved in lightness alone ([`lifted`]): each channel by one amount
//! toward white on a dark page and toward black on paper until the first meets its end, then
//! on toward white or black, until its line reads [`THIN_CARET_MIN`] against the page and
//! against the median of the light the frame lays over the rest of its cell (the reader's
//! measure, read off the frame's streams by [`crate::cursor_fx::light_at`]). The theme's caret
//! colour is no pin: its hue is kept while it can be, and its light is the caret's to choose. A
//! caret colour the user or a program pinned (config, OSC 12) goes along the first leg alone,
//! which keeps its hue and chroma.
//!
//! Where white (dark) or black (paper), or a pinned colour at its first leg's end, still reads
//! under the bar there (Laser's lavender head
//! `#ccb3ff` at 0.49 luminance: white reads 1.94:1 on it), lightness cannot do it, and the
//! light in the caret's cell is held to the largest share of itself the caret's end colour reads
//! [`THIN_CARET_MIN`] against, as the letter guard holds the light on a letter: the cell and
//! its ring at that share, easing back to whole over the guard's feather. Nothing is moved: the
//! head stays anchored on the bar and keeps its shape, only its light in the caret's own cell is
//! less. Classic's spectrum face on a dark theme keeps its bytes (the v0.28 ruling): its caret
//! is lifted, never its light. Control: [`CursorGlow::set_thin_caret_read`].

use super::*;

/// The least a thin caret's line reads against the page and the light of its cell
/// ([`CursorGlow::hold_light_for_thin_caret`]): the legibility bar and a guard for the post
/// passes (the bloom and the crown light the cell past what the frame's streams lay), as the
/// chosen bolt's ([`crate::cursor_fx::CHOSEN_BOLT_HALO_MIN`]).
pub(crate) const THIN_CARET_MIN: f32 = crate::cursor_fx::CARET_PAGE_MIN_CONTRAST + 0.6;

/// How many steps along its lightness path a thin caret's colour is tried at ([`lifted_to`]):
/// the path's two legs at a sixty-fourth of each.
const PATH_STEPS: u32 = 128;

/// Bisection steps for the share of the cell's light a held caret keeps.
const SHARE_STEPS: u32 = 8;

/// How many times a held caret's cell is held again on what the frame then lays there
/// ([`CursorGlow::hold_light_for_thin_caret`]).
const HOLD_ROUNDS: u32 = 3;

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
}

impl ThinCaret {
    /// The caret's line in its cell, `(x0, y0, x1, y1)` from the cell's top left, end exclusive,
    /// as the renderer draws it (`aterm_render::for_each_cursor_rect`).
    #[inline]
    const fn line(self, cw: i32, ch: i32) -> (i32, i32, i32, i32) {
        if self.bar {
            let t = clamp_i32(cw / 8, 2, cw);
            (0, 0, t, ch)
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

/// Whether `c` reads [`THIN_CARET_MIN`] against `page` and against `ground`.
#[inline]
fn reads(c: u32, page: u32, ground: u32) -> bool {
    use crate::color_math::contrast_ratio;
    contrast_ratio(c, page) >= THIN_CARET_MIN && contrast_ratio(c, ground) >= THIN_CARET_MIN
}

/// The least lift of `fill` along its path ([`lifted`]) up to `reach` that reads against
/// `page` and `ground` ([`reads`]), or `None` where none does. A fill that reads already is
/// `fill` itself.
#[inline]
pub(crate) fn lifted_to(fill: u32, page: u32, ground: u32, reach: f32) -> Option<u32> {
    let fill = fill & 0x00FF_FFFF;
    if reads(fill, page, ground) {
        return Some(fill);
    }
    let dark = aterm_render::theme_is_dark(page);
    // Not one bisection: on its way toward white a bar under a brighter ground reads it less
    // before it reads it more.
    let mut k = 1;
    while k <= PATH_STEPS {
        let c = lifted(fill, dark, reach * k as f32 / PATH_STEPS as f32);
        if reads(c, page, ground) {
            return Some(c);
        }
        k += 1;
    }
    None
}

/// `page` lit by `ground`'s light at `share` of it, channel by channel.
#[inline]
fn held(page: u32, ground: u32, share: f32) -> u32 {
    crate::effect_util::lerp_rgb(page, ground, share)
}

impl CursorGlow {
    /// A test's negative control: `on` (as shipped), a bar or underline caret reads against the
    /// light of its cell ([`Self::hold_light_for_thin_caret`]); off, it is drawn in its own
    /// colour on whatever light the trail lays there.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_thin_caret_read(&mut self, on: bool) {
        self.thin_caret_free = !on;
    }

    /// The median of the light the frame lays over `caret`'s cell outside its line, as the
    /// thin-caret reader takes it: every pixel of the cell (every other one read a gradient's
    /// median a few levels off the reader's), each composited on `under` (the cell's ground) in
    /// the renderer's order: the under stream, the fire's field ([`fire_over`]), the over
    /// stream (`over`) and the halos ([`crate::cursor_fx::light_at`]). `None` where no light
    /// reaches the cell. (Read without the fire, Fire's tongues in the caret's cell left a
    /// `#49331f` ground read `#2d1e1b`, composed r2.)
    fn thin_caret_ground(
        &mut self,
        over: &[GlowQuad],
        (caret, under): (ThinCaret, u32),
        geom: Geom,
    ) -> Option<u32> {
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        if cw <= 0 || ch <= 0 {
            return None;
        }
        let x0 = i32::from(geom.origin_x) + i32::from(caret.cell.1) * cw;
        let y0 = i32::from(geom.origin_y) + i32::from(caret.cell.0) * ch;
        let meets = |x: u16, y: u16, w: u16, h: u16| {
            let (x, y, w, h) = (i32::from(x), i32::from(y), i32::from(w), i32::from(h));
            w > 0 && h > 0 && x < x0 + cw && x + w > x0 && y < y0 + ch && y + h > y0
        };
        let [mut below, mut above] = std::mem::take(&mut self.thin_caret_laid);
        below.clear();
        above.clear();
        self.under_ramps.expand_into(&self.under_out, &mut below);
        self.out_ramps.expand_into(over, &mut above);
        below.retain(|q| meets(q.x, q.y, q.w, q.h));
        above.retain(|q| meets(q.x, q.y, q.w, q.h));
        let halos = &self.halo_out;
        let patches = &self.patch_out;
        let any_halo = halos.iter().any(|h| meets(h.x, h.y, h.w, h.h));
        let any_fire = patches.iter().any(|q| meets(q.x, q.y, q.w, q.h));
        if below.is_empty() && above.is_empty() && !any_halo && !any_fire {
            self.thin_caret_laid = [below, above];
            return None;
        }
        let under = under & 0x00FF_FFFF;
        let mut px = std::mem::take(&mut self.thin_caret_px);
        px.clear();
        for fy in 0..ch {
            for fx in 0..cw {
                px.push(crate::cursor_fx::light_at(
                    under,
                    (&below, &[]),
                    (x0 + fx, y0 + fy),
                ));
            }
        }
        if any_fire {
            fire_over(&mut px, patches, (x0, y0, cw, ch), i32::from(geom.origin_x));
        }
        let (lx0, ly0, lx1, ly1) = caret.line(cw, ch);
        let mut chans: [Vec<u8>; 3] = std::mem::take(&mut self.thin_caret_chans);
        for c in &mut chans {
            c.clear();
        }
        for fy in 0..ch {
            for fx in 0..cw {
                if fx >= lx0 && fx < lx1 && fy >= ly0 && fy < ly1 {
                    continue;
                }
                let at = px[(fy * cw + fx) as usize];
                let lit = crate::cursor_fx::light_at(at, (&above, halos), (x0 + fx, y0 + fy));
                chans[0].push(((lit >> 16) & 0xff) as u8);
                chans[1].push(((lit >> 8) & 0xff) as u8);
                chans[2].push((lit & 0xff) as u8);
            }
        }
        let mut ground = 0u32;
        let mut empty = false;
        for (i, c) in chans.iter_mut().enumerate() {
            if c.is_empty() {
                empty = true;
                break;
            }
            c.sort_unstable();
            ground |= u32::from(c[c.len() / 2]) << (16 - 8 * i as u32);
        }
        self.thin_caret_laid = [below, above];
        self.thin_caret_px = px;
        self.thin_caret_chans = chans;
        if empty || ground == under {
            None
        } else {
            Some(ground)
        }
    }

    /// A THIN CARET READS AGAINST ITS CELL ([`self`]): `caret` and the ground under the frame's
    /// lights in its cell (its page, or a trail cell's blend over it). Where white (dark) or
    /// black (paper) reads under [`THIN_CARET_MIN`] against the light the frame lays there, it is
    /// held to the largest share of itself the end colour reads the bar against (the cell and
    /// the letter guard's ring, easing back over its feather); the ground the caret is then
    /// read against is kept for [`Self::thin_caret_fill`]. Run on the frame's finished streams,
    /// after the letter guard. The frame's print of what it held.
    pub(crate) fn hold_light_for_thin_caret(
        &mut self,
        over: &mut Vec<GlowQuad>,
        caret: Option<(ThinCaret, u32)>,
        geom: Geom,
        style: GlowStyle,
    ) -> u64 {
        self.thin_caret_read = None;
        #[cfg(any(test, debug_assertions))]
        if self.thin_caret_free {
            return 0;
        }
        let Some((caret, under)) = caret else {
            return 0;
        };
        let under = under & 0x00FF_FFFF;
        let Some(ground) = self.thin_caret_ground(over, (caret, under), geom) else {
            // No light over the cell: the caret reads against the trail's cell under it alone.
            if under != caret.page & 0x00FF_FFFF {
                self.thin_caret_read = Some((caret, under));
            }
            return 0;
        };
        let page = caret.page & 0x00FF_FFFF;
        let dark = aterm_render::theme_is_dark(page);
        let end = match caret.pin {
            Some(pin) => lifted(pin, dark, 1.0),
            None if dark => 0x00FF_FFFF,
            None => 0,
        };
        if reads(end, page, ground) || self.classic_keeps_its_bytes(style) {
            self.thin_caret_read = Some((caret, ground));
            return 0;
        }
        // Hold the cell's light to the largest share of itself the end colour reads the bar
        // against, then read what the frame now lays there: a light past its channels' clip,
        // or one the guard never cuts (a light-owned quad, a foot rail), dims less than its
        // share (Laser's Return flash held to a third read `#8a6ace`, not the `#6d6d71` a
        // share of the clipped light foretold, composed r2), so the hold is read again.
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let (cx0, cy0) = (
            ox + i32::from(caret.cell.1) * cw,
            oy + i32::from(caret.cell.0) * ch,
        );
        let mut ground = ground;
        let mut print = 0u64;
        let mut round = 0;
        while round < HOLD_ROUNDS && !reads(end, page, ground) {
            round += 1;
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            let mut k = 0;
            while k < SHARE_STEPS {
                let mid = 0.5 * (lo + hi);
                if reads(end, page, held(under, ground, mid)) {
                    lo = mid;
                } else {
                    hi = mid;
                }
                k += 1;
            }
            let ledger = &mut self.ink_ledger;
            ledger.grid = (ox as f32, oy as f32, cw as f32, ch as f32);
            ledger.dims.clear();
            ledger.dims.push(Dim {
                x0: (cx0 - RING) as f32,
                y0: (cy0 - RING) as f32,
                x1: (cx0 + cw + RING) as f32,
                y1: (cy0 + ch + RING) as f32,
                share: lo,
            });
            ledger.guard_mask_stream(&mut self.under_out, &mut self.under_ramps, true);
            ledger.guard_mask_stream(over, &mut self.out_ramps, false);
            ledger.guard_mask_halos(&mut self.halo_out);
            ledger.dims.clear();
            print = aterm_hash::fnv_mix_u64(print ^ 0x7469_6e63_6172_6574, u64::from(lo.to_bits()));
            match self.thin_caret_ground(over, (caret, under), geom) {
                Some(now) if now != ground => ground = now,
                Some(_) => break,
                None => {
                    ground = under;
                    break;
                }
            }
        }
        self.thin_caret_read = Some((caret, ground));
        if print == 0 {
            return 0;
        }
        aterm_hash::fnv_mix_u64(
            print,
            (u64::from(caret.cell.0) << 48) ^ (u64::from(caret.cell.1) << 32) ^ u64::from(ground),
        )
    }

    /// The colour a thin caret `fill` is drawn in on the light [`Self::hold_light_for_thin_caret`]
    /// read in its cell this frame: the least lift along its lightness path ([`lifted_to`]) that
    /// reads [`THIN_CARET_MIN`] there, or its path's end where none does (Classic's spectrum
    /// face, whose light is never held). `None` where its cell holds no light, or the caret
    /// reads as it is.
    #[must_use]
    pub(crate) fn thin_caret_fill(&self, fill: u32) -> Option<u32> {
        let (caret, ground) = self.thin_caret_read?;
        let page = caret.page & 0x00FF_FFFF;
        let fill = fill & 0x00FF_FFFF;
        let lit = lifted_to(fill, page, ground, caret.reach())
            .unwrap_or_else(|| lifted(fill, aterm_render::theme_is_dark(page), caret.reach()));
        (lit != fill).then_some(lit)
    }
}

#[cfg(test)]
#[path = "thin_caret_tests.rs"]
mod tests;
