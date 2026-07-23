// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Thin RAII wrapper around the OS file-mapping primitives.
//!
//! Replaces the `memmap2` crate with a minimal inline implementation. On
//! macOS / Linux this is `libc::mmap` / `libc::munmap` (the same libc
//! dependency already used by `aterm-shm`); on Windows it is
//! `CreateFileMappingW` / `MapViewOfFile` / `UnmapViewOfFile` with the same
//! whole-file, read-write, shared semantics. The platform seam is the three
//! private [`sys`] calls; everything above them — the bounds-checked safe
//! API and its invariants — is shared.

use std::fs::File;
use std::io;
use std::ops::{Deref, DerefMut};

/// Mutable memory-mapped region backed by a file descriptor.
///
/// Unmaps the region on [`Drop`]. The mapping covers the entire file at the
/// time [`map_mut`](Self::map_mut) is called.
#[derive(Debug)]
// Trust: `ptr` is valid for `len` bytes — the relational backing-length invariant.
// Under `trustc -Z trust-verify` this lets the compiler PROVE the `from_raw_parts`
// bounds in `as_slice`/`slice` (spatial HIGH-2) instead of only catching them.
#[cfg_attr(trust_verify, trust::backing)]
pub struct MmapMut {
    ptr: *mut u8,
    len: usize,
    /// `true` for a read-write mapping ([`map_mut`](Self::map_mut)), `false`
    /// for a read-only one ([`map_read`](Self::map_read)). A read-only mapping
    /// can never hold dirty pages, so [`flush`](Self::flush) is a no-op for it
    /// and writing through [`as_mut_slice`](Self::as_mut_slice) is forbidden.
    writable: bool,
    /// Platform bookkeeping the unmap/flush calls need beyond `ptr`/`len`
    /// (unit on Unix; the duplicated file handle on Windows).
    sys: sys::Backing,
}

// SAFETY: The mapped region is exclusively owned by this struct. No other
// references alias the pointer, and the region is valid for `len` bytes
// from `ptr` until `Drop` calls `munmap`.
unsafe impl Send for MmapMut {}
// SAFETY: All public access goes through `&self` / `&mut self`, which the
// borrow checker serializes.
unsafe impl Sync for MmapMut {}

impl MmapMut {
    /// Create a read-write memory mapping of the entire file.
    ///
    /// # Safety
    ///
    /// The caller must ensure the file is not concurrently modified by
    /// another process while this mapping exists.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file metadata cannot be read, the file
    /// is empty, or `mmap(2)` fails.
    // Trust: the caller's `# Safety` contract (no concurrent modification of the
    // backing file) IS the single-writer invariant. Under `trustc -Z trust-verify`
    // this lets `ty` PROVE the temporal (truncation) safety of the mapping instead
    // of catching it — the `# Safety` promise made machine-checked.
    #[cfg_attr(trust_verify, trust::single_writer)]
    pub unsafe fn map_mut(file: &File) -> io::Result<Self> {
        // SAFETY: forwarded caller contract (exclusive access).
        unsafe { Self::map_inner(file, true) }
    }

    /// Create a read-only memory mapping of the entire file.
    ///
    /// Use this for mappings that are only ever read (e.g. the cold-tier
    /// page cache): the view is mapped `PROT_READ` / `PAGE_READONLY`, so it
    /// can hold no dirty pages, [`flush`](Self::flush) is a no-op, and
    /// dropping it performs no synchronous write-back.
    ///
    /// # Safety
    ///
    /// The caller must ensure the file is not concurrently truncated by
    /// another process while this mapping exists (a shrink leaves the tail of
    /// the mapping past EOF — SIGBUS on deref).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file metadata cannot be read, the file is
    /// empty, or the platform map call fails.
    pub unsafe fn map_read(file: &File) -> io::Result<Self> {
        // SAFETY: forwarded caller contract (no concurrent truncation).
        unsafe { Self::map_inner(file, false) }
    }

