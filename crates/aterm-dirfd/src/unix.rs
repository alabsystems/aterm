// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The unix half of `aterm-dirfd`: the fd-relative POSIX directory operations
//! aterm's confined artifact and media paths are built on, over `libc`
//! directly. Its items are the crate's root on a unix target.
//!
//! ## The property this exists to preserve
//!
//! Every operation here is **fd-relative and re-resolves nothing**. A `PathBuf`
//! is a claim about one instant: a same-uid process can rename an ancestor and
//! leave a symlink at the old pathname before a worker writes. `PinnedDir`
//! answers that by opening each absolute-path component with `O_NOFOLLOW`,
//! RETAINING the descriptors, and doing all later work relative to the leaf.
//!
//! So the contract of this crate is narrow and strict:
//!
//! * Every entry point takes a directory descriptor plus a NAME (or the `/` and
//!   `.` paths `PinnedDir`'s chain opens), and issues the `*at` syscall for it.
//!   No wrapper here ever joins, canonicalizes, walks or re-opens a path.
//!   [`Arg`] checks NUL only; it does NOT check that a name is one component,
//!   so a caller taking a name from elsewhere judges it first (ruling
//!   2026-10-10, FS-1: see [`Arg`]).
//! * No entry point follows a symlink on its own initiative; whether links are
//!   followed is the caller's [`OFlags`]/[`AtFlags`] decision, unchanged. Those
//!   flags govern the FINAL component only: the kernel resolves every earlier
//!   one, so `"link/"` reaches through the symlink `link`.
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
//! * **Linux** — `renameat2(2)` with `RENAME_NOREPLACE`,
//!   issued through `syscall(2)` so it works on a musl or pre-2.28 glibc that
//!   has no wrapper for it.
//! * **macOS** — `renameatx_np(2)` with `RENAME_EXCL`, the documented Darwin
//!   equivalent.
//! * **Anywhere else** — [`RenameFlags`] operations are REFUSED with `ENOSYS`:
//!   a plain `renameat` would clobber an existing destination, the race
//!   `RENAME_NOREPLACE` exists to close. Callers select a `linkat` + `unlinkat`
//!   fallback there at compile time.
//!
//! ## Tests
//!
//! `tests/dirfd.rs` drives this crate and `std::fs` over identical real trees
//! and requires agreement on every result, errno, `stat` field, listing and
//! the tree left behind.

use std::ffi::{CStr, OsStr};
use std::os::fd::{FromRawFd as _, IntoRawFd as _};
use std::os::unix::ffi::OsStrExt as _;

pub use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};

// ---------------------------------------------------------------------------
// Errno
// ---------------------------------------------------------------------------

/// A raw POSIX `errno`.
///
/// Not an `io::Error`, so callers can match specific values (`Errno::NOENT`)
/// in patterns. Converting to `io::Error` is one `.into()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Errno(pub i32);

impl Errno {
    /// `ENOENT` — no such file or directory. The one code the artifact cleanup
    /// path treats as success rather than failure.
    pub const NOENT: Self = Self(libc::ENOENT);
    /// `EINVAL` — used here for a name that cannot become a C string.
    pub const INVAL: Self = Self(libc::EINVAL);
    /// `ENOSYS` — the platform has no such operation. Returned instead of
    /// approximating one, by the flagged rename on a platform with none.
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    pub const NOSYS: Self = Self(libc::ENOSYS);

    /// The raw `errno` value.
    #[must_use]
    pub const fn raw_os_error(self) -> i32 {
        self.0
    }

    /// Return `true` when this errno is `ENOENT`.
    #[must_use]
    pub const fn is_noent(self) -> bool {
        self.0 == libc::ENOENT
    }

    /// Return `true` when this errno is `EINVAL`.
    #[must_use]
    pub const fn is_inval(self) -> bool {
        self.0 == libc::EINVAL
    }

