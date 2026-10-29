// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cursor aurora's bloom and SDR crown, one law for both renderers.

use crate::{Frame, GlowEnd, GlowQuad, GlowRamp, GlowRamps};

/// Half-resolution divisor for the bloom target.
pub const BLOOM_DOWNSCALE: u32 = 2;
/// The default bloom strength: the share of the blurred glow added back over the frame.
pub const BLOOM_STRENGTH: f32 = 0.85;
/// The default blur radius in half-res texels: how far the halo spreads.
pub const BLOOM_RADIUS: f32 = 2.2;
/// The chroma radius of the same 25-tap kernel (half-res texels).
pub const BLOOM_CHROMA_RADIUS: f32 = 1.2;
/// The chroma radius as a share of the white one.
pub const BLOOM_CHROMA_SHARE: f32 = BLOOM_CHROMA_RADIUS / BLOOM_RADIUS;

/// The chroma radius paired with a white blur `radius`.
#[inline]
#[must_use]
pub const fn chroma_radius(radius: f32) -> f32 {
    radius * BLOOM_CHROMA_SHARE
}

/// How far the bloom's light reaches past the glow's bounding box.
#[inline]
#[must_use]
pub fn reach_px(radius: f32) -> u32 {
    (((2.0 * radius + 1.0) * BLOOM_DOWNSCALE as f32).ceil() as u32).saturating_add(1)
}

/// The bloom's extract set for a frame of `rows` grid rows.
pub fn source_quads_into(
    glow: &[GlowQuad],
    rows: usize,
    out: &mut Vec<GlowQuad>,
) -> (Option<[u32; 4]>, usize) {
    source_into(rows, out, |x| glow.iter().for_each(|q| x.add(*q)))
}

/// [`source_quads_into`] for a glow stream with ramps: its lights in draw
/// order, each lit ramp row the quad it stands for.
pub fn source_stream_into(
    glow: &[GlowQuad],
    ramps: &GlowRamps,
    rows: usize,
    out: &mut Vec<GlowQuad>,
) -> (Option<[u32; 4]>, usize) {
    source_into(rows, out, |x| {
        use crate::glow_ramps::Lights;
        crate::glow_ramps::each_laid(glow, ramps, |laid| match laid {
            Lights::Quads(run) => run.iter().for_each(|q| x.add(*q)),
            Lights::Ramp(ramp) => {
                if usize::from(ramp.row) < rows {
                    ramp.quads(&ramps.ends).for_each(|q| x.add(q));
                }
            }
        });
    })
}

/// Header words before an item list's first instances ([`BloomItems::words`]).
pub const ITEM_HEAD: usize = 4;

/// The bloom's lights as the GPU's item readers draw them: the extract set
/// of [`source_stream_into`] (and the crown's, its lights unwidened) in the
/// stream's draw order, with no ramp row expanded.
///
/// Each item is one of the stream's quads or one of its ramps, both shaped
/// as a ramp: a quad `h` rows tall is a ramp whose run is all of them, so it
/// keeps one row of ends. Instance `i` of the item draw is row `s = i -
/// first` of the last item whose first instance `first` is at most `i`, and
/// draws the quad that row stands for ([`aterm_core::render::GlowRamp::quads`]),
/// or nothing where both its ends are zero; the extract widens each to a
/// half-res texel as [`source_quads_into`] widens its copies.
/// [`BloomItems::light`] is the readers' twin (`glow_item` in `cell.metal`,
/// `hdr_glow.metal`, their WGSL twins and `cell.glsl`, `hdr_glow.glsl`).
#[derive(Clone, Debug, Default)]
pub struct BloomItems {
    first: Vec<u32>,
    recs: Vec<[u32; 4]>,
    ends: Vec<u32>,
    words: Vec<u32>,
    instances: u32,
    footprint: Option<[u32; 4]>,
}

impl BloomItems {
    /// The words the GPU reads: [`ITEM_HEAD`] header words (the item count,
    /// the word its records start at, the word its ends start at, the
    /// instance count), each item's first instance, each item's record
    /// (`x | y << 16`, `w | h << 16`, `join | joined << 16` and its first
    /// end, counted from the ends' start), then the ends
    /// ([`aterm_core::render::GlowEnd`]): the ramps' own, then two a quad.
    #[must_use]
    #[inline]
    pub fn words(&self) -> &[u32] {
        &self.words
    }

    /// The instances an item draw draws: every row of ends its items keep.
    #[must_use]
    #[inline]
    pub const fn instances(&self) -> u32 {
        self.instances
    }

    /// The union of the rects its lit rows draw unwidened (the crown's
    /// footprint), `[x0, y0, x1, y1]`, or `None` where nothing draws.
    #[must_use]
    #[inline]
    pub const fn footprint(&self) -> Option<[u32; 4]> {
        self.footprint
    }

    /// How many items the list holds.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.first.len()
    }

    /// Whether the list holds no item.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.first.is_empty()
    }

    /// What instance `i` draws, read off [`Self::words`] as the GPU reads
    /// them: its quad (its `row` tag 0), widened to a half-res texel with its
    /// colour scaled down to match where `widen` (the extract), or `None`
    /// where its row is dark or `i` is past the list.
    #[must_use]
    pub fn light(&self, i: u32, widen: bool) -> Option<GlowQuad> {
        let w = &self.words;
        let n = *w.first()? as usize;
        if n == 0 || i >= w[3] {
            return None;
        }
        let first = &w[ITEM_HEAD..ITEM_HEAD + n];
        let k = first.partition_point(|&f| f <= i) - 1;
        let s = i - first[k];
        let rec = &w[w[1] as usize + 4 * k..][..4];
        let e = w[2] as usize + rec[3] as usize + 2 * s as usize;
        let (e0, e1) = (w[e], w[e + 1]);
        if e0 | e1 == 0 {
            return None;
        }
        let lo = |v: u32| (v & 0xFFFF) as u16;
        let hi = |v: u32| (v >> 16) as u16;
        let (join, joined) = (u32::from(lo(rec[2])), u32::from(hi(rec[2])));
        let mut q = GlowQuad {
            row: 0,
            x: lo(rec[0]),
            y: (u32::from(hi(rec[0])) + s + if s > join { joined } else { 0 }) as u16,
            w: lo(rec[1]),
            h: if s == join { joined as u16 + 1 } else { 1 },
            color: crate::end_color(e0),
            alpha: crate::end_alpha(e0),
            color2: crate::end_color(e1),
            alpha2: crate::end_alpha(e1),
        };
        if widen {
            widen_to_texel(&mut q);
        }
        Some(q)
    }
}

