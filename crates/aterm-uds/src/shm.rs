// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Anonymous POSIX shared-memory regions for the crash-restore journal.

use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering, fence};

const WORD: usize = 8;
const _: () = assert!(
    usize::BITS == 64 && std::mem::align_of::<AtomicU64>() == WORD,
    "the region's words need a 64-bit target"
);

/// Page size multiple required for every region (`16 KiB`).
pub const PAGE: usize = 16 * 1024;
/// Maximum region size accepted for mapping or custody (`64 MiB`).
pub const MAX_LEN: usize = 64 * 1024 * 1024;
const NAME_HEAD: &str = "/atrj.";
const NAME_RANDOM: usize = 25;
const NAME_TRIES: usize = 8;

/// Reason a mapped region access was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessError {
    /// `[at, at + len)` lies outside the region.
    OutOfBounds {
        /// Start offset of the access.
        at: usize,
        /// Byte length of the access.
        len: usize,
    },
    /// Word access at an offset that is not 8-byte aligned.
    Misaligned {
        /// Unaligned byte offset.
        at: usize,
    },
    /// Write attempted on a read-only mapping.
    ReadOnly,
}

impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfBounds { at, len } => write!(f, "{len} bytes at {at} run past the region"),
            Self::Misaligned { at } => write!(f, "a word access at {at} is not 8-byte aligned"),
            Self::ReadOnly => f.write_str("the region is mapped read-only"),
        }
    }
}

impl std::error::Error for AccessError {}

/// Mapped shared-memory region (read-write for creator, read-only for restorer).
#[derive(Debug)]
pub struct Region {
    base: NonNull<u8>,
    len: usize,
    writable: bool,
}

// SAFETY: the mapping is process-wide memory, not bound to a thread, and `Drop` unmaps it from whichever thread holds it.
unsafe impl Send for Region {}

/// The build fails here if `Region` is ever `Sync`: its plain `memcpy` copies must not race.
trait AmbiguousIfSync<A> {
    fn named() {}
}
impl<T: ?Sized> AmbiguousIfSync<()> for T {}
impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}
const _: fn() = || {
    let _ = <Region as AmbiguousIfSync<_>>::named;
};

fn last_os() -> io::Error {
    io::Error::last_os_error()
}

fn check_len(len: usize) -> io::Result<()> {
    if len == 0 || !len.is_multiple_of(PAGE) || len > MAX_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a region of {len} bytes: it must be a non-zero multiple of {PAGE} up to {MAX_LEN}"
            ),
        ));
    }
    Ok(())
}

fn fresh_name() -> io::Result<std::ffi::CString> {
    const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut raw = [0u8; NAME_RANDOM];
    crate::rand::fill(&mut raw)?;
    let mut name = String::with_capacity(NAME_HEAD.len() + NAME_RANDOM);
    name.push_str(NAME_HEAD);
    for b in raw {
        name.push(char::from(BASE32[usize::from(b & 0x1f)]));
    }
    std::ffi::CString::new(name).map_err(|_| io::Error::other("a region name held a NUL"))
}

/// `shm_open(name, oflag, 0600)`, its descriptor owned.
#[cfg_attr(trust_verify, trust::skip)]
fn open_named(name: &std::ffi::CStr, oflag: libc::c_int) -> io::Result<OwnedFd> {
    let mode: libc::c_uint = 0o600;
    // SAFETY: `name` is a live NUL-terminated string.
    let fd = unsafe { libc::shm_open(name.as_ptr(), oflag, mode) };
    if fd < 0 {
        return Err(last_os());
    }
    // SAFETY: wraps the fresh descriptor returned by `shm_open`.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `shm_unlink(name)`.
#[cfg_attr(trust_verify, trust::skip)]
fn unlink_named(name: &std::ffi::CStr) -> io::Result<()> {
    // SAFETY: `name` is a live NUL-terminated string.
    if unsafe { libc::shm_unlink(name.as_ptr()) } != 0 {
        return Err(last_os());
    }
    Ok(())
}

/// Mark `fd` close-on-exec: the custody descriptor goes to the keeper, never to a child.
#[cfg_attr(trust_verify, trust::skip)]
fn set_cloexec(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: sets `FD_CLOEXEC` on an open descriptor.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
        return Err(last_os());
    }
    Ok(())
}

/// Return the shared-memory object's byte size via `fstat`.
/// # Errors
/// Returns an error if `fstat` fails or the size is negative.
#[cfg_attr(trust_verify, trust::skip)]
pub fn fd_size(fd: BorrowedFd<'_>) -> io::Result<u64> {
    // SAFETY: `libc::stat` is plain old data valid when zeroed.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `st` is a valid out-parameter for the open descriptor `fd`.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(last_os());
    }
    u64::try_from(st.st_size).map_err(|_| io::Error::other("fstat reported a negative size"))
}

