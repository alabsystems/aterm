// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cat: an original tan tabby drawn as flat shapes with a dark warm outline, its feet at
//! the origin, in units of the design's box. Each group of shapes draws its outline first (each
//! silhouette shape grown by [`OW`] in the outline colour), then its shapes; detail (stripes,
//! the bib, the face's features) draws without one.

use super::canvas::{Affine, Canvas, Rgb, Shape, hex};
use super::{CatPose, Ink};

/// How far the outline reaches past each silhouette shape.
const OW: f32 = 2.6;

/// The head's ellipse: its radii about the face's centre.
pub(super) const HEAD: (f32, f32) = (32.0, 27.5);

/// Where a laid-back ear's base stands on the skull: two angles of the head's ellipse (degrees,
/// clockwise from the right as y runs down; the left ear's, the right one mirrored), this share
/// of the way out, so the base lies inside the head, which covers it.
const EAR_BASE: [f32; 2] = [198.0, 238.0];
const EAR_BASE_IN: f32 = 0.92;
/// A laid-back ear's tip: this far out from the middle of its base, pointing this way (degrees,
/// as above: out to the side and a touch up), its corner rounded over this many units.
const EAR_REACH: f32 = 22.0;
const EAR_POINT: f32 = 190.0;
const EAR_ROUND: f32 = 4.0;

/// A worried cat's ear on `side` (-1 left, 1 right) of the head at `(cx, cy)`: the ear, and its
/// pink inside.
pub(super) fn laid_back_ear(cx: f32, cy: f32, side: f32) -> (Shape, Shape) {
    let on_skull = |deg: f32| {
        let (sin, cos) = deg.to_radians().sin_cos();
        (
            cx - side * cos * HEAD.0 * EAR_BASE_IN,
            cy + sin * HEAD.1 * EAR_BASE_IN,
        )
    };
    let (low, high) = (on_skull(EAR_BASE[0]), on_skull(EAR_BASE[1]));
    let mid = ((low.0 + high.0) / 2.0, (low.1 + high.1) / 2.0);
    let (sin, cos) = EAR_POINT.to_radians().sin_cos();
    let tip = (mid.0 - side * cos * EAR_REACH, mid.1 + sin * EAR_REACH);
    let toward = |from: (f32, f32), to: (f32, f32), by: f32| {
        let d = super::canvas::dist(from, to).max(1e-3);
        (
            from.0 + (to.0 - from.0) * by / d,
            from.1 + (to.1 - from.1) * by / d,
        )
    };
    let (a, b) = (toward(tip, low, EAR_ROUND), toward(tip, high, EAR_ROUND));
    let ear = Shape::new()
        .m(low.0, low.1)
        .l(a.0, a.1)
        .q(tip.0, tip.1, b.0, b.1)
        .l(high.0, high.1)
        .z();
    let centre = (
        (low.0 + high.0 + tip.0) / 3.0,
        (low.1 + high.1 + tip.1) / 3.0,
    );
    let inner = |p: (f32, f32), share: f32| {
        (
            p.0 + (centre.0 - p.0) * share,
            p.1 + (centre.1 - p.1) * share,
        )
    };
    let pink = Shape::polygon(&[inner(low, 0.4), inner(tip, 0.45), inner(high, 0.4)]);
    (ear, pink)
}

/// The coat and its details.
struct Coat {
    coat: Rgb,
    /// The far legs, a shade under the coat.
    shade: Rgb,
    stripe: Rgb,
    cream: Rgb,
    bib: Rgb,
    pink: Rgb,
    nose: Rgb,
    blush: Rgb,
    eye: Rgb,
    iris: Rgb,
    outline: Rgb,
    white: Rgb,
    drop: Rgb,
}

