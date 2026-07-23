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

    #[test]
    fn test_empty() {
        let f = TestFlags::empty();
        assert!(f.is_empty());
        assert_eq!(f.bits(), 0);
    }

    #[test]
    fn test_constants() {
        assert_eq!(TestFlags::A.bits(), 1);
        assert_eq!(TestFlags::B.bits(), 2);
        assert_eq!(TestFlags::C.bits(), 4);
        assert_eq!(TestFlags::AB.bits(), 3);
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
    fn test_union() {
        let f = TestFlags::A.union(TestFlags::C);
        assert_eq!(f.bits(), 5);
    }

    #[test]
    fn test_intersection() {
        let f = (TestFlags::A | TestFlags::B).intersection(TestFlags::AB);
        assert_eq!(f, TestFlags::AB);
    }

    #[test]
    fn test_difference() {
        let f = TestFlags::AB.difference(TestFlags::A);
        assert_eq!(f, TestFlags::B);
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

    #[test]
    fn test_not_truncates_to_defined_bits() {
        let f = !TestFlags::A;
        assert!(!f.contains(TestFlags::A));
        assert!(f.contains(TestFlags::B));
        assert!(f.contains(TestFlags::C));
        // Must not set undefined bits (bits 3-7 of u8).
        assert_eq!(f.bits(), 0b0000_0110);
    }

    #[test]
    fn test_not_empty_is_all() {
        let f = !TestFlags::empty();
        assert!(f.is_all());
        assert_eq!(
            f.bits(),
            TestFlags::A.bits() | TestFlags::B.bits() | TestFlags::C.bits() | TestFlags::AB.bits()
        );
    }

    #[test]
    fn test_from_bits_truncate() {
        let f = TestFlags::from_bits_truncate(0xFF);
        assert_eq!(f.bits(), 0x07);
    }

    #[test]
    fn test_from_bits_retain() {
        let f = TestFlags::from_bits_retain(0xFF);
        assert_eq!(f.bits(), 0xFF);
    }

    #[test]
    fn test_from_bits_rejects_unknown() {
        assert!(TestFlags::from_bits(0x07).is_some());
        assert!(TestFlags::from_bits(0xFF).is_none());
    }

    #[test]
    fn test_is_all() {
        let f = TestFlags::A | TestFlags::B | TestFlags::C;
        assert!(f.is_all());
    }

    #[test]
    fn test_bitor_assign() {
        let mut f = TestFlags::A;
        f |= TestFlags::B;
        assert_eq!(f, TestFlags::AB);
    }

    #[test]
    fn test_bitand_assign() {
        let mut f = TestFlags::AB;
        f &= TestFlags::A;
        assert_eq!(f, TestFlags::A);
    }

    #[test]
    fn test_bitxor() {
        let f = TestFlags::AB ^ TestFlags::A;
        assert_eq!(f, TestFlags::B);
    }

    #[test]
    fn test_sub_operator() {
        let f = TestFlags::AB - TestFlags::A;
        assert_eq!(f, TestFlags::B);
    }

    #[test]
    fn test_sub_assign() {
        let mut f = TestFlags::AB;
        f -= TestFlags::A;
        assert_eq!(f, TestFlags::B);
    }
}