    /// # Safety
    ///
    /// See [`map_mut`](Self::map_mut) / [`map_read`](Self::map_read).
    unsafe fn map_inner(file: &File, writable: bool) -> io::Result<Self> {
        let len = file.metadata()?.len();
        if len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot mmap an empty file",
            ));
        }
        let len = usize::try_from(len).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "file size overflows usize")
        })?;

        // SAFETY: Caller guarantees exclusive/uncontended access; `len` is
        // non-zero and matches the file. The platform call maps the whole file
        // shared, read-write or read-only per `writable` (see `sys`).
        let (ptr, sys) = unsafe { sys::map(file, len, writable)? };
        Ok(Self {
            ptr,
            len,
            writable,
            sys,
        })
    }

    /// Returns a shared slice over the mapped region.
    #[must_use]
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` is a valid mmap'd region of `len` bytes, guaranteed
        // by the successful `mmap` call in `map_mut` and the absence of
        // `munmap` until `Drop`.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// Returns a checked sub-slice `[start, start + len)` of the mapped region.
    ///
    /// Returns `None` if `start + len` overflows or exceeds the recorded
    /// mapping length, ensuring the raw `from_raw_parts` is never indexed
    /// past the bytes the kernel mapped. This is the only sound way to read
    /// a sub-range from attacker-influenced offsets/lengths: bounding against
    /// `self.len` keeps the deref within the mapped region.
    ///
    /// Note: `self.len` is the length at map time. Callers that need to guard
    /// against an external truncation of the backing file should additionally
    /// validate against the live file length before calling here.
    #[must_use]
    #[inline]
    pub fn slice(&self, start: usize, len: usize) -> Option<&[u8]> {
        let end = start.checked_add(len)?;
        if end > self.len {
            return None;
        }
        // SAFETY: `start + len <= self.len`, so the sub-range lies entirely
        // within the mapped region described by `ptr`/`self.len`, which is
        // valid until `Drop`. `self.ptr.add(start)` therefore stays in bounds.
        Some(unsafe { std::slice::from_raw_parts(self.ptr.add(start), len) })
    }

    /// Returns a mutable slice over the mapped region.
    #[must_use]
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // A read-only mapping is not writable memory; handing out `&mut` to it
        // would let the caller fault (SIGSEGV / access violation).
        debug_assert!(
            self.writable,
            "as_mut_slice on a read-only mapping (use map_mut)"
        );
        // SAFETY: We have `&mut self`, so no other references exist.
        // The region is valid for `len` bytes (see `as_slice` rationale).
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// Returns the length of the mapped region in bytes.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the mapped region has zero length.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Flushes modified pages to the underlying file — `msync(2)` with
    /// `MS_SYNC` on Unix; `FlushViewOfFile` + `FlushFileBuffers` on Windows
    /// (the documented pair for the same synchronous-durability guarantee).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the platform flush fails.
    pub fn flush(&self) -> io::Result<()> {
        // A read-only mapping holds no dirty pages: flushing it would issue a
        // full FlushViewOfFile + FlushFileBuffers (device write-cache flush)
        // for nothing. Skip it.
        if !self.writable {
            return Ok(());
        }
        // SAFETY: `ptr` and `len` describe a valid mapped region.
        unsafe { sys::flush(self.ptr, self.len, &self.sys) }
    }
}

impl Deref for MmapMut {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl DerefMut for MmapMut {
    #[inline]
    fn deref_mut(&mut self) -> &mut [u8] {
        self.as_mut_slice()
    }
}

impl Drop for MmapMut {
    fn drop(&mut self) {
        // SAFETY: `ptr` and `len` are from a successful map call. After the
        // unmap, the pointer is invalidated and must not be used.
        unsafe {
            sys::unmap(self.ptr, self.len);
        }
    }
}

/// The platform seam: exactly three calls (`map`, `flush`, `unmap`) plus the
/// per-mapping [`Backing`] bookkeeping. Everything above this module is
/// platform-free.
#[cfg(unix)]
mod sys {
    use std::fs::File;
    use std::io;
    use std::os::unix::io::AsRawFd;

    /// Unix needs nothing beyond `ptr`/`len` to flush or unmap.
    #[derive(Debug)]
    pub(super) struct Backing;

    /// # Safety
    ///
    /// Caller guarantees exclusive access and that `len` is the non-zero
    /// current file length.
    pub(super) unsafe fn map(
        file: &File,
        len: usize,
        writable: bool,
    ) -> io::Result<(*mut u8, Backing)> {
        let prot = if writable {
            libc::PROT_READ | libc::PROT_WRITE
        } else {
            libc::PROT_READ
        };
        // SAFETY: We pass a valid fd, non-zero length, MAP_SHARED for
        // durability, `prot` per `writable`, and offset 0 to map the full file.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                prot,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok((ptr.cast::<u8>(), Backing))
    }

    /// # Safety
    ///
    /// `ptr`/`len` must describe a live mapping from [`map`].
    pub(super) unsafe fn flush(ptr: *mut u8, len: usize, _sys: &Backing) -> io::Result<()> {
        // SAFETY: caller contract; MS_SYNC performs a synchronous flush.
        let ret = unsafe { libc::msync(ptr.cast(), len, libc::MS_SYNC) };
        if ret != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// # Safety
    ///
    /// `ptr`/`len` must describe a live mapping from [`map`]; the pointer is
    /// dead after this call.
    pub(super) unsafe fn unmap(ptr: *mut u8, len: usize) {
        // SAFETY: caller contract.
        unsafe {
            libc::munmap(ptr.cast(), len);
        }
    }
}

/// Windows twin of the Unix seam: `CreateFileMappingW` + `MapViewOfFile`
/// (whole file, read-write, shared), `FlushViewOfFile` + `FlushFileBuffers`
/// for `MS_SYNC`-equivalent durability, `UnmapViewOfFile` on drop. The
/// mapping-object handle is closed immediately after the view is created
/// (the view keeps the mapping alive — documented Win32 behavior); a
/// duplicated file handle is retained so `flush` can reach the file.
#[cfg(windows)]
mod sys {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::{AsRawHandle, OwnedHandle};

    /// The duplicated file handle `flush` needs for `FlushFileBuffers`.
    #[derive(Debug)]
    pub(super) struct Backing {
        file: OwnedHandle,
    }

    const PAGE_READONLY: u32 = 0x02;
    const PAGE_READWRITE: u32 = 0x04;
    const FILE_MAP_READ: u32 = 0x0004;
    const FILE_MAP_WRITE: u32 = 0x0002;

    // SAFETY: standard kernel32 declarations, signatures per the Win32 docs.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileMappingW(
            hfile: isize,
            attributes: *mut core::ffi::c_void,
            protect: u32,
            max_size_high: u32,
            max_size_low: u32,
            name: *const u16,
        ) -> isize;
        fn MapViewOfFile(
            hmapping: isize,
            desired_access: u32,
            offset_high: u32,
            offset_low: u32,
            bytes_to_map: usize,
        ) -> *mut core::ffi::c_void;
        fn UnmapViewOfFile(base: *const core::ffi::c_void) -> i32;
        fn FlushViewOfFile(base: *const core::ffi::c_void, bytes: usize) -> i32;
        fn FlushFileBuffers(hfile: isize) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }

    /// # Safety
    ///
    /// Caller guarantees exclusive access and that `len` is the non-zero
    /// current file length.
    pub(super) unsafe fn map(
        file: &File,
        len: usize,
        writable: bool,
    ) -> io::Result<(*mut u8, Backing)> {
        // Duplicate the caller's handle first so `flush` can reach the file
        // for FlushFileBuffers after the borrow ends.
        let dup: OwnedHandle = file.try_clone()?.into();

        let (protect, access) = if writable {
            (PAGE_READWRITE, FILE_MAP_READ | FILE_MAP_WRITE)
        } else {
            (PAGE_READONLY, FILE_MAP_READ)
        };

        // SAFETY: valid file handle; `protect` + zero max size maps the whole
        // current file, matching the Unix `prot` + MAP_SHARED whole-file call.
        let mapping = unsafe {
            CreateFileMappingW(
                file.as_raw_handle() as isize,
                std::ptr::null_mut(),
                protect,
                0,
                0,
                std::ptr::null(),
            )
        };
        if mapping == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: live mapping handle; map exactly the validated `len` bytes.
        let view = unsafe { MapViewOfFile(mapping, access, 0, 0, len) };
        // Capture the failure reason BEFORE any cleanup: a successful
        // CloseHandle can reset the thread's last-error, clobbering the real
        // MapViewOfFile code (e.g. ERROR_NOT_ENOUGH_MEMORY) with 0.
        let err = if view.is_null() {
            Some(io::Error::last_os_error())
        } else {
            None
        };
        // The view (not the mapping handle) owns the region from here on.
        // SAFETY: `mapping` is live and no longer needed either way.
        unsafe { CloseHandle(mapping) };
        if let Some(err) = err {
            return Err(err);
        }
        Ok((view.cast::<u8>(), Backing { file: dup }))
    }

    /// # Safety
    ///
    /// `ptr`/`len` must describe a live view from [`map`].
    pub(super) unsafe fn flush(ptr: *mut u8, len: usize, sys: &Backing) -> io::Result<()> {
        // SAFETY: caller contract. FlushViewOfFile writes the dirty pages;
        // FlushFileBuffers waits for them to reach the file — together the
        // documented equivalent of msync(MS_SYNC).
        let ok = unsafe { FlushViewOfFile(ptr.cast_const().cast(), len) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `sys.file` is a live duplicated handle for this mapping's file.
        let ok = unsafe { FlushFileBuffers(sys.file.as_raw_handle() as isize) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// # Safety
    ///
    /// `ptr` must be a live view base from [`map`]; the pointer is dead
    /// after this call.
    pub(super) unsafe fn unmap(ptr: *mut u8, _len: usize) {
        // SAFETY: caller contract.
        unsafe {
            UnmapViewOfFile(ptr.cast_const().cast());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_roundtrip() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("mmap_test.bin");
        let content = b"hello mmap world";

        std::fs::write(&path, content).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        // SAFETY: exclusive access in test.
        let mmap = unsafe { MmapMut::map_mut(&file).unwrap() };
        assert_eq!(mmap.len(), content.len());
        assert_eq!(&*mmap, content);
    }

    #[test]
    fn map_mut_write_and_flush() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("mmap_write.bin");

        std::fs::write(&path, [0u8; 16]).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        // SAFETY: exclusive access in test.
        let mut mmap = unsafe { MmapMut::map_mut(&file).unwrap() };
        mmap.as_mut_slice()[0..5].copy_from_slice(b"HELLO");
        mmap.flush().unwrap();

        drop(mmap);
        drop(file);

        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[0..5], b"HELLO");
    }

    #[test]
    fn map_read_roundtrip_and_flush_is_noop() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("mmap_read.bin");
        let content = b"read-only mapping";

        std::fs::write(&path, content).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        // SAFETY: exclusive access in test.
        let mmap = unsafe { MmapMut::map_read(&file).unwrap() };
        assert_eq!(mmap.len(), content.len());
        assert_eq!(&*mmap, content);
        assert_eq!(mmap.slice(0, 4), Some(&content[0..4]));
        // A read-only mapping has no dirty pages: flush must succeed without
        // touching the device.
        mmap.flush().unwrap();
    }

    #[test]
    fn map_empty_file_fails() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.bin");

        std::fs::write(&path, []).unwrap();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        // SAFETY: test context.
        let result = unsafe { MmapMut::map_mut(&file) };
        assert!(result.is_err());
    }
}
