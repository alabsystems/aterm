// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A font is untrusted input.

use aterm_font::colr::{ClipBox, CompositeMode, Paint, Painter};
use aterm_font::{Face, GlyphId, OutlineSink, PointKind, PointSink, RgbaColor, Tag, Transform};

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { self.next() as usize % n }
    }
}

/// Counts what it is sent.
#[derive(Default)]
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
        self.0 += 1;
        let stops = match paint {
            Paint::Solid(_) => 0,
            Paint::LinearGradient(g) => g.stops(0, &[]).count(),
            Paint::RadialGradient(g) => g.stops(0, &[]).count(),
            Paint::SweepGradient(g) => g.stops(0, &[]).count(),
        };
        self.0 += stops as u64;
    }
    fn push_clip(&mut self) {
        self.0 += 1;
    }
    fn push_clip_box(&mut self, _: ClipBox) {
        self.0 += 1;
    }
    fn pop_clip(&mut self) {
        self.0 += 1;
    }
    fn push_layer(&mut self, _: CompositeMode) {
        self.0 += 1;
    }
    fn pop_layer(&mut self) {
        self.0 += 1;
    }
    fn push_transform(&mut self, _: Transform) {
        self.0 += 1;
    }
    fn pop_transform(&mut self) {
        self.0 += 1;
    }
}

fn synthetic_cff2(programs: &[&[u8]]) -> Vec<u8> {
    let be16 = |v: usize| (v as u16).to_be_bytes();
    let be32 = |v: usize| (v as u32).to_be_bytes();
    let top_len = 12;
    let globals = 5 + top_len;
    let vstore = globals + 4;
    let vstore_bytes: Vec<u8> = [
        &be16(2 + 2 + 4 + 2 + 4 + 4 + 6)[..],
        &be16(1),
        &be32(12),
        &be16(1),
        &be32(16),
        &[0, 0, 0, 0],
        &[0, 0, 0, 0, 0, 0],
    ]
    .concat();
    let char_strings = vstore + vstore_bytes.len();
    let mut cff = vec![2, 0, 5];
    cff.extend(be16(top_len));
    cff.push(29);
    cff.extend(be32(char_strings));
    cff.push(17);
    cff.push(29);
    cff.extend(be32(vstore));
    cff.push(24);
    cff.extend(be32(0));
    cff.extend(&vstore_bytes);
    cff.extend(be32(programs.len()));
    cff.push(2);
    let mut at = 1;
    cff.extend(be16(at));
    for p in programs {
        at += p.len();
        cff.extend(be16(at));
    }
    for p in programs {
        cff.extend_from_slice(p);
    }
    sfnt(programs.len(), &[(b"CFF2", &cff)])
}

/// An sfnt of `glyphs` glyphs holding `extra` plus a minimal `head`, `hhea` and `maxp`.
pub(crate) fn sfnt(glyphs: usize, extra: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let be16 = |v: usize| (v as u16).to_be_bytes();
    let be32 = |v: usize| (v as u32).to_be_bytes();
    let mut head = vec![0u8; 54];
    head[18..20].copy_from_slice(&be16(1000));
    let mut hhea = vec![0u8; 36];
    hhea[34..36].copy_from_slice(&be16(1));
    let mut maxp = vec![0, 0, 0x50, 0];
    maxp.extend(be16(glyphs));
    let mut tables: Vec<(&[u8; 4], &[u8])> = extra.to_vec();
    tables.extend([
        (b"head", head.as_slice()),
        (b"hhea", hhea.as_slice()),
        (b"maxp", maxp.as_slice()),
    ]);
    tables.sort_by_key(|(tag, _)| **tag);
    let mut font = vec![0x4F, 0x54, 0x54, 0x4F];
    font.extend(be16(tables.len()));
    font.extend([0u8; 6]);
    let mut offset = 12 + 16 * tables.len();
    for (tag, data) in &tables {
        font.extend(*tag);
        font.extend([0u8; 4]);
        font.extend(be32(offset));
        font.extend(be32(data.len()));
        offset += data.len().next_multiple_of(4);
    }
    for (_, data) in &tables {
        font.extend_from_slice(data);
        font.resize(font.len().next_multiple_of(4), 0);
    }
    font
}

#[test]
fn a_gsub_extension_of_an_extension_is_malformed() {
    let mut gsub: Vec<u8> = vec![0, 1, 0, 0, 0, 10, 0, 12, 0, 14];
    gsub.extend([0, 0, 0, 0]);
    gsub.extend([0, 2, 0, 6, 0, 22]);
    gsub.extend([0, 7, 0, 0, 0, 1, 0, 8]);
    gsub.extend([0, 1, 0, 7, 0, 0, 0, 0]);
    gsub.extend([0, 7, 0, 0, 0, 1, 0, 8]);
    gsub.extend([0, 1, 0, 1, 0, 0, 0, 8]);
    gsub.extend([0, 1, 0, 6, 0, 1]);
    gsub.extend([0, 1, 0, 1, 0, 0]);
    let bytes = sfnt(2, &[(b"GSUB", &gsub)]);
    let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
    let lookups = face.tables().gsub.expect("GSUB parses").lookups;
    let resolve = |i: u16| {
        lookups
            .get(i)
            .expect("lookup")
            .subtables
            .get::<aterm_font::gsub::SubstitutionSubtable>(0)
    };
    assert!(resolve(0).is_none(), "an extension of an extension");
    assert!(
        matches!(
            resolve(1),
            Some(aterm_font::gsub::SubstitutionSubtable::Single(
                aterm_font::gsub::SingleSubstitution::Format1 { delta: 1, .. }
            ))
        ),
        "a well-formed extension resolves"
    );
}

#[test]
fn a_gpos_extension_of_an_extension_is_malformed() {
    let mut gpos: Vec<u8> = vec![0, 1, 0, 0, 0, 10, 0, 12, 0, 14];
    gpos.extend([0, 0, 0, 0]);
    gpos.extend([0, 2, 0, 6, 0, 22]);
    // Lookup 0: LookupType 9 (Extension) wrapping LookupType 9 -> malformed.
    gpos.extend([0, 9, 0, 0, 0, 1, 0, 8]);
    gpos.extend([0, 1, 0, 9, 0, 0, 0, 0]);
    // Lookup 1: LookupType 9 (Extension) wrapping LookupType 1 (SinglePos Format 1, x_advance=-50).
    gpos.extend([0, 9, 0, 0, 0, 1, 0, 8]);
    gpos.extend([0, 1, 0, 1, 0, 0, 0, 8]);
    gpos.extend([0, 1, 0, 8, 0, 4]);
    gpos.extend((-50i16).to_be_bytes());
    gpos.extend([0, 1, 0, 1, 0, 0]);
    let bytes = sfnt(2, &[(b"GPOS", &gpos)]);
    let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
    let lookups = face.tables().gpos.expect("GPOS parses").lookups;
    let resolve = |i: u16| {
        lookups
            .get(i)
            .expect("lookup")
            .subtables
            .get::<aterm_font::gpos::PositioningSubtable>(0)
    };
    assert!(resolve(0).is_none(), "an extension of an extension");
    assert!(
        matches!(
            resolve(1),
            Some(aterm_font::gpos::PositioningSubtable::Single(
                aterm_font::gpos::SingleAdjustment::Format1 { value, .. }
            )) if value.x_advance == -50
        ),
        "a well-formed GPOS extension resolves"
    );
}

fn shared_paint_graph(depth: u32) -> Vec<u8> {
    paint_dag(depth, &[2, 0, 0, 0x40, 0], 0)
}

/// Glyph 1 as a `depth`-level `COLR` v1 graph whose paints share children
/// (2^`depth` paths) down to `leaf`'s paint table; with `v0_layers`, glyph 2
/// is a `COLR` v0 glyph of that many layers.
fn paint_dag(depth: u32, leaf: &[u8], v0_layers: u32) -> Vec<u8> {
    paint_dag_over(depth, leaf, v0_layers, &[])
}

