// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Glyph codegen (docs/cat-art-v4-design.md §1): turns the glyph asset TOMLs
//! under `art/glyphs/` into the checked-in drawlist module [`crate::cat_glyphs_gen`].
//!
//! [`generate_from_assets`] is a pure function (asset texts → Rust source), run by
//! the `gen_cat_glyphs` example and pinned byte-for-byte by the
//! `cat_glyphs_gen_matches_assets` drift test. It opens no file: the shipped
//! engine names no `std::fs` (docs/ATERM_DESIGN.md §2.7; aterm-census guard
//! `engine-platform`), so the reader lives in `examples/support/asset_dir.rs`.
//!
//! The pet, dog, animal and Robi generators share the parse/quantize/fail-closed
//! path and emit separate modules that import the cat module's shape types, so one
//! drawlist walker paints every roster. They never join `GLYPHS`: every
//! `GlyphKind::Special` there is a collectible kitty-log row, and the spec model
//! pins `RosterCap` to `GLYPH_IDS.len()`.
//!
//! Only roster kinds (`head`, `special`, `accessory`) not marked `excluded = true`
//! are emitted. Coordinates are quantized from the `viewbox` frame to the integer
//! `0..=FIXED_ONE` frame, so the drawlist is `Eq`-safe and byte-deterministic.

use aterm_scene::vector::{FIXED_ONE, PathCmd};

/// The kinds that make it into the roster (in emit order: heads, then specials, then
/// accessories). `prop` / `unknown` assets are held out (§0/§4/§8).
const INCLUDED_KINDS: &[&str] = &["head", "special", "accessory"];

/// Canonical `role` vocabulary → Rust variant, in fixed painter-role order
/// (docs/cat-art-v4-design.md §0). The emitted `GlyphRole` enum is this list
/// filtered to roles the roster uses; an unknown asset role is a hard error.
const ROLE_VOCAB: &[(&str, &str)] = &[
    ("outline", "Outline"),
    ("coat", "Coat"),
    ("coat_shade", "CoatShade"),
    ("muzzle", "Muzzle"),
    ("inner_ear", "InnerEar"),
    ("eye", "Eye"),
    ("iris", "Iris"),
    ("catch_light", "CatchLight"),
    ("nose", "Nose"),
    ("mouth", "Mouth"),
    ("whisker", "Whisker"),
    ("blush", "Blush"),
    ("pattern", "Pattern"),
    ("pink", "Pink"),
    ("accessory", "Accessory"),
    ("detail", "Detail"),
];

/// Canonical `recolor` vocabulary → variant (§2.1). Filtered to what the roster uses.
const RECOLOR_VOCAB: &[(&str, &str)] = &[
    ("coat", "Coat"),
    ("coat_shade", "CoatShade"),
    ("iris", "Iris"),
    ("fixed", "Fixed"),
];

/// Canonical `kind` vocabulary → variant. Only [`INCLUDED_KINDS`] are ever emitted, but
/// the table also validates that an included asset's kind is a known one.
const KIND_VOCAB: &[(&str, &str)] = &[
    ("head", "Head"),
    ("special", "Special"),
    ("accessory", "Accessory"),
    ("prop", "Prop"),
];

/// The authored-asset schema is path-only: unknown keys fail closed for roster
/// glyphs, so a `text`, `label` or `font` field is never mistaken for artwork or
/// silently ignored.
const GLYPH_KEYS: &[&str] = &["id", "kind", "viewbox", "anchor", "layer", "excluded"];
const ANCHOR_KEYS: &[&str] = &["eye_y", "center_x", "word_top", "attach"];
const LAYER_KEYS: &[&str] = &["role", "ref_fill", "recolor", "paths"];

/// One emitted layer: role + recolor variant names, its source `ref_fill` (packed
/// `0x00RR_GGBB`), and its quantized paths.
struct Layer {
    role: &'static str,
    recolor: &'static str,
    /// The layer's reference colour: baked verbatim for `Recolor::Fixed`;
    /// `Coat`/`Iris` layers bake a ramp colour instead.
    fill: u32,
    /// Each path is a sequence of `PathSeg` literal strings (e.g. `PathSeg::Move(1,2)`).
    paths: Vec<Vec<String>>,
}

/// One emitted glyph: its id, kind, quantized anchors, and layers.
struct Glyph {
    id: String,
    /// Emit-order rank of the kind: 0 head, 1 special, 2 accessory.
    kind_rank: usize,
    kind: &'static str,
    variant: String,
    /// Viewbox width/height, quantized to a fixed thousandth. Runtime layout
    /// preserves the authored art's proportions without parsing asset TOML.
    aspect_x1000: u16,
    eye_y: u16,
    center_x: u16,
    word_top: u16,
    layers: Vec<Layer>,
}

