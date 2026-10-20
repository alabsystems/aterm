// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The work budgets over real faces (TF.5 of
//! docs/AUDIT-third-party-2026-10-15.md): how many outline steps each glyph
//! takes against aterm-font's 65,536 (`src/work.rs`) and how many `COLR`
//! paint tables each colour glyph walks against its 2^16
//! (`MAX_PAINT_VISITS`), read through aterm-font's debug-build seam
//! `take_spent`. Beside each count, the glyph as ttf-parser 0.25.1 answers
//! it, which has no such budget: the outline's commands and box (each
//! coordinate's f32 bits) and the paint walk's calls must match, so a
//! budget that cut a real glyph short would show as a difference.
//!
//! The domain, fixed so a rerun measures the same thing:
//!
//! - faces: `crate::hint_diff::face_files` (this host's system font
//!   directories, aterm-render's committed fonts, and the extra roots
//!   named on the command line), every face of a collection;
//! - instances: the default, and on a variable face every axis at its
//!   minimum and every axis at its maximum (all axes at once);
//! - glyphs: every glyph of the face; the paint walk for each glyph
//!   `is_color_glyph` names, palette 0.

use std::collections::BTreeMap;

use crate::probe::Fnv;

/// An outline digest that both readers' sinks feed alike.
struct Digest<'a>(&'a mut Fnv);

impl Digest<'_> {
    fn op(&mut self, letter: u8, values: &[f32]) {
        self.0.bytes(&[letter]);
        for &v in values {
            self.0.f32(v);
        }
    }
}

impl aterm_font::OutlineSink for Digest<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.op(b'M', &[x, y]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.op(b'L', &[x, y]);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.op(b'Q', &[x1, y1, x, y]);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.op(b'C', &[x1, y1, x2, y2, x, y]);
    }
    fn close(&mut self) {
        self.op(b'Z', &[]);
    }
}

impl ttf_parser::OutlineBuilder for Digest<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.op(b'M', &[x, y]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.op(b'L', &[x, y]);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.op(b'Q', &[x1, y1, x, y]);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.op(b'C', &[x1, y1, x2, y2, x, y]);
    }
    fn close(&mut self) {
        self.op(b'Z', &[]);
    }
}

fn boxed(h: &mut Fnv, r: Option<[i16; 4]>) {
    match r {
        Some(r) => {
            h.bytes(b"R");
            for v in r {
                h.i16(v);
            }
        }
        None => h.bytes(b"-"),
    }
}

/// The paint walk's calls, kinds and geometry (a solid's colour, a clip
/// box, a transform, a layer's mode); gradients by kind alone (the reader
/// corpus's `colr` lines hold their stops).
struct Walk<'a>(&'a mut Fnv);

impl<'a> aterm_font::colr::Painter<'a> for Walk<'_> {
    fn outline_glyph(&mut self, g: aterm_font::GlyphId) {
        self.0.bytes(b"g");
        self.0.u16(g.0);
    }
    fn paint(&mut self, paint: aterm_font::colr::Paint<'a>) {
        use aterm_font::colr::Paint;
        match paint {
            Paint::Solid(c) => self.0.bytes(&[b's', c.red, c.green, c.blue, c.alpha]),
            Paint::LinearGradient(_) => self.0.bytes(b"l"),
            Paint::RadialGradient(_) => self.0.bytes(b"r"),
            Paint::SweepGradient(_) => self.0.bytes(b"w"),
        }
    }
    fn push_clip(&mut self) {
        self.0.bytes(b"c");
    }
    fn push_clip_box(&mut self, b: aterm_font::colr::ClipBox) {
        self.0.bytes(b"b");
        for v in [b.x_min, b.y_min, b.x_max, b.y_max] {
            self.0.f32(v);
        }
    }
    fn pop_clip(&mut self) {
        self.0.bytes(b"C");
    }
    fn push_layer(&mut self, mode: aterm_font::colr::CompositeMode) {
        self.0.bytes(format!("y{mode:?}").as_bytes());
    }
    fn pop_layer(&mut self) {
        self.0.bytes(b"Y");
    }
    fn push_transform(&mut self, t: aterm_font::Transform) {
        self.0.bytes(b"t");
        for v in [t.a, t.b, t.c, t.d, t.e, t.f] {
            self.0.f32(v);
        }
    }
    fn pop_transform(&mut self) {
        self.0.bytes(b"T");
    }
}

