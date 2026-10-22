// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Emoji grapheme clusters the colour face shapes to more than one glyph.
//!
//! A cluster (ZWJ sequence, flag, skin tone, keycap) the colour face ligates is drawn as
//! that glyph ([`GlyphClass::RgbaGid`]). One it shapes to several colour glyphs (ZWJ
//! members it does not carry, or a flag's regional-indicator letters drawn in colour) is
//! composited into one image fitted to the cluster's two cells, keyed
//! [`GlyphClass::RgbaCluster`]: both backends pull it from the shared glyph cache, and it
//! costs the atlas one emoji.
//!
//! [`GlyphClass::RgbaGid`]: crate::GlyphClass::RgbaGid
//! [`GlyphClass::RgbaCluster`]: crate::GlyphClass::RgbaCluster

use aterm_hash::FxHashMap;

use crate::TwoGenTextCache;

/// What the colour face shaped an emoji grapheme cluster to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClusterShape {
    /// One colour glyph: colour-chain face `face` ligated the whole cluster.
    Glyph { face: u8, gid: u16 },
    /// Several colour glyphs, composited: the id names a [`ClusterRun`].
    Run(u32),
}

/// One colour glyph of a [`ClusterRun`], in font units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RunGlyph {
    pub(crate) gid: u16,
    /// Pen position plus the shaper's x offset.
    pub(crate) x: i32,
    /// The shaper's y offset (up is positive).
    pub(crate) y: i32,
    pub(crate) advance: i32,
}

/// The colour glyphs a cluster shaped to, laid out as the shaper placed them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClusterRun {
    /// The colour-chain face whose glyph ids these are (the renderer's
    /// `Renderer::color_pick` order); [`layout_run`] leaves it `0`, the head.
    pub(crate) face: u8,
    pub(crate) glyphs: Box<[RunGlyph]>,
    /// The run's total advance.
    pub(crate) width: i32,
    /// The height the run is fitted by: its widest glyph's advance (colour
    /// emoji are square).
    pub(crate) em: i32,
}

/// The most colour glyphs one composite holds. A ZWJ family has four; past
/// eight, members fitted into two cells are too small to read, and a PTY could
/// otherwise make one cluster's run arbitrarily long.
pub(crate) const MAX_RUN_GLYPHS: usize = 8;

/// Lay out a shaped cluster as a run, from its glyphs' `(glyph id, x advance, x
/// offset, y offset)` in order. A glyph the colour face can draw joins the run;
/// one it cannot draw must advance nothing (a hidden joiner or selector),
/// otherwise the cluster is not colour and the run is refused. Refused too: a
/// run of fewer than two colour glyphs (the one-glyph case is
/// [`ClusterShape::Glyph`], and a lone base keeps its ordinary fallback) or more
/// than [`MAX_RUN_GLYPHS`].
///
/// A colour glyph that advances nothing joins only when `marks`: a face whose
/// run is painted in one canvas at the shaper's positions (COLR, see
/// `colr::rasterize_colr_run`) may stand a member by its outline alone, as
/// Segoe UI Emoji does the children of a ZWJ family; [`composite`] lays each
/// glyph in a slot as wide as its advance, so for it such a glyph is refused.
pub(crate) fn layout_run(
    glyphs: impl IntoIterator<Item = (u32, i32, i32, i32)>,
    drawable: impl Fn(u16) -> bool,
    marks: bool,
) -> Option<ClusterRun> {
    let mut run = Vec::new();
    let mut pen = 0i32;
    let mut em = 0i32;
    for (id, advance, x_offset, y_offset) in glyphs {
        let gid = u16::try_from(id).ok()?;
        if gid != 0 && drawable(gid) {
            if advance < 0 || (advance == 0 && !marks) || run.len() == MAX_RUN_GLYPHS {
                return None;
            }
            run.push(RunGlyph {
                gid,
                x: pen.checked_add(x_offset)?,
                y: y_offset,
                advance,
            });
            em = em.max(advance);
        } else if advance != 0 {
            return None;
        }
        pen = pen.checked_add(advance)?;
    }
    (run.len() >= 2 && pen > 0).then(|| ClusterRun {
        face: 0,
        glyphs: run.into(),
        width: pen,
        em,
    })
}

