// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptor-bound, bounded admission for local credentials and configuration.

use std::io;
use std::path::Path;

pub(super) fn admit(
    owner: u32,
    uid: u32,
    mode: u32,
    regular: bool,
    private: bool,
) -> io::Result<()> {
    if !regular || owner != uid || mode & 0o6022 != 0 || (private && mode & 0o077 != 0) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

pub(super) fn read(path: &Path, uid: u32, max: usize, private: bool) -> io::Result<Vec<u8>> {
    max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    read_admitted(open(path)?, uid, max, private)
}

fn open(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

fn admitted_metadata(
    file: &std::fs::File,
    uid: u32,
    max: usize,
    private: bool,
) -> io::Result<super::OwnedFileMetadata> {
    use aterm_toolchain::stat_digest::{StatDigest, StatKey};
    use std::os::unix::fs::MetadataExt as _;
    max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    let metadata = file.metadata()?;
    admit(
        metadata.uid(),
        uid,
        metadata.mode(),
        metadata.is_file(),
        private,
    )?;
    if metadata.len() > max as u64 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let key = StatKey::of(&metadata);
    let mut digest = StatDigest::new(b"owned file metadata observation");
    digest.entry(b"", &key);
    Ok(super::OwnedFileMetadata {
        stat: super::Stat {
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            dir: false,
            symlink: false,
            len: metadata.len(),
        },
        version: digest.finish().0,
        changed_ns: key.ctime_ns.max(key.mtime_ns),
    })
}

pub(super) fn metadata(
    path: &Path,
    uid: u32,
    max: usize,
    private: bool,
) -> io::Result<super::OwnedFileMetadata> {
    max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    admitted_metadata(&open(path)?, uid, max, private)
}

pub(super) fn observed(
    path: &Path,
    uid: u32,
    max: usize,
    private: bool,
) -> io::Result<super::OwnedFileRead> {
    use std::io::Read as _;
    max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    observed_with(open(path)?, uid, max, private, |file, bound| {
        let mut bytes = Vec::new();
        file.take(bound as u64).read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn observed_with(
    mut file: std::fs::File,
    uid: u32,
    max: usize,
    private: bool,
    read: impl FnOnce(&mut std::fs::File, usize) -> io::Result<Vec<u8>>,
) -> io::Result<super::OwnedFileRead> {
    let metadata = admitted_metadata(&file, uid, max, private)?;
    let bytes = read(
        &mut file,
        max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?,
    )?;
    if bytes.len() > max || metadata != admitted_metadata(&file, uid, max, private)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "owned file changed during read",
        ));
    }
    Ok(super::OwnedFileRead { bytes, metadata })
}

pub(super) fn read_scoped(path: &Path, uid: u32, max: usize, private: bool) -> io::Result<Vec<u8>> {
    max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    #[cfg(target_os = "macos")]
    let file = {
        let (fd, _) = aterm_uds::nofollow::open_reader_with_identity(path)?;
        std::fs::File::from(fd)
    };
    #[cfg(not(target_os = "macos"))]
    let file = open_components(path)?;
    read_admitted(file, uid, max, private)
}

/// How [`walk`] holds each folder on the way: on Linux as a path (`O_PATH`), which needs only
/// the search permission a plain open needs there, so a folder its owner may search but not
/// list (a home under a 0711 folder) is passed as `open` passes it; elsewhere opened to read.
#[cfg(any(target_os = "linux", target_os = "android"))]
const HELD: libc::c_int = libc::O_PATH;
#[cfg(all(
    any(not(target_os = "macos"), test),
    not(any(target_os = "linux", target_os = "android"))
))]
const HELD: libc::c_int = libc::O_RDONLY | libc::O_NONBLOCK;

/// Unix platforms without macOS's no-follow-any opener walk from an owned root descriptor
/// ([`walk`]); the last component is opened to read, never as a controlling terminal.
#[cfg(any(not(target_os = "macos"), test))]
pub(super) fn open_components(path: &Path) -> io::Result<std::fs::File> {
    walk(path, libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOCTTY)
}

/// [`open_components`] for a caller that only `fstat`s what is at `path` or looks names up
/// under it: the last component is held as each folder on the way is ([`HELD`]), so a scope
/// its owner may search but not list is read as on macOS (`O_EVTONLY`), and a link there
/// answers `ELOOP` as it does when opened to read.
#[cfg(not(target_os = "macos"))]
pub(super) fn hold_components(path: &Path) -> io::Result<std::fs::File> {
    let held = walk(path, HELD | libc::O_NOCTTY)?;
    if held.metadata()?.file_type().is_symlink() {
        return Err(io::Error::from_raw_os_error(libc::ELOOP));
    }
    Ok(held)
}

/// Each component opened relative to the previous descriptor with O_NOFOLLOW, each folder on
/// the way held as [`HELD`] says and the last with `last`; replacing a parent name cannot
/// redirect the next open away from the directory already held.
#[cfg(any(not(target_os = "macos"), test))]
fn walk(path: &Path, last: libc::c_int) -> io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let tail = path
        .strip_prefix("/")
        .map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut parts = tail.components().peekable();
    if parts.peek().is_none() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(HELD | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    while let Some(part) = parts.next() {
        let std::path::Component::Normal(name) = part else {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        let name = CString::new(name.as_bytes())?;
        let flags = libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | if parts.peek().is_some() {
                HELD | libc::O_DIRECTORY
            } else {
                last
            };
        // SAFETY: directory owns this live descriptor; name is a component-only C string.
        // The returned descriptor is newly owned and no borrowed fd survives reassignment.
        let raw = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh descriptor exclusively owned by this File.
        directory = unsafe { std::fs::File::from_raw_fd(raw) };
    }
    Ok(directory)
}

fn read_admitted(file: std::fs::File, uid: u32, max: usize, private: bool) -> io::Result<Vec<u8>> {
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let bound = max.checked_add(1).ok_or(io::ErrorKind::InvalidInput)?;
    let metadata = file.metadata()?;
    admit(
        metadata.uid(),
        uid,
        metadata.mode(),
        metadata.is_file(),
        private,
    )?;
    if metadata.len() > max as u64 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let cap = usize::try_from(metadata.len())
        .unwrap_or(0)
        .min(max)
        .saturating_add(1);
    let mut bytes = Vec::with_capacity(cap);
    file.take(bound as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "read_owned_file_tests.rs"]
mod tests;
