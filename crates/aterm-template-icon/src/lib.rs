// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A menu-bar template icon: a shape given as "is this point ink", supersampled to one
//! coverage byte per pixel ([`coverage`]) and encoded as a black RGBA PNG whose alpha is that
//! coverage ([`png`]), which AppKit tints as a template image. [`sdf`] holds the
//! signed-distance shapes the icons are composed from.

/// Samples per pixel along each axis.
const SUBSAMPLES: u32 = 8;

/// A point of the icon's canvas, in points from its top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    /// Points right of the left edge.
    pub x: f64,
    /// Points below the top edge.
    pub y: f64,
}

impl Pt {
    /// The point at (`x`, `y`).
    #[inline]
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// The canvas of `w_pt` × `h_pt` points drawn at `w_px` × `h_px` pixels: one coverage byte per
/// pixel in row order (0 clear, 255 ink), each the share of its 8 × 8 samples `ink` holds.
/// Empty for a canvas with no pixels, or one too large to hold.
#[must_use]
pub fn coverage(
    (w_px, h_px): (u32, u32),
    (w_pt, h_pt): (f64, f64),
    ink: impl Fn(Pt) -> bool,
) -> Vec<u8> {
    let Some((w, total)) = usize::try_from(w_px)
        .ok()
        .zip(usize::try_from(h_px).ok())
        .and_then(|(w, h)| w.checked_mul(h).map(|total| (w, total)))
    else {
        return Vec::new();
    };
    if total == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    if out.try_reserve_exact(total).is_err() {
        return out;
    }
    out.resize(total, 0);
    let (step_x, step_y) = (w_pt / f64::from(w_px), h_pt / f64::from(h_px));
    let sub = f64::from(SUBSAMPLES);
    let samples = SUBSAMPLES * SUBSAMPLES;
    for (row, line) in out.chunks_exact_mut(w).enumerate() {
        for (col, cell) in line.iter_mut().enumerate() {
            let mut hits = 0u32;
            for sy in 0..SUBSAMPLES {
                for sx in 0..SUBSAMPLES {
                    let p = Pt {
                        x: (col as f64 + (f64::from(sx) + 0.5) / sub) * step_x,
                        y: (row as f64 + (f64::from(sy) + 0.5) / sub) * step_y,
                    };
                    hits += u32::from(ink(p));
                }
            }
            *cell = ((hits * 255 + samples / 2) / samples) as u8;
        }
    }
    out
}

/// `alpha` (one coverage byte per pixel of a `w` × `h` image, [`coverage`]) as an 8-bit RGBA
/// template PNG: black, with the coverage as alpha. Empty where `alpha` is not `w` × `h` bytes,
/// the image has no pixels, or it could not be encoded.
#[must_use]
pub fn png(alpha: &[u8], w: u32, h: u32) -> Vec<u8> {
    let pixels = usize::try_from(w)
        .ok()
        .zip(usize::try_from(h).ok())
        .and_then(|(w, h)| w.checked_mul(h));
    if alpha.is_empty() || pixels != Some(alpha.len()) {
        return Vec::new();
    }
    let Some(cap) = alpha.len().checked_mul(4) else {
        return Vec::new();
    };
    let mut rgba = Vec::new();
    if rgba.try_reserve_exact(cap).is_err() {
        return Vec::new();
    }
    for &a in alpha {
        rgba.extend_from_slice(&[0, 0, 0, a]);
    }
    let mut out = Vec::new();
    {
        let mut enc = aterm_png::Encoder::new(&mut out, w, h);
        enc.set_color(aterm_png::ColorType::Rgba);
        enc.set_depth(aterm_png::BitDepth::Eight);
        let Ok(mut writer) = enc.write_header() else {
            return Vec::new();
        };
        if writer.write_image_data(&rgba).is_err() || writer.finish().is_err() {
            return Vec::new();
        }
    }
    out
}

/// Signed distances: negative inside a shape, positive outside, so a union is `min`, an
/// intersection `max`, and a cut `a.max(-b)`.
pub mod sdf {
    use super::Pt;

    /// The rectangle `[x0, y0, x1, y1]` with corner radii `[top-left, top-right, bottom-right,
    /// bottom-left]`.
    #[inline]
    #[must_use]
    pub fn round_rect(p: Pt, [x0, y0, x1, y1]: [f64; 4], radii: [f64; 4]) -> f64 {
        let (cx, cy) = (f64::midpoint(x0, x1), f64::midpoint(y0, y1));
        let r = match (p.x >= cx, p.y >= cy) {
            (false, false) => radii[0],
            (true, false) => radii[1],
            (true, true) => radii[2],
            (false, true) => radii[3],
        };
        let qx = (p.x - cx).abs() - (x1 - x0) / 2.0 + r;
        let qy = (p.y - cy).abs() - (y1 - y0) / 2.0 + r;
        qx.max(qy).min(0.0) + qx.max(0.0).hypot(qy.max(0.0)) - r
    }

    /// The ring centred at (`cx`, `cy`) between the radii `ri` and `ro`.
    #[inline]
    #[must_use]
    pub fn ring(p: Pt, [cx, cy, ro, ri]: [f64; 4]) -> f64 {
        let r = (p.x - cx).hypot(p.y - cy);
        (r - ro).max(ri - r)
    }

    /// The distance to the segment from `a` to `b`: never negative, so a stroke of width `w` is
    /// `segment(..) - w / 2`.
    #[inline]
    #[must_use]
    pub fn segment(p: Pt, a: Pt, b: Pt) -> f64 {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (p.x - a.x - t * dx).hypot(p.y - a.y - t * dy)
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
