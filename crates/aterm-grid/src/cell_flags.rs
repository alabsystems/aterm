// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Cell attribute flags (16-bit packed bitfield).

/// Cell flags packed into the Cell's flags field.
///
/// The Cell struct stores flags in 16 bits.
///
/// ## Bit allocation
/// - Bits 0-7: Visual attributes (bold, dim, italic, underline, blink, inverse, hidden, strikethrough)
/// - Bit 8: Double underline
/// - Bit 9: Wide character
/// - Bit 10: Wide continuation / Protected (shared bit - mutually exclusive)
/// - Bit 11: Superscript (SGR 73)
/// - Bit 12: Subscript (SGR 74)
/// - Bit 11+12: Overline (SGR 53) - combo encoding, mutually exclusive with super/subscript
/// - Bit 13: Curly underline
/// - Bit 14: USES_STYLE_ID (colors field stores a StyleId)
/// - Bit 15: COMPLEX (char_data is overflow table index)
///
/// Note: WIDE_CONTINUATION and PROTECTED share the same bit. Wide continuation
/// cells (spacers after wide characters) cannot be protected independently;
/// protection applies to the main wide character cell.
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
    /// Wide character continuation (spacer cell).
    /// Shares bit with PROTECTED - mutually exclusive.
    pub const WIDE_CONTINUATION: Self = Self(1 << 10);
    /// Protected from selective erase (DECSCA).
    /// Shares bit with WIDE_CONTINUATION - mutually exclusive.
    /// A non-wide cell uses this for protection status.
    pub const PROTECTED: Self = Self(1 << 10);
    /// Superscript text (SGR 73).
    pub const SUPERSCRIPT: Self = Self(1 << 11);
    /// Subscript text (SGR 74).
    pub const SUBSCRIPT: Self = Self(1 << 12);
    /// Overline text (SGR 53) - encoded as SUPERSCRIPT | SUBSCRIPT.
    /// Mutually exclusive with SUPERSCRIPT and SUBSCRIPT (same combination
    /// encoding pattern as DOTTED_UNDERLINE and DASHED_UNDERLINE).
    pub const OVERLINE: Self = Self((1 << 11) | (1 << 12)); // SUPERSCRIPT | SUBSCRIPT
    /// Curly underline.
    pub const CURLY_UNDERLINE: Self = Self(1 << 13);

    // Underline style encoding for cells:
    // - UNDERLINE alone = single underline
    // - DOUBLE_UNDERLINE alone = double underline
    // - CURLY_UNDERLINE alone = curly underline
    // - UNDERLINE + CURLY_UNDERLINE = dotted underline (SGR 4:4)
    // - DOUBLE_UNDERLINE + CURLY_UNDERLINE = dashed underline (SGR 4:5)
    // These combinations use bitwise OR of existing flags to encode additional styles.

    /// Dotted underline (SGR 4:4) - encoded as UNDERLINE | CURLY_UNDERLINE.
    pub const DOTTED_UNDERLINE: Self = Self((1 << 3) | (1 << 13)); // UNDERLINE | CURLY_UNDERLINE
    /// Dashed underline (SGR 4:5) - encoded as DOUBLE_UNDERLINE | CURLY_UNDERLINE.
    pub const DASHED_UNDERLINE: Self = Self((1 << 8) | (1 << 13)); // DOUBLE_UNDERLINE | CURLY_UNDERLINE

    /// Cell uses StyleId instead of inline colors.
    /// When set, the colors field stores a StyleId in its low 16 bits.
    pub const USES_STYLE_ID: Self = Self(1 << 14);
    /// Complex character - char_data is an index into the overflow string table.
    pub const COMPLEX: Self = Self(1 << 15);

    // Alacritty compatibility aliases
    // These alternative names remain available only for consumers that opt into
    // the compatibility surface explicitly.

    /// Alias for [`WIDE`](Self::WIDE) (Alacritty compatibility).
    #[cfg(feature = "alacritty-compat")]
    pub const WIDE_CHAR: Self = Self::WIDE;
    /// Alias for [`WIDE_CONTINUATION`](Self::WIDE_CONTINUATION) (Alacritty compatibility).
    /// This is the spacer cell after a wide character.
    #[cfg(feature = "alacritty-compat")]
    pub const WIDE_CHAR_SPACER: Self = Self::WIDE_CONTINUATION;
    /// Alias for [`STRIKETHROUGH`](Self::STRIKETHROUGH) (Alacritty compatibility).
    #[cfg(feature = "alacritty-compat")]
    pub const STRIKEOUT: Self = Self::STRIKETHROUGH;
    /// Alias for [`CURLY_UNDERLINE`](Self::CURLY_UNDERLINE) (Alacritty compatibility).
    #[cfg(feature = "alacritty-compat")]
    pub const UNDERCURL: Self = Self::CURLY_UNDERLINE;
    /// Combined DIM and BOLD flags (Alacritty compatibility).
    /// Some renderers handle dim+bold specially.
    #[cfg(feature = "alacritty-compat")]
    pub const DIM_BOLD: Self = Self((1 << 0) | (1 << 1)); // BOLD | DIM
    /// Combined BOLD and ITALIC flags (Alacritty compatibility).
    #[cfg(feature = "alacritty-compat")]
    pub const BOLD_ITALIC: Self = Self((1 << 0) | (1 << 2)); // BOLD | ITALIC
    /// Leading wide char spacer (Alacritty compatibility).
    /// Alias for WIDE_CONTINUATION — placed at end-of-line before a wrapped wide char.
    #[cfg(feature = "alacritty-compat")]
    pub const LEADING_WIDE_CHAR_SPACER: Self = Self::WIDE_CONTINUATION;
    /// All underline style flags combined (Alacritty compatibility).
    #[cfg(feature = "alacritty-compat")]
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

    /// Check if this cell uses StyleId instead of inline colors.
    #[must_use]
    #[inline]
    pub const fn uses_style_id(&self) -> bool {
        (self.0 & Self::USES_STYLE_ID.0) != 0
    }

    /// Get only the core flags (excluding COMPLEX).
    #[must_use]
    #[inline]
    pub const fn core_flags(&self) -> Self {
        Self(self.0 & Self::VISUAL_FLAGS_MASK)
    }

    /// Mask for extended flags (bits 11-13) that were previously in CellExtra.
    /// These are now stored directly in Cell.
    pub const EXTENDED_FLAGS_MASK: u16 = 0x3800; // bits 11-13

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

    /// Mask of the rendition a wide character's continuation spacer inherits
    /// from its lead: bits 0-8 and 11-13, every visual attribute and nothing else.
    ///
    /// Deliberately excluded, each for its own reason:
    /// - `WIDE` (bit 9) — the spacer is the tail of the pair, never a lead;
    /// - `WIDE_CONTINUATION`/`PROTECTED` (bit 10) — the spacer's own role bit, and
    ///   it aliases PROTECTED, so a blind union of the lead's flags would smuggle
    ///   DECSCA protection into a cell that cannot carry it independently;
    /// - `USES_STYLE_ID` (bit 14) and `COMPLEX` (bit 15) — storage discriminants of
    ///   the spacer's OWN colors/char fields, which its constructor owns.
    pub const SPACER_RENDITION_MASK: u16 =
        Self::VISUAL_FLAGS_MASK & !(Self::WIDE.0 | Self::WIDE_CONTINUATION.0);

    /// The flags for the continuation spacer of a wide character written with `self`.
    ///
    /// **The law: a rendition belongs to the CHARACTER, not to the column.** ECMA-48
    /// SGR sets an attribute on a character, and a double-width character is one
    /// character occupying two columns — there is no such thing as half a character
    /// carrying half a rendition. The spacer therefore inherits the lead's visual
    /// attributes and keeps only its own role bit.
    ///
    /// Measured reason (2026-09-16): a spacer built with `WIDE_CONTINUATION` alone
    /// discarded every rendition bit, and under SGR 7 that erased the right half of
    /// every double-width glyph. `resolve_both` swaps fg/bg for the lead only, so
    /// `lead.resolved_fg == spacer.resolved_bg` for EVERY colour pair — an identity,
    /// not a coincidence — and the glyph's spilled right half was composited
    /// invisibly. `\033[7m[漢字]` on a 9x17 cell measured 153 of 153 pixels of flat
    /// `#111318` in each spacer, distinct=1, zero glyph ink; the same line under
    /// `\033[31;44;7m` measured 153/153 of the un-swapped blue `#3b8eea`. A CJK user
    /// could not read a `less` search match. Underline, strikethrough and overline
    /// rules gapped under every right half for the same reason.
    #[must_use]
    #[inline]
    pub const fn wide_continuation_of(self) -> Self {
        Self((self.0 & Self::SPACER_RENDITION_MASK) | Self::WIDE_CONTINUATION.0)
    }
}

