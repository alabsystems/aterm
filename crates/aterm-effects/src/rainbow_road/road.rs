// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The road: the dot lanes and the star ahead (under everything), the ribbon's underglow, the
//! ribbon, the white line along its top edge, and, done, the sparkles along it.

use super::canvas::{self, Canvas, Shape};
use super::{HALF, Ink, ROAD_X, Road, STAR_R, centre_y, offset_point};

/// Where along the road (its share from the start to the box's edge) the done sparkles stand,
/// how high above the ribbon, their radius, whether pale (else teal), and their strength.
const ROAD_SPARKLES: [(f32, f32, f32, bool, f32); 4] = [
    (0.4015, 20.0, 3.0, true, 0.55),
    (0.5362, 30.0, 3.8, false, 0.7),
    (0.6559, 24.0, 4.6, true, 0.85),
    (0.7481, 44.0, 5.6, false, 0.9),
];

pub(super) fn paint(c: &mut Canvas, road: Road, ink: &Ink) {
    let nose = road.nose();
    if !road.done {
        for (lane, x, y) in road.dots() {
            c.dot(x, y, 1.15, ink.stripes[lane], ink.dots);
        }
        let (sx, sy) = road.star();
        let mut halo = c.layer();
        halo.dot(sx, sy, 13.0, ink.teal, 1.0);
        c.glow(&halo.tile, 2.5, 0.22, (0.0, 0.0));
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
        &ribbon.tile,
        9.0,
        ink.glow,
        (0.0, 7.0),
        canvas::Fade {
            left: 18.0,
            right: 18.0,
            bottom: 6.0,
        },
    );
    c.draw(&ribbon.tile);
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
        for (share, up, r, pale, alpha) in ROAD_SPARKLES {
            let x = ROAD_X + share * (road.right() - ROAD_X);
            let rgb = if pale { ink.spark } else { ink.teal };
            c.fill(&Shape::star(x, centre_y(x) - HALF - up, r), rgb, alpha);
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
