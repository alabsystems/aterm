// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    DeadlineOwner, EventWakeKind, Heal, Metrics, PresentClock, PresentObserver,
    SLOW_PRESENT_EPISODE_QUIET_NS, SLOW_PRESENT_LOG_THRESHOLD_NS, SlowEpisode, SlowEpisodeStep,
    WindowPresentState, emit_lateness, inert_text, needle_episode_left, now_ns, slow_episode_close,
    slow_episode_step, wire,
};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const LATE_NS: u64 = 400_000_000;

fn passed(deadline: Option<Instant>) -> Heal {
    Heal {
        armed: deadline,
        floor: false,
        streak: false,
    }
}

fn wait_out_the_process_clock() {
    while now_ns() < LATE_NS + 50_000_000 {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The needle's latest episode has left what it owed less its own `deck_clock` fires (never
/// the live edge's `stats_live`, never an interrupted wait), a rested episode owes none, and a reset clears the
/// window's sweeps and owed frames but keeps what the episode in flight has left: what the
/// `measure` spin row bounds the needle's fires by.
#[test]
fn a_needle_episode_has_left_what_it_owed_less_its_own_fires() {
    let m = Metrics::new();
    let now = Instant::now();
    let soon = now + Duration::from_millis(5);
    let fire = |owner| {
        m.note_deadline(owner, Some(soon), now, passed(Some(soon)));
        m.note_event_wake(EventWakeKind::Timer);
    };
    m.note_needle_episode(5);
    fire(DeadlineOwner::DeckClock);
    fire(DeadlineOwner::StatsLive);
    fire(DeadlineOwner::DeckClock);
    m.note_deadline(
        DeadlineOwner::DeckClock,
        Some(soon),
        now,
        passed(Some(soon)),
    );
    m.note_event_wake(EventWakeKind::WaitCancelled);
    assert_eq!(needle_episode_left(&m.live), 3, "two of its own fires");
    let text = inert_text(&m, 0);
    for field in [
        "needle_sweeps=1",
        "needle_frames_owed=5",
        "needle_episode_left=3",
    ] {
        assert!(
            text.split_whitespace().any(|f| f == field),
            "{field} in {text}"
        );
    }
    m.reset();
    assert_eq!(
        (
            m.window.sched.needle_sweeps.get(),
            m.window.sched.needle_frames_owed.get(),
            needle_episode_left(&m.live)
        ),
        (0, 0, 3),
        "a reset keeps what the episode in flight has left"
    );
    for _ in 0..4 {
        fire(DeadlineOwner::DeckClock);
    }
    assert_eq!(needle_episode_left(&m.live), 0, "fires past it leave none");
    m.note_needle_episode(7);
    assert_eq!(
        needle_episode_left(&m.live),
        7,
        "a new episode starts afresh"
    );
    m.note_needle_episode(0);
    assert_eq!(
        (
            needle_episode_left(&m.live),
            m.window.sched.needle_sweeps.get()
        ),
        (0, 1),
        "a rested episode owes none and is no sweep"
    );
}

#[test]
fn fires_and_gpu_scope_are_published() {
    let m = Metrics::new();
    let fires = |m: &Metrics| {
        m.window
            .sched
            .fires_by_owner
            .get(DeadlineOwner::FrameCap.index())
    };
    let now = Instant::now();
    let soon = now + Duration::from_millis(5);
    m.note_deadline(DeadlineOwner::FrameCap, Some(soon), now, passed(Some(soon)));
    m.note_event_wake(EventWakeKind::Timer);
    assert_eq!(fires(&m), 1, "a timer wake is a fire for its owner");
    m.note_event_wake(EventWakeKind::WaitCancelled);
    assert_eq!(fires(&m), 1, "an interrupted deadline did not fire");

    let scope = &m.window.gpu_scope;
    let counts = || {
        (
            scope.scissor_taken.since_reset(),
            scope.full_repaints.since_reset(),
            scope.scroll_rescues.since_reset(),
        )
    };
    scope.scissor_taken.set_total(10);
    scope.full_repaints.set_total(4);
    scope.scroll_rescues.set_total(2);
    m.reset();
    assert_eq!(counts(), (0, 0, 0), "reset rebases the window");
    scope.scissor_taken.set_total(13);
    scope.full_repaints.set_total(5);
    scope.scroll_rescues.set_total(3);
    assert_eq!(counts(), (3, 1, 1));
}

#[test]
fn a_late_timer_survives_the_reader_wake_that_erases_the_last_reading() {
    wait_out_the_process_clock();
    let m = Metrics::new();
    let sched = &m.window.sched;
    let last_wake_late = |m: &Metrics| m.live.last_wake_late.load(Ordering::Relaxed);
    let now = Instant::now();
    let stale = now - Duration::from_nanos(LATE_NS);
    m.note_deadline(
        DeadlineOwner::FrameCap,
        Some(stale),
        now,
        passed(Some(stale)),
    );
    m.note_event_wake(EventWakeKind::Timer);
    assert!(
        last_wake_late(&m) >= LATE_NS,
        "the timer wake must book its lateness at all"
    );

    m.note_event_wake(EventWakeKind::WaitCancelled);
    assert_eq!(
        last_wake_late(&m),
        0,
        "`wake_late_ms` is still the LAST reading — that contract is unchanged"
    );

    assert!(
        sched.wake_late.max() >= LATE_NS,
        "a 400 ms late timer must not be erased by the next wake"
    );
    assert_eq!(
        DeadlineOwner::from_raw(sched.wake_late.by()),
        DeadlineOwner::FrameCap,
        "a number that cannot name its producer cannot end an investigation"
    );
    assert!(
        sched.wake_late.at() != 0,
        "the worst wake must say WHEN it happened"
    );
    assert!(
        sched.deadline_late.max() >= LATE_NS && sched.deadline_late.at() != 0,
        "the deadline lateness keeps its worst case on the same rule"
    );
    assert_eq!(
        DeadlineOwner::from_raw(sched.deadline_late.by()),
        DeadlineOwner::FrameCap
    );

    let ctx = wire::ReadCtx::inert(&m, now_ns());
    let text = wire::render_text(|s| emit_lateness(&ctx, s));
    assert!(
        text.contains(" max_wake_late_owner=frame_cap "),
        "the text fragment must name the owner: {text}"
    );
    assert!(
        text.contains(" metrics_now_ms="),
        "an instant with no anchor cannot be read as `how long ago`: {text}"
    );
    let json = wire::render_json(|s| emit_lateness(&ctx, s));
    assert!(
        json.contains("\"max_deadline_late_owner\":\"frame_cap\""),
        "the JSON twin must carry the same owner: {json}"
    );
}

#[test]
fn the_worst_present_latency_records_its_instant_and_its_frame_gap() {
    let m = Metrics::new();
    let present = &m.window.present;
    let gap_ns = 300_000_000;
    let latency_ns = 4_000_000_000;
    let now = 10_000_000_000;
    let clock = PresentClock {
        now,
        epoch: 0,
        capture_forced: false,
        tainted: false,
    };
    let mut window = WindowPresentState {
        last_present_ns: now - gap_ns,
        ..WindowPresentState::default()
    };
    let mut observer = PresentObserver::default();
    present.submit_wall.set(700_000);
    present.submit_cpu.set(300_000);
    present.submit_faults.set(12);
    m.window.gpu.encode_last.set(400_000);
    let booked = m.record_present_at(
        clock,
        latency_ns,
        1_000_000,
        Some(&mut window),
        &mut observer,
    );

    assert_eq!(booked.window_gap_ns, Some(gap_ns));
    assert_eq!(present.latency.max(), latency_ns);
    assert_eq!(
        present.latency_at.get(),
        now,
        "the worst present must say when it happened"
    );
    assert_eq!(
        present.latency_gap.get(),
        gap_ns,
        "the frame gap of the winning sample is what separates a stall from an \
         idle stretch"
    );
    assert_eq!(
        present.latency_render.get(),
        1_000_000,
        "its own frame's render"
    );
    assert_eq!(present.latency_idle.get(), gap_ns);
    assert_eq!(
        (
            present.latency_submit_wall.get(),
            present.latency_submit_cpu.get(),
            present.latency_submit_faults.get(),
            present.latency_encode.get()
        ),
        (700_000, 300_000, 12, 400_000),
        "its own frame's submit, on and off a core, and its encode"
    );
    let reset = PresentClock {
        now: now + 2_000_000,
        epoch: now + 1_500_000,
        ..clock
    };
    let mut idle = WindowPresentState {
        last_present_ns: now - gap_ns,
        ..WindowPresentState::default()
    };
    let fresh = Metrics::new();
    let _ = fresh.record_present_at(reset, latency_ns, 2_000_000, Some(&mut idle), &mut observer);
    assert_eq!(
        fresh.window.present.latency_gap.get(),
        0,
        "the gap starts at the metrics window"
    );
    assert_eq!(
        fresh.window.present.latency_idle.get(),
        gap_ns + 2_000_000,
        "the idle stretch before the frame does not"
    );

    let later = PresentClock {
        now: now + 1_000_000,
        ..clock
    };
    let _ = m.record_present_at(
        later,
        1_000_000,
        1_000_000,
        Some(&mut window),
        &mut observer,
    );
    assert_eq!(present.latency_at.get(), now);
    assert_eq!(present.latency_gap.get(), gap_ns);
    present.submit_wall.set(9_000_000);
    let _ = m.record_present_at(
        later,
        2_000_000,
        1_000_000,
        Some(&mut window),
        &mut observer,
    );
    assert_eq!(
        present.latency_submit_wall.get(),
        700_000,
        "a faster present leaves the worst one's frame"
    );
}

#[test]
fn the_slow_present_breadcrumb_is_one_line_per_episode() {
    let slow = SLOW_PRESENT_LOG_THRESHOLD_NS + 1;
    let quiet = SLOW_PRESENT_EPISODE_QUIET_NS;
    let mut ep = SlowEpisode::default();
    let owner = DeadlineOwner::Blink;
    assert_eq!(
        slow_episode_step(&mut ep, SLOW_PRESENT_LOG_THRESHOLD_NS, owner, 1),
        SlowEpisodeStep::Ignored,
        "a frame at the threshold is a slow frame, not an episode"
    );
    assert_eq!(
        slow_episode_step(&mut ep, slow, DeadlineOwner::SessionStatus, 1),
        SlowEpisodeStep::Ignored,
        "a delay owned by the session-status observer is not logged"
    );
    assert_eq!(
        slow_episode_step(&mut ep, slow, DeadlineOwner::AccountProgress, 1),
        SlowEpisodeStep::Ignored,
        "nor is one owned by the account progress poll, which that observer's owner held"
    );
    assert_eq!(
        slow_episode_step(&mut ep, slow, owner, 1),
        SlowEpisodeStep::Opened { closed: None },
        "the first spike of a process opens an episode, even in its first second"
    );

    let mut at = 1;
    while at < 27 * 3600 * 1_000_000_000 {
        at += 10_000_000_000;
        assert_eq!(
            slow_episode_step(&mut ep, slow + at % 7, owner, at),
            SlowEpisodeStep::Joined,
            "a continuous stall wrote another line at {at}"
        );
        assert_eq!(slow_episode_close(&mut ep, at + 1, false), None);
    }
    assert_eq!(ep.samples, 27 * 360 + 1);
    assert_eq!(ep.worst_ns, slow + 6);
    let closed = slow_episode_close(&mut ep, at + quiet, false)
        .expect("a minute without a slow present closes the episode");
    assert_eq!(closed.samples, 27 * 360 + 1);
    assert_eq!(ep, SlowEpisode::default());

    assert_eq!(
        slow_episode_step(&mut ep, slow, owner, at),
        SlowEpisodeStep::Opened { closed: None }
    );
    let again = at + quiet;
    match slow_episode_step(&mut ep, slow * 2, owner, again) {
        SlowEpisodeStep::Opened { closed: Some(old) } => {
            assert_eq!((old.start_ns, old.samples), (at, 1));
        }
        other => panic!("expected a new episode, got {other:?}"),
    }
    assert_eq!((ep.start_ns, ep.worst_ns), (again, slow * 2));
    assert_eq!(
        slow_episode_step(&mut ep, slow, owner, again + quiet - 1),
        SlowEpisodeStep::Joined,
        "under a minute apart is the same episode"
    );
    assert_eq!(
        slow_episode_close(&mut ep, again + 1, true).map(|e| e.samples),
        Some(2),
        "`metrics reset` closes an open episode"
    );
}

#[test]
fn metrics_reset_leaves_the_episode_for_the_ui_thread_to_close() {
    let m = Metrics::new();
    let slow = SLOW_PRESENT_LOG_THRESHOLD_NS + 1;
    let owner = DeadlineOwner::Blink;
    let at = 1_000_000_000;
    let open = || {
        let mut observer = PresentObserver::default();
        observer.note_slow_present(&m, slow, 0, at, owner);
        observer.note_slow_present(&m, slow, 0, at + 1, owner);
        observer
    };
    let mut observer = open();
    assert_eq!(observer.episode.samples, 2);
    observer.honour(at + 1);
    assert_eq!(
        observer.episode.samples, 2,
        "a slow present at the epoch belongs to the new window"
    );

    let clock = PresentClock {
        now: at + 3,
        epoch: at + 2,
        capture_forced: false,
        tainted: false,
    };
    let _ = m.record_present_at(clock, 0, 1_000_000, None, &mut observer);
    assert_eq!(
        observer.episode.samples, 0,
        "the next present after a reset closes it"
    );

    observer.note_slow_present(&m, slow, 0, at + 4, owner);
    observer.honour(at + 2);
    assert_eq!(
        (observer.episode.samples, observer.episode.start_ns),
        (1, at + 4),
        "a slow present after the reset opens a new episode at its own instant"
    );

    let mut unreset = open();
    unreset.note_slow_present(&m, slow, 0, at + 4, owner);
    assert_eq!((unreset.episode.samples, unreset.episode.start_ns), (3, at));
}

#[test]
fn the_os_queue_share_of_key_write_is_published_apart_from_it() {
    let m = Metrics::new();
    let queue = &m.window.input.key_queue;

    let parked_ns = 17_000_000;
    let before = now_ns();
    m.with_key_arrival_queued(Some(parked_ns), || {
        let after = now_ns();
        let arrival = m.key_arrival_ns();
        assert!(
            (before.saturating_sub(parked_ns).max(1)..=after.saturating_sub(parked_ns).max(1))
                .contains(&arrival),
            "the arrival stamp is backdated by the queue wait"
        );
    });
    assert_eq!(
        (queue.last(), queue.max()),
        (parked_ns, parked_ns),
        "the backdate is now a reading of its own, not just a moved stamp"
    );
    assert_eq!(queue.copy_out().n(), 1);
    let queue_p99 = queue.copy_out().percentile(0.99).unwrap();
    assert!(
        (parked_ns..parked_ns + parked_ns / 4).contains(&queue_p99),
        "the queue distribution reports the wait it was handed, as a bucket \
         upper edge; got {queue_p99} ns"
    );

    m.with_key_arrival_queued(Some(0), || {});
    assert_eq!(
        (queue.last(), queue.max()),
        (0, parked_ns),
        "a 0 is this key's reading; the worst key still owns the max"
    );
    assert_eq!(queue.copy_out().n(), 2);

    m.with_key_arrival_queued(Some(parked_ns), || {});
    assert_eq!(queue.copy_out().n(), 3);
    assert_eq!((queue.last(), queue.max()), (parked_ns, parked_ns));
    assert_eq!(m.window.input.key_write.copy_out().n(), 0);

    m.reset();
    assert_eq!((queue.last(), queue.max()), (0, 0));
    assert_eq!(queue.copy_out().n(), 0);
}