/// Composite `run` into straight-alpha RGBA8 fitted into `box_w × box_h`, the
/// whole run scaled by one factor so each glyph keeps its aspect. `fit` draws
/// one glyph into a `(w, h)` slot and returns its RGBA8 and size (at most the
/// slot); each is centred in its slot. `None` when no glyph drew.
pub(crate) fn composite(
    run: &ClusterRun,
    box_w: usize,
    box_h: usize,
    mut fit: impl FnMut(u16, usize, usize) -> Option<(Vec<u8>, usize, usize)>,
) -> Option<(Vec<u8>, usize, usize)> {
    let k = (box_w as f32 / run.width as f32).min(box_h as f32 / run.em as f32);
    let px = |units: i32| (units as f32 * k).round() as i32;
    let w = usize::try_from(px(run.width)).ok()?.clamp(1, box_w.max(1));
    let h = usize::try_from(px(run.em)).ok()?.clamp(1, box_h.max(1));
    let mut out = vec![0u8; w * h * 4];
    let mut drew = false;
    for g in run.glyphs.iter() {
        let slot_w = usize::try_from(px(g.advance)).ok()?.max(1);
        let Some((tile, tw, th)) = fit(g.gid, slot_w, h) else {
            continue;
        };
        let x0 = px(g.x) + (slot_w as i32 - tw as i32) / 2;
        let y0 = (h as i32 - th as i32) / 2 - px(g.y);
        over((&mut out, w, h), (&tile, tw, th), x0, y0);
        drew = true;
    }
    drew.then_some((out, w, h))
}

/// Straight-alpha source-over of `src` onto `dst` at `(x0, y0)`, clipped.
fn over(dst: (&mut [u8], usize, usize), src: (&[u8], usize, usize), x0: i32, y0: i32) {
    let (dst, dw, dh) = dst;
    let (src, sw, sh) = src;
    for sy in 0..sh {
        let Ok(y) = usize::try_from(y0 + sy as i32) else {
            continue;
        };
        if y >= dh {
            break;
        }
        for sx in 0..sw {
            let Ok(x) = usize::try_from(x0 + sx as i32) else {
                continue;
            };
            if x >= dw {
                break;
            }
            let s = &src[(sy * sw + sx) * 4..][..4];
            let d = &mut dst[(y * dw + x) * 4..][..4];
            let (sa, da) = (u32::from(s[3]), u32::from(d[3]));
            if sa == 0 {
                continue;
            }
            if sa == 255 || da == 0 {
                d.copy_from_slice(s);
                continue;
            }
            // Alphas scaled by 255: out = sa + da·(1 − sa).
            let rest = da * (255 - sa);
            let oa = sa * 255 + rest;
            for c in 0..3 {
                d[c] = ((u32::from(s[c]) * sa * 255 + u32::from(d[c]) * rest + oa / 2) / oa) as u8;
            }
            d[3] = ((oa + 127) / 255) as u8;
        }
    }
}

/// The shaping memo for emoji clusters, and the runs it names.
///
/// Run ids are never reused: a key naming one outlives it in the CPU glyph cache
/// and the GPU atlas, so a reused id could serve another cluster's image. At id
/// exhaustion a new run is refused and the cluster keeps its base-codepoint
/// fallback. `runs` holds exactly the ids the memo names, so rotating or
/// clearing the memo frees them too.
#[derive(Default)]
pub(crate) struct ClusterShapes {
    /// Cluster text → its shape (`None`: not a colour cluster).
    memo: TwoGenTextCache<Option<ClusterShape>>,
    runs: FxHashMap<u32, ClusterRun>,
    next_run: u32,
}

impl ClusterShapes {
    /// The memoized shape, and whether it was served from the previous
    /// generation (the caller re-inserts it to promote it).
    #[inline]
    pub(crate) fn get(&self, cluster: &str) -> Option<(Option<ClusterShape>, bool)> {
        self.memo.get(cluster).map(|(&shape, prev)| (shape, prev))
    }

