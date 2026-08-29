// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet roster's art gate — the twin of `cat_art_quality.rs` for the
//! full-body companion under `art/pet/`.
//!
//! The cat roster's assets are independent characters; the pet's 55 poses are
//! frames of ONE animal that the renderer swaps in place at gait rate. That
//! changes what needs pinning: a head glyph is wrong if it is ugly, but a pose
//! is wrong if it disagrees with the other fifty-four. So the checks here are
//! mostly about the roster being internally consistent, and about the two
//! silent-corruption traps the pipeline has — coordinates outside the viewbox
//! are CLAMPED without a word by the codegen's `quant`, and a mismatched viewbox
//! or anchor would jolt the cat between frames with nothing to catch it.

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
/// planted. One outline width — enough for a paw's cap to differ between poses,
/// far too little to read as the cat sinking.
const PLANT_TOL: f32 = 4.5;
/// Poses that are legitimately off the floor. Deliberately NOT `pet_leap`: the
/// brain arcs a pounce with [`aterm_effects::kitty_pet::PetFrame::lift`], and
/// that arc starts and ends at zero — so the leap ART is planted, and floating
/// it too would double the clearance at exactly the frames the cat is meant to
/// be touching down. A gallop, by contrast, gets no lift from the brain, so its
/// two suspension frames carry their own. The rise/descend flight frames are
/// mid-arc by definition (rise serves flight u < 0.25, descend u > 0.6, never
/// the endpoints where lift returns to zero), so like the gallop's suspension
/// frames they carry authored clearance. The row hop's tuck (`pet_hop`) serves
/// the middle of a short vertical flight the same way. NOT `pet_launch` (the
/// hind paws are still on the floor: it plays where lift is zero) and NOT
/// `pet_skid` (braced on the forepaws, the folded hind pair inside the
/// tolerance) — both are planted by construction.
/// Listed per SPECIES rather than derived by stripping a prefix: the roster is
/// authored data, and a test that computes the expected answer from the same
/// naming rule the data uses would stop being an independent check.
const AIRBORNE: &[&str] = &[
    "pet_apex",
    "pet_hop",
    "pet_leap_descend",
    "pet_leap_rise",
    "pet_run_1",
    "pet_run_3",
    // The dog roster is the same pose sheet re-skinned (`art/pet/poses.py`),
    // so exactly the same six frames are off the floor.
    "pet_dog_apex",
    "pet_dog_hop",
    "pet_dog_leap_descend",
    "pet_dog_leap_rise",
    "pet_dog_run_1",
    "pet_dog_run_3",
];

/// Layer ceiling — the hero budget from `docs/CAT_ART.md`, which the pet roster
/// meets unchanged.
const MAX_LAYERS: usize = 14;
/// Parsed-command ceiling. `docs/CAT_ART.md` sets 240 for a hero HEAD; a
/// full-body cat is a strictly bigger drawing — a head plus a torso, a haunch,
/// four two-bone limbs and a tapered tail, each with an outline and a coat pass
/// — and the measured roster tops out at 345. The budget exists to keep the
/// COLD BAKE bounded, and the pet's bake is bounded twice over besides: one
/// resident tile per pose in an LRU that is itself capped, at most two cold
/// bakes per frame. Pinning the real number here is the honest form of the
/// budget; letting it drift silently past the head figure is not.
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

/// The swap-in-place rule: a roster the renderer cross-fades between frame to
/// frame must agree on its box and its anchors, or the cat jolts.
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

/// Out-of-viewbox coordinates are silently CLAMPED to the edge by the codegen
/// (`cat_glyphs_codegen::quant`), which deforms art with no diagnostic at all.
/// This is the only thing standing between a mis-authored pose and a squashed
/// cat that nobody notices.
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

/// The emitter stands the sprite's bottom edge on the text row's baseline, so a
/// pose's lowest ink IS its contact with the line. Weight-bearing poses must
/// therefore agree on one ground line, or the settle chain — walk, sit, wash,
/// curl — visibly sinks and pops as it plays.
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

/// One dark outline layer, painted first — the guide's hero rule, and what makes
/// the whole animal read as a single silhouette rather than a pile of parts.
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

