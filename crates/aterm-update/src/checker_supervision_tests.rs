// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! These tests drive the real loop ([`crate::run_checker`]) and the real lock gate ([`crate::checker_gate`]), asked once ([`the_gate_asked_once`]): a held lock is the deferral the bounded wait ends in, and the bound itself is `FileLock::acquire_within`'s, tested in aterm-update-core.

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
        check: Arc::new(|_, _| Ok(None)),
        pause: Some(Duration::ZERO),
        wake: &UNPOSTED,
        wake_settle: Duration::from_millis(10),
    }
}

/// The real gate ([`crate::checker_gate`]) with no wait: one try, so a held lock defers
/// at once, where the loop's bounded wait would defer after it.
fn the_gate_asked_once() -> Arc<crate::GateFn> {
    Arc::new(|path, _wait| crate::checker_gate(path, Duration::ZERO))
}

/// Runs the loop to its exit on a thread of its own and hands back the lines it
/// logged there.
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

/// The window posts `NSWorkspaceDidWakeNotification` to the loop's signal; the loop's
/// wait — here a ten-minute pause no check outlives within this test's one-minute hang
/// detector — ends at once, the loop settles (`wake_settle`) and checks again, and that
/// check runs inside the wake's grace (`check_follows_wake`, which books a failed
/// asset GET as `network`).
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

