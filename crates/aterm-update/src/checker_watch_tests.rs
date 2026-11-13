// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::sync::Arc;

fn beat(phase: CheckerPhase, at: u64, max_interval: u64) -> CheckerBeat {
    CheckerBeat {
        generation: 1,
        phase,
        at_secs: at,
        max_interval_secs: max_interval,
        respawns: 0,
        workers: 1,
        worker_epoch: 0,
        deferrals: 0,
        settings_misses: 0,
        lane: None,
        checks: 0,
    }
}

/// The loop's longest wait, as it registers it (`cadence_bounds::LONGEST_WAIT`).
const LONGEST: u64 = crate::cadence_bounds::LONGEST_WAIT.as_secs();

#[test]
fn a_stamp_is_stale_past_twice_its_phase_budget_plus_slack_and_not_before() {
    let slack = STALL_SLACK.as_secs();
    for (phase, budget) in [
        (CheckerPhase::Waiting, LONGEST),
        (CheckerPhase::Settings, LONGEST),
        (CheckerPhase::BundleProbe, LONGEST),
        (CheckerPhase::LockWait, CHECKER_LOCK_WAIT.as_secs()),
        (CheckerPhase::Checking, CHECK_PHASE_BUDGET.as_secs()),
    ] {
        let limit = 2 * budget + slack;
        let b = beat(phase, 1_000, LONGEST);
        assert_eq!(
            classify(&b, 1_000 + limit),
            CheckerVerdict::Fresh { stale_in_secs: 1 },
            "{phase:?}: at the threshold it is still fresh"
        );
        assert_eq!(
            classify(&b, 1_000 + limit + 1),
            CheckerVerdict::Stalled {
                for_secs: limit + 1,
                phase
            },
            "{phase:?}: one second past it is a stall, named by its phase"
        );
    }
    let parked = beat(CheckerPhase::LockWait, 0, LONGEST);
    assert!(matches!(
        classify(&parked, 10 * 60),
        CheckerVerdict::Stalled {
            phase: CheckerPhase::LockWait,
            ..
        }
    ));
    let hold = crate::UNCOMMITTED_CANDIDATE_HOLD_BOUND.as_secs();
    assert!(hold <= LONGEST, "the cadence's wait covers the hold");
    assert_eq!(phase_budget_secs(CheckerPhase::Starting, LONGEST), LONGEST);
    assert_eq!(phase_budget_secs(CheckerPhase::Starting, 0), hold);
    let starting = beat(CheckerPhase::Starting, 0, LONGEST);
    assert!(matches!(
        classify(&starting, 2 * hold),
        CheckerVerdict::Fresh { .. }
    ));
    assert!(matches!(
        classify(&beat(CheckerPhase::Waiting, 50, LONGEST), 10),
        CheckerVerdict::Fresh { .. }
    ));
    assert_eq!(
        classify(&beat(CheckerPhase::Waiting, 0, u64::MAX), 0),
        CheckerVerdict::Fresh {
            stale_in_secs: u64::MAX
        }
    );
    let _ = instant_at(u64::MAX);
}

#[test]
fn phase_change_wakes_host_to_replace_an_older_long_deadline() {
    let watch = CheckerWatch::new();
    let wakes = Arc::new(AtomicU64::new(0));
    let called = Arc::clone(&wakes);
    watch.set_phase_wake(Box::new(move || {
        called.fetch_add(1, Ordering::Relaxed);
    }));
    let generation = watch.register(LONGEST);
    assert!(watch.beat(generation, CheckerPhase::Settings));
    let old = watch.snapshot().unwrap();
    let old_deadline = stall_threshold_secs(old.phase, old.max_interval_secs);
    assert_eq!(wakes.load(Ordering::Relaxed), 1);

    assert!(watch.beat(generation, CheckerPhase::LockWait));
    let new = watch.snapshot().unwrap();
    let new_deadline = stall_threshold_secs(new.phase, new.max_interval_secs);
    assert!(
        new_deadline < old_deadline / 5,
        "the new phase needs a prompt look"
    );
    assert_eq!(wakes.load(Ordering::Relaxed), 2);
    assert!(watch.beat(generation, CheckerPhase::LockWait));
    watch.beat_progress(generation);
    assert_eq!(wakes.load(Ordering::Relaxed), 2, "no unchanged-phase wakes");

    let replacement = watch.supersede(generation).unwrap();
    assert!(!watch.beat(generation, CheckerPhase::Checking));
    assert_eq!(
        wakes.load(Ordering::Relaxed),
        2,
        "a retired thread is silent"
    );
    assert!(watch.beat(replacement, CheckerPhase::Settings));
    assert_eq!(wakes.load(Ordering::Relaxed), 3);
}

