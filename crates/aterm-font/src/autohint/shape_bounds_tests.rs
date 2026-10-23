// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::parser::FromSlice;

fn coverage2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
    let mut bytes = 2u16.to_be_bytes().to_vec();
    bytes.extend_from_slice(&u16::try_from(ranges.len()).unwrap().to_be_bytes());
    for (start, end, index) in ranges {
        bytes.extend_from_slice(&start.to_be_bytes());
        bytes.extend_from_slice(&end.to_be_bytes());
        bytes.extend_from_slice(&index.to_be_bytes());
    }
    bytes
}

#[test]
fn ordered_coverage_ranges_are_traversed_once() {
    let bytes = coverage2(&[(0, 0, 0), (2, 3, 1), (u16::MAX, u16::MAX, 3)]);
    let coverage = Coverage::parse(&bytes).unwrap();
    assert_eq!(
        coverage_glyphs(coverage).collect::<Vec<_>>(),
        [0, 2, 3, u16::MAX]
    );
    for (index, glyph) in [0, 2, 3, u16::MAX].into_iter().enumerate() {
        assert_eq!(
            coverage.get(crate::GlyphId(glyph)),
            Some(u16::try_from(index).unwrap())
        );
    }
    assert_eq!(coverage.get(crate::GlyphId(1)), None);
}

#[test]
fn overlapping_coverage_ranges_are_rejected_before_traversal() {
    let bytes = coverage2(&[(1, 2, 0), (2, 3, 2)]);
    let coverage = Coverage::parse(&bytes).unwrap();
    assert!(coverage_glyphs(coverage).collect::<Vec<_>>().is_empty());
}

#[test]
fn reversed_coverage_ranges_are_rejected_before_traversal() {
    let bytes = coverage2(&[(2, 1, 0)]);
    let coverage = Coverage::parse(&bytes).unwrap();
    assert!(coverage_glyphs(coverage).collect::<Vec<_>>().is_empty());
}

#[test]
fn unordered_coverage_ranges_are_rejected_before_traversal() {
    let bytes = coverage2(&[(3, 4, 0), (0, 1, 2)]);
    let coverage = Coverage::parse(&bytes).unwrap();
    assert!(coverage_glyphs(coverage).collect::<Vec<_>>().is_empty());
}

fn aliased_face(lookups: usize, subtables: usize) -> Vec<u8> {
    let be16 = |v: usize| u16::try_from(v).unwrap().to_be_bytes();
    let be32 = |v: usize| u32::try_from(v).unwrap().to_be_bytes();
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
    let mut head = vec![0u8; 54];
    head[18..20].copy_from_slice(&be16(1000));
    let mut hhea = vec![0u8; 36];
    hhea[34..36].copy_from_slice(&be16(1));
    let maxp = [0, 0, 0x50, 0, 0, 2];
    let mut tables: [(&[u8; 4], &[u8]); 7] = [
        (b"GSUB", &g),
        (b"glyf", &[0; 4]),
        (b"head", &head),
        (b"hhea", &hhea),
        (b"hmtx", &[0; 6]),
        (b"loca", &[0; 6]),
        (b"maxp", &maxp),
    ];
    tables.sort_by_key(|(tag, _)| **tag);
    let mut font = vec![0, 1, 0, 0];
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

fn output_style(face: &[u8]) -> (bool, Option<&'static str>) {
    let font = HintFont::new(face, 0).expect("the synthetic face reads");
    let shaper = Shaper::new(&font);
    let map = crate::autohint::style::GlyphStyleMap::new(font.glyph_count(), &shaper);
    let style = map.style(GlyphId::new(1)).and_then(|s| s.style_class());
    (shaper.refused(), style.map(|c| c.script.name))
}

#[test]
fn aliased_gsub_walks_stay_within_the_budget() {
    assert_eq!(output_style(&aliased_face(10, 10)), (false, Some("Latin")));
    assert_eq!(
        output_style(&aliased_face(1_000, 1_000)),
        (true, Some("CJKV ideographs"))
    );
}
