// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-dirfd` — the fd-relative POSIX directory operations aterm's confined
//! artifact and media paths are built on, over `libc` directly.
//!
//! ## What this replaces and why
//!
//! `rustix` cost 72,832 lines across two packages (with `errno`) — the largest
//! single import left in aterm's shipped graph — to supply about a dozen
//! syscall wrappers to exactly two files: `aterm-gui`'s `pinned_dir.rs` and
//! `control_media.rs`. Nothing else in the workspace touched it.
//!
//! ## The property this exists to preserve
//!
//! Every operation here is **fd-relative and re-resolves nothing**. That is the
//! entire point of the callers: a `PathBuf` is a claim about one instant, and a
//! same-uid process can rename an ancestor and leave a symlink at the old
//! pathname before a worker writes. `PinnedDir` answers that by opening each
//! absolute-path component with `O_NOFOLLOW` and RETAINING the descriptors, then
//! doing all later work relative to the retained leaf.
//!
//! So the contract of this crate is narrow and strict:
//!
//! * Every entry point takes a directory descriptor plus ONE name, and issues
//!   the `*at` syscall for it. No wrapper here ever joins, canonicalizes, walks
//!   or re-opens a path.
//! * No entry point follows a symlink on its own initiative; whether links are
//!   followed is the caller's [`OFlags`]/[`AtFlags`] decision, unchanged.
//! * Errors are the raw `errno`, never collapsed into a boolean or an
//!   `Option` — `remove_file_if_exists` distinguishes `ENOENT` from every other
//!   failure, and a wrapper that lost that distinction would turn "permission
//!   denied" into "already gone".
//!
//! ## Platform semantics that are NOT approximated
//!
//! [`renameat_with`] is the one place where the two platforms genuinely differ,
//! and it is implemented per-platform rather than emulated:
//!
//! * **Linux** — `renameat2(2)` with `RENAME_NOREPLACE` / `RENAME_EXCHANGE`,
//!   issued through `syscall(2)` so it works on a musl or pre-2.28 glibc that
//!   has no wrapper for it.
//! * **macOS** — `renameatx_np(2)` with `RENAME_EXCL` / `RENAME_SWAP`, the
//!   documented Darwin equivalents.
//! * **Anywhere else** — [`RenameFlags`] operations are REFUSED with `ENOSYS`
//!   rather than silently degraded to a plain `renameat`, because a plain
//!   `renameat` would clobber an existing destination, which is precisely the
//!   race `RENAME_NOREPLACE` exists to close. (The callers already carry a
//!   `linkat` + `unlinkat` fallback for those platforms and select it at compile
//!   time; this is the belt for the braces.)
//!
//! ## The oracle
//!
//! `rustix` is kept as a `[dev-dependencies]` ORACLE. `tests/oracle.rs` drives
//! both implementations over the same real directory trees and requires
//! agreement on the result of every operation, on the exact errno of every
//! failure, on every `stat` field read, on directory listings, and on the
//! observable filesystem state afterwards.

#![cfg(unix)]

use std::ffi::{CStr, OsStr};
use std::os::fd::{FromRawFd as _, IntoRawFd as _};
use std::os::unix::ffi::OsStrExt as _;

pub use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};

// ---------------------------------------------------------------------------
// Errno
// ---------------------------------------------------------------------------

/// A raw POSIX `errno`.
///
/// Deliberately NOT an `io::Error`: callers match on specific values
/// (`Errno::NOENT`) in `match` patterns, which needs a structurally-matchable
/// type. Converting to `io::Error` is one `.into()` at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Errno(pub i32);

impl Errno {
    /// `ENOENT` — no such file or directory. The one code the artifact cleanup
    /// path treats as success rather than failure.
    pub const NOENT: Self = Self(libc::ENOENT);
    /// `EINVAL` — used here for a name that cannot become a C string.
    pub const INVAL: Self = Self(libc::EINVAL);
    /// `ENOSYS` — the platform has no such operation. Returned instead of
    /// approximating one.
    pub const NOSYS: Self = Self(libc::ENOSYS);

