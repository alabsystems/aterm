// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Geometry, viewbox, ground-line, and silhouette checks for the full-body pet poses under `art/pet/`.

use std::path::PathBuf;

use aterm_effects::cat_baker::CatColorKey;
use aterm_effects::kitty_pet::ART_ROWS;
use aterm_effects::pet_baker::{MOUTH_DETAIL_MIN_H, PetBakeKey, PetBaker};
use aterm_effects::pet_glyphs_gen::PetGlyphId;
use aterm_scene::{PathCmd, PathTransform, Tile, fill_path, parse_path};

const VIEWBOX: (f32, f32) = (244.0, 148.0);
const GROUND_INK: f32 = 143.0;
const PLANT_TOL: f32 = 4.5;
/// Poses authored above `GROUND_INK` rather than planted on the baseline.
const AIRBORNE: &[&str] = &[
    "pet_apex",
    "pet_hop",
    "pet_leap_descend",
    "pet_leap_rise",
    "pet_run_1",
    "pet_run_3",
    "pet_dog_apex",
    "pet_dog_hop",
    "pet_dog_leap_descend",
    "pet_dog_leap_rise",
    "pet_dog_run_1",
    "pet_dog_run_3",
];

fn art_dir() -> PathBuf {
    aterm_tempfile::manifest_dir().join("art/pet")
}

struct Pose {
    id: String,
    doc: aterm_toml::Value,
}