/// The layout constants the emitter sizes the destination box from must agree
/// with the art, and the result must fit `CatBaker::host_tile`'s slot bounds
/// (`w <= 4·cell_h`, `h <= 2·cell_h`) at every cell metric — otherwise the pet
/// silently never bakes.
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
/// reference coat/iris, width from the pose's own aspect.
fn bake_dark(pose: PetGlyphId, h: u32) -> Tile {
    let w = ((h as f32) * PetBaker::aspect(pose)).round().max(1.0) as u32;
    PetBakeKey {
        pose,
        coat: 8,
        iris: 4,
        colors: CatColorKey {
            accent: 12,
            background: 0,
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

/// Near-black ink — eyes, nose button, mouth. On the DARK ground the outline
/// is pale, so this is face ink and nothing else.
fn ink(tile: &Tile) -> usize {
    count_px(tile, |r, g, b| {
        (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 < 70
    })
}

/// The yawn's gape must survive the ship tile. `pet_baker` culls the `mouth`
/// role below [`MOUTH_DETAIL_MIN_H`] (40 px) and the ship bake is 34 px, so a
/// yawn authored only as a mouth would have no mouth at the one size that
/// matters. The rig (`art/pet/pet.py: nose_paths`) therefore paints the gape
/// a second time in the never-culled `nose` layer, in the nose's own fill:
/// rose for the cat, the dark button's black for the dog. The purr is the
/// control — the same seat with the same happy arcs for eyes, so the only
/// face ink the two frames disagree on is the gape. Below the cull the yawn
/// carries that extra nose-fill ink; above it the dark mouth returns and
/// paints over it.
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

/// THE VERTICAL LIVES IN THE ART, AND IT HAS TO SURVIVE THE PIXEL GRID.
///
/// `docs/DESIGN-kitty-motion-2026-08-19.md` §3/§9 specified a brain-side
/// `RUN_BOB 0.05·ramp` (and a `TROT_BOB`) and then refused both *until a
/// pixel-domain test exists at `cell_h ∈ {16, 24, 32}`*. This is that test,
/// and it kills the bob rather than admitting it — because the brain's
/// channel for a vertical is `PetFrame::lift`, and the host lands it through
/// `body_px`'s `baseline = ((row + 1)·cell_h).round() − (lift·cell_h).round()`.
/// That second `round()` is a whole-pixel quantiser: at `cell_h = 16` a lift
/// of `0.05·ramp` is IDENTICALLY 0 px for every ramp below 0.67, which is two
/// thirds of the gallop band including the entire walk→run boundary the doc's
/// continuity argument was about; at 24 and 32 it dies below ramp 0.5. A
/// floored variant is alive but is a whole-pixel staircase — 1-tick level
/// runs at `MAX_SPEED`, which is a strobe, not a bound.
///
/// The art's clearance has no such problem: `art/pet/pet.py: registration()`
/// plants a pose by its MEASURED lowest ink minus `airborne`, so the lift is
/// baked into the geometry and arrives sub-pixel and anti-aliased at every
/// size. One viewbox unit is `ART_ROWS · cell_h / 148` px. So the rule is:
/// every frame the roster calls airborne must clear a whole pixel of ground
/// at the smallest cell height the docs ask us to support. The gallop's two
/// suspension frames are the ones this is really about (`pet_run_1` /
/// `pet_run_3`, deepened from 7.0/8.0 to 11.0/13.0 units on 2026-08-27), and
/// they are also the roster's top four poses by onsets.
///
/// Goes red if anyone shaves an `airborne` back toward the pixel floor, and
/// its companion in `kitty_pet.rs` (`the_gallop_keeps_its_lift_out_of_the_
/// pixel_grid`) goes red if anyone re-adds the brain-side bob.
#[test]
fn the_gaits_vertical_survives_the_pixel_grid_at_every_cell_height() {
    // The three cell heights `docs/CAT_ART.md` asks every frame to be
    // reviewed at, plus the 20 px ship cell between them.
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
    // The gallop specifically: its suspension frames must out-clear the
    // planted contact frames of its own cycle by a visible margin, or the
    // cycle reads as a shuffle. Measured at the 20 px ship cell: 2.53 px
    // (`pet_run_1`) and 2.99 px (`pet_run_3`).
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