    /// The raw `errno` value.
    #[must_use]
    pub const fn raw_os_error(self) -> i32 {
        self.0
    }

    /// The current thread's `errno`, read immediately after a failed call.
    fn last() -> Self {
        Self(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }
}

impl std::fmt::Display for Errno {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::io::Error::from_raw_os_error(self.0).fmt(f)
    }
}

impl std::error::Error for Errno {}

impl From<Errno> for std::io::Error {
    fn from(e: Errno) -> Self {
        Self::from_raw_os_error(e.0)
    }
}

/// This crate's result type.
pub type Result<T> = std::result::Result<T, Errno>;

/// Turn a libc return of `-1`-on-failure into a `Result`.
fn check(ret: libc::c_int) -> Result<()> {
    if ret < 0 { Err(Errno::last()) } else { Ok(()) }
}

// ---------------------------------------------------------------------------
// Path arguments
// ---------------------------------------------------------------------------

/// Anything that can be handed to a `*at` syscall as ONE name.
///
/// Implemented once, over `AsRef<OsStr>`, which covers `&str`, `&OsStr`,
/// `&OsString`, `&Path` and `&PathBuf` — every shape the call sites use.
pub trait Arg {
    /// Materialise the argument as a NUL-terminated C string and run `f`.
    ///
    /// # Errors
    /// [`Errno::INVAL`] if the name contains an interior NUL — a name that
    /// cannot be expressed to the kernel is rejected rather than truncated at
    /// the NUL, which would address a DIFFERENT file.
    fn with_c_str<T>(self, f: impl FnOnce(&CStr) -> Result<T>) -> Result<T>;
}

impl<A: AsRef<OsStr>> Arg for A {
    fn with_c_str<R>(self, f: impl FnOnce(&CStr) -> Result<R>) -> Result<R> {
        let bytes = self.as_ref().as_bytes();
        if bytes.contains(&0) {
            return Err(Errno::INVAL);
        }
        // Names are single path components or short absolute paths; keep them
        // off the heap. `SMALL` comfortably covers NAME_MAX (255) plus the NUL.
        const SMALL: usize = 512;
        if bytes.len() < SMALL {
            let mut buf = [0u8; SMALL];
            buf[..bytes.len()].copy_from_slice(bytes);
            // The tail is already zero, so the string is NUL-terminated.
            let c = CStr::from_bytes_until_nul(&buf[..=bytes.len()]).map_err(|_| Errno::INVAL)?;
            f(c)
        } else {
            let mut owned = Vec::with_capacity(bytes.len() + 1);
            owned.extend_from_slice(bytes);
            owned.push(0);
            let c = CStr::from_bytes_until_nul(&owned).map_err(|_| Errno::INVAL)?;
            f(c)
        }
    }
}

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

/// Build a newtype over a libc flag word with the `empty`/`BitOr`/`contains`
/// surface the call sites use. Declared as a macro because the three flag types
/// differ only in their underlying integer and their constants — writing the
/// same twelve impls three times is how one of them ends up subtly different.
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

            /// Whether every bit of `other` is set here.
            #[must_use]
            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
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

flags! {
    /// `open(2)` flags.
    OFlags(libc::c_int) {
        /// `O_RDONLY`.
        RDONLY = libc::O_RDONLY;
        /// `O_WRONLY`.
        WRONLY = libc::O_WRONLY;
        /// `O_RDWR`.
        RDWR = libc::O_RDWR;
        /// `O_CREAT`.
        CREATE = libc::O_CREAT;
        /// `O_EXCL` — with `O_CREAT`, fail if the name already exists. The
        /// atomic "create or lose" the private-file path depends on.
        EXCL = libc::O_EXCL;
        /// `O_DIRECTORY` — fail unless the name is a directory.
        DIRECTORY = libc::O_DIRECTORY;
        /// `O_NOFOLLOW` — fail if the FINAL component is a symlink. Applies to
        /// the last component only, which is why the callers pin every ancestor.
        NOFOLLOW = libc::O_NOFOLLOW;
        /// `O_NONBLOCK` — so opening a FIFO left in place by another process
        /// cannot wedge the server thread.
        NONBLOCK = libc::O_NONBLOCK;
        /// `O_CLOEXEC`.
        CLOEXEC = libc::O_CLOEXEC;
    }
}

