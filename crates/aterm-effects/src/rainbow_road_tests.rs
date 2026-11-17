// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The design's own box: 802 units of road past the words, as the approved render lays out.
const LENGTH: u16 = 802;
/// Two device pixels a unit.
const H: u32 = (ROAD_H * 2.0) as u32;

fn road_art(permille: u16, done: bool, dark: bool) -> RoadArt {
    RoadArt {
        length: LENGTH,
        permille,
        done,
        dark,
    }
}

fn road_size(art: RoadArt) -> (u32, u32) {
    ((H as f32 * art.aspect()).round() as u32, H)
}

fn alpha(tile: &Tile, x: u32, y: u32) -> u8 {
    tile.pixels()[((y * tile.width() + x) * 4 + 3) as usize]
}

/// Device column of design `x` in a road bake `H` tall.
fn road_col(x: f32) -> u32 {
    ((x - ROAD_X) * H as f32 / ROAD_H) as u32
}

fn device(v: f32) -> u32 {
    (v * H as f32 / ROAD_H) as u32
}

#[test]
fn every_bake_is_the_same_twice() {
    for dark in [true, false] {
        for (permille, done) in [(0, false), (400, false), (1000, true)] {
            let art = road_art(permille, done, dark);
            let (w, h) = road_size(art);
            assert_eq!(art.bake(w, h).pixels(), art.bake(w, h).pixels(), "{art:?}");
        }
        for pose in CatPose::ALL {
            let art = CatArt {
                pose,
                tilt: -4,
                dark,
            };
            let (w, h) = ((120.0 * CatArt::ASPECT).round() as u32, 120);
            let (a, b) = (art.bake(w, h), art.bake(w, h));
            assert_eq!(a.pixels(), b.pixels(), "{art:?}");
            assert!(
                a.pixels().chunks_exact(4).any(|p| p[3] == 255),
                "{art:?} draws"
            );
        }
    }
}

/// The road grows out of a point after the words: nothing of it reaches back over them (its box
/// starts at [`ROAD_X`]), and its first units draw next to nothing. NEGATIVE CONTROL: a little
/// further on it is the whole ribbon.
#[test]
fn the_road_grows_out_of_a_point_after_the_words() {
    for dark in [true, false] {
        let art = road_art(0, false, dark);
        let (w, h) = road_size(art);
        let tile = art.bake(w, h);
        let column = |x: u32| (0..h).map(|y| alpha(&tile, x, y)).max().unwrap_or(0);
        let first: u32 = (0..device(3.0)).map(|x| u32::from(column(x) > 128)).sum();
        assert_eq!(first, 0, "dark {dark}: the road starts as a point");
        let x = road_col(ROAD_X + 140.0);
        let solid = (0..h).filter(|y| alpha(&tile, x, *y) == 255).count() as u32;
        assert!(
            solid >= device(26.0),
            "dark {dark}: the ribbon is whole further on: {solid}"
        );
    }
}

/// The cat stands where the run is: at `p` of its way, its feet on the ribbon, its front paw
/// short of the nose, which the ribbon reaches and no further.
#[test]
fn the_cat_stands_on_the_ribbon_at_the_runs_progress() {
    let mut last = 0.0;
    for permille in [0, 250, 500, 750, 1000] {
        let art = road_art(permille, false, true);
        let road = art.road();
        let p = f32::from(permille) / 1000.0;
        let end = road.right() - 90.0;
        assert!((road.cat_x() - (330.0 + p * (end - 330.0))).abs() < 0.01);
        assert!(road.cat_x() > last || permille == 0);
        last = road.cat_x();
        let (fx, fy) = road.feet();
        assert!((fy - (centre_y(fx) - 7.0)).abs() < 0.01);
        let (w, h) = road_size(art);
        let tile = art.bake(w, h);
        // Under the feet, the ribbon: opaque from just above the centreline to below it.
        let col = road_col(fx);
        for y in [centre_y(fx) - 10.0, centre_y(fx), centre_y(fx) + 10.0] {
            assert_eq!(
                alpha(&tile, col, device(y)),
                255,
                "{permille}: ribbon at {y}"
            );
        }
        // The ribbon ends at its nose: none a little past it, on the centreline.
        let nose = road.nose();
        assert_eq!(
            alpha(&tile, road_col(nose - 2.0), device(centre_y(nose))),
            255
        );
        let past = road_col(nose + 3.0).min(w - 1);
        assert!(
            alpha(&tile, past, device(centre_y(nose))) < 255,
            "{permille}: past the nose"
        );
        for pose in CatPose::ALL
            .into_iter()
            .filter(|pose| *pose != CatPose::Loaf)
        {
            assert!(
                nose - fx > pose.front_paw() + 5.0,
                "{permille} {pose:?}: the nose runs ahead of the front paw"
            );
        }
    }
    // Done, the loaf lies at the far end with the nose still past its paws.
    let done = road_art(1000, true, true).road();
    assert!(done.nose() - done.cat_x() > CatPose::Loaf.front_paw() + 5.0);
}

