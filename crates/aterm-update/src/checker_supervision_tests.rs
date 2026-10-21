// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE CHECK LOOP CAN NEVER STALL SILENTLY. These tests drive the REAL loop
//! ([`crate::run_checker`]) and the real lock gate ([`crate::checker_gate`]), asked
//! once ([`the_gate_asked_once`]): a held lock is the deferral the bounded wait ends in,
//! and the bound itself is `FileLock::acquire_within`'s, tested in aterm-update-core.
//!
//! Every loop-level test ends its thread through the generation token — the
//! replacement mechanism itself — from inside the settings hook, and none ever
//! reaches the network: the tests that take the check lane stop at a held
//! `checker.lock` or at a sibling's fresh receipt in a scratch staging root, or
//! find the lane already held. The per-user staging root is never read. Each loop
//! takes a check lane of its own ([`args`]), as each process has one, so the tests
//! run side by side.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::checker_watch::{CheckerPhase, CheckerWatch, standing_stall};
use crate::{CheckerArgs, CheckerExit, Source};

fn source() -> Source {
    Source {
        owner: "alabsystems".into(),
        repo: "aterm".into(),
    }
}

/// Loop arguments around `provider`, with the pauses a test can afford, no
/// staging root at all, and a check lane of the loop's own: the process's one
/// (`crate::check_lane`) stands for every manual check, and a loop sharing it
/// with another test's would queue behind it.
fn args(provider: crate::SourceProvider) -> CheckerArgs {
    CheckerArgs {
        current_build: 1,
        source_provider: provider,
        health_hook: std::sync::Mutex::new(None),
        staged_hook: std::sync::Mutex::new(None),
        settings_retry: Duration::ZERO,
        staging: Arc::new(|| None),
        checker_lock_wait: Duration::from_millis(20),
        checker_ungated_after: crate::checker_watch::CHECKER_UNGATED_AFTER,
        lane: Box::leak(Box::new(crate::check_lane::Lane::new())),
        gate: the_gate_asked_once(),
        // Never the network: a check that finds nothing to do.
        check: Arc::new(|_, _| Ok(None)),
        pause: Some(Duration::ZERO),
        // Nothing here posts to it; the wake test brings its own.
        wake: &UNPOSTED,
        // A wake's settle is a pause no assertion here can see.
        wake_settle: Duration::from_millis(10),
    }
}

/// The real gate ([`crate::checker_gate`]) with no wait: one try, so a held lock defers
/// at once, where the loop's bounded wait would defer after it. What these tests pin is
/// what a cycle does with the gate's answer, never how long the wait took.
fn the_gate_asked_once() -> Arc<crate::GateFn> {
    Arc::new(|path, _wait| crate::checker_gate(path, Duration::ZERO))
}

/// Runs the loop to its exit on a thread of its own and hands back the lines it
/// logged there. The wait is a hang detector, never a latency budget: a cycle that
/// parks (on a sibling's lock, behind the lane) fails its test within the minute
/// instead of holding the binary open.
fn run_to_exit(
    watch: &'static CheckerWatch,
    generation: u64,
    args: CheckerArgs,
    must: &str,
) -> (CheckerExit, Vec<(aterm_log::Level, String)>) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = crate::log_capture::take();
        let exit = crate::run_checker(watch, generation, &args);
        let _ = tx.send((exit, crate::log_capture::take()));
    });
    rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).expect(must)
}

/// The wake signal of every test that does not post one.
static UNPOSTED: crate::cadence::WakeSignal = crate::cadence::WakeSignal::new();

/// A POSTED WAKE ENDS THE LOOP'S WAIT, AND THE CHECK AFTER IT KNOWS. The window posts
/// `NSWorkspaceDidWakeNotification` to the loop's signal; the loop's wait — here a
/// ten-minute pause no check outlives within this test's one-minute hang detector —
/// ends at once, the loop settles (`wake_settle`) and checks again, and that
/// check runs inside the wake's grace (`check_follows_wake`, which books a failed
/// asset GET as `network`). The post lands during the first check, as a lid closed
/// mid-check posts it, and still ends the wait after it. Negative controls: the check
/// before the wake is not after one; a pause that ignores the signal (the shape
/// before the post existed) never reaches the second check; and a check run without
/// the loop's wake scope reads `false`.
#[test]
fn a_posted_wake_ends_the_wait_and_the_next_check_follows_it() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    static WAKE: crate::cadence::WakeSignal = crate::cadence::WakeSignal::new();
    let generation = WATCH.register(1_000);
    let followed: Arc<Mutex<Vec<bool>>> = Arc::default();
    let mut loop_args = args(Arc::new(|| Some(source())));
    loop_args.pause = Some(Duration::from_secs(600));
    loop_args.wake = &WAKE;
    loop_args.check = {
        let followed = Arc::clone(&followed);
        Arc::new(move |_, _| {
            let mut seen = followed.lock().unwrap();
            seen.push(crate::check_follows_wake());
            match seen.len() {
                // The lid closes and opens while this check runs.
                1 => WAKE.post(),
                _ => {
                    WATCH.supersede(generation);
                }
            }
            Ok(None)
        })
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::run_checker(&WATCH, generation, &loop_args));
    });
    // A hang detector, not a latency budget: the second check is the settle away.
    let exit = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    let followed = followed.lock().unwrap().clone();
    assert_eq!(
        exit.ok(),
        Some(CheckerExit::Superseded {
            phase: CheckerPhase::Checking
        }),
        "the posted wake must end the ten-minute wait: checks seen {followed:?}"
    );
    assert_eq!(
        followed,
        [false, true],
        "only the check after the wake follows it"
    );
    assert!(!crate::check_follows_wake(), "no scope, no wake");
}

