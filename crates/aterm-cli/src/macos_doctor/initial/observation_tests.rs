// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
