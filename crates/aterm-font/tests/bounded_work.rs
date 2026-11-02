// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Cumulative work budget tests for outline and layout parsing.

use aterm_font::{Face, GlyphId, OutlineSink, PointKind, PointSink};

#[derive(Default)]
struct Guard(usize);

impl Guard {
    fn tick(&mut self) {
        self.0 += 1;
        assert!(
            self.0 < 131_072,
            "fixture callback guard: missing cumulative work limit"
        );
    }
}

impl OutlineSink for Guard {
    fn move_to(&mut self, _: f32, _: f32) {
        self.tick();
    }
    fn line_to(&mut self, _: f32, _: f32) {
        self.tick();
    }
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {
        self.tick();
    }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
        self.tick();
    }
    fn close(&mut self) {
        self.tick();
    }
}

impl PointSink for Guard {
    fn point(&mut self, _: f32, _: f32, _: PointKind) {
        self.tick();
    }
    fn end_contour(&mut self) {
        self.tick();
    }
}

fn refuses_expansion(bytes: &[u8], expanded: GlyphId) {
    let face = Face::parse(bytes, 0).expect("the synthetic face parses");
    for points in [false, true] {
        let outline = |glyph, sink: &mut Guard| {
            if points {
                face.outline_glyph_points(glyph, sink)
            } else {
                face.outline_glyph(glyph, sink)
            }
        };
        let mut ordinary = Guard::default();
        assert!(
            outline(GlyphId(0), &mut ordinary).is_some(),
            "ordinary glyph, points={points}"
        );
        assert!(ordinary.0 > 0);
        let mut guard = Guard::default();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            outline(expanded, &mut guard)
        }));
        assert!(
            result.is_ok(),
            "expansion reached the fixture guard, points={points}"
        );
        assert_eq!(result.unwrap(), None, "expansion must be refused");
        assert!(guard.0 > 0 && guard.0 < 131_072);
        assert!(outline(GlyphId(0), &mut Guard::default()).is_some());
    }
}

fn composite(child: u16) -> Vec<u8> {
    let mut data = (-1i16).to_be_bytes().to_vec();
    data.extend([0; 8]);
    for flags in [0x0023u16, 0x0003] {
        data.extend(flags.to_be_bytes());
        data.extend(child.to_be_bytes());
        data.extend([0; 4]);
    }
    data
}

fn composite_face(varied: bool) -> Vec<u8> {
    let mut glyphs = vec![(crate::gvar_tuples::triangle(), Vec::new())];
    for child in 0..24 {
        glyphs.push((composite(child), Vec::new()));
    }
    if varied {
        return crate::gvar_tuples::face_bytes(&glyphs);
    }
    let (mut glyf, mut loca) = (Vec::new(), Vec::new());
    for (record, _) in &glyphs {
        loca.extend(u16::try_from(glyf.len() / 2).unwrap().to_be_bytes());
        glyf.extend(record);
    }
    loca.extend(u16::try_from(glyf.len() / 2).unwrap().to_be_bytes());
    crate::never_panic::sfnt(glyphs.len(), &[(b"glyf", &glyf), (b"loca", &loca)])
}

#[test]
fn composite_work_is_shared_across_sibling_components() {
    refuses_expansion(&composite_face(false), GlyphId(24));
}

#[test]
fn varied_composite_work_is_shared_across_sibling_components() {
    refuses_expansion(&composite_face(true), GlyphId(24));
}

#[test]
fn spent_counts_the_outline_budget_steps() {
    let bytes = composite_face(false);
    let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
    let _ = aterm_font::take_spent();
    assert!(
        face.outline_glyph(GlyphId(0), &mut Guard::default())
            .is_some()
    );
    let ordinary = aterm_font::take_spent().outline_steps;
    assert!((1..64).contains(&ordinary), "the triangle took {ordinary}");
    assert!(
        face.outline_glyph(GlyphId(1), &mut Guard::default())
            .is_some()
    );
    assert_eq!(aterm_font::take_spent().outline_steps, 1 + 2 * ordinary);
    assert_eq!(face.outline_glyph(GlyphId(24), &mut Guard::default()), None);
    let refused = aterm_font::take_spent().outline_steps;
    assert!(refused > 65_536, "the refused expansion read {refused}");
    assert_eq!(aterm_font::take_spent(), aterm_font::Spent::default());
}

#[test]
fn a_zero_contour_component_keeps_the_existing_varied_outline_answer() {
    let mut record = composite(0);
    record[20..22].copy_from_slice(&1u16.to_be_bytes());
    let bytes = crate::gvar_tuples::face_bytes(&[
        (crate::gvar_tuples::triangle(), Vec::new()),
        (vec![0; 10], Vec::new()),
        (record, Vec::new()),
    ]);
    let face = Face::parse(&bytes, 0).expect("the synthetic face parses");
    assert_eq!(
        face.glyph_bounding_box(GlyphId(2)),
        face.glyph_bounding_box(GlyphId(0))
    );
    assert!(face.glyph_bounding_box(GlyphId(0)).is_some());
}