/// [`paint_dag`] with `extra` tables.
fn paint_dag_over(depth: u32, leaf: &[u8], v0_layers: u32, extra: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let be16 = |v: u32| (v as u16).to_be_bytes();
    let be32 = |v: u32| v.to_be_bytes();
    let base_list = 34u32;
    let layer_list = base_list + 10;
    let paints = layer_list + 4 + 8 * depth;
    let v0 = match v0_layers {
        0 => 0,
        _ => paints + 6 * depth + leaf.len() as u32,
    };
    let mut colr: Vec<u8> = Vec::new();
    colr.extend(be16(1));
    colr.extend(be16(u32::from(v0_layers > 0)));
    colr.extend(be32(v0));
    colr.extend(be32(if v0 == 0 { 0 } else { v0 + 6 }));
    colr.extend(be16(v0_layers));
    colr.extend(be32(base_list));
    colr.extend(be32(layer_list));
    colr.extend(be32(0));
    colr.extend(be32(0));
    colr.extend(be32(0));
    colr.extend(be32(1));
    colr.extend(be16(1));
    colr.extend(be32(paints - base_list));
    colr.extend(be32(2 * depth));
    for i in 0..depth {
        let next = paints + 6 * (i + 1) - layer_list;
        colr.extend(be32(next));
        colr.extend(be32(next));
    }
    for i in 0..depth {
        colr.push(1);
        colr.push(2);
        colr.extend(be32(2 * i));
    }
    colr.extend(leaf);
    if v0_layers > 0 {
        colr.extend([0, 2, 0, 0]);
        colr.extend(be16(v0_layers));
        for _ in 0..v0_layers {
            colr.extend([0, 1, 0, 0]);
        }
    }
    let mut cpal: Vec<u8> = Vec::new();
    for v in [0u16, 1, 1, 1] {
        cpal.extend(v.to_be_bytes());
    }
    cpal.extend(be32(14));
    cpal.extend(be16(0));
    cpal.extend([0x10, 0x20, 0x30, 0xFF]);
    let mut tables: Vec<(&[u8; 4], &[u8])> = vec![(b"COLR", &colr), (b"CPAL", &cpal)];
    tables.extend(extra);
    sfnt(3, &tables)
}

/// `glyf` and `loca` where glyph 2 is `contours` contours of one on-curve
/// point each, and glyphs 0 and 1 are empty: each contour is a move, a line
/// back and a close, three segments for one outline step.
fn glyf_dots(contours: u16) -> (Vec<u8>, Vec<u8>) {
    let mut glyf: Vec<u8> = Vec::new();
    glyf.extend(contours.to_be_bytes());
    for v in [0, 0, contours, contours] {
        glyf.extend(v.to_be_bytes());
    }
    for end in 0..contours {
        glyf.extend(end.to_be_bytes());
    }
    glyf.extend([0, 0]);
    // On curve, each coordinate one positive byte.
    glyf.extend(std::iter::repeat_n(0x37, usize::from(contours)));
    glyf.extend(std::iter::repeat_n(1, 2 * usize::from(contours)));
    glyf.resize(glyf.len().next_multiple_of(2), 0);
    let end = (glyf.len() / 2) as u16;
    let loca = [0u16, 0, 0, end]
        .iter()
        .flat_map(|v| v.to_be_bytes())
        .collect();
    (glyf, loca)
}

#[test]
fn a_colr_graph_that_shares_children_is_walked_within_a_budget() {
    let fg = RgbaColor::new(0, 0, 0, 255);
    let painted = |depth: u32| {
        let bytes = shared_paint_graph(depth);
        let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
        assert!(face.is_color_glyph(GlyphId(1)), "glyph 1 is a colour glyph");
        let mut count = Count::default();
        let painted = face.paint_color_glyph(GlyphId(1), 0, fg, &mut count);
        (painted, count.0)
    };
    let _ = aterm_font::take_spent();
    assert_eq!(
        painted(8),
        (Some(()), 256),
        "a small shared graph paints every path"
    );
    assert_eq!(aterm_font::take_spent().paint_tables, (1 << 9) - 1);
    // The budget stops the deep walk, which answers that it painted no glyph.
    let (whole, deep) = painted(60);
    assert_eq!(whole, None);
    assert!(
        (1..=1 << 16).contains(&deep),
        "the deep graph painted {deep} times"
    );
    let walked = aterm_font::take_spent().paint_tables;
    assert!(walked > 1 << 16, "the capped walk read {walked}");
}

/// A `PaintLinearGradient` whose `ColorLine` (at +16) holds `stops` stops.
fn gradient_leaf(stops: u32) -> Vec<u8> {
    let mut leaf = vec![4, 0, 0, 16];
    for v in [0i16, 0, 100, 0, 0, 100] {
        leaf.extend(v.to_be_bytes());
    }
    leaf.push(0);
    leaf.extend((stops as u16).to_be_bytes());
    for i in 0..stops {
        leaf.extend(((i * 16_384 / stops) as u16).to_be_bytes());
        leaf.extend([0, 0, 0x40, 0]);
    }
    leaf
}

/// The colour stops and outlines a paint walk hands its painter. It reads
/// stops only up to one past [`HANDED`], so a walk that hands out more fails
/// at once.
#[derive(Default)]
struct Handed {
    stops: usize,
    outlines: usize,
}

const HANDED: usize = 1 << 16;

