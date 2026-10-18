// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cell attribute flags (16-bit packed bitfield).

/// Cell flags, packed into the Cell's 16-bit flags field.
///
/// ## Bit allocation
/// - Bits 0-7: visual attributes (bold, dim, italic, underline, blink, inverse,
///   hidden, strikethrough)
/// - Bit 8: double underline
/// - Bit 9: wide character
/// - Bit 10: wide continuation / protected (shared; a spacer's protection is its lead's)
/// - Bit 11: superscript (SGR 73)
/// - Bit 12: subscript (SGR 74)
/// - Bits 11+12: overline (SGR 53), exclusive with super/subscript
/// - Bit 13: curly underline
/// - Bit 14: reserved, never set (it once marked a style-interned cell;
///   `aterm-scrollback`'s `CellAttrs::VISUAL_FLAGS_MASK` still masks it)
/// - Bit 15: COMPLEX (char_data is an overflow table index)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct CellFlags(pub u16);

impl CellFlags {
    /// Bold text.
    pub const BOLD: Self = Self(1 << 0);
    /// Dim/faint text.
    pub const DIM: Self = Self(1 << 1);
    /// Italic text.
    pub const ITALIC: Self = Self(1 << 2);
    /// Underlined text.
    pub const UNDERLINE: Self = Self(1 << 3);
    /// Blinking text.
    pub const BLINK: Self = Self(1 << 4);
    /// Inverse video.
    pub const INVERSE: Self = Self(1 << 5);
    /// Hidden/invisible text.
    pub const HIDDEN: Self = Self(1 << 6);
    /// Strikethrough text.
    pub const STRIKETHROUGH: Self = Self(1 << 7);
    /// Double underline.
    pub const DOUBLE_UNDERLINE: Self = Self(1 << 8);
    /// Wide character (occupies 2 cells).
    pub const WIDE: Self = Self(1 << 9);
    /// Wide character continuation (spacer cell); shares bit 10 with PROTECTED.
    pub const WIDE_CONTINUATION: Self = Self(1 << 10);
    /// Protected from selective erase (DECSCA). Shares bit 10 with
    /// WIDE_CONTINUATION; only non-spacer cells use it for protection.
    pub const PROTECTED: Self = Self(1 << 10);
    /// Superscript text (SGR 73).
    pub const SUPERSCRIPT: Self = Self(1 << 11);
    /// Subscript text (SGR 74).
    pub const SUBSCRIPT: Self = Self(1 << 12);
    /// Overline text (SGR 53), encoded as SUPERSCRIPT | SUBSCRIPT, so exclusive
    /// with both.
    pub const OVERLINE: Self = Self((1 << 11) | (1 << 12));
    /// Curly underline.
    pub const CURLY_UNDERLINE: Self = Self(1 << 13);

    /// Dotted underline (SGR 4:4) - encoded as UNDERLINE | CURLY_UNDERLINE.
    pub const DOTTED_UNDERLINE: Self = Self((1 << 3) | (1 << 13));
    /// Dashed underline (SGR 4:5) - encoded as DOUBLE_UNDERLINE | CURLY_UNDERLINE.
    pub const DASHED_UNDERLINE: Self = Self((1 << 8) | (1 << 13));

    /// Complex character - char_data is an index into the overflow string table.
    pub const COMPLEX: Self = Self(1 << 15);

    /// All underline style flags combined.
    pub const ALL_UNDERLINES: Self = Self(
        Self::UNDERLINE.0
            | Self::DOUBLE_UNDERLINE.0
            | Self::CURLY_UNDERLINE.0
            | Self::DOTTED_UNDERLINE.0
            | Self::DASHED_UNDERLINE.0,
    );

