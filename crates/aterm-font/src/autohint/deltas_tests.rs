// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/glyf/deltas.rs and modified by the
// aterm project in 2026: specialized to integer points and 16.16 deltas (the
// unscaled loader's types), every index checked.
// See ../../LICENSE-FONTATIONS for the upstream license.

use super::*;

fn make_points(tuples: &[(i32, i32)]) -> Vec<Point<i32>> {
    tuples.iter().map(|&(x, y)| Point::new(x, y)).collect()
}

fn make_working_points_and_flags(
    points: &[Point<i32>],
    deltas: &[Point<i32>],
) -> (Vec<Point<Fixed>>, Vec<PointFlags>) {
    let working_points = points
        .iter()
        .zip(deltas)
        .map(|(point, delta)| point.map(Fixed::from_i32) + delta.map(Fixed::from_i32))
        .collect();
    let flags = deltas
        .iter()
        .map(|delta| {
            let mut flags = PointFlags::default();
            if delta.x != 0 || delta.y != 0 {
                flags.set_marker(PointMarker::HAS_DELTA);
            }
            flags
        })
        .collect();
    (working_points, flags)
}

#[test]
fn shift() {
    let points = make_points(&[(245, 630), (260, 700), (305, 680)]);
    let deltas = make_points(&[(20, -10), (0, 0), (0, 0)]);
    let (mut working_points, flags) = make_working_points_and_flags(&points, &deltas);
    interpolate_deltas(&points, &flags, &[2], &mut working_points).unwrap();
    let expected = &[
        Point::new(265, 620).map(Fixed::from_i32),
        Point::new(280, 690).map(Fixed::from_i32),
        Point::new(325, 670).map(Fixed::from_i32),
    ];
    assert_eq!(&working_points, expected);
}

#[test]
fn interpolate() {
    let points = make_points(&[(245, 630), (260, 700), (305, 680)]);
    let deltas = make_points(&[(28, -62), (0, 0), (-42, -57)]);
    let (mut working_points, flags) = make_working_points_and_flags(&points, &deltas);
    interpolate_deltas(&points, &flags, &[2], &mut working_points).unwrap();
    assert_eq!(
        working_points[1],
        Point::new(
            Fixed::from_f64(260.0 + 10.4999237060547),
            Fixed::from_f64(700.0 - 57.0)
        )
    );
}
