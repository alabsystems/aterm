// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The art registry where effects turn pictures into [`FxArt`](aterm_core::render::FxArt)s and CPU painting draws fx layers and glow quads.

use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use aterm_core::render::{FxArt, FxBlend, FxLayer, FxSlot, FxTexels, PxRect};
use aterm_hash::FxHashMap;

use crate::FxFrame;
use crate::fx_raster::{FxScratch, draw_layers};

/// The process-wide art registry ([`registry`]): each live art by its id.
pub struct FxRegistry {
    arts: Mutex<Interned>,
}

/// The registry's table and when it next sweeps its dead entries.
#[derive(Default)]
struct Interned {
    by_id: FxHashMap<u128, Weak<FxArt>>,
    sweep_at: usize,
}

/// The table size under which the registry never sweeps: a sweep is a walk
/// of every entry, paid once the table has doubled since the last.
const SWEEP_FLOOR: usize = 64;

/// The process's art registry.
#[must_use]
pub fn registry() -> &'static FxRegistry {
    static REGISTRY: OnceLock<FxRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| FxRegistry {
        arts: Mutex::new(Interned::default()),
    })
}

impl FxRegistry {
    /// `art` interned: the live `Arc` of an art with the same id, size, kind and bytes when one is held anywhere in the process, else `art` itself, now held (weakly) for the next producer.
    pub fn intern(&self, art: FxArt) -> Arc<FxArt> {
        let mut t = self.arts.lock().unwrap_or_else(PoisonError::into_inner);
        let id = art.id();
        if let Some(live) = t.by_id.get(&id).and_then(Weak::upgrade) {
            return if same_art(&live, &art) {
                live
            } else {
                Arc::new(art)
            };
        }
        if t.by_id.len() >= t.sweep_at.max(SWEEP_FLOOR) {
            t.by_id.retain(|_, w| w.strong_count() > 0);
            t.sweep_at = 2 * t.by_id.len();
        }
        let art = Arc::new(art);
        t.by_id.insert(id, Arc::downgrade(&art));
        art
    }
}

/// Whether `a` and `b` are the same picture: kind, size and bytes.
#[inline]
fn same_art(a: &FxArt, b: &FxArt) -> bool {
    a.texels() == b.texels() && a.w() == b.w() && a.h() == b.h() && a.data() == b.data()
}

/// Book one art build on the thread's test counter.
#[inline]
fn count_build() {
    #[cfg(any(test, debug_assertions))]
    crate::test_counters::count_art_build();
}

/// The sides and texel count of a `w × h` art the registry holds, checked before a producer allocates its texels: `None` for a side of zero or past [`aterm_core::render::FX_ART_MAX_SIDE`].
#[inline]
#[must_use]
pub fn art_dimensions(w: usize, h: usize) -> Option<(u16, u16, usize)> {
    let (w, h) = (u16::try_from(w).ok()?, u16::try_from(h).ok()?);
    if w == 0
        || h == 0
        || w > aterm_core::render::FX_ART_MAX_SIDE
        || h > aterm_core::render::FX_ART_MAX_SIDE
    {
        return None;
    }
    Some((w, h, usize::from(w).checked_mul(usize::from(h))?))
}

/// An Index8 art from art strings: one row a string, every row as long as the first, `'.'` transparent (texel 0) and `chars[k]` texel `k + 1`, so a sprite of it at palette base `ink` paints `chars[k]` in entry `ink + k`.
#[must_use]
pub fn indexed(rows: &[&str], chars: &[u8]) -> Option<Arc<FxArt>> {
    count_build();
    if chars.len() > 255 {
        return None;
    }
    let w = rows.first().map_or(0, |r| r.len());
    let (w16, h16, len) = art_dimensions(w, rows.len())?;
    let mut data = Vec::with_capacity(len);
    for row in rows {
        let row = row.as_bytes();
        if row.len() != w {
            return None;
        }
        for &b in row {
            let k = if b == b'.' {
                0
            } else {
                let at = chars.iter().position(|&c| c == b)?;
                u8::try_from(at + 1).ok()?
            };
            data.push(k);
        }
    }
    let art = FxArt::new(w16, h16, FxTexels::Index8, data)?;
    Some(registry().intern(art))
}

