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
//! worst row. A letter on a dim page, its own reading (the same judge read on the clean frame)
//! under AA, keeps [`OWN_SHARE`] of that reading with a level in hand, and one at AA or over and
//! under the floor keeps AA with the level ([`super::raster_bar`]).
//!
//! [`RingCell`] holds that reading for one cell from the glyph's own pixels (the face's raster,
//! [`aterm_render::GlyphInk::raster`], laid as the renderer lays text, [`TextLevel`]) and its
//! neighbours' (whose glyphs reach its ring box and whose grounds bound its ring), so the letter
//! guard keeps exactly what the judge reads: where the lights are a glow's edge at the page's
//! lightness in another hue, the ring's channel medians read brighter than either, and a bound on
//! the grounds' luminance alone missed it (round 25, Nyan's `p` at 96.6% of its own reading on
//! Solarized Dark's wrapped line).
//!
//! The judges' law takes the lower of that reading and the cell's whole reading
//! (`trail-contrast`'s `contrast`, [`RingCell::whole`]): the median of its core ink (each pixel of
//! the clean glyph [`CORE_SHARE`] of the way from the ground to its extreme, in luminance) against
//! the median of its bed (each pixel the clean frame shows the ground), channel by channel on the
//! frame. The ring reads a stroke against what lies beside it, so a tiny glyph's few solid pixels
//! keep their whole ratio whatever burns elsewhere in the cell: a `.` on paper, a flame standing
//! over it, read 14.03 round its strokes and 2.49 of its own 3.90 by its core against its bed (the
//! Cursor card's `README.md` after its Tab).
//!
//! [`SOLID_SHARE`]: super::SOLID_SHARE
//! [`OWN_SHARE`]: super::OWN_SHARE

use aterm_render::glyph_ink::CORE_SHARE;
use aterm_render::{GlyphRaster, TextLevel};
use std::sync::Arc;

/// A face's rasters by glyph index ([`aterm_render::GlyphInk::raster_table`]).
pub(super) type RasterTable = Arc<[Option<GlyphRaster>]>;

/// One cell of a judged letter's neighbourhood (its own cell and the eight round it), as the
/// letter's ring box meets it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Neighbour {
    /// A cell whose pixels the guard knows: its ground, what the frame lays on it under every
    /// light (`base`: the ground, or the cursor trail's fill of the cell, which the clean frame
    /// does not draw) and, for a lettered cell, its glyph's index in the face's table and its
    /// colour.
    Known {
        ground: u32,
        base: u32,
        glyph: Option<(u8, u32)>,
    },
    /// A cell whose pixels it does not (a glyph the face's table holds no raster for, a slanted
    /// or decorated one, a block caret's fill): no pixel of it reads as the letter's ground.
    Opaque,
}

/// A ring box's frame pixel, read by [`RingCell::holds`].
pub(super) trait FramePx {
    /// The frame's colour at box pixel `k`.
    fn at(&self, k: usize) -> u32;
}

/// How many `(fg, ground)` pairs [`Levels`] keeps: a frame's lit rows of words, each word in its
/// own ink. At eight, a flood of the gallery's words, every one a colour of its own, evicted a way
/// a read: the profile of a GitHub Light flood held 435 of the 2308 main-thread samples the guard
/// took in finding and filling ways (after-land round 3, whole(c)).
const LEVEL_WAYS: usize = 64;

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

/// The most pixels one solid pixel's ring reads: the Chebyshev ring [`super::RING`] px out. The
/// longest list [`median_of`] orders; a longer one (a core or a bed) it counts.
const RING_MOST: usize = 8 * super::RING as usize;

/// Everything [`RingCell::read`] reads a neighbourhood from, and so all a [`RingRead`] depends on
/// but the face's table, which [`RingCell::reads`] are kept for.
#[derive(Clone, Copy, PartialEq, Eq)]
struct ReadKey {
    /// The ring box's size, the cell's size, the ring, px.
    sizes: [u32; 5],
    corrected: bool,
    cells: [Neighbour; 9],
}

impl ReadKey {
    /// The slot of [`RingCell::reads`] this key lies in, the first of its [`READ_WAYS`].
    fn slot(&self) -> usize {
        let mut h = u64::from(self.corrected);
        for v in self.sizes {
            h = super::fold(h, u64::from(v));
        }
        for cell in &self.cells {
            let (a, b) = match *cell {
                Neighbour::Opaque => (u64::MAX, u64::MAX),
                Neighbour::Known {
                    ground,
                    base,
                    glyph,
                } => (
                    u64::from(ground) | u64::from(base) << 32,
                    glyph.map_or(u64::MAX, |(i, fg)| u64::from(i) | u64::from(fg) << 8),
                ),
            };
            h = super::fold(super::fold(h, a), b);
        }
        ((h ^ (h >> 32)) as usize % (READ_SLOTS / READ_WAYS)) * READ_WAYS
    }
}

/// How many neighbourhoods a [`RingCell`] keeps read, in sets of [`READ_WAYS`]. A letter stands in
/// the same neighbourhood frame after frame while the lights over it move, and a line's letters
/// share many, so a trail's cells are read from these; at this many a neighbourhood that does not
/// fit is read again, as all were. The bound is in reads and a read's size follows its cell's
/// pixels, so the bytes follow the font: holding 107 to 118 reads of a typed Fire replay, DejaVu
/// Sans Mono, it retained 0.16 MB at 16 px, 0.34 MB at 22 px, 1.31 MB at 48 px and 3.81 MB at 144 px
/// (2026-11-05; the slots counted, an allocator's overhead not), until the face's table changes or
/// its guard goes.
const READ_SLOTS: usize = 128;
/// How many of [`READ_SLOTS`] one key can lie in, the latest read first.
const READ_WAYS: usize = 4;

