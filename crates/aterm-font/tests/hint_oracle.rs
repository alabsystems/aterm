// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-font's autohinter (`aterm_font::autohint`, TF.4 of
//! docs/AUDIT-third-party-2026-10-15.md) held to the TF.1 hinting corpus
//! (`crates/aterm-render/tests/fixtures/font-oracle/hinting.txt`): what
//! skrifa 0.46.2, the hinter it was carved from, answered for every hinted
//! face, instance, mode and size. A line's answer digests every sampled
//! glyph's grid-fitted outline (each coordinate's f32 bits) and aterm's
//! coverage mask of it, so a line answers alike only when every outline is
//! bit-identical and every mask byte-identical.
//!
//! The answers are written by `hint_raster.rs` (aterm-render's
//! `tests/font_oracle/hint_raster.rs`), the code the out-of-workspace
//! recorder runs on skrifa; here the [`Hinter`] is the carve, the reader
//! aterm-font. aterm-render compiles the hinter only for Linux and Windows;
//! aterm-font is platform-neutral, so this replay runs on every host. A
//! system face this host lacks is skipped, never failed; a committed face
//! must answer every line.

use std::collections::{BTreeMap, BTreeSet};

use aterm_font::autohint::{HintingInstance, Target};
use aterm_font::{Face, GlyphId, OutlineSink, Tag};

use crate::corpus::{self, FACES, Line, Origin};
use crate::hint_raster::{Hinter, MODES, Mode, PathPen, SIZES, answer};
use crate::reader_oracle::reader::latin_gids;

/// The carve, over aterm-font's reader.
struct Carve;

impl Carve {
    fn target(mode: Mode) -> Target {
        match mode {
            Mode::Full => Target::Normal,
            Mode::Light => Target::Light,
            Mode::Lcd => Target::Lcd,
        }
    }
}

/// An outline sink into the oracle's pen, scaled by `scale` (1.0 for the
/// hinter, which draws in pixels already).
struct Pen<'a> {
    pen: &'a mut PathPen,
    scale: f32,
}

impl OutlineSink for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let s = self.scale;
        self.pen.move_to(x * s, y * s);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let s = self.scale;
        self.pen.line_to(x * s, y * s);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let s = self.scale;
        self.pen.quad_to(x1 * s, y1 * s, x * s, y * s);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let s = self.scale;
        self.pen
            .curve_to(x1 * s, y1 * s, x2 * s, y2 * s, x * s, y * s);
    }
    fn close(&mut self) {
        self.pen.close();
    }
}

impl Hinter for Carve {
    type Instance = HintingInstance;

    fn instance(
        &self,
        bytes: &[u8],
        index: u32,
        px: f32,
        mode: Mode,
        coords: &[(u32, f32)],
    ) -> Option<HintingInstance> {
        HintingInstance::new(bytes, index, px, coords, Self::target(mode))
    }

    fn draw(
        &self,
        hint: &HintingInstance,
        bytes: &[u8],
        index: u32,
        gid: u16,
        pen: &mut PathPen,
    ) -> Option<()> {
        // 1.0 is the identity: multiplying an f32 by it changes no bits.
        hint.draw(bytes, index, gid, &mut Pen { pen, scale: 1.0 })
    }

    fn unhinted(
        &self,
        bytes: &[u8],
        index: u32,
        gid: u16,
        px: f32,
        pen: &mut PathPen,
    ) -> Option<()> {
        let face = Face::parse(bytes, index).ok()?;
        let scale = px / f32::from(face.units_per_em());
        face.outline_glyph(GlyphId(gid), &mut Pen { pen, scale })?;
        Some(())
    }

    fn linear_advance(
        &self,
        bytes: &[u8],
        index: u32,
        gid: u16,
        px: f32,
        coords: &[(u32, f32)],
    ) -> Option<f32> {
        let mut face = Face::parse(bytes, index).ok()?;
        for &(tag, value) in coords {
            let _ = face.set_variation(Tag(tag), value);
        }
        let upem = f32::from(face.units_per_em());
        if upem <= 0.0 || !px.is_finite() || px <= 0.0 {
            return None;
        }
        let scale = px / upem;
        Some(f32::from(face.glyph_hor_advance(GlyphId(gid)).unwrap_or(0)) * scale)
    }
}

