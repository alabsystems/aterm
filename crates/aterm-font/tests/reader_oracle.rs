// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-font held to the TF.1 reader corpus
//! (`crates/aterm-render/tests/fixtures/font-oracle/reader.txt`): the answers
//! ttf-parser 0.25.1 recorded, per face and variation instance, for every
//! query the reader answers (TF.3 and TF.5 of
//! docs/AUDIT-third-party-2026-10-15.md).
//!
//! - `head`, `os2`, `name`: units per em, glyph count, global box, the table
//!   directory, the OS/2 classes, flags and Unicode ranges, every name record;
//! - `fvar`, `coords`: the axes, and the `fvar`+`avar` normalized instance;
//! - `cmap.sub<i>`, `cmap.index`, `cmap.vs`: each subtable's enumeration and
//!   lookups, the face-level lookup, and variation sequences;
//! - `metrics`: the `hhea`/OS/2/`vhea`/`post` line metrics with `MVAR` deltas;
//! - `hmtx`, `vmtx`: advances and side bearings with `HVAR`/`VVAR`/`gvar`
//!   deltas, and `VORG`;
//! - `bbox`, `outline.<start>`: every glyph's outline as an exact point
//!   stream digest (`glyf`, `gvar`, CFF, CFF2), and its bounding box;
//! - `kern.<i>`, `gsub`, `gpos`, `gsub.lookups`, `gdef`: legacy kerning, the
//!   layout lists, every `GSUB` lookup's subtables whole, glyph classes;
//! - `sbix.strike<i>`, `raster.<ppem>`, `cpal`, `colr.<start>`: bitmap
//!   strikes and the face-level image lookup, and every COLR glyph's paint
//!   graph at each recorded instance.
//!
//! The answers are written by `reader.rs` (aterm-render's
//! `tests/font_oracle/reader.rs`), the same code the out-of-workspace
//! recorder runs on ttf-parser, so a line here is the line it recorded. A
//! system face this host lacks is skipped, never failed; a committed face
//! must answer every line.
//!
//! `reader.txt` is ttf-parser as `-p aterm-render` resolved it, without its
//! `gvar-alloc` feature; no face there has a glyph past 32 `gvar` tuples,
//! where that feature matters. The faces that do (`GVAR_ALLOC_FACES`) are
//! held to `reader-gvar-alloc.txt`, which ttf-parser recorded with the
//! feature, as aterm's Linux build ships it (`gvar_tuples.rs`).

use std::collections::{BTreeMap, BTreeSet};

use aterm_font::{Face, GlyphId, OutlineSink, Rect, ScaledSink};

use crate::corpus::{self, BLOCK, FACES, FaceFile, GVAR_ALLOC_FACES, Line, Origin};

/// The reader `reader.rs` answers with here: aterm-font, under ttf-parser's
/// names (its outline sink trait is `OutlineSink`).
mod font {
    pub(crate) use aterm_font::OutlineSink as OutlineBuilder;
    pub(crate) use aterm_font::*;
}
#[path = "../../aterm-render/tests/font_oracle/reader.rs"]
pub(crate) mod reader;

/// One face file this host has, and its corpus key.
struct Loaded {
    label: &'static str,
    committed: bool,
    bytes: Vec<u8>,
    key: String,
}

fn load(face: &corpus::FaceFile) -> Option<Loaded> {
    let path = match face.origin {
        Origin::Repo(rel) => format!("{}/../aterm-render/{rel}", env!("CARGO_MANIFEST_DIR")),
        Origin::System(abs) => abs.to_string(),
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if matches!(face.origin, Origin::Repo(_)) => {
            panic!("committed face {path} is unreadable: {e}")
        }
        Err(_) => return None,
    };
    Some(Loaded {
        label: face.label,
        committed: matches!(face.origin, Origin::Repo(_)),
        key: corpus::file_key(&bytes),
        bytes,
    })
}

/// The recorded `read` lines, by face key, then by `(index, instance)`, then
/// by query.
type Recorded = BTreeMap<String, BTreeMap<(u32, String), BTreeMap<String, String>>>;

