// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn a_reaped_elsewhere_child_releases_its_waiter_without_claiming_completion() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    struct Released(mpsc::Sender<()>);
    impl Drop for Released {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
    let child = aterm_uds::spawnfd::command("/usr/bin/true")
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let mut status = 0;
        // SAFETY: this test exclusively owns this native child. Reap it outside its
        // handle to exercise spawnfd's real lost-custody latch, without signalling a PID.
        let got = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if got == pid {
            break;
        }
        assert!(
            got >= 0 || std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
        );
        assert!(Instant::now() < until, "the native child did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
    let notified = Arc::new(AtomicBool::new(false));
    let called = Arc::clone(&notified);
    let (send, receive) = mpsc::channel();
    let released = Released(send);
    hand_off_pass(child, move || {
        let _held = &released;
        called.store(true, Ordering::Release);
    });
    receive.recv_timeout(Duration::from_secs(60)).unwrap();
    assert!(!notified.load(Ordering::Acquire));
}

#[cfg(unix)]
#[test]
fn actual_terminal_children_queue_readback_in_both_window_and_setup_paths() {
    use std::sync::mpsc;
    use std::time::Duration;

    for (program, expected) in [
        ("/usr/bin/true", Ended::Ran),
        ("/usr/bin/false", Ended::Failed(Some(1))),
    ] {
        let mut notified = false;
        let result = pass_starting(
            || aterm_uds::spawnfd::command(program).spawn(),
            || notified = true,
        );
        assert_eq!(result, expected);
        assert!(
            notified,
            "a failed child may have completed part of the work too"
        );

        let child = aterm_uds::spawnfd::command(program).spawn().unwrap();
        let (send, receive) = mpsc::channel();
        hand_off_pass(child, move || send.send(()).unwrap());
        receive.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(
            receive.recv_timeout(Duration::from_secs(60)).is_err(),
            "one child has one terminal notification"
        );
    }
    let mut notified = false;
    assert!(matches!(
        pass_starting(
            || Err(std::io::ErrorKind::NotFound.into()),
            || notified = true
        ),
        Ended::Unstarted(_)
    ));
    assert!(!notified, "a failed start is no completion");
}

/// The window's lane runs for a windowed launch with no first-run setup running, outside a
/// private state root and CI; headless, a setup running now, `ATERM_STATE_HOME` or `CI` each
/// skip it.
/// NEGATIVE CONTROL: a gate that ignores `unified_setup` starts the lane beside a setup.
#[test]
fn the_window_lane_is_skipped_headless_under_setup_and_in_a_private_root_or_ci() {
    let none = |_: &str| false;
    assert_eq!(runs(false, false, &none), cfg!(unix), "an ordinary window");
    assert!(!runs(true, false, &none), "headless");
    assert!(!runs(false, true, &none), "under unified_setup");
    assert!(
        !runs(false, false, &|name| name == "ATERM_STATE_HOME"),
        "a private state root"
    );
    assert!(!runs(false, false, &|name| name == "CI"), "CI");
    assert!(
        runs(false, false, &|name| name == "HOME") || !cfg!(unix),
        "another variable does not skip it"
    );
}

/// The lane runs at once, then after the recheck or a bounded retry, and
/// logs how passes end once per change: a pass failing the same way again says nothing more.
/// NEGATIVE CONTROL: a lane that waits before its first pass, or that logs every failed
/// pass, fails here.
#[cfg(unix)]
#[test]
fn the_lane_runs_at_once_and_retries_before_the_mend_interval() {
    let events = std::cell::RefCell::new(Vec::new());
    let mut ends = [
        Ended::Ran,
        Ended::Failed(Some(1)),
        Ended::Failed(Some(1)),
        Ended::Ran,
    ]
    .into_iter();
    let mut logged = Vec::new();
    let mut waits = Vec::new();
    lane(
        &mut |trigger, _| {
            events.borrow_mut().push(format!("pass {trigger}"));
            ends.next().expect("a scripted pass")
        },
        &mut |line| logged.push(line.to_owned()),
        &mut |every| {
            waits.push(every);
            let mut events = events.borrow_mut();
            events.push("wait".to_owned());
            (events.iter().filter(|event| *event == "wait").count() < 4)
                .then_some(LaneWake::Network(crate::network_moved::Woke::Timer))
        },
    );
    assert_eq!(
        *events.borrow(),
        [
            "pass window",
            "wait",
            "pass window",
            "wait",
            "pass window",
            "wait",
            "pass window",
            "wait"
        ]
    );
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert!(logged[0].contains("exit 1"), "{logged:?}");
    assert_eq!(
        waits,
        [
            crate::secrets_join::MEND_EVERY,
            std::time::Duration::from_millis(aterm_secrets::converge::FIRST_RETRY_MS),
            std::time::Duration::from_millis(2 * aterm_secrets::converge::FIRST_RETRY_MS),
            crate::secrets_join::MEND_EVERY,
        ]
    );

    // The real mixed wait hears identity input independently of a network event. Changes
    // arriving while the synchronous child runs remain queued, and a refused start retains
    // Repair through the existing retry rather than forcing remote repository freshness.
    use aterm_secrets::client::{ConvergenceInputs, ConvergenceWake, IdentityDebt};
    use aterm_secrets::converge::RepositoryWork;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };
    let source = Arc::new(AtomicU64::new(0));
    let observed = Arc::clone(&source);
    let mut seen = 0;
    let mut inputs =
        ConvergenceInputs::new(Box::new(|_| None)).with_identity(Box::new(move || {
            let current = observed.load(Ordering::SeqCst);
            if current == seen {
                IdentityDebt::None
            } else {
                seen = current;
                IdentityDebt::Changed
            }
        }));
    let moved = Arc::new(crate::network_moved::Moved::new());
    let mut network = moved.watch();
    let mut wake = ConvergenceWake::default();
    let calls = std::cell::Cell::new(0);
    let mut policies = Vec::new();
    let mut retry_wait = None;
    lane(
        &mut |_, work| {
            policies.push(work);
            let call = calls.get();
            calls.set(call + 1);
            if call == 0 || call == 2 {
                source.fetch_add(1, Ordering::SeqCst);
                ConvergenceInputs::identity_changed();
            }
            if call == 1 {
                Ended::Unstarted("injected dispatch refusal".into())
            } else {
                Ended::Ran
            }
        },
        &mut |_| {},
        &mut |every| match calls.get() {
            1 | 3 => until_moved(&mut network, &mut wake, &mut inputs, every),
            2 => {
                retry_wait = Some(every);
                Some(LaneWake::Network(crate::network_moved::Woke::Timer))
            }
            _ => None,
        },
    );
    assert_eq!(
        policies,
        [
            RepositoryWork::Refresh,
            RepositoryWork::Repair,
            RepositoryWork::Repair,
            RepositoryWork::Repair
        ]
    );
    assert_eq!(
        retry_wait,
        Some(std::time::Duration::from_millis(
            aterm_secrets::converge::FIRST_RETRY_MS
        ))
    );

    let mut calls = 0;
    let mut waits = 0;
    lane(
        &mut |_, work| {
            assert_eq!(
                work,
                RepositoryWork::Refresh,
                "identity debt preserves a queued stronger refresh"
            );
            calls += 1;
            if calls == 1 {
                Ended::Unfinished
            } else {
                Ended::Ran
            }
        },
        &mut |_| {},
        &mut |_| {
            waits += 1;
            (waits == 1).then_some(LaneWake::Inputs(RepositoryWork::Repair))
        },
    );
    assert_eq!(calls, 2);
}