/// The cat's own image puts its feet at [`CAT_FEET`]: its lowest opaque row stands within an
/// outline's reach of them, and its front paw reaches [`CatPose::front_paw`] and no further.
#[test]
fn the_cats_feet_stand_where_its_box_says() {
    let (w, h) = ((350.0 * CatArt::ASPECT).round() as u32, 350);
    let unit = h as f32 / CAT_H;
    for pose in CatPose::ALL {
        let tile = CatArt {
            pose,
            tilt: 0,
            dark: true,
        }
        .bake(w, h);
        let lowest = (0..h)
            .rev()
            .find(|y| (0..w).any(|x| alpha(&tile, x, *y) == 255))
            .expect("the cat draws");
        let feet = CAT_FEET.1 * unit;
        assert!(
            (lowest as f32 - feet).abs() <= 4.0 * unit,
            "{pose:?}: lowest {lowest}, feet {feet}"
        );
        // The paws' row, just above the ground: the front paw's edge, no further.
        let row = ((CAT_FEET.1 - 4.0) * unit) as u32;
        let right = (0..w)
            .rev()
            .find(|x| alpha(&tile, *x, row) == 255)
            .expect("paws on the ground");
        let reach = right as f32 / unit - CAT_FEET.0;
        if matches!(pose, CatPose::Walk | CatPose::Loaf) {
            assert!(
                (reach - pose.front_paw()).abs() < 2.0,
                "{pose:?}: the front paw reaches {reach}, not {}",
                pose.front_paw()
            );
        }
    }
}

/// The dots lie ahead of the nose, converging on the star; done, there are none, and the
/// ribbon runs to the far end. NEGATIVE CONTROL: before done, there are dots and they show.
#[test]
fn the_dots_lie_ahead_of_the_nose_and_are_gone_once_done() {
    for permille in [0, 400, 900] {
        let road = road_art(permille, false, true).road();
        let dots = road.dots();
        assert!(!dots.is_empty(), "{permille}");
        assert!(dots.iter().all(|(_, x, _)| *x >= road.nose() - 15.0));
        assert!(dots.iter().all(|(_, x, _)| *x <= road.right() - 27.0));
        assert!((0..6).all(|lane| dots.iter().any(|(l, _, _)| *l == lane)));
    }
    let art = road_art(400, false, true);
    let (w, h) = road_size(art);
    let tile = art.bake(w, h);
    let road = art.road();
    let ahead = (road_col(road.nose() + 20.0)..road_col(road.right() - 60.0))
        .filter(|x| {
            let x_units = ROAD_X + *x as f32 * ROAD_H / H as f32;
            let y = device(centre_y(x_units));
            (y.saturating_sub(device(14.0))..y + device(14.0)).any(|y| alpha(&tile, *x, y) > 0)
        })
        .count();
    assert!(ahead > 50, "the dots show: {ahead}");
    let done = road_art(1000, true, true);
    assert!(done.road().dots().is_empty());
    let (w, h) = road_size(done);
    let tile = done.bake(w, h);
    let x = done.road().right() - 30.0;
    assert_eq!(
        alpha(&tile, road_col(x), device(centre_y(x))),
        255,
        "done: the road runs on"
    );
}

/// Both grounds bake, each in its own inks.
#[test]
fn light_and_dark_both_bake() {
    let bake = |dark| {
        let art = road_art(500, false, dark);
        let (w, h) = road_size(art);
        art.bake(w, h)
    };
    let (dark, light) = (bake(true), bake(false));
    assert!(dark.pixels().chunks_exact(4).any(|p| p[3] == 255));
    assert!(light.pixels().chunks_exact(4).any(|p| p[3] == 255));
    assert_ne!(dark.pixels(), light.pixels());
}

