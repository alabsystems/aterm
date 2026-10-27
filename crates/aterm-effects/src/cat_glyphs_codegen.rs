// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Glyph codegen.

use core::fmt::Write;

use aterm_scene::vector::{FIXED_ONE, PathCmd};

const INCLUDED_KINDS: &[&str] = &["head", "special", "accessory"];

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

const RECOLOR_VOCAB: &[(&str, &str)] = &[
    ("coat", "Coat"),
    ("coat_shade", "CoatShade"),
    ("iris", "Iris"),
    ("fixed", "Fixed"),
];

const KIND_VOCAB: &[(&str, &str)] = &[
    ("head", "Head"),
    ("special", "Special"),
    ("accessory", "Accessory"),
    ("prop", "Prop"),
];

const GLYPH_KEYS: &[&str] = &["id", "kind", "viewbox", "anchor", "layer", "excluded"];
const ANCHOR_KEYS: &[&str] = &["eye_y", "center_x", "word_top", "attach"];
const LAYER_KEYS: &[&str] = &["role", "ref_fill", "recolor", "paths"];

struct Layer {
    role: &'static str,
    recolor: &'static str,
    /// The layer's reference colour.
    fill: u32,
    /// Each path is a sequence of `PathSeg` literal strings (e.g. `PathSeg::Move(1,2)`).
    paths: Vec<Vec<String>>,
}

struct Glyph {
    id: String,
    /// Emit-order rank of the kind: 0 head, 1 special, 2 accessory.
    kind_rank: usize,
    kind: &'static str,
    variant: String,
    /// Viewbox width/height, quantized to a fixed thousandth.
    aspect_x1000: u16,
    eye_y: u16,
    center_x: u16,
    word_top: u16,
    layers: Vec<Layer>,
}

/// Generate `cat_glyphs_gen.rs` from `(file name, TOML text)` pairs read from `art/glyphs/`.
/// # Errors
/// Any malformed TOML, unknown role/recolor/kind, missing viewbox/anchor, unknown roster-schema key, unparseable path `d`-string, or colliding variant name.
pub fn generate_from_assets(assets: &[(String, String)]) -> Result<String, String> {
    Ok(emit(&load_glyphs(assets)?))
}

/// Generate `pet_glyphs_gen.rs` from the pose texts `assets`.
/// # Errors
/// As [`generate_from_assets`].
pub fn generate_pet_from_assets(assets: &[(String, String)]) -> Result<String, String> {
    Ok(emit_pet(&load_glyphs(assets)?))
}

/// Generate `dog_glyphs_gen.rs` from the breed texts `assets`.
/// # Errors
/// As [`generate_from_assets`].
pub fn generate_dog_from_assets(assets: &[(String, String)]) -> Result<String, String> {
    Ok(emit_dog(&load_glyphs(assets)?))
}

/// Generate `animal_glyphs_gen.rs` from the species-head texts `assets`.
/// # Errors
/// As [`generate_from_assets`].
pub fn generate_animal_from_assets(assets: &[(String, String)]) -> Result<String, String> {
    Ok(emit_animal_roster(&load_glyphs(assets)?))
}

