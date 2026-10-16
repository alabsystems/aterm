// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet roster's art gate, the twin of `cat_art_quality.rs` for the full-body
//! companion under `art/pet/`.
//!
//! The renderer swaps poses in place at gait rate, so the checks are mostly that
//! the roster is internally consistent, plus the pipeline's two silent traps: the
//! codegen's `quant` clamps coordinates outside the viewbox, and a mismatched
//! viewbox or anchor jolts the cat between frames.

use std::path::{Path, PathBuf};

use aterm_effects::cat_baker::CatColorKey;
use aterm_effects::kitty_pet::{ART_ASPECT, ART_ROWS};
use aterm_effects::pet_baker::{MOUTH_DETAIL_MIN_H, PetBakeKey, PetBaker};
use aterm_effects::pet_glyphs_gen::PetGlyphId;
use aterm_scene::{PathCmd, Tile, parse_path};

/// Authored viewbox shared by every pose.
const VIEWBOX: (f32, f32) = (244.0, 148.0);
/// Where the lowest ink of a planted pose lands (`art/pet/rig.py: GROUND_INK`).
const GROUND_INK: f32 = 143.0;
/// How far a pose's lowest ink may sit from the ground line and still count as
/// planted: one outline width.
const PLANT_TOL: f32 = 4.5;
/// Poses that are off the floor, listed rather than derived so the test checks the
/// authored data independently. Not `pet_leap`: the brain arcs a pounce with
/// [`aterm_effects::kitty_pet::PetFrame::lift`], which starts and ends at zero. A
/// gallop gets no lift from the brain, so its suspension frames carry their own;
/// the rise/descend frames are mid-arc and the hop's tuck mid-flight. Not
/// `pet_launch` or `pet_skid`, which are planted within the tolerance.
const AIRBORNE: &[&str] = &[
    "pet_apex",
    "pet_hop",
    "pet_leap_descend",
    "pet_leap_rise",
    "pet_run_1",
    "pet_run_3",
    // The dog roster re-skins the same pose sheet (`art/pet/poses.py`).
    "pet_dog_apex",
    "pet_dog_hop",
    "pet_dog_leap_descend",
    "pet_dog_leap_rise",
    "pet_dog_run_1",
    "pet_dog_run_3",
];

/// Layer ceiling: the hero budget from `docs/CAT_ART.md`.
const MAX_LAYERS: usize = 14;
/// Parsed-command ceiling, above `docs/CAT_ART.md`'s 240 for a hero head because a
/// full-body animal is a bigger drawing. It bounds the cold bake.
const MAX_COMMANDS: usize = 360;

fn art_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("art/pet")
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