/// A check that fails inside a wake's grace is a network still coming up, whatever it
/// failed on: the loop dates the wake its wait ended on and books that failure offline
/// (`cadence::after_wake` into `cadence::note_failure`) — the quick rung, said at INFO —
/// where the same kind of failure with no wake behind it lengthens the backoff and is
/// warned.
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
    loop_args.pause = Some(Duration::from_millis(10));
    loop_args.wake = &WAKE;
    loop_args.check = {
        let checks = Arc::new(AtomicU64::new(0));
        Arc::new(move |_, _| {
            if checks.fetch_add(1, Ordering::SeqCst) == 0 {
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

/// A wake that ends the wait after a panicked cycle is settled and answered, as at
/// every other wait.
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

/// The loop defers on a held `checker.lock`, logs on the streak's edges, and resumes
/// the cycle the lock comes free.
#[test]
fn the_loop_defers_each_cycle_a_sibling_holds_the_checker_lock_and_resumes_when_it_lets_go() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    let generation = WATCH.register(1_000);
    let staging = crate::paths::Staging::scratch("checker-loop-deferral");
    let lock_path = staging.status.with_file_name("checker.lock");
    let sibling = Arc::new(Mutex::new(Some(
        aterm_update_core::FileLock::acquire(&lock_path).expect("the sibling takes it"),
    )));
    stamp_an_hour_ago(&lock_path);
    let every = crate::CHECKER_DEFERRAL_LOG_EVERY;
    let calls = Arc::new(AtomicU64::new(0));
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
                drop(sibling.lock().unwrap().take());
                crate::check_receipt::record(&staging, 1, &source(), false, None);
            }
            if n == every + 2 {
                // The streak ended, and the sibling's check counted as this loop's.
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

/// A sibling that takes the lock and never lets go (`SIGSTOP`ped mid-check) is deferred
/// to [`crate::checker_watch::CHECKER_UNGATED_AFTER`] times; then every cycle checks
/// without the lock, naming the holder once, and the streak stays published for the
/// window's watchdog.
#[test]
fn a_checker_lock_held_forever_does_not_stop_checks() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    let generation = WATCH.register(1_000);
    let staging = crate::paths::Staging::scratch("checker-lock-held-forever");
    let lock_path = staging.status.with_file_name("checker.lock");
    let _sibling = aterm_update_core::FileLock::acquire(&lock_path).expect("the sibling takes it");
    std::fs::write(&lock_path, format!("{}\n", std::process::id()))
        .expect("the holder names itself");
    stamp_an_hour_ago(&lock_path);
    let bound = crate::checker_watch::CHECKER_UNGATED_AFTER;
    let cycles = bound + 2;
    let checks = Arc::new(AtomicU64::new(0));
    let calls = Arc::new(AtomicU64::new(0));
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

/// With no check on the machine for `STALE_CHECK_AFTER`, the first deferral checks past a
/// held lock, since the holder is not checking for it; a fresh receipt still defers.
#[test]
fn a_stale_machine_ledger_checks_past_a_held_lock_at_once() {
    for (stale, want_checks) in [(true, 1), (false, 0)] {
        static WATCH: CheckerWatch = CheckerWatch::new();
        let generation = WATCH.register(1_000);
        let staging = crate::paths::Staging::scratch("checker-lock-stale-ledger");
        let lock_path = staging.status.with_file_name("checker.lock");
        let _sibling =
            aterm_update_core::FileLock::acquire(&lock_path).expect("the sibling takes it");
        stamp_an_hour_ago(&lock_path);
        let now = aterm_time::unix_secs().unwrap();
        let checked = if stale {
            now - crate::STALE_CHECK_AFTER.as_secs() - 60
        } else {
            now - 30
        };
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

/// A replacement blocked behind a retired generation stuck in its check warns on its first
/// blocked cycle, naming the stuck generation's phase; the stall stays standing for the
/// status line, and a manual check says why it cannot run instead of queueing.
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
    drop(lane);
    drop(mark);
    assert_eq!(crate::manual_check_blocked(&WATCH), None);
    let beat = WATCH.snapshot().unwrap();
    assert_eq!(beat.lane, None);
}

/// The first miss, every `SETTINGS_MISS_LOG_EVERY`-th repeat (at WARN) and the answer that
/// ends the streak are the whole log.
#[test]
fn an_unanswered_settings_query_is_logged_on_its_edges_and_not_every_miss() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    let generation = WATCH.register(1_000);
    let every = crate::SETTINGS_MISS_LOG_EVERY;
    let misses = 2 * every;
    let calls = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&calls);
    let unstamped = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&unstamped);
    let provider: crate::SourceProvider = Arc::new(move || {
        let n = counted.fetch_add(1, Ordering::SeqCst) + 1;
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

/// Uncaught, a panic would end automatic updating for the process's life (the thread's
/// `JoinHandle` is dropped).
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

/// The watchdog can only replace a stalled thread, so one that wakes must leave before
/// the lane, the lock or the network, or every replacement would leave two checkers.
#[test]
fn a_superseded_generation_exits_without_checking_and_leaves_the_replacements_stamp() {
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

/// The gate a cycle meets behind a sibling holding `checker.lock`, still checking
/// while `beating`: [`the_gate_asked_once`], with the holder's beat landing in the
/// wait, as a live holder's does every quarter of it (`crate::HolderBeat`).
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

/// A holder that writes the lock as it works (a long download) is deferred to, with no
/// published streak, however many cycles it takes; once it stops writing, the streak
/// starts and the bound's worth of cycles later the loop checks without the lock.
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
                // The holder stops writing from here on.
                beating.store(false, Ordering::SeqCst);
            }
            if n > cycles {
                WATCH.supersede(generation);
            }
            Some(source())
        })
    };
    let mut loop_args = args(provider);
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
    assert_eq!(
        *asked.lock().unwrap(),
        vec![crate::checker_watch::CHECKER_LOCK_WAIT; usize::try_from(cycles).unwrap()],
        "every cycle waits the loop's bound at the gate"
    );
    let said: Vec<_> = lines
        .iter()
        .filter(|(_, line)| line.starts_with("update check without the checker lock"))
        .collect();
    assert_eq!(said.len(), 1, "{lines:?}");
    let _ = std::fs::remove_dir_all(&staging.root);
}

/// A stale machine ledger does not ungate a holder that writes the lock as it works: the
/// sibling defers, unlike behind a silent holder
/// (`a_stale_machine_ledger_checks_past_a_held_lock_at_once`).
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

/// A cycle that holds `checker.lock` through a long check keeps writing it while its
/// checker is alive, so its siblings read it as working
/// (`crate::checker_holder_progressing`); a retired generation's beat writes
/// nothing, so a checker the watchdog replaced cannot vouch for itself.
#[test]
fn the_holder_writes_the_lock_while_its_checker_is_alive() {
    static WATCH: CheckerWatch = CheckerWatch::new();
    let staging = crate::paths::Staging::scratch("checker-lock-holder-beat");
    let path = staging.status.with_file_name("checker.lock");
    let _lock = aterm_update_core::FileLock::acquire(&path).expect("the cycle takes it");
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

/// The sibling reads the lock's stamp before its clock, so a stamp just past the clock
/// (granularity, ~1.4 ms under load) is progress; one a second ahead (a clock stepped
/// back) or older than `fresh` is not.
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