/// An art of `w × h` texels of kind `texels` from their bytes (row-major, top row first: one index a texel for Index8, `R, G, B, A` for the RGBA kinds), interned; `None` for a side of zero or too long ([`art_dimensions`]) or bytes that are not that many texels.
#[must_use]
pub fn bitmap(texels: FxTexels, w: u16, h: u16, data: impl Into<Box<[u8]>>) -> Option<Arc<FxArt>> {
    count_build();
    art_dimensions(usize::from(w), usize::from(h))?;
    let art = FxArt::new(w, h, texels, data)?;
    Some(registry().intern(art))
}

/// Ordered rectangles as art ([`pieces`]): the folded and the peeled forms of one picture, both laid with their top-left texel at [`Self::origin`] in the pieces' own space.
#[derive(Clone, Debug)]
pub struct PieceArt {
    origin: (i32, i32),
    folded: Arc<FxArt>,
    classes: Box<[Box<[u8]>]>,
    peeled: Box<[Arc<FxArt>]>,
}

impl PieceArt {
    /// Where the arts' top-left texel lies in the pieces' space: a sprite that lays the pieces at `(x, y)` sits at `(x + origin.0, y + origin.1)`.
    #[inline]
    #[must_use]
    pub const fn origin(&self) -> (i32, i32) {
        self.origin
    }

    /// The folded art: texel `k > 0` is class `k − 1` ([`Self::classes`]).
    #[inline]
    #[must_use]
    pub const fn folded(&self) -> &Arc<FxArt> {
        &self.folded
    }

    /// Each class's member pieces, in the order they were laid (a piece id once per covering piece): class `k − 1` for folded texel `k`.
    #[inline]
    #[must_use]
    pub const fn classes(&self) -> &[Box<[u8]>] {
        &self.classes
    }

    /// The peeled arts: in art `j`, texel `id + 1` names the `j`-th piece covering the pixel (piece id `id`), `0` where fewer cover it.
    #[inline]
    #[must_use]
    pub const fn peeled(&self) -> &[Arc<FxArt>] {
        &self.peeled
    }

    /// The most pieces covering one pixel: the peeled form's depth.
    #[inline]
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.peeled.len()
    }
}

/// The folded and the peeled arts of the rectangles `laid`, in order, each with its piece id (`0..=254`: a peeled texel is `id + 1`), interned.
#[must_use]
pub fn pieces(laid: &[(PxRect, u8)]) -> Option<PieceArt> {
    count_build();
    let hull = laid
        .iter()
        .filter(|(r, _)| !r.is_empty())
        .fold(PxRect::EMPTY, |h, (r, _)| h.hull(*r));
    if hull.is_empty() || laid.iter().any(|&(_, id)| id == u8::MAX) {
        return None;
    }
    let w = usize::try_from(i64::from(hull.x1) - i64::from(hull.x0)).ok()?;
    let h = usize::try_from(i64::from(hull.y1) - i64::from(hull.y0)).ok()?;
    let (w16, h16, len) = art_dimensions(w, h)?;
    let mut cover: Vec<Vec<u8>> = vec![Vec::new(); len];
    for &(r, id) in laid.iter().filter(|(r, _)| !r.is_empty()) {
        for y in r.y0..r.y1 {
            let row = (y - hull.y0) as usize * w;
            for x in r.x0..r.x1 {
                cover[row + (x - hull.x0) as usize].push(id);
            }
        }
    }
    let depth = cover.iter().map(Vec::len).max().unwrap_or(0);
    let mut classes: Vec<Box<[u8]>> = Vec::new();
    let mut class_of: FxHashMap<&[u8], u8> = FxHashMap::default();
    let mut folded = vec![0u8; len];
    for (texel, members) in folded.iter_mut().zip(&cover) {
        if members.is_empty() {
            continue;
        }
        let k = match class_of.get(members.as_slice()) {
            Some(&k) => k,
            None => {
                let k = u8::try_from(classes.len() + 1).ok()?;
                classes.push(members.clone().into_boxed_slice());
                class_of.insert(members, k);
                k
            }
        };
        *texel = k;
    }
    let peeled = (0..depth)
        .map(|j| {
            let data: Vec<u8> = cover
                .iter()
                .map(|m| m.get(j).map_or(0, |&id| id + 1))
                .collect();
            FxArt::new(w16, h16, FxTexels::Index8, data).map(|a| registry().intern(a))
        })
        .collect::<Option<Box<[_]>>>()?;
    let folded = registry().intern(FxArt::new(w16, h16, FxTexels::Index8, folded)?);
    Some(PieceArt {
        origin: (hull.x0, hull.y0),
        folded,
        classes: classes.into_boxed_slice(),
        peeled,
    })
}