/// `q` widened to a half-res texel each way, its colour scaled down by the
/// share of the texel it covers: the extract's copy of a thin light
/// ([`source_quads_into`]).
#[inline]
fn widen_to_texel(q: &mut GlowQuad) {
    let ds = BLOOM_DOWNSCALE as u16;
    let (w1, num_w, den_w) = if q.w > 0 && q.w < ds {
        (ds, u32::from(q.w), u32::from(ds))
    } else {
        (q.w, 1, 1)
    };
    let (h1, num_h, den_h) = if q.h > 0 && q.h < ds {
        (ds, u32::from(q.h), u32::from(ds))
    } else {
        (q.h, 1, 1)
    };
    let (num, den) = (num_w * num_h, den_w * den_h);
    if den > 1 {
        let scale = |c: u32| {
            let ch = |sh: u32| (((c >> sh) & 0xff) * num + den / 2) / den;
            (ch(16) << 16) | (ch(8) << 8) | ch(0)
        };
        q.color = scale(q.color);
        q.color2 = scale(q.color2);
    }
    q.w = w1;
    q.h = h1;
}

/// The bloom's item list ([`BloomItems`]) of a glow stream with ramps, then
/// `extra` lights (the fx layers' that bloom), for a frame of `rows` grid
/// rows, into `out`; returns the lights' bounding box, as
/// [`source_stream_into`] does for the same stream.
pub fn items_into(
    glow: &[GlowQuad],
    ramps: &GlowRamps,
    extra: &[GlowQuad],
    rows: usize,
    out: &mut BloomItems,
) -> Option<[u32; 4]> {
    use crate::glow_ramps::Lights;
    out.first.clear();
    out.recs.clear();
    out.ends.clear();
    let mut list = Items {
        rows,
        quad_ink: u32::try_from(ramps.ends.len()).unwrap_or(u32::MAX),
        out,
        bounds: Bounds::EMPTY,
        foot: None,
        instances: 0,
    };
    crate::glow_ramps::each_laid(glow, ramps, |laid| match laid {
        Lights::Quads(run) => run.iter().for_each(|q| list.quad(q)),
        Lights::Ramp(ramp) => list.ramp(ramp, &ramps.ends),
    });
    extra.iter().for_each(|q| list.quad(q));
    let Items {
        out,
        bounds,
        foot,
        instances,
        ..
    } = list;
    out.instances = instances;
    out.footprint = foot;
    let n = out.first.len();
    let place = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    let w = &mut out.words;
    w.clear();
    w.extend_from_slice(&[
        place(n),
        place(ITEM_HEAD + n),
        place(ITEM_HEAD + 5 * n),
        instances,
    ]);
    w.extend_from_slice(&out.first);
    w.extend(out.recs.iter().flatten());
    w.extend_from_slice(&ramps.ends);
    w.extend_from_slice(&out.ends);
    bounds.rect()
}

/// An item list being gathered ([`items_into`]).
struct Items<'a> {
    rows: usize,
    /// Where the next quad's ends go among the list's ends.
    quad_ink: u32,
    out: &'a mut BloomItems,
    bounds: Bounds,
    /// The union of the rects its lit rows draw unwidened.
    foot: Option<[u32; 4]>,
    instances: u32,
}

impl Items<'_> {
    #[inline]
    fn quad(&mut self, q: &GlowQuad) {
        if usize::from(q.row) >= self.rows {
            return;
        }
        self.bounds.add(q);
        if q.w == 0 || q.h == 0 {
            return;
        }
        self.lit(q);
        self.out.first.push(self.instances);
        self.instances += 1;
        self.out.recs.push([
            u32::from(q.x) | u32::from(q.y) << 16,
            u32::from(q.w) | u32::from(q.h) << 16,
            u32::from(q.h - 1) << 16,
            self.quad_ink,
        ]);
        self.out.ends.push(crate::glow_end(q.color, q.alpha));
        self.out.ends.push(crate::glow_end(q.color2, q.alpha2));
        self.quad_ink += 2;
    }

    /// Add a lit row `q`'s unwidened rect to the footprint.
    #[inline]
    fn lit(&mut self, q: &GlowQuad) {
        let (x, y) = (u32::from(q.x), u32::from(q.y));
        let r = [x, y, x + u32::from(q.w), y + u32::from(q.h)];
        self.foot = Some(self.foot.map_or(r, |f| {
            [
                f[0].min(r[0]),
                f[1].min(r[1]),
                f[2].max(r[2]),
                f[3].max(r[3]),
            ]
        }));
    }

    #[inline]
    fn ramp(&mut self, r: &GlowRamp, ends: &[GlowEnd]) {
        if usize::from(r.row) >= self.rows {
            return;
        }
        // Its rows run down the one column, each below the last, so its
        // first and last lit rows bound them all.
        let kept = r.kept_ends(ends).as_chunks::<2>().0;
        let lit = |e: &[GlowEnd; 2]| e[0] | e[1] != 0;
        let Some(top) = kept.iter().position(lit) else {
            return;
        };
        let foot = kept.iter().rposition(lit).unwrap_or(top);
        for s in [top, foot] {
            // A row of kept ends below the run sits `joined` rows lower.
            let s = s as u16;
            let row = s + if s > r.join { r.joined } else { 0 };
            if let Some(q) = r.row_quad(ends, row) {
                self.bounds.add(&q);
                self.lit(&q);
            }
        }
        self.out.first.push(self.instances);
        self.instances += u32::from(r.h - r.joined);
        self.out.recs.push([
            u32::from(r.x) | u32::from(r.y) << 16,
            u32::from(r.w) | u32::from(r.h) << 16,
            u32::from(r.join) | u32::from(r.joined) << 16,
            r.ink,
        ]);
    }
}

