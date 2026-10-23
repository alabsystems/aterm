// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ART REGISTRY (`docs/DESIGN-card-engine-2026-10-19.md` §4.7): the one place an effect turns its pictures into [`FxArt`]s, and the CPU painting a tool or a test draws fx layers and glow quads with.

use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use aterm_core::render::{FxArt, FxBlend, FxInk, FxLayer, FxSlot, FxTexels, GlowQuad, PxRect};
use aterm_hash::FxHashMap;

use crate::FxFrame;
use crate::fx_raster::{FxScratch, draw_layers, fill_glow_span};

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

    /// How many interned arts some producer still holds.
    #[must_use]
    pub fn live(&self) -> usize {
        let t = self.arts.lock().unwrap_or_else(PoisonError::into_inner);
        t.by_id.values().filter(|w| w.strong_count() > 0).count()
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

/// Validate the art's dimensions before a builder allocates its texels.
#[inline]
fn art_dimensions(w: usize, h: usize) -> Option<(u16, u16, usize)> {
    let (w, h) = (u16::try_from(w).ok()?, u16::try_from(h).ok()?);
    if w > aterm_core::render::FX_ART_MAX_SIDE || h > aterm_core::render::FX_ART_MAX_SIDE {
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

/// An Rgba8Premul art of `w × h` texels (`R, G, B, A` bytes, premultiplied, row-major).
#[must_use]
pub fn rgba(w: u16, h: u16, premul: impl Into<Box<[u8]>>) -> Option<Arc<FxArt>> {
    count_build();
    let art = FxArt::new(w, h, FxTexels::Rgba8Premul, premul)?;
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

/// The folded and the peeled arts of the rectangles `laid`, in order, each with its piece id (`0..=254`: a peeled texel is `id + 1`), interned (design §4.7).
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

/// A COVER of `art` (design §4.7 as built): at most `max` texel rectangles (at least one), pairwise disjoint, whose union holds every texel that can write a byte ([`draws`]).
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
pub fn paint_quads(px: &mut [u32], w: usize, h: usize, quads: &[GlowQuad], off: (usize, usize)) {
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
            FxInk::over(q.color, q.alpha),
            FxInk::over(q.color2, q.alpha2),
        );
        for y in y0..y0.saturating_add(usize::from(q.h)).min(h) {
            fill_glow_span(&mut px[y * w + x0..y * w + x1], 0, u32::from(q.w), c0, c1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx_raster::fill_flat;
    use aterm_core::render::{FxContent, FxItem, FxSpace, FxSprite};

    fn frame(w: usize, h: usize) -> FxFrame {
        FxFrame {
            w,
            h,
            pad: 0,
            grid_top: 0,
            cell_h: h.max(1),
            rows: 1,
        }
    }

    /// A deterministic stream of `u32`s.
    fn rng(seed: u64) -> impl FnMut() -> u32 {
        let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 16) as u32
        }
    }

    /// Lay `laid` piece by piece onto a `w × h` buffer at `(x, y)`, each in
    /// its piece's ink: the order a quad stream lays them in.
    fn lay_in_order(
        px: &mut [u32],
        w: usize,
        laid: &[(PxRect, u8)],
        inks: &[FxInk],
        (x, y): (i32, i32),
    ) {
        for &(r, id) in laid {
            let ink = inks[usize::from(id)];
            for py in r.y0..r.y1 {
                let row = (py + y) as usize * w;
                let (a, b) = ((r.x0 + x) as usize, (r.x1 + x) as usize);
                fill_flat(&mut px[row + a..row + b], ink.color, ink.alpha);
            }
        }
    }

    /// The layer of one sprite per art in `arts` at `(x, y)` over `palette`,
    /// each at palette base `base`.
    fn sprites(arts: &[(Arc<FxArt>, u32)], (x, y): (i32, i32), palette: Vec<FxInk>) -> FxLayer {
        let items = arts
            .iter()
            .map(|(art, base)| {
                FxItem::Sprite(FxSprite {
                    x,
                    y,
                    dw: art.w(),
                    dh: art.h(),
                    flip_x: false,
                    art: Arc::clone(art),
                    ink: *base,
                    clip: PxRect::EVERYWHERE,
                })
            })
            .collect();
        FxLayer::new(
            7,
            Arc::new(FxContent::new(items, palette)),
            FxSlot::Over,
            FxSpace::Window,
            FxBlend::Exact,
        )
    }

    /// AN ART IS INTERNED ONCE: the same picture built twice, while either is
    /// held, is one `Arc`; dropped by every producer, it is built afresh.
    #[test]
    fn an_art_is_interned_once() {
        let a = indexed(&["ab.", ".ba"], b"ab").expect("an art");
        let b = indexed(&["ab.", ".ba"], b"ab").expect("an art");
        assert!(Arc::ptr_eq(&a, &b), "one picture, one Arc");
        let other = indexed(&["ab.", ".bb"], b"ab").expect("an art");
        assert!(!Arc::ptr_eq(&a, &other), "CONTROL: another picture");
        let id = a.id();
        let weak = Arc::downgrade(&a);
        drop((a, b));
        assert!(weak.upgrade().is_none(), "the registry holds arts weakly");
        let again = indexed(&["ab.", ".ba"], b"ab").expect("an art");
        assert_eq!(again.id(), id, "the same picture, the same id");
    }

    /// `indexed` reads `'.'` as transparent and `chars[k]` as texel `k + 1`,
    /// and refuses a ragged row or a byte it does not know.
    #[test]
    fn indexed_names_each_character_by_its_place() {
        let art = indexed(&["o.c", "cko"], b"ock").expect("an art");
        assert_eq!((art.w(), art.h()), (3, 2));
        assert_eq!(art.data(), &[1, 0, 2, 2, 3, 1]);
        assert!(indexed(&["o.c", "ck"], b"ock").is_none(), "a ragged row");
        assert!(indexed(&["o.x"], b"ock").is_none(), "an unknown byte");
        let max = usize::from(aterm_core::render::FX_ART_MAX_SIDE);
        let edge = ".".repeat(max);
        assert!(indexed(&[&edge], b"o").is_some(), "the longest legal row");
        let too_wide = ".".repeat(max + 1);
        assert!(indexed(&[&too_wide], b"o").is_none());
        assert!(indexed(&vec!["o"; max + 1], b"o").is_none());
    }

    /// Random pieces (overlapping needles and dots) over a random ground.
    fn random_pieces(next: &mut impl FnMut() -> u32, ids: u8) -> Vec<(PxRect, u8)> {
        let n = 2 + next() % 14;
        (0..n)
            .map(|_| {
                let (x, y) = ((next() % 24) as i32 - 12, (next() % 24) as i32 - 12);
                let (w, h) = (1 + next() % 12, 1 + next() % 12);
                (PxRect::at(x, y, w, h), (next() % u32::from(ids)) as u8)
            })
            .collect()
    }

    /// THE FOLD AND THE PEEL ARE EXACT: random pieces laid in order onto a
    /// random ground equal, byte for byte, the folded art under each class's
    /// `add_sat` sum where every ink is additive, and the peeled arts drawn
    /// in order under the pieces' inks for translucent and opaque inks.
    #[test]
    fn the_fold_and_the_peel_are_exact() {
        let (w, h) = (64usize, 64usize);
        let at = (24, 24);
        let mut fold_breaks = 0;
        for seed in 0..200u64 {
            let mut next = rng(seed);
            let ids = 6u8;
            let laid = random_pieces(&mut next, ids);
            let ground: Vec<u32> = (0..w * h).map(|_| next() & 0x00FF_FFFF).collect();
            let art = pieces(&laid).expect("pieces");
            let origin = art.origin();
            let place = (at.0 + origin.0, at.1 + origin.1);
            for additive in [true, false] {
                let inks: Vec<FxInk> = (0..ids)
                    .map(|_| {
                        let a = if additive {
                            0
                        } else {
                            [255u8, 1 + (next() % 254) as u8][(next() % 2) as usize]
                        };
                        let rgb = next() & 0x00FF_FFFF;
                        FxInk::over(crate::premul_rgb(rgb, if a == 0 { 255 } else { a }), a)
                    })
                    .collect();
                let mut want = ground.clone();
                lay_in_order(&mut want, w, &laid, &inks, at);
                let sums: Vec<FxInk> = art
                    .classes()
                    .iter()
                    .map(|m| {
                        if additive {
                            FxInk::add(m.iter().fold(0, |s, &id| {
                                crate::add_sat(s, inks[usize::from(id)].color) & 0x00FF_FFFF
                            }))
                        } else {
                            // fold would.
                            inks[usize::from(*m.last().expect("a member"))]
                        }
                    })
                    .collect();
                let mut folded = ground.clone();
                paint_layers(
                    &mut folded,
                    &frame(w, h),
                    &[sprites(&[(Arc::clone(art.folded()), 0)], place, sums)],
                    &[],
                );
                let peel = art
                    .peeled()
                    .iter()
                    .map(|a| (Arc::clone(a), 0))
                    .collect::<Vec<_>>();
                let mut peeled = ground.clone();
                paint_layers(
                    &mut peeled,
                    &frame(w, h),
                    &[sprites(&peel, place, inks.clone())],
                    &[],
                );
                assert_eq!(peeled, want, "seed {seed}: the peel, additive {additive}");
                if additive {
                    assert_eq!(folded, want, "seed {seed}: the fold");
                } else if folded != want {
                    fold_breaks += 1;
                }
            }
        }
        assert!(fold_breaks > 0, "CONTROL: a fold of over inks is not exact");
    }

    /// A peeled art is as deep as the most pieces over one pixel, and a
    /// class lists its members in the order laid, once per covering piece.
    #[test]
    fn a_class_keeps_its_members_in_order() {
        let laid = [
            (PxRect::at(0, 1, 3, 1), 0),
            (PxRect::at(1, 0, 1, 3), 0),
            (PxRect::at(1, 1, 1, 1), 1),
        ];
        let art = pieces(&laid).expect("pieces");
        assert_eq!(art.origin(), (0, 0));
        assert_eq!(art.depth(), 3);
        let centre = art.folded().texel(1, 1) as usize;
        assert_eq!(&*art.classes()[centre - 1], &[0, 0, 1]);
        assert_eq!(art.peeled()[2].texel(1, 1), 2, "the nucleus is laid third");
        assert_eq!(art.folded().texel(0, 0), 0, "a corner no piece covers");
        let max = u32::from(aterm_core::render::FX_ART_MAX_SIDE);
        assert!(pieces(&[(PxRect::at(0, 0, max, 1), 0)]).is_some());
        assert!(pieces(&[(PxRect::at(0, 0, max + 1, 1), 0)]).is_none());
        assert!(pieces(&[(PxRect::at(0, 0, 1, max + 1), 0)]).is_none());
        assert!(
            pieces(&[(
                PxRect::at(0, 0, u32::from(u16::MAX), u32::from(u16::MAX)),
                0
            )])
            .is_none(),
            "reject the hull before allocating a pixel table"
        );
    }

    /// A random art of `kind`: a filled blob with holes and stray texels, so
    /// it has runs to merge and gaps to fill.
    fn random_art(next: &mut impl FnMut() -> u32, kind: FxTexels) -> Arc<FxArt> {
        let (w, h) = (1 + next() % 24, 1 + next() % 24);
        let (cx, cy, r) = (next() % w, next() % h, 1 + next() % 12);
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x.abs_diff(cx), y.abs_diff(cy));
                let ring = (dx * dx + dy * dy).abs_diff(r * r) <= r;
                let on = ring || next().is_multiple_of(11);
                match kind {
                    FxTexels::Index8 => data.push(if on { 1 + (next() % 4) as u8 } else { 0 }),
                    FxTexels::Rgba8Premul | FxTexels::Rgba8Straight => {
                        let a = (next() % 256) as u8;
                        let c = if on { [a / 2, a / 3, a, a] } else { [0; 4] };
                        let c = if on && next().is_multiple_of(5) {
                            [40, 0, 9, 0]
                        } else {
                            c
                        };
                        data.extend(c);
                    }
                }
            }
        }
        Arc::new(FxArt::new(w as u16, h as u16, kind, data).expect("an art"))
    }

    /// One frame of `art` at `(x, y)` scaled to `dw × dh` (mirrored under
    /// `flip`) on a random ground: the whole sprite once, then one item per
    /// rectangle of `clips` (item-local pixels, each an item's clip).
    fn whole_and_covered(
        art: &Arc<FxArt>,
        clips: &[PxRect],
        (x, y, dw, dh, flip): (i32, i32, u16, u16, bool),
        ground: &[u32],
        palette: &[FxInk],
    ) -> (Vec<u32>, Vec<u32>) {
        let blend = if art.texels() == FxTexels::Rgba8Straight {
            FxBlend::Linear
        } else {
            FxBlend::Exact
        };
        let item = |clip: PxRect| {
            FxItem::Sprite(FxSprite {
                x,
                y,
                dw,
                dh,
                flip_x: flip,
                art: Arc::clone(art),
                ink: 0,
                clip,
            })
        };
        let paint = |items: Vec<FxItem>| {
            let layer = FxLayer::new(
                7,
                Arc::new(FxContent::new(items, palette.to_vec())),
                FxSlot::Over,
                FxSpace::Window,
                blend,
            );
            let mut px = ground.to_vec();
            paint_layers(&mut px, &frame(48, 48), &[layer], &[]);
            px
        };
        let whole = paint(vec![item(PxRect::EVERYWHERE)]);
        let covered = paint(clips.iter().map(|l| item(l.translate(x, y))).collect());
        (whole, covered)
    }

    /// A COVER DRAWS ITS SPRITE'S BYTES (design §4.7 as built): random arts
    /// of every texel kind, laid scaled up and down and mirrored, drawn whole
    /// and drawn as one item per cover rectangle (its pixels the item's
    /// clip), are the same bytes on a random ground; the rectangles are at
    /// most `max`, inside the art, pairwise disjoint, and hold every texel
    /// that draws.
    #[test]
    fn a_cover_draws_its_sprites_bytes() {
        let mut next = rng(0xC0_7E2);
        let (mut short_breaks, mut floor_breaks, mut drawn) = (0, 0, 0);
        for round in 0..300 {
            let kind = [
                FxTexels::Index8,
                FxTexels::Rgba8Premul,
                FxTexels::Rgba8Straight,
            ][round % 3];
            let art = random_art(&mut next, kind);
            let palette: Vec<FxInk> = (0..5)
                .map(|_| FxInk::over(next() & 0x00FF_FFFF, (next() % 3 * 127) as u8))
                .collect();
            let ground: Vec<u32> = (0..48 * 48).map(|_| next() & 0x00FF_FFFF).collect();
            let at = (
                (next() % 20) as i32 - 4,
                (next() % 20) as i32 - 4,
                (1 + next() % 40) as u16,
                (1 + next() % 40) as u16,
                next().is_multiple_of(2),
            );
            let (aw, ah) = (art.w(), art.h());
            let (w, h) = (usize::from(aw), usize::from(ah));
            for max in [1, 2, 3, 5, 8, 64] {
                let rects = cover(&art, max);
                assert!(rects.len() <= max, "{} rectangles past {max}", rects.len());
                let mut held = vec![0u8; w * h];
                for r in &rects {
                    assert!(!r.is_empty() && r.x0 >= 0 && r.y0 >= 0);
                    assert!(
                        r.x1 as usize <= w && r.y1 as usize <= h,
                        "{r:?} outside the art"
                    );
                    for ty in r.y0..r.y1 {
                        for tx in r.x0..r.x1 {
                            held[ty as usize * w + tx as usize] += 1;
                        }
                    }
                }
                for (i, &n) in held.iter().enumerate() {
                    assert!(n <= 1, "two rectangles hold texel {i}");
                    assert!(!draws(&art, i) || n == 1, "texel {i} draws, uncovered");
                }
                let px: Vec<PxRect> = rects
                    .iter()
                    .map(|&t| cover_px(t, (aw, ah), (at.2, at.3), at.4))
                    .collect();
                let (whole, covered) = whole_and_covered(&art, &px, at, &ground, &palette);
                assert_eq!(
                    whole, covered,
                    "round {round}, max {max}: the cover's bytes"
                );
                drawn += usize::from(whole != ground);
                if let Some((_, short)) = px.split_last() {
                    let (whole, covered) = whole_and_covered(&art, short, at, &ground, &palette);
                    short_breaks += usize::from(whole != covered);
                }
                let down = |v: i32, n: u16, d: u16| v * i32::from(d) / i32::from(n);
                let floor: Vec<PxRect> = rects
                    .iter()
                    .map(|t| {
                        let (tx0, tx1) = if at.4 {
                            (i32::from(aw) - t.x1, i32::from(aw) - t.x0)
                        } else {
                            (t.x0, t.x1)
                        };
                        PxRect::new(
                            down(tx0, aw, at.2),
                            down(t.y0, ah, at.3),
                            down(tx1, aw, at.2),
                            down(t.y1, ah, at.3),
                        )
                    })
                    .collect();
                let (whole, by_floor) = whole_and_covered(&art, &floor, at, &ground, &palette);
                floor_breaks += usize::from(whole != by_floor);
            }
        }
        assert!(drawn > 1000, "the frames draw: {drawn}");
        assert!(
            short_breaks > 0,
            "CONTROL: a cover short of a rectangle loses bytes"
        );
        assert!(floor_breaks > 0, "CONTROL: the floor's mapping loses bytes");

        let art = Arc::new(FxArt::new(4, 3, FxTexels::Index8, vec![1; 12]).unwrap());
        let ground = vec![0x12_34_56; 48 * 48];
        let palette = [FxInk::over(0xAA_BB_CC, 255)];
        for flip in [false, true] {
            for (dw, dh) in [(3, 2), (17, 11)] {
                for rect in [
                    PxRect::EVERYWHERE,
                    PxRect::new(i32::MIN, -1, i32::MAX, i32::MAX),
                ] {
                    let clip = cover_px(rect, (4, 3), (dw, dh), flip);
                    assert_eq!(clip, PxRect::new(0, 0, i32::from(dw), i32::from(dh)));
                    let (whole, covered) =
                        whole_and_covered(&art, &[clip], (2, 3, dw, dh, flip), &ground, &palette);
                    assert_eq!(whole, covered);
                    assert_ne!(whole, ground, "CONTROL: the unbounded clip draws");
                }
            }
            for rect in [
                PxRect::new(i32::MIN, 0, -1, 3),
                PxRect::new(5, 0, i32::MAX, 3),
                PxRect::new(3, 0, 1, 3),
                PxRect::EMPTY,
            ] {
                assert!(cover_px(rect, (4, 3), (17, 11), flip).is_empty());
            }
            for (art_size, drawn_size) in [
                ((0, 3), (17, 11)),
                ((4, 0), (17, 11)),
                ((4, 3), (0, 11)),
                ((4, 3), (17, 0)),
            ] {
                assert!(cover_px(PxRect::EVERYWHERE, art_size, drawn_size, flip).is_empty());
            }
            assert_eq!(
                cover_px(PxRect::EVERYWHERE, (4, 3), (u16::MAX, u16::MAX), flip),
                PxRect::new(0, 0, i32::from(u16::MAX), i32::from(u16::MAX))
            );
        }
    }

    /// A cover of an outline is a sliver of its box, and a blank art has no
    /// rectangle.
    #[test]
    fn a_cover_of_an_outline_is_a_sliver_of_its_box() {
        let (w, h) = (60usize, 40usize);
        let mut data = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let edge = x < 2 || y < 2 || x >= w - 2 || y >= h - 2;
                data[y * w + x] = u8::from(edge);
            }
        }
        let art = FxArt::new(w as u16, h as u16, FxTexels::Index8, data).expect("an art");
        let rects = cover(&art, 8);
        let area: i64 = rects
            .iter()
            .map(|r| i64::from(r.x1 - r.x0) * i64::from(r.y1 - r.y0))
            .sum();
        assert_eq!(
            area,
            (2 * w + 2 * (h - 4)) as i64 * 2,
            "exactly the frame: {rects:?}"
        );
        assert_eq!(cover(&art, 1).len(), 1, "one rectangle: the box");
        let blank = FxArt::new(4, 4, FxTexels::Index8, vec![0; 16]).expect("an art");
        assert!(cover(&blank, 8).is_empty());
        let checks = |w: usize, h: usize| {
            let data: Vec<u8> = (0..w * h)
                .map(|i| u8::from((i % w + i / w).is_multiple_of(2)))
                .collect();
            FxArt::new(w as u16, h as u16, FxTexels::Index8, data).expect("an art")
        };
        let past = checks(128, 64);
        assert_eq!(
            &cover(&past, 4096)[..],
            &[PxRect::new(0, 0, 128, 64)],
            "4096 runs: the box"
        );
        assert_eq!(
            cover(&checks(64, 64), 4096).len(),
            COVER_EXACT_MAX,
            "CONTROL: 2048 runs are each a rectangle"
        );
        let mut late = vec![0u8; 128 * 70];
        for (i, byte) in late[..128 * 64].iter_mut().enumerate() {
            *byte = u8::from((i % 128 + i / 128).is_multiple_of(2));
        }
        late[128 * 70 - 1] = 1;
        let late = FxArt::new(128, 70, FxTexels::Index8, late).unwrap();
        assert_eq!(&cover(&late, 4096)[..], &[PxRect::new(0, 0, 128, 70)]);
    }

    /// `paint_quads` is the glow law: a gradient quad's columns ramp from its
    /// left edge, and a quad past the buffer's edge is cut there.
    #[test]
    fn paint_quads_ramps_from_the_quads_left_edge() {
        let (w, h) = (8usize, 2usize);
        let q = GlowQuad::gradient(0, 4, 0, 8, 1, 0x0000_0000, 0, 0x00FF_0000, 0);
        let mut px = vec![0u32; w * h];
        paint_quads(&mut px, w, h, &[q], (0, 1));
        assert_eq!(px[..w], [0; 8], "the first row is untouched");
        let row = &px[w..];
        assert_eq!(row[..4], [0; 4]);
        assert_eq!(row[4], crate::glow_lerp_rgb(0, 0x00FF_0000, 0, 8));
        assert_eq!(row[7], crate::glow_lerp_rgb(0, 0x00FF_0000, 3, 8));
        let before = px.clone();
        for off in [(usize::MAX, 0), (0, usize::MAX)] {
            paint_quads(&mut px, w, h, &[q], off);
            assert_eq!(px, before, "an offscreen offset is clipped");
        }
        for (w, h) in [(usize::MAX, 2), (8, 3)] {
            paint_quads(&mut px, w, h, &[q], (0, 0));
            paint_layers(&mut px, &frame(w, h), &[], &[]);
            assert_eq!(px, before, "an invalid framebuffer is left untouched");
        }
    }
}
