// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Full, Star, coverage, draw, is_spinner_star, spec};

/// `(ink sum in full texels, solid share, peak)` of a coverage mask:
/// solid is a texel at 90% coverage or more, ink any texel at all.
fn ink(mask: &[u8]) -> (f32, f32, u8) {
    let inked = mask.iter().filter(|&&c| c > 0).count();
    let solid = mask.iter().filter(|&&c| c >= 230).count();
    let sum = mask.iter().map(|&c| f32::from(c) / 255.0).sum();
    let peak = mask.iter().copied().max().unwrap_or(0);
    (sum, solid as f32 / inked.max(1) as f32, peak)
}

/// Claude Code's spinner frames read as one growing star, not a cycle
/// that dims: at the Windows default cell (9x23), its 1.25x and 1.5x
/// zooms (11x29, 13x36), larger cells (14x34, 18x46) and the cells of
/// other font sizes (8x17, 10x21), every frame reaches full ink, at
/// least a quarter of its ink is solid, and the frames' ink sums are
/// within 1.5x of each other.
#[test]
fn spinner_frames_are_solid_and_even() {
    let frames = ['\u{2722}', '\u{2733}', '\u{2736}', '\u{273B}', '\u{273D}'];
    assert!(frames.iter().all(|&ch| is_spinner_star(ch)));
    for (w, h) in [
        (9, 23),
        (11, 29),
        (13, 36),
        (14, 34),
        (18, 46),
        (8, 17),
        (10, 21),
    ] {
        let mut sums = Vec::new();
        for ch in frames {
            let mask = coverage(ch, w, h, 1).expect("drawn");
            let (sum, solid, peak) = ink(&mask);
            assert_eq!(peak, 255, "{ch} at {w}x{h}");
            assert!(solid >= 0.25, "{ch} at {w}x{h}: {solid:.2} solid");
            sums.push(sum);
        }
        let (lo, hi) = sums
            .iter()
            .fold((f32::MAX, 0.0f32), |(lo, hi), &s| (lo.min(s), hi.max(s)));
        assert!(hi <= lo * 1.5, "{w}x{h}: ink sums {sums:?}");
    }
    for ch in frames {
        let mask = coverage(ch, 7, 15, 1).expect("drawn");
        assert_eq!(ink(&mask).2, 255, "{ch} at 7x15");
    }
}

/// The coverage of `mask` (`w` wide) at the continuous point `(x, y)`,
/// bilinear between texel centres, in `0..=1`.
fn sample(mask: &[u8], w: usize, x: f32, y: f32) -> f32 {
    let h = mask.len() / w;
    let at = |i: f32, j: f32| {
        let (i, j) = (i.clamp(0.0, (w - 1) as f32), j.clamp(0.0, (h - 1) as f32));
        f32::from(mask[j as usize * w + i as usize]) / 255.0
    };
    let (fx, fy) = (x - 0.5, y - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1.0, y0) * tx;
    let bottom = at(x0, y0 + 1.0) * (1.0 - tx) + at(x0 + 1.0, y0 + 1.0) * tx;
    top * (1.0 - ty) + bottom * ty
}

/// `(least coverage along the six points, most along the six notches)`
/// of a six-pointed star drawn at `w`x`h`, sampled at `reach` of its
/// points' radius from the cell centre.
fn points_and_notches(mask: &[u8], w: usize, h: usize, reach: f32) -> (f32, f32) {
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let r = reach * (w as f32 / 2.0) / (std::f32::consts::PI / 6.0).cos();
    let along = |deg: f32| {
        let a = deg.to_radians();
        sample(mask, w, cx + r * a.cos(), cy + r * a.sin())
    };
    let points = (0..6).map(|k| along(-90.0 + 60.0 * k as f32));
    let notches = (0..6).map(|k| along(-60.0 + 60.0 * k as f32));
    (points.fold(1.0, f32::min), notches.fold(0.0, f32::max))
}

/// `✶` READS AS A SIX-POINTED STAR, NOT A HEXAGON.
#[test]
fn the_six_pointed_star_keeps_its_notches_open() {
    for (w, h) in [(9, 23), (11, 29), (13, 36), (7, 15), (8, 17), (10, 21)] {
        let star = coverage('\u{2736}', w, h, 1).expect("drawn");
        let (points, _) = points_and_notches(&star, w, h, 0.6);
        let (_, notches) = points_and_notches(&star, w, h, 0.65);
        assert!(points >= 0.5, "✶ at {w}x{h}: a point holds {points:.2}");
        assert!(notches <= 0.15, "✶ at {w}x{h}: a notch holds {notches:.2}");
        let edge = |x: usize| (0..h).map(|y| star[y * w + x]).max().unwrap_or(0);
        assert!(
            edge(0) > 0 && edge(w - 1) > 0,
            "✶ at {w}x{h}: its side points reach the cell's edges"
        );
        let fat = draw(spec(Full, &[Star(6, 0.65)]), w, h, 1);
        let (_, filled) = points_and_notches(&fat, w, h, 0.65);
        assert!(
            filled >= 0.5,
            "control at {w}x{h}: notches hold {filled:.2}"
        );
    }
}