impl<'a> ttf_parser::colr::Painter<'a> for Walk<'_> {
    fn outline_glyph(&mut self, g: ttf_parser::GlyphId) {
        self.0.bytes(b"g");
        self.0.u16(g.0);
    }
    fn paint(&mut self, paint: ttf_parser::colr::Paint<'a>) {
        use ttf_parser::colr::Paint;
        match paint {
            Paint::Solid(c) => self.0.bytes(&[b's', c.red, c.green, c.blue, c.alpha]),
            Paint::LinearGradient(_) => self.0.bytes(b"l"),
            Paint::RadialGradient(_) => self.0.bytes(b"r"),
            Paint::SweepGradient(_) => self.0.bytes(b"w"),
        }
    }
    fn push_clip(&mut self) {
        self.0.bytes(b"c");
    }
    fn push_clip_box(&mut self, b: ttf_parser::colr::ClipBox) {
        self.0.bytes(b"b");
        for v in [b.x_min, b.y_min, b.x_max, b.y_max] {
            self.0.f32(v);
        }
    }
    fn pop_clip(&mut self) {
        self.0.bytes(b"C");
    }
    fn push_layer(&mut self, mode: ttf_parser::colr::CompositeMode) {
        self.0.bytes(format!("y{mode:?}").as_bytes());
    }
    fn pop_layer(&mut self) {
        self.0.bytes(b"Y");
    }
    fn push_transform(&mut self, t: ttf_parser::Transform) {
        self.0.bytes(b"t");
        for v in [t.a, t.b, t.c, t.d, t.e, t.f] {
            self.0.f32(v);
        }
    }
    fn pop_transform(&mut self) {
        self.0.bytes(b"T");
    }
}

/// The heaviest glyph-instance seen, for the report.
#[derive(Default)]
struct Heaviest {
    spent: u64,
    at: String,
}

impl Heaviest {
    fn see(&mut self, spent: u64, at: impl FnOnce() -> String) {
        if spent > self.spent {
            self.spent = spent;
            self.at = at();
        }
    }
}

#[derive(Default)]
struct Tally {
    files: usize,
    faces: usize,
    glyph_instances: u64,
    outlines: u64,
    outline_differ: u64,
    over_4096: u64,
    over_16384: u64,
    refused: u64,
    steps: Heaviest,
    colour_faces: usize,
    colour_walks: u64,
    walk_differ: u64,
    walks_refused: u64,
    paint: Heaviest,
    /// Glyph-instances per power-of-two step bucket (`2^k <= steps < 2^(k+1)`).
    histogram: BTreeMap<u32, u64>,
}

const OUTLINE_BUDGET: u64 = 65_536;
const PAINT_BUDGET: u64 = 1 << 16;

fn instances(face: &ttf_parser::Face) -> Vec<Vec<(ttf_parser::Tag, f32)>> {
    let axes: Vec<_> = face.variation_axes().into_iter().collect();
    let mut out = vec![Vec::new()];
    if !axes.is_empty() {
        out.push(axes.iter().map(|a| (a.tag, a.min_value)).collect());
        out.push(axes.iter().map(|a| (a.tag, a.max_value)).collect());
    }
    out
}

