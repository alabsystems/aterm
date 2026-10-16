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
    // A shared endpoint already repeats a glyph. Repeating a wide range
    // can multiply traversal work without adding any covered glyphs.
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
