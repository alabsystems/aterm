// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the scanner classifiers over synthetic frames and kept paint fixtures.

use super::*;

const CELL: Cell = Cell { w: 7, h: 14 };
const LIT: Rgb = [240, 80, 0];

fn bgr(b: u8, g: u8, r: u8) -> Rgb {
    [r, g, b]
}

fn frame(w: usize, h: usize, colored: &[(usize, usize, Rgb)]) -> Frame {
    ground_frame(w, h, [0, 0, 0], colored)
}

fn ground_frame(w: usize, h: usize, ground: Rgb, colored: &[(usize, usize, Rgb)]) -> Frame {
    let mut f = Frame::filled(w, h, ground);
    for &(x, y, rgb) in colored {
        f.paint(x, y, rgb);
    }
    f
}

fn block(x0: usize, y0: usize, w: usize, h: usize, rgb: Rgb) -> Vec<(usize, usize, Rgb)> {
    (y0..y0 + h)
        .flat_map(|y| (x0..x0 + w).map(move |x| (x, y, rgb)))
        .collect()
}

fn at_hue(degrees: f64) -> Rgb {
    let (h, v) = (degrees / 360.0, 240.0 / 255.0);
    let i = (h * 6.0).trunc();
    let f = h * 6.0 - i;
    let s = 1.0;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match (i as i64).rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    let byte = |c: f64| (c * 255.0).round_ties_even() as u8;
    [byte(r), byte(g), byte(b)]
}

fn metrics(w: usize, h: usize, colored: &[(usize, usize, Rgb)]) -> DynamicMetrics {
    dynamic_metrics(&frame(w, h, &[]), &frame(w, h, colored), CELL, &[])
}

