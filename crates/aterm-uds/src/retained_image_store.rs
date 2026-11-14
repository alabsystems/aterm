// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable executable paths are retained for the whole kernel boot that can assess them.
//! Machine namespaces keep a shared account home from reclaiming another host's live images.

use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::private_file::{PrivateDirectory, check_private_dir, create_private_dir};

pub(crate) fn prepare(root: &Path, boot: &str) -> io::Result<PathBuf> {
    if !boot_name(boot) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    create_private_dir(root)?;
    let _ = sweep(root, boot);
    let current = root.join(boot);
    create_private_dir(&current)?;
    Ok(current)
}

pub(crate) fn boot_name(name: &str) -> bool {
    name.len() == 32
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && name.bytes().any(|b| b != b'0')
}

pub(crate) fn persistent_root(home: std::ffi::OsString, name: &str) -> io::Result<PathBuf> {
    let state = crate::per_user_default(None, Some(home))
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("persistent image storage unavailable"))?;
    // A first launch or an isolated frontend can have a HOME with no Library yet.
    // Make only missing ancestors; existing account directories keep their modes.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(
            state
                .parent()
                .ok_or_else(|| io::Error::other("image state parent missing"))?,
        )?;
    let root = crate::quiet_state::destination(&state.with_file_name(name));
    create_private_dir(
        root.parent()
            .ok_or_else(|| io::Error::other("image parent missing"))?,
    )?;
    machine_root(root)
}

pub(crate) fn machine_root(base: PathBuf) -> io::Result<PathBuf> {
    let host = crate::process::host_id()
        .ok_or_else(|| io::Error::other("image cache machine identity unavailable"))?;
    let prefix = if cfg!(target_vendor = "apple") {
        "darwin:"
    } else {
        "linux:"
    };
    let name = host
        .strip_prefix(prefix)
        .filter(|s| boot_name(s))
        .ok_or_else(|| io::Error::other("image cache machine identity malformed"))?;
    create_private_dir(&base)?;
    // Homes can be shared or copied. Another machine's current boot is never
    // evidence that a local assessment ended, and must stay outside our sweep.
    Ok(base.join(format!("host-{name}.noindex")))
}

pub(crate) fn sweep(root: &Path, current: &str) -> io::Result<()> {
    if !boot_name(current) {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let directory = PrivateDirectory::open(root)?;
    // Contention postpones reclamation, not image availability. Only this boot creates files;
    // an earlier boot has no process or assessment that can still use its directories.
    let _lock = directory.lock("gc.lock", Duration::ZERO, "image cleanup in progress")?;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == current || !boot_name(name) {
            continue;
        }
        let path = entry.path();
        if check_private_dir(&path).is_ok() && directory.is_at(root)? {
            std::fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "retained_image_store_tests.rs"]
mod tests;
