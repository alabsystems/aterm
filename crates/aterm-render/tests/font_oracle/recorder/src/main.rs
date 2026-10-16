// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The font oracle's recorder (TF.1 of docs/AUDIT-third-party-2026-10-15.md),
//! built outside the aterm workspace (see Cargo.toml).
//!
//!   replay <aterm-render dir>        re-answer every `shape` and `read` line of
//!                                    the committed corpora with the registry
//!                                    crates (no aterm facades) and diff
//!   hint <aterm-render dir> <out>    record skrifa's grid-fitted masks
//!   gvar-alloc <aterm-render dir> <out>
//!                                    record ttf-parser's `read` answers for
//!                                    `GVAR_ALLOC_FACES`
//!   differential <aterm-render dir> [runs]
//!                                    shape seeded random runs with rustybuzz
//!                                    and aterm-font and compare (TF.6)
//!   hint-differential <aterm-render dir>
//!                                    hint every face this host has with
//!                                    skrifa and aterm-font and compare (TF.4)

// Paths are relative to this file: tests/font_oracle/probe.rs, reader.rs,
// shaper.rs and aterm-render's src/raster.rs.
mod differential;
mod hint;
mod hint_diff;
/// The hinting oracle's shared half, over this crate's digest (`Fnv`) and
/// aterm-render's rasterizer (`raster`).
#[path = "../../hint_raster.rs"]
mod hint_raster;
#[path = "../../probe.rs"]
#[allow(dead_code)]
mod probe;
/// The reader `reader.rs` answers with here: the registry ttf-parser, the
/// one the TF.1 corpus was recorded from.
mod font {
    pub(crate) use ttf_parser::*;
}
use probe::corpus;
#[path = "../../../../src/raster.rs"]
#[allow(dead_code)]
mod raster;
#[path = "../../reader.rs"]
#[allow(dead_code)]
mod reader;
/// aterm-font's shaper, writing the same answer strings as `probe.rs`.
#[path = "../../shaper.rs"]
mod shaper;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use probe::{FACES, Fnv, GVAR_ALLOC_FACES, Line, Origin};

struct Loaded {
    face: &'static probe::FaceFile,
    bytes: Vec<u8>,
    key: String,
}

fn load(dir: &str) -> Vec<Loaded> {
    let mut out = Vec::new();
    for face in FACES.iter().chain(GVAR_ALLOC_FACES) {
        let path = match face.origin {
            Origin::Repo(rel) => format!("{dir}/{rel}"),
            Origin::System(abs) => abs.to_string(),
        };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let key = probe::file_key(&bytes);
        out.push(Loaded { face, bytes, key });
    }
    out
}

fn replay(dir: &str) {
    let faces = load(dir);
    let by_key: BTreeMap<&str, &[u8]> = faces
        .iter()
        .map(|l| (l.key.as_str(), l.bytes.as_slice()))
        .collect();
    let (mut same, mut moved, mut absent, mut not_engine) = (0usize, 0usize, 0usize, 0usize);
    for name in ["shaping", "reader", "reader-gvar-alloc"] {
        let path = format!("{dir}/tests/fixtures/font-oracle/{name}.txt");
        let text = std::fs::read_to_string(&path).expect("corpus");
        let mut read_cache: BTreeMap<(String, u32, String), BTreeMap<String, String>> =
            BTreeMap::new();
        for t in text
            .lines()
            .filter(|t| !t.is_empty() && !t.starts_with('#'))
        {
            let line = Line::parse(t).expect("line");
            let Some(bytes) = by_key.get(line.key.as_str()) else {
                absent += 1;
                continue;
            };
            let coords = probe::parse_coords(&line.inst).expect("coords");
            let now = match line.kind.as_str() {
                "shape" => {
                    let features = probe::parse_features(&line.feats).expect("features");
                    let text = probe::unescape(&line.input).expect("input");
                    probe::shape(bytes, line.index, &coords, &features, &text)
                }
                "read" => {
                    let answers = read_cache
                        .entry((line.key.clone(), line.index, line.inst.clone()))
                        .or_insert_with(|| {
                            reader::read(bytes, line.index, &coords)
                                .into_iter()
                                .collect()
                        });
                    answers
                        .get(&line.case)
                        .cloned()
                        .unwrap_or_else(|| "<no answer>".to_string())
                }
                _ => {
                    not_engine += 1;
                    continue;
                }
            };
            if now == line.answer {
                same += 1;
            } else {
                moved += 1;
                println!(
                    "MOVED {}\n  corpus   {}\n  registry {}",
                    line.id(),
                    line.answer,
                    now
                );
            }
        }
    }
    println!(
        "replay: {same} identical, {moved} moved, {absent} skipped (face absent), {not_engine} aterm-code lines not replayed"
    );
}