fn rgb_hex(hex: u32) -> Rgb {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

fn sat_patch(w: usize, x0: usize, y0: usize, x1: usize, y1: usize, degrees: f64) -> SatMap {
    (y0..=y1)
        .flat_map(|y| (x0..=x1).map(move |x| (y * w + x, degrees)))
        .collect()
}

fn rows_of(lines: &[String]) -> Vec<StatusRow> {
    lines.iter().map(|l| StatusRow::new(l.clone())).collect()
}

fn scan_paired(frames: &[Frame], rows: Vec<StatusRow>) -> ScanReport {
    let names: Vec<String> = (0..frames.len())
        .map(|n| format!("shot_{n:04}.png"))
        .collect();
    let ctx = TakeContext {
        cell: CELL,
        statuses: Some(rows),
        index: None,
        driven_ms: None,
    };
    scan_frames(&names, frames.iter().cloned().map(Ok), &ctx).expect("the take scans")
}

fn claiming_rows(count: usize, pet: bool, body: Option<&str>) -> Vec<StatusRow> {
    let spawns: Vec<usize> = (0..count - 1).chain([count - 2]).collect();
    spawns
        .into_iter()
        .map(|n| {
            let mut row = format!(
                "trail pet_active={pet} cat_active=false ribbon_hue_bands=8 spawns={n} speed=0.0"
            );
            if let Some(body) = body {
                row.push_str(&format!(" pet_body={body}"));
            }
            StatusRow::new(row)
        })
        .collect()
}

fn cyan_of(hex: u32) -> usize {
    dynamic_metrics(
        &frame(1, 1, &[]),
        &frame(1, 1, &[(0, 0, rgb_hex(hex))]),
        CELL,
        &[],
    )
    .cyan_pixels
}

#[test]
fn the_worst_sample_reported_is_the_one_nearest_pure_cyan() {
    let m = metrics(
        2,
        1,
        &[(0, 0, bgr(0xC0, 0xFF, 0x00)), (1, 0, bgr(0xB7, 0xB6, 0x0E))],
    );
    assert_eq!(m.cyan_pixels, 2);
    let worst = m.cyan_worst.expect("a cyan sample");
    assert!((worst.degrees - 180.36).abs() < 0.05, "{}", worst.degrees);
}

#[test]
fn cyan_is_the_band_between_the_declared_edges() {
    for (hex, want) in [
        (0x0EB6B7, 1), // the shipped gradient crossing
        (0x00D6FF, 1), // the rotated azure anchor
        (0x00FFC0, 1), // 165.2 deg, inside
        (0x00FFBF, 0), // 164.9 deg, outside
        (0x00C0FF, 1), // 194.8 deg, inside
        (0x00BFFF, 0), // 195.1 deg, outside
        (0xB4FFFF, 0), // a pale wash
        (0x063030, 0), // a near-black teal
        (0xFF0000, 0),
        (0xFF9900, 0),
        (0xFFFF00, 0),
        (0x33FF00, 0),
        (0x0099FF, 0),
        (0x6633FF, 0),
    ] {
        assert_eq!(cyan_of(hex), want, "#{hex:06X}");
    }
}

#[test]
fn the_colour_floor_admits_the_bed_and_refuses_grey_and_fringe() {
    for (px, ink) in [
        (bgr(0, 83, 83), true),
        (bgr(0, 94, 0), true),
        (bgr(90, 90, 0), true),
        (bgr(20, 75, 75), true),
        (bgr(20, 80, 18), true),
        (bgr(78, 78, 20), true),
        (bgr(0, 236, 236), true),
        (bgr(200, 0, 0), true),
        (bgr(0, 0, 180), true),
        (bgr(20, 45, 45), false),
        (bgr(24, 19, 17), false),
        (bgr(150, 150, 150), false),
        (bgr(255, 255, 255), false),
        (bgr(90, 84, 80), false),
    ] {
        assert_eq!(saturated_hue_degrees(px).is_some(), ink, "{px:?}");
    }
}

#[test]
fn a_star_on_a_long_band_s_edge_leaves_the_band_lit() {
    let (w, h) = (200, 80);
    let mut sat = sat_patch(w, 20, 40, 139, 51, 60.0);
    sat.extend(sat_patch(w, 60, 27, 68, 39, 200.0)); // a 9x13 star on the top edge
    assert_eq!(
        band_ink_metrics(w, h, &sat, 14, &[]),
        (120 * 12, HueSet::of(&[2]))
    );
}

#[test]
fn a_tall_object_whose_band_rows_still_exceed_the_cap_is_dropped() {
    let sat = sat_patch(200, 20, 20, 139, 59, 60.0); // 120x40: no band in it
    assert_eq!(
        band_ink_metrics(200, 80, &sat, 14, &[]),
        (0, HueSet::default())
    );
}

fn spine(spawns: u32, speed: f64) -> String {
    format!("trail licensed={spawns} spawns={spawns} speed={speed:.1}")
}

#[test]
fn the_driven_claim_ends_where_the_engine_stops_driving() {
    let cases: [(&str, Vec<String>, Option<usize>); 6] = [
        (
            "a row without speed is driven by spawns alone",
            vec![
                "trail spawns=0 licensed=0".into(),
                "trail spawns=1 licensed=1".into(),
                "trail spawns=1 licensed=1".into(),
            ],
            Some(1),
        ),
        (
            "an older row with speed still reads it",
            vec![
                "trail spawns=0 speed=0.0".into(),
                "trail spawns=0 speed=2.5".into(),
                "trail spawns=0 speed=0.0".into(),
            ],
            Some(1),
        ),
        (
            "the claim stops where the engine stops driving",
            vec![
                spine(0, 0.0),
                spine(1, 0.0),
                spine(2, 3.0),
                spine(3, 1.7),
                spine(3, 0.7),
                spine(3, 0.0),
                spine(3, 0.0),
            ],
            Some(4),
        ),
        (
            "an admitted move drives a frame with no velocity yet",
            vec![spine(0, 0.0), spine(1, 0.0), spine(1, 0.0)],
            Some(1),
        ),
        (
            "a spine that snapped to zero between keys still drives",
            vec![spine(0, 0.0), spine(1, 2.0), spine(1, 0.0), spine(2, 0.0)],
            Some(3),
        ),
        (
            "a take that never drives bounds nothing",
            vec![spine(0, 0.0); 5],
            None,
        ),
    ];
    for (name, lines, want) in cases {
        assert_eq!(driven_last_frame(&rows_of(&lines)), Ok(want), "{name}");
    }
}

#[test]
fn a_skewed_row_is_refused_rather_than_read_as_undriven() {
    let nan = rows_of(&[
        "trail spawns=0 speed=0.0".into(),
        "trail spawns=1 speed=nan".into(),
    ]);
    assert!(driven_last_frame(&nan).is_err());
    let missing = rows_of(&["trail spawns=0".into(), "trail licensed=1".into()]);
    assert!(driven_last_frame(&missing).is_err());
}

#[test]
fn status_tokens_are_exact() {
    let row = StatusRow::new(
        "trail ribbon_active=true ribbon_hue_bands=7 pet_active=true cat_active=false",
    );
    assert_eq!(row.value("ribbon_hue_bands"), Some("7"));
    assert_eq!(row.value("pet_active"), Some("true"));
    assert_eq!(row.value("active"), None);
    assert_eq!(row.uint("ribbon_hue_bands"), Ok(7));
    assert_eq!(row.bool("pet_active"), Ok(true));
    assert_eq!(row.bool("cat_active"), Ok(false));
    let styled = StatusRow::new(r#"trail style="rainbow kitty" resolved=rainbow-kitty"#);
    assert_eq!(styled.quoted("style"), Some("rainbow kitty"));
    assert_eq!(styled.value("resolved"), Some("rainbow-kitty"));
}

#[test]
fn required_status_tokens_fail_closed() {
    let row = StatusRow::new;
    assert!(row("trail pet_active=maybe").bool("pet_active").is_err());
    assert!(row("trail cat_active=true").bool("pet_active").is_err());
    assert!(
        row("trail ribbon_hue_bands=-1")
            .uint("ribbon_hue_bands")
            .is_err()
    );
    assert!(
        row("trail ribbon_active=true")
            .uint("ribbon_hue_bands")
            .is_err()
    );
    for line in [
        "trail speed=fast",
        "trail speed=-1.0",
        "trail speed=nan",
        "trail speed=inf",
        "trail momentum=0.5",
    ] {
        assert!(row(line).nonneg_float("speed").is_err(), "{line}");
    }
}

#[test]
fn the_emitted_body_uses_exclusive_ends_and_refuses_malformed_geometry() {
    let body = |raw: &str| StatusRow::new(format!("trail pet_body={raw}")).pet_body();
    assert_eq!(body("50,72,4,22"), Ok(Some(PixelBox::new(50, 4, 71, 21))));
    assert_eq!(body("none"), Ok(None));
    assert_eq!(StatusRow::new("trail").pet_body(), Ok(None));
    for raw in ["", "1,2,3", "1,2,3,4,5", "0,NaN,1,2", "5,5,1,2", "5,4,1,2"] {
        assert!(body(raw).is_err(), "{raw}");
    }
}

#[test]
fn cursor_row_cannot_impersonate_a_companion() {
    let cursor = block(3, 5, 7, 14, bgr(0, 80, 240));
    assert_eq!(largest_tall_component(&frame(40, 24, &cursor)), Shape::NONE);
}

#[test]
fn a_small_whole_body_is_a_companion_witness() {
    let body = block(12, 3, 12, 18, bgr(40, 130, 220));
    assert_eq!(
        largest_tall_component(&frame(40, 24, &body)),
        Shape::new(12 * 18, 12, 18)
    );
}

#[test]
fn compact_pet_is_allowed_only_in_isolated_endpoint_witnesses() {
    let f = frame(40, 24, &block(12, 7, 16, 9, bgr(40, 130, 220)));
    let black = [0, 0, 0];
    assert_eq!(largest_tall_component(&f), Shape::NONE);
    for frame_no in [0, 7, 8, 9] {
        assert_eq!(
            companion_component(&f, black, true, frame_no, 10, None, false),
            Shape::new(16 * 9, 16, 9)
        );
    }
    for frame_no in 1..7 {
        assert_eq!(
            companion_component(&f, black, true, frame_no, 10, None, false),
            Shape::NONE
        );
    }
    assert_eq!(
        companion_component(&f, black, false, 9, 10, None, false),
        Shape::NONE
    );
    assert_eq!(
        companion_component(&f, black, false, 9, 10, None, true),
        Shape::NONE
    );
}

#[test]
fn two_drawn_companions_are_two_components() {
    let left = block(4, 3, 12, 18, bgr(40, 130, 220));
    let right = block(40, 3, 12, 18, bgr(220, 130, 40));
    let both = frame(64, 24, &[left.clone(), right].concat());
    let found = tall_components(&both, [0, 0, 0], COMP_MIN_H);
    assert_eq!(found.len(), 2);
    assert_eq!(largest_tall_component(&both), found[0]);
    assert_eq!(
        tall_components(&frame(64, 24, &left), [0, 0, 0], COMP_MIN_H).len(),
        1
    );
    assert!(compact_pet_witness_frame(0, 10));
    assert!(compact_pet_witness_frame(9, 10));
    assert!(!compact_pet_witness_frame(4, 10));
}

#[test]
fn wide_one_row_ribbon_or_text_cannot_impersonate_a_companion() {
    let f = frame(48, 24, &block(8, 5, 28, 14, bgr(40, 130, 220)));
    assert_eq!(largest_tall_component(&f), Shape::NONE);
    assert_eq!(
        companion_component(&f, [0, 0, 0], true, 4, 10, None, false),
        Shape::NONE
    );
}

#[test]
fn a_charcoal_companion_is_not_rejected_for_lacking_saturation() {
    let body = block(12, 3, 12, 18, bgr(52, 52, 52));
    assert_eq!(
        largest_tall_component(&frame(40, 24, &body)),
        Shape::new(12 * 18, 12, 18)
    );
}

fn resting_head(w: usize, h: usize, frame_no: usize, pet: bool, cat: bool) -> Shape {
    let f = frame(48, 30, &block(10, 4, w, h, bgr(40, 130, 220)));
    companion_component(&f, [0, 0, 0], pet, frame_no, 10, None, cat)
}

#[test]
fn the_resting_head_is_a_witness_only_in_the_claimed_final_stills() {
    for frame_no in [0, 7, 8, 9] {
        assert_eq!(
            resting_head(24, 16, frame_no, false, true),
            Shape::new(24 * 16, 24, 16),
            "final still {frame_no}"
        );
    }
    for frame_no in 1..7 {
        assert_eq!(
            resting_head(24, 16, frame_no, false, true),
            Shape::NONE,
            "mid-take {frame_no}"
        );
    }
    for (w, h, cat) in [(24, 16, false), (28, 14, true), (7, 14, true)] {
        assert_eq!(
            resting_head(w, h, 9, false, cat),
            Shape::NONE,
            "{w}x{h} cat={cat}"
        );
    }
}

#[test]
fn only_interior_rainbow_blackouts_count() {
    let none = [false; 8];
    assert_eq!(
        longest_interior_gap(&[false, true, false, false, true, false]),
        2
    );
    assert_eq!(
        longest_sustained_interior_gap(
            &[false, true, false, false, false, true, true, false],
            &none
        ),
        0,
        "an isolated onset glint cannot start the mature continuity clock"
    );
    assert_eq!(
        longest_sustained_interior_gap(
            &[false, true, true, false, false, true, true, false],
            &none
        ),
        2
    );
    assert_eq!(
        longest_interior_gap(&[false, false, true, true, false, false]),
        0
    );
    assert_eq!(longest_interior_gap(&[false, false]), 0);
    assert_eq!(trailing_true_run(&[true, false, true, true, true]), 3);
    assert_eq!(trailing_true_run(&[true, false]), 0);
    assert_eq!(
        trailing_true_run(&[true, true, true, false, false, false]),
        0
    );
}

#[test]
fn duplicate_capture_cannot_manufacture_rainbow_maturity() {
    assert_eq!(rainbow_summary(&[true, true], &[false, true]), (1, 1, 0));
    assert_eq!(
        sustained_rainbow_bounds(&[true, true], &[false, true]),
        None
    );
}

#[test]
fn duplicate_capture_does_not_lengthen_an_interior_blackout() {
    let flags = [true, true, true, false, false, true, true];
    let duplicates = [false, true, false, false, true, false, false];
    assert_eq!(rainbow_summary(&flags, &duplicates), (4, 2, 1));
}

#[test]
fn moving_cursor_and_colored_glyphs_are_not_ribbon_presence() {
    let lit = bgr(0, 80, 240);
    let mut colored = block(2, 3, 7, 14, lit);
    for x0 in [16, 23] {
        for y in 5..14 {
            for x in x0..x0 + 4 {
                if x == x0 || x == x0 + 3 || y == 5 || y == 13 {
                    colored.push((x, y, lit));
                }
            }
        }
    }
    let m = metrics(40, 20, &colored);
    assert_eq!(m.row_hue_peak, 1, "the negative control really is one hue");
    assert!(!ribbon_present(&m));
}

#[test]
fn one_hue_thin_highlighter_is_ribbon_presence() {
    let strip: Vec<_> = (4..12).map(|x| (x, 3, bgr(0, 80, 240))).collect();
    let m = metrics(20, 8, &strip);
    assert_eq!(m.row_hue_separation, 0.0, "one hue cannot buy the hue arm");
    assert_eq!(m.ribbon_component, Shape::new(8, 8, 1));
    assert!(ribbon_present(&m));
    assert!(driven_lit(&m));
}

#[test]
fn one_pixel_highlighter_seam_still_has_presence() {
    let strip: Vec<_> = [2, 3, 4, 5, 7, 8, 9, 10]
        .iter()
        .map(|&x| (x, 3, bgr(0, 80, 240)))
        .collect();
    let m = metrics(20, 8, &strip);
    assert_eq!(m.ribbon_component, Shape::new(8, 9, 1));
    assert!(ribbon_present(&m));
}

#[test]
fn compact_resident_pet_is_not_ribbon_presence() {
    let pet: Vec<_> = (4..16)
        .flat_map(|y| (8..32).map(move |x| (x, y)))
        .filter(|&(x, y)| x == 8 || x == 31 || y == 4 || y == 15)
        .map(|(x, y)| (x, y, bgr(0, 80, 240)))
        .collect();
    assert!(!ribbon_present(&metrics(40, 24, &pet)));
}

#[test]
fn dense_two_cell_run_on_cursor_bottom_is_attached_ribbon() {
    let lit = bgr(0, 80, 240);
    let mut colored = block(2, 2, 7, 14, lit);
    colored.extend((10..19).map(|x| (x, 15, lit)));
    let m = metrics(30, 20, &colored);
    assert_eq!(m.row_hue_separation, 0.0);
    assert_eq!(
        m.ribbon_component,
        Shape::NONE,
        "the tall merge cannot buy thin geometry"
    );
    assert_eq!(m.ribbon_run, RibbonRun::new(16, 17, 15));
    assert!(ribbon_present(&m));
}

#[test]
fn remote_run_on_cursor_bottom_is_not_attached_ribbon() {
    let lit = bgr(0, 80, 240);
    let mut colored = block(2, 2, 7, 14, lit);
    colored.extend((30..45).map(|x| (x, 15, lit)));
    colored.extend((11..15).map(|y| (30, y, lit)));
    let m = metrics(50, 20, &colored);
    assert_eq!(
        m.ribbon_component,
        Shape::NONE,
        "the tall remote glyph is not thin"
    );
    assert_eq!(m.ribbon_run, RibbonRun::NONE);
    assert!(!ribbon_present(&m));
}

const GROUND: Rgb = [17, 19, 24];
fn pale_caret() -> Rgb {
    bgr(221, 182, 202)
}
fn tinted_caret() -> Rgb {
    bgr(221, 181, 201)
}
fn hole_caret() -> Rgb {
    bgr(221, 182, 201)
}

fn caret_block(fill: Rgb, holes: &[(usize, usize)]) -> Vec<(usize, usize, Rgb)> {
    block(2, 2, 7, 14, fill)
        .into_iter()
        .map(|(x, y, c)| {
            (
                x,
                y,
                if holes.contains(&(x, y)) {
                    hole_caret()
                } else {
                    c
                },
            )
        })
        .collect()
}

fn measured_holes() -> Vec<(usize, usize)> {
    (5..14)
        .map(|y| (8, y))
        .chain([(8, 15), (4, 12), (5, 12), (4, 15), (5, 15)])
        .collect()
}

#[test]
fn a_caret_under_the_colour_floor_still_anchors_the_attached_run() {
    let [r, g, b] = pale_caret();
    assert!(saturated_hue_degrees(pale_caret()).is_none());
    assert_eq!(r.max(g).max(b) - r.min(g).min(b), SAT_SPREAD - 1);
    assert!(saturated_hue_degrees(tinted_caret()).is_some());
    let band = block(9, 15, 14, 5, bgr(0, 80, 240));
    for (label, caret, expected) in [
        (
            "uniform chroma-39 block",
            caret_block(pale_caret(), &[]),
            RibbonRun::new(14, 14, 15),
        ),
        (
            "measured hole pattern",
            caret_block(tinted_caret(), &measured_holes()),
            RibbonRun::new(16, 17, 15),
        ),
    ] {
        let colored = [caret, band.clone()].concat();
        let (w, h) = (30, 22);
        let m = dynamic_metrics(
            &ground_frame(w, h, GROUND, &[]),
            &ground_frame(w, h, GROUND, &colored),
            CELL,
            &[],
        );
        assert_eq!(
            m.ribbon_component,
            Shape::NONE,
            "{label}: the five-row body is past the thin arm"
        );
        assert_eq!(m.ribbon_run, expected, "{label}");
        assert!(ribbon_present(&m), "{label}");
        let changed: BTreeSet<usize> = colored.iter().map(|&(x, y, _)| y * w + x).collect();
        let dynamic: BTreeSet<usize> = colored
            .iter()
            .filter(|&&(_, _, c)| saturated_hue_degrees(c).is_some())
            .map(|&(x, y, _)| y * w + x)
            .collect();
        assert_eq!(
            attached_ribbon_run(w, h, &dynamic, &changed, CELL),
            expected,
            "{label}"
        );
    }
}

#[test]
fn a_pale_caret_alone_is_still_nothing() {
    let m = dynamic_metrics(
        &ground_frame(30, 22, GROUND, &[]),
        &ground_frame(30, 22, GROUND, &caret_block(pale_caret(), &[])),
        CELL,
        &[],
    );
    assert_eq!(m.ink, 0);
    assert_eq!(m.ribbon_run, RibbonRun::NONE);
    assert_eq!(m.band_ink, 0);
    assert!(!ribbon_present(&m));
    assert!(!driven_lit(&m));
}

#[test]
fn a_remote_run_does_not_attach_to_a_pale_caret_either() {
    let lit = bgr(0, 80, 240);
    let mut remote: Vec<_> = (30..45).map(|x| (x, 15, lit)).collect();
    remote.extend((11..15).map(|y| (30, y, lit)));
    for caret in [
        caret_block(pale_caret(), &[]),
        caret_block(tinted_caret(), &measured_holes()),
    ] {
        let m = dynamic_metrics(
            &ground_frame(50, 22, GROUND, &[]),
            &ground_frame(50, 22, GROUND, &[caret, remote.clone()].concat()),
            CELL,
            &[],
        );
        assert_eq!(m.ribbon_component, Shape::NONE);
        assert_eq!(m.ribbon_run, RibbonRun::NONE);
        assert!(!ribbon_present(&m));
    }
}

#[test]
fn subthreshold_horizontal_spark_is_not_ribbon_presence() {
    let spark: Vec<_> = (4..11).map(|x| (x, 3, bgr(0, 80, 240))).collect();
    let m = metrics(20, 8, &spark);
    assert_eq!(m.ribbon_component, Shape::NONE);
    assert!(!ribbon_present(&m));
}

#[test]
fn fast_circular_max_matches_the_exact_oracle_including_wrap() {
    let brute = |d: &[f64]| {
        let mut best = 0.0_f64;
        for (i, &a) in d.iter().enumerate() {
            for &b in &d[i + 1..] {
                best = best.max(circular_hue_distance_degrees(a, b));
            }
        }
        best
    };
    let cases: [&[f64]; 9] = [
        &[],
        &[59.0],
        &[59.0, 61.0],
        &[45.0, 75.0],
        &[350.0, 10.0],
        &[0.0, 180.0],
        &[0.0, 100.0, 200.0],
        &[359.0, 1.0, 179.0, 181.0],
        &[12.0, 12.0, 12.0, 191.0, 350.0],
    ];
    for degrees in cases {
        assert!(
            (max_circular_hue_distance_degrees(degrees) - brute(degrees)).abs() < 1e-9,
            "{degrees:?}"
        );
    }
}

#[test]
fn the_row_hue_witness_needs_one_object_and_visible_separation() {
    let run = |xs: std::ops::Range<usize>, hue: f64| -> Vec<(usize, usize, Rgb)> {
        xs.map(|x| (x, 0, at_hue(hue))).collect()
    };
    let band = run(0..5, 280.0);
    #[expect(clippy::type_complexity, reason = "a test's case table")]
    let cases: Vec<(&str, usize, usize, Vec<(usize, usize, Rgb)>, bool)> = vec![
        (
            "adjacent bucket edges",
            2,
            1,
            vec![(0, 0, at_hue(59.0)), (1, 0, at_hue(61.0))],
            false,
        ),
        (
            "visibly separated",
            2,
            1,
            vec![(0, 0, at_hue(45.0)), (1, 0, at_hue(75.0))],
            true,
        ),
        (
            "two objects",
            8,
            1,
            vec![(0, 0, at_hue(45.0)), (5, 0, at_hue(200.0))],
            false,
        ),
        (
            "a seam-split slab run",
            40,
            1,
            [run(0..3, 40.0), run(36..39, 220.0)].concat(),
            true,
        ),
        (
            "a caret block beside a young band",
            16,
            8,
            [band.clone(), block(6, 0, 4, 8, at_hue(140.0))].concat(),
            false,
        ),
        (
            "the same block flattened to slab height",
            16,
            8,
            [band, block(6, 0, 4, 2, at_hue(140.0))].concat(),
            true,
        ),
        (
            "a detached single pixel",
            24,
            1,
            [run(0..7, 120.0), vec![(17, 0, at_hue(270.0))]].concat(),
            false,
        ),
        (
            "a diagonal neighbour",
            4,
            2,
            vec![(0, 0, at_hue(45.0)), (1, 1, at_hue(200.0))],
            false,
        ),
        (
            "an object walked across rows",
            3,
            2,
            vec![
                (0, 0, at_hue(45.0)),
                (0, 1, at_hue(45.0)),
                (1, 1, at_hue(200.0)),
            ],
            true,
        ),
    ];
    for (name, w, h, colored, witnesses) in cases {
        let m = metrics(w, h, &colored);
        assert_eq!(
            m.row_hue_separation >= ROW_RAINBOW_MIN_HUE_DEGREES,
            witnesses,
            "{name}: separation {}",
            m.row_hue_separation
        );
    }
    for colored in [
        [(0, 0, at_hue(59.0)), (1, 0, at_hue(61.0))],
        [(0, 0, at_hue(45.0)), (5, 0, at_hue(200.0))],
    ] {
        let m = metrics(8, 1, &colored);
        assert_eq!((m.ink, m.row_hue_peak), (2, 2));
    }
}

#[test]
fn a_young_tall_band_is_lit_though_it_is_not_thin_geometry() {
    let band: Vec<_> = (4..14)
        .flat_map(|x| (2 + x % 3..2 + x % 3 + 5).map(move |y| (x, y, LIT)))
        .collect();
    let m = metrics(24, 20, &band);
    assert_eq!(
        m.ribbon_component,
        Shape::NONE,
        "the young band is past the thin arm's cap"
    );
    assert_eq!(m.ribbon_run, RibbonRun::NONE);
    assert!(!ribbon_present(&m));
    assert_eq!(m.band_ink, 50);
    assert!(driven_lit(&m));
}

#[test]
fn a_lone_caret_block_is_never_lit() {
    let m = metrics(30, 20, &block(3, 2, 7, 14, LIT));
    assert_eq!(m.ink, 98);
    assert_eq!(m.band_ink, 0);
    assert!(!driven_lit(&m));
}

#[test]
fn a_companion_coat_lends_the_population_arm_nothing() {
    let m = metrics(34, 24, &block(4, 2, 24, 18, LIT));
    assert_eq!(m.ink, 24 * 18);
    assert_eq!(m.band_ink, 0, "an 18 px body is out by domain");
}

#[test]
fn a_subthreshold_blob_cannot_buy_continuity() {
    let m = metrics(24, 20, &block(4, 3, 3, 2, LIT));
    assert!(!ribbon_present(&m));
    assert_eq!(m.band_ink, 6);
    assert!(m.band_ink < RIBBON_LIT_MIN_PX);
    assert!(!driven_lit(&m));
}

#[test]
fn geometry_alone_still_lights_a_frame() {
    let m = DynamicMetrics {
        ribbon_run: RibbonRun::new(14, 14, 5),
        ..DynamicMetrics::default()
    };
    assert!(ribbon_present(&m));
    assert_eq!(m.band_ink, 0);
    assert!(driven_lit(&m));
}

fn baseline_take(first: &[(usize, usize, Rgb)]) -> ScanReport {
    let tail = [(3, 2, at_hue(45.0))];
    let frames = [
        frame(24, 4, &[]),
        frame(24, 4, first),
        frame(24, 4, first),
        frame(24, 4, &tail),
        frame(24, 4, &tail),
        frame(24, 4, &tail),
    ];
    let rows = rows_of(
        &[0, 1, 2, 3, 4, 4]
            .iter()
            .map(|n| {
                format!(
                    "trail pet_active=false cat_active=false ribbon_hue_bands=8 spawns={n} speed=0.0"
                )
            })
            .collect::<Vec<_>>(),
    );
    scan_paired(&frames, rows)
}

#[test]
fn a_claim_on_the_baseline_frame_is_never_charged() {
    let half = RIBBON_LIT_MIN_PX / 2;
    let banded: Vec<_> = (0..half)
        .map(|x| (x, 0, at_hue(45.0)))
        .chain((half..RIBBON_LIT_MIN_PX).map(|x| (x, 0, at_hue(200.0))))
        .collect();
    let r = baseline_take(&banded);
    assert_eq!(r.ribbon_claimed, 6);
    assert_eq!(
        r.ribbon_bound, 3,
        "the claim covered frame 0, so a charge there would show"
    );
    assert_eq!(r.ribbon_dark, 0);
}

#[test]
fn the_exemption_stops_at_frame_zero() {
    let r = baseline_take(&[(0, 0, at_hue(45.0)), (1, 0, at_hue(45.0))]);
    assert_eq!(r.ribbon_bound, 3);
    assert_eq!(r.ribbon_dark, 2);
}

fn single_bound_frame(colored: &[(usize, usize, Rgb)], pet: bool) -> ScanReport {
    let mut frames = vec![frame(60, 30, &[])];
    frames.extend(std::iter::repeat_n(frame(60, 30, colored), 5));
    scan_paired(&frames, claiming_rows(6, pet, None))
}

fn tall_caret() -> Vec<(usize, usize, Rgb)> {
    block(40, 2, 7, 14, at_hue(260.0))
}

#[test]
fn a_bound_frame_is_lit_only_by_band_ink() {
    let coat = [
        block(5, 2, 14, 12, at_hue(10.0)),
        block(19, 8, 14, 12, at_hue(350.0)),
    ]
    .concat();
    for (name, colored, pet, dark) in [
        (
            "the tall decay trough",
            [tall_caret(), block(20, 16, 5, 2, at_hue(120.0))].concat(),
            false,
            0,
        ),
        (
            "the merged tall body",
            [tall_caret(), block(33, 6, 7, 10, at_hue(120.0))].concat(),
            false,
            0,
        ),
        ("a caret only", tall_caret(), false, 2),
        ("a coat only", [tall_caret(), coat].concat(), true, 2),
        (
            "a grey band",
            [tall_caret(), block(10, 16, 30, 2, [128, 128, 128])].concat(),
            false,
            2,
        ),
    ] {
        let r = single_bound_frame(&colored, pet);
        assert_eq!((r.ribbon_bound, r.ribbon_dark), (3, dark), "{name}");
    }
}

#[test]
fn a_blanked_layer_mid_take_charges_its_full_run() {
    let band = [tall_caret(), block(20, 16, 10, 2, at_hue(120.0))].concat();
    let mut frames = vec![frame(60, 30, &[]), frame(60, 30, &band)];
    frames.extend(std::iter::repeat_n(frame(60, 30, &[]), 6));
    let r = scan_paired(&frames, claiming_rows(8, false, None));
    assert_eq!((r.ribbon_bound, r.ribbon_dark), (5, 3));
}

const ANIMAL_W: usize = 90;
const ANIMAL_H: usize = 30;
const ANIMAL_BOX: PixelBox = PixelBox::new(50, 4, 71, 21);
fn charcoal() -> Rgb {
    bgr(52, 52, 52)
}
fn orange() -> Rgb {
    bgr(20, 65, 95)
}

fn animal() -> Vec<(usize, usize, Rgb)> {
    [
        block(50, 4, 22, 12, charcoal()),
        block(68, 16, 4, 6, charcoal()),
    ]
    .concat()
}

fn fragment() -> Vec<(usize, usize, Rgb)> {
    block(54, 6, 12, 9, orange())
}

#[test]
fn the_box_sibling_frames_exactly_the_companion_component() {
    let f = frame(ANIMAL_W, ANIMAL_H, &animal());
    for frame_no in [0, 4, 9] {
        let shape = companion_component(&f, [0, 0, 0], true, frame_no, 10, None, false);
        let bbox = companion_component_box(&f, [0, 0, 0], true, frame_no, 10, None, false);
        assert_eq!(shape, Shape::new(288, 22, 18));
        assert_eq!(bbox, Some(ANIMAL_BOX));
    }
}

#[test]
fn a_box_that_holds_the_band_whole_refuses_it() {
    let sat = sat_patch(ANIMAL_W, 55, 19, 64, 20, 120.0);
    assert_eq!(
        band_ink_partition(ANIMAL_W, ANIMAL_H, &sat, 14, &[ANIMAL_BOX]),
        (0, HueSet::default(), 20)
    );
}

fn located(colored: &[(usize, usize, Rgb)], body: PixelBox, frame_no: usize) -> Shape {
    located_pet_component(
        &frame(ANIMAL_W, ANIMAL_H, colored),
        [0, 0, 0],
        Some(body),
        frame_no,
        10,
        None,
        None,
    )
}

#[test]
fn body_identity_keeps_strict_and_isolated_morphology_floors() {
    assert_eq!(located(&animal(), ANIMAL_BOX, 4), Shape::new(288, 22, 18));
    let compact = block(50, 4, 16, 9, charcoal());
    assert_eq!(located(&compact, ANIMAL_BOX, 4), Shape::NONE);
    assert_eq!(located(&compact, ANIMAL_BOX, 0), Shape::new(144, 16, 9));
    assert_eq!(located(&compact, ANIMAL_BOX, 9), Shape::new(144, 16, 9));
    for pixels in [
        vec![],
        block(50, 4, 7, 14, orange()),
        block(50, 4, 3, 3, orange()),
    ] {
        for frame_no in [0, 4, 9] {
            assert_eq!(located(&pixels, ANIMAL_BOX, frame_no), Shape::NONE);
        }
    }
}

#[test]
fn pixels_outside_the_emitted_rectangle_cannot_supply_missing_height() {
    assert_eq!(
        located(&animal(), PixelBox::new(50, 4, 71, 14), 4),
        Shape::NONE
    );
    assert_eq!(
        located(&animal(), PixelBox::new(0, 0, 39, 29), 4),
        Shape::NONE
    );
    assert_eq!(
        located(&animal(), PixelBox::new(90, 30, 100, 40), 4),
        Shape::NONE
    );
}

fn wired(colored: &[(usize, usize, Rgb)], pet: bool, body: Option<&str>) -> ScanReport {
    let mut frames = vec![frame(ANIMAL_W, ANIMAL_H, &[])];
    frames.extend(std::iter::repeat_n(frame(ANIMAL_W, ANIMAL_H, colored), 5));
    scan_paired(&frames, claiming_rows(6, pet, body))
}

fn wired_caret() -> Vec<(usize, usize, Rgb)> {
    block(4, 2, 7, 14, at_hue(260.0))
}

#[test]
fn a_coat_fragment_cannot_stand_in_for_a_missing_ribbon() {
    let r = wired(&[wired_caret(), animal(), fragment()].concat(), true, None);
    assert_eq!(r.ribbon_bound, 3);
    assert_eq!(r.companion_fused, 0);
    assert_eq!(
        (
            r.per_frame[1].metrics.band_ink,
            r.per_frame[1].metrics.animal_ink
        ),
        (0, 108)
    );
    assert_eq!(r.ribbon_dark, 2);
}

#[test]
fn the_emitted_body_identity_separates_a_pet_from_a_larger_typed_row() {
    let row = block(4, 2, 36, 24, charcoal());
    let band = block(8, 19, 23, 2, at_hue(120.0));
    for coat in [vec![], fragment()] {
        let animal_ink = if coat.is_empty() { 0 } else { 108 };
        let r = wired(
            &[row.clone(), animal(), coat.clone(), band.clone()].concat(),
            true,
            Some("50,72,4,22"),
        );
        assert_eq!(r.per_frame[1].companion, Shape::new(288, 22, 18));
        assert_eq!(
            (
                r.per_frame[1].metrics.band_ink,
                r.per_frame[1].metrics.animal_ink
            ),
            (46, animal_ink)
        );
        assert_eq!(r.ribbon_dark, 0);
        assert_eq!(r.pet_missing, 1, "the blank baseline");
        let dark = wired(
            &[row.clone(), animal(), coat].concat(),
            true,
            Some("50,72,4,22"),
        );
        assert_eq!(dark.ribbon_dark, 2);
        assert_eq!(
            (
                dark.per_frame[1].metrics.band_ink,
                dark.per_frame[1].metrics.animal_ink
            ),
            (0, animal_ink)
        );
    }
    assert_eq!(
        wired(&[row.clone(), animal(), band].concat(), true, None).ribbon_dark,
        2
    );
    assert_eq!(
        wired(&[row, animal(), fragment()].concat(), true, None).ribbon_dark,
        0
    );
}

#[test]
fn a_body_claim_requires_pixels_at_that_body() {
    let row = block(4, 2, 36, 24, charcoal());
    let band = block(8, 19, 23, 2, at_hue(120.0));
    for body in ["50,72,4,22", "none"] {
        let r = wired(&[row.clone(), band.clone()].concat(), true, Some(body));
        assert_eq!(r.pet_missing, 6, "{body}");
        assert_eq!(r.companion_frames, 0, "{body}");
        assert_eq!(r.ribbon_dark, 0, "{body}");
    }
}

#[test]
fn known_body_geometry_excludes_coat_even_when_fused_with_a_band() {
    let band = block(12, 16, 78, 3, at_hue(120.0));
    let r = wired(
        &[wired_caret(), animal(), fragment(), band].concat(),
        true,
        Some("50,72,4,22"),
    );
    assert_eq!(
        (
            r.per_frame[1].metrics.band_ink,
            r.per_frame[1].metrics.animal_ink
        ),
        (234, 108)
    );
    assert_eq!(r.ribbon_dark, 0);
    assert_eq!(r.companion_fused, 0);
}

#[test]
fn body_identity_does_not_hide_a_second_animal_outside_the_rectangle() {
    let second: Vec<_> = animal()
        .into_iter()
        .map(|(x, y, c)| (x - 40, y, c))
        .collect();
    let r = wired(&[animal(), second].concat(), true, Some("50,72,4,22"));
    assert_eq!(r.pet_missing, 1);
    assert_eq!(r.companion_stack, 3, "all three final isolated stills");
}

#[test]
fn a_band_crossing_the_animal_s_box_keeps_the_frame_lit() {
    let band = block(10, 19, 57, 2, at_hue(120.0));
    let r = wired(
        &[wired_caret(), animal(), fragment(), band].concat(),
        true,
        None,
    );
    assert_eq!(r.ribbon_bound, 3);
    assert_eq!(
        (
            r.per_frame[1].metrics.band_ink,
            r.per_frame[1].metrics.animal_ink
        ),
        (114, 108)
    );
    assert_eq!(r.ribbon_dark, 0);
}

#[test]
fn an_unclaimed_plane_excludes_nothing() {
    let r = wired(&[wired_caret(), animal(), fragment()].concat(), false, None);
    assert_eq!(r.ribbon_bound, 3);
    assert_eq!(
        (
            r.per_frame[1].metrics.band_ink,
            r.per_frame[1].metrics.animal_ink
        ),
        (108, 0)
    );
    assert_eq!(r.ribbon_dark, 0);
}

#[test]
fn a_fused_companion_s_box_is_the_row_s_and_excludes_nothing() {
    let band = block(12, 16, 78, 3, at_hue(120.0));
    let colored = [wired_caret(), animal(), fragment(), band].concat();
    let r = wired(&colored, true, None);
    assert_eq!(r.ribbon_bound, 3);
    assert_eq!(r.companion_fused, 5);
    let f1 = &r.per_frame[1];
    assert_eq!(f1.companion, Shape::new(510, 78, 18));
    assert!(f1.notes.contains(&FrameNote::CompanionFused {
        width: 78,
        limit: 42
    }));
    assert_eq!((f1.metrics.band_ink, f1.metrics.animal_ink), (342, 0));
    assert_eq!(r.ribbon_dark, 0);
    let plane = frame(ANIMAL_W, ANIMAL_H, &colored);
    let fused_box = companion_component_box(&plane, [0, 0, 0], true, 1, 6, None, false)
        .expect("the fused component");
    let sat: SatMap = colored
        .iter()
        .filter_map(|&(x, y, c)| saturated_hue_degrees(c).map(|d| (y * ANIMAL_W + x, d)))
        .collect();
    let (band_ink, _, animal_ink) = band_ink_partition(ANIMAL_W, ANIMAL_H, &sat, 14, &[fused_box]);
    assert_eq!((band_ink, animal_ink), (0, 342));
}

const NOMINAL: i64 = 16_000;

fn index_of(count: usize, holes: &[i64], ticks: Option<Vec<i64>>) -> (RecordingIndex, Vec<String>) {
    let start = 1_000_000;
    let mut t = start;
    let mut stamps = BTreeMap::new();
    let mut names = Vec::new();
    for n in 0..count {
        let name = format!("frame_{n:04}.png");
        stamps.insert(name.clone(), t);
        names.push(name);
        t += holes.get(n).copied().unwrap_or(NOMINAL);
    }
    (
        RecordingIndex {
            stamps,
            covered_from: Some(start),
            ticks: ticks.unwrap_or_default(),
        },
        names,
    )
}

fn measure(
    flags: &[bool],
    driven: Option<(i64, i64)>,
    holes: &[i64],
    duplicates: Option<&[bool]>,
    ticks: Option<Vec<i64>>,
) -> DrivenWindow {
    let (index, names) = index_of(flags.len(), holes, ticks);
    let none = vec![false; flags.len()];
    driven_window_darkness(
        Some(&index),
        driven,
        &names,
        flags,
        duplicates.unwrap_or(&none),
    )
}

fn measured(flags: &[bool]) -> DrivenWindow {
    measure(flags, Some((0, 10_000)), &[], None, None)
}

fn lit(runs: &[(bool, usize)]) -> Vec<bool> {
    runs.iter()
        .flat_map(|&(f, n)| std::iter::repeat_n(f, n))
        .collect()
}

#[test]
fn the_driven_window_charges_only_dark_inside_the_mature_window() {
    for (name, runs, dark, lead) in [
        ("an unbroken ribbon", vec![(true, 20)], 0, 0),
        (
            "pre-key onset latency",
            vec![(false, 6), (true, 14)],
            0,
            6 * NOMINAL,
        ),
        ("a dead effect", vec![(false, 20)], 20 * NOMINAL, 0),
        (
            "an interior blackout after maturity",
            vec![(false, 5), (true, 2), (false, 3), (true, 10)],
            3 * NOMINAL,
            5 * NOMINAL,
        ),
        (
            "a ribbon dying mid-take owes to the window's close",
            vec![(true, 5), (false, 45)],
            45 * NOMINAL - EXHALE_CLOSE_ALLOWANCE_US,
            0,
        ),
        (
            "the authored exhale at the close",
            vec![(true, 20), (false, 10)],
            0,
            0,
        ),
        (
            "an interior blackout cannot use the close allowance",
            vec![(true, 5), (false, 10), (true, 5)],
            10 * NOMINAL,
            0,
        ),
        (
            "a lone glint cannot open the mature window",
            vec![(false, 2), (true, 1), (false, 4), (true, 13)],
            0,
            7 * NOMINAL,
        ),
    ] {
        let d = measured(&lit(&runs));
        assert_eq!((d.dark_us, d.mature_lead_us), (dark, lead), "{name}");
    }
    for blackout in [1, 2, 3, 5] {
        let mut flags = vec![true; 20];
        flags[8..8 + blackout].fill(false);
        assert_eq!(
            measured(&flags).dark_us,
            blackout as i64 * NOMINAL,
            "{blackout}"
        );
    }
}

#[test]
fn a_sampling_hole_between_lit_frames_is_never_charged_as_dark() {
    let mut holes = vec![NOMINAL; 20];
    holes[9] = 200_000;
    let d = measure(&[true; 20], Some((0, 10_000)), &holes, None, None);
    assert_eq!((d.dark_us, d.sampling_hole_us), (0, 200_000));
}

#[test]
fn a_static_windows_frame_gap_is_not_a_hole_when_the_ledger_ticked() {
    let mut holes = vec![NOMINAL; 20];
    holes[9] = 3 * NOMINAL;
    let ticks: Vec<i64> = (0..22).map(|i| 1_000_000 + i * NOMINAL).collect();
    let with = measure(&[true; 20], Some((0, 10_000)), &holes, None, Some(ticks));
    assert_eq!(
        with.sampling_hole_us, NOMINAL,
        "the ledger shows no missed sample"
    );
    let without = measure(&[true; 20], Some((0, 10_000)), &holes, None, None);
    assert_eq!(without.sampling_hole_us, 3 * NOMINAL);
}

fn frame_times(holes: &[i64]) -> Vec<i64> {
    let mut times = vec![1_000_000];
    for h in &holes[..holes.len() - 1] {
        times.push(times.last().copied().unwrap_or_default() + h);
    }
    times
}

#[test]
fn a_starved_ledger_still_reads_as_a_hole() {
    let mut holes = vec![NOMINAL; 20];
    holes[9] = 200_000;
    let d = measure(
        &[true; 20],
        Some((0, 10_000)),
        &holes,
        None,
        Some(frame_times(&holes)),
    );
    assert_eq!(d.sampling_hole_us, 200_000);
}

#[test]
fn a_render_stall_under_a_live_loop_is_still_a_hole() {
    let mut holes = vec![NOMINAL; 20];
    holes[9] = 200_000;
    let times = frame_times(&holes);
    let (lo, hi) = (times[9], times[10]);
    let ticks: Vec<i64> = (1_000_000..=times[19])
        .step_by(NOMINAL as usize)
        .filter(|&t| !(lo < t && t < hi))
        .collect();
    let d = measure(&[true; 20], Some((0, 10_000)), &holes, None, Some(ticks));
    assert!(d.sampling_hole_us >= 200_000 - NOMINAL, "{d:?}");
}

#[test]
fn a_stall_beside_one_dark_frame_charges_only_that_frame() {
    let mut flags = vec![true; 20];
    flags[10] = false;
    let mut holes = vec![NOMINAL; 20];
    holes[9] = 200_000;
    assert_eq!(
        measure(&flags, Some((0, 10_000)), &holes, None, None).dark_us,
        NOMINAL
    );
}

#[test]
fn a_duplicate_cannot_open_the_mature_window() {
    let flags = lit(&[(false, 4), (true, 2), (false, 4), (true, 10)]);
    let mut duplicates = vec![false; 20];
    duplicates[5] = true;
    let d = measure(&flags, Some((0, 10_000)), &[], Some(&duplicates), None);
    assert_eq!((d.dark_us, d.mature_lead_us), (0, 10 * NOMINAL));
}

#[test]
fn darkness_outside_the_driven_window_is_not_the_rows_business() {
    let d = measure(
        &lit(&[(true, 10), (false, 20)]),
        Some((0, 5)),
        &[],
        None,
        None,
    );
    assert_eq!((d.dark_us, d.frames), (0, 10));
}

#[test]
fn no_bracket_reports_not_measured_rather_than_zero() {
    let d = measure(&[true; 20], None, &[], None, None);
    assert_eq!((d.dark_us, d.frames), (-1, 0));
}

#[test]
fn no_index_reports_not_measured() {
    assert_eq!(
        driven_window_darkness(
            None,
            Some((0, 10_000)),
            &["frame_0000.png".into()],
            &[true],
            &[false]
        ),
        DrivenWindow::NOT_MEASURED
    );
}

#[test]
fn the_recording_index_parses_stamps_coverage_and_integer_ticks() {
    let index = RecordingIndex::parse(
        br#"{"meta":{"covered_us":[1000,9000]},"frames":[{"file":"frame_0000.png","seq":0,"t_us":1000},{"file":"frame_0001.png","t_us":1016}],"ticks":[1008,1012.5,"x"]}"#,
    )
    .expect("parses");
    assert_eq!(index.covered_from, Some(1000));
    assert_eq!(index.stamps.get("frame_0001.png"), Some(&1016));
    assert_eq!(index.ticks, [1008]);
    assert!(RecordingIndex::parse(b"[]").is_err());
}

fn recorded_take(band_frames: &[usize]) -> ScanReport {
    let mut frames = vec![frame(40, 4, &[])];
    let mut stamps = BTreeMap::new();
    let mut names = vec!["shot_0000.png".to_string()];
    stamps.insert(names[0].clone(), 1_000_000);
    for n in 1..7 {
        let colored: Vec<_> = if band_frames.contains(&n) {
            (4..4 + RIBBON_LIT_MIN_PX + n)
                .map(|x| (x, 2, at_hue(120.0)))
                .collect()
        } else {
            vec![]
        };
        frames.push(frame(40, 4, &colored));
        let name = format!("shot_{n:04}.png");
        stamps.insert(name.clone(), 1_000_000 + n as i64 * NOMINAL);
        names.push(name);
    }
    let ctx = TakeContext {
        cell: CELL,
        statuses: None,
        index: Some(RecordingIndex {
            stamps,
            covered_from: Some(1_000_000),
            ticks: vec![],
        }),
        driven_ms: Some((0, 96)),
    };
    scan_frames(&names, frames.into_iter().map(Ok), &ctx).expect("scans")
}

#[test]
fn a_mono_hue_band_is_continuous_however_dark_the_hue_witness() {
    let r = recorded_take(&[1, 2, 3, 4, 5, 6]);
    assert_eq!(r.union_hues, 1);
    assert_eq!(r.rainbow_frames, 6);
    assert_eq!(r.driven.dark_us, 0);
}

#[test]
fn a_vanished_band_is_still_a_blackout_through_the_same_wiring() {
    assert_eq!(recorded_take(&[1, 2, 5, 6]).driven.dark_us, 2 * NOMINAL);
}

#[test]
fn a_take_scans_from_png_files_through_the_one_decoder() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let band: Vec<_> = (4..20).map(|x| (x, 3, at_hue(120.0))).collect();
    for (n, colored) in [vec![], band.clone(), band].iter().enumerate() {
        let f = frame(24, 8, colored);
        let mut bytes = Vec::new();
        {
            let mut enc = aterm_png::Encoder::new(&mut bytes, 24, 8);
            enc.set_color(aterm_png::ColorType::Rgb);
            enc.set_depth(aterm_png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("header");
            let rgb: Vec<u8> = (0..24 * 8).flat_map(|p| f.pixel(p)).collect();
            writer.write_image_data(&rgb).expect("data");
            writer.finish().expect("finish");
        }
        std::fs::write(dir.path().join(format!("shot_{n:04}.png")), bytes).expect("write");
    }
    let ctx = TakeContext {
        cell: CELL,
        statuses: None,
        index: None,
        driven_ms: None,
    };
    let r = scan_dir(dir.path(), &ctx).expect("scans");
    assert_eq!(r.frames, 3);
    assert_eq!(r.total_ink, 16, "frame 2 is a duplicate and adds no ink");
    assert_eq!(
        r.per_frame[1].metrics.ribbon_component,
        Shape::new(16, 16, 1)
    );
    assert!(r.per_frame[2].duplicate);
}

#[test]
fn a_take_with_mismatched_rows_or_sizes_is_refused() {
    let names: Vec<String> = vec!["a.png".into(), "b.png".into()];
    let ctx = TakeContext {
        cell: CELL,
        statuses: Some(vec![StatusRow::new("trail spawns=0")]),
        index: None,
        driven_ms: None,
    };
    let frames = [frame(4, 4, &[]), frame(4, 4, &[])];
    assert!(matches!(
        scan_frames(&names, frames.iter().cloned().map(Ok), &ctx),
        Err(ScanError::StatusCount { rows: 1, frames: 2 })
    ));
    let ctx = TakeContext {
        statuses: None,
        ..ctx
    };
    let frames = [frame(4, 4, &[]), frame(5, 4, &[])];
    assert!(matches!(
        scan_frames(&names, frames.into_iter().map(Ok), &ctx),
        Err(ScanError::SizeMismatch { .. })
    ));
}

struct Fixture {
    frames: BTreeMap<&'static str, Frame>,
    rows: Vec<StatusRow>,
    names: &'static [&'static str],
}

impl Fixture {
    fn load(dir: &str, names: &'static [&'static str]) -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/paint")
            .join(dir);
        let frames = names
            .iter()
            .map(|n| {
                (
                    *n,
                    Frame::load(&root.join(format!("{n}.png"))).expect("fixture frame"),
                )
            })
            .collect();
        let rows: Vec<StatusRow> = std::fs::read_to_string(root.join("trail-status.rows"))
            .expect("fixture rows")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| StatusRow::new(l.trim()))
            .collect();
        assert_eq!(rows.len(), names.len());
        Self {
            frames,
            rows,
            names,
        }
    }

    fn frame(&self, name: &str) -> &Frame {
        &self.frames[name]
    }

    fn row(&self, name: &str) -> &StatusRow {
        &self.rows[self
            .names
            .iter()
            .position(|n| *n == name)
            .expect("named frame")]
    }

    fn background(&self) -> Rgb {
        frame_background(self.frame("shot_0000"))
    }

    fn scan(&self, order: &[usize], rewrite: impl Fn(&str) -> String) -> ScanReport {
        let frames: Vec<Frame> = order
            .iter()
            .map(|&i| self.frames[self.names[i]].clone())
            .collect();
        let rows = order
            .iter()
            .map(|&i| StatusRow::new(rewrite(&self.rows[i].0)))
            .collect();
        scan_paired(&frames, rows)
    }

    fn located(
        &self,
        name: &str,
        frame_no: usize,
        count: usize,
        pose: Option<&str>,
        cell_w: Option<usize>,
    ) -> Shape {
        located_pet_component(
            self.frame(name),
            self.background(),
            self.row(name).pet_body().expect("body"),
            frame_no,
            count,
            pose,
            cell_w,
        )
    }
}

