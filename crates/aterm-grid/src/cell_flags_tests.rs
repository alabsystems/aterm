// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn bit_layout_is_pinned() {
    let rows: &[(&str, u16, u16)] = &[
        ("BOLD", CellFlags::BOLD.bits(), 1 << 0),
        ("DIM", CellFlags::DIM.bits(), 1 << 1),
        ("ITALIC", CellFlags::ITALIC.bits(), 1 << 2),
        ("UNDERLINE", CellFlags::UNDERLINE.bits(), 1 << 3),
        ("BLINK", CellFlags::BLINK.bits(), 1 << 4),
        ("INVERSE", CellFlags::INVERSE.bits(), 1 << 5),
        ("HIDDEN", CellFlags::HIDDEN.bits(), 1 << 6),
        ("STRIKETHROUGH", CellFlags::STRIKETHROUGH.bits(), 1 << 7),
        (
            "DOUBLE_UNDERLINE",
            CellFlags::DOUBLE_UNDERLINE.bits(),
            1 << 8,
        ),
        ("WIDE", CellFlags::WIDE.bits(), 1 << 9),
        (
            "WIDE_CONTINUATION",
            CellFlags::WIDE_CONTINUATION.bits(),
            1 << 10,
        ),
        ("PROTECTED", CellFlags::PROTECTED.bits(), 1 << 10),
        ("SUPERSCRIPT", CellFlags::SUPERSCRIPT.bits(), 1 << 11),
        ("SUBSCRIPT", CellFlags::SUBSCRIPT.bits(), 1 << 12),
        (
            "CURLY_UNDERLINE",
            CellFlags::CURLY_UNDERLINE.bits(),
            1 << 13,
        ),
        ("COMPLEX", CellFlags::COMPLEX.bits(), 1 << 15),
        (
            "OVERLINE = SUPERSCRIPT | SUBSCRIPT",
            CellFlags::OVERLINE.bits(),
            (1 << 11) | (1 << 12),
        ),
        (
            "DOTTED_UNDERLINE = UNDERLINE | CURLY_UNDERLINE",
            CellFlags::DOTTED_UNDERLINE.bits(),
            (1 << 3) | (1 << 13),
        ),
        (
            "DASHED_UNDERLINE = DOUBLE_UNDERLINE | CURLY_UNDERLINE",
            CellFlags::DASHED_UNDERLINE.bits(),
            (1 << 8) | (1 << 13),
        ),
        (
            "EXTENDED_FLAGS_MASK = bits 11-13",
            CellFlags::EXTENDED_FLAGS_MASK,
            0x3800,
        ),
        (
            "VISUAL_FLAGS_MASK = bits 0-13",
            CellFlags::VISUAL_FLAGS_MASK,
            0x3FFF,
        ),
    ];
    for &(name, bits, want) in rows {
        assert_eq!(bits, want, "{name}");
    }
}

#[test]
fn methods_agree_with_raw_bit_arithmetic() {
    assert!(CellFlags::default().is_empty());
    assert_eq!(CellFlags::empty(), CellFlags::default());

    let mut values: Vec<u16> = (0..16).map(|bit| 1u16 << bit).collect();
    values.extend([
        0,
        0xFFFF,
        CellFlags::OVERLINE.bits(),
        CellFlags::DOTTED_UNDERLINE.bits(),
        CellFlags::DASHED_UNDERLINE.bits(),
        CellFlags::EXTENDED_FLAGS_MASK,
        CellFlags::VISUAL_FLAGS_MASK,
        0x00FF,
        (CellFlags::BOLD | CellFlags::ITALIC).bits(),
        (CellFlags::BOLD | CellFlags::COMPLEX).bits(),
        (CellFlags::BOLD | CellFlags::SUPERSCRIPT | CellFlags::CURLY_UNDERLINE).bits(),
    ]);

    for &a in &values {
        let f = CellFlags::from_bits(a);
        let copy = f;
        assert_eq!(copy, f, "{a:#06x}: Copy");
        assert_eq!(f.bits(), a, "{a:#06x}: from_bits round trip");
        assert_eq!((!f).bits(), !a, "{a:#06x}: !");
        assert_eq!(f.is_empty(), a == 0, "{a:#06x}: is_empty");
        assert_eq!(f.is_complex(), a & 0x8000 != 0, "{a:#06x}: is_complex");
        assert_eq!(f.core_flags().bits(), a & 0x3FFF, "{a:#06x}: core_flags");
        assert_eq!(
            f.extended_flags().bits(),
            a & 0x3800,
            "{a:#06x}: extended_flags"
        );
        assert_eq!(
            f.has_extended_flags(),
            a & 0x3800 != 0,
            "{a:#06x}: has_extended_flags"
        );
        for &b in &values {
            let g = CellFlags::from_bits(b);
            let pair = format!("{a:#06x}, {b:#06x}");
            assert_eq!(f.contains(g), a & b == b, "{pair}: contains");
            assert_eq!(f.intersects(g), a & b != 0, "{pair}: intersects");
            assert_eq!(f.union(g).bits(), a | b, "{pair}: union");
            assert_eq!(f.difference(g).bits(), a & !b, "{pair}: difference");
            let mut inserted = f;
            inserted.insert(g);
            assert_eq!(inserted.bits(), a | b, "{pair}: insert");
            let mut removed = f;
            removed.remove(g);
            assert_eq!(removed.bits(), a & !b, "{pair}: remove");
            assert_eq!((f | g).bits(), a | b, "{pair}: |");
            assert_eq!((f & g).bits(), a & b, "{pair}: &");
            let mut or_assigned = f;
            or_assigned |= g;
            assert_eq!(or_assigned.bits(), a | b, "{pair}: |=");
            let mut and_assigned = f;
            and_assigned &= g;
            assert_eq!(and_assigned.bits(), a & b, "{pair}: &=");
        }
    }
}