/// The most exact rectangles [`cover`] retains before merging: an art whose
/// row bands need more is covered by the one rectangle round
/// its drawing texels.
const COVER_EXACT_MAX: usize = 2048;

/// Whether texel `i` of `art` can write a byte: an Index8 texel above 0, an
/// Rgba8Premul texel with any byte set (its light adds even at alpha 0), an
/// Rgba8Straight texel with alpha.
#[inline]
fn draws(art: &FxArt, i: usize) -> bool {
    let d = art.data();
    match art.texels() {
        FxTexels::Index8 => d[i] != 0,
        FxTexels::Rgba8Premul => d[4 * i..4 * i + 4] != [0; 4],
        FxTexels::Rgba8Straight => d[4 * i + 3] != 0,
    }
}

/// The drawing hull without retaining any per-texel runs.
fn drawing_hull(art: &FxArt) -> Box<[PxRect]> {
    let (w, h) = (art.w(), art.h());
    let mut hull = PxRect::EMPTY;
    for y in 0..h {
        for x in 0..w {
            let i = usize::from(y) * usize::from(w) + usize::from(x);
            if !draws(art, i) {
                continue;
            }
            let (x, y) = (i32::from(x), i32::from(y));
            hull = hull.hull(PxRect::new(x, y, x + 1, y + 1));
        }
    }
    if hull.is_empty() {
        Box::new([])
    } else {
        Box::new([hull])
    }
}

/// One band of a [`cover`]: art rows `[y0, y1)` and, in them, the column
/// runs `[x0, x1)` (disjoint, ascending, never touching).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Band {
    y0: u32,
    y1: u32,
    runs: Vec<(u32, u32)>,
}

impl Band {
    fn area(&self) -> u64 {
        let h = u64::from(self.y1 - self.y0);
        self.runs.iter().map(|&(a, b)| u64::from(b - a) * h).sum()
    }
}

/// A [`cover`]'s merge step at a band: two runs and the gap between them
/// as one run, or the band and the next as one band, of their runs' union
/// (`Join`) or of the one run round them both (`Hull`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Gap(usize),
    Join,
    Hull,
}

/// The union of two run lists, touching runs joined.
fn run_union(a: &[(u32, u32)], b: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut all: Vec<(u32, u32)> = a.iter().chain(b).copied().collect();
    all.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(all.len());
    for (x0, x1) in all {
        match out.last_mut() {
            Some(last) if x0 <= last.1 => last.1 = last.1.max(x1),
            _ => out.push((x0, x1)),
        }
    }
    out
}