/// A CHECK THAT FAILS INSIDE A WAKE'S GRACE IS A NETWORK STILL COMING UP, whatever it
/// failed on: the loop dates the wake its wait ended on and books that failure offline
/// (`cadence::after_wake` into `cadence::note_failure`) — the quick rung, said at INFO —
/// where the same kind of failure with no wake behind it lengthens the backoff and is
/// warned. Driven through the real loop: the first check fails before any wake while the
/// lid closes and opens (a post), the loop's wait ends on that post and settles, and the
/// second check fails inside the grace. The loop's own lines are read on its thread
/// (`log_capture` is per thread), from the settings hook of the cycle after. Negative
/// controls: a loop that books the failure by the channel alone, or that never dates the
/// wake it settled, warns the second failure too.
#[test]
fn a_check_that_fails_inside_a_wakes_grace_is_booked_offline() {
    const BEFORE: &str = "curl: (35) TLS connect error before the lid closed";
    const AFTER: &str = "curl: (35) TLS connect error after the lid opened";
    static WATCH: CheckerWatch = CheckerWatch::new();
    static WAKE: crate::cadence::WakeSignal = crate::cadence::WakeSignal::new();
    let generation = WATCH.register(1_000);
    let lines: Arc<Mutex<Vec<(aterm_log::Level, String)>>> = Arc::default();
    let mut loop_args = args({
        let (lines, asked) = (Arc::clone(&lines), Arc::new(AtomicU64::new(0)));
        Arc::new(move || {
            if asked.fetch_add(1, Ordering::SeqCst) == 2 {
                *lines.lock().unwrap() = crate::log_capture::take();
                WATCH.supersede(generation);
            }
            Some(source())
        })
    });
    // Short: the wait after the second check ends on its own. The first ends on the post.
    loop_args.pause = Some(Duration::from_millis(10));
    loop_args.wake = &WAKE;
    loop_args.check = {
        let checks = Arc::new(AtomicU64::new(0));
        Arc::new(move |_, _| {
            if checks.fetch_add(1, Ordering::SeqCst) == 0 {
                // The lid closes and opens while this check runs.
                WAKE.post();
                Err(BEFORE.to_string())
            } else {
                Err(AFTER.to_string())
            }
        })
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::run_checker(&WATCH, generation, &loop_args));
    });
    // A hang detector, not a latency budget: the second check is the settle away.
    let exit = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    let lines = lines.lock().unwrap().clone();
    assert_eq!(
        exit.ok(),
        Some(CheckerExit::Superseded {
            phase: CheckerPhase::Settings
        }),
        "the loop must reach a third cycle: {lines:?}"
    );
    let said = |level: aterm_log::Level, message: &str| {
        lines
            .iter()
            .any(|(at, line)| *at == level && line == &format!("update check failed: {message}"))
    };
    assert!(
        said(aterm_log::Level::Warn, BEFORE),
        "a failure with no wake behind it is warned: {lines:?}"
    );
    assert!(
        said(aterm_log::Level::Info, AFTER),
        "a failure inside the wake's grace is said at INFO: {lines:?}"
    );
    assert!(
        !said(aterm_log::Level::Warn, AFTER),
        "and is not warned: {lines:?}"
    );
}

/// A WAKE THAT ENDS THE WAIT AFTER A PANICKED CYCLE IS SETTLED AND ANSWERED, as at
/// every other wait. The lid closes and opens while a cycle runs, and the cycle
/// panics; the backed-off wait after the panic — ten minutes here, which nothing
/// outlives within this test's one-minute hang detector — ends on the post, the loop
/// settles, and the next check runs inside the wake's grace (`check_follows_wake`).
/// A post that wait left unanswered would end every later wait at once, so a cycle
/// that kept panicking would spin with no wait at all. Negative control: the loop
/// that dropped that wait's answer runs the next check outside any wake.
#[test]
fn a_wake_that_ends_the_wait_after_a_panic_is_answered() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    static WAKE: crate::cadence::WakeSignal = crate::cadence::WakeSignal::new();
    let generation = WATCH.register(1_000);
    let asked = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&asked);
    let mut loop_args = args(Arc::new(move || {
        if counted.fetch_add(1, Ordering::SeqCst) == 0 {
            WAKE.post();
            panic!("injected checker panic after a wake");
        }
        Some(source())
    }));
    loop_args.pause = Some(Duration::from_secs(600));
    loop_args.wake = &WAKE;
    let followed: Arc<Mutex<Vec<bool>>> = Arc::default();
    loop_args.check = {
        let followed = Arc::clone(&followed);
        Arc::new(move |_, _| {
            followed.lock().unwrap().push(crate::check_follows_wake());
            WATCH.supersede(generation);
            Ok(None)
        })
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::run_checker(&WATCH, generation, &loop_args));
    });
    // A hang detector, not a latency budget: the check is the settle away.
    let exit = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    let followed = followed.lock().unwrap().clone();
    assert_eq!(
        exit.ok(),
        Some(CheckerExit::Superseded {
            phase: CheckerPhase::Checking
        }),
        "the post must end the wait after the panic: checks seen {followed:?}"
    );
    assert_eq!(
        followed,
        [true],
        "the wake that ended that wait was settled"
    );
}