impl Coat {
    fn new() -> Self {
        Self {
            coat: hex(0xEEC27F),
            shade: hex(0xD7A566),
            stripe: hex(0xB57E4A),
            cream: hex(0xFBF0E4),
            bib: hex(0xF8E2BC),
            pink: hex(0xF2A3B0),
            nose: hex(0xD8718A),
            blush: hex(0xF6A9B5),
            eye: hex(0x2A2230),
            iris: hex(0x7FB89A),
            outline: hex(0x34232C),
            white: (1.0, 1.0, 1.0),
            drop: hex(0x9AD8FF),
        }
    }
}

/// One shape of a group: filled, or stroked `width` wide; with an outline (`sil`) or without.
struct Item {
    shape: Shape,
    rgb: Rgb,
    alpha: f32,
    width: Option<f32>,
    sil: bool,
}

#[derive(Default)]
struct Group(Vec<Item>);

impl Group {
    fn push(&mut self, shape: Shape, rgb: Rgb, alpha: f32, width: Option<f32>, sil: bool) {
        self.0.push(Item {
            shape,
            rgb,
            alpha,
            width,
            sil,
        });
    }

    /// A silhouette fill.
    fn p(&mut self, shape: Shape, rgb: Rgb) {
        self.push(shape, rgb, 1.0, None, true);
    }

    /// A detail fill.
    fn pi(&mut self, shape: Shape, rgb: Rgb) {
        self.push(shape, rgb, 1.0, None, false);
    }

    /// A silhouette stroke.
    fn l(&mut self, shape: Shape, rgb: Rgb, width: f32) {
        self.push(shape, rgb, 1.0, Some(width), true);
    }

    /// A detail stroke at `alpha`.
    fn la(&mut self, shape: Shape, rgb: Rgb, width: f32, alpha: f32) {
        self.push(shape, rgb, alpha, Some(width), false);
    }

    /// A detail stroke.
    fn li(&mut self, shape: Shape, rgb: Rgb, width: f32) {
        self.la(shape, rgb, width, 1.0);
    }

    /// A silhouette ellipse.
    fn e(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, rgb: Rgb) {
        self.p(Shape::ellipse(cx, cy, rx, ry), rgb);
    }

    /// A detail ellipse at `alpha`.
    fn ea(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, rgb: Rgb, alpha: f32) {
        self.push(Shape::ellipse(cx, cy, rx, ry), rgb, alpha, None, false);
    }

    /// A detail ellipse.
    fn ei(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, rgb: Rgb) {
        self.ea(cx, cy, rx, ry, rgb, 1.0);
    }

    /// The group: its silhouettes' outline in `outline` first where given, then its shapes.
    fn draw(&self, c: &mut Canvas, outline: Option<Rgb>) {
        if let Some(o) = outline {
            for item in self.0.iter().filter(|item| item.sil) {
                match item.width {
                    None => {
                        c.fill(&item.shape, o, 1.0);
                        c.stroke(&item.shape, 2.0 * OW, o, 1.0);
                    }
                    Some(width) => c.stroke(&item.shape, width + 2.0 * OW, o, 1.0),
                }
            }
        }
        for item in &self.0 {
            match item.width {
                None => c.fill(&item.shape, item.rgb, item.alpha),
                Some(width) => c.stroke(&item.shape, width, item.rgb, item.alpha),
            }
        }
    }
}

/// How large each pose is drawn: the sitting cat a touch smaller, as it stands taller.
fn scale(pose: CatPose) -> f32 {
    match pose {
        CatPose::Walk | CatPose::Loaf => 1.0,
        CatPose::Sit | CatPose::LookUp | CatPose::Worried => 0.95,
    }
}

/// How far right of its feet the pose's front paw reaches, outline included, at its scale.
pub(super) fn front_paw(pose: CatPose) -> f32 {
    let reach = match pose {
        CatPose::Walk => 27.0 + 10.0,
        CatPose::Sit | CatPose::LookUp | CatPose::Worried => 24.0 + 11.0,
        CatPose::Loaf => 46.0 + 11.0,
    };
    (reach + OW) * scale(pose)
}

