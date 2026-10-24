// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`ScaledSink`] and [`PointSink`] contour reconstruction.

use aterm_font::{Face, GlyphId, OutlineSink, PointKind, PointSink, ScaledSink, Tag};

/// One outline segment, as a sink receives it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Seg {
    M(f32, f32),
    L(f32, f32),
    Q(f32, f32, f32, f32),
    C(f32, f32, f32, f32, f32, f32),
    Z,
}

impl Seg {
    fn bits(self) -> Vec<u32> {
        let v: Vec<f32> = match self {
            Seg::M(x, y) | Seg::L(x, y) => vec![0.0, x, y],
            Seg::Q(a, b, x, y) => vec![1.0, a, b, x, y],
            Seg::C(a, b, c, d, x, y) => vec![2.0, a, b, c, d, x, y],
            Seg::Z => vec![3.0],
        };
        v.into_iter().map(f32::to_bits).collect()
    }
}

#[derive(Default)]
struct Segs(Vec<Seg>);

impl OutlineSink for Segs {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(Seg::M(x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(Seg::L(x, y));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.push(Seg::Q(x1, y1, x, y));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.push(Seg::C(x1, y1, x2, y2, x, y));
    }
    fn close(&mut self) {
        self.0.push(Seg::Z);
    }
}

fn same_bits(a: &[Seg], b: &[Seg]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.bits() == y.bits())
}

#[derive(Default)]
struct Points(Vec<Vec<(f32, f32, PointKind)>>, Vec<(f32, f32, PointKind)>);

impl PointSink for Points {
    fn point(&mut self, x: f32, y: f32, kind: PointKind) {
        self.1.push((x, y, kind));
    }
    fn end_contour(&mut self) {
        self.0.push(std::mem::take(&mut self.1));
    }
}

fn face_bytes(rel: &str) -> Vec<u8> {
    let path = format!("{}/../aterm-render/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("committed face {path}: {e}"))
}

fn truetype_segments(contours: &[Vec<(f32, f32, PointKind)>]) -> (Vec<Seg>, Vec<bool>) {
    let mid = |a: (f32, f32), b: (f32, f32)| (a.0 + 0.5 * (b.0 - a.0), a.1 + 0.5 * (b.1 - a.1));
    let mut out = Vec::new();
    let mut implied = Vec::new();
    let mut first_implied = false;
    for contour in contours {
        let (mut first_on, mut first_off, mut last_off) = (None, None, None);
        for &(x, y, kind) in contour {
            let p = (x, y);
            let on = kind == PointKind::OnCurve;
            match first_on {
                None if on => {
                    first_on = Some(p);
                    first_implied = false;
                    out.push(Seg::M(x, y));
                    implied.push(false);
                }
                None => match first_off {
                    Some(f) => {
                        let m = mid(f, p);
                        first_on = Some(m);
                        first_implied = true;
                        last_off = Some(p);
                        out.push(Seg::M(m.0, m.1));
                        implied.push(true);
                    }
                    None => first_off = Some(p),
                },
                Some(_) => match (last_off, on) {
                    (Some(o), true) => {
                        last_off = None;
                        out.push(Seg::Q(o.0, o.1, x, y));
                        implied.push(false);
                    }
                    (Some(o), false) => {
                        last_off = Some(p);
                        let m = mid(o, p);
                        out.push(Seg::Q(o.0, o.1, m.0, m.1));
                        implied.push(true);
                    }
                    (None, true) => {
                        out.push(Seg::L(x, y));
                        implied.push(false);
                    }
                    (None, false) => last_off = Some(p),
                },
            }
        }
        if let (Some(o1), Some(o2)) = (first_off, last_off) {
            last_off = None;
            let m = mid(o2, o1);
            out.push(Seg::Q(o2.0, o2.1, m.0, m.1));
            implied.push(true);
        }
        match (first_on, first_off, last_off) {
            (Some(p), Some(o), _) | (Some(p), None, Some(o)) => {
                out.push(Seg::Q(o.0, o.1, p.0, p.1));
                implied.push(first_implied);
            }
            (Some(p), None, None) => {
                out.push(Seg::L(p.0, p.1));
                implied.push(first_implied);
            }
            _ => {}
        }
        out.push(Seg::Z);
        implied.push(false);
    }
    (out, implied)
}

fn rebuilds(want: &[Seg], got: &[Seg], implied: &[bool]) -> Option<usize> {
    if want.len() != got.len() {
        return None;
    }
    let close = |a: u32, b: u32| {
        let (a, b) = (f32::from_bits(a), f32::from_bits(b));
        (a - b).abs() <= 1e-5 * a.abs().max(1.0)
    };
    let mut rounded = 0;
    for ((w, g), &imp) in want.iter().zip(got).zip(implied) {
        let (w, g) = (w.bits(), g.bits());
        if w.len() != g.len() || w[0] != g[0] {
            return None;
        }
        let n = w.len();
        for k in 1..n {
            let is_end = k >= n - 2;
            if w[k] == g[k] {
                continue;
            }
            if !(imp && is_end && close(w[k], g[k])) {
                return None;
            }
            rounded += 1;
        }
    }
    Some(rounded)
}

fn cff_segments(contours: &[Vec<(f32, f32, PointKind)>]) -> Vec<Seg> {
    let mut out = Vec::new();
    for contour in contours {
        let Some(&(sx, sy, _)) = contour.first() else {
            continue;
        };
        out.push(Seg::M(sx, sy));
        let mut controls: Vec<(f32, f32)> = Vec::new();
        for &(x, y, kind) in &contour[1..] {
            match kind {
                PointKind::OffCurveCubic => controls.push((x, y)),
                PointKind::OnCurve if controls.len() == 2 => {
                    out.push(Seg::C(
                        controls[0].0,
                        controls[0].1,
                        controls[1].0,
                        controls[1].1,
                        x,
                        y,
                    ));
                    controls.clear();
                }
                PointKind::OnCurve => out.push(Seg::L(x, y)),
                PointKind::OffCurveQuad => panic!("a CFF outline has no quadratic points"),
            }
        }
        if controls.len() == 2 {
            out.push(Seg::C(
                controls[0].0,
                controls[0].1,
                controls[1].0,
                controls[1].1,
                sx,
                sy,
            ));
        }
        out.push(Seg::Z);
    }
    out
}

fn without_closing_lines(segs: &[Seg]) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    let mut start = None;
    for (i, &s) in segs.iter().enumerate() {
        match s {
            Seg::M(x, y) => start = Some((x, y)),
            Seg::L(x, y) if Some((x, y)) == start && segs.get(i + 1) == Some(&Seg::Z) => continue,
            Seg::C(.., x, y) if Some((x, y)) == start && segs.get(i + 1) == Some(&Seg::Z) => {}
            _ => {}
        }
        out.push(s);
    }
    out
}

