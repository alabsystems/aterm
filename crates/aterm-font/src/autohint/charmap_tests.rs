// Copyright (c) 2019 Fontations Developers
// Modifications Copyright 2026 Andrew Yates
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Derived from skrifa 0.46.2 src/charmap.rs and read-fonts 0.43.2
// src/tables/cmap.rs and generated/generated_cmap.rs, and modified by the
// aterm project in 2026: the one `cmap` subtable skrifa picks (symbol, else
// the widest Unicode one, formats 4, 12 and 13), its lookup and its
// enumeration, every read checked; variation sequences dropped.
// See ../../LICENSE-FONTATIONS for the upstream license.

use super::*;

fn cmap_with(records: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(&0u16.to_be_bytes());
    t.extend_from_slice(&(records.len() as u16).to_be_bytes());
    let mut offset = 4 + records.len() * 8;
    let mut bodies = Vec::new();
    for (platform, encoding, body) in records {
        t.extend_from_slice(&platform.to_be_bytes());
        t.extend_from_slice(&encoding.to_be_bytes());
        t.extend_from_slice(&(offset as u32).to_be_bytes());
        offset += body.len();
        bodies.extend_from_slice(body);
    }
    t.extend_from_slice(&bodies);
    t
}

/// A format 12 subtable of `(start, end, glyph)` groups.
fn format12(groups: &[(u32, u32, u32)]) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(&12u16.to_be_bytes());
    t.extend_from_slice(&0u16.to_be_bytes());
    t.extend_from_slice(&((16 + groups.len() * 12) as u32).to_be_bytes());
    t.extend_from_slice(&0u32.to_be_bytes());
    t.extend_from_slice(&(groups.len() as u32).to_be_bytes());
    for (s, e, g) in groups {
        for v in [s, e, g] {
            t.extend_from_slice(&v.to_be_bytes());
        }
    }
    t
}

/// A format 4 subtable of `(start, end, delta)` segments plus the 0xFFFF terminator.
fn format4(segments: &[(u16, u16, i16)]) -> Vec<u8> {
    let mut segs: Vec<(u16, u16, i16)> = segments.to_vec();
    segs.push((0xFFFF, 0xFFFF, 1));
    let n = segs.len() as u16;
    let mut t = Vec::new();
    for v in [4u16, 0, 0, n * 2, 0, 0, 0] {
        t.extend_from_slice(&v.to_be_bytes());
    }
    for s in &segs {
        t.extend_from_slice(&s.1.to_be_bytes());
    }
    t.extend_from_slice(&0u16.to_be_bytes());
    for s in &segs {
        t.extend_from_slice(&s.0.to_be_bytes());
    }
    for s in &segs {
        t.extend_from_slice(&s.2.to_be_bytes());
    }
    for _ in &segs {
        t.extend_from_slice(&0u16.to_be_bytes());
    }
    let len = t.len() as u16;
    t[2..4].copy_from_slice(&len.to_be_bytes());
    t
}

/// The UCS-4 subtable wins over the BMP one, whatever their order, and only it answers.
#[test]
fn picks_the_full_unicode_subtable() {
    let bmp = format4(&[(0x41, 0x42, 1)]);
    let full = format12(&[(0x41, 0x41, 7), (0x1F600, 0x1F601, 9)]);
    let cmap = cmap_with(&[(3, 1, bmp.clone()), (3, 10, full.clone())]);
    let map = Charmap::new(Some(&cmap), 100);
    assert_eq!(map.map('A'), Some(GlyphId::new(7)));
    assert_eq!(map.map('B'), None);
    assert_eq!(map.map(0x1F601u32), Some(GlyphId::new(10)));
    let cmap = cmap_with(&[(3, 10, full), (3, 1, bmp)]);
    assert_eq!(
        Charmap::new(Some(&cmap), 100).map('A'),
        Some(GlyphId::new(7))
    );
}

/// A symbol subtable wins, and answers Latin-1 from the F0xx block.
#[test]
fn symbol_subtable_maps_the_f000_block() {
    let sym = format4(&[(0xF041, 0xF041, 0x0FC4)]);
    let bmp = format4(&[(0x41, 0x41, 1)]);
    let cmap = cmap_with(&[(3, 1, bmp), (3, 0, sym)]);
    let map = Charmap::new(Some(&cmap), 100);
    assert!(map.is_symbol());
    assert_eq!(map.map('A'), Some(GlyphId::new(5)));
}

/// Enumeration skips `.notdef`, cuts format 12 groups to the glyph count, and never steps a backwards group back.
#[test]
fn mappings_follow_read_fonts() {
    let full = format12(&[(0x20, 0x22, 0), (0x30, 0x39, 95), (0x31, 0x36, 1)]);
    let cmap = cmap_with(&[(3, 10, full)]);
    let pairs: Vec<_> = Charmap::new(Some(&cmap), 100)
        .mappings()
        .map(|(c, g)| (c, g.to_u32()))
        .collect();
    assert_eq!(
        pairs,
        [
            (0x21, 1),
            (0x22, 2),
            (0x30, 95),
            (0x31, 96),
            (0x32, 97),
            (0x33, 98),
            (0x34, 99),
            (0x35, 5),
            (0x36, 6)
        ]
    );
    let bmp = format4(&[(0x41, 0x43, 1)]);
    let cmap = cmap_with(&[(0, 3, bmp)]);
    let pairs: Vec<_> = Charmap::new(Some(&cmap), 100)
        .mappings()
        .map(|(c, g)| (c, g.to_u32()))
        .collect();
    assert_eq!(pairs[..3], [(0x41, 0x42), (0x42, 0x43), (0x43, 0x44)]);
}