flags! {
    /// `*at(2)` behaviour flags.
    AtFlags(libc::c_int) {
        /// `AT_REMOVEDIR` — make `unlinkat` behave as `rmdir`.
        REMOVEDIR = libc::AT_REMOVEDIR;
        /// `AT_SYMLINK_NOFOLLOW`.
        SYMLINK_NOFOLLOW = libc::AT_SYMLINK_NOFOLLOW;
    }
}

flags! {
    /// File mode bits.
    Mode(libc::mode_t) {
        /// `S_IRWXU` — 0o700.
        RWXU = libc::S_IRWXU;
        /// `S_IRUSR` — 0o400.
        RUSR = libc::S_IRUSR;
        /// `S_IWUSR` — 0o200.
        WUSR = libc::S_IWUSR;
        /// `S_IXUSR` — 0o100.
        XUSR = libc::S_IXUSR;
    }
}

/// Flags for [`renameat_with`].
///
/// These are NOT a bit union of the platform constants: the two platforms use
/// different values for the same idea, so the flag is carried abstractly and
/// translated at the syscall. Refusing to expose a raw bit word is deliberate —
/// a caller cannot accidentally pass a Linux constant on Darwin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenameFlags(u8);

impl RenameFlags {
    /// Fail with `EEXIST` if the destination already exists, instead of
    /// replacing it. `RENAME_NOREPLACE` on Linux, `RENAME_EXCL` on Darwin.
    pub const NOREPLACE: Self = Self(1);
    /// Atomically swap the two names. `RENAME_EXCHANGE` on Linux,
    /// `RENAME_SWAP` on Darwin.
    pub const EXCHANGE: Self = Self(2);

    /// No flags — equivalent to a plain [`renameat`].
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Whether every bit of `other` is set here.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for RenameFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// `AT_FDCWD`, as a descriptor that can be passed anywhere a directory
/// descriptor is taken.
///
/// Using it means "resolve this name against the process's current directory" —
/// the ONE place in these callers where resolution is not fd-relative, and it is
/// used only to open `/` and to re-open an absolute path for an identity check.
pub const CWD: BorrowedFd<'static> = {
    // SAFETY: `AT_FDCWD` is the kernel's reserved sentinel for "the current
    // directory" and is a valid argument to every `*at` call below. It is not
    // -1, so it satisfies `borrow_raw`'s requirement.
    unsafe { BorrowedFd::borrow_raw(libc::AT_FDCWD) }
};

// ---------------------------------------------------------------------------
// stat
// ---------------------------------------------------------------------------

/// The subset of `struct stat` these callers read, widened to fixed sizes so
/// the call sites do not vary by platform.
///
/// Only four fields, because only four are used, and every one of them is an
/// IDENTITY question: which inode on which device, is it the kind of thing we
/// expected, and how many names still refer to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    /// Device the inode lives on.
    pub st_dev: u64,
    /// Inode number.
    pub st_ino: u64,
    /// Type and permission bits.
    pub st_mode: u32,
    /// Hard-link count. Zero means the inode has been fully unlinked; one means
    /// this is the only name for it, which is what the private-file guarantee
    /// rests on.
    pub st_nlink: u64,
}

/// A file's type, extracted from its mode bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileType(u32);

impl FileType {
    /// Extract the type from a raw `st_mode`.
    #[must_use]
    pub const fn from_raw_mode(mode: u32) -> Self {
        Self(mode & libc::S_IFMT as u32)
    }

    /// A directory.
    #[must_use]
    pub const fn is_dir(self) -> bool {
        self.0 == libc::S_IFDIR as u32
    }

    /// A regular file.
    #[must_use]
    pub const fn is_file(self) -> bool {
        self.0 == libc::S_IFREG as u32
    }

    /// A symbolic link.
    #[must_use]
    pub const fn is_symlink(self) -> bool {
        self.0 == libc::S_IFLNK as u32
    }
}