/// A CFF INDEX of `items` (CFF2's 32-bit count when `cff2`), 2-byte offsets.
pub(crate) fn index(items: &[Vec<u8>], cff2: bool) -> Vec<u8> {
    let mut data = if cff2 {
        u32::try_from(items.len()).unwrap().to_be_bytes().to_vec()
    } else {
        u16::try_from(items.len()).unwrap().to_be_bytes().to_vec()
    };
    if items.is_empty() {
        return data;
    }
    data.push(2);
    let mut at = 1u16;
    data.extend(at.to_be_bytes());
    for item in items {
        at += u16::try_from(item.len()).unwrap();
        data.extend(at.to_be_bytes());
    }
    for item in items {
        data.extend(item);
    }
    data
}

fn number(value: i16) -> u8 {
    u8::try_from(value + 139).unwrap()
}

fn charstring_face(cff2: bool) -> Vec<u8> {
    let mut subrs = vec![vec![number(1), number(1), 5]]; // one line
    if !cff2 {
        subrs[0].push(11);
    }
    for child in 0..8 {
        let mut subr = [number(child - 107), 29].repeat(16);
        if !cff2 {
            subr.push(11);
        }
        subrs.push(subr);
    }
    let globals = index(&subrs, cff2);
    let mut ordinary = vec![number(0), number(0), 21, number(1), number(1), 5];
    let mut expanded = vec![number(0), number(0), 21, number(8 - 107), 29];
    if !cff2 {
        ordinary.push(14);
        expanded.push(14);
    }
    let chars = index(&[ordinary, expanded], cff2);
    let table = if cff2 {
        let vstore = 5 + 12 + globals.len();
        let store: Vec<u8> = [
            &[0, 24][..],
            &[0, 1],
            &[0, 0, 0, 12],
            &[0, 1],
            &[0, 0, 0, 16],
            &[0, 0, 0, 0],
            &[0, 0, 0, 0, 0, 0],
        ]
        .concat();
        let mut data = vec![2, 0, 5, 0, 12, 29];
        data.extend(u32::try_from(vstore + store.len()).unwrap().to_be_bytes());
        data.extend([17, 29]);
        data.extend(u32::try_from(vstore).unwrap().to_be_bytes());
        data.push(24);
        data.extend(globals);
        data.extend(store);
        data.extend(chars);
        data
    } else {
        let names = index(&[b"A".to_vec()], false);
        let strings = index(&[], false);
        let char_at = 4 + names.len() + 13 + strings.len() + globals.len();
        let mut top = vec![29];
        top.extend(u32::try_from(char_at).unwrap().to_be_bytes());
        top.push(17);
        let top = index(&[top], false);
        assert_eq!(top.len(), 13);
        let mut data = vec![1, 0, 4, 4];
        data.extend(names);
        data.extend(top);
        data.extend(strings);
        data.extend(globals);
        data.extend(chars);
        data
    };
    crate::never_panic::sfnt(2, &[(if cff2 { b"CFF2" } else { b"CFF " }, &table)])
}

#[test]
fn cff_work_is_shared_across_subroutine_calls() {
    refuses_expansion(&charstring_face(false), GlyphId(1));
}

#[test]
fn cff2_work_is_shared_across_subroutine_calls() {
    refuses_expansion(&charstring_face(true), GlyphId(1));
}

fn aliased_gsub(lookups: usize, subtables: usize) -> Vec<u8> {
    let be16 = |v: usize| u16::try_from(v).unwrap().to_be_bytes();
    let mut g = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let script_list = g.len();
    g.extend([0, 1, b'D', b'F', b'L', b'T', 0, 8, 0, 4, 0, 0]);
    g.extend([0, 0, 0xFF, 0xFF, 0, 1, 0, 0]);
    let feature_list = g.len();
    g.extend([0, 1, b'l', b'i', b'g', b'a', 0, 8, 0, 0]);
    g.extend(be16(lookups));
    (0..lookups).for_each(|i| g.extend(be16(i)));
    let lookup_list = g.len();
    g.extend(be16(lookups));
    (0..lookups).for_each(|_| g.extend(be16(2 + 2 * lookups)));
    g.extend([0, 1, 0, 0]);
    g.extend(be16(subtables));
    (0..subtables).for_each(|_| g.extend(be16(6 + 2 * subtables)));
    g.extend([0, 1, 0, 6, 0, 1, 0, 1, 0, 1, 0, 0]);
    g[4..6].copy_from_slice(&be16(script_list));
    g[6..8].copy_from_slice(&be16(feature_list));
    g[8..10].copy_from_slice(&be16(lookup_list));
    g
}