    #[inline]
    pub(crate) fn insert(&mut self, cluster: &str, shape: Option<ClusterShape>) {
        self.memo.insert(cluster.into(), shape);
    }

    /// Store `run` under a fresh id; `None` once ids are exhausted.
    #[inline]
    pub(crate) fn add_run(&mut self, run: ClusterRun) -> Option<u32> {
        let id = self.next_run;
        self.next_run = id.checked_add(1)?;
        self.runs.insert(id, run);
        Some(id)
    }

    #[inline]
    pub(crate) fn run(&self, id: u32) -> Option<&ClusterRun> {
        self.runs.get(&id)
    }

    /// Entries in the current generation (the cap trigger).
    #[inline]
    pub(crate) fn cur_len(&self) -> usize {
        self.memo.cur_len()
    }

    /// Rotate the memo at its cap, and drop every run it no longer names.
    pub(crate) fn rotate(&mut self) {
        self.memo.rotate();
        let named: aterm_hash::FxHashSet<u32> = self
            .memo
            .prev
            .values()
            .filter_map(|shape| match shape {
                Some(ClusterShape::Run(id)) => Some(*id),
                _ => None,
            })
            .collect();
        self.runs.retain(|id, _| named.contains(id));
    }

    /// Forget every shape and run (a face or size change). Ids stay unique.
    pub(crate) fn clear(&mut self) {
        self.memo.clear();
        self.runs.clear();
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.memo.is_empty() && self.runs.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn generations(&self) -> (usize, usize) {
        (self.memo.cur.len(), self.memo.prev.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A run of `n` square glyphs 10 units wide, with hidden joiners between.
    fn joined(n: u32) -> Vec<(u32, i32, i32, i32)> {
        let mut glyphs = Vec::new();
        for i in 0..n {
            if i > 0 {
                glyphs.push((3, 0, 0, 0)); // a hidden ZWJ: no colour, no advance
            }
            glyphs.push((100 + i, 10, 0, 0));
        }
        glyphs
    }

    fn colour(gid: u16) -> bool {
        gid >= 100
    }

    #[test]
    fn a_run_keeps_its_colour_glyphs_and_skips_hidden_ones() {
        let run = layout_run(joined(3), colour, false).expect("three colour glyphs");
        let at: Vec<(u16, i32)> = run.glyphs.iter().map(|g| (g.gid, g.x)).collect();
        assert_eq!(at, [(100, 0), (101, 10), (102, 20)]);
        assert_eq!((run.width, run.em), (30, 10));
    }

    #[test]
    fn a_run_is_refused_unless_it_is_all_colour_and_two_to_eight_glyphs() {
        // One colour glyph: the bare base's own fallback draws it.
        assert!(layout_run(joined(1), colour, false).is_none());
        assert!(layout_run(joined(MAX_RUN_GLYPHS as u32), colour, false).is_some());
        assert!(layout_run(joined(MAX_RUN_GLYPHS as u32 + 1), colour, false).is_none());
        // A visible glyph the colour face cannot draw: text, not an emoji.
        let mut text = joined(2);
        text.push((7, 10, 0, 0));
        assert!(layout_run(text, colour, false).is_none());
        // `.notdef` is never colour, even with a drawable-looking predicate.
        assert!(layout_run([(0, 10, 0, 0), (100, 10, 0, 0)], |_| true, false).is_none());
        // A colour glyph that advances nothing cannot be placed in a slot...
        assert!(layout_run([(100, 10, 0, 0), (101, 0, 0, 0)], colour, false).is_none());
        // ...but stands at its pen where the run is painted in one canvas
        // (Segoe UI Emoji's family children); a backwards advance never does.
        let marked = layout_run([(100, 10, 0, 0), (101, 0, 0, 0)], colour, true)
            .expect("a member placed by its pen");
        let at: Vec<(u16, i32, i32)> = marked
            .glyphs
            .iter()
            .map(|g| (g.gid, g.x, g.advance))
            .collect();
        assert_eq!(at, [(100, 0, 10), (101, 10, 0)]);
        assert!(layout_run([(100, 10, 0, 0), (101, -1, 0, 0)], colour, true).is_none());
    }

    /// Two opaque squares fitted into a 2:1 box land side by side, each in its
    /// half, and the composite never exceeds the box.
    #[test]
    fn a_composite_places_each_glyph_in_its_slot_within_the_box() {
        let run = layout_run(joined(2), colour, false).unwrap();
        let solid = |gid: u16, w: usize, h: usize| {
            let v = if gid == 100 { 10 } else { 200 };
            Some(([v, v, v, 255].repeat(w * h), w, h))
        };
        let (rgba, w, h) = composite(&run, 20, 10, solid).unwrap();
        assert_eq!((w, h), (20, 10));
        let texel = |x: usize, y: usize| rgba[(y * w + x) * 4];
        assert_eq!(
            (texel(0, 0), texel(9, 9)),
            (10, 10),
            "left half: first glyph"
        );
        assert_eq!(
            (texel(10, 0), texel(19, 9)),
            (200, 200),
            "right half: second"
        );

        // Three members in the same box shrink to fit its width, aspect kept.
        let run = layout_run(joined(3), colour, false).unwrap();
        let (_, w, h) = composite(&run, 20, 10, solid).unwrap();
        assert_eq!((w, h), (20, 7));

        assert!(
            composite(&run, 20, 10, |_, _, _| None).is_none(),
            "nothing drew"
        );
    }

    #[test]
    fn over_blends_straight_alpha_and_clips() {
        let mut dst = vec![0, 0, 255, 255];
        over((&mut dst, 1, 1), (&[255, 0, 0, 128], 1, 1), 0, 0);
        assert_eq!(dst, [128, 0, 127, 255]);
        // Off-canvas texels are dropped, not wrapped.
        let mut dst = vec![0u8; 2 * 4];
        over(
            (&mut dst, 2, 1),
            (&[9, 9, 9, 255, 7, 7, 7, 255], 2, 1),
            1,
            0,
        );
        assert_eq!(dst, [0, 0, 0, 0, 9, 9, 9, 255]);
        over((&mut dst, 2, 1), (&[5, 5, 5, 255], 1, 1), -1, 0);
        assert_eq!(dst, [0, 0, 0, 0, 9, 9, 9, 255]);
    }

    /// Ids are never reused, and the run table holds exactly what the memo
    /// names across rotations and clears.
    #[test]
    fn runs_live_exactly_as_long_as_the_memo_names_them() {
        let mut shapes = ClusterShapes::default();
        let run = || layout_run(joined(2), colour, false).unwrap();
        let a = shapes.add_run(run()).unwrap();
        shapes.insert("a", Some(ClusterShape::Run(a)));
        shapes.insert("g", Some(ClusterShape::Glyph { face: 0, gid: 5 }));
        let b = shapes.add_run(run()).unwrap();
        assert_ne!(a, b);
        shapes.insert("b", Some(ClusterShape::Run(b)));

        shapes.rotate();
        assert!(
            shapes.run(a).is_some() && shapes.run(b).is_some(),
            "prev names both"
        );
        // "a" is promoted back (a hit from the previous generation); "b" is not.
        let (shape, prev) = shapes.get("a").unwrap();
        assert!(prev);
        shapes.insert("a", shape);
        shapes.rotate();
        assert!(shapes.run(a).is_some(), "a hot cluster keeps its run");
        assert!(shapes.run(b).is_none(), "a dropped cluster frees its run");
        assert!(shapes.get("b").is_none());

        shapes.clear();
        assert!(shapes.is_empty());
        let c = shapes.add_run(run()).unwrap();
        assert!(c > b, "a clear does not recycle ids");

        shapes.next_run = u32::MAX;
        assert!(shapes.add_run(run()).is_none(), "exhausted ids refuse");
        assert!(shapes.add_run(run()).is_none());
    }
}