/// NOTHING IS CUT AT THE ROAD'S RIGHT EDGE: the star's halo ends inside the box and the ribbon's
/// glow fades out before it, so the image's last column is empty in every state, done (the nose
/// 14 units short of the edge) included. NEGATIVE CONTROL: a few units in, the glow shows.
#[test]
fn nothing_the_road_draws_is_cut_at_its_right_edge() {
    for dark in [true, false] {
        for (permille, done) in [(0, false), (500, false), (1000, false), (1000, true)] {
            let art = road_art(permille, done, dark);
            let (w, h) = road_size(art);
            let tile = art.bake(w, h);
            let column = |x: u32| (0..h).map(|y| alpha(&tile, x, y)).max().unwrap_or(0);
            assert_eq!(column(w - 1), 0, "{art:?}: drawn on the right edge");
            if done {
                assert!(column(w - 1 - device(6.0)) > 0, "{art:?}: the glow shows");
            }
        }
    }
}

/// THE CAT FITS UNDER THE HEADROOM: wherever it stands on any road, in any pose, nothing it draws
/// reaches more than [`HEADROOM`] units above the design box. NEGATIVE CONTROL: where the road
/// crests near its far end the walking cat does reach above the box, so the room is needed.
#[test]
fn the_cat_stays_within_the_headroom_above_the_box() {
    let mut tops: std::collections::HashMap<CatArt, f32> = std::collections::HashMap::new();
    let mut highest = f32::MAX;
    for length in [400_u16, 802, 1500, 3000] {
        for permille in (0..=1000).step_by(50) {
            for pose in CatPose::ALL {
                let road = RoadArt {
                    length,
                    permille,
                    done: pose == CatPose::Loaf,
                    dark: true,
                }
                .road();
                let art = CatArt::on(road, pose, true);
                // One device pixel a unit: the first row the cat draws, from its box's top.
                let top = *tops.entry(art).or_insert_with(|| {
                    let h = CAT_H as u32;
                    let w = (CAT_H * CatArt::ASPECT).round() as u32;
                    let tile = art.bake(w, h);
                    (0..h)
                        .find(|y| (0..w).any(|x| alpha(&tile, x, *y) >= 0x40))
                        .expect("the cat draws") as f32
                });
                let (_, fy) = road.feet();
                let reach = fy - CAT_FEET.1 + top;
                assert!(
                    reach >= -HEADROOM,
                    "{length} {permille} {pose:?}: {reach} units above the box"
                );
                highest = highest.min(reach);
            }
        }
    }
    assert!(highest < 0.0, "the cat reaches above the box: {highest}");
}

/// A WORRIED CAT'S EARS LIE BACK ON ITS SKULL: each ear's base stands inside the head's ellipse
/// (so the head covers the join and no notch shows) and its tip out beyond the head, to its side.
#[test]
fn laid_back_ears_stand_on_the_skull() {
    let (cx, cy) = (0.0, -92.0);
    let inside = |(x, y): (f32, f32)| {
        ((x - cx) / cat::HEAD.0).powi(2) + ((y - cy) / cat::HEAD.1).powi(2) < 1.0
    };
    for side in [-1.0_f32, 1.0] {
        let (ear, _) = cat::laid_back_ear(cx, cy, side);
        let points: Vec<(f32, f32)> = ear.flatten(|_| 8).into_iter().flatten().collect();
        // The closed outline runs from the base's low end round the tip to its high end and back.
        let (low, high) = (points[0], points[points.len() - 2]);
        assert!(inside(low) && inside(high), "{side}: the base on the skull");
        let tip = points
            .iter()
            .copied()
            .max_by(|a, b| (side * a.0).total_cmp(&(side * b.0)))
            .expect("points");
        assert!(!inside(tip), "{side}: the tip clear of the head");
        assert!(side * (tip.0 - cx) > cat::HEAD.0, "{side}: out to its side");
    }
}

/// `tile`'s straight-alpha pixel at `(x, y)` over `ground` in sRGB values, as the page
/// composites it.
fn over(tile: &Tile, x: u32, y: u32, ground: [f32; 3]) -> [f32; 3] {
    let p = &tile.pixels()[((y * tile.width() + x) * 4) as usize..][..4];
    let a = f32::from(p[3]) / 255.0;
    [0, 1, 2].map(|k| f32::from(p[k]) * a + ground[k] * (1.0 - a))
}

