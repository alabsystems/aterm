// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The setup screen's hero (`docs/DESIGN-init-on-by-default-2026-10-25.md` §4.5): the brand's
//! words, then a six-stripe rainbow road that grows out of a point after them and rides a gently
//! undulating line, a tan tabby standing on it at the run's progress, and ahead of the road's
//! round nose six lanes of dots converging on a teal star. Done, the whole road is travelled and
//! the cat loafs at its far end among a few sparkles.
//!
//! Everything is drawn in the design's own units: a box 200 tall laid out for a 26 px mono face
//! (the words' baseline at 184), so a page draws it at `px / 26` points per unit. The road and
//! the cat are baked as two images, each for its ground at the exact device size it is drawn
//! at: [`RoadArt`] (the ribbon, its underglow and highlight, the dots and the star, the done
//! sparkles), keyed by the road's length, the run's progress and whether it is done, and
//! [`CatArt`] (a pose with its contact shadow), keyed by the pose and its lean in whole degrees.
//! So a pose change rebakes only the cat, unless it is to or from the loaf, which comes with the
//! done road; a step of progress rebakes the road, whose ribbon runs to just past the cat, and
//! the cat only where its lean there rounds to another degree.

mod canvas;
mod cat;
mod road;

use aterm_scene::Tile;

use canvas::Affine;

/// The mono size the design is laid out for.
pub const DESIGN_PX: f32 = 26.0;
/// The design box's height in units.
pub const DESIGN_H: f32 = 200.0;
/// How far below the design box the road's underglow reaches, in units: the ribbon's lowest
/// edge (about 184, where it rises out of the words) moved 7 lower and blurred by 9 fades to
/// nothing about 3 blurs below it, and the design's SVG filter lets it spill out of its box.
pub const GLOW_BELOW: f32 = 26.0;
/// The road image's height in units: the design box and the glow under it.
pub const ROAD_H: f32 = DESIGN_H + GLOW_BELOW;
/// The words' baseline, from the box's top.
pub const BASELINE: f32 = 184.0;
/// Where the words begin, from the box's left edge.
pub const WORDS_X: f32 = 30.0;
/// The prompt chevron's three points from `(0, baseline)`, and its stroke.
pub const CHEVRON: [(f32, f32); 3] = [(3.0, -17.5), (13.0, -8.75), (3.0, 0.0)];
pub const CHEVRON_STROKE: f32 = 3.4;
/// The gap between the words' end and where the road grows out of its point.
pub const ROAD_GAP: f32 = 17.0;
/// The design x the road starts at (the left edge of [`RoadArt`]'s box).
pub const ROAD_X: f32 = 238.0;
/// The least road, from its start to the box's right edge: room for the cat to travel.
pub const LEAST_LENGTH: f32 = 400.0;
/// The longest road: a wider page leaves the rest of its width empty. It bounds a bake: the
/// longest road at Mono 22 on a 2x display is 5,077 × 382 device pixels, 7.8 MB.
pub const MAX_LENGTH: f32 = 3000.0;
/// How far above the design box the cat reaches, in units: where the road crests near its far
/// end (its centreline at 140.5) the walking cat's ear tips stand about 2 units above the box,
/// so a lockup leaves this much room over it.
pub const HEADROOM: f32 = 4.0;
/// Half the ribbon's height, and its nose's radius.
const HALF: f32 = 15.0;
/// Where the cat stands at the run's start, and how far short of the box's right edge it ends.
const CAT_FROM: f32 = 330.0;
const CAT_SHORT: f32 = 90.0;
/// Where the dot lanes meet and the star stands, short of the box's right edge: the star's halo
/// (13 units, blurred about 2.5) ends inside the box.
const CONVERGE_SHORT: f32 = 34.0;
const STAR_SHORT: f32 = 22.0;
/// The star's radius.
const STAR_R: f32 = 10.5;
/// How far the nose's tip runs ahead of the cat's feet (past the walking cat's front paw by a
/// nose), and once the run is done (past the loaf's paws); never closer to the box's right edge
/// than [`NOSE_SHORT`] while the star stands there (a little short of its left point), nor
/// than [`DONE_NOSE_SHORT`] once it is gone.
const NOSE_AHEAD: f32 = 61.0;
const DONE_NOSE_AHEAD: f32 = 75.0;
const NOSE_SHORT: f32 = STAR_SHORT + STAR_R + 3.5;
const DONE_NOSE_SHORT: f32 = 14.0;
/// The cat's feet stand this far above the road's centreline.
const FEET_UP: f32 = 7.0;
/// The cat leans with the road by this share of its slope.
const LEAN: f32 = 0.7;
/// The cat's box (its own image), in units, and where its feet stand in it.
pub const CAT_W: f32 = 260.0;
pub const CAT_H: f32 = 175.0;
pub const CAT_FEET: (f32, f32) = (135.0, 150.0);

