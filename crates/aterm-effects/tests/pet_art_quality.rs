// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet roster's art gate — the twin of `cat_art_quality.rs` for the
//! full-body companion under `art/pet/`.
//!
//! The cat roster's assets are independent characters; the pet's 23 poses are
//! frames of ONE animal that the renderer swaps in place at gait rate. That
//! changes what needs pinning: a head glyph is wrong if it is ugly, but a pose
//! is wrong if it disagrees with the other twenty-two. So the checks here are
//! mostly about the roster being internally consistent, and about the two
//! silent-corruption traps the pipeline has — coordinates outside the viewbox
//! are CLAMPED without a word by the codegen's `quant`, and a mismatched viewbox
//! or anchor would jolt the cat between frames with nothing to catch it.

use std::path::{Path, PathBuf};

use aterm_effects::kitty_pet::{ART_ASPECT, ART_ROWS};
use aterm_scene::{PathCmd, parse_path};

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
/// two suspension frames carry their own.
const AIRBORNE: &[&str] = &["pet_run_1", "pet_run_3"];

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
    doc: toml::Value,
}

fn poses() -> Vec<Pose> {
    let mut out: Vec<Pose> = std::fs::read_dir(art_dir())
        .expect("art/pet is readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .map(|p| {
            let text = std::fs::read_to_string(&p).expect("asset is readable");
            let doc: toml::Value = text.parse().expect("asset is valid TOML");
            let id = doc
                .get("id")
                .and_then(toml::Value::as_str)
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

fn layers(p: &Pose) -> &[toml::Value] {
    p.doc
        .get("layer")
        .and_then(toml::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn num(v: &toml::Value) -> f32 {
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
            .and_then(toml::Value::as_array)
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
            .and_then(toml::Value::as_array)
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
            p.doc.get("kind").and_then(toml::Value::as_str),
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
            .map(|l| l.get("role").and_then(toml::Value::as_str).unwrap_or(""))
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
            .and_then(toml::Value::as_str)
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