/// A Git readiness wait retries on unchanged inputs without a failure log. Once the
/// prerequisite arrives and the pass settles, only the ordinary recheck remains.
#[cfg(unix)]
#[test]
fn unfinished_work_retries_quietly_until_its_prerequisite_is_ready() {
    let first = std::time::Duration::from_millis(aterm_secrets::converge::FIRST_RETRY_MS);
    for (ends, expected) in [
        (
            vec![
                Ended::Unfinished,
                Ended::Held,
                Ended::Unfinished,
                Ended::Ran,
            ],
            vec![first, first, 2 * first, crate::secrets_join::MEND_EVERY],
        ),
        (
            vec![Ended::Held, Ended::Ran],
            vec![first, crate::secrets_join::MEND_EVERY],
        ),
    ] {
        let total = ends.len();
        let passes = std::cell::Cell::new(0);
        let mut ends = ends.into_iter();
        let mut waits = Vec::new();
        let mut logged = Vec::new();
        lane(
            &mut |_, _| {
                passes.set(passes.get() + 1);
                ends.next().unwrap()
            },
            &mut |line| logged.push(line.to_owned()),
            &mut |every| {
                waits.push(every);
                (passes.get() < total)
                    .then_some(LaneWake::Network(crate::network_moved::Woke::Timer))
            },
        );
        assert_eq!(waits, expected);
        assert!(logged.is_empty(), "{logged:?}");
    }
}

