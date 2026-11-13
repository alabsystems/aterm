// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The cold-launch apply + rollback lane: real private entry points of `install.rs`
//! against a scratch staging root and same-volume fixtures.
use std::path::Path;

use super::*;
use crate::health::Health;
use crate::manifest::Trial;
use aterm_update_core::update_record::{self, Cause, Witness};

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
        Trial::record(&s.trial(), trialed, &"ab".repeat(32));
        let bundle = bundle::Bundle {
            app_root: installed.clone(),
            exe: installed.join("Contents/MacOS/aterm"),
        };
        let lock = FileLock::acquire(&s.apply_lock).unwrap();

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
        let record = crate::hold::load(&s);
        let hold = record
            .hold
            .as_ref()
            .expect("the crash-looping build is held");
        assert_eq!((hold.build, hold.cause), (trialed, Cause::CrashLoop));
        assert_eq!(
            hold.witness,
            Witness::Headless,
            "no window launched the trial"
        );
        assert_eq!(hold.why, update_record::CRASH_WHY, "no miss gave a reason");
        assert_eq!(hold.tries, 1);
        assert_eq!(
            hold.until - hold.at,
            update_record::CRASH_SPANS[0],
            "a first headless crash loop holds for an hour"
        );
        assert!(record.trial.is_none(), "the trial is over");
        assert_eq!(record.running, 1000, "the restored build runs");
        let _ = std::fs::remove_dir_all(&s.root);
    }
}