impl<'a> Painter<'a> for Handed {
    fn outline_glyph(&mut self, _: GlyphId) {
        self.outlines += 1;
    }
    fn paint(&mut self, paint: Paint<'a>) {
        let left = (HANDED + 1).saturating_sub(self.stops);
        self.stops += match paint {
            Paint::Solid(_) => 0,
            Paint::LinearGradient(g) => g.stops(0, &[]).take(left).count(),
            Paint::RadialGradient(g) => g.stops(0, &[]).take(left).count(),
            Paint::SweepGradient(g) => g.stops(0, &[]).take(left).count(),
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

#[test]
fn a_colr_walk_charges_the_stops_and_outlines_it_hands_out_to_its_budget() {
    // A walk the budget stops answers None; only the small graph paints whole.
    let handed = |depth: u32, leaf: &[u8], v0_layers: u32| {
        let bytes = paint_dag(depth, leaf, v0_layers);
        let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
        let mut handed = Handed::default();
        let painted =
            face.paint_color_glyph(GlyphId(1), 0, RgbaColor::new(0, 0, 0, 255), &mut handed);
        assert_eq!(painted.is_some(), depth < 16, "a {depth}-level walk");
        handed
    };
    let small = handed(4, &gradient_leaf(4), 0);
    assert_eq!(
        (small.stops, small.outlines),
        (64, 0),
        "a small graph paints whole"
    );
    // Each of the walk's 2^15 gradients was 4,096 stops: 2^27 handed out.
    let stops = handed(30, &gradient_leaf(4_096), 0).stops;
    assert!(
        (4_096..=HANDED).contains(&stops),
        "the walk handed out {stops} stops"
    );
    // A PaintGlyph over a solid: 21,845 outlines under the table count alone.
    let glyphs = handed(30, &[10, 0, 0, 6, 0, 1, 2, 0, 0, 0x40, 0], 0).outlines;
    assert!(
        (1..=1 << 14).contains(&glyphs),
        "the walk handed out {glyphs} PaintGlyph outlines"
    );
    // A PaintColrGlyph onto a v0 glyph of 1,024 layers: 2^15 of them, 2^25 outlines.
    let layers = handed(30, &[11, 0, 2], 1_024).outlines;
    assert!(
        (1_024..=1 << 14).contains(&layers),
        "the walk handed out {layers} v0 layer outlines"
    );
    // A PaintGlyph of 4,096 one-point contours, 12,288 segments for 4,097
    // outline steps: the step budget alone lets 63 through, 774,144 segments.
    let (glyf, loca) = glyf_dots(4_096);
    let leaf = [10, 0, 0, 6, 0, 2, 2, 0, 0, 0x40, 0];
    let bytes = paint_dag_over(30, &leaf, 0, &[(b"glyf", &glyf), (b"loca", &loca)]);
    let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
    let mut segments = Count::default();
    let _ = aterm_font::take_spent();
    assert!(face.outline_glyph(GlyphId(2), &mut segments).is_some());
    let steps = aterm_font::take_spent().outline_steps;
    assert_eq!((steps, segments.0), (4_097, 12_288), "glyph 2");
    let mut handed = Handed::default();
    assert_eq!(
        face.paint_color_glyph(GlyphId(1), 0, RgbaColor::new(0, 0, 0, 255), &mut handed),
        None
    );
    let segments = handed.outlines as u64 * segments.0;
    assert!(
        (1..=1 << 18).contains(&segments),
        "the walk handed out {} outlines, {segments} segments",
        handed.outlines
    );
}

/// `glyf` and `loca` where glyph 0 has no contours, glyph 1 is `fan`
/// copies of glyph 0 and glyph 2 `fan` copies of glyph 1: outlining glyph 2
/// takes `1 + fan * (1 + fan)` steps and draws nothing.
fn glyf_composite_fan(fan: u16) -> (Vec<u8>, Vec<u8>) {
    let mut glyf: Vec<u8> = vec![0; 10];
    let mut loca = vec![0u16, 5];
    for child in [0u16, 1] {
        glyf.extend((-1i16).to_be_bytes());
        glyf.extend([0; 8]);
        for i in 0..fan {
            // ARGS_ARE_XY_VALUES, and MORE_COMPONENTS on all but the last.
            let flags: u16 = if i + 1 < fan { 0x22 } else { 0x02 };
            glyf.extend(flags.to_be_bytes());
            glyf.extend(child.to_be_bytes());
            glyf.extend([0, 0]);
        }
        loca.push((glyf.len() / 2) as u16);
    }
    (glyf, loca.iter().flat_map(|v| v.to_be_bytes()).collect())
}

#[test]
fn a_colr_walk_charges_what_outlining_each_glyph_costs() {
    // A PaintGlyph of a glyph that takes 4,033 outline steps and has no
    // segment: the count of segments alone let 9,359 through, each outlined
    // by the walk and again by the painter.
    let (glyf, loca) = glyf_composite_fan(63);
    let leaf = [10, 0, 0, 6, 0, 2, 2, 0, 0, 0x40, 0];
    let bytes = paint_dag_over(30, &leaf, 0, &[(b"glyf", &glyf), (b"loca", &loca)]);
    let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
    let mut segments = Count::default();
    let _ = aterm_font::take_spent();
    let _ = face.outline_glyph(GlyphId(2), &mut segments);
    let one = aterm_font::take_spent().outline_steps;
    assert_eq!(
        (one, segments.0),
        (4_033, 0),
        "glyph 2's steps and segments"
    );
    let mut handed = Handed::default();
    assert_eq!(
        face.paint_color_glyph(GlyphId(1), 0, RgbaColor::new(0, 0, 0, 255), &mut handed),
        None
    );
    let steps = aterm_font::take_spent().outline_steps;
    assert!(
        handed.outlines > 0 && steps <= (1 << 18) + one,
        "the walk handed out {} outlines and outlined {steps} steps",
        handed.outlines
    );
}

/// Glyph 1 as a `depth`-level graph whose paints share children down to a
/// PaintColrGlyph of glyph 5000, a solid, under a `ClipList` of `records`
/// one-glyph records from glyph 10 and, last, one for glyph 5000.
fn clipped_colr_glyph_dag(depth: u32, records: u32) -> Vec<u8> {
    let be16 = |v: u32| (v as u16).to_be_bytes();
    let be32 = |v: u32| v.to_be_bytes();
    let base_list = 34u32;
    let layer_list = base_list + 4 + 12;
    let paints = layer_list + 4 + 8 * depth;
    let solid = paints + 6 * depth + 3;
    let clip_list = solid + 5;
    let mut colr: Vec<u8> = Vec::new();
    colr.extend(be16(1));
    colr.extend(be16(0));
    colr.extend(be32(0));
    colr.extend(be32(0));
    colr.extend(be16(0));
    colr.extend(be32(base_list));
    colr.extend(be32(layer_list));
    colr.extend(be32(clip_list));
    colr.extend(be32(0));
    colr.extend(be32(0));
    colr.extend(be32(2));
    colr.extend(be16(1));
    colr.extend(be32(paints - base_list));
    colr.extend(be16(5000));
    colr.extend(be32(solid - base_list));
    colr.extend(be32(2 * depth));
    for i in 0..depth {
        let next = paints + 6 * (i + 1) - layer_list;
        colr.extend(be32(next));
        colr.extend(be32(next));
    }
    for i in 0..depth {
        colr.push(1);
        colr.push(2);
        colr.extend(be32(2 * i));
    }
    colr.push(11);
    colr.extend(be16(5000));
    colr.extend([2, 0, 0, 0x40, 0]);
    colr.push(1);
    colr.extend(be32(records + 1));
    let clip_box = 5 + 7 * (records + 1);
    for glyph in (10..10 + records).chain([5000]) {
        colr.extend(be16(glyph));
        colr.extend(be16(glyph));
        colr.extend(&be32(clip_box)[1..]);
    }
    colr.push(1);
    for v in [-5i16, -6, 700, 800] {
        colr.extend(v.to_be_bytes());
    }
    let mut cpal: Vec<u8> = Vec::new();
    for v in [0u16, 1, 1, 1] {
        cpal.extend(v.to_be_bytes());
    }
    cpal.extend(be32(14));
    cpal.extend(be16(0));
    cpal.extend([0x10, 0x20, 0x30, 0xFF]);
    sfnt(2, &[(b"COLR", &colr), (b"CPAL", &cpal)])
}

/// The clip boxes a paint walk pushes.
#[derive(Default)]
struct Clips(Vec<[f32; 4]>);

impl<'a> Painter<'a> for Clips {
    fn outline_glyph(&mut self, _: GlyphId) {}
    fn paint(&mut self, _: Paint<'a>) {}
    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, b: ClipBox) {
        self.0.push([b.x_min, b.y_min, b.x_max, b.y_max]);
    }
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _: CompositeMode) {}
    fn pop_layer(&mut self) {}
    fn push_transform(&mut self, _: Transform) {}
    fn pop_transform(&mut self) {}
}

#[test]
fn a_colr_walk_finds_each_clip_box_by_bisection() {
    let bytes = clipped_colr_glyph_dag(30, 4_096);
    let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
    let _ = aterm_font::take_spent();
    let mut clips = Clips::default();
    assert_eq!(
        face.paint_color_glyph(GlyphId(1), 0, RgbaColor::new(0, 0, 0, 255), &mut clips),
        None
    );
    let read = aterm_font::take_spent().clip_records;
    assert!(
        clips.0.len() > 1 << 12,
        "the walk reached glyph 5000 {} times",
        clips.0.len()
    );
    assert!(clips.0.iter().all(|&b| b == [-5.0, -6.0, 700.0, 800.0]));
    // Glyph 1 has no clip record, and each lookup bisects 4,097 records.
    let lookups = clips.0.len() as u64 + 1;
    assert!(
        read <= 14 * lookups,
        "{lookups} clip box lookups read {read} records"
    );
}

/// Glyph 1 as a `depth`-level `COLR` v1 graph whose paints share children
/// down to `leaf`, in a face of one axis whose `COLR` item variation store
/// holds one delta set of `regions` region indices, every variation index
/// mapped to it; glyph 2 is a solid under a clip box that varies.
fn variable_paint_dag(depth: u32, leaf: &[u8], regions: u16) -> Vec<u8> {
    let be16 = |v: u32| (v as u16).to_be_bytes();
    let be32 = |v: u32| v.to_be_bytes();
    let base_list = 34u32;
    let layer_list = base_list + 4 + 12;
    let paints = layer_list + 4 + 8 * depth;
    let solid = paints + 6 * depth + leaf.len() as u32;
    let clip_list = solid + 5;
    let map = clip_list + 12 + 13;
    let store = map + 5;
    let mut colr: Vec<u8> = Vec::new();
    colr.extend(be16(1));
    colr.extend(be16(0));
    colr.extend(be32(0));
    colr.extend(be32(0));
    colr.extend(be16(0));
    for offset in [base_list, layer_list, clip_list, map, store] {
        colr.extend(be32(offset));
    }
    colr.extend(be32(2));
    colr.extend(be16(1));
    colr.extend(be32(paints - base_list));
    colr.extend(be16(2));
    colr.extend(be32(solid - base_list));
    colr.extend(be32(2 * depth));
    for i in 0..depth {
        let next = paints + 6 * (i + 1) - layer_list;
        colr.extend(be32(next));
        colr.extend(be32(next));
    }
    for i in 0..depth {
        colr.push(1);
        colr.push(2);
        colr.extend(be32(2 * i));
    }
    colr.extend(leaf);
    colr.extend([2, 0, 0, 0x40, 0]);
    // One record, glyph 2's: a format 2 box whose deltas start at index 0.
    colr.push(1);
    colr.extend(be32(1));
    colr.extend(be16(2));
    colr.extend(be16(2));
    colr.extend(&be32(12)[1..]);
    colr.push(2);
    for v in [-5i16, -6, 700, 800] {
        colr.extend(v.to_be_bytes());
    }
    colr.extend(be32(0));
    // A DeltaSetIndexMap of one 1-byte entry, (0, 0).
    colr.extend([0, 0, 0, 1, 0]);
    colr.extend(be16(1));
    colr.extend(be32(12));
    colr.extend(be16(1));
    colr.extend(be32(22));
    // One region of one axis, peaking at its maximum.
    colr.extend(be16(1));
    colr.extend(be16(1));
    for v in [0i16, 0x4000, 0x4000] {
        colr.extend(v.to_be_bytes());
    }
    // One item of byte deltas, `regions` of them, each naming region 0.
    colr.extend(be16(1));
    colr.extend(be16(0));
    colr.extend(be16(u32::from(regions)));
    colr.extend(vec![0u8; 2 * usize::from(regions)]);
    colr.extend(vec![1u8; usize::from(regions)]);
    let mut cpal: Vec<u8> = Vec::new();
    for v in [0u16, 1, 1, 1] {
        cpal.extend(v.to_be_bytes());
    }
    cpal.extend(be32(14));
    cpal.extend(be16(0));
    cpal.extend([0x10, 0x20, 0x30, 0xFF]);
    let mut fvar: Vec<u8> = Vec::new();
    for v in [1u16, 0, 16, 2, 1, 20, 0, 8] {
        fvar.extend(v.to_be_bytes());
    }
    fvar.extend(*b"wght");
    for v in [100i32, 400, 900] {
        fvar.extend((v << 16).to_be_bytes());
    }
    fvar.extend(be16(0));
    fvar.extend(be16(256));
    sfnt(3, &[(b"COLR", &colr), (b"CPAL", &cpal), (b"fvar", &fvar)])
}

/// A PaintVarTransform (six deltas from index 0) over a solid.
fn var_transform_leaf() -> Vec<u8> {
    let mut leaf = vec![13, 0, 0, 7, 0, 0, 12, 2, 0, 0, 0x40, 0];
    for v in [1i32 << 16, 0, 0, 1 << 16, 0, 0] {
        leaf.extend(v.to_be_bytes());
    }
    leaf.extend(0u32.to_be_bytes());
    leaf
}

/// A PaintVarLinearGradient that does not vary, whose `VarColorLine` (at
/// +20) holds `stops` stops, each two deltas from index 0.
fn var_gradient_leaf(stops: u32) -> Vec<u8> {
    let mut leaf = vec![5, 0, 0, 20];
    for v in [0i16, 0, 100, 0, 0, 100] {
        leaf.extend(v.to_be_bytes());
    }
    leaf.extend(u32::MAX.to_be_bytes());
    leaf.push(0);
    leaf.extend((stops as u16).to_be_bytes());
    for i in 0..stops {
        leaf.extend(((i * 16_384 / stops) as u16).to_be_bytes());
        leaf.extend([0, 0, 0x40, 0]);
        leaf.extend(0u32.to_be_bytes());
    }
    leaf
}

/// Takes each linear gradient's stops once, at the face's instance.
struct AtInstance<'c>(&'c [aterm_font::NormalizedCoordinate]);

impl<'a> Painter<'a> for AtInstance<'_> {
    fn outline_glyph(&mut self, _: GlyphId) {}
    fn paint(&mut self, paint: Paint<'a>) {
        if let Paint::LinearGradient(g) = paint {
            let _ = g.stops(0, self.0).count();
        }
    }
    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, _: ClipBox) {}
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _: CompositeMode) {}
    fn pop_layer(&mut self) {}
    fn push_transform(&mut self, _: Transform) {}
    fn pop_transform(&mut self) {}
}

