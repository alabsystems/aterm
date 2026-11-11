// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn alpha_at(t: &Tile, x: u32, y: u32) -> u8 {
    t.pixels()[((y * t.width() + x) * 4 + 3) as usize]
}

#[test]
fn even_odd_holes_work() {
    let donut = [
        PathCmd::Move(0.1, 0.1),
        PathCmd::Line(0.9, 0.1),
        PathCmd::Line(0.9, 0.9),
        PathCmd::Line(0.1, 0.9),
        PathCmd::Close,
        PathCmd::Move(0.35, 0.35),
        PathCmd::Line(0.65, 0.35),
        PathCmd::Line(0.65, 0.65),
        PathCmd::Line(0.35, 0.65),
        PathCmd::Close,
    ];
    let mut t = Tile::new(80, 80);
    fill_path(
        &mut t,
        &[&donut],
        (0.0, 0.0, 0.0),
        1.0,
        PathTransform::fit(80, 80),
    );
    assert_eq!(alpha_at(&t, 40, 40), 0, "hole is transparent");
    assert!(alpha_at(&t, 40, 16) > 200, "ring is painted");
    let sq1 = [
        PathCmd::Move(0.1, 0.1),
        PathCmd::Line(0.6, 0.1),
        PathCmd::Line(0.6, 0.6),
        PathCmd::Line(0.1, 0.6),
        PathCmd::Close,
    ];
    let sq2 = [
        PathCmd::Move(0.4, 0.4),
        PathCmd::Line(0.9, 0.4),
        PathCmd::Line(0.9, 0.9),
        PathCmd::Line(0.4, 0.9),
        PathCmd::Close,
    ];
    let mut t2 = Tile::new(80, 80);
    fill_path(
        &mut t2,
        &[&sq1, &sq2],
        (0.0, 0.0, 0.0),
        1.0,
        PathTransform::fit(80, 80),
    );
    assert!(
        alpha_at(&t2, 40, 40) > 200,
        "sibling overlap unions, never cancels"
    );
}

#[test]
fn fill_path_fixed_matches_float_frame() {
    let q = |f: f32| (f * FIXED_ONE as f32) as u16;
    let seg = [
        PathSeg::Move(q(0.125), q(0.75)),
        PathSeg::Line(q(0.875), q(0.75)),
        PathSeg::Cubic(q(0.875), q(0.25), q(0.125), q(0.25), q(0.5), q(0.5)),
        PathSeg::Close,
    ];
    let cmd = [
        PathCmd::Move(0.125, 0.75),
        PathCmd::Line(0.875, 0.75),
        PathCmd::Cubic(0.875, 0.25, 0.125, 0.25, 0.5, 0.5),
        PathCmd::Close,
    ];
    let mut a = Tile::new(48, 48);
    let mut b = Tile::new(48, 48);
    fill_path_fixed(
        &mut a,
        &[&seg],
        (0.2, 0.7, 0.9),
        0.9,
        PathTransform::fit(48, 48),
    );
    fill_path(
        &mut b,
        &[&cmd],
        (0.2, 0.7, 0.9),
        0.9,
        PathTransform::fit(48, 48),
    );
    assert_eq!(
        a.pixels(),
        b.pixels(),
        "fixed-point frame == float frame raster"
    );
    assert!(a.pixels().iter().any(|&x| x != 0), "actually painted");
    let mut t = Tile::new(16, 16);
    let before = t.pixels().to_vec();
    fill_path_fixed(
        &mut t,
        &[],
        (1.0, 1.0, 1.0),
        1.0,
        PathTransform::fit(16, 16),
    );
    fill_path_fixed(
        &mut t,
        &[&[PathSeg::Move(0, 0)]],
        (1.0, 1.0, 1.0),
        1.0,
        PathTransform::fit(16, 16),
    );
    assert_eq!(
        t.pixels(),
        &before[..],
        "degenerate fixed input paints nothing"
    );
}

