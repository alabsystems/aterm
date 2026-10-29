// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The unix half of `aterm-dirfd`.

use std::ffi::{CStr, OsStr};
use std::os::fd::{FromRawFd as _, IntoRawFd as _};
use std::os::unix::ffi::OsStrExt as _;

pub use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};

/// A raw POSIX `errno`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Errno(pub i32);

impl Errno {
    /// `ENOENT` — no such file or directory.
    pub const NOENT: Self = Self(libc::ENOENT);
    /// `EINVAL` — used here for a name that cannot become a C string.
    pub const INVAL: Self = Self(libc::EINVAL);
    /// `ENOSYS` — the platform has no such operation.
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    pub const NOSYS: Self = Self(libc::ENOSYS);

    /// The raw `errno` value.
    #[inline]
    #[must_use]
    pub const fn raw_os_error(self) -> i32 {
        self.0
    }

    /// Return `true` when this errno is `ENOENT`.
    #[inline]
    #[must_use]
    pub const fn is_noent(self) -> bool {
        self.0 == libc::ENOENT
    }

    /// Return `true` when this errno is `EINVAL`.
    #[inline]
    #[must_use]
    pub const fn is_inval(self) -> bool {
        self.0 == libc::EINVAL
    }

    /// The current thread's `errno`, read immediately after a failed call.
    #[inline]
    fn last() -> Self {
        Self(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }
}

/// Encode a directory-relative inode identity (`dev`, `ino`) into 16 little-endian bytes.
#[inline]
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
#[inline]
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

/// A name handed to a `*at` syscall, or one of the `/` and `.` paths `PinnedDir`'s chain opens (`openat(CWD, "/")`, `openat(leaf, ".")`).
pub trait Arg {
    /// Materialise the argument as a NUL-terminated C string and run `f`.
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
        const SMALL: usize = 512;
        if bytes.len() < SMALL {
            let mut buf = [0u8; SMALL];
            buf[..bytes.len()].copy_from_slice(bytes);
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
        /// `O_EXCL` — with `O_CREAT`, fail if the name already exists.
        EXCL = libc::O_EXCL;
        /// `O_DIRECTORY` — fail unless the name is a directory.
        DIRECTORY = libc::O_DIRECTORY;
        /// `O_NOFOLLOW` — fail if the final component is a symlink.
        NOFOLLOW = libc::O_NOFOLLOW;
        /// `O_NONBLOCK` — open without blocking on a FIFO or device.
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
        /// `S_IXGRP` — 0o010.
        XGRP = libc::S_IXGRP;
        /// `S_IROTH` — 0o004.
        ROTH = libc::S_IROTH;
        /// `S_IWOTH` — 0o002.
        WOTH = libc::S_IWOTH;
        /// `S_IXOTH` — 0o001.
        XOTH = libc::S_IXOTH;
    }
}

/// Flags for [`renameat_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RenameFlags(u8);

impl RenameFlags {
    /// Fail with `EEXIST` if the destination already exists, instead of replacing it.
    pub const NOREPLACE: Self = Self(1);

    /// No flags — equivalent to a plain [`renameat`].
    #[inline]
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Whether every bit of `other` is set here.
    #[inline]
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for RenameFlags {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// `AT_FDCWD`, as a descriptor usable wherever a directory descriptor is taken.
pub const CWD: BorrowedFd<'static> = {
    // SAFETY: `AT_FDCWD` is the kernel's "current directory" sentinel, valid for every `*at` call, and is not -1 as `borrow_raw` requires.
    unsafe { BorrowedFd::borrow_raw(libc::AT_FDCWD) }
};

/// The subset of `struct stat` these callers read, widened to fixed sizes so the call sites do not vary by platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    /// Device the inode lives on.
    pub st_dev: u64,
    /// Inode number.
    pub st_ino: u64,
    /// Type and permission bits.
    pub st_mode: u32,
    /// Hard-link count.
    pub st_nlink: u64,
    /// The owner's user id.
    pub st_uid: u32,
    /// The 512-byte blocks allocated to it: what its removal gives back once its last name goes.
    pub st_blocks: u64,
}

impl Stat {
    /// The fields read off a raw `struct stat`.
    #[cfg_attr(
        any(target_os = "linux", target_os = "android"),
        expect(
            clippy::useless_conversion,
            reason = "st_mode is u32 here and u16 on Apple"
        )
    )]
    #[allow(
        clippy::unnecessary_cast,
        reason = "dev_t, ino_t and nlink_t differ in width by target (i32, u64 and u16 on Apple; \
                  u64, u64 and u32 or u64 on Linux), so a cast one target needs is the same type on another"
    )]
    fn of(raw: &libc::stat) -> Self {
        Self {
            st_dev: raw.st_dev as u64,
            st_ino: raw.st_ino as u64,
            st_mode: u32::from(raw.st_mode),
            st_nlink: raw.st_nlink as u64,
            st_uid: raw.st_uid,
            st_blocks: u64::try_from(raw.st_blocks).unwrap_or(0),
        }
    }
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
    #[inline]
    #[must_use]
    pub const fn from_raw_mode(mode: u32) -> Self {
        Self(mode & libc::S_IFMT as u32)
    }

    /// A directory.
    #[inline]
    #[must_use]
    pub const fn is_dir(self) -> bool {
        self.0 == libc::S_IFDIR as u32
    }

    /// A regular file.
    #[inline]
    #[must_use]
    pub const fn is_file(self) -> bool {
        self.0 == libc::S_IFREG as u32
    }
}

