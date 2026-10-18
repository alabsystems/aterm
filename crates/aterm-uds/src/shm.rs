// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Anonymous POSIX shared memory: the crash-restore journal's REGION, which outlives the window
//! that writes it (`aterm-restore`; the crash-restore plan §2.1, §5).
//!
//! A window W journals each session into one region it maps read-write. The PTY keeper K holds
//! a READ-ONLY descriptor of it beside the session's master and never maps it; after W dies, K
//! hands that descriptor to the next window W', which maps it read-only and rebuilds the screen.
//! A process that dies loses no store it already made to shared memory, and the object lives
//! while any descriptor or mapping of it does, so K's unmapped descriptor is what keeps W's
//! last words.
//!
//! [`Region::create`] makes one in the order the plan's privacy claim 1 fixes:
//!
//! 1. a random name (`/atrj.` and 25 base32 characters: 31 bytes, the longest macOS takes;
//!    `PSHMNAMLEN`, measured: a 32-byte name is `ENAMETOOLONG`);
//! 2. `shm_open(O_CREAT | O_EXCL | O_RDWR, 0600)`: a name nobody else made;
//! 3. `shm_open(O_RDONLY)`: the custody descriptor, opened while the name still exists, so the
//!    kernel itself refuses a writable mapping of it (a received `O_RDWR` descriptor maps
//!    writable, measured: the plan's §3.2 N8);
//! 4. `shm_unlink`: from here the object has no POSIX shared-memory name. This does not seal
//!    it against reopening through an existing descriptor (for example Linux procfs); custody
//!    mode alone does not exclude writable aliases. A crash between 2 and 4
//!    leaks an EMPTY name (it is unlinked before it is sized) until reboot: macOS lists no shm
//!    names, so there is nothing to sweep, and an empty object costs nothing;
//! 5. `ftruncate` once (macOS refuses a second one, `EINVAL`, measured), to a multiple of the
//!    16 KiB page (macOS rounds a shm size up to it, measured: 5000 bytes read back as 16384);
//! 6. `mmap(PROT_READ | PROT_WRITE, MAP_SHARED)`;
//! 7. the read-write descriptor closes (the mapping keeps the object), and the custody
//!    descriptor is marked close-on-exec.
//!
//! [`Region::map_readonly`] is W''s side: the descriptor must be `O_RDONLY` and exactly the size
//! the keeper's offer said, at most [`MAX_LEN`], and the mapping is `PROT_READ`.
//!
//! The mapping's unsafe implementation lives in this module; callers admit constructors under
//! their documented safety contracts. No reference into the mapping leaves it: callers copy
//! bytes in and out ([`Region::write`], [`Region::read_into`]) and touch the
//! header words through atomics ([`Region::store_u64_release`], [`Region::load_u64_acquire`]),
//! each bounds- and alignment-checked.
//!
//! A [`Region`] is `Send` and NOT `Sync`. Its copies are plain `memcpy`s, and a plain copy racing
//! another thread's access to the same bytes is undefined behaviour under Rust's memory model,
//! reference or no reference; were `&Region` shareable, safe code could race two copies (or a
//! copy and a header store) from two threads. So one thread at a time uses a mapping, and a
//! writer the parse thread, the gather thread and the journal worker share (the crash-restore
//! plan §2.2) goes behind one lock, as aterm-restore's `VecStore` does: that store measured
//! word-at-a-time atomic copies at a quarter of the parse thread's throughput in the
//! `crash-restore` perf lane (ratio 0.75 against the plan's 0.97 fence) and one mutex around a
//! `memcpy` at none (0.99-1.004), so atomic copies are not the way to share one. Another
//! mapping can still alias this mapping: `!Sync` protects only one value. Both constructors
//! are therefore unsafe. A creator must prevent aliases from changing or invalidating the
//! backing object while its Region lives; a read-only mapper must keep it unchanged and valid
//! for that mapping's whole lifetime. Read-only custody and unlinking are not a write seal.
//! In the restore protocol the keeper never maps a region and the
//! restorer admits it only after every writer has died; an `O_RDONLY` descriptor alone does not
//! establish that condition. Tests may retain an inert writer mapping but never write through
//! it while the read-only mapping lives.
//!
//! The header words are `AtomicU64`s made by a pointer cast, never `AtomicU64::from_ptr` (whose
//! contract asks for memory valid for writes), and an acquire load is a relaxed load and an
//! acquire fence: on the restorer's read-only mapping (`PROT_READ`) a relaxed load of at most 8
//! bytes is the one atomic access core defines ("Atomic accesses to read-only memory"), and
//! every store is refused before a word is reached.
//!
//! The declarations are aterm-libc's (`shm_open` and `fcntl` are variadic, and a hand
//! declaration of a variadic as fixed-arity passes its extra argument in the wrong place on
//! Apple arm64, where variadic arguments go on the stack).