#[inline]
const fn str_eq(a: &str, b: &str) -> bool {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    if ab.len() != bb.len() {
        return false;
    }
    let mut i = 0;
    while i < ab.len() {
        if ab[i] != bb[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[inline]
const fn included_kind_rank(kind_str: &str) -> Option<usize> {
    let mut i = 0;
    while i < INCLUDED_KINDS.len() {
        if str_eq(INCLUDED_KINDS[i], kind_str) {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn load_glyphs(assets: &[(String, String)]) -> Result<Vec<Glyph>, String> {
    let mut files: Vec<&(String, String)> = Vec::with_capacity(assets.len());
    let mut ai = 0;
    while ai < assets.len() {
        files.push(&assets[ai]);
        ai += 1;
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut glyphs: Vec<Glyph> = Vec::new();
    let mut fi = 0;
    while fi < files.len() {
        let (name, text) = files[fi];
        fi += 1;
        let doc: aterm_toml::Value = text.parse().map_err(|e| format!("parse {name}: {e}"))?;
        if let Some(g) = glyph_from_doc(&doc, name)? {
            glyphs.push(g);
        }
    }

    glyphs.sort_by(|a, b| a.kind_rank.cmp(&b.kind_rank).then_with(|| a.id.cmp(&b.id)));

    let mut seen = std::collections::BTreeSet::new();
    let mut gi = 0;
    while gi < glyphs.len() {
        let g = &glyphs[gi];
        gi += 1;
        if !seen.insert(g.variant.as_str()) {
            return Err(format!(
                "variant-name collision: `{}` derived from id `{}` twice",
                g.variant, g.id
            ));
        }
    }

    Ok(glyphs)
}

#[inline]
fn anchor_frac(anchor: &aterm_toml::Value, name: &str, key: &str) -> Result<u16, String> {
    let Some(v) = anchor.get(key) else {
        return Err(format!("{name}: anchor missing `{key}`"));
    };
    Ok(quant_frac(num(v)))
}

fn glyph_from_doc(doc: &aterm_toml::Value, name: &str) -> Result<Option<Glyph>, String> {
    if let Some(v) = doc.get("excluded")
        && v.as_bool() == Some(true)
    {
        return Ok(None);
    }
    let Some(id_val) = doc.get("id") else {
        return Err(format!("{name}: missing `id`"));
    };
    let Some(id_str) = id_val.as_str() else {
        return Err(format!("{name}: missing `id`"));
    };
    let id = id_str.to_string();
    let Some(kind_val) = doc.get("kind") else {
        return Err(format!("{name}: missing `kind`"));
    };
    let Some(kind_str) = kind_val.as_str() else {
        return Err(format!("{name}: missing `kind`"));
    };
    let Some(kind_rank) = included_kind_rank(kind_str) else {
        return Ok(None);
    };
    reject_unknown_keys(doc, GLYPH_KEYS, name, "glyph")?;
    let Some(kind) = lookup(KIND_VOCAB, kind_str) else {
        return Err(format!("{name}: kind `{kind_str}` not in KIND_VOCAB"));
    };

    let Some(vb_val) = doc.get("viewbox") else {
        return Err(format!("{name}: missing `viewbox = [w, h]`"));
    };
    let Some(vb) = vb_val.as_array() else {
        return Err(format!("{name}: missing `viewbox = [w, h]`"));
    };
    if vb.len() != 2 {
        return Err(format!("{name}: missing `viewbox = [w, h]`"));
    }
    let (vw, vh) = (num(&vb[0]), num(&vb[1]));
    if !vw.is_finite() || !vh.is_finite() || vw <= 0.0 || vh <= 0.0 {
        return Err(format!("{name}: degenerate viewbox [{vw}, {vh}]"));
    }

    let Some(anchor) = doc.get("anchor") else {
        return Err(format!("{name}: missing `anchor`"));
    };
    reject_unknown_keys(anchor, ANCHOR_KEYS, name, "anchor")?;
    let eye_y = anchor_frac(anchor, name, "eye_y")?;
    let center_x = anchor_frac(anchor, name, "center_x")?;
    let word_top = anchor_frac(anchor, name, "word_top")?;

    let mut layers = Vec::new();
    let raw_layers: &[aterm_toml::Value] = match doc.get("layer") {
        Some(v) => match v.as_array() {
            Some(arr) => arr.as_slice(),
            None => &[],
        },
        None => &[],
    };
    let mut li = 0;
    while li < raw_layers.len() {
        let layer = &raw_layers[li];
        reject_unknown_keys(layer, LAYER_KEYS, name, format_args!("layer {li}"))?;
        let Some(role_val) = layer.get("role") else {
            return Err(format!("{name}: layer {li} missing `role`"));
        };
        let Some(role_str) = role_val.as_str() else {
            return Err(format!("{name}: layer {li} missing `role`"));
        };
        let Some(role) = lookup(ROLE_VOCAB, role_str) else {
            return Err(format!(
                "{name}: layer {li} role `{role_str}` not in ROLE_VOCAB"
            ));
        };
        let Some(recolor_val) = layer.get("recolor") else {
            return Err(format!("{name}: layer {li} missing `recolor`"));
        };
        let Some(recolor_str) = recolor_val.as_str() else {
            return Err(format!("{name}: layer {li} missing `recolor`"));
        };
        let Some(recolor) = lookup(RECOLOR_VOCAB, recolor_str) else {
            return Err(format!(
                "{name}: layer {li} recolor `{recolor_str}` not in RECOLOR_VOCAB"
            ));
        };
        let fill = match layer.get("ref_fill") {
            Some(v) => match v.as_str() {
                Some(s) => match parse_hex_rgb(s) {
                    Some(rgb) => rgb,
                    None => return Err(format!("{name}: layer {li} missing/invalid `ref_fill`")),
                },
                None => return Err(format!("{name}: layer {li} missing/invalid `ref_fill`")),
            },
            None => return Err(format!("{name}: layer {li} missing/invalid `ref_fill`")),
        };

        let mut paths = Vec::new();
        let raw_paths: &[aterm_toml::Value] = match layer.get("paths") {
            Some(v) => match v.as_array() {
                Some(arr) => arr.as_slice(),
                None => &[],
            },
            None => &[],
        };
        let mut pi = 0;
        while pi < raw_paths.len() {
            let p = &raw_paths[pi];
            let Some(d) = p.as_str() else {
                return Err(format!("{name}: layer {li} path {pi} is not a string"));
            };
            let Some(cmds) = aterm_scene::vector::parse_path(d) else {
                return Err(format!(
                    "{name}: layer {li} path {pi} unparseable `d`-string"
                ));
            };
            let segs = quantize_path(&cmds, vw, vh);
            if !segs.is_empty() {
                paths.push(segs);
            }
            pi += 1;
        }
        if !paths.is_empty() {
            layers.push(Layer {
                role,
                recolor,
                fill,
                paths,
            });
        }
        li += 1;
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
    scope: impl core::fmt::Display,
) -> Result<(), String> {
    let Some(table) = value.as_table() else {
        return Err(format!("{name}: {scope} must be a table"));
    };
    for key in table.keys() {
        let ks = key.as_str();
        let mut ok = false;
        let mut ai = 0;
        while ai < allowed.len() {
            if str_eq(allowed[ai], ks) {
                ok = true;
                break;
            }
            ai += 1;
        }
        if !ok {
            return Err(format!(
                "{name}: unsupported {scope} key `{key}`; cat glyphs are path-only"
            ));
        }
    }
    Ok(())
}

fn quantize_path(cmds: &[PathCmd], vw: f32, vh: f32) -> Vec<String> {
    let mut out = Vec::with_capacity(cmds.len());
    let mut i = 0;
    while i < cmds.len() {
        let seg = match cmds[i] {
            PathCmd::Move(x, y) => format!("PathSeg::Move({},{})", quant(x, vw), quant(y, vh)),
            PathCmd::Line(x, y) => format!("PathSeg::Line({},{})", quant(x, vw), quant(y, vh)),
            PathCmd::Cubic(x1, y1, x2, y2, x, y) => format!(
                "PathSeg::Cubic({},{},{},{},{},{})",
                quant(x1, vw),
                quant(y1, vh),
                quant(x2, vw),
                quant(y2, vh),
                quant(x, vw),
                quant(y, vh)
            ),
            PathCmd::Close => "PathSeg::Close".to_string(),
        };
        out.push(seg);
        i += 1;
    }
    out
}

#[inline]
const fn quant(v: f32, extent: f32) -> u16 {
    let q = (v / extent * FIXED_ONE as f32).round();
    q.clamp(0.0, FIXED_ONE as f32) as u16
}

#[inline]
const fn quant_frac(f: f32) -> u16 {
    (f * FIXED_ONE as f32).round().clamp(0.0, FIXED_ONE as f32) as u16
}

#[inline]
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

#[inline]
const fn lookup(vocab: &[(&str, &'static str)], key: &str) -> Option<&'static str> {
    let mut i = 0;
    while i < vocab.len() {
        if str_eq(vocab[i].0, key) {
            return Some(vocab[i].1);
        }
        i += 1;
    }
    None
}

#[inline]
const fn parse_hex_rgb(s: &str) -> Option<u32> {
    let bytes = s.as_bytes();
    if bytes.len() != 7 || bytes[0] != b'#' {
        return None;
    }
    let mut out = 0u32;
    let mut i = 1;
    while i < 7 {
        let d = match bytes[i] {
            b'0'..=b'9' => bytes[i] - b'0',
            b'a'..=b'f' => bytes[i] - b'a' + 10,
            b'A'..=b'F' => bytes[i] - b'A' + 10,
            _ => return None,
        };
        out = (out << 4) | (d as u32);
        i += 1;
    }
    Some(out)
}

#[inline]
fn num(v: &aterm_toml::Value) -> f32 {
    if let Some(f) = v.as_float() {
        f as f32
    } else if let Some(i) = v.as_integer() {
        i as f32
    } else {
        0.0
    }
}

fn present_roles(glyphs: &[Glyph]) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut vi = 0;
    while vi < ROLE_VOCAB.len() {
        let v = ROLE_VOCAB[vi].1;
        vi += 1;
        let mut found = false;
        let mut gi = 0;
        while gi < glyphs.len() && !found {
            let layers = glyphs[gi].layers.as_slice();
            gi += 1;
            let mut li = 0;
            while li < layers.len() {
                if str_eq(layers[li].role, v) {
                    found = true;
                    break;
                }
                li += 1;
            }
        }
        if found {
            out.push(v);
        }
    }
    out
}

fn present_recolors(glyphs: &[Glyph]) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut vi = 0;
    while vi < RECOLOR_VOCAB.len() {
        let v = RECOLOR_VOCAB[vi].1;
        vi += 1;
        let mut found = false;
        let mut gi = 0;
        while gi < glyphs.len() && !found {
            let layers = glyphs[gi].layers.as_slice();
            gi += 1;
            let mut li = 0;
            while li < layers.len() {
                if str_eq(layers[li].recolor, v) {
                    found = true;
                    break;
                }
                li += 1;
            }
        }
        if found {
            out.push(v);
        }
    }
    out
}

fn present_kinds(glyphs: &[Glyph]) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut vi = 0;
    while vi < KIND_VOCAB.len() {
        let v = KIND_VOCAB[vi].1;
        vi += 1;
        let mut gi = 0;
        while gi < glyphs.len() {
            if str_eq(glyphs[gi].kind, v) {
                out.push(v);
                break;
            }
            gi += 1;
        }
    }
    out
}

fn emit(glyphs: &[Glyph]) -> String {
    let mut s = String::new();
    s.push_str(HEADER);

    let used_roles = present_roles(glyphs);
    let used_recolors = present_recolors(glyphs);
    let used_kinds = present_kinds(glyphs);

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
    let mut gi = 0;
    while gi < glyphs.len() {
        s.push_str("    ");
        s.push_str(&glyphs[gi].variant);
        s.push_str(",\n");
        gi += 1;
    }
    s.push_str("}\n\n");

    s.push_str("/// Every glyph id in [`GLYPHS`] order. Pair with `GLYPHS[i].id` for stable string lookup.\n");
    s.push_str("pub const GLYPH_IDS: &[CatGlyphId] = &[\n");
    let mut gi = 0;
    while gi < glyphs.len() {
        let _ = writeln!(s, "    CatGlyphId::{},", glyphs[gi].variant);
        gi += 1;
    }
    s.push_str("];\n\n");

    s.push_str("/// Every roster glyph's const drawlist, indexed by [`CatGlyphId`] order.\n");
    s.push_str("pub const GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n\n");

    s.push_str("/// The head variants (the roster the genome's `variant` field indexes, §3).\n");
    s.push_str("pub const HEADS: &[CatGlyphId] = &[\n");
    let mut gi = 0;
    while gi < glyphs.len() {
        if str_eq(glyphs[gi].kind, "Head") {
            let _ = writeln!(s, "    CatGlyphId::{},", glyphs[gi].variant);
        }
        gi += 1;
    }
    s.push_str("];\n");

    s
}

fn emit_pet(glyphs: &[Glyph]) -> String {
    let mut s = String::new();
    s.push_str(PET_HEADER);

    s.push_str("/// The pet roster: every authored full-body pose, ordered by id. Indexes\n");
    s.push_str("/// [`PET_GLYPHS`]. Not a collectible — the pet is one cat wearing the caret's\n");
    s.push_str("/// genome, so these are animation frames, not roster rows.\n");
    s.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n");
    s.push_str("pub enum PetGlyphId {\n");
    let mut gi = 0;
    while gi < glyphs.len() {
        s.push_str("    ");
        s.push_str(&glyphs[gi].variant);
        s.push_str(",\n");
        gi += 1;
    }
    s.push_str("}\n\n");

    s.push_str(
        "/// Every pose id in [`PET_GLYPHS`] order. Pair with `PET_GLYPHS[i].id` for stable\n",
    );
    s.push_str("/// string lookup.\n");
    s.push_str("pub const PET_GLYPH_IDS: &[PetGlyphId] = &[\n");
    let mut gi = 0;
    while gi < glyphs.len() {
        let _ = writeln!(s, "    PetGlyphId::{},", glyphs[gi].variant);
        gi += 1;
    }
    s.push_str("];\n\n");

    s.push_str("/// Every pet pose's const drawlist, indexed by [`PetGlyphId`] order.\n");
    s.push_str("pub const PET_GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n");

    s
}

fn emit_dog(glyphs: &[Glyph]) -> String {
    let mut s = String::new();
    s.push_str(DOG_HEADER);

    s.push_str("/// The dog roster: every authored breed head, ordered by id. Indexes\n");
    s.push_str("/// [`DOG_GLYPHS`]. Not a collectible — the typed-word dog cameo draws one\n");
    s.push_str("/// of these per summon; the kitty log never records them.\n");
    s.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n");
    s.push_str("pub enum DogGlyphId {\n");
    let mut gi = 0;
    while gi < glyphs.len() {
        s.push_str("    ");
        s.push_str(&glyphs[gi].variant);
        s.push_str(",\n");
        gi += 1;
    }
    s.push_str("}\n\n");

    s.push_str(
        "/// Every breed id in [`DOG_GLYPHS`] order. Pair with `DOG_GLYPHS[i].id` for stable\n",
    );
    s.push_str("/// string lookup.\n");
    s.push_str("pub const DOG_GLYPH_IDS: &[DogGlyphId] = &[\n");
    let mut gi = 0;
    while gi < glyphs.len() {
        let _ = writeln!(s, "    DogGlyphId::{},", glyphs[gi].variant);
        gi += 1;
    }
    s.push_str("];\n\n");

    s.push_str("/// Every breed head's const drawlist, indexed by [`DogGlyphId`] order.\n");
    s.push_str("pub const DOG_GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n\n");

    s.push_str("/// The breed variants the typed-summon picker draws from, uniformly.\n");
    s.push_str("pub const DOG_HEADS: &[DogGlyphId] = &[\n");
    let mut gi = 0;
    while gi < glyphs.len() {
        if str_eq(glyphs[gi].kind, "Head") {
            let _ = writeln!(s, "    DogGlyphId::{},", glyphs[gi].variant);
        }
        gi += 1;
    }
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
    let mut gi = 0;
    while gi < glyphs.len() {
        s.push_str("    ");
        s.push_str(&glyphs[gi].variant);
        s.push_str(",\n");
        gi += 1;
    }
    s.push_str("}\n\n");

    s.push_str(
        "/// Every species id in [`ANIMAL_GLYPHS`] order. Pair with `ANIMAL_GLYPHS[i].id`\n",
    );
    s.push_str("/// for stable string lookup.\n");
    s.push_str("pub const ANIMAL_GLYPH_IDS: &[AnimalGlyphId] = &[\n");
    let mut gi = 0;
    while gi < glyphs.len() {
        let _ = writeln!(s, "    AnimalGlyphId::{},", glyphs[gi].variant);
        gi += 1;
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

fn emit_glyph_rows(s: &mut String, glyphs: &[Glyph]) {
    let mut gi = 0;
    while gi < glyphs.len() {
        let g = &glyphs[gi];
        gi += 1;
        let _ = writeln!(s, "    // {} ({})", g.variant, g.id);
        let _ = writeln!(
            s,
            "    GlyphDef {{ id: {:?}, kind: GlyphKind::{}, aspect_x1000: {}, eye_y: {}, center_x: {}, word_top: {}, layers: &[",
            g.id, g.kind, g.aspect_x1000, g.eye_y, g.center_x, g.word_top
        );
        let mut li = 0;
        while li < g.layers.len() {
            let l = &g.layers[li];
            li += 1;
            let _ = writeln!(
                s,
                "        Layer {{ role: GlyphRole::{}, recolor: Recolor::{}, fill: {:#08X}, paths: &[",
                l.role, l.recolor, l.fill
            );
            let mut pi = 0;
            while pi < l.paths.len() {
                s.push_str("            &[");
                let segs = l.paths[pi].as_slice();
                let mut si = 0;
                while si < segs.len() {
                    if si > 0 {
                        s.push(',');
                    }
                    s.push_str(&segs[si]);
                    si += 1;
                }
                s.push_str("],\n");
                pi += 1;
            }
            s.push_str("        ] },\n");
        }
        s.push_str("    ] },\n");
    }
}

fn emit_enum(s: &mut String, name: &str, doc: &str, variants: &[&str]) {
    let _ = writeln!(s, "/// {doc}");
    s.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]\n");
    let _ = writeln!(s, "pub enum {name} {{");
    let mut vi = 0;
    while vi < variants.len() {
        let _ = writeln!(s, "    {},", variants[vi]);
        vi += 1;
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

/// The `gen_*` examples' asset-dir reader.
#[cfg(test)]
#[path = "../examples/support/asset_dir.rs"]
mod asset_dir;

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

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
