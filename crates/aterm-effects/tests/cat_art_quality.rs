// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Asset-level quality gates for the cursor cat's high-visibility glyphs.
//!
//! These tests intentionally read the TOML sources rather than the generated
//! drawlist. That keeps the art-review loop useful before codegen is committed.

use std::path::PathBuf;

use aterm_effects::cat_baker::{CatColorKey, ResolvedFills, bake_variant};
use aterm_effects::cat_glyphs_gen::CatGlyphId;
use aterm_scene::{PathCmd, PathTransform, Tile, fill_path, parse_path};

const HEROES: [&str; 4] = ["s1_03", "s1_15", "s1_21", "spec_stretch"];
const REPAIRS: [&str; 5] = ["s1_04", "s1_05", "s1_06", "s1_19", "s1_22"];
const ARTIST_PASS: [&str; 24] = [
    "s1_00",
    "s1_01",
    "s1_02",
    "s1_07",
    "s1_08",
    "s1_09",
    "s1_10",
    "s1_11",
    "s1_12",
    "s1_13",
    "s1_14",
    "s1_16",
    "s1_17",
    "s1_18",
    "s1_20",
    "s1_23",
    "s2_06b",
    "spec_fluffy",
    "spec_maneki",
    "spec_sleeping",
    "spec_tabbybell",
    "spec_tuxedo",
    "spec_witch",
    "spec_yarn",
];
const ACCESSORIES: [&str; 3] = ["acc_bow", "acc_crown", "acc_bell"];
const TINY_FACE_POLISH: [(&str, f32); 4] = [
    ("s1_09", 2.0),
    ("s1_18", 0.7),
    ("s1_20", 4.0),
    ("s2_06b", 2.5),
];
const HEAD_ROLES: [&str; 8] = [
    "outline",
    "coat",
    "inner_ear",
    "muzzle",
    "eye",
    "nose",
    "mouth",
    "whisker",
];
const REPAIR_ROLES: [&str; 6] = ["outline", "coat", "eye", "nose", "mouth", "whisker"];

struct Asset {
    id: String,
    viewbox: (f32, f32),
    anchor: (f32, f32, f32),
    layers: Vec<toml::Value>,
}

fn asset_path(id: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("art/glyphs")
        .join(format!("{id}.toml"))
}

fn number(value: &toml::Value) -> f32 {
    value
        .as_float()
        .map(|v| v as f32)
        .or_else(|| value.as_integer().map(|v| v as f32))
        .expect("numeric asset value")
}

fn load(id: &str) -> Asset {
    let source = std::fs::read_to_string(asset_path(id)).expect("read hero cat asset");
    let doc: toml::Value = source.parse().expect("parse hero cat asset");
    let viewbox = doc["viewbox"].as_array().expect("viewbox array");
    let anchor = doc["anchor"].as_table().expect("anchor table");
    Asset {
        id: doc["id"].as_str().expect("id").to_owned(),
        viewbox: (number(&viewbox[0]), number(&viewbox[1])),
        anchor: (
            number(&anchor["eye_y"]),
            number(&anchor["center_x"]),
            number(&anchor["word_top"]),
        ),
        layers: doc["layer"].as_array().expect("layer array").clone(),
    }
}

fn role(layer: &toml::Value) -> &str {
    layer["role"].as_str().expect("semantic layer role")
}

fn role_layer<'a>(asset: &'a Asset, wanted: &str) -> &'a toml::Value {
    asset
        .layers
        .iter()
        .find(|layer| role(layer) == wanted)
        .unwrap_or_else(|| panic!("{} is missing `{wanted}`", asset.id))
}

fn parsed_paths(layer: &toml::Value) -> Vec<Vec<PathCmd>> {
    layer["paths"]
        .as_array()
        .expect("paths array")
        .iter()
        .map(|path| {
            parse_path(path.as_str().expect("SVG path string"))
                .unwrap_or_else(|| panic!("invalid SVG path in `{}`", role(layer)))
        })
        .collect()
}

fn render_roles(asset: &Asset, wanted: &[&str], height: u32) -> Tile {
    let scale = height as f32 / asset.viewbox.1;
    let width = (asset.viewbox.0 * scale).round().max(1.0) as u32;
    let mut tile = Tile::new(width, height);
    for layer in &asset.layers {
        if !wanted.is_empty() && !wanted.contains(&role(layer)) {
            continue;
        }
        let paths = parsed_paths(layer);
        let refs: Vec<&[PathCmd]> = paths.iter().map(Vec::as_slice).collect();
        fill_path(
            &mut tile,
            &refs,
            (1.0, 1.0, 1.0),
            1.0,
            PathTransform {
                scale_x: scale,
                scale_y: scale,
                dx: 0.0,
                dy: 0.0,
            },
        );
    }
    tile
}

