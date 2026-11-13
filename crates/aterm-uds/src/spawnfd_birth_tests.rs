// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn dedicated_group_control_requires_the_same_current_unreaped_birth() {
    let mut born = super::super::command("/bin/cat")
        .stdin(super::super::Stdio::piped())
        .stdout(super::super::Stdio::piped())
        .spawn_cohort(BirthAttempt::new().unwrap())
        .unwrap();
    let input = crate::deadline::cat_echoes(born.child_mut());
    let mut replacement = super::super::command("/bin/cat")
        .stdin(super::super::Stdio::piped())
        .spawn()
        .unwrap();
    std::mem::swap(born.child_mut(), &mut replacement);
    let replaced = born.kill_group().unwrap_err().kind();
    std::mem::swap(born.child_mut(), &mut replacement);
    let owner = born.observed.key.owner;
    born.observed.key.owner.pid = if std::process::id() == 2 { 3 } else { 2 };
    let stale = born.terminate_group().unwrap_err().kind();
    born.observed.key.owner = owner;
    born.continue_group().unwrap();
    born.kill_group().unwrap();
    drop(input);
    let status = born.child_mut().wait().unwrap();
    replacement.kill().unwrap();
    replacement.wait().unwrap();
    assert_eq!(replaced, io::ErrorKind::PermissionDenied);
    assert_eq!(stale, io::ErrorKind::PermissionDenied);
    assert!(!status.success());
    for refused in [
        born.terminate_group(),
        born.continue_group(),
        born.kill_group(),
    ] {
        assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }
}