/// The bounding box of an extract set's lights, each side at least a
/// half-res texel.
#[derive(Clone, Copy)]
struct Bounds {
    bbox: (u16, u16, u16, u16),
    any: bool,
}

impl Bounds {
    const EMPTY: Self = Self {
        bbox: (u16::MAX, u16::MAX, 0, 0),
        any: false,
    };

    #[inline]
    fn add(&mut self, q: &GlowQuad) {
        let ds = BLOOM_DOWNSCALE as u16;
        let ew = if q.w > 0 { q.w.max(ds) } else { 0 };
        let eh = if q.h > 0 { q.h.max(ds) } else { 0 };
        let (bx0, by0, bx1, by1) = &mut self.bbox;
        *bx0 = (*bx0).min(q.x);
        *by0 = (*by0).min(q.y);
        *bx1 = (*bx1).max(q.x.saturating_add(ew));
        *by1 = (*by1).max(q.y.saturating_add(eh));
        self.any = true;
    }

    #[inline]
    fn rect(self) -> Option<[u32; 4]> {
        let (bx0, by0, bx1, by1) = self.bbox;
        self.any.then_some([
            u32::from(bx0),
            u32::from(by0),
            u32::from(bx1),
            u32::from(by1),
        ])
    }
}

/// The extract set being gathered: its lights so far, their bounds and
/// whether any is thinner than a half-res texel.
struct Extract<'a> {
    rows: usize,
    out: &'a mut Vec<GlowQuad>,
    bounds: Bounds,
    any_thin: bool,
}

impl Extract<'_> {
    #[inline]
    fn add(&mut self, q: GlowQuad) {
        let ds = BLOOM_DOWNSCALE as u16;
        if usize::from(q.row) >= self.rows {
            return;
        }
        self.any_thin |= (q.w > 0 && q.w < ds) || (q.h > 0 && q.h < ds);
        self.bounds.add(&q);
        self.out.push(q);
    }
}

fn source_into(
    rows: usize,
    out: &mut Vec<GlowQuad>,
    lights: impl FnOnce(&mut Extract<'_>),
) -> (Option<[u32; 4]>, usize) {
    out.clear();
    let mut x = Extract {
        rows,
        out,
        bounds: Bounds::EMPTY,
        any_thin: false,
    };
    lights(&mut x);
    let Extract {
        out,
        bounds,
        any_thin,
        ..
    } = x;
    let n = out.len();
    let extract_first = if any_thin {
        for i in 0..n {
            let mut q = out[i];
            widen_to_texel(&mut q);
            out.push(q);
        }
        n
    } else {
        0
    };
    (bounds.rect(), extract_first)
}

/// The bloom's tunables for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BloomParams {
    /// The share of the blurred glow added back ([`BLOOM_STRENGTH`] default).
    pub strength: f32,
    /// The white part's blur radius in half-res texels; the chroma's is [`chroma_radius`] of it.
    pub radius: f32,
}

impl Default for BloomParams {
    fn default() -> Self {
        Self {
            strength: BLOOM_STRENGTH,
            radius: BLOOM_RADIUS,
        }
    }
}

/// The colour channels of a frame pixel (`0xTTRRGGBB`, `TT` transmittance)
/// as floats in `0..=1`.
#[inline]
const fn channels(p: u32) -> [f32; 3] {
    [
        ((p >> 16) & 0xff) as f32 / 255.0,
        ((p >> 8) & 0xff) as f32 / 255.0,
        (p & 0xff) as f32 / 255.0,
    ]
}

/// SCREEN `src` (`0..=1` per channel, clamped first as a Unorm target clamps a
/// fragment) over the pixel `dst`, rounded once on store; the transmittance
/// byte rides through (the GPU writes colour only).
#[inline]
fn screen(dst: u32, src: [f32; 3]) -> u32 {
    let d = channels(dst);
    let mut out = dst & 0xFF00_0000;
    for (k, sh) in [16u32, 8, 0].into_iter().enumerate() {
        let s = src[k].clamp(0.0, 1.0);
        let v = s + d[k] * (1.0 - s);
        out |= ((v * 255.0).round().clamp(0.0, 255.0) as u32) << sh;
    }
    out
}

