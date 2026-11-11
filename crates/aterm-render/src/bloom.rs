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
/// The chroma radius of the same kernel (half-res texels).
pub const BLOOM_CHROMA_RADIUS: f32 = 1.2;
/// The chroma radius as a share of the white one.
pub const BLOOM_CHROMA_SHARE: f32 = BLOOM_CHROMA_RADIUS / BLOOM_RADIUS;

/// The most taps the bloom's kernel lays each side of its centre ([`taps_per_side`]).
pub const BLOOM_TAPS_MAX: i32 = 3;

/// The taps the kernel's scratch holds at most: `(2 * TAPS_SIDE_CAP + 1)^2`.
const TAPS_CAP: usize = TAPS_ROWS_CAP * TAPS_ROWS_CAP;
/// The tap rows (and the taps a row) the kernel's scratch holds at most: `2 * TAPS_SIDE_CAP + 1`.
const TAPS_ROWS_CAP: usize = (2 * TAPS_SIDE_CAP + 1) as usize;
/// The most taps a side the kernel's scratch holds (a test may force more than
/// [`BLOOM_TAPS_MAX`]).
const TAPS_SIDE_CAP: i32 = 8;

#[cfg(test)]
thread_local! {
    /// A test's kernel, taps each side (`0`: [`taps_per_side`]'s); `2` is the negative
    /// control's 25-tap kernel at every radius.
    static FORCE_TAPS: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

/// The bloom kernel's taps each side of its centre for a white blur `radius` (half-res
/// texels): the kernel spans `2 * radius` either way in `2 * n + 1` taps a side, weighted
/// `exp(-d^2 / n^2)` by the tap's distance `d` in taps (the one Gaussian at every `n`). Two
/// (a 25-tap kernel) to a radius of 6, its taps up to 6 texels apart; three (49 taps) past
/// it. At the knob's top, 8, the 25-tap kernel's taps stood 16 px apart, wider than the
/// caret's cell, and its bloom lay as vertical bands right of the caret (composed r3's
/// access judge; the after-land lows' W5). Every arm reads it so: `fs_bloom` in
/// `bloom.metal`, `bloom.glsl` and `bloom.hlsl` computes `ceil(radius / 3)` the same way.
#[inline]
#[must_use]
pub fn taps_per_side(radius: f32) -> i32 {
    #[cfg(test)]
    {
        let forced = FORCE_TAPS.with(std::cell::Cell::get);
        if forced > 0 {
            return forced;
        }
    }
    ((radius / 3.0).ceil() as i32).clamp(2, BLOOM_TAPS_MAX)
}

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
    apply_kept_off(frame, glow, rows, params, None)
}

/// [`apply`], the bloom laid at each pixel at `mask`'s weight there ([`LetterMask::weight`]):
/// none on a cell the mask keeps the post passes off, nor within its ring.
/// # Errors
/// As [`apply`].
pub fn apply_kept_off(
    frame: &mut Frame,
    glow: &[GlowQuad],
    rows: usize,
    params: BloomParams,
    mask: Option<&LetterMask>,
) -> Result<(), String> {
    bloom_kept_off(frame, glow, rows, params, mask, Some(1.0))
}

