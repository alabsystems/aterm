// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cursor aurora's bloom and SDR crown, one law for both renderers.
//!
//! The GPU draws the bloom as a post pass (`aterm-gpu`'s `build_bloom_glow`,
//! `fs_bloom` and `fs_sdr_glow`): the `cursor_glow_add` stream is drawn into a
//! half-resolution target ([`BLOOM_DOWNSCALE`]), read back through a 25-tap
//! gaussian at two radii (the white part of each sample at the radius, its
//! chroma at [`chroma_radius`]), scaled by the strength and SCREEN-composited
//! over the frame; on an SDR screen the glow quads are then SCREEN-composited
//! once more, scaled by the SDR budget. This module owns the constants and the
//! extract set the GPU uploads ([`source_quads_into`]), and evaluates the same
//! passes on a CPU [`Frame`] ([`apply`], [`sdr_crown`]), so a frame the CPU
//! renderer draws (the Settings preview) blooms as the live terminal does.
//!
//! The CPU twin evaluates each pass as the GPU's fixed function does: the
//! extract covers a half-res texel when its centre lies in a quad, at the
//! quad's own column there ([`crate::glow_lerp`]), blending as
//! [`crate::over_premul`] does on the Unorm8 target; the blur samples that
//! target bilinearly with clamped edges at every full-res pixel's centre;
//! SCREEN is `src + dst·(1 − src)` on 8-bit channels, rounded once.

use crate::{Frame, GlowQuad};

/// Half-resolution divisor for the bloom target: the halo is soft, so a
/// quarter of the pixels is plenty and keeps the blur cheap and naturally
/// wider.
pub const BLOOM_DOWNSCALE: u32 = 2;
/// The default bloom strength: the share of the blurred glow added back over
/// the frame.
pub const BLOOM_STRENGTH: f32 = 0.85;
/// The default blur radius in half-res texels: how far the halo spreads. It
/// is the WHITE part's radius ([`BLOOM_CHROMA_RADIUS`] is the colour's).
pub const BLOOM_RADIUS: f32 = 2.2;
/// The chroma radius of the same 25-tap kernel (half-res texels): colour
/// spreads less than white, so less coloured light smears into neighbouring
/// glyphs. Always the shorter radius, so a scissor dilated by the white
/// radius bounds it.
pub const BLOOM_CHROMA_RADIUS: f32 = 1.2;
/// The chroma radius as a share of the white one, so a configured radius
/// moves both halves together.
pub const BLOOM_CHROMA_SHARE: f32 = BLOOM_CHROMA_RADIUS / BLOOM_RADIUS;

/// The chroma radius paired with a white blur `radius`.
#[must_use]
pub fn chroma_radius(radius: f32) -> f32 {
    radius * BLOOM_CHROMA_SHARE
}

/// How far the bloom's light reaches past the glow's bounding box, in
/// full-res pixels, saturating at `u32::MAX`: `(2·radius + 1)` half-res texels
/// (the taps plus one texel of linear filtering) and one pixel of rounding.
/// Outside it the added light is exactly zero, which lets the GPU scissor its
/// composite there.
#[must_use]
pub fn reach_px(radius: f32) -> u32 {
    (((2.0 * radius + 1.0) * BLOOM_DOWNSCALE as f32).ceil() as u32).saturating_add(1)
}

/// The bloom's extract set for a frame of `rows` grid rows: every
/// `cursor_glow_add` quad on the grid (a quad past the last row is skipped,
/// as the glow pass skips it), into `out`, cleared first.
///
/// Half-res stability: a quad thinner than one half-res texel covers a texel
/// centre only at some alignments, so a hairline would blink in the halo.
/// When any quad is that thin, a second copy of the set is appended with the
/// thin axes widened to one texel and both colours of its ramp scaled by the
/// area ratio, which conserves its energy. The extract draws
/// `out[extract_first..]`; the SDR crown draws `out[..extract_first]` when
/// there is a copy, else the whole set (`extract_first == 0`).
///
/// Returns the window-space bounding box of the set (`[x0, y0, x1, y1]`, x1
/// and y1 exclusive, thin axes at their widened extent) and `extract_first`.
pub fn source_quads_into(
    glow: &[GlowQuad],
    rows: usize,
    out: &mut Vec<GlowQuad>,
) -> (Option<[u32; 4]>, usize) {
    let ds = BLOOM_DOWNSCALE as u16;
    out.clear();
    let (mut bx0, mut by0, mut bx1, mut by1) = (u16::MAX, u16::MAX, 0u16, 0u16);
    let mut any_thin = false;
    for q in glow {
        if usize::from(q.row) >= rows {
            continue;
        }
        any_thin |= (q.w > 0 && q.w < ds) || (q.h > 0 && q.h < ds);
        let ew = if q.w > 0 { q.w.max(ds) } else { 0 };
        let eh = if q.h > 0 { q.h.max(ds) } else { 0 };
        bx0 = bx0.min(q.x);
        by0 = by0.min(q.y);
        bx1 = bx1.max(q.x.saturating_add(ew));
        by1 = by1.max(q.y.saturating_add(eh));
        out.push(*q);
    }
    let n = out.len();
    let extract_first = if any_thin {
        for i in 0..n {
            let mut q = out[i];
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
                // Both ends of the ramp scale by the same area ratio, so the
                // widened quad's gradient is the thin quad's, dimmed.
                let scale = |c: u32| {
                    let ch = |sh: u32| (((c >> sh) & 0xff) * num + den / 2) / den;
                    (ch(16) << 16) | (ch(8) << 8) | ch(0)
                };
                q.color = scale(q.color);
                q.color2 = scale(q.color2);
            }
            q.w = w1;
            q.h = h1;
            out.push(q);
        }
        n
    } else {
        0
    };
    let bbox = (n > 0).then_some([
        u32::from(bx0),
        u32::from(by0),
        u32::from(bx1),
        u32::from(by1),
    ]);
    (bbox, extract_first)
}