fn shaped(gsub: &[u8]) -> Vec<u32> {
    use aterm_font::Tag;
    use aterm_font::shape::{Feature, ShapeFace, UnicodeBuffer, shape};
    let bytes = crate::never_panic::sfnt(2, &[(b"GSUB", gsub)]);
    let face = ShapeFace::new(Face::parse(&bytes, 0).expect("the synthetic face parses"));
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str("ab");
    let liga = [Feature::new(Tag::from_bytes(b"liga"), 1)];
    shape(&face, &liga, buffer)
        .glyph_infos()
        .iter()
        .map(|info| info.glyph_id)
        .collect()
}

#[test]
fn aliased_gsub_lookups_are_parsed_within_a_budget() {
    assert_eq!(shaped(&aliased_gsub(10, 10)), [1, 1], "within the budget");
    assert_eq!(
        shaped(&aliased_gsub(1_000, 1_000)),
        [0, 0],
        "past the budget, GSUB is refused"
    );
}

fn nested_gsub(subtables: usize, coverage: usize) -> Vec<u8> {
    let be16 = |v: usize| u16::try_from(v).unwrap().to_be_bytes();
    let mut g = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let script_list = g.len();
    g.extend([0, 1, b'D', b'F', b'L', b'T', 0, 8, 0, 4, 0, 0]);
    g.extend([0, 0, 0xFF, 0xFF, 0, 1, 0, 0]);
    let feature_list = g.len();
    g.extend([0, 1, b'l', b'i', b'g', b'a', 0, 8, 0, 0, 0, 2, 0, 0, 0, 1]);
    let lookup_list = g.len();
    g.extend([0, 3, 0, 8, 0, 28, 0, 56]);
    g.extend([0, 1, 0, 0, 0, 1, 0, 8]);
    g.extend([0, 1, 0, 6, 0, 1]);
    g.extend([0, 1, 0, 1, 0, 0]);
    g.extend([0, 5, 0, 0, 0, 1, 0, 8]);
    g.extend([0, 3, 0, 1, 0, 1, 0, 12, 0, 0, 0, 2]);
    g.extend([0, 1, 0, 2, 0, 0, 0, 1]);
    assert_eq!(g.len() - lookup_list, 56, "lookup 2's offset");
    g.extend([0, 1, 0, 0]);
    g.extend(be16(subtables));
    (0..subtables).for_each(|_| g.extend(be16(6 + 2 * subtables)));
    g.extend([0, 1, 0, 6, 0, 0]);
    g.extend([0, 1]);
    g.extend(be16(coverage));
    (1..=coverage).for_each(|gid| g.extend(be16(gid)));
    g[4..6].copy_from_slice(&be16(script_list));
    g[6..8].copy_from_slice(&be16(feature_list));
    g[8..10].copy_from_slice(&be16(lookup_list));
    g
}

fn shaped_twice(gsub: &[u8]) -> [Vec<u32>; 2] {
    use aterm_font::Tag;
    use aterm_font::shape::{Feature, ShapeFace, UnicodeBuffer, shape};
    let bytes = crate::never_panic::sfnt(2, &[(b"GSUB", gsub)]);
    let face = ShapeFace::new(Face::parse(&bytes, 0).expect("the synthetic face parses"));
    let liga = [Feature::new(Tag::from_bytes(b"liga"), 1)];
    let once = || {
        let mut buffer = UnicodeBuffer::new();
        buffer.push_str("a");
        shape(&face, &liga, buffer)
            .glyph_infos()
            .iter()
            .map(|info| info.glyph_id)
            .collect()
    };
    [once(), once()]
}

#[test]
fn a_gsub_refused_through_a_nested_lookup_applies_none_of_it() {
    assert_eq!(
        shaped_twice(&nested_gsub(2, 4)),
        [[1], [1]],
        "within the budget"
    );
    assert_eq!(
        shaped_twice(&nested_gsub(2_000, 1_000)),
        [[0], [0]],
        "past the budget, through a nested lookup: GSUB is refused from the first shape"
    );
}

fn aliased_gpos(lookups: usize, subtables: usize) -> Vec<u8> {
    let be16 = |v: usize| u16::try_from(v).unwrap().to_be_bytes();
    let mut g = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let script_list = g.len();
    g.extend([0, 1, b'D', b'F', b'L', b'T', 0, 8, 0, 4, 0, 0]);
    g.extend([0, 0, 0xFF, 0xFF, 0, 1, 0, 0]);
    let feature_list = g.len();
    g.extend([0, 1, b'k', b'e', b'r', b'n', 0, 8, 0, 0]);
    g.extend(be16(lookups));
    (0..lookups).for_each(|i| g.extend(be16(i)));
    let lookup_list = g.len();
    g.extend(be16(lookups));
    (0..lookups).for_each(|_| g.extend(be16(2 + 2 * lookups)));
    // LookupType 1 (SinglePos), flags 0, `subtables` aliased to the same Format 1 subtable (+10 x_offset).
    g.extend([0, 1, 0, 0]);
    g.extend(be16(subtables));
    (0..subtables).for_each(|_| g.extend(be16(6 + 2 * subtables)));
    g.extend([0, 1, 0, 8, 0, 1, 0, 10, 0, 1, 0, 1, 0, 0]);
    g[4..6].copy_from_slice(&be16(script_list));
    g[6..8].copy_from_slice(&be16(feature_list));
    g[8..10].copy_from_slice(&be16(lookup_list));
    g
}