use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering, fence};

/// A header word's size and alignment.
const WORD: usize = 8;
// The words are `AtomicU64`s, defined to load from a read-only mapping only where 8 bytes is a
// "sufficiently small" atomic load: every 64-bit target, and every unix target aterm builds.
const _: () = assert!(
    usize::BITS == 64 && std::mem::align_of::<AtomicU64>() == WORD,
    "the region's words need a 64-bit target"
);

/// The page every region size is a multiple of (macOS rounds a shm size up to it; 16 KiB on
/// Apple silicon, a multiple of x86_64's 4 KiB).
pub const PAGE: usize = 16 * 1024;
/// The largest region this module maps, and the largest a keeper accepts in custody: eight v1
/// journal regions. A descriptor that says more is refused before it is mapped.
pub const MAX_LEN: usize = 64 * 1024 * 1024;
/// The name's fixed head; [`NAME_RANDOM`] base32 characters follow it.
const NAME_HEAD: &str = "/atrj.";
/// Random base32 characters in a name: 125 bits, and with the head 31 bytes, macOS's
/// `PSHMNAMLEN`. `O_EXCL` turns a collision into a retry, never a shared object.
const NAME_RANDOM: usize = 25;
/// How many fresh names [`Region::create`] tries when one already exists.
const NAME_TRIES: usize = 8;

/// Why an access to a mapped region was refused. Every access is checked; none panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessError {
    /// `[at, at + len)` is not inside the region.
    OutOfBounds {
        /// Where the access started.
        at: usize,
        /// How many bytes it covered.
        len: usize,
    },
    /// A word access at an offset that is not a multiple of 8.
    Misaligned {
        /// The offset.
        at: usize,
    },
    /// A store into a read-only mapping.
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

/// One mapped region: read-write in the window that created it, read-only in a restorer.
/// Dropping it unmaps it; the object lives on while a descriptor (the keeper's) holds it.
#[derive(Debug)]
pub struct Region {
    base: NonNull<u8>,
    len: usize,
    writable: bool,
}

// SAFETY: the mapping is process-wide memory, not tied to a thread, so moving a `Region` to
// another thread moves nothing a thread owns; `Drop` unmaps it from whichever thread holds it.
// There is deliberately no `Sync` (module doc): `write` and `read_into` are plain copies, which
// two threads sharing one `&Region` could race.
unsafe impl Send for Region {}

