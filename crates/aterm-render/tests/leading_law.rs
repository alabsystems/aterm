// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Half-leading split law for [`aterm_render::cell_h_baseline`] over a dense metrics lattice
//! and on the live [`Renderer`].

use aterm_render::{Renderer, Theme, cell_h_baseline};

const ASCENTS: [f32; 7] = [4.0, 7.3, 9.6, 12.0, 15.5, 20.0, 24.8];
const DESCENTS: [f32; 5] = [-1.0, -2.4, -3.0, -5.7, -8.0];
const GAPS: [f32; 6] = [0.0, 0.5, 1.0, 2.3, 4.0, 7.9];
const SCALES: [f32; 8] = [0.5, 0.8, 0.9, 1.0, 1.05, 1.25, 1.5, 2.0];

#[test]
fn half_leading_splits_within_one_px() {
    let mut split_moved_baseline = 0u32; // non-vacuity
    for a in ASCENTS {
        for d in DESCENTS {
            for g in GAPS {
                for s in SCALES {
                    let (cell_h, baseline) = cell_h_baseline(a, d, g, s);
                    let content = a - d;
                    let above = baseline as f32 - a;
                    let below = cell_h as f32 - content - above;
                    assert!(
                        (above - below).abs() <= 1.0 + 1e-4,
                        "half-leading: ascent={a} descent={d} gap={g} scale={s} \
                         above={above} below={below}"
                    );
                    if baseline != a.round() as i32 {
                        split_moved_baseline += 1;
                    }
                }
            }
        }
    }
    assert!(split_moved_baseline > 100, "got {split_moved_baseline}");
}

#[test]
fn renderer_geometry_follows_the_law_live() {
    let bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/jetbrains-mono.ttf"
    ))
    .expect("committed JetBrains Mono fixture");
    let mut r = Renderer::from_bytes(&bytes, 16.0, Theme::default()).expect("fixture parses");

    let (asc, desc, gap) = r
        .debug_geometry_metrics()
        .expect("committed fixture exposes live vertical metrics");
    let (_, h0) = r.cell_size();
    let b0 = r.baseline();
    assert_eq!((h0, b0), cell_h_baseline(asc, desc, gap, 1.0));

    for scale in SCALES {
        r.set_line_height(scale);
        assert_eq!(
            (r.cell_size().1, r.baseline()),
            cell_h_baseline(asc, desc, gap, scale),
            "live geometry matches cell_h_baseline at scale {scale}"
        );
    }

    r.set_line_height(1.5);
    let (_, h1) = r.cell_size();
    let b1 = r.baseline();
    assert!(h1 > h0, "1.5 line-height grows the cell box");

    r.set_adjust_baseline(3);
    assert_eq!(r.baseline(), b1 + 3, "escape hatch shifts the baseline");
    assert_eq!(r.cell_size().1, h1, "cell box is untouched");
    r.set_adjust_baseline(1000);
    assert_eq!(r.baseline(), b1 + 64, "clamped to +/-64");
    r.set_adjust_baseline(0);
    assert_eq!(r.baseline(), b1, "0 restores the pure derivation");
}