/// THE LOOP ITSELF DEFERS ON A HELD `checker.lock`, SAYS SO ON THE STREAK'S EDGES,
/// AND RESUMES THE CYCLE THE LOCK COMES FREE. This drives [`crate::run_checker`]
/// through the real gate, in a scratch staging root, with a sibling holding the lock
/// for [`crate::CHECKER_DEFERRAL_LOG_EVERY`] cycles, so the streak is restated once.
/// Put the loop back on the blocking `FileLock::acquire` (the 4.6-day shape) and its
/// first cycle parks on the sibling's lock: the hang detector gives up and the test
/// fails there. Drop the deferral's streak log, its published count or the recovery
/// line, and the assertions after it fail.
#[test]
fn the_loop_defers_each_cycle_a_sibling_holds_the_checker_lock_and_resumes_when_it_lets_go() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    let generation = WATCH.register(1_000);
    let staging = crate::paths::Staging::scratch("checker-loop-deferral");
    let lock_path = staging.status.with_file_name("checker.lock");
    let sibling = Arc::new(Mutex::new(Some(
        aterm_update_core::FileLock::acquire(&lock_path).expect("the sibling takes it"),
    )));
    // A sibling mid-check that writes nothing: no progress to read.
    stamp_an_hour_ago(&lock_path);
    let every = crate::CHECKER_DEFERRAL_LOG_EVERY;
    let calls = Arc::new(AtomicU64::new(0));
    // Observations from inside the hook are COUNTED, not asserted there (the loop
    // catches a panicking cycle).
    let wrong = Arc::new(AtomicU64::new(0));
    let provider: crate::SourceProvider = {
        let (calls, wrong, sibling, staging) = (
            Arc::clone(&calls),
            Arc::clone(&wrong),
            Arc::clone(&sibling),
            staging.clone(),
        );
        Arc::new(move || {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let beat = WATCH.snapshot();
            if n <= every + 1 {
                // Every cycle so far deferred, and published its running count.
                if beat.is_none_or(|beat| beat.deferrals != n - 1 || beat.checks != 0) {
                    wrong.fetch_add(1, Ordering::SeqCst);
                }
            }
            if n == every + 1 {
                // The sibling finishes its check: it lets go of the lock and leaves
                // its receipt, so the next cycle gets the lock and skips the
                // network as the machine's check is already done. The next cycle
                // asks the gate once, no wait at all, so this relies on the release
                // being deterministic: `FileLock`'s drop is `LOCK_UN`.
                drop(sibling.lock().unwrap().take());
                crate::check_receipt::record(&staging, 1, &source(), false, None);
            }
            if n == every + 2 {
                // The streak ended, and the sibling's check counted as this loop
                // checking.
                if beat.is_none_or(|beat| beat.deferrals != 0 || beat.checks != 1) {
                    wrong.fetch_add(1, Ordering::SeqCst);
                }
                WATCH.supersede(generation);
            }
            Some(source())
        })
    };
    let mut loop_args = args(provider);
    loop_args.staging = {
        let staging = staging.clone();
        Arc::new(move || Some(staging.clone()))
    };
    // This test pins the deferral streak itself — its lines, its published
    // count, its end — over more cycles than the loop now defers before it
    // checks without the lock; that bound has its own test
    // (`a_checker_lock_held_forever_does_not_stop_checks`).
    loop_args.checker_ungated_after = u64::MAX;
    let (exit, lines) = run_to_exit(
        &WATCH,
        generation,
        loop_args,
        "the loop must defer each cycle, not park on the sibling's lock",
    );
    assert_eq!(
        exit,
        CheckerExit::Superseded {
            phase: CheckerPhase::Settings
        }
    );
    assert_eq!(calls.load(Ordering::SeqCst), every + 2);
    assert_eq!(
        wrong.load(Ordering::SeqCst),
        0,
        "each deferral was published as it happened, and cleared when it ended"
    );
    let said: Vec<_> = lines
        .iter()
        .filter(|(_, line)| {
            line.starts_with("update check deferred") || line.starts_with("update checks resumed")
        })
        .collect();
    assert_eq!(said.len(), 3, "began, restated once, resumed: {lines:?}");
    assert_eq!(said[0].0, aterm_log::Level::Info);
    assert_eq!(said[1].0, aterm_log::Level::Warn);
    assert!(said[2].1.starts_with("update checks resumed"), "{said:?}");
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A STOPPED SIBLING HOLDING `checker.lock` DOES NOT STOP UPDATE CHECKS ON THE
/// WHOLE MACHINE. The sibling here takes the lock and never lets go — a
/// `SIGSTOP`ped aterm mid-check, which has written its pid into the lock. The
/// loop defers to it [`crate::checker_watch::CHECKER_UNGATED_AFTER`] times, then
/// CHECKS WITHOUT THE LOCK on that cycle and every one after, naming the holder
/// once; the deferral streak stays published (the window's watchdog raises its
/// warning from it) and is not reported as over.
#[test]
fn a_checker_lock_held_forever_does_not_stop_checks() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    let generation = WATCH.register(1_000);
    let staging = crate::paths::Staging::scratch("checker-lock-held-forever");
    let lock_path = staging.status.with_file_name("checker.lock");
    let _sibling = aterm_update_core::FileLock::acquire(&lock_path).expect("the sibling takes it");
    // The holder names itself — a live pid, as a stopped aterm's is.
    std::fs::write(&lock_path, format!("{}\n", std::process::id()))
        .expect("the holder names itself");
    // ...when it took the lock, an hour ago: it has written nothing since.
    stamp_an_hour_ago(&lock_path);
    let bound = crate::checker_watch::CHECKER_UNGATED_AFTER;
    let cycles = bound + 2;
    let checks = Arc::new(AtomicU64::new(0));
    let calls = Arc::new(AtomicU64::new(0));
    // (cycle, deferrals published, checks run) as each cycle began.
    let seen: Arc<Mutex<Vec<(u64, u64, u64)>>> = Arc::default();
    let provider: crate::SourceProvider = {
        let (calls, checks, seen) = (Arc::clone(&calls), Arc::clone(&checks), Arc::clone(&seen));
        Arc::new(move || {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let deferrals = WATCH.snapshot().map_or(0, |beat| beat.deferrals);
            seen.lock()
                .unwrap()
                .push((n, deferrals, checks.load(Ordering::SeqCst)));
            if n > cycles {
                WATCH.supersede(generation);
            }
            Some(source())
        })
    };
    let mut loop_args = args(provider);
    loop_args.staging = {
        let staging = staging.clone();
        Arc::new(move || Some(staging.clone()))
    };
    loop_args.check = {
        let checks = Arc::clone(&checks);
        Arc::new(move |_, _| {
            checks.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    };
    let (exit, lines) = run_to_exit(
        &WATCH,
        generation,
        loop_args,
        "the loop must come round every cycle",
    );
    assert_eq!(
        exit,
        CheckerExit::Superseded {
            phase: CheckerPhase::Settings
        }
    );
    // The first `bound - 1` cycles deferred without checking; from the
    // `bound`-th on, every cycle checked, and the streak kept counting.
    assert_eq!(
        checks.load(Ordering::SeqCst),
        cycles - bound + 1,
        "checked on the bound-th cycle and every one after: {:?}",
        seen.lock().unwrap()
    );
    let seen = seen.lock().unwrap().clone();
    for (n, deferrals, ran) in &seen[1..] {
        assert_eq!(
            *deferrals,
            n - 1,
            "the streak stands while the holder does: {seen:?}"
        );
        assert_eq!(*ran, (n - 1).saturating_sub(bound - 1), "{seen:?}");
    }
    let said: Vec<_> = lines
        .iter()
        .filter(|(_, line)| line.starts_with("update check without the checker lock"))
        .collect();
    assert_eq!(said.len(), 1, "said once per streak: {lines:?}");
    assert_eq!(said[0].0, aterm_log::Level::Warn);
    assert!(
        said[0].1.contains(&std::process::id().to_string()),
        "the holder named: {said:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|(_, line)| line.starts_with("update checks resumed")),
        "a streak the holder still stands in is not over: {lines:?}"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A MACHINE WHOSE LAST COMPLETED CHECK IS OLDER THAN `STALE_CHECK_AFTER` checks
/// past a held `checker.lock` at the FIRST deferral:
/// nobody on this machine has checked for hours, so whoever holds the lock is not
/// checking for it. NEGATIVE CONTROL: a fresh receipt is honoured — the cycle
/// defers, as a sibling mid-check deserves.
#[test]
fn a_stale_machine_ledger_checks_past_a_held_lock_at_once() {
    for (stale, want_checks) in [(true, 1), (false, 0)] {
        static WATCH: CheckerWatch = CheckerWatch::new();
        let generation = WATCH.register(1_000);
        let staging = crate::paths::Staging::scratch("checker-lock-stale-ledger");
        let lock_path = staging.status.with_file_name("checker.lock");
        let _sibling =
            aterm_update_core::FileLock::acquire(&lock_path).expect("the sibling takes it");
        // A holder showing no progress: its lock written an hour ago.
        stamp_an_hour_ago(&lock_path);
        let now = aterm_time::unix_secs().unwrap();
        let checked = if stale {
            now - crate::STALE_CHECK_AFTER.as_secs() - 60
        } else {
            now - 30
        };
        // A receipt some OTHER channel's check left: it cannot excuse this
        // cycle's own check (`checker_skip_for`), only date the machine's last.
        std::fs::write(
            crate::check_receipt::path(&staging),
            format!(
                "schema = 1\nupdated_at = \"{}\"\ncurrent_build = 1\nsource = \"other/channel\"\n",
                aterm_types::rfc3339::format_rfc3339(checked)
            ),
        )
        .unwrap();
        let checks = Arc::new(AtomicU64::new(0));
        let calls = Arc::new(AtomicU64::new(0));
        let provider: crate::SourceProvider = {
            let calls = Arc::clone(&calls);
            Arc::new(move || {
                if calls.fetch_add(1, Ordering::SeqCst) >= 1 {
                    WATCH.supersede(generation);
                }
                Some(source())
            })
        };
        let mut loop_args = args(provider);
        loop_args.staging = {
            let staging = staging.clone();
            Arc::new(move || Some(staging.clone()))
        };
        loop_args.check = {
            let checks = Arc::clone(&checks);
            Arc::new(move |_, _| {
                checks.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            })
        };
        let (exit, _) = run_to_exit(
            &WATCH,
            generation,
            loop_args,
            "a held lock must defer or ungate the cycle, never park it",
        );
        assert_eq!(
            exit,
            CheckerExit::Superseded {
                phase: CheckerPhase::Settings
            }
        );
        assert_eq!(
            checks.load(Ordering::SeqCst),
            want_checks,
            "stale ledger {stale}: one cycle, one deferral"
        );
        let _ = std::fs::remove_dir_all(&staging.root);
    }
}

/// A STALL INSIDE THE LANE IS STILL A STALL AFTER ITS REPLACEMENT STARTS. The
/// generation below stalled in its check, holding the check lane (the test holds
/// it for it), and the watchdog retired it. Its replacement can never take the
/// lane. Before this change it said so at DEBUG once, then a WARN every sixth
/// cycle that blamed "a long manual check", while its fresh stamps took
/// `checker_stalled=` off the status line and a manual check queued behind the
/// lane with no bound. Now the first blocked cycle is a WARN naming the stuck
/// generation and its phase, the stall stays standing for the status line, and a
/// manual check says why it cannot run instead of waiting.
#[test]
fn a_replacement_blocked_behind_a_stuck_check_says_so_and_keeps_the_stall_standing() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    static LANE: crate::check_lane::Lane = crate::check_lane::Lane::new();
    let stuck = WATCH.register(1_000);
    let mark = WATCH.hold_lane(stuck);
    let lane = LANE.try_lock().expect("the stuck check's lane");
    assert!(WATCH.beat(stuck, CheckerPhase::Checking));
    assert_eq!(crate::manual_check_blocked(&WATCH), None, "not retired yet");
    let replacement = WATCH.supersede(stuck).expect("the watchdog retires it");
    let every = crate::LANE_BUSY_LOG_EVERY;
    let calls = Arc::new(AtomicU64::new(0));
    let healthy_reads = Arc::new(AtomicU64::new(0));
    let provider: crate::SourceProvider = {
        let (calls, healthy_reads) = (Arc::clone(&calls), Arc::clone(&healthy_reads));
        Arc::new(move || {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some(beat) = WATCH.snapshot()
                && n > 1
                && standing_stall(&beat, crate::checker_watch::now_secs())
                    .is_none_or(|(_, phase)| phase != CheckerPhase::Checking)
            {
                healthy_reads.fetch_add(1, Ordering::SeqCst);
            }
            if n > every {
                WATCH.supersede(replacement);
            }
            Some(source())
        })
    };
    let mut loop_args = args(provider);
    loop_args.lane = &LANE;
    let (exit, lines) = run_to_exit(
        &WATCH,
        replacement,
        loop_args,
        "a cycle behind a held lane must come round, not queue on it",
    );
    assert_eq!(
        exit,
        CheckerExit::Superseded {
            phase: CheckerPhase::Settings
        }
    );
    assert_eq!(
        healthy_reads.load(Ordering::SeqCst),
        0,
        "the status line reads the stuck check's stall for as long as it holds the lane"
    );
    let blocked: Vec<_> = lines
        .iter()
        .filter(|(_, line)| line.starts_with("update checks"))
        .collect();
    assert_eq!(
        blocked.len(),
        2,
        "the first blocked cycle, then every {every}th: {lines:?}"
    );
    assert!(
        blocked
            .iter()
            .all(|(level, _)| *level == aterm_log::Level::Warn)
    );
    assert!(
        crate::manual_check_blocked(&WATCH).is_some(),
        "a manual check does not queue"
    );
    // The stuck check lets go: the stall is over, and a manual check may run.
    drop(lane);
    drop(mark);
    assert_eq!(crate::manual_check_blocked(&WATCH), None);
    let beat = WATCH.snapshot().unwrap();
    assert_eq!(beat.lane, None);
}

/// AN UNANSWERED SETTINGS QUERY IS SAID, ON ITS EDGES: the first miss, every
/// `SETTINGS_MISS_LOG_EVERY`-th repeat (at WARN), and the answer that ends the
/// streak are the whole log — never silence, never a line per miss.
#[test]
fn an_unanswered_settings_query_is_logged_on_its_edges_and_not_every_miss() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    let generation = WATCH.register(1_000);
    let every = crate::SETTINGS_MISS_LOG_EVERY;
    let misses = 2 * every;
    let calls = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&calls);
    // Observations from inside the hook are COUNTED, not asserted there: a panic in
    // the hook is exactly what the loop now survives, so an assert would be caught
    // and lost.
    let unstamped = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&unstamped);
    let provider: crate::SourceProvider = Arc::new(move || {
        let n = counted.fetch_add(1, Ordering::SeqCst) + 1;
        // Every miss is stamped and counted, so a loop that only misses is visibly
        // alive — and visibly not checking.
        if n > 1
            && WATCH.snapshot().is_none_or(|beat| {
                beat.phase != CheckerPhase::Settings || beat.settings_misses != n - 1
            })
        {
            seen.fetch_add(1, Ordering::SeqCst);
        }
        if n <= misses {
            return None;
        }
        // The window answers — and the test ends the thread the way a
        // replacement would, before the loop can reach anything real.
        WATCH.supersede(generation);
        Some(source())
    });
    let (exit, lines) = run_to_exit(
        &WATCH,
        generation,
        args(provider),
        "the loop must come round every cycle",
    );
    assert_eq!(
        exit,
        CheckerExit::Superseded {
            phase: CheckerPhase::Settings
        }
    );
    assert_eq!(calls.load(Ordering::SeqCst), misses + 1);
    assert_eq!(
        unstamped.load(Ordering::SeqCst),
        0,
        "every miss stamped `settings` with its running count"
    );
    let said: Vec<_> = lines
        .iter()
        .filter(|(_, line)| line.starts_with("update checks"))
        .collect();
    assert!(
        said.first()
            .is_some_and(|(level, line)| *level == aterm_log::Level::Info
                && line.starts_with("update checks paused")),
        "the first miss is news: {lines:?}"
    );
    assert_eq!(
        said.len(),
        4,
        "first miss, two restatements and the recovery — not one line per miss: {said:?}"
    );
    for i in [1, 2] {
        assert_eq!(said[i].0, aterm_log::Level::Warn, "{said:?}");
    }
}

