// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What one pose of the setup screen's kitty costs to bake (`docs/DESIGN-init-on-by-default-2026-10-25.md`
//! §4.5): each pose the lockup shows, in the brand coat on both grounds, at the lockup's device
//! heights for 1×, 2× and 3× displays. The page bakes them on a background thread before it
//! needs them, so this is the work kept off the main thread, and the budget is what a cold
//! first frame pays where a pose was not ready yet.

use std::time::{Duration, Instant};

use aterm_effects::cat_baker::ResolvedFills;
use aterm_effects::pet_baker::{PetBaker, bake_pose};
use aterm_effects::pet_glyphs_gen::PetGlyphId;

/// The poses the lockup draws (§4.5's table).
const POSES: [PetGlyphId; 9] = [
    PetGlyphId::PetStand,
    PetGlyphId::PetSit,
    PetGlyphId::PetSitLookup,
    PetGlyphId::PetSitEar,
    PetGlyphId::PetLoaf,
    PetGlyphId::PetWalk0,
    PetGlyphId::PetWalk1,
    PetGlyphId::PetWalk2,
    PetGlyphId::PetWalk3,
];

/// The kitty's height in device pixels on a 1×, 2× and 3× display (29 pt).
const HEIGHTS: [u32; 3] = [29, 58, 87];

/// Bakes of each pose and size measured.
const RUNS: usize = 40;

/// One pose at 2× may take this long at the 90th percentile: a cold first frame stays inside
/// one display refresh's slack.
const P90_BUDGET_2X: Duration = Duration::from_millis(4);

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let at = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[at.min(sorted.len() - 1)]
}

fn main() {
    if cfg!(debug_assertions) {
        panic!("a bake's cost is a release-build number: run it with `targo --unverified bench`");
    }
    let mut reds = Vec::new();
    println!("pose                h    p50 µs   p90 µs   max µs");
    for h in HEIGHTS {
        let mut all = Vec::new();
        for pose in POSES {
            let w = (h as f32 * PetBaker::aspect(pose)).round().max(1.0) as u32;
            let mut took = Vec::with_capacity(RUNS * 2);
            for dark in [false, true] {
                let fills = ResolvedFills::from_indices(13, 4, dark);
                for _ in 0..RUNS {
                    let start = Instant::now();
                    let tile = bake_pose(pose, &fills, w, h);
                    took.push(start.elapsed());
                    std::hint::black_box(tile);
                }
            }
            took.sort_unstable();
            println!(
                "{:<18} {h:>3} {:>9} {:>8} {:>8}",
                format!("{pose:?}"),
                percentile(&took, 0.5).as_micros(),
                percentile(&took, 0.9).as_micros(),
                took[took.len() - 1].as_micros(),
            );
            all.extend(took);
        }
        all.sort_unstable();
        let p90 = percentile(&all, 0.9);
        println!(
            "all poses at {h} px: p50 {} µs, p90 {} µs",
            percentile(&all, 0.5).as_micros(),
            p90.as_micros()
        );
        if h == 58 && p90 > P90_BUDGET_2X {
            reds.push(format!(
                "a 2× pose bake's p90 is {} µs, over {} µs",
                p90.as_micros(),
                P90_BUDGET_2X.as_micros()
            ));
        }
    }
    assert!(reds.is_empty(), "{}", reds.join("\n"));
}
