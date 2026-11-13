// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A window's start asks for this executable's code in one read.

/// Ask the kernel to read this executable's `__TEXT` into the page cache now.
#[cfg(target_os = "macos")]
pub(crate) fn read_ahead_own_code() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let Some((offset, length)) = own_code_range() else {
        return;
    };
    let Ok(file) = std::env::current_exe().and_then(|exe| {
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(exe)
    }) else {
        return;
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return;
    }
    let _ = read_ahead(&file, offset, length);
}

/// Issues `fcntl(F_RDADVISE)` for `length` bytes of `file` from `offset`.
#[cfg(target_os = "macos")]
fn read_ahead(file: &std::fs::File, offset: u64, length: u64) -> bool {
    use std::os::fd::AsRawFd as _;
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
    // SAFETY: `advice` is a live `struct radvisory` for the call's duration; the descriptor is
    // `file`'s, open for reading.
    unsafe { libc::fcntl(file.as_raw_fd(), F_RDADVISE, &raw const advice) == 0 }
}

/// Returns the running Mach-O slice's `__TEXT` segment as `(file_offset, length)`.
#[cfg(target_os = "macos")]
fn own_code_range() -> Option<(u64, u64)> {
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
    // SAFETY: the linker defines the symbol at this executable's mapped Mach-O header, which stays
    // mapped (read-only) for the process's life.
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
        // SAFETY: `at + 8` is within `sizeofcmds`, the mapped load commands; every command starts
        // with its `cmd` and `cmdsize`.
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
#[path = "code_readahead_tests.rs"]
mod tests;