/// Smoothstep on `t` clamped to `0..=1`.
#[must_use]
pub fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The road's centreline at design `x`: a gentle swell, and a rise out of the words' baseline
/// over its first 140 units.
#[must_use]
pub fn centre_y(x: f32) -> f32 {
    151.0 + 10.5 * ((x - 360.0) / 125.0).sin() + 33.0 * (1.0 - smooth((x - ROAD_X) / 140.0))
}

/// The unit normal to the centreline at `x`, pointing down the stripes (red to violet).
fn normal(x: f32) -> (f32, f32) {
    let dy = centre_y(x + 0.5) - centre_y(x - 0.5);
    let len = 1.0_f32.hypot(dy);
    (-dy / len, 1.0 / len)
}

/// The point `o` units along the normal at `x`, the ribbon's width grown out of its point over
/// its first 130 units where `taper`.
fn offset_point(x: f32, o: f32, taper: bool) -> (f32, f32) {
    let t = if taper {
        smooth((x - ROAD_X) / 130.0)
    } else {
        1.0
    };
    let (nx, ny) = normal(x);
    (x + nx * o * t, centre_y(x) + ny * o * t)
}

/// The road at one moment of the run, in design units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Road {
    /// From [`ROAD_X`] to the box's right edge.
    pub length: f32,
    /// The run's progress, `0.0..=1.0`.
    pub progress: f32,
    /// The run is done: the whole road travelled, no dots ahead, sparkles along it.
    pub done: bool,
}

impl Road {
    /// The box's right edge.
    #[must_use]
    pub fn right(self) -> f32 {
        ROAD_X + self.length.clamp(LEAST_LENGTH, MAX_LENGTH)
    }

    fn progress(self) -> f32 {
        if self.done {
            1.0
        } else {
            self.progress.clamp(0.0, 1.0)
        }
    }

    /// The cat's feet's x.
    #[must_use]
    pub fn cat_x(self) -> f32 {
        let end = (self.right() - CAT_SHORT).max(CAT_FROM);
        CAT_FROM + self.progress() * (end - CAT_FROM)
    }

    /// Where the cat's feet stand.
    #[must_use]
    pub fn feet(self) -> (f32, f32) {
        let x = self.cat_x();
        (x, centre_y(x) - FEET_UP)
    }

    /// The cat's lean, in degrees (SVG's: clockwise), a share of the road's slope under it.
    #[must_use]
    pub fn tilt(self) -> f32 {
        let x = self.cat_x();
        let slope = (centre_y(x + 30.0) - centre_y(x - 30.0)) / 60.0;
        slope.atan().to_degrees() * LEAN
    }

    /// The nose's tip: a nose past the cat's front paw, short of the star, or done, of the
    /// box's edge.
    #[must_use]
    pub fn nose(self) -> f32 {
        let (ahead, short) = if self.done {
            (DONE_NOSE_AHEAD, DONE_NOSE_SHORT)
        } else {
            (NOSE_AHEAD, NOSE_SHORT)
        };
        (self.cat_x() + ahead).min(self.right() - short)
    }

    /// Where the dot lanes meet, and the star past it.
    fn converge_x(self) -> f32 {
        self.right() - CONVERGE_SHORT
    }

    fn star(self) -> (f32, f32) {
        (self.right() - STAR_SHORT, centre_y(self.converge_x()))
    }

    /// The dot lanes' dots, ahead of the nose (each lane begins under the nose's cap): their
    /// lane (0 red … 5 violet) and centre. None once the run is done.
    #[must_use]
    pub fn dots(self) -> Vec<(usize, f32, f32)> {
        let mut dots = Vec::new();
        if self.done {
            return dots;
        }
        let (from, to) = (self.nose() - 13.0, self.converge_x());
        for lane in 0..6 {
            let o = -12.5 + 5.0 * lane as f32;
            let mut points = Vec::new();
            let mut x = from;
            while x <= to {
                let t = smooth((to - x) / 150.0).powf(0.8);
                let (nx, ny) = normal(x);
                points.push((x + nx * o * t, centre_y(x) + ny * o * t));
                x += 3.0;
            }
            // A dot every 9.1 units of the lane: the design's `0.1 9` dash with round caps.
            let mut next = 0.0;
            let mut walked = 0.0;
            for pair in points.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let step = canvas::dist(a, b);
                while next <= walked + step {
                    let u = if step > 0.0 {
                        (next - walked) / step
                    } else {
                        0.0
                    };
                    dots.push((lane, a.0 + (b.0 - a.0) * u, a.1 + (b.1 - a.1) * u));
                    next += 9.1;
                }
                walked += step;
            }
        }
        dots
    }
}

/// The cat's poses, one for each moment of the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CatPose {
    /// A step is running: walking along the road.
    Walk,
    /// Between steps: sitting.
    Sit,
    /// The run needs the person: sitting, a paw raised, looking up.
    LookUp,
    /// A step failed: sitting, ears flattened back, worried.
    Worried,
    /// The run is done: a loaf, eyes closed.
    Loaf,
}