fn shaped_x_offsets(gpos: &[u8], text: &str) -> Vec<i32> {
    use aterm_font::shape::{ShapeFace, UnicodeBuffer, shape};
    let bytes = crate::never_panic::sfnt(2, &[(b"GPOS", gpos)]);
    let face = ShapeFace::new(Face::parse(&bytes, 0).expect("the synthetic face parses"));
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    shape(&face, &[], buffer)
        .glyph_positions()
        .iter()
        .map(|pos| pos.x_offset)
        .collect()
}

#[test]
fn aliased_gpos_lookups_are_parsed_within_a_budget() {
    assert_eq!(
        shaped_x_offsets(&aliased_gpos(10, 10), "ab"),
        [100, 100],
        "within the budget, each of the 10 lookups shifts x_offset by +10"
    );
    assert_eq!(
        shaped_x_offsets(&aliased_gpos(1_000, 1_000), "ab"),
        [0, 0],
        "past the budget, GPOS is refused as a whole"
    );
}

fn nested_gpos(subtables: usize, coverage: usize) -> Vec<u8> {
    let be16 = |v: usize| u16::try_from(v).unwrap().to_be_bytes();
    let mut g = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let script_list = g.len();
    g.extend([0, 1, b'D', b'F', b'L', b'T', 0, 8, 0, 4, 0, 0]);
    g.extend([0, 0, 0xFF, 0xFF, 0, 1, 0, 0]);
    let feature_list = g.len();
    g.extend([0, 1, b'k', b'e', b'r', b'n', 0, 8, 0, 0, 0, 2, 0, 0, 0, 1]);
    let lookup_list = g.len();
    g.extend([0, 3, 0, 8, 0, 30, 0, 58]);
    // Lookup 0 (22 bytes): SinglePos Format 1 on glyph 0, x_offset += 25.
    g.extend([0, 1, 0, 0, 0, 1, 0, 8]);
    g.extend([0, 1, 0, 8, 0, 1, 0, 25]);
    g.extend([0, 1, 0, 1, 0, 0]);
    assert_eq!(g.len() - lookup_list, 30, "lookup 1's offset");
    // Lookup 1 (28 bytes): ContextPos (LookupType 7) Format 3 recursing into Lookup 2.
    g.extend([0, 7, 0, 0, 0, 1, 0, 8]);
    g.extend([0, 3, 0, 1, 0, 1, 0, 12, 0, 0, 0, 2]);
    g.extend([0, 1, 0, 2, 0, 0, 0, 1]);
    assert_eq!(g.len() - lookup_list, 58, "lookup 2's offset");
    // Lookup 2: SinglePos Format 1 with `subtables` aliased subtables over `coverage` glyphs.
    g.extend([0, 1, 0, 0]);
    g.extend(be16(subtables));
    (0..subtables).for_each(|_| g.extend(be16(6 + 2 * subtables)));
    g.extend([0, 1, 0, 8, 0, 1, 0, 0]);
    g.extend([0, 1]);
    g.extend(be16(coverage));
    (1..=coverage).for_each(|gid| g.extend(be16(gid)));
    g[4..6].copy_from_slice(&be16(script_list));
    g[6..8].copy_from_slice(&be16(feature_list));
    g[8..10].copy_from_slice(&be16(lookup_list));
    g
}

fn shaped_gpos_twice(gpos: &[u8]) -> [Vec<i32>; 2] {
    use aterm_font::shape::{ShapeFace, UnicodeBuffer, shape};
    let bytes = crate::never_panic::sfnt(2, &[(b"GPOS", gpos)]);
    let face = ShapeFace::new(Face::parse(&bytes, 0).expect("the synthetic face parses"));
    let once = || {
        let mut buffer = UnicodeBuffer::new();
        buffer.push_str("a");
        shape(&face, &[], buffer)
            .glyph_positions()
            .iter()
            .map(|pos| pos.x_offset)
            .collect()
    };
    [once(), once()]
}

#[test]
fn a_gpos_refused_through_a_nested_lookup_applies_none_of_it() {
    assert_eq!(
        shaped_gpos_twice(&nested_gpos(2, 4)),
        [[25], [25]],
        "within the budget"
    );
    assert_eq!(
        shaped_gpos_twice(&nested_gpos(2_000, 1_000)),
        [[0], [0]],
        "past the budget, through a nested lookup: GPOS is refused from the first shape"
    );
}