/// `Region` must never be `Sync` (module doc). If it were, both impls below would apply and the
/// item named in the closure would be ambiguous: the build fails here.
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
// Skip: bottoms out at the `shm_open` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
fn open_named(name: &std::ffi::CStr, oflag: libc::c_int) -> io::Result<OwnedFd> {
    // The mode rides as an unsigned int: `shm_open` is variadic on Darwin (an argument narrower
    // than `int` cannot be passed to one) and takes a `mode_t` (`unsigned int`) on Linux.
    let mode: libc::c_uint = 0o600;
    // SAFETY: `name` is a NUL-terminated string that outlives the call; the mode is read only
    // under `O_CREAT`.
    let fd = unsafe { libc::shm_open(name.as_ptr(), oflag, mode) };
    if fd < 0 {
        return Err(last_os());
    }
    // SAFETY: a descriptor `shm_open` just returned, owned by nothing else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// `shm_unlink(name)`.
// Skip: bottoms out at the `shm_unlink` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
fn unlink_named(name: &std::ffi::CStr) -> io::Result<()> {
    // SAFETY: `name` is a NUL-terminated string that outlives the call.
    if unsafe { libc::shm_unlink(name.as_ptr()) } != 0 {
        return Err(last_os());
    }
    Ok(())
}

/// Mark `fd` close-on-exec: the custody descriptor goes to the keeper, never to a child.
// Skip: bottoms out at the `fcntl` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
fn set_cloexec(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: `F_SETFD` on a descriptor the caller holds open; the flag is an int.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
        return Err(last_os());
    }
    Ok(())
}

/// The object's size, as `fstat` reports it.
///
/// # Errors
///
/// `fstat` failing (a closed descriptor), or a negative size.
// Skip: bottoms out at the `fstat` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn fd_size(fd: BorrowedFd<'_>) -> io::Result<u64> {
    // SAFETY: `stat` is plain old data; all-zero is a valid value for `fstat` to overwrite.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: a live out-parameter of the right type, on a descriptor the caller holds open.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(last_os());
    }
    u64::try_from(st.st_size).map_err(|_| io::Error::other("fstat reported a negative size"))
}

/// Whether `fd` was opened read-only (`F_GETFL`'s access mode is `O_RDONLY`): what the keeper
/// requires of a custody descriptor before it holds one, so no window it offers the region to can
/// map the dead window's words writable.
///
/// # Errors
///
/// `fcntl` failing (a closed descriptor).
// Skip: bottoms out at the `fcntl` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
pub fn is_read_only(fd: BorrowedFd<'_>) -> io::Result<bool> {
    // SAFETY: `F_GETFL` takes no argument and only reads the descriptor's status flags.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(last_os());
    }
    Ok(flags & libc::O_ACCMODE == libc::O_RDONLY)
}

