// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The reader half of the font oracle: what a font reader answers for a face, written as text.

use super::corpus::{BLOCK, Fnv, escape, pin, probe_chars, tag_str};
use super::font;

#[path = "layout.rs"]
mod layout_digest;

/// The glyphs of printable ASCII and Latin-1 through the face's Unicode cmap, deduplicated,
/// `.notdef` dropped: the pair set the `kern` answers sweep.
pub(crate) fn latin_gids(face: &font::Face) -> Vec<u16> {
    let mut gids: Vec<u16> = (0x20u32..=0x7E)
        .chain(0xA0..=0xFF)
        .filter_map(char::from_u32)
        .filter_map(|c| face.glyph_index(c))
        .map(|g| g.0)
        .filter(|&g| g != 0)
        .collect();
    gids.sort_unstable();
    gids.dedup();
    gids
}

/// Every code point the face's Unicode cmap subtables enumerate, sorted.
pub(crate) fn unicode_coverage(face: &font::Face) -> Vec<u32> {
    let mut cps = Vec::new();
    if let Some(cmap) = face.tables().cmap {
        for sub in cmap.subtables {
            if sub.is_unicode() {
                sub.codepoints(|cp| cps.push(cp));
            }
        }
    }
    cps.sort_unstable();
    cps.dedup();
    cps
}

/// The pixel sizes `glyph_raster_image` is asked at (strike selection and the
/// bitmap tables' own lookups).
const RASTER_PPEM: [u16; 4] = [16, 32, 64, 128];

struct OutlineDigest<'a>(&'a mut Fnv);

impl font::OutlineBuilder for OutlineDigest<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.bytes(b"M");
        self.0.f32(x);
        self.0.f32(y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.bytes(b"L");
        self.0.f32(x);
        self.0.f32(y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.bytes(b"Q");
        for v in [x1, y1, x, y] {
            self.0.f32(v);
        }
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.bytes(b"C");
        for v in [x1, y1, x2, y2, x, y] {
            self.0.f32(v);
        }
    }
    fn close(&mut self) {
        self.0.bytes(b"Z");
    }
}

/// What an outline call answers for one glyph: the glyph's outline, routed into the digest's sink,
/// and its box.
pub(crate) type Outline<'f> = dyn Fn(&font::Face<'_>, font::GlyphId, &mut dyn font::OutlineBuilder) -> Option<font::Rect>
    + 'f;

/// The `outline.<start>` answer for glyphs `start..end`: each glyph id, the segment stream
/// `outline` sends, and its box.
pub(crate) fn outline_block(
    face: &font::Face<'_>,
    start: u32,
    end: u32,
    outline: &Outline<'_>,
) -> String {
    let mut h = Fnv::new();
    let mut n = 0u32;
    for g in start..end {
        let g = g as u16;
        h.u16(g);
        let r = outline(face, font::GlyphId(g), &mut OutlineDigest(&mut h));
        n += u32::from(r.is_some());
        rect(&mut h, r);
    }
    format!("n={n} fnv={}", h.hex())
}

fn rect(h: &mut Fnv, r: Option<font::Rect>) {
    match r {
        Some(r) => {
            h.bytes(b"R");
            for v in [r.x_min, r.y_min, r.x_max, r.y_max] {
                h.i16(v);
            }
        }
        None => h.bytes(b"-"),
    }
}

/// A COLR paint graph as a digest of its painter calls, gradient stops
/// resolved against the face's current variation coordinates.
struct PaintDigest<'h> {
    h: &'h mut Fnv,
    palette: u16,
    coords: Vec<font::NormalizedCoordinate>,
}

impl PaintDigest<'_> {
    fn color(&mut self, c: font::RgbaColor) {
        self.h.bytes(&[c.red, c.green, c.blue, c.alpha]);
    }

    fn stops(&mut self, stops: impl Iterator<Item = font::colr::ColorStop>) {
        for s in stops {
            self.h.f32(s.stop_offset);
            self.color(s.color);
        }
    }

    fn extend(&mut self, e: font::colr::GradientExtend) {
        self.h.bytes(format!("{e:?}").as_bytes());
    }
}

