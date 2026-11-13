// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn access_record_an_unreadable_history_is_preserved_until_it_can_be_read() {
    let dir = crate::test_scratch_dir("access");
    let root = dir.path();
    let revision = 8_176;
    append(
        root,
        revision,
        Access::DeveloperTools,
        AccessOutcome::Declined,
    )
    .unwrap();
    append(
        root,
        revision + 1,
        Access::FullDiskAccess,
        AccessOutcome::Granted,
    )
    .unwrap();
    let path = root.join(FILE);
    let readable = std::fs::read(&path).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&[0xff])
        .unwrap();
    let unreadable = std::fs::read(&path).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );

    let result = append(root, revision, Access::Accessibility, AccessOutcome::Handed);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        unreadable,
        "a failed history read must preserve the earlier answers"
    );
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);

    // Repair this test's damaged input, then the same append can succeed
    // while retaining both earlier outcomes and their separate revisions.
    std::fs::write(&path, readable).unwrap();
    append(root, revision, Access::Accessibility, AccessOutcome::Handed).unwrap();
    assert_eq!(
        read(root, revision),
        [
            (Access::DeveloperTools, AccessOutcome::Declined),
            (Access::Accessibility, AccessOutcome::Handed),
        ]
    );
    assert_eq!(
        latest(root, revision + 1, Access::FullDiskAccess),
        Some(AccessOutcome::Granted)
    );
}

#[test]
fn access_record_write_failure_releases_its_lock_and_removes_its_staging() {
    let dir = crate::test_scratch_dir("access");
    let root = dir.path();
    assert!(
        append_with(
            root,
            8_175,
            Access::Debugger,
            AccessOutcome::Granted,
            || {
                std::fs::create_dir(root.join(FILE)).unwrap();
            }
        )
        .is_err()
    );
    let remaining: Vec<_> = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        remaining.len(),
        2,
        "a failed publication kept private staging"
    );
    assert!(remaining.contains(&std::ffi::OsString::from(FILE)));
    assert!(remaining.contains(&std::ffi::OsString::from(LOCK_FILE)));
    let probe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(LOCK_FILE))
        .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "an independent handle verifies the failed publication released its writer lock"
    )]
    probe.try_lock().unwrap();
    probe.unlock().unwrap();
    std::fs::remove_dir(root.join(FILE)).unwrap();
    append(root, 8_175, Access::Debugger, AccessOutcome::Granted).unwrap();
    assert_eq!(
        latest(root, 8_175, Access::Debugger),
        Some(AccessOutcome::Granted)
    );
}

#[test]
fn access_record_concurrent_writers_keep_independent_outcomes_and_the_latest_decline() {
    let dir = crate::test_scratch_dir("access");
    let root = dir.path();
    let revision = 8_173;
    let other_revision = revision + 1;
    append(
        root,
        revision,
        Access::FullDiskAccess,
        AccessOutcome::Handed,
    )
    .unwrap();
    append(
        root,
        other_revision,
        Access::FullDiskAccess,
        AccessOutcome::Granted,
    )
    .unwrap();
    let (ready, read_ready) = mpsc::sync_channel(1);
    let (release, resume) = mpsc::sync_channel(1);
    let (first_done, first_result) = mpsc::sync_channel(1);
    let first_root = root.to_path_buf();
    let first = std::thread::spawn(move || {
        let result = append_with(
            &first_root,
            revision,
            Access::Debugger,
            AccessOutcome::Granted,
            || {
                // Coordinate the old unlocked path and the corrected locked path
                // without waiting for a writer held behind our own lock.
                let probe = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(first_root.join(LOCK_FILE))
                    .unwrap();
                #[expect(
                    clippy::disallowed_methods,
                    reason = "an independently opened handle samples writer ownership only to schedule this regression"
                )]
                let unlocked = match probe.try_lock() {
                    Ok(()) => {
                        probe.unlock().unwrap();
                        true
                    }
                    Err(std::fs::TryLockError::WouldBlock) => false,
                    Err(std::fs::TryLockError::Error(error)) => panic!("{error}"),
                };
                ready.send(unlocked).unwrap();
                resume.recv_timeout(Duration::from_secs(60)).unwrap();
            },
        );
        first_done.send(result).unwrap();
    });
    let unlocked = read_ready.recv_timeout(Duration::from_secs(60)).unwrap();
    let (second_done, second_result) = mpsc::sync_channel(1);
    let second_root = root.to_path_buf();
    let second = std::thread::spawn(move || {
        second_done
            .send(append(
                &second_root,
                revision,
                Access::FullDiskAccess,
                AccessOutcome::Declined,
            ))
            .unwrap();
    });
    let result = if unlocked {
        let result = second_result.recv_timeout(Duration::from_secs(60)).unwrap();
        release.send(()).unwrap();
        result
    } else {
        release.send(()).unwrap();
        second_result.recv_timeout(Duration::from_secs(60)).unwrap()
    };
    result.unwrap();
    first_result
        .recv_timeout(Duration::from_secs(60))
        .unwrap()
        .unwrap();
    first.join().unwrap();
    second.join().unwrap();

    let rows = read(root, revision);
    assert!(rows.contains(&(Access::Debugger, AccessOutcome::Granted)));
    assert!(rows.contains(&(Access::FullDiskAccess, AccessOutcome::Handed)));
    assert!(
        rows.contains(&(Access::FullDiskAccess, AccessOutcome::Declined)),
        "an independent append lost the newer Not now: {rows:?}"
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(
        latest(root, revision, Access::FullDiskAccess),
        Some(AccessOutcome::Declined)
    );
    assert_eq!(
        read(root, other_revision),
        [(Access::FullDiskAccess, AccessOutcome::Granted)]
    );
}

/// The latest of any revision is the row's last line in any revision, revision 0 (an older
/// aterm's legacy run) included, past every line this build does not know: an older
/// aterm's `local-network` row, an older aterm's `triggered` outcome, an unknown outcome,
/// extra fields, an unparsed revision.
#[test]
fn access_record_the_latest_of_any_revision_reads_every_revision_and_skips_unknown_lines() {
    let dir = crate::test_scratch_dir("access");
    let root = dir.path();
    assert_eq!(latest_of_any_revision(root, Access::Debugger), None);
    append(root, 0, Access::Debugger, AccessOutcome::Handed).unwrap();
    assert_eq!(
        latest_of_any_revision(root, Access::Debugger),
        Some(AccessOutcome::Handed)
    );
    std::fs::write(
        root.join(FILE),
        "0 debugger triggered\n\
         7 debugger granted\n\
         9 full-disk-access handed\n\
         10 local-network granted 5454545454545454545454545454545454545454545454545454545454545454\n\
         11 local-network triggered\n\
         12 debugger asked-twice\n\
         13 bluetooth triggered\n\
         14 debugger declined later\n\
         not-a-revision debugger declined\n",
    )
    .unwrap();
    assert_eq!(
        latest_of_any_revision(root, Access::Debugger),
        Some(AccessOutcome::Granted),
        "an older revision's line, past another row's and every unknown or newer line"
    );
    assert_eq!(
        latest_of_any_revision(root, Access::FullDiskAccess),
        Some(AccessOutcome::Handed)
    );
    assert_eq!(
        read(root, 10),
        [],
        "an older aterm's local-network row is skipped"
    );
    assert_eq!(latest_of_any_revision(root, Access::Accessibility), None);
    append(root, 3, Access::Debugger, AccessOutcome::Handed).unwrap();
    assert_eq!(
        latest_of_any_revision(root, Access::Debugger),
        Some(AccessOutcome::Handed),
        "the file's last line for the row, whatever its revision"
    );
}