/// The recorded lines of `tests/fixtures/font-oracle/<file>` in aterm-render.
fn recorded(file: &str) -> Recorded {
    let path = format!(
        "{}/../aterm-render/tests/fixtures/font-oracle/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the TF.1 reader corpus {path}: {e}"))
        .replace("\r\n", "\n");
    let mut out = Recorded::new();
    for t in text
        .lines()
        .filter(|t| !t.is_empty() && !t.starts_with('#'))
    {
        let line = Line::parse(t).unwrap_or_else(|| panic!("malformed corpus line: {t:?}"));
        if line.kind != "read" {
            continue;
        }
        out.entry(line.key)
            .or_default()
            .entry((line.index, line.inst))
            .or_default()
            .insert(line.case, line.answer);
    }
    out
}

/// What one replay compared: by outline source, and by query family (the
/// case name before its first `.`).
#[derive(Default, Debug)]
struct Tally {
    compared: usize,
    outline_lines: BTreeMap<&'static str, usize>,
    families: BTreeMap<String, usize>,
    moved: Vec<String>,
    faces: BTreeSet<&'static str>,
    /// The faces with a compared `colr.*` answer that paints something.
    painted: BTreeSet<&'static str>,
}

/// The table a face's outlines come from, as [`Face::outline_glyph`] picks it.
fn outline_source(face: &Face, varied: bool) -> &'static str {
    let t = face.tables();
    if t.gvar.is_some() && t.glyf.is_some() {
        if varied { "gvar" } else { "glyf+gvar" }
    } else if t.glyf.is_some() {
        "glyf"
    } else if t.cff.is_some() {
        "cff"
    } else if t.cff2.is_some() {
        if varied { "cff2-varied" } else { "cff2" }
    } else {
        "none"
    }
}

/// Replay `file`'s lines for `faces` against aterm-font.
fn replay(faces: &[FaceFile], file: &str) -> Tally {
    let recorded = recorded(file);
    let mut tally = Tally::default();
    for face in faces {
        let Some(l) = load(face) else { continue };
        let Some(instances) = recorded.get(&l.key) else {
            assert!(
                !l.committed,
                "committed face {} has no recorded reader lines",
                l.label
            );
            continue;
        };
        for &index in face.indices {
            let mut seen_index = false;
            for ((rec_index, inst), want) in instances {
                if *rec_index != index {
                    continue;
                }
                seen_index = true;
                let coords = corpus::parse_coords(inst).expect("corpus coordinates");
                let got: BTreeMap<String, String> =
                    reader::read(&l.bytes, index, &coords).into_iter().collect();
                let source = Face::parse(&l.bytes, index)
                    .map(|f| outline_source(&f, inst != "-"))
                    .unwrap_or("unparsed");
                for (case, answer) in want {
                    match got.get(case) {
                        Some(a) if a == answer => {
                            tally.compared += 1;
                            let family = case.split('.').next().unwrap_or(case);
                            *tally.families.entry(family.to_string()).or_default() += 1;
                            if case.starts_with("outline.") {
                                *tally.outline_lines.entry(source).or_default() += 1;
                            }
                            if case.starts_with("colr.") && !answer.starts_with("n=0") {
                                tally.painted.insert(l.label);
                            }
                        }
                        Some(a) => tally.moved.push(format!(
                            "  {} #{index} [{inst}] {case}\n    recorded {answer}\n    now      {a}",
                            l.label
                        )),
                        None => tally.moved.push(format!(
                            "  {} #{index} [{inst}] {case}: recorded {answer}, not answered",
                            l.label
                        )),
                    }
                }
                for case in got.keys() {
                    if !want.contains_key(case) {
                        tally.moved.push(format!(
                            "  {} #{index} [{inst}] {case}: answered, never recorded",
                            l.label
                        ));
                    }
                }
            }
            assert!(
                seen_index || !l.committed,
                "committed face {} #{index} has no recorded reader lines",
                l.label
            );
        }
        tally.faces.insert(l.label);
    }
    tally
}

