// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-dirfd` — the handle-relative directory operations aterm's confined
//! artifact and media paths and its disk reclaim are built on: fd-relative
//! POSIX `*at` calls over `libc` on unix, NT handle-relative opens over
//! `ntdll`/`kernel32` on Windows. This crate owns all of that `unsafe`, so its
//! callers write none.
//!
//! ## The property this exists to preserve
//!
//! Every operation here is **relative to a handle the caller holds and
//! re-resolves nothing**. A `PathBuf` is a claim about one instant: a same-user
//! process can rename an ancestor and leave a link at the old pathname between
//! a check and the act. So a caller opens each absolute-path component without
//! following a link, RETAINS the handles, and does all later work relative to
//! the leaf.
//!
//! ## The two halves
//!
//! * **unix** (`src/unix.rs`, its items the crate root on a unix target):
//!   `openat`, `fstat`, `unlinkat`, `renameat(_with)`, `linkat`, `mkdirat`,
//!   `fchmod`, `fsync` and `Dir` over `libc`. Its module docs carry the
//!   contract and the per-platform rename.
//! * **Windows** (`src/windows/`, its items the crate root on a Windows
//!   target): the subset the disk reclaim (`aterm-agent`'s
//!   `harness::disk_target::fd`) calls, under the same names and shapes where
//!   the semantics carry — `openat`, `fstat`, `unlinkat`, `Dir`/`DirEntry`,
//!   `Errno`, `OFlags`/`AtFlags`/`Mode`, `OwnedFd` — plus what Windows needs
//!   on top: `pin`, which opens an absolute path from a local volume's root
//!   (never a share's), a 128-bit `FileId` identity, an `unlinkat` that
//!   takes the identity the name must still carry, and a facts-only open
//!   (`OFlags::PATH`) with the owner read through it (`owner`, `Sid`) for the
//!   reclaim's other-user fence, and a handle's volume by its own GUID
//!   (`volume_guid`) for its volume fence. Nothing there ever follows
//!   a reparse point (its module docs say how, and why a non-link reparse
//!   point is kept), and every call sees a cloud placeholder as the reparse
//!   point it is (`expose_placeholders`), never as the plain file a process
//!   is shown by default.
//!
//! ## Tests
//!
//! `tests/dirfd.rs` (unix) drives the unix half and `std::fs` over identical
//! real trees and requires agreement on every result, errno, `stat` field,
//! listing and the tree left behind. `tests/windows.rs` drives the Windows half
//! over real NTFS trees: junctions, read-only files, files held open with and
//! without delete sharing, a swapped name, a path past `MAX_PATH`, a real
//! cloud sync root and its placeholders.

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
            #[must_use]
            pub const fn empty() -> Self {
                Self(0)
            }

            /// The raw flag word, as the syscall takes it.
            #[must_use]
            pub const fn bits(self) -> $int {
                self.0
            }
        }

        impl std::ops::BitOr for $name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self {
                Self(self.0 | rhs.0)
            }
        }

        impl std::ops::BitOrAssign for $name {
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