/// The recorder's glyph sample: the face's printable ASCII and Latin-1
/// glyphs, plus every `max(glyph_count / 128, 1)`th glyph id from 0.
fn sample_gids(bytes: &[u8], index: u32) -> Vec<u16> {
    let face = Face::parse(bytes, index).expect("a hinted corpus face parses");
    let n = face.number_of_glyphs();
    let mut gids = latin_gids(&face);
    let stride = (n / 128).max(1);
    gids.extend((0..n).step_by(usize::from(stride)));
    gids.sort_unstable();
    gids.dedup();
    gids
}

/// A hinted corpus face this host has.
struct Loaded {
    committed: bool,
    bytes: Vec<u8>,
    key: String,
}

fn hinted_faces() -> Vec<Loaded> {
    FACES
        .iter()
        .filter(|f| f.roles & corpus::HINT != 0)
        .filter_map(|face| {
            let path = match face.origin {
                Origin::Repo(rel) => {
                    format!("{}/../aterm-render/{rel}", env!("CARGO_MANIFEST_DIR"))
                }
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
                committed: matches!(face.origin, Origin::Repo(_)),
                key: corpus::file_key(&bytes),
                bytes,
            })
        })
        .collect()
}

fn corpus_lines() -> Vec<Line> {
    let path = format!(
        "{}/../aterm-render/tests/fixtures/font-oracle/hinting.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).expect("the hinting corpus");
    text.lines()
        .filter(|t| !t.is_empty() && !t.starts_with('#'))
        .map(|t| Line::parse(t).expect("a corpus line"))
        .collect()
}

/// A line's inputs: user-space coords and the size from its `px<size>` case.
fn inputs(line: &Line) -> (Vec<(u32, f32)>, f32) {
    let coords = corpus::parse_coords(&line.inst)
        .expect("corpus coords")
        .into_iter()
        .map(|(t, v)| (u32::from_be_bytes(t), v))
        .collect();
    let px = line
        .case
        .strip_prefix("px")
        .and_then(|p| p.parse().ok())
        .expect("a px case");
    (coords, px)
}

/// Whether one face has every mode/size case the recorder asks, independently
/// of the cases that happen to remain in the corpus.
fn covers_recording_cases<'a>(lines: impl Iterator<Item = &'a Line>, key: &str) -> bool {
    let recorded: BTreeSet<_> = lines
        .filter(|line| line.key == key)
        .map(|line| (line.feats.clone(), line.case.clone()))
        .collect();
    let expected: BTreeSet<_> = SIZES
        .into_iter()
        .flat_map(|px| MODES.map(|mode| (mode.to_string(), format!("px{px:?}"))))
        .collect();
    recorded == expected
}

/// Every hinting line answers as skrifa recorded it, on every committed face
/// and every system face this host has.
#[test]
fn hinting_answers_hold() {
    let faces = hinted_faces();
    let mut gids_by_key = BTreeMap::new();
    let (mut same, mut skipped) = (0usize, 0usize);
    let mut moved = Vec::new();
    let lines = corpus_lines();
    for line in &lines {
        assert_eq!(line.kind, "hint", "a hinting corpus line: {}", line.id());
        let Some(face) = faces.iter().find(|f| f.key == line.key) else {
            skipped += 1;
            continue;
        };
        let gids = gids_by_key
            .entry((line.key.clone(), line.index))
            .or_insert_with(|| sample_gids(&face.bytes, line.index));
        let (coords, px) = inputs(line);
        let now = answer(
            &Carve,
            &face.bytes,
            line.index,
            gids,
            &coords,
            &line.feats,
            px,
        )
        .expect("a recorded mode");
        if now == line.answer {
            same += 1;
        } else {
            moved.push(format!(
                "{}\n  corpus {}\n  carve  {}",
                line.id(),
                line.answer,
                now
            ));
        }
    }
    assert!(
        moved.is_empty(),
        "{} of {} hinting lines moved:\n{}",
        moved.len(),
        same + moved.len(),
        moved.join("\n")
    );
    // Not vacuous: every committed face (a `FACES` row with the HINT role and
    // a repo path; `hinted_faces` fails on one it cannot read) answered the
    // recorder's whole mode-by-size matrix, so a face whose lines lost their
    // key, or a corpus cut to fewer modes or sizes, fails here.
    let committed: Vec<&Loaded> = faces.iter().filter(|f| f.committed).collect();
    let short: Vec<&str> = committed
        .iter()
        .filter(|f| !covers_recording_cases(lines.iter(), &f.key))
        .map(|f| f.key.as_str())
        .collect();
    assert!(
        !committed.is_empty() && short.is_empty(),
        "every committed face must answer all {} (mode, size) pairs; short: {short:?} \
         ({} committed faces, {same} lines answered, {skipped} skipped)",
        SIZES.len() * MODES.len(),
        committed.len()
    );
}