#[test]
fn a_colr_walk_charges_the_variation_deltas_it_and_its_painter_read() {
    // Region-axis evaluations: each delta of the face's one set costs 1,024.
    let read = |depth: u32, leaf: &[u8]| {
        let bytes = variable_paint_dag(depth, leaf, 1_024);
        let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
        let coords = face.variation_coordinates();
        assert_eq!(coords.len(), 1, "the face has one axis");
        let _ = aterm_font::take_spent();
        let fg = RgbaColor::new(0, 0, 0, 255);
        let painted = face.paint_color_glyph(GlyphId(1), 0, fg, &mut AtInstance(coords));
        (painted, aterm_font::take_spent().delta_steps)
    };
    let leaves = [
        ("PaintVarTransform", var_transform_leaf(), 6),
        ("variable colour line", var_gradient_leaf(16), 2 * 16),
        ("PaintColrGlyph under a varying clip box", vec![11, 0, 2], 4),
    ];
    for (what, leaf, deltas) in leaves {
        // A small graph reads every delta at each of its 4 leaves.
        assert_eq!(read(2, &leaf), (Some(()), 4 * deltas * 1_024), "{what}");
        // The walk's 2^16 tables alone reach 10^8 evaluations.
        let (painted, deep) = read(30, &leaf);
        assert_eq!(painted, None, "{what}");
        assert!(
            deep <= 1 << 20,
            "{what}: the walk and its painter made {deep} evaluations"
        );
    }
}

/// The sweep gradients a COLR glyph paints: centre and angles.
#[derive(Default)]
struct Sweeps(Vec<[f32; 4]>);

impl<'a> Painter<'a> for Sweeps {
    fn outline_glyph(&mut self, _: GlyphId) {}
    fn paint(&mut self, paint: Paint<'a>) {
        if let Paint::SweepGradient(g) = paint {
            self.0
                .push([g.center_x, g.center_y, g.start_angle, g.end_angle]);
        }
    }
    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, _: ClipBox) {}
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _: CompositeMode) {}
    fn pop_layer(&mut self) {}
    fn push_transform(&mut self, _: Transform) {}
    fn pop_transform(&mut self) {}
}

#[test]
fn a_colr_delta_index_past_u32_max_wraps_as_upstream_release_does() {
    let path =
        aterm_tempfile::manifest_dir().join("../aterm-render/tests/fixtures/colr_1_variable.ttf");
    let seed =
        std::fs::read(&path).unwrap_or_else(|e| panic!("committed face {}: {e}", path.display()));
    let sweep = |base: Option<u32>| {
        let mut bytes = seed.clone();
        if let Some(base) = base {
            bytes[0x511f..0x5123].copy_from_slice(&base.to_be_bytes());
        }
        let mut face = Face::parse(&bytes, 0).expect("the committed face parses");
        let axes: Vec<_> = face
            .variation_axes()
            .into_iter()
            .map(|a| (a.tag, a.max_value))
            .collect();
        for (tag, max) in axes {
            assert_eq!(face.set_variation(tag, max), Some(()));
        }
        let mut sweeps = Sweeps::default();
        let fg = RgbaColor::new(0, 0, 0, 255);
        assert_eq!(
            face.paint_color_glyph(GlyphId(12), 0, fg, &mut sweeps),
            Some(())
        );
        sweeps.0
    };
    assert_eq!(sweep(None), [[500.0, 600.0, -0.5, 1.5]], "as committed");
    assert_eq!(sweep(Some(0xFFFF_FFFE)), [[0.0, 100.0, 1.0, 1.0]]);
    assert_eq!(sweep(Some(0xFFFF_FFFD)), [[0.0, 100.0, -1.030_517_6, 3.0]]);
    assert_eq!(
        sweep(Some(0xFFFF_FFFC)),
        [[0.0, 100.0, -1.030_517_6, 0.969_482_4]]
    );
}

fn cmap(subtable: &[u8]) -> Vec<u8> {
    let mut c = vec![0, 0, 0, 1, 0, 3, 0, 10, 0, 0, 0, 12];
    c.extend_from_slice(subtable);
    c
}

fn enumerated(bytes: &[u8]) -> u64 {
    let face = Face::parse(bytes, 0).expect("the synthetic face parses");
    let sub = face
        .tables()
        .cmap
        .expect("cmap parses")
        .subtables
        .get(0)
        .expect("one subtable");
    let mut n = 0u64;
    sub.codepoints(|_| n += 1);
    n
}