fn sample_gids(bytes: &[u8], index: u32) -> Vec<u16> {
    let face = ttf_parser::Face::parse(bytes, index).expect("face");
    let n = face.number_of_glyphs();
    let mut gids = reader::latin_gids(&face);
    let stride = (n / 128).max(1);
    gids.extend((0..n).step_by(usize::from(stride)));
    gids.sort_unstable();
    gids.dedup();
    gids
}

fn hint_lines(dir: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for l in load(dir) {
        if l.face.roles & probe::HINT == 0 {
            continue;
        }
        let index = 0;
        let gids = sample_gids(&l.bytes, index);
        let variable = ttf_parser::Face::parse(&l.bytes, index)
            .expect("face")
            .is_variable();
        let mut instances: Vec<probe::Coords> = vec![Vec::new()];
        if variable {
            // aterm's resolved instance of a variable primary (Cinzel's
            // `Regular`, wght 400, which is also its `fvar` default, so
            // those lines equal the default's) and the real-bold overlay
            // (wght 700).
            instances.push(vec![(*b"wght", 400.0)]);
            instances.push(vec![(*b"wght", 700.0)]);
        }
        for coords in &instances {
            let c: Vec<(u32, f32)> = coords
                .iter()
                .map(|(t, v)| (u32::from_be_bytes(*t), *v))
                .collect();
            for px in hint_raster::SIZES {
                for mode_name in hint_raster::MODES {
                    // The subpixel raster's stage-1 set excludes variable
                    // instances: it is only ever asked at the default.
                    if mode_name.starts_with("lcd") && !coords.is_empty() {
                        continue;
                    }
                    let answer =
                        hint::answer(&hint::Skrifa, &l.bytes, index, &gids, &c, mode_name, px)
                            .expect("a recorded mode");
                    out.push(line(&l, index, coords, mode_name, px, answer));
                }
            }
        }
    }
    out
}

fn line(
    l: &Loaded,
    index: u32,
    coords: &probe::Coords,
    mode: &str,
    px: f32,
    answer: String,
) -> Line {
    Line {
        kind: "hint".to_string(),
        label: l.face.label.to_string(),
        key: l.key.clone(),
        index,
        inst: probe::fmt_coords(coords),
        feats: mode.to_string(),
        case: format!("px{px:?}"),
        input: "-".to_string(),
        answer,
    }
}

const HINT_HEADER: &str = "\
# The font oracle's hinting answers (TF.1, docs/AUDIT-third-party-2026-10-15.md):
# skrifa 0.46.2's grid-fitted outlines and aterm's masks of them, recorded on
# macOS by crates/aterm-render/tests/font_oracle/recorder, a crate OUTSIDE the
# workspace (skrifa compiled into aterm-render only on Linux and Windows, and
# the workspace's patched graph is not the registry's). It runs
# tests/font_oracle/hint_raster.rs: copies of src/hinted.rs
# (hinted_glyph_raster, PathPen) and src/subpixel.rs (subpixel_glyph_raster,
# RGB order) over src/raster.rs included by path. Since TF.2 the linear
# advance and the lcd-unhinted outline are ttf-parser 0.25.1's
# (src/variation.rs VariedFace::linear_advance, src/subpixel.rs Scaled), so
# skrifa answers the grid fit alone. `native` has no lines: since TF.2b it
# renders as `light` (tests/font_hinting_config.rs native_renders_as_light),
# and the answers of the font's own TrueType bytecode it ran until then are
# in this file's git history. Since TF.4 crates/aterm-font/tests/hint_oracle.rs
# replays every line on every host against aterm-font's autohinter, carved
# from skrifa.
# One line per face, instance, mode and size, TAB-separated:
#   hint  label  file-key#index  instance  mode  px  -  answer
# mode: full | light (grayscale, hinted_glyph_raster) | lcd (LCD target,
# subpixel_glyph_raster) | lcd-unhinted (subpixel, no instance).
# Glyphs: the face's printable ASCII + Latin-1 glyphs (reader.rs latin_gids)
# plus every max(glyph_count/128, 1)th glyph id from 0, sorted, deduplicated.
# answer: glyphs=N, none=how many glyphs the raster declined, and two FNV-1a
# 64 digests (offset 0xcbf29ce484222325, prime 0x100000001b3) fed in glyph
# order, every integer little-endian and every f32 as its bits:
#   fnv: per glyph, its gid as u16; then for a raster w and h as u32, xmin
#     and ymin as i32, the advance as f32 (the linear advance in px for full
#     and light: hmtx, with HVAR at the instance, times px/upem in f32;
#     f32::NAN for lcd and lcd-unhinted, which have none)
#     and the coverage bytes (w*h, or w*h*3 RGB for the subpixel modes); for a
#     decline the byte `-` instead. A blank glyph is a 0x0 raster in the
#     grayscale modes and a decline in the subpixel ones.
#   outline: per glyph whose outline drew, the pen's commands, each as its
#     letter byte (M, L, Q, C, Z) then its coordinates' f32 bits, and the byte
#     `|` after the glyph's last command (a blank glyph is `|` alone; a glyph
#     whose outline fails to load or draw adds nothing).
# A face and size whose hinting instance cannot be built answers `no hinting
# instance`.
";