/// One neighbourhood as the judge reads it ([`self`]): what its letter's readings take from it,
/// whichever frame's lights lie over it, wherever on the page it stands. Kept for as long as
/// [`RingCell::reads`] holds it, so its lists hold no room to grow.
struct RingRead {
    /// The text its glyphs lay on the pixels a reading reads, in the renderer's draw order: the
    /// box pixel and its level.
    text: Box<[(u32, TextLevel)]>,
    /// The pixels a reading reads, each once and in order, where a glyph lays text: the box
    /// pixel, the frame's ground under every light there ([`Neighbour::Known`]'s `base`), and
    /// that with the text laid on it.
    text_px: Box<[(u32, u32, u32)]>,
    /// The pixels a reading reads where no glyph lays text: the box pixel, and the frame's ground
    /// under every light there.
    plain_px: Box<[(u32, u32)]>,
    /// The solid ink pixels, row-major: each box pixel, its row, and where its ring starts in
    /// [`Self::rings`] (`ring_at[i] .. ring_at[i + 1]`).
    solid: Box<[(u32, u32)]>,
    ring_at: Box<[u32]>,
    rings: Box<[u32]>,
    /// The letter's own reading (the judge on the clean frame); infinite where no solid pixel has
    /// ground within the ring.
    own: f32,
    /// The least luminance of the clean solid ink.
    l_min: f32,
    /// Whether some ring pixel holds text (a glyph's faint rim that rounds to the ground).
    ring_text: bool,
    /// Whether some box pixel's colour on the clean frame is not what the frame lays there under
    /// every light: a cursor trail's fill, or a glyph's ink.
    filled: bool,
    /// The cell's core ink and its bed, as box pixels ([`RingCell::whole`]).
    core: Box<[u32]>,
    bed: Box<[u32]>,
    /// The lower of the whole reading on the clean frame and [`Self::own`], the judges' own
    /// reading, whose [`super::raster_bar`] the whole reading keeps ([`RingCell::whole_holds`]);
    /// and the luminance of the clean core's median ink.
    whole_own: f32,
    l_core: f32,
}

impl RingRead {
    /// The read of a neighbourhood whose own cell the guard does not know: nothing to read.
    fn nothing() -> Self {
        Self {
            text: Box::default(),
            text_px: Box::default(),
            plain_px: Box::default(),
            solid: Box::default(),
            ring_at: Box::new([0]),
            rings: Box::default(),
            own: f32::INFINITY,
            l_min: f32::INFINITY,
            ring_text: false,
            filled: false,
            core: Box::default(),
            bed: Box::default(),
            whole_own: f32::INFINITY,
            l_core: f32::INFINITY,
        }
    }
}

/// One lettered cell read as the judge reads it ([`self`]): the read of its neighbourhood, and the
/// neighbourhoods kept read for the next.
pub(super) struct RingCell {
    /// The neighbourhood last read. [`Self::read`] comes first.
    current: Option<Arc<RingRead>>,
    /// The table every kept read's glyphs were read from: a read is only as good as its rasters,
    /// so another table starts them over.
    table: Option<RasterTable>,
    /// The neighbourhoods kept read, [`READ_SLOTS`] of them once one is, each in one of the
    /// [`READ_WAYS`] its key names ([`ReadKey::slot`]), those the latest read first.
    reads: Vec<Option<(ReadKey, Arc<RingRead>)>>,
    /// The letter's own reading, as the read last read holds it.
    pub(super) own: f32,
    /// The least luminance of its clean solid ink.
    pub(super) l_min: f32,
    /// Whether some ring pixel holds text.
    pub(super) ring_text: bool,
    /// Scratch of a read: the clean frame's colour per box pixel (meaningless where
    /// [`Self::opaque`]), whether the guard knows a box pixel, whether the clean frame shows the
    /// letter's ground there, and whether a glyph lays text there.
    clean: Vec<u32>,
    opaque: Vec<bool>,
    ground: Vec<bool>,
    has_text: Vec<bool>,
    /// Scratch: a row's ratios.
    ratios: Vec<f32>,
    levels: Levels,
    /// A test's negative control: the whole reading keeps no bar, each letter read by its ring
    /// alone, as before after-land's A1 ([`super::CursorGlow::set_letter_guard_whole`]).
    #[cfg(any(test, debug_assertions))]
    pub(super) whole_off: bool,
    /// A test's count of the neighbourhoods read anew, not taken from [`Self::reads`].
    #[cfg(test)]
    pub(super) anew: usize,
}

impl RingCell {
    pub(super) const EMPTY: Self = Self {
        current: None,
        table: None,
        reads: Vec::new(),
        own: f32::INFINITY,
        l_min: f32::INFINITY,
        ring_text: false,
        clean: Vec::new(),
        opaque: Vec::new(),
        ground: Vec::new(),
        has_text: Vec::new(),
        ratios: Vec::new(),
        levels: Levels {
            ways: Vec::new(),
            next: 0,
        },
        #[cfg(any(test, debug_assertions))]
        whole_off: false,
        #[cfg(test)]
        anew: 0,
    };

