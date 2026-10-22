// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only result identity. Open descriptors pin atomic publications so a
//! record that changes and returns to the same contents cannot revive old rows.

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

/// Pinned namespace identity only. This neither reads records nor proves that
/// a failed writer flushed them; recovery must compare a later strict observation.
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
/// Cloning retains the publications against inode reuse until cached rows expire.
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

/// An operation's original result and the state it observed. An absent
/// observation must never be attached to a later state by the caller.
#[derive(Debug)]
pub struct InitialSetupObserved<T> {
    pub result: Result<T, String>,
    pub observation: Option<InitialSetupObservation>,
}

impl<T> InitialSetupObserved<T> {
    pub(in crate::macos_doctor) fn new(
        result: Result<T, String>,
        observation: Option<InitialSetupObservation>,
    ) -> Self {
        Self {
            result,
            observation,
        }
    }
}

pub(super) fn at(root: &Path) -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    with(root, || Records::read(root))
}

fn with(
    root: &Path,
    mut read: impl FnMut() -> Result<Records, String>,
) -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    let before = publications(root).map_err(InitialSetupSnapshotError::Read)?;
    let records = read().map_err(InitialSetupSnapshotError::Read)?;
    let confirmed = read().map_err(InitialSetupSnapshotError::Read)?;
    let after = publications(root).map_err(InitialSetupSnapshotError::Read)?;
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
mod tests {
    use super::super::{HeldLock, Plan, Stage, authorize_at, progress, save_progress};
    use super::*;

    #[test]
    fn observation_double_collect_refuses_identical_content_publication_aba() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        let original = progress(root.path()).unwrap().unwrap();
        let mut first = true;
        let result = with(root.path(), || {
            let records = Records::read(root.path())?;
            if first {
                first = false;
                let _writer = HeldLock::take(root.path())?;
                let mut running = original;
                running.running = Some(Stage::Secrets);
                save_progress(root.path(), running)?;
                save_progress(root.path(), original)?;
            }
            Ok(records)
        });
        assert!(matches!(result, Err(InitialSetupSnapshotError::Updating)));
        assert_eq!(progress(root.path()).unwrap().unwrap(), original);
    }
}
