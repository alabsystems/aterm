// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::{Duration, Instant};

/// Returns `(resident_pages, total_pages)` for the first `len` bytes of `file`.
fn resident_pages(file: &std::fs::File, len: usize) -> (usize, usize) {
    use std::os::fd::AsRawFd as _;
    unsafe extern "C" {
        /// `<sys/mman.h>`: `int mincore(const void *, size_t, char *)`.
        fn mincore(addr: *const libc::c_void, len: usize, vec: *mut libc::c_char) -> i32;
    }
    // SAFETY: a read-only shared mapping of an open file, unmapped below.
    let map = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    assert_ne!(map, libc::MAP_FAILED, "mmap");
    // SAFETY: `sysconf` reads a constant.
    let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).expect("page");
    let mut vec = vec![0 as libc::c_char; len.div_ceil(page)];
    // SAFETY: `vec` holds one byte per page of the `len`-byte mapping.
    let rc = unsafe { mincore(map, len, vec.as_mut_ptr()) };
    // SAFETY: the mapping made above, of `len` bytes.
    unsafe { libc::munmap(map, len) };
    assert_eq!(rc, 0, "mincore");
    let resident = vec.iter().filter(|&&b| b & 1 != 0).count();
    (resident, vec.len())
}

/// `own_code_range` matches the `__TEXT` segment parsed from the on-disk executable.
#[test]
fn own_code_range_is_the_running_slices_text() {
    let exe = std::fs::read(std::env::current_exe().expect("current_exe")).expect("read");
    let le32 = |o: usize| u32::from_le_bytes(exe[o..o + 4].try_into().expect("4 bytes"));
    let le64 = |o: usize| u64::from_le_bytes(exe[o..o + 8].try_into().expect("8 bytes"));
    assert_eq!(le32(0), 0xfeed_facf, "a thin 64-bit Mach-O");
    let mut at = 32;
    let mut text = None;
    for _ in 0..le32(16) {
        if le32(at) == 0x19 && exe[at + 8..at + 24].starts_with(b"__TEXT\0") {
            text = Some((le64(at + 40), le64(at + 48)));
        }
        at += le32(at + 4) as usize;
    }
    let text = text.expect("an LC_SEGMENT_64 __TEXT");
    assert!(text.1 > 0, "{text:?}");
    assert_eq!(own_code_range(), Some(text));
}

/// `read_ahead` brings an uncached file range into the page cache.
#[test]
fn read_ahead_brings_a_cold_range_into_the_page_cache() {
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let path = scratch.path().join("cold");
    let len = 8 << 20;
    {
        let mut out = std::fs::File::create(&path).expect("create");
        // SAFETY: an fcntl on a descriptor `out` owns.
        assert_eq!(
            unsafe { libc::fcntl(out.as_raw_fd(), libc::F_NOCACHE, 1) },
            0
        );
        out.write_all(&vec![0x5a; len]).expect("write");
        aterm_durable::ordered(&out).expect("flush");
    }
    let file = std::fs::File::open(&path).expect("open");
    let (cold, pages) = resident_pages(&file, len);
    assert_eq!(
        cold, 0,
        "written uncached, the file starts out of the cache"
    );
    assert!(read_ahead(&file, 0, len as u64));
    let hang = Instant::now() + Duration::from_secs(60);
    loop {
        let (resident, _) = resident_pages(&file, len);
        if resident == pages {
            break;
        }
        assert!(
            Instant::now() < hang,
            "{resident} of {pages} pages a minute after the advice"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
