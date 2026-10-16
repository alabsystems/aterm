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

fn index(items: &[Vec<u8>], cff2: bool) -> Vec<u8> {
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