/// A cover of `art`: at most `max` texel rectangles (at least one), pairwise disjoint, whose union holds every texel that can write a byte ([`draws`]).
#[must_use]
pub fn cover(art: &FxArt, max: usize) -> Box<[PxRect]> {
    let (w, h) = (usize::from(art.w()), usize::from(art.h()));
    let max = max.max(1);
    let row_runs = |y: usize| {
        let mut runs = Vec::new();
        let mut x = 0;
        while x < w {
            if !draws(art, y * w + x) {
                x += 1;
                continue;
            }
            let x0 = x;
            while x < w && draws(art, y * w + x) {
                x += 1;
            }
            runs.push((x0 as u32, x as u32));
        }
        runs
    };
    let mut bands: Vec<Band> = Vec::new();
    let mut count = 0;
    for y in 0..h {
        let runs = row_runs(y);
        if runs.is_empty() {
            continue;
        }
        match bands.last_mut() {
            Some(b) if b.y1 == y as u32 && b.runs == runs => b.y1 += 1,
            _ => {
                // The exact count only grows: a later row can extend the last
                // band but cannot retire an earlier band. Switch to the hull
                // before keeping more, with only this one row of scratch live.
                if count + runs.len() > COVER_EXACT_MAX {
                    return drawing_hull(art);
                }
                count += runs.len();
                bands.push(Band {
                    y0: y as u32,
                    y1: y as u32 + 1,
                    runs,
                });
            }
        }
    }
    while count > max {
        let mut best: Option<(u64, u64, usize, Step)> = None;
        let mut consider = |added: u64, saved: u64, at: usize, step: Step| {
            if saved == 0 {
                return;
            }
            let better = best.is_none_or(|(a, s, ..)| {
                u128::from(added) * u128::from(s) < u128::from(a) * u128::from(saved)
            });
            if better {
                best = Some((added, saved, at, step));
            }
        };
        for i in 0..bands.len() {
            let b = &bands[i];
            let h = u64::from(b.y1 - b.y0);
            for (j, pair) in b.runs.windows(2).enumerate() {
                consider(u64::from(pair[1].0 - pair[0].1) * h, 1, i, Step::Gap(j));
            }
            if let Some(next) = bands.get(i + 1) {
                let (n, rows) = (b.runs.len() + next.runs.len(), u64::from(next.y1 - b.y0));
                let union = run_union(&b.runs, &next.runs);
                let width = |runs: &[(u32, u32)]| -> u64 {
                    runs.iter().map(|&(a, b)| u64::from(b - a)).sum()
                };
                let (was, union_area) = (b.area() + next.area(), width(&union) * rows);
                consider(union_area - was, (n - union.len()) as u64, i, Step::Join);
                let hull = u64::from(union[union.len() - 1].1 - union[0].0) * rows;
                consider(hull - was, n as u64 - 1, i, Step::Hull);
            }
        }
        let Some((_, _, i, step)) = best else {
            debug_assert!(false, "a cover past its bound has a step");
            break;
        };
        match step {
            Step::Gap(j) => {
                let runs = &mut bands[i].runs;
                runs[j].1 = runs[j + 1].1;
                runs.remove(j + 1);
            }
            Step::Join | Step::Hull => {
                let next = bands.remove(i + 1);
                let b = &mut bands[i];
                b.y1 = next.y1;
                b.runs = run_union(&b.runs, &next.runs);
                if step == Step::Hull {
                    b.runs = vec![(b.runs[0].0, b.runs[b.runs.len() - 1].1)];
                }
            }
        }
        count = bands.iter().map(|b| b.runs.len()).sum();
    }
    let mut out = Vec::with_capacity(count);
    for b in &bands {
        for &(x0, x1) in &b.runs {
            let lit = |y: u32| (x0..x1).any(|x| draws(art, y as usize * w + x as usize));
            let Some(top) = (b.y0..b.y1).find(|&y| lit(y)) else {
                continue;
            };
            let bottom = (b.y0..b.y1).rev().find(|&y| lit(y)).unwrap_or(top) + 1;
            out.push(PxRect::new(x0 as i32, top as i32, x1 as i32, bottom as i32));
        }
    }
    out.into_boxed_slice()
}

