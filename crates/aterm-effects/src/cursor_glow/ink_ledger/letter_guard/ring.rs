// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A letter read as a reader sees it: each stroke against the ring of ground round it.
//!
//! The trail judges (`aterm-lab trail-contrast`'s `local_contrast`, aterm-render's
//! `trail_letters_read`) read a lettered cell by its solid ink: the pixels of the clean glyph
//! that read at least [`SOLID_SHARE`] of its most contrasting pixel's ratio against the cell's
//! ground. Each is read in the frame against its ring: the median, channel by channel, of the
//! frame's pixels on the nearest Chebyshev ring (1 px out, else 2) that are the cell's ground in
//! the clean frame. A pixel row reads the lower median of its pixels' ratios, and the letter its
//! worst row. A letter on a dim page keeps [`OWN_SHARE`] of its own reading, the same judge read
//! on the clean frame.
//!
//! [`RingCell`] holds that reading for one cell from the glyph's own pixels (the face's raster,
//! [`aterm_render::GlyphInk::raster`], laid as the renderer lays text, [`TextLevel`]) and its
//! neighbours' (whose glyphs reach its ring box and whose grounds bound its ring), so the letter
//! guard keeps exactly what the judge reads: where the lights are a glow's edge at the page's
//! lightness in another hue, the ring's channel medians read brighter than either, and a bound on
//! the grounds' luminance alone missed it (round 25, Nyan's `p` at 96.6% of its own reading on
//! Solarized Dark's wrapped line).
//!
//! [`SOLID_SHARE`]: super::SOLID_SHARE
//! [`OWN_SHARE`]: super::OWN_SHARE

use aterm_render::{GlyphRaster, TextLevel};

/// One cell of a judged letter's neighbourhood (its own cell and the eight round it), as the
/// letter's ring box meets it.
#[derive(Clone, Copy)]
pub(super) enum Neighbour<'a> {
    /// A cell whose pixels the guard knows: its ground, what the frame lays on it under every
    /// light (`base`: the ground, or the cursor trail's fill of the cell, which the clean frame
    /// does not draw) and, for a lettered cell, its glyph's raster and colour.
    Known {
        ground: u32,
        base: u32,
        glyph: Option<(&'a GlyphRaster, u32)>,
    },
    /// A cell whose pixels it does not (a glyph the face's table holds no raster for, a slanted
    /// or decorated one, a block caret's fill): no pixel of it reads as the letter's ground.
    Opaque,
}

/// A ring box's frame pixel, read by [`RingCell::reading`].
pub(super) trait FramePx {
    /// The frame's colour at box pixel `k`.
    fn at(&self, k: usize) -> u32;
}

/// How many `(fg, ground)` pairs [`Levels`] keeps.
const LEVEL_WAYS: usize = 8;

/// One `(fg, ground, corrected)` pair's text levels, each worked out when first read.
type LevelWay = ((u32, u32, bool), Box<[Option<TextLevel>; 256]>);

/// The text levels ([`TextLevel`]) of a few `(fg, ground)` pairs, each worked out once: a level's
/// corrected alpha takes three sRGB transfers.
struct Levels {
    ways: Vec<LevelWay>,
    next: usize,
}

impl Levels {
    /// The way that holds `(fg, ground, corrected)`'s levels, taken (the oldest way evicted) if
    /// none does. A glyph's pixels share one pair, so its way is found once, not per pixel.
    fn way(&mut self, fg: u32, ground: u32, corrected: bool) -> usize {
        let key = (fg, ground, corrected);
        if let Some(i) = self.ways.iter().position(|(k, _)| *k == key) {
            i
        } else if self.ways.len() < LEVEL_WAYS {
            self.ways.push((key, Box::new([None; 256])));
            self.ways.len() - 1
        } else {
            let i = self.next;
            self.next = (self.next + 1) % LEVEL_WAYS;
            self.ways[i] = (key, Box::new([None; 256]));
            i
        }
    }

    /// Level `cov` of way `i` ([`Self::way`]).
    fn level_at(&mut self, i: usize, cov: u8) -> TextLevel {
        let ((fg, ground, corrected), levels) = &mut self.ways[i];
        let (fg, ground, corrected) = (*fg, *ground, *corrected);
        *levels[usize::from(cov)].get_or_insert_with(|| TextLevel::new(fg, ground, cov, corrected))
    }
}

