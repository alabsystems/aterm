// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared memory for the Linux CPU presenters: an anonymous memfd, mapped
//! read-write here and handed to the display server by descriptor (MIT-SHM
//! AttachFd on X11, `wl_shm.create_pool` on Wayland), so a frame reaches the
//! server without its pixels crossing the socket.
//!
//! The file is sealed against shrinking, so a server that maps it can trust
//! its size for as long as it holds it; it may grow (a Wayland pool's
//! resize).

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;

/// One mapped memfd.
pub(crate) struct ShmMem {
    fd: OwnedFd,
    map: NonNull<u8>,
    /// The mapping's length, which the server is told; 0 when unmapped.
    len: usize,
    /// The file's size: `len` or more (a grow whose mapping failed left it
    /// larger, and the seal keeps it from shrinking).
    file_len: usize,
}

// SAFETY: the mapping is owned by this value alone; the server reads it only
// between a commit and the presenter's wait for that commit, which the
// presenter holds `&mut self` across.
unsafe impl Send for ShmMem {}

impl ShmMem {
    /// A fresh `len`-byte file, mapped.
    pub(crate) fn new(name: &CStr, len: usize) -> io::Result<ShmMem> {
        // SAFETY: a NUL-terminated name and flags memfd_create(2) defines.
        let raw = unsafe {
            libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a descriptor memfd_create just returned, owned from here.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut mem = ShmMem {
            fd,
            map: NonNull::dangling(),
            len: 0,
            file_len: 0,
        };
        mem.set_len(len)?;
        // SAFETY: a memfd this value owns; F_SEAL_SHRINK is a seal it allows.
        if unsafe { libc::fcntl(mem.fd.as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_SHRINK) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(mem)
    }

    /// Grow the file to `len` bytes and map it again. The old mapping goes
    /// only once the new one is made, so a failure leaves the memory as it
    /// was (its words, its length) for the caller to keep using.
    pub(crate) fn set_len(&mut self, len: usize) -> io::Result<()> {
        if len > self.file_len {
            let size = libc::off_t::try_from(len)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "shm size"))?;
            // SAFETY: a memfd this value owns.
            if unsafe { libc::ftruncate(self.fd.as_raw_fd(), size) } < 0 {
                return Err(io::Error::last_os_error());
            }
            self.file_len = len;
        }
        // SAFETY: a fresh shared read-write mapping of `len` bytes of a file
        // that is at least `len` bytes long.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.fd.as_raw_fd(),
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let map = NonNull::new(p.cast()).ok_or_else(io::Error::last_os_error)?;
        self.unmap();
        self.map = map;
        self.len = len;
        Ok(())
    }

    fn unmap(&mut self) {
        if self.len != 0 {
            // SAFETY: the mapping `set_len` made, unmapped once.
            unsafe { libc::munmap(self.map.as_ptr().cast(), self.len) };
            self.len = 0;
        }
    }

    /// The file's size in bytes.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// The descriptor, for a server to map.
    pub(crate) fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// The first `count` 32-bit words at byte `offset` (a multiple of 4).
    /// `None` when they do not fit.
    pub(crate) fn words_mut(&mut self, offset: usize, count: usize) -> Option<&mut [u32]> {
        let end = count.checked_mul(4)?.checked_add(offset)?;
        if end > self.len || !offset.is_multiple_of(4) {
            return None;
        }
        // SAFETY: inside the mapping, word-aligned (mmap is page-aligned and
        // `offset` is a multiple of 4), and borrowed for `&mut self`.
        Some(unsafe {
            std::slice::from_raw_parts_mut(self.map.as_ptr().add(offset).cast::<u32>(), count)
        })
    }

    /// Read-only twin of [`ShmMem::words_mut`].
    pub(crate) fn words(&self, offset: usize, count: usize) -> Option<&[u32]> {
        let end = count.checked_mul(4)?.checked_add(offset)?;
        if end > self.len || !offset.is_multiple_of(4) {
            return None;
        }
        // SAFETY: as for `words_mut`, shared.
        Some(unsafe {
            std::slice::from_raw_parts(self.map.as_ptr().add(offset).cast::<u32>(), count)
        })
    }
}

impl Drop for ShmMem {
    fn drop(&mut self) {
        self.unmap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grow the file refuses leaves the memory as it was: its length and
    /// its words, for the presenter to keep drawing in; a later grow keeps
    /// the words too.
    #[test]
    fn a_refused_grow_leaves_the_memory_as_it_was() {
        let mut mem = ShmMem::new(c"aterm-shm-test", 4096).expect("memfd");
        mem.words_mut(0, 1024)
            .expect("the mapping")
            .iter_mut()
            .enumerate()
            .for_each(|(i, w)| *w = u32::try_from(i).expect("small"));
        // SAFETY: the test's own memfd; F_SEAL_GROW is a seal it allows.
        let sealed =
            unsafe { libc::fcntl(mem.fd().as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_GROW) };
        assert_eq!(sealed, 0, "{}", io::Error::last_os_error());
        assert!(mem.set_len(8192).is_err(), "a sealed file grew");
        assert_eq!(mem.len(), 4096);
        let words = mem.words(0, 1024).expect("the old mapping");
        assert!(words.iter().enumerate().all(|(i, &w)| w as usize == i));
        assert!(mem.words(0, 1025).is_none());

        let mut grown = ShmMem::new(c"aterm-shm-test", 4096).expect("memfd");
        grown.words_mut(0, 1).expect("the mapping")[0] = 0x00ab_cdef;
        grown.set_len(8192).expect("grow");
        assert_eq!(grown.len(), 8192);
        assert_eq!(
            grown.words(0, 2048).expect("the new mapping")[0],
            0x00ab_cdef
        );
    }
}
