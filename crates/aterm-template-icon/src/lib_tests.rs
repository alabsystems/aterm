// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The left half of a 4 × 2 pt canvas.
fn left_half(p: Pt) -> bool {
    p.x < 2.0
}

#[test]
fn coverage_is_a_byte_per_pixel_in_row_order() {
    let cov = coverage((4, 2), (4.0, 2.0), left_half);
    assert_eq!(cov, [255, 255, 0, 0, 255, 255, 0, 0]);
    // The same canvas at two pixels a point.
    let fine = coverage((8, 4), (4.0, 2.0), left_half);
    assert_eq!(fine.len(), 32);
    let (rows, rest) = fine.as_chunks::<8>();
    assert!(rest.is_empty());
    assert!(
        rows.iter()
            .all(|row| *row == [255, 255, 255, 255, 0, 0, 0, 0])
    );
    // An edge through the middle of a pixel covers half of it.
    let half = coverage((1, 1), (1.0, 1.0), |p| p.x < 0.5);
    assert_eq!(half, [128]);
}

#[test]
fn a_canvas_with_no_pixels_or_too_many_is_empty() {
    assert!(coverage((0, 4), (1.0, 1.0), |_| true).is_empty());
    assert!(coverage((4, 0), (1.0, 1.0), |_| true).is_empty());
    assert!(coverage((u32::MAX, u32::MAX), (1.0, 1.0), |_| true).is_empty());
    assert!(png(&[], 0, 0).is_empty());
    assert!(png(&[1, 2, 3], 2, 2).is_empty());
}

#[test]
fn the_png_is_black_with_the_coverage_as_alpha() {
    let cov = coverage((6, 4), (3.0, 2.0), |p| p.x + p.y < 2.5);
    let (rgba, w, h) = aterm_png::decode_rgba8(&png(&cov, 6, 4), 6).expect("it decodes");
    assert_eq!((w, h), (6, 4));
    let (pixels, rest) = rgba.as_chunks::<4>();
    assert!(rest.is_empty());
    assert_eq!(pixels.iter().map(|p| p[3]).collect::<Vec<u8>>(), cov);
    assert!(pixels.iter().all(|p| p[..3] == [0, 0, 0]));
}

#[test]
fn the_shapes_are_negative_inside_and_positive_outside() {
    let square = [1.0, 1.0, 3.0, 3.0];
    assert!(sdf::round_rect(Pt::new(2.0, 2.0), square, [0.0; 4]) < 0.0);
    assert!(sdf::round_rect(Pt::new(3.5, 2.0), square, [0.0; 4]) > 0.0);
    // A rounded corner cuts the square's own corner away.
    assert!(sdf::round_rect(Pt::new(1.05, 1.05), square, [0.0; 4]) < 0.0);
    assert!(sdf::round_rect(Pt::new(1.05, 1.05), square, [1.0; 4]) > 0.0);
    let band = [2.0, 2.0, 1.5, 0.5];
    assert!(sdf::ring(Pt::new(3.0, 2.0), band) < 0.0);
    assert!(sdf::ring(Pt::new(2.0, 2.0), band) > 0.0);
    assert!(sdf::ring(Pt::new(4.0, 2.0), band) > 0.0);
    let (a, b) = (Pt::new(0.0, 0.0), Pt::new(4.0, 0.0));
    assert_eq!(sdf::segment(Pt::new(2.0, 1.0), a, b), 1.0);
    assert_eq!(sdf::segment(Pt::new(-3.0, 4.0), a, b), 5.0);
    assert_eq!(sdf::segment(Pt::new(3.0, 4.0), a, a), 5.0);
}