/// A `GSUB` whose `calt` runs lookup 0 (glyph 0 to 1), then `lookups` chain-context lookups
/// sharing one format-3 subtable that takes any glyph followed by `lookahead` more and applies
/// nothing, so each walks up to `lookahead` glyphs ahead of every glyph of a run, then a last
/// lookup (glyph 1 to 2).
fn chained_gsub(lookups: usize, lookahead: usize) -> Vec<u8> {
    let be16 = |v: usize| u16::try_from(v).unwrap().to_be_bytes();
    let count = lookups + 2;
    let mut g = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let script_list = g.len();
    g.extend([0, 1, b'D', b'F', b'L', b'T', 0, 8, 0, 4, 0, 0]);
    g.extend([0, 0, 0xFF, 0xFF, 0, 1, 0, 0]);
    let feature_list = g.len();
    g.extend([0, 1, b'c', b'a', b'l', b't', 0, 8, 0, 0]);
    g.extend(be16(count));
    (0..count).for_each(|i| g.extend(be16(i)));
    let lookup_list = g.len();
    // The lookup tables follow their offsets, then the two single substitutions, then the
    // shared chain context.
    let tables = 2 + 2 * count;
    let singles = tables + 8 * count;
    let chain = singles + 24;
    g.extend(be16(count));
    (0..count).for_each(|i| g.extend(be16(tables + 8 * i)));
    for i in 0..count {
        let (kind, subtable) = match i {
            0 => (1, singles),
            i if i == count - 1 => (1, singles + 12),
            _ => (6, chain),
        };
        g.extend(be16(kind));
        g.extend([0, 0, 0, 1]);
        g.extend(be16(subtable - (tables + 8 * i)));
    }
    for glyph in [0, 1] {
        g.extend([0, 1, 0, 6, 0, 1, 0, 1, 0, 1, 0, glyph]);
    }
    let coverage = 12 + 2 * lookahead;
    g.extend([0, 3, 0, 0, 0, 1]);
    g.extend(be16(coverage));
    g.extend(be16(lookahead));
    (0..lookahead).for_each(|_| g.extend(be16(coverage)));
    g.extend([0, 0]);
    g.extend([0, 2, 0, 1, 0, 0, 0xFF, 0xFF, 0, 0]);
    g[4..6].copy_from_slice(&be16(script_list));
    g[6..8].copy_from_slice(&be16(feature_list));
    g[8..10].copy_from_slice(&be16(lookup_list));
    g
}

#[test]
fn contextual_lookups_stop_applying_once_the_layout_budget_is_spent() {
    use aterm_font::shape::{ShapeFace, UnicodeBuffer, shape};
    const RUN: usize = 256;
    let shaped = |lookups: usize| {
        let bytes = crate::never_panic::sfnt(3, &[(b"GSUB", &chained_gsub(lookups, 128))]);
        let face = ShapeFace::new(Face::parse(&bytes, 0).expect("the synthetic face parses"));
        let mut buffer = UnicodeBuffer::new();
        buffer.push_str(&"x".repeat(RUN));
        let _ = aterm_font::take_spent();
        let glyphs: Vec<u32> = shape(&face, &[], buffer)
            .glyph_infos()
            .iter()
            .map(|info| info.glyph_id)
            .collect();
        (glyphs, aterm_font::take_spent().layout_ops)
    };
    let (glyphs, _) = shaped(2);
    assert_eq!(glyphs, [2; RUN], "within the budget every lookup runs");
    // 4,000 lookups walking 128 glyphs ahead at half the run's glyphs would spend about 70M
    // units, against 16k a character (4.2M here).
    let (glyphs, spent) = shaped(4_000);
    let budget = RUN as u64 * 16 * 1024;
    assert!(
        (budget..2 * budget).contains(&spent),
        "spent {spent} of {budget}"
    );
    assert_eq!(
        glyphs, [1; RUN],
        "past the budget no lookup applies, and the first lookup's substitution stays"
    );
}

