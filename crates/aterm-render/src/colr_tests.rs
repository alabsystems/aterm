// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// Segoe UI Emoji's bytes, or this host COULD NOT RUN the test.
#[cfg(windows)]
fn segoe_ui_emoji() -> Vec<u8> {
    const SEGOE: &str = r"C:\Windows\Fonts\seguiemj.ttf";
    std::fs::read(SEGOE).unwrap_or_else(|e| {
        aterm_spec::host::could_not_run(format!("{SEGOE} does not read here ({e})"))
    })
}

/// The `(columns, rows)` an RGBA raster inks (alpha >= 32), in order.
#[cfg(windows)]
fn inked_extent(rgba: &[u8], w: usize, h: usize) -> (Vec<usize>, Vec<usize>) {
    let inked = |x: usize, y: usize| rgba[(y * w + x) * 4 + 3] >= 32;
    let cols = (0..w).filter(|&x| (0..h).any(|y| inked(x, y))).collect();
    let rows = (0..h).filter(|&y| (0..w).any(|x| inked(x, y))).collect();
    (cols, rows)
}

#[test]
#[cfg(windows)]
fn segoe_ui_emoji_glyphs_draw_whole_and_centred_in_their_box() {
    let bytes = segoe_ui_emoji();
    let face = Face::parse(&bytes, 0).expect("Segoe UI Emoji parses");
    let (w, h) = (18usize, 23usize);
    let mut bottoms = Vec::new();
    for ch in ['✅', '❌', '⭐', '⚡', '⏳', '😀', '🚀', '👍', '⚠', '❤'] {
        let gid = face.glyph_index(ch).expect("Segoe UI Emoji maps it");
        let rgba = rasterize_colr(&face, gid, w, h).expect("a COLR glyph");
        let (cols, rows) = inked_extent(&rgba, w, h);
        let (left, right) = (cols[0], w - 1 - cols[cols.len() - 1]);
        let (top, bottom) = (rows[0], h - 1 - rows[rows.len() - 1]);
        assert!(
            left > 0 && right > 0 && top > 0 && bottom > 0,
            "{ch}: ink reaches the box's edge (margins l{left} r{right} t{top} b{bottom})"
        );
        assert!(
            left.abs_diff(right) <= 1,
            "{ch}: not centered across the box (margins l{left} r{right})"
        );
        if matches!(ch, '✅' | '❌' | '😀') {
            assert!(
                cols.len() >= 15,
                "{ch}: drawn {} columns wide in an 18-column box",
                cols.len()
            );
        }
        if !matches!(ch, '⚠' | '❤') {
            bottoms.push(bottom);
        }
    }
    let (lo, hi) = (bottoms.iter().min(), bottoms.iter().max());
    assert!(
        hi.zip(lo).is_some_and(|(hi, lo)| hi - lo <= 1),
        "the emoji sit on different baselines: {bottoms:?}"
    );
}

#[test]
#[cfg(windows)]
fn a_segoe_family_paints_every_member_inside_its_box() {
    let bytes = segoe_ui_emoji();
    let face = Face::parse(&bytes, 0).expect("Segoe UI Emoji parses");
    let shape_face = aterm_font::shape::ShapeFace::from_slice(&bytes, 0).expect("shapes");
    let mut buffer = aterm_font::shape::UnicodeBuffer::new();
    buffer.push_str("\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}");
    let shaped = aterm_font::shape::shape(&shape_face, &[], buffer);
    let mut pen = 0i32;
    let mut run = Vec::new();
    for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        let gid = GlyphId(u16::try_from(info.glyph_id).expect("a u16 glyph id"));
        if face.is_color_glyph(gid) {
            run.push((gid, (pen + pos.x_offset) as f32, pos.y_offset as f32));
        }
        pen += pos.x_advance;
    }
    assert_eq!(run.len(), 3, "three members: {run:?}");
    let (w, h) = (18usize, 23usize);
    let rgba = rasterize_colr_run(&face, &run, w, h).expect("a COLR run");
    let (cols, rows) = inked_extent(&rgba, w, h);
    assert!(
        cols[0] > 0 && cols[cols.len() - 1] < w - 1 && rows[0] > 0 && rows[rows.len() - 1] < h - 1,
        "the family reaches the box's edge: columns {cols:?}, rows {rows:?}"
    );
    for third in 0..3 {
        assert!(
            cols.iter().any(|&x| x * 3 / w == third),
            "no member inks third {third} of the box: columns {cols:?}"
        );
    }
}

