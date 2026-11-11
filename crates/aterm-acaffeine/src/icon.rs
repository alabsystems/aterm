// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Menu-bar coffee-mug icon rasterized as coverage and encoded as a template PNG.

/// The icon's width and height in points.
pub const SIZE_POINTS: f64 = 18.0;

const SUBSAMPLES: u32 = 8;

/// The icon at `px` × `px` pixels: one coverage byte per pixel (0 clear, 255 ink).
#[must_use]
pub fn coverage(lit: bool, px: u32) -> Vec<u8> {
    let Some((n, total)) = usize::try_from(px)
        .ok()
        .and_then(|n| n.checked_mul(n).map(|total| (n, total)))
    else {
        return Vec::new();
    };
    if n == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    if out.try_reserve_exact(total).is_err() {
        return out;
    }
    out.resize(total, 0);
    let step = SIZE_POINTS / f64::from(px);
    let sub = f64::from(SUBSAMPLES);
    let samples = SUBSAMPLES * SUBSAMPLES;
    for (row, line) in out.chunks_exact_mut(n).enumerate() {
        for (col, cell) in line.iter_mut().enumerate() {
            let mut hits = 0u32;
            for sy in 0..SUBSAMPLES {
                for sx in 0..SUBSAMPLES {
                    let p = Pt {
                        x: (col as f64 + (f64::from(sx) + 0.5) / sub) * step,
                        y: (row as f64 + (f64::from(sy) + 0.5) / sub) * step,
                    };
                    hits += u32::from(ink(lit, p));
                }
            }
            *cell = ((hits * 255 + samples / 2) / samples) as u8;
        }
    }
    out
}

/// [`coverage`] as an 8-bit RGBA template PNG (black with coverage as alpha).
#[must_use]
pub fn png(lit: bool, px: u32) -> Vec<u8> {
    if px == 0 {
        return Vec::new();
    }
    let alpha = coverage(lit, px);
    if alpha.is_empty() {
        return Vec::new();
    }
    let Some(cap) = alpha.len().checked_mul(4) else {
        return Vec::new();
    };
    let mut rgba = Vec::new();
    if rgba.try_reserve_exact(cap).is_err() {
        return Vec::new();
    }
    for a in alpha {
        rgba.extend_from_slice(&[0, 0, 0, a]);
    }
    let mut out = Vec::new();
    {
        let mut enc = aterm_png::Encoder::new(&mut out, px, px);
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

const BODY: [f64; 4] = [2.0, 6.0, 13.0, 17.0];
const BODY_RADII: [f64; 2] = [1.0, 3.0];
const WALL: f64 = 1.5;
const HANDLE: [f64; 4] = [13.0, 11.0, 3.0, 1.5];
const LETTER: [f64; 5] = [5.0, 9.0, 5.0, 5.0, 1.0];
const LETTER_TURN: f64 = 1.0;
const STEAM_X: [f64; 3] = [4.5, 7.5, 10.5];
const STEAM_LINE: [f64; 3] = [1.5, 4.5, 0.75];
const STEAM_STROKE: f64 = 0.9;

#[derive(Clone, Copy)]
struct Pt {
    x: f64,
    y: f64,
}

impl Pt {
    #[inline]
    const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[inline]
fn ink(lit: bool, p: Pt) -> bool {
    let body = body(p);
    let handle = ring(p, HANDLE).max(-body);
    let a = letter_a(p);
    let d = if lit {
        body.max(-a).min(handle).min(steam(p))
    } else {
        let wall = body.max(-(body + WALL));
        wall.min(handle).min(a)
    };
    d <= 0.0
}

#[inline]
fn body(p: Pt) -> f64 {
    let [rim, base] = BODY_RADII;
    round_rect(p, BODY, [rim, rim, base, base])
}

#[inline]
fn letter_a(p: Pt) -> f64 {
    let [left, top, width, height, stroke] = LETTER;
    let half = stroke / 2.0;
    let r = LETTER_TURN;
    let bar_y = top + half;
    let stem_x = left + width - half;
    let bowl_top = top + 2.0 * stroke + half;
    let bowl_foot = top + height - half;
    let bar = segment(
        p,
        Pt::new(left + stroke + half, bar_y),
        Pt::new(stem_x - r, bar_y),
    );
    let turn = quarter_arc(p, Pt::new(stem_x - r, bar_y + r), r);
    let arch = bar.min(turn) - half;
    let stem = round_rect(
        p,
        [stem_x - half, bar_y + r, stem_x + half, top + height],
        [0.0; 4],
    );
    let bowl = round_rect(
        p,
        [left + half, bowl_top, stem_x, bowl_foot],
        [r, 0.0, 0.0, r],
    )
    .abs()
        - half;
    arch.min(stem).min(bowl)
}

fn steam(p: Pt) -> f64 {
    let [top, bottom, swing] = STEAM_LINE;
    let half = STEAM_STROKE / 2.0;
    STEAM_X
        .iter()
        .map(|&cx| {
            let bx = (p.x - cx).abs() - swing;
            let by = (p.y - f64::midpoint(top, bottom)).abs() - (bottom - top) / 2.0;
            let to_box = bx.max(0.0).hypot(by.max(0.0));
            let d = if to_box > half { to_box } else { wisp(p, cx) };
            d - half
        })
        .fold(f64::INFINITY, f64::min)
}

fn wisp(p: Pt, cx: f64) -> f64 {
    const STEPS: u32 = 24;
    let [top, bottom, swing] = STEAM_LINE;
    let at = |i: u32| {
        let t = f64::from(i) / f64::from(STEPS);
        Pt::new(
            cx - swing * (t * std::f64::consts::TAU).sin(),
            top + t * (bottom - top),
        )
    };
    (0..STEPS)
        .map(|i| segment(p, at(i), at(i + 1)))
        .fold(f64::INFINITY, f64::min)
}

#[inline]
fn quarter_arc(p: Pt, c: Pt, r: f64) -> f64 {
    let (dx, dy) = (p.x - c.x, p.y - c.y);
    if dx >= 0.0 && dy <= 0.0 {
        (dx.hypot(dy) - r).abs()
    } else {
        let right = Pt::new(c.x + r, c.y);
        let up = Pt::new(c.x, c.y - r);
        segment(p, right, right).min(segment(p, up, up))
    }
}

#[inline]
fn round_rect(p: Pt, [x0, y0, x1, y1]: [f64; 4], radii: [f64; 4]) -> f64 {
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

#[inline]
fn ring(p: Pt, [cx, cy, ro, ri]: [f64; 4]) -> f64 {
    let r = (p.x - cx).hypot(p.y - cy);
    (r - ro).max(ri - r)
}

#[inline]
fn segment(p: Pt, a: Pt, b: Pt) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p.x - a.x - t * dx).hypot(p.y - a.y - t * dy)
}

#[cfg(test)]
#[path = "icon_tests.rs"]
mod tests;