/// `openat(2)`: open `name` relative to the directory `dirfd`.
///
/// # Errors
/// The raw `errno` from the syscall, or [`Errno::INVAL`] for an unusable name.
pub fn openat<Fd: AsFd, P: Arg>(dirfd: Fd, name: P, flags: OFlags, mode: Mode) -> Result<OwnedFd> {
    name.with_c_str(|name| {
        // SAFETY: `dirfd` is a live descriptor for the duration of the call and `name` is a NUL-terminated C string valid for it.
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

/// `fstat(2)`: identity and type of an already-open descriptor, independent of whatever a name points at now.
///
/// # Errors
/// The raw `errno` from the syscall.
pub fn fstat<Fd: AsFd>(fd: Fd) -> Result<Stat> {
    // SAFETY: `stat` is a plain C struct with no invalid bit patterns for the fields read below, and `fstat` fills it before we look.
    let mut raw: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is live and `raw` is a writable `struct stat`.
    check(unsafe { libc::fstat(fd.as_fd().as_raw_fd(), &raw mut raw) })?;
    Ok(Stat::of(&raw))
}

/// `fstatat(2)`: the entry `name` of the directory `dirfd`, a link at `name` read as itself under
/// [`AtFlags::SYMLINK_NOFOLLOW`]. `name` is looked up in `dirfd` alone: no path above it is
/// resolved again.
///
/// # Errors
/// The raw `errno` from the syscall, or [`Errno::INVAL`] for an unusable name.
pub fn fstatat<Fd: AsFd, P: Arg>(dirfd: Fd, name: P, flags: AtFlags) -> Result<Stat> {
    name.with_c_str(|name| {
        // SAFETY: `stat` is a plain C struct with no invalid bit patterns for the fields read, and `fstatat` fills it before it is read.
        let mut raw: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: live descriptor, NUL-terminated name, writable `struct stat`.
        check(unsafe {
            libc::fstatat(
                dirfd.as_fd().as_raw_fd(),
                name.as_ptr(),
                &raw mut raw,
                flags.bits(),
            )
        })?;
        Ok(Stat::of(&raw))
    })
}

/// `fchmodat(2)`: set the permission bits of the entry `name` of the directory `dirfd`; under
/// [`AtFlags::SYMLINK_NOFOLLOW`] a link at `name` is never followed (Linux refuses a link then,
/// and needs `/proc` for anything else).
///
/// # Errors
/// The raw `errno` from the syscall, or [`Errno::INVAL`] for an unusable name.
pub fn fchmodat<Fd: AsFd, P: Arg>(dirfd: Fd, name: P, mode: Mode, flags: AtFlags) -> Result<()> {
    name.with_c_str(|name| {
        // SAFETY: live descriptor, NUL-terminated name.
        check(unsafe {
            libc::fchmodat(
                dirfd.as_fd().as_raw_fd(),
                name.as_ptr(),
                mode.bits(),
                flags.bits(),
            )
        })
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

/// `unlinkat(2)`.
///
/// # Errors
/// The raw `errno` from the syscall, or [`Errno::INVAL`] for an unusable name.
pub fn unlinkat<Fd: AsFd, P: Arg>(dirfd: Fd, name: P, flags: AtFlags) -> Result<()> {
    name.with_c_str(|name| {
        // SAFETY: live descriptor, NUL-terminated name.
        check(unsafe { libc::unlinkat(dirfd.as_fd().as_raw_fd(), name.as_ptr(), flags.bits()) })
    })
}

/// `renameat(2)`.
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

/// `linkat(2)`: create `to` under `to_dir` as a new hard link to `from` under `from_dir`.
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

/// A rename with platform rename flags.
///
/// # Errors
/// The raw `errno` from the syscall (a flag combination the kernel refuses is passed through).
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
                // SAFETY: both descriptors are live and both names are NUL-terminated for the duration of the call.
                check(unsafe {
                    libc::renameatx_np(from_fd, from_c.as_ptr(), to_fd, to_c.as_ptr(), raw)
                })
            }
            #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
            {
                let _ = (from_fd, to_fd, from_c, to_c);
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

/// One directory entry.
#[derive(Debug, Clone)]
pub struct DirEntry {
    name: std::ffi::CString,
}

impl DirEntry {
    /// The entry's name.
    #[inline]
    #[must_use]
    pub fn file_name(&self) -> &CStr {
        &self.name
    }
}

/// A directory being read: over an independent open file description ([`Dir::read_from`]), or
/// over the descriptor it was handed ([`Dir::new`]), which it then holds as its own ([`AsFd`]).
pub struct Dir {
    dirp: *mut libc::DIR,
    /// Latched by the first failed `readdir` so iteration ends there, until [`Dir::rewind`].
    errored: bool,
    /// A stream over a directory already removed, sharing the caller's file offset: it yields
    /// nothing, ever (a rewind would move the caller's offset).
    gone: bool,
}

// SAFETY: this type owns its `DIR*` exclusively and closes it on drop, so it may
// move between threads. Not `Sync`: `readdir` mutates the stream position.
unsafe impl Send for Dir {}

impl Dir {
    /// Open a directory stream over `fd`, on its own file description, positioned at the start.
    ///
    /// # Errors
    /// The raw `errno` from `fcntl`, `openat` or `fdopendir`.
    pub fn read_from<Fd: AsFd>(fd: Fd) -> Result<Self> {
        let borrowed = fd.as_fd();
        // SAFETY: `borrowed` is a live descriptor.
        let status = unsafe { libc::fcntl(borrowed.as_raw_fd(), libc::F_GETFL) };
        if status < 0 {
            return Err(Errno::last());
        }
        let mut gone = false;
        let owned = match openat(
            borrowed,
            ".",
            OFlags(status) | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(owned) => owned,
            Err(Errno::NOENT) => {
                gone = true;
                // SAFETY: `borrowed` is live; F_DUPFD_CLOEXEC returns a new
                // descriptor. Sharing the offset is harmless because `gone`
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
            // SAFETY: `raw` is a live descriptor this function owns.
            unsafe { libc::close(raw) };
            return Err(err);
        }
        Ok(Self {
            dirp,
            errored: false,
            gone,
        })
    }

    /// A directory stream over `fd` itself, which it takes (`fdopendir`): one descriptor for the
    /// directory and its listing, as `std::fs::remove_dir_all` holds one per level. The
    /// descriptor stays the stream's to use for `*at` calls, `fstat`, `fchmod` and the like
    /// ([`AsFd`]); its file offset is the stream's, so nothing else reads through it.
    ///
    /// # Errors
    /// The raw `errno` from `fdopendir` (`fd` is closed then).
    pub fn new(fd: OwnedFd) -> Result<Self> {
        let raw = fd.into_raw_fd();
        // SAFETY: `raw` is a live descriptor this function owns; on success `fdopendir` takes
        // ownership of it.
        let dirp = unsafe { libc::fdopendir(raw) };
        if dirp.is_null() {
            let err = Errno::last();
            // SAFETY: `raw` is a live descriptor this function still owns.
            unsafe { libc::close(raw) };
            return Err(err);
        }
        Ok(Self {
            dirp,
            errored: false,
            gone: false,
        })
    }

    /// Back to the directory's first entry (`rewinddir`), so a directory emptied as it was read
    /// is read again whole; a failed read no longer ends the stream.
    pub fn rewind(&mut self) {
        // SAFETY: `self.dirp` is a live stream owned by this value.
        unsafe { libc::rewinddir(self.dirp) };
        self.errored = false;
    }
}

impl AsFd for Dir {
    /// The descriptor the stream reads through, open until the stream is dropped.
    fn as_fd(&self) -> BorrowedFd<'_> {
        // SAFETY: `self.dirp` is a live stream owned by this value, so `dirfd` names the
        // descriptor it holds open until `closedir` in `drop`, which the borrow of `self` the
        // returned value carries cannot outlive.
        unsafe { BorrowedFd::borrow_raw(libc::dirfd(self.dirp)) }
    }
}

/// `readdir` returning 64-bit inode and offset fields to avoid `EOVERFLOW` on 32-bit glibc.
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
        if self.errored || self.gone {
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
            if err.is_noent() {
                self.errored = true;
                return None;
            }
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