    /// The current thread's `errno`, read immediately after a failed call.
    fn last() -> Self {
        Self(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }
}

/// Encode a directory-relative inode identity (`dev`, `ino`) into 16 little-endian bytes.
#[must_use]
pub const fn encode_inode_identity_le(dev: u64, ino: u64) -> [u8; 16] {
    [
        dev as u8,
        (dev >> 8) as u8,
        (dev >> 16) as u8,
        (dev >> 24) as u8,
        (dev >> 32) as u8,
        (dev >> 40) as u8,
        (dev >> 48) as u8,
        (dev >> 56) as u8,
        ino as u8,
        (ino >> 8) as u8,
        (ino >> 16) as u8,
        (ino >> 24) as u8,
        (ino >> 32) as u8,
        (ino >> 40) as u8,
        (ino >> 48) as u8,
        (ino >> 56) as u8,
    ]
}

/// Decode 16 little-endian bytes into a directory-relative inode identity (`dev`, `ino`).
#[must_use]
pub const fn decode_inode_identity_le(bytes: [u8; 16]) -> (u64, u64) {
    let dev = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let ino = (bytes[8] as u64)
        | ((bytes[9] as u64) << 8)
        | ((bytes[10] as u64) << 16)
        | ((bytes[11] as u64) << 24)
        | ((bytes[12] as u64) << 32)
        | ((bytes[13] as u64) << 40)
        | ((bytes[14] as u64) << 48)
        | ((bytes[15] as u64) << 56);
    (dev, ino)
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

/// A name handed to a `*at` syscall, or one of the `/` and `.` paths
/// `PinnedDir`'s chain opens (`openat(CWD, "/")`, `openat(leaf, ".")`).
///
/// It checks NUL only and does NOT check that a name is one component: any
/// path passes. Ruling 2026-10-10 (FS-1): `Arg` stays able to carry those
/// paths, so the one-name rule belongs to the caller. A caller taking a name
/// from elsewhere judges it on its RAW bytes first (aterm-gui's
/// `pinned_dir::validate_component`), never through `Path::components()`,
/// which normalizes a trailing `/`, `/.` and `//` away. `O_NOFOLLOW`,
/// `unlinkat` and `renameat` govern only the final component, so `"link/"`
/// reaches through the symlink `link`.
///
/// Implemented once, over `AsRef<OsStr>`, which covers `&str`, `&OsStr`,
/// `&OsString`, `&Path` and `&PathBuf` — every shape the call sites use.
pub trait Arg {
    /// Materialise the argument as a NUL-terminated C string and run `f`.
    ///
    /// # Errors
    /// [`Errno::INVAL`] if the name contains an interior NUL (truncating would
    /// address a different file).
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
        /// `O_APPEND` — every write lands at the file's end.
        APPEND = libc::O_APPEND;
        /// `O_EXCL` — with `O_CREAT`, fail if the name already exists. The
        /// atomic "create or lose" the private-file path depends on.
        EXCL = libc::O_EXCL;
        /// `O_DIRECTORY` — fail unless the name is a directory.
        DIRECTORY = libc::O_DIRECTORY;
        /// `O_NOFOLLOW` — fail if the final component is a symlink. Applies to
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
        /// `S_IRGRP` — 0o040.
        RGRP = libc::S_IRGRP;
        /// `S_IWGRP` — 0o020.
        WGRP = libc::S_IWGRP;
        /// `S_IROTH` — 0o004.
        ROTH = libc::S_IROTH;
        /// `S_IWOTH` — 0o002.
        WOTH = libc::S_IWOTH;
    }
}

/// Flags for [`renameat_with`].
///
/// Carried abstractly and translated at the syscall, because the platforms use
/// different values for the same idea; a caller cannot pass a Linux constant on
/// Darwin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenameFlags(u8);

impl RenameFlags {
    /// Fail with `EEXIST` if the destination already exists, instead of
    /// replacing it. `RENAME_NOREPLACE` on Linux, `RENAME_EXCL` on Darwin.
    pub const NOREPLACE: Self = Self(1);

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

/// `AT_FDCWD`, as a descriptor usable wherever a directory descriptor is taken.
///
/// Resolves names against the current directory; callers use it only to open
/// `/` and to re-open an absolute path for an identity check.
pub const CWD: BorrowedFd<'static> = {
    // SAFETY: `AT_FDCWD` is the kernel's "current directory" sentinel, valid for
    // every `*at` call, and is not -1 as `borrow_raw` requires.
    unsafe { BorrowedFd::borrow_raw(libc::AT_FDCWD) }
};

// ---------------------------------------------------------------------------
// stat
// ---------------------------------------------------------------------------

/// The subset of `struct stat` these callers read, widened to fixed sizes so
/// the call sites do not vary by platform. All four fields answer identity
/// questions: which inode on which device, what kind, how many names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    /// Device the inode lives on.
    pub st_dev: u64,
    /// Inode number.
    pub st_ino: u64,
    /// Type and permission bits.
    pub st_mode: u32,
    /// Hard-link count. Zero means fully unlinked; one means this is the only
    /// name, which the private-file guarantee rests on.
    pub st_nlink: u64,
}