/// Generate `cat_glyphs_gen.rs` from `(file name, TOML text)` pairs read from
/// `art/glyphs/`. Deterministic: glyphs are ordered (heads, specials,
/// accessories; each by `id`) and every coordinate is integer-quantized.
///
/// # Errors
/// Any malformed TOML, unknown role/recolor/kind, missing viewbox/anchor, unknown
/// roster-schema key, unparseable path `d`-string, or colliding variant name.
pub fn generate_from_assets(assets: &[(String, String)]) -> Result<String, String> {
    Ok(emit(&load_glyphs(assets)?))
}

/// Generate `pet_glyphs_gen.rs` from the pose texts `assets`: the same parse,
/// quantization and key rejection as [`generate_from_assets`], emitted as the
/// separate `PetGlyphId` / `PET_GLYPHS` roster reusing the cat module's types.
/// The poses are one kind, so the order is by id.
///
/// # Errors
/// As [`generate_from_assets`].
pub fn generate_pet_from_assets(assets: &[(String, String)]) -> Result<String, String> {
    Ok(emit_pet(&load_glyphs(assets)?))
}

/// Generate `dog_glyphs_gen.rs` from the breed texts `assets`: a separate roster
/// reusing the cat module's shape types (the dog cameo is not a collectible).
///
/// # Errors
/// As [`generate_from_assets`].
pub fn generate_dog_from_assets(assets: &[(String, String)]) -> Result<String, String> {
    Ok(emit_dog(&load_glyphs(assets)?))
}

/// Generate `animal_glyphs_gen.rs` from the species-head texts `assets`: the
/// separate `AnimalGlyphId` / `ANIMAL_GLYPHS` roster for the ambient animal-word
/// decoration. The asset `id` doubles as the lexicon's `species` key, so it also
/// emits the key → id resolver.
///
/// # Errors
/// As [`generate_from_assets`].
pub fn generate_animal_from_assets(assets: &[(String, String)]) -> Result<String, String> {
    Ok(emit_animal_roster(&load_glyphs(assets)?))
}

/// Generate `robi_glyphs_gen.rs` from the pose texts `assets`: a separate roster
/// of the helper robot's animation frames, reusing the cat module's shape types.
///
/// # Errors
/// As [`generate_from_assets`].
pub fn generate_robi_from_assets(assets: &[(String, String)]) -> Result<String, String> {
    Ok(emit_robi(&load_glyphs(assets)?))
}

/// Parse, order and collision-check every roster asset text, for all rosters.
/// Texts are visited in file-name order, so the first error named is stable.
fn load_glyphs(assets: &[(String, String)]) -> Result<Vec<Glyph>, String> {
    let mut files: Vec<&(String, String)> = assets.iter().collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut glyphs: Vec<Glyph> = Vec::new();
    for (name, text) in files {
        let doc: aterm_toml::Value = text.parse().map_err(|e| format!("parse {name}: {e}"))?;
        if let Some(g) = glyph_from_doc(&doc, name)? {
            glyphs.push(g);
        }
    }

    // Emit order: kind rank (head < special < accessory), then id.
    glyphs.sort_by(|a, b| a.kind_rank.cmp(&b.kind_rank).then_with(|| a.id.cmp(&b.id)));

    // Variant-name collision guard: the ids are unique, but the derived
    // variants must be too.
    let mut seen = std::collections::BTreeSet::new();
    for g in &glyphs {
        if !seen.insert(g.variant.clone()) {
            return Err(format!(
                "variant-name collision: `{}` derived from id `{}` twice",
                g.variant, g.id
            ));
        }
    }

    Ok(glyphs)
}