fn render_context(asset: &Asset, height: u32, dark: bool) -> Tile {
    let colors = CatColorKey {
        accent: 12,
        background: if dark { 0 } else { 3 },
    };
    let fills = ResolvedFills::from_context(8, 4, colors);
    let scale = height as f32 / asset.viewbox.1;
    let width = (asset.viewbox.0 * scale).round().max(1.0) as u32;
    let mut tile = Tile::new(width, height);
    for layer in &asset.layers {
        let color = if matches!(role(layer), "outline" | "whisker") {
            fills.outline
        } else {
            match layer["recolor"].as_str().expect("recolor") {
                "coat" => fills.coat,
                "iris" => fills.iris,
                "fixed" => {
                    let [r, g, b] = hex_rgb(layer["ref_fill"].as_str().expect("ref_fill"));
                    (r, g, b)
                }
                other => panic!("unsupported preview recolor `{other}`"),
            }
        };
        let paths = parsed_paths(layer);
        let refs: Vec<&[PathCmd]> = paths.iter().map(Vec::as_slice).collect();
        fill_path(
            &mut tile,
            &refs,
            color,
            1.0,
            PathTransform {
                scale_x: scale,
                scale_y: scale,
                dx: 0.0,
                dy: 0.0,
            },
        );
    }
    tile
}

fn coverage(tile: &Tile) -> f32 {
    tile.pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| f32::from(pixel[3]) / 255.0)
        .sum()
}

fn hex_rgb(value: &str) -> [f32; 3] {
    let value = value.strip_prefix('#').expect("#RRGGBB");
    assert_eq!(value.len(), 6, "asset colors use #RRGGBB");
    [
        u8::from_str_radix(&value[0..2], 16).expect("red") as f32 / 255.0,
        u8::from_str_radix(&value[2..4], 16).expect("green") as f32 / 255.0,
        u8::from_str_radix(&value[4..6], 16).expect("blue") as f32 / 255.0,
    ]
}

fn luma(layer: &toml::Value) -> f32 {
    let [r, g, b] = hex_rgb(layer["ref_fill"].as_str().expect("ref_fill"));
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

fn write_preview(path: &std::path::Path, tile: &Tile, dark: bool) {
    let background: [u8; 3] = if dark {
        [0x1A, 0x1B, 0x26]
    } else {
        [0xFA, 0xFA, 0xF4]
    };
    let mut rgb = Vec::with_capacity((tile.width() * tile.height() * 3) as usize);
    for pixel in tile.pixels().as_chunks::<4>().0 {
        let alpha = f32::from(pixel[3]) / 255.0;
        for channel in 0..3 {
            rgb.push(
                (f32::from(pixel[channel]) * alpha + f32::from(background[channel]) * (1.0 - alpha))
                    .round() as u8,
            );
        }
    }
    let file = std::fs::File::create(path).expect("create preview PNG");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), tile.width(), tile.height());
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("write PNG header")
        .write_image_data(&rgb)
        .expect("write PNG pixels");
}

#[test]
fn hero_assets_are_semantic_bounded_and_layout_stable() {
    let assets: Vec<Asset> = HEROES.iter().map(|id| load(id)).collect();
    for asset in &assets {
        assert_eq!(
            asset.id,
            asset_path(&asset.id).file_stem().unwrap().to_string_lossy()
        );
        assert!(
            asset.layers.len() <= 14,
            "{} has {} layers; hero art must stay cheap to bake",
            asset.id,
            asset.layers.len()
        );
        assert_eq!(role(&asset.layers[0]), "outline", "outline paints first");
        assert_eq!(
            asset
                .layers
                .iter()
                .filter(|layer| role(layer) == "outline")
                .count(),
            1,
            "{} uses one coherent silhouette",
            asset.id
        );
        assert_eq!(
            role_layer(asset, "outline")["recolor"].as_str(),
            Some("fixed")
        );
        assert_eq!(role_layer(asset, "coat")["recolor"].as_str(), Some("coat"));
        assert!(
            luma(role_layer(asset, "outline")) < 0.25,
            "light-theme ink is dark"
        );
        assert!(
            luma(role_layer(asset, "muzzle")) > 0.80,
            "muzzle protects face contrast"
        );

        let commands: usize = asset
            .layers
            .iter()
            .flat_map(parsed_paths)
            .map(|path| path.len())
            .sum();
        assert!(commands <= 240, "{} has {commands} path commands", asset.id);
    }

    for asset in &assets[..3] {
        for required in HEAD_ROLES {
            role_layer(asset, required);
        }
        assert_eq!(
            asset.viewbox, assets[0].viewbox,
            "reaction swap changes no size"
        );
        assert_eq!(
            asset.anchor, assets[0].anchor,
            "reaction swap changes no anchor"
        );
    }
    assert!(
        assets[3].viewbox.0 / assets[3].viewbox.1 > 1.35,
        "the cursor stretch pose must remain visibly wide"
    );
    role_layer(&assets[3], "accessory");
    role_layer(&assets[3], "pattern");
}

