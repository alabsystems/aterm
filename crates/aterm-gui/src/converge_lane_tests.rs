// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// [`lane`] for a test of its pacing: `pass` takes no start note, and the clock is the real one,
/// as the lane's was before its passes were noted.
#[cfg(unix)]
fn lane_plain(
    pass: &mut dyn FnMut(&str, aterm_secrets::converge::RepositoryWork) -> Ended,
    log: &mut dyn FnMut(&str),
    wait: &mut dyn FnMut(std::time::Duration) -> Option<LaneWake>,
) {
    let clock = std::time::Instant::now();
    lane(
        &mut |trigger, work, _| pass(trigger, work),
        log,
        wait,
        &mut || u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
}

/// EACH WINDOW PASS IS STARTED UNDER A NOTE THAT SAYS WHY, IN FULL: the first pass is `first`;
/// a timer names how the pass before it ended (`timer` after one that ran, `retry-failed`,
/// `retry-unfinished`); a network move is `network` and an input its work, with what moved the
/// inputs' mark and in how many looks. Each note is its own start's alone: nothing of one is
/// carried to the next, so the eight notes are exactly those below.
/// NEGATIVE CONTROLS: a timer read as `timer` however the last pass ended makes the third note
/// `c=timer`; a lane that keeps the last note's classes makes the eighth `c=timer,m=config,n=2`.
#[cfg(unix)]
#[test]
fn each_window_pass_is_started_under_a_note_of_its_causes() {
    use crate::network_moved::Woke;
    use aterm_secrets::converge::MarkMoved;
    use aterm_secrets::converge::RepositoryWork::Repair;
    let mut ends = [
        Ended::Ran,
        Ended::Failed(Some(1)),
        Ended::Unfinished,
        Ended::Ran,
        Ended::Ran,
        Ended::Ran,
        Ended::Ran,
        Ended::Ran,
    ]
    .into_iter();
    let mut wakes = [
        LaneWake::Network(Woke::Timer),
        LaneWake::Network(Woke::Timer),
        LaneWake::Network(Woke::Timer),
        LaneWake::Inputs(Repair, MarkMoved::default(), 0),
        LaneWake::Network(Woke::Route),
        LaneWake::Inputs(Repair, MarkMoved::CONFIG, 2),
        LaneWake::Network(Woke::Timer),
    ]
    .into_iter();
    let mut notes = Vec::new();
    lane(
        &mut |_, _, note| {
            notes.push(note.word());
            ends.next().expect("a scripted pass")
        },
        &mut |_| {},
        &mut |_| wakes.next(),
        &mut || 0,
    );
    assert_eq!(
        notes,
        [
            "c=first",
            "c=timer",
            "c=retry-failed",
            "c=retry-unfinished",
            "c=input-repair",
            "c=network",
            "c=input-repair,m=config,n=2",
            "c=timer",
        ]
    );
}

/// THE WINDOW'S OWN PASS STARTS ITS RUNNER UNDER THE NOTE THE LANE GAVE IT: the live lane's
/// pass (`window_pass`, the one `spawn_lane` hands the lane) hands that note to `pass`, which
/// builds its runner inside it (`aterm_secrets::converge::noting`), so the runner reads the
/// note's word in `aterm_secrets::converge::START_NOTE_ENV`, as `aterm init --converge` does, and
/// the pass ends as its runner did.
/// NEGATIVE CONTROLS: a `pass` that starts its runner outside `noting` leaves the variable
/// unset: the runner writes `""` where `"c=input-repair,m=index,n=1"` is wanted. A `window_pass`
/// that hands `pass` `StartNote::default()` in place of the lane's note does the same: `""` where
/// that word is wanted.
#[cfg(unix)]
#[test]
fn the_window_pass_starts_its_runner_under_its_note() {
    use aterm_secrets::converge::{Causes, MarkMoved, RepositoryWork, START_NOTE_ENV, StartNote};
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let said = dir.path().join("said");
    let runner = dir.path().join("runner");
    let sh = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/sh"));
    std::fs::write(
        &runner,
        format!(
            "#!{}\nprintf '%s' \"${START_NOTE_ENV}\" > '{}'\nexit 0\n",
            sh.display(),
            said.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();
    let note = StartNote {
        causes: Causes::INPUT_REPAIR,
        moved: MarkMoved::INDEX,
        moves: 1,
    };
    let ended = window_pass(&runner)(
        aterm_secrets::converge::WINDOW,
        RepositoryWork::Refresh,
        &note,
    );
    assert_eq!(ended, Ended::Ran);
    assert_eq!(
        std::fs::read_to_string(&said).unwrap(),
        "c=input-repair,m=index,n=1"
    );
}

/// A WINDOW PASS THAT MEETS THE LOCK ANOTHER PROCESS HOLDS WAITS FOR IT AND RUNS ONCE IT IS LET
/// GO: the live lane's `pass` starts one runner, which waits for `align.lock` in the kernel as
/// `aterm init --converge --trigger window` does (`aterm_machines::sync::wait_lock`; here
/// `lockf`, which takes the same BSD lock on the same file). While the holder keeps it the lane
/// stays in that one pass: no second runner starts and no retry is waited out. Once it is let
/// go the runner takes the lock and ends, the pass ran, and the lane's next wait is the
/// recheck ([`crate::secrets_join::MEND_EVERY`]), not a retry five seconds on.
/// NEGATIVE CONTROL: a runner that ends at once for the held lock (exit 75, as each did before
/// runners waited) ends the pass while the lock is still held, and the lane's next wait is a
/// retry ([`aterm_secrets::converge::FIRST_RETRY_MS`]) where the recheck is wanted.
#[cfg(target_vendor = "apple")]
#[test]
fn a_window_pass_meeting_a_held_lock_waits_for_it_and_runs_once_it_is_let_go() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = aterm_tempfile::tempdir().unwrap();
    let converge = dir.path().join("converge");
    aterm_uds::private_file::ensure_private_dir(&converge).unwrap();
    let holder = aterm_uds::private_file::PrivateLock::take(
        &converge,
        "align.lock",
        std::time::Duration::ZERO,
        "held by the test",
    )
    .expect("the lock is free");
    let started = dir.path().join("started");
    let runner = dir.path().join("runner");
    let stand_in = |path: &str| aterm_uds::deadline::stand_in(std::path::Path::new(path));
    std::fs::write(
        &runner,
        format!(
            "#!{}\necho started >> '{}'\nexec {} -k '{}' {}\n",
            stand_in("/bin/sh").display(),
            started.display(),
            stand_in("/usr/bin/lockf").display(),
            converge.join("align.lock").display(),
            stand_in("/usr/bin/true").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();
    let starts = || {
        std::fs::read_to_string(&started)
            .map(|said| said.lines().count())
            .unwrap_or(0)
    };
    std::thread::scope(|scope| {
        let lane_thread = scope.spawn(|| {
            let mut ends = Vec::new();
            let mut waits = Vec::new();
            lane(
                &mut |trigger, work, note| {
                    let ended = pass(&runner, trigger, work, note);
                    ends.push(ended.clone());
                    ended
                },
                &mut |_| {},
                &mut |every| {
                    waits.push(every);
                    None
                },
                &mut || 0,
            );
            (ends, waits)
        });
        aterm_spec::wait::until_while_descendants_work("the window's runner starts", || {
            (starts() == 1).then_some(())
        });
        assert!(
            !lane_thread.is_finished(),
            "the pass waits for the lock while it is held"
        );
        assert_eq!(starts(), 1, "and starts no second runner");
        drop(holder);
        aterm_spec::wait::until_while_descendants_work(
            "the runner takes the lock once it is let go and ends",
            || lane_thread.is_finished().then_some(()),
        );
        let (ends, waits) = lane_thread.join().expect("the lane");
        assert_eq!(ends, [Ended::Ran], "one pass, which ran");
        assert_eq!(waits, [crate::secrets_join::MEND_EVERY], "then the recheck");
    });
    assert_eq!(starts(), 1);
}

/// AN INPUT WAKE CARRIES WHAT MOVED THE INPUTS' MARK: the live lane's wait (`until_moved`)
/// returns an input look's work with the classes and the number of looks its snapshot took
/// (`aterm_secrets::client::ConvergenceInputs::with_moved`), which the lane puts in the next
/// pass's note.
/// NEGATIVE CONTROL: an `until_moved` that drops them returns `Inputs(Repair, MarkMoved(0), 0)`
/// where `Inputs(Repair, MarkMoved(36), 2)` is wanted.
#[cfg(unix)]
#[test]
fn an_input_wake_carries_what_moved_the_inputs_mark() {
    use aterm_secrets::client::{ConvergenceInputs, ConvergenceWake};
    use aterm_secrets::converge::{MarkMoved, RepositoryRequest, RepositoryWork};
    let both = MarkMoved::CONFIG.with(MarkMoved::INDEX);
    let mut inputs = ConvergenceInputs::new(Box::new(|_| Some(RepositoryRequest::Repair)))
        .with_moved(Box::new(move || (both, 2)));
    let moved = std::sync::Arc::new(crate::network_moved::Moved::new());
    let mut network = moved.watch();
    let mut wake = ConvergenceWake::default();
    // An input event since the wake was made: the wait looks at once.
    ConvergenceInputs::identity_changed();
    assert_eq!(
        until_moved(
            &mut network,
            &mut wake,
            &mut inputs,
            std::time::Duration::from_secs(60)
        ),
        Some(LaneWake::Inputs(RepositoryWork::Repair, both, 2))
    );
}

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
    lane_plain(
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
    lane_plain(
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
    lane_plain(
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
            (waits == 1).then_some(LaneWake::Inputs(
                RepositoryWork::Repair,
                aterm_secrets::converge::MarkMoved::default(),
                0,
            ))
        },
    );
    assert_eq!(calls, 2);
}

/// A Git readiness wait retries on unchanged inputs without a failure log, backing off. Once
/// the prerequisite arrives and the pass settles, only the ordinary recheck remains; a pass that
/// ran at once (its runner having waited for the lock, or not) owes nothing sooner.
/// NEGATIVE CONTROL: a lane that retries every unfinished pass after the first pause waits
/// `[first, first, …]` where `[first, 2 × first, …]` is wanted.
#[cfg(unix)]
#[test]
fn unfinished_work_retries_quietly_until_its_prerequisite_is_ready() {
    let first = std::time::Duration::from_millis(aterm_secrets::converge::FIRST_RETRY_MS);
    for (ends, expected) in [
        (
            vec![Ended::Unfinished, Ended::Unfinished, Ended::Ran],
            vec![first, 2 * first, crate::secrets_join::MEND_EVERY],
        ),
        (vec![Ended::Ran], vec![crate::secrets_join::MEND_EVERY]),
    ] {
        let total = ends.len();
        let passes = std::cell::Cell::new(0);
        let mut ends = ends.into_iter();
        let mut waits = Vec::new();
        let mut logged = Vec::new();
        lane_plain(
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
    lane_plain(
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

/// A pass that ran says nothing (its runner logs its own line); one that failed or did not
/// start says so.
#[test]
fn only_a_pass_that_did_not_run_right_is_logged_by_the_lane() {
    assert_eq!(ended_line(&Ended::Ran), None);
    assert_eq!(ended_line(&Ended::Unfinished), None);
    assert!(ended_line(&Ended::Failed(Some(1))).is_some_and(|line| line.contains("exit 1")));
    assert!(
        ended_line(&Ended::Unstarted("no such file".to_owned()))
            .is_some_and(|line| line.contains("no such file"))
    );
}