fn grey_sleeper() -> Fixture {
    Fixture::load(
        "grey-sleeper-2026-09-12",
        &["shot_0000", "shot_0011", "shot_0022"],
    )
}

const SLEEPER_ORDER: [usize; 6] = [0, 1, 1, 2, 2, 2];

#[test]
fn the_sleeper_is_located_on_the_mid_take_frames_it_is_claimed_on() {
    let r = grey_sleeper().scan(&SLEEPER_ORDER, str::to_string);
    assert_eq!(
        (
            r.pet_claimed,
            r.pet_missing,
            r.companion_frames,
            r.pet_final_run,
            r.companion_stack
        ),
        (6, 0, 6, 6, 0)
    );
    let mid = &r.per_frame[1];
    assert_eq!(mid.companion, Shape::new(209, 24, 13));
    assert!(mid.notes.contains(&FrameNote::RestingPoseWitness {
        pose: Some("pet_sleep_0".into())
    }));
    assert_eq!((mid.metrics.band_ink, mid.metrics.animal_ink), (0, 6));
    for (i, shape) in [(0, Shape::new(215, 24, 14)), (5, Shape::new(226, 24, 15))] {
        let f = &r.per_frame[i];
        assert_eq!(f.companion, shape, "frame {i}");
        assert!(f.notes.contains(&FrameNote::CompactPetWitness), "frame {i}");
        assert!(
            !f.notes
                .iter()
                .any(|n| matches!(n, FrameNote::RestingPoseWitness { .. })),
            "frame {i}"
        );
    }
}