#[test]
fn only_the_current_waiter_publishes_a_representable_deadline() {
    let watch = CheckerWatch::new();
    let old = watch.register(60);
    assert!(watch.publish_next_check(old, 0).is_none());
    assert!(
        watch
            .publish_next_check(old, DEADLINE_SECS_MASK + 1)
            .is_none()
    );
    let old_mark = watch.publish_next_check(old, 100).unwrap();
    assert!(
        watch.publish_next_check(old, 200).is_none(),
        "one live wait per generation"
    );
    assert_eq!(watch.next_check_unix(), Some(100));
    let new = watch.supersede(old).unwrap();
    assert_eq!(watch.next_check_unix(), None);
    assert!(watch.publish_next_check(old, 200).is_none());
    let new_mark = watch.publish_next_check(new, 100).unwrap();
    drop(old_mark);
    assert_eq!(watch.next_check_unix(), Some(100));
    drop(new_mark);
    assert_eq!(watch.next_check_unix(), None);
    assert!(watch.publish_next_check(old, 300).is_none());
    let next_mark = watch.publish_next_check(new, DEADLINE_SECS_MASK).unwrap();
    assert_eq!(watch.next_check_unix(), Some(DEADLINE_SECS_MASK));
    drop(next_mark);
    assert_eq!(watch.next_check_unix(), None);
}

#[test]
fn a_superseded_generation_cannot_beat_and_a_stall_is_replaced_once() {
    let watch = CheckerWatch::new();
    assert!(watch.snapshot().is_none(), "nothing registered yet");
    let first = watch.register(100);
    assert_eq!(first, 1);
    assert!(watch.beat(first, CheckerPhase::LockWait));
    let second = watch.supersede(first).expect("the stall is replaced");
    assert_eq!(second, 2);
    assert_eq!(watch.supersede(first), None, "and only once");
    assert!(!watch.is_current(first));
    assert!(
        !watch.beat(first, CheckerPhase::Checking),
        "the old thread's late beat is refused, and tells it to exit"
    );
    let snap = watch.snapshot().unwrap();
    assert_eq!(snap.generation, second);
    assert_eq!(snap.phase, CheckerPhase::Starting);
    assert_eq!(snap.respawns, 1);
    assert_eq!(
        watch.snapshot().unwrap().max_interval_secs,
        100,
        "a replacement inherits the registered cadence ceiling"
    );
    let mut live = second;
    for _ in 1..MAX_RESPAWNS {
        live = watch.supersede(live).unwrap();
    }
    assert_eq!(watch.supersede(live), None, "MAX_RESPAWNS is a ceiling");
    assert_eq!(watch.snapshot().unwrap().respawns, MAX_RESPAWNS);
    watch.unstarted(live);
    assert_eq!(watch.snapshot().unwrap().respawns, MAX_RESPAWNS - 1);
    assert!(watch.supersede(live).is_some(), "the slot is spent again");
}

#[test]
fn exited_workers_return_capacity_without_resetting_generation_or_history() {
    let watch = CheckerWatch::new();
    let mut current = watch.register(100);
    let mut worker = watch.worker(current);
    for count in 1..=20 {
        let next = watch.supersede(current).unwrap();
        let next_worker = watch.worker(next);
        assert_eq!(watch.snapshot().unwrap().workers, 2);
        drop(worker);
        let beat = watch.snapshot().unwrap();
        assert_eq!(beat.workers, 1);
        assert_eq!(beat.worker_epoch, count);
        assert_eq!(beat.respawns, count);
        assert_eq!(beat.generation, next);
        assert!(!watch.beat(current, CheckerPhase::Checking));
        current = next;
        worker = next_worker;
    }
    drop(worker);
    assert_eq!(watch.snapshot().unwrap().workers, 0);
}

