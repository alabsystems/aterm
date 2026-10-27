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
