// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The cold-launch apply + rollback lane.
//!
//! Each test drives a real private entry point of `install.rs` against a scratch
//! staging root and a same-volume fixture pair. They never touch the per-user
//! ledgers (`Staging::scratch`) and never reach a real `execve`: the fixture
//! bundles carry no executable, so the re-exec at the end of a revert fails with
//! ENOENT and returns.

use std::path::Path;

use super::*;
use crate::health::Health;
use crate::manifest::FailedMark;

fn scratch(label: &str) -> crate::paths::ScratchStaging {
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

/// A crash-loop revert — the previous build swapped back over the install, the
/// new one quarantined, a re-exec — is booked in `health.toml`, so `update
/// status`, the pull-down row and the persistent-failure notice can see it.
///
/// Drives the real revert: armed sentinel at `MAX_BOOT_ATTEMPTS`, OLD at the
/// fixed rollback path, NEW installed, under the apply lock, once for each way the
/// image may be replaced. A replaceable launch's revert, and a window's that keeps its
/// image, re-exec the restored build: the fixture has no executable, so that exec fails
/// and the function returns `ReExecFailed` instead of replacing this process. A
/// LaunchServices-launched successor's revert attempts no exec at all and returns
/// `Reverted` for its caller to exit. Either way the disk ends the same.
#[test]
fn a_crash_loop_revert_is_booked_in_the_failure_ledger() {
    use crate::ImageReplacement::{LaunchedSuccessor, Replaceable, WindowKeepsImage};
    for image in [Replaceable, WindowKeepsImage, LaunchedSuccessor] {
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
        // revert still happens — a crash loop is strictly worse than a yank.
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
            RollbackHandoff {
                image,
                ..RollbackHandoff::NONE
            },
            lock,
        );

        if image == LaunchedSuccessor {
            assert!(
                matches!(outcome, ApplyOutcome::Reverted(_)),
                "a launched successor never execs the restored build: {outcome:?}"
            );
        } else {
            assert!(
                matches!(outcome, ApplyOutcome::ReExecFailed(_)),
                "the fixture has no executable, so the revert must return: {outcome:?}"
            );
        }
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

        // The revert reached the failure ledger.
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
        let _ = std::fs::remove_dir_all(&s.root);
    }
}

/// A crash-loop revert's inverse swap and the quarantine it records before the
/// rollback goes take `install_flush`'s tier (aterm-durable's counting seam), on this
/// volume and as if the install root were on another device. Those two sites are
/// every flush the revert takes through aterm-durable.
#[test]
fn a_crash_loop_reverts_swap_and_quarantine_take_the_install_tier() {
    use aterm_durable::Flush::{Durable, Ordered};
    let [here, elsewhere] = on_and_off_the_install_device(|| {
        let s = scratch("boot-revert-tier");
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
        FailedMark::record(&s.trial(), trialed, &"ab".repeat(32));
        let bundle = bundle::Bundle {
            app_root: installed.clone(),
            exe: installed.join("Contents/MacOS/aterm"),
        };
        let lock = FileLock::acquire(&s.apply_lock).unwrap();
        let restored = VerifiedRollback {
            path: rb.clone(),
            build: 1000,
        };
        let recording = aterm_durable::seam::Recording::start();
        let outcome = revert_to_rollback(
            &bundle,
            &s,
            &sentinel,
            trialed,
            restored,
            RollbackHandoff::NONE,
            lock,
        );
        let flushes = recording.take();
        assert!(
            matches!(outcome, ApplyOutcome::ReExecFailed(_)),
            "{outcome:?}"
        );
        assert_eq!(read_id(&installed), "OLD");
        let _ = std::fs::remove_dir_all(&s.root);
        flushes
    });
    assert_eq!(
        here, [Ordered; 5],
        "one device: the inverse swap, then the quarantine ledger and failed.toml, \
         file and entry each"
    );
    assert_eq!(elsewhere, [Durable; 5], "another device");
}

/// A BURST OF LAUNCHES IS ONE LAUNCH: the pure law behind
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
        let result = check_boot_health_with_lock_wait(
            &worker_staging,
            42,
            None,
            RollbackHandoff::NONE,
            false,
            crate::TrialWitness::Window,
            Duration::ZERO,
            bundle::resolve_layout,
        );
        send.send(result).unwrap();
    });
    let before_release = receive.recv_timeout(Duration::from_secs(60));
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
    let model = aterm_models_update::native_update_boot_health_lock_model();
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
    let _ = std::fs::remove_dir_all(&s.root);
}

