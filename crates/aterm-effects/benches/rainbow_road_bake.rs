// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the setup screen's lockup costs to bake (`docs/DESIGN-init-on-by-default-2026-10-25.md`
//! §4.5): the rainbow road at the longest length a page draws ([`MAX_LENGTH`], the widest
//! supported page) at progress 0, a half and done, and the cat in each pose, at the regular
//! lockup's device sizes on 1×, 2× and 3× displays, on both grounds. The page bakes them on one
//! background worker, never where it paints, so this is how long a new moment or a new size
//! takes to land after the page asks for it.

use std::time::{Duration, Instant};

use aterm_effects::rainbow_road::{CAT_H, CatArt, CatPose, DESIGN_PX, MAX_LENGTH, ROAD_H, RoadArt};

/// The regular lockup's type size: a unit is `PX / DESIGN_PX` points.
const PX: f32 = 22.0;

/// Bakes of each art and size measured.
const RUNS: usize = 12;

/// The road at the longest length, its progress at 0, a half, and done, may take this long at
/// the 90th percentile on a 2× display: the page shows the moment before within a few frames.
const ROAD_P90_BUDGET_2X: Duration = Duration::from_millis(60);

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let at = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[at.min(sorted.len() - 1)]
}

/// Bake `RUNS` times on each ground, and print and return the sorted times.
fn time(label: &str, scale: u32, mut bake: impl FnMut(bool)) -> Vec<Duration> {
    let mut took = Vec::with_capacity(RUNS * 2);
    for dark in [false, true] {
        for _ in 0..RUNS {
            let start = Instant::now();
            bake(dark);
            took.push(start.elapsed());
        }
    }
    took.sort_unstable();
    println!(
        "{label:<16} {scale}x {:>9} {:>9} {:>9}",
        percentile(&took, 0.5).as_micros(),
        percentile(&took, 0.9).as_micros(),
        took[took.len() - 1].as_micros(),
    );
    took
}

fn main() {
    if cfg!(debug_assertions) {
        panic!("a bake's cost is a release-build number: run it with `targo --unverified bench`");
    }
    let mut reds = Vec::new();
    println!("art              scale  p50 µs    p90 µs    max µs");
    for scale in [1_u32, 2, 3] {
        let unit = PX / DESIGN_PX * scale as f32;
        let h = (ROAD_H * unit).round() as u32;
        for (label, permille, done) in [
            ("road at 0", 0, false),
            ("road at a half", 500, false),
            ("road done", 1000, true),
        ] {
            let art = |dark| RoadArt {
                length: MAX_LENGTH as u16,
                permille,
                done,
                dark,
            };
            let w = (h as f32 * art(true).aspect()).round() as u32;
            let took = time(label, scale, |dark| {
                std::hint::black_box(art(dark).bake(w, h));
            });
            let p90 = percentile(&took, 0.9);
            if scale == 2 && p90 > ROAD_P90_BUDGET_2X {
                reds.push(format!(
                    "{label} at 2x ({w}×{h}): p90 {} µs, over {} µs",
                    p90.as_micros(),
                    ROAD_P90_BUDGET_2X.as_micros()
                ));
            }
        }
        let ch = (CAT_H * unit).round() as u32;
        let cw = (ch as f32 * CatArt::ASPECT).round() as u32;
        for pose in CatPose::ALL {
            time(&format!("cat {pose:?}"), scale, |dark| {
                let art = CatArt {
                    pose,
                    tilt: 0,
                    dark,
                };
                std::hint::black_box(art.bake(cw, ch));
            });
        }
    }
    assert!(reds.is_empty(), "{}", reds.join("\n"));
}