impl<'a> font::colr::Painter<'a> for PaintDigest<'_> {
    fn outline_glyph(&mut self, glyph_id: font::GlyphId) {
        self.h.bytes(b"g");
        self.h.u16(glyph_id.0);
    }
    fn paint(&mut self, paint: font::colr::Paint<'a>) {
        use font::colr::Paint;
        let coords = self.coords.clone();
        match paint {
            Paint::Solid(c) => {
                self.h.bytes(b"s");
                self.color(c);
            }
            Paint::LinearGradient(g) => {
                self.h.bytes(b"lin");
                for v in [g.x0, g.y0, g.x1, g.y1, g.x2, g.y2] {
                    self.h.f32(v);
                }
                self.extend(g.extend);
                self.stops(g.stops(self.palette, &coords));
            }
            Paint::RadialGradient(g) => {
                self.h.bytes(b"rad");
                for v in [g.x0, g.y0, g.r0, g.r1, g.x1, g.y1] {
                    self.h.f32(v);
                }
                self.extend(g.extend);
                self.stops(g.stops(self.palette, &coords));
            }
            Paint::SweepGradient(g) => {
                self.h.bytes(b"swp");
                for v in [g.center_x, g.center_y, g.start_angle, g.end_angle] {
                    self.h.f32(v);
                }
                self.extend(g.extend);
                self.stops(g.stops(self.palette, &coords));
            }
        }
    }
    fn push_clip(&mut self) {
        self.h.bytes(b"clip");
    }
    fn push_clip_box(&mut self, clipbox: font::colr::ClipBox) {
        self.h.bytes(b"cbox");
        for v in [clipbox.x_min, clipbox.y_min, clipbox.x_max, clipbox.y_max] {
            self.h.f32(v);
        }
    }
    fn pop_clip(&mut self) {
        self.h.bytes(b"/clip");
    }
    fn push_layer(&mut self, mode: font::colr::CompositeMode) {
        self.h.bytes(format!("layer{mode:?}").as_bytes());
    }
    fn pop_layer(&mut self) {
        self.h.bytes(b"/layer");
    }
    fn push_transform(&mut self, t: font::Transform) {
        self.h.bytes(b"xf");
        for v in [t.a, t.b, t.c, t.d, t.e, t.f] {
            self.h.i32((v * 65536.0).round() as i32);
        }
    }
    fn pop_transform(&mut self) {
        self.h.bytes(b"/xf");
    }
}

fn line_metrics(m: Option<font::LineMetrics>) -> String {
    m.map_or("-".to_string(), |m| {
        format!("{},{}", m.position, m.thickness)
    })
}

fn script_metrics(m: Option<font::os2::ScriptMetrics>) -> String {
    m.map_or("-".to_string(), |m| {
        format!("{},{},{},{}", m.x_size, m.y_size, m.x_offset, m.y_offset)
    })
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or("-".to_string(), |v| v.to_string())
}