/// [`apply_kept_off`], passing over a pixel no lit half-res texel lies within `quiet` of the
/// taps' reach of and each tap on four dark texels (`None`: none passed over, every pixel its two
/// samples a tap). Only a share below 1 can change a pixel: the negative control of
/// `a_quiet_pixel_passed_over_takes_no_light`.
fn bloom_kept_off(
    frame: &mut Frame,
    glow: &[GlowQuad],
    rows: usize,
    params: BloomParams,
    mask: Option<&LetterMask>,
    quiet: Option<f32>,
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
    // A tap on four dark texels reads zero whatever its weights, and adds nothing to a sum of
    // non-negative parts. A trail's half-res target is mostly dark, so most taps are such.
    let pass_dark_taps = quiet.is_some();
    let sample = |u: f32, v: f32| -> Option<[f32; 3]> {
        let fx = u * bw as f32 - 0.5;
        let fy = v * bh as f32 - 0.5;
        let (ix, iy) = (fx.floor(), fy.floor());
        let (ax, ay) = (fx - ix, fy - iy);
        let clampx = |x: f32| (x.max(0.0) as usize).min(bw - 1);
        let clampy = |y: f32| (y.max(0.0) as usize).min(bh - 1);
        let (xa, xb) = (clampx(ix), clampx(ix + 1.0));
        let (ya, yb) = (clampy(iy), clampy(iy + 1.0));
        let t = |x: usize, y: usize| half[y * bw + x];
        let (aa, ba, ab, bb) = (t(xa, ya), t(xb, ya), t(xa, yb), t(xb, yb));
        if pass_dark_taps && aa | ba | ab | bb == 0 {
            return None;
        }
        let (aa, ba, ab, bb) = (channels(aa), channels(ba), channels(ab), channels(bb));
        let mut out = [0.0; 3];
        for (k, o) in out.iter_mut().enumerate() {
            let top = aa[k] * (1.0 - ax) + ba[k] * ax;
            let bottom = ab[k] * (1.0 - ax) + bb[k] * ax;
            *o = top * (1.0 - ay) + bottom * ay;
        }
        Some(out)
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
    // The kernel's taps each side ([`taps_per_side`]): a 25-tap kernel to a radius of 6, a
    // 49-tap one past it, the same Gaussian spread over the same reach either way.
    let n = taps_per_side(params.radius);
    let side = (2 * n + 1) as usize;
    let step = 2.0 / n as f32;
    let inv_n2 = 1.0 / (n * n) as f32;
    let mut taps = [(0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32); TAPS_CAP];
    let mut count = 0usize;
    let mut wsum = 0.0f32;
    for j in -n..=n {
        for i in -n..=n {
            let weight = (-((i * i + j * j) as f32) * inv_n2).exp();
            let (oi, oj) = (i as f32 * step * tu, j as f32 * step * tv);
            taps[count] = (oi * rw, oj * rw, oi * rc, oj * rc, weight);
            wsum += weight;
            count += 1;
        }
    }
    let taps = &taps[..count];
    // A pixel's taps read the half-res texels within twice the wider radius of its own centre,
    // the kernel's reach at any tap count ([`taps_per_side`]), and each one's bilinear neighbour
    // (a texel more of slack covers rounding). Where none of those is lit, every sample is zero
    // and so is the pixel's light: it is passed over without its two samples a tap, as is a
    // pixel its mask keeps whole off. Most of a trail's box is such.
    let lit = quiet
        .map(|share| lit_table(&half, bw, bh).map(|t| (t, 2.0 * rw.max(rc) * share + 1.0)))
        .transpose()?;
    let span = |c: f32, reach: f32, len: usize| {
        let clamp = |x: f32| (x.max(0.0) as usize).min(len - 1);
        (clamp((c - reach).floor()), clamp((c + reach).floor() + 1.0))
    };
    // The half-res rows that hold a lit texel. The taps stand in `2 * n + 1` rows, each row's
    // `2 * n + 1` on the same two texel rows at a given pixel row: a tap row on two dark rows
    // reads nothing across, and a thin light leaves most of a pixel's tap rows so.
    let row_lit: Vec<bool> = half
        .chunks_exact(bw)
        .map(|r| r.iter().any(|&t| t != 0))
        .collect();
    for py in sy0..sy1 {
        let v = (py as f32 + 0.5) / h as f32;
        let ys = lit
            .as_ref()
            .map(|(_, reach)| span(v * bh as f32 - 0.5, *reach, bh));
        // Each tap row's two texel rows as [`sample`] finds them: white, chroma.
        let row_reads = |dv: f32| {
            let fy = (v + dv) * bh as f32 - 0.5;
            let iy = fy.floor();
            let clampy = |y: f32| (y.max(0.0) as usize).min(bh - 1);
            !pass_dark_taps || row_lit[clampy(iy)] || row_lit[clampy(iy + 1.0)]
        };
        let mut tap_rows = [(false, false); TAPS_ROWS_CAP];
        for (reads, row) in tap_rows.iter_mut().zip(taps.chunks_exact(side)) {
            *reads = (row_reads(row[0].1), row_reads(row[0].3));
        }
        for px in sx0..sx1 {
            let u = (px as f32 + 0.5) / w as f32;
            if let (Some((table, reach)), Some(ys)) = (&lit, ys)
                && lit_in(table, bw, span(u * bw as f32 - 0.5, *reach, bw), ys) == 0
            {
                continue;
            }
            let kept = mask.map_or(1.0, |m| m.weight(px as f32 + 0.5, py as f32 + 0.5));
            if kept <= 0.0 {
                continue;
            }
            let mut white = 0.0f32;
            let mut chroma = [0.0f32; 3];
            for (row, &(reads_w, reads_c)) in taps.chunks_exact(side).zip(&tap_rows) {
                for &(du_w, dv_w, du_c, dv_c, weight) in row {
                    if reads_w && let Some(sw) = sample(u + du_w, v + dv_w) {
                        white += white_part(sw) * weight;
                    }
                    if reads_c && let Some(sc) = sample(u + du_c, v + dv_c) {
                        let wc = white_part(sc);
                        for k in 0..3 {
                            chroma[k] += (sc[k] - wc) * weight;
                        }
                    }
                }
            }
            let src = [
                (white + chroma[0]) / wsum * params.strength * kept,
                (white + chroma[1]) / wsum * params.strength * kept,
                (white + chroma[2]) / wsum * params.strength * kept,
            ];
            if src.iter().all(|&c| c <= 0.0) {
                continue;
            }
            #[cfg(any(test, debug_assertions))]
            crate::test_counters::count_bloom_pixel();
            let idx = py * w + px;
            frame.pixels[idx] = screen(frame.pixels[idx], src);
        }
    }
    Ok(())
}

/// The summed-area table of `half`'s lit texels (`bw` x `bh`): entry `(x, y)` of its
/// `(bw + 1) x (bh + 1)` counts those above and left of texel `(x, y)`.
fn lit_table(half: &[u32], bw: usize, bh: usize) -> Result<Vec<u32>, String> {
    let stride = bw + 1;
    let len = stride
        .checked_mul(bh + 1)
        .ok_or_else(|| "bloom lit table overflow".to_string())?;
    let mut table = Vec::new();
    table
        .try_reserve_exact(len)
        .map_err(|e| format!("cannot reserve bloom lit table: {e}"))?;
    table.resize(len, 0u32);
    for y in 0..bh {
        let mut run = 0u32;
        for x in 0..bw {
            run += u32::from(half[y * bw + x] != 0);
            table[(y + 1) * stride + x + 1] = table[y * stride + x + 1] + run;
        }
    }
    Ok(table)
}

/// The lit texels of [`lit_table`]'s `table` in columns `xs` and rows `ys`, both inclusive.
fn lit_in(table: &[u32], bw: usize, xs: (usize, usize), ys: (usize, usize)) -> u32 {
    let at = |x: usize, y: usize| table[y * (bw + 1) + x];
    (at(xs.1 + 1, ys.1 + 1) + at(xs.0, ys.0)) - (at(xs.0, ys.1 + 1) + at(xs.1 + 1, ys.0))
}

/// How far past a lettered cell's box a frame whose bloom spares its letters
/// ([`aterm_core::render::BloomGate::SparesLetters`]) once kept the post passes' source
/// ([`spare_letters`]): the blur's whole reach ([`reach_px`]) and the half-res texel the
/// extract widens a thin light to, so no light the passes kept reached the cell's pixels.
/// The bloom's composite now keeps off the letters itself ([`LetterMask`]); this cut stands
/// only for a frame whose kept cells outrun the mask's bits ([`MASK_WORDS`]).
#[inline]
#[must_use]
pub fn letter_shield_px(radius: f32) -> u32 {
    reach_px(radius).saturating_add(BLOOM_DOWNSCALE)
}

/// How far past a lettered cell's box the post passes' source keeps off it when the frame's
/// bloom spares its letters: the crown screens each light over its own rect alone, and the
/// bloom's composite keeps off the cell and its ring ([`LetterMask`]), so only the ring a
/// letter's ground is read in (two pixels past its ink, which may lie at the cell's edge)
/// needs keeping.
pub const CROWN_SHIELD_PX: u32 = 2;

/// The share of a cell's width and height over which the bloom eases back to whole past a kept
/// cell's ring ([`LetterMask`]): the letter guard's feather (`InkLedger::FEATHER_X`/`_Y`).
pub const MASK_FEATHER: [f32; 2] = [0.45, 0.35];

/// Words of a [`LetterMask`]'s bits, one bit a cell: 32 768 cells, a 2x Retina 4K grid's
/// 240 x 63 and more. A frame whose bloom could reach more cells than that keeps its source
/// off them by the blur's reach instead ([`letter_shield_px`]).
pub const MASK_WORDS: usize = 1024;

/// The cells a frame's bloom keeps off at its composite, and how: none of its light within a
/// kept cell's box grown by its ring ([`CROWN_SHIELD_PX`], and a smooth scroll's slack each
/// way vertically), easing back to whole over [`MASK_FEATHER`] of a cell past it (a
/// smoothstep of the normalised distance, as the letter guard eases a held light). A cell is
/// kept where it holds a glyph, or the bar or underline caret, which is read against the light
/// of its own cell (`trail-contrast`'s thin-caret reader).
///
/// The bloom once kept off the letters by cutting its source a whole blur's reach from them
/// ([`letter_shield_px`]): the cut grew with the radius faster than the blur won it back, so a
/// larger Bloom size lit less, and in the Settings card's seven lines of text it left nothing
/// to blur (composed r1, B1). Kept off at the composite, the bloom's lit area grows with its
/// radius and the letters read as the guard weighed them at every radius.
///
/// The GPU's `fs_bloom` (`bloom.metal`, `bloom.glsl`, `bloom.hlsl`) reads the same fields and
/// bits ([`Self::weight`] is its twin).
#[derive(Clone, Debug, PartialEq)]
pub struct LetterMask {
    /// The grid's top-left corner, frame px.
    pub origin: [f32; 2],
    /// A cell, px.
    pub cell: [f32; 2],
    /// How far past a kept cell's box the bloom keeps off entirely, px.
    pub grow: [f32; 2],
    /// How far past that the bloom eases back to whole, px.
    pub feather: [f32; 2],
    /// The cells the bits cover: first column, first row, columns, rows.
    pub window: [u32; 4],
    /// One bit a cell of the window, row-major (bit `i & 31` of word `i >> 5`): set where the
    /// bloom keeps off.
    pub bits: Vec<u32>,
}

impl LetterMask {
    /// Whether cell `(c, r)` is kept off: in the window and its bit set.
    #[inline]
    #[must_use]
    pub fn kept(&self, c: i32, r: i32) -> bool {
        let [c0, r0, nc, nr] = self.window;
        let (lc, lr) = (i64::from(c) - i64::from(c0), i64::from(r) - i64::from(r0));
        if lc < 0 || lr < 0 || lc >= i64::from(nc) || lr >= i64::from(nr) {
            return false;
        }
        let i = (lr as usize) * nc as usize + lc as usize;
        self.bits
            .get(i >> 5)
            .is_some_and(|w| (w >> (i & 31)) & 1 != 0)
    }

    /// The share of the bloom laid at frame point `(x, y)` (a pixel's centre): 0 within a kept
    /// cell's box grown by [`Self::grow`], rising to 1 over [`Self::feather`] past it, the
    /// least over the kept cells about the point. `fs_bloom`'s `letter_mask` is its twin.
    #[must_use]
    pub fn weight(&self, x: f32, y: f32) -> f32 {
        let c = ((x - self.origin[0]) / self.cell[0]).floor() as i32;
        let r = ((y - self.origin[1]) / self.cell[1]).floor() as i32;
        let mut m = 1.0f32;
        for dr in -2..=2 {
            for dc in -1..=1 {
                let (cc, rr) = (c + dc, r + dr);
                if !self.kept(cc, rr) {
                    continue;
                }
                let bx0 = self.origin[0] + cc as f32 * self.cell[0] - self.grow[0];
                let by0 = self.origin[1] + rr as f32 * self.cell[1] - self.grow[1];
                let bx1 = bx0 + self.cell[0] + 2.0 * self.grow[0];
                let by1 = by0 + self.cell[1] + 2.0 * self.grow[1];
                let dx = (bx0 - x).max(x - bx1).max(0.0) / self.feather[0];
                let dy = (by0 - y).max(y - by1).max(0.0) / self.feather[1];
                let e2 = dx * dx + dy * dy;
                if e2 >= 1.0 {
                    continue;
                }
                let e = e2.sqrt();
                m = m.min(e * e * (3.0 - 2.0 * e));
            }
        }
        m
    }

    /// The mask of `input`'s kept cells ([`kept_off`]) about the frame box `area` (`x0, y0,
    /// x1, y1`, px: where the bloom may lay light), its grid at `grid`; `None` where the cells
    /// that could reach the box outrun [`MASK_WORDS`].
    #[must_use]
    pub fn about(
        input: &aterm_core::render::RenderInput,
        grid: LetterGrid,
        area: [i64; 4],
    ) -> Option<Self> {
        if grid.cell_w == 0 || grid.cell_h == 0 {
            return None;
        }
        let (cw, ch) = (grid.cell_w as f32, grid.cell_h as f32);
        let slack = input.scroll_frac_px.unsigned_abs() as f32;
        let ring = CROWN_SHIELD_PX as f32;
        let grow = [ring, ring + slack];
        let feather = [
            (cw * MASK_FEATHER[0]).max(1.0),
            (ch * MASK_FEATHER[1]).max(1.0),
        ];
        let reach = |g: f32, f: f32| (g + f).ceil() as i64;
        let (rx, ry) = (reach(grow[0], feather[0]), reach(grow[1], feather[1]));
        let (pad, top) = (grid.pad as i64, grid.grid_top as i64);
        let (cwi, chi) = (grid.cell_w as i64, grid.cell_h as i64);
        // A row's cells may stop at its last ink; the caret's cell may lie past it.
        let cols = input.cols as i64;
        let rows = input.rows as i64;
        let c0 = (area[0] - rx - pad).div_euclid(cwi).clamp(0, cols);
        let c1 = (area[2] + rx - pad)
            .div_euclid(cwi)
            .saturating_add(1)
            .clamp(0, cols);
        let r0 = (area[1] - ry - top).div_euclid(chi).clamp(0, rows);
        let r1 = (area[3] + ry - top)
            .div_euclid(chi)
            .saturating_add(1)
            .clamp(0, rows);
        let (nc, nr) = ((c1 - c0).max(0) as usize, (r1 - r0).max(0) as usize);
        if nc.saturating_mul(nr) > MASK_WORDS * 32 {
            return None;
        }
        let mut bits = vec![0u32; (nc * nr).div_ceil(32)];
        let caret = thin_caret_cell(input);
        for lr in 0..nr {
            let r = r0 as usize + lr;
            let Some(line) = input.cells.get(r) else {
                continue;
            };
            for lc in 0..nc {
                let c = c0 as usize + lc;
                if kept_off(line.get(c), caret == Some((r, c))) {
                    let i = lr * nc + lc;
                    bits[i >> 5] |= 1 << (i & 31);
                }
            }
        }
        Some(Self {
            origin: [grid.pad as f32, grid.grid_top as f32],
            cell: [cw, ch],
            grow,
            feather,
            window: [c0 as u32, r0 as u32, nc as u32, nr as u32],
            bits,
        })
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static CUT_BY_REACH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A test's negative control: `f` run with [`spared_sources`] keeping the bloom's source a
/// whole blur's reach off the kept cells ([`letter_shield_px`]) and no [`LetterMask`], as
/// looks10 did. Debug and test builds only.
#[cfg(any(test, debug_assertions))]
pub fn with_source_cut_by_reach<R>(f: impl FnOnce() -> R) -> R {
    CUT_BY_REACH.with(|c| c.set(true));
    let out = f();
    CUT_BY_REACH.with(|c| c.set(false));
    out
}

/// The post passes' sources of a frame whose bloom spares its letters
/// ([`aterm_core::render::BloomGate::SparesLetters`]): `source` (the lights both passes read,
/// window px) kept [`CROWN_SHIELD_PX`] off every kept cell ([`kept_off`]) into `crown` and
/// `bloom`, and the mask the bloom's composite lays its light by ([`apply_kept_off`]) for a
/// blur of `radius`. Where the cells the bloom could reach outrun the mask's bits, `bloom` is
/// kept a whole blur's reach off them instead ([`letter_shield_px`]) and there is no mask.
/// Both renderers draw from these, so they bloom and crown alike.
pub fn spared_sources(
    source: &[GlowQuad],
    input: &aterm_core::render::RenderInput,
    grid: LetterGrid,
    radius: f32,
    crown: &mut Vec<GlowQuad>,
    bloom: &mut Vec<GlowQuad>,
) -> Option<LetterMask> {
    crown.clear();
    crown.extend_from_slice(source);
    spare_letters(crown, input, grid, CROWN_SHIELD_PX);
    bloom.clear();
    #[cfg(any(test, debug_assertions))]
    let by_reach = CUT_BY_REACH.with(std::cell::Cell::get);
    #[cfg(not(any(test, debug_assertions)))]
    let by_reach = false;
    let mask = if by_reach {
        None
    } else {
        let mut b = Bounds::EMPTY;
        for q in crown.iter().filter(|q| usize::from(q.row) < input.rows) {
            b.add(q);
        }
        b.rect().and_then(|[x0, y0, x1, y1]| {
            let d = i64::from(reach_px(radius)) + i64::from(BLOOM_DOWNSCALE);
            LetterMask::about(
                input,
                grid,
                [
                    i64::from(x0) - d,
                    i64::from(y0) - d,
                    i64::from(x1) + d,
                    i64::from(y1) + d,
                ],
            )
        })
    };
    bloom.extend_from_slice(crown);
    if mask.is_none() && (by_reach || !bloom.is_empty()) {
        bloom.clear();
        bloom.extend_from_slice(source);
        spare_letters(bloom, input, grid, letter_shield_px(radius));
    }
    mask
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

/// Whether the post passes keep off a cell: one holding ink ([`lettered`]), or the thin
/// caret's (`caret`, [`thin_caret_cell`]).
#[inline]
fn kept_off(c: Option<&aterm_core::terminal::RenderCell>, caret: bool) -> bool {
    caret || c.is_some_and(lettered)
}

/// The cell of `input`'s bar or underline caret, where it is shown: two pixels or so of the
/// caret's colour, read against the light of its own cell (the effects' thin-caret law weighs
/// it on the frame's streams, so the post passes keep off the cell as off a letter's). A block
/// paints its whole cell and keeps the bloom over it.
#[must_use]
pub fn thin_caret_cell(input: &aterm_core::render::RenderInput) -> Option<(usize, usize)> {
    use aterm_core::terminal::CursorStyle as S;
    let thin = matches!(
        crate::effective_cursor_style(input, None),
        S::BlinkingBar | S::SteadyBar | S::BlinkingUnderline | S::SteadyUnderline
    );
    (thin && input.cursor_visible && input.cursor_row < input.rows && input.cursor_col < input.cols)
        .then_some((input.cursor_row, input.cursor_col))
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
    let caret = thin_caret_cell(input);
    for (r, line) in input.cells.iter().enumerate().take(input.rows) {
        let y0 = top + r as i64 * ch - slack - grow;
        let y1 = top + (r as i64 + 1) * ch + slack + grow;
        if y1 <= by0 || y0 >= by1 {
            continue;
        }
        let mut run: Option<(i64, i64)> = None;
        // A row's cells may stop at its last ink; the caret's cell may lie past it.
        let end = match caret {
            Some((cr, cc)) if cr == r => line.len().max(cc + 1),
            _ => line.len(),
        };
        for c in 0..end {
            if !kept_off(line.get(c), caret == Some((r, c))) {
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
#[path = "bloom_tests.rs"]
mod tests;
