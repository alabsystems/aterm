// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/autohint/outline.rs and modified by
// the aterm project in 2026: filled from aterm's outline sources, the
// ahead-of-time (ttfautohint) corner rule and the advance dropped.
// See ../../LICENSE-FONTATIONS for the upstream license.

use super::*;

#[test]
fn direction_from_vectors() {
    assert_eq!(Direction::new(-100, 0), Direction::Left);
    assert_eq!(Direction::new(100, 0), Direction::Right);
    assert_eq!(Direction::new(0, -100), Direction::Down);
    assert_eq!(Direction::new(0, 100), Direction::Up);
    assert_eq!(Direction::new(7, 100), Direction::Up);
    assert_eq!(Direction::new(8, 100), Direction::None);
}

#[test]
fn direction_axes() {
    use Direction::*;
    let hori = [Left, Right];
    let vert = [Up, Down];
    for h in hori {
        for h2 in hori {
            assert!(h.is_same_axis(h2));
            if h != h2 {
                assert!(h.is_opposite(h2));
            } else {
                assert!(!h.is_opposite(h2));
            }
        }
        for v in vert {
            assert!(!h.is_same_axis(v));
            assert!(!h.is_opposite(v));
        }
    }
    for v in vert {
        for v2 in vert {
            assert!(v.is_same_axis(v2));
            if v != v2 {
                assert!(v.is_opposite(v2));
            } else {
                assert!(!v.is_opposite(v2));
            }
        }
    }
}

#[test]
fn fill_sanity_checks_u16_index_bounds() {
    let mut outline = Outline::default();
    outline
        .points
        .resize(u16::MAX as usize + 1, Point::default());
    outline
        .contours
        .resize(u16::MAX as usize + 1, Contour::default());
    assert!(outline.analyze_and_validate().is_ok());
    outline
        .points
        .resize(u16::MAX as usize + 2, Point::default());
    assert_eq!(
        outline.analyze_and_validate(),
        Err(DrawError::TooManyPoints)
    );
    outline.points.clear();
    outline
        .contours
        .resize(u16::MAX as usize + 2, Contour::default());
    assert_eq!(
        outline.analyze_and_validate(),
        Err(DrawError::TooManyPoints)
    );
}

#[test]
fn link_points_handles_max_points_last_next_overflow() {
    let mut outline = Outline::default();
    outline
        .points
        .resize(u16::MAX as usize + 1, Point::default());
    outline.contours.push(Contour {
        first_ix: 0,
        last_ix: u16::MAX,
    });
    outline.analyze_and_validate().unwrap();
    assert_eq!(outline.points[0].prev(), u16::MAX as usize);
    assert_eq!(outline.points[0].next(), 1);
    assert_eq!(
        outline.points[u16::MAX as usize].prev(),
        u16::MAX as usize - 1
    );
    assert_eq!(outline.points[u16::MAX as usize].next(), 0);
}
