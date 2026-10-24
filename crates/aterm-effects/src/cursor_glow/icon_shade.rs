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
//! It shades the streams the host draws, not the glow's own: a capture, a
//! split frame and the web host hand the host the trail's streams before
//! their word pass runs, so a shade of the glow's buffers reached only the
//! single-grid present. It shades once per word pass. A frame with no icon
//! is untouched, and one whose keep-out already held every light off its
//! icons finds nothing to lift.

use super::*;
use aterm_core::render::{FxArt, FxTexels};
use aterm_render::sprites::FreeSprite;
use std::sync::Arc;

/// A window-pixel box `(x0, x1, y0, y1)`, right and bottom exclusive.
type PxBox = (i32, i32, i32, i32);

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

    /// Lift this frame's light off the word icons the word pass just drew
    /// ([`Self::note_word_icon_sprites`]), in the streams the host draws:
    /// `over` the over-text quads, `under` the under-text quads and `halos`
    /// the halos; an icon noted with no art ([`Self::note_word_icons`]) is
    /// cut as its box. Each word pass's icons shade once. Returns whether
    /// anything was lifted.
    pub fn shade_word_icons(
        &mut self,
        (over, under, halos): (&mut Vec<GlowQuad>, &mut Vec<GlowQuad>, &mut Vec<RainHalo>),
    ) -> bool {
        if !std::mem::take(&mut self.word_icons_unshaded)
            || !self.v2.engaged()
            || self.word_icons.is_empty()
        {
            return false;
        }
        let Some((ox, oy, _)) = self.keep_geom else {
            return false;
        };
        let mut lifted = lift_stars(
            (self.v2.sky_marks(), self.v2.sky_mark_lights()),
            &self.word_icons,
            (ox, oy),
            (&mut *over, &mut *under, &mut *halos),
        );
        let mut bands = std::mem::take(&mut self.icon_scratch);
        let mut pieces = std::mem::take(&mut self.icon_quads);
        let mut halo_pieces = std::mem::take(&mut self.icon_halos);
        let mut cut = std::mem::take(&mut self.icon_cut);
        for (i, &icon) in self.word_icons.iter().enumerate() {
            let icon = (icon.0 + ox, icon.1 + ox, icon.2 + oy, icon.3 + oy);
            if icon.0 >= icon.1 || icon.2 >= icon.3 {
                continue;
            }
            drawn_rows(icon, self.word_icon_src.get(i), (ox, oy), &mut bands);
            lifted |= shade_quads(over, &bands, (&mut pieces, &mut cut));
            lifted |= shade_quads(under, &bands, (&mut pieces, &mut cut));
            lifted |= shade_halos(halos, &bands, (&mut halo_pieces, &mut cut));
        }
        self.icon_scratch = bands;
        self.icon_quads = pieces;
        self.icon_halos = halo_pieces;
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
    (over, under, halos): (&mut Vec<GlowQuad>, &mut Vec<GlowQuad>, &mut Vec<RainHalo>),
) -> bool {
    let mut lifted = false;
    for mark in marks {
        let r = mark.reach / 2;
        let (x, y) = (mark.at.0 - ox, mark.at.1 - oy);
        if !icons
            .iter()
            .any(|&(x0, x1, y0, y1)| x + r >= x0 && x - r < x1 && y + r >= y0 && y - r < y1)
        {
            continue;
        }
        let own = keys
            .get(mark.lights.0 as usize..mark.lights.1 as usize)
            .unwrap_or_default();
        let inside = |b: PxBox| {
            b.0 >= mark.rect.0 && b.1 <= mark.rect.1 && b.2 >= mark.rect.2 && b.3 <= mark.rect.3
        };
        let star_quad =
            |q: &GlowQuad| inside(quad_box(q)) && own.contains(&rk::stardust::sky_quad_key(q));
        let (nq, nu, nh) = (over.len(), under.len(), halos.len());
        over.retain(|q| !star_quad(q));
        under.retain(|q| !star_quad(q));
        halos.retain(|h| !(inside(halo_box(h)) && own.contains(&rk::stardust::sky_halo_key(h))));
        lifted |= over.len() != nq || under.len() != nu || halos.len() != nh;
    }
    lifted
}

#[inline]
fn quad_box(q: &GlowQuad) -> PxBox {
    let (x, y) = (i32::from(q.x), i32::from(q.y));
    (x, x + i32::from(q.w), y, y + i32::from(q.h))
}

#[inline]
fn halo_box(h: &RainHalo) -> PxBox {
    let (x, y) = (i32::from(h.x), i32::from(h.y));
    (x, x + i32::from(h.w), y, y + i32::from(h.h))
}