pub(crate) fn run(dir: &str, extra: &[String]) {
    let mut t = Tally::default();
    let mut report = Vec::new();
    for path in crate::hint_diff::face_files(dir, extra) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        t.files += 1;
        let count = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1);
        for index in 0..count {
            let (Ok(mut theirs), Ok(mut ours)) = (
                ttf_parser::Face::parse(&bytes, index),
                aterm_font::Face::parse(&bytes, index),
            ) else {
                continue;
            };
            t.faces += 1;
            let colour = ours.tables().colr.is_some();
            t.colour_faces += usize::from(colour);
            let mut face_differ = 0u64;
            for coords in instances(&theirs) {
                for &(tag, value) in &coords {
                    let _ = theirs.set_variation(tag, value);
                    let _ = ours.set_variation(aterm_font::Tag(tag.0), value);
                }
                let inst = coords
                    .iter()
                    .map(|(tag, v)| format!("{}={v}", crate::probe::tag_str(tag.to_bytes())))
                    .collect::<Vec<_>>()
                    .join(",");
                let _ = aterm_font::take_spent();
                for g in 0..ours.number_of_glyphs() {
                    t.glyph_instances += 1;
                    let (mut a, mut b) = (Fnv::new(), Fnv::new());
                    let r = ours.outline_glyph(aterm_font::GlyphId(g), &mut Digest(&mut a));
                    let steps = aterm_font::take_spent().outline_steps;
                    boxed(&mut a, r.map(|r| [r.x_min, r.y_min, r.x_max, r.y_max]));
                    let r = theirs.outline_glyph(ttf_parser::GlyphId(g), &mut Digest(&mut b));
                    boxed(&mut b, r.map(|r| [r.x_min, r.y_min, r.x_max, r.y_max]));
                    t.outlines += u64::from(r.is_some());
                    *t.histogram
                        .entry(u64::BITS - steps.leading_zeros())
                        .or_default() += 1;
                    t.over_4096 += u64::from(steps > 4096);
                    t.over_16384 += u64::from(steps > 16_384);
                    t.refused += u64::from(steps > OUTLINE_BUDGET);
                    t.steps
                        .see(steps, || format!("{path}#{index} [{inst}] gid {g}"));
                    if a.hex() != b.hex() {
                        t.outline_differ += 1;
                        face_differ += 1;
                        if face_differ <= 3 {
                            report.push(format!(
                                "OUTLINE DIFFERS {path}#{index} [{inst}] gid {g} ({steps} steps)"
                            ));
                        }
                    }
                    if !colour || !ours.is_color_glyph(aterm_font::GlyphId(g)) {
                        continue;
                    }
                    t.colour_walks += 1;
                    let fg = (0, 0, 0, 255);
                    let (mut a, mut b) = (Fnv::new(), Fnv::new());
                    let painted = ours.paint_color_glyph(
                        aterm_font::GlyphId(g),
                        0,
                        aterm_font::RgbaColor::new(fg.0, fg.1, fg.2, fg.3),
                        &mut Walk(&mut a),
                    );
                    let tables = aterm_font::take_spent().paint_tables;
                    a.bytes(if painted.is_some() { b"+" } else { b"!" });
                    let painted = theirs.paint_color_glyph(
                        ttf_parser::GlyphId(g),
                        0,
                        ttf_parser::RgbaColor::new(fg.0, fg.1, fg.2, fg.3),
                        &mut Walk(&mut b),
                    );
                    b.bytes(if painted.is_some() { b"+" } else { b"!" });
                    t.walks_refused += u64::from(tables > PAINT_BUDGET);
                    t.paint
                        .see(tables, || format!("{path}#{index} [{inst}] gid {g}"));
                    if a.hex() != b.hex() {
                        t.walk_differ += 1;
                        face_differ += 1;
                        if face_differ <= 3 {
                            report.push(format!(
                                "PAINT DIFFERS {path}#{index} [{inst}] gid {g} ({tables} tables)"
                            ));
                        }
                    }
                }
            }
            if face_differ > 0 {
                report.push(format!("FACE {path}#{index}: {face_differ} differ"));
            }
        }
    }
    for line in &report {
        println!("{line}");
    }
    let buckets: Vec<String> = t
        .histogram
        .iter()
        .map(|(k, n)| match k {
            0 => format!("0: {n}"),
            k => format!("{}-{}: {n}", 1u64 << (k - 1), (1u64 << k) - 1),
        })
        .collect();
    println!(
        "budgets: outline steps per glyph-instance {}",
        buckets.join(", ")
    );
    println!(
        "budgets: {} files, {} faces, {} glyph-instances ({} with an outline): heaviest {} steps ({}), {} over 4,096, {} over 16,384, {} refused at 65,536; {} differ from ttf-parser",
        t.files,
        t.faces,
        t.glyph_instances,
        t.outlines,
        t.steps.spent,
        t.steps.at,
        t.over_4096,
        t.over_16384,
        t.refused,
        t.outline_differ
    );
    println!(
        "budgets: {} COLR faces, {} colour glyph walks: heaviest {} paint tables ({}), {} refused at 2^16; {} differ from ttf-parser",
        t.colour_faces, t.colour_walks, t.paint.spent, t.paint.at, t.walks_refused, t.walk_differ
    );
}