/// A PANIC IN A CYCLE IS ONE LOGGED CYCLE: uncaught, it would end automatic
/// updating for the life of the process (the thread's `JoinHandle` is dropped).
/// The loop logs it and goes round again — the hook is asked a second time.
#[test]
fn a_panic_in_a_cycle_is_logged_and_the_loop_goes_round_again() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    let generation = WATCH.register(1_000);
    let calls = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&calls);
    let provider: crate::SourceProvider = Arc::new(move || {
        if counted.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("injected checker panic");
        }
        WATCH.supersede(generation);
        None
    });
    let (exit, lines) = run_to_exit(
        &WATCH,
        generation,
        args(provider),
        "the loop must come round every cycle",
    );
    assert_eq!(
        exit,
        CheckerExit::Superseded {
            phase: CheckerPhase::Settings
        },
        "the thread survived the panic and left only through its generation"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the cycle after the panic ran"
    );
    assert!(
        lines
            .iter()
            .any(|(level, line)| *level == aterm_log::Level::Warn
                && line.contains("injected checker panic")),
        "the panic is on record with its message: {lines:?}"
    );
}

/// A SUPERSEDED GENERATION EXITS WITHOUT CHECKING. The watchdog cannot kill a
/// stalled thread, only replace it; if the stalled thread ever wakes it must see it
/// was replaced and leave before it reaches the lane, the lock or the network —
/// otherwise every replacement would leave two checkers running. The code before
/// this change had no generation at all: the one thread never exited.
#[test]
fn a_superseded_generation_exits_without_checking_and_leaves_the_replacements_stamp() {
    // (a) Replaced before it ever ran: it never asks for its settings.
    static FIRST: CheckerWatch = CheckerWatch::new();
    let stale = FIRST.register(1_000);
    let live = FIRST.supersede(stale).expect("replaced");
    let asked = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&asked);
    let never: crate::SourceProvider = Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        Some(source())
    });
    assert_eq!(
        crate::run_checker(&FIRST, stale, &args(never)),
        CheckerExit::Superseded {
            phase: CheckerPhase::Starting
        }
    );
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    assert_eq!(FIRST.snapshot().unwrap().generation, live);

    // (b) Replaced while it was stuck in its settings query (the window's watchdog
    // acting mid-stall): it wakes with a usable answer and still leaves, and the
    // replacement's `Starting` stamp is untouched by anything it did.
    static SECOND: CheckerWatch = CheckerWatch::new();
    let stalled = SECOND.register(1_000);
    let calls = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&calls);
    let replaced_mid_query: crate::SourceProvider = Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        SECOND.supersede(stalled);
        Some(source())
    });
    assert_eq!(
        crate::run_checker(&SECOND, stalled, &args(replaced_mid_query)),
        CheckerExit::Superseded {
            phase: CheckerPhase::Settings
        }
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one question, then gone");
    let beat = SECOND.snapshot().unwrap();
    assert_eq!(beat.generation, stalled + 1);
    assert_eq!(
        beat.phase,
        CheckerPhase::Starting,
        "the stale thread wrote nothing over its replacement's stamp"
    );
}

