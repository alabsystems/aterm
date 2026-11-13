// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright 2026 Andrew Yates

//! Read-only owner-private opens. The Apple ACL policy follows maintained
//! astream-sys owner_private: an allow entry is not owner-only, deny entries
//! confer no access. Unknown ACL observations refuse instead of granting.
use std::ffi::{CString, OsStr};
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use crate::schema::{FileId, refused};

pub(crate) fn identity(meta: &Metadata) -> FileId {
    FileId::of_metadata(meta)
}
pub(crate) fn stamp(meta: &Metadata) -> (FileId, u64, u64, u32, u64, i64, i64, i64, i64) {
    (
        identity(meta),
        meta.len(),
        meta.nlink(),
        meta.mode(),
        u64::from(meta.uid()),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}
pub(crate) fn owned(file: &File, directory: bool) -> io::Result<()> {
    let m = file.metadata()?;
    // SAFETY: geteuid reads the current effective account identity only.
    if m.uid() != unsafe { libc::geteuid() } {
        return Err(refused("provenance backing belongs to another account"));
    }
    if m.mode() & 0o7077 != 0 {
        return Err(refused("provenance backing has public or special mode bits"));
    }
    if if directory { !m.is_dir() } else { !m.is_file() || m.nlink() != 1 } {
        return Err(refused("provenance backing has wrong kind or link count"));
    }
    require_no_acl_grants(file)?;
    Ok(())
}

/// Recheck a held directory's current name without opening any replacement.
/// Every component is observed without following its final symlink. The held
/// descriptor remains the input owner; metadata at a name only vetoes reuse.
pub(crate) fn named_directory(path: &Path, held: &File) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(refused("absolute provenance path required"));
    }
    let mut prefix = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Normal(_) => prefix.push(component.as_os_str()),
            _ => return Err(refused("noncanonical provenance path")),
        }
        if !std::fs::symlink_metadata(&prefix)?.is_dir() {
            return Err(refused("provenance directory component changed"));
        }
    }
    owned(held, true)?;
    if identity(&std::fs::symlink_metadata(path)?) != identity(&held.metadata()?) {
        return Err(refused("provenance directory replaced"));
    }
    Ok(())
}

/// A current-name check is never an open or an import of newly selected bytes.
pub(crate) fn named_file(path: &Path, held: &File) -> io::Result<()> {
    owned(held, false)?;
    if stamp(&std::fs::symlink_metadata(path)?) != stamp(&held.metadata()?) {
        return Err(refused("provenance original file name changed"));
    }
    Ok(())
}

pub(crate) fn read_original(file: &File, max: usize) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt as _;
    owned(file, false)?;
    let before = file.metadata()?;
    if before.len() > max as u64 {
        return Err(refused("oversized original private record"));
    }
    let mut bytes = vec![0; before.len() as usize];
    file.read_exact_at(&mut bytes, 0)?;
    if stamp(&before) != stamp(&file.metadata()?) {
        return Err(refused("original private record changed during read"));
    }
    Ok(bytes)
}

/// Read-only ACL admission shared with native owners. The caller must also
/// check its required owner, kind, link count, POSIX modes and path custody.
/// No cache, process, resource or proof authority is created by this check.
pub fn require_no_acl_grants(file: &File) -> io::Result<()> {
    if acl_forbids(file, AclPolicy::NoGrants)? {
        return Err(refused("provenance backing ACL grants access"));
    }
    Ok(())
}