/// `mmap(len, prot, MAP_SHARED)` of `fd` at offset 0.
// Skip: bottoms out at the `mmap` FFI call.
#[cfg_attr(trust_verify, trust::skip)]
fn map(fd: BorrowedFd<'_>, len: usize, writable: bool) -> io::Result<Region> {
    let prot = if writable {
        libc::PROT_READ | libc::PROT_WRITE
    } else {
        libc::PROT_READ
    };
    // SAFETY: a fresh mapping at an address the kernel picks, of `len` bytes of an object at
    // least that long (the callers sized or checked it), so no existing memory is replaced.
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
    /// Create a region of `len` bytes (a multiple of [`PAGE`], at most [`MAX_LEN`]) mapped
    /// read-write, and its read-only custody descriptor (close-on-exec), in the module doc's
    /// seven steps. The region is all zeros.
    ///
    /// # Errors
    ///
    /// A bad `len` (`InvalidInput`), no entropy for a name, or the first OS call that failed;
    /// a name that already exists is retried under a fresh one ([`NAME_TRIES`] times).
    ///
    /// # Safety
    ///
    /// From call entry until the returned `Region` is dropped, the caller must prevent any
    /// descriptor, mapping or process from modifying, resizing or invalidating its backing
    /// object except through that `Region`'s methods. Payload reads through aliases may occur
    /// only while this `Region` stays inert; descriptor metadata queries remain permitted.
    /// Any separately mapped reader must meet [`Self::map_readonly`]'s contract, which requires
    /// this writer to stay inert while the reader lives. The returned custody descriptor may
    /// be passed only to a recipient known to preserve these obligations.
    ///
    /// `O_RDONLY` restricts this descriptor, not aliases reopened from it; unlinking removes
    /// the POSIX name, not every route to the live object. Neither establishes write or resize
    /// exclusion. The caller need not prove `len` valid: this method checks it and returns an
    /// error for an invalid length.
    pub unsafe fn create(len: usize) -> io::Result<(Region, OwnedFd)> {
        check_len(len)?;
        let (name, writer) = Self::open_fresh()?;
        // The custody descriptor, opened while the name exists, then the name goes. The
        // unlink runs whether or not the read-only open succeeded: no named object outlives
        // this call.
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
    // Skip: bottoms out at the `ftruncate` FFI call.
    #[cfg_attr(trust_verify, trust::skip)]
    fn size_once(fd: BorrowedFd<'_>, size: libc::off_t) -> io::Result<()> {
        // SAFETY: `ftruncate` on a descriptor the caller holds open for writing.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), size) } != 0 {
            return Err(last_os());
        }
        Ok(())
    }

    /// Map a custody descriptor read-only: what a restorer does with the region the keeper
    /// offered it. The descriptor must be open `O_RDONLY` and the object exactly `expected_len`
    /// bytes, at most [`MAX_LEN`]; the mapping is `PROT_READ`, so even a writable descriptor
    /// that slipped through would yield no store. The descriptor stays the caller's (the
    /// mapping keeps the object alive after it closes).
    ///
    /// # Errors
    ///
    /// `InvalidInput` for an `expected_len` that is not a region length, `PermissionDenied` for
    /// a descriptor not opened read-only, `InvalidData` for a size other than `expected_len`, or
    /// the OS call that failed.
    ///
    /// # Safety
    ///
    /// From this call until the returned `Region` is dropped, the caller must ensure the
    /// backing object is never modified, resized or invalidated through any other mapping or
    /// descriptor, in this process or another. Existing writer mappings may remain only if
    /// they stay inert throughout that interval. Read-only descriptor mode proves access
    /// permissions, not this lifetime quiescence.
    ///
    /// The caller need not prove `expected_len`, descriptor mode or size valid: this method
    /// checks those inputs and returns an error when they are invalid. The unsafe obligation
    /// is stable, unmodified backing during the call and the successful mapping's lifetime.
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

    /// The region's length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Never true: a region has at least one page.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether this mapping may be written (the creator's), or only read (a restorer's).
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
        // SAFETY: `at + 8 <= len` (checked above), so the pointer stays inside the mapping; the
        // mapping is page-aligned and `at` is a multiple of 8, so the word is aligned for
        // `AtomicU64` (8: the assertion beside `WORD`); the mapping lives as long as `self`,
        // which the returned borrow cannot outlive. A shared reference to `AtomicU64` (an
        // `UnsafeCell`) promises no immutability. `Region` is not `Sync`, and the constructor's
        // unsafe contract excludes conflicting aliases, so accesses through this mapping are
        // this thread's own, ordered by program order. On a read-only mapping the only
        // operation ever made through the word is a
        // `Relaxed` load (every store is refused before `word` is called), which core defines on
        // read-only memory for a load of at most 8 bytes on a 64-bit target; the reference is
        // made by a cast because `AtomicU64::from_ptr` asks for memory valid for writes.
        Ok(unsafe { &*self.base.as_ptr().add(at).cast::<AtomicU64>() })
    }

    /// Load the little-endian `u64` word at `at` (8-aligned) with acquire semantics: every
    /// store its writer made before the release store that wrote it is visible after. Made as a
    /// relaxed load and an acquire fence, the form core defines on a read-only mapping (the
    /// restorer's), and the same ordering on the writer's.
    ///
    /// # Errors
    ///
    /// [`AccessError::Misaligned`] or [`AccessError::OutOfBounds`].
    pub fn load_u64_acquire(&self, at: usize) -> Result<u64, AccessError> {
        let value = self.word(at)?.load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        Ok(u64::from_le(value))
    }

    /// Store the little-endian `u64` word at `at` (8-aligned) with release semantics: a commit
    /// word stored after the bytes it admits.
    ///
    /// # Errors
    ///
    /// [`AccessError::ReadOnly`] on a restorer's mapping, [`AccessError::Misaligned`] or
    /// [`AccessError::OutOfBounds`].
    pub fn store_u64_release(&self, at: usize, value: u64) -> Result<(), AccessError> {
        if !self.writable {
            return Err(AccessError::ReadOnly);
        }
        self.word(at)?.store(value.to_le(), Ordering::Release);
        Ok(())
    }

    /// Copy `bytes` to `at`, with no ordering of its own (a later release store publishes it).
    ///
    /// # Errors
    ///
    /// [`AccessError::ReadOnly`] on a restorer's mapping, or [`AccessError::OutOfBounds`].
    pub fn write(&self, at: usize, bytes: &[u8]) -> Result<(), AccessError> {
        if !self.writable {
            return Err(AccessError::ReadOnly);
        }
        self.span(at, bytes.len())?;
        // SAFETY: `[at, at + len)` lies inside the writable mapping (checked above), and
        // `bytes` is caller memory, never the mapping (no reference into it leaves this
        // module), so the two do not overlap. `Region` is not `Sync`, so no other thread of
        // this process accesses the mapping during the copy.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base.as_ptr().add(at), bytes.len());
        }
        Ok(())
    }

    /// Copy `out.len()` bytes at `at` into `out`.
    ///
    /// # Errors
    ///
    /// [`AccessError::OutOfBounds`].
    pub fn read_into(&self, at: usize, out: &mut [u8]) -> Result<(), AccessError> {
        self.span(at, out.len())?;
        // SAFETY: `[at, at + len)` lies inside the mapping (checked above), and `out` is
        // caller memory, never the mapping, so the two do not overlap. `Region` is not `Sync`,
        // so no other thread of this process accesses the mapping during the copy.
        unsafe {
            std::ptr::copy_nonoverlapping(self.base.as_ptr().add(at), out.as_mut_ptr(), out.len());
        }
        Ok(())
    }
}