/// Bloom `frame` (a grid of `rows` rows) with its `glow` stream, as the GPU's post pass does.
/// # Errors
/// Invalid frame geometry, nonfinite tunables, a negative radius or scratch
/// reservation failure is reported before any frame pixel changes.
pub fn apply(
    frame: &mut Frame,
    glow: &[GlowQuad],
    rows: usize,
    params: BloomParams,
) -> Result<(), String> {
    check_frame(frame)?;
    if !params.strength.is_finite() || !params.radius.is_finite() || params.radius < 0.0 {
        return Err("bloom requires finite strength and a finite nonnegative radius".to_string());
    }
    let (w, h) = (frame.width, frame.height);
    if w == 0 || h == 0 || params.strength <= 0.0 {
        return Ok(());
    }
    let mut set = Vec::new();
    let count = glow.iter().filter(|q| usize::from(q.row) < rows).count();
    let capacity = count
        .checked_mul(2)
        .ok_or_else(|| "bloom quad count overflow".to_string())?;
    set.try_reserve_exact(capacity)
        .map_err(|e| format!("cannot reserve bloom quads: {e}"))?;
    let (Some([x0, y0, x1, y1]), extract_first) = source_quads_into(glow, rows, &mut set) else {
        return Ok(());
    };
    let ds = BLOOM_DOWNSCALE as usize;
    let (bw, bh) = ((w / ds).max(1), (h / ds).max(1));
    crate::checked_frame_size_px(bh, bw, 1, 1, 0, 0)?;
    let mut half = Vec::new();
    let len = bw
        .checked_mul(bh)
        .ok_or_else(|| "bloom dimensions overflow".to_string())?;
    half.try_reserve_exact(len)
        .map_err(|e| format!("cannot reserve bloom target: {e}"))?;
    half.resize(len, 0u32);
    for q in &set[extract_first..] {
        if q.w == 0 || q.h == 0 {
            continue;
        }
        let (qx, qy, qw, qh) = (
            usize::from(q.x),
            usize::from(q.y),
            usize::from(q.w),
            usize::from(q.h),
        );
        let first = |start: usize| start / ds;
        let last = |end: usize| end.saturating_sub(1).div_ceil(ds);
        let (tx0, tx1) = (first(qx), last(qx + qw).min(bw));
        let (ty0, ty1) = (first(qy), last(qy + qh).min(bh));
        let flat = q.is_flat();
        for ty in ty0..ty1 {
            let cy = ty * ds + 1;
            if cy < qy || cy >= qy + qh {
                continue;
            }
            for tx in tx0..tx1 {
                let cx = tx * ds + 1;
                if cx < qx || cx >= qx + qw {
                    continue;
                }
                let px = &mut half[ty * bw + tx];
                *px = if flat {
                    crate::over_premul(*px, q.color, q.alpha)
                } else {
                    let i = (cx - qx) as u32;
                    let wq = u32::from(q.w);
                    let a = crate::glow_lerp(u32::from(q.alpha), u32::from(q.alpha2), i, wq) as u8;
                    crate::over_premul(*px, crate::glow_lerp_rgb(q.color, q.color2, i, wq), a)
                };
            }
        }
    }
    let sample = |u: f32, v: f32| -> [f32; 3] {
        let fx = u * bw as f32 - 0.5;
        let fy = v * bh as f32 - 0.5;
        let (ix, iy) = (fx.floor(), fy.floor());
        let (ax, ay) = (fx - ix, fy - iy);
        let clampx = |x: f32| (x.max(0.0) as usize).min(bw - 1);
        let clampy = |y: f32| (y.max(0.0) as usize).min(bh - 1);
        let (xa, xb) = (clampx(ix), clampx(ix + 1.0));
        let (ya, yb) = (clampy(iy), clampy(iy + 1.0));
        let t = |x: usize, y: usize| channels(half[y * bw + x]);
        let (aa, ba, ab, bb) = (t(xa, ya), t(xb, ya), t(xa, yb), t(xb, yb));
        let mut out = [0.0; 3];
        for (k, o) in out.iter_mut().enumerate() {
            let top = aa[k] * (1.0 - ax) + ba[k] * ax;
            let bottom = ab[k] * (1.0 - ax) + bb[k] * ax;
            *o = top * (1.0 - ay) + bottom * ay;
        }
        out
    };
    let white_part = |c: [f32; 3]| c[0].min(c[1]).min(c[2]);
    let extent = bw.max(bh) as f32;
    let (rw, rc) = (
        params.radius.min(extent),
        chroma_radius(params.radius).min(extent),
    );
    let (tu, tv) = (1.0 / bw as f32, 1.0 / bh as f32);
    let d = reach_px(params.radius) as usize;
    let sx0 = (x0 as usize).saturating_sub(d);
    let sy0 = (y0 as usize).saturating_sub(d);
    let sx1 = (x1 as usize).saturating_add(d).min(w);
    let sy1 = (y1 as usize).saturating_add(d).min(h);
    struct BloomKernel {
        taps: [(f32, f32, f32); 25],
        wsum: f32,
    }
    static KERNEL: std::sync::OnceLock<BloomKernel> = std::sync::OnceLock::new();
    let BloomKernel {
        taps: ref kernel,
        wsum,
    } = *KERNEL.get_or_init(|| {
        let mut taps = [(0.0f32, 0.0f32, 0.0f32); 25];
        let mut wsum = 0.0f32;
        let mut idx = 0usize;
        for j in -2i32..=2 {
            for i in -2i32..=2 {
                let weight = (-((i * i + j * j) as f32) / 4.0).exp();
                taps[idx] = (i as f32, j as f32, weight);
                wsum += weight;
                idx += 1;
            }
        }
        BloomKernel { taps, wsum }
    });
    let mut taps = [(0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32); 25];
    for (tap, &(fi, fj, weight)) in taps.iter_mut().zip(kernel.iter()) {
        let (oi, oj) = (fi * tu, fj * tv);
        *tap = (oi * rw, oj * rw, oi * rc, oj * rc, weight);
    }
    for py in sy0..sy1 {
        let v = (py as f32 + 0.5) / h as f32;
        for px in sx0..sx1 {
            let u = (px as f32 + 0.5) / w as f32;
            let mut white = 0.0f32;
            let mut chroma = [0.0f32; 3];
            for &(du_w, dv_w, du_c, dv_c, weight) in &taps {
                let sw = sample(u + du_w, v + dv_w);
                white += white_part(sw) * weight;
                let sc = sample(u + du_c, v + dv_c);
                let wc = white_part(sc);
                for k in 0..3 {
                    chroma[k] += (sc[k] - wc) * weight;
                }
            }
            let src = [
                (white + chroma[0]) / wsum * params.strength,
                (white + chroma[1]) / wsum * params.strength,
                (white + chroma[2]) / wsum * params.strength,
            ];
            if src.iter().all(|&c| c <= 0.0) {
                continue;
            }
            let idx = py * w + px;
            frame.pixels[idx] = screen(frame.pixels[idx], src);
        }
    }
    Ok(())
}

/// How far past a lettered cell's box a frame whose bloom spares its letters
/// ([`aterm_core::render::BloomGate::SparesLetters`]) keeps the post passes' source
/// ([`spare_letters`]): the blur's whole reach ([`reach_px`]) and the
/// half-res texel the extract widens a thin light to, so no light the passes
/// keep reaches the cell's pixels.
#[inline]
#[must_use]
pub fn letter_shield_px(radius: f32) -> u32 {
    reach_px(radius).saturating_add(BLOOM_DOWNSCALE)
}

/// How far past a lettered cell's box the SDR crown's source keeps off it
/// when the frame's bloom spares its letters: the crown screens each light
/// over its own rect alone, so only the ring a letter's ground is read in
/// (two pixels past its ink, which may lie at the cell's edge) needs keeping.
pub const CROWN_SHIELD_PX: u32 = 2;

