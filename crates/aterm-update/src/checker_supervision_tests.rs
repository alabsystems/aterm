// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE CHECK LOOP CAN NEVER STALL SILENTLY (the 2026-09-22/23 update audit, plan
//! P2-1). On the owner's machine the loop went quiet for 4.6 days in a live process
//! and two releases were never staged. These tests drive the REAL loop
//! ([`crate::run_checker`]) and the real lock gate ([`crate::checker_gate`]); each
//! one names the behaviour the code before this change had instead.
//!
//! Every loop-level test ends its thread through the generation token — the
//! replacement mechanism itself — from inside the settings hook, and none ever
//! reaches the network: the tests that take the check lane stop at a held
//! `checker.lock` or at a sibling's fresh receipt in a scratch staging root, or
//! find the lane already held. The per-user staging root is never read.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::checker_watch::{CheckerPhase, CheckerWatch, standing_stall};
use crate::{CheckerArgs, CheckerExit, CheckerGate, Source};

/// The check lane is one per process (`crate::check_lane`), so the tests that
/// take it, or hold it, take turns.
static LANE_TESTS: Mutex<()> = Mutex::new(());

fn source() -> Source {
    Source {
        owner: "alabsystems".into(),
        repo: "aterm".into(),
    }
}

/// Loop arguments around `provider`, with the pauses a test can afford and no
/// staging root at all.
fn args(provider: crate::SourceProvider) -> CheckerArgs {
    CheckerArgs {
        current_build: 1,
        source_provider: provider,
        health_hook: std::sync::Mutex::new(None),
        staged_hook: std::sync::Mutex::new(None),
        settings_retry: Duration::ZERO,
        staging: Arc::new(|| None),
        checker_lock_wait: Duration::from_millis(200),
        pause: Some(Duration::ZERO),
    }
}

/// A HELD `checker.lock` DEFERS THE CYCLE, PROMPTLY. The holder here is a second
/// open file description of the same lock file — exactly what a sibling aterm
/// process (or one SIGSTOPped mid-check) is. Before this change the loop took the
/// lock with the blocking `FileLock::acquire` while holding its check lane, so this
/// call never returned: the helper thread below would still be parked when the
/// ten-second `recv_timeout` gives up, and the test fails there.
#[test]
fn a_held_checker_lock_defers_the_cycle_promptly_instead_of_parking_it() {
    let staging = crate::paths::Staging::scratch("checker-gate-deferral");
    let path = staging.root.join("checker.lock");
    let holder = aterm_update_core::FileLock::acquire(&path).expect("the sibling takes it");
    let (tx, rx) = std::sync::mpsc::channel();
    let asked = path.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        let deferred = matches!(
            crate::checker_gate(&asked, Duration::from_millis(300)),
            CheckerGate::Deferred
        );
        let _ = tx.send((deferred, started.elapsed()));
    });
    let (deferred, took) = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the cycle must come back with an answer, not park on the sibling's lock");
    assert!(deferred, "a held lock is a typed deferral, not an error");
    assert!(
        took < Duration::from_secs(5),
        "deferred promptly (took {took:?})"
    );
    // The holder going away is all it takes for the next cycle to be the checker.
    // That release is deterministic, not a close that a child mid-spawn in another
    // test could outlive with its inherited copy of the descriptor (~523 ms under
    // load): `FileLock`'s drop is `LOCK_UN`, which strips the lock from the open
    // file description itself — pinned in aterm-update-core, with a live duplicate
    // of the descriptor, by `a_dropped_lock_is_free_while_a_copy_of_its_descriptor_lives`.
    drop(holder);
    // The gate is `acquire_within`, so a lock that did stay held is a bounded wait
    // that ends in a named answer below, never a hang or a silent pass.
    let gate = match crate::checker_gate(&path, Duration::from_secs(5)) {
        CheckerGate::Held(_) => "held",
        CheckerGate::Deferred => "deferred: still held 5 s after its holder let go",
        CheckerGate::Unavailable => "unavailable: the lock itself failed",
    };
    assert_eq!(gate, "held", "the next cycle takes the freed lock");
    let _ = std::fs::remove_dir_all(staging.root);
}

