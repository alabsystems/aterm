// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Golden coverage grids and out-of-bounds escape tests for [`crate::raster`].

use crate::raster::{Point, Rasterizer, point};

fn cov(w: usize, h: usize, f: impl FnOnce(&mut Rasterizer)) -> Vec<f32> {
    let mut r = Rasterizer::new(w, h);
    f(&mut r);
    let mut out = vec![0.0f32; w * h];
    r.for_each_pixel(|i, a| out[i] = a);
    out
}

fn polygon(r: &mut Rasterizer, pts: &[Point]) {
    for (i, p) in pts.iter().enumerate() {
        let next = pts.get(i + 1).unwrap_or(&pts[0]);
        r.draw_line(*p, *next);
    }
}

#[track_caller]
fn assert_grid(actual: &[f32], expected: &[f32], w: usize, what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: grid size");
    for (i, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (a - b).abs() <= 1e-6,
            "{what}: coverage differs at cell {i} (x={}, y={}): got {a}, pinned {b}",
            i % w,
            i / w
        );
    }
}

#[test]
fn golden_integer_aligned_square() {
    let got = cov(5, 5, |r| {
        polygon(
            r,
            &[
                point(1.0, 1.0),
                point(3.0, 1.0),
                point(3.0, 3.0),
                point(1.0, 3.0),
            ],
        );
    });
    #[rustfmt::skip]
    let want = [
        0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 1.0, 0.0, 0.0,
        0.0, 1.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 0.0, 0.0,
    ];
    assert_grid(&got, &want, 5, "integer-aligned square");
}

#[test]
fn golden_fractional_triangle() {
    let got = cov(4, 4, |r| {
        polygon(r, &[point(0.5, 0.25), point(3.25, 1.75), point(1.0, 3.5)]);
    });
    #[rustfmt::skip]
    let want = [
        0.263_549, 0.208_807, 0.0,       0.0,
        0.307_692, 0.995_739, 0.657_107, 0.041_351,
        0.153_846, 0.950_397, 0.335_317, 0.0,
        0.019_231, 0.160_714, 0.0,       0.0,
    ];
    assert_grid(&got, &want, 4, "fractional triangle");
}

#[test]
fn reversed_edges_cancel_to_exact_zero() {
    let got = cov(8, 8, |r| {
        for (a, b) in [
            (point(1.3, 0.7), point(6.9, 5.2)),
            (point(0.1, 7.9), point(7.5, 0.2)),
            (point(3.0, 1.0), point(3.0, 7.0)),
        ] {
            r.draw_line(a, b);
            r.draw_line(b, a);
        }
    });
    for (i, v) in got.iter().enumerate() {
        assert_eq!(*v, 0.0, "cell {i} did not cancel: {v}");
    }
}

#[test]
fn overlapping_contours_report_above_one_unclamped() {
    let got = cov(4, 4, |r| {
        for _ in 0..2 {
            polygon(
                r,
                &[
                    point(1.0, 1.0),
                    point(3.0, 1.0),
                    point(3.0, 3.0),
                    point(1.0, 3.0),
                ],
            );
        }
    });
    let doubled = got[4 + 1];
    assert!(
        (doubled - 2.0).abs() < 1e-6,
        "doubly-covered cell reported {doubled} instead of 2.0"
    );
}

fn escape_case(what: &str, escape: impl FnOnce(&mut Rasterizer)) {
    let mut r = Rasterizer::new(8, 8);
    polygon(
        &mut r,
        &[
            point(1.0, 1.0),
            point(3.0, 1.0),
            point(3.0, 3.0),
            point(1.0, 3.0),
        ],
    );
    escape(&mut r);
    let mut out = vec![0.0f32; 64];
    r.for_each_pixel(|i, a| out[i] = a);
    for (i, v) in out.iter().enumerate() {
        assert!(v.is_finite(), "{what}: cell {i} is not finite: {v}");
    }
}

#[test]
fn an_edge_far_left_of_the_grid_terminates() {
    escape_case("far left, row 0", |r| {
        r.draw_line(point(-1.0e7, 0.0), point(-1.0e7, 8.0));
    });
    escape_case("far left, crossing in", |r| {
        r.draw_line(point(-1.6e7, 4.0), point(1.6e7, 8.0));
    });
    escape_case("far left, single scanline", |r| {
        r.draw_line(point(-3.0e7, 5.1), point(4.0, 5.9));
    });
}

#[test]
fn an_edge_far_right_of_the_grid_terminates() {
    escape_case("far right", |r| {
        r.draw_line(point(3.0e7, 0.0), point(3.0e7, 8.0));
    });
    escape_case("far right, crossing out", |r| {
        r.draw_line(point(1.0, 0.5), point(3.0e2, 7.5));
    });
}

#[test]
fn an_absurdly_wide_span_terminates() {
    let mut r = Rasterizer::new(4096, 4096);
    r.draw_line(point(-1.6e7, 0.0), point(1.6e7, 4096.0));
    r.draw_line(point(1.6e7, 4096.0), point(-1.6e7, 0.0));
    let mut nonzero = 0usize;
    r.for_each_pixel(|_, a| {
        if a != 0.0 {
            nonzero += 1;
        }
    });
    assert_eq!(nonzero, 0, "an edge and its reverse must still cancel");
}

#[test]
fn an_absurd_quadratic_terminates() {
    escape_case("absurd quad", |r| {
        r.draw_quad(point(1.0, 1.0), point(1.0e15, 1.0e15), point(2.0, 7.0));
    });
    escape_case("absurd cubic", |r| {
        r.draw_cubic(
            point(1.0, 1.0),
            point(1.0e15, -1.0e15),
            point(-1.0e15, 1.0e15),
            point(2.0, 7.0),
        );
    });
}

#[test]
fn non_finite_coordinates_terminate_without_panicking() {
    for (what, escape) in [
        (
            "infinite endpoint",
            Box::new(|r: &mut Rasterizer| {
                r.draw_line(point(f32::INFINITY, 0.0), point(1.0, 8.0));
            }) as Box<dyn FnOnce(&mut Rasterizer)>,
        ),
        (
            "nan endpoint",
            Box::new(|r: &mut Rasterizer| {
                r.draw_line(point(f32::NAN, 0.0), point(1.0, 8.0));
            }),
        ),
        (
            "nan control point",
            Box::new(|r: &mut Rasterizer| {
                r.draw_quad(point(1.0, 1.0), point(f32::NAN, f32::NAN), point(2.0, 7.0));
            }),
        ),
        (
            "infinite cubic control point",
            Box::new(|r: &mut Rasterizer| {
                r.draw_cubic(
                    point(1.0, 1.0),
                    point(f32::INFINITY, 1.0),
                    point(1.0, f32::NEG_INFINITY),
                    point(2.0, 7.0),
                );
            }),
        ),
    ] {
        let mut r = Rasterizer::new(8, 8);
        escape(&mut r);
        let mut bytes = Vec::with_capacity(64);
        r.for_each_pixel(|_, a| bytes.push((a * 255.0 + 0.5).clamp(0.0, 255.0) as u8));
        assert_eq!(bytes.len(), 64, "{what}: the walk must visit every cell");
    }
}

#[test]
fn degenerate_grids_are_inert() {
    for (w, h) in [(0usize, 0usize), (0, 4), (4, 0), (1, 1)] {
        let got = cov(w, h, |r| {
            polygon(r, &[point(0.5, 0.5), point(3.5, 0.5), point(3.5, 3.5)]);
        });
        assert_eq!(got.len(), w * h);
        for v in &got {
            assert!(v.is_finite());
        }
    }
}