/// The node service's miss, noted before its run ends (`note_trial_miss`), is kept in the update
/// record with its reason, and the crash-loop revert's hold carries that reason instead of the
/// bare launch count; a miss of a build no trial is armed for notes nothing.
/// NEGATIVE CONTROL: with `note_trial_miss` not calling `record.note_miss`, nothing is noted
/// (`` the miss is noted ``).
#[test]
fn a_noted_trial_miss_gives_the_reverts_hold_its_reason() {
    let s = scratch("boot-node-miss");
    let trialed = 2000_u64;
    let why = "the node service could not prove its listener: no reply";
    Trial::record(&s.trial(), trialed, &"ab".repeat(32));
    assert!(!note_trial_miss(&s, 1999, why), "no trial of that build");
    assert!(note_trial_miss(&s, trialed, why), "the miss is noted");
    let trial = crate::hold::load(&s)
        .trial
        .expect("the trial is in the record");
    assert_eq!(
        (trial.build, trial.misses, trial.why.as_str()),
        (trialed, 1, why)
    );
    let hold = hold_reverted_trial(&s, trialed, 1000).expect("the revert holds the build");
    assert_eq!((hold.cause, hold.why.as_str()), (Cause::CrashLoop, why));
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_crash_loop_reverts_swap_takes_the_install_tier_and_its_hold_is_durable() {
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
        Trial::record(&s.trial(), trialed, &"ab".repeat(32));
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
        here,
        [Ordered, Durable, Durable],
        "one device: the inverse swap, then the update record, file and folder"
    );
    assert_eq!(elsewhere, [Durable; 3], "another device");
}

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
        let confirmed = confirm_boot_health_verdict_at(
            &worker_staging,
            11,
            Some("abc"),
            crate::TrialWitness::Window,
            true,
            ProbeClock::now(),
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
    assert!(
        matches!(&confirmed, crate::BootHealthVerdict::Unanswered { why } if why.contains("health lock")),
        "a timed-out confirmation could not be asked, and is asked again: {confirmed:?}"
    );
    assert_eq!(
        trial_refusal_count(&s),
        0,
        "a lock that timed out refused no proof"
    );
    assert_eq!(
        sentinel.read_state(),
        Some((11, 1)),
        "the trial stays armed and the launch counted until a holder of the lock pays it"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

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
    {
        let _lock = FileLock::acquire(&s.apply_lock).unwrap();
        pay_owed_trial_launches(&s, &sentinel);
    }
    assert_eq!(
        sentinel.read_state(),
        Some((11, 0)),
        "the owed launch is given back once the lock frees"
    );
    sentinel.observe_launch(11).unwrap();
    {
        let _lock = FileLock::acquire(&s.apply_lock).unwrap();
        pay_owed_trial_launches(&s, &sentinel);
    }
    assert_eq!(sentinel.read_state(), Some((11, 1)));
    let _ = std::fs::remove_dir_all(&s.root);
}

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
            count_trial_launch(
                &s,
                &sentinel,
                11,
                None,
                true,
                crate::TrialWitness::Window,
                false,
            )
            .unwrap();
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

#[test]
fn overlapping_launches_of_one_trial_are_given_back_by_name() {
    use aterm_uds::spawnfd::{Child, Command};
    const BUILD: u64 = 11;
    let spawn = || -> Child {
        Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("start a stand-in launcher")
    };
    let stop = |child: &mut Child| {
        child.kill().unwrap();
        child.wait().unwrap();
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
    let count_as = |s: &Staging, launch: TrialLauncher, handoff_target: bool| {
        count_trial_launch(
            s,
            &boot_sentinel(s),
            BUILD,
            Some(launch),
            handoff_target,
            crate::TrialWitness::Window,
            false,
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
    let by_movement = |s: &Staging, snapshot: u32| {
        let now = trial_launch_count_at(s, BUILD);
        if now > 0 && now != snapshot {
            boot_sentinel(s).forgive_launch(BUILD).unwrap();
        }
        backdate(s);
    };
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
    let kept = scratch("trial-give-back-kept-crash");
    boot_sentinel(&kept).arm(BUILD).unwrap();
    let mut crash = spawn();
    count(&kept, launcher(&crash));
    stop(&mut crash);
    let model = aterm_models_update::native_update_trial_give_back_model();
    let replay = |buggy: i64| {
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
        broken
    };
    let replays: Vec<Vec<(&str, &str)>> = std::thread::scope(|scope| {
        let replay = &replay;
        let running: Vec<_> = (0..4)
            .map(|buggy| scope.spawn(move || replay(buggy)))
            .collect();
        running
            .into_iter()
            .map(|ran| {
                ran.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    for (buggy, mut broken) in (0..).zip(replays) {
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
            1 => [erased("A"), erased("I2"), erased("K"), erased("X")]
                .concat()
                .into_iter()
                .chain([
                    ("O", "AnOwedLaunchIsPaidBeforeTheNextCount"),
                    ("Y", "ASettledLaunchIsOffTheCount"),
                ])
                .collect(),
            2 => [erased("A"), erased("I2"), erased("K")].concat(),
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
        child.kill().unwrap();
        child.wait().unwrap();
    };

    for (label, later_are_targets) in [("boot-wedged-driver", false), ("boot-wedged-ctl", true)] {
        let s = scratch(label);
        let sentinel = boot_sentinel(&s);
        sentinel.arm(11).unwrap();
        let mut driver = spawn();
        count_trial_launch(
            &s,
            &sentinel,
            11,
            Some(launcher(&driver)),
            true,
            crate::TrialWitness::Window,
            false,
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
                false,
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
    let stop = |child: &mut Child| {
        child.kill().unwrap();
        child.wait().unwrap();
    };
    let launcher = |child: &Child| TrialLauncher::of(child.id()).expect("a running child");

    let s = scratch("boot-headless-witness");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let sentinel = boot_sentinel(&s);
    count_trial_launch(&s, &sentinel, 11, None, true, Window, false).unwrap();
    backdate(&s);
    assert_eq!(sentinel.read_state(), Some((11, 1)));

    let mut harness = spawn();
    count_trial_launch(
        &s,
        &sentinel,
        11,
        Some(launcher(&harness)),
        false,
        Headless,
        false,
    )
    .unwrap();
    backdate(&s);
    assert_eq!(sentinel.read_state(), Some((11, 1)), "not counted");
    assert!(
        !read_trial_launchers(&s, 11)
            .iter()
            .any(|recorded| recorded.launcher == launcher(&harness)),
        "not a launcher whose overlap could shield the window's next candidate"
    );
    let standing = "a quiet moment is awaited; retried at 12:00";
    Health::record_apply_refusal(&s.health(), 10, standing);
    let status_before = std::fs::read_to_string(&s.status).ok();
    let waiting = confirm_boot_health_verdict_at(
        &s,
        11,
        Some("abc"),
        Headless,
        true,
        ProbeClock::now(),
        bundle_at(&installed),
        Duration::from_millis(100),
    );
    assert!(
        matches!(
            &waiting,
            crate::BootHealthVerdict::Pending { why, retry_after: Some(after) }
                if why.contains("no window of it has confirmed it")
                    && *after <= HEADLESS_WINDOW_WAIT_RECHECK
        ),
        "the headless instance holds by name and asks again within the hour: {waiting:?}"
    );
    assert_eq!(trial_refusal_count(&s), 0, "a wait refuses no proof");
    assert_eq!(
        sentinel.read_state(),
        Some((11, 1)),
        "the trial stays armed"
    );
    assert!(rollback.exists(), "and the predecessor stays on disk");
    window_trial_wait_at(&s, 11, unix_now_secs()).expect("the trial waits");
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

    let mut candidate = spawn();
    count_trial_launch(
        &s,
        &sentinel,
        11,
        Some(launcher(&candidate)),
        true,
        Window,
        false,
    )
    .unwrap();
    backdate(&s);
    assert_eq!(sentinel.read_state(), Some((11, 2)), "toward the revert");
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

    let lane = scratch("boot-headless-lane");
    let installed = lane.root.join("Applications").join("aterm.app");
    arm_window_trial(&lane, &installed, 12, &"cd".repeat(32));
    let sentinel = boot_sentinel(&lane);
    count_trial_launch(&lane, &sentinel, 12, None, false, Headless, false).unwrap();
    assert_eq!(sentinel.read_state(), Some((12, 1)));
    assert!(!confirm_boot_health_at(
        &lane,
        12,
        Some("abc"),
        Headless,
        bundle_at(&installed),
        Duration::from_millis(100),
    ));
    let report = window_trial_wait_at(&lane, 12, unix_now_secs()).expect("the refusal is said");
    let refusal = crate::hold::load(&lane)
        .trial
        .map(|trial| trial.why)
        .unwrap_or_default();
    assert!(!refusal.is_empty() && report.contains(&refusal), "{report}");
    let _ = std::fs::remove_dir_all(&s.root);
    let _ = std::fs::remove_dir_all(&lane.root);
}

fn span_ms() -> u64 {
    u64::try_from(crate::TRIAL_SETTLE_REFUSAL_SPAN.as_millis()).unwrap()
}

/// A reading of the awake clock at `ms`, on today's wall clock.
fn awake(ms: u64) -> ProbeClock {
    ProbeClock {
        awake_ms: Some(ms),
        wall_secs: unix_now_secs(),
    }
}

/// One checkpoint ask about build 11 at `now`, whose proof answers `proof`.
fn ask_at(
    s: &Staging,
    installed: &Path,
    witness: crate::TrialWitness,
    now: ProbeClock,
    proof: Result<(), &str>,
) -> crate::BootHealthVerdict {
    let proof = proof.map_err(|why| ProofRefusal::from(why.to_string()));
    confirm_boot_health_verdict_at_with_proof(
        s,
        11,
        witness,
        true,
        now,
        bundle_at(installed),
        std::time::Duration::from_millis(100),
        move |_| proof,
    )
}

fn is_pending(verdict: &crate::BootHealthVerdict) -> bool {
    matches!(verdict, crate::BootHealthVerdict::Pending { .. })
}

fn is_settled_unproven(verdict: &crate::BootHealthVerdict) -> bool {
    matches!(
        verdict,
        crate::BootHealthVerdict::SettledUnproven { build: 11, .. }
    )
}

/// Whether `rollback` is on disk and keyed to no process: what an unproven settle leaves, so
/// that no later launch's release of an ended window's predecessor removes it.
fn left_in_place(s: &Staging, rollback: &Path) -> bool {
    let unkeyed = KeptPredecessor::read(s).is_none();
    // What the next launch's checkpoint runs, with nothing running from the bundle.
    release_kept_predecessor(s, std::time::Duration::from_millis(100), |_| Some(false));
    unkeyed && read_id(rollback) == "OLD"
}

/// A `predecessor.kept` an earlier window left: its holder has ended.
fn keep_for_an_ended_window(s: &Staging, rollback: &Path) {
    KeptPredecessor {
        holder: TrialLauncher {
            pid: std::process::id(),
            started: (1, 0),
        },
        bundle: rollback.to_path_buf(),
    }
    .record(s)
    .unwrap();
}

#[test]
fn a_window_confirmation_the_proof_refuses_is_retried_until_its_bound_then_settled() {
    use crate::BootHealthVerdict::{Pending, SettledUnproven};
    use crate::TrialWitness::Window;
    let s = scratch("boot-window-proof-refused");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let sentinel = boot_sentinel(&s);
    count_trial_launch(&s, &sentinel, 11, None, true, Window, false).unwrap();
    backdate(&s);
    keep_for_an_ended_window(&s, &rollback);
    Health::record_apply_failure(&s.health(), 10, 11, "an earlier apply failed");
    let before = Health::read(&s.health());
    let said = |words: &str| {
        crate::log_capture::take()
            .iter()
            .filter(|(_, line)| line.contains(words))
            .count()
    };
    let _ = crate::log_capture::take();
    let start = 1_000_000;
    let mut asked = 0;
    for at in [0, 300, 900] {
        let verdict = ask_at(
            &s,
            &installed,
            Window,
            awake(start + at),
            Err("rollback refused"),
        );
        asked += 1;
        assert!(
            matches!(&verdict, Pending { why, retry_after: None } if why.contains("rollback refused")),
            "a refused proof below the bound is retried: {verdict:?}"
        );
        assert_eq!(trial_refusal_count(&s), asked);
    }
    assert_eq!(said("stays armed"), 1, "said once, not once per try");
    let report = window_trial_wait_at(&s, 11, unix_now_secs()).expect("the trial is reported");
    assert!(
        report.contains("rollback refused") && report.contains(&trial_bound_words()),
        "the report names the reason and the bound: {report}"
    );
    let refusal = refused_trial_refusal(&s, 11);
    assert!(
        crate::is_boot_health_confirmation_wait(&refusal, 11)
            && refusal.contains("rollback refused")
            && refusal.contains(&trial_bound_words()),
        "an apply's refusal names the reason and the bound: {refusal}"
    );
    assert!(
        matches!(
            settle_trials_under_apply_lock(&s, &installed, 11, None),
            Err(ApplyOutcome::Deferred(why)) if why == refusal
        ),
        "and that is what an apply is refused with"
    );
    assert_eq!(
        sentinel.read_state(),
        Some((11, 1)),
        "three refusals in a second settle nothing"
    );

    let verdict = ask_at(
        &s,
        &installed,
        Window,
        awake(start + span_ms() + 1000),
        Err("rollback refused"),
    );
    let SettledUnproven { build: 11, why } = &verdict else {
        panic!("the bound settles the trial: {verdict:?}");
    };
    assert!(
        why.contains("unproven at its window checkpoint") && why.contains("rollback refused"),
        "{why}"
    );
    assert!(verdict.opens_gate());
    assert_eq!(verdict.settled_build(), Some(11));
    assert_eq!(sentinel.read_state(), None, "the trial is disarmed");
    assert!(Trial::read(&s.trial()).is_none(), "with its identity");
    assert_eq!(read_id(&installed), "NEW", "the build stays: no revert");
    assert_eq!(read_id(&rollback), "OLD");
    assert!(
        why.contains(&format!(
            "the previous build stays at {} until the next update replaces it",
            rollback.display()
        )),
        "{why}"
    );
    assert!(
        left_in_place(&s, &rollback),
        "and so it does: keyed to no window, an earlier window's record cleared, so a \
         relaunch's release removes nothing"
    );
    let status = std::fs::read_to_string(&s.status).unwrap_or_default();
    assert!(status.contains("update trial unproven"), "{status}");
    let after = Health::read(&s.health());
    assert!(
        crate::hold::load(&s).trial.is_none(),
        "the refusal no longer stands"
    );
    assert_eq!(
        (
            after.apply_failures,
            after.apply_failures_for_target,
            after.last_apply_error.as_str()
        ),
        (
            before.apply_failures,
            before.apply_failures_for_target,
            before.last_apply_error.as_str()
        ),
        "an unproven settle is no apply success: the failure ledger is as it was"
    );
    assert_eq!(trial_refusal_count(&s), 0, "the streak ends with the trial");
    assert_eq!(window_trial_wait_at(&s, 11, unix_now_secs()), None);
    assert!(
        settle_trials_under_apply_lock(&s, &installed, 11, None).is_ok(),
        "updates are re-enabled"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_burst_of_refusals_is_not_a_lasting_one() {
    use crate::TrialWitness::Window;
    let s = scratch("boot-refusal-burst");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    for ask in 0..10 {
        let verdict = ask_at(
            &s,
            &installed,
            Window,
            awake(1_000_000 + ask * 30_000),
            Err("rollback refused"),
        );
        assert!(is_pending(&verdict), "ask {ask}: {verdict:?}");
    }
    assert_eq!(
        trial_refusal_count(&s),
        10,
        "ten refusals in under five minutes"
    );
    assert_eq!(boot_sentinel(&s).read_state(), Some((11, 0)));
    assert!(Trial::read(&s.trial()).is_some());
    assert_eq!(read_id(&installed), "NEW");
    assert_eq!(read_id(&rollback), "OLD");
    assert!(KeptPredecessor::read(&s).is_none(), "nothing moved");
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_refusal_streak_restarts_with_another_trial_and_spans_only_awake_time() {
    let s = scratch("boot-refusal-streak");
    let other = scratch("boot-refusal-streak-other");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let wall = |secs: u64| ProbeClock {
        awake_ms: None,
        wall_secs: secs,
    };

    assert_eq!(count_trial_refusal(&s, 11, awake(1_000)), (1, 0));
    assert_eq!(count_trial_refusal(&s, 11, awake(2_000)), (2, 1_000));
    assert_eq!(
        count_trial_refusal(&other, 11, awake(2_500)),
        (1, 0),
        "another staging root counts apart"
    );
    assert_eq!(trial_refusal_count(&s), 2);

    let asleep = ProbeClock {
        awake_ms: Some(3_000),
        wall_secs: unix_now_secs() + 24 * 60 * 60,
    };
    assert_eq!(
        count_trial_refusal(&s, 11, asleep),
        (3, 2_000),
        "a day of sleep on the wall clock spans nothing on the awake one"
    );

    interrupt_trial_refusals(&s);
    assert_eq!(
        count_trial_refusal(&s, 11, awake(9_000)),
        (4, 2_000),
        "the gap across an ask that ran no proof spans nothing"
    );
    assert_eq!(
        count_trial_refusal(&s, 11, awake(9_500)),
        (5, 2_500),
        "and the next gap spans again"
    );

    Trial::record_required(&s.trial(), 11, &"cd".repeat(32), Some(&installed)).unwrap();
    assert_eq!(
        count_trial_refusal(&s, 11, awake(10_000)),
        (1, 0),
        "another artifact's trial starts its own streak"
    );
    assert_eq!(count_trial_refusal(&s, 11, awake(11_000)), (2, 1_000));
    assert_eq!(
        count_trial_refusal(&s, 11, awake(10)),
        (1, 0),
        "an awake clock that reads below the last refusal restarted"
    );
    assert_eq!(count_trial_refusal(&s, 11, awake(510)), (2, 500));
    // No awake clock: the wall clock, which a sleep and a forward step both age, spans
    // nothing, in either direction.
    assert_eq!(
        count_trial_refusal(&s, 11, wall(500)),
        (3, 500),
        "a refusal with no awake reading is counted and spans nothing"
    );
    assert_eq!(
        count_trial_refusal(&s, 11, wall(500 + 24 * 60 * 60)),
        (4, 500),
        "a day on the wall clock alone spans nothing"
    );
    assert_eq!(
        count_trial_refusal(&s, 11, awake(99_000_000)),
        (5, 500),
        "nor does the gap back to an awake reading"
    );
    assert_eq!(
        count_trial_refusal(&s, 12, awake(30)),
        (1, 0),
        "another build's trial starts its own streak"
    );
    forget_trial_refusals(&s);
    assert_eq!(trial_refusal_count(&s), 0);
    assert_eq!(trial_refusal_count(&other), 1);
    forget_trial_refusals(&other);
    let _ = std::fs::remove_dir_all(&s.root);
    let _ = std::fs::remove_dir_all(&other.root);
}

#[test]
fn without_an_awake_clock_no_refusal_settles_the_trial() {
    use crate::TrialWitness::Window;
    let s = scratch("boot-refusal-wall-clock");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let wall = unix_now_secs();
    // Two refusals, then a closed lid (or a clock stepped forward) and refusals for a day.
    for after_secs in [0, 1, 1_900, 3_800, 24 * 60 * 60] {
        let now = ProbeClock {
            awake_ms: None,
            wall_secs: wall + after_secs,
        };
        let verdict = ask_at(&s, &installed, Window, now, Err("rollback refused"));
        assert!(
            is_pending(&verdict),
            "wall time is no awake time, +{after_secs} s: {verdict:?}"
        );
    }
    assert_eq!(trial_refusal_count(&s), 5, "each was counted");
    assert_eq!(boot_sentinel(&s).read_state(), Some((11, 0)));
    assert_eq!(read_id(&rollback), "OLD");
    forget_trial_refusals(&s);
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_refusal_spans_the_bound_only_while_it_lasts() {
    use crate::BootHealthVerdict::{Pending, Unanswered};
    use crate::TrialWitness::{Headless, Window};
    use std::os::unix::fs::PermissionsExt;
    let minute = 60_000;
    let start = 1_000_000;

    // A proof that passed between two refusals: the refusal did not last.
    let s = scratch("boot-refusal-then-pass");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    for at in [0, 1_000] {
        let verdict = ask_at(
            &s,
            &installed,
            Window,
            awake(start + at),
            Err("mid-rewrite"),
        );
        assert!(is_pending(&verdict), "{verdict:?}");
    }
    let lock = |mode: u32| {
        std::fs::set_permissions(&s.root, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    lock(0o500);
    let passed = ask_at(&s, &installed, Window, awake(start + 2_000), Ok(()));
    lock(0o700);
    assert!(
        matches!(&passed, Pending { why, .. } if why.contains("the recovery proof passed")),
        "{passed:?}"
    );
    assert_eq!(
        trial_refusal_count(&s),
        0,
        "a proof that passed ends the streak"
    );
    let verdict = ask_at(
        &s,
        &installed,
        Window,
        awake(start + span_ms() + minute),
        Err("one slow verifier"),
    );
    assert!(
        is_pending(&verdict),
        "one refusal half an hour after a pass settles nothing: {verdict:?}"
    );
    assert_eq!(trial_refusal_count(&s), 1);
    assert_eq!(boot_sentinel(&s).read_state(), Some((11, 0)));
    forget_trial_refusals(&s);
    let _ = std::fs::remove_dir_all(&s.root);

    // Asks that ran no proof between refusals: their time spans nothing.
    let s = scratch("boot-refusal-then-lock");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    for at in [0, 1_000] {
        let verdict = ask_at(&s, &installed, Window, awake(start + at), Err("refused"));
        assert!(is_pending(&verdict), "{verdict:?}");
    }
    let holder = FileLock::acquire(&s.apply_lock).unwrap();
    let unanswered = confirm_boot_health_verdict_at_with_proof(
        &s,
        11,
        Window,
        true,
        awake(start + 10 * minute),
        bundle_at(&installed),
        std::time::Duration::ZERO,
        |_| panic!("no proof runs without the apply lock"),
    );
    drop(holder);
    assert!(matches!(unanswered, Unanswered { .. }), "{unanswered:?}");
    assert_eq!(trial_refusal_count(&s), 2, "the count is kept");
    let verdict = ask_at(
        &s,
        &installed,
        Window,
        awake(start + span_ms() + minute),
        Err("refused"),
    );
    assert!(
        is_pending(&verdict),
        "three refusals, but only a second of refusal is known to have lasted: {verdict:?}"
    );
    assert_eq!(trial_refusal_count(&s), 3);
    let verdict = ask_at(
        &s,
        &installed,
        Window,
        awake(start + 2 * span_ms()),
        Err("refused"),
    );
    assert!(
        is_pending(&verdict),
        "a minute short of the span from the refusal after the unanswered ask: {verdict:?}"
    );
    let verdict = ask_at(
        &s,
        &installed,
        Window,
        awake(start + 2 * span_ms() + minute),
        Err("refused"),
    );
    assert!(
        is_settled_unproven(&verdict),
        "refusals that lasted the span since then settle it: {verdict:?}"
    );
    let _ = std::fs::remove_dir_all(&s.root);

    // A headless instance's refusals, then a window launches the trial: after its wait the
    // instance starts a fresh streak.
    let s = scratch("boot-refusal-then-window-wait");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    for at in [0, 1_000] {
        let verdict = ask_at(&s, &installed, Headless, awake(start + at), Err("refused"));
        assert!(is_pending(&verdict), "{verdict:?}");
    }
    assert_eq!(trial_refusal_count(&s), 2);
    count_trial_launch(&s, &boot_sentinel(&s), 11, None, true, Window, false).unwrap();
    let waiting = ask_at(
        &s,
        &installed,
        Headless,
        awake(start + 2_000),
        Err("no proof runs while a window is awaited"),
    );
    assert!(
        matches!(
            &waiting,
            Pending {
                retry_after: Some(_),
                ..
            }
        ),
        "{waiting:?}"
    );
    assert_eq!(
        trial_refusal_count(&s),
        0,
        "the wait for a window ends the instance's streak"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_refusal_the_launch_count_reverts_is_never_settled_at_the_bound() {
    use crate::BootHealthVerdict::Pending;
    use crate::TrialWitness::Window;
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    type Identity = Result<(u64, String), String>;
    let digest = "ab".repeat(32);
    let start = 1_000_000;
    // The installed bundle no longer verifies (a broken seal, a resource lost in the swap);
    // `predecessor` is what the retained one reads as.
    let asks = |label: &str, predecessor: Identity| {
        let s = scratch(label);
        let installed = s.root.join("Applications").join("aterm.app");
        let rollback = arm_window_trial(&s, &installed, 11, &digest);
        receipt_for(&s, 11, &digest);
        let verdicts: Vec<_> = (0..40)
            .map(|ask| {
                let sealed = installed.clone();
                let predecessor = predecessor.clone();
                let identity = move |path: &Path| -> Identity {
                    if path == sealed.as_path() {
                        Err("bundle policy: a sealed resource is missing or invalid".to_string())
                    } else {
                        predecessor.clone()
                    }
                };
                confirm_boot_health_verdict_at_with_proof(
                    &s,
                    11,
                    Window,
                    true,
                    awake(start + ask * 60_000),
                    bundle_at(&installed),
                    std::time::Duration::from_millis(100),
                    |root| prove_current_trial(&s, root, 11, Some(COMMIT), &identity).map(|_| ()),
                )
            })
            .collect();
        (s, installed, rollback, verdicts)
    };

    let (s, installed, rollback, verdicts) =
        asks("boot-installed-unproven", Ok((10, COMMIT.to_string())));
    for verdict in &verdicts {
        assert!(
            matches!(
                verdict,
                Pending { why, retry_after: None }
                    if why.contains("installed trial is not verified")
                        && why.contains(&format!("launch {MAX_BOOT_ATTEMPTS} reverts"))
                        && !why.contains(&trial_bound_words())
            ),
            "forty refusals over forty awake minutes leave it to the launch count: {verdict:?}"
        );
    }
    assert_eq!(trial_refusal_count(&s), 0, "none counts toward the bound");
    assert_eq!(
        boot_sentinel(&s).read_state(),
        Some((11, 0)),
        "the trial stays armed, so the launch that reaches the count still reverts"
    );
    assert!(Trial::read(&s.trial()).is_some());
    assert_eq!(read_id(&installed), "NEW");
    assert_eq!(read_id(&rollback), "OLD");
    for words in [
        window_trial_wait_at(&s, 11, unix_now_secs()).expect("the trial is reported"),
        refused_trial_refusal(&s, 11),
    ] {
        assert!(
            words.contains("reverts to the previous build")
                && !words.contains(&trial_bound_words()),
            "what is said names the launch count, not a settle that will not come: {words}"
        );
    }
    let _ = std::fs::remove_dir_all(&s.root);

    // Control: with no predecessor to revert to, the launch count would disarm it too, so
    // the bound settles it.
    let (s, installed, _rollback, verdicts) = asks(
        "boot-installed-unproven-no-revert",
        Err(
            "bundle policy: codesign --verify failed: code failed to satisfy specified code \
             requirement(s)"
                .to_string(),
        ),
    );
    assert!(
        verdicts.iter().any(is_settled_unproven),
        "PRECONDITION: the stand-in predecessor reads as no revert; verdicts: {verdicts:?}"
    );
    assert_eq!(boot_sentinel(&s).read_state(), None);
    assert_eq!(read_id(&installed), "NEW");
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_trial_whose_identity_is_lost_is_settled_at_its_bound_with_its_predecessor_in_place() {
    use crate::TrialWitness::{Headless, Window};
    let start = 1_000_000;
    // `trial.toml` is gone (the audit's state): the proof can never pass, and the streak and
    // the settle are keyed by no digest.
    let settle = |label: &str, witness, lane_blocked: bool, identity_lost: bool| {
        let s = scratch(label);
        let installed = s.root.join("Applications").join("aterm.app");
        let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
        if identity_lost {
            Trial::clear(&s.trial());
            assert_eq!(trial_digest(&s, 11), "", "PRECONDITION: no identity reads");
        }
        if lane_blocked {
            // A folder where the record would go: it cannot be written.
            std::fs::create_dir_all(window_unproven_path(&s).join("in the way")).unwrap();
        }
        for at in [0, 1_000] {
            let verdict = ask_at(
                &s,
                &installed,
                witness,
                awake(start + at),
                Err("installed trial has no exact trial identity"),
            );
            assert!(is_pending(&verdict), "{label}: {verdict:?}");
        }
        assert_eq!(
            trial_refusal_count(&s),
            2,
            "{label}: the streak is kept between asks"
        );
        let verdict = ask_at(
            &s,
            &installed,
            witness,
            awake(start + span_ms()),
            Err("installed trial has no exact trial identity"),
        );
        assert!(is_settled_unproven(&verdict), "{label}: {verdict:?}");
        assert_eq!(boot_sentinel(&s).read_state(), None, "{label}");
        assert_eq!(read_id(&installed), "NEW", "{label}: no revert");
        assert!(
            WindowUnproven::read(&s).is_none(),
            "{label}: no window lane is recorded"
        );
        assert!(
            left_in_place(&s, &rollback),
            "{label}: nothing proved the build, so its predecessor is never removed"
        );
        let crate::BootHealthVerdict::SettledUnproven { why, .. } = verdict else {
            unreachable!("asserted above");
        };
        assert!(
            why.contains("until the next update replaces it"),
            "{label}: {why}"
        );
        let _ = std::fs::remove_dir_all(&s.root);
    };
    settle("boot-lost-identity-window", Window, false, true);
    settle("boot-lost-identity-headless", Headless, false, true);
    settle("boot-unrecordable-window-lane", Headless, true, false);
}

#[test]
fn only_a_checkpoint_ask_releases_an_ended_windows_predecessor() {
    use crate::TrialWitness::Window;
    let s = scratch("boot-precheckpoint-release");
    let installed = s.root.join("Applications").join("aterm.app");
    make_app(&installed, "CURRENT");
    let rollback = rollback_path(&installed);
    make_app(&rollback, "OLD");
    keep_for_an_ended_window(&s, &rollback);
    let ask = |checkpoint: bool| {
        confirm_boot_health_in(
            &s,
            11,
            None,
            Window,
            checkpoint,
            bundle_at(&installed),
            |_| Some(false),
        )
    };

    // Held throughout the first ask: one before the checkpoint takes no lock.
    let holder = FileLock::acquire(&s.apply_lock).unwrap();
    let asked = std::time::Instant::now();
    assert_eq!(ask(false), crate::BootHealthVerdict::NO_TRIAL);
    assert!(
        asked.elapsed() < APPLY_LOCK_WAIT,
        "it did not wait on the apply lock"
    );
    drop(holder);
    assert_eq!(ask(false), crate::BootHealthVerdict::NO_TRIAL);
    assert_eq!(
        read_id(&rollback),
        "OLD",
        "an ask before the checkpoint removes nothing"
    );
    assert!(
        KeptPredecessor::read(&s).is_some(),
        "and leaves the record as it was"
    );

    assert_eq!(ask(true), crate::BootHealthVerdict::NO_TRIAL);
    assert!(
        !rollback.exists() && KeptPredecessor::read(&s).is_none(),
        "the checkpoint's ask releases it"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_lock_or_an_unresolved_install_counts_no_refusal() {
    use crate::BootHealthVerdict::Unanswered;
    use crate::TrialWitness::Window;
    let s = scratch("boot-refusal-unanswered");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let start = 1_000_000;
    let holder = FileLock::acquire(&s.apply_lock).unwrap();
    for ask in 0..10 {
        let verdict = confirm_boot_health_verdict_at_with_proof(
            &s,
            11,
            Window,
            true,
            awake(start + ask * (span_ms() / 9 + 60_000)),
            bundle_at(&installed),
            std::time::Duration::ZERO,
            |_| panic!("no proof runs without the apply lock"),
        );
        assert!(
            matches!(&verdict, Unanswered { why } if why.contains("health lock")),
            "ask {ask}: {verdict:?}"
        );
    }
    drop(holder);
    for ask in 0..2 {
        let verdict = confirm_boot_health_verdict_at_with_proof(
            &s,
            11,
            Window,
            true,
            awake(start + 2 * span_ms() + ask),
            || None,
            std::time::Duration::from_millis(100),
            |_| panic!("no proof runs for an install that does not resolve"),
        );
        assert!(matches!(verdict, Unanswered { .. }), "{verdict:?}");
    }
    assert_eq!(
        trial_refusal_count(&s),
        0,
        "twelve asks over an hour that could not be answered refused nothing"
    );
    let verdict = ask_at(
        &s,
        &installed,
        Window,
        awake(start + 3 * span_ms()),
        Err("rollback refused"),
    );
    assert!(is_pending(&verdict), "the first refusal: {verdict:?}");
    assert_eq!(trial_refusal_count(&s), 1);
    assert_eq!(boot_sentinel(&s).read_state(), Some((11, 0)));
    forget_trial_refusals(&s);
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_panicking_proof_is_a_counted_refusal() {
    use crate::BootHealthVerdict::Pending;
    use crate::TrialWitness::Window;
    let s = scratch("boot-refusal-panic");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let verdict = confirm_boot_health_verdict_at_with_proof(
        &s,
        11,
        Window,
        true,
        awake(1_000_000),
        bundle_at(&installed),
        std::time::Duration::from_millis(100),
        |_| panic!("the verifier fell over"),
    );
    assert!(
        matches!(&verdict, Pending { why, .. } if why.contains("the recovery proof panicked")),
        "{verdict:?}"
    );
    assert_eq!(trial_refusal_count(&s), 1);
    assert_eq!(boot_sentinel(&s).read_state(), Some((11, 0)));
    assert!(
        FileLock::acquire(&s.apply_lock).is_ok(),
        "the apply lock is released"
    );
    forget_trial_refusals(&s);
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn an_unreadable_sentinel_whose_proof_is_refused_is_settled_at_its_bound() {
    use crate::TrialWitness::Window;
    let s = scratch("boot-unreadable-sentinel-bound");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    corrupt_sentinel(&s);
    let start = 1_000_000;
    for at in [0, 1_000] {
        let verdict = ask_at(&s, &installed, Window, awake(start + at), Err("no receipt"));
        assert!(is_pending(&verdict), "{verdict:?}");
        assert!(
            boot_sentinel(&s).unreadable(),
            "the marker is left as it was"
        );
    }
    let verdict = ask_at(
        &s,
        &installed,
        Window,
        awake(start + span_ms()),
        Err("no receipt"),
    );
    assert!(is_settled_unproven(&verdict), "{verdict:?}");
    assert!(
        !s.root.join("boot.sentinel").exists(),
        "the unreadable marker is removed"
    );
    assert!(Trial::read(&s.trial()).is_none());
    assert!(left_in_place(&s, &rollback), "the predecessor stays");
    assert_eq!(read_id(&installed), "NEW");
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_sentinel_that_cannot_be_removed_at_the_bound_stays_pending_and_is_said_once() {
    use crate::BootHealthVerdict::Pending;
    use crate::TrialWitness::Window;
    use std::os::unix::fs::PermissionsExt;
    let s = scratch("boot-sentinel-stuck");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let start = 1_000_000;
    for at in [0, 1_000] {
        let verdict = ask_at(&s, &installed, Window, awake(start + at), Err("no receipt"));
        assert!(is_pending(&verdict), "{verdict:?}");
    }
    let _ = crate::log_capture::take();
    // A folder that admits no removal, as a locked flag or a read-only volume refuses one.
    let lock = |mode: u32| {
        std::fs::set_permissions(&s.root, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    lock(0o500);
    let stuck = [span_ms(), span_ms() + 1_000]
        .map(|at| ask_at(&s, &installed, Window, awake(start + at), Err("no receipt")));
    let armed_while_stuck = boot_sentinel(&s).read_state();
    lock(0o700);
    for verdict in &stuck {
        assert!(
            matches!(verdict, Pending { why, .. } if why.contains("could not be removed")),
            "a sentinel that cannot be removed leaves the trial armed and named: {verdict:?}"
        );
    }
    assert_eq!(armed_while_stuck, Some((11, 0)));
    assert_eq!(
        crate::log_capture::take()
            .iter()
            .filter(|(_, line)| line.contains("could not be removed"))
            .count(),
        1,
        "said once, not once per ask"
    );
    assert_eq!(read_id(&installed), "NEW");
    assert_eq!(read_id(&rollback), "OLD");
    assert!(Trial::read(&s.trial()).is_some(), "nothing else moved");
    assert!(KeptPredecessor::read(&s).is_none());

    let verdict = ask_at(
        &s,
        &installed,
        Window,
        awake(start + span_ms() + 1_001),
        Err("no receipt"),
    );
    assert!(
        is_settled_unproven(&verdict),
        "once it can be removed the next ask settles, with no new span: {verdict:?}"
    );
    assert_eq!(boot_sentinel(&s).read_state(), None);
    assert!(left_in_place(&s, &rollback));
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_headless_settle_at_the_bound_keeps_the_window_lane_open() {
    use crate::TrialWitness::{Headless, Window};
    use std::time::Duration;
    let s = scratch("boot-headless-bound");
    let installed = s.root.join("Applications").join("aterm.app");
    let digest = "ab".repeat(32);
    let rollback = arm_window_trial(&s, &installed, 11, &digest);
    receipt_for(&s, 11, &digest);
    let sentinel = boot_sentinel(&s);
    count_trial_launch(&s, &sentinel, 11, None, false, Headless, false).unwrap();
    let start = 1_000_000;
    for at in [0, 1_000] {
        let verdict = ask_at(&s, &installed, Headless, awake(start + at), Err("refused"));
        assert!(is_pending(&verdict), "{verdict:?}");
    }
    let verdict = ask_at(
        &s,
        &installed,
        Headless,
        awake(start + span_ms()),
        Err("refused"),
    );
    assert!(
        matches!(
            &verdict,
            crate::BootHealthVerdict::SettledUnproven { build: 11, why }
                if why.contains("unproven at its headless checkpoint")
        ),
        "{verdict:?}"
    );
    assert_eq!(sentinel.read_state(), None, "later updates are not held");
    assert_eq!(read_id(&rollback), "OLD", "the predecessor stays");
    let record = WindowUnproven::read(&s).expect("the windows are unproven");
    assert_eq!(
        (
            record.build,
            record.digest.as_str(),
            record.install_root.as_path()
        ),
        (11, digest.as_str(), installed.as_path())
    );
    assert!(
        KeptPredecessor::read(&s).is_none(),
        "it is the windows' revert, not this instance's bundle"
    );

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
        "a window launch puts it back on trial, counted"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_relaunch_starts_no_inherited_streak_and_the_third_launch_still_reverts() {
    use crate::TrialWitness::Headless;
    let s = scratch("boot-relaunch-streak");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let sentinel = boot_sentinel(&s);
    let start = 1_000_000;
    for at in [0, span_ms() + 1_000] {
        let verdict = ask_at(&s, &installed, Headless, awake(start + at), Err("refused"));
        assert!(
            is_pending(&verdict),
            "two refusals are below the bound: {verdict:?}"
        );
    }
    assert_eq!(trial_refusal_count(&s), 2);

    // A relaunch is another process: its memory holds no streak, whatever the ledger says.
    forget_trial_refusals(&s);
    assert!(
        crate::hold::load(&s)
            .trial
            .is_some_and(|trial| !trial.why.is_empty()),
        "PRECONDITION: the record still says the refusal"
    );
    let verdict = ask_at(
        &s,
        &installed,
        Headless,
        awake(start + 3 * span_ms()),
        Err("refused"),
    );
    assert!(
        is_pending(&verdict),
        "what an earlier process counted settles nothing in this one: {verdict:?}"
    );
    assert_eq!(trial_refusal_count(&s), 1);
    assert_eq!(read_id(&installed), "NEW");

    // The launch-counted revert is untouched by any of it.
    for _ in 1..MAX_BOOT_ATTEMPTS {
        sentinel.observe_launch(11).unwrap();
    }
    backdate(&s);
    let outcome = recover_boot_trial_at(&s, 11, Headless, bundle_at(&installed), |_| {
        Ok(TrialRevert::To(VerifiedRollback {
            path: rollback.clone(),
            build: 10,
        }))
    });
    assert!(
        matches!(outcome, ApplyOutcome::ReExecFailed(_)),
        "the launch that reaches the count reverts; this fixture has no executable: {outcome:?}"
    );
    assert_eq!(read_id(&installed), "OLD");
    assert_eq!(sentinel.read_state(), None);
    forget_trial_refusals(&s);
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn before_the_checkpoint_only_an_unarmed_build_is_confirmed() {
    use crate::BootHealthVerdict::{Pending, Unanswered};
    const NO_TRIAL: crate::BootHealthVerdict = crate::BootHealthVerdict::NO_TRIAL;
    use crate::TrialWitness::{Headless, Window};
    let ask = |s: &Staging, build: u64, witness, installed: Option<&Path>| {
        let installed = installed.map(Path::to_path_buf);
        confirm_boot_health_verdict_at_with_proof(
            s,
            build,
            witness,
            false,
            ProbeClock::now(),
            move || {
                installed.map(|installed| bundle::Bundle {
                    exe: installed.join("Contents/MacOS/aterm"),
                    app_root: installed,
                })
            },
            std::time::Duration::ZERO,
            |_| panic!("no proof runs before a checkpoint"),
        )
    };
    let none = scratch("boot-precheckpoint-none");
    let installed = none.root.join("Applications").join("aterm.app");
    make_app(&installed, "CURRENT");
    assert_eq!(
        ask(&none, 11, Window, Some(&installed)),
        NO_TRIAL,
        "a process that owes no trial is confirmed without drawing"
    );
    assert_eq!(ask(&none, 11, Window, None), NO_TRIAL);

    let s = scratch("boot-precheckpoint-armed");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let other = s.root.join("Other").join("aterm.app");
    make_app(&other, "OTHER");
    // Held throughout: an ask before the checkpoint takes no lock.
    let holder = FileLock::acquire(&s.apply_lock).unwrap();
    let window = ask(&s, 11, Window, Some(&installed));
    assert!(
        matches!(&window, Pending { why, retry_after: None } if why.contains("window's first frame")),
        "{window:?}"
    );
    let headless = ask(&s, 11, Headless, Some(&installed));
    assert!(
        matches!(&headless, Pending { why, .. } if why.contains("instance's control socket")),
        "{headless:?}"
    );
    assert_eq!(
        ask(&s, 12, Window, Some(&installed)),
        NO_TRIAL,
        "another build's trial is not this process's"
    );
    assert_eq!(
        ask(&s, 11, Window, Some(&other)),
        NO_TRIAL,
        "nor another install's"
    );
    let unresolved = ask(&s, 11, Window, None);
    assert!(matches!(unresolved, Unanswered { .. }), "{unresolved:?}");
    drop(holder);
    assert_eq!(
        boot_sentinel(&s).read_state(),
        Some((11, 0)),
        "nothing is disarmed"
    );
    assert!(Trial::read(&s.trial()).is_some());
    assert_eq!(read_id(&rollback), "OLD");
    assert!(!s.health().exists(), "nothing is written");
    assert!(!s.status.exists());
    assert_eq!(trial_refusal_count(&s), 0, "nothing is counted");
    let _ = std::fs::remove_dir_all(&none.root);
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn the_checkpoint_settle_follows_the_trial_model() {
    use crate::TrialWitness::Window;
    let model = aterm_models_update::native_update_trial_settle_model();
    let constant = |name: &str| {
        model
            .consts
            .iter()
            .find(|(constant, _)| *constant == name)
            .map(|(_, value)| *value)
            .expect("the model declares it")
    };
    assert_eq!(
        constant("SettleAfter"),
        i64::from(crate::TRIAL_SETTLE_AFTER_REFUSALS)
    );
    assert_eq!(constant("MaxLaunch"), i64::from(MAX_BOOT_ATTEMPTS));

    // Every state the committed model reaches.
    let mut reached = vec![model.init_state()];
    let mut next = 0;
    while next < reached.len() {
        let state = reached[next].clone();
        next += 1;
        for action in &model.actions {
            for successor in model.successors(action.name, &state) {
                if !reached.contains(&successor) {
                    reached.push(successor);
                }
            }
        }
    }
    let start = 1_000_000;
    // The code's view of a model state: an armed trial, a streak of `refusals` whose refusal
    // has lasted the span by the next ask when `spanned`.
    let realize = |state: &aterm_spec::interp::State| {
        let s = scratch("boot-settle-model");
        let installed = s.root.join("Applications").join("aterm.app");
        arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
        let refusals = u32::try_from(state["refusals"]).unwrap();
        if refusals > 0 {
            seed_trial_refusals(&s, 11, refusals, 1, awake(start + 1));
        }
        let now = awake(
            start
                + if state["spanned"] == 1 {
                    span_ms() + 1_000
                } else {
                    1_000
                },
        );
        (s, installed, now)
    };
    let project = |s: &Staging, installed: &Path| {
        (
            i64::from(trial_armed_for(s, 11)),
            i64::from(rollback_path(installed).exists()),
            i64::from(read_id(installed) != "NEW"),
        )
    };
    let of =
        |state: &aterm_spec::interp::State| (state["armed"], state["rollback"], state["reverted"]);
    let mut asked = 0;
    for state in reached.iter().filter(|state| state["armed"] == 1) {
        let refuse = model.action_enabled("Refuse", state);
        let settle = model.action_enabled("Settle", state);
        assert!(
            refuse != settle,
            "a refused proof is retried or settled, never both or neither: {state:?}"
        );
        let (s, installed, now) = realize(state);
        assert_eq!(project(&s, &installed), of(state), "{state:?}");
        let verdict = ask_at(&s, &installed, Window, now, Err("refused"));
        assert_eq!(is_pending(&verdict), refuse, "{state:?}: {verdict:?}");
        assert_eq!(
            is_settled_unproven(&verdict),
            settle,
            "{state:?}: {verdict:?}"
        );
        let action = if refuse { "Refuse" } else { "Settle" };
        let successors = model.successors(action, state);
        assert_eq!(successors.len(), 1, "{action} from {state:?}");
        assert_eq!(
            project(&s, &installed),
            of(&successors[0]),
            "{action} from {state:?}"
        );
        if refuse {
            assert_eq!(
                i64::from(trial_refusal_count(&s)).min(constant("SettleAfter") - 1),
                successors[0]["refusals"],
                "the streak after Refuse from {state:?}"
            );
        } else {
            assert_eq!(successors[0]["settled"], 2);
            assert_eq!(trial_refusal_count(&s), 0);
        }
        forget_trial_refusals(&s);
        let _ = std::fs::remove_dir_all(&s.root);

        let (s, installed, now) = realize(state);
        let verdict = ask_at(&s, &installed, Window, now, Ok(()));
        assert_eq!(
            verdict,
            crate::BootHealthVerdict::Confirmed { settled: Some(11) },
            "{state:?}"
        );
        let proved = model.successors("Prove", state);
        assert_eq!(proved.len(), 1);
        assert_eq!(
            project(&s, &installed),
            of(&proved[0]),
            "Prove from {state:?}"
        );
        assert_eq!(trial_refusal_count(&s), 0);
        let _ = std::fs::remove_dir_all(&s.root);
        asked += 1;
    }
    assert!(asked >= 5, "the walk reached {asked} armed states");

    // Negative controls: a settle before the span, and a settle that reverts.
    let early = aterm_spec::interp::with_buggy(&model, 2);
    let mut state = early.init_state();
    for action in ["Refuse", "Refuse", "Settle"] {
        assert!(early.fire(action, &mut state), "{action} at {state:?}");
    }
    assert!(!early.check_invariant("SettledOnlyPastSpan", &state));
    let reverting = aterm_spec::interp::with_buggy(&model, 3);
    let mut state = reverting.init_state();
    for action in ["Refuse", "Elapse", "Refuse", "Settle"] {
        assert!(reverting.fire(action, &mut state), "{action} at {state:?}");
    }
    assert!(!reverting.check_invariant("UnprovenSettleKeepsTheBuild", &state));
    assert!(!reverting.check_invariant("RevertOnlyByLaunches", &state));
}

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
    // An earlier window trial of this artifact crash-looped, and its hold has lapsed.
    let sha = crate::hold::sha(&digest).unwrap();
    let lapsed_at = unix_now_secs() - update_record::HOLD_MAX;
    crate::hold::edit(&s, "a lapsed window hold", |record| {
        record.set_hold(
            update_record::Failure {
                build: 11,
                version: "0.11.0".into(),
                sha,
                cause: Cause::CrashLoop,
                witness: Witness::Window,
                why: "it did not start cleanly 3 times".into(),
            },
            lapsed_at,
        )
    })
    .unwrap();

    count_trial_launch(&s, &sentinel, 11, None, false, Headless, false).unwrap();
    assert_eq!(sentinel.read_state(), Some((11, 1)));
    assert!(confirm_boot_health_at_with_proof(
        &s,
        11,
        Headless,
        bundle_at(&installed),
        Duration::from_millis(100),
        proven,
    ));
    let kept = crate::hold::load(&s);
    assert_eq!(
        (kept.running, kept.proven, kept.trial.is_none()),
        (11, false, true),
        "a headless settle proves nothing about the windows: it runs, unproven"
    );
    assert_eq!(
        kept.hold
            .as_ref()
            .map(|hold| (hold.build, hold.tries, hold.witness)),
        Some((11, 1, Witness::Window)),
        "and the window's lineage stays, so its next crash loop escalates"
    );
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
        Trial::read(&s.trial()).is_some_and(|trial| trial.matches(11, &digest)),
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

    std::fs::write(trial_launchers_path(&s), "11\n").unwrap();
    for _ in 1..MAX_BOOT_ATTEMPTS {
        backdate(&s);
        count_trial_launch(&s, &sentinel, 11, None, true, Window, false).unwrap();
    }
    assert!(
        sentinel.should_revert(11, MAX_BOOT_ATTEMPTS),
        "a crash loop reverts"
    );

    assert!(
        !confirm_boot_health_at_with_proof(
            &s,
            11,
            Headless,
            bundle_at(&installed),
            Duration::from_millis(100),
            proven,
        ),
        "a headless ask waits for the window, by name"
    );
    assert_eq!(sentinel.read_state(), Some((11, MAX_BOOT_ATTEMPTS)));
    assert!(rollback.exists());

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
    let started = crate::hold::load(&s);
    assert!(
        started.proven && started.hold.is_none(),
        "a window's settle proves it, and ends its lineage: {started:?}"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn a_window_re_trial_is_bounded() {
    use crate::TrialWitness::Window;
    use std::time::Duration;
    let digest = "ab".repeat(32);
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

    let (s, installed) = settled("boot-retrial-scope");
    let sentinel = boot_sentinel(&s);
    let elsewhere = s.root.join("Other").join("aterm.app");
    assert!(rearm_window_unproven_trial(&s, &sentinel, 11, &elsewhere, now).is_none());
    assert!(rearm_window_unproven_trial(&s, &sentinel, 12, &installed, now).is_none());
    std::fs::write(&s.ready, "pending").unwrap();
    assert!(rearm_window_unproven_trial(&s, &sentinel, 11, &installed, now).is_none());
    std::fs::remove_file(&s.ready).unwrap();
    std::os::unix::fs::symlink("missing-ready.toml", &s.ready).unwrap();
    assert!(
        rearm_window_unproven_trial(&s, &sentinel, 11, &installed, now).is_none(),
        "a dangling ready link is a stage, not its absence"
    );
    std::fs::remove_file(&s.ready).unwrap();
    assert_eq!(sentinel.read_state(), None);
    assert!(
        WindowUnproven::read(&s).is_some(),
        "still the windows' record"
    );
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
    assert!(
        rearm_window_unproven_trial(
            &s,
            &sentinel,
            11,
            &installed,
            WindowUnproven::read(&s).unwrap().at + wait - 1,
        )
        .is_some()
    );
    assert_eq!(sentinel.read_state(), Some((11, 0)));
    let _ = std::fs::remove_dir_all(&s.root);

    for (label, at) in [
        ("boot-retrial-lapsed", now - wait),
        ("boot-retrial-ahead-one-sec", now + 1),
        ("boot-retrial-ahead", now + wait),
    ] {
        let (s, installed) = settled(label);
        let sentinel = boot_sentinel(&s);
        let mut record = WindowUnproven::read(&s).unwrap();
        record.at = at;
        record.record(&s).unwrap();
        assert!(rearm_window_unproven_trial(&s, &sentinel, 11, &installed, now).is_none());
        assert_eq!(sentinel.read_state(), None);
        assert!(WindowUnproven::read(&s).is_none(), "{label}: dropped");
        let _ = std::fs::remove_dir_all(&s.root);
    }

    let (s, installed) = settled("boot-retrial-no-predecessor");
    let sentinel = boot_sentinel(&s);
    std::fs::remove_dir_all(rollback_path(&installed)).unwrap();
    assert!(rearm_window_unproven_trial(&s, &sentinel, 11, &installed, now).is_none());
    assert!(WindowUnproven::read(&s).is_none());
    let _ = std::fs::remove_dir_all(&s.root);
    let (s, installed) = settled("boot-retrial-receipt");
    let sentinel = boot_sentinel(&s);
    receipt_for(&s, 11, &"ef".repeat(32));
    assert!(rearm_window_unproven_trial(&s, &sentinel, 11, &installed, now).is_none());
    assert!(WindowUnproven::read(&s).is_none());
    assert_eq!(sentinel.read_state(), None);
    let _ = std::fs::remove_dir_all(&s.root);

    let [here, elsewhere] = on_and_off_the_install_device(|| {
        let (s, installed) = settled("boot-retrial-tier");
        let sentinel = boot_sentinel(&s);
        let at = WindowUnproven::read(&s).unwrap().at + wait - 1;
        let recording = aterm_durable::seam::Recording::start();
        assert!(rearm_window_unproven_trial(&s, &sentinel, 11, &installed, at).is_some());
        let flushes = recording.take();
        let _ = std::fs::remove_dir_all(&s.root);
        flushes
    });
    use aterm_durable::Flush::{Durable, Ordered};
    assert_eq!(
        here,
        [Ordered, Ordered, Ordered, Ordered, Durable, Durable],
        "one device: the trial identity, then the sentinel, file and entry each, then the \
         update record's trial, file and folder (always durable)"
    );
    assert_eq!(
        elsewhere,
        [Ordered, Ordered, Durable, Durable, Durable, Durable],
        "another device: the trial identity keeps its own same-device guard; the rest, and \
         the update record, are durable"
    );

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

/// A window re-trial whose sentinel's folder flush fails after the rename is not counted
/// against the build, as its warning says: nothing is armed and no trial identity is left,
/// and the windows' record stays, so a later window launch puts the build back on trial.
/// With the arm's record left in place, the sentinel stayed armed for the running build
/// with no identity, so no window launch could confirm it and each one counted it.
#[test]
fn a_window_re_trial_whose_arm_flush_fails_is_not_counted() {
    use aterm_durable::seam::{EIO, FailingDirs};
    use std::time::Duration;
    let digest = "ab".repeat(32);
    let s = scratch("boot-retrial-arm-flush");
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
    let sentinel = boot_sentinel(&s);
    assert_eq!(sentinel.read_state(), None, "settled by a headless launch");
    let at = WindowUnproven::read(&s).unwrap().at + 1;
    let rearmed = {
        let _eio = FailingDirs::with(EIO);
        rearm_window_unproven_trial(&s, &sentinel, 11, &installed, at).is_some()
    };
    assert!(!rearmed);
    assert_eq!(sentinel.read_state(), None, "nothing armed");
    assert!(Trial::read(&s.trial()).is_none(), "no trial identity");
    assert!(
        WindowUnproven::read(&s).is_some(),
        "still the windows' record"
    );
    assert!(rearm_window_unproven_trial(&s, &sentinel, 11, &installed, at).is_some());
    assert_eq!(
        sentinel.read_state(),
        Some((11, 0)),
        "a later window re-trial"
    );
    assert!(Trial::read(&s.trial()).is_some());
    let _ = std::fs::remove_dir_all(&s.root);
}

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
        count_trial_launch(&s, &sentinel, 11, Some(launcher(&live)), true, first, false).unwrap();
        backdate(&s);
        assert_eq!(
            read_trial_launchers(&s, 11),
            vec![RecordedLauncher {
                launcher: launcher(&live),
                witness: first
            }],
            "{label}: recorded with what it proves"
        );
        count_trial_launch(
            &s,
            &sentinel,
            11,
            Some(launcher(&candidate)),
            true,
            target,
            false,
        )
        .unwrap();
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

fn arm_window_trial(s: &Staging, installed: &Path, build: u64, digest: &str) -> PathBuf {
    make_app(installed, "NEW");
    let rollback = rollback_path(installed);
    make_app(&rollback, "OLD");
    Trial::record_required(&s.trial(), build, digest, Some(installed)).unwrap();
    boot_sentinel(s).arm(build).unwrap();
    rollback
}

#[test]
fn a_boot_health_verdict_distinguishes_a_settlement_from_nothing_owed_and_from_no_answer() {
    use crate::BootHealthVerdict::{Confirmed, Pending, Unanswered};
    const NO_TRIAL: crate::BootHealthVerdict = crate::BootHealthVerdict::NO_TRIAL;
    use crate::TrialWitness::{Headless, Window};

    let wait = std::time::Duration::from_millis(100);
    let ask =
        |s: &Staging, witness, installed: Option<&Path>, prove: fn(&Path) -> Result<(), String>| {
            let prove = move |root: &Path| prove(root).map_err(ProofRefusal::from);
            let installed = installed.map(Path::to_path_buf);
            confirm_boot_health_verdict_at_with_proof(
                s,
                11,
                witness,
                true,
                ProbeClock::now(),
                move || {
                    installed.map(|installed| bundle::Bundle {
                        exe: installed.join("Contents/MacOS/aterm"),
                        app_root: installed,
                    })
                },
                wait,
                prove,
            )
        };
    let no_trial = scratch("boot-result-none");
    assert_eq!(
        confirm_boot_health_verdict_at_with_proof(
            &no_trial,
            11,
            Window,
            true,
            ProbeClock::now(),
            || panic!("no trial resolves no installed bundle"),
            wait,
            |_| panic!("no trial runs no proof"),
        ),
        NO_TRIAL
    );

    let foreign = scratch("boot-result-foreign");
    let owner = foreign.root.join("Applications/aterm.app");
    let other = foreign.root.join("Other/aterm.app");
    arm_window_trial(&foreign, &owner, 11, &"ab".repeat(32));
    make_app(&other, "OTHER");
    assert_eq!(
        ask(&foreign, Window, Some(&other), |_| panic!(
            "a foreign trial runs no proof"
        )),
        NO_TRIAL,
        "another install's trial is nothing this process owes"
    );
    assert!(trial_armed_for(&foreign, 11), "and it stays its owner's");

    let unresolved = scratch("boot-result-retry");
    let installed = unresolved.root.join("Applications/aterm.app");
    arm_window_trial(&unresolved, &installed, 11, &"ab".repeat(32));
    let unanswered = ask(&unresolved, Window, None, |_| {
        panic!("an unresolved install runs no proof")
    });
    assert!(
        matches!(&unanswered, Unanswered { why } if why == UNRESOLVED_INSTALL),
        "{unanswered:?}"
    );
    assert_eq!(trial_refusal_count(&unresolved), 0);

    let window_owned = scratch("boot-result-window-owned");
    let installed = window_owned.root.join("Applications/aterm.app");
    arm_window_trial(&window_owned, &installed, 11, &"ab".repeat(32));
    record_window_launch(&window_owned, 11);
    let waiting = ask(&window_owned, Headless, Some(&installed), |_| {
        panic!("a headless witness runs no proof for a window-owned trial")
    });
    assert!(
        matches!(
            &waiting,
            Pending {
                retry_after: Some(_),
                ..
            }
        ),
        "{waiting:?}"
    );

    let settled = scratch("boot-result-settled");
    let installed = settled.root.join("Applications/aterm.app");
    arm_window_trial(&settled, &installed, 11, &"ab".repeat(32));
    let verdict = ask(&settled, Window, Some(&installed), |_| Ok(()));
    assert_eq!(verdict, Confirmed { settled: Some(11) });
    assert_eq!(verdict.settled_build(), Some(11));
    assert!(verdict.opens_gate() && NO_TRIAL.opens_gate());
    assert_eq!(NO_TRIAL.settled_build(), None);
    assert!(!waiting.opens_gate() && !unanswered.opens_gate());
    assert!(!trial_armed_for(&settled, 11));

    for staging in [no_trial, foreign, unresolved, window_owned, settled] {
        let _ = std::fs::remove_dir_all(&staging.root);
    }
}

#[test]
fn only_an_owned_trial_not_waiting_for_a_window_needs_headless_service_health() {
    let s = scratch("headless-service-trial");
    let installed = s.root.join("Applications/aterm.app");
    assert!(!headless_boot_trial_pending_at(
        &s,
        11,
        &installed,
        unix_now_secs()
    ));
    arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    assert!(headless_boot_trial_pending_at(
        &s,
        11,
        &installed,
        unix_now_secs()
    ));
    assert!(!headless_boot_trial_pending_at(
        &s,
        12,
        &installed,
        unix_now_secs()
    ));
    let other = s.root.join("Another.app");
    make_app(&other, "OTHER");
    assert!(!headless_boot_trial_pending_at(
        &s,
        11,
        &other,
        unix_now_secs()
    ));
    record_window_launch(&s, 11);
    assert!(!headless_boot_trial_pending_at(
        &s,
        11,
        &installed,
        unix_now_secs()
    ));
    assert!(headless_boot_trial_pending_at(
        &s,
        11,
        &installed,
        unix_now_secs() + WINDOW_TRIAL_HEADLESS_WAIT.as_secs() + 1,
    ));
}

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

#[test]
fn recovery_only_counts_and_reverts_its_trial_without_applying_a_stage() {
    use crate::TrialWitness::Headless;
    for previous_attempts in [0, MAX_BOOT_ATTEMPTS - 1] {
        let s = scratch("boot-recovery-only");
        let installed = s.root.join("Applications").join("aterm.app");
        let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
        make_app(&s.staged_app, "STAGED-NEWER");
        std::fs::write(&s.ready, "a newer stage is ready").unwrap();
        let sentinel = boot_sentinel(&s);
        for _ in 0..previous_attempts {
            sentinel.observe_launch(11).unwrap();
        }
        backdate(&s);
        let outcome = recover_boot_trial_at(&s, 11, Headless, bundle_at(&installed), |_| {
            Ok(TrialRevert::To(VerifiedRollback {
                path: rollback.clone(),
                build: 10,
            }))
        });
        if previous_attempts == 0 {
            assert!(matches!(outcome, ApplyOutcome::NotApplicable));
            assert_eq!(sentinel.read_state(), Some((11, 1)));
            assert_eq!(read_id(&installed), "NEW");
            assert_eq!(read_id(&rollback), "OLD");
        } else {
            assert!(
                matches!(outcome, ApplyOutcome::ReExecFailed(_)),
                "the verified predecessor is restored; this fixture has no executable: {outcome:?}"
            );
            assert_eq!(read_id(&installed), "OLD");
            assert_eq!(sentinel.read_state(), None);
            assert!(crate::hold::load(&s).hold.is_some(), "the build is held");
            assert!(
                !s.staged_app.exists() && !s.ready.exists(),
                "the ordinary crash-loop revert retires the pending stage"
            );
            continue;
        }
        assert_eq!(read_id(&s.staged_app), "STAGED-NEWER");
        assert_eq!(
            std::fs::read_to_string(&s.ready).unwrap(),
            "a newer stage is ready"
        );
    }

    for trial_owner in [None, Some("another-install")] {
        let s = scratch("boot-recovery-only-unowned");
        let installed = s.root.join("Applications").join("aterm.app");
        make_app(&installed, "CURRENT");
        make_app(&s.staged_app, "STAGED-NEWER");
        std::fs::write(&s.ready, "a newer stage is ready").unwrap();
        if let Some(owner) = trial_owner {
            let owner = s.root.join(owner);
            arm_window_trial(&s, &owner, 11, &"ab".repeat(32));
        }
        let before = boot_sentinel(&s).read_state();
        let outcome = recover_boot_trial_at(&s, 11, Headless, bundle_at(&installed), |_| {
            panic!("an absent or foreign trial is never proved")
        });
        assert!(matches!(outcome, ApplyOutcome::NotApplicable));
        assert_eq!(boot_sentinel(&s).read_state(), before);
        assert_eq!(read_id(&installed), "CURRENT");
        assert_eq!(read_id(&s.staged_app), "STAGED-NEWER");
        assert!(s.ready.exists());
    }
}

#[test]
fn a_window_launch_record_answers_only_for_the_armed_trial() {
    use crate::TrialWitness::{Headless, Window};
    let s = scratch("boot-window-record-identity");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let sentinel = boot_sentinel(&s);
    count_trial_launch(&s, &sentinel, 11, None, true, Window, false).unwrap();
    assert!(matches!(
        trial_window_launch(&s, 11, unix_now_secs()),
        WindowLaunch::Waiting { .. }
    ));

    sentinel.confirm().unwrap();
    Trial::record_required(&s.trial(), 11, &"ef".repeat(32), Some(&installed)).unwrap();
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
    count_trial_launch(&s, &sentinel, 11, None, false, Headless, false).unwrap();
    assert_eq!(
        sentinel.read_state(),
        Some((11, 1)),
        "a headless launch of it counts, as before"
    );

    count_trial_launch(&s, &sentinel, 11, None, true, Window, false).unwrap();
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
    clear_trial_identity(&s);
    assert!(!trial_window_launch_path(&s).exists());
    assert!(Trial::read(&s.trial()).is_none());
    let target = s.root.join("private-write-target");
    std::fs::write(&target, "untouched").unwrap();
    let link = s.root.join("private-write-link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(write_private_file(&link, b"clobbered").is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
    let _ = std::fs::remove_dir_all(&s.root);
}

/// A private write makes its file 0600 before a byte of it changes. A file it cannot make 0600
/// (another user's, whose chmod is refused: the seam stands in for one) keeps its bytes and its
/// mode, and the refusal comes back instead of the data landing readable by others. A FIFO in a
/// record's place is refused at once. Control: a file already 0600 takes the new bytes and stays
/// 0600.
#[test]
fn a_private_write_is_private_before_its_bytes_land() {
    use std::os::unix::fs::PermissionsExt as _;
    let s = scratch("boot-private-write-mode");
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let wide = s.root.join("private-write-wide");
    std::fs::write(&wide, "before").unwrap();
    std::fs::set_permissions(&wide, std::fs::Permissions::from_mode(0o644)).unwrap();
    let refused = as_if_the_private_mode_is_refused(|| write_private_file(&wide, b"secret"));
    assert_eq!(
        refused.map_err(|e| e.raw_os_error()),
        Err(Some(libc::EPERM))
    );
    assert_eq!(std::fs::read(&wide).unwrap(), b"before");
    assert_eq!(mode(&wide), 0o644);
    write_private_file(&wide, b"secret").unwrap();
    assert_eq!(std::fs::read(&wide).unwrap(), b"secret");
    assert_eq!(mode(&wide), 0o600, "a 0644 file ends 0600");

    let private = s.root.join("private-write-private");
    std::fs::write(&private, "before").unwrap();
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
    write_private_file(&private, b"after").unwrap();
    assert_eq!(std::fs::read(&private).unwrap(), b"after");
    assert_eq!(mode(&private), 0o600);

    let fifo = s.root.join("private-write-fifo");
    let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `c_fifo` is a NUL-terminated path in this test's scratch folder.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0, "mkfifo");
    assert!(write_private_file(&fifo, b"blocked").is_err());
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn the_wait_for_a_window_ends() {
    use crate::TrialWitness::{Headless, Window};
    use std::time::Duration;
    let s = scratch("boot-window-wait-bound");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    let sentinel = boot_sentinel(&s);
    count_trial_launch(&s, &sentinel, 11, None, true, Window, false).unwrap();
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
        trial_window_launch(&s, 11, at.saturating_sub(1)),
        WindowLaunch::Lapsed,
        "a record ahead of the clock holds nothing"
    );
    assert_eq!(
        trial_window_launch(&s, 11, at.saturating_sub(wait)),
        WindowLaunch::Lapsed,
        "a record the clock cannot place holds nothing"
    );

    let stale = format!("11 {} {}\n", "ab".repeat(32), unix_now_secs() - wait - 1);
    std::fs::write(trial_window_launch_path(&s), stale).unwrap();
    count_trial_launch(&s, &sentinel, 11, None, false, Headless, false).unwrap();
    assert_eq!(sentinel.read_state(), Some((11, 2)), "counted again");
    assert!(!confirm_boot_health_at(
        &s,
        11,
        Some("abc"),
        Headless,
        bundle_at(&installed),
        Duration::from_millis(100),
    ));
    backdate(&s);
    count_trial_launch(&s, &sentinel, 11, None, true, Window, false).unwrap();
    assert!(matches!(
        trial_window_launch(&s, 11, unix_now_secs()),
        WindowLaunch::Waiting { .. }
    ));
    let _ = std::fs::remove_dir_all(&s.root);
}

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

#[test]
fn a_predecessor_this_build_cannot_verify_does_not_hold_its_trial() {
    use crate::TrialWitness::Window;
    use std::time::Duration;
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    type Identity = Result<(u64, String), String>;
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

    let s = scratch("boot-no-revert-confirm");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &digest);
    receipt_for(&s, 11, &digest);
    let confirm = |fixed: Identity| {
        let verify = verifier(&installed, fixed);
        confirm_boot_health_at_with_proof(&s, 11, Window, bundle_at(&installed), wait, |root| {
            prove_current_trial(&s, root, 11, Some(COMMIT), &verify)
                .map(|_| ())
                .map_err(|refusal| refusal.why)
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
            |root| {
                prove_current_trial(&s, root, 11, Some(COMMIT), &verify)
                    .map_err(|refusal| refusal.why)
            },
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
    assert!(Trial::read(&s.trial()).is_none());
    assert!(!rollback.exists(), "the fixed path is cleared");
    assert!(
        s.ready.exists() && read_id(&s.staged_app) == "STAGED-NEWER",
        "the newer stage is kept for this launch to apply"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

fn kept_for_this_window(s: &Staging, rollback: &Path) -> bool {
    rollback.exists()
        && KeptPredecessor::read(s).is_some_and(|kept| {
            kept.bundle == rollback && Some(kept.holder) == TrialLauncher::this_process()
        })
}

fn corrupt_sentinel(s: &Staging) {
    std::fs::write(s.root.join("boot.sentinel"), [0u8; 16]).unwrap();
    assert_eq!(
        boot_sentinel(s).read_state(),
        None,
        "PRECONDITION: the sentinel does not read"
    );
}

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
    let settled = Trial::read(&s.trial()).is_none()
        && kept_for_this_window(&s, &rollback)
        && !s.root.join("boot.sentinel").exists();
    assert!(
        confirmed && settled,
        "a confirmation is answered true only once the trial is settled: answered \
         {confirmed}, trial identity kept {}, predecessor kept for the window {}, \
         sentinel kept {}",
        Trial::read(&s.trial()).is_some(),
        kept_for_this_window(&s, &rollback),
        s.root.join("boot.sentinel").exists()
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

#[test]
fn an_unreadable_sentinel_is_settled_only_for_its_own_trial_after_its_proof() {
    use crate::TrialWitness::{Headless, Window};
    let s = scratch("boot-unreadable-sentinel-controls");
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &"ab".repeat(32));
    count_trial_launch(&s, &boot_sentinel(&s), 11, None, true, Window, false).unwrap();
    corrupt_sentinel(&s);
    let untouched = |s: &Staging| {
        Trial::read(&s.trial()).is_some() && rollback.exists() && boot_sentinel(s).unreadable()
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
        !confirm_boot_health_at_with_proof(&s, 11, Headless, bundle_at(&installed), wait, |_| {
            Err("a headless start never proves a window's trial".into())
        }),
        "a headless start of a window's trial waits for the window and runs no proof"
    );
    assert!(untouched(&s), "and leaves the trial for a window");

    assert!(
        confirm_boot_health_at_with_proof(&s, 11, Window, bundle_at(&installed), wait, |_| Ok(())),
        "the window's confirmation settles it"
    );
    assert!(Trial::read(&s.trial()).is_none() && kept_for_this_window(&s, &rollback));
    assert!(!s.root.join("boot.sentinel").exists());
    let _ = std::fs::remove_dir_all(&s.root);
}

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
    assert!(crate::hold::load(&s).hold.is_none(), "nothing is held");
    let _ = std::fs::remove_dir_all(&s.root);
}

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

/// The record a window running build 10 writes when it sees its successor of `build`'s bytes
/// `digest` stall.
fn record_start_failure(s: &Staging, build: u64, digest: &str) {
    crate::window_start_failure::record(
        s,
        10,
        crate::FailedStartBytes::Download {
            build,
            dmg_sha256: digest,
        },
        "unexplained/stalled",
        unix_now_secs(),
    )
    .unwrap();
}

/// A window launch of `build`'s trial of `digest`, as [`record_window_launch`] keeps it, at `at`.
fn window_launched_at(s: &Staging, build: u64, digest: &str, at: u64) {
    write_private_file(
        &trial_window_launch_path(s),
        format!("{build} {digest} {at}\n").as_bytes(),
    )
    .unwrap();
}

/// The window's successor of build 11, by name, as its trial counts it.
const SUCCESSOR: TrialLauncher = TrialLauncher {
    pid: 4242,
    started: (1, 2),
};

/// How the window's successor of build 11 ended, as its parent books its counted launch
/// (`forgives_the_counted_trial_launch` in aterm-gui).
#[derive(Clone, Copy, Debug)]
enum Successor {
    /// It crashed, or exited 73 because it could not make its first surface (Structural): its
    /// parent keeps the counted launch.
    Crashed,
    /// It stalled (Unobserved, booked Unexplained): its parent gives the launch back by name
    /// under the apply lock.
    StalledGivenBack,
    /// It stalled, and its parent's give-back met an apply lock held past its wait (a loaded
    /// Mac): the launch is owed back until the next holder of the lock pays it.
    StalledOwed,
}

/// The disk a window's pre-launch swap of build 11 (digest `ab`×32) and its failed successor
/// leave once the node job settled the trial headless after the window's wait lapsed, with
/// `failed` on record: the successor's launch counted and booked as `successor` says, the trial
/// disarmed, the predecessor kept, and the windows' lane recorded.
fn settled_by_the_job(
    label: &str,
    failed: Option<(u64, &str)>,
    successor: Successor,
) -> (crate::paths::ScratchStaging, PathBuf) {
    use crate::TrialWitness::{Headless, Window};
    use std::time::Duration;
    let digest = "ab".repeat(32);
    let proven = |_: &Path| Ok(());
    let s = scratch(label);
    let installed = s.root.join("Applications").join("aterm.app");
    let rollback = arm_window_trial(&s, &installed, 11, &digest);
    receipt_for(&s, 11, &digest);
    if let Some((build, bytes)) = failed {
        record_start_failure(&s, build, bytes);
    }
    let sentinel = boot_sentinel(&s);
    count_trial_launch(&s, &sentinel, 11, Some(SUCCESSOR), true, Window, false).unwrap();
    assert_eq!(
        sentinel.read_state(),
        Some((11, 1)),
        "{label}: the successor's counted launch"
    );
    let given_back = |lock_wait: Duration| {
        forgive_trial_launch_of_at(
            &s,
            11,
            SUCCESSOR.pid,
            Some(SUCCESSOR.started),
            Some(&installed),
            lock_wait,
        )
    };
    let counted = match successor {
        Successor::Crashed => 1,
        Successor::StalledGivenBack => {
            assert!(
                given_back(APPLY_LOCK_WAIT),
                "{label}: the stall is given back"
            );
            0
        }
        Successor::StalledOwed => {
            let holder = FileLock::acquire(&s.apply_lock).unwrap();
            assert!(
                given_back(Duration::from_millis(100)),
                "{label}: the give-back that met a held lock is owed"
            );
            drop(holder);
            1
        }
    };
    assert_eq!(
        sentinel.read_state(),
        Some((11, counted)),
        "{label}: the count as the parent left it"
    );
    assert!(
        window_launch_of_trial_at(&s, 11).is_some(),
        "{label}: a window launched it"
    );
    assert!(
        !confirm_boot_health_at_with_proof(
            &s,
            11,
            Headless,
            bundle_at(&installed),
            Duration::from_millis(100),
            proven,
        ),
        "{label}: the job waits for a window while the wait lasts"
    );
    assert_eq!(sentinel.read_state(), Some((11, counted)), "{label}");
    window_launched_at(
        &s,
        11,
        &digest,
        unix_now_secs() - WINDOW_TRIAL_HEADLESS_WAIT.as_secs() - 1,
    );
    assert!(
        confirm_boot_health_at_with_proof(
            &s,
            11,
            Headless,
            bundle_at(&installed),
            Duration::from_millis(100),
            proven,
        ),
        "{label}: the job settles it once the wait lapsed"
    );
    assert_eq!(sentinel.read_state(), None, "{label}: updates flow");
    assert_eq!(read_id(&rollback), "OLD", "{label}: the predecessor stays");
    (s, installed)
}

/// A window launch of build 11 whose recovery proof finds the predecessor at the rollback path.
fn window_launch_of_11(s: &Staging, installed: &Path) -> Option<ApplyOutcome> {
    let rollback = rollback_path(installed);
    check_boot_health_with_proof(
        s,
        11,
        RollbackHandoff::NONE,
        false,
        crate::TrialWitness::Window,
        std::time::Duration::from_millis(100),
        bundle_at(installed),
        move |_: &Path| {
            Ok(TrialRevert::To(VerifiedRollback {
                path: rollback,
                build: 10,
            }))
        },
    )
}

/// KI-1112-144, the installed shape: A HEADLESS SETTLE OF BYTES A WINDOW SAW FAIL KEEPS THEIR
/// WINDOW TRIAL. The window's pre-launch swap installed build 11 and its successor crashed (or
/// exited 73), a Structural death whose counted launch its parent keeps; the node job settles the
/// trial headless once the window's wait lapses, which disarms it so updates flow, and carries
/// the successor's unproven launch. THE QUIT WINDOW: a window launch a month later still puts the
/// bytes back on trial. THE RESUMED LAUNCH: that launch counts at the carried count and is not
/// swallowed as a burst. THE COUNT: the third unconfirmed launch reverts, quarantines the bytes
/// and withdraws the record, and the count carries across a second headless settle. The controls
/// at the same instant (no record, another artifact of 11, the digest under build 12) keep
/// today's lapse: nothing armed and the windows' record dropped. The burst rule's own control (a
/// launch at a carried count that is not marked resumed, its sentinel stamped fresh, is
/// swallowed) shows the flag decides THE RESUMED LAUNCH whatever the machine's load. A stall,
/// which its parent gives back, is [`a_stall_its_parent_gave_back_is_not_carried`].
/// NEGATIVE CONTROLS: the old lapse rule fails THE QUIT WINDOW; re-arming at 0 fails THE RESUMED
/// LAUNCH, which then reads `(11, 1)` and leaves THE COUNT a launch short of its revert; keeping
/// the burst rule on a resumed arm (`count_trial_launch` ignoring `resumed`) fails the cycles
/// arm's THE RESUMED LAUNCH, whose sentinel is stamped fresh just before its count (the first
/// arm's re-arm and count run inside one `check_boot_health_with_proof`, which catches a flag
/// dropped at that call site only while the two run within the 2 s burst window).
#[test]
fn a_headless_settle_of_bytes_a_window_saw_fail_keeps_their_window_trial() {
    use crate::TrialWitness::{Headless, Window};
    use std::time::Duration;
    let digest = "ab".repeat(32);
    let wait = WINDOW_TRIAL_HEADLESS_WAIT.as_secs();
    let month = 30 * 24 * 60 * 60;

    let (s, installed) =
        settled_by_the_job("boot-failed-bytes", Some((11, &digest)), Successor::Crashed);
    let sentinel = boot_sentinel(&s);
    let mut record = WindowUnproven::read(&s).expect("the windows' lane");
    assert_eq!(
        (record.build, record.digest.as_str(), record.attempts),
        (11, digest.as_str(), 1),
        "the successor's unproven launch is carried"
    );
    record.at = unix_now_secs() - wait - month;
    record.record(&s).unwrap();
    let outcome = window_launch_of_11(&s, &installed);
    assert!(
        matches!(outcome, Some(ApplyOutcome::NotApplicable)),
        "THE QUIT WINDOW: a month later a window launch is back on trial: {outcome:?}"
    );
    assert_eq!(
        sentinel.read_state(),
        Some((11, 2)),
        "THE RESUMED LAUNCH: counted on top of the carried launch"
    );
    backdate(&s);
    let outcome = window_launch_of_11(&s, &installed);
    assert!(
        matches!(outcome, Some(ApplyOutcome::ReExecFailed(_))),
        "THE COUNT: the third unconfirmed launch reverts (this fixture has no executable to \
         exec): {outcome:?}"
    );
    assert_eq!(read_id(&installed), "OLD", "the previous build is back");
    assert_eq!(
        crate::hold::held(&s, 11, &digest, unix_now_secs()).map(|hold| hold.cause),
        Some(Cause::CrashLoop),
        "the bytes are held from every lane"
    );
    assert_eq!(
        crate::window_start_failure::read(&s),
        Ok(None),
        "the crash-loop hold took the window-start failure's slot"
    );
    let _ = std::fs::remove_dir_all(&s.root);

    let other = "cd".repeat(32);
    for (label, failed) in [
        ("boot-failed-bytes-none", None),
        ("boot-failed-bytes-other", Some((11, other.as_str()))),
        ("boot-failed-bytes-build", Some((12, digest.as_str()))),
    ] {
        let (s, installed) = settled_by_the_job(label, failed, Successor::Crashed);
        let sentinel = boot_sentinel(&s);
        let mut record = WindowUnproven::read(&s).expect("the windows' lane");
        assert_eq!(record.attempts, 0, "{label}: nothing carried");
        record.at = unix_now_secs() - wait - month;
        record.record(&s).unwrap();
        assert_eq!(
            rearm_window_unproven_trial(&s, &sentinel, 11, &installed, unix_now_secs()),
            None,
            "{label}: the lapse holds"
        );
        assert!(WindowUnproven::read(&s).is_none(), "{label}: dropped");
        assert_eq!(sentinel.read_state(), None, "{label}: nothing armed");
        let _ = std::fs::remove_dir_all(&s.root);
    }

    let (s, installed) = settled_by_the_job(
        "boot-failed-bytes-cycles",
        Some((11, &digest)),
        Successor::Crashed,
    );
    let sentinel = boot_sentinel(&s);
    assert_eq!(
        rearm_window_unproven_trial(&s, &sentinel, 11, &installed, unix_now_secs()),
        Some(1)
    );
    // Stamped now, so the burst window certainly holds at the count, however loaded the Mac.
    let fresh = || {
        std::fs::File::options()
            .write(true)
            .open(s.root.join("boot.sentinel"))
            .unwrap()
            .set_modified(std::time::SystemTime::now())
            .unwrap();
    };
    fresh();
    count_trial_launch(&s, &sentinel, 11, None, true, Window, false).unwrap();
    assert_eq!(
        sentinel.read_state(),
        Some((11, 1)),
        "CONTROL: at a carried count with a fresh sentinel, a launch not marked resumed is \
         swallowed as a burst"
    );
    fresh();
    count_trial_launch(&s, &sentinel, 11, None, true, Window, true).unwrap();
    assert_eq!(sentinel.read_state(), Some((11, 2)), "THE RESUMED LAUNCH");
    window_launched_at(&s, 11, &digest, unix_now_secs() - wait - 1);
    assert!(confirm_boot_health_at_with_proof(
        &s,
        11,
        Headless,
        bundle_at(&installed),
        Duration::from_millis(100),
        |_: &Path| Ok(()),
    ));
    assert_eq!(
        WindowUnproven::read(&s).map(|record| record.attempts),
        Some(2),
        "THE COUNT: a second headless settle carries both unproven launches"
    );
    let outcome = window_launch_of_11(&s, &installed);
    assert!(
        matches!(outcome, Some(ApplyOutcome::ReExecFailed(_))),
        "THE COUNT: re-armed at 2, the next window launch is the third and reverts: {outcome:?}"
    );
    assert_eq!(read_id(&installed), "OLD");
    let _ = std::fs::remove_dir_all(&s.root);
}

/// A NEWER RELEASE DOES NOT ERASE A WINDOW'S EVIDENCE: after the node job settled build 11's
/// trial headless and carried its successor's unproven launch, a check that sees build 12
/// (`offer`) and a failed stage of it (`set_hold`) leave the window-start failure of 11's bytes
/// in place, so a window launch of 11 still re-arms at the carried count and the windowless
/// lanes still leave those bytes alone.
/// NEGATIVE CONTROL: a window-start failure kept in the hold slot (ended Newer by the offer,
/// replaced by the stage hold) re-arms at 0 and frees the bytes for a headless start.
#[test]
fn a_newer_offer_keeps_the_carried_window_trial_of_failed_bytes() {
    let digest = "ab".repeat(32);
    let (s, installed) = settled_by_the_job(
        "boot-failed-bytes-newer-offer",
        Some((11, &digest)),
        Successor::Crashed,
    );
    let now = unix_now_secs();
    let newer = crate::hold::sha(&"ef".repeat(32)).unwrap();
    crate::hold::edit(&s, "a newer release", |record| {
        record.offer(12, now);
        record.set_hold(
            update_record::Failure {
                build: 12,
                version: String::new(),
                sha: newer,
                cause: Cause::Stage,
                witness: Witness::Headless,
                why: "download failed".into(),
            },
            now,
        );
    });
    assert_eq!(
        crate::window_start_failure::holds(&s, 11, &digest),
        Ok(true),
        "the windowless lanes still leave those bytes alone"
    );
    let sentinel = boot_sentinel(&s);
    assert_eq!(
        rearm_window_unproven_trial(&s, &sentinel, 11, &installed, unix_now_secs()),
        Some(1),
        "the window re-trial resumes at the carried count"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// KI-1112-144, the installed shape as m16 met it: A STALL ITS PARENT GAVE BACK IS NOT CARRIED.
/// The window's successor of build 11 stalled, which its parent books Unexplained and gives back
/// by name: under the apply lock, or, where a loaded Mac holds that lock past the wait, owed back
/// until the next holder pays it. THE GIVE-BACK: the node job's headless settle carries nothing for
/// it, the owed give-back paid before the count is read, so a stall that may have been the
/// machine's moment is never charged for good. THE QUIT WINDOW: the bytes a window saw fail keep
/// their re-trial with no lapse all the same, re-armed at 0 a month later. THE COUNT: the third
/// person launch after the settle reverts and quarantines them.
/// NEGATIVE CONTROLS: settling without paying the owed give-back carries 1 in the owed arm (THE
/// GIVE-BACK); the old lapse rule fails THE QUIT WINDOW, arming nothing a month later.
#[test]
fn a_stall_its_parent_gave_back_is_not_carried() {
    let digest = "ab".repeat(32);
    let month = 30 * 24 * 60 * 60;
    for (label, successor) in [
        ("boot-stall-given-back", Successor::StalledGivenBack),
        ("boot-stall-owed", Successor::StalledOwed),
    ] {
        let (s, installed) = settled_by_the_job(label, Some((11, &digest)), successor);
        let sentinel = boot_sentinel(&s);
        let mut record = WindowUnproven::read(&s).expect("the windows' lane");
        assert_eq!(
            (record.build, record.digest.as_str(), record.attempts),
            (11, digest.as_str(), 0),
            "{label}: THE GIVE-BACK: the forgiven stall is not carried"
        );
        assert!(
            std::fs::read_dir(owed_give_backs_dir(&s))
                .ok()
                .is_none_or(|mut entries| entries.next().is_none()),
            "{label}: no give-back is left owed"
        );
        record.at = unix_now_secs() - WINDOW_TRIAL_HEADLESS_WAIT.as_secs() - month;
        record.record(&s).unwrap();
        for launch in 1..MAX_BOOT_ATTEMPTS {
            let outcome = window_launch_of_11(&s, &installed);
            assert!(
                matches!(outcome, Some(ApplyOutcome::NotApplicable)),
                "{label}: THE QUIT WINDOW: person launch {launch} is on trial: {outcome:?}"
            );
            assert_eq!(
                sentinel.read_state(),
                Some((11, launch)),
                "{label}: THE QUIT WINDOW: re-armed at 0, then counted"
            );
            backdate(&s);
        }
        let outcome = window_launch_of_11(&s, &installed);
        assert!(
            matches!(outcome, Some(ApplyOutcome::ReExecFailed(_))),
            "{label}: THE COUNT: the third person launch reverts: {outcome:?}"
        );
        assert_eq!(
            read_id(&installed),
            "OLD",
            "{label}: the previous build is back"
        );
        assert_eq!(
            crate::hold::held(&s, 11, &digest, unix_now_secs()).map(|hold| hold.cause),
            Some(Cause::CrashLoop),
            "{label}: the bytes are held from every lane"
        );
        let _ = std::fs::remove_dir_all(&s.root);
    }
}

/// A START-FAILURE RECORD THAT CANNOT BE READ NEITHER DROPS NOR ZEROES A RE-TRIAL: the headless
/// settle of a trial of bytes a window saw fail meets a record it cannot read (a symlink at its
/// path, standing for EMFILE or EIO). THE KEPT COUNT: the settle keeps the unproven launch, since
/// whether it counts cannot be decided then. THE FAILED READ: a window launch a month later while
/// the record still cannot be read reads it closed: it puts the bytes back on trial at the kept
/// count, the lapse ignored, as for bytes on record, and the launch is counted.
/// NEGATIVE CONTROLS: reading an unreadable record as absent at the re-arm drops the lapsed lane
/// for good, and arming nothing leaves the launch uncounted (THE FAILED READ); writing 0 for an
/// unreadable record at the settle resumes at 0 (THE KEPT COUNT).
#[test]
fn a_start_failure_record_that_cannot_be_read_neither_drops_nor_zeroes_a_re_trial() {
    use crate::TrialWitness::{Headless, Window};
    use std::time::Duration;
    let digest = "ab".repeat(32);
    let s = scratch("boot-failed-bytes-unreadable");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &digest);
    receipt_for(&s, 11, &digest);
    record_start_failure(&s, 11, &digest);
    let sentinel = boot_sentinel(&s);
    count_trial_launch(&s, &sentinel, 11, Some(SUCCESSOR), true, Window, false).unwrap();
    window_launched_at(
        &s,
        11,
        &digest,
        unix_now_secs() - WINDOW_TRIAL_HEADLESS_WAIT.as_secs() - 1,
    );
    let kept = s.root.join("record.kept");
    std::fs::rename(s.record(), &kept).unwrap();
    std::os::unix::fs::symlink(s.record(), s.record()).unwrap();
    assert!(
        crate::window_start_failure::read(&s).is_err(),
        "PRECONDITION: the record cannot be read"
    );
    assert!(confirm_boot_health_at_with_proof(
        &s,
        11,
        Headless,
        bundle_at(&installed),
        Duration::from_millis(100),
        |_: &Path| Ok(()),
    ));
    let mut record = WindowUnproven::read(&s).expect("the windows' lane");
    assert_eq!(record.attempts, 1, "THE KEPT COUNT");
    // The settle's own write replaced the record it could not read (an update record that
    // cannot be read is written afresh): the read fails again, as an EMFILE or EIO would.
    let _ = std::fs::remove_file(s.record());
    std::os::unix::fs::symlink(s.record(), s.record()).unwrap();
    record.at = unix_now_secs() - WINDOW_TRIAL_HEADLESS_WAIT.as_secs() - 30 * 24 * 60 * 60;
    record.record(&s).unwrap();
    assert_eq!(
        rearm_window_unproven_trial(&s, &sentinel, 11, &installed, unix_now_secs()),
        Some(1),
        "THE FAILED READ: back on trial at the kept count, the lapse ignored"
    );
    assert_eq!(
        sentinel.read_state(),
        Some((11, 1)),
        "THE FAILED READ: armed at the kept count"
    );
    count_trial_launch(&s, &sentinel, 11, Some(SUCCESSOR), true, Window, true).unwrap();
    assert_eq!(
        sentinel.read_state(),
        Some((11, 2)),
        "THE FAILED READ: the window launch is counted"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// THE REPORT NAMES ONLY A START FAILURE THAT BEARS ON THE RUNNING BUILD: the record has one slot
/// and nothing withdraws build 11's bytes once build 12 installs, so `update status` on 12 would
/// go on naming them. It names the record for a newer build, or for the running build's own
/// armed trial or the bytes its headless settle left awaiting a window, and for nothing else.
/// NEGATIVE CONTROLS: reporting the record always fails "an older build's bytes" and "another
/// artifact of the running build"; dropping the armed-trial or the awaiting-window arm fails
/// "its own armed trial's bytes" or "its bytes awaiting a window".
#[test]
fn the_report_names_only_a_start_failure_that_bears_on_the_running_build() {
    let s = scratch("start-failure-report");
    let installed = s.root.join("Applications").join("aterm.app");
    let (d, e) = ("ab".repeat(32), "cd".repeat(32));
    record_start_failure(&s, 11, &d);
    let reported = |current: u64| {
        window_start_failure_for_report(&s, current)
            .map(|failure| (failure.build, failure.dmg_sha256))
    };
    let on_record = Some((11, d.clone()));
    assert_eq!(reported(12), None, "an older build's bytes");
    assert_eq!(reported(10), on_record, "a newer build's bytes");
    assert_eq!(reported(11), None, "the running build, with no trial of it");
    arm_window_trial(&s, &installed, 11, &d);
    assert_eq!(reported(11), on_record, "its own armed trial's bytes");
    Trial::record_required(&s.trial(), 11, &e, Some(&installed)).unwrap();
    assert_eq!(reported(11), None, "another artifact of the running build");
    boot_sentinel(&s).confirm().unwrap();
    clear_trial_identity(&s);
    let unproven = |digest: &str| WindowUnproven {
        build: 11,
        digest: digest.to_string(),
        at: unix_now_secs(),
        attempts: 0,
        install_root: installed.clone(),
    };
    unproven(&d).record(&s).unwrap();
    assert_eq!(reported(11), on_record, "its bytes awaiting a window");
    unproven(&e).record(&s).unwrap();
    assert_eq!(
        reported(11),
        None,
        "another artifact of the running build awaiting a window"
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// A WINDOW THAT PRESENTS FAILED BYTES ENDS THE EVIDENCE: a Window confirmation of a re-trial of
/// the bytes on record withdraws the record (and the windows' lane is spent), as does a Window
/// settle of their trial at its bound; a Window confirmation of another build's trial leaves the
/// record of build 11's bytes in place.
/// NEGATIVE CONTROL: without the withdraw in `confirm_health_under_apply_lock_with_proof` the
/// record survives the first confirmation.
#[test]
fn a_window_that_presents_failed_bytes_ends_the_evidence() {
    use crate::TrialWitness::Window;
    use std::time::Duration;
    let digest = "ab".repeat(32);
    let proven = |_: &Path| Ok(());

    let (s, installed) = settled_by_the_job(
        "boot-failed-bytes-present",
        Some((11, &digest)),
        Successor::Crashed,
    );
    let sentinel = boot_sentinel(&s);
    assert_eq!(
        rearm_window_unproven_trial(&s, &sentinel, 11, &installed, unix_now_secs()),
        Some(1)
    );
    assert!(confirm_boot_health_at_with_proof(
        &s,
        11,
        Window,
        bundle_at(&installed),
        Duration::from_millis(100),
        proven,
    ));
    assert_eq!(
        crate::window_start_failure::read(&s),
        Ok(None),
        "a window of them presented"
    );
    assert!(WindowUnproven::read(&s).is_none());
    assert_eq!(sentinel.read_state(), None);
    let _ = std::fs::remove_dir_all(&s.root);

    let s = scratch("boot-failed-bytes-window-bound");
    let installed = s.root.join("Applications").join("aterm.app");
    arm_window_trial(&s, &installed, 11, &digest);
    receipt_for(&s, 11, &digest);
    record_start_failure(&s, 11, &digest);
    count_trial_launch(&s, &boot_sentinel(&s), 11, None, true, Window, false).unwrap();
    let start = 1_000_000;
    for at in [0, 1_000] {
        let verdict = ask_at(&s, &installed, Window, awake(start + at), Err("refused"));
        assert!(is_pending(&verdict), "{verdict:?}");
    }
    let verdict = ask_at(
        &s,
        &installed,
        Window,
        awake(start + span_ms()),
        Err("refused"),
    );
    assert!(is_settled_unproven(&verdict), "{verdict:?}");
    assert_eq!(
        crate::window_start_failure::read(&s),
        Ok(None),
        "a window settled them at the bound"
    );
    let _ = std::fs::remove_dir_all(&s.root);

    let s = scratch("boot-failed-bytes-other-trial");
    let installed = s.root.join("Applications").join("aterm.app");
    let newer = "cd".repeat(32);
    arm_window_trial(&s, &installed, 12, &newer);
    receipt_for(&s, 12, &newer);
    record_start_failure(&s, 11, &digest);
    assert!(confirm_boot_health_at_with_proof(
        &s,
        12,
        Window,
        bundle_at(&installed),
        Duration::from_millis(100),
        proven,
    ));
    // Build 12's window proves nothing about build 11's bytes, and a newer build running ends
    // no window-start failure: the record of build 11's bytes stays, as evidence and as a hold
    // of the windowless lanes.
    assert_eq!(
        crate::window_start_failure::names(&s, 11, &digest),
        Ok(true),
        "build 12's window leaves the record of build 11's bytes"
    );
    assert_eq!(
        crate::window_start_failure::holds(&s, 11, &digest),
        Ok(true)
    );
    let _ = std::fs::remove_dir_all(&s.root);
}

/// A SETTLING HEADLESS LAUNCH CARRIES ONLY LAUNCHES THAT DID NOT PROVE THE BYTES: two named
/// launches are counted (a window's candidate, then this process's headless launch once the
/// window's wait lapsed), and the headless settle of bytes on record carries 1, since its own
/// launch proved the bytes start headless; the settle at the bound carries the same. Without a
/// record nothing is carried.
/// NEGATIVE CONTROL: carrying the raw sentinel count records 2.
#[test]
fn a_settling_headless_launch_carries_only_launches_that_did_not_prove_the_bytes() {
    use crate::TrialWitness::{Headless, Window};
    use std::time::Duration;
    let digest = "ab".repeat(32);
    let candidate = TrialLauncher {
        pid: 4242,
        started: (1, 2),
    };
    let carried = |label: &str, failed: bool, at_the_bound: bool| -> u32 {
        let s = scratch(label);
        let installed = s.root.join("Applications").join("aterm.app");
        arm_window_trial(&s, &installed, 11, &digest);
        receipt_for(&s, 11, &digest);
        if failed {
            record_start_failure(&s, 11, &digest);
        }
        let sentinel = boot_sentinel(&s);
        count_trial_launch(&s, &sentinel, 11, Some(candidate), true, Window, false).unwrap();
        window_launched_at(
            &s,
            11,
            &digest,
            unix_now_secs() - WINDOW_TRIAL_HEADLESS_WAIT.as_secs() - 1,
        );
        backdate(&s);
        let this = TrialLauncher::this_process().expect("this process's name");
        count_trial_launch(&s, &sentinel, 11, Some(this), false, Headless, false).unwrap();
        assert_eq!(sentinel.read_state(), Some((11, 2)), "{label}");
        assert_eq!(
            sentinel.named_launches(11),
            vec![candidate.name(), this.name()],
            "{label}"
        );
        if at_the_bound {
            let start = 1_000_000;
            for at in [0, 1_000] {
                let verdict = ask_at(&s, &installed, Headless, awake(start + at), Err("refused"));
                assert!(is_pending(&verdict), "{label}: {verdict:?}");
            }
            let verdict = ask_at(
                &s,
                &installed,
                Headless,
                awake(start + span_ms()),
                Err("refused"),
            );
            assert!(is_settled_unproven(&verdict), "{label}: {verdict:?}");
        } else {
            assert!(confirm_boot_health_at_with_proof(
                &s,
                11,
                Headless,
                bundle_at(&installed),
                Duration::from_millis(100),
                |_: &Path| Ok(()),
            ));
        }
        let attempts = WindowUnproven::read(&s)
            .expect("the windows' lane")
            .attempts;
        let _ = std::fs::remove_dir_all(&s.root);
        attempts
    };
    assert_eq!(
        carried("boot-carry-failed", true, false),
        1,
        "the candidate's launch, not the settler's own"
    );
    assert_eq!(
        carried("boot-carry-failed-bound", true, true),
        1,
        "the bound's headless settle carries the same"
    );
    assert_eq!(
        carried("boot-carry-none", false, false),
        0,
        "no record: nothing carried"
    );
}