#[test]
fn overlapping_cmap_groups_enumerate_once_per_code_point() {
    let groups = 1000u32;
    let mut f12: Vec<u8> = vec![0, 12, 0, 0];
    f12.extend((16 + 12 * groups).to_be_bytes());
    f12.extend(0u32.to_be_bytes());
    f12.extend(groups.to_be_bytes());
    for _ in 0..groups {
        for v in [0u32, 0x10FFFF, 0] {
            f12.extend(v.to_be_bytes());
        }
    }
    assert_eq!(enumerated(&sfnt(1, &[(b"cmap", &cmap(&f12))])), 0x110000);

    let segments = 8000u16;
    let mut f4: Vec<u8> = Vec::new();
    for v in [4u16, 16 + 8 * segments, 0, 2 * segments, 0, 0, 0] {
        f4.extend(v.to_be_bytes());
    }
    let ends = (0..segments).map(|i| if i + 1 == segments { 0xFFFF } else { 0xFFFE });
    let starts = (0..segments).map(|i| if i + 1 == segments { 0xFFFF } else { 0 });
    f4.extend(ends.flat_map(u16::to_be_bytes));
    f4.extend([0, 0]);
    f4.extend(starts.flat_map(u16::to_be_bytes));
    f4.extend((0..2 * segments).flat_map(|_| [0u8, 0]));
    assert_eq!(enumerated(&sfnt(1, &[(b"cmap", &cmap(&f4))])), 0x10000);
}

fn n(v: i32) -> u8 {
    u8::try_from(v + 139).expect("-107..=107")
}

#[test]
fn cff2_blend_on_an_empty_stack_is_an_error() {
    let triangle: &[u8] = &[n(10), n(10), 21, n(50), n(0), 5, n(0), n(50), 5];
    let blend_then_triangle = [&[16u8][..], triangle].concat();
    let bytes = synthetic_cff2(&[triangle, &[16], &[n(1), 16, 21], &blend_then_triangle]);
    let face = Face::parse(&bytes, 0).expect("the synthetic CFF2 face parses");
    let mut sink = Count::default();
    assert_eq!(
        face.outline_glyph(GlyphId(0), &mut sink),
        Some(aterm_font::Rect {
            x_min: 10,
            y_min: 10,
            x_max: 60,
            y_max: 60
        }),
        "the well-formed glyph draws"
    );
    assert_eq!(sink.0, 3, "move and two lines");
    assert_eq!(face.outline_glyph(GlyphId(1), &mut sink), None);
    assert_eq!(face.outline_glyph(GlyphId(2), &mut sink), None);
    assert_eq!(face.outline_glyph(GlyphId(3), &mut sink), None);
}

fn kern_format_2(offsets: [u16; 3]) -> Vec<u8> {
    let mut kern: Vec<u8> = vec![0, 0, 0, 1, 0, 0, 0, 30, 2, 1];
    kern.extend(2u16.to_be_bytes());
    for v in offsets.into_iter().chain([10, 1, 26, 20, 1, 2]) {
        kern.extend(v.to_be_bytes());
    }
    for v in [-7i16, -9] {
        kern.extend(v.to_be_bytes());
    }
    sfnt(1, &[(b"kern", &kern)])
}

fn kerning(bytes: &[u8], left: u16, right: u16) -> Option<i64> {
    let face = Face::parse(bytes, 0).expect("the synthetic face parses");
    let kern = face.tables().kern.expect("kern parses");
    let sub = kern.subtables.into_iter().next().expect("one subtable");
    sub.glyphs_kerning(GlyphId(left), GlyphId(right))
        .map(i64::from)
}

/// Each point's x, as a point sink sees it.
#[derive(Default)]
struct Xs(Vec<Option<i64>>);

impl PointSink for Xs {
    fn point(&mut self, x: f32, _: f32, _: PointKind) {
        self.0.push(Some(x as i64));
    }
    fn end_contour(&mut self) {}
}

#[test]
fn crafted_table_edges_answer_as_upstream() {
    type Answer = fn() -> Vec<Option<i64>>;
    let rows: [(&str, Answer, &[Option<i64>]); 7] = [
        (
            "kern format 2: a glyph before a class table's first is class 0; an offset \
             inside the subtable header, or a value index before it, is refused; a left \
             class below the array offset less the header is refused",
            || {
                [
                    ([14, 20, 26], 10, 20),
                    ([14, 20, 26], 10, 5),
                    ([14, 20, 26], 5, 20),
                    ([2, 20, 26], 3, 20),
                    ([14, 2, 26], 10, 20),
                    ([14, 20, 2], 10, 20),
                    ([14, 20, 6], 5, 5),
                    ([14, 14, 26], 5, 10),
                    ([14, 20, 32], 10, 20),
                ]
                .map(|(offsets, left, right)| kerning(&kern_format_2(offsets), left, right))
                .to_vec()
            },
            &[
                Some(-9),
                Some(-7),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(-9),
            ],
        ),
        (
            "gvar: a point inferred between touched points 40,000 or 50,000 units apart \
             (past i16) is not moved",
            || {
                let mut iup = Vec::new();
                for v in [1u16, 10, 8, 0xA000, 0x4000] {
                    iup.extend(v.to_be_bytes());
                }
                iup.extend([2, 0x01, 0, 3, 0x01, 0, 40, 0x81]);
                let narrow = [(0, 0), (100, 100), (300, 100), (400, 0)];
                let wide = [(-20000, 0), (0, 100), (20000, 100), (30000, 0)];
                let bytes = crate::gvar_tuples::face_bytes(&[
                    (crate::gvar_tuples::simple(&narrow), iup.clone()),
                    (crate::gvar_tuples::simple(&wide), iup),
                ]);
                let mut face = Face::parse(&bytes, 0).expect("the synthetic face parses");
                face.set_variation(Tag::from_bytes(b"wght"), 900.0)
                    .expect("wght");
                let mut xs = Xs::default();
                for g in 0..2 {
                    face.outline_glyph_points(GlyphId(g), &mut xs)
                        .expect("draws");
                }
                xs.0
            },
            &[
                Some(0),
                Some(110),
                Some(330),
                Some(440),
                Some(-20000),
                Some(0),
                Some(20000),
                Some(30040),
            ],
        ),
        (
            "COLR v0: a layer range ending past u16::MAX is refused, not cut at the last \
             record",
            || {
                let (mut colr, mut cpal) = (Vec::new(), Vec::new());
                for v in [0u16, 2, 0, 14, 0, 26, 0xFFFF, 1, 0, 1, 2, 0xFFFF, 1] {
                    colr.extend(v.to_be_bytes());
                }
                colr.extend([0, 3, 0xFF, 0xFF].repeat(0xFFFF));
                for v in [0u16, 1, 1, 1, 0, 14, 0] {
                    cpal.extend(v.to_be_bytes());
                }
                cpal.extend([0, 0, 0, 255]);
                let bytes = sfnt(4, &[(b"COLR", &colr), (b"CPAL", &cpal)]);
                let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
                let fg = RgbaColor::new(0, 0, 0, 255);
                [1, 2]
                    .map(|g| {
                        let mut calls = Count::default();
                        face.paint_color_glyph(GlyphId(g), 0, fg, &mut calls)
                            .map(|()| calls.0 as i64)
                    })
                    .to_vec()
            },
            &[Some(2), None],
        ),
        (
            "cmap format 12: a group whose start glyph plus the code point passes u32::MAX \
             maps nothing (wrapped, U+0101 would be glyph 0)",
            || {
                let mut f12: Vec<u8> = vec![0, 12, 0, 0];
                for v in [40u32, 0, 2, 0x41, 0x42, 1, 0x100, 0x101, u32::MAX] {
                    f12.extend(v.to_be_bytes());
                }
                let bytes = sfnt(1, &[(b"cmap", &cmap(&f12))]);
                let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
                let cmap = face.tables().cmap.expect("cmap parses");
                let sub = cmap.subtables.get(0).expect("one subtable");
                [0x41, 0x42, 0x100, 0x101]
                    .map(|cp| sub.glyph_index(cp).map(|g| i64::from(g.0)))
                    .to_vec()
            },
            &[Some(1), Some(2), None, None],
        ),
        (
            "GDEF mark glyph set, coverage format 2: a coverage index past u16::MAX is not \
             covered",
            || {
                let mut gdef = Vec::new();
                for v in [1u16, 2, 0, 0, 0, 0, 14, 1, 1, 0, 8, 2, 1, 7, 8, 0xFFFF] {
                    gdef.extend(v.to_be_bytes());
                }
                let bytes = sfnt(9, &[(b"GDEF", &gdef)]);
                let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
                let gdef = face.tables().gdef.expect("GDEF parses");
                [6, 7, 8]
                    .map(|g| Some(i64::from(gdef.is_mark_glyph(GlyphId(g), Some(0)))))
                    .to_vec()
            },
            &[Some(0), Some(1), Some(0)],
        ),
        (
            "CFF charset formats 1 and 2: a range naming more glyphs than the font has, \
             or 65,536 glyphs, is refused, whatever ranges follow",
            || {
                use crate::bounded_work::index;
                let names = index(&[b"A".to_vec()], false);
                let at = 4 + names.len() + 19 + 4;
                let charsets: [&[u8]; 5] = [
                    &[1, 0, 1, 0],
                    &[1, 0, 1, 1],
                    &[2, 0, 1, 0, 0],
                    &[2, 0, 1, 0, 1, 0, 2, 0xFF, 0xFE],
                    &[2, 0, 1, 0xFF, 0xFF, 0, 2, 0, 0],
                ];
                charsets
                    .map(|charset| {
                        let mut top = vec![29];
                        top.extend(u32::try_from(at).expect("small").to_be_bytes());
                        top.extend([15, 29]);
                        let char_strings = at + charset.len();
                        top.extend(u32::try_from(char_strings).expect("small").to_be_bytes());
                        top.push(17);
                        let mut cff = vec![1, 0, 4, 4];
                        cff.extend(&names);
                        cff.extend(index(&[top], false));
                        cff.extend([0, 0, 0, 0]);
                        cff.extend(charset);
                        cff.extend(index(&[vec![14], vec![14]], false));
                        let bytes = sfnt(2, &[(b"CFF ", &cff)]);
                        let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
                        face.tables()
                            .cff
                            .map(|cff| i64::from(cff.number_of_glyphs()))
                    })
                    .to_vec()
            },
            &[Some(2), None, Some(2), None, None],
        ),
        (
            "avar: a coordinate past the last mapping is shifted by that mapping",
            || {
                let mut fvar = Vec::new();
                for v in [1u16, 0, 16, 2, 1, 20, 0, 8] {
                    fvar.extend(v.to_be_bytes());
                }
                fvar.extend(*b"wght");
                for user in [100i32, 400, 900] {
                    fvar.extend((user << 16).to_be_bytes());
                }
                fvar.extend([0, 0, 1, 0]);
                let mut avar = Vec::new();
                for v in [1u16, 0, 0, 1, 2, 0xC000, 0xC000, 0x2000, 0x1000] {
                    avar.extend(v.to_be_bytes());
                }
                let bytes = sfnt(1, &[(b"avar", &avar), (b"fvar", &fvar)]);
                let mut face = Face::parse(&bytes, 0).expect("the synthetic face parses");
                [650.0, 900.0]
                    .map(|wght| {
                        face.set_variation(Tag::from_bytes(b"wght"), wght)
                            .expect("wght");
                        let coords = face.variation_coordinates();
                        coords.first().map(|c| i64::from(c.get()))
                    })
                    .to_vec()
            },
            &[Some(0x1000), Some(0x3000)],
        ),
    ];
    for (case, answer, want) in rows {
        assert_eq!(answer(), want, "{case}");
    }
}

