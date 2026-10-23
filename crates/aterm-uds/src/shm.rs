// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Anonymous POSIX shared-memory regions for the crash-restore journal.

use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering, fence};

/// A header word's size and alignment.
const WORD: usize = 8;
const _: () = assert!(
    usize::BITS == 64 && std::mem::align_of::<AtomicU64>() == WORD,
    "the region's words need a 64-bit target"
);

/// Page size multiple required for every region (`16 KiB`).
pub const PAGE: usize = 16 * 1024;
/// Maximum region size accepted for mapping or custody (`64 MiB`).
pub const MAX_LEN: usize = 64 * 1024 * 1024;
/// The name's fixed head; [`NAME_RANDOM`] base32 characters follow it.
const NAME_HEAD: &str = "/atrj.";
/// Random base32 characters in a name: 125 bits, and with the head 31 bytes, macOS's
/// `PSHMNAMLEN`.
const NAME_RANDOM: usize = 25;
/// How many fresh names [`Region::create`] tries when one already exists.
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

// SAFETY: the mapping is process-wide memory not bound to a specific thread.
unsafe impl Send for Region {}

/// `Region` must never be `Sync` (module doc).
trait AmbiguousIfSync<A> {
    fn named() {}
}
impl<T: ?Sized> AmbiguousIfSync<()> for T {}
impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}
const _: fn() = || {
    let _ = <Region as AmbiguousIfSync<_>>::named;
};

/// The last OS error, as an `io::Error`.
fn last_os() -> io::Error {
    io::Error::last_os_error()
}

/// `len` as a region length: non-zero, a multiple of [`PAGE`], at most [`MAX_LEN`].
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

/// A fresh random name: [`NAME_HEAD`] and [`NAME_RANDOM`] lowercase base32 characters.
fn fresh_name() -> io::Result<std::ffi::CString> {
    const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut raw = [0u8; NAME_RANDOM];
    crate::rand::fill(&mut raw)?;
    let mut name = String::with_capacity(NAME_HEAD.len() + NAME_RANDOM);
    name.push_str(NAME_HEAD);
    for b in raw {
        // The low five bits: each character is 5 uniform bits of its byte.
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
///
/// # Errors
///
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
///
/// # Errors
///
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
    // SAFETY: maps `len` bytes of `fd` at a kernel-chosen address.
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
    ///
    /// # Errors
    ///
    /// Returns an error if `len` is invalid or shared-memory creation/mapping fails.
    ///
    /// # Safety
    ///
    /// The caller must ensure no other handle truncates or mutates the underlying object.
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

    /// `shm_open(O_CREAT | O_EXCL | O_RDWR)` under a fresh name, retried while the name exists.
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
    ///
    /// # Errors
    ///
    /// Returns an error if `fd` is writable, size-mismatched, or mapping fails.
    ///
    /// # Safety
    ///
    /// The underlying object must not be concurrently mutated or truncated while mapped.
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
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the region is empty (always `false` since regions are at least one page).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether this mapping is writable.
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// `[at, at + len)` if it lies inside the region.
    fn span(&self, at: usize, len: usize) -> Result<(), AccessError> {
        match at.checked_add(len) {
            Some(end) if end <= self.len => Ok(()),
            _ => Err(AccessError::OutOfBounds { at, len }),
        }
    }

    /// The word at `at`, checked: 8-aligned and inside the region.
    fn word(&self, at: usize) -> Result<&AtomicU64, AccessError> {
        if !at.is_multiple_of(WORD) {
            return Err(AccessError::Misaligned { at });
        }
        self.span(at, WORD)?;
        // SAFETY: `at + 8 <= len` and `at` is 8-byte aligned within the valid mapping.
        Ok(unsafe { &*self.base.as_ptr().add(at).cast::<AtomicU64>() })
    }

    /// Load the little-endian `u64` word at 8-byte-aligned offset `at` with acquire ordering.
    ///
    /// # Errors
    ///
    /// Returns [`AccessError::OutOfBounds`] or [`AccessError::Misaligned`].
    pub fn load_u64_acquire(&self, at: usize) -> Result<u64, AccessError> {
        let value = self.word(at)?.load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        Ok(u64::from_le(value))
    }

    /// Store the little-endian `u64` word at 8-byte-aligned offset `at` with release ordering.
    ///
    /// # Errors
    ///
    /// Returns [`AccessError`] if read-only, out of bounds, or misaligned.
    pub fn store_u64_release(&self, at: usize, value: u64) -> Result<(), AccessError> {
        if !self.writable {
            return Err(AccessError::ReadOnly);
        }
        self.word(at)?.store(value.to_le(), Ordering::Release);
        Ok(())
    }

    /// Copy `bytes` into the region at `at`.
    ///
    /// # Errors
    ///
    /// Returns [`AccessError::ReadOnly`] or [`AccessError::OutOfBounds`].
    pub fn write(&self, at: usize, bytes: &[u8]) -> Result<(), AccessError> {
        if !self.writable {
            return Err(AccessError::ReadOnly);
        }
        self.span(at, bytes.len())?;
        // SAFETY: `[at, at + len)` is in bounds of the writable mapping and non-overlapping.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.as_ptr().add(at), bytes.len());
        }
        Ok(())
    }

    /// Copy `out.len()` bytes from the region at `at` into `out`.
    ///
    /// # Errors
    ///
    /// Returns [`AccessError::OutOfBounds`] if `[at, at + out.len())` exceeds the region.
    pub fn read_into(&self, at: usize, out: &mut [u8]) -> Result<(), AccessError> {
        self.span(at, out.len())?;
        // SAFETY: `[at, at + len)` is in bounds of the mapping and `out` is non-overlapping.
        unsafe {
            std::ptr::copy_nonoverlapping(self.base.as_ptr().add(at), out.as_mut_ptr(), out.len());
        }
        Ok(())
    }
}