/// A `GSUB` whose `calt` grows each glyph 0 into 64 glyph 1s, then runs one chain-context
/// lookup that takes any glyph followed by `lookahead` more and applies nothing.
fn expanded_single_chain(lookahead: usize) -> Vec<u8> {
    let be16 = |v: usize| u16::try_from(v).unwrap().to_be_bytes();
    let mut g = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let script_list = g.len();
    g.extend([0, 1, b'D', b'F', b'L', b'T', 0, 8, 0, 4, 0, 0]);
    g.extend([0, 0, 0xFF, 0xFF, 0, 1, 0, 0]);
    let feature_list = g.len();
    g.extend([0, 1, b'c', b'a', b'l', b't', 0, 8, 0, 0, 0, 2, 0, 0, 0, 1]);
    let lookup_list = g.len();
    // Two lookup tables at 6 and 14, then the multiple substitution, then the chain context.
    let multiple = 22;
    let chain = multiple + 16 + 2 * 64;
    g.extend([0, 2, 0, 6, 0, 14]);
    g.extend([0, 2, 0, 0, 0, 1]);
    g.extend(be16(multiple - 6));
    g.extend([0, 6, 0, 0, 0, 1]);
    g.extend(be16(chain - 14));
    g.extend([0, 1, 0, 8, 0, 1, 0, 14, 0, 1, 0, 1, 0, 0]);
    g.extend(be16(64));
    (0..64).for_each(|_| g.extend([0, 1]));
    let coverage = 12 + 2 * lookahead;
    g.extend([0, 3, 0, 0, 0, 1]);
    g.extend(be16(coverage));
    g.extend(be16(lookahead));
    (0..lookahead).for_each(|_| g.extend(be16(coverage)));
    g.extend([0, 0]);
    g.extend([0, 2, 0, 1, 0, 0, 0xFF, 0xFF, 0, 0]);
    g[4..6].copy_from_slice(&be16(script_list));
    g[6..8].copy_from_slice(&be16(feature_list));
    g[8..10].copy_from_slice(&be16(lookup_list));
    g
}

#[test]
fn one_lookup_over_a_run_a_substitution_grew_is_refused_partway() {
    use aterm_font::shape::{ShapeFace, UnicodeBuffer, shape};
    const RUN: usize = 64;
    let bytes = crate::never_panic::sfnt(3, &[(b"GSUB", &expanded_single_chain(2_048))]);
    let face = ShapeFace::new(Face::parse(&bytes, 0).expect("the synthetic face parses"));
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(&"x".repeat(RUN));
    let _ = aterm_font::take_spent();
    let glyphs = shape(&face, &[], buffer).glyph_infos().len();
    let spent = aterm_font::take_spent().layout_ops;
    assert_eq!(
        glyphs,
        RUN * 64,
        "the multiple substitution applies in full"
    );
    // Walking 2,048 glyphs ahead at the first half of 4,096 would spend about 4.2M units; only
    // refusing within the one lookup keeps it near the budget.
    let budget = RUN as u64 * 16 * 1024;
    assert!(
        (budget..2 * budget).contains(&spent),
        "spent {spent} of {budget}"
    );
}

/// The synthetic face's glyphs: bases `B` and `x`, a mark `m`, ligatures `L` and `Z` of `B`s
/// and of `Z0`s, and `OUT`, which the last lookup of each `GSUB` makes.
const B: usize = 1;
const M: usize = 2;
const X: usize = 3;
const L: usize = 4;
const Z0: usize = 5;
const Z: usize = 6;
const OUT: usize = 7;
const IGNORE_MARKS: usize = 8;

fn be16(v: usize) -> [u8; 2] {
    u16::try_from(v).unwrap().to_be_bytes()
}

/// `from` to `to` (SingleSubstFormat1).
fn single(from: usize, to: usize) -> Vec<u8> {
    [
        &[0, 1, 0, 6][..],
        &be16(to.wrapping_sub(from) & 0xFFFF),
        &[0, 1, 0, 1],
        &be16(from),
    ]
    .concat()
}

/// `from` to the glyphs `to` (MultipleSubstFormat1).
fn multiple(from: usize, to: &[usize]) -> Vec<u8> {
    let mut t = vec![0, 1, 0, 8, 0, 1, 0, 14, 0, 1, 0, 1];
    t.extend(be16(from));
    t.extend(be16(to.len()));
    to.iter().for_each(|&glyph| t.extend(be16(glyph)));
    t
}

/// `first` and then `rest` to `glyph` (LigatureSubstFormat1).
fn ligature(first: usize, rest: &[usize], glyph: usize) -> Vec<u8> {
    let mut t = vec![0, 1, 0, 8, 0, 1, 0, 14, 0, 1, 0, 1];
    t.extend(be16(first));
    t.extend([0, 1, 0, 4]);
    t.extend(be16(glyph));
    t.extend(be16(rest.len() + 1));
    rest.iter().for_each(|&glyph| t.extend(be16(glyph)));
    t
}

/// `input` copies of `glyph`, running each `(sequence index, lookup)` record (ContextFormat3).
fn context(input: usize, glyph: usize, records: &[(usize, usize)]) -> Vec<u8> {
    let coverage = 6 + 2 * input + 4 * records.len();
    let mut t = vec![0, 3];
    t.extend(be16(input));
    t.extend(be16(records.len()));
    (0..input).for_each(|_| t.extend(be16(coverage)));
    records
        .iter()
        .for_each(|&(index, lookup)| t.extend([be16(index), be16(lookup)].concat()));
    t.extend([0, 1, 0, 1]);
    t.extend(be16(glyph));
    t
}