/// Every answer the reader gives matches what ttf-parser recorded, on every
/// face this host has and every instance the corpus recorded it at.
#[test]
fn reader_corpus_answers_hold() {
    let tally = replay(FACES, "reader.txt");
    eprintln!(
        "aterm-font reader oracle: {} answers compared over {} faces; outline blocks by source {:?}; by query {:?}",
        tally.compared,
        tally.faces.len(),
        tally.outline_lines,
        tally.families
    );
    assert!(
        tally.moved.is_empty(),
        "aterm-font moved {} reader answer(s) from the TF.1 corpus:\n{}",
        tally.moved.len(),
        tally.moved.join("\n")
    );
    // Not vacuous: the committed faces alone answer hundreds of lines, and
    // they cover plain glyf, glyf at a gvar instance, and CFF.
    assert!(
        tally.compared >= 300,
        "only {} answers compared",
        tally.compared
    );
    for source in ["glyf", "gvar", "cff"] {
        assert!(
            tally.outline_lines.get(source).copied().unwrap_or(0) > 0,
            "no `{source}` outline block compared: {:?}",
            tally.outline_lines
        );
    }
    // And every query family the committed faces carry: the COLR fixtures
    // paint, Chewy and Luckiest Guy kern, every face has OS/2 and `name`.
    for family in [
        "head", "os2", "name", "metrics", "cmap", "hmtx", "gsub", "gdef", "kern", "cpal", "colr",
    ] {
        assert!(
            tally.families.get(family).copied().unwrap_or(0) > 0,
            "no `{family}` answer compared: {:?}",
            tally.families
        );
    }
    // A bless that recorded empty paint graphs would compare `colr` lines too.
    assert!(
        tally.painted.contains("fixture-colr_1"),
        "the COLR fixture paints no colour glyph: {:?}",
        tally.painted
    );
}

/// NEGATIVE CONTROL: the outline digest sees a one-unit shift. The same
/// committed-face block, rerouted through a [`ScaledSink`], matches the
/// recorded answer at the identity and moves when the sink translates by 1.
#[test]
fn a_shifted_outline_does_not_match() {
    let recorded = recorded("reader.txt");
    let face_file = FACES
        .iter()
        .find(|f| f.label == "Cinzel-var")
        .expect("Cinzel-var is a committed face");
    let l = load(face_file).expect("committed face");
    let want = &recorded[&l.key][&(0, "-".to_string())]["outline.0"];
    let face = Face::parse(&l.bytes, 0).expect("Cinzel parses");
    let glyphs = u32::from(face.number_of_glyphs()).min(BLOCK);
    let identity = |face: &Face<'_>, g: GlyphId, sink: &mut dyn OutlineSink| -> Option<Rect> {
        face.outline_glyph(g, &mut ScaledSink::new(sink, 1.0))
    };
    let shifted = |face: &Face<'_>, g: GlyphId, sink: &mut dyn OutlineSink| -> Option<Rect> {
        face.outline_glyph(g, &mut ScaledSink::affine(sink, 1.0, 1.0, -0.0, 1.0))
    };
    assert_eq!(
        &reader::outline_block(&face, 0, glyphs, &identity),
        want,
        "a unit-scale ScaledSink is bit-transparent"
    );
    assert_ne!(
        &reader::outline_block(&face, 0, glyphs, &shifted),
        want,
        "a one-unit shift must move the digest"
    );
}

/// The faces with glyphs past 32 `gvar` tuples answer what ttf-parser with
/// `gvar-alloc` recorded: every glyph draws, at every recorded instance. A
/// host without them (they are macOS system faces) compares nothing here;
/// `gvar_tuples.rs` covers the same path on every host.
#[test]
fn gvar_alloc_answers_hold() {
    let tally = replay(GVAR_ALLOC_FACES, "reader-gvar-alloc.txt");
    eprintln!(
        "aterm-font gvar-alloc oracle: {} answers compared over {} faces; outline blocks by source {:?}",
        tally.compared,
        tally.faces.len(),
        tally.outline_lines
    );
    assert!(
        tally.moved.is_empty(),
        "aterm-font moved {} answer(s) from ttf-parser's gvar-alloc corpus:\n{}",
        tally.moved.len(),
        tally.moved.join("\n")
    );
    if !tally.faces.is_empty() {
        for source in ["glyf+gvar", "gvar"] {
            assert!(
                tally.outline_lines.get(source).copied().unwrap_or(0) > 0,
                "no `{source}` outline block compared: {:?}",
                tally.outline_lines
            );
        }
    }
}
