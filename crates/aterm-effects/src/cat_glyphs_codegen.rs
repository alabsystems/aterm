// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Cat-art v4 codegen (docs/cat-art-v4-design.md §1): turn the semantic glyph asset
//! TOMLs under `crates/aterm-effects/art/glyphs/` into the checked-in const drawlist
//! module [`crate::cat_glyphs_gen`].
//!
//! This is the checked-in-codegen half of the repo's xtask/gate culture: the
//! generator is a pure function [`generate_from_dir`] (asset dir → Rust source
//! string), driven by the `gen_cat_glyphs` example to (re)write the file, and pinned
//! by the [`cat_glyphs_gen_matches_assets`](tests) drift test that regenerates into a
//! string and asserts byte-equality with the checked-in file.
//!
//! ## Two rosters, one parser
//!
//! [`generate_pet_from_dir`] runs the SAME parse/quantize/fail-closed path over the
//! full-body cursor-pet poses in `art/pet/` and emits [`crate::pet_glyphs_gen`]. The
//! pet deliberately does NOT join `GLYPHS`: every `GlyphKind::Special` there is a
//! collectible roster row in the GUI's kitty log, and the spec model pins its
//! `RosterCap` to `GLYPH_IDS.len()`, so 23 animation frames landing in that table
//! would corrupt the collection. Only the *shape vocabulary* is shared — the pet
//! module imports `GlyphDef`/`Layer`/`GlyphRole`/`Recolor`/`GlyphKind` from the cat
//! module instead of redeclaring them, so one drawlist walker paints both rosters.
//!
//! ## What crosses the boundary
//!
//! Only glyphs whose `kind` is a roster kind (`head`, `special`, `accessory`) and that
//! are not `excluded = true` are emitted. The paw-print / decoration **props** (paws
//! are removed, §0/§4) and the not-yet-promoted legacy sheet-2 imports are held
//! out. Each path coordinate `(x, y)` in the glyph's `viewbox` pixel frame is
//! normalized and quantized to the integer `0..=FIXED_ONE` frame
//! (`round(x / vw * FIXED_ONE)`, clamped) so the whole drawlist is integer — `Eq`-safe,
//! byte-deterministic. Floats live only here at bake/codegen time, never in the const
//! data or any hashed key.

use std::path::Path;

use aterm_scene::vector::{FIXED_ONE, PathCmd};

/// The kinds that make it into the roster (in emit order: heads, then specials, then
/// accessories). `prop` / `unknown` assets are held out (§0/§4/§8).
const INCLUDED_KINDS: &[&str] = &["head", "special", "accessory"];

/// Canonical `role` vocabulary → Rust variant, in a FIXED order (painter-role order of
/// docs/cat-art-v4-design.md §0). The emitted `GlyphRole` enum is this list filtered to
/// the roles the roster actually uses; an asset role outside this table is a hard error
/// (register it here — the repo's fail-closed idiom).
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

/// The authored-asset boundary is deliberately path-only. Unknown keys fail
/// closed for roster glyphs so a future `text`, `label`, or `font` field cannot
/// be mistaken for shipping artwork or silently ignored by codegen.
const GLYPH_KEYS: &[&str] = &["id", "kind", "viewbox", "anchor", "layer", "excluded"];
const ANCHOR_KEYS: &[&str] = &["eye_y", "center_x", "word_top", "attach"];
const LAYER_KEYS: &[&str] = &["role", "ref_fill", "recolor", "paths"];