/// The cat in `pose`, leaning `tilt` degrees, over its contact shadow; loafing, with its
/// sparkles about it.
pub(super) fn paint(c: &mut Canvas, pose: CatPose, tilt: f32, ink: &Ink) {
    let k = Coat::new();
    let s = scale(pose);
    let mut shadow = c.layer();
    shadow.fill(
        &Shape::ellipse(0.0, 1.0, 62.0 * s, 5.0),
        (0.0, 0.0, 0.0),
        1.0,
    );
    c.glow(&shadow.tile, 2.5, ink.shadow, (0.0, 0.0));
    c.with(
        Affine::rotate(tilt).then(Affine::scale(s)),
        |c| match pose {
            CatPose::Walk => walk(c, &k),
            CatPose::Sit => sit(c, &k, Sitting::Plain),
            CatPose::LookUp => sit(c, &k, Sitting::Waving),
            CatPose::Worried => sit(c, &k, Sitting::Worried),
            CatPose::Loaf => loaf(c, &k),
        },
    );
    if pose == CatPose::Loaf {
        for (x, y, r, pale, alpha) in [
            (-6.0, -104.0, 11.0, false, 1.0),
            (-34.0, -82.0, 5.5, true, 1.0),
            (12.0, -126.0, 4.5, true, 0.9),
            (-62.0, -66.0, 3.5, false, 0.7),
        ] {
            let rgb = if pale { ink.spark } else { ink.teal };
            c.fill(&Shape::star(x, y, r), rgb, alpha);
        }
    }
}

/// A tail along `path`, 11 wide and outlined, banded with stripes from a plain base to a plain
/// tip.
fn tail(c: &mut Canvas, path: Shape, k: &Coat) {
    const W: f32 = 11.0;
    const BASE: f32 = 16.0;
    const TIP: f32 = 11.0;
    const BAND: f32 = 4.5;
    const GAP: f32 = 8.5;
    let mut g = Group::default();
    g.l(path.clone(), k.coat, W);
    g.draw(c, Some(k.outline));
    let line: Vec<(f32, f32)> = path.flatten(|_| 40).into_iter().flatten().collect();
    let mut at = vec![0.0_f32];
    for pair in line.windows(2) {
        let last = at[at.len() - 1];
        at.push(last + super::canvas::dist(pair[0], pair[1]));
    }
    let length = at[at.len() - 1];
    let bands = ((length - BASE - TIP + GAP) / (BAND + GAP))
        .floor()
        .max(0.0);
    let lead = length - TIP - (bands * (BAND + GAP) - GAP);
    let point = |s: f32| {
        let i = at.partition_point(|&a| a < s).clamp(1, line.len() - 1);
        let (a, b) = (line[i - 1], line[i]);
        let span = (at[i] - at[i - 1]).max(1e-6);
        let u = ((s - at[i - 1]) / span).clamp(0.0, 1.0);
        (a.0 + (b.0 - a.0) * u, a.1 + (b.1 - a.1) * u)
    };
    for band in 0..bands as usize {
        let from = lead + band as f32 * (BAND + GAP);
        let to = from + BAND;
        let mut points = vec![point(from)];
        points.extend(
            at.iter()
                .zip(&line)
                .filter(|(a, _)| **a > from && **a < to)
                .map(|(_, p)| *p),
        );
        points.push(point(to));
        c.band(&points, W, k.stripe, 1.0);
    }
}

/// A head centred on `(cx, cy)`, its features `turn` right of centre (a three-quarter view),
/// its eyes looking `look`, closed or open (`eye` their radii), `tilt` degrees about its chin;
/// worried, its ears flattened back, brows raised in the middle, a small frown and a bead of
/// sweat.
struct Face {
    cx: f32,
    cy: f32,
    turn: f32,
    look: (f32, f32),
    closed: bool,
    tilt: f32,
    eye: (f32, f32),
    worried: bool,
}

