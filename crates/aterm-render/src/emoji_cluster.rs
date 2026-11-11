// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Layout, compositing, and caching for multi-glyph colour emoji grapheme clusters.
//!
//! Single-glyph ligatures use [`crate::GlyphClass::RgbaGid`]; multi-glyph colour
//! runs are composited into a two-cell image keyed by [`crate::GlyphClass::RgbaCluster`].

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
    /// Colour-chain face index (`0` from [`layout_run`]).
    pub(crate) face: u8,
    pub(crate) glyphs: Box<[RunGlyph]>,
    /// The run's total advance.
    pub(crate) width: i32,
    /// Widest glyph advance, used as the run's reference height.
    pub(crate) em: i32,
}

/// Maximum colour glyphs in a single composite run.
pub(crate) const MAX_RUN_GLYPHS: usize = 8;

/// Lays out a shaped cluster of 2..=[`MAX_RUN_GLYPHS`] colour glyphs into a [`ClusterRun`].
///
/// Zero-advance colour glyphs are permitted only when `marks` is true (e.g. COLR
/// runs painted on a shared canvas); non-colour glyphs must have zero advance.
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

/// Composites `run` into straight-alpha RGBA8 fitted within `box_w × box_h`.
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
            let rest = da * (255 - sa);
            let oa = sa * 255 + rest;
            for c in 0..3 {
                d[c] = ((u32::from(s[c]) * sa * 255 + u32::from(d[c]) * rest + oa / 2) / oa) as u8;
            }
            d[3] = ((oa + 127) / 255) as u8;
        }
    }
}

/// Two-generation shaping memo for emoji clusters and their [`ClusterRun`] entries.
#[derive(Default)]
pub(crate) struct ClusterShapes {
    /// Cluster text → its shape (`None`: not a colour cluster).
    memo: TwoGenTextCache<Option<ClusterShape>>,
    runs: FxHashMap<u32, ClusterRun>,
    next_run: u32,
}

impl ClusterShapes {
    /// Looks up the memoized shape and whether it came from the previous generation.
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
#[path = "emoji_cluster_tests.rs"]
mod tests;
