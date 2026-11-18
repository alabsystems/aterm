// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The road: the dot lanes and the star ahead (under everything), the ribbon's underglow, the
//! ribbon, the white line along its top edge, and, done, the sparkles along it behind the loaf.

use super::canvas::{self, Canvas, Shape};
use super::{CAT_SHORT, HALF, Ink, ROAD_X, Road, STAR_R, centre_y, offset_point};

/// The done sparkles along the road: where each stands on the design's artboard, how high above
/// the ribbon, their radius, whether pale (else teal), and their strength.
const ROAD_SPARKLES: [(f32, f32, f32, bool, f32); 4] = [
    (560.0, 20.0, 3.0, true, 0.55),
    (668.0, 30.0, 3.8, false, 0.7),
    (764.0, 24.0, 4.6, true, 0.85),
    (LAST_SPARKLE, 44.0, 5.6, false, 0.9),
];
/// The artboard's road: 802 units past the words, its loaf's feet [`CAT_SHORT`] short of the end.
const ARTBOARD_LENGTH: f32 = 802.0;
/// The last sparkle's x on the artboard, and how far behind the loaf's feet it stands there. On
/// a road of any length the sparkles keep their shares of the way from the road's start to that
/// spot behind the loaf, so the loaf (its tail 101 units back, its outline 2.6 more) covers none
/// of them, and at the artboard's length they stand where it put them.
const LAST_SPARKLE: f32 = 838.0;
const SPARKLES_BEHIND: f32 = ROAD_X + ARTBOARD_LENGTH - CAT_SHORT - LAST_SPARKLE;
/// The star's halo: its radius and its blur.
const HALO_R: f32 = 13.0;
const HALO_BLUR: f32 = 2.5;

/// The done sparkles on `road`: each one's centre, radius, whether pale, and strength.
pub(super) fn sparkles(road: Road) -> impl Iterator<Item = ((f32, f32), f32, bool, f32)> {
    let end = road.cat_x() - SPARKLES_BEHIND;
    let stretch = (end - ROAD_X) / (LAST_SPARKLE - ROAD_X);
    ROAD_SPARKLES
        .into_iter()
        .map(move |(at, up, r, pale, alpha)| {
            let x = ROAD_X + (at - ROAD_X) * stretch;
            ((x, centre_y(x) - HALF - up), r, pale, alpha)
        })
}

/// The star's halo, a teal disc blurred on a layer just its size.
pub(super) fn halo(c: &mut Canvas, star: (f32, f32), ink: &Ink) {
    c.dot_glow(star, HALO_R, ink.teal, HALO_BLUR, 0.22);
}

pub(super) fn paint(c: &mut Canvas, road: Road, ink: &Ink) {
    let nose = road.nose();
    if !road.done {
        for (lane, x, y) in road.dots() {
            c.dot(x, y, 1.15, ink.stripes[lane], ink.dots);
        }
        let (sx, sy) = road.star();
        halo(c, (sx, sy), ink);
        c.fill(&Shape::star(sx, sy, STAR_R), ink.teal, 1.0);
    }
    // The ribbon on a layer of its own: blurred under itself as its glow, then drawn.
    let mut ribbon = c.layer();
    // Each stripe covers its own band and one device pixel of the next, which is drawn over it,
    // so no seam shows the ground between two stripes; only violet reaches the ribbon's bottom
    // edge and the lower arc of its nose, so no stripe above it fringes that edge.
    let lap = 1.0 / c.unit();
    for (k, rgb) in ink.stripes.iter().enumerate() {
        let top = -HALF + 5.0 * k as f32;
        let bottom = if k + 1 == ink.stripes.len() {
            HALF
        } else {
            top + 5.0 + lap
        };
        ribbon.fill(&stripe(top, bottom, nose), *rgb, 1.0);
    }
    // The glow ends softly at the box's left and right edges rather than cut by them (done, the
    // nose stands 14 units short of the right edge and its glow reaches past it); below, the
    // image runs [`super::GLOW_BELOW`] units past the design box, where the glow has all but
    // faded, and its last few units fade out the rest.
    c.glow_fading(
        &ribbon,
        9.0,
        ink.glow,
        (0.0, 7.0),
        canvas::Fade {
            left: 18.0,
            right: 18.0,
            bottom: 6.0,
        },
    );
    c.draw(&ribbon);
    let mut line = Vec::new();
    let mut x = ROAD_X + 30.0;
    while x < nose - HALF - 2.0 {
        line.push(offset_point(x, -14.4, true));
        x += 3.0;
    }
    if line.len() > 1 {
        c.stroke(&Shape::polyline(&line), 1.1, (1.0, 1.0, 1.0), ink.highlight);
    }
    if road.done {
        for ((x, y), r, pale, alpha) in sparkles(road) {
            let rgb = if pale { ink.spark } else { ink.teal };
            c.fill(&Shape::star(x, y, r), rgb, alpha);
        }
    }
}

/// The ribbon between offsets `top` and `bottom` along its normal, from its point at
/// [`ROAD_X`] to its round nose, whose tip is at `nose`.
fn stripe(top: f32, bottom: f32, nose: f32) -> Shape {
    let end = |o: f32| nose - HALF + (HALF * HALF - o * o).max(0.0).sqrt();
    let edge = |o: f32| {
        let mut points = Vec::new();
        let mut x = ROAD_X;
        while x < end(o) {
            points.push(offset_point(x, o, true));
            x += 2.0;
        }
        points
    };
    let mut points = edge(top);
    // The nose: the arc from `top` to `bottom`, even steps of angle.
    let (a0, a1) = (
        (top / HALF).clamp(-1.0, 1.0).asin(),
        (bottom / HALF).clamp(-1.0, 1.0).asin(),
    );
    let steps = 24;
    for i in 0..=steps {
        let o = HALF * (a0 + (a1 - a0) * i as f32 / steps as f32).sin();
        points.push(offset_point(end(o), o, true));
    }
    points.extend(edge(bottom).into_iter().rev());
    Shape::polygon(&points)
}