/// Confirming a trial and forgiving a killed trial launch give up within their
/// wait on the apply lock, so a stopped sibling holding it cannot park the GUI's
/// one updater facts worker. The confirmation answers "retry later", the trial
/// stays armed, and the launch stays counted until the next holder of the lock
/// pays the give-back it is owed
/// (`a_forgive_that_meets_a_held_apply_lock_is_given_back_once_it_frees`).
#[test]
fn confirming_or_forgiving_a_trial_never_waits_forever_on_a_held_apply_lock() {
    use std::sync::mpsc;
    use std::time::Duration;
    let s = scratch("boot-confirm-lock-deadline");
    let installed = s.root.join("Applications").join("aterm.app");
    make_app(&installed, "NEW");
    let sentinel = boot_sentinel(&s);
    sentinel.arm(11).unwrap();
    let killed = LaunchName {
        pid: 4242,
        started: (1, 2),
    };
    sentinel.observe_launch_by(11, Some(killed)).unwrap();
    let holder = FileLock::acquire(&s.apply_lock).unwrap();

    let (send, receive) = mpsc::channel();
    let worker_staging = s.clone();
    let worker_installed = installed.clone();
    let worker = std::thread::spawn(move || {
        let confirmed = confirm_boot_health_at(
            &worker_staging,
            11,
            Some("abc"),
            crate::TrialWitness::Window,
            || {
                Some(bundle::Bundle {
                    exe: worker_installed.join("Contents/MacOS/aterm"),
                    app_root: worker_installed,
                })
            },
            Duration::from_millis(100),
        );
        forgive_trial_launch_of_at(
            &worker_staging,
            11,
            killed.pid,
            Some(killed.started),
            Some(&installed),
            Duration::from_millis(100),
        );
        send.send(confirmed).unwrap();
    });
    let before_release = receive.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    drop(holder);
    worker.join().unwrap();
    let confirmed = before_release
        .expect("confirm and forgive must return while another process holds the apply lock");
    assert!(!confirmed, "a timed-out confirmation is retried later");
    assert_eq!(
        sentinel.read_state(),
        Some((11, 1)),
        "the trial stays armed and the launch counted until a holder of the lock pays it"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// A give-back that meets an apply lock another aterm holds past its wait is
/// OWED, never dropped: dropped give-backs would add up to a revert of a build
/// that never failed. The parent writes the debt beside the sentinel, and the next
/// holder of the lock pays it before anything reads the count.
#[test]
fn a_forgive_that_meets_a_held_apply_lock_is_given_back_once_it_frees() {
    use std::time::Duration;
    let s = scratch("boot-owed-give-back");
    let installed = s.root.join("Applications").join("aterm.app");
    make_app(&installed, "NEW");
    let sentinel = boot_sentinel(&s);
    sentinel.arm(11).unwrap();
    let killed = LaunchName {
        pid: 4242,
        started: (1, 2),
    };
    sentinel.observe_launch_by(11, Some(killed)).unwrap();
    // A sibling's ditto or codesign, holding the lock past the give-back's wait.
    let holder = FileLock::acquire(&s.apply_lock).unwrap();
    assert!(
        forgive_trial_launch_of_at(
            &s,
            11,
            killed.pid,
            Some(killed.started),
            Some(&installed),
            Duration::from_millis(100),
        ),
        "a give-back that meets a held lock is owed, not refused"
    );
    assert_eq!(
        sentinel.read_state(),
        Some((11, 1)),
        "nothing moves the count without the lock"
    );
    drop(holder);
    // The next holder of the lock pays what is owed before it reads the count.
    {
        let _lock = FileLock::acquire(&s.apply_lock).unwrap();
        pay_owed_trial_launches(&s, &sentinel);
    }
    assert_eq!(
        sentinel.read_state(),
        Some((11, 0)),
        "the owed launch is given back once the lock frees"
    );
    // Paid once: a second pass, or the same ask again, gives back nothing more.
    sentinel.observe_launch(11).unwrap();
    {
        let _lock = FileLock::acquire(&s.apply_lock).unwrap();
        pay_owed_trial_launches(&s, &sentinel);
    }
    assert_eq!(sentinel.read_state(), Some((11, 1)));
    let _ = std::fs::remove_dir_all(&s.root);
}

/// U7's end-to-end shape: two killed candidates whose give-backs each met a held
/// apply lock, then a third candidate's boot. Before the fix the third boot
/// counted attempt `MAX_BOOT_ATTEMPTS` and the trial reverted a healthy build;
/// now its count pays the two debts first.
///
/// NEGATIVE CONTROL: two candidates that really crashed (no give-back asked) still
/// bring the third boot to the revert.
#[test]
fn owed_give_backs_are_paid_before_a_later_boot_counts() {
    use std::time::Duration;
    let backdate = |s: &Staging| {
        std::fs::File::options()
            .write(true)
            .open(s.root.join("boot.sentinel"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(700))
            .unwrap();
    };
    for crashed in [false, true] {
        let s = scratch(&format!("boot-owed-give-back-e2e-{crashed}"));
        let installed = s.root.join("Applications").join("aterm.app");
        make_app(&installed, "NEW");
        let sentinel = boot_sentinel(&s);
        sentinel.arm(11).unwrap();
        for pid in [4242, 4343] {
            let name = LaunchName {
                pid,
                started: (1, 2),
            };
            sentinel.observe_launch_by(11, Some(name)).unwrap();
            if !crashed {
                let holder = FileLock::acquire(&s.apply_lock).unwrap();
                forgive_trial_launch_of_at(
                    &s,
                    11,
                    name.pid,
                    Some(name.started),
                    Some(&installed),
                    Duration::from_millis(100),
                );
                drop(holder);
            }
            backdate(&s);
        }
        {
            let _lock = FileLock::acquire(&s.apply_lock).unwrap();
            count_trial_launch(&s, &sentinel, 11, None, true, crate::TrialWitness::Window).unwrap();
        }
        assert_eq!(
            sentinel.should_revert(11, MAX_BOOT_ATTEMPTS),
            crashed,
            "crashed={crashed}: {:?}",
            sentinel.read_state()
        );
        let _ = std::fs::remove_dir_all(&s.root);
    }
}

/// The `NativeUpdateTrialGiveBack` variables the real ledger answers for: the
/// sentinel's count, and whether the candidate's launch and the sibling's are on
/// it by name.
fn trial_give_back_projection(
    s: &Staging,
    build: u64,
    candidate: Option<TrialLauncher>,
    sibling: Option<TrialLauncher>,
) -> [(&'static str, i64); 4] {
    let named = boot_sentinel(s).named_launches(build);
    let on = |launch: Option<TrialLauncher>| {
        i64::from(launch.is_some_and(|launch| named.contains(&launch.name())))
    };
    // The candidate's launch is owed back: its debt is on disk (`owe_trial_launch`).
    let owed = candidate.is_some_and(|launch| {
        owed_give_backs_dir(s)
            .join(format!(
                "{build}-{}-{}-{}",
                launch.pid, launch.started.0, launch.started.1
            ))
            .is_file()
    });
    [
        ("count", i64::from(trial_launch_count_at(s, build))),
        ("mine", on(candidate)),
        ("theirs", on(sibling)),
        ("owed", i64::from(owed)),
    ]
}

/// OVERLAPPING LAUNCHES OF ONE TRIAL ARE GIVEN BACK BY NAME (Tier-1 of
/// `NativeUpdateTrialGiveBack`). Several aterm windows each hand off to the one
/// installed build, so a sibling window's successor of the trial boots and ends
/// beside this window's candidate, over a count that already keeps a crash. Each
/// step runs the real `count_trial_launch` and give-backs against a scratch
/// sentinel, with live stand-ins as the launchers, and the real ledger must stand
/// where the model does after every step.
///
/// * X — the sibling's give-back lands after this candidate's snapshots, and the
///   candidate, booted beside the live sibling, was never counted.
/// * Y — the sibling leaves between the parent's snapshot and the candidate's
///   boot, so the candidate IS counted, and its parent kills it after the grant.
/// * Z — no overlap: the candidate counted and both ends ask.
///
/// And the death LaunchServices reports with no pid (`SettledUnnamed`), settled by
/// the name the candidate stamped before it could count:
///
/// * A — the sibling counts after the parent's snapshot and is still alive when
///   the uncounted candidate beside it dies, then leaves by rule.
/// * P — the sibling counts after the snapshot and dies; its parent settles it
///   by name only after the counted candidate died and was settled.
/// * E — the candidate gave its own launch back on its way out, and the kept
///   crash is in the snapshot.
/// * K — the sibling counts after the snapshot, the candidate
///   boots beside it uncounted, and the sibling crashes a crash the trial keeps.
/// * I2 — a cold launch counts beside the counted candidate and crashes, and the
///   candidate gives its own launch back on its way out.
/// * L — the counted candidate dies, and a sibling that boots after it counts and
///   is gone before the candidate's parent settles.
///
/// And the settle that meets an apply lock another aterm holds past its wait
/// (`SettledOwed`), whose launch is owed
/// back:
///
/// * O — a sibling's boot after the owed settle pays the debt before it counts.
/// * Q — another give-back that takes the lock pays it (`PaysOwed`).
///
/// NEGATIVE CONTROL: the rule the names replaced — give one back when the count
/// moved since the asker's snapshot — replayed over the same steps is `Buggy=1`
/// step for step, and kept for the unnamed settle alone it is `Buggy=2`. The
/// unnamed settle's next rule — the newest launch named since the parent's
/// snapshot whose process is gone — is `Buggy=3`: it takes K's and I2's crashed
/// siblings' launches, and L's sibling's for the candidate's. The give-back the
/// debt replaced — dropped when the lock wait ran out, so O's launch stays counted
/// under the sibling's — is `Buggy=1` too (Q's `PaysOwed` is not enabled there).
#[test]
fn overlapping_launches_of_one_trial_are_given_back_by_name() {
    // `spawnfd`'s: a stand-in holds its stdio and nothing else of this process,
    // not another test thread's pipe for as long as it lives.
    use aterm_uds::spawnfd::{Child, Command};
    const BUILD: u64 = 11;
    let spawn = || -> Child {
        Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("start a stand-in launcher")
    };
    // A stand-in that must die dies by SIGKILL (AGENTS.md).
    let stop = |child: &mut Child| {
        child.kill().unwrap();
        child.wait().unwrap();
    };
    // Every step is seconds apart in the field: never a burst.
    let backdate = |s: &Staging| {
        std::fs::File::options()
            .write(true)
            .open(s.root.join("boot.sentinel"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3))
            .unwrap();
    };
    let launcher = |child: &Child| TrialLauncher::of(child.id()).expect("a running child");
    // A handoff target, or (`handoff_target = false`) a cold launch.
    let count_as = |s: &Staging, launch: TrialLauncher, handoff_target: bool| {
        count_trial_launch(
            s,
            &boot_sentinel(s),
            BUILD,
            Some(launch),
            handoff_target,
            crate::TrialWitness::Window,
        )
        .unwrap();
        backdate(s);
    };
    let count = |s: &Staging, launch: TrialLauncher| count_as(s, launch, true);
    let by_name = |s: &Staging, launch: TrialLauncher| {
        forgive_trial_launch_of_at(
            s,
            BUILD,
            launch.pid,
            Some(launch.started),
            None,
            APPLY_LOCK_WAIT,
        );
        backdate(s);
    };
    // THE RULE THIS REPLACED: one launch given back, whichever it is, when the
    // count moved since the asker's snapshot and stands above zero.
    let by_movement = |s: &Staging, snapshot: u32| {
        let now = trial_launch_count_at(s, BUILD);
        if now > 0 && now != snapshot {
            boot_sentinel(s).forgive_launch(BUILD).unwrap();
        }
        backdate(s);
    };
    // THE UNNAMED SETTLE'S NEXT RULE: the newest launch named since the parent's
    // snapshot whose process is gone, whoever made it.
    let by_newest_gone = |s: &Staging, snapshot: &[LaunchName]| {
        let gone = boot_sentinel(s)
            .named_launches(BUILD)
            .into_iter()
            .rev()
            .find(|name| {
                !snapshot.contains(name)
                    && !TrialLauncher {
                        pid: name.pid,
                        started: name.started,
                    }
                    .is_alive()
            });
        if let Some(name) = gone {
            forgive_trial_launch_of_at(
                s,
                BUILD,
                name.pid,
                Some(name.started),
                None,
                APPLY_LOCK_WAIT,
            );
        }
        backdate(s);
    };
    let traces: [(&str, &[&str]); 11] = [
        (
            "X",
            &[
                "SiblingBoots",
                "Launch",
                "Boots",
                "SiblingGivesBack",
                "ExitsByRule",
                "Settled",
            ],
        ),
        (
            "Y",
            &[
                "SiblingBoots",
                "Launch",
                "SiblingGivesBack",
                "Boots",
                "Killed",
                "Settled",
            ],
        ),
        (
            "Z",
            &[
                "Launch",
                "Boots",
                "ExitsByRule",
                "Settled",
                "SiblingBoots",
                "SiblingGivesBack",
            ],
        ),
        (
            "A",
            &[
                "Launch",
                "SiblingBoots",
                "Boots",
                "Killed",
                "SettledUnnamed",
                "SiblingGivesBack",
            ],
        ),
        (
            "P",
            &[
                "Launch",
                "SiblingBoots",
                "SiblingDies",
                "Boots",
                "Killed",
                "SettledUnnamed",
                "SiblingSettled",
            ],
        ),
        ("E", &["Launch", "Boots", "ExitsByRule", "SettledUnnamed"]),
        (
            "K",
            &[
                "Launch",
                "SiblingBoots",
                "Boots",
                "SiblingCrashes",
                "Killed",
                "SettledUnnamed",
            ],
        ),
        (
            "I2",
            &[
                "Launch",
                "Boots",
                "SiblingLaunchedCold",
                "SiblingCrashes",
                "ExitsByRule",
                "SettledUnnamed",
            ],
        ),
        (
            "L",
            &[
                "Launch",
                "Boots",
                "Killed",
                "SiblingBoots",
                "SiblingDies",
                "SettledUnnamed",
                "SiblingSettled",
            ],
        ),
        (
            "O",
            &[
                "Launch",
                "Boots",
                "Killed",
                "SettledOwed",
                "SiblingBoots",
                "SiblingGivesBack",
            ],
        ),
        (
            "Q",
            &[
                "SiblingBoots",
                "Launch",
                "SiblingGivesBack",
                "Boots",
                "Killed",
                "SettledOwed",
                "PaysOwed",
            ],
        ),
    ];
    // Every trace starts from the same disk: the trial armed, and the launch it kept
    // counted, its process gone. It is made ONCE, by the real `arm` and
    // `count_trial_launch`, and each trace starts from a copy of what they wrote (the
    // sentinel and the launcher record), backdated as a count is. Sharing one dead
    // process's name is sound: the code reads a name only to match it or to ask
    // whether its process still runs, and both answers are the same for any dead
    // process. (The window-launch record a count also writes needs a trial
    // identity, which this test never records.) Measured: per-trace arming made
    // 172 of the test's 448 `F_FULLFSYNC`s and ~25% of its wall time.
    let kept = scratch("trial-give-back-kept-crash");
    boot_sentinel(&kept).arm(BUILD).unwrap();
    let mut crash = spawn();
    count(&kept, launcher(&crash));
    stop(&mut crash);
    let model = aterm_models_update::native_update_trial_give_back_model();
    for buggy in [0, 1, 2, 3] {
        let model = aterm_spec::interp::with_buggy(&model, buggy);
        let mut broken = Vec::new();
        for (label, trace) in traces {
            // Nothing pays a debt at `Buggy=1`: Q's `PaysOwed` is not enabled.
            if buggy == 1 && trace.contains(&"PaysOwed") {
                continue;
            }
            let s = scratch(&format!("trial-give-back-{label}-{buggy}"));
            for entry in std::fs::read_dir(&kept.root).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_file() {
                    std::fs::copy(entry.path(), s.root.join(entry.file_name())).unwrap();
                }
            }
            backdate(&s);
            let mut state = model.init_state();
            let (mut sibling, mut candidate) = (None::<Child>, None::<Child>);
            let (mut sibling_launch, mut candidate_launch) = (None, None);
            let (mut before, mut at_boot) = (0, 0);
            let mut names_before: Vec<LaunchName> = Vec::new();
            let stamp = s.root.join("candidate.launch");
            for &action in trace {
                assert!(
                    model.action_enabled(action, &state),
                    "{label}: {action} is not enabled at {state:?}"
                );
                match action {
                    "SiblingBoots" => {
                        let child = spawn();
                        sibling_launch = Some(launcher(&child));
                        count(&s, launcher(&child));
                        sibling = Some(child);
                    }
                    "SiblingLaunchedCold" => {
                        let child = spawn();
                        sibling_launch = Some(launcher(&child));
                        count_as(&s, launcher(&child), false);
                        sibling = Some(child);
                    }
                    // The sibling is this machine's environment: it gives its own
                    // launch back by name under either rule, or its parent does.
                    "SiblingGivesBack" => {
                        by_name(&s, sibling_launch.unwrap());
                        stop(sibling.as_mut().unwrap());
                    }
                    "SiblingDies" | "SiblingCrashes" => stop(sibling.as_mut().unwrap()),
                    "SiblingSettled" => by_name(&s, sibling_launch.unwrap()),
                    "Launch" => {
                        before = trial_launch_count_at(&s, BUILD);
                        names_before = boot_sentinel(&s).named_launches(BUILD);
                        let child = spawn();
                        candidate_launch = Some(launcher(&child));
                        candidate = Some(child);
                        // The candidate's first act, before its boot apply can
                        // count: its own name, beside the rendezvous.
                        write_trial_launch_stamp(&stamp, candidate_launch.unwrap()).unwrap();
                    }
                    "Boots" => {
                        at_boot = trial_launch_count_at(&s, BUILD);
                        count(&s, candidate_launch.unwrap());
                    }
                    "ExitsByRule" => {
                        if buggy == 1 {
                            by_movement(&s, at_boot);
                        } else {
                            by_name(&s, candidate_launch.unwrap());
                        }
                        stop(candidate.as_mut().unwrap());
                    }
                    "Killed" => stop(candidate.as_mut().unwrap()),
                    "Settled" => {
                        if buggy == 1 {
                            by_movement(&s, before);
                        } else {
                            by_name(&s, candidate_launch.unwrap());
                        }
                    }
                    // Another aterm holds the apply lock past the settle's wait.
                    // The rule the debt replaced dropped the give-back.
                    "SettledOwed" => {
                        if buggy != 1 {
                            let holder = FileLock::acquire(&s.apply_lock).unwrap();
                            forgive_trial_launch_of_at(
                                &s,
                                BUILD,
                                candidate_launch.unwrap().pid,
                                Some(candidate_launch.unwrap().started),
                                None,
                                std::time::Duration::from_millis(50),
                            );
                            drop(holder);
                        }
                        backdate(&s);
                    }
                    // The next holder of the lock that is no counted boot.
                    "PaysOwed" => {
                        let _lock = FileLock::acquire(&s.apply_lock).unwrap();
                        pay_owed_trial_launches(&s, &boot_sentinel(&s));
                    }
                    "SettledUnnamed" => match buggy {
                        0 => {
                            forgive_stamped_trial_launch_at(
                                &s,
                                BUILD,
                                &stamp,
                                None,
                                APPLY_LOCK_WAIT,
                            );
                            backdate(&s);
                        }
                        3 => by_newest_gone(&s, &names_before),
                        _ => by_movement(&s, before),
                    },
                    _ => unreachable!("{action}"),
                }
                assert!(model.fire(action, &mut state));
                // The count's movement leaves names behind, so under it only the
                // count is the ledger's; the model's `mine` is the truth it failed
                // to keep.
                for (var, real) in
                    trial_give_back_projection(&s, BUILD, candidate_launch, sibling_launch)
                {
                    if buggy == 0 || buggy == 3 || (var != "mine" && var != "owed") {
                        assert_eq!(
                            state[var], real,
                            "{label} Buggy={buggy}: `{var}` after {action}: model {state:?}"
                        );
                    }
                }
                for invariant in &model.invariants {
                    if !model.check_invariant(invariant.name, &state) {
                        broken.push((label, invariant.name));
                    }
                }
            }
            for child in [sibling.as_mut(), candidate.as_mut()].into_iter().flatten() {
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = std::fs::remove_dir_all(&s.root);
        }
        broken.sort_unstable();
        broken.dedup();
        let erased = |trace| {
            [
                (trace, "AKeptCrashStaysCounted"),
                (trace, "TheCountIsTheLaunchesOnIt"),
            ]
        };
        let mut expected: Vec<(&str, &str)> = match buggy {
            0 => Vec::new(),
            // The count's movement erases a kept crash in A, I2, K and X, and
            // keeps Y's killed candidate's launch; O's dropped give-back leaves
            // its launch under the sibling's count.
            1 => [erased("A"), erased("I2"), erased("K"), erased("X")]
                .concat()
                .into_iter()
                .chain([
                    ("O", "AnOwedLaunchIsPaidBeforeTheNextCount"),
                    ("Y", "ASettledLaunchIsOffTheCount"),
                ])
                .collect(),
            2 => [erased("A"), erased("I2"), erased("K")].concat(),
            // The newest gone name takes I2's and K's crashed siblings' launches,
            // and L's sibling's for the candidate's.
            _ => vec![
                ("I2", "AKeptCrashStaysCounted"),
                ("K", "AKeptCrashStaysCounted"),
                ("L", "ASettledLaunchIsOffTheCount"),
            ],
        };
        expected.sort_unstable();
        assert_eq!(broken, expected, "Buggy={buggy}");
    }
}

/// An earlier launcher of the trial that stays ALIVE and
/// unconfirmed (its recovery proof failing on every retry) must not shield later
/// launches that are NOT the handoff target — a new window, a sibling, a candidate
/// booting the installed image for a newer target. Each of those counts, so the
/// budget-exhausted escape is still reached and the trial cannot stay armed (and
/// every newer update deferred) for as long as that one process lives.
///
/// NEGATIVE CONTROL: the same later launches as handoff targets are not counted.
#[test]
fn a_live_unconfirmed_launcher_does_not_shield_non_target_launches() {
    use aterm_uds::spawnfd::{Child, Command};
    let spawn = || -> Child {
        Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("start a stand-in launcher")
    };
    let backdate = |s: &Staging| {
        std::fs::File::options()
            .write(true)
            .open(s.root.join("boot.sentinel"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3))
            .unwrap();
    };
    let launcher = |child: &Child| TrialLauncher::of(child.id()).expect("a running child");
    let stop = |child: &mut Child| {
        // A stand-in that must die dies by SIGKILL (AGENTS.md).
        child.kill().unwrap();
        child.wait().unwrap();
    };

    for (label, later_are_targets) in [("boot-wedged-driver", false), ("boot-wedged-ctl", true)] {
        let s = scratch(label);
        let sentinel = boot_sentinel(&s);
        sentinel.arm(11).unwrap();
        // The wedged driver: the first launch, alive for the whole test.
        let mut driver = spawn();
        count_trial_launch(
            &s,
            &sentinel,
            11,
            Some(launcher(&driver)),
            true,
            crate::TrialWitness::Window,
        )
        .unwrap();
        backdate(&s);
        let mut later: Vec<Child> = Vec::new();
        for _ in 1..MAX_BOOT_ATTEMPTS {
            let child = spawn();
            count_trial_launch(
                &s,
                &sentinel,
                11,
                Some(launcher(&child)),
                later_are_targets,
                crate::TrialWitness::Window,
            )
            .unwrap();
            backdate(&s);
            later.push(child);
        }
        let reverts = sentinel.should_revert(11, MAX_BOOT_ATTEMPTS);
        let state = sentinel.read_state();
        stop(&mut driver);
        for child in &mut later {
            stop(child);
        }
        if later_are_targets {
            assert_eq!(
                state,
                Some((11, 1)),
                "overlapping handoff targets are not counted"
            );
            assert!(!reverts);
        } else {
            assert_eq!(
                state,
                Some((11, MAX_BOOT_ATTEMPTS)),
                "every non-target launch counts beside a live unconfirmed launcher"
            );
            assert!(reverts, "the budget-exhausted escape is still reached");
        }
        let _ = std::fs::remove_dir_all(&s.root);
    }
}

/// A headless launch of a trial a window has launched is neither counted nor
/// recorded as a live launcher (whose overlap would exempt the window's next
/// candidate from the count), and its confirmation settles nothing: a healthy
/// headless boot must not confirm a build whose windows crash before their first
/// frame. CONTROL: the headless self-update lane — a trial no window has launched
/// — counts and confirms, and a window's confirmation goes on to the proof.
#[test]
fn a_headless_launch_neither_counts_nor_settles_a_trial_a_window_launched() {
    use crate::TrialWitness::{Headless, Window};
    use aterm_uds::spawnfd::{Child, Command};
    use std::time::Duration;
    let spawn = || -> Child {
        Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("start a stand-in launch")
    };
    // A stand-in that must die dies by SIGKILL (AGENTS.md).
    let stop = |child: &mut Child| {
        child.kill().unwrap();
        child.wait().unwrap();
    };
    let launcher = |child: &Child| TrialLauncher::of(child.id()).expect("a running child");

    let s = scratch("boot-headless-witness");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let sentinel = boot_sentinel(&s);
    // The window's candidate: counted, then it crashes before Commit.
    count_trial_launch(&s, &sentinel, 11, None, true, Window).unwrap();
    backdate(&s);
    assert_eq!(sentinel.read_state(), Some((11, 1)));

    // The harness's headless instance, alive and well.
    let mut harness = spawn();
    count_trial_launch(&s, &sentinel, 11, Some(launcher(&harness)), false, Headless).unwrap();
    backdate(&s);
    assert_eq!(sentinel.read_state(), Some((11, 1)), "not counted");
    assert!(
        !read_trial_launchers(&s, 11)
            .iter()
            .any(|recorded| recorded.launcher == launcher(&harness)),
        "not a launcher whose overlap could shield the window's next candidate"
    );
    // The window of the OLDER build keeps its own standing reason in the ledger's
    // one refusal slot, keyed to the build it runs.
    let standing = "a quiet moment is awaited; retried at 12:00";
    Health::record_apply_refusal(&s.health(), 10, standing);
    let status_before = std::fs::read_to_string(&s.status).ok();
    assert!(
        confirm_boot_health_at(
            &s,
            11,
            Some("abc"),
            Headless,
            bundle_at(&installed),
            Duration::from_millis(100),
        ),
        "nothing for the headless instance to retry"
    );
    assert_eq!(
        sentinel.read_state(),
        Some((11, 1)),
        "the trial stays armed"
    );
    assert!(rollback.exists(), "and the predecessor stays on disk");
    // …and the wait is SAID where `update status` reads it, with its deadline
    // (law L5) — read from the trial's own records…
    window_trial_wait_at(&s, 11, unix_now_secs()).expect("the trial waits");
    // …never written over what the older build's window keeps there: a headless
    // confirmation inside the wait must not replace it with a refusal keyed to
    // build 11, which the window would ignore.
    let ledger = Health::read(&s.health());
    assert_eq!(
        ledger.last_apply_refusal, standing,
        "the window's reason stands"
    );
    assert_eq!(ledger.last_apply_refusal_build, 10);
    assert_eq!(
        std::fs::read_to_string(&s.status).ok(),
        status_before,
        "and the status line is the window's too"
    );
    assert_eq!(
        window_trial_wait_at(&s, 12, unix_now_secs()),
        None,
        "another build's report says nothing about it"
    );

    // The window's next candidate is counted, beside the live headless instance.
    let mut candidate = spawn();
    count_trial_launch(&s, &sentinel, 11, Some(launcher(&candidate)), true, Window).unwrap();
    backdate(&s);
    assert_eq!(sentinel.read_state(), Some((11, 2)), "toward the revert");
    // CONTROL: a window's confirmation goes on to the proof (which these unsealed
    // fixtures fail, so it answers "retry"); only the headless one stopped short.
    assert!(!confirm_boot_health_at(
        &s,
        11,
        Some("abc"),
        Window,
        bundle_at(&installed),
        Duration::from_millis(100),
    ));
    stop(&mut harness);
    stop(&mut candidate);

    // CONTROL: the headless self-update lane. No window has launched this trial:
    // the headless launch counts, and its confirmation goes on to the proof (which
    // these unsealed fixtures fail: "retry"). What a proven one settles, and what
    // it leaves for the windows, is
    // `a_headless_launch_that_comes_first_leaves_the_predecessor_for_the_windows`.
    let lane = scratch("boot-headless-lane");
    let installed = lane.root.join("Applications").join("aterm.app");
    arm_window_trial(&lane, &installed, 12, &"cd".repeat(32));
    let sentinel = boot_sentinel(&lane);
    count_trial_launch(&lane, &sentinel, 12, None, false, Headless).unwrap();
    assert_eq!(sentinel.read_state(), Some((12, 1)));
    assert!(!confirm_boot_health_at(
        &lane,
        12,
        Some("abc"),
        Headless,
        bundle_at(&installed),
        Duration::from_millis(100),
    ));
    // No window is awaited: what the report says is the proof that refused it.
    let report = window_trial_wait_at(&lane, 12, unix_now_secs()).expect("the refusal is said");
    let refusal = crate::health::Health::read(&lane.health()).trial_proof_failure;
    assert!(!refusal.is_empty() && report.contains(&refusal), "{report}");
    let _ = std::fs::remove_dir_all(&s.root);
    let _ = std::fs::remove_dir_all(&lane.root);
}

/// A window's confirmation that the trial's recovery proof refuses answers only
/// "retry", and the GUI retries it on its backoff for as long as it runs: the
/// reason is said once per distinct reason, not once per try, and `update status`
/// reports it instead of asking for a window that is already open. The passing
/// proof that confirms the trial ends the report.
#[test]
fn a_window_confirmation_the_proof_refuses_is_said_once_and_asks_for_no_window() {
    use crate::TrialWitness::Window;
    use std::time::Duration;
    let s = scratch("boot-window-proof-refused");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let sentinel = boot_sentinel(&s);
    count_trial_launch(&s, &sentinel, 11, None, true, Window).unwrap();
    backdate(&s);
    let confirm = |reason: &'static str| {
        confirm_boot_health_at_with_proof(
            &s,
            11,
            Window,
            bundle_at(&installed),
            Duration::from_millis(100),
            move |_| {
                if reason.is_empty() {
                    Ok(())
                } else {
                    Err(reason.to_string())
                }
            },
        )
    };
    let said = |reason: &str| {
        crate::log_capture::take()
            .iter()
            .filter(|(_, line)| line.contains(reason))
            .count()
    };
    let _ = crate::log_capture::take();
    for reason in ["rollback refused", "receipt missing"] {
        for _ in 0..3 {
            assert!(!confirm(reason), "a refused proof is retried");
        }
        assert_eq!(said(reason), 1, "said once, not once per try");
        let report = window_trial_wait_at(&s, 11, unix_now_secs()).expect("the trial is reported");
        assert!(report.contains(reason), "{report}");
    }
    assert!(confirm(""), "a passing proof confirms the trial");
    assert_eq!(sentinel.read_state(), None);
    assert_eq!(window_trial_wait_at(&s, 11, unix_now_secs()), None);
    let _ = std::fs::remove_dir_all(&s.root);
}

/// The installed receipt `prepare`'s swap records for `build`'s artifact `digest`.
fn receipt_for(s: &Staging, build: u64, digest: &str) {
    crate::manifest::InstalledReceipt::record(
        &s.installed_receipt(),
        build,
        "0123456789abcdef0123456789abcdef01234567",
        digest,
        aterm_durable::Flush::Durable,
    )
    .unwrap();
}

/// `a_headless_launch_neither_counts_nor_settles_a_trial_a_window_launched` in
/// the OTHER ORDER: a harness starts the installed app first, its boot apply
/// swaps the new build in, and its instance confirms the trial before any window
/// has launched the build. That headless confirmation settles the headless path
/// only: the sentinel disarms (later updates are not held) and the predecessor
/// stays, with a record that the build's windows are unproven. The build's next WINDOW launch puts it back on trial: its crashes
/// count toward the revert, a headless confirmation then settles nothing, and a
/// window's confirmation settles it and deletes the predecessor.
#[test]
fn a_headless_launch_that_comes_first_leaves_the_predecessor_for_the_windows() {
    use crate::TrialWitness::{Headless, Window};
    use std::time::Duration;
    let s = scratch("boot-headless-first");
    let installed = s.root.join("Applications").join("aterm.app");
    let digest = "ab".repeat(32);
    let rollback = arm_window_trial(&s, &installed, 11, &digest);
    receipt_for(&s, 11, &digest);
    let sentinel = boot_sentinel(&s);
    let proven = |_: &Path| Ok(());

    // The harness's instance: counted (no window has launched the trial), and
    // proven at its socket bind.
    count_trial_launch(&s, &sentinel, 11, None, false, Headless).unwrap();
    assert_eq!(sentinel.read_state(), Some((11, 1)));
    assert!(confirm_boot_health_at_with_proof(
        &s,
        11,
        Headless,
        bundle_at(&installed),
        Duration::from_millis(100),
        proven,
    ));
    assert_eq!(
        sentinel.read_state(),
        None,
        "the headless path is settled: later updates are not held"
    );
    assert!(
        rollback.exists(),
        "…and the predecessor stays on disk for the windows"
    );
    assert_eq!(read_id(&rollback), "OLD");
    let record = WindowUnproven::read(&s).expect("the windows are unproven");
    assert_eq!(
        (
            record.build,
            record.digest.as_str(),
            record.install_root.as_path()
        ),
        (11, digest.as_str(), installed.as_path())
    );

    // Another headless launch changes nothing: nothing is armed, nothing is counted.
    assert!(
        check_boot_health_with_lock_wait(
            &s,
            11,
            None,
            RollbackHandoff::NONE,
            false,
            Headless,
            Duration::from_millis(100),
            bundle_at(&installed),
        )
        .is_none()
    );
    assert_eq!(sentinel.read_state(), None);

    // The window switches: its candidate (a handoff target) is the build's first
    // window launch, and puts it back on trial — counted.
    let outcome = check_boot_health_with_lock_wait(
        &s,
        11,
        None,
        RollbackHandoff::NONE,
        true,
        Window,
        Duration::from_millis(100),
        bundle_at(&installed),
    );
    assert!(
        matches!(outcome, Some(ApplyOutcome::NotApplicable)),
        "{outcome:?}"
    );
    assert_eq!(
        sentinel.read_state(),
        Some((11, 1)),
        "back on trial, counted"
    );
    assert!(
        FailedMark::read(&s.trial()).is_some_and(|trial| trial.matches(11, &digest)),
        "under its own identity"
    );
    assert!(trial_owned_by(&s, &installed));
    assert!(
        WindowUnproven::read(&s).is_none(),
        "the record became the trial"
    );
    assert!(matches!(
        trial_window_launch(&s, 11, unix_now_secs()),
        WindowLaunch::Waiting { .. }
    ));

    // It crashed before its first frame (this test process stood in for it, and is
    // no launcher once it is gone); so do the window's next two candidates, each
    // counted toward the revert.
    std::fs::write(trial_launchers_path(&s), "11\n").unwrap();
    for _ in 1..MAX_BOOT_ATTEMPTS {
        backdate(&s);
        count_trial_launch(&s, &sentinel, 11, None, true, Window).unwrap();
    }
    assert!(
        sentinel.should_revert(11, MAX_BOOT_ATTEMPTS),
        "a crash loop reverts"
    );

    // Meanwhile a headless confirmation settles nothing…
    assert!(confirm_boot_health_at_with_proof(
        &s,
        11,
        Headless,
        bundle_at(&installed),
        Duration::from_millis(100),
        proven,
    ));
    assert_eq!(sentinel.read_state(), Some((11, MAX_BOOT_ATTEMPTS)));
    assert!(rollback.exists());

    // CONTROL: …and a window's confirmation settles it and keeps the predecessor only for
    // that window.
    assert!(confirm_boot_health_at_with_proof(
        &s,
        11,
        Window,
        bundle_at(&installed),
        Duration::from_millis(100),
        proven,
    ));
    assert_eq!(sentinel.read_state(), None);
    assert!(
        kept_for_this_window(&s, &rollback),
        "a window proved it: the predecessor is no revert, only that window's bundle"
    );
    assert!(WindowUnproven::read(&s).is_none());
    assert!(!window_unproven_path(&s).exists());
    let _ = std::fs::remove_dir_all(&s.root);
}

/// What the unproven-windows record does NOT do. It re-arms
/// nothing for another install, another build or beside a ready publication (whose
/// swap supersedes the build and clears it), and it is dropped — with nothing armed
/// — once it has lapsed, or its predecessor or the receipt naming its artifact is
/// gone. A window launch that cannot take the apply lock re-arms nothing either,
/// and is not deferred for it. A fresh swap's trial clears it.
#[test]
fn a_window_re_trial_is_bounded() {
    use crate::TrialWitness::Window;
    use std::time::Duration;
    let digest = "ab".repeat(32);
    // A trial of 11 settled by a headless launch, predecessor kept.
    let settled = |label: &str| {
        let s = scratch(label);
        let installed = s.root.join("Applications").join("aterm.app");
        arm_window_trial(&s, &installed, 11, &digest);
        receipt_for(&s, 11, &digest);
        assert!(confirm_boot_health_at_with_proof(
            &s,
            11,
            crate::TrialWitness::Headless,
            bundle_at(&installed),
            Duration::from_millis(100),
            |_: &Path| Ok(()),
        ));
        assert!(WindowUnproven::read(&s).is_some());
        (s, installed)
    };
    let now = unix_now_secs();
    let wait = WINDOW_TRIAL_HEADLESS_WAIT.as_secs();

    // Another install, another build, a ready publication: nothing armed, record kept.
    let (s, installed) = settled("boot-retrial-scope");
    let sentinel = boot_sentinel(&s);
    let elsewhere = s.root.join("Other").join("aterm.app");
    assert!(!rearm_window_unproven_trial(
        &s, &sentinel, 11, &elsewhere, now
    ));
    assert!(!rearm_window_unproven_trial(
        &s, &sentinel, 12, &installed, now
    ));
    std::fs::write(&s.ready, "pending").unwrap();
    assert!(!rearm_window_unproven_trial(
        &s, &sentinel, 11, &installed, now
    ));
    std::fs::remove_file(&s.ready).unwrap();
    assert_eq!(sentinel.read_state(), None);
    assert!(
        WindowUnproven::read(&s).is_some(),
        "still the windows' record"
    );
    // A window launch that cannot take the apply lock re-arms nothing, and goes on.
    let held = FileLock::acquire(&s.apply_lock).unwrap();
    assert!(
        check_boot_health_with_lock_wait(
            &s,
            11,
            None,
            RollbackHandoff::NONE,
            true,
            Window,
            Duration::ZERO,
            bundle_at(&installed),
        )
        .is_none(),
        "not deferred: a re-trial is never worth a refusal"
    );
    drop(held);
    assert_eq!(sentinel.read_state(), None);
    // Inside the wait (the edge) it re-arms; the next swap's trial clears a record.
    assert!(rearm_window_unproven_trial(
        &s,
        &sentinel,
        11,
        &installed,
        WindowUnproven::read(&s).unwrap().at + wait - 1,
    ));
    assert_eq!(sentinel.read_state(), Some((11, 0)));
    let _ = std::fs::remove_dir_all(&s.root);

    // Lapsed (and a record the clock cannot place): dropped, nothing armed.
    for (label, at) in [
        ("boot-retrial-lapsed", now - wait),
        ("boot-retrial-ahead", now + wait),
    ] {
        let (s, installed) = settled(label);
        let sentinel = boot_sentinel(&s);
        let mut record = WindowUnproven::read(&s).unwrap();
        record.at = at;
        record.record(&s).unwrap();
        assert!(!rearm_window_unproven_trial(
            &s, &sentinel, 11, &installed, now
        ));
        assert_eq!(sentinel.read_state(), None);
        assert!(WindowUnproven::read(&s).is_none(), "{label}: dropped");
        let _ = std::fs::remove_dir_all(&s.root);
    }

    // The predecessor gone, or the receipt naming another artifact: dropped.
    let (s, installed) = settled("boot-retrial-no-predecessor");
    let sentinel = boot_sentinel(&s);
    std::fs::remove_dir_all(rollback_path(&installed)).unwrap();
    assert!(!rearm_window_unproven_trial(
        &s, &sentinel, 11, &installed, now
    ));
    assert!(WindowUnproven::read(&s).is_none());
    let _ = std::fs::remove_dir_all(&s.root);
    let (s, installed) = settled("boot-retrial-receipt");
    let sentinel = boot_sentinel(&s);
    receipt_for(&s, 11, &"ef".repeat(32));
    assert!(!rearm_window_unproven_trial(
        &s, &sentinel, 11, &installed, now
    ));
    assert!(WindowUnproven::read(&s).is_none());
    assert_eq!(sentinel.read_state(), None);
    let _ = std::fs::remove_dir_all(&s.root);

    // The re-arm takes `install_flush`'s tier (aterm-durable's counting seam): ordered
    // on this volume, durable as if the install root were on another device.
    let [here, elsewhere] = on_and_off_the_install_device(|| {
        let (s, installed) = settled("boot-retrial-tier");
        let sentinel = boot_sentinel(&s);
        let at = WindowUnproven::read(&s).unwrap().at + wait - 1;
        let recording = aterm_durable::seam::Recording::start();
        assert!(rearm_window_unproven_trial(
            &s, &sentinel, 11, &installed, at
        ));
        let flushes = recording.take();
        let _ = std::fs::remove_dir_all(&s.root);
        flushes
    });
    use aterm_durable::Flush::{Durable, Ordered};
    assert_eq!(
        here, [Ordered; 4],
        "one device: the trial identity, then the sentinel, file and entry each"
    );
    assert_eq!(
        elsewhere,
        [Ordered, Ordered, Durable, Durable],
        "another device: the trial identity keeps its own same-device guard"
    );

    // A trial armed by the next swap owns the fixed path: the record goes with it.
    let (s, installed) = settled("boot-retrial-next-swap");
    let ready = Ready {
        build_number: 12,
        version: "0.12.0".into(),
        commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
        dmg_sha256: "cd".repeat(32),
        team_id: "T".into(),
        staged_at: String::new(),
        changelog: None,
        machine_id: None,
        roster_seq: None,
        roster_confirmed_at: Some(crate::install::now_rfc3339()),
    };
    prepare_trial(&s, &ready, &installed).unwrap();
    assert!(WindowUnproven::read(&s).is_none());
    assert!(!window_unproven_path(&s).exists());
    let _ = std::fs::remove_dir_all(&s.root);
}

/// A live HEADLESS launch of the trial does not exempt a WINDOW's overlapping
/// handoff target from the count: a long-lived harness must not shield window
/// candidates that crash before their first frame. A window launch exempts a
/// window's target, and a headless target is exempted beside any live launch. A launcher line an older
/// build wrote (no fourth field) reads as a window's.
#[test]
fn a_live_headless_launch_does_not_shield_a_windows_candidate() {
    use crate::TrialWitness::{Headless, Window};
    use aterm_uds::spawnfd::{Child, Command};
    let spawn = || -> Child {
        Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("start a stand-in launch")
    };
    // A stand-in that must die dies by SIGKILL (AGENTS.md).
    let stop = |child: &mut Child| {
        child.kill().unwrap();
        child.wait().unwrap();
    };
    let launcher = |child: &Child| TrialLauncher::of(child.id()).expect("a running child");

    for (label, first, target, exempt) in [
        ("boot-shield-headless-window", Headless, Window, false),
        ("boot-shield-window-window", Window, Window, true),
        ("boot-shield-headless-headless", Headless, Headless, true),
        ("boot-shield-window-headless", Window, Headless, true),
    ] {
        let s = scratch(label);
        let sentinel = boot_sentinel(&s);
        sentinel.arm(11).unwrap();
        let mut live = spawn();
        let mut candidate = spawn();
        count_trial_launch(&s, &sentinel, 11, Some(launcher(&live)), true, first).unwrap();
        backdate(&s);
        assert_eq!(
            read_trial_launchers(&s, 11),
            vec![RecordedLauncher {
                launcher: launcher(&live),
                witness: first
            }],
            "{label}: recorded with what it proves"
        );
        count_trial_launch(&s, &sentinel, 11, Some(launcher(&candidate)), true, target).unwrap();
        let state = sentinel.read_state();
        stop(&mut live);
        stop(&mut candidate);
        assert_eq!(
            state,
            Some((11, if exempt { 1 } else { 2 })),
            "{label}: a {target:?} target beside a live {first:?} launch"
        );
        let _ = std::fs::remove_dir_all(&s.root);
    }

    // An older build's line has no fourth field: a window's launch, as it always was.
    let s = scratch("boot-shield-old-line");
    std::fs::write(
        trial_launchers_path(&s),
        "11\n4242 17 5\n4243 18 6 headless\n",
    )
    .unwrap();
    assert_eq!(
        read_trial_launchers(&s, 11)
            .iter()
            .map(|recorded| (recorded.launcher.pid, recorded.witness))
            .collect::<Vec<_>>(),
        vec![(4242, Window), (4243, Headless)]
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// A trial of `build` armed over `installed` for `digest`, as `prepare_trial`
/// arms one — the identity marker, then the sentinel — with NEW installed and OLD
/// at the fixed rollback path, which is returned.
fn arm_window_trial(s: &Staging, installed: &Path, build: u64, digest: &str) -> PathBuf {
    make_app(installed, "NEW");
    let rollback = rollback_path(installed);
    make_app(&rollback, "OLD");
    FailedMark::record_required(&s.trial(), build, digest, Some(installed)).unwrap();
    boot_sentinel(s).arm(build).unwrap();
    rollback
}

/// The sentinel's last count moved out of the launch-burst window.
fn backdate(s: &Staging) {
    std::fs::File::options()
        .write(true)
        .open(s.root.join("boot.sentinel"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3))
        .unwrap();
}

fn bundle_at(installed: &Path) -> impl FnOnce() -> Option<bundle::Bundle> + use<> {
    let installed = installed.to_path_buf();
    move || {
        Some(bundle::Bundle {
            exe: installed.join("Contents/MacOS/aterm"),
            app_root: installed,
        })
    }
}

/// THE RECORD IS THE ARMED TRIAL'S, never a build number's. A trial a window launched is reverted; the build is re-published
/// under a new digest with the same number and armed by an OLDER image, whose
/// `prepare_trial` does not know the record and leaves it. The new trial was never
/// launched by a window, so its headless launches count and settle it, as every
/// mixed-version pairing did before. And a trial that ENDS — confirmed, reverted,
/// disarmed any way — leaves no record that answers for it.
#[test]
fn a_window_launch_record_answers_only_for_the_armed_trial() {
    use crate::TrialWitness::{Headless, Window};
    let s = scratch("boot-window-record-identity");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let sentinel = boot_sentinel(&s);
    count_trial_launch(&s, &sentinel, 11, None, true, Window).unwrap();
    assert!(matches!(
        trial_window_launch(&s, 11, unix_now_secs()),
        WindowLaunch::Waiting { .. }
    ));

    // Re-published under a new digest and armed by an image that leaves the
    // record where it was (a raw re-arm: marker, then sentinel, nothing else).
    sentinel.confirm().unwrap();
    FailedMark::record_required(&s.trial(), 11, &"ef".repeat(32), Some(&installed)).unwrap();
    sentinel.arm(11).unwrap();
    assert!(
        trial_window_launch_path(&s).exists(),
        "PRECONDITION: the old trial's record is still on disk"
    );
    assert_eq!(
        trial_window_launch(&s, 11, unix_now_secs()),
        WindowLaunch::None,
        "another trial's record is no record"
    );
    count_trial_launch(&s, &sentinel, 11, None, false, Headless).unwrap();
    assert_eq!(
        sentinel.read_state(),
        Some((11, 1)),
        "a headless launch of it counts, as before"
    );

    // A trial that ends answers for nothing, even with the file left behind…
    count_trial_launch(&s, &sentinel, 11, None, true, Window).unwrap();
    assert!(matches!(
        trial_window_launch(&s, 11, unix_now_secs()),
        WindowLaunch::Waiting { .. }
    ));
    sentinel.confirm().unwrap();
    assert_eq!(
        trial_window_launch(&s, 11, unix_now_secs()),
        WindowLaunch::None,
        "a disarmed trial's record is no record"
    );
    // …and every end this crate owns removes it with the trial's identity.
    clear_trial_identity(&s);
    assert!(!trial_window_launch_path(&s).exists());
    assert!(FailedMark::read(&s.trial()).is_none());
    let _ = std::fs::remove_dir_all(&s.root);
}

/// THE WAIT FOR A WINDOW HAS A DEADLINE (law L5). A window-launched trial on a
/// machine where only harnesses run must not stay armed for ever, holding every
/// later update. After [`WINDOW_TRIAL_HEADLESS_WAIT`] with no window launch,
/// headless launches count toward it and confirm it; a window launch inside the
/// wait renews it.
#[test]
fn the_wait_for_a_window_ends() {
    use crate::TrialWitness::{Headless, Window};
    use std::time::Duration;
    let s = scratch("boot-window-wait-bound");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let sentinel = boot_sentinel(&s);
    count_trial_launch(&s, &sentinel, 11, None, true, Window).unwrap();
    backdate(&s);
    let wait = WINDOW_TRIAL_HEADLESS_WAIT.as_secs();
    let recorded_at = |s: &Staging| {
        std::fs::read_to_string(trial_window_launch_path(s))
            .unwrap()
            .split_whitespace()
            .nth(2)
            .and_then(|at| at.parse::<u64>().ok())
            .expect("the record says when")
    };
    let at = recorded_at(&s);
    assert_eq!(
        trial_window_launch(&s, 11, at + wait - 1),
        WindowLaunch::Waiting { until: at + wait }
    );
    assert_eq!(trial_window_launch(&s, 11, at + wait), WindowLaunch::Lapsed);
    assert_eq!(
        trial_window_launch(&s, 11, at.saturating_sub(wait)),
        WindowLaunch::Lapsed,
        "a record the clock cannot place holds nothing"
    );

    // Lapsed: backdate the record past the wait, and a headless launch counts…
    let stale = format!("11 {} {}\n", "ab".repeat(32), unix_now_secs() - wait - 1);
    std::fs::write(trial_window_launch_path(&s), stale).unwrap();
    count_trial_launch(&s, &sentinel, 11, None, false, Headless).unwrap();
    assert_eq!(sentinel.read_state(), Some((11, 2)), "counted again");
    // …and its confirmation goes on to the proof (these unsealed fixtures fail it,
    // so it answers "retry"), where one inside the wait stopped short.
    assert!(!confirm_boot_health_at(
        &s,
        11,
        Some("abc"),
        Headless,
        bundle_at(&installed),
        Duration::from_millis(100),
    ));
    // A window launch renews the wait.
    backdate(&s);
    count_trial_launch(&s, &sentinel, 11, None, true, Window).unwrap();
    assert!(matches!(
        trial_window_launch(&s, 11, unix_now_secs()),
        WindowLaunch::Waiting { .. }
    ));
    let _ = std::fs::remove_dir_all(&s.root);
}

/// ANOTHER update trial holding the install
/// — here build 12's, armed beside an installed bundle this launch of build 10 can
/// neither recover nor call the just-swapped shape — refuses the boot swap, and the
/// refusal is a passing MOMENT: the trial confirms, reverts or is escaped by itself,
/// and none of that is a verdict on the staged build's bytes, so a candidate that
/// meets it must not file a STRUCTURAL refusal against a healthy release.
#[test]
fn another_trial_holding_the_install_refuses_the_swap_as_a_passing_moment() {
    let s = scratch("boot-foreign-trial-passing");
    let installed = s.root.join("Applications").join("aterm.app");
    make_app(&installed, "INSTALLED");
    boot_sentinel(&s).arm(12).unwrap();

    let refusal = settle_trials_under_apply_lock(&s, &installed, 10, None)
        .expect_err("another trial holds the install");
    let ApplyOutcome::Deferred(reason) = &refusal else {
        panic!("a refusal of the swap: {refusal:?}");
    };
    assert!(
        crate::is_passing_refusal(reason),
        "a moment, never a verdict on the stage: {reason}"
    );
    assert!(
        !crate::refusal_needs_person(reason),
        "and nobody's to clear: {reason}"
    );
    assert_eq!(
        boot_sentinel(&s).read_state().map(|(build, _)| build),
        Some(12),
        "the trial is its owner's: one launch disarms nothing"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// A PREDECESSOR NO REVERT COULD RESTORE IS NO REVERT, NOT UNPROVEN HEALTH. The
/// 2026-10-16 shape: a Team-ID-pinned build swapped in over an ad hoc one, whose
/// retained bundle this build's verifier refuses for ever. Every other term of the
/// recovery proof holds (the installed trial is sealed, verified and exactly
/// receipted), so a window's confirmation settles the trial and clears the fixed
/// path, and a counted launch of a trial nobody confirmed settles it too, keeping
/// the stage for that launch to apply instead of deferring on the proof.
///
/// CONTROL: a fixed path that VERIFIES but is not a strict predecessor — what a
/// crash-cut NEW looks like — still holds the trial, and stays on disk.
#[test]
fn a_predecessor_this_build_cannot_verify_does_not_hold_its_trial() {
    use crate::TrialWitness::Window;
    use std::time::Duration;
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    type Identity = Result<(u64, String), String>;
    /// This build's verifier: the installed trial is sealed build 11 at `COMMIT`,
    /// and anything else (the fixed path) reads `fixed`.
    fn verifier(installed: &Path, fixed: Identity) -> impl Fn(&Path) -> Identity + use<> {
        let installed = installed.to_path_buf();
        move |path: &Path| {
            if path == installed.as_path() {
                Ok((11, COMMIT.to_string()))
            } else {
                fixed.clone()
            }
        }
    }
    // 0.104's verdict on the ad hoc 0.11 it replaced, as the owner's log has it.
    let ad_hoc = || -> Identity {
        Err(
            "bundle policy: codesign --verify (team-pinned requirement) failed: \
             aterm.app.rollback: valid on disk / satisfies its Designated Requirement / \
             test-requirement: code failed to satisfy specified code requirement(s)"
                .to_string(),
        )
    };
    let crash_cut_new = || -> Identity { Ok((12, COMMIT.to_string())) };
    let digest = "ab".repeat(32);
    let wait = Duration::from_millis(100);

    // A window's confirmation at its health checkpoint.
    let s = scratch("boot-no-revert-confirm");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &digest);
    receipt_for(&s, 11, &digest);
    let confirm = |fixed: Identity| {
        let verify = verifier(&installed, fixed);
        confirm_boot_health_at_with_proof(&s, 11, Window, bundle_at(&installed), wait, |root| {
            prove_current_trial(&s, root, 11, Some(COMMIT), &verify).map(|_| ())
        })
    };
    assert!(
        !confirm(crash_cut_new()),
        "control: a crash-cut NEW holds the trial"
    );
    assert_eq!(boot_sentinel(&s).read_state(), Some((11, 0)));
    assert_eq!(read_id(&rollback), "OLD");
    assert!(
        confirm(ad_hoc()),
        "a window proved the build; a predecessor it cannot verify is no revert, not \
         unproven health"
    );
    assert_eq!(boot_sentinel(&s).read_state(), None, "the trial is settled");
    assert!(
        kept_for_this_window(&s, &rollback),
        "and the predecessor is kept for the window, as any window's confirmation keeps it"
    );
    let _ = std::fs::remove_dir_all(&s.root);

    // A counted launch of a trial no window has confirmed, with a newer stage waiting.
    let s = scratch("boot-no-revert-launch");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &digest);
    receipt_for(&s, 11, &digest);
    make_app(&s.staged_app, "STAGED-NEWER");
    std::fs::write(&s.ready, "a newer stage").unwrap();
    let launch = |fixed: Identity| {
        let verify = verifier(&installed, fixed);
        check_boot_health_with_proof(
            &s,
            11,
            RollbackHandoff::NONE,
            false,
            Window,
            wait,
            bundle_at(&installed),
            |root| prove_current_trial(&s, root, 11, Some(COMMIT), &verify),
        )
    };
    let outcome = launch(crash_cut_new());
    assert!(
        matches!(&outcome, Some(ApplyOutcome::Deferred(why)) if why.contains("trial recovery proof")),
        "control: a crash-cut NEW defers the apply: {outcome:?}"
    );
    assert_eq!(
        boot_sentinel(&s).read_state().map(|(build, _)| build),
        Some(11)
    );
    assert!(rollback.exists());
    let outcome = launch(ad_hoc());
    assert!(
        outcome.is_none(),
        "the launch goes on to its boot apply: {outcome:?}"
    );
    assert_eq!(boot_sentinel(&s).read_state(), None, "the trial is settled");
    assert!(FailedMark::read(&s.trial()).is_none());
    assert!(!rollback.exists(), "the fixed path is cleared");
    assert!(
        s.ready.exists() && read_id(&s.staged_app) == "STAGED-NEWER",
        "the newer stage is kept for this launch to apply"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// Whether the predecessor at `rollback` is kept for this process, the window that
/// confirmed its trial ([`keep_settled_predecessor`]).
fn kept_for_this_window(s: &Staging, rollback: &Path) -> bool {
    rollback.exists()
        && KeptPredecessor::read(s).is_some_and(|kept| {
            kept.bundle == rollback && Some(kept.holder) == TrialLauncher::this_process()
        })
}

/// What a boot sentinel reads as when its file holds bytes no writer wrote: the
/// zeros of an unwritten inode, the case `Sentinel::write_record`'s fsyncs are
/// for.
fn corrupt_sentinel(s: &Staging) {
    std::fs::write(s.root.join("boot.sentinel"), [0u8; 16]).unwrap();
    assert_eq!(
        boot_sentinel(s).read_state(),
        None,
        "PRECONDITION: the sentinel does not read"
    );
}

/// A TRIAL WHOSE SENTINEL CANNOT BE READ IS NEVER ANSWERED "CONFIRMED" UNSETTLED
/// (ATERM-KI-1015-2, U55). The sentinel reads a corrupt file as no trial at all,
/// so the window's confirmation answered `true` — nothing to do — and was not
/// asked again, while the trial's identity, the retained predecessor and the
/// corrupt sentinel itself all stayed on disk: a healthy build that reached its
/// checkpoint left its trial unconfirmed for as long as it ran.
#[test]
fn a_window_confirmation_over_an_unreadable_sentinel_settles_the_trial() {
    use crate::TrialWitness::Window;
    let s = scratch("boot-unreadable-sentinel-confirm");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    corrupt_sentinel(&s);
    let confirmed = confirm_boot_health_at_with_proof(
        &s,
        11,
        Window,
        bundle_at(&installed),
        std::time::Duration::from_millis(100),
        |_| Ok(()),
    );
    let settled = FailedMark::read(&s.trial()).is_none()
        && kept_for_this_window(&s, &rollback)
        && !s.root.join("boot.sentinel").exists();
    assert!(
        confirmed && settled,
        "a confirmation is answered true only once the trial is settled: answered \
         {confirmed}, trial identity kept {}, predecessor kept for the window {}, \
         sentinel kept {}",
        FailedMark::read(&s.trial()).is_some(),
        kept_for_this_window(&s, &rollback),
        s.root.join("boot.sentinel").exists()
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// …BUT ONLY ITS OWN TRIAL, ONLY AFTER THE PROOF, AND ONLY FOR THE WITNESS IT IS
/// OWED TO (U55's negative controls). An unreadable sentinel beside another
/// build's trial identity is nothing of this build's: answered `true`, nothing
/// touched. A recovery proof that refuses leaves everything as it was and
/// answers `false`, to be retried, and `update status` says its reason
/// ([`window_trial_wait_at`]). A headless start of a trial a window launched
/// leaves it for a window, as it does over a sentinel that reads.
#[test]
fn an_unreadable_sentinel_is_settled_only_for_its_own_trial_after_its_proof() {
    use crate::TrialWitness::{Headless, Window};
    let s = scratch("boot-unreadable-sentinel-controls");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    count_trial_launch(&s, &boot_sentinel(&s), 11, None, true, Window).unwrap();
    corrupt_sentinel(&s);
    let untouched = |s: &Staging| {
        FailedMark::read(&s.trial()).is_some() && rollback.exists() && boot_sentinel(s).unreadable()
    };
    let wait = std::time::Duration::from_millis(100);

    assert!(
        confirm_boot_health_at_with_proof(&s, 12, Window, bundle_at(&installed), wait, |_| {
            Err("another build's trial is never proved".into())
        }),
        "another build's trial is nothing for build 12 to retry"
    );
    assert!(untouched(&s), "and nothing of it is touched");

    assert!(
        !confirm_boot_health_at_with_proof(&s, 11, Window, bundle_at(&installed), wait, |_| {
            Err("the receipt does not match".into())
        }),
        "a refused proof is retried"
    );
    assert!(untouched(&s), "and leaves the trial as it was");
    let report = window_trial_wait_at(&s, 11, unix_now_secs()).expect("the refusal is said");
    assert!(report.contains("the receipt does not match"), "{report}");

    assert!(
        confirm_boot_health_at_with_proof(&s, 11, Headless, bundle_at(&installed), wait, |_| {
            Err("a headless start never proves a window's trial".into())
        }),
        "a headless start of a window's trial has nothing to retry"
    );
    assert!(untouched(&s), "and leaves the trial for a window");

    assert!(
        confirm_boot_health_at_with_proof(&s, 11, Window, bundle_at(&installed), wait, |_| Ok(())),
        "the window's confirmation settles it"
    );
    assert!(FailedMark::read(&s.trial()).is_none() && kept_for_this_window(&s, &rollback));
    assert!(!s.root.join("boot.sentinel").exists());
    let _ = std::fs::remove_dir_all(&s.root);
}

/// …AND A CORRUPT SENTINEL NEVER COUNTS TOWARD A REVERT (U55's other half, which
/// does not hold). However many launches of the trialed build find it, the boot
/// lane counts none and reverts nothing, so no corrupt file can quarantine the
/// build that runs: a launch is counted only into a sentinel that reads.
#[test]
fn an_unreadable_sentinel_counts_no_launch_toward_a_revert() {
    use crate::TrialWitness::Window;
    let s = scratch("boot-unreadable-sentinel-count");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    corrupt_sentinel(&s);
    for launch in 0..=MAX_BOOT_ATTEMPTS {
        let outcome = check_boot_health_with_proof(
            &s,
            11,
            RollbackHandoff::NONE,
            false,
            Window,
            std::time::Duration::from_millis(100),
            bundle_at(&installed),
            |_| Err("the proof is never asked".to_string()),
        );
        assert!(outcome.is_none(), "launch {launch}: {outcome:?}");
        assert_eq!(boot_sentinel(&s).read_state(), None, "launch {launch}");
    }
    assert_eq!(read_id(&installed), "NEW", "the build that runs stays");
    assert!(rollback.exists());
    assert!(
        FailedMark::read(&s.failed()).is_none(),
        "nothing is quarantined"
    );
    assert!(
        !crate::manifest::artifact_quarantined(&s, 11, &"ab".repeat(32)),
        "nothing is quarantined"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// A SENTINEL PATH THAT HOLDS NO FILE IS NO UNREADABLE TRIAL (the review of
/// ATERM-KI-1015-2, U55). Read as "unreadable", a directory at the sentinel's
/// path counted as this build's trial: each window confirmation ran the recovery
/// proof, then failed to remove the directory, and was retried on its backoff for
/// as long as the build ran. Only a regular file that does not parse is an
/// unreadable marker; anything else is no trial, answered at once with no proof.
#[test]
fn a_sentinel_path_that_holds_no_file_runs_no_proof() {
    use crate::TrialWitness::Window;
    let s = scratch("boot-sentinel-not-a-file");
    let installed = s.root.join("Applications").join("aterm.app");
    let _rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let marker = s.root.join("boot.sentinel");
    let _ = std::fs::remove_file(&marker);
    std::fs::create_dir(&marker).unwrap();
    assert_eq!(
        boot_sentinel(&s).read_state(),
        None,
        "PRECONDITION: the path does not read"
    );
    let proved = std::cell::Cell::new(false);
    let confirmed = confirm_boot_health_at_with_proof(
        &s,
        11,
        Window,
        bundle_at(&installed),
        std::time::Duration::from_millis(100),
        |_| {
            proved.set(true);
            Ok(())
        },
    );
    assert!(
        confirmed && !proved.get(),
        "a directory is no trial: answered {confirmed}, proof run {}",
        proved.get()
    );
    let _ = std::fs::remove_dir_all(&s.root);
}