// Standard library bitwise operator implementations for Alacritty compatibility.
// These allow using `flags & Flags::DIM` and similar patterns.

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

    // ---- Bit layout ----
    //
    // Persisted: checkpoints and the render path read these bits raw, so every
    // position is pinned. One row per former `*_flag_bit`, combo and mask test.

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
            ("USES_STYLE_ID", CellFlags::USES_STYLE_ID.bits(), 1 << 14),
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
        // One loop in place of the one-case tests of default/empty, from_bits,
        // contains, intersects, union, difference, insert/remove, is_complex,
        // uses_style_id, core_flags, extended_flags, has_extended_flags and Copy:
        // each method is checked against the u16 arithmetic it stands for, for
        // every pair drawn from the sixteen single bits, the combos, the masks,
        // a few mixes, 0 and 0xFFFF.
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
            (CellFlags::BOLD | CellFlags::COMPLEX | CellFlags::USES_STYLE_ID).bits(),
            (CellFlags::BOLD | CellFlags::SUPERSCRIPT | CellFlags::CURLY_UNDERLINE).bits(),
        ]);

        for &a in &values {
            let f = CellFlags::from_bits(a);
            let copy = f;
            assert_eq!(copy, f, "{a:#06x}: Copy");
            assert_eq!(f.bits(), a, "{a:#06x}: from_bits round trip");
            assert_eq!(f.is_empty(), a == 0, "{a:#06x}: is_empty");
            assert_eq!(f.is_complex(), a & 0x8000 != 0, "{a:#06x}: is_complex");
            assert_eq!(
                f.uses_style_id(),
                a & 0x4000 != 0,
                "{a:#06x}: uses_style_id"
            );
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
            }
        }
    }

    // ---- contains: the combo flags need every one of their bits ----

    #[test]
    fn contains_requires_all_bits() {
        let f = CellFlags::SUPERSCRIPT; // only bit 11
        // OVERLINE is bits 11+12 -- SUPERSCRIPT alone does not contain OVERLINE
        assert!(!f.contains(CellFlags::OVERLINE));
    }

    #[test]
    fn contains_combo_flag() {
        let f = CellFlags::OVERLINE;
        assert!(f.contains(CellFlags::SUPERSCRIPT));
        assert!(f.contains(CellFlags::SUBSCRIPT));
        assert!(f.contains(CellFlags::OVERLINE));
    }

    // ---- Bitwise operators (the std traits, for Alacritty compatibility) ----

    #[test]
    fn bitor_operator() {
        let f = CellFlags::BOLD | CellFlags::DIM;
        assert!(f.contains(CellFlags::BOLD));
        assert!(f.contains(CellFlags::DIM));
    }

    #[test]
    fn bitand_operator() {
        let f = (CellFlags::BOLD | CellFlags::DIM) & CellFlags::BOLD;
        assert!(f.contains(CellFlags::BOLD));
        assert!(!f.contains(CellFlags::DIM));
    }

    #[test]
    fn bitand_disjoint_is_empty() {
        let f = CellFlags::BOLD & CellFlags::DIM;
        assert!(f.is_empty());
    }

    #[test]
    fn not_operator() {
        let f = !CellFlags::empty();
        assert_eq!(f.bits(), 0xFFFF);
        let f2 = !f;
        assert!(f2.is_empty());
    }

    #[test]
    fn bitor_assign_operator() {
        let mut f = CellFlags::BOLD;
        f |= CellFlags::ITALIC;
        assert!(f.contains(CellFlags::BOLD));
        assert!(f.contains(CellFlags::ITALIC));
    }

    #[test]
    fn bitand_assign_operator() {
        let mut f = CellFlags::BOLD | CellFlags::ITALIC;
        f &= CellFlags::BOLD;
        assert!(f.contains(CellFlags::BOLD));
        assert!(!f.contains(CellFlags::ITALIC));
    }
}