/// `glyph` followed by `ahead`, running each `(sequence index, lookup)` record
/// (ChainContextFormat3).
fn chain(glyph: usize, ahead: usize, records: &[(usize, usize)]) -> Vec<u8> {
    let coverage = 14 + 4 * records.len();
    let mut t = vec![0, 3, 0, 0, 0, 1];
    t.extend(be16(coverage));
    t.extend([0, 1]);
    t.extend(be16(coverage + 6));
    t.extend(be16(records.len()));
    records
        .iter()
        .for_each(|&(index, lookup)| t.extend([be16(index), be16(lookup)].concat()));
    t.extend([0, 1, 0, 1]);
    t.extend(be16(glyph));
    t.extend([0, 1, 0, 1]);
    t.extend(be16(ahead));
    t
}

type Lookup = (usize, usize, Vec<u8>);

/// A `GSUB` whose `calt` runs the `first` lookups (each a type, flags and subtable), then
/// `copies` lookups sharing one rule, then a last lookup that turns `last` into `OUT`. The
/// `nested` lookups follow the copies, outside `calt`; `rule` makes the rule's subtable given
/// the first one's index.
fn repeated_rule_gsub(
    first: &[Lookup],
    copies: usize,
    rule: (usize, usize, &dyn Fn(usize) -> Vec<u8>),
    nested: &[Lookup],
    last: usize,
) -> Vec<u8> {
    let (kind, flags, rule) = rule;
    let mut subtables = vec![rule(first.len() + copies), single(last, OUT)];
    let mut lookups = Vec::new();
    let mut own = |(kind, flags, subtable): &Lookup| {
        subtables.push(subtable.clone());
        (*kind, *flags, subtables.len() - 1)
    };
    lookups.extend(first.iter().map(&mut own));
    lookups.extend(std::iter::repeat_n((kind, flags, 0), copies));
    lookups.extend(nested.iter().map(&mut own));
    lookups.push((1, 0, 1));
    let in_calt = (0..first.len() + copies).chain([lookups.len() - 1]);

    let mut g = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let script_list = g.len();
    g.extend([0, 1, b'D', b'F', b'L', b'T', 0, 8, 0, 4, 0, 0]);
    g.extend([0, 0, 0xFF, 0xFF, 0, 1, 0, 0]);
    let feature_list = g.len();
    g.extend([0, 1, b'c', b'a', b'l', b't', 0, 8, 0, 0]);
    g.extend(be16(first.len() + copies + 1));
    in_calt.for_each(|i| g.extend(be16(i)));
    let lookup_list = g.len();
    // The lookup tables follow their offsets, then the subtables.
    let tables = 2 + 2 * lookups.len();
    let mut at = tables + 8 * lookups.len();
    let offsets: Vec<usize> = subtables
        .iter()
        .map(|subtable| {
            at += subtable.len();
            at - subtable.len()
        })
        .collect();
    g.extend(be16(lookups.len()));
    (0..lookups.len()).for_each(|i| g.extend(be16(tables + 8 * i)));
    for (i, &(kind, flags, subtable)) in lookups.iter().enumerate() {
        g.extend([be16(kind), be16(flags), be16(1)].concat());
        g.extend(be16(offsets[subtable] - (tables + 8 * i)));
    }
    subtables.iter().for_each(|subtable| g.extend(subtable));
    g[4..6].copy_from_slice(&be16(script_list));
    g[6..8].copy_from_slice(&be16(feature_list));
    g[8..10].copy_from_slice(&be16(lookup_list));
    g
}

fn shaped_spending(gsub: &[u8], text: &str) -> (Vec<u32>, aterm_font::Spent) {
    use aterm_font::shape::{ShapeFace, UnicodeBuffer, shape};
    let cmap = crate::gpos_shaping::cmap_format4(&[
        (u16::from(b'B'), B as u16),
        (u16::from(b'm'), M as u16),
        (u16::from(b'x'), X as u16),
    ]);
    let mut gdef = vec![0, 1, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 7];
    // Glyph classes from `B`: base, mark, base, ligature, base, ligature, base.
    [1, 3, 1, 2, 1, 2, 1]
        .iter()
        .for_each(|&class| gdef.extend(be16(class)));
    let bytes = crate::never_panic::sfnt(8, &[(b"cmap", &cmap), (b"GDEF", &gdef), (b"GSUB", gsub)]);
    let face = ShapeFace::new(Face::parse(&bytes, 0).expect("the synthetic face parses"));
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    let _ = aterm_font::take_spent();
    let glyphs = shape(&face, &[], buffer)
        .glyph_infos()
        .iter()
        .map(|info| info.glyph_id)
        .collect();
    (glyphs, aterm_font::take_spent())
}

