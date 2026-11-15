// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private, immutable, file-backed input for the slice JSON parser.

#[cfg(any(unix, windows))]
use std::fs::File;
use std::io::{self, Read};
use std::ptr::NonNull;

#[cfg(windows)]
use aterm_win32::Win32::{
    Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE},
    Storage::FileSystem::{DELETE, FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE},
    System::Memory::{
        CreateFileMappingW, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
        PAGE_READONLY, UnmapViewOfFile,
    },
};

/// No writable handle or name for this file escapes this module. The source is
/// copied before mapping, so its later writes and truncations cannot change a
/// borrowed slice or fault a page of the mapping.
pub(super) struct Snapshot {
    ptr: NonNull<u8>,
    len: usize,
    #[cfg(windows)]
    _backing: Backing,
}

// Fields drop in this order, including on a copy/map failure: Windows must
// close its delete-on-close file before removing its enclosing directory.
#[cfg(any(unix, windows))]
struct Backing {
    file: File,
    #[cfg(windows)]
    _directory: aterm_tempfile::TempDir,
}

#[cfg(any(unix, windows))]
impl Backing {
    fn new() -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                file: aterm_tempfile::tempfile()?,
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            let directory = aterm_tempfile::tempdir()?;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .share_mode(0)
                .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
                .custom_flags(FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE)
                .open(directory.path().join("snapshot"))?;
            Ok(Self {
                file,
                _directory: directory,
            })
        }
    }
}

impl Snapshot {
    pub(super) fn read(source: impl Read) -> io::Result<Self> {
        #[cfg(not(any(unix, windows)))]
        {
            let _ = source;
            Err(io::ErrorKind::Unsupported.into())
        }
        #[cfg(any(unix, windows))]
        {
            let mut backing = Backing::new()?;
            // io::copy uses bounded buffers, including when the input is a very
            // long record. Only the private copy will ever back a Rust slice.
            let length = io::copy(&mut { source }, &mut backing.file)?;
            let len = usize::try_from(length)
                .ok()
                .filter(|&n| n <= isize::MAX as usize)
                .ok_or_else(|| io::Error::other("report snapshot exceeds address space"))?;
            let ptr = if len == 0 {
                NonNull::dangling()
            } else {
                map(&backing.file, len)?
            };
            Ok(Self {
                ptr,
                len,
                #[cfg(windows)]
                _backing: backing,
            })
        }
    }

    pub(super) fn bytes(&self) -> &[u8] {
        // SAFETY: ptr owns len initialized read-only bytes until Drop, or is a
        // non-null aligned dangling pointer for the empty slice. The file is
        // our private completed copy, no writer can access it, and len fits
        // isize. The returned slice cannot outlive this mapping.
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

#[cfg(unix)]
fn map(file: &File, len: usize) -> io::Result<NonNull<u8>> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: file is an unlinked private temporary file holding len bytes.
    // The read-only mapping starts at offset zero; no writable handle is kept
    // or shared after construction. MAP_FAILED is checked before use.
    let raw = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if raw == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    if let Some(ptr) = NonNull::new(raw.cast()) {
        return Ok(ptr);
    }
    // SAFETY: a null address can be a successful mmap; it cannot back a Rust
    // slice, but is still this mapping and must be released on refusal.
    unsafe { libc::munmap(raw, len) };
    Err(io::Error::other("report snapshot mapped at null"))
}

#[cfg(windows)]
fn map(file: &File, len: usize) -> io::Result<NonNull<u8>> {
    use std::os::windows::io::AsRawHandle as _;
    // SAFETY: the completed private file contains len bytes; null attributes
    // and name create an unnamed read-only section. Its no-sharing handle is
    // retained by Snapshot until after the view is unmapped.
    let section = unsafe {
        CreateFileMappingW(
            file.as_raw_handle(),
            std::ptr::null(),
            PAGE_READONLY,
            0,
            0,
            std::ptr::null(),
        )
    };
    if section.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: section is live and covers len bytes from offset zero;
    // FILE_MAP_READ creates a read-only view.
    let raw = unsafe { MapViewOfFile(section, FILE_MAP_READ, 0, 0, len) }.Value;
    let error = raw.is_null().then(io::Error::last_os_error);
    // SAFETY: this live section handle is owned here; the view keeps the
    // section alive independently after this handle is closed.
    unsafe { CloseHandle(section) };
    match NonNull::new(raw.cast()) {
        Some(ptr) => Ok(ptr),
        None => Err(error.unwrap_or_else(|| io::Error::other("null snapshot view"))),
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if self.len == 0 {
            return;
        }
        #[cfg(unix)]
        // SAFETY: this is the owned mapping, no slice outlives self, and Drop
        // runs exactly once with the same address and length mmap returned.
        unsafe {
            libc::munmap(self.ptr.as_ptr().cast(), self.len);
        }
        #[cfg(windows)]
        // SAFETY: this is the owned view, and no borrowed slice outlives self.
        // The file is closed and deleted only after this view is unmapped.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.ptr.as_ptr().cast(),
            });
        }
    }
}
