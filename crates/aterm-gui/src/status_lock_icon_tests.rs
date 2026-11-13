// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A pixel is ink where it is at least half covered.
fn inked(cov: &[u8]) -> Vec<bool> {
    cov.iter().map(|&a| a >= 128).collect()
}

fn differing(a: &[u8], b: &[u8]) -> usize {
    inked(a)
        .iter()
        .zip(inked(b))
        .filter(|(a, b)| **a != *b)
        .count()
}

fn at(cov: &[u8], scale: u32, p: Pt) -> u8 {
    let (w, _) = pixels(scale);
    let k = f64::from(scale);
    let (col, row) = ((p.x * k).floor() as usize, (p.y * k).floor() as usize);
    cov[row * w as usize + col]
}

#[test]
fn each_level_is_a_byte_per_pixel_of_ink_and_the_same_every_time() {
    for scale in SCALES {
        let (w, h) = pixels(scale);
        assert_eq!((w, h), (14 * scale, 18 * scale));
        for level in FleetAccess::ALL {
            let cov = coverage(level, scale);
            assert_eq!(cov.len(), (w * h) as usize, "{level:?} x{scale}");
            assert_eq!(cov, coverage(level, scale), "{level:?} x{scale}");
            let ink = inked(&cov).iter().filter(|on| **on).count();
            // A lock, not a speck and not a slab: between a tenth and two thirds of the
            // canvas.
            assert!(
                ink * 10 >= cov.len() && ink * 3 <= cov.len() * 2,
                "{level:?} x{scale}: {ink} of {} pixels inked",
                cov.len()
            );
        }
    }
}

/// Any two levels differ in enough whole pixels to be told apart on the bar, at one pixel a
/// point too. The floors stand a margin under what this drawing measures (1×: read/write
/// 36, write/full 21, read/full 57 of 252 pixels; 2×: 176, 87 and 263 of 1008), so a change
/// that erodes a cue fails here.
#[test]
fn any_two_levels_differ_in_enough_pixels_to_tell_apart() {
    use FleetAccess::{Full, ReadOnly, ReadWrite};
    for (scale, floors) in [(1, [30, 18, 48]), (2, [150, 75, 225])] {
        let [read, write, full] = [ReadOnly, ReadWrite, Full].map(|l| coverage(l, scale));
        let got = [
            differing(&read, &write),
            differing(&write, &full),
            differing(&read, &full),
        ];
        for (pair, (got, floor)) in ["read/write", "write/full", "read/full"]
            .iter()
            .zip(got.iter().zip(floors))
        {
            assert!(
                *got >= floor,
                "x{scale} {pair}: {got} pixels differ, want {floor}"
            );
        }
    }
}

#[test]
fn a_point_of_clear_margin_on_every_side() {
    for scale in SCALES {
        let (w, h) = pixels(scale);
        let (w, h, margin) = (w as usize, h as usize, scale as usize);
        for level in FleetAccess::ALL {
            let cov = coverage(level, scale);
            for row in 0..h {
                for col in 0..w {
                    let edge =
                        row < margin || row >= h - margin || col < margin || col >= w - margin;
                    if edge {
                        assert_eq!(
                            cov[row * w + col],
                            0,
                            "{level:?} x{scale} col={col} row={row}"
                        );
                    }
                }
            }
        }
    }
}

/// The two cues, read off the pixels: the closed shackle's right leg reaches the body, the
/// ajar one's ends above it and the open one's is a stub; the body is solid, solid below its
/// middle, or empty.
#[test]
fn the_shackle_and_the_fill_say_the_level() {
    use FleetAccess::{Full, ReadOnly, ReadWrite};
    let right_leg_above_the_body = Pt::new(LEGS[1], 8.5);
    let right_leg_under_the_arch = Pt::new(LEGS[1], 7.25);
    let left_leg_above_the_body = Pt::new(LEGS[0], 8.5);
    let upper_body = Pt::new(7.0, 12.25);
    let lower_body = Pt::new(7.0, 14.25);
    let wall = Pt::new(1.5, 13.0);
    for scale in SCALES {
        let [read, write, full] = [ReadOnly, ReadWrite, Full].map(|l| coverage(l, scale));
        // Shackle.
        assert_eq!(at(&read, scale, right_leg_above_the_body), 255, "x{scale}");
        assert_eq!(at(&write, scale, right_leg_above_the_body), 0, "x{scale}");
        assert_eq!(at(&full, scale, right_leg_above_the_body), 0, "x{scale}");
        assert_eq!(at(&write, scale, right_leg_under_the_arch), 255, "x{scale}");
        assert_eq!(at(&full, scale, right_leg_under_the_arch), 0, "x{scale}");
        for cov in [&read, &write, &full] {
            assert_eq!(at(cov, scale, left_leg_above_the_body), 255, "x{scale}");
            assert_eq!(at(cov, scale, wall), 255, "x{scale}");
        }
        // Fill.
        assert_eq!(at(&read, scale, upper_body), 255, "x{scale}");
        assert_eq!(at(&read, scale, lower_body), 255, "x{scale}");
        assert_eq!(at(&write, scale, upper_body), 0, "x{scale}");
        assert_eq!(at(&write, scale, lower_body), 255, "x{scale}");
        assert_eq!(at(&full, scale, upper_body), 0, "x{scale}");
        assert_eq!(at(&full, scale, lower_body), 0, "x{scale}");
    }
}

#[test]
fn the_pngs_decode_to_the_coverage_and_each_level_is_described() {
    for scale in SCALES {
        let (w, h) = pixels(scale);
        for level in FleetAccess::ALL {
            let (rgba, got_w, got_h) =
                aterm_png::decode_rgba8(&png(level, scale), w.max(h)).expect("the icon decodes");
            assert_eq!((got_w, got_h), (w as usize, h as usize));
            let (px, rest) = rgba.as_chunks::<4>();
            assert!(rest.is_empty());
            let alpha: Vec<u8> = px.iter().map(|p| p[3]).collect();
            assert_eq!(alpha, coverage(level, scale), "{level:?} x{scale}");
        }
    }
    let said: Vec<&str> = FleetAccess::ALL.map(description).to_vec();
    assert_eq!(said, ["lock closed", "lock half open", "lock open"]);
}