/// Shapes `text` with `gsub(2)`, then `gsub(2_000)` (the rule's copies), checking that every
/// glyph `walked` counts is charged, that two copies run within the budget and walk the run,
/// and that 2,000 stop within twice the budget, before the last lookup.
fn charges_what_it_walks(
    gsub: impl Fn(usize) -> Vec<u8>,
    text: &str,
    last: usize,
    walked: impl Fn(&aterm_font::Spent) -> u64,
) {
    let run = text.chars().count() as u64;
    let budget = run * 16 * 1024;
    let last_glyph = |glyphs: &[u32]| {
        let at = glyphs
            .iter()
            .rposition(|&g| [last, OUT].contains(&(g as usize)));
        glyphs[at.expect("the last lookup's glyph is there")] as usize
    };
    let (glyphs, spent) = shaped_spending(&gsub(2), text);
    assert_eq!(
        last_glyph(&glyphs),
        OUT,
        "within the budget every lookup runs"
    );
    assert!(
        walked(&spent) > 32 * run,
        "the rules walk the run: {spent:?}"
    );
    assert!(
        walked(&spent) <= spent.layout_ops,
        "every glyph walked is charged: {spent:?}"
    );
    let (glyphs, spent) = shaped_spending(&gsub(2_000), text);
    assert!(
        walked(&spent) <= spent.layout_ops,
        "every glyph walked is charged: {spent:?}"
    );
    assert!(
        (budget..2 * budget).contains(&spent.layout_ops),
        "spent {spent:?} of {budget}"
    );
    assert_eq!(
        last_glyph(&glyphs),
        last,
        "past the budget the last lookup does not apply"
    );
}

#[test]
fn a_rule_moving_between_its_positions_spends_the_layout_budget() {
    // Records alternating between two `B`s that the match paired across 254 marks, so each
    // moves those marks to the output or back. Unmetered, 2,000 copies move 33M glyphs.
    let alternating = |nested| {
        context(
            2,
            B,
            &(0..64).map(|k| (1 - k % 2, nested)).collect::<Vec<_>>(),
        )
    };
    charges_what_it_walks(
        |copies| {
            let nothing = (1, 0, single(0, 0));
            repeated_rule_gsub(&[], copies, (5, IGNORE_MARKS, &alternating), &[nothing], B)
        },
        &("B".to_owned() + &"m".repeat(254) + "B"),
        B,
        |spent| spent.glyphs_moved,
    );
    // Records all at a `B` that a multiple substitution grows into `B m`: each rewinds past the
    // input read so far, which shifts the rest of the input forward.
    let regrowing = |nested| context(1, B, &[(0, nested); 64]);
    charges_what_it_walks(
        |copies| {
            let grow = (2, 0, multiple(B, &[B, M]));
            repeated_rule_gsub(&[], copies, (5, 0, &regrowing), &[grow], X)
        },
        &("B".to_owned() + &"x".repeat(255)),
        X,
        |spent| spent.glyphs_moved,
    );
}

#[test]
fn walks_over_a_ligatures_marks_spend_the_layout_budget() {
    // `B`, 253 marks and `B` ligate into `L` with the marks after it, then `x`.
    let text = "B".to_owned() + &"m".repeat(253) + "Bx";
    let first = [(4, IGNORE_MARKS, ligature(B, &[B], L))];
    // Records cycling `L` to 16 `Z0`s and `L`, those to `Z` (a ligature of 16 components,
    // which its 4 bits store as 0), and `Z` and `L` back to `L`, which renumbers every mark
    // after `L` and leaves their numbers as they were.
    let cycling = |nested| {
        let records: Vec<_> = (0..192).map(|k| (0, nested + k % 3)).collect();
        context(1, L, &records)
    };
    charges_what_it_walks(
        |copies| {
            let nested = [
                (2, 0, multiple(L, &[[Z0; 16].as_slice(), &[L]].concat())),
                (4, 0, ligature(Z0, &[Z0; 15], Z)),
                (4, 0, ligature(Z, &[L], L)),
            ];
            repeated_rule_gsub(&first, copies, (5, 0, &cycling), &nested, X)
        },
        &text,
        X,
        |spent| spent.marks_walked,
    );
    // Records at the last mark, before `x`, trying to ligate it with `x` past the marks: the
    // mark belongs to `L` and `x` does not, so the match walks back over every mark to `L` to
    // see whether it may be skipped (it may not).
    let at_the_last_mark = |nested| chain(M, X, &[(0, nested); 64]);
    charges_what_it_walks(
        |copies| {
            let past_the_marks = (4, IGNORE_MARKS, ligature(M, &[X], L));
            repeated_rule_gsub(
                &first,
                copies,
                (6, 0, &at_the_last_mark),
                &[past_the_marks],
                X,
            )
        },
        &text,
        X,
        |spent| spent.marks_walked,
    );
}