/// One emitted layer: role + recolor variant names, its source `ref_fill` (packed
/// `0x00RR_GGBB`), and its quantized paths.
struct Layer {
    role: &'static str,
    recolor: &'static str,
    /// The colour this layer had in the reference. `Recolor::Fixed` layers bake it
    /// verbatim; `Coat`/`Iris` layers swap it for a ramp colour but keep it as the
    /// fallback and for the shade derivation.
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

/// Generate the full `cat_glyphs_gen.rs` source from the asset TOMLs in `glyph_dir`.
/// Deterministic: assets are ordered (heads, specials, accessories; each by `id`) and
/// every coordinate quantized by a pure integer rule. Returns `Err(msg)` on any
/// malformed asset (fail-closed) so the drift test / generator never emits junk.
///
/// # Errors
/// Any unreadable dir, malformed TOML, unknown role/recolor/kind, missing
/// viewbox/anchor, unknown roster-schema key, unparseable path `d`-string, or
/// colliding variant name.
pub fn generate_from_dir(glyph_dir: &Path) -> Result<String, String> {
    Ok(emit(&load_glyphs(glyph_dir)?))
}

/// Generate the full `pet_glyphs_gen.rs` source from the pose TOMLs in `pet_dir` — the
/// same parse, the same quantization, the same fail-closed key rejection as
/// [`generate_from_dir`], but emitted as the SEPARATE `PetGlyphId` / `PET_GLYPHS`
/// roster that reuses the cat module's types (see the module doc). The poses are all
/// one kind, so the shared (kind, id) order collapses to a plain id order.
///
/// # Errors
/// Identical to [`generate_from_dir`]: any unreadable dir, malformed TOML, unknown
/// role/recolor/kind, missing viewbox/anchor, unknown roster-schema key, unparseable
/// path `d`-string, or colliding variant name.
pub fn generate_pet_from_dir(pet_dir: &Path) -> Result<String, String> {
    Ok(emit_pet(&load_glyphs(pet_dir)?))
}

/// Generate the full `dog_glyphs_gen.rs` source from the breed TOMLs in `dog_dir` — the
/// third roster on the same parse/quantize/fail-closed path. Like the pet, the dogs are
/// a SEPARATE roster reusing the cat module's shape types: the typed-word dog cameo is
/// not a collectible, so dog heads must never join `GLYPHS` (the kitty log's roster cap
/// is pinned to `GLYPH_IDS.len()`).
///
/// # Errors
/// Identical to [`generate_from_dir`]: any unreadable dir, malformed TOML, unknown
/// role/recolor/kind, missing viewbox/anchor, unknown roster-schema key, unparseable
/// path `d`-string, or colliding variant name.
pub fn generate_dog_from_dir(dog_dir: &Path) -> Result<String, String> {
    Ok(emit_dog(&load_glyphs(dog_dir)?))
}

/// Generate the full `animal_glyphs_gen.rs` source from the species-head TOMLs in
/// `animal_dir` — the same parse, quantization, and fail-closed key rejection as
/// [`generate_from_dir`], emitted as the FOURTH separate roster (`AnimalGlyphId` /
/// `ANIMAL_GLYPHS`): one authored head per species for the ambient animal-word
/// decoration. Separate for the same reason the pet roster is — these must never
/// join the collectible cat table — plus one of its own: the asset `id` doubles as
/// the lexicon's `species` key, so the roster also emits the key → id resolver.
///
/// # Errors
/// Identical to [`generate_from_dir`].
pub fn generate_animal_from_dir(animal_dir: &Path) -> Result<String, String> {
    Ok(emit_animal_roster(&load_glyphs(animal_dir)?))
}

/// Generate the full `robi_glyphs_gen.rs` source from the pose TOMLs in `robi_dir` —
/// the helper-robot roster on the same parse/quantize/fail-closed path. Like the pet,
/// Robi is a SEPARATE roster reusing the cat module's shape types: his poses are
/// animation frames for the chrome-walking robot, never collectible rows, so they
/// must not join `GLYPHS` (the kitty log's roster cap is pinned to `GLYPH_IDS.len()`).
///
/// # Errors
/// Identical to [`generate_from_dir`]: any unreadable dir, malformed TOML, unknown
/// role/recolor/kind, missing viewbox/anchor, unknown roster-schema key, unparseable
/// path `d`-string, or colliding variant name.
pub fn generate_robi_from_dir(robi_dir: &Path) -> Result<String, String> {
    Ok(emit_robi(&load_glyphs(robi_dir)?))
}

/// Read, parse, order, and collision-check every roster asset in `dir` — the half of
/// codegen the two rosters share verbatim. Non-`.toml` files (the authoring scripts
/// that live beside the pet poses) are skipped by extension, and non-roster kinds /
/// `excluded = true` assets are dropped by [`glyph_from_doc`].
fn load_glyphs(dir: &Path) -> Result<Vec<Glyph>, String> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("read_dir {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("toml"))
        .collect();
    files.sort();

    let mut glyphs: Vec<Glyph> = Vec::new();
    for path in &files {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let text = std::fs::read_to_string(path).map_err(|e| format!("read {name}: {e}"))?;
        let doc: aterm_toml::Value = text.parse().map_err(|e| format!("parse {name}: {e}"))?;
        if let Some(g) = glyph_from_doc(&doc, name)? {
            glyphs.push(g);
        }
    }

    // Emit order: kind rank (head < special < accessory), then id.
    glyphs.sort_by(|a, b| a.kind_rank.cmp(&b.kind_rank).then_with(|| a.id.cmp(&b.id)));

    // Variant-name collision guard (the ids are unique; assert the derived variants are
    // too, so the roster enum is always well-formed).
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

    // CatGlyphId roster enum.
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

    // Stable enum-value roster. Persistence stores the semantic `GlyphDef::id`
    // string, then resolves it through this table; it never serializes a Rust
    // discriminant whose ordinal could change when contributors add art.
    s.push_str("/// Every glyph id in [`GLYPHS`] order. Pair with `GLYPHS[i].id` for stable string lookup.\n");
    s.push_str("pub const GLYPH_IDS: &[CatGlyphId] = &[\n");
    for g in glyphs {
        s.push_str(&format!("    CatGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n\n");

    // GLYPHS table.
    s.push_str("/// Every roster glyph's const drawlist, indexed by [`CatGlyphId`] order.\n");
    s.push_str("pub const GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n\n");

    // HEADS roster.
    s.push_str("/// The head variants (the roster the genome's `variant` field indexes, §3).\n");
    s.push_str("pub const HEADS: &[CatGlyphId] = &[\n");
    for g in glyphs.iter().filter(|g| g.kind == "Head") {
        s.push_str(&format!("    CatGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n");

    s
}

/// Render the pet roster source. Same drawlist rows as [`emit`], but a separate
/// `PetGlyphId` / `PET_GLYPH_IDS` / `PET_GLYPHS` roster that *imports* the cat module's
/// shape types rather than redeclaring them — so the pet can never be mistaken for a
/// collectible cat row, and there is exactly one `GlyphDef` shape in the crate.
fn emit_pet(glyphs: &[Glyph]) -> String {
    let mut s = String::new();
    s.push_str(PET_HEADER);

    // PetGlyphId roster enum.
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

    // Stable enum-value roster (same reason as the cat's GLYPH_IDS: pose selection
    // resolves through `PET_GLYPHS[i].id` strings, never a Rust discriminant).
    s.push_str(
        "/// Every pose id in [`PET_GLYPHS`] order. Pair with `PET_GLYPHS[i].id` for stable\n",
    );
    s.push_str("/// string lookup.\n");
    s.push_str("pub const PET_GLYPH_IDS: &[PetGlyphId] = &[\n");
    for g in glyphs {
        s.push_str(&format!("    PetGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n\n");

    // PET_GLYPHS table.
    s.push_str("/// Every pet pose's const drawlist, indexed by [`PetGlyphId`] order.\n");
    s.push_str("pub const PET_GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n");

    s
}

/// Render the dog roster source. Same drawlist rows as [`emit`], but a separate
/// `DogGlyphId` / `DOG_GLYPH_IDS` / `DOG_GLYPHS` roster that *imports* the cat module's
/// shape types rather than redeclaring them — the pet precedent applied to the typed
/// dog cameo's breed selection.
fn emit_dog(glyphs: &[Glyph]) -> String {
    let mut s = String::new();
    s.push_str(DOG_HEADER);

    // DogGlyphId roster enum.
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

    // Stable enum-value roster (same reason as the cat's GLYPH_IDS: any persisted
    // reference resolves through `DOG_GLYPHS[i].id` strings, never a discriminant).
    s.push_str(
        "/// Every breed id in [`DOG_GLYPHS`] order. Pair with `DOG_GLYPHS[i].id` for stable\n",
    );
    s.push_str("/// string lookup.\n");
    s.push_str("pub const DOG_GLYPH_IDS: &[DogGlyphId] = &[\n");
    for g in glyphs {
        s.push_str(&format!("    DogGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n\n");

    // DOG_GLYPHS table.
    s.push_str("/// Every breed head's const drawlist, indexed by [`DogGlyphId`] order.\n");
    s.push_str("pub const DOG_GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n\n");

    // DOG_HEADS roster (parallel to the cat's HEADS: what the summon picker draws from).
    s.push_str("/// The breed variants the typed-summon picker draws from, uniformly.\n");
    s.push_str("pub const DOG_HEADS: &[DogGlyphId] = &[\n");
    for g in glyphs.iter().filter(|g| g.kind == "Head") {
        s.push_str(&format!("    DogGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n");

    s
}

/// Render the ROBI roster source. Same drawlist rows as [`emit`], but a separate
/// `RobiGlyphId` / `ROBI_GLYPH_IDS` / `ROBI_GLYPHS` roster that *imports* the cat
/// module's shape types rather than redeclaring them — the pet precedent applied to
/// the chrome-walking helper robot's animation frames.
fn emit_robi(glyphs: &[Glyph]) -> String {
    let mut s = String::new();
    s.push_str(ROBI_HEADER);

    // RobiGlyphId roster enum.
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

    // Stable enum-value roster (same reason as the cat's GLYPH_IDS: pose selection
    // resolves through `ROBI_GLYPHS[i].id` strings, never a Rust discriminant).
    s.push_str("/// Every pose id in [`ROBI_GLYPHS`] order. Pair with `ROBI_GLYPHS[i].id` for\n");
    s.push_str("/// stable string lookup.\n");
    s.push_str("pub const ROBI_GLYPH_IDS: &[RobiGlyphId] = &[\n");
    for g in glyphs {
        s.push_str(&format!("    RobiGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n\n");

    // ROBI_GLYPHS table.
    s.push_str("/// Every Robi pose's const drawlist, indexed by [`RobiGlyphId`] order.\n");
    s.push_str("pub const ROBI_GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n");

    s
}

fn emit_animal_roster(glyphs: &[Glyph]) -> String {
    let mut s = String::new();
    s.push_str(ANIMAL_HEADER);

    // AnimalGlyphId roster enum.
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

    // Stable enum-value roster (same reason as the cat's GLYPH_IDS: species
    // resolution goes through `ANIMAL_GLYPHS[i].id` strings, never a Rust
    // discriminant).
    s.push_str(
        "/// Every species id in [`ANIMAL_GLYPHS`] order. Pair with `ANIMAL_GLYPHS[i].id`\n",
    );
    s.push_str("/// for stable string lookup.\n");
    s.push_str("pub const ANIMAL_GLYPH_IDS: &[AnimalGlyphId] = &[\n");
    for g in glyphs {
        s.push_str(&format!("    AnimalGlyphId::{},\n", g.variant));
    }
    s.push_str("];\n\n");

    // ANIMAL_GLYPHS table.
    s.push_str("/// Every species head's const drawlist, indexed by [`AnimalGlyphId`] order.\n");
    s.push_str("pub const ANIMAL_GLYPHS: &[GlyphDef] = &[\n");
    emit_glyph_rows(&mut s, glyphs);
    s.push_str("];\n\n");

    // The lexicon-key resolver: `species = \"monkey\"` → `AnimalGlyphId::Monkey`.
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

/// The `GlyphDef { … }` rows both roster tables are built from — identical bytes for
/// identical assets, so the pet and the cat can never drift in drawlist encoding.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The asset dir shipped with the crate.
    fn glyph_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("art/glyphs")
    }

    /// The pet-pose asset dir shipped with the crate.
    fn pet_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("art/pet")
    }

    /// The species-head asset dir shipped with the crate.
    fn animal_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("art/animal")
    }

    /// The robot-pose asset dir shipped with the crate.
    fn robi_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("art/robi")
    }

    /// The Robi roster's drift gate: regenerate the source from the assets and
    /// assert it equals the checked-in `robi_glyphs_gen.rs`. Regenerate with
    /// `cargo run -p aterm-effects --example gen_robi_glyphs` after touching any pose.
    #[test]
    fn robi_glyphs_gen_matches_assets() {
        let generated = generate_robi_from_dir(&robi_dir()).expect("generate robi glyph drawlists");
        let checked_in = include_str!("robi_glyphs_gen.rs");
        assert_eq!(
            generated, checked_in,
            "robi_glyphs_gen.rs is stale — rerun `cargo run -p aterm-effects --example gen_robi_glyphs`"
        );
    }

    /// The animal roster's drift gate: regenerate the source from the assets and
    /// assert it equals the checked-in `animal_glyphs_gen.rs`. Regenerate with
    /// `cargo run -p aterm-effects --example gen_animal_glyphs` after touching any head.
    #[test]
    fn animal_glyphs_gen_matches_assets() {
        let generated =
            generate_animal_from_dir(&animal_dir()).expect("generate animal glyph drawlists");
        let checked_in = include_str!("animal_glyphs_gen.rs");
        assert_eq!(
            generated, checked_in,
            "animal_glyphs_gen.rs is stale — rerun `cargo run -p aterm-effects --example gen_animal_glyphs`"
        );
    }

    /// The drift gate (§1): regenerate the source from the assets and assert it equals
    /// the checked-in `cat_glyphs_gen.rs`. Regenerate with
    /// `cargo run -p aterm-effects --example gen_cat_glyphs` after touching any asset.
    #[test]
    fn cat_glyphs_gen_matches_assets() {
        let generated = generate_from_dir(&glyph_dir()).expect("generate cat glyph drawlists");
        let checked_in = include_str!("cat_glyphs_gen.rs");
        assert_eq!(
            generated, checked_in,
            "cat_glyphs_gen.rs is stale — rerun `cargo run -p aterm-effects --example gen_cat_glyphs`"
        );
    }

    /// The pet half of the same gate: regenerate the pet roster from `art/pet/` and
    /// assert it equals the checked-in `pet_glyphs_gen.rs`. Regenerate with
    /// `cargo run -p aterm-effects --example gen_pet_glyphs` after touching any pose.
    #[test]
    fn pet_glyphs_gen_matches_assets() {
        let generated = generate_pet_from_dir(&pet_dir()).expect("generate pet glyph drawlists");
        let checked_in = include_str!("pet_glyphs_gen.rs");
        assert_eq!(
            generated, checked_in,
            "pet_glyphs_gen.rs is stale — rerun `cargo run -p aterm-effects --example gen_pet_glyphs`"
        );
    }

    /// The breed asset dir shipped with the crate.
    fn dog_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("art/dogs")
    }

    /// The dog third of the same gate: regenerate the breed roster from `art/dogs/`
    /// and assert it equals the checked-in `dog_glyphs_gen.rs`. Regenerate with
    /// `cargo run -p aterm-effects --example gen_dog_glyphs` after touching any breed.
    #[test]
    fn dog_glyphs_gen_matches_assets() {
        let generated = generate_dog_from_dir(&dog_dir()).expect("generate dog glyph drawlists");
        let checked_in = include_str!("dog_glyphs_gen.rs");
        assert_eq!(
            generated, checked_in,
            "dog_glyphs_gen.rs is stale — rerun `cargo run -p aterm-effects --example gen_dog_glyphs`"
        );
    }

    /// The generator is a pure function of the assets: two runs are byte-identical.
    #[test]
    fn cat_glyphs_codegen_is_deterministic() {
        let a = generate_from_dir(&glyph_dir()).expect("gen a");
        let b = generate_from_dir(&glyph_dir()).expect("gen b");
        assert_eq!(a, b, "codegen must be deterministic");
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