/// One lettered cell read as the judge reads it ([`self`]): its ring box's clean pixels, its solid
/// ink and each solid pixel's ring.
pub(super) struct RingCell {
    /// The ring box's size, px.
    w: usize,
    h: usize,
    /// The clean frame's colour per box pixel (meaningless where [`Self::opaque`]), and the
    /// frame's ground under every light ([`Neighbour::Known`]'s `base`).
    clean: Vec<u32>,
    base: Vec<u32>,
    opaque: Vec<bool>,
    /// Per box pixel, whether the clean frame shows the letter's ground there.
    ground: Vec<bool>,
    /// Per box pixel, whether a glyph lays text there; and each glyph's text, in the renderer's
    /// draw order: the box pixel and its level.
    has_text: Vec<bool>,
    text: Vec<(u32, TextLevel)>,
    /// The solid ink pixels, row-major: each box pixel, its row, and where its ring starts in
    /// [`Self::rings`] (`ring_at[i] .. ring_at[i + 1]`).
    solid: Vec<(u32, u32)>,
    ring_at: Vec<u32>,
    rings: Vec<u32>,
    /// The letter's own reading (the judge on the clean frame); infinite where no solid pixel has
    /// ground within the ring.
    pub(super) own: f32,
    /// The least luminance of the clean solid ink.
    pub(super) l_min: f32,
    /// Whether some ring pixel holds text (a glyph's faint rim that rounds to the ground).
    pub(super) ring_text: bool,
    /// Scratch: a ring's colours per channel, a row's ratios.
    chan: [Vec<u8>; 3],
    ratios: Vec<f32>,
    levels: Levels,
}

impl RingCell {
    pub(super) const EMPTY: Self = Self {
        w: 0,
        h: 0,
        clean: Vec::new(),
        base: Vec::new(),
        opaque: Vec::new(),
        ground: Vec::new(),
        has_text: Vec::new(),
        text: Vec::new(),
        solid: Vec::new(),
        ring_at: Vec::new(),
        rings: Vec::new(),
        own: f32::INFINITY,
        l_min: f32::INFINITY,
        ring_text: false,
        chan: [Vec::new(), Vec::new(), Vec::new()],
        ratios: Vec::new(),
        levels: Levels {
            ways: Vec::new(),
            next: 0,
        },
    };

