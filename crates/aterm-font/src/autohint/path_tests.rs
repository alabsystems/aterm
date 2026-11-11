// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/outline/path.rs and read-fonts 0.43.2
// src/tables/glyf.rs (`PointCoord for F26Dot6`), and modified by the aterm
// project in 2026: FreeType's decomposition alone, on 26.6 points, into a
// recording that replays to an `OutlineSink`.
// See ../../LICENSE-FONTATIONS for the upstream license.

use super::*;

fn path_of(all_off_curve: bool) -> Vec<Segment> {
    let pt = |x: i32, y: i32, on: bool| ContourPoint {
        x: F26Dot6::from_bits(x),
        y: F26Dot6::from_bits(y),
        flags: if on {
            PointFlags::on_curve()
        } else {
            PointFlags::off_curve_quad()
        },
    };
    let points = [
        pt(640, 128, false),
        pt(256, 64, !all_off_curve),
        pt(640, 64, false),
        pt(128, 128, false),
    ];
    let mut pen = Recorded::default();
    contour_to_path(points.iter().copied(), points[0], points[3], &mut pen).unwrap();
    pen.0
}

#[test]
fn all_off_curve_to_path_scan_backward() {
    use Segment::*;
    assert_eq!(
        path_of(true),
        [
            Move(6.0, 2.0),
            Quad(10.0, 2.0, 7.0, 1.5),
            Quad(4.0, 1.0, 7.0, 1.0),
            Quad(10.0, 1.0, 6.0, 1.5),
            Quad(2.0, 2.0, 6.0, 2.0),
            Close
        ]
    );
}

#[test]
fn start_off_curve_to_path_scan_backward() {
    use Segment::*;
    assert_eq!(
        path_of(false),
        [
            Move(6.0, 2.0),
            Quad(10.0, 2.0, 4.0, 1.0),
            Quad(10.0, 1.0, 6.0, 1.5),
            Quad(2.0, 2.0, 6.0, 2.0),
            Close
        ]
    );
}
