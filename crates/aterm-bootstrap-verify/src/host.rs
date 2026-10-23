// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The facts the ancestry walk reads from the kernel.

use std::ffi::CStr;
use std::path::Path;

/// This process's real uid.
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// The extended-ACL attributes Linux exposes.
pub const ACL_ATTRIBUTES: [&CStr; 2] = [c"system.posix_acl_access", c"system.nfs4_acl"];

const ENODATA: i32 = 61;

const ENOTSUP: i32 = 95;

/// Whether a file may carry an extended access ACL.
pub fn acl_rule(mut answer: impl FnMut(&CStr) -> Option<i32>) -> bool {
    ACL_ATTRIBUTES
        .iter()
        .any(|name| !matches!(answer(name), Some(ENODATA | ENOTSUP)))
}

/// Whether what `path` itself names (a final symbolic link not followed) may carry an extended
/// access ACL.
#[cfg(target_os = "linux")]
#[cfg_attr(trust_verify, trust::skip)]
pub(crate) fn acl_carried(path: &Path) -> bool {
    use std::ffi::{c_char, c_void};
    use std::os::unix::ffi::OsStrExt as _;
    unsafe extern "C" {
        fn lgetxattr(
            path: *const c_char,
            name: *const c_char,
            value: *mut c_void,
            size: usize,
        ) -> isize;
    }
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return true;
    };
    acl_rule(|name| {
        // SAFETY: a NUL-terminated path and name; a null value with a zero size asks only the
        // attribute's length, so nothing is written.
        let ret = unsafe { lgetxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
        if ret >= 0 {
            None
        } else {
            Some(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
        }
    })
}

/// Off Linux an ACL is never ruled out.
#[cfg(not(target_os = "linux"))]
pub(crate) fn acl_carried(_path: &Path) -> bool {
    true
}

/// `statfs` `f_type` of the network and FUSE filesystems.
pub const REMOTE_FILESYSTEMS: [u32; 10] = [
    0x6969,
    0xFF53_4D42,
    0xFE53_4D42,
    0x00C3_6400,
    0x6B41_4653,
    0x5346_414F,
    0x0102_1997,
    0x0BD0_0BD0,
    0x4750_4653,
    0x6573_5546,
];

/// Whether a filesystem of type `magic` is a network or FUSE one.
#[must_use]
pub fn remote_filesystem_type(magic: u32) -> bool {
    REMOTE_FILESYSTEMS.contains(&magic)
}

/// The `f_type` of the filesystem `path` is on (`statfs`, a final symbolic link followed).
#[cfg(target_os = "linux")]
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn filesystem_type(path: &Path) -> Option<u32> {
    use std::ffi::{c_char, c_int};
    use std::os::unix::ffi::OsStrExt as _;
    /// `<sys/vfs.h>`'s `struct statfs` as glibc lays it out on x86_64 and aarch64.
    #[repr(C)]
    struct Statfs {
        f_type: i64,
        _room: [i64; 31],
    }
    unsafe extern "C" {
        fn statfs(path: *const c_char, buf: *mut Statfs) -> c_int;
    }
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut buf = Statfs {
        f_type: 0,
        _room: [0; 31],
    };
    // SAFETY: a NUL-terminated path and a live buffer larger than glibc's `struct statfs`, which
    // the call only writes.
    let ret = unsafe { statfs(path.as_ptr(), &raw mut buf) };
    (ret == 0).then(|| u32::try_from(buf.f_type & 0xFFFF_FFFF).ok())?
}

/// Off Linux no filesystem type is read.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn filesystem_type(_path: &Path) -> Option<u32> {
    None
}

/// Whether `path` is on a network or FUSE filesystem, or one whose type cannot be read.
pub(crate) fn remote_filesystem(path: &Path) -> bool {
    filesystem_type(path).is_none_or(remote_filesystem_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_acl_rule_is_the_updaters() {
        let outcomes = [None, Some(ENODATA), Some(ENOTSUP), Some(5)];
        for posix in outcomes {
            for nfs4 in outcomes {
                let table = |name: &CStr| match name.to_bytes() {
                    b"system.posix_acl_access" => posix,
                    b"system.nfs4_acl" => nfs4,
                    other => panic!("asked {other:?}"),
                };
                assert_eq!(
                    acl_rule(table),
                    aterm_uds::shared_write::acl_carried(table),
                    "posix {posix:?}, nfs4 {nfs4:?}"
                );
            }
        }
        assert!(!acl_rule(|_| Some(ENODATA)), "no ACL");
        assert!(acl_rule(|_| Some(5)), "an unreadable ACL");
    }

    #[test]
    fn the_remote_filesystems_are_the_updaters() {
        assert_eq!(
            REMOTE_FILESYSTEMS,
            aterm_uds::shared_write::REMOTE_FILESYSTEMS
        );
        for magic in [0xEF53, 0x9123_683E, 0x9FA0, 0, u32::MAX] {
            assert_eq!(
                remote_filesystem_type(magic),
                aterm_uds::shared_write::remote_filesystem(magic),
                "{magic:#x}"
            );
        }
    }

    /// The type statfs reads is the updater's on this machine's paths.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_filesystem_type_is_the_updaters_on_this_machine() {
        let scratch = aterm_tempfile::Builder::new().tempdir().expect("scratch");
        for path in [
            Path::new("/"),
            Path::new("/proc"),
            scratch.path(),
            &scratch.path().join("no-such-entry"),
        ] {
            assert_eq!(
                filesystem_type(path),
                aterm_uds::shared_write::filesystem_type_at(path),
                "{}",
                path.display()
            );
        }
        assert_eq!(filesystem_type(Path::new("/proc")), Some(0x9FA0));
    }
}