/// The bloom's tunables for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BloomParams {
    /// The share of the blurred glow added back ([`BLOOM_STRENGTH`] default).
    pub strength: f32,
    /// The white part's blur radius in half-res texels ([`BLOOM_RADIUS`]
    /// default); the chroma's is [`chroma_radius`] of it.
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
fn channels(p: u32) -> [f32; 3] {
    [
        ((p >> 16) & 0xff) as f32 / 255.0,
        ((p >> 8) & 0xff) as f32 / 255.0,
        (p & 0xff) as f32 / 255.0,
    ]
}

/// SCREEN `src` (`0..=1` per channel, clamped first as a Unorm target clamps a
/// fragment) over the pixel `dst`, rounded once on store; the transmittance
/// byte rides through (the GPU writes colour only).
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

/// Bloom `frame` (a grid of `rows` rows) with its `glow` stream, as the GPU's
/// post pass does: extract at half res, blur at two radii, SCREEN the result
/// over the frame inside the glow's box dilated by [`reach_px`]. A frame with
/// no glow on its grid is untouched.
///
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
    // 1) The extract: the set over a cleared half-res Unorm8 target.
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
        // Texel t is covered when its centre, full-res 2t + 1 (odd, so never
        // on an integer edge), lies in [q, q + extent).
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
    // A linear, clamp-to-edge sample of the half-res target at `uv`.
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
    // At one complete target extent every nonzero tap already clamps to an
    // edge. Capping each radius there preserves those samples and keeps finite
    // but enormous requests out of overflowing texture-coordinate arithmetic.
    let extent = bw.max(bh) as f32;
    let (rw, rc) = (
        params.radius.min(extent),
        chroma_radius(params.radius).min(extent),
    );
    let (tu, tv) = (1.0 / bw as f32, 1.0 / bh as f32);
    // 2) Blur and composite, inside the dilated box.
    let d = reach_px(params.radius) as usize;
    let sx0 = (x0 as usize).saturating_sub(d);
    let sy0 = (y0 as usize).saturating_sub(d);
    let sx1 = (x1 as usize).saturating_add(d).min(w);
    let sy1 = (y1 as usize).saturating_add(d).min(h);
    for py in sy0..sy1 {
        let v = (py as f32 + 0.5) / h as f32;
        for px in sx0..sx1 {
            let u = (px as f32 + 0.5) / w as f32;
            let mut white = 0.0f32;
            let mut chroma = [0.0f32; 3];
            let mut wsum = 0.0f32;
            for j in -2i32..=2 {
                for i in -2i32..=2 {
                    let (oi, oj) = (i as f32 * tu, j as f32 * tv);
                    let weight = (-((i * i + j * j) as f32) / 4.0).exp();
                    let sw = sample(u + oi * rw, v + oj * rw);
                    white += white_part(sw) * weight;
                    let sc = sample(u + oi * rc, v + oj * rc);
                    let wc = white_part(sc);
                    for k in 0..3 {
                        chroma[k] += (sc[k] - wc) * weight;
                    }
                    wsum += weight;
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

fn check_frame(frame: &Frame) -> Result<(), String> {
    crate::checked_frame_size_px(frame.height, frame.width, 1, 1, 0, 0)?;
    if frame.width.checked_mul(frame.height) != Some(frame.pixels.len()) {
        return Err("bloom frame dimensions do not match its pixels".to_string());
    }
    Ok(())
}

/// The SDR crown over `frame` (a grid of `rows` rows): every glow quad on the
/// grid SCREEN-composited once more at its ramped colour times `budget`
/// (`hdr::sdr_glow_budget`), as the GPU's `fs_sdr_glow` draws it on an SDR
/// screen. `budget <= 0` draws nothing; this pass allocates no scratch.
///
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
        let wq = u32::from(q.w);
        for py in qy..ye {
            for px in qx..xe {
                let c = crate::glow_lerp_rgb(q.color, q.color2, (px - qx) as u32, wq);
                let src = channels(c).map(|v| v * budget);
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
        // 255·0.4 + 16·(1 − 0.4) = 111.6 -> 112.
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
}
