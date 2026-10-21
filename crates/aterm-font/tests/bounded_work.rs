// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Acyclic font programs can expand exponentially without exceeding their
//! nesting limit. The sink's finite guard stops the old parser before that
//! expansion runs away; the parser must refuse before the guard is reached.

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
        // A budget belongs to one query, not the face or the caller.
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

/// The measurement seam (`take_spent`, debug builds; the recorder's
/// `budgets` mode reads it) counts the budget's own steps: an ordinary glyph
/// reads what it took, a refused expansion more than the budget, and each
/// read starts the count again.
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
    // One composite level more is one glyph entry per component, and the
    // child's own steps twice.
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

/// Work exhaustion must not change gvar's existing answer for an ordinary
/// composite whose nonempty component is followed by a zero-contour one.
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
        // An empty item variation store, valid at the default instance.
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

/// A `GSUB` 1.0 whose one `DFLT` script's default language names feature
/// `liga`, which names lookups `0..lookups`. Every lookup offset names one
/// type-1 lookup of `subtables` subtables, and every subtable offset names one
/// single substitution (format 1, delta +1) covering glyph 0. Well-formed
/// fonts never alias offsets like this; a crafted one can, and each aliased
/// pair costs the shaper a subtable parse: `lookups` x `subtables` of them.
fn aliased_gsub(lookups: usize, subtables: usize) -> Vec<u8> {
    let be16 = |v: usize| u16::try_from(v).unwrap().to_be_bytes();
    // Header (offsets patched below), then the ScriptList at 10: DFLT at +8,
    // its default LangSys at +4 naming feature 0.
    let mut g = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let script_list = g.len();
    g.extend([0, 1, b'D', b'F', b'L', b'T', 0, 8, 0, 4, 0, 0]);
    g.extend([0, 0, 0xFF, 0xFF, 0, 1, 0, 0]);
    // FeatureList: `liga` at +8, naming lookups 0..lookups.
    let feature_list = g.len();
    g.extend([0, 1, b'l', b'i', b'g', b'a', 0, 8, 0, 0]);
    g.extend(be16(lookups));
    (0..lookups).for_each(|i| g.extend(be16(i)));
    // LookupList: every offset names the one lookup after the array.
    let lookup_list = g.len();
    g.extend(be16(lookups));
    (0..lookups).for_each(|_| g.extend(be16(2 + 2 * lookups)));
    // The lookup: type 1, no flags, every subtable offset aliasing one.
    g.extend([0, 1, 0, 0]);
    g.extend(be16(subtables));
    (0..subtables).for_each(|_| g.extend(be16(6 + 2 * subtables)));
    // Single substitution format 1 (coverage at +6, delta +1), then its
    // coverage, format 1: glyph 0.
    g.extend([0, 1, 0, 6, 0, 1, 0, 1, 0, 1, 0, 0]);
    g[4..6].copy_from_slice(&be16(script_list));
    g[6..8].copy_from_slice(&be16(feature_list));
    g[8..10].copy_from_slice(&be16(lookup_list));
    g
}

/// The glyphs two characters shape to on `gsub`'s face, whose cmap-less
/// characters are glyph 0 until a substitution moves them.
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

/// Aliased `GSUB` offsets cannot multiply the shaper's parsing. A lookup
/// list whose 1,000 offsets name one lookup of 1,000 subtables that all name
/// one substitution asks for a million subtable parses from a 6 KB table
/// (32,000 x 32,000, 192 KB, took 27 s for one two-character shape before the
/// budget, measured 2026-10-19). The shaper spends at most aterm's own
/// budget, 8 units per byte of the table (`LayoutWork`), and drops the face's
/// `GSUB` once a parse runs past it, as HarfBuzz drops a table that fails its
/// sanitizer. The same structure at 10 x 10 is within the budget and
/// substitutes as before.
#[test]
fn aliased_gsub_lookups_are_parsed_within_a_budget() {
    assert_eq!(shaped(&aliased_gsub(10, 10)), [1, 1], "within the budget");
    assert_eq!(
        shaped(&aliased_gsub(1_000, 1_000)),
        [0, 0],
        "past the budget, GSUB is refused"
    );
}

/// A `GSUB` 1.0 whose `liga` names lookups 0 and 1, and whose lookup 2 no
/// feature names. Lookup 0 substitutes glyph 0 with glyph 1 (single, delta
/// +1). Lookup 1 is a context (format 3) over glyphs {0, 1} that applies
/// lookup 2 at its one position. Lookup 2 has `subtables` offsets aliasing
/// one single substitution (delta 0, so it changes nothing) whose coverage
/// lists glyphs `1..=coverage`: past the budget, it is only a nested lookup
/// that the shaper's parse refuses.
fn nested_gsub(subtables: usize, coverage: usize) -> Vec<u8> {
    let be16 = |v: usize| u16::try_from(v).unwrap().to_be_bytes();
    let mut g = vec![0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let script_list = g.len();
    g.extend([0, 1, b'D', b'F', b'L', b'T', 0, 8, 0, 4, 0, 0]);
    g.extend([0, 0, 0xFF, 0xFF, 0, 1, 0, 0]);
    // FeatureList: `liga` at +8, naming lookups 0 and 1.
    let feature_list = g.len();
    g.extend([0, 1, b'l', b'i', b'g', b'a', 0, 8, 0, 0, 0, 2, 0, 0, 0, 1]);
    // LookupList: lookup 0 at +8, 1 at +28, 2 at +56.
    let lookup_list = g.len();
    g.extend([0, 3, 0, 8, 0, 28, 0, 56]);
    // Lookup 0: one single substitution (coverage at +6, delta +1), its
    // coverage glyph 0.
    g.extend([0, 1, 0, 0, 0, 1, 0, 8]);
    g.extend([0, 1, 0, 6, 0, 1]);
    g.extend([0, 1, 0, 1, 0, 0]);
    // Lookup 1: a context format 3 of one position (coverage at +12), whose
    // one record applies lookup 2 at position 0; its coverage glyphs 0, 1.
    g.extend([0, 5, 0, 0, 0, 1, 0, 8]);
    g.extend([0, 3, 0, 1, 0, 1, 0, 12, 0, 0, 0, 2]);
    g.extend([0, 1, 0, 2, 0, 0, 0, 1]);
    assert_eq!(g.len() - lookup_list, 56, "lookup 2's offset");
    // Lookup 2: every subtable offset aliasing one single substitution
    // (coverage at +6, delta 0), its coverage glyphs 1..=coverage.
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

/// What one character shapes to, twice, on one face of `gsub`.
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

/// A refused `GSUB` is refused before any of it applies, whichever lookup
/// runs past the budget, so a face shapes the same text the same way every
/// time. Lookup 2 of a 6 KB table names 2,000 x 1,000 entries; no feature
/// names it, so parsing only the lookups a plan names would refuse it in the
/// middle of the first shape, after lookup 0 had applied (glyph 1), and
/// every shape after it would get no `GSUB` (glyph 0). At 2 x 4 the same
/// table is within the budget and substitutes every time.
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