// ---------------------------------------------------------------------------
// The operations
// ---------------------------------------------------------------------------

/// `openat(2)`: open `name` relative to the directory `dirfd`.
///
/// The returned descriptor is owned; nothing here ever re-resolves `name`.
///
/// # Errors
/// The raw `errno` from the syscall, or [`Errno::INVAL`] for an unusable name.
pub fn openat<Fd: AsFd, P: Arg>(dirfd: Fd, name: P, flags: OFlags, mode: Mode) -> Result<OwnedFd> {
    name.with_c_str(|name| {
        // SAFETY: `dirfd` is a live descriptor for the duration of the call and
        // `name` is a NUL-terminated C string valid for it.
        let fd = unsafe {
            libc::openat(
                dirfd.as_fd().as_raw_fd(),
                name.as_ptr(),
                flags.bits(),
                libc::c_uint::from(mode.bits()),
            )
        };
        if fd < 0 {
            return Err(Errno::last());
        }
        // SAFETY: `fd` is a fresh, exclusively-owned descriptor from `openat`.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    })
}

/// `fstat(2)`: identity and type of an ALREADY-OPEN descriptor.
///
/// There is no path here at all, which is exactly why the callers use it: the
/// answer is about the inode they are holding, not about whatever a name points
/// at now.
///
/// # Errors
/// The raw `errno` from the syscall.
pub fn fstat<Fd: AsFd>(fd: Fd) -> Result<Stat> {
    // SAFETY: `stat` is a plain C struct with no invalid bit patterns for the
    // fields read below, and `fstat` fills it before we look.
    let mut raw: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is live and `raw` is a writable `struct stat`.
    check(unsafe { libc::fstat(fd.as_fd().as_raw_fd(), &raw mut raw) })?;
    Ok(Stat {
        st_dev: raw.st_dev as u64,
        st_ino: raw.st_ino as u64,
        st_mode: u32::from(raw.st_mode),
        st_nlink: raw.st_nlink as u64,
    })
}

/// `mkdirat(2)`.
///
/// # Errors
/// The raw `errno` from the syscall, or [`Errno::INVAL`] for an unusable name.
pub fn mkdirat<Fd: AsFd, P: Arg>(dirfd: Fd, name: P, mode: Mode) -> Result<()> {
    name.with_c_str(|name| {
        // SAFETY: live descriptor, NUL-terminated name.
        check(unsafe { libc::mkdirat(dirfd.as_fd().as_raw_fd(), name.as_ptr(), mode.bits()) })
    })
}

/// `unlinkat(2)`. Pass [`AtFlags::REMOVEDIR`] to remove a directory.
///
/// # Errors
/// The raw `errno` from the syscall, or [`Errno::INVAL`] for an unusable name.
pub fn unlinkat<Fd: AsFd, P: Arg>(dirfd: Fd, name: P, flags: AtFlags) -> Result<()> {
    name.with_c_str(|name| {
        // SAFETY: live descriptor, NUL-terminated name.
        check(unsafe { libc::unlinkat(dirfd.as_fd().as_raw_fd(), name.as_ptr(), flags.bits()) })
    })
}

/// `renameat(2)`: rename `from` under `from_dir` to `to` under `to_dir`.
///
/// This REPLACES an existing destination, as POSIX specifies. Callers that must
/// not replace use [`renameat_with`] with [`RenameFlags::NOREPLACE`].
///
/// # Errors
/// The raw `errno` from the syscall, or [`Errno::INVAL`] for an unusable name.
pub fn renameat<F1: AsFd, F2: AsFd, P: Arg, Q: Arg>(
    from_dir: F1,
    from: P,
    to_dir: F2,
    to: Q,
) -> Result<()> {
    from.with_c_str(|from| {
        to.with_c_str(|to| {
            // SAFETY: both descriptors are live and both names NUL-terminated.
            check(unsafe {
                libc::renameat(
                    from_dir.as_fd().as_raw_fd(),
                    from.as_ptr(),
                    to_dir.as_fd().as_raw_fd(),
                    to.as_ptr(),
                )
            })
        })
    })
}

/// `linkat(2)`: create `to` under `to_dir` as a new hard link to `from` under
/// `from_dir`.
///
/// # Errors
/// The raw `errno` from the syscall, or [`Errno::INVAL`] for an unusable name.
pub fn linkat<F1: AsFd, F2: AsFd, P: Arg, Q: Arg>(
    from_dir: F1,
    from: P,
    to_dir: F2,
    to: Q,
    flags: AtFlags,
) -> Result<()> {
    from.with_c_str(|from| {
        to.with_c_str(|to| {
            // SAFETY: both descriptors are live and both names NUL-terminated.
            check(unsafe {
                libc::linkat(
                    from_dir.as_fd().as_raw_fd(),
                    from.as_ptr(),
                    to_dir.as_fd().as_raw_fd(),
                    to.as_ptr(),
                    flags.bits(),
                )
            })
        })
    })
}

/// A rename with platform rename FLAGS — the non-clobbering publish, and the
/// atomic swap.
///
/// See this module's header for why the two platforms are implemented
/// separately and why everything else returns `ENOSYS` instead of falling back
/// to a plain rename.
///
/// # Errors
/// The raw `errno` from the syscall — including whatever the kernel returns for
/// a flag combination it will not accept, which is never second-guessed here;
/// [`Errno::INVAL`] for an unusable name; [`Errno::NOSYS`] on a platform with no
/// flagged rename at all.
pub fn renameat_with<F1: AsFd, F2: AsFd, P: Arg, Q: Arg>(
    from_dir: F1,
    from: P,
    to_dir: F2,
    to: Q,
    flags: RenameFlags,
) -> Result<()> {
    if flags == RenameFlags::empty() {
        return renameat(from_dir, from, to_dir, to);
    }
    from.with_c_str(|from_c| {
        to.with_c_str(|to_c| {
            let from_fd = from_dir.as_fd().as_raw_fd();
            let to_fd = to_dir.as_fd().as_raw_fd();
            #[cfg(any(target_os = "linux", target_os = "android"))]
            {
                /// `RENAME_NOREPLACE` (Linux `include/uapi/linux/fs.h`).
                const RENAME_NOREPLACE: libc::c_uint = 1 << 0;
                /// `RENAME_EXCHANGE`.
                const RENAME_EXCHANGE: libc::c_uint = 1 << 1;
                let mut raw: libc::c_uint = 0;
                if flags.contains(RenameFlags::NOREPLACE) {
                    raw |= RENAME_NOREPLACE;
                }
                if flags.contains(RenameFlags::EXCHANGE) {
                    raw |= RENAME_EXCHANGE;
                }
                // SAFETY: the raw syscall form is used because glibc gained a
                // `renameat2` wrapper only in 2.28 and musl has none; the
                // argument list is the kernel's. All four inputs are live.
                let ret = unsafe {
                    libc::syscall(
                        libc::SYS_renameat2,
                        from_fd,
                        from_c.as_ptr(),
                        to_fd,
                        to_c.as_ptr(),
                        raw,
                    )
                };
                if ret < 0 { Err(Errno::last()) } else { Ok(()) }
            }
            #[cfg(target_vendor = "apple")]
            {
                // `libc` already binds `renameatx_np` and both flag values, so
                // they are used from there rather than re-declared: a foreign
                // symbol declared twice is a constant that a `libc` bump can no
                // longer correct. (`RENAME_SWAP` = 0x2, `RENAME_EXCL` = 0x4,
                // `sys/stdio.h`.)
                let mut raw: libc::c_uint = 0;
                if flags.contains(RenameFlags::NOREPLACE) {
                    raw |= libc::RENAME_EXCL;
                }
                if flags.contains(RenameFlags::EXCHANGE) {
                    raw |= libc::RENAME_SWAP;
                }
                // SAFETY: both descriptors are live and both names are
                // NUL-terminated for the duration of the call.
                check(unsafe {
                    libc::renameatx_np(from_fd, from_c.as_ptr(), to_fd, to_c.as_ptr(), raw)
                })
            }
            #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
            {
                let _ = (from_fd, to_fd, from_c, to_c);
                // NOT a plain renameat: that would REPLACE the destination,
                // which is the exact race NOREPLACE exists to close.
                Err(Errno::NOSYS)
            }
        })
    })
}

/// `fchmod(2)`.
///
/// # Errors
/// The raw `errno` from the syscall.
pub fn fchmod<Fd: AsFd>(fd: Fd, mode: Mode) -> Result<()> {
    // SAFETY: `fd` is a live descriptor.
    check(unsafe { libc::fchmod(fd.as_fd().as_raw_fd(), mode.bits()) })
}

/// `fsync(2)`.
///
/// # Errors
/// The raw `errno` from the syscall.
pub fn fsync<Fd: AsFd>(fd: Fd) -> Result<()> {
    // SAFETY: `fd` is a live descriptor.
    check(unsafe { libc::fsync(fd.as_fd().as_raw_fd()) })
}

// ---------------------------------------------------------------------------
// Directory reading
// ---------------------------------------------------------------------------

/// One directory entry. Owns its name, so the iterator may advance.
#[derive(Debug, Clone)]
pub struct DirEntry {
    name: std::ffi::CString,
}

impl DirEntry {
    /// The entry's name. `.` and `..` ARE returned, as `readdir` returns them;
    /// callers filter.
    #[must_use]
    pub fn file_name(&self) -> &CStr {
        &self.name
    }
}

/// A directory being read, over an INDEPENDENT open file description.
///
/// Two separate reasons force that, and only the first is obvious:
///
/// * `fdopendir` takes ownership of the descriptor it is given and `closedir`
///   closes it, so handing it the caller's retained pin would close the very
///   handle the whole design exists to keep alive.
/// * A `dup` would not do, even though it answers the first reason. A duplicate
///   shares the original's OPEN FILE DESCRIPTION, and therefore its offset.
///   The caller is exactly the case that bites: a pinned directory is cloned
///   across threads over one `Arc<OwnedFd>`, so two streams built by
///   duplication would split one directory's entries between them — each
///   silently returning a PARTIAL listing — and a `rewinddir` on either would
///   restart the other mid-iteration. Measured on a 2000-entry directory, two
///   interleaved duplicate-based streams saw 986 and 1016 entries; two
///   `openat(".")`-based streams saw 2002 and 2002.
///
/// So the stream is built from `openat(fd, ".")`, which is still fully
/// fd-relative — `"."` resolves against the descriptor and cannot be redirected
/// by a rename or a planted symlink — and which starts at offset 0 with no
/// rewind needed.
pub struct Dir {
    dirp: *mut libc::DIR,
    /// Latched by the first failed `readdir`. A stream that has faulted is not
    /// required to return anything meaningful afterwards, so iteration ends
    /// there rather than calling `readdir` again on it.
    errored: bool,
}

// SAFETY: a `DIR*` is not shared — this type owns it exclusively and closes it
// on drop — so it may move between threads. It is deliberately NOT `Sync`:
// `readdir` mutates the stream's internal position.
unsafe impl Send for Dir {}

impl Dir {
    /// Open a directory stream over `fd`, on its own file description,
    /// positioned at the start.
    ///
    /// # Errors
    /// The raw `errno` from `fcntl`, `openat` or `fdopendir`.
    pub fn read_from<Fd: AsFd>(fd: Fd) -> Result<Self> {
        let borrowed = fd.as_fd();
        // The caller's status flags are carried over so a stream is opened the
        // same way the pin was; `O_CLOEXEC` is added because every descriptor
        // this crate creates is close-on-exec.
        // SAFETY: `borrowed` is a live descriptor.
        let status = unsafe { libc::fcntl(borrowed.as_raw_fd(), libc::F_GETFL) };
        if status < 0 {
            return Err(Errno::last());
        }
        let mut errored = false;
        let owned = match openat(
            borrowed,
            ".",
            OFlags(status) | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(owned) => owned,
            // `"."` is gone, so the directory itself was removed while the pin
            // held it open. There is nothing left to enumerate; read it as an
            // empty directory rather than failing a sweep whose work is done.
            Err(Errno::NOENT) => {
                errored = true;
                // SAFETY: `borrowed` is live; F_DUPFD_CLOEXEC returns a new
                // descriptor. Sharing the offset is harmless here precisely
                // because `errored` makes the stream yield nothing.
                let dup = unsafe { libc::fcntl(borrowed.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
                if dup < 0 {
                    return Err(Errno::last());
                }
                // SAFETY: `dup` is a fresh, exclusively-owned descriptor.
                unsafe { OwnedFd::from_raw_fd(dup) }
            }
            Err(err) => return Err(err),
        };
        let raw = owned.into_raw_fd();
        // SAFETY: `raw` is a fresh directory descriptor this function owns; on
        // success `fdopendir` takes ownership of it.
        let dirp = unsafe { libc::fdopendir(raw) };
        if dirp.is_null() {
            let err = Errno::last();
            // `fdopendir` did NOT take ownership on failure, so the descriptor
            // is still ours to close.
            // SAFETY: `raw` is a live descriptor this function owns.
            unsafe { libc::close(raw) };
            return Err(err);
        }
        Ok(Self { dirp, errored })
    }
}

/// `readdir` under the name that returns 64-bit inode and offset fields.
///
/// On a 32-bit glibc built without `_FILE_OFFSET_BITS=64`, plain `readdir` can
/// fail with `EOVERFLOW` on an inode or offset that `readdir64` returns fine.
/// `rustix` binds `readdir64` for every `linux_like` target for exactly that
/// reason, and the oracle cannot see the difference because it only ever runs
/// on the host.
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn readdir_ffi(dirp: *mut libc::DIR) -> *mut libc::dirent64 {
    // SAFETY: forwarded from the caller, which owns a live stream.
    unsafe { libc::readdir64(dirp) }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
unsafe fn readdir_ffi(dirp: *mut libc::DIR) -> *mut libc::dirent {
    // SAFETY: forwarded from the caller, which owns a live stream.
    unsafe { libc::readdir(dirp) }
}

/// The thread's `errno` slot, so it can be CLEARED before a `readdir`.
///
/// `readdir` returns NULL for both end-of-directory and error, and the only way
/// to tell them apart is `errno` — which it does not set on a clean end. Two
/// platforms are named because those are the two aterm ships; anywhere else the
/// slot is unavailable and a NULL is read as end-of-directory, which is what
/// every implementation that cannot clear errno must do.
#[cfg(target_vendor = "apple")]
fn errno_slot() -> Option<*mut libc::c_int> {
    // SAFETY: `__error` returns this thread's errno location.
    Some(unsafe { libc::__error() })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn errno_slot() -> Option<*mut libc::c_int> {
    // SAFETY: `__errno_location` returns this thread's errno location.
    Some(unsafe { libc::__errno_location() })
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn errno_slot() -> Option<*mut libc::c_int> {
    None
}

impl Iterator for Dir {
    type Item = Result<DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        // A stream that has already faulted is not required to return anything
        // meaningful, so it is never read again.
        if self.errored {
            return None;
        }
        let slot = errno_slot();
        if let Some(slot) = slot {
            // SAFETY: `slot` is this thread's own errno location.
            unsafe { *slot = 0 };
        }
        // SAFETY: `self.dirp` is a live stream owned by this value.
        let entry = unsafe { readdir_ffi(self.dirp) };
        if entry.is_null() {
            let err = Errno::last();
            // With no way to clear errno beforehand, a stale value would turn
            // a clean end-of-directory into a spurious error; end it instead.
            return if slot.is_none() || err.0 == 0 {
                None
            } else {
                self.errored = true;
                Some(Err(err))
            };
        }
        // SAFETY: a non-null `readdir` result points at a valid `dirent` owned
        // by the stream, whose `d_name` is NUL-terminated. The name is COPIED
        // here, before the next `readdir` invalidates it.
        let name = unsafe { CStr::from_ptr((&raw const (*entry).d_name).cast()) };
        Some(Ok(DirEntry {
            name: name.to_owned(),
        }))
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        // SAFETY: `self.dirp` is a live stream this value owns, closed once.
        unsafe { libc::closedir(self.dirp) };
    }
}
