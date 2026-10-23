// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compares `aterm-font` against the `ttf-parser` 0.25.1 reader corpus (`reader.txt` and `reader-gvar-alloc.txt` in `aterm-render/tests/fixtures/font-oracle/`).

use std::collections::{BTreeMap, BTreeSet};

use aterm_font::{Face, GlyphId, OutlineSink, Rect, ScaledSink};

use crate::corpus::{self, BLOCK, FACES, FaceFile, GVAR_ALLOC_FACES, Line, Origin};

/// Re-exports `aterm_font` under `ttf-parser` names for the shared `reader.rs` harness.
mod font {
    pub(crate) use aterm_font::OutlineSink as OutlineBuilder;
    pub(crate) use aterm_font::*;
}
#[path = "../../aterm-render/tests/font_oracle/reader.rs"]
pub(crate) mod reader;

/// Loaded font face bytes and corpus key.
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

/// Recorded `read` lines indexed by face key, `(index, instance)`, and query.
type Recorded = BTreeMap<String, BTreeMap<(u32, String), BTreeMap<String, String>>>;

/// Parses recorded `read` lines from `tests/fixtures/font-oracle/<file>` in `aterm-render`.
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

/// Summary counts and mismatches from one corpus replay.
#[derive(Default, Debug)]
struct Tally {
    compared: usize,
    outline_lines: BTreeMap<&'static str, usize>,
    families: BTreeMap<String, usize>,
    moved: Vec<String>,
    faces: BTreeSet<&'static str>,
    /// Faces with a compared `colr.*` answer that paints something.
    painted: BTreeSet<&'static str>,
}

/// Outline table source selected by [`Face::outline_glyph`].
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

/// Replays `file`'s lines for `faces` against `aterm-font`.
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

/// Every reader answer on available faces matches the recorded `reader.txt` corpus.
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
    for family in [
        "head", "os2", "name", "metrics", "cmap", "hmtx", "gsub", "gdef", "kern", "cpal", "colr",
    ] {
        assert!(
            tally.families.get(family).copied().unwrap_or(0) > 0,
            "no `{family}` answer compared: {:?}",
            tally.families
        );
    }
    assert!(
        tally.painted.contains("fixture-colr_1"),
        "the COLR fixture paints no colour glyph: {:?}",
        tally.painted
    );
}

/// Negative control: translating an outline by 1 unit through [`ScaledSink`] changes its digest.
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

/// Faces with glyphs past 32 `gvar` tuples match `reader-gvar-alloc.txt` when present on this host.
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