fn seeds() -> Vec<(&'static str, Vec<u8>)> {
    let render = aterm_tempfile::manifest_dir().join("../aterm-render");
    let mut out = Vec::new();
    for (label, rel) in [
        ("glyf", "tests/fixtures/jetbrains-mono.ttf"),
        ("gvar+HVAR+avar", "assets/game/Cinzel-var.ttf"),
        (
            "gvar, 43 axes, COLR v1 variable",
            "tests/fixtures/colr_1_variable.ttf",
        ),
        ("COLR v1", "tests/fixtures/colr_1.ttf"),
        ("kern format 0", "assets/game/Chewy-Regular.ttf"),
        ("GSUB Arabic joining, rlig", "assets/DejaVuSansMono.ttf"),
        ("CFF", "tests/fixtures/NotoSansSunuwar-Regular.otf"),
    ] {
        let path = render.join(rel);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("committed face {}: {e}", path.display()));
        out.push((label, bytes));
    }
    let curve: &[u8] = &[n(5), n(5), 21, n(10), n(20), n(30), n(40), n(50), n(60), 8];
    let blended: &[u8] = &[n(10), n(20), n(2), 16, 21, n(3), n(4), 5];
    out.push((
        "CFF2 synthetic",
        synthetic_cff2(&[curve, blended, &[n(1), n(1), 21, 19, 0xFF, n(2), n(2), 5]]),
    ));
    for (label, path) in [
        ("CFF2", "/System/Library/Fonts/SFIndia.ttc"),
        (
            "CFF CID",
            "/System/Library/Fonts/Supplemental/STIXTwoMath.otf",
        ),
        ("bdat", "/System/Library/Fonts/Monaco.ttf"),
        ("kern format 1", "/System/Library/Fonts/GeezaPro.ttc"),
        ("EBDT", "/System/Library/Fonts/Supplemental/Courier New.ttf"),
        ("MVAR", "/System/Library/Fonts/SFNSMono.ttf"),
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            out.push((label, bytes));
        }
    }
    out.push(("gvar past 32 tuples", crate::gvar_tuples::past_32_face()));
    out.push(("CBLC/CBDT synthetic", crate::synthetic_tables::cbdt_face()));
    out.push((
        "kerx+ankr synthetic",
        crate::gpos_shaping::kerx_ankr_seed_face(),
    ));
    out
}

const READ_TABLES: &[&[u8; 4]] = &[
    b"head", b"hhea", b"maxp", b"hmtx", b"vhea", b"vmtx", b"VORG", b"post", b"cmap", b"loca",
    b"glyf", b"gvar", b"fvar", b"avar", b"HVAR", b"VVAR", b"MVAR", b"CFF ", b"CFF2", b"GSUB",
    b"GPOS", b"GDEF", b"OS/2", b"name", b"kern", b"kerx", b"ankr", b"sbix", b"bloc", b"bdat",
    b"EBLC", b"EBDT", b"CBLC", b"CBDT", b"COLR", b"CPAL",
];

/// A table of face `index`: its byte range, and whether it holds CFF charstrings.
#[derive(Clone, Copy)]
struct Target {
    start: usize,
    end: usize,
    charstrings: bool,
    cff2: bool,
}

fn targets(bytes: &[u8], index: u32) -> Vec<Target> {
    let Ok(raw) = aterm_font::RawFace::parse(bytes, index) else {
        return Vec::new();
    };
    raw.table_records
        .into_iter()
        .filter(|r| READ_TABLES.contains(&&r.tag.to_bytes()))
        .filter_map(|r| {
            let start = usize::try_from(r.offset).ok()?;
            let end = start.checked_add(usize::try_from(r.length).ok()?)?;
            (end <= bytes.len() && start < end).then_some(Target {
                start,
                end,
                charstrings: matches!(&r.tag.to_bytes(), b"CFF " | b"CFF2"),
                cff2: &r.tag.to_bytes() == b"CFF2",
            })
        })
        .collect()
}

fn index_ranges(cff: &[u8], at: usize, count_width: usize) -> Option<(Vec<(usize, usize)>, usize)> {
    let be = |at: usize, n: usize| -> Option<usize> {
        let b = cff.get(at..at.checked_add(n)?)?;
        Some(b.iter().fold(0usize, |v, &x| (v << 8) | usize::from(x)))
    };
    let count = be(at, count_width)?;
    if count == 0 {
        return Some((Vec::new(), at + count_width));
    }
    let size = be(at + count_width, 1)?;
    let offsets = at + count_width + 1;
    let data = offsets + (count + 1) * size - 1;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let start = data + be(offsets + i * size, size)?;
        let end = data + be(offsets + (i + 1) * size, size)?;
        out.push((start, end));
    }
    let end = data + be(offsets + count * size, size)?;
    Some((out, end))
}

fn char_strings_offset(dict: &[u8]) -> Option<usize> {
    let mut i = 0;
    let mut last = None;
    while let Some(&b) = dict.get(i) {
        match b {
            28 => {
                last = Some(i16::from_be_bytes([*dict.get(i + 1)?, *dict.get(i + 2)?]) as i64);
                i += 3;
            }
            29 => {
                let v = i32::from_be_bytes([
                    *dict.get(i + 1)?,
                    *dict.get(i + 2)?,
                    *dict.get(i + 3)?,
                    *dict.get(i + 4)?,
                ]);
                last = Some(i64::from(v));
                i += 5;
            }
            30 => {
                i += 1;
                while dict
                    .get(i)
                    .is_some_and(|&n| n >> 4 != 0xF && n & 0xF != 0xF)
                {
                    i += 1;
                }
                i += 1;
            }
            32..=246 => {
                last = Some(i64::from(b) - 139);
                i += 1;
            }
            247..=250 => {
                last = Some((i64::from(b) - 247) * 256 + i64::from(*dict.get(i + 1)?) + 108);
                i += 2;
            }
            251..=254 => {
                last = Some(-(i64::from(b) - 251) * 256 - i64::from(*dict.get(i + 1)?) - 108);
                i += 2;
            }
            17 => return usize::try_from(last?).ok(),
            12 => i += 2,
            _ => i += 1,
        }
    }
    None
}