/// Every parsed command of every layer, and the ink bounding box.
fn parsed(p: &Pose) -> (usize, (f32, f32, f32, f32)) {
    let (mut n, mut x0, mut y0, mut x1, mut y1) = (0usize, f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for layer in layers(p) {
        for d in layer
            .get("paths")
            .and_then(aterm_toml::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let s = d.as_str().expect("a path is a string");
            let cmds = parse_path(s).unwrap_or_else(|| panic!("{}: unparseable path {s:?}", p.id));
            n += cmds.len();
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
    (n, (x0, y0, x1, y1))
}

/// The renderer swaps poses in place, so they share one box and anchor set or the
/// cat jolts.
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

/// The codegen (`cat_glyphs_codegen::quant`) clamps out-of-viewbox coordinates to
/// the edge without a diagnostic, deforming the art.
#[test]
fn no_pose_escapes_its_viewbox() {
    for p in poses() {
        let (_, (x0, y0, x1, y1)) = parsed(&p);
        assert!(
            x0 >= 0.0 && y0 >= 0.0 && x1 <= VIEWBOX.0 && y1 <= VIEWBOX.1,
            "{}: ink ({x0:.1},{y0:.1})-({x1:.1},{y1:.1}) escapes the {VIEWBOX:?} \
             viewbox — codegen would CLAMP it and deform the pose",
            p.id
        );
    }
}

/// The emitter stands the sprite's bottom edge on the row's baseline, so planted
/// poses share one ground line or the settle chain sinks and pops as it plays.
#[test]
fn every_planted_pose_stands_on_one_ground_line() {
    for p in poses() {
        let (_, (_, _, _, y1)) = parsed(&p);
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

#[test]
fn every_pose_respects_the_layer_and_command_budget() {
    for p in poses() {
        let n_layers = layers(&p).len();
        assert!(
            n_layers <= MAX_LAYERS,
            "{}: {n_layers} layers exceeds the {MAX_LAYERS} budget",
            p.id
        );
        let (cmds, _) = parsed(&p);
        assert!(
            cmds <= MAX_COMMANDS,
            "{}: {cmds} parsed commands exceeds the {MAX_COMMANDS} budget",
            p.id
        );
    }
}

/// One dark outline layer, painted first, so the animal reads as one silhouette.
#[test]
fn every_pose_opens_with_exactly_one_dark_outline() {
    for p in poses() {
        let ls = layers(&p);
        let roles: Vec<&str> = ls
            .iter()
            .map(|l| {
                l.get("role")
                    .and_then(aterm_toml::Value::as_str)
                    .unwrap_or("")
            })
            .collect();
        assert_eq!(
            roles.first(),
            Some(&"outline"),
            "{}: outline paints first",
            p.id
        );
        assert_eq!(
            roles.iter().filter(|r| **r == "outline").count(),
            1,
            "{}: exactly one outline layer",
            p.id
        );
        let fill = ls[0]
            .get("ref_fill")
            .and_then(aterm_toml::Value::as_str)
            .expect("outline ref_fill");
        let hex = u32::from_str_radix(fill.trim_start_matches('#'), 16).expect("hex");
        let (r, g, b) = (
            ((hex >> 16) & 0xFF) as f32 / 255.0,
            ((hex >> 8) & 0xFF) as f32 / 255.0,
            (hex & 0xFF) as f32 / 255.0,
        );
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        assert!(
            luma < 0.25,
            "{}: outline luminance {luma:.3} must stay under 0.25",
            p.id
        );
    }
}

/// The emitter's layout constants agree with the art and fit
/// `CatBaker::host_tile`'s slot (`w <= 4·cell_h`, `h <= 2·cell_h`) at every cell
/// metric, or the pet never bakes.
#[test]
fn the_layout_constants_match_the_art_and_fit_the_atlas_slot() {
    assert!(
        (ART_ASPECT - VIEWBOX.0 / VIEWBOX.1).abs() < 1e-4,
        "ART_ASPECT {ART_ASPECT} must equal the authored viewbox aspect {}",
        VIEWBOX.0 / VIEWBOX.1
    );
    for cell_h in 8u32..=64 {
        let h = (ART_ROWS * cell_h as f32).round();
        let w = (h * ART_ASPECT).round();
        assert!(
            h <= 2.0 * cell_h as f32,
            "cell_h {cell_h}: art height {h} exceeds the 2·cell_h slot bound"
        );
        assert!(
            w <= 4.0 * cell_h as f32,
            "cell_h {cell_h}: art width {w} exceeds the 4·cell_h slot width"
        );
    }
}

/// Bake one pose through the real path on the dark ground band, the gallery's
/// reference coat and iris.
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

/// Opaque pixels of `tile` that satisfy `pred` on their RGB.
fn count_px(tile: &Tile, pred: impl Fn(u8, u8, u8) -> bool) -> usize {
    tile.pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 128 && pred(p[0], p[1], p[2]))
        .count()
}

/// The nose's rose: warm, red-led, well clear of the coat's browns.
fn rose(tile: &Tile) -> usize {
    count_px(tile, |r, g, b| r > 180 && g < 150 && b < 170)
}

/// Near-black ink: eyes, nose button, mouth. On the dark ground the outline is
/// pale, so this is face ink only.
fn ink(tile: &Tile) -> usize {
    count_px(tile, |r, g, b| {
        (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 < 70
    })
}

/// The yawn's gape survives the ship tile. `pet_baker` culls the `mouth` role below
/// [`MOUTH_DETAIL_MIN_H`] (40 px) and the ship bake is 34 px, so the rig
/// (`art/pet/pet.py: nose_paths`) paints the gape again in the never-culled `nose`
/// layer. The purr (same seat and eyes) is the control.
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
    // Above the cull the mouth role is back and the gape is dark ink.
    let big = ship * 2;
    assert!(big >= MOUTH_DETAIL_MIN_H);
    let over = ink(&bake_dark(PetGlyphId::PetYawn, big)) as i64
        - ink(&bake_dark(PetGlyphId::PetPurr, big)) as i64;
    assert!(
        over >= 20,
        "at {big} px the yawn's dark mouth must paint over the gape (measured +{over} px of ink over the purr)"
    );
}

/// The gait's vertical lives in the art, not in `PetFrame::lift`: the host lands
/// lift through `body_px`'s `(lift·cell_h).round()`, a whole-pixel quantiser that
/// zeroes a small bob at small cells, while `art/pet/pet.py: registration()` plants
/// a pose by its lowest ink minus `airborne`, so art clearance arrives sub-pixel.
/// Rule: every airborne frame clears a whole pixel at every reviewed cell height.
/// Its companion in `kitty_pet.rs` is
/// `the_gallop_keeps_its_lift_out_of_the_pixel_grid`.
#[test]
fn the_gaits_vertical_survives_the_pixel_grid_at_every_cell_height() {
    // The cell heights `docs/CAT_ART.md` reviews at, plus the 20 px ship cell.
    for cell_h in [16.0f32, 20.0, 24.0, 32.0] {
        let px_per_unit = ART_ROWS * cell_h / VIEWBOX.1;
        for p in poses() {
            if !AIRBORNE.contains(&p.id.as_str()) {
                continue;
            }
            let (_, (_, _, _, y1)) = parsed(&p);
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
    // The gallop's suspension frames clear the ground visibly at the 20 px ship
    // cell, or the cycle reads as a shuffle.
    let ship = ART_ROWS * 20.0 / VIEWBOX.1;
    for id in ["pet_run_1", "pet_run_3", "pet_dog_run_1", "pet_dog_run_3"] {
        let p = poses()
            .into_iter()
            .find(|p| p.id == id)
            .unwrap_or_else(|| panic!("{id} is in the roster"));
        let (_, (_, _, _, y1)) = parsed(&p);
        let px = (GROUND_INK - y1) * ship;
        assert!(
            px >= 2.0,
            "{id}: the gallop's suspension clears only {px:.2} px at the ship cell — \
             the brain adds no lift on a run, so this is the whole vertical"
        );
    }
}

/// Contact beats change the body contour at terminal sizes, against the pose each
/// replaces and between the paired reach/withdraw and perch/lean beats, for both
/// species; a change only to a mouth culled by LOD would not count.
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
                // Negative control: the prior pose against itself.
                assert_eq!(changed_silhouette(&old, &old), 0);
            }
        }
    }
}

/// Reading moves the head and keeps the body still, checked on the registered asset
/// strings so a change to registration or the Python sheet cannot move the body.
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
        ["outline", "coat"]
            .into_iter()
            .flat_map(|role| {
                let paths = role_paths(p, role);
                // The edge rig paints tail, far foreleg, haunch, torso, near
                // foreleg, two ears, skull; pinning the count keeps an omitted
                // limb from making the prefix compare vacuous.
                assert_eq!(paths.len(), 8, "{}: {role} part count", p.id);
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
        // Negative control: a forward lean moves the shoulder and supporting paw.
        assert_ne!(body(get("edge_lean")), body(perch));
    }
}