#[test]
fn the_pose_claim_is_load_bearing_on_the_wired_scanner() {
    let fx = grey_sleeper();
    let awake = fx.scan(&SLEEPER_ORDER, |row| {
        row.replace("pet_pose=pet_sleep_0", "pet_pose=pet_sit")
            .replace("pet_pose=pet_sleep_1", "pet_pose=pet_sit")
    });
    assert_eq!(awake.pet_missing, 2);
    assert_eq!(awake.per_frame[1].companion, Shape::NONE);
    assert!(
        awake.per_frame[1]
            .notes
            .contains(&FrameNote::CompanionMissing)
    );
    let blank = fx.scan(&SLEEPER_ORDER, |row| {
        let body = StatusRow::new(row)
            .value("pet_body")
            .unwrap_or_default()
            .to_string();
        row.replace(&format!("pet_body={body}"), "pet_body=none")
    });
    assert_eq!((blank.pet_missing, blank.companion_frames), (6, 0));
}

#[test]
fn the_pose_claim_selects_the_floor_inside_the_emitted_rect() {
    let fx = grey_sleeper();
    assert_eq!(
        fx.located("shot_0011", 11, 25, Some("pet_sleep_0"), Some(7)),
        Shape::new(209, 24, 13)
    );
    for pose in [
        Some("pet_sit"),
        Some("pet_walk_0"),
        Some("pet_stand"),
        Some("pet_loaf"),
        Some("pet_dog_sit"),
        None,
    ] {
        assert_eq!(
            fx.located("shot_0011", 11, 25, pose, Some(7)),
            Shape::NONE,
            "{pose:?}"
        );
    }
    assert_eq!(
        fx.located("shot_0011", 11, 25, Some("pet_sleep_0"), None),
        Shape::NONE
    );
    assert_eq!(
        fx.located("shot_0000", 0, 25, None, None),
        Shape::new(215, 24, 14)
    );
    assert_eq!(
        fx.located("shot_0022", 22, 25, None, None),
        Shape::new(226, 24, 15)
    );
}