/// Set `path`'s stamp an hour back: a lock its holder took long ago and has not
/// written since.
fn stamp_an_hour_ago(path: &std::path::Path) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| {
            file.set_modified(std::time::SystemTime::now() - Duration::from_secs(3600))
        })
        .expect("the lock's stamp is set");
}

/// The gate a cycle meets behind a sibling holding `checker.lock`, STILL CHECKING
/// while `beating`: [`the_gate_asked_once`], with the holder's beat landing in the
/// wait, as a live holder's does every quarter of it (`crate::HolderBeat`). Once the
/// holder stops, the lock's stamp is an hour old: a stopped holder writes nothing
/// through a wait. Each ask's wait goes into `asked`: the bound the loop hands the
/// gate is what this stand-in does not spend, so it is read instead.
fn gate_behind_a_holder(
    beating: Arc<AtomicBool>,
    asked: Arc<Mutex<Vec<Duration>>>,
) -> Arc<crate::GateFn> {
    Arc::new(move |path, wait| {
        asked.lock().unwrap().push(wait);
        let gate = crate::checker_gate(path, Duration::ZERO);
        if beating.load(Ordering::SeqCst) {
            std::fs::write(path, format!("{}\n", std::process::id())).expect("the holder's beat");
        } else {
            stamp_an_hour_ago(path);
        }
        gate
    })
}

