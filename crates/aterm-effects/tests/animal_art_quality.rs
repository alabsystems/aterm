// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The animal-roster art gates, over the asset TOML SOURCES (like
//! `cat_art_quality`, so review works pre-codegen). Every species head in
//! `art/animal/` is held to one shared bar — the roster is authored by many
//! hands and must read as one species of art:
//!
//! * semantic + bounded: one dark outline first, all-`fixed` recolors, layer
//!   and path-command budgets, sane anchors and aspect;
//! * legible at terminal size: silhouette and eye coverage floors at a 16 px
//!   bake, and pairwise distinctness (a zebra must not alias a horse);
//! * complete: the lexicon's species tags and the authored assets are the SAME
//!   set — every animal word the scanner can match has art to show, and no
//!   orphan art ships untriggerable.

use std::collections::BTreeSet;
use std::path::PathBuf;

use aterm_scene::vector::PathCmd;
use aterm_scene::vector::parse_path;
use aterm_scene::{PathTransform, Tile, fill_path};

fn animal_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("art/animal")
}

struct Asset {
    stem: String,
    id: String,
    kind: String,
    viewbox: (f32, f32),
    anchor: (f32, f32, f32),
    layers: Vec<toml::Value>,
}

fn number(value: &toml::Value) -> f32 {
    value
        .as_float()
        .map(|v| v as f32)
        .or_else(|| value.as_integer().map(|v| v as f32))
        .expect("numeric asset value")
}

fn load_all() -> Vec<Asset> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(animal_dir())
        .expect("read art/animal")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "the animal roster must not be empty");
    files
        .iter()
        .map(|path| {
            let stem = path.file_stem().unwrap().to_str().unwrap().to_owned();
            let source = std::fs::read_to_string(path).expect("read animal asset");
            let doc: toml::Value = source
                .parse()
                .unwrap_or_else(|e| panic!("{stem}: parse: {e}"));
            let viewbox = doc["viewbox"].as_array().expect("viewbox array");
            let anchor = doc["anchor"].as_table().expect("anchor table");
            Asset {
                stem,
                id: doc["id"].as_str().expect("id").to_owned(),
                kind: doc["kind"].as_str().expect("kind").to_owned(),
                viewbox: (number(&viewbox[0]), number(&viewbox[1])),
                anchor: (
                    number(&anchor["eye_y"]),
                    number(&anchor["center_x"]),
                    number(&anchor["word_top"]),
                ),
                layers: doc["layer"].as_array().expect("layer array").clone(),
            }
        })
        .collect()
}