/// ONLY VIOLET REACHES THE RIBBON'S BOTTOM EDGE: every pixel the bottom edge crosses, composited
/// on the dark page, lies on the line between violet and the glow just under it (no stripe above
/// violet tints the fringe), along the ribbon and round the lower half of its nose. NEGATIVE
/// CONTROL: the edge pixels are part violet, part glow, so the test reads the fringe.
#[test]
fn only_violet_reaches_the_ribbons_bottom_edge() {
    let ground = [17.0, 19.0, 24.0];
    let violet = [0x9B as f32, 0x5C as f32, 0xF6 as f32];
    // Done: no dot lanes ahead of the nose to read for its glow.
    let art = road_art(1000, true, true);
    let (w, h) = road_size(art);
    let tile = art.bake(w, h);
    let road = art.road();
    let unit = H as f32 / ROAD_H;
    // The bottom edge's points along the ribbon.
    let nose = road.nose();
    let mut edge: Vec<((f32, f32), (f32, f32))> = (0..)
        .map(|at| ROAD_X + 140.0 + at as f32 * 5.0)
        .take_while(|x| *x < nose - HALF - 2.0)
        .map(|x| (offset_point(x, HALF, true), offset_point(x, 0.0, true)))
        .collect();
    // Round the nose's arc where it bounds the violet band, from the bottom edge up.
    edge.extend((1..8).map(|at| {
        let o = HALF - at as f32 * 0.6;
        (
            offset_point(nose - HALF + (HALF * HALF - o * o).sqrt(), o, true),
            offset_point(nose - HALF, 0.0, true),
        )
    }));
    let mut fringes = 0;
    // Each edge point with the point inside it the edge faces away from: the glow just outside
    // is read 2 units further out.
    for ((x, y), (ix, iy)) in edge {
        let len = (x - ix).hypot(y - iy);
        let (ox, oy) = ((x - ix) / len, (y - iy) / len);
        let (col, row) = (((x - ROAD_X) * unit) as u32, (y * unit) as u32);
        for row in row.saturating_sub(1)..=row + 1 {
            let a = alpha(&tile, col, row);
            if !(40..=215).contains(&a) {
                continue;
            }
            let pixel = over(&tile, col, row, ground);
            let below = over(
                &tile,
                ((x + 2.0 * ox - ROAD_X) * unit) as u32,
                ((y + 2.0 * oy) * unit) as u32,
                ground,
            );
            // The share of violet that best explains the pixel, and how far it is from that mix.
            let (d, v) = (
                [0, 1, 2].map(|k| violet[k] - below[k]),
                [0, 1, 2].map(|k| pixel[k] - below[k]),
            );
            let t = (v[0] * d[0] + v[1] * d[1] + v[2] * d[2])
                / (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).max(1.0);
            let off = [0, 1, 2]
                .map(|k| (below[k] + t * d[k] - pixel[k]).abs())
                .into_iter()
                .fold(0.0, f32::max);
            assert!(
                off <= 10.0,
                "({x:.1}, {y:.1}): {pixel:?} is not between violet and {below:?} (off {off:.1})"
            );
            fringes += 1;
        }
    }
    assert!(
        fringes > 20,
        "the edge's fringe pixels were read: {fringes}"
    );
}

/// THE GLOW SPILLS UNDER THE BOX AND FADES THERE: the road image runs [`GLOW_BELOW`] units under
/// the design box and the glow reaches into them; under the sag it still shows 18 units below
/// the ribbon, and the image's last row is empty. NEGATIVE CONTROL: the glow under the sag is stronger close in than far.
#[test]
fn the_glow_spills_under_the_box_and_fades_there() {
    let art = road_art(400, false, true);
    let (w, h) = road_size(art);
    let tile = art.bake(w, h);
    let sag = 556.0;
    let bottom = centre_y(sag) + HALF;
    let col = road_col(sag);
    let near = alpha(&tile, col, device(bottom + 7.0));
    let far = alpha(&tile, col, device(bottom + 18.0));
    assert!(far > 0, "the glow shows 18 units under the sag");
    assert!(near > far, "and is stronger close in: {near} vs {far}");
    assert!(
        (0..w).all(|x| alpha(&tile, x, h - 1) == 0),
        "the last row is empty"
    );
    let under = device(DESIGN_H + 4.0);
    assert!(
        (0..w).any(|x| alpha(&tile, x, under) > 0),
        "the glow reaches under the design box"
    );
}