const SLEEPER_CELL: PixelBox = PixelBox::new(33, 146, 72, 169);

fn sleeper_synthetic(colored: &[(usize, usize, Rgb)], pose: Option<&str>, cell_w: usize) -> Shape {
    located_pet_component(
        &frame(120, 200, colored),
        [0, 0, 0],
        Some(SLEEPER_CELL),
        4,
        10,
        pose,
        Some(cell_w),
    )
}

fn sleeper_shaped() -> Vec<(usize, usize, Rgb)> {
    block(36, 156, 24, 14, charcoal())
}

#[test]
fn the_positive_control_is_a_sleeper_shaped_body_on_the_cell_floor() {
    for pose in [
        "pet_sleep_0",
        "pet_sleep_1",
        "pet_dog_sleep_0",
        "pet_dog_sleep_1",
        "pet_run_0",
        "pet_dog_run_0",
    ] {
        assert_eq!(
            sleeper_synthetic(&sleeper_shaped(), Some(pose), 7),
            Shape::new(24 * 14, 24, 14),
            "{pose}"
        );
    }
    for pose in [
        Some("pet_sit"),
        Some("pet_walk_2"),
        Some("pet_stand_ear"),
        Some("pet_loaf"),
        Some("pet_dog_run_1"),
        Some("pet_run_3"),
        None,
    ] {
        assert_eq!(
            sleeper_synthetic(&sleeper_shaped(), pose, 7),
            Shape::NONE,
            "{pose:?}"
        );
    }
    let no_cell = located_pet_component(
        &frame(120, 200, &sleeper_shaped()),
        [0, 0, 0],
        None,
        4,
        10,
        Some("pet_sleep_0"),
        Some(7),
    );
    assert_eq!(no_cell, Shape::NONE);
}

