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

    /// A read that met a write is taken again, and the one after the write is the observation:
    /// the same records a read with no write gives.
    #[test]
    fn an_observation_that_met_a_write_is_taken_again() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        let original = progress(root.path()).unwrap().unwrap();
        let mut running = original;
        running.running = Some(Stage::Secrets);
        let (mut writes, mut pauses) = (0, Vec::new());
        let observed = at_with(root.path(), &mut |pause| pauses.push(pause), || {
            let records = Records::read(root.path())?;
            if writes == 0 {
                writes += 1;
                let _writer = HeldLock::take(root.path())?;
                save_progress(root.path(), running)?;
            }
            Ok(records)
        })
        .unwrap();
        assert_eq!((writes, pauses), (1, vec![REREAD]));
        assert!(observed == at(root.path()).unwrap());
        assert_eq!(observed.records.progress, Some(running));
    }

    /// Records that keep changing say so after [`READS`] observations; a read that failed is not
    /// taken again.
    #[test]
    fn records_that_keep_changing_say_so_and_a_failed_read_is_not_taken_again() {
        let mut taken = 0;
        let mut pauses = 0;
        let result: Result<(), _> = settled(READS, &mut |_| pauses += 1, || {
            taken += 1;
            Err(InitialSetupSnapshotError::Updating)
        });
        assert!(matches!(result, Err(InitialSetupSnapshotError::Updating)));
        assert_eq!((taken, pauses), (READS, READS - 1));
        let mut taken = 0;
        let result: Result<(), _> = settled(READS, &mut |_| panic!("no pause"), || {
            taken += 1;
            Err(InitialSetupSnapshotError::Read("unreadable".into()))
        });
        assert!(matches!(result, Err(InitialSetupSnapshotError::Read(_))));
        assert_eq!(taken, 1);
    }
}
