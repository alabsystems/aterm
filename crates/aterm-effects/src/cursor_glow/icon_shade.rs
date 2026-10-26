// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WORD ICONS' SHADE: no light of the ribbon's frame lies on a word icon.
//!
//! The word pass draws its icons (a `dog`'s or a `fox`'s face over its word)
//! after the trail has drawn the frame, so the keep-out the sky and the
//! ribbon draw by ([`rk::companion::KeepOut`]) is the last frame's icons. An
//! icon new to the frame (a word a Return completed, a peek rising into a
//! taller box) found the sky's stars and the Return's flare on it: pink
//! sparkles on the fox's cheek and chin on paper, its white chin washed lilac
//! on dark, in every ribbon's `enter-bottom` capture. Once the word pass has
//! named the frame's icons, [`CursorGlow::shade_word_icons`] lifts what the
//! trail drew on them, as if each icon were drawn over the light: a star
//! whose arm reaches an icon's box goes whole, light by light
//! ([`rk::stardust::SkyMark`]), and every other light loses the pixels of the icon's drawn rows (each
//! row's span from its first to its last texel at least half opaque, read
//! from the sprite's own art, inside its visible window), keeping everything
//! beside, above and below the face. A cut to the box's foot instead carved
//! a hard notch out of the wash over the box's transparent margin. An icon
//! with no art behind it is cut as its whole visible box.
//!
//! Every style's light goes: a fire's tongues and embers stood over the
//! fox's chin on paper and beside its cheek on dark (looks7 M5), its
//! flames' field patches cut as the quads are, the field anchored to the
//! window so a piece keeps its pixels.
//!
//! It shades the streams the host draws, not the glow's own: a capture, a
//! split frame and the web host hand the host the trail's streams before
//! their word pass runs, so a shade of the glow's buffers reached only the
//! single-grid present. It shades once per word pass. A frame with no icon
//! is untouched, and one whose keep-out already held every light off its
//! icons finds nothing to lift.

use super::*;
use aterm_core::render::{FxArt, FxTexels, GlowEnd, GlowRamp, GlowRamps, Laid};
use aterm_render::sprites::FreeSprite;
use std::sync::Arc;

/// A window-pixel box `(x0, x1, y0, y1)`, right and bottom exclusive.
type PxBox = (i32, i32, i32, i32);

/// A glow stream the shade cuts: its quads and its ramps beside them.
pub type ShadeStream<'a> = (&'a mut Vec<GlowQuad>, &'a mut GlowRamps);

/// The least art alpha that counts a texel as the icon's drawn face: the
/// middle of its anti-aliased edge, so a light ends where the edge reads.
const ICON_ALPHA_EDGE: u8 = 128;

/// A word icon's sprite ([`FreeSprite`]): its art and the whole art's
/// destination box in grid pixels, for its drawn rows.
#[derive(Clone, Debug)]
pub(super) struct IconSrc {
    art: Arc<FxArt>,
    dest: PxBox,
    flip_x: bool,
}