#[test]
fn add_span_full_and_partial_coverage() {
    let mut cov = vec![0.0f32; 5];
    add_span(&mut cov, 1.0, 3.0, 1.0);
    assert_eq!(cov, vec![0.0, 1.0, 1.0, 0.0, 0.0]);
    let mut cov = vec![0.0f32; 5];
    add_span(&mut cov, 1.5, 2.5, 1.0);
    assert!((cov[1] - 0.5).abs() < 1e-6 && (cov[2] - 0.5).abs() < 1e-6);
    let mut cov_nan = vec![0.0f32; 5];
    add_span(&mut cov_nan, f32::NAN, f32::NAN, 1.0);
    assert_eq!(cov_nan, vec![0.0; 5]);
}

#[test]
fn affine_compose_then_apply() {
    let scale = Affine {
        a: 2.0,
        b: 0.0,
        c: 0.0,
        d: 2.0,
        e: 0.0,
        f: 0.0,
    };
    let translate = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 3.0,
        f: 4.0,
    };
    let m = scale.then(translate);
    assert_eq!(m.apply(0.0, 0.0), (6.0, 8.0));
}

fn colr_test_font() -> &'static [u8] {
    include_bytes!("../tests/fixtures/colr_1.ttf")
}

#[test]
fn colr_glyph_rasterizes_straight_alpha() {
    let bytes = colr_test_font();
    let face = Face::parse(bytes, 0).expect("valid font");
    let fg = RgbaColor::new(0, 0, 0, 255);
    let (mut opaque_checked, mut translucent_checked) = (false, false);
    for gid in (0..face.number_of_glyphs()).map(GlyphId) {
        if !face.is_color_glyph(gid) {
            continue;
        }
        let mut probe = PaintProbe::default();
        if face.paint_color_glyph(gid, 0, fg, &mut probe).is_none()
            || probe.solid_layers != 1
            || probe.gradient_layers != 0
        {
            continue;
        }
        let color = probe.solid_color.expect("solid layer recorded");
        if color.alpha == 0 {
            continue;
        }
        let (w, h) = (48, 48);
        let rgba = rasterize_colr(&face, gid, w, h).expect("COLR glyph rasterizes");
        assert_eq!(rgba.len(), w * h * 4);
        let full: Vec<&[u8; 4]> = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] == color.alpha)
            .collect();
        assert!(!full.is_empty(), "gid {}: no fully-covered pixels", gid.0);
        for p in &full {
            assert!(
                p[0].abs_diff(color.red) <= 2
                    && p[1].abs_diff(color.green) <= 2
                    && p[2].abs_diff(color.blue) <= 2,
                "gid {}: fully-covered pixel {:?} (a={}) must carry the layer's \
                 straight rgb ({},{},{}), not a premultiplied-dark value",
                gid.0,
                &p[..3],
                p[3],
                color.red,
                color.green,
                color.blue
            );
        }
        if color.alpha == 255 {
            opaque_checked = true;
        } else {
            translucent_checked = true;
        }
    }
    assert!(
        opaque_checked && translucent_checked,
        "colr_1.ttf must exercise both the opaque fast path and the \
         un-premultiply path (opaque={opaque_checked}, translucent={translucent_checked})"
    );
}

