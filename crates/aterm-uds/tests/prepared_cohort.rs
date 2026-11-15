// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Actual helper entry is held before the requested program, with native custody.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn main() {
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use aterm_uds::process::{ProcessPresence, group_presence, read_lineage};
    use aterm_uds::spawnfd::{BirthAttempt, SpawnFailure, command};

    if let Some(code) = aterm_uds::tether::entry() {
        std::process::exit(if code == std::process::ExitCode::SUCCESS {
            0
        } else {
            1
        });
    }
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if arguments
        .get(1)
        .is_some_and(|argument| argument == "requested-program" || argument == "running-program")
    {
        assert!(std::env::var_os("ATERM_TETHER_ENTRY_BARRIER").is_none());
        std::fs::write(&arguments[2], b"requested program really executed\n").unwrap();
        if arguments[1] == "running-program" {
            loop {
                std::thread::park();
            }
        }
        return;
    }
    let world = aterm_tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let marker = world.path().join("released");
    let attempt = BirthAttempt::new().unwrap();
    let key = attempt.key().clone();
    let mut prepared = command(&exe)
        .arg("requested-program")
        .arg(&marker)
        .prepare_cohort(attempt)
        .expect("actual prepared helper");
    let pid = prepared.id();
    let observed = read_lineage(pid as i32).expect("actual retained child");
    assert_eq!(observed.identity.pid, pid as i32);
    assert_eq!(observed.parent, std::process::id());
    assert_eq!(observed.pgid, pid);
    assert_eq!(prepared.key(), &key);
    assert!(
        !marker.exists(),
        "requested code must not run before release"
    );
    let (mut running, permit) = prepared.into_held();
    assert!(
        !marker.exists(),
        "broker owns the actual child before release"
    );
    let observer = permit
        .commit(&running)
        .expect("release this exact original birth");
    assert!(running.is_original_child());
    assert_eq!(running.child_mut().id(), pid);
    observer
        .observe()
        .expect("independent exec status observation");
    assert!(running.child_mut().wait().unwrap().success());
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        b"requested program really executed\n"
    );
    until_within(MUST_HAPPEN, "released cohort and watcher ended", || {
        (group_presence(pid) == ProcessPresence::Absent).then_some(())
    });

    let cancelled = world.path().join("cancelled");
    let prepared = command(&exe)
        .arg("requested-program")
        .arg(&cancelled)
        .prepare_cohort(BirthAttempt::new().unwrap())
        .unwrap();
    let mut cancelled_child = prepared.cancel();
    let cancelled_pid = cancelled_child.child_mut().id();
    assert!(
        cancelled_child.is_original_child(),
        "cancel returns real post-birth custody"
    );
    until_within(
        MUST_HAPPEN,
        "withdrawn helper exits without target exec",
        || cancelled_child.child_mut().try_wait().unwrap(),
    );
    assert!(!cancelled.exists());
    until_within(MUST_HAPPEN, "cancelled cohort and watcher ended", || {
        (group_presence(cancelled_pid) == ProcessPresence::Absent).then_some(())
    });

    let dropped = world.path().join("dropped");
    let mut prepared = command(&exe)
        .arg("requested-program")
        .arg(&dropped)
        .prepare_cohort(BirthAttempt::new().unwrap())
        .unwrap();
    let dropped_pid = prepared.id();
    drop(prepared);
    assert!(!dropped.exists());
    until_within(MUST_HAPPEN, "dropped helper and watcher ended", || {
        (group_presence(dropped_pid) == ProcessPresence::Absent).then_some(())
    });

    let rejected = world.path().join("wrong-cohort");
    let prepared = command(&exe)
        .arg("requested-program")
        .arg(&rejected)
        .prepare_cohort(BirthAttempt::new().unwrap())
        .unwrap();
    let (mut original, permit) = prepared.into_held();
    let original_pid = original.child_mut().id();
    let mut bystander = command("/bin/sleep")
        .arg("600")
        .spawn_cohort(BirthAttempt::new().unwrap())
        .unwrap();
    assert_eq!(
        permit.commit(&bystander).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    until_within(
        MUST_HAPPEN,
        "wrong-cohort permit withdraws the original helper",
        || original.child_mut().try_wait().unwrap(),
    );
    assert!(!rejected.exists());
    assert!(bystander.child_mut().try_wait().unwrap().is_none());
    bystander.kill_group().unwrap();
    let bystander_pid = bystander.child_mut().id();
    bystander.child_mut().wait().unwrap();
    until_within(MUST_HAPPEN, "rejected original cohort ended", || {
        (group_presence(original_pid) == ProcessPresence::Absent).then_some(())
    });
    until_within(MUST_HAPPEN, "test bystander ended", || {
        (group_presence(bystander_pid) == ProcessPresence::Absent).then_some(())
    });

    let stopped = world.path().join("stopped-before-entry");
    let prepared = command(&exe)
        .arg("requested-program")
        .arg(&stopped)
        .prepare_cohort(BirthAttempt::new().unwrap())
        .unwrap();
    let (mut stopped_child, permit) = prepared.into_held();
    let stopped_pid = stopped_child.child_mut().id();
    stopped_child.kill_group().unwrap();
    stopped_child.child_mut().wait().unwrap();
    assert_eq!(
        permit.commit(&stopped_child).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(!stopped.exists());
    until_within(MUST_HAPPEN, "stopped prepared cohort ended", || {
        (group_presence(stopped_pid) == ProcessPresence::Absent).then_some(())
    });

    let live = world.path().join("controlled-with-observer-pending");
    let prepared = command(&exe)
        .arg("running-program")
        .arg(&live)
        .prepare_cohort(BirthAttempt::new().unwrap())
        .unwrap();
    let (mut controlled, permit) = prepared.into_held();
    let observer = permit.commit(&controlled).unwrap();
    until_within(
        MUST_HAPPEN,
        "actual target executed before root stop",
        || live.exists().then_some(()),
    );
    let controlled_pid = controlled.child_mut().id();
    controlled.kill_group().unwrap();
    assert!(!controlled.child_mut().wait().unwrap().success());
    observer.observe().unwrap();
    until_within(MUST_HAPPEN, "root controlled cohort ended", || {
        (group_presence(controlled_pid) == ProcessPresence::Absent).then_some(())
    });

    let prepared = command("/aterm-intentionally-absent/prepared-tool")
        .prepare_cohort(BirthAttempt::new().unwrap())
        .unwrap();
    match prepared
        .release()
        .expect_err("target exec must fail after real helper birth")
    {
        SpawnFailure::Unbound { mut spawned, error } => {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            assert!(spawned.is_original_child());
            let pid = spawned.child_mut().id();
            spawned.child_mut().wait().unwrap();
            until_within(MUST_HAPPEN, "failed-exec helper and watcher ended", || {
                (group_presence(pid) == ProcessPresence::Absent).then_some(())
            });
        }
        other => panic!("real helper birth was discarded: {other:?}"),
    }
    println!(
        "prepared native cohorts: held, released, cancelled, dropped and failed-exec custody passed"
    );
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() {}