/// What ttf-parser answers for face `index` of `bytes` at `coords`, as `(query, answer)` pairs.
pub(crate) fn read(bytes: &[u8], index: u32, coords: &[([u8; 4], f32)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut face = match font::Face::parse(bytes, index) {
        Ok(face) => face,
        Err(e) => {
            out.push(("parse".to_string(), format!("error {e}")));
            return out;
        }
    };
    for &(t, v) in coords {
        let _ = face.set_variation(font::Tag::from_bytes(&t), v);
    }
    let default = coords.is_empty();
    let glyphs = face.number_of_glyphs();
    let tables = face.tables();

    if default {
        let mut tags: Vec<String> = face
            .raw_face()
            .table_records
            .into_iter()
            .map(|r| tag_str(r.tag.to_bytes()))
            .collect();
        tags.sort_unstable();
        let b = face.global_bounding_box();
        out.push((
            "head".to_string(),
            format!(
                "upem={} glyphs={glyphs} bbox={},{},{},{} variable={} faces={} italic_angle={:?} mono={} tables={}",
                face.units_per_em(),
                b.x_min,
                b.y_min,
                b.x_max,
                b.y_max,
                u8::from(face.is_variable()),
                opt(font::fonts_in_collection(bytes)),
                face.italic_angle(),
                u8::from(face.is_monospaced()),
                tags.join(",")
            ),
        ));
        out.push((
            "os2".to_string(),
            format!(
                "weight={} width={} style={:?} bold={} italic={} oblique={} regular={} perm={} subset={} outline_embed={} ranges={:032x}",
                face.weight().to_number(),
                face.width().to_number(),
                face.style(),
                u8::from(face.is_bold()),
                u8::from(face.is_italic()),
                u8::from(face.is_oblique()),
                u8::from(face.is_regular()),
                face.permissions().map_or("-".to_string(), |p| format!("{p:?}")),
                u8::from(face.is_subsetting_allowed()),
                u8::from(face.is_outline_embedding_allowed()),
                face.unicode_ranges().0
            ),
        ));
        let mut h = Fnv::new();
        let mut n = 0u32;
        let mut families: Vec<String> = Vec::new();
        for name in face.names() {
            n += 1;
            h.bytes(format!("{:?}", name.platform_id).as_bytes());
            h.u16(name.encoding_id);
            h.u16(name.language_id);
            h.u16(name.name_id);
            h.bytes(name.name);
            if name.name_id == font::name_id::FAMILY
                && let Some(s) = name.to_string()
                && !families.contains(&s)
            {
                families.push(s);
            }
        }
        let families: Vec<String> = families.iter().map(|s| escape(s)).collect();
        out.push((
            "name".to_string(),
            pin(format!(
                "n={n} fnv={} family={}",
                h.hex(),
                families.join("|")
            )),
        ));
        let axes: Vec<String> = face
            .variation_axes()
            .into_iter()
            .map(|a| {
                format!(
                    "{}:{:?},{:?},{:?}{}",
                    tag_str(a.tag.to_bytes()),
                    a.min_value,
                    a.def_value,
                    a.max_value,
                    if a.hidden { ",hidden" } else { "" }
                )
            })
            .collect();
        if !axes.is_empty() {
            out.push(("fvar".to_string(), axes.join(" ")));
        }

        if let Some(cmap) = tables.cmap {
            for (i, sub) in cmap.subtables.into_iter().enumerate() {
                use font::cmap::Format;
                let format = match sub.format {
                    Format::ByteEncodingTable(_) => 0,
                    Format::HighByteMappingThroughTable(_) => 2,
                    Format::SegmentMappingToDeltaValues(_) => 4,
                    Format::TrimmedTableMapping(_) => 6,
                    Format::MixedCoverage => 8,
                    Format::TrimmedArray(_) => 10,
                    Format::SegmentedCoverage(_) => 12,
                    Format::ManyToOneRangeMappings(_) => 13,
                    Format::UnicodeVariationSequences(_) => 14,
                };
                let mut cps = Vec::new();
                sub.codepoints(|cp| cps.push(cp));
                let mut h = Fnv::new();
                for &cp in &cps {
                    h.u32(cp);
                    h.u32(sub.glyph_index(cp).map_or(u32::MAX, |g| u32::from(g.0)));
                }
                out.push((
                    format!("cmap.sub{i}"),
                    format!(
                        "p={:?} e={} f={format} unicode={} n={} fnv={}",
                        sub.platform_id,
                        sub.encoding_id,
                        u8::from(sub.is_unicode()),
                        cps.len(),
                        h.hex()
                    ),
                ));
            }
        }
        let mut cps = unicode_coverage(&face);
        cps.extend(probe_chars().into_iter().map(u32::from));
        cps.sort_unstable();
        cps.dedup();
        let (mut h, mut hv) = (Fnv::new(), Fnv::new());
        let (mut mapped, mut varied) = (0u32, 0u32);
        for &cp in &cps {
            let Some(ch) = char::from_u32(cp) else {
                continue;
            };
            let g = face.glyph_index(ch);
            mapped += u32::from(g.is_some());
            h.u32(cp);
            h.u32(g.map_or(u32::MAX, |g| u32::from(g.0)));
            for vs in ['\u{FE0E}', '\u{FE0F}', '\u{E0100}'] {
                if let Some(g) = face.glyph_variation_index(ch, vs) {
                    varied += 1;
                    hv.u32(cp);
                    hv.u32(u32::from(vs));
                    hv.u16(g.0);
                }
            }
        }
        out.push((
            "cmap.index".to_string(),
            format!("probed={} mapped={mapped} fnv={}", cps.len(), h.hex()),
        ));
        out.push((
            "cmap.vs".to_string(),
            format!("n={varied} fnv={}", hv.hex()),
        ));

        if let Some(kern) = tables.kern {
            let gids = latin_gids(&face);
            for (i, sub) in kern.subtables.into_iter().enumerate() {
                let mut h = Fnv::new();
                let mut n = 0u32;
                for &l in &gids {
                    for &r in &gids {
                        if let Some(v) = sub.glyphs_kerning(font::GlyphId(l), font::GlyphId(r)) {
                            n += 1;
                            h.u16(l);
                            h.u16(r);
                            h.i16(v);
                        }
                    }
                }
                let format = match sub.format {
                    font::kern::Format::Format0(_) => "0",
                    font::kern::Format::Format2(_) => "2",
                    font::kern::Format::Format3(_) => "3",
                    _ => "1",
                };
                out.push((
                    format!("kern.{i}"),
                    format!(
                        "h={} v={} cross={} sm={} f={format} glyphs={} pairs={n} fnv={}",
                        u8::from(sub.horizontal),
                        u8::from(sub.variable),
                        u8::from(sub.has_cross_stream),
                        u8::from(sub.has_state_machine),
                        gids.len(),
                        h.hex()
                    ),
                ));
            }
        }
        out.push(("gsub".to_string(), layout_digest::layout(tables.gsub)));
        out.push((
            "gsub.lookups".to_string(),
            layout_digest::gsub_lookups(tables.gsub),
        ));
        out.push(("gpos".to_string(), layout_digest::layout(tables.gpos)));
        if let Some(gdef) = tables.gdef {
            let mut h = Fnv::new();
            for g in 0..glyphs {
                let id = font::GlyphId(g);
                h.u16(gdef.glyph_class(id).map_or(0, |c| c as u16));
                h.u16(gdef.glyph_mark_attachment_class(id));
                h.bytes(&[u8::from(gdef.is_mark_glyph(id, None))]);
            }
            out.push((
                "gdef".to_string(),
                format!(
                    "classes={} fnv={}",
                    u8::from(gdef.has_glyph_classes()),
                    h.hex()
                ),
            ));
        }

        if let Some(sbix) = tables.sbix {
            for i in 0..sbix.strikes.len() {
                let Some(strike) = sbix.strikes.get(i) else {
                    continue;
                };
                let mut h = Fnv::new();
                let mut n = 0u32;
                for g in 0..glyphs {
                    if let Some(img) = strike.get(font::GlyphId(g)) {
                        n += 1;
                        raster_image(&mut h, g, &img);
                    }
                }
                out.push((
                    format!("sbix.strike{i}"),
                    format!(
                        "ppem={} ppi={} n={n} fnv={}",
                        strike.pixels_per_em,
                        strike.ppi,
                        h.hex()
                    ),
                ));
            }
        }
        if tables.sbix.is_some()
            || tables.bdat.is_some()
            || tables.ebdt.is_some()
            || tables.cbdt.is_some()
        {
            for ppem in RASTER_PPEM {
                let mut h = Fnv::new();
                let mut n = 0u32;
                for g in 0..glyphs {
                    if let Some(img) = face.glyph_raster_image(font::GlyphId(g), ppem) {
                        n += 1;
                        raster_image(&mut h, g, &img);
                    }
                }
                out.push((format!("raster.{ppem}"), format!("n={n} fnv={}", h.hex())));
            }
        }
        if let Some(p) = face.color_palettes() {
            out.push(("cpal".to_string(), format!("palettes={p}")));
        }
    }

    let b = |v: i16| v.to_string();
    out.push((
        "metrics".to_string(),
        format!(
            "asc={} desc={} gap={} height={} typo={},{},{} vert={},{},{},{} x={} cap={} ul={} st={} sub={} sup={}",
            b(face.ascender()),
            b(face.descender()),
            b(face.line_gap()),
            b(face.height()),
            opt(face.typographic_ascender()),
            opt(face.typographic_descender()),
            opt(face.typographic_line_gap()),
            opt(face.vertical_ascender()),
            opt(face.vertical_descender()),
            opt(face.vertical_line_gap()),
            opt(face.vertical_height()),
            opt(face.x_height()),
            opt(face.capital_height()),
            line_metrics(face.underline_metrics()),
            line_metrics(face.strikeout_metrics()),
            script_metrics(face.subscript_metrics()),
            script_metrics(face.superscript_metrics())
        ),
    ));
    if face.is_variable() {
        let c: Vec<String> = face
            .variation_coordinates()
            .iter()
            .map(|c| c.get().to_string())
            .collect();
        out.push(("coords".to_string(), c.join(",")));
    }
    let (mut hh, mut hv, mut hb) = (Fnv::new(), Fnv::new(), Fnv::new());
    let mut boxed = 0u32;
    for g in 0..glyphs {
        let id = font::GlyphId(g);
        hh.u16(face.glyph_hor_advance(id).unwrap_or(u16::MAX));
        hh.i16(face.glyph_hor_side_bearing(id).unwrap_or(i16::MIN));
        hv.u16(face.glyph_ver_advance(id).unwrap_or(u16::MAX));
        hv.i16(face.glyph_ver_side_bearing(id).unwrap_or(i16::MIN));
        hv.i16(face.glyph_y_origin(id).unwrap_or(i16::MIN));
        let bbox = face.glyph_bounding_box(id);
        boxed += u32::from(bbox.is_some());
        rect(&mut hb, bbox);
    }
    out.push(("hmtx".to_string(), format!("n={glyphs} fnv={}", hh.hex())));
    out.push(("vmtx".to_string(), format!("n={glyphs} fnv={}", hv.hex())));
    out.push(("bbox".to_string(), format!("n={boxed} fnv={}", hb.hex())));
    let mut start = 0u32;
    while start < u32::from(glyphs) {
        let end = (start + BLOCK).min(u32::from(glyphs));
        out.push((
            format!("outline.{start}"),
            outline_block(&face, start, end, &|face, g, sink| {
                face.outline_glyph(g, sink)
            }),
        ));
        start = end;
    }
    if let Some(palettes) = face.color_palettes() {
        let coords_now: Vec<font::NormalizedCoordinate> = face.variation_coordinates().to_vec();
        let last = palettes.get() - 1;
        let mut start = 0u32;
        while start < u32::from(glyphs) {
            let end = (start + BLOCK).min(u32::from(glyphs));
            let mut h = Fnv::new();
            let mut n = 0u32;
            for g in start..end {
                let id = font::GlyphId(g as u16);
                if !face.is_color_glyph(id) {
                    continue;
                }
                n += 1;
                h.u16(id.0);
                if let Some(colr) = tables.colr {
                    let clip = colr.clip_box(id, &coords_now);
                    match clip {
                        Some(c) => {
                            for v in [c.x_min, c.y_min, c.x_max, c.y_max] {
                                h.f32(v);
                            }
                        }
                        None => h.bytes(b"-"),
                    }
                }
                for palette in [0, last] {
                    let fg = font::RgbaColor::new(0, 0, 0, 255);
                    let mut p = PaintDigest {
                        h: &mut h,
                        palette,
                        coords: coords_now.clone(),
                    };
                    let r = face.paint_color_glyph(id, palette, fg, &mut p);
                    h.bytes(if r.is_some() { b"+" } else { b"!" });
                }
            }
            if n > 0 {
                out.push((format!("colr.{start}"), format!("n={n} fnv={}", h.hex())));
            }
            start = end;
        }
    }
    out
}

fn raster_image(h: &mut Fnv, gid: u16, img: &font::RasterGlyphImage) {
    h.u16(gid);
    h.i16(img.x);
    h.i16(img.y);
    h.u16(img.width);
    h.u16(img.height);
    h.u16(img.pixels_per_em);
    h.bytes(format!("{:?}", img.format).as_bytes());
    h.u32(img.data.len() as u32);
    h.bytes(img.data);
}
