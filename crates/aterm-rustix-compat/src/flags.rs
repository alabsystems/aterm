// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The flag types, in the shape upstream builds with `bitflags` 2.
//!
//! Every upstream type here ends in `const _ = !0;`: all bits are KNOWN, so
//! `from_bits` never fails, `from_bits_truncate` keeps every bit and `!` flips
//! all of them. A value read back from the kernel (`fcntl_getfl`, `revents`)
//! therefore keeps bits this crate has no name for.

use core::fmt;

/// Upstream's `Debug` shape: `Name(A | B | 0x40)`, `Name(0x0)` when empty.
/// Named flags are taken in declaration order, each only if all of its bits
/// are still unclaimed; whatever is left prints as hex. Not always upstream's
/// text: `bitflags` takes a flag when any of its bits is unclaimed (the crate
/// doc's divergences say where the two differ).
pub(crate) fn debug(
    f: &mut fmt::Formatter<'_>,
    name: &str,
    bits: u64,
    named: &[(&str, u64)],
) -> fmt::Result {
    write!(f, "{name}(")?;
    if bits == 0 {
        return f.write_str("0x0)");
    }
    let mut rest = bits;
    let mut first = true;
    for &(flag, value) in named {
        if value != 0 && rest & value == value {
            if !first {
                f.write_str(" | ")?;
            }
            f.write_str(flag)?;
            rest &= !value;
            first = false;
        }
    }
    if rest != 0 {
        if !first {
            f.write_str(" | ")?;
        }
        write!(f, "{rest:#x}")?;
    }
    f.write_str(")")
}

/// Declares one flag type: a transparent wrapper over the raw integer the
/// kernel takes, its named values, and the operators consumers use.
macro_rules! flags {
    (
        $(#[$outer:meta])*
        pub struct $name:ident: $raw:ty {
            $( $(#[$inner:meta])* const $flag:ident = $value:expr; )*
        }
    ) => {
        $(#[$outer])*
        #[repr(transparent)]
        #[derive(Copy, Clone, Eq, PartialEq, Hash)]
        pub struct $name($raw);

        impl $name {
            $( $(#[$inner])* pub const $flag: Self = Self($value); )*

            /// No bits.
            #[inline]
            pub const fn empty() -> Self {
                Self(0)
            }

            /// Every bit (upstream's types declare `const _ = !0`).
            #[inline]
            pub const fn all() -> Self {
                Self(!0)
            }

            /// The raw value.
            #[inline]
            pub const fn bits(&self) -> $raw {
                self.0
            }

            /// Always `Some`: every bit is a known bit.
            #[inline]
            pub const fn from_bits(bits: $raw) -> Option<Self> {
                Some(Self(bits))
            }

            /// The value, unchanged.
            #[inline]
            pub const fn from_bits_retain(bits: $raw) -> Self {
                Self(bits)
            }

            /// The value, unchanged: nothing is unknown, so nothing is cut.
            #[inline]
            pub const fn from_bits_truncate(bits: $raw) -> Self {
                Self(bits)
            }

            /// Whether no bit is set.
            #[inline]
            pub const fn is_empty(&self) -> bool {
                self.0 == 0
            }

            /// Whether every bit of `other` is set here.
            #[inline]
            pub const fn contains(&self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            /// Whether any bit of `other` is set here.
            #[inline]
            pub const fn intersects(&self, other: Self) -> bool {
                self.0 & other.0 != 0
            }

            /// Sets the bits of `other`.
            #[inline]
            pub fn insert(&mut self, other: Self) {
                self.0 |= other.0;
            }

            /// Clears the bits of `other`.
            #[inline]
            pub fn remove(&mut self, other: Self) {
                self.0 &= !other.0;
            }

            /// Sets or clears the bits of `other`.
            #[inline]
            pub fn set(&mut self, other: Self, value: bool) {
                if value {
                    self.insert(other);
                } else {
                    self.remove(other);
                }
            }
        }

        impl core::ops::BitOr for $name {
            type Output = Self;
            #[inline]
            fn bitor(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }
        }

        impl core::ops::BitOrAssign for $name {
            #[inline]
            fn bitor_assign(&mut self, other: Self) {
                self.0 |= other.0;
            }
        }

        impl core::ops::BitAnd for $name {
            type Output = Self;
            #[inline]
            fn bitand(self, other: Self) -> Self {
                Self(self.0 & other.0)
            }
        }

        impl core::ops::BitAndAssign for $name {
            #[inline]
            fn bitand_assign(&mut self, other: Self) {
                self.0 &= other.0;
            }
        }

        impl core::ops::Sub for $name {
            type Output = Self;
            #[inline]
            fn sub(self, other: Self) -> Self {
                Self(self.0 & !other.0)
            }
        }

        impl core::ops::Not for $name {
            type Output = Self;
            #[inline]
            fn not(self) -> Self {
                Self(!self.0)
            }
        }

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                $crate::flags::debug(
                    f,
                    stringify!($name),
                    u64::from(self.0),
                    &[$( (stringify!($flag), u64::from($value)) ),*],
                )
            }
        }
    };
}
