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

/// Every image the item holds: each level's lock, plain and with the bar.
fn every_image() -> impl Iterator<Item = (FleetAccess, bool)> {
    FleetAccess::ALL
        .into_iter()
        .flat_map(|level| [(level, false), (level, true)])
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
        for (level, blocked) in every_image() {
            let cov = coverage(level, blocked, scale);
            assert_eq!(cov.len(), (w * h) as usize, "{level:?} {blocked} x{scale}");
            assert_eq!(
                cov,
                coverage(level, blocked, scale),
                "{level:?} {blocked} x{scale}"
            );
            let ink = inked(&cov).iter().filter(|on| **on).count();
            // A lock, not a speck and not a slab: between a tenth and two thirds of the
            // canvas.
            assert!(
                ink * 10 >= cov.len() && ink * 3 <= cov.len() * 2,
                "{level:?} {blocked} x{scale}: {ink} of {} pixels inked",
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
        let [read, write, full] = [ReadOnly, ReadWrite, Full].map(|l| coverage(l, false, scale));
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
        for (level, blocked) in every_image() {
            let cov = coverage(level, blocked, scale);
            for row in 0..h {
                for col in 0..w {
                    let edge =
                        row < margin || row >= h - margin || col < margin || col >= w - margin;
                    if edge {
                        assert_eq!(
                            cov[row * w + col],
                            0,
                            "{level:?} {blocked} x{scale} col={col} row={row}"
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
        let [read, write, full] = [ReadOnly, ReadWrite, Full].map(|l| coverage(l, false, scale));
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
        for (level, blocked) in every_image() {
            let (rgba, got_w, got_h) =
                aterm_png::decode_rgba8(&png(level, blocked, scale), w.max(h))
                    .expect("the icon decodes");
            assert_eq!((got_w, got_h), (w as usize, h as usize));
            let (px, rest) = rgba.as_chunks::<4>();
            assert!(rest.is_empty());
            let alpha: Vec<u8> = px.iter().map(|p| p[3]).collect();
            assert_eq!(
                alpha,
                coverage(level, blocked, scale),
                "{level:?} {blocked} x{scale}"
            );
        }
        let (rgba, got_w, got_h) =
            aterm_png::decode_rgba8(&blank_png(scale), w.max(h)).expect("the blank decodes");
        assert_eq!((got_w, got_h), (w as usize, h as usize));
        assert!(
            rgba.iter().all(|byte| *byte == 0),
            "x{scale}: nothing drawn"
        );
    }
    let said: Vec<&str> = every_image()
        .map(|(level, blocked)| description(level, blocked))
        .collect();
    let distinct: std::collections::BTreeSet<&str> = said.iter().copied().collect();
    assert_eq!(distinct.len(), said.len(), "{said:?}");
    for (level, blocked) in every_image() {
        assert_eq!(
            description(level, blocked).ends_with("a request blocked"),
            blocked,
            "{level:?}"
        );
    }
}

/// The bar, read off the pixels: ink along its centre line from end to end, and a clear band
/// on both sides of it wherever the lock would have been ink.
#[test]
fn the_blocked_mark_is_a_bar_in_a_clear_band() {
    let [from, to] = BAR;
    let along = |t: f64| Pt::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
    let len = (to.x - from.x).hypot(to.y - from.y);
    // The unit normal of the bar.
    let (nx, ny) = ((to.y - from.y) / len, -(to.x - from.x) / len);
    let beside = STROKE / 2.0 + BAR_CLEAR / 2.0;
    for scale in SCALES {
        for level in FleetAccess::ALL {
            let cov = coverage(level, true, scale);
            for step in 0..=9 {
                let on = along(f64::from(step) / 9.0);
                assert_eq!(at(&cov, scale, on), 255, "{level:?} x{scale} {on:?}");
                // At one pixel a point a pixel is as wide as the band, and holds bar too.
                for side in [-1.0, 1.0].into_iter().filter(|_| scale > 1) {
                    let off = Pt::new(on.x + side * nx * beside, on.y + side * ny * beside);
                    assert!(
                        at(&cov, scale, off) < 128,
                        "{level:?} x{scale} {off:?}: the band beside the bar is clear"
                    );
                }
            }
        }
    }
}

/// A level's lock with the bar differs from the same lock without it in enough whole pixels to
/// see at one pixel a point, and the barred locks still differ from each other, so the mark
/// hides no level. The floors stand a margin under what this drawing measures (1×: 24, 29 and
/// 30 pixels with and without the bar, and read/write 33, write/full 16, read/full 49 between
/// the barred locks; 2×: 94, 101 and 101, and 139, 69 and 208).
#[test]
fn the_bar_is_seen_and_hides_no_level() {
    use FleetAccess::{Full, ReadOnly, ReadWrite};
    for scale in SCALES {
        let k = (scale * scale) as usize;
        for level in FleetAccess::ALL {
            let got = differing(
                &coverage(level, false, scale),
                &coverage(level, true, scale),
            );
            assert!(got >= 20 * k, "x{scale} {level:?}: {got} pixels differ");
        }
        let [read, write, full] = [ReadOnly, ReadWrite, Full].map(|l| coverage(l, true, scale));
        for (pair, got) in [
            ("read/write", differing(&read, &write)),
            ("write/full", differing(&write, &full)),
            ("read/full", differing(&read, &full)),
        ] {
            assert!(got >= 12 * k, "x{scale} {pair}: {got} pixels differ");
        }
    }
}

/// The two level cues survive the bar: the right leg above the body and the lower body read as
/// they do without it.
#[test]
fn the_bar_leaves_the_level_cues() {
    let right_leg_above_the_body = Pt::new(LEGS[1], 8.5);
    let lower_body = Pt::new(4.0, 15.25);
    for scale in SCALES {
        for level in FleetAccess::ALL {
            let (plain, barred) = (coverage(level, false, scale), coverage(level, true, scale));
            for cue in [right_leg_above_the_body, lower_body] {
                assert_eq!(
                    at(&plain, scale, cue),
                    at(&barred, scale, cue),
                    "{level:?} x{scale} {cue:?}"
                );
            }
        }
    }
}