fn charstrings(bytes: &[u8], t: Target, cff2: bool) -> Vec<(usize, usize)> {
    let Some(cff) = bytes.get(t.start..t.end) else {
        return Vec::new();
    };
    let found = (|| {
        let top = if cff2 {
            let header = usize::from(*cff.get(2)?);
            let len = usize::from(u16::from_be_bytes([*cff.get(3)?, *cff.get(4)?]));
            cff.get(header..header + len)?
        } else {
            let header = usize::from(*cff.get(2)?);
            let (_, names_end) = index_ranges(cff, header, 2)?;
            let (dicts, _) = index_ranges(cff, names_end, 2)?;
            let &(s, e) = dicts.first()?;
            cff.get(s..e)?
        };
        let at = char_strings_offset(top)?;
        let (ranges, _) = index_ranges(cff, at, if cff2 { 4 } else { 2 })?;
        Some(ranges)
    })();
    found
        .unwrap_or_default()
        .into_iter()
        .filter(|&(s, e)| s < e && e <= cff.len())
        .map(|(s, e)| (t.start + s, t.start + e))
        .collect()
}

const CHARSTRING_OPS: &[u8] = &[
    1, 3, 4, 5, 6, 7, 8, 10, 11, 12, 14, 15, 16, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29,
    30, 31, 247, 251, 255,
];

fn mutate(
    seed: &[u8],
    targets: &[Target],
    glyph_programs: &[(usize, usize)],
    rng: &mut Lcg,
) -> Vec<u8> {
    let mut b = seed.to_vec();
    match rng.below(10) {
        2..=5 if !glyph_programs.is_empty() => {
            let (start, end) = glyph_programs[rng.below(glyph_programs.len())];
            for _ in 0..1 + rng.below(3) {
                let i = if rng.below(3) == 0 {
                    start
                } else {
                    start + rng.below(end - start)
                };
                b[i] = CHARSTRING_OPS[rng.below(CHARSTRING_OPS.len())];
            }
        }
        0 => {
            let at = rng.below(b.len());
            b.truncate(at);
        }
        1 => {
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(b.len().min(512));
                b[i] = rng.next() as u8;
            }
        }
        _ => {
            let t = targets[rng.below(targets.len())];
            for _ in 0..1 + rng.below(8) {
                let i = t.start + rng.below(t.end - t.start);
                match rng.below(5) {
                    0 => b[i] = rng.next() as u8,
                    1 => b[i] = 0xFF,
                    2 => b[i] = 0,
                    3 if t.charstrings => b[i] = CHARSTRING_OPS[rng.below(CHARSTRING_OPS.len())],
                    _ => {
                        if i + 1 < t.end {
                            let v: u16 = [0, 1, 0x7FFF, 0x8000, 0xFFFE, 0xFFFF][rng.below(6)];
                            b[i..i + 2].copy_from_slice(&v.to_be_bytes());
                        }
                    }
                }
            }
        }
    }
    b
}

const SHAPED: &[&str] = &[
    "a => b != c === d <!-- --> ffi 0xFF",
    "1\u{2044}2 12\u{2044}345",
    "\u{645}\u{631}\u{62D}\u{628}\u{627} \u{644}\u{627}\u{640}\u{647}",
    "\u{5E9}\u{5B8}\u{5C1}\u{5DC}\u{5D5}\u{5DD}",
    "e\u{301}\u{302}\u{34F}\u{303} A\u{30A}",
    "\u{2764}\u{FE0F}\u{200D}\u{1F525} \u{263A}\u{FE0E} 1\u{FE0F}\u{20E3}",
    "a\u{200B}b\u{200C}c\u{200D}d\u{AD}e\u{2060}f\u{180B}\u{E0020}",
    "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \u{1F1FA}\u{1F1F8}\u{1F1E6}",
    "--------------------------------------------------------------------------------",
];

fn shape_strings(face: Face<'_>) -> u64 {
    use aterm_font::shape::{Feature, ShapeFace, UnicodeBuffer, shape};
    let face = ShapeFace::new(face);
    let feature = |t: &[u8; 4], v| Feature::new(Tag::from_bytes(t), v);
    let lists = [
        Vec::new(),
        vec![
            feature(b"liga", 1),
            feature(b"calt", 1),
            feature(b"zero", 1),
            feature(b"ss01", 1),
            feature(b"cv01", 2),
            feature(b"aalt", 255),
            feature(b"liga", 0),
        ],
    ];
    let mut n = 0u64;
    for text in SHAPED {
        for list in &lists {
            let mut buffer = UnicodeBuffer::new();
            buffer.push_str(text);
            let shaped = shape(&face, list, buffer);
            n += shaped.len() as u64 + shaped.glyph_positions().len() as u64;
        }
    }
    n
}

