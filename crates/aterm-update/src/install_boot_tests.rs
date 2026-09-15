// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Boot-apply audit tests (2026-09-14, the cold-launch apply + rollback lane).
//!
//! These pin behaviour the cold-launch lane does NOT have yet. Each one drives a
//! real private entry point of `install.rs` against a scratch staging root and a
//! same-volume fixture pair, exactly like the disk-transaction tests in
//! `install.rs`'s own module, and is RED on purpose until the finding it names is
//! fixed. They never touch the per-user ledgers (`Staging::scratch`) and never
//! reach a real `execve`: the fixture bundles carry no executable, so the
//! re-exec at the end of a revert fails with ENOENT and returns.

use std::path::Path;

use super::*;
use crate::health::Health;
use crate::manifest::FailedMark;

fn scratch(label: &str) -> Staging {
    let s = Staging::scratch(label);
    std::fs::create_dir_all(s.staged_dir()).unwrap();
    s
}

fn make_app(dir: &Path, id: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("id"), id).unwrap();
}

fn read_id(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("id")).unwrap()
}

/// FINDING: a crash-loop revert is the loudest thing the boot lane can do — it
/// swaps the previous build back over the install, quarantines the new one, and
/// re-execs — and it never reaches `health.toml`. `revert_to_rollback` writes a
/// `status.toml` line (overwritten by the next check within the interval) and the
/// `failed.toml` quarantine, but not `Health::record_apply_failure`; the same is
/// true of the exec-failure rollback at the end of `apply_staged_if_ready_inner`
/// (`ApplyOutcome::ReExecFailed`, which the wrapper deliberately does not record
/// as a refusal and nothing records as a failure). So `apply_failures` stays 0,
/// `aterm-ctl update status` reports `failing=0`, the pull-down never shows the
/// failing row, and the persistent-failure notification cannot fire — for the
/// one outcome that means "the update landed and was taken back".
///
/// Drives the real revert: armed sentinel at `MAX_BOOT_ATTEMPTS`, OLD at the
/// fixed rollback path, NEW installed, under the apply lock. The fixture has no
/// executable, so the trailing re-exec fails and the function returns
/// `ReExecFailed` instead of replacing this process.
#[test]
fn a_crash_loop_revert_is_booked_in_the_failure_ledger() {
    let s = scratch("boot-revert-ledger");
    let installed = s.root.join("Applications").join("aterm.app");
    make_app(&installed, "NEW");
    let rb = rollback_path(&installed);
    make_app(&rb, "OLD");
    let trialed = 2000_u64;
    let sentinel = boot_sentinel(&s);
    sentinel.arm(trialed).unwrap();
    for _ in 0..MAX_BOOT_ATTEMPTS {
        sentinel.observe_launch(trialed).unwrap();
    }
    assert!(sentinel.should_revert(trialed, MAX_BOOT_ATTEMPTS));
    FailedMark::record(&s.trial(), trialed, &"ab".repeat(32));
    let bundle = bundle::Bundle {
        app_root: installed.clone(),
        exe: installed.join("Contents/MacOS/aterm"),
    };
    let lock = FileLock::acquire(&s.apply_lock).unwrap();

    // The operator floor sits ABOVE the restored build (a yanked predecessor): the
    // revert still happens — a crash loop is strictly worse than a yank — and every
    // surface names the breach and the remedy (2026-09-14, audit BA-4).
    crate::manifest::Floor::bump_and_write(&s.floor(), 1500, 0, 0);
    assert_eq!(crate::manifest::Floor::read(&s.floor()).min_build, 1500);
    let restored = VerifiedRollback {
        path: rb.clone(),
        build: 1000,
    };

    let outcome = revert_to_rollback(
        &bundle,
        &s,
        &sentinel,
        trialed,
        restored,
        RollbackHandoff { fds: &[], env: &[] },
        lock,
    );

    // The physical revert itself is shipping behaviour and passes today.
    assert!(
        matches!(outcome, ApplyOutcome::ReExecFailed(_)),
        "the fixture has no executable, so the revert must return: {outcome:?}"
    );
    assert_eq!(
        read_id(&installed),
        "OLD",
        "the previous build is back at the install"
    );
    assert!(sentinel.read_state().is_none(), "the trial is disarmed");
    assert!(
        FailedMark::read(&s.failed()).is_some(),
        "the crash-looping build is quarantined"
    );

    // THE GAP: nothing above reached the failure ledger.
    let health = Health::read(&s.health());
    assert!(
        health.apply_failures >= 1,
        "a crash-loop revert is an apply that failed end to end and must advance the \
         apply streak so `update status`, the pull-down row and the persistent notice \
         can see it; health.toml has apply_failures={} kind={:?} last_apply_error={:?}",
        health.apply_failures,
        health.kind,
        health.last_apply_error
    );
    assert_eq!(health.kind, "apply");
    assert_eq!(health.last_apply_failure_build, trialed);
    assert!(
        health.last_apply_error.contains("revert"),
        "the ledger names what happened: {:?}",
        health.last_apply_error
    );
    assert!(
        health.last_apply_error.contains("reverted to build 1000")
            && health
                .last_apply_error
                .contains("below the operator floor 1500")
            && health.last_apply_error.contains("reinstall"),
        "a restored build below the floor is said out loud, with the remedy: {:?}",
        health.last_apply_error
    );
    let status = std::fs::read_to_string(&s.status).unwrap_or_default();
    assert!(
        status.contains("previous build 1000") && status.contains("below the operator floor 1500"),
        "the status line names the breach too: {status}"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// A BURST OF LAUNCHES IS ONE LAUNCH (2026-09-14, audit BA-3): the pure law behind
/// `check_boot_health`'s count. The arm itself (attempts 0) is never a burst — the
/// first launch always counts; an observed trial rewritten inside the window is;
/// one older than the window is a relaunch and counts; and a sentinel whose age
/// cannot be read counts (fail toward counting, never toward silence).
#[test]
fn a_launch_inside_the_burst_window_of_a_counted_one_is_not_counted_again() {
    use std::time::Duration;
    assert!(
        !launch_is_burst(0, Some(Duration::from_millis(5))),
        "the arm's first launch counts"
    );
    assert!(
        launch_is_burst(1, Some(Duration::from_millis(50))),
        "50 ms after a count: burst"
    );
    assert!(launch_is_burst(
        2,
        Some(BOOT_LAUNCH_BURST_WINDOW - Duration::from_millis(1))
    ));
    assert!(
        !launch_is_burst(1, Some(BOOT_LAUNCH_BURST_WINDOW)),
        "at the window: a relaunch"
    );
    assert!(
        !launch_is_burst(1, Some(Duration::from_secs(30))),
        "a user's relaunch counts"
    );
    assert!(
        !launch_is_burst(1, None),
        "an unreadable age counts the launch"
    );
}

#[test]
fn an_armed_boot_health_check_times_out_without_counting_a_launch() {
    use std::sync::mpsc;
    use std::time::Duration;
    let s = scratch("boot-health-lock-deadline");
    let sentinel = boot_sentinel(&s);
    sentinel.arm(42).unwrap();
    let holder = FileLock::acquire(&s.apply_lock).unwrap();
    let worker_staging = s.clone();
    let (send, receive) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result =
            check_boot_health_with_lock_wait(&worker_staging, 42, None, &[], &[], Duration::ZERO);
        send.send(result).unwrap();
    });
    let before_release = receive.recv_timeout(Duration::from_secs(2));
    // Always release and join before asserting, including the historical unbounded case.
    drop(holder);
    worker.join().unwrap();
    let result =
        before_release.expect("startup must return while the other process still holds the lock");
    assert!(
        matches!(result, Some(ApplyOutcome::Deferred(ref reason)) if reason.contains("health lock")),
        "{result:?}"
    );
    assert_eq!(
        sentinel.read_state(),
        Some((42, 0)),
        "a timed-out observation counts no launch"
    );
    let model = aterm_spec::derive::native_update_boot_health_lock_model();
    let mut state = model.init_state();
    state.insert("expired", 1);
    state.insert("returned", 1);
    state.insert("counted", i64::from(sentinel.read_state().unwrap().1));
    assert!(model.check_invariant("LaunchDoesNotAwaitHeldLock", &state));

    // Replay the former primitive under the same real held file lock. It cannot
    // complete before release; the resulting state violates the bounded-launch law.
    let holder = FileLock::acquire(&s.apply_lock).unwrap();
    let lock_path = s.apply_lock.clone();
    let (send, receive) = mpsc::channel();
    let old = std::thread::spawn(move || {
        let result = FileLock::acquire(&lock_path);
        send.send(result.is_ok()).unwrap();
    });
    let old_completed = receive.recv_timeout(Duration::from_millis(25)).is_ok();
    drop(holder);
    old.join().unwrap();
    assert!(!old_completed);
    state.insert("returned", i64::from(old_completed));
    assert!(!model.check_invariant("LaunchDoesNotAwaitHeldLock", &state));
    let _ = std::fs::remove_dir_all(s.root);
}