/// A file's type, extracted from its mode bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileType(u32);

#[cfg_attr(
    any(
        target_os = "linux",
        all(target_os = "android", target_pointer_width = "64")
    ),
    expect(
        clippy::unnecessary_cast,
        reason = "mode_t is u32 here and u16 on Apple and 32-bit Android"
    )
)]
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

/// `fstat(2)`: identity and type of an already-open descriptor, independent of
/// whatever a name points at now.
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
        #[cfg_attr(
            any(target_os = "linux", target_os = "android"),
            expect(
                clippy::useless_conversion,
                reason = "st_mode is u32 here and u16 on Apple"
            )
        )]
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
/// Replaces an existing destination, as POSIX specifies; use [`renameat_with`]
/// with [`RenameFlags::NOREPLACE`] to refuse instead.
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

/// A rename with platform rename flags: the non-clobbering publish (see the
/// crate docs for the per-platform implementation).
///
/// # Errors
/// The raw `errno` from the syscall (a flag combination the kernel refuses is
/// passed through); [`Errno::INVAL`] for an unusable name; `ENOSYS` on a
/// platform with no flagged rename.
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
                let mut raw: libc::c_uint = 0;
                if flags.contains(RenameFlags::NOREPLACE) {
                    raw |= RENAME_NOREPLACE;
                }
                // SAFETY: raw syscall form because not every libc has a
                // `renameat2` wrapper; the argument list is the kernel's and all
                // four inputs are live.
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
                let mut raw: libc::c_uint = 0;
                if flags.contains(RenameFlags::NOREPLACE) {
                    raw |= libc::RENAME_EXCL;
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
                // Not a plain renameat: that would replace the destination,
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
    /// The entry's name. `.` and `..` are returned, as `readdir` returns them;
    /// callers filter.
    #[must_use]
    pub fn file_name(&self) -> &CStr {
        &self.name
    }
}

/// A directory being read, over an independent open file description.
///
/// * `fdopendir` takes ownership of its descriptor and `closedir` closes it,
///   so the caller's retained pin cannot be handed over.
/// * A `dup` shares the original's open file description and offset. A pinned
///   directory is cloned across threads over one `Arc<OwnedFd>`, so two
///   dup-based streams would split the entries between them and a `rewinddir`
///   on one would restart the other.
///
/// So the stream is built from `openat(fd, ".")`, which is still fd-relative
/// and starts at offset 0.
pub struct Dir {
    dirp: *mut libc::DIR,
    /// Latched by the first failed `readdir`; iteration ends there, since a
    /// faulted stream need not return anything meaningful.
    errored: bool,
}

// SAFETY: this type owns its `DIR*` exclusively and closes it on drop, so it may
// move between threads. Not `Sync`: `readdir` mutates the stream position.
unsafe impl Send for Dir {}

impl Dir {
    /// Open a directory stream over `fd`, on its own file description,
    /// positioned at the start.
    ///
    /// # Errors
    /// The raw `errno` from `fcntl`, `openat` or `fdopendir`.
    pub fn read_from<Fd: AsFd>(fd: Fd) -> Result<Self> {
        let borrowed = fd.as_fd();
        // Carry the caller's status flags so the stream opens like the pin, plus
        // `O_CLOEXEC` (every descriptor this crate creates is close-on-exec).
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
            // `"."` is gone: the directory was removed while the pin held it.
            // Read it as empty rather than failing a sweep whose work is done.
            Err(Errno::NOENT) => {
                errored = true;
                // SAFETY: `borrowed` is live; F_DUPFD_CLOEXEC returns a new
                // descriptor. Sharing the offset is harmless because `errored`
                // makes the stream yield nothing.
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
            // `fdopendir` does not take ownership on failure.
            // SAFETY: `raw` is a live descriptor this function owns.
            unsafe { libc::close(raw) };
            return Err(err);
        }
        Ok(Self { dirp, errored })
    }
}

/// `readdir` under the name that returns 64-bit inode and offset fields: on a
/// 32-bit glibc without `_FILE_OFFSET_BITS=64`, plain `readdir` can fail with
/// `EOVERFLOW`.
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

/// The thread's `errno` slot, cleared before each `readdir`.
///
/// `readdir` returns NULL for both end-of-directory and error and sets `errno`
/// only on error. On platforms other than the two named, the slot is
/// unavailable and a NULL reads as end-of-directory.
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
        // by the stream with a NUL-terminated `d_name`; the name is copied
        // before the next `readdir` invalidates it.
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
