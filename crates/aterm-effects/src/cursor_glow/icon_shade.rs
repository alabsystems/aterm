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
    #[inline]
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

    #[inline]
    pub(crate) fn note_word_icon_sprite_slice(&mut self, sprites: &[FreeSprite]) {
        self.word_icons.clear();
        self.word_icon_src.clear();
        let mut i = 0;
        while i < sprites.len() {
            let s = &sprites[i];
            i += 1;
            if s.z == aterm_render::sprites::FreeZ::UnderText && !s.is_blank() {
                let v = s.visible();
                self.word_icons.push((v.x0, v.x1, v.y0, v.y1));
                self.word_icon_src.push(IconSrc {
                    art: Arc::clone(&s.art),
                    dest: (s.x, s.x + i32::from(s.w), s.y, s.y + i32::from(s.h)),
                    flip_x: s.flip_x,
                });
            }
        }
        self.word_icons_unshaded = true;
    }

    /// The word icons the last word pass named, grid pixels `(x0, x1, y0, y1)`
    /// ([`Self::note_word_icon_sprites`]): what the typed dog keeps off
    /// ([`crate::word_decorations::WordDecorations::dog_cameo`]).
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
        let (x_lo, x_hi, y_lo, y_hi) = (x - reach, x + reach, y - reach, y + reach);
        let icons = self.word_icons.as_slice();
        let mut i = 0;
        while i < icons.len() {
            let (x0, x1, y0, y1) = icons[i];
            if x_hi >= x0 as f32 && x_lo < x1 as f32 && y_hi >= y0 as f32 && y_lo < y1 as f32 {
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
    #[inline]
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
                if !bands.is_empty() {
                    let bounds = bands_bounds(&bands);
                    lifted |= shade_stream_with_bounds(
                        (&mut *over.0, &mut *over.1),
                        (&bands, bounds),
                        (&mut pieces, &mut piece_ramps, &mut cut),
                    );
                    lifted |= shade_stream_with_bounds(
                        (&mut *under.0, &mut *under.1),
                        (&bands, bounds),
                        (&mut pieces, &mut piece_ramps, &mut cut),
                    );
                    lifted |= shade_halos_with_bounds(
                        halos,
                        (&bands, bounds),
                        (&mut halo_pieces, &mut cut),
                    );
                    lifted |= shade_patches_with_bounds(
                        patches,
                        (&bands, bounds),
                        (&mut patch_pieces, &mut cut),
                    );
                }
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
#[inline]
fn lift_stars(
    (marks, keys): (&[rk::stardust::SkyMark], &[u64]),
    icons: &[PxBox],
    (ox, oy): (i32, i32),
    (over, under, halos): (ShadeStream<'_>, ShadeStream<'_>, &mut Vec<RainHalo>),
) -> bool {
    if marks.is_empty() || icons.is_empty() {
        return false;
    }
    let mut lifted = false;
    let mut mi = 0;
    while mi < marks.len() {
        let mark = &marks[mi];
        mi += 1;
        let r = mark.reach / 2;
        let (x, y) = (mark.at.0 - ox, mark.at.1 - oy);
        let (x_lo, x_hi, y_lo, y_hi) = (x - r, x + r, y - r, y + r);
        let mut hit = false;
        let mut ii = 0;
        while ii < icons.len() {
            let (x0, x1, y0, y1) = icons[ii];
            if x_hi >= x0 && x_lo < x1 && y_hi >= y0 && y_lo < y1 {
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
        let (nq, nu, nh) = (over.0.len(), under.0.len(), halos.len());
        // A star's lights are quads of their own, never a ramp's rows.
        if nq > 0 {
            over.1.retain_quads(over.0, |q| !is_star_quad(q, rect, own));
        }
        if nu > 0 {
            under
                .1
                .retain_quads(under.0, |q| !is_star_quad(q, rect, own));
        }
        if nh > 0 {
            halos.retain(|h| !is_star_halo(h, rect, own));
        }
        lifted |= over.0.len() != nq || under.0.len() != nu || halos.len() != nh;
    }
    lifted
}

#[inline]
const fn has_light_key(own: &[u64], key: u64) -> bool {
    let mut i = 0;
    while i < own.len() {
        if own[i] == key {
            return true;
        }
        i += 1;
    }
    false
}

#[inline]
const fn is_star_quad(q: &GlowQuad, rect: PxBox, own: &[u64]) -> bool {
    let b = quad_box(q);
    b.0 >= rect.0
        && b.1 <= rect.1
        && b.2 >= rect.2
        && b.3 <= rect.3
        && has_light_key(own, rk::stardust::sky_quad_key(q))
}

#[inline]
const fn is_star_halo(h: &RainHalo, rect: PxBox, own: &[u64]) -> bool {
    let b = halo_box(h);
    b.0 >= rect.0
        && b.1 <= rect.1
        && b.2 >= rect.2
        && b.3 <= rect.3
        && has_light_key(own, rk::stardust::sky_halo_key(h))
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
/// page patch then laid under its letters as specks between them (looks7
/// round 2).
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
#[inline]
fn drawn_rows(icon: PxBox, src: Option<&IconSrc>, origin: (i32, i32), out: &mut Vec<PxBox>) {
    drawn_rows_by(icon, src, origin, out, (stamp_texel, ICON_ALPHA_EDGE));
}

/// [`drawn_rows`], each pixel's texel read by `texel` and counted drawn
/// from `edge` alpha up.
#[inline]
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

#[inline]
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
/// top to bottom: the silhouette the glass yield's rule 1 cuts
/// ([`super::glass_yield`]). Every texel the stamp draws counts, its
/// anti-aliased edge with it: counted from the icon's half-opaque edge, a
/// chin's sloping edge (texels a quarter to a half opaque, five pixels past
/// the row's opaque span) would lie outside the cut and lay glass under the
/// sprite's edge. An empty art yields `clip` whole.
#[inline]
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
#[inline]
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
#[cfg(test)]
#[inline]
fn shade_quads(
    quads: &mut Vec<GlowQuad>,
    bands: &[PxBox],
    (pieces, cut): (&mut Vec<GlowQuad>, &mut Vec<PxBox>),
) -> bool {
    if quads.is_empty() || bands.is_empty() {
        return false;
    }
    shade_quads_with_bounds(quads, (bands, bands_bounds(bands)), (pieces, cut))
}

#[inline]
fn shade_quads_with_bounds(
    quads: &mut Vec<GlowQuad>,
    (bands, bounds): (&[PxBox], PxBox),
    (pieces, cut): (&mut Vec<GlowQuad>, &mut Vec<PxBox>),
) -> bool {
    if quads.is_empty() || bands.is_empty() {
        return false;
    }
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
#[cfg(test)]
#[inline]
fn shade_stream(
    (quads, ramps): ShadeStream<'_>,
    bands: &[PxBox],
    (pieces, piece_ramps, cut): (&mut Vec<GlowQuad>, &mut GlowRamps, &mut Vec<PxBox>),
) -> bool {
    if bands.is_empty() {
        return false;
    }
    shade_stream_with_bounds(
        (quads, ramps),
        (bands, bands_bounds(bands)),
        (pieces, piece_ramps, cut),
    )
}

#[inline]
fn shade_stream_with_bounds(
    (quads, ramps): ShadeStream<'_>,
    (bands, bounds): (&[PxBox], PxBox),
    (pieces, piece_ramps, cut): (&mut Vec<GlowQuad>, &mut GlowRamps, &mut Vec<PxBox>),
) -> bool {
    if ramps.is_empty() {
        return shade_quads_with_bounds(quads, (bands, bounds), (pieces, cut));
    }
    if bands.is_empty() {
        return false;
    }
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
#[inline]
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
#[inline]
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
#[inline]
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
#[inline]
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
#[inline]
fn shade_halos_with_bounds(
    halos: &mut Vec<RainHalo>,
    (bands, (bx0, bx1, by0, by1)): (&[PxBox], PxBox),
    (pieces, cut): (&mut Vec<RainHalo>, &mut Vec<PxBox>),
) -> bool {
    if halos.is_empty() || bands.is_empty() {
        return false;
    }
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
#[inline]
fn shade_patches_with_bounds(
    patches: &mut Vec<FirePatch>,
    (bands, (bx0, bx1, by0, by1)): (&[PxBox], PxBox),
    (pieces, cut): (&mut Vec<FirePatch>, &mut Vec<PxBox>),
) -> bool {
    if patches.is_empty() || bands.is_empty() {
        return false;
    }
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
#[path = "icon_shade_tests.rs"]
mod tests;
