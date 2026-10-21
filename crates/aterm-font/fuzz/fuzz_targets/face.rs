// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Every query aterm-font answers, over arbitrary bytes as a font file.
//!
//! ## Running
//!
//! STOCK EXCEPTION (Trust ships no cargo-fuzz and no libFuzzer runtime).
//! On a stock nightly with cargo-fuzz, seeded with the faces the reader
//! corpus records:
//!
//! ```bash
//! cd crates/aterm-font
//! mkdir -p fuzz/corpus/face
//! cp ../aterm-render/tests/fixtures/*.ttf ../aterm-render/tests/fixtures/*.otf \
//!    ../aterm-render/assets/game/Cinzel-var.ttf fuzz/corpus/face/
//! cargo +nightly fuzz run face -- -max_total_time=600 -timeout=10
//! ```
//!
//! ## Property
//!
//! A malformed font returns errors and `None`s: `Face::parse`, every
//! variation (NaN and infinities included), cmap lookup and enumeration,
//! names, OS/2 and the `MVAR`-varied line metrics, legacy kerning,
//! advances, side bearings, origins, phantom points, `GSUB`/`GPOS` lookups
//! and their subtables, `GDEF` classes, bitmap images, every glyph's outline
//! into both sinks and every COLR glyph's paint graph never panic, and never
//! run away (cmap enumeration stops at U+10FFFF and after one call per code
//! point; charstring recursion and `glyf` composites are depth-limited; an
//! extension subtable never wraps another; a COLR walk stops at its depth
//! limit and after 2^16 paint tables). The shaper (`aterm_font::shape`, TF.6)
//! shapes a ligature run, an Arabic word, marks and an emoji sequence on the
//! same face, with and without features, under the same property, and
//! parses every `GSUB` lookup of the face, before any applies, within
//! aterm's own budget of 8 units per byte of the table (one unit per
//! subtable and coverage entry parsed), however their offsets alias.

#![no_main]

use aterm_font::colr::{ClipBox, CompositeMode, Paint, Painter};
use aterm_font::{Face, GlyphId, OutlineSink, PointKind, PointSink, RgbaColor, Transform};
use libfuzzer_sys::fuzz_target;

struct Count(u64);

impl OutlineSink for Count {
    fn move_to(&mut self, _: f32, _: f32) {
        self.0 += 1;
    }
    fn line_to(&mut self, _: f32, _: f32) {
        self.0 += 1;
    }
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {
        self.0 += 1;
    }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
        self.0 += 1;
    }
    fn close(&mut self) {
        self.0 += 1;
    }
}

impl PointSink for Count {
    fn point(&mut self, _: f32, _: f32, _: PointKind) {
        self.0 += 1;
    }
    fn end_contour(&mut self) {
        self.0 += 1;
    }
}

impl<'a> Painter<'a> for Count {
    fn outline_glyph(&mut self, _: GlyphId) {
        self.0 += 1;
    }
    fn paint(&mut self, paint: Paint<'a>) {
        self.0 += match paint {
            Paint::Solid(_) => 1,
            Paint::LinearGradient(g) => g.stops(0, &[]).count() as u64,
            Paint::RadialGradient(g) => g.stops(0, &[]).count() as u64,
            Paint::SweepGradient(g) => g.stops(0, &[]).count() as u64,
        };
    }
    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, _: ClipBox) {}
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _: CompositeMode) {}
    fn pop_layer(&mut self) {}
    fn push_transform(&mut self, _: Transform) {}
    fn pop_transform(&mut self) {}
}