/// THE LOOP ITSELF DEFERS ON A HELD `checker.lock`, SAYS SO ON THE STREAK'S EDGES,
/// AND RESUMES THE CYCLE THE LOCK COMES FREE. The test above proves the gate; this
/// one drives [`crate::run_checker`] through it, in a scratch staging root, with a
/// sibling holding the lock for six cycles. Put the loop back on the blocking
/// `FileLock::acquire` (the 4.6-day shape) and its first cycle parks on the
/// sibling's lock: the `recv_timeout` below gives up and the test fails there. Drop
/// the deferral's streak log, its published count or the recovery line, and the
/// assertions after it fail.
#[test]
fn the_loop_defers_each_cycle_a_sibling_holds_the_checker_lock_and_resumes_when_it_lets_go() {
    let _turn = LANE_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    static WATCH: CheckerWatch = CheckerWatch::new();
    let generation = WATCH.register(1_000);
    let staging = crate::paths::Staging::scratch("checker-loop-deferral");
    let lock_path = staging.status.with_file_name("checker.lock");
    let sibling = Arc::new(Mutex::new(Some(
        aterm_update_core::FileLock::acquire(&lock_path).expect("the sibling takes it"),
    )));
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
                // waits only `checker_lock_wait` (200 ms) for it, less than one
                // spawn under load can hold a copied descriptor, so this relies on
                // the release being deterministic: `FileLock`'s drop is `LOCK_UN`.
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
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = crate::log_capture::take();
        let exit = crate::run_checker(&WATCH, generation, &loop_args);
        let _ = tx.send((exit, crate::log_capture::take()));
    });
    let (exit, lines) = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the loop must defer each cycle, not park on the sibling's lock");
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
    assert!(
        said[0]
            .1
            .contains("another aterm process has held the checker lock")
    );
    assert_eq!(said[1].0, aterm_log::Level::Warn);
    assert!(
        said[1]
            .1
            .contains(&format!("deferred {every} cycles in a row")),
        "{said:?}"
    );
    assert!(
        said[2]
            .1
            .contains(&format!("after {every} deferred cycle(s)")),
        "{said:?}"
    );
    assert!(
        lines
            .iter()
            .any(|(_, line)| line == "another aterm process completed this interval's update check"),
        "the cycle past the lock read the sibling's receipt: {lines:?}"
    );
    let _ = std::fs::remove_dir_all(staging.root);
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
    let _turn = LANE_TESTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    static WATCH: CheckerWatch = CheckerWatch::new();
    let stuck = WATCH.register(1_000);
    let mark = WATCH.hold_lane(stuck);
    let lane = crate::check_lane()
        .try_lock()
        .expect("no other test holds the lane");
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
    let _ = crate::log_capture::take();
    let exit = crate::run_checker(&WATCH, replacement, &args(provider));
    let lines = crate::log_capture::take();
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
        blocked[0].1.contains(&format!(
            "checker generation {stuck} stopped answering while checking"
        )),
        "{blocked:?}"
    );
    assert!(
        blocked[1]
            .1
            .contains(&format!("still cannot run after {every} cycles")),
        "{blocked:?}"
    );
    let refusal = crate::manual_check_blocked(&WATCH).expect("a manual check does not queue");
    assert!(refusal.contains("while checking"), "{refusal}");
    // The stuck check lets go: the stall is over, and a manual check may run.
    drop(lane);
    drop(mark);
    assert_eq!(crate::manual_check_blocked(&WATCH), None);
    let beat = WATCH.snapshot().unwrap();
    assert_eq!(beat.lane, None);
}

/// AN UNANSWERED SETTINGS QUERY IS SAID, ON ITS EDGES. The old branch slept five
/// seconds and looped with NO line at all — so the first assertion (a line on the
/// first miss) fails against it — and a naive fix would say it every miss, which
/// the count assertion refuses. The first miss, every
/// `SETTINGS_MISS_LOG_EVERY`-th repeat (at WARN), and the answer that ends the
/// streak are the whole log.
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
    let _ = crate::log_capture::take();
    let exit = crate::run_checker(&WATCH, generation, &args(provider));
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
    let lines = crate::log_capture::take();
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
    for (i, count) in [(1, every), (2, misses)] {
        let (level, line) = said[i];
        assert_eq!(*level, aterm_log::Level::Warn);
        assert!(
            line.contains(&format!("unanswered {count} times in a row")),
            "{line}"
        );
    }
    assert!(
        said[3]
            .1
            .contains(&format!("answered after {misses} unanswered attempt(s)")),
        "{said:?}"
    );
}

/// A PANIC IN A CYCLE IS ONE LOGGED CYCLE. Before this change nothing caught it:
/// the panic unwound out of the thread's closure, the dropped `JoinHandle` meant no
/// one noticed, and automatic updating ended for the life of the process. Here the
/// same panic would unwind out of `run_checker` into this test and fail it; with
/// the change the loop logs it and goes round again — the hook is asked a second
/// time.
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
    let _ = crate::log_capture::take();
    let exit = crate::run_checker(&WATCH, generation, &args(provider));
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
    let lines = crate::log_capture::take();
    assert!(
        lines
            .iter()
            .any(|(level, line)| *level == aterm_log::Level::Warn
                && line.contains("panicked (injected checker panic)")),
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