#[test]
fn unpremultiply_restores_straight_rgb() {
    let mut buf = vec![
        100, 50, 20, 128, //
        9, 8, 7, 255, //
        0, 0, 0, 0,
    ];
    unpremultiply_straight(&mut buf);
    let p = &buf[0..4];
    assert!(
        p[0].abs_diff(200) <= 2 && p[1].abs_diff(100) <= 2 && p[2].abs_diff(40) <= 2,
        "straight rgb must be restored, got {p:?}"
    );
    assert_eq!(&buf[4..8], &[9, 8, 7, 255], "opaque pixels stay byte-exact");
    assert_eq!(&buf[8..12], &[0, 0, 0, 0], "transparent pixels stay zero");
}

const IDENTITY: Affine = Affine {
    a: 1.0,
    b: 0.0,
    c: 0.0,
    d: 1.0,
    e: 0.0,
    f: 0.0,
};

type RawStop = (f32, (u8, u8, u8, u8));

fn ramp(extend: GradientExtend, stops: &[RawStop]) -> ColorRamp {
    ColorRamp {
        extend,
        stops: stops
            .iter()
            .map(|&(o, (r, g, b, a))| (o, RgbaColor::new(r, g, b, a)))
            .collect(),
    }
}

#[test]
fn color_ramp_pad_repeat_reflect() {
    let two = |e| ramp(e, &[(0.0, (0, 0, 0, 255)), (1.0, (200, 100, 50, 255))]);
    let mid = two(GradientExtend::Pad).sample(0.5);
    assert_eq!(
        (mid.red, mid.green, mid.blue, mid.alpha),
        (100, 50, 25, 255)
    );
    let p = two(GradientExtend::Pad);
    assert_eq!(p.sample(-1.0).red, 0);
    assert_eq!(p.sample(2.0).red, 200);
    assert_eq!(
        two(GradientExtend::Repeat).sample(1.25).red,
        p.sample(0.25).red
    );
    assert_eq!(
        two(GradientExtend::Reflect).sample(1.25).red,
        p.sample(0.75).red
    );
    assert_eq!(
        ramp(GradientExtend::Repeat, &[(f32::INFINITY, (90, 0, 0, 255))])
            .sample(0.5)
            .red,
        90
    );
    assert_eq!(two(GradientExtend::Repeat).sample(f32::INFINITY).alpha, 255);
}

#[test]
fn a_ramp_of_many_stops_is_sampled_by_bisection_as_a_scan_would() {
    let colours = [
        RgbaColor::new(200, 30, 10, 255),
        RgbaColor::new(10, 40, 220, 128),
        RgbaColor::new(0, 250, 90, 0),
    ];
    // Offsets in pairs, as hard stops come: 2^16 stops, 2^15 offsets.
    let n = 1usize << 16;
    let stops: Vec<(f32, RgbaColor)> = (0..n)
        .map(|i| ((i / 2) as f32 / (n / 2 - 1) as f32, colours[i % 3]))
        .collect();
    let scan = |t: f32| {
        let mut i = 0;
        while stops[i + 1].0 < t {
            i += 1;
        }
        let ((o0, c0), (o1, c1)) = (stops[i], stops[i + 1]);
        lerp_premul(c0, c1, (t - o0) / (o1 - o0))
    };
    let ramp = ColorRamp {
        extend: GradientExtend::Pad,
        stops: stops.clone(),
    };
    let _ = crate::test_counters::take_ramp_stops_read();
    let samples = 1_000;
    for k in 0..samples {
        let t = (k as f32 + 0.5) / samples as f32;
        let (got, want) = (ramp.sample(t), scan(t));
        assert!(
            rgba_eq(got, want),
            "t {t}: {got:?} where a scan finds {want:?}"
        );
    }
    let read = crate::test_counters::take_ramp_stops_read();
    assert!(
        read <= samples * 17,
        "{samples} samples of {n} stops compared {read} of them"
    );
}

#[test]
fn stop_interpolation_is_premultiplied() {
    let c = lerp_premul(
        RgbaColor::new(255, 0, 0, 255),
        RgbaColor::new(0, 0, 255, 0),
        0.5,
    );
    assert_eq!((c.red, c.green, c.blue, c.alpha), (255, 0, 0, 128));
}