/// A HOLDER STILL CHECKING IS NEVER CALLED STOPPED, HOWEVER LONG IT TAKES. A
/// sibling holds `checker.lock` through a download far longer
/// than a check cycle — twenty minutes on a slow link — and writes the lock as it
/// works: every cycle behind it defers, none checks without the lock, and the streak
/// the window's watchdog warns on stays 0. The moment it stops writing (a `SIGSTOP`
/// mid-download) the streak starts, and the bound's worth of cycles later the loop
/// checks without it.
#[test]
fn a_holder_still_checking_is_never_called_stopped() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    let generation = WATCH.register(1_000);
    let staging = crate::paths::Staging::scratch("checker-lock-live-holder");
    let lock_path = staging.status.with_file_name("checker.lock");
    let _sibling = aterm_update_core::FileLock::acquire(&lock_path).expect("the sibling takes it");
    let beating = Arc::new(AtomicBool::new(true));
    let bound = crate::checker_watch::CHECKER_UNGATED_AFTER;
    let working = bound + 1;
    let cycles = working + bound + 1;
    let checks = Arc::new(AtomicU64::new(0));
    let calls = Arc::new(AtomicU64::new(0));
    let seen: Arc<Mutex<Vec<(u64, u64, u64)>>> = Arc::default();
    let provider: crate::SourceProvider = {
        let (calls, checks, seen, beating) = (
            Arc::clone(&calls),
            Arc::clone(&checks),
            Arc::clone(&seen),
            Arc::clone(&beating),
        );
        Arc::new(move || {
            let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let deferrals = WATCH.snapshot().map_or(0, |beat| beat.deferrals);
            seen.lock()
                .unwrap()
                .push((n, deferrals, checks.load(Ordering::SeqCst)));
            if n > working {
                // The holder is stopped: it writes nothing from here on.
                beating.store(false, Ordering::SeqCst);
            }
            if n > cycles {
                WATCH.supersede(generation);
            }
            Some(source())
        })
    };
    let mut loop_args = args(provider);
    // The shipping wait: the half of it a sibling reads as progress is thirty
    // seconds, which no descheduling between a beat and its read comes near.
    loop_args.checker_lock_wait = crate::checker_watch::CHECKER_LOCK_WAIT;
    let asked: Arc<Mutex<Vec<Duration>>> = Arc::default();
    loop_args.gate = gate_behind_a_holder(Arc::clone(&beating), Arc::clone(&asked));
    loop_args.staging = {
        let staging = staging.clone();
        Arc::new(move || Some(staging.clone()))
    };
    loop_args.check = {
        let checks = Arc::clone(&checks);
        Arc::new(move |_, _| {
            checks.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    };
    let (exit, lines) = run_to_exit(
        &WATCH,
        generation,
        loop_args,
        "the loop must come round every cycle",
    );
    assert_eq!(
        exit,
        CheckerExit::Superseded {
            phase: CheckerPhase::Settings
        }
    );
    let seen = seen.lock().unwrap().clone();
    // Cycles 1..=working deferred to a working holder: nothing published, nothing
    // checked. From the first cycle behind a stopped holder the streak counts, and
    // the bound-th such cycle checks without the lock.
    for (n, deferrals, ran) in &seen[1..] {
        let stalled_before = (n - 1).saturating_sub(working);
        assert_eq!(*deferrals, stalled_before, "cycle {n}: {seen:?}");
        assert_eq!(
            *ran,
            stalled_before.saturating_sub(bound - 1),
            "cycle {n}: {seen:?}"
        );
    }
    assert_eq!(
        checks.load(Ordering::SeqCst),
        cycles - working - bound + 1,
        "{seen:?}"
    );
    // Each cycle asked the gate once, with the loop's own bound: the wait a cycle
    // spends behind a holder before it reads the holder's progress.
    assert_eq!(
        *asked.lock().unwrap(),
        vec![crate::checker_watch::CHECKER_LOCK_WAIT; usize::try_from(cycles).unwrap()],
        "every cycle waits the loop's bound at the gate"
    );
    // Said once the holder stops, and not before: the one line that calls it
    // stopped comes after the cycles it was seen working.
    let said: Vec<_> = lines
        .iter()
        .filter(|(_, line)| line.starts_with("update check without the checker lock"))
        .collect();
    assert_eq!(said.len(), 1, "{lines:?}");
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A STALE MACHINE LEDGER DOES NOT UNGATE A HOLDER THAT IS WORKING. The machine
/// slept overnight, so no check has completed for hours; on wake one aterm takes
/// the lock and starts a download that outlasts a sibling's lock wait. With the
/// holder writing the lock as it works, the sibling defers — no check, nothing
/// published — while the same stale ledger behind a holder that writes nothing
/// still checks at once (`a_stale_machine_ledger_checks_past_a_held_lock_at_once`).
#[test]
fn a_stale_ledger_waits_for_a_holder_that_is_working() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    let generation = WATCH.register(1_000);
    let staging = crate::paths::Staging::scratch("checker-lock-stale-ledger-live");
    let lock_path = staging.status.with_file_name("checker.lock");
    let _sibling = aterm_update_core::FileLock::acquire(&lock_path).expect("the sibling takes it");
    let now = aterm_time::unix_secs().unwrap();
    std::fs::write(
        crate::check_receipt::path(&staging),
        format!(
            "schema = 1\nupdated_at = \"{}\"\ncurrent_build = 1\nsource = \"other/channel\"\n",
            aterm_types::rfc3339::format_rfc3339(now - crate::STALE_CHECK_AFTER.as_secs() - 60)
        ),
    )
    .unwrap();
    let checks = Arc::new(AtomicU64::new(0));
    let calls = Arc::new(AtomicU64::new(0));
    let published = Arc::new(AtomicU64::new(0));
    let provider: crate::SourceProvider = {
        let (calls, published) = (Arc::clone(&calls), Arc::clone(&published));
        Arc::new(move || {
            published.fetch_max(
                WATCH.snapshot().map_or(0, |beat| beat.deferrals),
                Ordering::SeqCst,
            );
            if calls.fetch_add(1, Ordering::SeqCst) >= 2 {
                WATCH.supersede(generation);
            }
            Some(source())
        })
    };
    let mut loop_args = args(provider);
    loop_args.checker_lock_wait = crate::checker_watch::CHECKER_LOCK_WAIT;
    loop_args.gate = gate_behind_a_holder(Arc::new(AtomicBool::new(true)), Arc::default());
    loop_args.staging = {
        let staging = staging.clone();
        Arc::new(move || Some(staging.clone()))
    };
    loop_args.check = {
        let checks = Arc::clone(&checks);
        Arc::new(move |_, _| {
            checks.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    };
    let (exit, _) = run_to_exit(
        &WATCH,
        generation,
        loop_args,
        "a held lock must defer or ungate the cycle, never park it",
    );
    assert_eq!(
        exit,
        CheckerExit::Superseded {
            phase: CheckerPhase::Settings
        }
    );
    assert_eq!(
        checks.load(Ordering::SeqCst),
        0,
        "two deferrals to a working holder, no check without the lock"
    );
    assert_eq!(
        published.load(Ordering::SeqCst),
        0,
        "and no stuck sibling shown"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// THE HOLDER'S BEAT: a cycle that holds `checker.lock` through a long check keeps
/// writing it while its checker is alive, so its siblings read it as working
/// (`crate::checker_holder_progressing`); a retired generation's beat writes
/// nothing, so a checker the watchdog replaced cannot vouch for itself.
///
/// FAILS WITHOUT THE FIX: the lock was written once, when it was taken, and read as
/// no progress by every sibling a few seconds into any long check.
///
/// Each half starts from a lock stamped an hour ago, so the beat's own write is the
/// only progress there is to read; the retired half reads the lock after two beats
/// have run to their end (the beat's test-only count), not after a stretch of sleep.
#[test]
fn the_holder_writes_the_lock_while_its_checker_is_alive() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    let staging = crate::paths::Staging::scratch("checker-lock-holder-beat");
    let path = staging.status.with_file_name("checker.lock");
    let _lock = aterm_update_core::FileLock::acquire(&path).expect("the cycle takes it");
    // The shipping window: a write just made reads as fresh however long a loaded
    // machine holds this thread between it and the read.
    let fresh = crate::holder_fresh(crate::checker_watch::CHECKER_LOCK_WAIT);
    let every = Duration::from_millis(1);
    let written_within = || crate::checker_holder_progressing(&path, fresh);

    let live = WATCH.register(1_000);
    assert!(WATCH.beat(live, CheckerPhase::Checking));
    stamp_an_hour_ago(&path);
    assert!(!written_within(), "an hour-old stamp is no progress");
    let beat = crate::HolderBeat::start(path.clone(), &WATCH, live, every);
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !written_within() {
        assert!(
            Instant::now() < deadline,
            "a live holder never rewrote its lock"
        );
        std::thread::sleep(every);
    }
    drop(beat);

    let retired = WATCH.register(1_000);
    assert!(WATCH.supersede(retired).is_some());
    stamp_an_hour_ago(&path);
    let beat = crate::HolderBeat::start(path.clone(), &WATCH, retired, every);
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while beat.beats.load(Ordering::SeqCst) < 2 {
        assert!(
            Instant::now() < deadline,
            "the retired generation's beat never ran"
        );
        std::thread::sleep(every);
    }
    drop(beat);
    assert!(
        !written_within(),
        "a retired generation's beat writes nothing"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A BEAT THAT LANDS AS A SIBLING READS IS PROGRESS. The sibling reads the lock's
/// stamp before its clock, so a stamp can be past the clock only by the clocks'
/// granularity (measured under load: 1.4 ms): a stamp that close counts as written
/// now, while a stamp a second ahead (a clock stepped back) defers nothing, and a
/// stamp older than `fresh` is no progress.
#[test]
fn a_stamp_just_past_the_readers_clock_is_progress_and_a_wrong_clock_is_not() {
    let staging = crate::paths::Staging::scratch("checker-lock-stamp-order");
    let path = staging.status.with_file_name("checker.lock");
    std::fs::write(&path, "1\n").expect("the lock file");
    let stamp = |at: std::time::SystemTime| {
        std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_modified(at))
            .expect("the lock's stamp is set");
    };
    // Fresh is wide on purpose: a "recent" beat must stay recent however long a
    // loaded machine descheduled this thread between the stamp and the reader's
    // own clock read (at 200 ms, 150 ms of delay flipped the second check).
    let fresh = Duration::from_secs(30);
    let now = std::time::SystemTime::now;

    stamp(now() + Duration::from_millis(2));
    assert!(
        crate::checker_holder_progressing(&path, fresh),
        "a beat that landed just after the reader's clock is progress"
    );
    stamp(now() - Duration::from_millis(50));
    assert!(
        crate::checker_holder_progressing(&path, fresh),
        "a beat 50 ms ago is progress"
    );
    stamp(now() - Duration::from_secs(60));
    assert!(
        !crate::checker_holder_progressing(&path, fresh),
        "a stamp older than fresh is none"
    );
    stamp(now() + Duration::from_secs(1));
    assert!(
        !crate::checker_holder_progressing(&path, fresh),
        "a clock stepped back a second defers nothing"
    );
    let _ = std::fs::remove_dir_all(&staging.root);
}