#[test]
fn a_glyph_pair_inside_the_cell_is_not_the_sleeper() {
    assert_eq!(
        sleeper_synthetic(&block(45, 160, 13, 9, charcoal()), Some("pet_sleep_0"), 7),
        Shape::NONE
    );
    assert_eq!(
        sleeper_synthetic(&block(45, 156, 13, 14, charcoal()), Some("pet_sleep_0"), 7),
        Shape::NONE
    );
}

#[test]
fn two_touching_cells_are_not_the_sleeper_and_the_edge_is_where_it_is_declared() {
    let sleep = Some("pet_sleep_0");
    assert_eq!(
        sleeper_synthetic(&block(45, 156, 14, 14, charcoal()), sleep, 7),
        Shape::NONE
    );
    assert_eq!(resting_pet_min_width(7), 15);
    assert_eq!(
        sleeper_synthetic(&block(45, 156, 15, 14, charcoal()), sleep, 7),
        Shape::new(15 * 14, 15, 14)
    );
    assert_eq!(
        sleeper_synthetic(&block(45, 156, 15, 14, charcoal()), sleep, 8),
        Shape::NONE
    );
}

#[test]
fn a_row_running_through_the_cell_is_the_row_s_not_the_sleeper_s() {
    let sleep = Some("pet_sleep_0");
    for clipped in [
        block(10, 156, 100, 14, charcoal()),
        block(10, 156, 45, 14, charcoal()),
        block(50, 156, 40, 14, charcoal()),
        block(40, 140, 24, 14, charcoal()),
    ] {
        assert_eq!(sleeper_synthetic(&clipped, sleep, 7), Shape::NONE);
    }
    for (x, y) in [(34, 156), (48, 156), (40, 147)] {
        assert_eq!(
            sleeper_synthetic(&block(x, y, 24, 14, charcoal()), sleep, 7),
            Shape::new(24 * 14, 24, 14),
            "({x}, {y})"
        );
    }
}