#[test]
fn degenerate_paths_are_safe() {
    let mut t = Tile::new(16, 16);
    let before = t.pixels().to_vec();
    fill_path(
        &mut t,
        &[],
        (1.0, 1.0, 1.0),
        1.0,
        PathTransform::fit(16, 16),
    );
    let move_only = [PathCmd::Move(0.5, 0.5)];
    let zero_area = [
        PathCmd::Move(0.2, 0.2),
        PathCmd::Line(0.8, 0.2),
        PathCmd::Close,
    ];
    let lone_close = [PathCmd::Close];
    fill_path(
        &mut t,
        &[&move_only, &zero_area, &lone_close, &[]],
        (1.0, 1.0, 1.0),
        1.0,
        PathTransform::fit(16, 16),
    );
    let box_cmd = [
        PathCmd::Move(0.0, 0.0),
        PathCmd::Line(1.0, 0.0),
        PathCmd::Line(1.0, 1.0),
        PathCmd::Line(0.0, 1.0),
        PathCmd::Close,
    ];
    fill_path(
        &mut t,
        &[&box_cmd],
        (1.0, 1.0, 1.0),
        f32::NAN,
        PathTransform::fit(16, 16),
    );
    assert_eq!(t.pixels(), &before[..], "degenerate input paints nothing");
}

#[test]
fn out_of_range_coords_clamp() {
    let mut t = Tile::new(32, 32);
    let wild = [
        PathCmd::Move(-1.0e9, -1.0e9),
        PathCmd::Line(1.0e9, 0.5),
        PathCmd::Line(f32::NAN, f32::INFINITY),
        PathCmd::Close,
    ];
    fill_path(
        &mut t,
        &[&wild],
        (1.0, 0.0, 0.0),
        1.0,
        PathTransform::fit(32, 32),
    );
    let half_off = [
        PathCmd::Move(-0.5, 0.25),
        PathCmd::Line(0.5, 0.25),
        PathCmd::Line(0.5, 0.75),
        PathCmd::Line(-0.5, 0.75),
        PathCmd::Close,
    ];
    let mut t2 = Tile::new(32, 32);
    fill_path(
        &mut t2,
        &[&half_off],
        (0.0, 1.0, 0.0),
        1.0,
        PathTransform::fit(32, 32),
    );
    assert!(alpha_at(&t2, 0, 16) > 200, "clipped fill reaches the edge");
    assert_eq!(alpha_at(&t2, 31, 16), 0, "right half stays empty");
}

#[test]
fn parse_path_roundtrips_the_asset_grammar() {
    let render = |cmds: &[PathCmd]| -> String {
        cmds.iter()
            .map(|cmd| match *cmd {
                PathCmd::Move(x, y) => format!("M {x} {y}"),
                PathCmd::Line(x, y) => format!("L {x} {y}"),
                PathCmd::Cubic(x1, y1, x2, y2, x, y) => {
                    format!("C {x1} {y1} {x2} {y2} {x} {y}")
                }
                PathCmd::Close => "Z".to_string(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let cmds = parse_path("M 0.1 0.2 C 0.3 0.4 0.5 0.6 0.7 0.8 L 0.9 1.0 Z").unwrap();
    assert_eq!(
        cmds,
        vec![
            PathCmd::Move(0.1, 0.2),
            PathCmd::Cubic(0.3, 0.4, 0.5, 0.6, 0.7, 0.8),
            PathCmd::Line(0.9, 1.0),
            PathCmd::Close,
        ]
    );
    assert_eq!(parse_path(&render(&cmds)).as_ref(), Some(&cmds));
    let imp = parse_path("M0 0 1 0 C1 0 1 1 0 1 0 1 0 0 0 0Z").unwrap();
    assert_eq!(
        imp,
        vec![
            PathCmd::Move(0.0, 0.0),
            PathCmd::Line(1.0, 0.0),
            PathCmd::Cubic(1.0, 0.0, 1.0, 1.0, 0.0, 1.0),
            PathCmd::Cubic(0.0, 1.0, 0.0, 0.0, 0.0, 0.0),
            PathCmd::Close,
        ]
    );
    assert_eq!(parse_path(&render(&imp)).as_ref(), Some(&imp));
    let signed = vec![PathCmd::Move(-0.0, -12.375), PathCmd::Line(1e-6, 4096.0)];
    assert_eq!(parse_path(&render(&signed)).as_ref(), Some(&signed));
    assert!(parse_path("M 0.1").is_none(), "dangling args");
    assert!(parse_path("Q 1 2 3 4").is_none(), "unsupported command");
    assert!(parse_path("M x y").is_none(), "bare command before letter");
    assert!(parse_path("M 1.2.3 4").is_none(), "malformed decimal");
    assert!(parse_path("M 1e 2").is_none(), "malformed exponent");
    assert!(parse_path("Z 1 2").is_none(), "numbers after close");
    assert!(parse_path("1 2 3").is_none(), "numbers before a command");
    assert_eq!(parse_path("").unwrap(), vec![]);
}
