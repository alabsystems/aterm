// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only result identity.

use super::{InitialSetupSnapshotError, Path, Records};
use std::fs::File;
use std::sync::Arc;

#[derive(Clone)]
struct Publication {
    _file: Arc<File>,
    device: u64,
    inode: u64,
}

impl PartialEq for Publication {
    fn eq(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

impl Eq for Publication {}

/// Pinned namespace identity only.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Publications([Option<Publication>; 3]);

pub(super) fn publications(root: &Path) -> Result<Publications, String> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let mut result = [None, None, None];
    for (slot, name) in result.iter_mut().zip([
        "automatic-plan.json",
        "initial-setup.json",
        "first-run.json",
    ]) {
        let path = root.join(name);
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let metadata = file
            .metadata()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if !metadata.file_type().is_file() {
            return Err(format!("{}: not a regular setup record", path.display()));
        }
        *slot = Some(Publication {
            device: metadata.dev(),
            inode: metadata.ino(),
            _file: Arc::new(file),
        });
    }
    Ok(Publications(result))
}

/// An opaque observation of setup records, not permission to perform setup.
#[derive(Clone, PartialEq, Eq)]
pub struct InitialSetupObservation {
    pub(super) records: Records,
    publications: Publications,
}

impl InitialSetupObservation {
    pub(super) fn publications(&self) -> Publications {
        self.publications.clone()
    }
}

impl std::fmt::Debug for InitialSetupObservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InitialSetupObservation")
            .finish_non_exhaustive()
    }
}

/// An operation's original result and the state it observed.
#[derive(Debug)]
pub struct InitialSetupObserved<T> {
    pub result: Result<T, String>,
    pub observation: Option<InitialSetupObservation>,
}

impl<T> InitialSetupObserved<T> {
    pub(in crate::macos_doctor) const fn new(
        result: Result<T, String>,
        observation: Option<InitialSetupObservation>,
    ) -> Self {
        Self {
            result,
            observation,
        }
    }
}

/// Observations taken before one says the records are changing. A write can land in the
/// middle of one: a reader that holds no profile lock (System State's status read) meets any
/// pass saving its progress, this process's own included, and one that holds it still meets a
/// Stop, which saves its request under a lock of its own. Each publishes by a rename, over in a
/// moment, so a read that met one is taken again ([`REREAD`] later) rather than handed on as a
/// change under way.
pub(super) const READS: usize = 5;

/// The pause before an observation that met a write is taken again.
const REREAD: std::time::Duration = std::time::Duration::from_millis(10);

pub(super) fn at(root: &Path) -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    at_with(root, &mut std::thread::sleep, || Records::read(root))
}

/// [`at`], reading the records with `read` and pausing with `pause`.
fn at_with(
    root: &Path,
    pause: &mut dyn FnMut(std::time::Duration),
    mut read: impl FnMut() -> Result<Records, String>,
) -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    settled(READS, pause, || with(root, &mut read))
}

/// Take `observe` until it meets no write, at most `reads` times, with `pause` between: the last
/// [`InitialSetupSnapshotError::Updating`] when every one met one. A read that failed is handed
/// on at once.
pub(super) fn settled<T>(
    reads: usize,
    pause: &mut dyn FnMut(std::time::Duration),
    mut observe: impl FnMut() -> Result<T, InitialSetupSnapshotError>,
) -> Result<T, InitialSetupSnapshotError> {
    let mut left = reads;
    loop {
        match observe() {
            Err(InitialSetupSnapshotError::Updating) if left > 1 => {
                left -= 1;
                pause(REREAD);
            }
            result => return result,
        }
    }
}

fn with(
    root: &Path,
    mut read: impl FnMut() -> Result<Records, String>,
) -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    let before = publications(root).map_err(InitialSetupSnapshotError::read)?;
    let unread = |why| InitialSetupSnapshotError::read_at(root, why);
    let records = read().map_err(unread)?;
    let confirmed = read().map_err(unread)?;
    let after = publications(root).map_err(InitialSetupSnapshotError::read)?;
    if before != after || records != confirmed {
        return Err(InitialSetupSnapshotError::Updating);
    }
    Ok(InitialSetupObservation {
        records,
        publications: before,
    })
}

#[cfg(test)]
pub(super) fn from_records(records: Records) -> InitialSetupObservation {
    InitialSetupObservation {
        records,
        publications: Publications([None, None, None]),
    }
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
