// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Declarative bitflags macro (zero external dependencies).
//!
//! Drop-in replacement for the `bitflags` crate covering the API surface
//! used in aterm: construction, testing, set operations, and raw access.

/// Define a bitflags struct with named constants and standard set operations.
///
/// # Example
///
/// ```ignore
/// aterm_types::bitflags! {
///     #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
///     pub struct Flags: u8 {
///         const A = 1 << 0;
///         const B = 1 << 1;
///         const AB = Self::A.bits() | Self::B.bits();
///     }
/// }
/// ```
#[macro_export]
macro_rules! bitflags {
    (
        $(#[$outer:meta])*
        $vis:vis struct $Name:ident : $T:ty {
            $(
                $(#[$inner:meta])*
                const $FLAG:ident = $value:expr;
            )*
        }
    ) => {
        $(#[$outer])*
        $vis struct $Name {
            bits: $T,
        }

        #[allow(dead_code, non_upper_case_globals)]
        impl $Name {
            $(
                $(#[$inner])*
                pub const $FLAG: Self = Self { bits: $value };
            )*

            /// Create with no flags set.
            #[inline]
            #[must_use]
            pub const fn empty() -> Self {
                Self { bits: 0 }
            }

            /// Raw bits value.
            #[inline]
            #[must_use]
            pub const fn bits(&self) -> $T {
                self.bits
            }

            /// Create from raw bits, discarding unknown bits.
            #[inline]
            #[must_use]
            pub const fn from_bits_truncate(bits: $T) -> Self {
                Self { bits: bits & Self::__all_bits() }
            }

            /// Create from raw bits, retaining all bits (even unknown ones).
            #[inline]
            #[must_use]
            pub const fn from_bits_retain(bits: $T) -> Self {
                Self { bits }
            }

            /// Create from raw bits, returning `None` if unknown bits are set.
            #[inline]
            #[must_use]
            pub const fn from_bits(bits: $T) -> Option<Self> {
                if bits & !Self::__all_bits() == 0 {
                    Some(Self { bits })
                } else {
                    None
                }
            }

            /// Whether no flags are set.
            #[inline]
            #[must_use]
            pub const fn is_empty(&self) -> bool {
                self.bits == 0
            }

            /// Whether all known flags are set.
            #[inline]
            #[must_use]
            pub const fn is_all(&self) -> bool {
                self.bits & Self::__all_bits() == Self::__all_bits()
            }

            /// Whether `self` contains all flags in `other`.
            #[inline]
            #[must_use]
            pub const fn contains(&self, other: Self) -> bool {
                self.bits & other.bits == other.bits
            }

            /// Whether `self` and `other` have any flags in common.
            #[inline]
            #[must_use]
            pub const fn intersects(&self, other: Self) -> bool {
                self.bits & other.bits != 0
            }

            /// Return the union of `self` and `other`.
            #[inline]
            #[must_use]
            pub const fn union(self, other: Self) -> Self {
                Self { bits: self.bits | other.bits }
            }

            /// Return the intersection of `self` and `other`.
            #[inline]
            #[must_use]
            pub const fn intersection(self, other: Self) -> Self {
                Self { bits: self.bits & other.bits }
            }

            /// Return `self` with the flags in `other` removed.
            #[inline]
            #[must_use]
            pub const fn difference(self, other: Self) -> Self {
                Self { bits: self.bits & !other.bits }
            }

            /// Insert `other` flags into `self`.
            #[inline]
            pub fn insert(&mut self, other: Self) {
                self.bits |= other.bits;
            }

            /// Remove `other` flags from `self`.
            #[inline]
            pub fn remove(&mut self, other: Self) {
                self.bits &= !other.bits;
            }

            /// Toggle `other` flags in `self`.
            #[inline]
            pub fn toggle(&mut self, other: Self) {
                self.bits ^= other.bits;
            }

            /// Set or unset `other` flags based on `value`.
            #[inline]
            pub fn set(&mut self, other: Self, value: bool) {
                if value {
                    self.insert(other);
                } else {
                    self.remove(other);
                }
            }

            // Union of all defined flag bits. Used for truncation.
            #[doc(hidden)]
            const fn __all_bits() -> $T {
                0 $(| Self::$FLAG.bits)*
            }
        }

        impl ::core::ops::BitOr for $Name {
            type Output = Self;
            #[inline]
            fn bitor(self, rhs: Self) -> Self {
                Self { bits: self.bits | rhs.bits }
            }
        }

        impl ::core::ops::BitOrAssign for $Name {
            #[inline]
            fn bitor_assign(&mut self, rhs: Self) {
                self.bits |= rhs.bits;
            }
        }

        impl ::core::ops::BitAnd for $Name {
            type Output = Self;
            #[inline]
            fn bitand(self, rhs: Self) -> Self {
                Self { bits: self.bits & rhs.bits }
            }
        }

        impl ::core::ops::BitAndAssign for $Name {
            #[inline]
            fn bitand_assign(&mut self, rhs: Self) {
                self.bits &= rhs.bits;
            }
        }

        impl ::core::ops::BitXor for $Name {
            type Output = Self;
            #[inline]
            fn bitxor(self, rhs: Self) -> Self {
                Self { bits: self.bits ^ rhs.bits }
            }
        }

        impl ::core::ops::BitXorAssign for $Name {
            #[inline]
            fn bitxor_assign(&mut self, rhs: Self) {
                self.bits ^= rhs.bits;
            }
        }

        impl ::core::ops::Not for $Name {
            type Output = Self;
            #[inline]
            fn not(self) -> Self {
                Self { bits: !self.bits & Self::__all_bits() }
            }
        }

        impl ::core::ops::Sub for $Name {
            type Output = Self;
            #[inline]
            fn sub(self, rhs: Self) -> Self {
                self.difference(rhs)
            }
        }

        impl ::core::ops::SubAssign for $Name {
            #[inline]
            fn sub_assign(&mut self, rhs: Self) {
                self.remove(rhs);
            }
        }
    };
}

#[cfg(test)]
mod tests {
    bitflags! {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        struct TestFlags: u8 {
            const A = 1 << 0;
            const B = 1 << 1;
            const C = 1 << 2;
            const AB = Self::A.bits() | Self::B.bits();
        }
    }

    /// Every constructor and operator, as the bits it must produce. A generated
    /// flag type is only as sound as these: `!` and `from_bits_truncate` must
    /// drop undefined bits, and only `from_bits_retain` may keep them.
    #[test]
    fn operations_produce_expected_bits() {
        use TestFlags as F;
        let assigned = |mut f: F, op: fn(&mut F)| {
            op(&mut f);
            f
        };
        for (what, flags, bits) in [
            ("empty", F::empty(), 0),
            ("A", F::A, 1),
            ("B", F::B, 2),
            ("C", F::C, 4),
            ("AB", F::AB, 3),
            ("union", F::A.union(F::C), 5),
            ("intersection", (F::A | F::B).intersection(F::AB), 3),
            ("difference", F::AB.difference(F::A), 2),
            // Must not set undefined bits (bits 3-7 of u8).
            ("not A", !F::A, 0b0000_0110),
            ("not empty", !F::empty(), 0b0000_0111),
            ("from_bits_truncate", F::from_bits_truncate(0xFF), 0x07),
            ("from_bits_retain", F::from_bits_retain(0xFF), 0xFF),
            ("|=", assigned(F::A, |f| *f |= F::B), 3),
            ("&=", assigned(F::AB, |f| *f &= F::A), 1),
            ("^", F::AB ^ F::A, 2),
            ("-", F::AB - F::A, 2),
            ("-=", assigned(F::AB, |f| *f -= F::A), 2),
        ] {
            assert_eq!(flags.bits(), bits, "{what}");
        }
        for (what, holds) in [
            ("empty is empty", F::empty().is_empty()),
            ("not empty is all", (!F::empty()).is_all()),
            ("A|B|C is all", (F::A | F::B | F::C).is_all()),
            ("from_bits keeps defined bits", F::from_bits(0x07).is_some()),
            (
                "from_bits rejects unknown bits",
                F::from_bits(0xFF).is_none(),
            ),
        ] {
            assert!(holds, "{what}");
        }
    }

    #[test]
    fn test_contains() {
        let f = TestFlags::A | TestFlags::B;
        assert!(f.contains(TestFlags::A));
        assert!(f.contains(TestFlags::B));
        assert!(!f.contains(TestFlags::C));
        assert!(f.contains(TestFlags::AB));
    }

    #[test]
    fn test_intersects() {
        let f = TestFlags::A | TestFlags::C;
        assert!(f.intersects(TestFlags::A));
        assert!(!f.intersects(TestFlags::B));
        assert!(f.intersects(TestFlags::AB));
    }

    #[test]
    fn test_insert_remove_toggle() {
        let mut f = TestFlags::empty();
        f.insert(TestFlags::A);
        assert!(f.contains(TestFlags::A));
        f.remove(TestFlags::A);
        assert!(!f.contains(TestFlags::A));
        f.toggle(TestFlags::B);
        assert!(f.contains(TestFlags::B));
        f.toggle(TestFlags::B);
        assert!(!f.contains(TestFlags::B));
    }

    #[test]
    fn test_set() {
        let mut f = TestFlags::empty();
        f.set(TestFlags::C, true);
        assert!(f.contains(TestFlags::C));
        f.set(TestFlags::C, false);
        assert!(!f.contains(TestFlags::C));
    }
}