impl Drop for Region {
    #[cfg_attr(trust_verify, trust::skip)]
    fn drop(&mut self) {
        // SAFETY: unmaps the exact region created by `map` once.
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
    ///
    /// # Errors
    ///
    /// Returns an error if shared-memory creation fails.
    ///
    /// # Safety
    ///
    /// Same requirements as [`Region::create`].
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
mod tests {
    use super::*;

    const LEN: usize = 2 * PAGE;

    /// The creator writes; a read-only mapping of the custody descriptor reads every byte and
    /// word back, and refuses every store; the custody descriptor is read-only, close-on-exec
    /// and exactly the region's size.
    #[test]
    fn a_region_maps_back_read_only_from_its_custody_descriptor() {
        // SAFETY: fixture keeps custody local without concurrent writes or truncation.
        let (region, custody) = unsafe { Region::create(LEN) }.expect("create");
        assert!(region.is_writable());
        assert_eq!(region.len(), LEN);
        region.write(100, b"written by the window").expect("write");
        region
            .store_u64_release(8, 0x0102_0304_0506_0708)
            .expect("store");
        assert_eq!(is_read_only(custody.as_fd()).ok(), Some(true));
        assert_eq!(fd_size(custody.as_fd()).ok(), Some(LEN as u64));
        // SAFETY: `F_GETFD` reads descriptor flags.
        let fd_flags = unsafe { libc::fcntl(custody.as_raw_fd(), libc::F_GETFD) };
        assert_eq!(
            fd_flags & libc::FD_CLOEXEC,
            libc::FD_CLOEXEC,
            "close-on-exec"
        );

        // SAFETY: all writes above are complete and the custody object is not mutated.
        let reader = unsafe { Region::map_readonly(custody.as_fd(), LEN) }.expect("map read-only");
        assert!(!reader.is_writable());
        let mut back = [0u8; 21];
        reader.read_into(100, &mut back).expect("read");
        assert_eq!(&back, b"written by the window");
        assert_eq!(reader.load_u64_acquire(8), Ok(0x0102_0304_0506_0708));
        let mut le = [0u8; 8];
        reader.read_into(8, &mut le).expect("read");
        assert_eq!(le, 0x0102_0304_0506_0708_u64.to_le_bytes(), "little-endian");
        assert_eq!(reader.write(0, b"x"), Err(AccessError::ReadOnly));
        assert_eq!(reader.store_u64_release(0, 1), Err(AccessError::ReadOnly));

        drop(region);
        assert_eq!(reader.load_u64_acquire(8), Ok(0x0102_0304_0506_0708));
        drop(reader);
        // SAFETY: writer and prior reader were dropped; only read-only custody remains.
        let again = unsafe { Region::map_readonly(custody.as_fd(), LEN) }.expect("map again");
        assert_eq!(again.load_u64_acquire(8), Ok(0x0102_0304_0506_0708));
    }

    /// `map_readonly` refuses a writable descriptor, a size other than the offer's, and a length
    /// that is no region's; a region is refused at a length that is not a whole number of
    /// pages.
    #[test]
    fn map_readonly_refuses_a_writable_descriptor_and_a_wrong_size() {
        // SAFETY: private writable descriptor is only inspected and refused below.
        let (_region, writable) =
            unsafe { testing::create_with_writable_custody(LEN) }.expect("create");
        assert_eq!(is_read_only(writable.as_fd()).ok(), Some(false));
        // SAFETY: the object is unchanged during this refused call.
        let e = unsafe { Region::map_readonly(writable.as_fd(), LEN) }.expect_err("writable");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");

        // SAFETY: private custody is only used for refusal checks below.
        let (_region, custody) = unsafe { Region::create(LEN) }.expect("create");
        // SAFETY: private object is unchanged during this refused call.
        let e = unsafe { Region::map_readonly(custody.as_fd(), 2 * LEN) }.expect_err("wrong size");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
        // SAFETY: private object is unchanged during this refused call.
        let e = unsafe { Region::map_readonly(custody.as_fd(), LEN + 1) }
            .expect_err("not a region length");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
        // SAFETY: private object is unchanged during this refused call.
        let e =
            unsafe { Region::map_readonly(custody.as_fd(), MAX_LEN + PAGE) }.expect_err("too long");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");

        for len in [0, PAGE - 1, PAGE + 1, MAX_LEN + PAGE] {
            // SAFETY: invalid lengths return an error without mapping.
            let e = unsafe { Region::create(len) }.expect_err("not a region length");
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{len}: {e}");
        }
    }

    /// Every access outside the region, or at a misaligned word, is refused, never a panic.
    #[test]
    fn accesses_are_bounds_and_alignment_checked() {
        // SAFETY: custody stays local and unused while `region` accesses the private object.
        let (region, _custody) = unsafe { Region::create(PAGE) }.expect("create");
        assert_eq!(
            region.load_u64_acquire(12),
            Err(AccessError::Misaligned { at: 12 })
        );
        assert_eq!(
            region.load_u64_acquire(PAGE),
            Err(AccessError::OutOfBounds { at: PAGE, len: 8 })
        );
        assert_eq!(
            region.write(PAGE - 4, &[0; 5]),
            Err(AccessError::OutOfBounds {
                at: PAGE - 4,
                len: 5
            })
        );
        assert_eq!(
            region.read_into(usize::MAX, &mut [0; 2]),
            Err(AccessError::OutOfBounds {
                at: usize::MAX,
                len: 2
            })
        );
        region.write(PAGE - 5, &[7; 5]).expect("the last bytes");
        let mut last = [0u8; 5];
        region.read_into(PAGE - 5, &mut last).expect("read");
        assert_eq!(last, [7; 5]);
    }

    /// Copies at every offset and length across word edges leave exactly the bytes a `Vec` would
    /// hold, and the restorer's read-only mapping reads every span back the same, as do its
    /// words (relaxed loads and a fence, on `PROT_READ` pages).
    #[test]
    fn copies_across_word_edges_are_plain_memory() {
        // SAFETY: custody is never reopened or mutated; all writes go through `region`.
        let (region, custody) = unsafe { Region::create(PAGE) }.expect("create");
        let mut model = vec![0u8; PAGE];
        let mut seed = 0x9e37_79b9_u32;
        for at in 0..3 * WORD {
            for len in 0..3 * WORD {
                let bytes: Vec<u8> = (0..len)
                    .map(|_| {
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        seed.to_be_bytes()[0]
                    })
                    .collect();
                region.write(at, &bytes).expect("write");
                model[at..at + len].copy_from_slice(&bytes);
            }
        }
        // SAFETY: all writes above are complete before reading via `reader`.
        let reader = unsafe { Region::map_readonly(custody.as_fd(), PAGE) }.expect("map read-only");
        for at in 0..4 * WORD {
            for len in 0..4 * WORD {
                let mut back = vec![0u8; len];
                reader.read_into(at, &mut back).expect("read");
                assert_eq!(back, model[at..at + len], "at={at} len={len}");
                region.read_into(at, &mut back).expect("read");
                assert_eq!(
                    back,
                    model[at..at + len],
                    "the writer's own, at={at} len={len}"
                );
            }
        }
        for at in (0..4 * WORD).step_by(WORD) {
            let word = <[u8; WORD]>::try_from(&model[at..at + WORD]).expect("a word");
            assert_eq!(reader.load_u64_acquire(at), Ok(u64::from_le_bytes(word)));
        }
    }

    /// A fresh name is 31 bytes (macOS's `PSHMNAMLEN`), of the head and base32 characters.
    #[test]
    fn a_region_name_is_31_bytes_of_base32() {
        for _ in 0..16 {
            let name = fresh_name().expect("name");
            let bytes = name.as_bytes();
            assert_eq!(bytes.len(), 31, "{name:?}");
            assert!(bytes.starts_with(NAME_HEAD.as_bytes()));
            assert!(
                bytes[NAME_HEAD.len()..]
                    .iter()
                    .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(b)),
                "{name:?}"
            );
        }
    }
}