fuzz_target!(|data: &[u8]| {
    let mut sink = Count(0);
    let _ = aterm_font::fonts_in_collection(data);
    for index in 0..2 {
        let Ok(mut face) = Face::parse(data, index) else {
            continue;
        };
        // An instance from the input's own last bytes, so the fuzzer steers it.
        let axes: Vec<_> = face.variation_axes().into_iter().map(|a| a.tag).collect();
        for (i, tag) in axes.into_iter().enumerate() {
            let at = data.len().wrapping_sub(4 * (i + 1));
            let v = data
                .get(at..at.wrapping_add(4))
                .and_then(|b| b.try_into().ok())
                .map_or(0.0, f32::from_le_bytes);
            let _ = face.set_variation(tag, v);
        }
        if let Some(cmap) = face.tables().cmap {
            for sub in cmap.subtables {
                sub.codepoints(|_| sink.0 += 1);
                let _ = sub.glyph_index(0x41);
                let _ = sub.glyph_variation_index(0x41, 0xFE0F);
            }
        }
        // Every (lookup, subtable) pair, up to 2^16 of them: lookup and
        // subtable offsets may alias, so the pairs a table names grow as the
        // square of its size, and an unbounded walk spends the run's time on
        // re-reading one subtable (slow units of 2.3-2.4 s, 2026-10-19). The
        // library's own walks are bounded by its `GSUB` budget
        // (`work::LayoutWork`).
        let mut visits = 0u32;
        'walk: for table in [face.tables().gsub, face.tables().gpos]
            .into_iter()
            .flatten()
        {
            for i in 0..table.lookups.len() {
                let Some(lookup) = table.lookups.get(i) else {
                    continue;
                };
                for j in 0..lookup.subtables.len() {
                    visits += 1;
                    if visits > 1 << 16 {
                        break 'walk;
                    }
                    if let Some(s) = lookup
                        .subtables
                        .get::<aterm_font::gsub::SubstitutionSubtable>(j)
                    {
                        sink.0 += u64::from(s.coverage().get(GlyphId(1)).is_some());
                    }
                }
            }
        }
        for name in face.names() {
            sink.0 += name.to_string().map_or(0, |s| s.len() as u64);
        }
        let _ = (face.weight(), face.style(), face.permissions());
        let _ = (
            face.ascender(),
            face.descender(),
            face.height(),
            face.line_gap(),
        );
        let _ = (
            face.x_height(),
            face.underline_metrics(),
            face.strikeout_metrics(),
        );
        let _ = (face.vertical_height(), face.subscript_metrics());
        if let Some(kern) = face.tables().kern {
            for sub in kern.subtables {
                let _ = sub.glyphs_kerning(GlyphId(1), GlyphId(2));
            }
        }
        for ch in ['A', '\u{FFFD}', '\u{1F600}'] {
            let _ = face.glyph_index(ch);
            let _ = face.glyph_variation_index(ch, '\u{FE0F}');
        }
        {
            use aterm_font::shape::{Feature, ShapeFace, UnicodeBuffer, shape};
            let shaper = ShapeFace::new(face.clone());
            let features = [
                Feature::new(aterm_font::Tag::from_bytes(b"liga"), 1),
                Feature::new(aterm_font::Tag::from_bytes(b"calt"), 1),
                Feature::new(aterm_font::Tag::from_bytes(b"ss01"), 2),
            ];
            for text in [
                "a => b != c ffi 1\u{2044}2",
                "\u{645}\u{631}\u{62D}\u{628}\u{627}\u{644}\u{627}",
                "e\u{301}\u{302} \u{2764}\u{FE0F}\u{200D}\u{1F525}",
            ] {
                for list in [&features[..], &[]] {
                    let mut buffer = UnicodeBuffer::new();
                    buffer.push_str(text);
                    sink.0 += shape(&shaper, list, buffer).len() as u64;
                }
            }
        }
        for g in 0..face.number_of_glyphs().min(512) {
            let g = GlyphId(g);
            let _ = face.glyph_hor_advance(g);
            let _ = face.glyph_hor_side_bearing(g);
            let _ = face.glyph_ver_advance(g);
            let _ = face.glyph_ver_side_bearing(g);
            let _ = face.glyph_y_origin(g);
            let _ = face.glyph_phantom_points(g);
            let _ = face.outline_glyph(g, &mut sink);
            let _ = face.outline_glyph_points(g, &mut sink);
            if let Some(gdef) = face.tables().gdef {
                let _ = (gdef.glyph_class(g), gdef.is_mark_glyph(g, None));
            }
            if let Some(img) = face.glyph_raster_image(g, 64) {
                sink.0 += img.data.len() as u64;
            }
            if face.is_color_glyph(g) {
                let fg = RgbaColor::new(0, 0, 0, 255);
                let _ = face.paint_color_glyph(g, 0, fg, &mut sink);
            }
        }
    }
    std::hint::black_box(sink.0);
});