/// Whether `fd` was opened read-only (`O_RDONLY`).
/// # Errors
/// Returns an error if `fcntl(F_GETFL)` fails.
#[cfg_attr(trust_verify, trust::skip)]
pub fn is_read_only(fd: BorrowedFd<'_>) -> io::Result<bool> {
    // SAFETY: `F_GETFL` reads status flags on an open descriptor.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(last_os());
    }
    Ok(flags & libc::O_ACCMODE == libc::O_RDONLY)
}

/// `mmap(len, prot, MAP_SHARED)` of `fd` at offset 0.
#[cfg_attr(trust_verify, trust::skip)]
fn map(fd: BorrowedFd<'_>, len: usize, writable: bool) -> io::Result<Region> {
    let prot = if writable {
        libc::PROT_READ | libc::PROT_WRITE
    } else {
        libc::PROT_READ
    };
    // SAFETY: a fresh mapping at a kernel-chosen address of `len` bytes of an object at least that long (callers sized or checked it), so no existing memory is replaced.
    let addr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            prot,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    if addr == libc::MAP_FAILED {
        return Err(last_os());
    }
    let base = NonNull::new(addr.cast::<u8>())
        .ok_or_else(|| io::Error::other("mmap returned a null mapping"))?;
    Ok(Region {
        base,
        len,
        writable,
    })
}

impl Region {
    /// Create a writable region of `len` bytes and its read-only custody descriptor.
    /// # Errors
    /// Returns an error if `len` is invalid or shared-memory creation/mapping fails.
    /// # Safety
    /// From call entry until the returned `Region` drops, no descriptor, mapping or process may
    /// modify, resize or invalidate the backing object except through that `Region`'s methods.
    /// Payload reads through aliases may occur only while the `Region` stays inert; metadata
    /// queries are allowed. A separately mapped reader must meet [`Self::map_readonly`]'s
    /// contract, and the custody descriptor goes only to a recipient known to keep these
    /// obligations: `O_RDONLY` restricts this descriptor, not aliases reopened from it, and
    /// unlinking does not remove every route to the object.
    pub unsafe fn create(len: usize) -> io::Result<(Region, OwnedFd)> {
        check_len(len)?;
        let (name, writer) = Self::open_fresh()?;
        let custody = open_named(&name, libc::O_RDONLY);
        let unlinked = unlink_named(&name);
        let custody = custody?;
        unlinked?;
        let size = libc::off_t::try_from(len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "region too long"))?;
        Self::size_once(writer.as_fd(), size)?;
        let region = map(writer.as_fd(), len, true)?;
        drop(writer);
        set_cloexec(custody.as_fd())?;
        Ok((region, custody))
    }

    fn open_fresh() -> io::Result<(std::ffi::CString, OwnedFd)> {
        let mut last = io::Error::other("no region name was tried");
        for _ in 0..NAME_TRIES {
            let name = fresh_name()?;
            match open_named(&name, libc::O_CREAT | libc::O_EXCL | libc::O_RDWR) {
                Ok(fd) => return Ok((name, fd)),
                Err(e) if e.raw_os_error() == Some(libc::EEXIST) => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// `ftruncate(fd, size)`, the one sizing a shm object takes.
    #[cfg_attr(trust_verify, trust::skip)]
    fn size_once(fd: BorrowedFd<'_>, size: libc::off_t) -> io::Result<()> {
        // SAFETY: `ftruncate` on an open writable descriptor.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), size) } != 0 {
            return Err(last_os());
        }
        Ok(())
    }

    /// Map a custody descriptor read-only with expected byte length `expected_len`.
    /// # Errors
    /// Returns an error if `fd` is writable, size-mismatched, or mapping fails.
    /// # Safety
    /// From this call until the returned `Region` drops, the backing object is never modified,
    /// resized or invalidated through any other mapping or descriptor, in this process or another;
    /// an existing writer mapping may remain only if it stays inert throughout. A read-only
    /// descriptor proves permissions, not this quiescence.
    pub unsafe fn map_readonly(fd: BorrowedFd<'_>, expected_len: usize) -> io::Result<Region> {
        check_len(expected_len)?;
        if !is_read_only(fd)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "a custody descriptor must be open read-only",
            ));
        }
        let size = fd_size(fd)?;
        if u64::try_from(expected_len).ok() != Some(size) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the region is {size} bytes, not the {expected_len} it was offered as"),
            ));
        }
        map(fd, expected_len, false)
    }

    /// Region length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the region is empty (always `false` since regions are at least one page).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether this mapping is writable.
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        self.writable
    }

    const fn span(&self, at: usize, len: usize) -> Result<(), AccessError> {
        match at.checked_add(len) {
            Some(end) if end <= self.len => Ok(()),
            _ => Err(AccessError::OutOfBounds { at, len }),
        }
    }

    fn word(&self, at: usize) -> Result<&AtomicU64, AccessError> {
        if !at.is_multiple_of(WORD) {
            return Err(AccessError::Misaligned { at });
        }
        self.span(at, WORD)?;
        // SAFETY: `at + 8 <= len` (checked above) keeps the word inside the page-aligned mapping, and `at` is a multiple of 8, so it is aligned for `AtomicU64`; the borrow cannot outlive `self`, which owns the mapping. `Region` is not `Sync` and the constructor's contract excludes conflicting aliases. A read-only mapping only ever sees a `Relaxed` load through the word (every store is refused before `word`), which core defines on read-only memory; hence a cast, not `AtomicU64::from_ptr`, which asks for writable memory.
        Ok(unsafe { &*self.base.as_ptr().add(at).cast::<AtomicU64>() })
    }

    /// Load the little-endian `u64` word at 8-byte-aligned offset `at` with acquire ordering.
    /// # Errors
    /// Returns [`AccessError::OutOfBounds`] or [`AccessError::Misaligned`].
    pub fn load_u64_acquire(&self, at: usize) -> Result<u64, AccessError> {
        let value = self.word(at)?.load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        Ok(u64::from_le(value))
    }

    /// Store the little-endian `u64` word at 8-byte-aligned offset `at` with release ordering.
    /// # Errors
    /// Returns [`AccessError`] if read-only, out of bounds, or misaligned.
    pub fn store_u64_release(&self, at: usize, value: u64) -> Result<(), AccessError> {
        if !self.writable {
            return Err(AccessError::ReadOnly);
        }
        self.word(at)?.store(value.to_le(), Ordering::Release);
        Ok(())
    }

    /// Copy `bytes` into the region at `at`.
    /// # Errors
    /// Returns [`AccessError::ReadOnly`] or [`AccessError::OutOfBounds`].
    pub fn write(&self, at: usize, bytes: &[u8]) -> Result<(), AccessError> {
        if !self.writable {
            return Err(AccessError::ReadOnly);
        }
        self.span(at, bytes.len())?;
        // SAFETY: `[at, at + len)` is inside the writable mapping (checked above), and `bytes` is caller memory, never the mapping, so they do not overlap; `Region` is not `Sync`, so no other thread touches the mapping during the copy.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.as_ptr().add(at), bytes.len());
        }
        Ok(())
    }

    /// Copy `out.len()` bytes from the region at `at` into `out`.
    /// # Errors
    /// Returns [`AccessError::OutOfBounds`] if `at..at + out.len()` exceeds the region.
    pub fn read_into(&self, at: usize, out: &mut [u8]) -> Result<(), AccessError> {
        self.span(at, out.len())?;
        // SAFETY: `[at, at + len)` is inside the mapping (checked above), and `out` is caller memory, never the mapping, so they do not overlap; `Region` is not `Sync`, so no other thread touches the mapping during the copy.
        unsafe {
            std::ptr::copy_nonoverlapping(self.base.as_ptr().add(at), out.as_mut_ptr(), out.len());
        }
        Ok(())
    }
}