fn low_runner() -> Fixture {
    Fixture::load(
        "low-runner-2026-09-12",
        &["shot_0000", "shot_0014", "shot_0019"],
    )
}

#[test]
fn the_runner_is_located_on_the_mid_take_frames_it_is_claimed_on() {
    let r = low_runner().scan(&SLEEPER_ORDER, str::to_string);
    assert_eq!(
        (r.pet_claimed, r.pet_missing, r.companion_frames),
        (6, 0, 6)
    );
    assert_eq!(r.per_frame[1].companion, Shape::new(274, 30, 16));
    assert!(
        r.per_frame[1]
            .notes
            .contains(&FrameNote::RestingPoseWitness {
                pose: Some("pet_run_0".into())
            })
    );
}

#[test]
fn the_runner_s_pose_claim_is_load_bearing() {
    let fx = low_runner();
    let awake = fx.scan(&SLEEPER_ORDER, |row| {
        row.replace("pet_pose=pet_run_0", "pet_pose=pet_run_1")
    });
    assert_eq!(awake.pet_missing, 2);
    assert_eq!(
        fx.located("shot_0014", 5, 30, Some("pet_run_0"), Some(7)),
        Shape::new(274, 30, 16)
    );
    for pose in [
        Some("pet_run_1"),
        Some("pet_run_2"),
        Some("pet_run_3"),
        Some("pet_walk_0"),
        Some("pet_sit"),
        None,
    ] {
        assert_eq!(
            fx.located("shot_0014", 5, 30, pose, Some(7)),
            Shape::NONE,
            "{pose:?}"
        );
    }
    assert_eq!(
        fx.located("shot_0014", 5, 30, Some("pet_run_0"), None),
        Shape::NONE
    );
}

fn fused_band() -> Fixture {
    Fixture::load(
        "fused-band-2026-09-12",
        &[
            "shot_0000",
            "shot_0006",
            "shot_0007",
            "shot_0008",
            "shot_0009",
            "shot_0010",
            "shot_0011",
        ],
    )
}

#[test]
fn the_composed_fused_take_is_not_charged() {
    let r = fused_band().scan(&[0, 1, 2, 3, 4, 5, 6], str::to_string);
    assert!(r.ribbon_bound >= 2);
    assert_eq!(r.ribbon_dark, 0);
    for (i, ink) in [(2, 203), (3, 223)] {
        assert_eq!(r.per_frame[i].metrics.band_ink, ink, "frame {i}");
        assert!(
            !r.per_frame[i]
                .notes
                .iter()
                .any(|n| matches!(n, FrameNote::RibbonUnlit { .. })),
            "frame {i}"
        );
    }
}