const GVAR_ALLOC_HEADER: &str = "\
# The font oracle's reader answers (TF.1 format, docs/AUDIT-third-party-2026-10-15.md)
# for the faces past 32 `gvar` tuples (GVAR_ALLOC_FACES in
# crates/aterm-render/tests/font_oracle/corpus.rs): ttf-parser 0.25.1 built
# with `gvar-alloc`, as aterm's Linux build ships it, recorded on macOS by
# crates/aterm-render/tests/font_oracle/recorder (`gvar-alloc` mode). Without
# that feature (aterm's macOS, Windows and wasm builds) such a glyph has no
# outline at any instance. aterm-font answers these on every platform
# (TF.3; crates/aterm-font/tests/reader_oracle.rs replays them).
# Same line format and digests as reader.txt; instances: none set, every axis
# at its minimum, every axis at its maximum, and wght 700 alone.
";

/// ttf-parser's `read` answers for [`GVAR_ALLOC_FACES`]: none set, all axes
/// at their minimum, all at their maximum, and `wght` 700 alone.
fn gvar_alloc_lines(dir: &str) -> Vec<Line> {
    let mut out = Vec::new();
    for l in load(dir) {
        if !GVAR_ALLOC_FACES.iter().any(|f| std::ptr::eq(f, l.face)) {
            continue;
        }
        for &index in l.face.indices {
            let face = ttf_parser::Face::parse(&l.bytes, index).expect("face");
            let axes: Vec<_> = face.variation_axes().into_iter().collect();
            let mut instances: Vec<probe::Coords> = vec![Vec::new()];
            instances.push(
                axes.iter()
                    .map(|a| (a.tag.to_bytes(), a.min_value))
                    .collect(),
            );
            instances.push(
                axes.iter()
                    .map(|a| (a.tag.to_bytes(), a.max_value))
                    .collect(),
            );
            if axes.iter().any(|a| a.tag.to_bytes() == *b"wght") {
                instances.push(vec![(*b"wght", 700.0)]);
            }
            for coords in &instances {
                for (case, answer) in reader::read(&l.bytes, index, coords) {
                    out.push(Line {
                        kind: "read".to_string(),
                        label: l.face.label.to_string(),
                        key: l.key.clone(),
                        index,
                        inst: probe::fmt_coords(coords),
                        feats: "-".to_string(),
                        case,
                        input: "-".to_string(),
                        answer,
                    });
                }
            }
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("replay") => replay(&args[2]),
        Some("hint") => {
            let mut lines = hint_lines(&args[2]);
            lines.sort_by_key(Line::id);
            let mut text = HINT_HEADER.to_string();
            for l in &lines {
                let _ = writeln!(text, "{}", l.render());
            }
            std::fs::write(&args[3], text).expect("write hinting corpus");
            println!("hint: {} lines", lines.len());
        }
        Some("gvar-alloc") => {
            let mut lines = gvar_alloc_lines(&args[2]);
            lines.sort_by_key(Line::id);
            let mut text = GVAR_ALLOC_HEADER.to_string();
            for l in &lines {
                let _ = writeln!(text, "{}", l.render());
            }
            std::fs::write(&args[3], text).expect("write gvar-alloc reader corpus");
            println!("gvar-alloc: {} lines", lines.len());
        }
        Some("hint-differential") => hint_diff::run(&args[2]),
        Some("differential") => {
            let runs = args.get(3).and_then(|n| n.parse().ok()).unwrap_or(2000);
            differential::run(&args[2], runs);
        }
        _ => eprintln!(
            "usage: replay <dir> | hint <dir> <out> | gvar-alloc <dir> <out> | differential <dir> [runs] | hint-differential <dir>"
        ),
    }
}