#[test]
fn capacity_is_returned_only_by_the_exact_exited_or_unstarted_worker() {
    let watch = CheckerWatch::new();
    let first = watch.register(100);
    let first_worker = watch.worker(first);
    assert!(first_worker.is_some());
    assert!(
        watch.worker(first).is_none(),
        "a slot cannot be claimed twice"
    );
    let mut current = first;
    let mut workers = Vec::new();
    for _ in 0..MAX_RESPAWNS {
        current = watch.supersede(current).unwrap();
        workers.push(watch.worker(current));
    }
    assert!(watch.supersede(current).is_none());
    watch.unstarted(first);
    watch.unstarted(current);
    assert!(
        watch.supersede(current).is_none(),
        "live workers retain their slots"
    );
    drop(first_worker);
    let reserved = watch.supersede(current).unwrap();
    assert!(watch.supersede(reserved).is_none());
    let latest = watch.snapshot().unwrap();
    watch.unstarted(reserved);
    watch.unstarted(reserved);
    let failed = watch.snapshot().unwrap();
    assert_eq!(failed.workers, latest.workers - 1);
    assert_eq!(failed.worker_epoch, latest.worker_epoch + 1);
    assert_eq!(failed.respawns, latest.respawns - 1);
    assert!(watch.supersede(reserved).is_some());
}

#[test]
fn retired_worker_unwind_and_late_spawn_failure_release_only_their_own_slots() {
    let watch = CheckerWatch::new();
    let first = watch.register(100);
    let next = watch.supersede(first).unwrap();
    watch.unstarted(first);
    assert_eq!(watch.snapshot().unwrap().workers, 1);
    assert_eq!(watch.snapshot().unwrap().generation, next);
    assert_eq!(watch.snapshot().unwrap().phase, CheckerPhase::Starting);
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _worker = watch.worker(next);
        panic!("worker exit");
    }));
    assert!(stopped.is_err());
    assert_eq!(watch.snapshot().unwrap().workers, 0);
    assert_eq!(watch.snapshot().unwrap().generation, next);
    let registered = watch.register(100);
    watch.unstarted(registered);
    assert_eq!(
        watch.snapshot().unwrap().respawns,
        1,
        "initial registration cannot refund a replacement"
    );
}

#[test]
fn a_worker_exit_is_observed_without_waiting_for_its_last_heartbeat_to_stale() {
    for unwind in [false, true] {
        let watch = CheckerWatch::new();
        let first = watch.register(10_000);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _worker = watch.worker(first).unwrap();
            assert!(watch.beat(first, CheckerPhase::Checking));
            if unwind {
                panic!("worker escaped the cycle guard");
            }
        }));
        assert_eq!(result.is_err(), unwind);
        let stopped = watch.snapshot().unwrap();
        assert_eq!(stopped.phase, CheckerPhase::Stopped);
        assert_eq!(stopped.workers, 0);
        assert_eq!(
            classify(&stopped, stopped.at_secs + START_RETRY.as_secs() - 1),
            CheckerVerdict::Fresh { stale_in_secs: 1 }
        );
        assert!(matches!(
            classify(&stopped, stopped.at_secs + START_RETRY.as_secs()),
            CheckerVerdict::Stalled {
                phase: CheckerPhase::Stopped,
                ..
            }
        ));
        let next = watch.supersede(first).unwrap();
        let current = watch.worker(next).unwrap();
        assert!(watch.beat(next, CheckerPhase::Checking));
        drop(current);
        assert_eq!(watch.snapshot().unwrap().phase, CheckerPhase::Stopped);
    }
    let watch = CheckerWatch::new();
    let first = watch.register(10_000);
    let old = watch.worker(first).unwrap();
    let next = watch.supersede(first).unwrap();
    assert!(watch.beat(next, CheckerPhase::Checking));
    let before = watch.snapshot().unwrap();
    drop(old);
    let after = watch.snapshot().unwrap();
    assert_eq!(
        (after.generation, after.phase, after.at_secs),
        (before.generation, before.phase, before.at_secs)
    );
    assert!(!after.phase.worker_absent());
}