#[test]
fn original_group_control_survives_leader_exit_and_reaches_stubborn_descendants() {
    use std::time::{Duration, Instant};
    let directory = aterm_tempfile::tempdir().unwrap();
    let leave = directory.path().join("leave");
    let member = directory.path().join("member");
    let mut born = super::super::command("/bin/sh")
        .args([
            "-c",
            "trap '' TERM; exec 3<&0; /bin/cat <&3 & printf '%s\\n' \"$!\" > \"$2\"; while test ! -f \"$1\"; do /bin/sleep 0.01; done; exit 0",
            "original-group-control",
        ])
        .arg(&leave)
        .arg(&member)
        .stdin(super::super::Stdio::piped())
        .stdout(super::super::Stdio::piped())
        .spawn_cohort(BirthAttempt::new().unwrap())
        .unwrap();
    // Actual descendant output proves both the shell and its child executed.
    let input = crate::deadline::cat_echoes(born.child_mut());
    let descendant: i32 = std::fs::read_to_string(&member)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let before = process::read_lineage(descendant).unwrap();
    let root = born.child_mut().id();
    assert_eq!(before.parent, root);
    assert_eq!(before.pgid, root);
    std::fs::write(leave, b"exit").unwrap();
    born.child_mut().wait_exited().unwrap();
    assert!(born.child_mut().owns_unreaped());
    assert_eq!(
        process::process_presence(descendant),
        process::ProcessPresence::Present
    );
    // A root-only signal cannot end the cat; the exited root still pins the
    // original group number until this actual owning reap.
    born.terminate_group().unwrap();
    born.continue_group().unwrap();
    born.kill_group().unwrap();
    let until = Instant::now() + Duration::from_secs(60);
    while process::process_presence(descendant) == process::ProcessPresence::Present
        && Instant::now() < until
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let ended = process::process_presence(descendant);
    drop(input);
    let status = born.child_mut().wait().unwrap();
    assert!(status.success(), "preserve the root's actual zero exit");
    assert!(matches!(
        ended,
        process::ProcessPresence::Exited | process::ProcessPresence::Absent
    ));
    assert_eq!(
        born.kill_group().unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
fn a_mutable_child_replacement_cannot_retarget_its_birth_receipt() {
    for dedicated in [false, true] {
        let mut command = super::super::command("/bin/cat");
        command.stdin(super::super::Stdio::piped());
        let mut observed = if dedicated {
            command
                .spawn_cohort(BirthAttempt::new().unwrap())
                .unwrap()
                .observed
        } else {
            command
                .spawn_observed(BirthAttempt::new().unwrap())
                .unwrap()
        };
        let mut replacement = super::super::command("/bin/cat")
            .stdin(super::super::Stdio::piped())
            .spawn()
            .unwrap();
        assert!(observed.is_original_child());
        std::mem::swap(observed.child_mut(), &mut replacement);
        let substituted = observed.is_original_child();
        std::mem::swap(observed.child_mut(), &mut replacement);
        let restored = observed.is_original_child();
        observed.child_mut().kill().unwrap();
        observed.child_mut().wait().unwrap();
        replacement.kill().unwrap();
        replacement.wait().unwrap();
        assert!(
            !substituted,
            "an actual different child cannot inherit the receipt"
        );
        assert!(restored, "moving the original child preserves its seal");
    }
}

#[test]
fn no_child_receipt_names_the_consumed_attempt() {
    let directory = aterm_tempfile::tempdir().unwrap();
    for dedicated in [false, true] {
        let attempt = BirthAttempt::new().unwrap();
        let key = attempt.key().clone();
        let mut command = super::super::command(directory.path().join("absent-program"));
        let outcome = if dedicated {
            command
                .spawn_cohort(attempt)
                .map(Spawned::into_child)
                .map_err(|failure| failure.into_not_spawned().unwrap())
        } else {
            command
                .spawn_observed(attempt)
                .map(ObservedSpawned::into_child)
                .map_err(|failure| failure.into_not_spawned().unwrap())
        };
        let failed = outcome.unwrap_err();
        assert_eq!(failed.key(), &key);
        assert!(key.is_current());
        assert_eq!(failed.error().kind(), io::ErrorKind::NotFound);
        let unused = BirthAttempt::new().unwrap();
        let unused_key = unused.key().clone();
        let cancelled = unused.cancel().unwrap();
        assert_eq!(cancelled.key(), &unused_key);
        assert_ne!(cancelled.key(), failed.key());
        assert_eq!(cancelled.error().kind(), io::ErrorKind::Interrupted);
    }
}

#[test]
fn a_stale_attempt_neither_spawns_nor_mints_a_no_child_receipt() {
    let mut attempt = BirthAttempt::new().unwrap();
    attempt.key.owner.pid = if std::process::id() == 2 { 3 } else { 2 };
    let key = attempt.key().clone();
    assert!(!key.is_current());
    let failure = observe(attempt, || {
        panic!("stale attempt must never invoke the spawn")
    })
    .unwrap_err();
    assert!(matches!(failure, SpawnFailure::Refused(_)));
    assert!(BirthAttempt { key }.cancel().is_err());
}

#[test]
fn a_birth_receipt_keeps_the_actual_owned_self_led_child() {
    let attempt = BirthAttempt::new().unwrap();
    let key = attempt.key().clone();
    let mut born = super::super::command("/bin/cat")
        .stdin(super::super::Stdio::piped())
        .stdout(super::super::Stdio::piped())
        .spawn_cohort(attempt)
        .unwrap();
    let input = crate::deadline::cat_echoes(born.child_mut());
    assert_eq!(born.key(), &key);
    let id = born.child_mut().id();
    let lineage = process::read_lineage(i32::try_from(id).unwrap()).unwrap();
    assert_eq!(lineage.pgid, id);
    assert_eq!(lineage.parent, std::process::id());
    drop(input);
    assert!(born.into_child().wait().unwrap().success());
}

#[test]
fn an_observed_birth_preserves_the_actual_parent_group() {
    let parent = process::read_lineage(std::process::id() as i32).unwrap();
    let attempt = BirthAttempt::new().unwrap();
    let key = attempt.key().clone();
    let mut born = super::super::command("/bin/cat")
        .stdin(super::super::Stdio::piped())
        .stdout(super::super::Stdio::piped())
        .spawn_observed(attempt)
        .unwrap();
    let input = crate::deadline::cat_echoes(born.child_mut());
    assert_eq!(born.key(), &key);
    let id = born.child_mut().id();
    let lineage = process::read_lineage(i32::try_from(id).unwrap()).unwrap();
    assert_eq!(lineage.pgid, parent.pgid);
    assert_ne!(lineage.pgid, id);
    assert_eq!(lineage.parent, std::process::id());
    drop(input);
    assert!(born.into_child().wait().unwrap().success());
}

/// The kernel process group of one piped `cat`, read while the owned child is
/// live and has already proved it ran; closing its stdin lets it exit.
fn cat_group(child: &mut Child) -> u32 {
    let input = crate::deadline::cat_echoes(child);
    let id = child.id();
    let lineage = process::read_lineage(i32::try_from(id).unwrap()).unwrap();
    assert_eq!(lineage.parent, std::process::id());
    drop(input);
    lineage.pgid
}

#[test]
fn a_cohort_spawn_leaves_its_command_as_configured() {
    let parent = process::read_lineage(std::process::id() as i32).unwrap();
    let mut command = super::super::command("/bin/cat");
    command
        .stdin(super::super::Stdio::piped())
        .stdout(super::super::Stdio::piped())
        .process_group(i32::try_from(parent.pgid).unwrap());
    let mut born = command.spawn_cohort(BirthAttempt::new().unwrap()).unwrap();
    let id = born.child_mut().id();
    assert_eq!(cat_group(born.child_mut()), id);
    assert!(born.into_child().wait().unwrap().success());
    // The consumed attempt held the dedicated group for its one spawn only:
    // the command's configured group governs later births of every kind.
    let mut observed = command
        .spawn_observed(BirthAttempt::new().unwrap())
        .unwrap();
    assert_eq!(cat_group(observed.child_mut()), parent.pgid);
    assert!(observed.into_child().wait().unwrap().success());
    let mut plain = command.spawn().unwrap();
    assert_eq!(cat_group(&mut plain), parent.pgid);
    assert!(plain.wait().unwrap().success());
    // A later cohort spawn on the same command is dedicated again.
    let mut again = command.spawn_cohort(BirthAttempt::new().unwrap()).unwrap();
    let id = again.child_mut().id();
    assert_eq!(cat_group(again.child_mut()), id);
    assert!(again.into_child().wait().unwrap().success());
}

#[test]
fn a_refused_cohort_spawn_leaves_its_command_as_configured() {
    let parent = process::read_lineage(std::process::id() as i32).unwrap();
    let mut command = super::super::command("/bin/cat");
    command
        .stdin(super::super::Stdio::piped())
        .stdout(super::super::Stdio::piped());
    let mut stale = BirthAttempt::new().unwrap();
    stale.key.owner.pid = if std::process::id() == 2 { 3 } else { 2 };
    assert!(matches!(
        command.spawn_cohort(stale),
        Err(SpawnFailure::Refused(_))
    ));
    let mut observed = command
        .spawn_observed(BirthAttempt::new().unwrap())
        .unwrap();
    assert_eq!(cat_group(observed.child_mut()), parent.pgid);
    assert!(observed.into_child().wait().unwrap().success());
}

#[test]
fn a_failed_cohort_spawn_leaves_its_command_as_configured() {
    let parent = process::read_lineage(std::process::id() as i32).unwrap();
    let directory = aterm_tempfile::tempdir().unwrap();
    let mut command = super::super::command("/bin/cat");
    command
        .stdin(super::super::Stdio::piped())
        .stdout(super::super::Stdio::piped())
        .current_dir(directory.path().join("absent"));
    let attempt = BirthAttempt::new().unwrap();
    let key = attempt.key().clone();
    let failed = command
        .spawn_cohort(attempt)
        .unwrap_err()
        .into_not_spawned()
        .unwrap();
    assert_eq!(failed.key(), &key);
    command.current_dir(directory.path());
    let mut observed = command
        .spawn_observed(BirthAttempt::new().unwrap())
        .unwrap();
    assert_eq!(cat_group(observed.child_mut()), parent.pgid);
    assert!(observed.into_child().wait().unwrap().success());
}
