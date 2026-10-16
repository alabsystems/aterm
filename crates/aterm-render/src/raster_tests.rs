// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`crate::raster`]'s golden grids and escape cases. A sibling file, not a
//! `mod tests` inside `raster.rs`: aterm-font's `font_suite` and the font
//! oracle's recorder include `raster.rs` by path, and would run these again.

use crate::raster::{Point, Rasterizer, point};

/// Fill a `w`×`h` grid and read the coverage back as a flat row-major
/// buffer — the shape every caller in this crate consumes.
fn cov(w: usize, h: usize, f: impl FnOnce(&mut Rasterizer)) -> Vec<f32> {
    let mut r = Rasterizer::new(w, h);
    f(&mut r);
    let mut out = vec![0.0f32; w * h];
    r.for_each_pixel(|i, a| out[i] = a);
    out
}

/// Close a polygon into the rasterizer, edge by edge.
fn polygon(r: &mut Rasterizer, pts: &[Point]) {
    for (i, p) in pts.iter().enumerate() {
        let next = pts.get(i + 1).unwrap_or(&pts[0]);
        r.draw_line(*p, *next);
    }
}

/// Compare against a pinned grid to 1e-6, because antialiased coverage is where
/// "close enough" hides a visible bug.
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

// Golden grids. The two line-only grids are the retired `ab_glyph_rasterizer`'s own
// answers and pin the fill; the quad and cubic grids also carry the
// module's flattening.

/// An axis-aligned unit square on whole-pixel boundaries: the case where
/// coverage must be exactly 1.0 inside and exactly 0.0 outside, with no
/// antialiasing anywhere.
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

/// A triangle on fractional coordinates: every edge crosses columns at a
/// different sub-cell offset, so this exercises the trapezoid split, the
/// two-cell fast path and the interior slabs in one figure.
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

/// A quadratic, the TrueType outline form. This is the fill's answer over this
/// module's flattening, not the retired crate's: the accuracy limits in
/// `tests/raster_accuracy_survey.rs` bound it, and this grid makes a re-record loud.
#[test]
fn golden_quadratic() {
    let got = cov(6, 6, |r| {
        r.draw_quad(point(0.5, 0.5), point(5.5, 2.0), point(0.5, 5.5));
        r.draw_line(point(0.5, 5.5), point(0.5, 0.5));
    });
    #[rustfmt::skip]
    let want = [
        0.209_617, 0.138_579, 0.0,       0.0, 0.0, 0.0,
        0.5,       0.987_065, 0.469_326, 0.0, 0.0, 0.0,
        0.5,       1.0,       0.963_990, 0.0, 0.0, 0.0,
        0.5,       1.0,       0.629_342, 0.0, 0.0, 0.0,
        0.5,       0.713_891, 0.037_827, 0.0, 0.0, 0.0,
        0.159_617, 0.011_752, 0.0,       0.0, 0.0, 0.0,
    ];
    assert_grid(&got, &want, 6, "quadratic");
}

/// A cubic, the CFF outline form, pinned on the same terms as [`golden_quadratic`].
#[test]
fn golden_cubic() {
    let got = cov(6, 6, |r| {
        r.draw_cubic(
            point(0.5, 0.5),
            point(5.5, 1.0),
            point(5.5, 5.0),
            point(0.5, 5.5),
        );
        r.draw_line(point(0.5, 5.5), point(0.5, 0.5));
    });
    #[rustfmt::skip]
    let want = [
        0.234_436, 0.332_630, 0.060_644, 0.0,       0.0,       0.0,
        0.5,       1.0,       0.953_358, 0.400_362, 0.0,       0.0,
        0.5,       1.0,       1.0,       0.992_968, 0.141_929, 0.0,
        0.5,       1.0,       1.0,       0.992_968, 0.141_929, 0.0,
        0.5,       1.0,       0.953_358, 0.400_362, 0.0,       0.0,
        0.234_436, 0.332_630, 0.060_644, 0.0,       0.0,       0.0,
    ];
    assert_grid(&got, &want, 6, "cubic");
}

/// Winding cancels exactly: an edge and its reverse leave the grid at
/// literal zero, which is what lets `for_each_pixel` carry one accumulator
/// across every row of the buffer.
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

/// `|acc|` is reported unclamped: two overlapping same-winding contours
/// push a cell above 1.0, and every caller clamps on its way to 8 bits. A
/// `.min(1.0)` here would silently darken every overlapping contour.
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
    // Cell (1, 1) of the 4-wide grid — inside both copies of the square.
    let doubled = got[4 + 1];
    assert!(
        (doubled - 2.0).abs() < 1e-6,
        "doubly-covered cell reported {doubled} instead of 2.0"
    );
}

// The grid-escape paths: each must terminate without panicking.

/// A reference square plus an escaping contour: the escape may cost its own area,
/// but must not hang, panic or leave a non-finite cell.
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

/// Far left of the grid, including from a row past the first, where the
/// `linestart + x0i < 0` guard does not fire and the interior slab loop's
/// lower bound is the thing doing the work.
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

/// Far right of the grid, where the interior loop's upper bound and
/// [`Rasterizer::add`]'s drop are what keep it bounded.
#[test]
fn an_edge_far_right_of_the_grid_terminates() {
    escape_case("far right", |r| {
        r.draw_line(point(3.0e7, 0.0), point(3.0e7, 8.0));
    });
    escape_case("far right, crossing out", |r| {
        r.draw_line(point(1.0, 0.5), point(3.0e2, 7.5));
    });
}

/// A span so wide an unclamped interior loop would run tens of millions of
/// iterations per scanline: the call must return.
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

/// A quadratic whose control point is astronomically far away asks the
/// deviation heuristic for millions of flattening segments. The cap is what
/// makes it return.
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

/// Non-finite input (a font coordinate that scales to infinity or NaN) must not hang
/// or panic. Output finiteness is not promised (a NaN poisons the prefix sum), but the
/// callers' 8-bit quantisation still yields bytes: `NaN as u8` is zero, a blank glyph.
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

/// A degenerate grid must not panic on construction or on a fill.
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
