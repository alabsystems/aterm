// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Declarative bitflags macro for flag construction, testing, and set operations.

/// Defines a bitflags struct with named constants and standard set operations.
///
/// # Example
///
/// ```
/// aterm_types::bitflags! {
///     #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
///     pub struct Flags: u8 {
///         const A = 1 << 0;
///         const B = 1 << 1;
///         const AB = Self::A.bits() | Self::B.bits();
///     }
/// }
/// assert_eq!(Flags::AB, Flags::A | Flags::B);
/// assert!(Flags::AB.contains(Flags::A));
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

        #[allow(non_upper_case_globals)]
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

            /// Every defined flag set.
            #[inline]
            #[must_use]
            pub const fn all() -> Self {
                Self { bits: Self::__all_bits() }
            }

            /// Create from raw bits, keeping any bits no flag defines.
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
            pub const fn insert(&mut self, other: Self) {
                self.bits |= other.bits;
            }

            /// Remove `other` flags from `self`.
            #[inline]
            pub const fn remove(&mut self, other: Self) {
                self.bits &= !other.bits;
            }

            /// Toggle `other` flags in `self`.
            #[inline]
            pub const fn toggle(&mut self, other: Self) {
                self.bits ^= other.bits;
            }

            /// Set or unset `other` flags based on `value`.
            #[inline]
            pub const fn set(&mut self, other: Self, value: bool) {
                if value {
                    self.insert(other);
                } else {
                    self.remove(other);
                }
            }

            #[doc(hidden)]
            #[inline]
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
            ("not A", !F::A, 0b0000_0110),
            ("not empty", !F::empty(), 0b0000_0111),
            ("from_bits_truncate", F::from_bits_truncate(0xFF), 0x07),
            ("all", F::all(), 0x07),
            ("from_bits_retain", F::from_bits_retain(0xF1), 0xF1),
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
            ("all is all", F::all().is_all()),
            (
                "a retained unknown bit is not all",
                !F::from_bits_retain(0x08).is_all(),
            ),
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
    fn predicates_and_mutators() {
        use TestFlags as F;
        let f = F::A | F::B;
        assert!(f.contains(F::A) && f.contains(F::B) && f.contains(F::AB));
        assert!(!f.contains(F::C));
        let f = F::A | F::C;
        assert!(f.intersects(F::A) && f.intersects(F::AB));
        assert!(!f.intersects(F::B));
        let mut f = F::empty();
        f.insert(F::A);
        assert_eq!(f, F::A);
        f.remove(F::A);
        assert!(f.is_empty());
        f.toggle(F::B);
        assert_eq!(f, F::B);
        f.toggle(F::B);
        assert!(f.is_empty());
        f.set(F::C, true);
        assert_eq!(f, F::C);
        f.set(F::C, false);
        assert!(f.is_empty());
    }
}