impl Drop for Region {
    #[cfg_attr(trust_verify, trust::skip)]
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping `map` made, once; no reference into it survives (none leaves this module).
        unsafe {
            libc::munmap(self.base.as_ptr().cast(), self.len);
        }
    }
}

/// Test seams for shared-memory custody checks.
#[cfg(any(test, debug_assertions))]
pub mod testing {
    use super::{Region, check_len, map, unlink_named};
    use std::io;
    use std::os::fd::{AsFd as _, OwnedFd};

    /// Create a region whose custody descriptor remains writable (negative-control test seam).
    /// # Errors
    /// Returns an error if shared-memory creation fails.
    /// # Safety
    /// Until the returned `Region` drops, nothing writes, resizes, invalidates or maps the object
    /// through the returned writable descriptor or its aliases, so a recipient must be known to
    /// make no such access; writes through the `Region` itself are allowed. The descriptor is for
    /// metadata and refusal tests only, never a production handoff.
    pub unsafe fn create_with_writable_custody(len: usize) -> io::Result<(Region, OwnedFd)> {
        check_len(len)?;
        let (name, writer) = Region::open_fresh()?;
        unlink_named(&name)?;
        let size = libc::off_t::try_from(len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "region too long"))?;
        Region::size_once(writer.as_fd(), size)?;
        let region = map(writer.as_fd(), len, true)?;
        Ok((region, writer))
    }
}

#[cfg(test)]
#[path = "shm_tests.rs"]
mod tests;