impl Drop for Region {
    // Skip: bottoms out at the `munmap` FFI call.
    #[cfg_attr(trust_verify, trust::skip)]
    fn drop(&mut self) {
        // SAFETY: exactly the mapping `map` made, unmapped once; no reference into it
        // survives (none ever leaves this module).
        unsafe {
            libc::munmap(self.base.as_ptr().cast(), self.len);
        }
    }
}

/// Test seams other crates' tests call (AGENTS.md: `cfg(any(test, debug_assertions))`).
#[cfg(any(test, debug_assertions))]
pub mod testing {
    use super::{Region, check_len, map, unlink_named};
    use std::io;
    use std::os::fd::{AsFd as _, OwnedFd};

    /// A region whose custody descriptor is open READ-WRITE: the defect the plan's §3.2 N8
    /// removes (any receiver of such a descriptor maps the dead window's words writable). The
    /// keeper must refuse it as a companion, and [`Region::map_readonly`] must refuse to map
    /// it: the negative controls.
    ///
    /// # Errors
    ///
    /// As [`Region::create`].
    ///
    /// # Safety
    ///
    /// Until the returned `Region` is dropped, the caller must prevent all writes, resizing,
    /// invalidation and overlapping mapping accesses through the returned writable descriptor
    /// or any of its aliases. The descriptor is for metadata/refusal tests only; a recipient
    /// must be known to make no such access. Writes through the returned `Region` itself remain
    /// permitted. A writable custody descriptor is never a safe production handoff.
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
        // SAFETY: this fixture keeps custody local and never reopens it for writing or
        // resizing. All writes use `region` and finish before either read-only mapping.
        let (region, custody) = unsafe { Region::create(LEN) }.expect("create");
        assert!(region.is_writable());
        assert_eq!(region.len(), LEN);
        region.write(100, b"written by the window").expect("write");
        region
            .store_u64_release(8, 0x0102_0304_0506_0708)
            .expect("store");
        assert_eq!(is_read_only(custody.as_fd()).ok(), Some(true));
        assert_eq!(fd_size(custody.as_fd()).ok(), Some(LEN as u64));
        // SAFETY: `F_GETFD` only reads the descriptor's flags.
        let fd_flags = unsafe { libc::fcntl(custody.as_raw_fd(), libc::F_GETFD) };
        assert_eq!(
            fd_flags & libc::FD_CLOEXEC,
            libc::FD_CLOEXEC,
            "close-on-exec"
        );

