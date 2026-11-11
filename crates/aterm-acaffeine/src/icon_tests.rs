// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    assert!(coverage(true, u32::MAX).is_empty());
    assert!(png(true, 0).is_empty());
    assert!(png(true, u32::MAX).is_empty());
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
                let by_the_a =
                    x + 1.0 / k > left && x < left + width && y + 1.0 / k > top && y < top + height;
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
