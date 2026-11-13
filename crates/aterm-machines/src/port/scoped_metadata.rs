// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Descriptor-bound observations for the private inventory's declared checkout scope.

use super::{ScopedPathMetadata, Stat};
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

fn open(path: &Path) -> io::Result<std::fs::File> {
    #[cfg(target_os = "macos")]
    {
        aterm_uds::nofollow::open_evtonly_path(path).map(std::fs::File::from)
    }
    #[cfg(not(target_os = "macos"))]
    {
        super::read_owned_file::hold_components(path)
    }
}

pub(super) fn observe(path: &Path) -> io::Result<ScopedPathMetadata> {
    use aterm_toolchain::stat_digest::{StatDigest, StatKey};
    let file = open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut digest = StatDigest::new(b"repository scoped metadata observation");
    digest.entry(b"", &StatKey::of(&metadata));
    Ok(ScopedPathMetadata {
        stat: Stat {
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            dir: metadata.is_dir(),
            symlink: false,
            len: metadata.len(),
        },
        version: digest.finish().0,
    })
}

/// Probe only the supplied bounded single-component directory names, relative to a held
/// no-follow descriptor. No path-based namespace enumeration can escape after admission.
pub(super) fn directories(path: &Path, uid: u32, names: &[String]) -> io::Result<Vec<String>> {
    use std::ffi::CString;
    use std::os::fd::AsRawFd as _;
    if names.len() > 256
        || names.iter().any(|name| {
            let mut parts = Path::new(name).components();
            !matches!(parts.next(), Some(std::path::Component::Normal(_)))
                || parts.next().is_some()
                || name.as_bytes().contains(&0)
                || name.as_bytes().contains(&b'/')
        })
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let file = open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut present = Vec::new();
    for name in names {
        let name_c = CString::new(name.as_bytes())?;
        // SAFETY: stat is plain storage which fstatat initializes on success. The held file
        // owns the directory descriptor; name_c is exactly one validated component.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: pointers remain live for the call and AT_SYMLINK_NOFOLLOW refuses the only
        // component that can be replaced. Ancestor names are not consulted by this lookup.
        let result = unsafe {
            libc::fstatat(
                file.as_raw_fd(),
                name_c.as_ptr(),
                &raw mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                continue;
            }
            return Err(error);
        }
        if stat.st_mode & libc::S_IFMT == libc::S_IFDIR
            && stat.st_uid == uid
            && stat.st_mode & 0o022 == 0
        {
            present.push(name.clone());
        }
    }
    Ok(present)
}

#[cfg(test)]
#[path = "scoped_metadata_tests.rs"]
mod tests;