    /// Read the cell whose ring box is `w` x `h` px, its cell `cw` x `ch` px at `(ring, ring)` in
    /// it, from its neighbourhood `cells` (row by row from the row above, each row left to right:
    /// the renderer's draw order), its own at `cells[4]`, their glyphs from the face's `table`,
    /// and the renderer's blend (`corrected`). A neighbourhood read before from this table is
    /// taken as it was read: a read depends on nothing else.
    pub(super) fn read(
        &mut self,
        (w, h): (usize, usize),
        (cw, ch): (usize, usize),
        ring: usize,
        cells: &[Neighbour; 9],
        (table, corrected): (&RasterTable, bool),
    ) {
        if !self.table.as_ref().is_some_and(|t| Arc::ptr_eq(t, table)) {
            self.reads.clear();
            self.table = Some(Arc::clone(table));
        }
        if self.reads.is_empty() {
            self.reads.resize_with(READ_SLOTS, || None);
        }
        let key = ReadKey {
            sizes: [w, h, cw, ch, ring].map(|v| u32::try_from(v).unwrap_or(u32::MAX)),
            corrected,
            cells: *cells,
        };
        let slot = key.slot();
        let held = self.reads[slot..slot + READ_WAYS]
            .iter()
            .position(|way| way.as_ref().is_some_and(|(k, _)| *k == key));
        let read = match held {
            Some(way) => {
                self.reads[slot..=slot + way].rotate_right(1);
                self.reads[slot].as_ref().map(|(_, read)| Arc::clone(read))
            }
            None => None,
        };
        let read = read.unwrap_or_else(|| {
            let read = Arc::new(self.read_anew((w, h), (cw, ch), ring, cells, table, corrected));
            self.reads[slot..slot + READ_WAYS].rotate_right(1);
            self.reads[slot] = Some((key, Arc::clone(&read)));
            read
        });
        (self.own, self.l_min, self.ring_text) = (read.own, read.l_min, read.ring_text);
        self.current = Some(read);
    }