        // SAFETY: all writes above are complete; the retained writer stays inert, and the
        // read-only custody keeps this private object stable until both reader mappings drop.
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

        // The creator's mapping gone, the custody descriptor still holds the words.
        drop(region);
        assert_eq!(reader.load_u64_acquire(8), Ok(0x0102_0304_0506_0708));
        drop(reader);
        // SAFETY: the writer and prior reader were dropped; only read-only custody remains.
        let again = unsafe { Region::map_readonly(custody.as_fd(), LEN) }.expect("map again");
        assert_eq!(again.load_u64_acquire(8), Ok(0x0102_0304_0506_0708));
    }

    /// `map_readonly` refuses a writable descriptor, a size other than the offer's, and a length
    /// that is no region's; a region is refused at a length that is not a whole number of
    /// pages.
    #[test]
    fn map_readonly_refuses_a_writable_descriptor_and_a_wrong_size() {
        // SAFETY: this private writable descriptor is only inspected/refused below; neither
        // it nor an alias writes, resizes or maps the object while `_region` lives.
        let (_region, writable) =
            unsafe { testing::create_with_writable_custody(LEN) }.expect("create");
        assert_eq!(is_read_only(writable.as_fd()).ok(), Some(false));
        // SAFETY: the object stays unchanged during this call; no mapping is returned.
        let e = unsafe { Region::map_readonly(writable.as_fd(), LEN) }.expect_err("writable");
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{e}");

        // SAFETY: this private custody is only inspected by the refusal calls below; it
        // and its aliases never modify or resize the object while `_region` lives.
        let (_region, custody) = unsafe { Region::create(LEN) }.expect("create");
        // SAFETY: the private object stays unchanged during these refusal calls. No mapping
        // is returned, and neither retained writer nor descriptor is used to modify it.
        let e = unsafe { Region::map_readonly(custody.as_fd(), 2 * LEN) }.expect_err("wrong size");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
        // SAFETY: the same unchanged private object; this invalid length returns no mapping.
        let e = unsafe { Region::map_readonly(custody.as_fd(), LEN + 1) }
            .expect_err("not a region length");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");
        // SAFETY: the same unchanged private object; this invalid length returns no mapping.
        let e =
            unsafe { Region::map_readonly(custody.as_fd(), MAX_LEN + PAGE) }.expect_err("too long");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{e}");

        for len in [0, PAGE - 1, PAGE + 1, MAX_LEN + PAGE] {
            // SAFETY: any returned owners would stay local without alias writes/resizing;
            // these invalid lengths are checked by create and produce no mapping.
            let e = unsafe { Region::create(len) }.expect_err("not a region length");
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{len}: {e}");
        }
    }

    /// Every access outside the region, or at a misaligned word, is refused, never a panic.
    #[test]
    fn accesses_are_bounds_and_alignment_checked() {
        // SAFETY: custody stays local and unused; only `region` accesses this private
        // object, and no descriptor or alias changes its size.
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
        // SAFETY: custody is never reopened or used to change the object; all writes use
        // `region` and finish before the read-only mapping, after which both only read.
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
        // SAFETY: every write above is finished; the writer below is only read, the custody
        // stays open, and no other descriptor or mapping can modify this private object.
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
