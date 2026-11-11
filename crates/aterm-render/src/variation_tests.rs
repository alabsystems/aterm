// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// Why [`RASTER_PAD`] exists, on a bare rasterizer: the segment
/// `(1.828125, 0.21875) → (0.0, 0.875)` marches to `x = -1.19e-7`, floors to `-1`,
/// and its scanline's area is skipped.
#[test]
fn a_boundary_hugging_outline_needs_the_raster_pad() {
    let tri = [(0.0f32, 0.875f32), (1.828125, 0.21875), (1.828125, 3.0)];
    let area = 0.5
        * (0..3)
            .map(|i| {
                let (a, b) = (tri[i], tri[(i + 1) % 3]);
                a.0 * b.1 - b.0 * a.1
            })
            .sum::<f32>()
            .abs();
    let ink_at = |pad: usize| {
        let (w, h) = (2usize, 3usize);
        let (gw, gh) = (w + 2 * pad, h + 2 * pad);
        let mut ras = crate::raster::Rasterizer::new(gw, gh);
        let m = |p: &(f32, f32)| crate::raster::point(p.0 + pad as f32, p.1 + pad as f32);
        for i in 0..3 {
            ras.draw_line(m(&tri[i]), m(&tri[(i + 1) % 3]));
        }
        let mut sum = 0.0f32;
        ras.for_each_pixel(|_, a| sum += a);
        sum
    };
    let flush = ink_at(0);
    let padded = ink_at(RASTER_PAD);
    let generous = ink_at(4 * RASTER_PAD);
    assert!(
        (padded - area).abs() < 0.01,
        "with RASTER_PAD the fill must deposit the outline's area: {padded} vs {area}"
    );
    assert!(
        (generous - padded).abs() < 0.01,
        "more slack must change nothing: {generous} vs {padded}"
    );
    assert!(
        (flush - area).abs() > 0.5,
        "PRECONDITION: flush against the grid edge this outline must lose ink \
         ({flush} vs {area}) — if it no longer does, the rasterizer changed and \
         this test has stopped proving why RASTER_PAD is there"
    );
}

/// The portable raster's coverage does not depend on how much empty grid surrounds
/// the outline (a scaled `bbox.x_min` of 0 puts it flush against the left edge):
/// held against a 4×-slack refill over printable ASCII and the box-drawing run at
/// every desktop ppem on the bundled face.
#[test]
#[cfg(feature = "embedded-font")]
fn varied_raster_is_invariant_to_grid_slack() {
    const SLACK: usize = 4;
    let bytes = crate::embedded_font();
    let face = aterm_font::Face::parse(bytes, 0).expect("the bundled face parses");
    let upem = f32::from(face.units_per_em());
    let mut checked = 0usize;
    for pxi in 12..=40u32 {
        let px = pxi as f32;
        let scale = px / upem;
        let mut chars: Vec<char> = (' '..='~').collect();
        chars.extend("─│┌┐└┘├┤┬┴┼━┃█▀▄▌▐░▒▓".chars());
        for ch in chars {
            let Some(g) = face.glyph_index(ch) else {
                continue;
            };
            let Some((w, h, _, _, _, cov)) = varied_glyph_raster_with_face(&face, g.0, px) else {
                continue;
            };
            if cov.is_empty() {
                continue;
            }
            checked += 1;
            let bbox = face
                .glyph_bounding_box(g)
                .expect("an inked glyph has a bbox");
            let x_min = (f32::from(bbox.x_min) * scale).floor();
            let y_max = (f32::from(bbox.y_max) * scale).ceil();
            let mut ras = crate::raster::Rasterizer::new(w + 2 * SLACK, h + 2 * SLACK);
            let mut b = OutlineToRaster {
                ras: &mut ras,
                scale,
                ox: x_min - SLACK as f32,
                oy: y_max + SLACK as f32,
                last: crate::raster::point(0.0, 0.0),
                start: crate::raster::point(0.0, 0.0),
            };
            face.outline_glyph(g, &mut b)
                .expect("the glyph we just drew still draws");
            b.close_contour();
            let gw = w + 2 * SLACK;
            let mut reference = vec![0u8; w * h];
            ras.for_each_pixel(|i, a| {
                let (gx, gy) = (i % gw, i / gw);
                let (Some(x), Some(y)) = (gx.checked_sub(SLACK), gy.checked_sub(SLACK)) else {
                    return;
                };
                if x < w && y < h {
                    reference[y * w + x] = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
                }
            });
            let worst = cov
                .iter()
                .zip(&reference)
                .map(|(a, b)| i32::from(*a) - i32::from(*b))
                .map(i32::abs)
                .max()
                .unwrap_or(0);
            assert!(
                worst <= 1,
                "{ch:?} at {pxi}px: coverage moved by {worst}/255 when the fill grid \
                 gained slack — the outline is sitting on the grid boundary and the \
                 rasterizer is losing a scanline"
            );
        }
    }
    assert!(
        checked > 2_000,
        "the sweep must rasterize thousands of glyphs, got {checked}"
    );
}

/// A blank glyph (the bundled JetBrains Mono's space) rasters to a 0x0 tile carrying
/// just the advance.
#[test]
fn varied_raster_blank_glyph_is_zero_sized() {
    let bytes = include_bytes!("../assets/bundled/JetBrainsMono-Regular.ttf");
    let face = aterm_font::Face::parse(bytes, 0).expect("the bundled face parses");
    let sp = face
        .glyph_index(' ')
        .filter(|g| g.0 != 0)
        .expect("the bundled face maps a space");
    let (w, h, _, _, adv, cov) =
        varied_glyph_raster(bytes, 0, &[], sp.0, 32.0).expect("the space rasters");
    assert_eq!((w, h), (0, 0), "space has no ink box");
    assert!(cov.is_empty(), "no coverage for a blank glyph");
    assert!(adv >= 0.0, "advance is finite and non-negative");
    assert!(parse_variation_spec("wght=+400").is_none());
}