fn poses() -> Vec<Pose> {
    let mut out: Vec<Pose> = std::fs::read_dir(art_dir())
        .expect("art/pet is readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .map(|p| {
            let text = std::fs::read_to_string(&p).expect("asset is readable");
            let doc: aterm_toml::Value = text.parse().expect("asset is valid TOML");
            let id = doc
                .get("id")
                .and_then(aterm_toml::Value::as_str)
                .expect("asset has an id")
                .to_string();
            assert_eq!(
                Some(id.as_str()),
                p.file_stem().and_then(|s| s.to_str()),
                "file stem and `id` must match"
            );
            Pose { id, doc }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    assert!(!out.is_empty(), "the pet roster must not be empty");
    out
}

fn layers(p: &Pose) -> &[aterm_toml::Value] {
    p.doc
        .get("layer")
        .and_then(aterm_toml::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn num(v: &aterm_toml::Value) -> f32 {
    v.as_float()
        .map(|f| f as f32)
        .or_else(|| v.as_integer().map(|i| i as f32))
        .unwrap_or(f32::NAN)
}

fn parsed(p: &Pose) -> (f32, f32, f32, f32) {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for layer in layers(p) {
        for d in layer
            .get("paths")
            .and_then(aterm_toml::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let s = d.as_str().expect("a path is a string");
            let cmds = parse_path(s).unwrap_or_else(|| panic!("{}: unparseable path {s:?}", p.id));
            for c in &cmds {
                let pts: &[(f32, f32)] = &match *c {
                    PathCmd::Move(x, y) | PathCmd::Line(x, y) => vec![(x, y)],
                    PathCmd::Cubic(a, b, c2, d2, e, f) => vec![(a, b), (c2, d2), (e, f)],
                    PathCmd::Close => vec![],
                };
                for &(x, y) in pts {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
    }
    (x0, y0, x1, y1)
}

#[test]
fn every_pose_shares_one_viewbox_and_one_anchor_set() {
    for p in poses() {
        let vb = p
            .doc
            .get("viewbox")
            .and_then(aterm_toml::Value::as_array)
            .expect("viewbox");
        assert_eq!(
            (num(&vb[0]), num(&vb[1])),
            VIEWBOX,
            "{}: every pet pose must share one viewbox",
            p.id
        );
        let a = p.doc.get("anchor").expect("anchor");
        for key in ["eye_y", "center_x", "word_top"] {
            let v = a.get(key).map(num).unwrap_or(f32::NAN);
            let want = match key {
                "eye_y" => 0.34,
                "center_x" => 0.5,
                _ => 1.0,
            };
            assert!(
                (v - want).abs() < 1e-6,
                "{}: anchor {key} = {v}, every pose must share {want}",
                p.id
            );
        }
        assert_eq!(
            p.doc.get("kind").and_then(aterm_toml::Value::as_str),
            Some("special"),
            "{}: pet poses are `special` compositions",
            p.id
        );
    }
}

/// Ensures no control point falls outside `VIEWBOX`, where `cat_glyphs_codegen::quant` would clamp it.
#[test]
fn no_pose_escapes_its_viewbox() {
    for p in poses() {
        let (x0, y0, x1, y1) = parsed(&p);
        assert!(
            x0 >= 0.0 && y0 >= 0.0 && x1 <= VIEWBOX.0 && y1 <= VIEWBOX.1,
            "{}: ink ({x0:.1},{y0:.1})-({x1:.1},{y1:.1}) escapes the {VIEWBOX:?} \
             viewbox — codegen would CLAMP it and deform the pose",
            p.id
        );
    }
}

#[test]
fn every_planted_pose_stands_on_one_ground_line() {
    for p in poses() {
        let (_, _, _, y1) = parsed(&p);
        if AIRBORNE.contains(&p.id.as_str()) {
            assert!(
                y1 < GROUND_INK - PLANT_TOL,
                "{}: declared airborne but its ink reaches {y1:.1}, on the floor",
                p.id
            );
            continue;
        }
        assert!(
            (y1 - GROUND_INK).abs() <= PLANT_TOL,
            "{}: lowest ink {y1:.1} is {:.1} off the ground line {GROUND_INK}",
            p.id,
            y1 - GROUND_INK
        );
    }
}

fn bake_dark(pose: PetGlyphId, h: u32) -> Tile {
    bake_ground(pose, h, 0)
}

fn bake_ground(pose: PetGlyphId, h: u32, background: u8) -> Tile {
    let w = ((h as f32) * PetBaker::aspect(pose)).round().max(1.0) as u32;
    PetBakeKey {
        pose,
        coat: 8,
        iris: 4,
        colors: CatColorKey {
            accent: 12,
            background,
        },
        w: w as u16,
        h: h as u16,
    }
    .bake()
}

fn count_px(tile: &Tile, pred: impl Fn(u8, u8, u8) -> bool) -> usize {
    tile.pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 128 && pred(p[0], p[1], p[2]))
        .count()
}

fn rose(tile: &Tile) -> usize {
    count_px(tile, |r, g, b| r > 180 && g < 150 && b < 170)
}

fn ink(tile: &Tile) -> usize {
    count_px(tile, |r, g, b| {
        (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 < 70
    })
}

/// Verifies that the yawn gape remains visible below [`MOUTH_DETAIL_MIN_H`] via the uncullable `nose` layer.
#[test]
fn a_yawn_keeps_its_gape_below_the_mouth_cull() {
    let ship = (ART_ROWS * 20.0).round() as u32;
    assert!(
        ship < MOUTH_DETAIL_MIN_H,
        "the ship bake ({ship} px) must sit below the mouth cull ({MOUTH_DETAIL_MIN_H} px) for this test to mean anything"
    );
    let cat = rose(&bake_dark(PetGlyphId::PetYawn, ship)) as i64
        - rose(&bake_dark(PetGlyphId::PetPurr, ship)) as i64;
    assert!(
        cat >= 5,
        "at {ship} px the cat's yawn must carry a rose gape the purr lacks (measured +{cat} px; the nose-layer copy of the gape is missing)"
    );
    let dog = ink(&bake_dark(PetGlyphId::PetDogYawn, ship)) as i64
        - ink(&bake_dark(PetGlyphId::PetDogPurr, ship)) as i64;
    assert!(
        dog >= 4,
        "at {ship} px the dog's yawn must carry a dark gape the purr lacks (measured +{dog} px)"
    );
    let big = ship * 2;
    assert!(big >= MOUTH_DETAIL_MIN_H);
    let over = ink(&bake_dark(PetGlyphId::PetYawn, big)) as i64
        - ink(&bake_dark(PetGlyphId::PetPurr, big)) as i64;
    assert!(
        over >= 20,
        "at {big} px the yawn's dark mouth must paint over the gape (measured +{over} px of ink over the purr)"
    );
}

/// Verifies that every airborne frame clears at least 1 pixel above `GROUND_INK` at reviewed cell heights.
#[test]
fn the_gaits_vertical_survives_the_pixel_grid_at_every_cell_height() {
    for cell_h in [16.0f32, 20.0, 24.0, 32.0] {
        let px_per_unit = ART_ROWS * cell_h / VIEWBOX.1;
        for p in poses() {
            if !AIRBORNE.contains(&p.id.as_str()) {
                continue;
            }
            let (_, _, _, y1) = parsed(&p);
            let clearance = (GROUND_INK - y1) * px_per_unit;
            assert!(
                clearance >= 1.0,
                "{}: at cell_h {cell_h} the airborne clearance is {clearance:.2} px \
                 ({:.1} viewbox units) — under one pixel it is not on the glass at all, \
                 and a bob paid through `lift` would round to zero here too",
                p.id,
                GROUND_INK - y1
            );
        }
    }
    let ship = ART_ROWS * 20.0 / VIEWBOX.1;
    for id in ["pet_run_1", "pet_run_3", "pet_dog_run_1", "pet_dog_run_3"] {
        let p = poses()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap_or_else(|| panic!("{id} is in the roster"));
        let (_, _, _, y1) = parsed(&p);
        let px = (GROUND_INK - y1) * ship;
        assert!(
            px >= 2.0,
            "{id}: the gallop's suspension clears only {px:.2} px at the ship cell — \
             the brain adds no lift on a run, so this is the whole vertical"
        );
    }
}

#[test]
fn contact_beats_have_distinct_silhouettes_at_terminal_sizes() {
    use PetGlyphId::*;
    let pairs = [
        (PetContactBrace, PetSkid),
        (PetTailTuck, PetStand),
        (PetContactRecover, PetWalk1),
        (PetInspectDown, PetSitLookdown),
        (PetReachPaw, PetBat),
        (PetWithdrawPaw, PetReachPaw),
        (PetEdgePerch, PetSit),
        (PetEdgeLean, PetEdgePerch),
        (PetEdgeLookUp, PetEdgePerch),
        (PetEdgeLookDown, PetEdgePerch),
        (PetDogContactBrace, PetDogSkid),
        (PetDogTailTuck, PetDogStand),
        (PetDogContactRecover, PetDogWalk1),
        (PetDogInspectDown, PetDogSitLookdown),
        (PetDogReachPaw, PetDogBat),
        (PetDogWithdrawPaw, PetDogReachPaw),
        (PetDogEdgePerch, PetDogSit),
        (PetDogEdgeLean, PetDogEdgePerch),
        (PetDogEdgeLookUp, PetDogEdgePerch),
        (PetDogEdgeLookDown, PetDogEdgePerch),
    ];
    fn changed_silhouette(a: &Tile, b: &Tile) -> usize {
        assert_eq!((a.width(), a.height()), (b.width(), b.height()));
        a.pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .zip(b.pixels().as_chunks::<4>().0)
            .filter(|(a, b)| (a[3] > 128) != (b[3] > 128))
            .count()
    }
    for background in [0, 3] {
        for h in [16, 24, 32, 34] {
            for (pose, prior) in pairs {
                let new = bake_ground(pose, h, background);
                let old = bake_ground(prior, h, background);
                let difference = changed_silhouette(&new, &old);
                assert!(
                    difference >= 3,
                    "{pose:?} vs {prior:?} at {h}px, ground {background}: only \
                     {difference} changed silhouette pixels; the contact beat disappeared"
                );
                assert_eq!(changed_silhouette(&old, &old), 0);
            }
        }
    }
}

#[test]
fn reading_gaze_keeps_the_perch_body_geometry_fixed() {
    fn role_paths<'a>(p: &'a Pose, role: &str) -> Vec<&'a str> {
        layers(p)
            .iter()
            .filter(|layer| layer.get("role").and_then(aterm_toml::Value::as_str) == Some(role))
            .flat_map(|layer| {
                layer
                    .get("paths")
                    .and_then(aterm_toml::Value::as_array)
                    .unwrap()
            })
            .map(|path| path.as_str().expect("path string"))
            .collect()
    }
    fn body(p: &Pose) -> Vec<&str> {
        // Tail, legs, haunch, torso, legs, two ears and the head; a dog's
        // profile head adds its snout.
        let parts = if p.id.starts_with("pet_dog_") { 9 } else { 8 };
        ["outline", "coat"]
            .into_iter()
            .flat_map(|role| {
                let paths = role_paths(p, role);
                assert_eq!(paths.len(), parts, "{}: {role} part count", p.id);
                paths.into_iter().take(5)
            })
            .collect()
    }
    let all = poses();
    for species in ["pet_", "pet_dog_"] {
        let get = |suffix: &str| {
            all.iter()
                .find(|p| p.id == format!("{species}{suffix}"))
                .expect("reading pose")
        };
        let perch = get("edge_perch");
        for suffix in ["edge_look_up", "edge_look_down"] {
            let gaze = get(suffix);
            assert_eq!(
                body(gaze),
                body(perch),
                "{}: reading moved the body",
                gaze.id
            );
            assert_eq!(role_paths(gaze, "pattern"), role_paths(perch, "pattern"));
            assert_ne!(
                role_paths(gaze, "eye"),
                role_paths(perch, "eye"),
                "{}: head gaze did not move",
                gaze.id
            );
            assert_ne!(
                role_paths(gaze, "detail"),
                role_paths(perch, "detail"),
                "{}: pupils did not move",
                gaze.id
            );
        }
        assert_ne!(body(get("edge_lean")), body(perch));
    }
}

/// The face-on dog poses: their ears hang at both cheeks and the snout points
/// at the viewer, so no profile snout leads their eyes.
const FACE_ON: &[&str] = &["pet_dog_sit_front", "pet_dog_sit_front_blink"];

/// The profile walk before after-land R8 (`fixtures/pet_dog_walk_0_pre_r8.toml`,
/// the art at c1cf1c830d): pink-lined 3/4 drop ears that read as cheeks and a
/// muzzle inside the skull, so at 1x the dog read as a seal.
const PRE_R8_DOG_WALK: &str = include_str!("fixtures/pet_dog_walk_0_pre_r8.toml");

/// How far a profile snout must carry the nose ahead of the near eye, in
/// viewbox units (2.3 px at the ship bake): R8's poses lead by 13.6 or more,
/// the pre-R8 ones by 6.9 at most.
const SNOUT_LEAD: f32 = 10.0;

/// The WCAG ratio a dog's ear leather keeps against its coat.
const EAR_CONTRAST: f32 = 2.5;

/// The ear leather left showing at the ship bake, in px (R8's poses show 19
/// or more).
const EAR_PX: usize = 12;

fn pose_of(text: &str) -> Pose {
    let doc: aterm_toml::Value = text.parse().expect("asset is valid TOML");
    let id = doc
        .get("id")
        .and_then(aterm_toml::Value::as_str)
        .expect("asset has an id")
        .to_string();
    Pose { id, doc }
}

fn role_layer<'a>(p: &'a Pose, role: &str) -> &'a aterm_toml::Value {
    layers(p)
        .iter()
        .find(|layer| layer.get("role").and_then(aterm_toml::Value::as_str) == Some(role))
        .unwrap_or_else(|| panic!("{}: no {role} layer", p.id))
}

fn layer_paths(layer: &aterm_toml::Value) -> Vec<Vec<PathCmd>> {
    layer
        .get("paths")
        .and_then(aterm_toml::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .map(|d| parse_path(d.as_str().expect("a path is a string")).expect("parseable path"))
        .collect()
}

fn layer_fill(layer: &aterm_toml::Value) -> (f32, f32, f32) {
    let hex = layer
        .get("ref_fill")
        .and_then(aterm_toml::Value::as_str)
        .and_then(|s| s.strip_prefix('#'))
        .expect("a layer has a #RRGGBB ref_fill");
    let v = u32::from_str_radix(hex, 16).expect("hex fill");
    let c = |shift: u32| ((v >> shift) & 0xFF) as f32 / 255.0;
    (c(16), c(8), c(0))
}

fn wcag_ratio(a: (f32, f32, f32), b: (f32, f32, f32)) -> f32 {
    let lin = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let lum = |(r, g, b): (f32, f32, f32)| 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
    let (la, lb) = (lum(a), lum(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The centre of one path's control-point box.
fn centre(path: &[PathCmd]) -> (f32, f32) {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for c in path {
        let pts: &[(f32, f32)] = &match *c {
            PathCmd::Move(x, y) | PathCmd::Line(x, y) => vec![(x, y)],
            PathCmd::Cubic(a, b, c2, d2, e, f) => vec![(a, b), (c2, d2), (e, f)],
            PathCmd::Close => vec![],
        };
        for &(x, y) in pts {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    ((x0 + x1) / 2.0, (y0 + y1) / 2.0)
}

/// How far, in viewbox units, the nose leads the near eye along the face's
/// own axis (far eye to near eye), so a turned head reads as a level one:
/// the snout.
fn snout_lead(p: &Pose) -> f32 {
    let eyes = layer_paths(role_layer(p, "eye"));
    let nose = layer_paths(role_layer(p, "nose"));
    let (near, far) = (centre(&eyes[0]), centre(&eyes[1]));
    let tip = centre(nose.last().expect("a nose path"));
    let axis = (near.0 - far.0, near.1 - far.1);
    let len = axis.0.hypot(axis.1);
    ((tip.0 - near.0) * axis.0 + (tip.1 - near.1) * axis.1) / len
}

/// The ear leather showing at `h` px: the pixels the `inner_ear` fill still
/// owns once every layer is painted at its authored fill, counted only when
/// that fill reads against the coat.
fn ear_leather_px(p: &Pose, h: u32) -> usize {
    let ear = layer_fill(role_layer(p, "inner_ear"));
    if wcag_ratio(ear, layer_fill(role_layer(p, "coat"))) < EAR_CONTRAST {
        return 0;
    }
    let w = (h as f32 * VIEWBOX.0 / VIEWBOX.1).round() as u32;
    let xform = PathTransform {
        scale_x: w as f32 / VIEWBOX.0,
        scale_y: h as f32 / VIEWBOX.1,
        dx: 0.0,
        dy: 0.0,
    };
    let mut tile = Tile::new(w, h);
    for layer in layers(p) {
        let paths = layer_paths(layer);
        let refs: Vec<&[PathCmd]> = paths.iter().map(Vec::as_slice).collect();
        fill_path(&mut tile, &refs, layer_fill(layer), 1.0, xform);
    }
    let byte = |c: f32| (c * 255.0).round() as i32;
    let ink = [byte(ear.0), byte(ear.1), byte(ear.2)];
    count_px(&tile, |r, g, b| {
        [r, g, b]
            .iter()
            .zip(ink)
            .all(|(&c, e)| (i32::from(c) - e).abs() <= 12)
    })
}

/// after-land R8: at its ship size every dog pose shows a drop ear in dark
/// leather on its skull, and every profile pose carries its nose ahead of its
/// eyes on a snout. Negative control: the pre-R8 walk fails both.
#[test]
fn the_dog_shows_a_leather_drop_ear_and_a_snout_at_ship_size() {
    let ship = (ART_ROWS * 20.0).round() as u32;
    let dogs: Vec<Pose> = poses()
        .into_iter()
        .filter(|p| p.id.starts_with("pet_dog_"))
        .collect();
    assert_eq!(dogs.len(), 67, "the dog roster");
    for p in &dogs {
        let ear = ear_leather_px(p, ship);
        assert!(
            ear >= EAR_PX,
            "{}: {ear} px of ear leather at {ship} px (want >= {EAR_PX} at WCAG >= {EAR_CONTRAST} against the coat)",
            p.id
        );
        if FACE_ON.contains(&p.id.as_str()) {
            continue;
        }
        let lead = snout_lead(p);
        assert!(
            lead >= SNOUT_LEAD,
            "{}: the nose leads the near eye by {lead:.1} units (want >= {SNOUT_LEAD})",
            p.id
        );
    }

    let before = pose_of(PRE_R8_DOG_WALK);
    assert_eq!(before.id, "pet_dog_walk_0");
    assert!(
        ear_leather_px(&before, ship) < EAR_PX,
        "negative control: the pre-R8 pink inner ear passed as leather"
    );
    assert!(
        snout_lead(&before) < SNOUT_LEAD,
        "negative control: the pre-R8 muzzle passed as a snout ({:.1} units)",
        snout_lead(&before)
    );
}

/// The profile stand-ear flick before the drop ear took a share of the
/// swivel (`fixtures/pet_dog_stand_ear_pre_flick.toml`, the art at
/// 227e3ad742): the cat's whole 70 degree swivel swung the flap straight back
/// from the head, a horizontal tab at 1x.
const PRE_FLICK_DOG_STAND_EAR: &str = include_str!("fixtures/pet_dog_stand_ear_pre_flick.toml");

/// The least height over width a profile drop ear's box keeps: every profile
/// pose keeps 0.85 or more (the pinned droop), the pre-flick tab 0.48.
const EAR_HANG: f32 = 0.75;

/// A path's control-point box, `(width, height)`.
fn path_box(path: &[PathCmd]) -> (f32, f32) {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for c in path {
        let pts: &[(f32, f32)] = &match *c {
            PathCmd::Move(x, y) | PathCmd::Line(x, y) => vec![(x, y)],
            PathCmd::Cubic(a, b, c2, d2, e, f) => vec![(a, b), (c2, d2), (e, f)],
            PathCmd::Close => vec![],
        };
        for &(x, y) in pts {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    (x1 - x0, y1 - y0)
}

/// The near ear's height over its width, a profile dog pose's.
fn ear_hang(p: &Pose) -> f32 {
    let ear = layer_paths(role_layer(p, "inner_ear"));
    let (w, h) = path_box(ear.first().expect("the near ear's leather"));
    h / w
}

/// after-land R8 (r1 judge's low): a profile dog's drop ear hangs from its
/// crown in every pose, an ear flick lifting the flap partway, never swung
/// out level behind the head. Negative control: the pre-flick stand-ear pose,
/// whose flap stood out as a horizontal tab.
#[test]
fn a_profile_dog_s_flicked_ear_still_hangs() {
    for p in poses()
        .iter()
        .filter(|p| p.id.starts_with("pet_dog_") && !FACE_ON.contains(&p.id.as_str()))
    {
        let hang = ear_hang(p);
        assert!(
            hang >= EAR_HANG,
            "{}: the near ear's box is {hang:.2} as tall as it is wide (want >= {EAR_HANG})",
            p.id
        );
    }
    let before = pose_of(PRE_FLICK_DOG_STAND_EAR);
    assert_eq!(before.id, "pet_dog_stand_ear");
    assert!(
        ear_hang(&before) < EAR_HANG,
        "negative control: the pre-flick tab passed as a hanging ear ({:.2})",
        ear_hang(&before)
    );
}