#[test]
fn hero_faces_survive_a_sixteen_pixel_bake() {
    for id in HEROES {
        let asset = load(id);
        let core = render_roles(&asset, &["outline", "coat"], 16);
        assert!(
            coverage(&core) >= 55.0,
            "{id}: silhouette collapsed at 16 px"
        );

        for (feature, minimum) in [
            ("inner_ear", 0.7),
            ("muzzle", 2.0),
            ("eye", 1.0),
            ("nose", 0.2),
            ("mouth", 0.45),
            ("whisker", 1.5),
        ] {
            let mask = render_roles(&asset, &[feature], 16);
            let actual = coverage(&mask);
            assert!(
                actual >= minimum,
                "{id}: `{feature}` has only {actual:.2}px coverage at 16 px"
            );
        }
    }
}

#[test]
fn repaired_roster_heads_are_semantic_bounded_and_legible() {
    let mut rendered = Vec::new();
    for id in REPAIRS {
        let asset = load(id);
        assert!(
            asset.layers.len() <= 14,
            "{id}: {} layers exceed the hero bake budget",
            asset.layers.len()
        );
        assert_eq!(
            role(&asset.layers[0]),
            "outline",
            "{id}: silhouette paints first"
        );
        assert_eq!(
            asset
                .layers
                .iter()
                .filter(|layer| role(layer) == "outline")
                .count(),
            1,
            "{id}: exterior ink must be one coherent silhouette"
        );
        for required in REPAIR_ROLES {
            role_layer(&asset, required);
        }
        assert_eq!(role_layer(&asset, "coat")["recolor"].as_str(), Some("coat"));
        assert!(luma(role_layer(&asset, "outline")) < 0.25);

        let commands: usize = asset
            .layers
            .iter()
            .flat_map(parsed_paths)
            .map(|path| path.len())
            .sum();
        assert!(
            commands <= 240,
            "{id}: {commands} commands exceed the art budget"
        );
        assert!(
            (1.10..=1.30).contains(&(asset.viewbox.0 / asset.viewbox.1)),
            "{id}: implausible head aspect {:?}",
            asset.viewbox
        );
        assert!(
            (0.45..=0.58).contains(&asset.anchor.0),
            "{id}: eye anchor drift"
        );
        assert_eq!(
            asset.anchor.1, 0.5,
            "{id}: head must stay horizontally centered"
        );
        assert_eq!(asset.anchor.2, 1.0, "{id}: word-top anchor drift");

        let core = render_roles(&asset, &["outline", "coat"], 16);
        assert!(
            coverage(&core) >= 55.0,
            "{id}: silhouette collapsed at 16 px"
        );
        for (feature, minimum) in [
            ("eye", 0.9),
            ("nose", 0.15),
            ("mouth", 0.35),
            ("whisker", 1.0),
        ] {
            let actual = coverage(&render_roles(&asset, &[feature], 16));
            assert!(
                actual >= minimum,
                "{id}: `{feature}` has only {actual:.2}px coverage at 16 px"
            );
        }
        rendered.push((id, render_context(&asset, 16, false)));
    }

    for left in 0..rendered.len() {
        for right in left + 1..rendered.len() {
            assert_ne!(
                rendered[left].1.pixels(),
                rendered[right].1.pixels(),
                "{} and {} alias at 16 px",
                rendered[left].0,
                rendered[right].0
            );
        }
    }
}

#[test]
fn cursor_reactions_remain_visibly_distinct_at_terminal_size() {
    let expression_roles = ["eye", "iris", "catch_light", "mouth", "pink"];
    let happy = render_roles(&load("s1_03"), &expression_roles, 16);
    let meow = render_roles(&load("s1_15"), &expression_roles, 16);
    let wink = render_roles(&load("s1_21"), &expression_roles, 16);
    assert_ne!(
        happy.pixels(),
        meow.pixels(),
        "happy and meow must not alias"
    );
    assert_ne!(
        happy.pixels(),
        wink.pixels(),
        "happy and wink must not alias"
    );
    assert_ne!(meow.pixels(), wink.pixels(), "meow and wink must not alias");
}