fn role(layer: &toml::Value) -> &str {
    layer["role"].as_str().expect("semantic layer role")
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

/// Render every layer in its authored `ref_fill` (all animal layers are
/// `fixed`, so this IS the shipped palette up to outline context ink).
fn render_species_colors(asset: &Asset, height: u32) -> Tile {
    let scale = height as f32 / asset.viewbox.1;
    let width = (asset.viewbox.0 * scale).round().max(1.0) as u32;
    let mut tile = Tile::new(width, height);
    for layer in &asset.layers {
        let [r, g, b] = hex_rgb(layer["ref_fill"].as_str().expect("ref_fill"));
        let paths = parsed_paths(layer);
        let refs: Vec<&[PathCmd]> = paths.iter().map(Vec::as_slice).collect();
        fill_path(
            &mut tile,
            &refs,
            (r, g, b),
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

fn commands(asset: &Asset) -> usize {
    asset
        .layers
        .iter()
        .flat_map(parsed_paths)
        .map(|path| path.len())
        .sum()
}

#[test]
fn animal_heads_are_semantic_bounded_and_species_true() {
    for a in load_all() {
        let id = &a.id;
        assert_eq!(&a.stem, id, "{id}: file stem and id must match");
        assert_eq!(a.kind, "head", "{id}: animal roster assets are heads");
        let aspect = a.viewbox.0 / a.viewbox.1;
        assert!(
            (0.95..=1.45).contains(&aspect),
            "{id}: aspect {aspect} outside the head band"
        );
        let (eye_y, center_x, word_top) = a.anchor;
        // Wider than the cat heads' band on purpose: the roster has honest
        // eyes-on-top shapes (frog, crocodile) and whole-body reads whose
        // face is a top bump (ladybug). The animal render path never squashes
        // toward the eye line, so the anchor is descriptive, not load-bearing.
        assert!(
            (0.18..=0.68).contains(&eye_y),
            "{id}: eye_y {eye_y} outside the portrait band"
        );
        assert!(
            (0.45..=0.55).contains(&center_x),
            "{id}: center_x {center_x} — keep the visual center near the word midpoint"
        );
        assert!(
            (word_top - 1.0).abs() < f32::EPSILON,
            "{id}: word_top must be 1.0 (heads meet the word at the art's bottom)"
        );
        assert!(a.layers.len() <= 14, "{id}: more than 14 layers");
        assert_eq!(
            role(&a.layers[0]),
            "outline",
            "{id}: the outline paints first"
        );
        let outlines = a.layers.iter().filter(|l| role(l) == "outline").count();
        assert_eq!(outlines, 1, "{id}: exactly one outline layer");
        assert!(
            luma(&a.layers[0]) < 0.30,
            "{id}: outline reference fill must be dark (luma < 0.30)"
        );
        for layer in &a.layers {
            assert_eq!(
                layer["recolor"].as_str().expect("recolor"),
                "fixed",
                "{id}: animal layers are species-true (`fixed`) — no genome recolor"
            );
        }
        assert!(
            a.layers.iter().any(|l| role(l) == "eye"),
            "{id}: every species head authors eyes"
        );
        let cmds = commands(&a);
        assert!(cmds <= 240, "{id}: {cmds} path commands exceeds the budget");
    }
}

#[test]
fn animal_faces_survive_a_sixteen_pixel_bake() {
    for a in load_all() {
        let id = &a.id;
        let silhouette = coverage(&render_roles(&a, &["outline"], 16));
        assert!(
            silhouette >= 35.0,
            "{id}: 16 px silhouette coverage {silhouette} too thin"
        );
        let eyes = coverage(&render_roles(&a, &["eye"], 16));
        assert!(
            eyes >= 0.35,
            "{id}: 16 px eye coverage {eyes} — the face goes blank at terminal size"
        );
    }
}

#[test]
fn species_heads_do_not_alias_each_other_at_sprite_size() {
    let assets = load_all();
    let masks: Vec<(String, Vec<u16>)> = assets
        .iter()
        .map(|a| {
            // Color-aware, coarsely quantized: at sprite size the palette IS
            // part of a species' signature (a white eagle head and a red
            // parrot head may share a bird silhouette), but the 3-bit
            // channels keep near-identical palettes from excusing identical
            // shapes.
            let tile = render_species_colors(a, 16);
            let mask = tile
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| {
                    if p[3] > 96 {
                        0x1000
                            | (u16::from(p[0] >> 5) << 6)
                            | (u16::from(p[1] >> 5) << 3)
                            | u16::from(p[2] >> 5)
                    } else {
                        0
                    }
                })
                .collect();
            (a.id.clone(), mask)
        })
        .collect();
    for i in 0..masks.len() {
        for j in i + 1..masks.len() {
            assert!(
                masks[i].1 != masks[j].1,
                "{} and {} alias at 16 px — one of them needs a stronger signature",
                masks[i].0,
                masks[j].0
            );
        }
    }
}

/// The completeness gate — the reason this roster exists: EVERY species tag
/// the builtin lexicon can attach to a word has authored art, and every
/// authored head is reachable from at least one word. One set, two sources —
/// plus the one cross-class seam: the `dog` head carries no species tag
/// because dog words are [`aterm_lexicon::Class::Canine`] (the typed dog
/// summon's class), which the Occurrence resolver pins to species `dog`. The
/// seam is asserted, not exempted: the canine class must actually reach the
/// head, or the head really is orphaned.
#[test]
fn lexicon_species_and_authored_roster_are_the_same_set() {
    let authored: BTreeSet<String> = load_all().into_iter().map(|a| a.id).collect();
    let mut tagged: BTreeSet<String> = aterm_lexicon::Lexicon::builtin()
        .species_codes()
        .iter()
        .cloned()
        .collect();
    // The canine seam: `dog` is summonable iff the builtin lexicon classifies
    // a dog word as Canine (the resolver maps Canine → species "dog").
    assert_eq!(
        aterm_lexicon::Lexicon::builtin().classify_token("dog"),
        Some(aterm_lexicon::Class::Canine),
        "the canine class must cover the dog head"
    );
    tagged.insert("dog".to_string());
    let missing_art: Vec<_> = tagged.difference(&authored).collect();
    let orphan_art: Vec<_> = authored.difference(&tagged).collect();
    assert!(
        missing_art.is_empty() && orphan_art.is_empty(),
        "lexicon tags without art: {missing_art:?}; authored heads no word can summon: {orphan_art:?}"
    );
}

/// The generated roster stays in lock-step with the sources (the include-level
/// drift test lives in `cat_glyphs_codegen`; this cross-checks the compiled
/// lookup the renderer actually uses).
#[test]
fn every_authored_head_resolves_through_the_compiled_lookup() {
    for a in load_all() {
        assert!(
            aterm_effects::animal_glyphs_gen::animal_glyph_from_key(&a.id).is_some(),
            "{}: not in the compiled roster — rerun gen_animal_glyphs",
            a.id
        );
    }
}