impl Face {
    const fn at(cx: f32, cy: f32) -> Self {
        Self {
            cx,
            cy,
            turn: 0.0,
            look: (0.0, 0.0),
            closed: false,
            tilt: 0.0,
            eye: (7.0, 8.0),
            worried: false,
        }
    }
}

fn face(c: &mut Canvas, f: &Face, k: &Coat) {
    c.with(Affine::rotate_about(f.tilt, f.cx, f.cy + 10.0), |c| {
        face_upright(c, f, k);
    });
}

fn face_upright(c: &mut Canvas, f: &Face, k: &Coat) {
    let (cx, cy) = (f.cx, f.cy);
    let mut g = Group::default();
    let mut h = Group::default();
    if f.worried {
        // Ears laid back: each a flat triangle pointing out sideways, its base on the skull
        // (inside the head's ellipse, which covers it), so no notch shows where it joins.
        for side in [-1.0, 1.0] {
            let (ear, pink) = laid_back_ear(cx, cy, side);
            g.p(ear, k.coat);
            h.pi(pink, k.pink);
        }
    } else {
        g.p(
            Shape::new()
                .m(cx - 27.0, cy - 10.0)
                .l(cx - 29.0, cy - 41.0)
                .q(cx - 29.0, cy - 45.0, cx - 25.0, cy - 43.0)
                .l(cx - 3.0, cy - 25.0)
                .z(),
            k.coat,
        );
        g.p(
            Shape::new()
                .m(cx + 5.0, cy - 25.0)
                .l(cx + 26.0, cy - 43.0)
                .q(cx + 30.0, cy - 45.0, cx + 30.0, cy - 41.0)
                .l(cx + 28.0, cy - 10.0)
                .z(),
            k.coat,
        );
        h.pi(
            Shape::polygon(&[
                (cx - 23.0, cy - 18.0),
                (cx - 25.0, cy - 36.0),
                (cx - 11.0, cy - 25.0),
            ]),
            k.pink,
        );
        h.pi(
            Shape::polygon(&[
                (cx + 12.0, cy - 25.0),
                (cx + 26.0, cy - 36.0),
                (cx + 24.0, cy - 18.0),
            ]),
            k.pink,
        );
    }
    g.e(cx, cy, HEAD.0, HEAD.1, k.coat);
    g.draw(c, Some(k.outline));

    for dx in [-7.0, 0.0, 7.0] {
        h.li(
            Shape::new()
                .m(cx + f.turn * 0.6 + dx, cy - 24.5)
                .rl(dx * 0.08, 7.0),
            k.stripe,
            3.2,
        );
    }
    let fx = cx + f.turn;
    let (ex1, ex2) = (fx - 13.5, fx + 13.5);
    let ey = cy - 3.0;
    let (rx, ry) = f.eye;
    h.ei(fx, cy + 9.5, 13.0, 9.0, k.cream);
    if f.closed {
        for ex in [ex1, ex2] {
            h.li(
                Shape::new()
                    .m(ex - 6.0, ey + 1.0)
                    .q(ex, ey - 6.0, ex + 6.0, ey + 1.0),
                k.eye,
                2.4,
            );
        }
    } else {
        let far = if f.turn == 0.0 { 1.0 } else { 0.86 };
        let (lx, ly) = f.look;
        for (ex, s) in [(ex1, 1.0), (ex2, far)] {
            h.ei(ex, ey, rx * s, ry, k.eye);
            h.ei(
                ex + lx * 0.5,
                ey + ly * 0.5,
                (rx - 1.6) * s,
                ry - 1.6,
                k.iris,
            );
            h.ei(ex + lx, ey + ly, (rx - 3.6) * s, ry - 3.0, k.eye);
            h.ei(ex + lx + 2.2, ey + ly - 3.0, 2.1, 2.1, k.white);
            h.ea(ex + lx - 2.0, ey + ly + 2.6, 1.0, 1.0, k.white, 0.8);
        }
    }
    if f.worried {
        // Brows raised where they meet.
        for (ex, inward) in [(ex1, 1.0), (ex2, -1.0)] {
            h.li(
                Shape::new()
                    .m(ex - 5.5 * inward, ey - 10.0)
                    .l(ex + 4.5 * inward, ey - 13.5),
                k.eye,
                1.8,
            );
        }
    }
    let (nx, ny) = (fx, cy + 5.0);
    h.pi(
        Shape::new()
            .m(nx - 3.6, ny - 1.6)
            .q(nx, ny - 3.0, nx + 3.6, ny - 1.6)
            .q(nx + 3.0, ny + 2.4, nx, ny + 3.2)
            .q(nx - 3.0, ny + 2.4, nx - 3.6, ny - 1.6)
            .z(),
        k.nose,
    );
    let mouth = if f.worried {
        Shape::new()
            .m(nx, ny + 3.2)
            .l(nx, ny + 5.2)
            .m(nx - 4.6, ny + 9.2)
            .q(nx, ny + 5.4, nx + 4.6, ny + 9.2)
    } else {
        Shape::new()
            .m(nx, ny + 3.2)
            .l(nx, ny + 5.6)
            .m(nx - 5.6, ny + 6.0)
            .q(nx - 2.6, ny + 9.0, nx, ny + 5.6)
            .q(nx + 2.6, ny + 9.0, nx + 5.6, ny + 6.0)
    };
    h.li(mouth, k.eye, 1.5);
    h.ea(fx - 22.0, cy + 9.0, 5.0, 3.0, k.blush, 0.85);
    h.ea(
        fx + 22.0,
        cy + 9.0,
        if f.turn == 0.0 { 5.0 } else { 4.0 },
        3.0,
        k.blush,
        0.85,
    );
    for sgn in [-1.0, 1.0] {
        let x0 = fx + sgn * 17.0;
        h.la(
            Shape::new().m(x0, cy + 8.0).rl(sgn * 17.0, -3.0),
            k.cream,
            1.1,
            0.8,
        );
        h.la(
            Shape::new().m(x0, cy + 11.0).rl(sgn * 16.0, 3.0),
            k.cream,
            1.1,
            0.8,
        );
    }
    h.draw(c, None);
    if f.worried {
        // A bead of sweat at the temple, above the laid-back ear.
        let (x, y) = (cx + 34.0, cy - 30.0);
        let drop = Shape::new()
            .m(x, y - 5.5)
            .c(x + 1.2, y - 3.2, x + 3.4, y - 1.0, x + 3.4, y + 1.4)
            .c(x + 3.4, y + 3.4, x + 1.9, y + 4.9, x, y + 4.9)
            .c(x - 1.9, y + 4.9, x - 3.4, y + 3.4, x - 3.4, y + 1.4)
            .c(x - 3.4, y - 1.0, x - 1.2, y - 3.2, x, y - 5.5)
            .z();
        c.stroke(&drop, 2.4, k.outline, 1.0);
        c.fill(&drop, k.drop, 1.0);
        c.fill(&Shape::ellipse(x - 1.2, y + 1.6, 0.9, 1.3), k.white, 0.9);
    }
}

