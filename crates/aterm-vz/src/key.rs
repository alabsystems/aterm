// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's half of the boot's key (`docs/DESIGN-vz-guest-2026-10-23.md` §6,
//! [`aterm_vz_agent::key`]): the helper makes a fresh key at each `Boot` and writes it to
//! the key disk, `run/key.img`, which the VM attaches read-only and only guest root reads.
//! The agent's `Hello` must carry it, or no frame of that dial is believed. The key never
//! leaves the helper: the lab's `Ready` carries a hello without it.

use std::io::{self, Write as _};
use std::path::Path;

pub use aterm_vz_agent::key::{DISK_BYTES, Key, LEN, same};

/// A fresh key from the kernel's random source.
///
/// # Errors
/// `/dev/urandom` could not be read.
pub fn fresh() -> io::Result<Key> {
    let mut key = [0u8; LEN];
    aterm_uds::rand::fill(&mut key)?;
    if key.iter().all(|b| *b == 0) {
        return Err(io::Error::other("/dev/urandom gave 32 zero bytes"));
    }
    Ok(key)
}

/// Write the key disk for `key` at `path`, replacing any earlier one: owner-only (0600),
/// the key's sector first, [`DISK_BYTES`] long and sparse.
///
/// # Errors
/// `path`'s parent directory is not this user's plain directory, or the disk could not be
/// removed, made or written.
pub fn write_disk(path: &Path, key: &Key) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let parent = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !crate::record::owned_plain_dir(parent) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: not this user's plain directory", parent.display()),
        ));
    }
    let stage = aterm_tempfile::Builder::new()
        .prefix(".key-")
        .tempdir_in(parent)?;
    let staged = stage.path().join("key.img");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&staged)?;
    f.write_all(&aterm_vz_agent::key::sector(key))?;
    f.set_len(DISK_BYTES)?;
    aterm_durable::durable(&f)?;
    drop(f);
    if !crate::record::owned_plain_dir(parent) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: not this user's plain directory", parent.display()),
        ));
    }
    std::fs::rename(&staged, path)?;
    aterm_durable::durable_dir(parent)
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod tests;