    /// Empty flags.
    #[must_use]
    #[inline]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Check if flag is set (all bits in `other` must be present).
    #[must_use]
    #[inline]
    pub const fn contains(&self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Check if any flag in `other` is set.
    #[must_use]
    #[inline]
    pub const fn intersects(&self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    /// Set a flag.
    #[must_use]
    #[inline]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Clear a flag.
    #[must_use]
    #[inline]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Insert a flag (mutating).
    #[inline]
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    /// Remove a flag (mutating).
    #[inline]
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    /// Check if flags are empty.
    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// Get raw bits.
    #[must_use]
    #[inline]
    pub const fn bits(&self) -> u16 {
        self.0
    }

    /// Create from raw bits.
    #[must_use]
    #[inline]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// Mask for core visual flags (bits 0-13).
    pub const VISUAL_FLAGS_MASK: u16 = 0x3FFF;

    /// Check if this has the COMPLEX flag set.
    #[must_use]
    #[inline]
    pub const fn is_complex(&self) -> bool {
        (self.0 & Self::COMPLEX.0) != 0
    }

    /// Get only the core flags (bits 0-13: without the reserved bit 14 and
    /// COMPLEX).
    #[must_use]
    #[inline]
    #[cfg(test)]
    pub const fn core_flags(&self) -> Self {
        Self(self.0 & Self::VISUAL_FLAGS_MASK)
    }

    /// The SGR parameter selecting this rendition's underline style (`4`, or
    /// `4:2` to `4:5`), if it has one. The composite styles share bits with the
    /// singletons (DASHED = DOUBLE | CURLY, DOTTED = UNDERLINE | CURLY), so they
    /// are tested first. DECRQSS and the screen serializer both encode with it.
    #[must_use]
    pub const fn underline_sgr(self) -> Option<&'static str> {
        if self.contains(Self::DASHED_UNDERLINE) {
            Some("4:5")
        } else if self.contains(Self::DOTTED_UNDERLINE) {
            Some("4:4")
        } else if self.contains(Self::CURLY_UNDERLINE) {
            Some("4:3")
        } else if self.contains(Self::DOUBLE_UNDERLINE) {
            Some("4:2")
        } else if self.contains(Self::UNDERLINE) {
            Some("4")
        } else {
            None
        }
    }

    /// The SGR parameter for this rendition's overline (`53`), superscript
    /// (`73`) or subscript (`74`), if any. OVERLINE is SUPERSCRIPT | SUBSCRIPT,
    /// so it is tested first. DECRQSS and the screen serializer both encode
    /// with it.
    #[must_use]
    pub const fn overline_or_script_sgr(self) -> Option<&'static str> {
        if self.contains(Self::OVERLINE) {
            Some("53")
        } else if self.contains(Self::SUPERSCRIPT) {
            Some("73")
        } else if self.contains(Self::SUBSCRIPT) {
            Some("74")
        } else {
            None
        }
    }

    /// Mask for the extended flags (bits 11-13).
    pub const EXTENDED_FLAGS_MASK: u16 = 0x3800;

    /// Get only the extended flags (bits 11-13).
    #[must_use]
    #[inline]
    pub const fn extended_flags(&self) -> Self {
        Self(self.0 & Self::EXTENDED_FLAGS_MASK)
    }

    /// Check if this has any extended flags set.
    #[must_use]
    #[inline]
    pub const fn has_extended_flags(&self) -> bool {
        (self.0 & Self::EXTENDED_FLAGS_MASK) != 0
    }

    /// Rendition a wide character's continuation spacer inherits from its lead:
    /// bits 0-8 and 11-13, every visual attribute and nothing else.
    ///
    /// Excluded: `WIDE` (bit 9, the spacer is never a lead); bit 10 (the
    /// spacer's own role bit, which aliases PROTECTED); the reserved bit 14
    /// and `COMPLEX` (bit 15, a storage discriminant the spacer's constructor
    /// owns).
    pub const SPACER_RENDITION_MASK: u16 =
        Self::VISUAL_FLAGS_MASK & !(Self::WIDE.0 | Self::WIDE_CONTINUATION.0);

    /// The flags for the continuation spacer of a wide character written with `self`.
    ///
    /// A rendition belongs to the character, not the column: a double-width
    /// character is one character over two columns, so the spacer inherits the
    /// lead's visual attributes and keeps only its own role bit. Without this,
    /// SGR 7 (swapped for the lead only) paints the glyph's right half invisibly,
    /// and underline/strikethrough/overline gap under every right half.
    #[must_use]
    #[inline]
    pub const fn wide_continuation_of(self) -> Self {
        Self((self.0 & Self::SPACER_RENDITION_MASK) | Self::WIDE_CONTINUATION.0)
    }
}

// Bitwise operators (`flags & CellFlags::DIM`), for Alacritty-style callers.

impl std::ops::BitAnd for CellFlags {
    type Output = Self;

    #[inline]
    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl std::ops::BitOr for CellFlags {
    type Output = Self;

    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitAndAssign for CellFlags {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl std::ops::BitOrAssign for CellFlags {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::Not for CellFlags {
    type Output = Self;

    #[inline]
    fn not(self) -> Self::Output {
        Self(!self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bit layout: checkpoints and the render path read these bits raw, so every
    // position is pinned.

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
            // PROTECTED shares WIDE_CONTINUATION's bit.
            ("PROTECTED", CellFlags::PROTECTED.bits(), 1 << 10),
            ("SUPERSCRIPT", CellFlags::SUPERSCRIPT.bits(), 1 << 11),
            ("SUBSCRIPT", CellFlags::SUBSCRIPT.bits(), 1 << 12),
            (
                "CURLY_UNDERLINE",
                CellFlags::CURLY_UNDERLINE.bits(),
                1 << 13,
            ),
            ("COMPLEX", CellFlags::COMPLEX.bits(), 1 << 15),
            // Combo encodings: two bits each, no bit of their own.
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
            // Masks.
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

    // ---- Methods ----

    #[test]
    fn methods_agree_with_raw_bit_arithmetic() {
        // Each method is checked against the u16 arithmetic it stands for, over
        // every pair drawn from the single bits, combos, masks, a few mixes, 0
        // and 0xFFFF.
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
}
