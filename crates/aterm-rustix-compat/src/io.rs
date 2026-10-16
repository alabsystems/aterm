// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `rustix::io`: errno, `read`, `write` and `ioctl_fionbio`.

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

use libc::c_int;

/// A specialized [`Result`](core::result::Result) whose error is an [`Errno`].
pub type Result<T> = core::result::Result<T, Errno>;

/// An OS error code (a positive `errno` value).
///
/// The named values are the ones a consumer matches on (`AGAIN`, `EXIST`,
/// `INTR`, `NOSYS`, `PROTO`) and the ones this crate returns itself (`INVAL`,
/// `NAMETOOLONG`); any other value is carried as it came.
#[repr(transparent)]
#[doc(alias = "errno")]
#[derive(Eq, PartialEq, Hash, Copy, Clone)]
pub struct Errno(c_int);

impl Errno {
    /// `EAGAIN`
    pub const AGAIN: Self = Self(libc::EAGAIN);
    /// `EWOULDBLOCK` (the same value as `EAGAIN` on Linux)
    pub const WOULDBLOCK: Self = Self(libc::EWOULDBLOCK);
    /// `EEXIST`
    pub const EXIST: Self = Self(libc::EEXIST);
    /// `EINTR`
    pub const INTR: Self = Self(libc::EINTR);
    /// `EINVAL`
    pub const INVAL: Self = Self(libc::EINVAL);
    /// `ENAMETOOLONG`
    pub const NAMETOOLONG: Self = Self(libc::ENAMETOOLONG);
    /// `ENOSYS`
    pub const NOSYS: Self = Self(libc::ENOSYS);
    /// `EPROTO`
    pub const PROTO: Self = Self(libc::EPROTO);

    /// The error code of `io_err`, if it is an OS error in Linux's errno
    /// range (`1..4096`).
    #[inline]
    pub fn from_io_error(io_err: &std::io::Error) -> Option<Self> {
        io_err
            .raw_os_error()
            .filter(|raw| (1..4096).contains(raw))
            .map(Self)
    }

    /// The raw `errno` value.
    #[inline]
    pub const fn raw_os_error(self) -> i32 {
        self.0
    }

    /// An `Errno` from a raw `errno` value.
    #[inline]
    pub const fn from_raw_os_error(raw: i32) -> Self {
        Self(raw)
    }

    /// Shorthand for `std::io::Error::from(self).kind()`.
    #[inline]
    pub fn kind(self) -> std::io::ErrorKind {
        std::io::Error::from(self).kind()
    }

    /// The calling thread's `errno`, read right after a libc call failed.
    #[inline]
    pub(crate) fn last() -> Self {
        Self(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }
}

impl core::fmt::Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&std::io::Error::from(*self), f)
    }
}

impl core::fmt::Debug for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&std::io::Error::from(*self), f)
    }
}

impl std::error::Error for Errno {}

impl From<Errno> for std::io::Error {
    #[inline]
    fn from(err: Errno) -> Self {
        Self::from_raw_os_error(err.0)
    }
}

/// A libc return value that is `-1` exactly on failure, with `errno` set.
#[inline]
pub(crate) fn check(ret: c_int) -> Result<c_int> {
    if ret == -1 {
        Err(Errno::last())
    } else {
        Ok(ret)
    }
}

/// A libc return value that is a new descriptor, or `-1` with `errno` set.
#[inline]
pub(crate) fn check_fd(ret: c_int) -> Result<OwnedFd> {
    let fd = check(ret)?;
    // SAFETY: the call just created `fd` and returned it to us alone; nothing
    // else holds it, so this is its one owner.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `read(fd, buf)`: reads up to `buf.len()` bytes; `Ok(0)` at end of file.
#[inline]
pub fn read<Fd: AsFd>(fd: Fd, buf: &mut [u8]) -> Result<usize> {
    // SAFETY: `buf` is valid for writes of `buf.len()` bytes for the whole
    // call, and the descriptor is borrowed for it.
    let n = unsafe { libc::read(fd.as_fd().as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
    // `read` returns -1 or a count no larger than `buf.len()`.
    usize::try_from(n).map_err(|_| Errno::last())
}

/// `write(fd, buf)`: writes up to `buf.len()` bytes.
#[inline]
pub fn write<Fd: AsFd>(fd: Fd, buf: &[u8]) -> Result<usize> {
    // SAFETY: `buf` is valid for reads of `buf.len()` bytes for the whole
    // call, and the descriptor is borrowed for it.
    let n = unsafe { libc::write(fd.as_fd().as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
    usize::try_from(n).map_err(|_| Errno::last())
}

/// `ioctl(fd, FIONBIO, &value)`: sets or clears non-blocking mode.
#[inline]
#[doc(alias = "FIONBIO")]
pub fn ioctl_fionbio<Fd: AsFd>(fd: Fd, value: bool) -> Result<()> {
    let mut arg = c_int::from(value);
    // SAFETY: FIONBIO reads one `int` through the pointer, which is live for
    // the call.
    let ret = unsafe { libc::ioctl(fd.as_fd().as_raw_fd(), libc::FIONBIO, &mut arg) };
    check(ret).map(drop)
}