fn walk(c: &mut Canvas, k: &Coat) {
    tail(
        c,
        Shape::new()
            .m(-50.0, -62.0)
            .c(-72.0, -62.0, -88.0, -76.0, -85.0, -97.0)
            .c(-83.0, -110.0, -70.0, -116.0, -64.0, -106.0),
        k,
    );
    let mut far = Group::default();
    far.l(
        Shape::new()
            .m(-30.0, -42.0)
            .c(-27.0, -30.0, -21.0, -20.0, -17.0, -10.0),
        k.shade,
        13.0,
    );
    far.e(-14.0, -6.5, 9.0, 6.0, k.shade);
    far.l(
        Shape::new()
            .m(18.0, -44.0)
            .c(24.0, -37.0, 31.0, -31.0, 39.0, -27.0),
        k.shade,
        13.0,
    );
    far.p(
        Shape::ellipse(42.0, -25.0, 8.0, 6.5).mapped(Affine::rotate_about(-22.0, 42.0, -25.0)),
        k.shade,
    );
    far.draw(c, Some(k.outline));
    let mut b = Group::default();
    b.p(
        Shape::new()
            .m(-58.0, -54.0)
            .c(-59.0, -70.0, -46.0, -79.0, -26.0, -79.0)
            .c(-10.0, -79.0, 4.0, -77.0, 18.0, -78.0)
            .c(32.0, -79.0, 40.0, -68.0, 40.0, -56.0)
            .c(40.0, -44.0, 32.0, -34.0, 16.0, -32.0)
            .c(4.0, -30.5, -14.0, -33.0, -30.0, -32.0)
            .c(-46.0, -31.0, -57.0, -40.0, -58.0, -54.0)
            .z(),
        k.coat,
    );
    b.e(-37.0, -46.0, 16.0, 16.0, k.coat);
    b.l(
        Shape::new()
            .m(-43.0, -40.0)
            .c(-47.0, -28.0, -53.0, -18.0, -56.0, -10.0),
        k.coat,
        14.0,
    );
    b.e(-54.0, -6.5, 10.0, 6.5, k.coat);
    b.l(
        Shape::new()
            .m(22.0, -44.0)
            .c(22.0, -32.0, 23.0, -20.0, 24.0, -10.0),
        k.coat,
        15.0,
    );
    b.e(27.0, -6.5, 10.0, 6.5, k.coat);
    b.draw(c, Some(k.outline));
    let mut d = Group::default();
    for x in [-30.0, -15.0, 0.0] {
        d.li(
            Shape::new().m(x, -75.5).rq(3.0, 8.0, 0.0, 16.0),
            k.stripe,
            5.0,
        );
    }
    d.la(
        Shape::new()
            .m(-26.0, -38.0)
            .c(-22.0, -44.0, -22.0, -54.0, -28.0, -61.0),
        k.outline,
        1.8,
        0.55,
    );
    d.draw(c, None);
    face(
        c,
        &Face {
            turn: 6.0,
            look: (1.0, 0.0),
            ..Face::at(46.0, -88.0)
        },
        k,
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sitting {
    /// Both front paws down, looking up a little.
    Plain,
    /// A paw raised to the person, looking up.
    Waving,
    /// Both paws down, ears back, worried.
    Worried,
}

fn sit(c: &mut Canvas, k: &Coat, how: Sitting) {
    let wave = how == Sitting::Waving;
    tail(
        c,
        Shape::new()
            .m(24.0, -9.0)
            .c(48.0, -3.0, 68.0, -8.0, 66.0, -27.0)
            .c(65.0, -38.0, 54.0, -41.0, 51.0, -33.0),
        k,
    );
    let mut b = Group::default();
    b.e(-23.0, -17.0, 16.0, 17.0, k.coat);
    b.e(23.0, -17.0, 16.0, 17.0, k.coat);
    b.p(
        Shape::new()
            .m(0.0, -72.0)
            .c(18.0, -72.0, 26.0, -52.0, 29.0, -34.0)
            .c(32.0, -18.0, 30.0, -4.0, 18.0, -2.0)
            .l(-18.0, -2.0)
            .c(-30.0, -4.0, -32.0, -18.0, -29.0, -34.0)
            .c(-26.0, -52.0, -18.0, -72.0, 0.0, -72.0)
            .z(),
        k.coat,
    );
    b.e(-24.0, -5.0, 11.0, 6.0, k.coat);
    b.e(24.0, -5.0, 11.0, 6.0, k.coat);
    if wave {
        b.l(
            Shape::new()
                .m(14.0, -44.0)
                .c(27.0, -44.0, 36.0, -51.0, 38.0, -62.0),
            k.coat,
            12.0,
        );
        b.e(39.0, -67.5, 8.5, 8.0, k.coat);
    }
    b.draw(c, Some(k.outline));
    let mut chest = Group::default();
    chest.pi(
        Shape::new()
            .m(-12.0, -64.0)
            .q(0.0, -56.0, 12.0, -64.0)
            .q(13.0, -45.0, 0.0, -38.0)
            .q(-13.0, -45.0, -12.0, -64.0)
            .z(),
        k.bib,
    );
    chest.li(Shape::new().m(-15.5, -27.0).l(-15.5, -10.0), k.outline, 2.0);
    if wave {
        chest.li(Shape::new().m(1.0, -30.0).l(1.0, -10.0), k.outline, 2.0);
    } else {
        chest.li(Shape::new().m(0.0, -30.0).l(0.0, -10.0), k.outline, 2.0);
        chest.li(Shape::new().m(15.5, -27.0).l(15.5, -10.0), k.outline, 2.0);
    }
    chest.draw(c, None);
    let mut paws = Group::default();
    paws.e(-8.0, -5.5, 9.0, 5.5, k.coat);
    if !wave {
        paws.e(8.0, -5.5, 9.0, 5.5, k.coat);
    }
    paws.draw(c, Some(k.outline));
    let worried = how == Sitting::Worried;
    face(
        c,
        &Face {
            look: if worried { (0.0, -1.2) } else { (0.6, -1.6) },
            tilt: if worried { 5.0 } else { -7.0 },
            eye: (8.0, 9.0),
            worried,
            ..Face::at(0.0, -92.0)
        },
        k,
    );
    if wave {
        let mut pad = Group::default();
        pad.la(
            Shape::new()
                .m(27.0, -49.0)
                .c(30.0, -52.0, 32.0, -56.0, 32.5, -60.0),
            k.outline,
            1.8,
            0.55,
        );
        pad.ei(39.0, -65.5, 3.2, 2.6, k.pink);
        for (dx, dy) in [(-4.2, -4.6), (0.0, -6.4), (4.2, -4.6)] {
            pad.ei(39.0 + dx, -65.5 + dy, 1.5, 1.5, k.pink);
        }
        pad.draw(c, None);
    }
}

fn loaf(c: &mut Canvas, k: &Coat) {
    tail(
        c,
        Shape::new()
            .m(-56.0, -8.0)
            .c(-74.0, -6.0, -90.0, -8.0, -98.0, -18.0)
            .c(-101.0, -23.0, -99.0, -29.0, -94.0, -28.0),
        k,
    );
    let mut b = Group::default();
    b.p(
        Shape::new()
            .m(-60.0, -2.0)
            .c(-70.0, -2.0, -72.0, -14.0, -68.0, -28.0)
            .c(-62.0, -50.0, -40.0, -60.0, -10.0, -60.0)
            .c(22.0, -60.0, 46.0, -50.0, 53.0, -30.0)
            .c(57.0, -16.0, 54.0, -2.0, 44.0, -2.0)
            .z(),
        k.coat,
    );
    b.e(24.0, -5.0, 11.0, 6.0, k.coat);
    b.e(46.0, -5.0, 11.0, 6.0, k.coat);
    b.draw(c, Some(k.outline));
    let mut d = Group::default();
    for x in [-40.0, -24.0, -8.0] {
        d.li(
            Shape::new().m(x, -56.0).rq(3.0, 8.0, 0.0, 16.0),
            k.stripe,
            5.0,
        );
    }
    d.la(
        Shape::new()
            .m(-48.0, -5.0)
            .c(-34.0, -8.0, -26.0, -22.0, -32.0, -36.0),
        k.outline,
        1.8,
        0.5,
    );
    d.draw(c, None);
    face(
        c,
        &Face {
            turn: 5.0,
            closed: true,
            tilt: 4.0,
            ..Face::at(40.0, -54.0)
        },
        k,
    );
}
