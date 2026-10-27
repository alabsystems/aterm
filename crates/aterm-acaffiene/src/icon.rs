// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Menu-bar coffee-mug icon rasterized as coverage and encoded as a template PNG.

/// The icon's width and height in points.
pub const SIZE_POINTS: f64 = 18.0;

const SUBSAMPLES: u32 = 8;

/// The icon at `px` × `px` pixels: one coverage byte per pixel (0 clear, 255 ink).
#[must_use]
pub fn coverage(lit: bool, px: u32) -> Vec<u8> {
    let n = px as usize;
    let mut out = vec![0u8; n * n];
    if n == 0 {
        return out;
    }
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
    let alpha = coverage(lit, px);
    let mut rgba = Vec::with_capacity(alpha.len() * 4);
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
mod tests {
    use super::*;

    const SCALES: [u32; 2] = [18, 36];

    const STEAM_REGION: [f64; 4] = [2.0, 0.0, 13.0, 5.0];
    const HANDLE_REGION: [f64; 4] = [13.0, 5.0, 18.0, 18.0];
    const ON_THE_A: Pt = Pt::new(9.5, 12.5);
    const IN_THE_BODY: Pt = Pt::new(4.25, 12.25);

    fn at(cov: &[u8], px: u32, p: Pt) -> u8 {
        let k = f64::from(px) / SIZE_POINTS;
        let (col, row) = ((p.x * k).floor() as usize, (p.y * k).floor() as usize);
        cov[row * px as usize + col]
    }

    fn region(cov: &[u8], px: u32, [x0, y0, x1, y1]: [f64; 4]) -> Vec<u8> {
        let k = f64::from(px) / SIZE_POINTS;
        let n = px as usize;
        let cols = (x0 * k) as usize..(x1 * k) as usize;
        let rows = (y0 * k) as usize..(y1 * k) as usize;
        rows.flat_map(|r| cols.clone().map(move |c| cov[r * n + c]))
            .collect()
    }

    fn ink_total(cov: &[u8]) -> u64 {
        cov.iter().map(|&a| u64::from(a)).sum()
    }

    #[test]
    fn one_byte_per_pixel_and_the_same_every_time() {
        for px in SCALES {
            for lit in [false, true] {
                let cov = coverage(lit, px);
                assert_eq!(cov.len(), (px * px) as usize, "lit={lit} px={px}");
                assert_eq!(cov, coverage(lit, px), "lit={lit} px={px}");
            }
        }
        assert!(coverage(true, 0).is_empty());
    }

    #[test]
    fn skipping_the_far_steam_changes_no_ink() {
        let exact = |p: Pt| {
            STEAM_X
                .iter()
                .map(|&cx| wisp(p, cx) - STEAM_STROKE / 2.0)
                .fold(f64::INFINITY, f64::min)
        };
        let grid = 0..=(SIZE_POINTS as u32 * 8);
        for row in grid.clone() {
            for col in grid.clone() {
                let p = Pt::new(f64::from(col) / 8.0, f64::from(row) / 8.0);
                assert_eq!(steam(p) <= 0.0, exact(p) <= 0.0, "at ({}, {})", p.x, p.y);
            }
        }
    }

    #[test]
    fn a_point_of_clear_margin_on_every_side() {
        for px in SCALES {
            let n = px as usize;
            let margin = n / 18;
            assert!(margin >= 1);
            for lit in [false, true] {
                let cov = coverage(lit, px);
                for i in 0..n {
                    for m in 0..margin {
                        for (col, row) in [(i, m), (i, n - 1 - m), (m, i), (n - 1 - m, i)] {
                            assert_eq!(
                                cov[row * n + col],
                                0,
                                "lit={lit} px={px} col={col} row={row}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_full_mug_has_more_ink_and_only_it_steams() {
        for px in SCALES {
            let (off, on) = (coverage(false, px), coverage(true, px));
            assert!(ink_total(&on) > ink_total(&off), "px={px}");
            assert!(
                region(&off, px, STEAM_REGION).iter().all(|&a| a == 0),
                "px={px}"
            );
            assert!(ink_total(&region(&on, px, STEAM_REGION)) > 0, "px={px}");
        }
    }

    #[test]
    fn the_a_is_ink_on_the_empty_mug_and_cut_out_of_the_full_one() {
        for px in SCALES {
            let (off, on) = (coverage(false, px), coverage(true, px));
            assert_eq!(at(&off, px, ON_THE_A), 255, "px={px}");
            assert_eq!(at(&on, px, ON_THE_A), 0, "px={px}");
            assert_eq!(at(&off, px, IN_THE_BODY), 0, "px={px}");
            assert_eq!(at(&on, px, IN_THE_BODY), 255, "px={px}");
        }
    }

    #[test]
    fn the_handle_is_the_same_ink_in_both_states() {
        for px in SCALES {
            let off = region(&coverage(false, px), px, HANDLE_REGION);
            let on = region(&coverage(true, px), px, HANDLE_REGION);
            assert!(ink_total(&off) > 0, "px={px}");
            assert_eq!(off, on, "px={px}");
        }
    }

    #[test]
    fn the_full_mug_is_the_empty_one_filled() {
        let [left, top, width, height, _] = LETTER;
        for px in SCALES {
            let (off, on) = (coverage(false, px), coverage(true, px));
            let n = px as usize;
            let k = f64::from(px) / SIZE_POINTS;
            for row in 0..n {
                for col in 0..n {
                    let (x, y) = (col as f64 / k, row as f64 / k);
                    let by_the_a = x + 1.0 / k > left
                        && x < left + width
                        && y + 1.0 / k > top
                        && y < top + height;
                    if by_the_a || y < STEAM_REGION[3] {
                        continue;
                    }
                    let (was, is) = (off[row * n + col], on[row * n + col]);
                    assert!(
                        was <= is && (is == 255 || was == is),
                        "px={px} col={col} row={row}: off {was} on {is}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_png_is_black_with_the_coverage_as_alpha() {
        for px in SCALES {
            for lit in [false, true] {
                let (rgba, w, h) =
                    aterm_png::decode_rgba8(&png(lit, px), px).expect("the icon decodes");
                assert_eq!((w, h), (px as usize, px as usize), "lit={lit} px={px}");
                let (pixels, rest) = rgba.as_chunks::<4>();
                assert!(rest.is_empty(), "lit={lit} px={px}");
                let alpha: Vec<u8> = pixels.iter().map(|p| p[3]).collect();
                assert_eq!(alpha, coverage(lit, px), "lit={lit} px={px}");
                assert!(
                    pixels.iter().all(|p| p[..3] == [0, 0, 0]),
                    "lit={lit} px={px}"
                );
            }
        }
    }
}