/// Require an empty extended ACL, including inherited and deny-only entries.
/// This is stricter than owner-only access: a proof store need not interpret
/// platform-specific ACL ordering or inheritance. Other descriptor checks
/// remain the caller's responsibility.
pub fn require_no_acl_entries(file: &File) -> io::Result<()> {
    if acl_forbids(file, AclPolicy::NoEntries)? {
        return Err(refused("provenance backing has extended ACL entries"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum AclPolicy {
    NoGrants,
    NoEntries,
}
fn open_at(parent: i32, name: &OsStr, flags: i32) -> io::Result<File> {
    let name = CString::new(name.as_bytes()).map_err(|_| refused("path contains NUL"))?;
    // SAFETY: name is NUL-terminated. Read-only flags never create an entry;
    // parent is a held directory or AT_FDCWD for the absolute root.
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned this newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub(crate) fn directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(refused("absolute provenance path required"));
    }
    let mut dir = open_at(libc::AT_FDCWD, OsStr::new("/"), libc::O_RDONLY | libc::O_DIRECTORY)?;
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name) => {
                dir = open_at(dir.as_raw_fd(), name, libc::O_RDONLY | libc::O_DIRECTORY)?
            }
            _ => return Err(refused("noncanonical provenance path")),
        }
    }
    owned(&dir, true)?;
    Ok(dir)
}
pub(crate) fn open_private(dir: &File, name: &OsStr) -> io::Result<File> {
    if name.is_empty()
        || Path::new(name).components().count() != 1
        || !matches!(Path::new(name).components().next(), Some(Component::Normal(_)))
    {
        return Err(refused("private filename must be one component"));
    }
    owned(dir, true)?;
    let file = open_at(dir.as_raw_fd(), name, libc::O_RDONLY)?;
    owned(&file, false)?;
    Ok(file)
}
pub(crate) fn read_private(dir: &File, name: &OsStr, max: usize) -> io::Result<(Vec<u8>, File)> {
    let mut file = open_private(dir, name)?;
    let before = file.metadata()?;
    if before.len() > max as u64 {
        return Err(refused("oversized private record"));
    }
    let mut bytes = Vec::new();
    (&mut file).take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max
        || bytes.len() as u64 != before.len()
        || stamp(&before) != stamp(&file.metadata()?)
    {
        return Err(refused("private record changed during read"));
    }
    Ok((bytes, file))
}

#[cfg(target_os = "macos")]
fn acl_forbids(file: &File, policy: AclPolicy) -> io::Result<bool> {
    use std::ffi::{c_int, c_void};
    // ABI and constants are the maintained astream-sys owner.rs declarations
    // from Apple's sys/acl.h, not a filesystem permission inference.
    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
        fn acl_get_entry(acl: *mut c_void, which: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_get_tag_type(entry: *mut c_void, tag: *mut c_int) -> c_int;
        fn acl_free(object: *mut c_void) -> c_int;
        fn acl_valid(acl: *mut c_void) -> c_int;
    }
    struct Acl(*mut c_void);
    impl Drop for Acl {
        fn drop(&mut self) {
            // SAFETY: this handle came from acl_get_fd_np and is freed once.
            unsafe {
                acl_free(self.0);
            }
        }
    }
    // SAFETY: the file is held through the call; success returns an owned ACL.
    let raw = unsafe { acl_get_fd_np(file.as_raw_fd(), 0x100) };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::ENOENT) { Ok(false) } else { Err(error) };
    }
    let acl = Acl(raw);
    // SAFETY: acl is this owned, live ACL snapshot. Validate it before treating
    // the documented terminal EINVAL for our known cursor as end-of-list.
    if unsafe { acl_valid(acl.0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut which = 0; // ACL_FIRST_ENTRY, then ACL_NEXT_ENTRY=-1
    loop {
        let mut entry = std::ptr::null_mut();
        // SAFETY: acl is live and entry is one writable output pointer.
        if unsafe { acl_get_entry(acl.0, which, &mut entry) } != 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::EINVAL) { Ok(false) } else { Err(error) };
        }
        if matches!(policy, AclPolicy::NoEntries) {
            return Ok(true);
        }
        which = -1;
        let mut tag = 0;
        // SAFETY: entry is from the live ACL and tag is writable.
        if unsafe { acl_get_tag_type(entry, &mut tag) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if tag == 1 {
            return Ok(true);
        } // ACL_EXTENDED_ALLOW
        if tag != 2 {
            return Err(refused("unknown extended ACL tag"));
        } // ACL_EXTENDED_DENY
    }
}
#[cfg(not(target_os = "macos"))]
fn acl_forbids(file: &File, policy: AclPolicy) -> io::Result<bool> {
    // On Linux POSIX ACL permissions to other users are bounded by group mask
    // bits, which owned() rejects. Other platforms have no qualified route.
    #[cfg(target_os = "linux")]
    {
        if matches!(policy, AclPolicy::NoGrants) {
            return Ok(false);
        }
        for name in [c"system.posix_acl_access", c"system.posix_acl_default"] {
            // SAFETY: live descriptor and constant NUL-terminated name. A
            // null/zero buffer asks only whether this ACL attribute exists.
            let size = unsafe {
                libc::fgetxattr(file.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0)
            };
            if size >= 0 {
                return Ok(true);
            }
            let error = io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(libc::ENODATA) | Some(libc::ENOTSUP)) {
                return Err(error);
            }
        }
        Ok(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (file, policy);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
#[path = "private/tests.rs"]
mod tests;