/// The item-local pixels a sprite of a `w × h` art drawn at `dw × dh` (mirrored under `flip_x`) lays texel rectangle `t` on, by pixel law F: pixel `l` reads texel `⌊l·w / dw⌋`, so the columns reading `t0, t1)` are `⌈t0·dw / w⌉, ⌈t1·dw / w⌉)`, and under `flip_x` those reading `w − t1, w − t0)`; the rows alike.
#[inline]
#[must_use]
pub const fn cover_px(t: PxRect, (w, h): (u16, u16), (dw, dh): (u16, u16), flip_x: bool) -> PxRect {
    if w == 0 || h == 0 {
        return PxRect::EMPTY;
    }
    #[inline]
    const fn clamp_i32(v: i32, hi: i32) -> i32 {
        if v < 0 {
            0
        } else if v > hi {
            hi
        } else {
            v
        }
    }
    #[inline]
    const fn up(v: i32, n: u16, d: u16) -> i32 {
        let v = clamp_i32(v, n as i32) as i64;
        ((v * (d as i64) + (n as i64) - 1) / (n as i64)) as i32
    }
    let (tx0, tx1) = (clamp_i32(t.x0, w as i32), clamp_i32(t.x1, w as i32));
    let (tx0, tx1) = if flip_x {
        (w as i32 - tx1, w as i32 - tx0)
    } else {
        (tx0, tx1)
    };
    PxRect::new(
        up(tx0, w, dw),
        up(t.y0, h, dh),
        up(tx1, w, dw),
        up(t.y1, h, dh),
    )
}

/// Paint `layers` onto `px` (`frame.w × frame.h`, row-major) as the CPU renderer composites them, every band open: slot by slot (Under, Glow, Over), each slot's Exact layers before its Linear ones, minus `occluders`.
pub fn paint_layers(px: &mut [u32], frame: &FxFrame, layers: &[FxLayer], occluders: &[PxRect]) {
    if frame
        .w
        .checked_mul(frame.h)
        .is_none_or(|len| px.len() < len)
    {
        return;
    }
    let mut scratch = FxScratch::default();
    for slot in [FxSlot::Under, FxSlot::Glow, FxSlot::Over] {
        for blend in [FxBlend::Exact, FxBlend::Linear] {
            draw_layers(
                px,
                frame,
                layers,
                occluders,
                slot,
                blend,
                None,
                &mut scratch,
            );
        }
    }
}

/// Paint `quads` onto `px` (`w × h`, row-major) by the glow streams' one law (`Renderer::draw_flat_add`'s span loop: per column the ramp between the quad's ends, `over_premul`, additive at alpha 0), each moved by `off` and cut at the buffer's edges, with no row gate.
///
/// The painter of the quad oracles the fx layers are held to (the card's and the rainbow's
/// tests), so it exists in test and debug builds only, like [`crate::legacy_sprite_oracle`].
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn paint_quads(
    px: &mut [u32],
    w: usize,
    h: usize,
    quads: &[aterm_core::render::GlowQuad],
    off: (usize, usize),
) {
    if w.checked_mul(h).is_none_or(|len| px.len() < len) {
        return;
    }
    for q in quads {
        let (x0, y0) = (
            off.0.saturating_add(usize::from(q.x)),
            off.1.saturating_add(usize::from(q.y)),
        );
        let x1 = x0.saturating_add(usize::from(q.w)).min(w);
        if x0 >= x1 || y0 >= h {
            continue;
        }
        let (c0, c1) = (
            aterm_core::render::FxInk::over(q.color, q.alpha),
            aterm_core::render::FxInk::over(q.color2, q.alpha2),
        );
        for y in y0..y0.saturating_add(usize::from(q.h)).min(h) {
            crate::fx_raster::fill_glow_span(
                &mut px[y * w + x0..y * w + x1],
                0,
                u32::from(q.w),
                c0,
                c1,
            );
        }
    }
}

#[cfg(test)]
#[path = "fx_tests.rs"]
mod tests;
