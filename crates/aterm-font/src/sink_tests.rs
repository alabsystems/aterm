// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0 AND (MIT OR Apache-2.0)
//
// `OutlineSink` is ttf-parser 0.25.1's `OutlineBuilder` (src/lib.rs, MIT OR
// Apache-2.0; see LICENSE-MIT and LICENSE-APACHE), renamed by the aterm
// project in 2026. The rest of this file is aterm's.

use super::*;

#[derive(Default)]
struct Log(Vec<(f32, f32, PointKind)>, usize);

impl PointSink for Log {
    fn point(&mut self, x: f32, y: f32, kind: PointKind) {
        self.0.push((x, y, kind));
    }
    fn end_contour(&mut self) {
        self.1 += 1;
    }
}

use PointKind::{OffCurveCubic as Cubic, OnCurve as On};

#[test]
fn a_contour_closing_on_its_start_drops_the_repeat() {
    let mut log = Log::default();
    let mut s = SegmentPoints::new(&mut log);
    s.move_to(0.0, 0.0);
    s.line_to(10.0, 0.0);
    s.line_to(0.0, 0.0);
    s.close();
    s.move_to(0.0, 0.0);
    s.curve_to(1.0, 2.0, 3.0, 4.0, 5.0, 6.0);
    s.curve_to(7.0, 8.0, 9.0, 10.0, 0.0, 0.0);
    s.close();
    s.finish();
    assert_eq!(
        log.0,
        vec![
            (0.0, 0.0, On),
            (10.0, 0.0, On),
            (0.0, 0.0, On),
            (1.0, 2.0, Cubic),
            (3.0, 4.0, Cubic),
            (5.0, 6.0, On),
            (7.0, 8.0, Cubic),
            (9.0, 10.0, Cubic),
        ]
    );
    assert_eq!(log.1, 2);
}

#[test]
fn an_open_contour_keeps_its_last_point() {
    let mut log = Log::default();
    let mut s = SegmentPoints::new(&mut log);
    s.move_to(0.0, 0.0);
    s.line_to(10.0, 0.0);
    s.line_to(10.0, 10.0);
    s.finish();
    assert_eq!(
        log.0,
        vec![(0.0, 0.0, On), (10.0, 0.0, On), (10.0, 10.0, On)]
    );
    assert_eq!(log.1, 1);
}