    /// Read the cell whose ring box is `w` x `h` px, its cell `cw` x `ch` px at `(ring, ring)` in
    /// it, from its neighbourhood `cells` (row by row from the row above, each row left to right:
    /// the renderer's draw order), its own at `cells[4]`, and the renderer's blend (`corrected`).
    pub(super) fn read(
        &mut self,
        (w, h): (usize, usize),
        (cw, ch): (usize, usize),
        ring: usize,
        cells: &[Neighbour<'_>; 9],
        corrected: bool,
    ) {
        let table = crate::color_math::relative_luminance_table();
        let lum = |c: u32| crate::color_math::relative_luminance_with(table, c);
        let ratio = |a: f32, b: f32| (a.max(b) + 0.05) / (a.min(b) + 0.05);
        let Neighbour::Known { ground, .. } = cells[4] else {
            self.own = f32::INFINITY;
            self.solid.clear();
            return;
        };
        (self.w, self.h) = (w, h);
        let n = w * h;
        self.clean.clear();
        self.clean.resize(n, ground);
        self.base.clear();
        self.base.resize(n, ground);
        self.opaque.clear();
        self.opaque.resize(n, false);
        self.has_text.clear();
        self.has_text.resize(n, false);
        self.text.clear();
        // Which neighbour a box pixel lies in, along one axis.
        let side = |v: usize, cell: usize| -> usize {
            if v < ring {
                0
            } else if v < ring + cell {
                1
            } else {
                2
            }
        };
        for y in 0..h {
            for x in 0..w {
                let k = y * w + x;
                match cells[side(y, ch) * 3 + side(x, cw)] {
                    Neighbour::Known {
                        ground: g, base, ..
                    } => (self.clean[k], self.base[k]) = (g, base),
                    Neighbour::Opaque => self.opaque[k] = true,
                }
            }
        }
        // Each glyph laid over its cell's ground, in draw order.
        for (i, cell) in cells.iter().enumerate() {
            let Neighbour::Known {
                ground: g,
                glyph: Some((raster, fg)),
                ..
            } = *cell
            else {
                continue;
            };
            // The cell's top left in the box.
            let ox = ring as i64 + (i as i64 % 3 - 1) * cw as i64;
            let oy = ring as i64 + (i as i64 / 3 - 1) * ch as i64;
            let rh = raster.h();
            let way = self.levels.way(fg & 0x00FF_FFFF, g, corrected);
            for ry in 0..rh {
                let by = oy + i64::from(raster.y) + ry as i64;
                if by < 0 || by >= h as i64 {
                    continue;
                }
                for rx in 0..raster.w {
                    let cov = raster.cov[ry * raster.w + rx];
                    let bx = ox + i64::from(raster.x) + rx as i64;
                    if cov == 0 || bx < 0 || bx >= w as i64 {
                        continue;
                    }
                    let k = by as usize * w + bx as usize;
                    let level = self.levels.level_at(way, cov);
                    self.clean[k] = level.over(self.clean[k]);
                    self.has_text[k] = true;
                    self.text.push((k as u32, level));
                }
            }
        }
        self.ground.clear();
        self.ground
            .extend((0..n).map(|k| !self.opaque[k] && self.clean[k] == ground));
        // The cell's extreme and its solid ink.
        let l_g = lum(ground);
        // The cell's own pixels in the box, row-major, each clean pixel's luminance read once
        // (into `ratios`, which [`Self::reading`] clears before it reads its own).
        let (x_end, y_end) = ((ring + cw).min(w), (ring + ch).min(h));
        self.ratios.clear();
        let mut extreme = 1.0f32;
        for y in ring..y_end {
            for k in y * w + ring..y * w + x_end {
                if !self.opaque[k] {
                    let lk = lum(self.clean[k]);
                    self.ratios.push(lk);
                    extreme = extreme.max(ratio(lk, l_g));
                }
            }
        }
        let solid_at = super::SOLID_SHARE * extreme;
        self.solid.clear();
        self.ring_at.clear();
        self.rings.clear();
        self.ring_text = false;
        self.l_min = f32::INFINITY;
        let mut li = 0;
        for y in ring..y_end {
            for x in ring..x_end {
                let k = y * w + x;
                if self.opaque[k] {
                    continue;
                }
                let lk = self.ratios[li];
                li += 1;
                if ratio(lk, l_g) < solid_at {
                    continue;
                }
                let (x, y) = (x as i64, y as i64);
                let from = self.rings.len();
                for d in 1..=super::RING as i64 {
                    // The Chebyshev ring at `d`, row-major: its top and bottom rows whole, its
                    // two sides between.
                    for dy in -d..=d {
                        let yy = y + dy;
                        if yy < 0 || yy >= h as i64 {
                            continue;
                        }
                        let edge = dy.abs() == d;
                        let mut dx = -d;
                        while dx <= d {
                            let xx = x + dx;
                            if xx >= 0 && xx < w as i64 {
                                let kk = yy as usize * w + xx as usize;
                                if self.ground[kk] {
                                    self.rings.push(kk as u32);
                                    self.ring_text |= self.has_text[kk];
                                }
                            }
                            dx += if edge { 1 } else { 2 * d };
                        }
                    }
                    if self.rings.len() > from {
                        break;
                    }
                }
                if self.rings.len() == from {
                    continue;
                }
                self.ring_at.push(from as u32);
                self.solid.push((k as u32, y as u32));
                self.l_min = self.l_min.min(lk);
            }
        }
        self.ring_at.push(self.rings.len() as u32);
        // Its own reading: every ring on the clean frame is the ground itself.
        struct Clean<'a>(&'a [u32]);
        impl FramePx for Clean<'_> {
            fn at(&self, k: usize) -> u32 {
                self.0[k]
            }
        }
        let clean = std::mem::take(&mut self.clean);
        self.own = self.reading(&Clean(&clean));
        self.clean = clean;
    }

    /// The text this cell's neighbourhood lays, in draw order: each box pixel and its level.
    pub(super) fn text(&self) -> &[(u32, TextLevel)] {
        &self.text
    }

    /// The frame's ground under every light, per box pixel ([`Neighbour::Known`]'s `base`).
    pub(super) fn base(&self) -> &[u32] {
        &self.base
    }

    /// Whether some box pixel's ground under the lights is not the cell's clean ground (a cursor
    /// trail's fill).
    pub(super) fn filled(&self) -> bool {
        self.base
            .iter()
            .zip(&self.clean)
            .zip(&self.opaque)
            .any(|((b, c), o)| !o && b != c)
    }

    /// Whether a glyph lays text on box pixel `k`.
    pub(super) fn has_text(&self, k: usize) -> bool {
        self.has_text[k]
    }

    /// The letter's reading on frame `px`: each solid ink pixel against its ring's channel
    /// medians, each pixel row's lower median, the worst row (infinite with no solid pixel).
    pub(super) fn reading(&mut self, px: &impl FramePx) -> f32 {
        let table = crate::color_math::relative_luminance_table();
        let lum = |c: u32| crate::color_math::relative_luminance_with(table, c);
        let mut worst = f32::INFINITY;
        let mut i = 0;
        while i < self.solid.len() {
            let row = self.solid[i].1;
            self.ratios.clear();
            while i < self.solid.len() && self.solid[i].1 == row {
                let (k, _) = self.solid[i];
                let ring = &self.rings[self.ring_at[i] as usize..self.ring_at[i + 1] as usize];
                for c in &mut self.chan {
                    c.clear();
                }
                for &kk in ring {
                    let v = px.at(kk as usize);
                    self.chan[0].push((v >> 16) as u8);
                    self.chan[1].push((v >> 8) as u8);
                    self.chan[2].push(v as u8);
                }
                // Each channel's median, the upper one of an even count (the judges').
                let mut g = 0u32;
                for (c, sh) in self.chan.iter_mut().zip([16u32, 8, 0]) {
                    let mid = c.len() / 2;
                    g |= u32::from(*c.select_nth_unstable(mid).1) << sh;
                }
                let (li, lg) = (lum(px.at(k as usize) & 0x00FF_FFFF), lum(g));
                self.ratios.push((li.max(lg) + 0.05) / (li.min(lg) + 0.05));
                i += 1;
            }
            let mid = (self.ratios.len() - 1) / 2;
            worst = worst.min(*self.ratios.select_nth_unstable_by(mid, f32::total_cmp).1);
        }
        worst
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: u32 = 0x0000_2B36;
    const INK: u32 = 0x0083_9496;
    /// Two lights at the page's lightness in other hues: each channel's median of the two (the
    /// upper of an even count) is brighter than either.
    const WARM: u32 = 0x0050_2B36;
    const TEAL: u32 = 0x0000_4A36;

    fn ratio(a: u32, b: u32) -> f32 {
        let t = crate::color_math::relative_luminance_table();
        let (la, lb) = (
            crate::color_math::relative_luminance_with(t, a),
            crate::color_math::relative_luminance_with(t, b),
        );
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    struct Px<'a>(&'a [u32]);
    impl FramePx for Px<'_> {
        fn at(&self, k: usize) -> u32 {
            self.0[k]
        }
    }

    /// The judges' reading, written out: each solid pixel (`solid`) against each channel's
    /// median (the upper of an even count) of the frame on its nearest ring of `ground`, each
    /// row's lower median, the worst row.
    fn judged(frame: &[u32], w: usize, solid: &[usize], ground: &[bool]) -> f32 {
        let mut rows: Vec<(usize, f32)> = Vec::new();
        for &k in solid {
            let (x, y) = ((k % w) as i64, (k / w) as i64);
            let h = (frame.len() / w) as i64;
            let ring = (1..=2i64).find_map(|d| {
                let px: Vec<u32> = (-d..=d)
                    .flat_map(|dy| (-d..=d).map(move |dx| (dx, dy)))
                    .filter(|&(dx, dy)| dx.abs().max(dy.abs()) == d)
                    .map(|(dx, dy)| (x + dx, y + dy))
                    .filter(|&(xx, yy)| xx >= 0 && yy >= 0 && xx < w as i64 && yy < h)
                    .map(|(xx, yy)| yy as usize * w + xx as usize)
                    .filter(|&kk| ground[kk])
                    .map(|kk| frame[kk])
                    .collect();
                (!px.is_empty()).then_some(px)
            });
            let Some(ring) = ring else { continue };
            let median = [16u32, 8, 0].iter().fold(0, |out, &sh| {
                let mut c: Vec<u32> = ring.iter().map(|p| (p >> sh) & 0xff).collect();
                c.sort_unstable();
                out | c[c.len() / 2] << sh
            });
            rows.push((y as usize, ratio(frame[k], median)));
        }
        let mut worst = f32::INFINITY;
        for line in rows.chunk_by(|a, b| a.0 == b.0) {
            let mut r: Vec<f32> = line.iter().map(|p| p.1).collect();
            r.sort_by(f32::total_cmp);
            worst = worst.min(r[(r.len() - 1) / 2]);
        }
        worst
    }

    /// A STROKE READS AGAINST ITS RING'S CHANNEL MEDIANS, AS THE JUDGES READ IT: a cell read from
    /// its glyph's own raster finds the judges' solid ink and rings (the nearest ring of its
    /// ground, a neighbour's glyph and a block caret's cell no ground), reads its own reading on
    /// the clean frame (the ink against the page), and on a frame whose ground takes a warm light
    /// left of the stroke and a teal one right of it, at the page's lightness, it reads exactly
    /// what the judges read: under the reading against either light alone, each channel's median
    /// of the two being brighter than both (round 25: Nyan's `p` at 96.6% of its own reading on
    /// Solarized Dark, which no bound on the grounds' luminance saw).
    #[test]
    fn a_stroke_reads_against_its_ring_s_channel_medians() {
        let (cw, ch, r) = (6usize, 10usize, 2usize);
        let (w, h) = (cw + 2 * r, ch + 2 * r);
        // A one-pixel stem, full coverage, rows 2..8 of its cell.
        let mut stem = vec![0u8; cw * ch];
        for y in 2..8 {
            stem[y * cw + 2] = 255;
        }
        let stem = aterm_render::GlyphRaster::placed(&stem, cw, (0, 0)).expect("ink");
        // The neighbour to the right lays a pixel of ink at its left edge, beside the stem's
        // ring; the neighbour below is a block caret's fill.
        let mut dot = vec![0u8; cw * ch];
        dot[4 * cw] = 255;
        let dot = aterm_render::GlyphRaster::placed(&dot, cw, (0, 0)).expect("ink");
        let blank = Neighbour::Known {
            ground: PAGE,
            base: PAGE,
            glyph: None,
        };
        let mut cells = [blank; 9];
        cells[4] = Neighbour::Known {
            ground: PAGE,
            base: PAGE,
            glyph: Some((&stem, INK)),
        };
        cells[5] = Neighbour::Known {
            ground: PAGE,
            base: PAGE,
            glyph: Some((&dot, INK)),
        };
        cells[7] = Neighbour::Opaque;
        let mut cell = RingCell::EMPTY;
        cell.read((w, h), (cw, ch), r, &cells, false);
        let own = ratio(INK, PAGE);
        assert!(
            (cell.own - own).abs() < 1e-4,
            "its own reading {} is the ink against the page {own}",
            cell.own
        );
        let solid: Vec<usize> = (0..w * h)
            .filter(|&k| {
                let (x, y) = (k % w, k / w);
                x == r + 2 && (r + 2..r + 8).contains(&y)
            })
            .collect();
        assert_eq!(
            cell.solid
                .iter()
                .map(|&(k, _)| k as usize)
                .collect::<Vec<_>>(),
            solid,
            "the solid ink is the stem's"
        );
        let ground: Vec<bool> = (0..w * h).map(|k| cell.ground[k]).collect();
        let dot_px = (r + 4) * w + r + cw;
        assert!(!ground[dot_px], "a neighbour's ink is no ground");
        assert!(
            !ground[(r + ch) * w + r + 2],
            "a block caret's cell is no ground"
        );
        // The ground lit warm up to the stem's column and teal past it.
        let mut frame = cell.clean.clone();
        for (k, px) in frame.iter_mut().enumerate() {
            if ground[k] {
                *px = if k % w <= r + 2 { WARM } else { TEAL };
            }
        }
        let read = cell.reading(&Px(&frame));
        let want = judged(&frame, w, &solid, &ground);
        assert!(
            (read - want).abs() < 1e-5,
            "the cell reads {read}, the judges {want}"
        );
        let alone = ratio(INK, WARM).min(ratio(INK, TEAL));
        assert!(
            read < alone,
            "fixture: the ring's medians read brighter than either light ({read} against {alone})"
        );
    }
}
