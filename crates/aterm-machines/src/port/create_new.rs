// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Prepare an owned file before publishing it at an absent destination.

use super::{Scratch, rename_noreplace};
use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

pub(super) fn create(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    finalize: impl FnOnce(&File) -> io::Result<()>,
) -> io::Result<()> {
    create_with(path, bytes, mode, finalize, rename_noreplace::rename)
}

fn create_with(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    finalize: impl FnOnce(&File) -> io::Result<()>,
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let scratch = Scratch::live(Some(parent), "create")?;
    let prepared = scratch.path().join("payload");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&prepared)?;
    file.write_all(bytes)?;
    file.set_permissions(Permissions::from_mode(mode))?;
    finalize(&file)?;
    drop(file);
    // Both primitives preserve an occupied destination. No fallible flush or cleanup follows
    // successful publication; Scratch only removes the private preparation path.
    match rename(&prepared, path) {
        Err(error) if rename_unsupported(&error) => std::fs::hard_link(&prepared, path),
        result => result,
    }
}

fn rename_unsupported(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Unsupported
        || error.raw_os_error().is_some_and(|code| {
            [libc::ENOSYS, libc::ENOTSUP, libc::EOPNOTSUPP, libc::EINVAL].contains(&code)
        })
}

#[cfg(test)]
#[path = "create_new_tests.rs"]
mod tests;