#[test]
fn a_retired_publish_cannot_restore_old_failure_counts() {
    let watch = CheckerWatch::new();
    let old = watch.register(100);
    watch.set_deferrals(old, 7);
    watch.set_settings_misses(old, 9);
    let new = watch.supersede(old).unwrap();
    // The retired setters passed their current-generation check before
    // retirement, then resumed at publication after the replacement began.
    publish_count(&watch.deferrals, old, 7);
    publish_count(&watch.settings_misses, old, 9);
    let snapshot = watch.snapshot().unwrap();
    assert_eq!(snapshot.deferrals, 0);
    assert_eq!(snapshot.settings_misses, 0);
    watch.set_deferrals(new, 2);
    watch.set_settings_misses(new, 3);
    publish_count(&watch.deferrals, old, 8);
    publish_count(&watch.settings_misses, old, 10);
    let snapshot = watch.snapshot().unwrap();
    assert_eq!(snapshot.deferrals, 2);
    assert_eq!(snapshot.settings_misses, 3);
    watch.set_deferrals(new, u64::MAX);
    watch.set_settings_misses(new, u64::MAX);
    let snapshot = watch.snapshot().unwrap();
    assert_eq!(snapshot.deferrals, COUNT_MASK);
    assert_eq!(snapshot.settings_misses, COUNT_MASK);
    let next = watch.supersede(new).unwrap();
    let snapshot = watch.snapshot().unwrap();
    assert_eq!(snapshot.generation, next);
    assert_eq!(snapshot.deferrals, 0);
    assert_eq!(snapshot.settings_misses, 0);
}

#[test]
fn a_retired_generation_holding_the_lane_keeps_its_stall_standing_until_it_lets_go() {
    let watch = CheckerWatch::new();
    let first = watch.register(10_000);
    let mark = watch.hold_lane(first);
    assert!(watch.beat(first, CheckerPhase::Checking));
    let stalled_at = watch.snapshot().unwrap().at_secs;
    assert!(watch.snapshot().unwrap().holds_lane(), "the live holder");
    assert_eq!(watch.snapshot().unwrap().blocked_by(), None);

    let second = watch.supersede(first).expect("replaced");
    assert!(watch.beat(second, CheckerPhase::Waiting));
    let live = watch.snapshot().unwrap();
    assert!(matches!(
        classify(&live, live.at_secs),
        CheckerVerdict::Fresh { .. }
    ));
    assert!(!live.holds_lane());
    assert_eq!(
        live.blocked_by(),
        Some(LaneHolder {
            generation: first,
            phase: CheckerPhase::Checking,
            at_secs: stalled_at,
        })
    );
    assert_eq!(
        standing_stall(&live, stalled_at + 4_000),
        Some((4_000, CheckerPhase::Checking))
    );
    drop(mark);
    let live = watch.snapshot().unwrap();
    assert_eq!(live.lane, None, "the stuck check let go");
    assert_eq!(standing_stall(&live, live.at_secs + 60), None);
    let own = watch.hold_lane(second);
    assert!(watch.snapshot().unwrap().holds_lane());
    drop(own);
    assert_eq!(watch.snapshot().unwrap().lane, None);
}

#[test]
fn only_the_retired_holder_is_kept_and_only_the_live_generation_counts_checks() {
    let watch = CheckerWatch::new();
    let first = watch.register(100);
    watch.note_check(first);
    assert_eq!(watch.snapshot().unwrap().checks, 1);
    let second = watch.supersede(first).unwrap();
    assert_eq!(watch.snapshot().unwrap().lane, None, "it held nothing");
    watch.note_check(first);
    assert_eq!(
        watch.snapshot().unwrap().checks,
        1,
        "a retired thread's late check is not the live loop checking"
    );
    watch.note_check(second);
    assert_eq!(watch.snapshot().unwrap().checks, 2);
}

