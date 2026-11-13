// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, System};

fn fake() -> Fake {
    let mut sys = Fake::root();
    sys.next_pid = 4242;
    sys
}

#[test]
fn staged_commit_transfers_once_but_child_admission_needs_an_explicit_gate_answer() {
    let sys = fake();
    let cmd = Cmd::new("/synthetic-job").args(["run-job", "1-5"]);
    let mut child = sys.stage_detached(&cmd, Path::new("/ram/job.log")).unwrap();
    assert_eq!(child.id(), 4242);
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.starts_with("stage committed"))
    );
    child.commit().unwrap();
    child.commit().unwrap();
    drop(child);
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| *call == "stage committed 4242")
            .count(),
        1
    );
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.starts_with("stage abort"))
    );
    assert!(sys.await_start(START_LIMIT).is_err());
    sys.start_answer(Ok(()));
    sys.await_start(START_LIMIT).unwrap();
    sys.start_answer(Err(RunFailed::Refused("synthetic EOF".into())));
    assert!(sys.await_start(START_LIMIT).is_err());
    assert!(sys.await_start(START_LIMIT).is_err());
}

#[test]
fn a_refused_commit_keeps_the_stage_abort_owned() {
    let sys = fake();
    sys.stage_commit_refuses(true);
    let mut child = sys
        .stage_detached(&Cmd::new("/synthetic-job"), Path::new("/ram/job.log"))
        .unwrap();
    assert!(child.commit().is_err());
    assert_eq!(child.id(), 4242);
    child.abort().unwrap();
    assert!(child.commit().is_err());
    drop(child);
    assert_eq!(
        sys.calls()
            .iter()
            .filter(|call| *call == "stage aborted 4242")
            .count(),
        1
    );
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.starts_with("stage committed") || call.starts_with("stage reaper"))
    );
}

#[test]
fn unproven_abort_preserves_custody_and_never_becomes_a_commit() {
    let sys = fake();
    sys.stage_abort_refuses(true);
    let mut child = sys
        .stage_detached(&Cmd::new("/synthetic-job"), Path::new("/ram/job.log"))
        .unwrap();
    assert!(child.abort().is_err());
    assert_eq!(child.id(), 4242);
    assert!(child.commit().is_err());
    drop(child);
    assert!(
        sys.calls()
            .iter()
            .any(|call| call == "stage abort unproven 4242")
    );
    assert!(
        sys.calls()
            .iter()
            .any(|call| call == "stage reaper custody 4242")
    );
    assert!(
        !sys.calls()
            .iter()
            .any(|call| call.starts_with("stage committed") || call == "stage aborted 4242")
    );
    let sys = Fake::root();
    assert!(
        sys.stage_detached(&Cmd::new("/synthetic-job"), Path::new("/ram/log"))
            .is_err()
    );
    let sys = fake();
    assert!(
        sys.stage_detached(&Cmd::new("relative"), Path::new("/ram/log"))
            .is_err()
    );
    let with_stdin = Cmd::new("/synthetic-job").stdin_file(Path::new("/ram/payload"));
    assert!(
        sys.stage_detached(&with_stdin, Path::new("/ram/log"))
            .is_err()
    );
    assert!(sys.calls().is_empty());
}
