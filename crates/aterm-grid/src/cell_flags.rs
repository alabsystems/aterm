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

    // ---- Default / empty state ----

    #[test]
    fn default_is_empty() {
        let f = CellFlags::default();
        assert!(f.is_empty());
        assert_eq!(f.bits(), 0);
    }

    #[test]
    fn empty_is_empty() {
        let f = CellFlags::empty();
        assert!(f.is_empty());
        assert_eq!(f, CellFlags::default());
    }

    #[test]
    fn from_bits_roundtrip() {
        let f = CellFlags::from_bits(0x00FF);
        assert_eq!(f.bits(), 0x00FF);
    }

    // ---- Individual flag constants ----

    #[test]
    fn bold_flag_bit() {
        assert_eq!(CellFlags::BOLD.bits(), 1 << 0);
    }

    #[test]
    fn dim_flag_bit() {
        assert_eq!(CellFlags::DIM.bits(), 1 << 1);
    }

    #[test]
    fn italic_flag_bit() {
        assert_eq!(CellFlags::ITALIC.bits(), 1 << 2);
    }

    #[test]
    fn underline_flag_bit() {
        assert_eq!(CellFlags::UNDERLINE.bits(), 1 << 3);
    }

    #[test]
    fn blink_flag_bit() {
        assert_eq!(CellFlags::BLINK.bits(), 1 << 4);
    }

    #[test]
    fn inverse_flag_bit() {
        assert_eq!(CellFlags::INVERSE.bits(), 1 << 5);
    }

    #[test]
    fn hidden_flag_bit() {
        assert_eq!(CellFlags::HIDDEN.bits(), 1 << 6);
    }

    #[test]
    fn strikethrough_flag_bit() {
        assert_eq!(CellFlags::STRIKETHROUGH.bits(), 1 << 7);
    }

    #[test]
    fn double_underline_flag_bit() {
        assert_eq!(CellFlags::DOUBLE_UNDERLINE.bits(), 1 << 8);
    }

    #[test]
    fn wide_flag_bit() {
        assert_eq!(CellFlags::WIDE.bits(), 1 << 9);
    }

    #[test]
    fn wide_continuation_shares_bit_with_protected() {
        assert_eq!(CellFlags::WIDE_CONTINUATION, CellFlags::PROTECTED);
        assert_eq!(CellFlags::WIDE_CONTINUATION.bits(), 1 << 10);
    }

    #[test]
    fn superscript_flag_bit() {
        assert_eq!(CellFlags::SUPERSCRIPT.bits(), 1 << 11);
    }

    #[test]
    fn subscript_flag_bit() {
        assert_eq!(CellFlags::SUBSCRIPT.bits(), 1 << 12);
    }

    #[test]
    fn curly_underline_flag_bit() {
        assert_eq!(CellFlags::CURLY_UNDERLINE.bits(), 1 << 13);
    }

    #[test]
    fn uses_style_id_flag_bit() {
        assert_eq!(CellFlags::USES_STYLE_ID.bits(), 1 << 14);
    }

    #[test]
    fn complex_flag_bit() {
        assert_eq!(CellFlags::COMPLEX.bits(), 1 << 15);
    }

    // ---- Combo-encoded flags ----

    #[test]
    fn overline_is_superscript_or_subscript() {
        assert_eq!(
            CellFlags::OVERLINE,
            CellFlags(CellFlags::SUPERSCRIPT.0 | CellFlags::SUBSCRIPT.0)
        );
    }

    #[test]
    fn dotted_underline_is_underline_or_curly() {
        assert_eq!(
            CellFlags::DOTTED_UNDERLINE,
            CellFlags(CellFlags::UNDERLINE.0 | CellFlags::CURLY_UNDERLINE.0)
        );
    }

    #[test]
    fn dashed_underline_is_double_or_curly() {
        assert_eq!(
            CellFlags::DASHED_UNDERLINE,
            CellFlags(CellFlags::DOUBLE_UNDERLINE.0 | CellFlags::CURLY_UNDERLINE.0)
        );
    }

    // ---- contains ----

    #[test]
    fn contains_single_flag() {
        let f = CellFlags::BOLD;
        assert!(f.contains(CellFlags::BOLD));
        assert!(!f.contains(CellFlags::ITALIC));
    }

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

    #[test]
    fn contains_empty_always_true() {
        let f = CellFlags::BOLD;
        assert!(f.contains(CellFlags::empty()));
        assert!(CellFlags::empty().contains(CellFlags::empty()));
    }

    // ---- intersects ----

    #[test]
    fn intersects_single_flag() {
        let f = CellFlags::BOLD;
        assert!(f.intersects(CellFlags::BOLD));
        assert!(!f.intersects(CellFlags::ITALIC));
    }

    #[test]
    fn intersects_partial_overlap() {
        let f = CellFlags::SUPERSCRIPT;
        // OVERLINE = SUPERSCRIPT | SUBSCRIPT -- shares SUPERSCRIPT bit
        assert!(f.intersects(CellFlags::OVERLINE));
    }

    #[test]
    fn intersects_empty_always_false() {
        let f = CellFlags::BOLD;
        assert!(!f.intersects(CellFlags::empty()));
    }

    // ---- union / difference ----

    #[test]
    fn union_combines_flags() {
        let f = CellFlags::BOLD.union(CellFlags::ITALIC);
        assert!(f.contains(CellFlags::BOLD));
        assert!(f.contains(CellFlags::ITALIC));
        assert!(!f.contains(CellFlags::UNDERLINE));
    }

    #[test]
    fn difference_removes_flags() {
        let f = CellFlags::BOLD
            .union(CellFlags::ITALIC)
            .difference(CellFlags::BOLD);
        assert!(!f.contains(CellFlags::BOLD));
        assert!(f.contains(CellFlags::ITALIC));
    }

    #[test]
    fn difference_of_absent_flag_is_noop() {
        let f = CellFlags::BOLD;
        let f2 = f.difference(CellFlags::ITALIC);
        assert_eq!(f, f2);
    }

    // ---- insert / remove (mutating) ----

    #[test]
    fn insert_sets_flag() {
        let mut f = CellFlags::empty();
        f.insert(CellFlags::BLINK);
        assert!(f.contains(CellFlags::BLINK));
    }

    #[test]
    fn remove_clears_flag() {
        let mut f = CellFlags::BLINK;
        f.remove(CellFlags::BLINK);
        assert!(!f.contains(CellFlags::BLINK));
        assert!(f.is_empty());
    }

    #[test]
    fn insert_is_idempotent() {
        let mut f = CellFlags::BOLD;
        f.insert(CellFlags::BOLD);
        assert_eq!(f, CellFlags::BOLD);
    }

    // ---- Bitwise operators ----

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

    // ---- is_complex / uses_style_id ----

    #[test]
    fn is_complex_only_when_complex_set() {
        assert!(!CellFlags::empty().is_complex());
        assert!(!CellFlags::BOLD.is_complex());
        assert!(CellFlags::COMPLEX.is_complex());
        assert!((CellFlags::BOLD | CellFlags::COMPLEX).is_complex());
    }

    #[test]
    fn uses_style_id_only_when_set() {
        assert!(!CellFlags::empty().uses_style_id());
        assert!(!CellFlags::BOLD.uses_style_id());
        assert!(CellFlags::USES_STYLE_ID.uses_style_id());
    }

    // ---- core_flags / extended_flags / has_extended_flags ----

    #[test]
    fn core_flags_strips_complex_and_style_id() {
        let f = CellFlags::BOLD | CellFlags::COMPLEX | CellFlags::USES_STYLE_ID;
        let core = f.core_flags();
        assert!(core.contains(CellFlags::BOLD));
        assert!(!core.is_complex());
        assert!(!core.uses_style_id());
    }

    #[test]
    fn core_flags_preserves_visual_flags() {
        let all_visual = CellFlags::from_bits(CellFlags::VISUAL_FLAGS_MASK);
        assert_eq!(all_visual.core_flags(), all_visual);
    }

    #[test]
    fn extended_flags_mask_covers_bits_11_to_13() {
        assert_eq!(CellFlags::EXTENDED_FLAGS_MASK, 0x3800);
        // Bits 11, 12, 13
        assert_eq!(
            CellFlags::EXTENDED_FLAGS_MASK,
            (1 << 11) | (1 << 12) | (1 << 13)
        );
    }

    #[test]
    fn extended_flags_extracts_only_bits_11_13() {
        let f = CellFlags::BOLD | CellFlags::SUPERSCRIPT | CellFlags::CURLY_UNDERLINE;
        let ext = f.extended_flags();
        assert!(ext.contains(CellFlags::SUPERSCRIPT));
        assert!(ext.contains(CellFlags::CURLY_UNDERLINE));
        assert!(!ext.contains(CellFlags::BOLD));
    }

    #[test]
    fn has_extended_flags_detects_superscript() {
        assert!(CellFlags::SUPERSCRIPT.has_extended_flags());
    }

    #[test]
    fn has_extended_flags_detects_subscript() {
        assert!(CellFlags::SUBSCRIPT.has_extended_flags());
    }

    #[test]
    fn has_extended_flags_detects_overline() {
        assert!(CellFlags::OVERLINE.has_extended_flags());
    }

    #[test]
    fn has_extended_flags_detects_curly_underline() {
        assert!(CellFlags::CURLY_UNDERLINE.has_extended_flags());
    }

    #[test]
    fn has_extended_flags_false_for_basic() {
        assert!(!CellFlags::BOLD.has_extended_flags());
        assert!(!CellFlags::DIM.has_extended_flags());
        assert!(!CellFlags::UNDERLINE.has_extended_flags());
        assert!(!CellFlags::DOUBLE_UNDERLINE.has_extended_flags());
    }

    // ---- All visual flags are distinct single-bit (bits 0-9 only) ----

    #[test]
    fn basic_flags_are_distinct_single_bits() {
        let singles = [
            CellFlags::BOLD,
            CellFlags::DIM,
            CellFlags::ITALIC,
            CellFlags::UNDERLINE,
            CellFlags::BLINK,
            CellFlags::INVERSE,
            CellFlags::HIDDEN,
            CellFlags::STRIKETHROUGH,
            CellFlags::DOUBLE_UNDERLINE,
            CellFlags::WIDE,
        ];
        for (i, a) in singles.iter().enumerate() {
            for (j, b) in singles.iter().enumerate() {
                if i != j {
                    assert!(
                        !a.intersects(*b),
                        "flags at indices {i} and {j} should not overlap"
                    );
                }
            }
            // Each is a single bit
            assert_eq!(
                a.bits().count_ones(),
                1,
                "flag at index {i} should be a single bit"
            );
        }
    }

    // ---- Copy semantics ----

    #[test]
    fn copy_semantics() {
        let f = CellFlags::BOLD | CellFlags::ITALIC;
        let f2 = f; // Copy
        assert_eq!(f, f2);
        // Original still usable after copy
        assert!(f.contains(CellFlags::BOLD));
    }
}
