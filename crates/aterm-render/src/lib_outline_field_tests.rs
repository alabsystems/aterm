// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{OutlineField, coverage_ink, evened_ink};

fn disc(w: usize, h: usize, r: f32) -> Vec<u8> {
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            let hits = (0..16)
                .filter(|k| {
                    let (sx, sy) = (
                        x + (k % 4) as f32 / 4.0 + 0.125,
                        y + (k / 4) as f32 / 4.0 + 0.125,
                    );
                    (sx - cx).hypot(sy - cy) <= r
                })
                .count();
            (hits * 255 / 16) as u8
        })
        .collect()
}

#[test]
fn an_outline_field_moves_the_outline_by_a_fraction_of_a_pixel() {
    let (w, h) = (17, 17);
    let mask = disc(w, h, 5.0);
    let field = OutlineField::new(&mask, w, h, 3);
    assert_eq!(field.coverage(0.0), mask, "offset 0 is the mask");
    let mut last = 0.0;
    for step in -12..=12 {
        let ink = field.ink(step as f32 / 8.0);
        assert!(ink >= last, "ink falls at offset {}", step as f32 / 8.0);
        last = ink;
    }
    for (offset, r) in [(0.5, 5.5), (-0.5, 4.5)] {
        let want = coverage_ink(&disc(w, h, r));
        let got = coverage_ink(&field.coverage(offset));
        assert!(
            (got - want).abs() <= 0.04 * want,
            "offset {offset}: ink {got}, a disc of radius {r} holds {want}"
        );
    }
    let target = coverage_ink(&mask) * 1.2;
    let offset = field.offset_for_ink(target, 2.0);
    assert!(
        offset > 0.0 && (field.ink(offset) - target).abs() < 0.01,
        "{offset}"
    );
    assert_eq!(field.offset_for_ink(1e6, 2.0), 2.0, "past reach: the bound");
}

#[test]
fn a_cycle_s_frames_come_down_to_its_star_and_no_further_than_its_rest() {
    use super::cycle_cap;
    // The Semibold's `*` at 34 px: its spread lies over every evened frame (the heaviest
    // 132), so none moves.
    let cap = cycle_cap(96.0, Some(118.7), 1.5).expect("a star");
    assert!(
        cap > 132.0 && (cap - 0.99 * 1.5 * 96.0).abs() < 1e-3,
        "{cap}"
    );
    // The Regular's lighter `*`: its spread lies under the rest, which is kept, so the
    // heavier frames come down to the rest.
    assert_eq!(cycle_cap(75.8, Some(118.7), 1.5), Some(118.7));
    // A cycle with no rest among its frames: the spread alone.
    let bare = cycle_cap(75.8, None, 1.5).expect("a star");
    assert!((bare - 0.99 * 1.5 * 75.8).abs() < 1e-3, "{bare}");
    // No star: no cap.
    assert_eq!(cycle_cap(0.0, Some(118.7), 1.5), None);
    assert_eq!(cycle_cap(75.8, Some(118.7), 1.0), None);
}

#[test]
fn a_cycle_evens_to_the_spread_about_its_anchor() {
    assert_eq!(evened_ink(&[70.0, 90.0], 70.0, None, 1.4), None);
    let inks = [70.5, 63.7, 71.3, 118.7, 157.7];
    let even = |anchor| -> Vec<f32> {
        inks.iter()
            .map(|&i| evened_ink(&inks, i, anchor, 1.4).expect("evened"))
            .collect()
    };
    for anchor in [Some(118.7), None] {
        let out = even(anchor);
        let (lo, hi) = (out[1], out[4]);
        assert!((hi / lo - 1.4).abs() < 1e-3, "{anchor:?}: {out:?}");
        let mut ranked: Vec<usize> = (0..5).collect();
        ranked.sort_by(|&a, &b| out[a].total_cmp(&out[b]));
        assert_eq!(ranked, [1, 0, 2, 3, 4], "{anchor:?}: ranks kept, {out:?}");
        if anchor.is_some() {
            assert!(
                (out[3] - 118.7).abs() < 1e-3,
                "the anchor keeps its ink: {out:?}"
            );
        } else {
            assert!((lo / 63.7 - 157.7 / hi).abs() < 1e-3, "{out:?}");
        }
    }
}