    /// [`Self::read`]'s work.
    fn read_anew(
        &mut self,
        (w, h): (usize, usize),
        (cw, ch): (usize, usize),
        ring: usize,
        cells: &[Neighbour; 9],
        table: &RasterTable,
        corrected: bool,
    ) -> RingRead {
        #[cfg(test)]
        {
            self.anew += 1;
        }
        let lums = crate::color_math::relative_luminance_table();
        let lum = |c: u32| crate::color_math::relative_luminance_with(lums, c);
        let ratio = |a: f32, b: f32| (a.max(b) + 0.05) / (a.min(b) + 0.05);
        let Neighbour::Known { ground, .. } = cells[4] else {
            return RingRead::nothing();
        };
        let n = w * h;
        let mut base = vec![ground; n];
        self.clean.clear();
        self.clean.resize(n, ground);
        self.opaque.clear();
        self.opaque.resize(n, false);
        self.has_text.clear();
        self.has_text.resize(n, false);
        let mut text = Vec::new();
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
                        ground: g, base: b, ..
                    } => (self.clean[k], base[k]) = (g, b),
                    Neighbour::Opaque => self.opaque[k] = true,
                }
            }
        }
        // Each glyph laid over its cell's ground, in draw order.
        for (i, cell) in cells.iter().enumerate() {
            let Neighbour::Known {
                ground: g,
                glyph: Some((glyph, fg)),
                ..
            } = *cell
            else {
                continue;
            };
            let Some(raster) = table.get(usize::from(glyph)).and_then(Option::as_ref) else {
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
                    text.push((k as u32, level));
                }
            }
        }
        self.ground.clear();
        self.ground
            .extend((0..n).map(|k| !self.opaque[k] && self.clean[k] == ground));
        // The cell's extreme and its solid ink.
        let l_g = lum(ground);
        // The cell's own pixels in the box, row-major, each clean pixel's luminance read once
        // (into `ratios`, which [`Self::read_until`] clears before it reads its own).
        let (x_end, y_end) = ((ring + cw).min(w), (ring + ch).min(h));
        self.ratios.clear();
        let (mut extreme, mut l_extreme) = (1.0f32, l_g);
        for y in ring..y_end {
            for k in y * w + ring..y * w + x_end {
                if !self.opaque[k] {
                    let lk = lum(self.clean[k]);
                    self.ratios.push(lk);
                    let r = ratio(lk, l_g);
                    if r > extreme {
                        (extreme, l_extreme) = (r, lk);
                    }
                }
            }
        }
        let solid_at = super::SOLID_SHARE * extreme;
        let core_reach = CORE_SHARE * (l_extreme - l_g).abs();
        let (mut core, mut bed) = (Vec::new(), Vec::new());
        let (mut solid, mut ring_at, mut rings) = (Vec::new(), Vec::new(), Vec::<u32>::new());
        let mut ring_text = false;
        let mut l_min = f32::INFINITY;
        let mut li = 0;
        for y in ring..y_end {
            for x in ring..x_end {
                let k = y * w + x;
                if self.opaque[k] {
                    continue;
                }
                let lk = self.ratios[li];
                li += 1;
                if self.ground[k] {
                    bed.push(k as u32);
                } else if (lk - l_g).abs() >= core_reach {
                    core.push(k as u32);
                }
                if ratio(lk, l_g) < solid_at {
                    continue;
                }
                let (x, y) = (x as i64, y as i64);
                let from = rings.len();
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
                                    rings.push(kk as u32);
                                    ring_text |= self.has_text[kk];
                                }
                            }
                            dx += if edge { 1 } else { 2 * d };
                        }
                    }
                    if rings.len() > from {
                        break;
                    }
                }
                if rings.len() == from {
                    continue;
                }
                ring_at.push(from as u32);
                solid.push((k as u32, y as u32));
                l_min = l_min.min(lk);
            }
        }
        ring_at.push(rings.len() as u32);
        // Its own reading: every ring on the clean frame is the ground itself.
        let own = Self::read_until(
            (&solid, &ring_at, &rings),
            &Clean(&self.clean),
            f32::NEG_INFINITY,
            &mut self.ratios,
        );
        // Its whole reading on the clean frame: the bed is the pixels that frame shows the ground.
        let l_core = lum(median_of(&core, &Clean(&self.clean)));
        let whole = if core.is_empty() || bed.is_empty() {
            f32::INFINITY
        } else {
            ratio(l_core, l_g)
        };
        let filled = base
            .iter()
            .zip(&self.clean)
            .zip(&self.opaque)
            .any(|((b, c), o)| !o && b != c);
        // What a reading reads: the solid ink and its rings, and the core and its bed, each pixel
        // once.
        let mut is_read = vec![false; n];
        for &k in solid
            .iter()
            .map(|(k, _)| k)
            .chain(&rings)
            .chain(&core)
            .chain(&bed)
        {
            is_read[k as usize] = true;
        }
        text.retain(|&(k, _)| is_read[k as usize]);
        let mut on_base = base.clone();
        for &(k, level) in &text {
            let k = k as usize;
            on_base[k] = level.over(on_base[k]);
        }
        let (mut text_px, mut plain_px) = (Vec::new(), Vec::new());
        for k in (0..n).filter(|&k| is_read[k]) {
            if self.has_text[k] {
                text_px.push((k as u32, base[k], on_base[k]));
            } else {
                plain_px.push((k as u32, base[k]));
            }
        }
        RingRead {
            text: text.into_boxed_slice(),
            text_px: text_px.into_boxed_slice(),
            plain_px: plain_px.into_boxed_slice(),
            solid: solid.into_boxed_slice(),
            ring_at: ring_at.into_boxed_slice(),
            rings: rings.into_boxed_slice(),
            own,
            l_min,
            ring_text,
            filled,
            core: core.into_boxed_slice(),
            bed: bed.into_boxed_slice(),
            whole_own: whole.min(own),
            l_core,
        }
    }

    /// A test's list of the pixels a reading reads: the solid ink, then each ring, then the core
    /// and its bed ([`Self::whole`]).
    #[cfg(test)]
    pub(super) fn read_pixels(&self) -> Vec<usize> {
        let read = self.current();
        read.solid
            .iter()
            .map(|&(k, _)| k)
            .chain(read.rings.iter().copied())
            .chain(read.core.iter().copied())
            .chain(read.bed.iter().copied())
            .map(|k| k as usize)
            .collect()
    }

    /// The neighbourhood last read.
    fn current(&self) -> &RingRead {
        self.current.as_deref().expect("a cell is read first")
    }

    /// The text this cell's neighbourhood lays on the pixels a reading reads, in draw order: each
    /// box pixel and its level.
    pub(super) fn text(&self) -> &[(u32, TextLevel)] {
        &self.current().text
    }

    /// The pixels a reading reads where a glyph lays text: each box pixel, the frame's ground
    /// under every light there, and that with the text laid on it.
    pub(super) fn text_px(&self) -> &[(u32, u32, u32)] {
        &self.current().text_px
    }

    /// The pixels a reading reads where no glyph lays text: each box pixel, and the frame's
    /// ground under every light there.
    pub(super) fn plain_px(&self) -> &[(u32, u32)] {
        &self.current().plain_px
    }

    /// Whether some box pixel's colour on the clean frame is not what the frame lays there under
    /// every light: a cursor trail's fill, or a glyph's ink.
    pub(super) fn filled(&self) -> bool {
        self.current().filled
    }

    /// The letter's reading on frame `px`: each solid ink pixel against its ring's channel
    /// medians, each pixel row's lower median, the worst row (infinite with no solid pixel).
    #[cfg(test)]
    pub(super) fn reading(&mut self, px: &impl FramePx) -> f32 {
        self.read_under(px, f32::NEG_INFINITY)
    }

    /// Whether the letter's reading on frame `px` (each solid ink pixel against its ring's channel
    /// medians, each pixel row's lower median, the worst row, infinite with no solid pixel) is at
    /// least `bar`: the first row under it settles that.
    pub(super) fn holds(&mut self, px: &impl FramePx, bar: f32) -> bool {
        self.read_under(px, bar) >= bar
    }

    fn read_under(&mut self, px: &impl FramePx, stop: f32) -> f32 {
        let read = self.current.as_deref().expect("a cell is read first");
        Self::read_until(
            (&read.solid, &read.ring_at, &read.rings),
            px,
            stop,
            &mut self.ratios,
        )
    }

    /// The reading of solid ink `solid` and its rings `(ring_at, rings)` on frame `px`, or the
    /// first row's worst so far once it falls under `stop`.
    fn read_until(
        (solid, ring_at, rings): (&[(u32, u32)], &[u32], &[u32]),
        px: &impl FramePx,
        stop: f32,
        ratios: &mut Vec<f32>,
    ) -> f32 {
        let table = crate::color_math::relative_luminance_table();
        let lum = |c: u32| crate::color_math::relative_luminance_with(table, c);
        let mut worst = f32::INFINITY;
        // A stroke's pixels are mostly one colour over one ring colour: the last pair's ratio
        // stands for the next.
        let mut last = (u32::MAX, u32::MAX, 0.0f32);
        let mut i = 0;
        while i < solid.len() {
            let row = solid[i].1;
            ratios.clear();
            while i < solid.len() && solid[i].1 == row {
                let ring = &rings[ring_at[i] as usize..ring_at[i + 1] as usize];
                let g = median_of(ring, px);
                let p = px.at(solid[i].0 as usize) & 0x00FF_FFFF;
                if (p, g) != (last.0, last.1) {
                    let (li, lg) = (lum(p), lum(g));
                    last = (p, g, (li.max(lg) + 0.05) / (li.min(lg) + 0.05));
                }
                ratios.push(last.2);
                i += 1;
            }
            let mid = (ratios.len() - 1) / 2;
            worst = worst.min(*ratios.select_nth_unstable_by(mid, f32::total_cmp).1);
            if worst < stop {
                return worst;
            }
        }
        worst
    }

    /// The cell's whole reading on frame `px`: its core ink's median against its bed's (infinite
    /// with neither). It is the judge's `contrast`, the higher reading of an ordinary letter's two
    /// and the lower of a glyph so small that its core is mostly antialiased rim.
    pub(super) fn whole(&self, px: &impl FramePx) -> f32 {
        let read = self.current();
        if read.core.is_empty() || read.bed.is_empty() {
            return f32::INFINITY;
        }
        let table = crate::color_math::relative_luminance_table();
        let lum = |c: u32| crate::color_math::relative_luminance_with(table, c);
        let (li, lg) = (
            lum(median_of(&read.core, px)),
            lum(median_of(&read.bed, px)),
        );
        (li.max(lg) + 0.05) / (li.min(lg) + 0.05)
    }

    /// The bar [`Self::whole`] keeps: [`super::raster_bar`] of the lower of its reading on the
    /// clean frame and [`Self::own`], the judges' own reading; none under the test's control
    /// ([`Self::whole_off`]). Worked out at each use, not kept with the read, so the bar's own
    /// controls (`with_share_over_aa`, `with_aa_unrounded`) reach a neighbourhood read before.
    fn whole_bar(&self) -> f32 {
        #[cfg(any(test, debug_assertions))]
        if self.whole_off {
            return 0.0;
        }
        super::raster_bar(self.current().whole_own)
    }

    /// Whether frame `px` keeps the letter's whole reading at its bar.
    pub(super) fn whole_holds(&self, px: &impl FramePx) -> bool {
        self.whole(px) >= self.whole_bar()
    }

    /// Whether the whole reading keeps its bar on any frame whose lights add, lifting each pixel
    /// and no channel of the bed's median past luminance `lg`: the core's ink can only rise.
    pub(super) fn whole_clears(&self, lg: f32) -> bool {
        let read = self.current();
        read.core.is_empty()
            || read.bed.is_empty()
            || (lg < read.l_core && (read.l_core + 0.05) / (lg + 0.05) >= self.whole_bar())
    }
}