/// Parse one asset doc into a [`Glyph`], or `Ok(None)` if it is not a roster glyph
/// (wrong kind, or `excluded = true`).
fn glyph_from_doc(doc: &aterm_toml::Value, name: &str) -> Result<Option<Glyph>, String> {
    if doc.get("excluded").and_then(aterm_toml::Value::as_bool) == Some(true) {
        return Ok(None);
    }
    let id = doc
        .get("id")
        .and_then(aterm_toml::Value::as_str)
        .ok_or_else(|| format!("{name}: missing `id`"))?
        .to_string();
    let kind_str = doc
        .get("kind")
        .and_then(aterm_toml::Value::as_str)
        .ok_or_else(|| format!("{name}: missing `kind`"))?;
    let Some(kind_rank) = INCLUDED_KINDS.iter().position(|k| *k == kind_str) else {
        // Not a roster kind (prop / unknown / …): held out, not an error.
        return Ok(None);
    };
    reject_unknown_keys(doc, GLYPH_KEYS, name, "glyph")?;
    let kind = lookup(KIND_VOCAB, kind_str)
        .ok_or_else(|| format!("{name}: kind `{kind_str}` not in KIND_VOCAB"))?;

    let vb = doc
        .get("viewbox")
        .and_then(aterm_toml::Value::as_array)
        .filter(|a| a.len() == 2)
        .ok_or_else(|| format!("{name}: missing `viewbox = [w, h]`"))?;
    let (vw, vh) = (num(&vb[0]), num(&vb[1]));
    if !vw.is_finite() || !vh.is_finite() || vw <= 0.0 || vh <= 0.0 {
        return Err(format!("{name}: degenerate viewbox [{vw}, {vh}]"));
    }

    let anchor = doc
        .get("anchor")
        .ok_or_else(|| format!("{name}: missing `anchor`"))?;
    reject_unknown_keys(anchor, ANCHOR_KEYS, name, "anchor")?;
    let anchor_frac = |key: &str| -> Result<u16, String> {
        let f = anchor
            .get(key)
            .map(num)
            .ok_or_else(|| format!("{name}: anchor missing `{key}`"))?;
        Ok(quant_frac(f))
    };
    let eye_y = anchor_frac("eye_y")?;
    let center_x = anchor_frac("center_x")?;
    let word_top = anchor_frac("word_top")?;

    let mut layers = Vec::new();
    for (li, layer) in doc
        .get("layer")
        .and_then(aterm_toml::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        reject_unknown_keys(layer, LAYER_KEYS, name, &format!("layer {li}"))?;
        let role_str = layer
            .get("role")
            .and_then(aterm_toml::Value::as_str)
            .ok_or_else(|| format!("{name}: layer {li} missing `role`"))?;
        let role = lookup(ROLE_VOCAB, role_str)
            .ok_or_else(|| format!("{name}: layer {li} role `{role_str}` not in ROLE_VOCAB"))?;
        let recolor_str = layer
            .get("recolor")
            .and_then(aterm_toml::Value::as_str)
            .ok_or_else(|| format!("{name}: layer {li} missing `recolor`"))?;
        let recolor = lookup(RECOLOR_VOCAB, recolor_str).ok_or_else(|| {
            format!("{name}: layer {li} recolor `{recolor_str}` not in RECOLOR_VOCAB")
        })?;
        let fill = layer
            .get("ref_fill")
            .and_then(aterm_toml::Value::as_str)
            .and_then(parse_hex_rgb)
            .ok_or_else(|| format!("{name}: layer {li} missing/invalid `ref_fill`"))?;

        let mut paths = Vec::new();
        for (pi, p) in layer
            .get("paths")
            .and_then(aterm_toml::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .enumerate()
        {
            let d = p
                .as_str()
                .ok_or_else(|| format!("{name}: layer {li} path {pi} is not a string"))?;
            let cmds = aterm_scene::vector::parse_path(d)
                .ok_or_else(|| format!("{name}: layer {li} path {pi} unparseable `d`-string"))?;
            let segs = quantize_path(&cmds, vw, vh);
            if !segs.is_empty() {
                paths.push(segs);
            }
        }
        if !paths.is_empty() {
            layers.push(Layer {
                role,
                recolor,
                fill,
                paths,
            });
        }
    }

    Ok(Some(Glyph {
        variant: variant_name(&id),
        id,
        kind_rank,
        kind,
        aspect_x1000: ((vw / vh) * 1000.0).round().clamp(1.0, f32::from(u16::MAX)) as u16,
        eye_y,
        center_x,
        word_top,
        layers,
    }))
}

fn reject_unknown_keys(
    value: &aterm_toml::Value,
    allowed: &[&str],
    name: &str,
    scope: &str,
) -> Result<(), String> {
    let table = value
        .as_table()
        .ok_or_else(|| format!("{name}: {scope} must be a table"))?;
    if let Some(key) = table.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!(
            "{name}: unsupported {scope} key `{key}`; cat glyphs are path-only"
        ));
    }
    Ok(())
}

/// Quantize one parsed path into `PathSeg` literal strings in the `0..=FIXED_ONE` frame.
fn quantize_path(cmds: &[PathCmd], vw: f32, vh: f32) -> Vec<String> {
    let qx = |v: f32| quant(v, vw);
    let qy = |v: f32| quant(v, vh);
    cmds.iter()
        .map(|c| match *c {
            PathCmd::Move(x, y) => format!("PathSeg::Move({},{})", qx(x), qy(y)),
            PathCmd::Line(x, y) => format!("PathSeg::Line({},{})", qx(x), qy(y)),
            PathCmd::Cubic(x1, y1, x2, y2, x, y) => format!(
                "PathSeg::Cubic({},{},{},{},{},{})",
                qx(x1),
                qy(y1),
                qx(x2),
                qy(y2),
                qx(x),
                qy(y)
            ),
            PathCmd::Close => "PathSeg::Close".to_string(),
        })
        .collect()
}

