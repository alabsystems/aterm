// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A window's start asks for this executable's code in one read.
//!
//! A launch whose code is out of the page cache (after a reboot, an update, or memory pressure
//! that evicted it) otherwise brings it in one page fault at a time, and the kernel's
//! read-around is slow at that: a sequential walk of a profiling build's 55 MB `__TEXT`
//! through a mapping took 66-68 ms (0.8 GB/s), and aterm's scattered first calls are no walk.
//! One `F_RDADVISE` of the running slice's `__TEXT` queues the whole range at the disk's
//! sequential rate and returns: the same walk then finished in 8.5 ms, for 1.2 ms of the
//! caller's time with the pages cold and 44 µs (opening the file and advising it, median of
//! 200) with them resident. A headless start with its pages cold reached `ready` in 47 ms
//! against 77 without it, n=12 (r6 sweep #19, `docs/measured/cold-pages-2026-10-20.md`).
//!
//! The range is the running slice's in a universal file (`csops(CS_OPS_PIDOFFSET)`) and
//! `__TEXT`'s size comes from this image's own header, so nothing is read to find it. macOS
//! only: the measured fault cost and the advisory call are Darwin's.

/// Ask the kernel to read this executable's `__TEXT` into the page cache now; best effort
/// and asynchronous (the call queues the reads and returns). The window's entry calls it
/// first, once the front door has answered every verb, `--help` and `--version`.
#[cfg(target_os = "macos")]
pub(crate) fn read_ahead_own_code() {
    let Some((offset, length)) = own_code_range() else {
        return;
    };
    let Ok(file) = std::env::current_exe().and_then(std::fs::File::open) else {
        return;
    };
    let _ = read_ahead(&file, offset, length);
}

/// `fcntl(F_RDADVISE)` of `length` bytes of `file` from `offset`: the kernel starts reading
/// them into the page cache and the call returns. `false` when it refuses.
#[cfg(target_os = "macos")]
fn read_ahead(file: &std::fs::File, offset: u64, length: u64) -> bool {
    use std::os::fd::AsRawFd as _;
    // `<sys/fcntl.h>`: `F_RDADVISE` and its `struct radvisory { off_t ra_offset; int
    // ra_count; }`.
    const F_RDADVISE: libc::c_int = 44;
    #[repr(C)]
    struct Radvisory {
        ra_offset: i64,
        ra_count: libc::c_int,
    }
    let (Ok(ra_offset), Ok(ra_count)) = (i64::try_from(offset), libc::c_int::try_from(length))
    else {
        return false;
    };
    let advice = Radvisory {
        ra_offset,
        ra_count,
    };
    // SAFETY: `advice` is a live `struct radvisory` for the call's duration; the descriptor
    // is `file`'s, open for reading.
    unsafe { libc::fcntl(file.as_raw_fd(), F_RDADVISE, &raw const advice) == 0 }
}

/// The running slice's `__TEXT` as (file offset, length): the slice's offset in its file from
/// the kernel, `__TEXT`'s file size from this image's mapped header. `None` if either is
/// missing.
#[cfg(target_os = "macos")]
fn own_code_range() -> Option<(u64, u64)> {
    // `<mach-o/loader.h>`.
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const LC_SEGMENT_64: u32 = 0x19;
    /// `struct mach_header_64`; its load commands follow it in memory.
    #[repr(C)]
    struct MachHeader64 {
        magic: u32,
        cputype: i32,
        cpusubtype: i32,
        filetype: u32,
        ncmds: u32,
        sizeofcmds: u32,
        flags: u32,
        reserved: u32,
    }
    /// The head of `struct segment_command_64`, up to its file size.
    #[repr(C)]
    struct SegmentCommand64 {
        cmd: u32,
        cmdsize: u32,
        segname: [u8; 16],
        vmaddr: u64,
        vmsize: u64,
        fileoff: u64,
        filesize: u64,
    }
    // xnu `bsd/sys/codesign.h`: the offset of the process's Mach-O slice in its file.
    const CS_OPS_PIDOFFSET: u32 = 6;
    unsafe extern "C" {
        /// The linker's symbol for this executable's own header, mapped at `__TEXT`'s start.
        static _mh_execute_header: MachHeader64;
        /// `int csops(pid_t, unsigned int ops, void *useraddr, size_t usersize)`; no SDK
        /// header declares it.
        fn csops(pid: libc::pid_t, ops: u32, useraddr: *mut libc::c_void, usersize: usize) -> i32;
    }
    let mut slice: i64 = 0;
    // SAFETY: `slice` is one writable `off_t`, the size passed; `getpid` has no preconditions.
    let rc = unsafe {
        csops(
            libc::getpid(),
            CS_OPS_PIDOFFSET,
            (&raw mut slice).cast(),
            size_of::<i64>(),
        )
    };
    let slice = u64::try_from(slice).ok().filter(|_| rc == 0)?;
    let header = &raw const _mh_execute_header;
    // SAFETY: the linker defines the symbol at this executable's mapped Mach-O header, which
    // stays mapped (read-only) for the process's life.
    let (magic, ncmds, sizeofcmds) =
        unsafe { ((*header).magic, (*header).ncmds, (*header).sizeofcmds) };
    if magic != MH_MAGIC_64 {
        return None;
    }
    let commands = header.cast::<u8>().wrapping_add(size_of::<MachHeader64>());
    let end = sizeofcmds as usize;
    let mut at = 0usize;
    for _ in 0..ncmds {
        if at + 8 > end {
            return None;
        }
        // SAFETY: `at + 8` is within `sizeofcmds`, the mapped load commands; every command
        // starts with its `cmd` and `cmdsize`.
        let (cmd, size) = unsafe {
            let head = commands.add(at).cast::<[u32; 2]>().read_unaligned();
            (head[0], head[1] as usize)
        };
        if cmd == LC_SEGMENT_64 && size >= size_of::<SegmentCommand64>() && at + size <= end {
            // SAFETY: a segment command of at least the head's size, inside the commands.
            let segment = unsafe { commands.add(at).cast::<SegmentCommand64>().read_unaligned() };
            if segment.segname.starts_with(b"__TEXT\0") {
                return Some((slice.checked_add(segment.fileoff)?, segment.filesize));
            }
        }
        if size == 0 {
            return None;
        }
        at += size;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// How many of `file`'s first `len` bytes' pages are in the page cache: `mincore` over a
    /// fresh read-only mapping, which takes no fault.
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
        // `MINCORE_INCORE` is bit 0.
        let resident = vec.iter().filter(|&&b| b & 1 != 0).count();
        (resident, vec.len())
    }

    /// The range is this test binary's own `__TEXT` as its file records it: a thin Mach-O, so
    /// the slice starts at 0, and `__TEXT`'s load command read from the file, not memory.
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

    /// `read_ahead` brings a range whose pages are out of the page cache into it with no read
    /// of ours: a file written with `F_NOCACHE` holds no resident page until the advice, and
    /// every page of the range after it (the wait is a hang detector).
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
            // Written out, so no page of it lingers dirty; a test promises nothing past a crash.
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
}