#[test]
fn progress_restamps_only_a_check_in_flight() {
    let watch = CheckerWatch::new();
    assert!(!watch.beat_progress(0));
    assert!(watch.snapshot().is_none(), "no checker, nothing to stamp");
    let g = watch.register(100);
    assert!(watch.beat(g, CheckerPhase::Waiting));
    assert!(!watch.beat_progress(g));
    assert_eq!(
        watch.snapshot().unwrap().phase,
        CheckerPhase::Waiting,
        "a manual check's download says nothing about a waiting loop"
    );
    assert!(watch.beat(g, CheckerPhase::Checking));
    assert!(watch.beat_progress(g));
    let snap = watch.snapshot().unwrap();
    assert_eq!(snap.phase, CheckerPhase::Checking);
    assert_eq!(snap.generation, g);
    let replacement = watch.supersede(g).unwrap();
    assert!(watch.beat(replacement, CheckerPhase::Checking));
    assert!(!watch.beat_progress(g));
    assert!(watch.beat_progress(replacement));
}

#[test]
fn a_streak_speaks_on_its_edges_and_every_nth_repeat() {
    let mut log = StreakLog::new(3);
    assert_eq!(log.clear(), None, "no streak, no recovery line");
    assert_eq!(log.note(), Streak::Began);
    assert_eq!(log.note(), Streak::Quiet);
    assert_eq!(log.note(), Streak::Continues(3));
    assert_eq!(log.note(), Streak::Quiet);
    assert_eq!(log.note(), Streak::Quiet);
    assert_eq!(log.note(), Streak::Continues(6));
    assert_eq!(log.consecutive(), 6);
    assert_eq!(log.clear(), Some(6));
    assert_eq!(log.clear(), None);
    assert_eq!(log.note(), Streak::Began, "a new streak begins again");
}

#[test]
fn every_phase_round_trips_through_the_packed_word() {
    for phase in [
        CheckerPhase::Starting,
        CheckerPhase::StartFailed,
        CheckerPhase::Stopped,
        CheckerPhase::Settings,
        CheckerPhase::BundleProbe,
        CheckerPhase::LockWait,
        CheckerPhase::Checking,
        CheckerPhase::Waiting,
    ] {
        let word = pack(7, phase, 12_345);
        assert_eq!(generation_of(word), 7);
        assert_eq!(
            CheckerPhase::from_tag((word >> PHASE_SHIFT) & PHASE_MASK),
            phase
        );
        assert_eq!(word & SECS_MASK, 12_345);
    }
}

#[test]
fn refused_thread_starts_retry_without_reviving_or_spending_worker_slots() {
    use std::sync::Arc;
    let watch = CheckerWatch::new();
    let wakes = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&wakes);
    watch.set_phase_wake(Box::new(move || {
        seen.fetch_add(1, Ordering::Relaxed);
    }));
    let first = watch.register(10_000);
    watch.unstarted(first);
    let failed = watch.snapshot().unwrap();
    assert_eq!(failed.phase, CheckerPhase::StartFailed);
    assert_eq!(failed.respawns, 0);
    assert_eq!(wakes.load(Ordering::Relaxed), 1);
    assert_eq!(
        classify(&failed, failed.at_secs + START_RETRY.as_secs() - 1),
        CheckerVerdict::Fresh { stale_in_secs: 1 }
    );
    assert!(matches!(
        classify(&failed, failed.at_secs + START_RETRY.as_secs()),
        CheckerVerdict::Stalled {
            phase: CheckerPhase::StartFailed,
            ..
        }
    ));
    let mut current = first;
    for _ in 0..MAX_RESPAWNS + 1 {
        let next = watch.supersede(current).unwrap();
        watch.unstarted(current);
        assert_eq!(
            watch.snapshot().unwrap().respawns,
            1,
            "stale failure ignored"
        );
        watch.unstarted(next);
        watch.unstarted(next);
        assert_eq!(watch.snapshot().unwrap().respawns, 0, "returned only once");
        assert!(!watch.beat(current, CheckerPhase::Checking));
        current = next;
    }
    let next = watch.supersede(current).unwrap();
    assert!(watch.beat(next, CheckerPhase::Checking));
    watch.unstarted(next);
    assert_eq!(watch.snapshot().unwrap().phase, CheckerPhase::Checking);
    assert_eq!(watch.snapshot().unwrap().respawns, 1);
}