/// The post passes' sources of a frame whose bloom spares its letters
/// ([`aterm_core::render::BloomGate::SparesLetters`]): `source` (the lights
/// both passes read, window px) kept [`CROWN_SHIELD_PX`] off every lettered
/// cell into `crown`, and [`letter_shield_px`] off them into `bloom`. Both
/// renderers draw from these, so they bloom and crown alike.
pub fn spared_sources(
    source: &[GlowQuad],
    input: &aterm_core::render::RenderInput,
    grid: LetterGrid,
    radius: f32,
    crown: &mut Vec<GlowQuad>,
    bloom: &mut Vec<GlowQuad>,
) {
    crown.clear();
    crown.extend_from_slice(source);
    spare_letters(crown, input, grid, CROWN_SHIELD_PX);
    bloom.clear();
    bloom.extend_from_slice(source);
    spare_letters(bloom, input, grid, letter_shield_px(radius));
}

/// Where a frame's grid lies in its window pixels ([`spare_letters`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LetterGrid {
    /// The grid's left edge.
    pub pad: usize,
    /// The grid's top edge.
    pub grid_top: usize,
    /// Cell width, px.
    pub cell_w: usize,
    /// Cell height, px.
    pub cell_h: usize,
}

/// Whether a cell holds ink a post pass must not light: a glyph, or the
/// right half of a wide one.
#[inline]
fn lettered(c: &aterm_core::terminal::RenderCell) -> bool {
    c.wide || !(c.ch == ' ' || c.ch == '\0' || c.ch.is_control())
}