/// `round(v / extent * FIXED_ONE)`, clamped to `0..=FIXED_ONE`. The integer rule is the
/// same on every host (no platform float variance in the const data).
fn quant(v: f32, extent: f32) -> u16 {
    let q = (v / extent * f32::from(FIXED_ONE)).round();
    q.clamp(0.0, f32::from(FIXED_ONE)) as u16
}

/// Quantize an already-normalized `0..1` anchor fraction to the fixed frame.
fn quant_frac(f: f32) -> u16 {
    (f * f32::from(FIXED_ONE))
        .round()
        .clamp(0.0, f32::from(FIXED_ONE)) as u16
}

/// Snake-case asset id → UpperCamel Rust variant (`s1_00` → `S100`, `spec_maneki` →
/// `SpecManeki`, `acc_bow` → `AccBow`). Injective over the asset ids (guarded).
fn variant_name(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    let mut cap = true;
    for ch in id.chars() {
        if ch == '_' {
            cap = true;
            continue;
        }
        if cap {
            out.extend(ch.to_uppercase());
            cap = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn lookup(vocab: &[(&str, &'static str)], key: &str) -> Option<&'static str> {
    vocab.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Parse a `#RRGGBB` asset colour into packed `0x00RR_GGBB`.
fn parse_hex_rgb(s: &str) -> Option<u32> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(h, 16).ok()
}

fn num(v: &aterm_toml::Value) -> f32 {
    v.as_float()
        .map(|f| f as f32)
        .or_else(|| v.as_integer().map(|i| i as f32))
        .unwrap_or(0.0)
}

/// Render the final Rust source. Deterministic given ordered `glyphs`.
fn emit(glyphs: &[Glyph]) -> String {
    let mut s = String::new();
    s.push_str(HEADER);

    // Enum variant sets that the roster actually uses (canonical order, filtered).
    let used_roles = present(
        ROLE_VOCAB,
        glyphs.iter().flat_map(|g| g.layers.iter().map(|l| l.role)),
    );
    let used_recolors = present(
        RECOLOR_VOCAB,
        glyphs
            .iter()
            .flat_map(|g| g.layers.iter().map(|l| l.recolor)),
    );
    let used_kinds = present(KIND_VOCAB, glyphs.iter().map(|g| g.kind));

    emit_enum(
        &mut s,
        "GlyphRole",
        "Painter role of a glyph layer (§0).",
        &used_roles,
    );
    emit_enum(
        &mut s,
        "Recolor",
        "Which genome channel recolors a layer at bake time (§2.1).",
        &used_recolors,
    );
    emit_enum(
        &mut s,
        "GlyphKind",
        "The roster class of a glyph.",
        &used_kinds,
    );

    s.push_str(STRUCTS);

    s.push_str(
        "/// The glyph roster: heads first, then specials, then accessories. Indexes [`GLYPHS`].\n",
    );
    s.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n");
    s.push_str("pub enum CatGlyphId {\n");
    for g in glyphs {
        s.push_str("    ");
        s.push_str(&g.variant);
        s.push_str(",\n");
    }
    s.push_str("}\n\n");

    // Stable roster: persistence stores the `GlyphDef::id` string and resolves
    // it through this table, never a Rust discriminant that could shift. Every
    // other roster's `*_GLYPH_IDS` follows the same rule.
    s.push_str("/// Every glyph id in [`GLYPHS`] order. Pair with `GLYPHS[i].id` for stable string lookup.\n");
    s.push_str("pub const GLYPH_IDS: &[CatGlyphId] = &[\n");
    for g in glyphs {
        s.push_str(&format!("    CatGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n\n");

    s.push_str("/// Every roster glyph's const drawlist, indexed by [`CatGlyphId`] order.\n");
    s.push_str("pub const GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n\n");

    s.push_str("/// The head variants (the roster the genome's `variant` field indexes, §3).\n");
    s.push_str("pub const HEADS: &[CatGlyphId] = &[\n");
    for g in glyphs.iter().filter(|g| g.kind == "Head") {
        s.push_str(&format!("    CatGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n");

    s
}

/// Render the pet roster source: [`emit`]'s drawlist rows as a separate
/// `PetGlyphId` / `PET_GLYPH_IDS` / `PET_GLYPHS` roster that imports the cat
/// module's shape types, so there is one `GlyphDef` shape in the crate.
fn emit_pet(glyphs: &[Glyph]) -> String {
    let mut s = String::new();
    s.push_str(PET_HEADER);

    s.push_str("/// The pet roster: every authored full-body pose, ordered by id. Indexes\n");
    s.push_str("/// [`PET_GLYPHS`]. Not a collectible — the pet is one cat wearing the caret's\n");
    s.push_str("/// genome, so these are animation frames, not roster rows.\n");
    s.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n");
    s.push_str("pub enum PetGlyphId {\n");
    for g in glyphs {
        s.push_str("    ");
        s.push_str(&g.variant);
        s.push_str(",\n");
    }
    s.push_str("}\n\n");

    s.push_str(
        "/// Every pose id in [`PET_GLYPHS`] order. Pair with `PET_GLYPHS[i].id` for stable\n",
    );
    s.push_str("/// string lookup.\n");
    s.push_str("pub const PET_GLYPH_IDS: &[PetGlyphId] = &[\n");
    for g in glyphs {
        s.push_str(&format!("    PetGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n\n");

    s.push_str("/// Every pet pose's const drawlist, indexed by [`PetGlyphId`] order.\n");
    s.push_str("pub const PET_GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n");

    s
}

/// Render the dog roster source: [`emit`]'s drawlist rows as a separate
/// `DogGlyphId` / `DOG_GLYPH_IDS` / `DOG_GLYPHS` roster importing the cat
/// module's shape types.
fn emit_dog(glyphs: &[Glyph]) -> String {
    let mut s = String::new();
    s.push_str(DOG_HEADER);

    s.push_str("/// The dog roster: every authored breed head, ordered by id. Indexes\n");
    s.push_str("/// [`DOG_GLYPHS`]. Not a collectible — the typed-word dog cameo draws one\n");
    s.push_str("/// of these per summon; the kitty log never records them.\n");
    s.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n");
    s.push_str("pub enum DogGlyphId {\n");
    for g in glyphs {
        s.push_str("    ");
        s.push_str(&g.variant);
        s.push_str(",\n");
    }
    s.push_str("}\n\n");

    s.push_str(
        "/// Every breed id in [`DOG_GLYPHS`] order. Pair with `DOG_GLYPHS[i].id` for stable\n",
    );
    s.push_str("/// string lookup.\n");
    s.push_str("pub const DOG_GLYPH_IDS: &[DogGlyphId] = &[\n");
    for g in glyphs {
        s.push_str(&format!("    DogGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n\n");

    s.push_str("/// Every breed head's const drawlist, indexed by [`DogGlyphId`] order.\n");
    s.push_str("pub const DOG_GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n\n");

    s.push_str("/// The breed variants the typed-summon picker draws from, uniformly.\n");
    s.push_str("pub const DOG_HEADS: &[DogGlyphId] = &[\n");
    for g in glyphs.iter().filter(|g| g.kind == "Head") {
        s.push_str(&format!("    DogGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n");

    s
}

/// Render the Robi roster source: [`emit`]'s drawlist rows as a separate
/// `RobiGlyphId` / `ROBI_GLYPH_IDS` / `ROBI_GLYPHS` roster importing the cat
/// module's shape types.
fn emit_robi(glyphs: &[Glyph]) -> String {
    let mut s = String::new();
    s.push_str(ROBI_HEADER);

    s.push_str("/// The Robi roster: every authored robot pose (plus the tiling ladder\n");
    s.push_str("/// segment), ordered by id. Indexes [`ROBI_GLYPHS`]. Not a collectible —\n");
    s.push_str("/// these are animation frames for the chrome-walking helper robot.\n");
    s.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n");
    s.push_str("pub enum RobiGlyphId {\n");
    for g in glyphs {
        s.push_str("    ");
        s.push_str(&g.variant);
        s.push_str(",\n");
    }
    s.push_str("}\n\n");

    s.push_str("/// Every pose id in [`ROBI_GLYPHS`] order. Pair with `ROBI_GLYPHS[i].id` for\n");
    s.push_str("/// stable string lookup.\n");
    s.push_str("pub const ROBI_GLYPH_IDS: &[RobiGlyphId] = &[\n");
    for g in glyphs {
        s.push_str(&format!("    RobiGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n\n");

    s.push_str("/// Every Robi pose's const drawlist, indexed by [`RobiGlyphId`] order.\n");
    s.push_str("pub const ROBI_GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n");

    s
}

fn emit_animal_roster(glyphs: &[Glyph]) -> String {
    let mut s = String::new();
    s.push_str(ANIMAL_HEADER);

    s.push_str("/// The animal roster: every authored species head, ordered by id. Indexes\n");
    s.push_str("/// [`ANIMAL_GLYPHS`]. One head per species; the id string IS the lexicon's\n");
    s.push_str("/// `species` key.\n");
    s.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n");
    s.push_str("pub enum AnimalGlyphId {\n");
    for g in glyphs {
        s.push_str("    ");
        s.push_str(&g.variant);
        s.push_str(",\n");
    }
    s.push_str("}\n\n");

    s.push_str(
        "/// Every species id in [`ANIMAL_GLYPHS`] order. Pair with `ANIMAL_GLYPHS[i].id`\n",
    );
    s.push_str("/// for stable string lookup.\n");
    s.push_str("pub const ANIMAL_GLYPH_IDS: &[AnimalGlyphId] = &[\n");
    for g in glyphs {
        s.push_str(&format!("    AnimalGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n\n");

    s.push_str("/// Every species head's const drawlist, indexed by [`AnimalGlyphId`] order.\n");
    s.push_str("pub const ANIMAL_GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n\n");

    s.push_str("/// Resolve a lexicon `species` key (`ANIMAL_GLYPHS[i].id`) to its roster id.\n");
    s.push_str("/// Linear over a few dozen species; called at rescan, not per frame.\n");
    s.push_str("#[must_use]\n");
    s.push_str("pub fn animal_glyph_from_key(key: &str) -> Option<AnimalGlyphId> {\n");
    s.push_str("    ANIMAL_GLYPHS\n");
    s.push_str("        .iter()\n");
    s.push_str("        .position(|g| g.id == key)\n");
    s.push_str("        .map(|i| ANIMAL_GLYPH_IDS[i])\n");
    s.push_str("}\n");

    s
}

/// The `GlyphDef { … }` rows every roster table shares, so no two rosters can
/// drift in drawlist encoding.
fn emit_glyph_rows(s: &mut String, glyphs: &[Glyph]) {
    for g in glyphs {
        s.push_str(&format!("    // {} ({})\n", g.variant, g.id));
        s.push_str(&format!(
            "    GlyphDef {{ id: {:?}, kind: GlyphKind::{}, aspect_x1000: {}, eye_y: {}, center_x: {}, word_top: {}, layers: &[\n",
            g.id, g.kind, g.aspect_x1000, g.eye_y, g.center_x, g.word_top
        ));
        for l in &g.layers {
            s.push_str(&format!(
                "        Layer {{ role: GlyphRole::{}, recolor: Recolor::{}, fill: {:#08X}, paths: &[\n",
                l.role, l.recolor, l.fill
            ));
            for path in &l.paths {
                s.push_str("            &[");
                s.push_str(&path.join(","));
                s.push_str("],\n");
            }
            s.push_str("        ] },\n");
        }
        s.push_str("    ] },\n");
    }
}

/// The canonical-ordered subset of `vocab` variants that appear in `used`.
fn present<'a>(
    vocab: &[(&str, &'static str)],
    used: impl Iterator<Item = &'a str>,
) -> Vec<&'static str> {
    let set: std::collections::BTreeSet<&str> = used.collect();
    vocab
        .iter()
        .filter(|(_, v)| set.contains(v))
        .map(|(_, v)| *v)
        .collect()
}

fn emit_enum(s: &mut String, name: &str, doc: &str, variants: &[&str]) {
    s.push_str(&format!("/// {doc}\n"));
    s.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n");
    s.push_str(&format!("pub enum {name} {{\n"));
    for v in variants {
        s.push_str(&format!("    {v},\n"));
    }
    s.push_str("}\n\n");
}

const HEADER: &str = "\
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// @generated by: cargo run -p aterm-effects --example gen_cat_glyphs
//
// DO NOT EDIT BY HAND. Regenerated from the semantic glyph asset TOMLs under
// crates/aterm-effects/art/glyphs/. The drift test `cat_glyphs_gen_matches_assets`
// fails if this file is stale relative to those assets. See docs/cat-art-v4-design.md §1.

use aterm_scene::vector::PathSeg;

";

const PET_HEADER: &str = "\
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// @generated by: cargo run -p aterm-effects --example gen_pet_glyphs
//
// DO NOT EDIT BY HAND. Regenerated from the full-body pose asset TOMLs under
// crates/aterm-effects/art/pet/. The drift test `pet_glyphs_gen_matches_assets`
// fails if this file is stale relative to those assets.
//
// A SEPARATE roster on purpose: every `GlyphKind::Special` in the cat module's
// `GLYPHS` is a collectible row in the GUI's kitty log, and the spec model pins its
// roster cap to `GLYPH_IDS.len()` — so these animation frames must not join that
// table. Only the shape vocabulary is shared: the types below are the cat module's,
// imported rather than redeclared, so one drawlist walker paints both rosters.

use aterm_scene::vector::PathSeg;

use crate::cat_glyphs_gen::{GlyphDef, GlyphKind, GlyphRole, Layer, Recolor};

";

const DOG_HEADER: &str = "\
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// @generated by: cargo run -p aterm-effects --example gen_dog_glyphs
//
// DO NOT EDIT BY HAND. Regenerated from the breed head asset TOMLs under
// crates/aterm-effects/art/dogs/. The drift test `dog_glyphs_gen_matches_assets`
// fails if this file is stale relative to those assets.
//
// A SEPARATE roster on purpose, exactly like the pet's: every `GlyphKind` row in
// the cat module's `GLYPHS` is a collectible in the GUI's kitty log, whose roster
// cap is pinned to `GLYPH_IDS.len()` — dog heads must not join that table. Only
// the shape vocabulary is shared: the types below are the cat module's, imported
// rather than redeclared, so one drawlist walker paints every roster.

use aterm_scene::vector::PathSeg;

use crate::cat_glyphs_gen::{GlyphDef, GlyphKind, GlyphRole, Layer, Recolor};

";

const ANIMAL_HEADER: &str = "\
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// @generated by: cargo run -p aterm-effects --example gen_animal_glyphs
//
// DO NOT EDIT BY HAND. Regenerated from the species-head asset TOMLs under
// crates/aterm-effects/art/animal/. The drift test `animal_glyphs_gen_matches_assets`
// fails if this file is stale relative to those assets.
//
// A SEPARATE roster on purpose (the pet precedent): the cat module's `GLYPHS` is
// the collectible Kitty-Log table with a spec-pinned cap, so species heads must not
// join it. Only the shape vocabulary is shared: the types below are the cat
// module's, imported rather than redeclared, so one drawlist walker paints all
// three rosters. The asset `id` doubles as the lexicon's `species` key.

use aterm_scene::vector::PathSeg;

use crate::cat_glyphs_gen::{GlyphDef, GlyphKind, GlyphRole, Layer, Recolor};

";

const ROBI_HEADER: &str = "\
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// @generated by: cargo run -p aterm-effects --example gen_robi_glyphs
//
// DO NOT EDIT BY HAND. Regenerated from the robot pose asset TOMLs under
// crates/aterm-effects/art/robi/. The drift test `robi_glyphs_gen_matches_assets`
// fails if this file is stale relative to those assets.
//
// A SEPARATE roster on purpose, exactly like the pet's: every `GlyphKind` row in
// the cat module's `GLYPHS` is a collectible in the GUI's kitty log, whose roster
// cap is pinned to `GLYPH_IDS.len()` — robot poses must not join that table. Only
// the shape vocabulary is shared: the types below are the cat module's, imported
// rather than redeclared, so one drawlist walker paints every roster.

use aterm_scene::vector::PathSeg;

use crate::cat_glyphs_gen::{GlyphDef, GlyphKind, GlyphRole, Layer, Recolor};

";

const STRUCTS: &str = "\
/// One glyph layer: a painter-order fill of `paths` in the glyph's fixed-point frame.
pub struct Layer {
    pub role: GlyphRole,
    pub recolor: Recolor,
    /// The source reference colour, packed `0x00RR_GGBB`. Baked verbatim for
    /// `Recolor::Fixed`; the ramp fallback / shade base otherwise.
    pub fill: u32,
    pub paths: &'static [&'static [PathSeg]],
}

/// One roster glyph: id, kind, fixed-point anchors, and its painter-ordered layers.
pub struct GlyphDef {
    pub id: &'static str,
    pub kind: GlyphKind,
    /// Authored asset viewbox width/height, fixed-point ×1000.
    pub aspect_x1000: u16,
    pub eye_y: u16,
    pub center_x: u16,
    pub word_top: u16,
    pub layers: &'static [Layer],
}

";

/// The `gen_*` examples' asset-dir reader, compiled into the drift tests so
/// they regenerate from the same texts (dev-only; the engine reads no file).
#[cfg(test)]
#[path = "../examples/support/asset_dir.rs"]
mod asset_dir;

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// Every roster asset under `dir`, read the way the generator examples read it.
    fn read_assets(dir: &Path) -> Vec<(String, String)> {
        asset_dir::read_toml_dir(dir).expect("read the roster asset dir")
    }

    fn glyph_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("art/glyphs")
    }

    fn pet_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("art/pet")
    }

    fn animal_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("art/animal")
    }

    fn robi_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("art/robi")
    }

    #[test]
    fn robi_glyphs_gen_matches_assets() {
        let generated = generate_robi_from_assets(&read_assets(&robi_dir()))
            .expect("generate robi glyph drawlists");
        let checked_in = include_str!("robi_glyphs_gen.rs");
        assert_eq!(
            generated, checked_in,
            "robi_glyphs_gen.rs is stale — rerun `targo --unverified run -p aterm-effects --example gen_robi_glyphs`"
        );
    }

    #[test]
    fn animal_glyphs_gen_matches_assets() {
        let generated = generate_animal_from_assets(&read_assets(&animal_dir()))
            .expect("generate animal glyph drawlists");
        let checked_in = include_str!("animal_glyphs_gen.rs");
        assert_eq!(
            generated, checked_in,
            "animal_glyphs_gen.rs is stale — rerun `targo --unverified run -p aterm-effects --example gen_animal_glyphs`"
        );
    }

    /// Cat drift gate (§1): the regenerated source equals `cat_glyphs_gen.rs`.
    #[test]
    fn cat_glyphs_gen_matches_assets() {
        let generated =
            generate_from_assets(&read_assets(&glyph_dir())).expect("generate cat glyph drawlists");
        let checked_in = include_str!("cat_glyphs_gen.rs");
        assert_eq!(
            generated, checked_in,
            "cat_glyphs_gen.rs is stale — rerun `targo --unverified run -p aterm-effects --example gen_cat_glyphs`"
        );
    }

    #[test]
    fn pet_glyphs_gen_matches_assets() {
        let generated = generate_pet_from_assets(&read_assets(&pet_dir()))
            .expect("generate pet glyph drawlists");
        let checked_in = include_str!("pet_glyphs_gen.rs");
        assert_eq!(
            generated, checked_in,
            "pet_glyphs_gen.rs is stale — rerun `targo --unverified run -p aterm-effects --example gen_pet_glyphs`"
        );
    }

    fn dog_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("art/dogs")
    }

    #[test]
    fn dog_glyphs_gen_matches_assets() {
        let generated = generate_dog_from_assets(&read_assets(&dog_dir()))
            .expect("generate dog glyph drawlists");
        let checked_in = include_str!("dog_glyphs_gen.rs");
        assert_eq!(
            generated, checked_in,
            "dog_glyphs_gen.rs is stale — rerun `targo --unverified run -p aterm-effects --example gen_dog_glyphs`"
        );
    }

    #[test]
    fn variant_names_are_camel_case() {
        assert_eq!(variant_name("s1_00"), "S100");
        assert_eq!(variant_name("s2_06b"), "S206b");
        assert_eq!(variant_name("spec_maneki"), "SpecManeki");
        assert_eq!(variant_name("acc_bow"), "AccBow");
    }

    fn roster_doc(extra: &str) -> aterm_toml::Value {
        format!(
            r##"
id = "head_schema_test"
kind = "head"
viewbox = [16, 16]
anchor = {{ eye_y = 0.5, center_x = 0.5, word_top = 1.0 }}
{extra}

[[layer]]
role = "outline"
ref_fill = "#202020"
recolor = "fixed"
paths = ["M 1 1 L 15 1 L 8 15 Z"]
"##
        )
        .parse()
        .expect("schema fixture")
    }

    #[test]
    fn roster_schema_rejects_text_primitives_and_unknown_keys() {
        for (scope, source) in [
            ("glyph", roster_doc("text = \"not artwork\"")),
            (
                "anchor",
                r##"
id = "head_schema_test"
kind = "head"
viewbox = [16, 16]
anchor = { eye_y = 0.5, center_x = 0.5, word_top = 1.0, label = "cat" }
[[layer]]
role = "outline"
ref_fill = "#202020"
recolor = "fixed"
paths = ["M 1 1 L 15 1 L 8 15 Z"]
"##
                .parse()
                .expect("anchor fixture"),
            ),
            (
                "layer 0",
                r##"
id = "head_schema_test"
kind = "head"
viewbox = [16, 16]
anchor = { eye_y = 0.5, center_x = 0.5, word_top = 1.0 }
[[layer]]
role = "outline"
ref_fill = "#202020"
recolor = "fixed"
font = "terminal"
paths = ["M 1 1 L 15 1 L 8 15 Z"]
"##
                .parse()
                .expect("layer fixture"),
            ),
        ] {
            let error = match glyph_from_doc(&source, "schema.toml") {
                Ok(_) => panic!("{scope} text primitive must be rejected"),
                Err(error) => error,
            };
            assert!(error.contains(scope), "unexpected error: {error}");
            assert!(error.contains("path-only"), "unexpected error: {error}");
        }
    }
}