/// A clean frame's pixels, for a neighbourhood's own reading.
struct Clean<'a>(&'a [u32]);

impl FramePx for Clean<'_> {
    fn at(&self, k: usize) -> u32 {
        self.0[k]
    }
}

/// Each channel's median of frame `px` over the box pixels `at`, the upper one of an even count
/// (the judges'); `0` over none. A guarded letter reads one per solid pixel on each share it tries,
/// the heart of a typed key's emission, so pixels all alike read at once, a ring orders three short
/// lists on the stack in the pass that reads it, and a core or bed, hundreds of pixels, counts each
/// channel's values in one pass.
#[inline]
fn median_of(at: &[u32], px: &impl FramePx) -> u32 {
    let Some(&head) = at.first() else {
        return 0;
    };
    let first = px.at(head as usize) & 0x00FF_FFFF;
    let mid = at.len() / 2;
    let mut out = 0u32;
    if at.len() <= RING_MOST {
        let mut chan = [[0u8; RING_MOST]; 3];
        let mut uniform = true;
        for (i, &k) in at.iter().enumerate() {
            let v = px.at(k as usize);
            uniform &= v & 0x00FF_FFFF == first;
            chan[0][i] = (v >> 16) as u8;
            chan[1][i] = (v >> 8) as u8;
            chan[2][i] = v as u8;
        }
        if uniform {
            return first;
        }
        for (c, sh) in chan.iter_mut().zip([16u32, 8, 0]) {
            out |= u32::from(*c[..at.len()].select_nth_unstable(mid).1) << sh;
        }
        return out;
    }
    if at[1..]
        .iter()
        .all(|&k| px.at(k as usize) & 0x00FF_FFFF == first)
    {
        return first;
    }
    let mut counts = [[0u32; 256]; 3];
    for &k in at {
        let v = px.at(k as usize);
        counts[0][((v >> 16) & 0xFF) as usize] += 1;
        counts[1][((v >> 8) & 0xFF) as usize] += 1;
        counts[2][(v & 0xFF) as usize] += 1;
    }
    for (channel, sh) in counts.iter().zip([16u32, 8, 0]) {
        let mut seen = 0;
        for (value, &n) in channel.iter().enumerate() {
            seen += n as usize;
            if seen > mid {
                out |= (value as u32) << sh;
                break;
            }
        }
    }
    out
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

    /// Every part of `kept`'s read and `fresh`'s, a reading of each on a frame, and where
    /// [`RingCell::holds`] stands either side of it.
    fn same_read(kept: &mut RingCell, fresh: &mut RingCell, frame: &[u32], what: &str) {
        let (k, f) = (kept.current(), fresh.current());
        assert_eq!(k.text, f.text, "{what}: its text");
        assert_eq!(k.text_px, f.text_px, "{what}: its text pixels");
        assert_eq!(k.plain_px, f.plain_px, "{what}: its plain pixels");
        assert_eq!(k.solid, f.solid, "{what}: its solid ink");
        assert_eq!(k.ring_at, f.ring_at, "{what}: its rings' starts");
        assert_eq!(k.rings, f.rings, "{what}: its rings");
        assert_eq!(k.filled, f.filled, "{what}: whether it is filled");
        assert_eq!(k.core, f.core, "{what}: its core");
        assert_eq!(k.bed, f.bed, "{what}: its bed");
        for (a, b, part) in [
            (k.own, f.own, "own reading"),
            (k.l_min, f.l_min, "least ink"),
            (k.whole_own, f.whole_own, "whole reading's own"),
            (k.l_core, f.l_core, "core's ink"),
            (kept.own, f.own, "own reading kept"),
            (kept.l_min, f.l_min, "least ink kept"),
        ] {
            assert_eq!(a.to_bits(), b.to_bits(), "{what}: its {part}: {a} {b}");
        }
        assert_eq!(k.ring_text, f.ring_text, "{what}: its ring text");
        assert_eq!(kept.ring_text, f.ring_text, "{what}: its ring text kept");
        let read = kept.reading(&Px(frame));
        assert_eq!(
            read.to_bits(),
            fresh.reading(&Px(frame)).to_bits(),
            "{what}: its reading"
        );
        for (bar, holds) in [
            (read.next_down(), true),
            (read, true),
            (read.next_up(), read.is_infinite()),
        ] {
            assert_eq!(kept.holds(&Px(frame), bar), holds, "{what}: holds {bar}");
        }
        assert_eq!(
            kept.whole(&Px(frame)).to_bits(),
            fresh.whole(&Px(frame)).to_bits(),
            "{what}: its whole reading"
        );
        assert_eq!(
            kept.whole_holds(&Px(frame)),
            fresh.whole_holds(&Px(frame)),
            "{what}: whether its whole reading holds"
        );
    }

    /// A NEIGHBOURHOOD READ BEFORE IS TAKEN AS IT WAS READ, AND ONLY THEN. Over neighbourhoods
    /// differing in one part each (a cell's ground, base, glyph, colour, or being opaque, a box or
    /// cell or ring size, the blend), read twice, more of them than are kept, and on another
    /// table at the same glyph indices, a cell that keeps its reads holds exactly what a new one
    /// reads, and its reads stay within [`READ_SLOTS`]; the neighbourhood just read, and most of a
    /// second pass, are not read again. NEGATIVE CONTROLS (2026-11-05), each fails here on that
    /// part of the key's neighbourhood: a key blind to `base` on its text pixels, to the blend or a
    /// glyph's colour on its text, and a table change that keeps the reads on another table's text.
    #[test]
    fn a_neighbourhood_read_before_is_taken_as_it_was_read() {
        let (cw, ch, r) = (6usize, 10usize, 2usize);
        let raster = |cols: [usize; 2], rows: std::ops::Range<usize>, cov: u8| {
            let mut c = vec![0u8; cw * ch];
            for y in rows {
                for x in cols {
                    c[y * cw + x] = cov;
                }
            }
            c[(ch - 1) * cw + cols[0]] = 90;
            Some(GlyphRaster::placed(&c, cw, (0, 0)).expect("ink"))
        };
        let table: RasterTable = vec![
            raster([2, 2], 2..8, 255),
            raster([0, 5], 1..9, 200),
            raster([1, 3], 3..6, 255),
        ]
        .into();
        let other: RasterTable = vec![
            raster([3, 4], 1..7, 255),
            raster([1, 1], 0..4, 160),
            raster([0, 2], 5..9, 255),
        ]
        .into();
        let known = |ground, glyph| Neighbour::Known {
            ground,
            base: ground,
            glyph,
        };
        let mut fixture = [known(PAGE, None); 9];
        fixture[4] = known(PAGE, Some((0, INK)));
        fixture[5] = known(PAGE, Some((1, INK)));
        fixture[7] = known(WARM, None);
        let sizes = [
            (cw + 2 * r, ch + 2 * r, cw, ch, r),
            (cw + 2, ch + 2, cw, ch, 1),
            (cw + 2 * r, ch + 2 * r + 2, cw, ch + 2, r),
        ];
        type Hood = ((usize, usize, usize, usize, usize), [Neighbour; 9], bool);
        let mut hoods: Vec<Hood> = vec![(sizes[0], fixture, false)];
        hoods.extend(sizes[1..].iter().map(|&s| (s, fixture, false)));
        hoods.push((sizes[0], fixture, true));
        for at in 0..9 {
            let Neighbour::Known {
                ground,
                base,
                glyph,
            } = fixture[at]
            else {
                continue;
            };
            for changed in [
                Neighbour::Known {
                    ground: TEAL,
                    base,
                    glyph,
                },
                Neighbour::Known {
                    ground,
                    base: TEAL,
                    glyph,
                },
                Neighbour::Known {
                    ground,
                    base,
                    glyph: Some((2, INK)),
                },
                Neighbour::Known {
                    ground,
                    base,
                    glyph: Some((0, WARM)),
                },
                Neighbour::Opaque,
            ] {
                let mut cells = fixture;
                cells[at] = changed;
                hoods.push((sizes[0], cells, false));
            }
        }
        // A frame of some other colours, for the readings.
        let mut x = 0x9E37_79B9u32;
        let mut frame = vec![0u32; 256 * 16];
        for px in &mut frame {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *px = [PAGE, WARM, TEAL, INK][(x % 4) as usize];
        }
        let mut kept = RingCell::EMPTY;
        let read = |cell: &mut RingCell, hood: &Hood, table: &RasterTable| {
            let &((w, h, cw, ch, r), cells, corrected) = hood;
            cell.read((w, h), (cw, ch), r, &cells, (table, corrected));
        };
        let mut first_pass = 0;
        for pass in 0..2 {
            if pass == 1 {
                first_pass = kept.anew;
            }
            for (i, hood) in hoods.iter().enumerate() {
                read(&mut kept, hood, &table);
                let mut fresh = RingCell::EMPTY;
                read(&mut fresh, hood, &table);
                same_read(
                    &mut kept,
                    &mut fresh,
                    &frame,
                    &format!("pass {pass} hood {i}"),
                );
                let anew = kept.anew;
                read(&mut kept, hood, &table);
                assert_eq!(
                    kept.anew, anew,
                    "hood {i}: read again as soon as it was read"
                );
            }
        }
        let again = kept.anew - first_pass;
        assert!(
            again < hoods.len() / 2,
            "a second pass read {again} of {} neighbourhoods again",
            hoods.len()
        );
        // Another table: the same key, other rasters.
        let hood = &hoods[0];
        read(&mut kept, hood, &table);
        let of_table = kept.current().text.clone();
        read(&mut kept, hood, &other);
        let mut fresh = RingCell::EMPTY;
        read(&mut fresh, hood, &other);
        same_read(&mut kept, &mut fresh, &frame, "another table");
        assert_ne!(kept.current().text, of_table, "fixture: the tables differ");
        // More neighbourhoods than are kept.
        for i in 0..3 * READ_SLOTS as u32 {
            let mut cells = fixture;
            cells[0] = known(0x0010_0000 + i, None);
            let hood = (sizes[0], cells, false);
            read(&mut kept, &hood, &other);
            let mut fresh = RingCell::EMPTY;
            read(&mut fresh, &hood, &other);
            same_read(&mut kept, &mut fresh, &frame, &format!("sweep {i}"));
            assert_eq!(kept.reads.len(), READ_SLOTS, "its reads stay bounded");
        }
    }

    /// [`RingCell::holds`] STOPS ONLY AT A ROW UNDER THE BAR: over a stem whose rows read lower
    /// and lower, at each row's reading and either side of it, it answers as the whole reading
    /// does. NEGATIVE CONTROL (2026-11-05): a stop at `worst <= stop` takes a first row at the bar
    /// for the reading and fails here.
    #[test]
    fn holds_answers_as_the_whole_reading_at_every_row_s_reading() {
        let (cw, ch, r) = (6usize, 10usize, 2usize);
        let (w, h) = (cw + 2 * r, ch + 2 * r);
        let mut stem = vec![0u8; cw * ch];
        for y in 2..8 {
            stem[y * cw + 2] = 255;
        }
        let table: RasterTable = vec![GlyphRaster::placed(&stem, cw, (0, 0))].into();
        let mut cells = [Neighbour::Known {
            ground: PAGE,
            base: PAGE,
            glyph: None,
        }; 9];
        cells[4] = Neighbour::Known {
            ground: PAGE,
            base: PAGE,
            glyph: Some((0, 0x00FF_FFFF)),
        };
        let mut cell = RingCell::EMPTY;
        cell.read((w, h), (cw, ch), r, &cells, (&table, false));
        let mut frame = vec![PAGE; w * h];
        let inks: Vec<u32> = (0..6u32).map(|i| 0x00F0_F0F0 - i * 0x0020_2020).collect();
        for (i, ink) in inks.iter().enumerate() {
            frame[(r + 2 + i) * w + r + 2] = *ink;
        }
        let reading = cell.reading(&Px(&frame));
        assert_eq!(
            reading.to_bits(),
            ratio(inks[5], PAGE).to_bits(),
            "the last row is the worst"
        );
        for bar in inks.iter().map(|ink| ratio(*ink, PAGE)) {
            for bar in [bar.next_down(), bar, bar.next_up()] {
                assert_eq!(
                    cell.holds(&Px(&frame), bar),
                    reading >= bar,
                    "a bar of {bar} on a reading of {reading}"
                );
            }
        }
    }

    /// EACH CHANNEL'S MEDIAN IS THE ONE A SORTED LIST READS: the upper one of an even count, each
    /// channel its own, on both sides of the length where the reader stops ordering and counts,
    /// with values from all alike to all different.
    #[test]
    fn each_channel_s_median_is_the_one_a_sorted_list_reads() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u32
        };
        let lens = (1..=RING_MOST + 2).chain([255, 256, 257, 720, 2880]);
        for len in lens {
            for spread in [1u32, 2, 7, 256] {
                for _ in 0..8 {
                    let frame: Vec<u32> = (0..len)
                        .map(|_| {
                            let (a, b, c) = (next() % spread, next() % spread, next() % spread);
                            (next() & 0xFF00_0000) | a << 16 | b << 8 | c
                        })
                        .collect();
                    let at: Vec<u32> = (0..len as u32).rev().collect();
                    let mut want = 0;
                    for sh in [16u32, 8, 0] {
                        let mut channel: Vec<u32> =
                            frame.iter().map(|v| (v >> sh) & 0xFF).collect();
                        channel.sort_unstable();
                        want |= channel[len / 2] << sh;
                    }
                    assert_eq!(
                        median_of(&at, &Px(&frame)),
                        want,
                        "{len} pixels of {spread} values"
                    );
                }
            }
        }
        assert_eq!(median_of(&[], &Px(&[])), 0, "over none");
    }

    /// A TINY GLYPH'S WHOLE READING IS ITS CORE AGAINST ITS BED, AS THE JUDGES READ IT: a `.` on
    /// paper. Its solid ink keeps its ring on a frame that lights the cell's upper rows, so the
    /// ring's reading stays its own; the whole reading (the core's channel medians against the
    /// bed's) falls to what `trail-contrast`'s `contrast` computes, under its bar, and the frame
    /// fails [`RingCell::whole_holds`]. Its bar is its own share, the dot's core reading under
    /// the floor though its solid ink reads far over it.
    #[test]
    fn a_tiny_glyph_reads_its_core_against_its_bed() {
        const PAPER: u32 = 0x00FF_FFFF;
        const DOT: u32 = 0x001F_2328;
        const FLAME: u32 = 0x00FF_C080;
        let (cw, ch, r) = (6usize, 10usize, 2usize);
        let (w, h) = (cw + 2 * r, ch + 2 * r);
        let mut dot = vec![0u8; cw * ch];
        // The real `.` of the Cursor card's `README.md`, its coverage read off the clean frame.
        dot[7 * cw + 2..7 * cw + 4].copy_from_slice(&[195, 30]);
        dot[8 * cw + 1..8 * cw + 4].copy_from_slice(&[243, 255, 188]);
        dot[9 * cw + 1..9 * cw + 4].copy_from_slice(&[205, 252, 100]);
        let table: RasterTable =
            vec![Some(GlyphRaster::placed(&dot, cw, (0, 0)).expect("ink"))].into();
        let blank = Neighbour::Known {
            ground: PAPER,
            base: PAPER,
            glyph: None,
        };
        let mut cells = [blank; 9];
        cells[4] = Neighbour::Known {
            ground: PAPER,
            base: PAPER,
            glyph: Some((0, DOT)),
        };
        let mut cell = RingCell::EMPTY;
        cell.read((w, h), (cw, ch), r, &cells, (&table, false));
        let clean = cell.clean.clone();
        assert_eq!(
            cell.whole_bar(),
            super::super::raster_bar(cell.whole(&Px(&clean)).min(cell.own)),
            "the bar is what the whole reading reads on the clean frame"
        );
        let own = cell.reading(&Px(&clean));
        assert!(
            own > 4.6 && cell.whole_bar() < 4.6,
            "fixture: the ring reads {own}, the core's bar is {}",
            cell.whole_bar()
        );
        assert!(
            cell.whole_holds(&Px(&clean)),
            "the clean frame keeps its bar"
        );
        // The upper half of the cell burns; the row over the dot and everything lower does not.
        let mut frame = clean.clone();
        for (k, px) in frame.iter_mut().enumerate() {
            if cell.ground[k] && k / w < r + 6 {
                *px = FLAME;
            }
        }
        assert_eq!(cell.reading(&Px(&frame)), own, "its ring is untouched");
        let core: Vec<u32> = cell
            .current()
            .core
            .iter()
            .map(|&k| frame[k as usize])
            .collect();
        let bed: Vec<u32> = cell
            .current()
            .bed
            .iter()
            .map(|&k| frame[k as usize])
            .collect();
        let med = |px: &[u32]| {
            [16u32, 8, 0].iter().fold(0, |out, &sh| {
                let mut c: Vec<u32> = px.iter().map(|p| (p >> sh) & 0xff).collect();
                c.sort_unstable();
                out | c[c.len() / 2] << sh
            })
        };
        let want = ratio(med(&core), med(&bed));
        let read = cell.whole(&Px(&frame));
        assert!(
            (read - want).abs() < 1e-5,
            "the cell's whole reading {read}, the judges' {want}"
        );
        assert!(
            !cell.whole_holds(&Px(&frame)),
            "a flame over the cell takes its bed: the whole reading {read} against its bar {}",
            cell.whole_bar()
        );
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
        let stem = GlyphRaster::placed(&stem, cw, (0, 0)).expect("ink");
        // The neighbour to the right lays a pixel of ink at its left edge, beside the stem's
        // ring; the neighbour below is a block caret's fill.
        let mut dot = vec![0u8; cw * ch];
        dot[4 * cw] = 255;
        let dot = GlyphRaster::placed(&dot, cw, (0, 0)).expect("ink");
        let table: RasterTable = vec![Some(stem), Some(dot)].into();
        let blank = Neighbour::Known {
            ground: PAGE,
            base: PAGE,
            glyph: None,
        };
        let mut cells = [blank; 9];
        cells[4] = Neighbour::Known {
            ground: PAGE,
            base: PAGE,
            glyph: Some((0, INK)),
        };
        cells[5] = Neighbour::Known {
            ground: PAGE,
            base: PAGE,
            glyph: Some((1, INK)),
        };
        cells[7] = Neighbour::Opaque;
        let mut cell = RingCell::EMPTY;
        cell.read((w, h), (cw, ch), r, &cells, (&table, false));
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
            cell.current()
                .solid
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