/// Cut the post passes' source (`source`: the lights the bloom extracts and
/// the SDR crown screens, window px) away from every lettered cell of
/// `input`, each cell's box grown by `shield` px ([`letter_shield_px`]): a
/// light is kept only where it lies farther than that from every glyph, so
/// neither the bloom's blur nor the crown lays light on a letter. The
/// frame's lights themselves are drawn whole; only their spill is withheld.
///
/// A light cut across its width keeps its gradient: each piece takes the
/// colour and opacity the whole light had at its edges.
pub fn spare_letters(
    source: &mut Vec<GlowQuad>,
    input: &aterm_core::render::RenderInput,
    grid: LetterGrid,
    shield: u32,
) {
    if source.is_empty() || grid.cell_w == 0 || grid.cell_h == 0 {
        return;
    }
    let (mut bx0, mut by0, mut bx1, mut by1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for q in source.iter().filter(|q| q.w > 0 && q.h > 0) {
        bx0 = bx0.min(i64::from(q.x));
        by0 = by0.min(i64::from(q.y));
        bx1 = bx1.max(i64::from(q.x) + i64::from(q.w));
        by1 = by1.max(i64::from(q.y) + i64::from(q.h));
    }
    if bx0 >= bx1 || by0 >= by1 {
        return;
    }
    let grow = i64::from(shield);
    let slack = i64::from(input.scroll_frac_px.unsigned_abs());
    let (cw, ch) = (grid.cell_w as i64, grid.cell_h as i64);
    let (pad, top) = (grid.pad as i64, grid.grid_top as i64);
    let mut shields: Vec<[i64; 4]> = Vec::new();
    for (r, line) in input.cells.iter().enumerate().take(input.rows) {
        let y0 = top + r as i64 * ch - slack - grow;
        let y1 = top + (r as i64 + 1) * ch + slack + grow;
        if y1 <= by0 || y0 >= by1 {
            continue;
        }
        let mut run: Option<(i64, i64)> = None;
        for (c, cell) in line.iter().enumerate() {
            if !lettered(cell) {
                continue;
            }
            let (x0, x1) = (pad + c as i64 * cw - grow, pad + (c as i64 + 1) * cw + grow);
            run = match run {
                Some((a, b)) if x0 <= b => Some((a, x1)),
                Some((a, b)) => {
                    shields.push([a, y0, b, y1]);
                    Some((x0, x1))
                }
                None => Some((x0, x1)),
            };
        }
        if let Some((a, b)) = run {
            shields.push([a, y0, b, y1]);
        }
    }
    shields.retain(|s| s[0] < bx1 && s[2] > bx0);
    if shields.is_empty() {
        return;
    }
    let mut kept = Vec::with_capacity(source.len());
    for s in &shields {
        kept.clear();
        for q in source.iter() {
            cut_away(*q, *s, &mut kept);
        }
        std::mem::swap(source, &mut kept);
    }
}

/// `q` with the rect `s` cut out of it, its pieces pushed to `out` (`q`
/// itself where they do not meet).
fn cut_away(q: GlowQuad, s: [i64; 4], out: &mut Vec<GlowQuad>) {
    let (qx0, qy0) = (i64::from(q.x), i64::from(q.y));
    let (qx1, qy1) = (qx0 + i64::from(q.w), qy0 + i64::from(q.h));
    let [sx0, sy0, sx1, sy1] = s;
    if q.w == 0 || q.h == 0 || sx0 >= qx1 || sx1 <= qx0 || sy0 >= qy1 || sy1 <= qy0 {
        out.push(q);
        return;
    }
    let band = |y0: i64, y1: i64| GlowQuad {
        y: y0 as u16,
        h: (y1 - y0) as u16,
        ..q
    };
    if sy0 > qy0 {
        out.push(band(qy0, sy0));
    }
    if sy1 < qy1 {
        out.push(band(sy1, qy1));
    }
    let (my0, my1) = (qy0.max(sy0), qy1.min(sy1));
    if sx0 > qx0 {
        out.push(columns(band(my0, my1), 0, (sx0 - qx0) as u16));
    }
    if sx1 < qx1 {
        out.push(columns(band(my0, my1), (sx1 - qx0) as u16, q.w));
    }
}

/// Columns `a..b` of `q`, its gradient's colour and opacity at their edges.
fn columns(q: GlowQuad, a: u16, b: u16) -> GlowQuad {
    let w = u32::from(q.w).max(1);
    let edge = |c0: u32, c1: u32, at: u16| -> u32 {
        let at = u32::from(at);
        (c0 * (w - at) + c1 * at + w / 2) / w
    };
    let edge_rgb = |c0: u32, c1: u32, at: u16| -> u32 {
        if c0 == c1 {
            return c0 & 0x00FF_FFFF;
        }
        let ch = |sh: u32| edge((c0 >> sh) & 0xff, (c1 >> sh) & 0xff, at);
        (ch(16) << 16) | (ch(8) << 8) | ch(0)
    };
    GlowQuad {
        x: q.x + a,
        w: b - a,
        color: edge_rgb(q.color, q.color2, a),
        alpha: edge(u32::from(q.alpha), u32::from(q.alpha2), a) as u8,
        color2: edge_rgb(q.color, q.color2, b),
        alpha2: edge(u32::from(q.alpha), u32::from(q.alpha2), b) as u8,
        ..q
    }
}

fn check_frame(frame: &Frame) -> Result<(), String> {
    crate::checked_frame_size_px(frame.height, frame.width, 1, 1, 0, 0)?;
    if frame.width.checked_mul(frame.height) != Some(frame.pixels.len()) {
        return Err("bloom frame dimensions do not match its pixels".to_string());
    }
    Ok(())
}

/// The SDR crown over `frame` (a grid of `rows` rows).
/// # Errors
/// Invalid frame geometry or a nonfinite budget is reported before any pixel changes.
pub fn sdr_crown(
    frame: &mut Frame,
    glow: &[GlowQuad],
    rows: usize,
    budget: f32,
) -> Result<(), String> {
    check_frame(frame)?;
    if !budget.is_finite() {
        return Err("SDR crown requires a finite budget".to_string());
    }
    let (w, h) = (frame.width, frame.height);
    if budget <= 0.0 || w == 0 || h == 0 {
        return Ok(());
    }
    for q in glow.iter().filter(|q| usize::from(q.row) < rows) {
        let (qx, qy) = (usize::from(q.x), usize::from(q.y));
        let (xe, ye) = (
            (qx + usize::from(q.w)).min(w),
            (qy + usize::from(q.h)).min(h),
        );
        if q.color == q.color2 {
            let src = channels(q.color).map(|v| v * budget);
            for py in qy..ye {
                for px in qx..xe {
                    let idx = py * w + px;
                    frame.pixels[idx] = screen(frame.pixels[idx], src);
                }
            }
            continue;
        }
        let wq = u32::from(q.w);
        for px in qx..xe {
            let c = crate::glow_lerp_rgb(q.color, q.color2, (px - qx) as u32, wq);
            let src = channels(c).map(|v| v * budget);
            for py in qy..ye {
                let idx = py * w + px;
                frame.pixels[idx] = screen(frame.pixels[idx], src);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, bg: u32) -> Frame {
        Frame {
            width: w,
            height: h,
            pixels: vec![bg; w * h],
        }
    }

    /// The bloom spreads the glow's light past its own rect, falls off with
    /// distance, and leaves the frame untouched beyond its reach; the halo of
    /// a red quad is red. NEGATIVE CONTROL: no glow, no change.
    #[test]
    fn the_bloom_spreads_light_past_the_glow_and_no_further_than_its_reach() {
        let mut f = frame(80, 40, 0);
        let glow = [GlowQuad::flat(1, 30, 16, 8, 8, 0x00E0_2020, 0)];
        apply(&mut f, &glow, 3, BloomParams::default()).unwrap();
        let at = |f: &Frame, x: usize, y: usize| f.pixels[y * 80 + x];
        let red = |p: u32| (p >> 16) & 0xff;
        assert!(red(at(&f, 34, 20)) > red(at(&f, 26, 20)));
        assert!(red(at(&f, 26, 20)) > 0, "light past the quad's own rect");
        assert!(red(at(&f, 26, 20)) > (at(&f, 26, 20) & 0xff), "a red halo");
        let reach = reach_px(BLOOM_RADIUS) as usize;
        assert_eq!(at(&f, 30 - reach - 1, 20), 0, "nothing past the reach");
        let mut bare = frame(80, 40, 0);
        apply(&mut bare, &[], 3, BloomParams::default()).unwrap();
        assert!(bare.pixels.iter().all(|&p| p == 0));
    }

    /// A row of 10 cells 8x16 at the frame's origin, `x` standing in cells `3..5`.
    fn lettered_row() -> (aterm_core::render::RenderInput, LetterGrid) {
        let mut input = aterm_core::render::RenderInput::empty();
        input.rows = 1;
        input.cols = 10;
        let mut row = vec![crate::RenderCell::default(); 10];
        for cell in &mut row[3..5] {
            cell.ch = 'x';
        }
        input.cells = vec![row];
        let grid = LetterGrid {
            pad: 0,
            grid_top: 0,
            cell_w: 8,
            cell_h: 16,
        };
        (input, grid)
    }

    /// looks10 L1: the post passes' source keeps `shield` px off every lettered cell, and
    /// keeps every other pixel of the light once, each at the colour and opacity the whole
    /// light had there. NEGATIVE CONTROL: a light clear of the letters by more than the
    /// shield is kept whole.
    #[test]
    fn spared_lights_keep_off_the_letters_and_keep_the_rest_whole() {
        let (input, grid) = lettered_row();
        let light = GlowQuad {
            color2: 0x0000_00FF,
            alpha2: 200,
            ..GlowQuad::flat(0, 0, 4, 80, 8, 0x00FF_0000, 40)
        };
        let shield = 3;
        let mut kept = vec![light];
        spare_letters(&mut kept, &input, grid, shield);
        let shielded = |x: usize, y: usize| {
            (24 - shield as usize..40 + shield as usize).contains(&x) && y < 16 + shield as usize
        };
        let mut covered = vec![0u8; 80 * 16];
        for q in &kept {
            for y in usize::from(q.y)..usize::from(q.y + q.h) {
                for x in usize::from(q.x)..usize::from(q.x + q.w) {
                    assert!(!shielded(x, y), "a kept light at ({x}, {y}) by a letter");
                    covered[y * 80 + x] += 1;
                    let at = |c: u32, c2: u32, q: &GlowQuad, x: usize| {
                        crate::glow_lerp_rgb(c, c2, (x - usize::from(q.x)) as u32, u32::from(q.w))
                    };
                    let (whole, piece) = (
                        at(light.color, light.color2, &light, x),
                        at(q.color, q.color2, q, x),
                    );
                    for sh in [16, 8, 0] {
                        let d = ((whole >> sh) & 0xff).abs_diff((piece >> sh) & 0xff);
                        assert!(
                            d <= 1,
                            "a piece's colour at x {x}: {piece:06x} for {whole:06x}"
                        );
                    }
                }
            }
        }
        for y in 4..12 {
            for x in 0..80 {
                let want = u8::from(!shielded(x, y));
                assert_eq!(
                    covered[y * 80 + x],
                    want,
                    "({x}, {y}) kept {} times",
                    covered[y * 80 + x]
                );
            }
        }
        let clear = GlowQuad::flat(0, 60, 4, 16, 8, 0x00FF_0000, 40);
        let mut whole = vec![clear];
        spare_letters(&mut whole, &input, grid, shield);
        assert_eq!(
            whole,
            vec![clear],
            "a light clear of the letters is kept whole"
        );
    }

    /// looks10 L1: the bloom of a light beside a letter, its source spared
    /// ([`spared_sources`]), lays nothing on the letter's cell; bloomed from the light whole
    /// (the control), it lights the cell.
    #[test]
    fn a_spared_bloom_lays_no_light_on_a_letter() {
        let (input, grid) = lettered_row();
        let light = [GlowQuad::flat(0, 44, 0, 32, 16, 0x00E0_E0E0, 0)];
        let (mut crown, mut blur) = (Vec::new(), Vec::new());
        spared_sources(&light, &input, grid, BLOOM_RADIUS, &mut crown, &mut blur);
        let letter_lit = |glow: &[GlowQuad]| {
            let mut f = frame(80, 16, 0);
            apply(&mut f, glow, 1, BloomParams::default()).unwrap();
            (0..16).any(|y| (24..40).any(|x| f.pixels[y * 80 + x] != 0))
        };
        assert!(
            letter_lit(&light),
            "control: the whole light blooms onto the letter"
        );
        assert!(
            !blur.is_empty(),
            "fixture: the light reaches past the bloom's shield"
        );
        assert!(!letter_lit(&blur), "the spared bloom lights the letter");
        assert_eq!(
            crown,
            light.to_vec(),
            "the crown keeps a light past its shield whole"
        );
    }

    /// The SDR crown screens each quad once at its colour times the budget.
    #[test]
    fn the_sdr_crown_screens_the_glow_at_its_budget() {
        let mut f = frame(20, 20, 0x0010_1010);
        sdr_crown(
            &mut f,
            &[GlowQuad::flat(0, 2, 2, 4, 4, 0x00FF_0000, 0)],
            2,
            0.4,
        )
        .unwrap();
        let p = f.pixels[3 * 20 + 3];
        assert_eq!((p >> 16) & 0xff, 112);
        assert_eq!(p & 0xff, 0x10, "a channel the quad lacks keeps the ground");
        assert_eq!(f.pixels[0], 0x0010_1010, "outside the quad nothing changes");
    }

    #[test]
    fn rejected_inputs_leave_pixels_intact_and_valid_retries_still_draw() {
        let glow = [GlowQuad::flat(0, 2, 2, 4, 4, 0x00FF_0000, 0)];
        for (width, height, len) in [
            (usize::MAX, 1, 1),
            (0, usize::MAX, 0),
            (8193, 8192, 1),
            (2, 2, 3),
            (1, 1, 2),
        ] {
            let mut f = Frame {
                width,
                height,
                pixels: vec![0x0010_1010; len],
            };
            let before = f.pixels.clone();
            assert!(apply(&mut f, &glow, 1, BloomParams::default()).is_err());
            assert_eq!(f.pixels, before);
            assert!(sdr_crown(&mut f, &glow, 1, 0.4).is_err());
            assert_eq!(f.pixels, before);
        }
        let mut f = frame(20, 20, 0x0010_1010);
        let before = f.pixels.clone();
        for (strength, radius) in [
            (f32::NAN, BLOOM_RADIUS),
            (f32::INFINITY, BLOOM_RADIUS),
            (BLOOM_STRENGTH, f32::NAN),
            (BLOOM_STRENGTH, f32::INFINITY),
            (BLOOM_STRENGTH, f32::NEG_INFINITY),
            (BLOOM_STRENGTH, -1.0),
        ] {
            assert!(apply(&mut f, &glow, 1, BloomParams { strength, radius }).is_err());
            assert_eq!(f.pixels, before);
        }
        assert!(sdr_crown(&mut f, &glow, 1, f32::NAN).is_err());
        assert_eq!(f.pixels, before);
        apply(&mut f, &glow, 1, BloomParams::default()).unwrap();
        assert_ne!(f.pixels, before);
        let mut empty = frame(0, 0, 0);
        apply(&mut empty, &glow, 1, BloomParams::default()).unwrap();
        sdr_crown(&mut empty, &glow, 1, 0.4).unwrap();
    }

    #[test]
    fn enormous_finite_radii_sample_clamped_edges_without_overflow() {
        assert_eq!(reach_px(BLOOM_RADIUS), 12);
        assert_eq!(reach_px(f32::MAX), u32::MAX);
        assert_eq!(reach_px(f32::INFINITY), u32::MAX);
        let glow = [GlowQuad::flat(0, 2, 2, 4, 4, 0x00FF_0000, 0)];
        let mut large = frame(20, 20, 0x0010_1010);
        let mut saturated = large.clone();
        for (frame, radius) in [(&mut large, 1000.0), (&mut saturated, f32::MAX)] {
            apply(
                frame,
                &glow,
                1,
                BloomParams {
                    radius,
                    ..BloomParams::default()
                },
            )
            .unwrap();
        }
        assert_eq!(large.pixels, saturated.pixels);
        assert!(large.pixels.iter().any(|&p| p != 0x0010_1010));
    }

    /// A stream for the item list's law: quads before, between and after
    /// its ramps (a tall one, a one-pixel-wide one, an over light, an empty
    /// one, one past the frame's rows), ramps with and without a run, dark
    /// rows inside one and at both its ends, a dark ramp, a one-pixel-wide
    /// ramp and a ramp past the frame's rows.
    fn stream() -> (Vec<GlowQuad>, GlowRamps, Vec<GlowQuad>) {
        let quad = |row, x, y, w, h, c: u32, a| GlowQuad {
            color2: c.rotate_left(8) & 0x00FF_FFFF,
            alpha2: a,
            ..GlowQuad::flat(row, x, y, w, h, c, a)
        };
        let mut glow = vec![
            quad(0, 3, 2, 6, 3, 0x0030_2010, 0),
            quad(1, 9, 20, 1, 1, 0x00FF_8040, 0),
            quad(1, 40, 21, 0, 1, 0x0011_1111, 0),
        ];
        let mut ramps = GlowRamps::default();
        let mut lay = |glow: &Vec<GlowQuad>, row, x, y, w, rows: &[(u32, u32)], run: (u16, u16)| {
            let ink = ramps.ends.len() as u32;
            for &(a, b) in rows {
                ramps.ends.extend([a, b]);
            }
            ramps.items.push(GlowRamp {
                at: glow.len() as u32,
                row,
                x,
                y,
                w,
                h: rows.len() as u16 + run.1,
                join: run.0,
                joined: run.1,
                ink,
            });
        };
        let lit = |k: u32| (0x0010_2030 + k * 0x0003_0507, 0x0050_4030 - k * 0x0001_0203);
        let rows: Vec<_> = (0..7).map(lit).collect();
        lay(&glow, 1, 12, 17, 9, &rows, (0, 0));
        glow.push(quad(1, 30, 18, 4, 2, 0x8020_4060, 0x80));
        lay(&glow, 1, 14, 18, 7, &rows[..5], (2, 3));
        let mut dark = rows.clone();
        for k in [0, 3, 6] {
            dark[k] = (0, 0);
        }
        lay(&glow, 2, 5, 33, 11, &dark, (0, 0));
        lay(&glow, 2, 7, 34, 4, &[(0, 0), (0, 0)], (0, 0));
        lay(&glow, 2, 21, 35, 1, &rows[..4], (1, 2));
        glow.push(quad(2, 2, 40, 5, 0, 0x0001_0101, 0));
        lay(&glow, 9, 1, 150, 6, &rows, (0, 0));
        glow.push(quad(9, 1, 160, 5, 1, 0x0001_0101, 0));
        glow.push(quad(2, 50, 44, 2, 1, 0x00F0_F0F0, 0));
        ramps.lit = ramps.count_lit();
        let extra = vec![
            quad(1, 60, 19, 3, 1, 0x0000_FF00, 0),
            quad(2, 61, 36, 8, 4, 0x00FF_0000, 0),
        ];
        (glow, ramps, extra)
    }

    /// The item list draws the extract set and the crown's lights: instance
    /// by instance its lit rows are the stream's lights, the crown's in
    /// their order and the extract's widened as its copies are, with the
    /// extra lights after them, and both bound alike.
    #[test]
    fn the_item_list_draws_its_streams_extract_set() {
        let (glow, ramps, extra) = stream();
        let rows = 4;
        let drawn = |set: &[GlowQuad]| -> Vec<GlowQuad> {
            set.iter()
                .filter(|q| q.w > 0 && q.h > 0)
                .map(|q| GlowQuad {
                    row: 0,
                    color: q.color & 0x00FF_FFFF,
                    color2: q.color2 & 0x00FF_FFFF,
                    ..*q
                })
                .collect()
        };
        let union = |set: &[GlowQuad]| -> Option<[u32; 4]> {
            drawn(set)
                .iter()
                .map(|q| {
                    let (x, y) = (u32::from(q.x), u32::from(q.y));
                    [x, y, x + u32::from(q.w), y + u32::from(q.h)]
                })
                .reduce(|a, b| {
                    [
                        a[0].min(b[0]),
                        a[1].min(b[1]),
                        a[2].max(b[2]),
                        a[3].max(b[3]),
                    ]
                })
        };
        let mut items = BloomItems::default();
        for extra in [&[][..], &extra[..]] {
            let mut all = Vec::new();
            ramps.expand_into(&glow, &mut all);
            all.extend_from_slice(extra);
            let mut set = Vec::new();
            let (bbox, first) = source_quads_into(&all, rows, &mut set);
            let n = if first > 0 { first } else { set.len() };
            if extra.is_empty() {
                let mut walked = Vec::new();
                assert_eq!(
                    source_stream_into(&glow, &ramps, rows, &mut walked),
                    (bbox, first)
                );
                assert_eq!(walked, set);
            }
            assert!(first > 0, "the stream has thin lights to widen");
            let got = items_into(&glow, &ramps, extra, rows, &mut items);
            assert_eq!(
                got, bbox,
                "the list bounds its lights as the extract set does"
            );
            let read = |widen| -> Vec<GlowQuad> {
                (0..items.instances() + 3)
                    .filter_map(|i| items.light(i, widen))
                    .collect()
            };
            assert_eq!(read(false), drawn(&set[..n]), "the crown's lights");
            assert_eq!(read(true), drawn(&set[first..]), "the extract's copies");
            assert_eq!(items.footprint(), union(&set[..n]), "the crown's footprint");
            assert!(items.len() < drawn(&set[..n]).len(), "ramps ride whole");
        }
        // Each ramp alone: its first and last lit rows bound it.
        for ramp in &ramps.items {
            let alone = GlowRamps {
                items: vec![GlowRamp { at: 0, ..*ramp }],
                ends: ramps.ends.clone(),
                lit: 0,
            };
            let mut set = Vec::new();
            let (bbox, _) = source_stream_into(&[], &alone, rows, &mut set);
            assert_eq!(
                items_into(&[], &alone, &[], rows, &mut items),
                bbox,
                "{ramp:?}"
            );
            let mut lit = Vec::new();
            alone.expand_into(&[], &mut lit);
            lit.retain(|q| usize::from(q.row) < rows);
            assert_eq!(items.footprint(), union(&lit), "{ramp:?}");
        }
        let skipped = ramps
            .items
            .iter()
            .filter(|r| usize::from(r.row) >= rows)
            .count();
        assert!(skipped > 0 && ramps.items.iter().any(|r| r.joined > 0));
        // A stream with nothing to bloom lists nothing and bounds nothing.
        assert_eq!(
            items_into(&[], &GlowRamps::default(), &[], rows, &mut items),
            None
        );
        assert!(items.is_empty() && items.instances() == 0 && items.light(0, true).is_none());
        assert_eq!(items.footprint(), None);
    }
}