#[test]
fn authored_characters_stay_clean_bounded_and_readable() {
    for id in ARTIST_PASS {
        let asset = load(id);
        assert!(
            asset.layers.len() <= 14,
            "{id}: {} layers exceed the authored-art budget",
            asset.layers.len()
        );
        assert_eq!(role(&asset.layers[0]), "outline", "{id}: silhouette first");
        assert_eq!(
            asset
                .layers
                .iter()
                .filter(|layer| role(layer) == "outline")
                .count(),
            1,
            "{id}: use one intentional outer silhouette, not patch fragments"
        );
        for required in ["outline", "coat", "eye", "nose", "mouth", "whisker"] {
            role_layer(&asset, required);
        }
        assert!(
            matches!(
                role_layer(&asset, "coat")["recolor"].as_str(),
                Some("coat" | "fixed")
            ),
            "{id}: coat must use a declared palette policy"
        );

        let commands: usize = asset
            .layers
            .iter()
            .flat_map(parsed_paths)
            .map(|path| path.len())
            .sum();
        assert!(
            commands <= 240,
            "{id}: {commands} commands exceed the tiny-art geometry budget"
        );

        let silhouette = render_roles(&asset, &["outline", "coat"], 16);
        assert!(
            coverage(&silhouette) >= 35.0,
            "{id}: silhouette collapsed at 16 px"
        );
        for (feature, minimum) in [("eye", 0.35), ("nose", 0.10), ("mouth", 0.20)] {
            let actual = coverage(&render_roles(&asset, &[feature], 16));
            assert!(
                actual >= minimum,
                "{id}: `{feature}` has only {actual:.2}px coverage at 16 px"
            );
        }
    }
}

#[test]
fn polished_faces_keep_visible_non_textual_features_at_sixteen_pixels() {
    for (id, minimum_eye_coverage) in TINY_FACE_POLISH {
        let asset = load(id);
        let eye_coverage = coverage(&render_roles(&asset, &["eye"], 16));
        assert!(
            eye_coverage >= minimum_eye_coverage,
            "{id}: eye coverage {eye_coverage:.2}px collapsed below {minimum_eye_coverage:.2}px"
        );
        assert!(
            (luma(role_layer(&asset, "eye")) - luma(role_layer(&asset, "coat"))).abs() >= 0.20,
            "{id}: eye and coat lost their authored luminance separation"
        );
        assert!(
            coverage(&render_roles(&asset, &["mouth"], 16)) >= 0.25,
            "{id}: mouth collapsed into punctuation noise"
        );
    }

    role_layer(&load("s1_18"), "muzzle");
}

#[test]
fn authored_accessories_have_no_backdrop_or_fragmented_outline() {
    for id in ACCESSORIES {
        let asset = load(id);
        assert!(
            asset.layers.len() <= 6,
            "{id}: {} layers exceed the accessory budget",
            asset.layers.len()
        );
        assert_eq!(role(&asset.layers[0]), "outline", "{id}: silhouette first");
        assert_eq!(
            asset
                .layers
                .iter()
                .filter(|layer| role(layer) == "outline")
                .count(),
            1,
            "{id}: accessory needs one coherent outline"
        );
        role_layer(&asset, "accessory");

        let commands: usize = asset
            .layers
            .iter()
            .flat_map(parsed_paths)
            .map(|path| path.len())
            .sum();
        assert!(commands <= 100, "{id}: {commands} commands are too noisy");
        assert!(
            coverage(&render_roles(&asset, &[], 16)) >= 20.0,
            "{id}: accessory disappears at 16 px"
        );
    }
}

#[test]
fn dump_hero_context_crops_when_requested() {
    let Some(directory) = std::env::var_os("ATERM_CAT_ART_DUMP_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).expect("create crop directory");
    for id in HEROES
        .into_iter()
        .chain(REPAIRS)
        .chain(ARTIST_PASS)
        .chain(ACCESSORIES)
    {
        let asset = load(id);
        for height in [16, 24, 32] {
            for dark in [false, true] {
                let theme = if dark { "dark" } else { "light" };
                let tile = render_context(&asset, height, dark);
                write_preview(
                    &directory.join(format!("{id}_{height}_{theme}.png")),
                    &tile,
                    dark,
                );
            }
        }
    }

    let s100 = load("s1_00");
    for coat in [0, 1] {
        for height in [16, 24, 32] {
            let width = (height as f32 * s100.viewbox.0 / s100.viewbox.1).round() as u32;
            for dark in [false, true] {
                let theme = if dark { "dark" } else { "light" };
                let colors = CatColorKey {
                    accent: 12,
                    background: if dark { 0 } else { 3 },
                };
                let fills = ResolvedFills::from_context(coat, 4, colors);
                let tile = bake_variant(CatGlyphId::S100, &fills, width, height);
                write_preview(
                    &directory.join(format!("s100_coat{coat:02}_{height}_{theme}.png")),
                    &tile,
                    dark,
                );
            }
        }
    }
}