#[test]
fn linear_projection_respects_p2_rotation() {
    assert_eq!(linear_projection(10.0, 0.0, 0.0, 10.0), Some((10.0, 0.0)));
    let (qx, qy) = linear_projection(10.0, 0.0, 5.0, 10.0).expect("well-formed");
    let q2 = qx * qx + qy * qy;
    assert!(((10.0 * qx) / q2 - 1.0).abs() < 1e-5);
    assert!(((5.0 * qx + 10.0 * qy) / q2).abs() < 1e-5);
    assert_eq!(linear_projection(5.0, 10.0, 5.0, 10.0), None);
    assert_eq!(linear_projection(10.0, 0.0, 0.0, 0.0), None);
}

#[test]
fn radial_t_is_radius_fraction() {
    let g = GradientFill {
        kind: GradientKind::Radial {
            x0: 0.0,
            y0: 0.0,
            r0: 0.0,
            dx: 0.0,
            dy: 0.0,
            dr: 10.0,
        },
        ramp: ramp(
            GradientExtend::Pad,
            &[(0.0, (0, 0, 0, 255)), (1.0, (200, 0, 0, 255))],
        ),
        inv: IDENTITY,
    };
    assert_eq!(g.sample_at(0.0, 0.0).expect("defined").red, 0);
    assert_eq!(g.sample_at(5.0, 0.0).expect("defined").red, 100);
    assert_eq!(g.sample_at(0.0, 10.0).expect("defined").red, 200);
    assert_eq!(g.sample_at(30.0, 0.0).expect("defined").red, 200);
}

#[test]
fn sweep_t_follows_angle() {
    let g = GradientFill {
        kind: GradientKind::Sweep {
            cx: 0.0,
            cy: 0.0,
            start_deg: 0.0,
            sweep_deg: 180.0,
        },
        ramp: ramp(
            GradientExtend::Pad,
            &[(0.0, (0, 0, 0, 255)), (1.0, (200, 0, 0, 255))],
        ),
        inv: IDENTITY,
    };
    assert_eq!(g.sample_at(10.0, 0.0).expect("defined").red, 0);
    assert_eq!(g.sample_at(0.0, 10.0).expect("defined").red, 100);
    assert_eq!(g.sample_at(-10.0, 1e-4).expect("defined").red, 200);
}

/// A radial or sweep paint with a parameter that is not finite builds no gradient (the
/// glyph's fill is dropped) instead of one whose every sample is the first stop; the same
/// paints with finite parameters build and sample as before.
#[test]
fn a_gradient_with_a_non_finite_parameter_builds_nothing() {
    let red_at = |kind, x: f32, y: f32| {
        GradientFill {
            kind,
            ramp: ramp(
                GradientExtend::Pad,
                &[(0.0, (0, 0, 0, 255)), (1.0, (200, 0, 0, 255))],
            ),
            inv: IDENTITY,
        }
        .sample_at(x, y)
        .map(|c| c.red)
    };
    let radial = radial_kind((0.0, 0.0, 0.0), (0.0, 0.0, 10.0)).expect("a finite radial");
    assert_eq!(red_at(radial, 5.0, 0.0), Some(100));
    let sweep = sweep_kind((0.0, 0.0), -1.0, 0.0).expect("a finite sweep");
    assert_eq!(red_at(sweep, 0.0, 10.0), Some(100));
    assert!(radial_kind((1.0, 2.0, 3.0), (1.0, 2.0, 3.0)).is_none());
    assert!(sweep_kind((0.0, 0.0), 0.5, 0.5).is_none());
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let base = [0.0, 0.0, 0.0, 0.0, 0.0, 10.0];
        for i in 0..base.len() {
            let mut p = base;
            p[i] = bad;
            assert!(
                radial_kind((p[0], p[1], p[2]), (p[3], p[4], p[5])).is_none(),
                "radial parameter {i} = {bad}"
            );
        }
        let base = [0.0, 0.0, -1.0, 0.0];
        for i in 0..base.len() {
            let mut p = base;
            p[i] = bad;
            assert!(
                sweep_kind((p[0], p[1]), p[2], p[3]).is_none(),
                "sweep parameter {i} = {bad}"
            );
        }
    }
}