impl CursorGlow {
    /// The word pass's icons, its under-text sprites in grid pixels: the
    /// next frame's keep-out ([`Self::note_word_icons`]), each its visible
    /// box, and this frame's shade, which cuts each by its art's drawn rows.
    pub fn note_word_icon_sprites<'a>(
        &mut self,
        sprites: impl IntoIterator<Item = &'a FreeSprite>,
    ) {
        self.word_icons.clear();
        self.word_icon_src.clear();
        for s in sprites {
            let v = s.visible();
            self.word_icons.push((v.x0, v.x1, v.y0, v.y1));
            self.word_icon_src.push(IconSrc {
                art: Arc::clone(&s.art),
                dest: (s.x, s.x + i32::from(s.w), s.y, s.y + i32::from(s.h)),
                flip_x: s.flip_x,
            });
        }
        self.word_icons_unshaded = true;
    }

    /// The word icons the last word pass named, grid pixels `(x0, x1, y0, y1)`
    /// ([`Self::note_word_icon_sprites`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn word_icon_boxes(&self) -> &[(i32, i32, i32, i32)] {
        self.word_icons.as_slice()
    }

    /// Whether a speck centred at window pixel `(x, y)` reaching `reach` px
    /// touches a word icon the last word pass drew: a fire's ember keeps off
    /// it whole, as the sky's stars keep off it ([`rk::companion::KeepOut`]),
    /// where the shade would leave a crescent of it.
    #[inline]
    pub(super) const fn near_word_icon(&self, (x, y): (f32, f32), reach: f32) -> bool {
        let Some((ox, oy, _)) = self.keep_geom else {
            return false;
        };
        let (x, y) = (x - ox as f32, y - oy as f32);
        let icons = self.word_icons.as_slice();
        let mut i = 0;
        while i < icons.len() {
            let (x0, x1, y0, y1) = icons[i];
            if x + reach >= x0 as f32
                && x - reach < x1 as f32
                && y + reach >= y0 as f32
                && y - reach < y1 as f32
            {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Lift this frame's light off the word icons the word pass just drew
    /// ([`Self::note_word_icon_sprites`]), in the streams the host draws:
    /// `over` the over-text stream, `under` the under-text stream, `halos`
    /// the halos and `patches` a fire's field patches; an icon noted with no
    /// art ([`Self::note_word_icons`]) is cut as its box. A stream's ramps
    /// are cut in place: the rows an icon reaches as the quads they stand
    /// for would be, the rest kept as ramps. Each word pass's icons shade
    /// once. Returns whether anything was lifted.
    pub fn shade_word_icons(
        &mut self,
        (over, under, halos, patches): (
            ShadeStream<'_>,
            ShadeStream<'_>,
            &mut Vec<RainHalo>,
            &mut Vec<FirePatch>,
        ),
    ) -> bool {
        if !std::mem::take(&mut self.word_icons_unshaded) || self.word_icons.is_empty() {
            return false;
        }
        let Some((ox, oy, _)) = self.keep_geom else {
            return false;
        };
        let (over, under) = ((over.0, over.1), (under.0, under.1));
        let mut lifted = self.v2.engaged()
            && lift_stars(
                (self.v2.sky_marks(), self.v2.sky_mark_lights()),
                &self.word_icons,
                (ox, oy),
                (
                    (&mut *over.0, &mut *over.1),
                    (&mut *under.0, &mut *under.1),
                    &mut *halos,
                ),
            );
        let mut patch_pieces = std::mem::take(&mut self.icon_patches);
        let mut bands = std::mem::take(&mut self.icon_scratch);
        let mut pieces = std::mem::take(&mut self.icon_quads);
        let mut piece_ramps = std::mem::take(&mut self.icon_ramps);
        let mut halo_pieces = std::mem::take(&mut self.icon_halos);
        let mut cut = std::mem::take(&mut self.icon_cut);
        let mut i = 0;
        while i < self.word_icons.len() {
            let icon = self.word_icons[i];
            let icon = (icon.0 + ox, icon.1 + ox, icon.2 + oy, icon.3 + oy);
            if icon.0 < icon.1 && icon.2 < icon.3 {
                drawn_rows(icon, self.word_icon_src.get(i), (ox, oy), &mut bands);
                lifted |= shade_stream(
                    (&mut *over.0, &mut *over.1),
                    &bands,
                    (&mut pieces, &mut piece_ramps, &mut cut),
                );
                lifted |= shade_stream(
                    (&mut *under.0, &mut *under.1),
                    &bands,
                    (&mut pieces, &mut piece_ramps, &mut cut),
                );
                lifted |= shade_halos(halos, &bands, (&mut halo_pieces, &mut cut));
                lifted |= shade_patches(patches, &bands, (&mut patch_pieces, &mut cut));
            }
            i += 1;
        }
        self.icon_scratch = bands;
        self.icon_quads = pieces;
        self.icon_ramps = piece_ramps;
        self.icon_halos = halo_pieces;
        self.icon_patches = patch_pieces;
        self.icon_cut = cut;
        lifted
    }
}

/// Lift whole every star whose arm reaches an icon's box (`icons` in grid
/// pixels, `origin` the grid's in the window), light by light: a light goes
/// when it lies in the star's box and is one the star painted
/// ([`rk::stardust::SkyMark::lights`]). Returns whether any went.
fn lift_stars(
    (marks, keys): (&[rk::stardust::SkyMark], &[u64]),
    icons: &[PxBox],
    (ox, oy): (i32, i32),
    (over, under, halos): (ShadeStream<'_>, ShadeStream<'_>, &mut Vec<RainHalo>),
) -> bool {
    if marks.is_empty() || icons.is_empty() {
        return false;
    }
    #[inline]
    const fn inside(b: PxBox, rect: PxBox) -> bool {
        b.0 >= rect.0 && b.1 <= rect.1 && b.2 >= rect.2 && b.3 <= rect.3
    }
    let mut lifted = false;
    let mut mi = 0;
    while mi < marks.len() {
        let mark = &marks[mi];
        mi += 1;
        let r = mark.reach / 2;
        let (x, y) = (mark.at.0 - ox, mark.at.1 - oy);
        let mut hit = false;
        let mut ii = 0;
        while ii < icons.len() {
            let (x0, x1, y0, y1) = icons[ii];
            if x + r >= x0 && x - r < x1 && y + r >= y0 && y - r < y1 {
                hit = true;
                break;
            }
            ii += 1;
        }
        if !hit {
            continue;
        }
        let Some(own) = keys.get(mark.lights.0 as usize..mark.lights.1 as usize) else {
            continue;
        };
        if own.is_empty() {
            continue;
        }
        let rect = mark.rect;
        let star_quad = |q: &GlowQuad| {
            inside(quad_box(q), rect) && own.contains(&rk::stardust::sky_quad_key(q))
        };
        let (nq, nu, nh) = (over.0.len(), under.0.len(), halos.len());
        // A star's lights are quads of their own, never a ramp's rows.
        if nq > 0 {
            over.1.retain_quads(over.0, |q| !star_quad(q));
        }
        if nu > 0 {
            under.1.retain_quads(under.0, |q| !star_quad(q));
        }
        if nh > 0 {
            halos.retain(|h| {
                !(inside(halo_box(h), rect) && own.contains(&rk::stardust::sky_halo_key(h)))
            });
        }
        lifted |= over.0.len() != nq || under.0.len() != nu || halos.len() != nh;
    }
    lifted
}

#[inline]
const fn quad_box(q: &GlowQuad) -> PxBox {
    let (x, y) = (q.x as i32, q.y as i32);
    (x, x + q.w as i32, y, y + q.h as i32)
}

#[inline]
const fn ramp_box(r: &GlowRamp) -> PxBox {
    let (x, y) = (r.x as i32, r.y as i32);
    (x, x + r.w as i32, y, y + r.h as i32)
}

#[inline]
const fn halo_box(h: &RainHalo) -> PxBox {
    let (x, y) = (h.x as i32, h.y as i32);
    (x, x + h.w as i32, y, y + h.h as i32)
}

/// The art index the stamp shows at offset `l` of a `dest`-pixel box over
/// `art` texels, mirrored for `flip`: `⌊l·art/dest⌋` (fx_raster's column
/// table and row walk, the GPU sprite fragment's `l·w/dw`), mirrored as
/// texel `art − 1 −` that. Read at the texel centres instead, a scaled
/// sprite's silhouette sat a texel off what it drew: the companion's paw
/// and chin, drawn from the art row above the one read, peeked through the
/// page veil as specks between the letters (looks7 round 2).
#[inline]
const fn stamp_texel(l: i32, art: i32, dest: i32, flip: bool) -> i32 {
    let t = l * art / dest;
    if flip { art - 1 - t } else { t }
}

/// The icon's drawn rows as bands `(x0, x1, y0, y1)` in window pixels, top
/// to bottom, rows of one span merged, inside `icon` (its visible box in
/// window pixels): each row spans its first to its last texel at least
/// [`ICON_ALPHA_EDGE`] opaque (an Index8 texel: any ink), each pixel's the
/// texel the stamp shows there ([`stamp_texel`] over the whole art's box,
/// `origin` the grid's in the window). With no art, the box.
fn drawn_rows(icon: PxBox, src: Option<&IconSrc>, origin: (i32, i32), out: &mut Vec<PxBox>) {
    drawn_rows_by(icon, src, origin, out, (stamp_texel, ICON_ALPHA_EDGE));
}

/// [`drawn_rows`], each pixel's texel read by `texel` and counted drawn
/// from `edge` alpha up.
fn drawn_rows_by(
    icon: PxBox,
    src: Option<&IconSrc>,
    origin: (i32, i32),
    out: &mut Vec<PxBox>,
    (texel, edge): (impl Fn(i32, i32, i32, bool) -> i32, u8),
) {
    let Some(s) = src else {
        out.clear();
        out.push(icon);
        return;
    };
    drawn_rows_ref(
        icon,
        Some((&s.art, s.dest, s.flip_x)),
        origin,
        out,
        (texel, edge),
    );
}

fn drawn_rows_ref(
    icon: PxBox,
    src: Option<(&FxArt, PxBox, bool)>,
    (ox, oy): (i32, i32),
    out: &mut Vec<PxBox>,
    (texel, edge): (impl Fn(i32, i32, i32, bool) -> i32, u8),
) {
    #[inline]
    const fn texel_drawn(kind: FxTexels, edge: u8, v: u32) -> bool {
        match kind {
            FxTexels::Index8 => v != 0,
            FxTexels::Rgba8Premul | FxTexels::Rgba8Straight => v >> 24 >= edge as u32,
        }
    }
    out.clear();
    let Some((art, dest, flip_x)) = src else {
        out.push(icon);
        return;
    };
    if art.is_empty() {
        out.push(icon);
        return;
    }
    let (bx0, by0) = (dest.0 + ox, dest.2 + oy);
    let (w, h) = (dest.1 - dest.0, dest.3 - dest.2);
    if w <= 0 || h <= 0 {
        out.push(icon);
        return;
    }
    let (aw, ah) = (i32::from(art.w()), i32::from(art.h()));
    let (aw_max, ah_max) = (aw - 1, ah - 1);
    let kind = art.texels();
    let (lo, hi) = (icon.0.max(bx0), icon.1.min(bx0 + w));
    let (y_lo, y_hi) = (icon.2.max(by0), icon.3.min(by0 + h));
    if lo >= hi || y_lo >= y_hi {
        return;
    }
    let mut prev_sy: Option<(usize, Option<(i32, i32)>)> = None;
    let mut y = y_lo;
    while y < y_hi {
        let dy = y - by0;
        let sy = texel(dy, ah, h, false).clamp(0, ah_max) as usize;
        let span = match prev_sy {
            Some((ps, span)) if ps == sy => span,
            _ => {
                let mut first = lo;
                while first < hi {
                    let sx = texel(first - bx0, aw, w, flip_x).clamp(0, aw_max) as usize;
                    if texel_drawn(kind, edge, art.texel(sx, sy)) {
                        break;
                    }
                    first += 1;
                }
                let span = if first < hi {
                    let mut last = hi - 1;
                    while last > first {
                        let sx = texel(last - bx0, aw, w, flip_x).clamp(0, aw_max) as usize;
                        if texel_drawn(kind, edge, art.texel(sx, sy)) {
                            break;
                        }
                        last -= 1;
                    }
                    Some((first, last + 1))
                } else {
                    None
                };
                prev_sy = Some((sy, span));
                span
            }
        };
        if let Some((x0, x1)) = span {
            match out.last_mut() {
                Some(b) if b.3 == y && b.0 == x0 && b.1 == x1 => b.3 = y + 1,
                _ => out.push((x0, x1, y, y + 1)),
            }
        }
        y += 1;
    }
}

/// The rows `sprite` (grid pixels, the grid at `origin` in the window) draws
/// within `clip` (window pixels), each its drawn texels' span, into `out`,
/// top to bottom: the silhouette the pet's page veil keeps to
/// ([`CursorGlow::veil_pet_letters`]). Every texel the stamp draws counts,
/// its anti-aliased edge with it: counted from the icon's half-opaque edge,
/// a chin's sloping edge (texels a quarter to a half opaque, five pixels
/// past the row's opaque span) lay outside the veil and lit specks between
/// the letters (looks7 round 2). An empty art yields `clip` whole.
pub(super) fn sprite_rows(
    sprite: &FreeSprite,
    clip: PxBox,
    origin: (i32, i32),
    out: &mut Vec<PxBox>,
) {
    let (x, y) = (sprite.x, sprite.y);
    let dest = (x, x + i32::from(sprite.w), y, y + i32::from(sprite.h));
    drawn_rows_ref(
        clip,
        Some((&sprite.art, dest, sprite.flip_x)),
        origin,
        out,
        (stamp_texel, 1),
    );
}

/// The pieces of `rect` outside `bands` (top to bottom, disjoint rows) into
/// `out`, or `false` (and `out` untouched) when no band reaches it: in a
/// band's rows the parts left and right of its span, elsewhere whole slices.
fn cut_rect(rect: PxBox, bands: &[PxBox], out: &mut Vec<PxBox>) -> bool {
    let (x0, x1, y0, y1) = rect;
    if x0 >= x1
        || y0 >= y1
        || bands.is_empty()
        || y1 <= bands[0].2
        || y0 >= bands[bands.len() - 1].3
    {
        return false;
    }
    let mut hit = false;
    let mut y = y0;
    let mut bi = 0;
    while bi < bands.len() {
        let b = bands[bi];
        bi += 1;
        if b.3 <= y0 {
            continue;
        }
        if b.2 >= y1 {
            break;
        }
        if b.0 >= x1 || b.1 <= x0 {
            continue;
        }
        if !hit {
            out.clear();
            hit = true;
        }
        let (by0, by1) = (b.2.max(y0), b.3.min(y1));
        if by0 > y {
            out.push((x0, x1, y, by0));
        }
        if b.0 > x0 {
            out.push((x0, b.0, by0, by1));
        }
        if b.1 < x1 {
            out.push((b.1, x1, by0, by1));
        }
        y = by1;
    }
    if !hit {
        return false;
    }
    if y < y1 {
        out.push((x0, x1, y, y1));
    }
    true
}

/// `a` to `b` at column share `k / w` of a quad's horizontal gradient, per
/// channel (the gradient a sub-quad starting there continues exactly).
#[cfg(test)]
#[inline]
const fn lerp_rgb(a: u32, b: u32, k: i32, w: i32) -> u32 {
    const fn ch(a: u32, b: u32, sh: u32, k: i32, w: i32) -> u32 {
        let (ca, cb) = (((a >> sh) & 0xff) as i32, ((b >> sh) & 0xff) as i32);
        let v = (ca * (w - k) + cb * k + w / 2) / w;
        if v < 0 {
            0
        } else if v > 255 {
            255
        } else {
            v as u32
        }
    }
    let w = if w > 1 { w } else { 1 };
    (ch(a, b, 16, k, w) << 16) | (ch(a, b, 8, k, w) << 8) | ch(a, b, 0, k, w)
}

/// Bounding box `(x0, x1, y0, y1)` of a non-empty sorted `bands` slice.
#[inline]
const fn bands_bounds(bands: &[PxBox]) -> PxBox {
    let (mut x0, mut x1) = (i32::MAX, i32::MIN);
    let mut i = 0;
    while i < bands.len() {
        let b = bands[i];
        if b.0 < x0 {
            x0 = b.0;
        }
        if b.1 > x1 {
            x1 = b.1;
        }
        i += 1;
    }
    (x0, x1, bands[0].2, bands[bands.len() - 1].3)
}

/// Cut every quad of `quads` the icon's drawn rows reach ([`cut_rect`]); a
/// split keeps its horizontal gradient. Returns whether any was cut.
fn shade_quads(
    quads: &mut Vec<GlowQuad>,
    bands: &[PxBox],
    (pieces, cut): (&mut Vec<GlowQuad>, &mut Vec<PxBox>),
) -> bool {
    if quads.is_empty() || bands.is_empty() {
        return false;
    }
    let bounds = bands_bounds(bands);
    let mut any = false;
    let mut i = 0;
    while i < quads.len() {
        let q = &quads[i];
        if !reached(quad_box(q), bands, bounds, cut) {
            if any {
                pieces.push(*q);
            }
            i += 1;
            continue;
        }
        if !any {
            pieces.clear();
            pieces.extend_from_slice(&quads[..i]);
            any = true;
        }
        push_pieces(q, cut, pieces);
        i += 1;
    }
    if any {
        std::mem::swap(quads, pieces);
    }
    any
}

/// Whether the icon's drawn rows (`bands`, within `bounds`) reach the box
/// `b`, its pieces outside them then in `cut` ([`cut_rect`]).
#[inline]
fn reached(b: PxBox, bands: &[PxBox], bounds: PxBox, cut: &mut Vec<PxBox>) -> bool {
    let (bx0, bx1, by0, by1) = bounds;
    !(b.1 <= bx0 || b.0 >= bx1 || b.3 <= by0 || b.2 >= by1) && cut_rect(b, bands, cut)
}

/// [`shade_quads`] for a stream with ramps: each quad cut as there, and of
/// each ramp the icon's rows reach the rows they reach (a run whole where
/// any of its rows is) cut as the quads they stand for, the rows between
/// and around kept as ramps, all in the stream's order. Returns whether
/// any light was cut; a stream nothing was cut from stays as laid, and
/// costs no copy: the stream is rebuilt only once a light is known reached.
fn shade_stream(
    (quads, ramps): ShadeStream<'_>,
    bands: &[PxBox],
    (pieces, piece_ramps, cut): (&mut Vec<GlowQuad>, &mut GlowRamps, &mut Vec<PxBox>),
) -> bool {
    if ramps.is_empty() {
        return shade_quads(quads, bands, (pieces, cut));
    }
    if bands.is_empty() {
        return false;
    }
    let bounds = bands_bounds(bands);
    let mut any_reached = false;
    let mut qi = 0;
    while qi < quads.len() {
        if reached(quad_box(&quads[qi]), bands, bounds, cut) {
            any_reached = true;
            break;
        }
        qi += 1;
    }
    if !any_reached {
        let mut ri = 0;
        while ri < ramps.items.len() {
            if ramp_reached(&ramps.items[ri], &ramps.ends, (bands, bounds), cut) {
                any_reached = true;
                break;
            }
            ri += 1;
        }
    }
    if !any_reached {
        return false;
    }
    pieces.clear();
    piece_ramps.clear();
    let mut any = false;
    ramps.in_order(quads.len(), |laid| match laid {
        Laid::Quads(run) => {
            let slice = &quads[run];
            let mut si = 0;
            while si < slice.len() {
                let q = &slice[si];
                if reached(quad_box(q), bands, bounds, cut) {
                    push_pieces(q, cut, pieces);
                    any = true;
                } else {
                    pieces.push(*q);
                }
                si += 1;
            }
        }
        Laid::Ramp(k) => {
            any |= shade_ramp(
                &ramps.items[k],
                &ramps.ends,
                (bands, bounds),
                (&mut *pieces, &mut *piece_ramps, &mut *cut),
            );
        }
    });
    if any {
        std::mem::swap(quads, pieces);
        std::mem::swap(ramps, piece_ramps);
    }
    any
}

/// Whether `ramp`'s box lies clear of the icon's drawn rows' `bounds`.
#[inline]
const fn ramp_clear(ramp: &GlowRamp, (bx0, bx1, by0, by1): PxBox) -> bool {
    let rb = ramp_box(ramp);
    rb.1 <= bx0 || rb.0 >= bx1 || rb.3 <= by0 || rb.2 >= by1
}

/// Each span of `ramp`'s rows a band of `bands` reaches, widened to its run
/// where it meets it, spans that touch merged, top to bottom.
fn ramp_spans(ramp: &GlowRamp, bands: &[PxBox], mut each: impl FnMut((u16, u16))) {
    let rb = ramp_box(ramp);
    let run = if ramp.joined > 0 {
        Some((ramp.join, ramp.join + ramp.joined + 1))
    } else {
        None
    };
    let mut span: Option<(u16, u16)> = None;
    let mut bi = 0;
    while bi < bands.len() {
        let b = bands[bi];
        bi += 1;
        if !(b.0 < rb.1 && b.1 > rb.0 && b.3 > rb.2 && b.2 < rb.3) {
            continue;
        }
        let mut s = (
            (b.2 - rb.2).max(0) as u16,
            (b.3 - rb.2).min(i32::from(ramp.h)) as u16,
        );
        if let Some((j0, j1)) = run
            && j0 < s.1
            && s.0 < j1
        {
            s = (s.0.min(j0), s.1.max(j1));
        }
        span = match span {
            Some(p) if s.0 <= p.1 => Some((p.0, p.1.max(s.1))),
            Some(p) => {
                each(p);
                Some(s)
            }
            None => Some(s),
        };
    }
    if let Some(p) = span {
        each(p);
    }
}

/// Whether the icon's drawn rows reach any light of `ramp` (its ends in
/// `ends`): the lights [`shade_ramp`] would cut.
fn ramp_reached(
    ramp: &GlowRamp,
    ends: &[GlowEnd],
    (bands, bounds): (&[PxBox], PxBox),
    cut: &mut Vec<PxBox>,
) -> bool {
    if ramp_clear(ramp, bounds) {
        return false;
    }
    let mut hit = false;
    ramp_spans(ramp, bands, |(a, e)| {
        if hit {
            return;
        }
        let mut r = a;
        while r < e {
            if let Some(q) = ramp.row_quad(ends, r)
                && reached(quad_box(&q), bands, bounds, cut)
            {
                hit = true;
                break;
            }
            r += 1;
        }
    });
    hit
}

/// Lay `ramp` (its ends in `ends`) after `pieces` with the icon's drawn
/// rows cut out of it: each span of rows a band reaches (widened to its run
/// where it meets it, [`ramp_spans`]) as the quads its lights stand for,
/// cut ([`push_pieces`]), and each span between as a ramp of those rows.
/// Returns whether any light was cut.
fn shade_ramp(
    ramp: &GlowRamp,
    ends: &[GlowEnd],
    (bands, bounds): (&[PxBox], PxBox),
    (pieces, piece_ramps, cut): (&mut Vec<GlowQuad>, &mut GlowRamps, &mut Vec<PxBox>),
) -> bool {
    if ramp_clear(ramp, bounds) {
        piece_ramps.copy_rows(ramp, ends, pieces.len(), (0, ramp.h));
        return false;
    }
    let (mut next, mut any) = (0u16, false);
    ramp_spans(ramp, bands, |(a, e)| {
        if a > next {
            piece_ramps.copy_rows(ramp, ends, pieces.len(), (next, a - next));
        }
        let mut r = a;
        while r < e {
            if let Some(q) = ramp.row_quad(ends, r) {
                if reached(quad_box(&q), bands, bounds, cut) {
                    push_pieces(&q, cut, pieces);
                    any = true;
                } else {
                    pieces.push(q);
                }
            }
            r += 1;
        }
        next = e;
    });
    if next < ramp.h {
        piece_ramps.copy_rows(ramp, ends, pieces.len(), (next, ramp.h - next));
    }
    any
}

/// Lay `q`'s pieces outside the icon's drawn rows (`cut`, [`cut_rect`]'s),
/// each continuing the quad's horizontal gradient.
fn push_pieces(q: &GlowQuad, cut: &[PxBox], pieces: &mut Vec<GlowQuad>) {
    #[inline]
    const fn lerp_ch(c0: i32, c1: i32, wk: i32, k: i32, half_w_max: i32, w_max: i32) -> u32 {
        let v = (c0 * wk + c1 * k + half_w_max) / w_max;
        if v < 0 {
            0
        } else if v > 255 {
            255
        } else {
            v as u32
        }
    }
    #[inline]
    const fn lerp_alpha(a0: i32, a1: i32, w: i32, k: i32, half_w: i32, w_max: i32) -> u8 {
        let v = (a0 * (w - k) + a1 * k + half_w) / w_max;
        if v < 0 {
            0
        } else if v > 255 {
            255
        } else {
            v as u8
        }
    }
    let b = quad_box(q);
    let w = i32::from(q.w);
    let w_max = w.max(1);
    let half_w = w / 2;
    let half_w_max = w_max / 2;
    let (a0, a1) = (i32::from(q.alpha), i32::from(q.alpha2));
    let flat_c = q.color == q.color2;
    let flat_a = q.alpha == q.alpha2;
    let flat = flat_c && flat_a;
    let (ca, cb) = (
        (
            ((q.color >> 16) & 0xff) as i32,
            ((q.color >> 8) & 0xff) as i32,
            (q.color & 0xff) as i32,
        ),
        (
            ((q.color2 >> 16) & 0xff) as i32,
            ((q.color2 >> 8) & 0xff) as i32,
            (q.color2 & 0xff) as i32,
        ),
    );
    let mut ci = 0;
    while ci < cut.len() {
        let (xa, xb, ya, yb) = cut[ci];
        ci += 1;
        let (ka, kb) = (xa - b.0, xb - b.0);
        let whole = ka == 0 && kb == w;
        let (color, color2, alpha, alpha2) = if flat || whole {
            (q.color, q.color2, q.alpha, q.alpha2)
        } else {
            let (c1, c2) = if flat_c {
                (q.color, q.color2)
            } else {
                let wka = w_max - ka;
                let wkb = w_max - kb;
                (
                    (lerp_ch(ca.0, cb.0, wka, ka, half_w_max, w_max) << 16)
                        | (lerp_ch(ca.1, cb.1, wka, ka, half_w_max, w_max) << 8)
                        | lerp_ch(ca.2, cb.2, wka, ka, half_w_max, w_max),
                    (lerp_ch(ca.0, cb.0, wkb, kb, half_w_max, w_max) << 16)
                        | (lerp_ch(ca.1, cb.1, wkb, kb, half_w_max, w_max) << 8)
                        | lerp_ch(ca.2, cb.2, wkb, kb, half_w_max, w_max),
                )
            };
            let (al1, al2) = if flat_a {
                (q.alpha, q.alpha2)
            } else {
                (
                    lerp_alpha(a0, a1, w, ka, half_w, w_max),
                    lerp_alpha(a0, a1, w, kb, half_w, w_max),
                )
            };
            (c1, c2, al1, al2)
        };
        pieces.push(GlowQuad {
            x: xa as u16,
            y: ya as u16,
            w: (xb - xa) as u16,
            h: (yb - ya) as u16,
            color,
            alpha,
            color2,
            alpha2,
            ..*q
        });
    }
}

/// Cut every halo of `halos` the icon's drawn rows reach ([`cut_rect`]);
/// each piece keeps its centre and falloff, so what stays lit is unchanged.
fn shade_halos(
    halos: &mut Vec<RainHalo>,
    bands: &[PxBox],
    (pieces, cut): (&mut Vec<RainHalo>, &mut Vec<PxBox>),
) -> bool {
    if halos.is_empty() || bands.is_empty() {
        return false;
    }
    let (bx0, bx1, by0, by1) = bands_bounds(bands);
    let mut any = false;
    let mut i = 0;
    while i < halos.len() {
        let h = &halos[i];
        let b = halo_box(h);
        if b.1 <= bx0 || b.0 >= bx1 || b.3 <= by0 || b.2 >= by1 || !cut_rect(b, bands, cut) {
            if any {
                pieces.push(*h);
            }
            i += 1;
            continue;
        }
        if !any {
            pieces.clear();
            pieces.extend_from_slice(&halos[..i]);
            any = true;
        }
        let mut ci = 0;
        while ci < cut.len() {
            let (xa, xb, ya, yb) = cut[ci];
            ci += 1;
            pieces.push(RainHalo {
                x: xa as u16,
                y: ya as u16,
                w: (xb - xa) as u16,
                h: (yb - ya) as u16,
                ..*h
            });
        }
        i += 1;
    }
    if any {
        std::mem::swap(halos, pieces);
    }
    any
}

/// Cut every fire patch of `patches` the icon's drawn rows reach
/// ([`cut_rect`]); each piece samples the same window-anchored field (its
/// root, height and phase kept), so what stays lit is unchanged.
fn shade_patches(
    patches: &mut Vec<FirePatch>,
    bands: &[PxBox],
    (pieces, cut): (&mut Vec<FirePatch>, &mut Vec<PxBox>),
) -> bool {
    if patches.is_empty() || bands.is_empty() {
        return false;
    }
    let (bx0, bx1, by0, by1) = bands_bounds(bands);
    let mut any = false;
    let mut i = 0;
    while i < patches.len() {
        let p = &patches[i];
        let (x, y) = (i32::from(p.x), i32::from(p.y));
        let b = (x, x + i32::from(p.w), y, y + i32::from(p.h));
        if b.1 <= bx0 || b.0 >= bx1 || b.3 <= by0 || b.2 >= by1 || !cut_rect(b, bands, cut) {
            if any {
                pieces.push(*p);
            }
            i += 1;
            continue;
        }
        if !any {
            pieces.clear();
            pieces.extend_from_slice(&patches[..i]);
            any = true;
        }
        let mut ci = 0;
        while ci < cut.len() {
            let (xa, xb, ya, yb) = cut[ci];
            ci += 1;
            pieces.push(FirePatch {
                x: xa as u16,
                y: ya as u16,
                w: (xb - xa) as u16,
                h: (yb - ya) as u16,
                ..*p
            });
        }
        i += 1;
    }
    if any {
        std::mem::swap(patches, pieces);
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A round face in a 12×10 art with a two-texel transparent margin on
    /// every side: row `r` of 2..8 spans `2 + |r - 5| / 2 ..= 9 - |r - 5| / 2`,
    /// its columns from `keep` only.
    fn face_art(keep: std::ops::Range<u32>) -> Arc<FxArt> {
        let (w, h) = (12u32, 10u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for r in 2..8i32 {
            let inset = (r - 5).abs() / 2;
            for c in (2 + inset)..=(9 - inset) {
                if keep.contains(&(c as u32)) {
                    rgba[((r as u32 * w + c as u32) * 4 + 3) as usize] = 255;
                }
            }
        }
        Arc::new(FxArt::new(12, 10, FxTexels::Rgba8Straight, rgba).expect("a 12x10 art"))
    }

    /// The face drawn 1:1 into `dest`.
    fn face(dest: PxBox, keep: std::ops::Range<u32>, flip_x: bool) -> IconSrc {
        IconSrc {
            art: face_art(keep),
            dest,
            flip_x,
        }
    }

    /// How many of `rects` cover each pixel.
    fn lit(rects: &[PxBox]) -> std::collections::HashMap<(i32, i32), u32> {
        let mut m = std::collections::HashMap::new();
        for &(x0, x1, y0, y1) in rects {
            for y in y0..y1 {
                for x in x0..x1 {
                    *m.entry((x, y)).or_insert(0) += 1;
                }
            }
        }
        m
    }

    /// The shade takes exactly the face's drawn rows out of a light and
    /// leaves every other pixel lit once, the box's transparent margin with
    /// it, so no notch is cut beside the face. Negative control: with no
    /// art the icon is its box, and the margin goes dark.
    #[test]
    fn the_shade_takes_the_face_s_drawn_rows_and_keeps_its_margin() {
        let icon = (100, 112, 40, 50);
        let src = face(icon, 0..12, false);
        let mut bands = Vec::new();
        drawn_rows(icon, Some(&src), (0, 0), &mut bands);
        let face = lit(&bands);
        assert_eq!(
            face.len(),
            (2..8i32).map(|r| 8 - 2 * ((r - 5).abs() / 2)).sum::<i32>() as usize,
            "the drawn rows are the face's texels: {bands:?}"
        );
        let light = (90, 130, 30, 60);
        let mut pieces = Vec::new();
        assert!(cut_rect(light, &bands, &mut pieces));
        let after = lit(&pieces);
        for px in lit(&[light]).into_keys() {
            let want = u32::from(!face.contains_key(&px));
            assert_eq!(
                after.get(&px).copied().unwrap_or(0),
                want,
                "pixel {px:?}: lit {want} time(s) after the shade"
            );
        }
        let margin = (100, 40);
        assert!(
            after.contains_key(&margin),
            "the box's margin keeps its light"
        );
        drawn_rows(icon, None, (0, 0), &mut bands);
        assert_eq!(bands, vec![icon], "control: no art, the icon is its box");
        assert!(cut_rect(light, &bands, &mut pieces));
        assert!(
            !lit(&pieces).contains_key(&margin),
            "control: cut as its box, the margin goes dark"
        );
        assert!(
            !cut_rect((0, 50, 0, 20), &bands, &mut pieces),
            "a light clear of the icon is untouched"
        );
    }

    /// A stream's ramps shade as the quads they stand for: the rows the
    /// face's drawn rows reach are cut as their quads are (a run whole where
    /// any of its rows is reached), the rows above and below stay ramps, a
    /// ramp clear of the face stays whole, all in the stream's order. A
    /// stream the face reaches nothing of stays as laid.
    #[test]
    fn a_stream_s_ramps_shade_as_their_quads() {
        let icon = (100, 112, 40, 50);
        let mut bands = Vec::new();
        drawn_rows(icon, Some(&face(icon, 0..12, false)), (0, 0), &mut bands);
        let ends = |k: u16, a: u8| {
            let c = 0x0020_3040 + u32::from(k) * 0x0003_0201;
            (
                aterm_render::glow_end(c, a),
                aterm_render::glow_end(c ^ 0x0011_1111, a / 2),
            )
        };
        let quads = vec![
            GlowQuad::gradient(0, 90, 38, 30, 6, 0x0030_0810, 0x30, 0x0008_1030, 0x50),
            GlowQuad::flat(0, 200, 40, 10, 4, 0x0010_2030, 0),
        ];
        let mut ramps = GlowRamps::default();
        // Rows 30..61 across the face (its rows 42..48), its run over rows
        // 40..44 across the face's top, a dark row at 50.
        ramps.open(1, 0, (95, 30, 30), ends(0, 0x40));
        for k in 1..30 {
            if k == 10 {
                assert!(ramps.extend_run(0, 4, ends(k, 0)));
            } else if !(11..14).contains(&k) {
                ramps.extend_last(
                    u16::from(k == 20),
                    ends(k, if k % 3 == 0 { 0x60 } else { 0 }),
                );
            }
        }
        // A ramp clear of the face.
        ramps.open(2, 0, (10, 10, 20), ends(40, 0));
        for k in 41..48 {
            ramps.extend_last(0, ends(k, 0));
        }
        let mut legacy = Vec::new();
        ramps.expand_into(&quads, &mut legacy);
        let (mut cut, mut pieces, mut piece_ramps) = (Vec::new(), Vec::new(), GlowRamps::default());
        let (mut q, mut r) = (quads.clone(), ramps.clone());
        assert!(shade_stream(
            (&mut q, &mut r),
            &bands,
            (&mut pieces, &mut piece_ramps, &mut cut)
        ));
        assert!(shade_quads(&mut legacy, &bands, (&mut pieces, &mut cut)));
        let mut got = Vec::new();
        r.expand_into(&q, &mut got);
        assert_eq!(got, legacy, "the ramps shade as their quads");
        assert_eq!(r.lit, r.count_lit());
        let kept: Vec<_> = r.items.iter().map(|k| (k.x, k.y, k.h)).collect();
        assert_eq!(
            kept,
            [(95, 30, 10), (95, 48, 13), (10, 10, 8)],
            "the rows above and below the face, and the ramp clear of it, stay ramps"
        );
        // A stream clear of the face stays as laid.
        let (mut q, mut r) = (quads[1..].to_vec(), GlowRamps::default());
        r.open(1, 0, (10, 10, 20), ends(40, 0));
        let laid = (q.clone(), r.clone());
        assert!(!shade_stream(
            (&mut q, &mut r),
            &bands,
            (&mut pieces, &mut piece_ramps, &mut cut)
        ));
        assert_eq!((q, r), laid);
    }

    /// A stream with ramps that the face reaches nothing of costs the shade
    /// no copy, even where its lights lie inside the face's drawn rows'
    /// bounds, beside the rows: it stays as laid, and the scratch the shade
    /// rebuilds a stream in is left as it was.
    #[test]
    fn a_stream_the_face_reaches_nothing_of_is_not_copied() {
        let icon = (100, 112, 40, 50);
        let mut bands = Vec::new();
        drawn_rows(icon, Some(&face(icon, 0..12, false)), (0, 0), &mut bands);
        let end = aterm_render::glow_end(0x0020_3040, 0);
        // The face's top corners, each two rows tall: the rows there span
        // 103..109, the rows below them 102..110.
        let mut quads = vec![GlowQuad::flat(0, 102, 42, 1, 2, 0x0010_2030, 0)];
        let mut ramps = GlowRamps::default();
        ramps.open(1, 0, (109, 42, 1), (end, end));
        ramps.extend_last(0, (end, end));
        let bounds = bands_bounds(&bands);
        let (b, rb) = (quad_box(&quads[0]), ramp_box(&ramps.items[0]));
        for (x0, x1, y0, y1) in [b, rb] {
            assert!(
                x0 >= bounds.0 && x1 <= bounds.1 && y0 >= bounds.2 && y1 <= bounds.3,
                "inside the drawn rows' bounds {bounds:?}"
            );
        }
        let laid = (quads.clone(), ramps.clone());
        let sentinel = GlowQuad::flat(0, 1, 1, 1, 1, 0x0001_0203, 0);
        let mut held = GlowRamps::default();
        held.open(0, 7, (1, 1, 1), (end, 0));
        let (mut pieces, mut piece_ramps, mut cut) = (vec![sentinel], held.clone(), Vec::new());
        assert!(!shade_stream(
            (&mut quads, &mut ramps),
            &bands,
            (&mut pieces, &mut piece_ramps, &mut cut)
        ));
        assert_eq!((quads, ramps), laid, "the stream stays as laid");
        assert_eq!(pieces, [sentinel], "its quads are not copied");
        assert_eq!(piece_ramps, held, "nor its ramps");
        // Beside that quad, a ramp the face reaches through its rows, and one
        // it reaches only through its run (rows 40..44, the face's from 42):
        // each stream is shaded as its quads are.
        for run in [false, true] {
            let mut quads = vec![GlowQuad::flat(0, 102, 42, 1, 2, 0x0010_2030, 0)];
            let mut ramps = GlowRamps::default();
            if run {
                ramps.open_run(1, 0, (100, 40, 12), 4, (end, end));
            } else {
                ramps.open(1, 0, (100, 43, 12), (end, end));
                ramps.extend_last(0, (end, end));
            }
            let mut legacy = Vec::new();
            ramps.expand_into(&quads, &mut legacy);
            assert!(
                shade_stream(
                    (&mut quads, &mut ramps),
                    &bands,
                    (&mut pieces, &mut piece_ramps, &mut cut)
                ),
                "run {run}: the ramp is reached"
            );
            assert!(shade_quads(&mut legacy, &bands, (&mut pieces, &mut cut)));
            let mut got = Vec::new();
            ramps.expand_into(&quads, &mut got);
            assert_eq!(got, legacy, "run {run}: shaded as its quads");
        }
    }

    /// A mirrored icon reads its texels mirrored: a face drawn only in the
    /// art's left half is shaded in the box's right half.
    #[test]
    fn a_mirrored_icon_is_shaded_where_it_is_drawn() {
        let icon = (0, 12, 0, 10);
        let mut bands = Vec::new();
        drawn_rows(icon, Some(&face(icon, 0..6, false)), (0, 0), &mut bands);
        assert!(
            !bands.is_empty() && bands.iter().all(|b| b.1 <= 6),
            "unmirrored, the left half: {bands:?}"
        );
        drawn_rows(icon, Some(&face(icon, 0..6, true)), (0, 0), &mut bands);
        assert!(
            !bands.is_empty() && bands.iter().all(|b| b.0 >= 6),
            "mirrored, the right half: {bands:?}"
        );
    }

    /// A peeking icon is shaded only where it shows: its art's box reaches
    /// past its visible window, and the drawn rows stop at the window's
    /// edge, scaled to the art's whole box (here twice the art's size, and
    /// placed by the grid's origin in the window).
    #[test]
    fn a_peeking_icon_is_shaded_only_where_it_shows() {
        let src = face((10, 34, 20, 40), 0..12, false);
        let (ox, oy) = (5, 3);
        // The visible window: the box's top half, in window pixels.
        let shown = (10 + ox, 34 + ox, 20 + oy, 30 + oy);
        let mut bands = Vec::new();
        drawn_rows(shown, Some(&src), (ox, oy), &mut bands);
        assert!(!bands.is_empty(), "the face shows in the window");
        assert!(
            bands.iter().all(|b| b.2 >= shown.2 && b.3 <= shown.3),
            "only the visible rows: {bands:?}"
        );
        // Art row 2 (the face's top) lands on box rows 4-5, window 27-28.
        assert_eq!(
            bands[0].2,
            20 + oy + 4,
            "scaled to the art's box: {bands:?}"
        );
        let mut whole = Vec::new();
        drawn_rows((15, 39, 23, 43), Some(&src), (ox, oy), &mut whole);
        assert!(
            whole.iter().map(|b| b.3).max() > bands.iter().map(|b| b.3).max(),
            "control: shown whole, the face's lower rows are drawn too"
        );
    }

    /// The pixels a sprite's stamp draws, `(x, y)` in window pixels (the
    /// grid at the window's origin): the CPU engine's own spans, read
    /// through its bloom source ([`aterm_render::fx_bloom_quads`]).
    fn stamped(sprite: &FreeSprite) -> std::collections::HashSet<(i32, i32)> {
        let mut layers = Vec::new();
        aterm_render::sprites::SpriteLayers::new(7)
            .publish(std::slice::from_ref(sprite), &mut layers);
        for l in &mut layers {
            l.bloom = true;
        }
        let f = aterm_render::FxFrame {
            w: 256,
            h: 256,
            pad: 0,
            grid_top: 0,
            cell_h: 16,
            rows: 16,
        };
        let mut quads = Vec::new();
        aterm_render::fx_bloom_quads(
            &f,
            &layers,
            &[],
            &mut aterm_render::FxSlicer::default(),
            &mut aterm_render::SpriteCols::default(),
            &mut quads,
        );
        let mut px = std::collections::HashSet::new();
        for q in quads.iter().filter(|q| q.alpha > 0) {
            let (x, y) = (i32::from(q.x), i32::from(q.y));
            for yy in y..y + i32::from(q.h) {
                for xx in x..x + i32::from(q.w) {
                    px.insert((xx, yy));
                }
            }
        }
        px
    }

    /// A SCALED SPRITE'S DRAWN ROWS ARE THE PIXELS ITS STAMP DRAWS (looks7
    /// round 2): the companion's art drawn at half its size, the page veil
    /// keeps to these rows, and read a texel off them, or from the icon's
    /// half-opaque edge, it missed the paw and the chin's anti-aliased edge
    /// the stamp drew (specks between the letters on Solarized Dark). Over
    /// arts scaled down, up and unevenly, plain and mirrored, with
    /// translucent texels among the opaque, every pixel the CPU engine
    /// stamps lies in a drawn row, and every row's span starts and ends on a
    /// stamped pixel. Negative controls: read at the texel centres, or
    /// counted from the icon's edge, a stamped pixel lies outside the rows.
    #[test]
    fn a_scaled_sprite_s_drawn_rows_are_the_pixels_its_stamp_draws() {
        use aterm_core::render::PxRect;
        use aterm_render::sprites::FreeZ;
        // A speckled art: about a third of its texels opaque, the rest clear.
        let art = |w: u16, h: u16| {
            let mut rgba = vec![0u8; usize::from(w) * usize::from(h) * 4];
            let mut seed = 0x9E37_79B9u32;
            for t in rgba.as_chunks_mut::<4>().0 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                if seed >> 30 == 0 || (seed >> 28) & 3 == 1 {
                    *t = [0xE0, 0xD8, 0xC8, 0xFF];
                } else if (seed >> 26) & 7 == 5 {
                    *t = [0xE0, 0xD8, 0xC8, 0x40];
                }
            }
            Arc::new(FxArt::new(w, h, FxTexels::Rgba8Straight, rgba).expect("an art"))
        };
        let centred = |l: i32, a: i32, d: i32, flip: bool| {
            let m = if flip { d - 1 - l } else { l };
            (2 * m + 1) * a / (2 * d)
        };
        let (mut missed_centred, mut missed_edge) = (0usize, 0usize);
        for (aw, ah, dw, dh) in [
            (40, 20, 20, 10),
            (34, 26, 21, 13),
            (12, 10, 30, 23),
            (9, 7, 9, 7),
        ] {
            for flip_x in [false, true] {
                let sprite = FreeSprite {
                    x: 17,
                    y: 11,
                    w: dw,
                    h: dh,
                    art: art(aw, ah),
                    clip: PxRect::EVERYWHERE,
                    tint: 0x00FF_FFFF,
                    alpha: 255,
                    flip_x,
                    z: FreeZ::UnderText,
                };
                let drawn = stamped(&sprite);
                assert!(!drawn.is_empty(), "{aw}x{ah} at {dw}x{dh}: the stamp draws");
                let clip = (0, 256, 0, 256);
                let mut bands = Vec::new();
                sprite_rows(&sprite, clip, (0, 0), &mut bands);
                let held = lit(&bands);
                for &px in &drawn {
                    assert!(
                        held.contains_key(&px),
                        "{aw}x{ah} at {dw}x{dh}, flip {flip_x}: stamped {px:?} lies in no drawn row: {bands:?}"
                    );
                }
                for &(x0, x1, y0, y1) in &bands {
                    for y in y0..y1 {
                        assert!(
                            drawn.contains(&(x0, y)) && drawn.contains(&(x1 - 1, y)),
                            "{aw}x{ah} at {dw}x{dh}, flip {flip_x}: row {y} spans {x0}..{x1} past what the stamp drew"
                        );
                    }
                }
                let src = IconSrc {
                    art: Arc::clone(&sprite.art),
                    dest: (17, 17 + i32::from(dw), 11, 11 + i32::from(dh)),
                    flip_x,
                };
                drawn_rows_by(clip, Some(&src), (0, 0), &mut bands, (centred, 1));
                let held = lit(&bands);
                missed_centred += drawn.iter().filter(|px| !held.contains_key(px)).count();
                drawn_rows_by(
                    clip,
                    Some(&src),
                    (0, 0),
                    &mut bands,
                    (stamp_texel, ICON_ALPHA_EDGE),
                );
                let held = lit(&bands);
                missed_edge += drawn.iter().filter(|px| !held.contains_key(px)).count();
            }
        }
        assert!(
            missed_centred > 0,
            "control: read at the texel centres, a stamped pixel lies outside the rows"
        );
        assert!(
            missed_edge > 0,
            "control: counted from the icon's edge, a stamped pixel lies outside the rows"
        );
    }

    /// A star by an icon goes whole, and only the star: a piece of the wash
    /// inside the star's box (a train cut row by row) keeps its light, where
    /// the star's box alone took it and left a dark notch. A star clear of
    /// the icon stays.
    #[test]
    fn a_star_by_an_icon_goes_whole_and_alone() {
        let quad = |x: u16, y: u16, w: u16, h: u16, color: u32| GlowQuad {
            row: 0,
            x,
            y,
            w,
            h,
            color,
            alpha: 0,
            color2: color,
            alpha2: 0,
        };
        let star = [
            quad(40, 50, 11, 1, 0x00FF_C0FF),
            quad(45, 45, 1, 11, 0x00FF_C0FF),
        ];
        let wash = quad(41, 47, 6, 1, 0x0030_1060);
        let far = [
            quad(140, 50, 11, 1, 0x00FF_C0FF),
            quad(145, 45, 1, 11, 0x00FF_C0FF),
        ];
        let mut keys = Vec::new();
        let near = rk::stardust::SkyMark::of((45, 50), 6, (&star[..], &[]), &mut keys)
            .expect("the star painted");
        let away = rk::stardust::SkyMark::of((145, 50), 6, (&far[..], &[]), &mut keys)
            .expect("the far star painted");
        let in_box = |q: &GlowQuad| {
            let b = quad_box(q);
            b.0 >= near.rect.0 && b.1 <= near.rect.1 && b.2 >= near.rect.2 && b.3 <= near.rect.3
        };
        assert!(
            in_box(&wash),
            "fixture: the wash's piece lies in the star's box"
        );
        let mut over: Vec<GlowQuad> = star.iter().chain(&far).copied().chain([wash]).collect();
        let (mut under, mut halos) = (Vec::new(), Vec::new());
        let (mut over_ramps, mut under_ramps) = (GlowRamps::default(), GlowRamps::default());
        // An icon just right of the near star, its arm reaching it.
        let icon = [(48, 70, 30, 60)];
        assert!(lift_stars(
            (&[near, away], &keys),
            &icon,
            (0, 0),
            (
                (&mut over, &mut over_ramps),
                (&mut under, &mut under_ramps),
                &mut halos
            )
        ));
        assert!(
            star.iter().all(|q| !over.contains(q)),
            "the star by the icon goes whole: {over:?}"
        );
        assert!(
            over.contains(&wash),
            "the wash's piece in its box stays lit"
        );
        assert!(
            far.iter().all(|q| over.contains(q)),
            "a star clear of the icon stays"
        );
    }

    /// A split quad continues its gradient: the pieces' colours at their
    /// columns are the whole quad's at the same columns.
    #[test]
    fn a_split_quad_keeps_its_gradient() {
        let q = GlowQuad {
            row: 2,
            x: 90,
            y: 50,
            w: 50,
            h: 20,
            color: 0x0000_0000,
            alpha: 0,
            color2: 0x00C8_6432,
            alpha2: 200,
        };
        let mut quads = vec![q];
        let (mut pieces, mut cut) = (Vec::new(), Vec::new());
        assert!(shade_quads(
            &mut quads,
            &[(100, 130, 30, 60)],
            (&mut pieces, &mut cut)
        ));
        assert!(quads.len() >= 2, "the quad split: {quads:?}");
        let at = |q: &GlowQuad, x: i32| {
            let k = x - i32::from(q.x);
            lerp_rgb(q.color, q.color2, k, i32::from(q.w))
        };
        for p in &quads {
            let x = i32::from(p.x);
            let whole = at(&q, x);
            let piece = at(p, x);
            let gap = [16u32, 8, 0]
                .iter()
                .map(|&sh| (((whole >> sh) & 0xff) as i32 - ((piece >> sh) & 0xff) as i32).abs())
                .max()
                .unwrap_or(0);
            assert!(
                gap <= 1,
                "piece {p:?} at {x}: {piece:06x} against {whole:06x}"
            );
            assert_eq!(p.row, 2, "a piece keeps its row band");
        }
    }
}
