// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Handle-relative directory operations for confined filesystem access.

/// A newtype over a flag word with the `empty`/`bits`/`BitOr` surface the call
/// sites use.
#[cfg(any(unix, windows))]
macro_rules! flags {
    ($(#[$meta:meta])* $name:ident($int:ty) { $($(#[$cmeta:meta])* $konst:ident = $value:expr;)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name($int);

        impl $name {
            $($(#[$cmeta])* pub const $konst: Self = Self($value);)*

            /// No flags set.
            #[inline]
            #[must_use]
            pub const fn empty() -> Self {
                Self(0)
            }

            /// The raw flag word, as the syscall takes it.
            #[inline]
            #[must_use]
            pub const fn bits(self) -> $int {
                self.0
            }

            /// The raw flag word `bits`, every bit kept as given (a mode read off a file, with
            /// its group, other and set-id bits, to write back changed).
            #[inline]
            #[must_use]
            pub const fn from_bits(bits: $int) -> Self {
                Self(bits)
            }
        }

        impl std::ops::BitOr for $name {
            type Output = Self;
            #[inline]
            fn bitor(self, rhs: Self) -> Self {
                Self(self.0 | rhs.0)
            }
        }

        impl std::ops::BitOrAssign for $name {
            #[inline]
            fn bitor_assign(&mut self, rhs: Self) {
                self.0 |= rhs.0;
            }
        }
    };
}

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