/// Counts multi-colour gradient layers vs solid layers (and the last solid colour)
/// without rasterizing.
#[derive(Default)]
struct PaintProbe {
    gradient_layers: usize,
    solid_layers: usize,
    solid_color: Option<RgbaColor>,
}

impl Painter<'_> for PaintProbe {
    fn outline_glyph(&mut self, _: GlyphId) {}
    fn paint(&mut self, paint: Paint<'_>) {
        let stops: Vec<RgbaColor> = match &paint {
            Paint::Solid(c) => {
                self.solid_layers += 1;
                self.solid_color = Some(*c);
                return;
            }
            Paint::LinearGradient(g) => g.stops(0, &[]).map(|s| s.color).collect(),
            Paint::RadialGradient(g) => g.stops(0, &[]).map(|s| s.color).collect(),
            Paint::SweepGradient(g) => g.stops(0, &[]).map(|s| s.color).collect(),
        };
        let multi = stops
            .first()
            .is_some_and(|f| stops.iter().any(|c| !rgba_eq(*c, *f)));
        if multi {
            self.gradient_layers += 1;
        } else {
            self.solid_layers += 1;
        }
    }
    fn push_transform(&mut self, _: Transform) {}
    fn pop_transform(&mut self) {}
    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, _: aterm_font::colr::ClipBox) {}
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _: CompositeMode) {}
    fn pop_layer(&mut self) {}
}

fn max_constant_alpha_run_spread(rgba: &[u8], w: usize, h: usize) -> usize {
    let px = rgba.as_chunks::<4>().0;
    assert_eq!(px.len(), w * h);
    fn scan(seq: impl Iterator<Item = [u8; 4]>, best: &mut usize) {
        let mut colors: std::collections::HashSet<(u8, u8, u8)> = std::collections::HashSet::new();
        let mut run_alpha = 0u8;
        let mut len = 0usize;
        for p in seq.chain(std::iter::once([0, 0, 0, 0])) {
            if p[3] >= 128 && (len == 0 || p[3] == run_alpha) {
                run_alpha = p[3];
                colors.insert((p[0], p[1], p[2]));
                len += 1;
                continue;
            }
            if len >= 8 {
                *best = (*best).max(colors.len());
            }
            colors.clear();
            len = 0;
            if p[3] >= 128 {
                run_alpha = p[3];
                colors.insert((p[0], p[1], p[2]));
                len = 1;
            }
        }
    }
    let mut best = 0;
    for y in 0..h {
        scan((0..w).map(|x| px[y * w + x]), &mut best);
    }
    for x in 0..w {
        scan((0..h).map(|y| px[y * w + x]), &mut best);
    }
    best
}

#[test]
fn colr_gradient_renders_a_colour_spread() {
    let bytes = colr_test_font();
    let face = Face::parse(bytes, 0).expect("valid font");
    let fg = RgbaColor::new(0, 0, 0, 255);
    let mut best = 0usize;
    for gid in (0..face.number_of_glyphs()).map(GlyphId) {
        if !face.is_color_glyph(gid) {
            continue;
        }
        let mut probe = PaintProbe::default();
        if face.paint_color_glyph(gid, 0, fg, &mut probe).is_none() {
            continue;
        }
        if probe.gradient_layers == 0 || probe.solid_layers != 0 || probe.gradient_layers > 4 {
            continue;
        }
        let Some(rgba) = rasterize_colr(&face, gid, 64, 64) else {
            continue;
        };
        best = best.max(max_constant_alpha_run_spread(&rgba, 64, 64));
        if best >= 8 {
            break;
        }
    }
    assert!(
        best >= 8,
        "a multi-stop gradient glyph must sweep >= 8 distinct colours \
         within one constant-alpha run, best glyph gave {best}"
    );
}