/// The control: the same replay with the grayscale lines asked of the other
/// grayscale target (`full` answered by `light` and back) must move every
/// line whose face fits at all, or `hinting_answers_hold` could not tell the
/// targets apart and proves nothing about them. Removing every line of any
/// mode or size must also fail the replay's coverage check.
#[test]
fn swapped_targets_move_the_answers() {
    struct Swapped;
    impl Hinter for Swapped {
        type Instance = HintingInstance;
        fn instance(
            &self,
            bytes: &[u8],
            index: u32,
            px: f32,
            mode: Mode,
            coords: &[(u32, f32)],
        ) -> Option<HintingInstance> {
            let swapped = match mode {
                Mode::Full => Mode::Light,
                Mode::Light => Mode::Full,
                Mode::Lcd => Mode::Lcd,
            };
            Carve.instance(bytes, index, px, swapped, coords)
        }
        fn draw(
            &self,
            hint: &HintingInstance,
            bytes: &[u8],
            index: u32,
            gid: u16,
            pen: &mut PathPen,
        ) -> Option<()> {
            Carve.draw(hint, bytes, index, gid, pen)
        }
        fn unhinted(
            &self,
            bytes: &[u8],
            index: u32,
            gid: u16,
            px: f32,
            pen: &mut PathPen,
        ) -> Option<()> {
            Carve.unhinted(bytes, index, gid, px, pen)
        }
        fn linear_advance(
            &self,
            bytes: &[u8],
            index: u32,
            gid: u16,
            px: f32,
            coords: &[(u32, f32)],
        ) -> Option<f32> {
            Carve.linear_advance(bytes, index, gid, px, coords)
        }
    }
    let faces = hinted_faces();
    let lines = corpus_lines();
    let (mut asked, mut moved) = (0usize, 0usize);
    // The committed faces' 12 px full and light lines.
    for line in lines
        .iter()
        .filter(|l| (l.feats == "full" || l.feats == "light") && l.case == "px12.0")
    {
        let Some(face) = faces.iter().find(|f| f.key == line.key && f.committed) else {
            continue;
        };
        let gids = sample_gids(&face.bytes, line.index);
        let (coords, px) = inputs(line);
        let now = answer(
            &Swapped,
            &face.bytes,
            line.index,
            &gids,
            &coords,
            &line.feats,
            px,
        )
        .expect("a recorded mode");
        asked += 1;
        moved += usize::from(now != line.answer);
    }
    // Each committed face answers 12 px in both grayscale modes.
    let committed = faces.iter().filter(|f| f.committed).count();
    assert!(
        committed > 0 && asked >= 2 * committed,
        "the control asked {asked} lines of {committed} committed faces"
    );
    assert_eq!(
        moved, asked,
        "every swapped-target line must answer differently from the corpus"
    );
    for face in faces.iter().filter(|f| f.committed) {
        assert!(covers_recording_cases(lines.iter(), &face.key));
        for mode in MODES {
            assert!(
                !covers_recording_cases(lines.iter().filter(|l| l.feats != mode), &face.key),
                "removing every {mode} line must leave missing cases for {}",
                face.key
            );
        }
        for px in SIZES {
            let case = format!("px{px:?}");
            assert!(
                !covers_recording_cases(lines.iter().filter(|l| l.case != case), &face.key),
                "removing every {case} line must leave missing cases for {}",
                face.key
            );
        }
    }
}