fn exercise(bytes: &[u8], rng: &mut Lcg, glyph_cap: u16) -> u64 {
    let mut sink = Count::default();
    let _ = aterm_font::fonts_in_collection(bytes);
    for index in 0..2 {
        let Ok(mut face) = Face::parse(bytes, index) else {
            continue;
        };
        let axes: Vec<Tag> = face.variation_axes().into_iter().map(|a| a.tag).collect();
        if !axes.is_empty() && rng.below(3) != 0 {
            for tag in axes {
                let v = [
                    f32::NAN,
                    f32::INFINITY,
                    f32::NEG_INFINITY,
                    f32::MAX,
                    -1.0e9,
                    0.0,
                    rng.next() as f32 / 1000.0,
                ][rng.below(7)];
                let _ = face.set_variation(tag, v);
            }
        }
        let _ = face.variation_coordinates();
        let _ = (
            face.italic_angle(),
            face.is_monospaced(),
            face.global_bounding_box(),
        );
        for name in face.names() {
            sink.0 += name.to_string().map_or(0, |s| s.len() as u64);
        }
        let _ = (
            face.weight(),
            face.width(),
            face.style(),
            face.is_bold(),
            face.is_italic(),
            face.permissions(),
            face.is_subsetting_allowed(),
            face.is_outline_embedding_allowed(),
            face.unicode_ranges().contains_char('A'),
        );
        let _ = (
            face.ascender(),
            face.descender(),
            face.height(),
            face.line_gap(),
            face.typographic_ascender(),
            face.typographic_descender(),
            face.typographic_line_gap(),
            face.vertical_ascender(),
            face.vertical_height(),
            face.vertical_line_gap(),
            face.x_height(),
            face.capital_height(),
            face.underline_metrics(),
            face.strikeout_metrics(),
            face.subscript_metrics(),
            face.superscript_metrics(),
        );
        if let Some(kern) = face.tables().kern {
            for sub in kern.subtables {
                for (l, r) in [(0, 0), (1, 2), (36, 37), (u16::MAX, 3)] {
                    let _ = sub.glyphs_kerning(GlyphId(l), GlyphId(r));
                }
            }
        }
        if let Some(sbix) = face.tables().sbix {
            for strike in sbix.strikes {
                sink.0 += u64::from(strike.len());
                for g in [0, 1, 2, u16::MAX] {
                    let _ = strike.get(GlyphId(g));
                }
            }
        }
        let palettes = face.color_palettes().map_or(1, |p| p.get());
        if let Some(cmap) = face.tables().cmap {
            for sub in cmap.subtables {
                let mut n = 0u64;
                sub.codepoints(|_| n += 1);
                sink.0 += n;
                for cp in [0u32, 0x41, 0xFFFF, 0x1F600, 0x10FFFF, u32::MAX] {
                    let _ = sub.glyph_index(cp);
                    let _ = sub.glyph_variation_index(cp, 0xFE0F);
                }
            }
        }
        for (is_gpos, table) in [(false, face.tables().gsub), (true, face.tables().gpos)] {
            let Some(table) = table else {
                continue;
            };
            sink.0 += u64::from(table.scripts.len()) + u64::from(table.features.len());
            for i in 0..table.lookups.len() {
                let Some(lookup) = table.lookups.get(i) else {
                    continue;
                };
                for j in 0..lookup.subtables.len() {
                    if !is_gpos {
                        if let Some(s) = lookup
                            .subtables
                            .get::<aterm_font::gsub::SubstitutionSubtable>(j)
                        {
                            let c = s.coverage();
                            for g in [0, 1, 0x7FFF, u16::MAX] {
                                sink.0 += u64::from(c.get(GlyphId(g)).is_some());
                            }
                        }
                    } else if let Some(s) = lookup
                        .subtables
                        .get::<aterm_font::gpos::PositioningSubtable>(j)
                    {
                        let c = s.coverage();
                        for g in [0, 1, 0x7FFF, u16::MAX] {
                            sink.0 += u64::from(c.get(GlyphId(g)).is_some());
                        }
                        match s {
                            aterm_font::gpos::PositioningSubtable::Single(
                                aterm_font::gpos::SingleAdjustment::Format2 { values, .. },
                            ) => {
                                let _ = values.get(0);
                                let _ = values.get(u16::MAX);
                            }
                            aterm_font::gpos::PositioningSubtable::Pair(
                                aterm_font::gpos::PairAdjustment::Format1 { sets, .. },
                            ) => {
                                if let Some(set) = sets.get(0) {
                                    let _ = set.get(GlyphId(0));
                                    let _ = set.get(GlyphId(1));
                                }
                            }
                            aterm_font::gpos::PositioningSubtable::Pair(
                                aterm_font::gpos::PairAdjustment::Format2 { matrix, .. },
                            ) => {
                                let _ = matrix.get((0, 0));
                                let _ = matrix.get((u16::MAX, u16::MAX));
                            }
                            aterm_font::gpos::PositioningSubtable::Cursive(
                                aterm_font::gpos::CursiveAdjustment { sets, .. },
                            ) => {
                                let _ = sets.entry(0);
                                let _ = sets.exit(0);
                                let _ = sets.entry(u16::MAX);
                            }
                            aterm_font::gpos::PositioningSubtable::MarkToBase(
                                aterm_font::gpos::MarkToBaseAdjustment { marks, anchors, .. },
                            )
                            | aterm_font::gpos::PositioningSubtable::MarkToMark(
                                aterm_font::gpos::MarkToMarkAdjustment {
                                    marks,
                                    mark2_matrix: anchors,
                                    ..
                                },
                            ) => {
                                let _ = marks.get(0);
                                let _ = anchors.get(0, 0);
                                let _ = anchors.get(u16::MAX, u16::MAX);
                            }
                            aterm_font::gpos::PositioningSubtable::MarkToLigature(
                                aterm_font::gpos::MarkToLigatureAdjustment {
                                    marks,
                                    ligature_array,
                                    ..
                                },
                            ) => {
                                let _ = marks.get(0);
                                if let Some(m) = ligature_array.get(0) {
                                    let _ = m.get(0, 0);
                                    let _ = m.get(u16::MAX, u16::MAX);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            if let Some(v) = table.variations {
                let _ = v.find_index(face.variation_coordinates());
            }
        }
        for ch in ['\0', 'A', 'é', '\u{FFFD}', '😀', '\u{10FFFF}'] {
            let _ = face.glyph_index(ch);
            let _ = face.glyph_variation_index(ch, '\u{FE0F}');
        }
        sink.0 += shape_strings(face.clone());
        for g in 0..face.number_of_glyphs().min(glyph_cap) {
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
                let _ = gdef.glyph_class(g);
                let _ = gdef.glyph_mark_attachment_class(g);
                let _ = gdef.is_mark_glyph(g, None);
                let _ = gdef.is_mark_glyph(g, Some(0));
            }
            for ppem in [0, 16, 64, u16::MAX] {
                if let Some(img) = face.glyph_raster_image(g, ppem) {
                    sink.0 += img.data.len() as u64;
                }
            }
            if face.is_color_glyph(g) {
                if let Some(colr) = face.tables().colr {
                    let _ = colr.clip_box(g, face.variation_coordinates());
                }
                for palette in [0, palettes - 1, u16::MAX] {
                    let fg = RgbaColor::new(0, 0, 0, 255);
                    let _ = face.paint_color_glyph(g, palette, fg, &mut sink);
                }
            }
        }
        for g in [face.number_of_glyphs(), u16::MAX] {
            let _ = face.outline_glyph(GlyphId(g), &mut sink);
            let _ = face.glyph_hor_advance(GlyphId(g));
        }
        #[cfg(any(feature = "hint", debug_assertions))]
        {
            sink.0 += hint(bytes, index, &face, rng, glyph_cap);
        }
    }
    sink.0
}

/// The autohinter over the face.
#[cfg(any(feature = "hint", debug_assertions))]
fn hint(bytes: &[u8], index: u32, face: &Face<'_>, rng: &mut Lcg, glyph_cap: u16) -> u64 {
    use aterm_font::autohint::{HintingInstance, Target};
    let target = [Target::Normal, Target::Light, Target::Lcd][rng.below(3)];
    let px = [0.01f32, 0.5, 9.0, 13.0, 16.5, 300.0, 1.0e5][rng.below(7)];
    let settings: Vec<(u32, f32)> = face
        .variation_axes()
        .into_iter()
        .map(|a| {
            let v = [
                f32::NAN,
                f32::INFINITY,
                -1.0e9,
                a.min_value,
                a.max_value,
                rng.next() as f32 / 1000.0,
            ][rng.below(6)];
            (a.tag.0, v)
        })
        .collect();
    let Some(hint) = HintingInstance::new(bytes, index, px, &settings, target) else {
        return 0;
    };
    let mut sink = Count::default();
    let n = face.number_of_glyphs();
    for g in (0..n.min(glyph_cap / 8)).chain([n, u16::MAX]) {
        let _ = hint.draw(bytes, index, g, &mut sink);
    }
    sink.0
}

#[test]
fn seeds_answer() {
    let mut rng = Lcg(1);
    for (label, bytes) in seeds() {
        assert!(
            exercise(&bytes, &mut rng, u16::MAX) > 0,
            "seed `{label}` drew nothing: the fuzz below would be vacuous"
        );
    }
}

#[test]
fn mutated_faces_never_panic() {
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get().div_ceil(2));
    let (send, queue) = std::sync::mpsc::sync_channel::<(usize, &str, u32, Vec<u8>)>(workers);
    let queue = std::sync::Mutex::new(queue);
    let mut panicked: Vec<(usize, &str, u32)> = std::thread::scope(|scope| {
        let pool: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut panicked = Vec::new();
                    loop {
                        let next = queue
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .recv();
                        let Ok((at, label, round, bytes)) = next else {
                            return panicked;
                        };
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let mut inner = Lcg(u64::from(round) ^ 0x5DEE_CE66);
                            exercise(&bytes, &mut inner, 2048)
                        }));
                        if r.is_err() {
                            panicked.push((at, label, round));
                        }
                    }
                })
            })
            .collect();
        let mut rng = Lcg(0x9E37_79B9_7F4A_7C15);
        for (at, (label, seed)) in seeds().into_iter().enumerate() {
            let targets = targets(&seed, 0);
            assert!(!targets.is_empty(), "seed `{label}` has no tables");
            let glyph_programs: Vec<(usize, usize)> = targets
                .iter()
                .filter(|t| t.charstrings)
                .flat_map(|&t| charstrings(&seed, t, t.cff2))
                .take(2048)
                .collect();
            assert!(
                !label.starts_with("CFF") || !glyph_programs.is_empty(),
                "seed `{label}`: no charstrings found to mutate"
            );
            let rounds = if seed.len() > 1 << 20 { 150 } else { 400 };
            for round in 0..rounds {
                let bytes = mutate(&seed, &targets, &glyph_programs, &mut rng);
                send.send((at, label, round, bytes))
                    .expect("a query worker is alive");
            }
        }
        drop(send);
        pool.into_iter()
            .flat_map(|worker| {
                worker
                    .join()
                    .expect("a query worker panicked outside a round")
            })
            .collect()
    });
    panicked.sort_unstable();
    assert!(
        panicked.is_empty(),
        "mutated seeds panicked (seed number, seed, round): {panicked:?} (replay: mutate \
         the seed with that round's LCG state)"
    );
}

#[test]
fn arbitrary_bytes_never_panic() {
    let mut rng = Lcg(0xD1B5_4A32_D192_ED03);
    for _ in 0..4_000 {
        let len = rng.below(512);
        let mut b: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        if rng.below(2) == 0 && b.len() >= 4 {
            b[..4].copy_from_slice(&[0, 1, 0, 0]);
        }
        let mut inner = Lcg(u64::from(rng.next()));
        let _ = exercise(&b, &mut inner, 64);
    }
}