/// The icon's drawn rows as bands `(x0, x1, y0, y1)` in window pixels, top
/// to bottom, rows of one span merged, inside `icon` (its visible box in
/// window pixels): each row spans its first to its last texel at least
/// [`ICON_ALPHA_EDGE`] opaque (an Index8 texel: any ink), sampled as the
/// stamp samples (nearest over the whole art's box, `origin` the grid's in
/// the window, mirrored for `flip_x`). With no art, the box.
fn drawn_rows(icon: PxBox, src: Option<&IconSrc>, (ox, oy): (i32, i32), out: &mut Vec<PxBox>) {
    out.clear();
    let Some(s) = src.filter(|s| !s.art.is_empty()) else {
        out.push(icon);
        return;
    };
    let (bx0, by0) = (s.dest.0 + ox, s.dest.2 + oy);
    let (w, h) = (s.dest.1 - s.dest.0, s.dest.3 - s.dest.2);
    if w <= 0 || h <= 0 {
        out.push(icon);
        return;
    }
    let (aw, ah) = (i32::from(s.art.w()), i32::from(s.art.h()));
    let kind = s.art.texels();
    let drawn = |v: u32| match kind {
        FxTexels::Index8 => v != 0,
        FxTexels::Rgba8Premul | FxTexels::Rgba8Straight => v >> 24 >= u32::from(ICON_ALPHA_EDGE),
    };
    let (lo, hi) = (icon.0.max(bx0), icon.1.min(bx0 + w));
    for y in icon.2.max(by0)..icon.3.min(by0 + h) {
        let dy = y - by0;
        let sy = ((2 * dy + 1) * ah / (2 * h)).min(ah - 1) as usize;
        let opaque = |x: i32| {
            let dx = x - bx0;
            let mx = if s.flip_x { w - 1 - dx } else { dx };
            let sx = ((2 * mx + 1) * aw / (2 * w)).min(aw - 1) as usize;
            drawn(s.art.texel(sx, sy))
        };
        let Some(first) = (lo..hi).find(|&x| opaque(x)) else {
            continue;
        };
        let last = (first..hi).rev().find(|&x| opaque(x)).unwrap_or(first);
        let (x0, x1) = (first, last + 1);
        match out.last_mut() {
            Some(b) if b.3 == y && b.0 == x0 && b.1 == x1 => b.3 = y + 1,
            _ => out.push((x0, x1, y, y + 1)),
        }
    }
}

/// The pieces of `rect` outside `bands` (top to bottom, disjoint rows) into
/// `out`, or `false` (and `out` untouched) when no band reaches it: in a
/// band's rows the parts left and right of its span, elsewhere whole slices.
fn cut_rect(rect: PxBox, bands: &[PxBox], out: &mut Vec<PxBox>) -> bool {
    let (x0, x1, y0, y1) = rect;
    let reaches = |b: &&PxBox| b.2 < y1 && b.3 > y0 && b.0 < x1 && b.1 > x0;
    if x0 >= x1
        || y0 >= y1
        || bands.is_empty()
        || y1 <= bands[0].2
        || y0 >= bands[bands.len() - 1].3
        || !bands.iter().any(|b| reaches(&b))
    {
        return false;
    }
    out.clear();
    let mut y = y0;
    for b in bands.iter().filter(reaches) {
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
    if y < y1 {
        out.push((x0, x1, y, y1));
    }
    true
}

/// `a` to `b` at column share `k / w` of a quad's horizontal gradient, per
/// channel (the gradient a sub-quad starting there continues exactly).
#[inline]
fn lerp_rgb(a: u32, b: u32, k: i32, w: i32) -> u32 {
    let w = w.max(1);
    let ch = |sh: u32| {
        let (ca, cb) = (((a >> sh) & 0xff) as i32, ((b >> sh) & 0xff) as i32);
        ((ca * (w - k) + cb * k + w / 2) / w).clamp(0, 255) as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
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
    let mut any = false;
    for (i, q) in quads.iter().enumerate() {
        let b = quad_box(q);
        if !cut_rect(b, bands, cut) {
            if any {
                pieces.push(*q);
            }
            continue;
        }
        if !any {
            pieces.clear();
            pieces.extend_from_slice(&quads[..i]);
            any = true;
        }
        let w = i32::from(q.w);
        let flat = q.color == q.color2 && q.alpha == q.alpha2;
        for &(xa, xb, ya, yb) in cut.iter() {
            let (ka, kb) = (xa - b.0, xb - b.0);
            let (color, color2, alpha, alpha2) = if flat || (ka == 0 && kb == w) {
                (q.color, q.color2, q.alpha, q.alpha2)
            } else {
                let a = |k: i32| {
                    (i32::from(q.alpha) * (w - k) + i32::from(q.alpha2) * k + w / 2) / w.max(1)
                };
                (
                    lerp_rgb(q.color, q.color2, ka, w),
                    lerp_rgb(q.color, q.color2, kb, w),
                    a(ka).clamp(0, 255) as u8,
                    a(kb).clamp(0, 255) as u8,
                )
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
    if any {
        std::mem::swap(quads, pieces);
    }
    any
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
    let mut any = false;
    for (i, h) in halos.iter().enumerate() {
        if !cut_rect(halo_box(h), bands, cut) {
            if any {
                pieces.push(*h);
            }
            continue;
        }
        if !any {
            pieces.clear();
            pieces.extend_from_slice(&halos[..i]);
            any = true;
        }
        for &(xa, xb, ya, yb) in cut.iter() {
            pieces.push(RainHalo {
                x: xa as u16,
                y: ya as u16,
                w: (xb - xa) as u16,
                h: (yb - ya) as u16,
                ..*h
            });
        }
    }
    if any {
        std::mem::swap(halos, pieces);
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
        // An icon just right of the near star, its arm reaching it.
        let icon = [(48, 70, 30, 60)];
        assert!(lift_stars(
            (&[near, away], &keys),
            &icon,
            (0, 0),
            (&mut over, &mut under, &mut halos)
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