impl CatPose {
    /// Every pose, in the order a node value numbers them.
    pub const ALL: [Self; 5] = [
        Self::Walk,
        Self::Sit,
        Self::LookUp,
        Self::Worried,
        Self::Loaf,
    ];

    /// How far right of its feet the pose's front paw reaches, outline included, in units.
    #[must_use]
    pub fn front_paw(self) -> f32 {
        cat::front_paw(self)
    }
}

/// The road's image: the ribbon to the run's progress with its underglow and highlight, the dot
/// lanes and the star ahead, or the done sparkles; its box runs from [`ROAD_X`] for `length`
/// units and spans the design's whole height and [`GLOW_BELOW`] units under it ([`ROAD_H`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RoadArt {
    /// From [`ROAD_X`] to the box's right edge, whole units.
    pub length: u16,
    /// The run's progress in thousandths.
    pub permille: u16,
    pub done: bool,
    /// Drawn on a dark ground, else a light one.
    pub dark: bool,
}

impl RoadArt {
    /// The road this art draws.
    #[must_use]
    pub fn road(self) -> Road {
        Road {
            length: f32::from(self.length),
            progress: f32::from(self.permille.min(1000)) / 1000.0,
            done: self.done,
        }
    }

    /// Its box's width ÷ height: the road's length, at least [`LEAST_LENGTH`] and at most
    /// [`MAX_LENGTH`], over [`ROAD_H`].
    #[must_use]
    pub fn aspect(self) -> f32 {
        (self.road().right() - ROAD_X) / ROAD_H
    }

    /// Straight-alpha RGBA8 at exactly `w`×`h` device pixels: `h / ROAD_H` pixels a unit, the
    /// design box's top at its top.
    #[must_use]
    pub fn bake(self, w: u32, h: u32) -> Tile {
        let unit = h as f32 / ROAD_H;
        let mut canvas = canvas::Canvas::new(
            w,
            h,
            Affine::scale(unit).then(Affine::translate(-ROAD_X, 0.0)),
        );
        road::paint(&mut canvas, self.road(), &Ink::of(self.dark));
        canvas.tile
    }
}

/// The cat's image: a pose leaning `tilt` degrees with its contact shadow (and, loafing, its
/// sparkles), its feet at [`CAT_FEET`] in a [`CAT_W`]×[`CAT_H`] box.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CatArt {
    pub pose: CatPose,
    /// The lean, whole degrees (SVG's: clockwise).
    pub tilt: i8,
    pub dark: bool,
}

impl CatArt {
    /// Its box's width ÷ height.
    pub const ASPECT: f32 = CAT_W / CAT_H;

    /// The cat standing on `road` in `pose`.
    #[must_use]
    pub fn on(road: Road, pose: CatPose, dark: bool) -> Self {
        Self {
            pose,
            tilt: road.tilt().round().clamp(-45.0, 45.0) as i8,
            dark,
        }
    }

    /// Straight-alpha RGBA8 at exactly `w`×`h` device pixels: `h / 175` pixels a unit.
    #[must_use]
    pub fn bake(self, w: u32, h: u32) -> Tile {
        let unit = h as f32 / CAT_H;
        let mut canvas = canvas::Canvas::new(
            w,
            h,
            Affine::scale(unit).then(Affine::translate(CAT_FEET.0, CAT_FEET.1)),
        );
        cat::paint(
            &mut canvas,
            self.pose,
            f32::from(self.tilt),
            &Ink::of(self.dark),
        );
        canvas.tile
    }
}

/// The colours a ground draws in.
struct Ink {
    stripes: [canvas::Rgb; 6],
    /// The dot lanes' strength.
    dots: f32,
    /// The underglow's strength.
    glow: f32,
    /// The top edge's white line's strength.
    highlight: f32,
    teal: canvas::Rgb,
    /// The pale sparkles' colour.
    spark: canvas::Rgb,
    /// The cat's contact shadow's strength.
    shadow: f32,
}

impl Ink {
    fn of(dark: bool) -> Self {
        if dark {
            Self {
                stripes: [0xF2384A, 0xFF8A2B, 0xFFD23F, 0x2FCB71, 0x2F8BFF, 0x9B5CF6]
                    .map(canvas::hex),
                dots: 0.62,
                glow: 0.6,
                highlight: 0.35,
                teal: canvas::hex(0x5EEAD4),
                spark: canvas::hex(0xFFF1C2),
                shadow: 0.4,
            }
        } else {
            Self {
                stripes: [0xE3141B, 0xFF7A00, 0xFFC20E, 0x22B848, 0x1060D8, 0x8226C0]
                    .map(canvas::hex),
                dots: 0.8,
                glow: 0.22,
                highlight: 0.28,
                teal: canvas::hex(0x0D9488),
                spark: canvas::hex(0xE0A21A),
                shadow: 0.2,
            }
        }
    }
}

#[cfg(test)]
#[path = "rainbow_road_tests.rs"]
mod tests;