/// A route change this window's route source hears ends the lane's wait at once: a second
/// pass runs, and aterm.log says the network moved and which source heard it.
/// NEGATIVE CONTROL: unhook the lane's wait from the signal (`until_moved` that only sleeps),
/// or the source from the route socket's stand-in: the wait runs to its hang detector, the
/// wake reason reads `Timer`, and no line names the route change.
#[cfg(unix)]
#[test]
fn the_converge_lane_wakes_on_a_route_change_and_says_why() {
    use crate::network_moved::{Moved, Woke, spawn_source, tests::HANG};
    let moved = std::sync::Arc::new(Moved::new());
    let mut watch = moved.watch();
    let (route, routes) = std::sync::mpsc::channel::<()>();
    assert!(spawn_source(
        "test-routes",
        std::sync::Arc::clone(&moved),
        (Woke::Route, std::time::Duration::ZERO),
        move || routes.recv().ok().map(|()| true)
    ));
    let passes = std::cell::Cell::new(0);
    let woke = std::cell::Cell::new(None);
    let mut input_wake = aterm_secrets::client::ConvergenceWake::default();
    let mut inputs = aterm_secrets::client::ConvergenceInputs::new(Box::new(|_| None));
    let mut logged = Vec::new();
    lane(
        &mut |trigger, _| {
            assert_eq!(trigger, aterm_secrets::converge::WINDOW);
            passes.set(passes.get() + 1);
            if passes.get() == 1 {
                route.send(()).expect("the route source runs");
            }
            Ended::Ran
        },
        &mut |line| logged.push(line.to_owned()),
        &mut |every| {
            if passes.get() > 1 {
                return None;
            }
            assert_eq!(every, crate::secrets_join::MEND_EVERY);
            let reason = until_moved(&mut watch, &mut input_wake, &mut inputs, HANG.min(every));
            woke.set(reason);
            reason
        },
    );
    assert_eq!(
        woke.get(),
        Some(LaneWake::Network(Woke::Route)),
        "{logged:?}"
    );
    assert_eq!(passes.get(), 2);
    assert_eq!(
        logged,
        ["converge: the window's pass runs now: the network moved (a route change)"]
    );
}

/// The Secrets stage's pass names the `setup` trigger, and one that does not start is no
/// panic.
/// NEGATIVE CONTROL: a start that names the window's trigger fails here.
#[cfg(unix)]
#[test]
fn the_secrets_stage_starts_a_setup_pass() {
    let mut asked = Vec::new();
    after_secrets(&mut |trigger| {
        asked.push(trigger.to_owned());
        Err(std::io::Error::other("no such file"))
    });
    assert_eq!(asked, [aterm_secrets::converge::SETUP]);
}

/// A pass that ran, or met another pass's lock, says nothing (its runner logs its own line);
/// one that failed or did not start says so.
#[test]
fn only_a_pass_that_did_not_run_right_is_logged_by_the_lane() {
    assert_eq!(ended_line(&Ended::Ran), None);
    assert_eq!(ended_line(&Ended::Held), None);
    assert_eq!(ended_line(&Ended::Unfinished), None);
    assert!(ended_line(&Ended::Failed(Some(1))).is_some_and(|line| line.contains("exit 1")));
    assert!(
        ended_line(&Ended::Unstarted("no such file".to_owned()))
            .is_some_and(|line| line.contains("no such file"))
    );
}