#[test]
fn points_rebuild_the_outline() {
    let mut rebuilt = 0usize;
    for (rel, coords, cff) in [
        ("tests/fixtures/jetbrains-mono.ttf", &[][..], false),
        (
            "assets/game/Cinzel-var.ttf",
            &[(*b"wght", 700.0)][..],
            false,
        ),
        ("tests/fixtures/NotoSansSunuwar-Regular.otf", &[][..], true),
    ] {
        let bytes = face_bytes(rel);
        let mut face = Face::parse(&bytes, 0).expect("committed face parses");
        let mut rounded = 0usize;
        for &(tag, v) in coords {
            face.set_variation(Tag::from_bytes(&tag), v).expect("axis");
        }
        for g in 0..face.number_of_glyphs() {
            let g = GlyphId(g);
            let mut segs = Segs::default();
            let bbox = face.outline_glyph(g, &mut segs);
            let mut points = Points::default();
            let point_bbox = face.outline_glyph_points(g, &mut points);
            assert_eq!(
                bbox, point_bbox,
                "{rel} glyph {}: bounding boxes differ",
                g.0
            );
            assert!(
                points.1.is_empty(),
                "{rel} glyph {}: an unterminated contour",
                g.0
            );
            let (want, got, implied) = if cff {
                let got = cff_segments(&points.0);
                let none = vec![false; got.len()];
                (without_closing_lines(&segs.0), got, none)
            } else {
                let (got, implied) = truetype_segments(&points.0);
                (segs.0.clone(), got, implied)
            };
            let r = rebuilds(&want, &got, &implied);
            assert!(
                r.is_some(),
                "{rel} glyph {}: the point stream does not rebuild the outline\n  outline {want:?}\n  rebuilt {got:?}",
                g.0
            );
            rounded += r.unwrap_or(0);
            rebuilt += usize::from(!want.is_empty());
        }
        assert!(
            !coords.is_empty() || rounded == 0,
            "{rel}: {rounded} midpoints rounded at the default instance"
        );
    }
    assert!(rebuilt > 1000, "only {rebuilt} outlines rebuilt");
}

#[test]
fn scaled_sink_is_the_renderers_arithmetic() {
    let bytes = face_bytes("assets/bundled/JetBrainsMono-Regular.ttf");
    let face = Face::parse(&bytes, 0).expect("bundled face parses");
    let s = 13.0 / f32::from(face.units_per_em());
    let (left, top) = (-0.75_f32, 11.375_f32);
    let map = |segs: &[Seg], f: &dyn Fn(f32, f32) -> (f32, f32)| -> Vec<Seg> {
        segs.iter()
            .map(|&seg| match seg {
                Seg::M(x, y) => {
                    let (x, y) = f(x, y);
                    Seg::M(x, y)
                }
                Seg::L(x, y) => {
                    let (x, y) = f(x, y);
                    Seg::L(x, y)
                }
                Seg::Q(a, b, x, y) => {
                    let ((a, b), (x, y)) = (f(a, b), f(x, y));
                    Seg::Q(a, b, x, y)
                }
                Seg::C(a, b, c, d, x, y) => {
                    let ((a, b), (c, d), (x, y)) = (f(a, b), f(c, d), f(x, y));
                    Seg::C(a, b, c, d, x, y)
                }
                Seg::Z => Seg::Z,
            })
            .collect()
    };
    let mut drawn = 0usize;
    for g in 0..face.number_of_glyphs() {
        let g = GlyphId(g);
        let mut units = Segs::default();
        face.outline_glyph(g, &mut units);
        drawn += usize::from(!units.0.is_empty());

        let mut up = Segs::default();
        face.outline_glyph(g, &mut ScaledSink::new(&mut up, s));
        assert!(same_bits(&up.0, &map(&units.0, &|x, y| (x * s, y * s))));

        let mut ink = Segs::default();
        face.outline_glyph(g, &mut ScaledSink::affine(&mut ink, s, -s, -left, top));
        assert!(same_bits(
            &ink.0,
            &map(&units.0, &|x, y| (x * s - left, top - y * s))
        ));
    }
    assert!(drawn > 1000, "only {drawn} glyphs drew");
}
