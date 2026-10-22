// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The cordoned Unix module: hand-declared FFI for the owner-only file rule (no libc
//! dependency). `geteuid` on every Unix, and on Apple the four `<sys/acl.h>` calls that
//! read a descriptor's extended access list. The `open` flags the rule opens with are
//! plain per-OS numbers, kept here beside the calls they serve. Raw pointers stay inside
//! this module: every function returns an owned value or `io::Result`.

use std::ffi::c_int;
#[cfg(target_vendor = "apple")]
use std::ffi::c_void;
use std::fs::File;
use std::io;

unsafe extern "C" {
    fn geteuid() -> u32;
}

#[cfg(target_vendor = "apple")]
unsafe extern "C" {
    fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
    fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
    fn acl_get_tag_type(entry: *mut c_void, tag: *mut c_int) -> c_int;
    fn acl_free(object: *mut c_void) -> c_int;
}

/// `<sys/acl.h>`'s `ACL_TYPE_EXTENDED`: the one ACL type macOS keeps.
#[cfg(target_vendor = "apple")]
const ACL_TYPE_EXTENDED: c_int = 0x0000_0100;
/// `ACL_FIRST_ENTRY` and `ACL_NEXT_ENTRY`, `acl_get_entry`'s cursor.
#[cfg(target_vendor = "apple")]
const ACL_FIRST_ENTRY: c_int = 0;
#[cfg(target_vendor = "apple")]
const ACL_NEXT_ENTRY: c_int = -1;
/// `acl_tag_t`'s `ACL_EXTENDED_ALLOW`: an entry that grants (the other is a deny).
#[cfg(target_vendor = "apple")]
const ACL_EXTENDED_ALLOW: c_int = 1;
/// `ENOENT`, which `acl_get_fd_np` answers for a file that carries no extended list.
#[cfg(target_vendor = "apple")]
const ENOENT: i32 = 2;

/// `(O_NONBLOCK, O_NOFOLLOW)` for this target, or `None` where this crate declares
/// none. Darwin and the BSDs share `<sys/fcntl.h>`'s numbers; Linux's differ by
/// architecture (`asm-generic/fcntl.h`, and `arch/{arm,arm64,powerpc}`'s overrides of
/// `O_NOFOLLOW`).
pub(crate) const OPEN_FLAGS: Option<(c_int, c_int)> = open_flags();

const fn open_flags() -> Option<(c_int, c_int)> {
    if cfg!(any(
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )) {
        Some((0x0004, 0x0100))
    } else if cfg!(all(
        any(target_os = "linux", target_os = "android"),
        any(
            target_arch = "x86",
            target_arch = "x86_64",
            target_arch = "riscv32",
            target_arch = "riscv64",
            target_arch = "s390x",
            target_arch = "loongarch64"
        )
    )) {
        Some((0o4000, 0o400000))
    } else if cfg!(all(
        any(target_os = "linux", target_os = "android"),
        any(
            target_arch = "arm",
            target_arch = "aarch64",
            target_arch = "powerpc",
            target_arch = "powerpc64"
        )
    )) {
        Some((0o4000, 0o100000))
    } else {
        None
    }
}

/// This process's effective user id.
pub(crate) fn euid() -> u32 {
    // SAFETY: `geteuid` takes nothing, cannot fail, and touches no memory of ours.
    unsafe { geteuid() }
}

/// Whether `file`'s extended access list holds an entry that ALLOWS anyone anything.
/// A file with no list at all (`ENOENT`) holds none; a deny entry is not counted.
#[cfg(target_vendor = "apple")]
pub(crate) fn acl_allows(file: &File) -> io::Result<bool> {
    use std::os::fd::AsRawFd as _;
    // SAFETY: the descriptor is `file`'s, open for the whole call; the list returned
    // is owned here and freed once below.
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(ENOENT) => Ok(false),
            _ => Err(error),
        };
    }
    let mut allows = false;
    let mut entry: *mut c_void = std::ptr::null_mut();
    let mut which = ACL_FIRST_ENTRY;
    // SAFETY: `acl` is the live list; `entry` is written by the call and read only
    // while `acl` lives.
    while unsafe { acl_get_entry(acl, which, &mut entry) } == 0 {
        which = ACL_NEXT_ENTRY;
        let mut tag: c_int = 0;
        // SAFETY: `entry` is an entry of the live `acl`; `tag` is this frame's.
        if unsafe { acl_get_tag_type(entry, &mut tag) } == 0 && tag == ACL_EXTENDED_ALLOW {
            allows = true;
            break;
        }
    }
    // SAFETY: `acl` came from `acl_get_fd_np` and is freed exactly once.
    unsafe { acl_free(acl) };
    Ok(allows)
}

/// No extended access list off Apple: a Linux POSIX ACL that grants anyone but the
/// owner shows in the group mode bits (its mask), which the mode clause already reads.
#[cfg(not(target_vendor = "apple"))]
pub(crate) fn acl_allows(_file: &File) -> io::Result<bool> {
    Ok(false)
}
