// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

static BEAT_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn beat_serial() -> std::sync::MutexGuard<'static, ()> {
    BEAT_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn park_modal_restores_the_outer_root_with_a_fresh_beat() {
    static WATCH: MainWatch = MainWatch::new();
    let _serial = beat_serial();
    beat_on(&WATCH, Breadcrumb::WindowEvent);
    let before = WATCH.heartbeat.load(Ordering::Relaxed);
    {
        let _park = park_modal_in(&WATCH);
        assert_eq!(WATCH.current(), Breadcrumb::Modal);
    }
    assert_eq!(WATCH.current(), Breadcrumb::WindowEvent);
    assert_eq!(WATCH.heartbeat.load(Ordering::Relaxed), before + 2);
}

#[test]
fn a_phase_is_scoped_to_its_guard_and_cleared_by_the_next_root() {
    static PH: AtomicU8 = AtomicU8::new(Phase::None as u8);
    const T0: u64 = 11_000_000_000;
    const MS: u64 = 1_000_000;

    let _serial = beat_serial();
    let turns = TurnLedger::new();
    let watch = MainWatch::new();

    beat_into(&watch, &turns, &PH, Breadcrumb::UserEvent, T0);
    assert_eq!(
        watch.heartbeat.load(Ordering::Relaxed),
        1,
        "a root entry beats"
    );
    assert_eq!(phase_in(&PH), Phase::None, "a root entry announces nothing");
    {
        let _capture = announce(&PH, Phase::ImageCapture);
        assert_eq!(phase_in(&PH), Phase::ImageCapture);
        {
            let _redeem = announce(&PH, Phase::PixelBackendRedeem);
            assert_eq!(
                phase_in(&PH),
                Phase::PixelBackendRedeem,
                "the inner phase reads while it lasts"
            );
        }
        assert_eq!(
            phase_in(&PH),
            Phase::ImageCapture,
            "the outer phase comes back when the inner one ends"
        );
    }
    assert_eq!(phase_in(&PH), Phase::None);
    let held = announce(&PH, Phase::PixelBackendRedeem);
    beat_into(&watch, &turns, &PH, Breadcrumb::AboutToWait, T0 + MS);
    assert_eq!(
        phase_in(&PH),
        Phase::None,
        "the next root's beat clears a phase whose guard is still alive"
    );
    drop(held);
    assert_eq!(phase_in(&PH), Phase::None);
    beat_into(&watch, &turns, &PH, Breadcrumb::UserEvent, T0 + 2 * MS);
    let outer = announce(&PH, Phase::ImageCapture);
    let inner = announce(&PH, Phase::PixelBackendRedeem);
    beat_into(&watch, &turns, &PH, Breadcrumb::AboutToWait, T0 + 3 * MS);
    assert_eq!(phase_in(&PH), Phase::None);
    drop(inner);
    assert_eq!(
        phase_in(&PH),
        Phase::None,
        "a stale inner guard must not re-announce the outer phase"
    );
    drop(outer);
    assert_eq!(phase_in(&PH), Phase::None);
    let outer = announce(&PH, Phase::ImageCapture);
    let inner = announce(&PH, Phase::PixelBackendRedeem);
    beat_into(&watch, &turns, &PH, Breadcrumb::UserEvent, T0 + 4 * MS);
    let fresh = announce(&PH, Phase::ImageCapture);
    assert_eq!(phase_in(&PH), Phase::ImageCapture);
    drop(fresh);
    assert_eq!(
        phase_in(&PH),
        Phase::None,
        "the fresh guard sampled the beat's None and puts it back"
    );
    drop(inner);
    drop(outer);
    assert_eq!(phase_in(&PH), Phase::None);
}

#[test]
fn a_work_root_turn_is_attributable_and_a_designed_park_is_never_booked() {
    const OPEN: u64 = 1_000_000;
    const PARK_NS: u64 = 600 * 1_000_000_000;
    for work in [
        Breadcrumb::WindowEvent,
        Breadcrumb::UserEvent,
        Breadcrumb::NewEvents,
        Breadcrumb::ResizeSettle,
        Breadcrumb::AboutToWaitWork,
    ] {
        assert_eq!(
            TurnLedger::attributable_span(work, OPEN, OPEN + 300_000_000),
            Some(300_000_000),
            "a 300 ms park in `{}` is the exact reading this census exists to \
             name: the release sampler ignores it and the redraw timer never \
             sees it",
            work.as_str()
        );
    }
    for park in [
        Breadcrumb::Startup,
        Breadcrumb::AboutToWait,
        Breadcrumb::UpdateHandoff,
        Breadcrumb::Modal,
        Breadcrumb::Launching,
    ] {
        assert_eq!(
            TurnLedger::attributable_span(park, OPEN, OPEN + PARK_NS),
            None,
            "`{}` is a DESIGNED freeze — booking it would make every reading \
             on this line meaningless",
            park.as_str()
        );
    }
    assert_eq!(
        TurnLedger::attributable_span(Breadcrumb::UserEvent, 0, OPEN + PARK_NS),
        None,
        "a turn that began before this window is not this window's to price"
    );
}

#[test]
fn the_turn_census_names_the_worst_root_and_counts_the_long_turns() {
    const T0: u64 = 5_000_000_000;
    const LONG: u64 = 312_000_000;
    const SHORT: u64 = 2_000_000;
    let ledger = TurnLedger::new();

    assert_eq!(ledger.close(Breadcrumb::AboutToWait, T0), None);
    let c = ledger.snapshot();
    assert_eq!(c.turns, 0);
    assert_eq!(
        c.max_owner, None,
        "an empty census must name no owner at all: `startup` would be a \
         claim about a root that never ran"
    );

    assert_eq!(ledger.close(Breadcrumb::UserEvent, T0 + LONG), Some(LONG));
    let c = ledger.snapshot();
    assert_eq!(c.max_ns, LONG);
    assert_eq!(
        c.max_owner,
        Some(Breadcrumb::UserEvent),
        "a max that cannot name its root sends the next reader to the GPU"
    );
    assert_eq!(c.max_at_ns, T0 + LONG, "the worst turn says WHEN it ended");
    assert_eq!(c.last_ns, LONG);
    assert_eq!((c.turns, c.long_turns), (1, 1));

    assert_eq!(
        ledger.close(Breadcrumb::WindowEvent, T0 + LONG + SHORT),
        Some(SHORT)
    );
    let c = ledger.snapshot();
    assert_eq!((c.max_ns, c.last_ns), (LONG, SHORT));
    assert_eq!(
        c.max_owner,
        Some(Breadcrumb::UserEvent),
        "a shorter turn must never steal the max's owner label"
    );
    assert_eq!(
        (c.turns, c.long_turns),
        (2, 1),
        "the count is what separates one hitch from a main thread that is \
         late all the time"
    );

    let roots = |ledger: &TurnLedger| {
        ledger
            .root_rows()
            .into_iter()
            .map(|(root, n, ..)| (root, n))
            .collect::<Vec<_>>()
    };
    assert_eq!(roots(&ledger), [("window_event", 1), ("user_event", 1)]);

    let idle = T0 + LONG + SHORT;
    assert_eq!(
        ledger.close(Breadcrumb::AboutToWait, idle + 600_000_000_000),
        None
    );
    assert_eq!(ledger.snapshot().turns, 2);
    assert_eq!(roots(&ledger), [("window_event", 1), ("user_event", 1)]);

    ledger.reset();
    let c = ledger.snapshot();
    assert_eq!((c.max_ns, c.last_ns, c.turns, c.long_turns), (0, 0, 0, 0));
    assert_eq!(c.max_owner, None);
    assert_eq!(roots(&ledger), [], "the distributions are window stats");
    assert_eq!(
        ledger.close(Breadcrumb::UserEvent, T0 + 900_000_000_000),
        None,
        "the turn open across a `metrics reset` began outside the new window"
    );
}

#[test]
fn a_beat_books_the_turn_it_closes_to_the_root_that_owned_it() {
    let _serial = beat_serial();
    const T0: u64 = 7_000_000_000;
    const PARKED: u64 = 250_000_000;
    let turns = TurnLedger::new();
    let phase = AtomicU8::new(Phase::None as u8);
    let watch = MainWatch::new();

    assert_eq!(
        beat_into(&watch, &turns, &phase, Breadcrumb::UserEvent, T0),
        None
    );
    assert_eq!(watch.current(), Breadcrumb::UserEvent);

    assert_eq!(
        beat_into(&watch, &turns, &phase, Breadcrumb::AboutToWait, T0 + PARKED),
        Some(PARKED),
        "the beat that ends a 250 ms `user_event` turn must book it"
    );
    let c = turns.snapshot();
    assert_eq!(c.max_ns, PARKED);
    assert_eq!(
        c.max_owner,
        Some(Breadcrumb::UserEvent),
        "the turn belongs to the root that HELD the thread, not to the one \
         that ended it"
    );
    assert_eq!((c.turns, c.long_turns), (1, 1));

    assert_eq!(
        beat_into(
            &watch,
            &turns,
            &phase,
            Breadcrumb::NewEvents,
            T0 + PARKED + 600_000_000_000
        ),
        None
    );
    assert_eq!(turns.snapshot().turns, 1);
}

#[test]
fn each_root_is_a_stall_only_as_a_work_root_past_the_bar() {
    let ten_min = Duration::from_secs(600);
    let under = STALL_THRESHOLD - Duration::from_millis(1);
    #[rustfmt::skip]
    let table = [
        (Breadcrumb::ResizeSettle, STALL_THRESHOLD, STALL_THRESHOLD, true),
        (Breadcrumb::ResizeSettle, under, STALL_THRESHOLD, false),
        (Breadcrumb::WindowEvent, STALL_THRESHOLD, STALL_THRESHOLD, true),
        (Breadcrumb::WindowEvent, Duration::from_secs(1), RELEASE_STALL_THRESHOLD, false),
        (Breadcrumb::NewEvents, ten_min, STALL_THRESHOLD, true),
        (Breadcrumb::UserEvent, Duration::from_secs(3588), RELEASE_STALL_THRESHOLD, true),
        (Breadcrumb::AboutToWait, ten_min, STALL_THRESHOLD, false),
        (Breadcrumb::Startup, ten_min, STALL_THRESHOLD, false),
        (Breadcrumb::Modal, ten_min, STALL_THRESHOLD, false),
        (Breadcrumb::Modal, ten_min, RELEASE_STALL_THRESHOLD, false),
        (Breadcrumb::UpdateHandoff, ten_min, RELEASE_STALL_THRESHOLD, false),
        (Breadcrumb::AboutToWaitWork, STALL_THRESHOLD, STALL_THRESHOLD, true),
        (Breadcrumb::ShutdownWait, ten_min, STALL_THRESHOLD, false),
        (Breadcrumb::Launching, ten_min, STALL_THRESHOLD, true),
        (Breadcrumb::Launching, LAUNCH_STALL_BAR, LAUNCH_STALL_BAR, true),
    ];
    for (bc, frozen, bar, stall) in table {
        assert_eq!(
            is_stall_at(bc, frozen, bar),
            stall,
            "{bc:?} frozen {frozen:?} at the {bar:?} bar"
        );
        assert_eq!(Breadcrumb::from_u8(bc as u8), bc);
    }
    assert_eq!(Breadcrumb::from_u8(200), Breadcrumb::Startup);
    assert!(Breadcrumb::from_u8(200).is_park_point());
}

#[test]
fn the_about_to_wait_body_is_booked_and_the_wait_after_it_never_is() {
    const MS: u64 = 1_000_000;
    const T0: u64 = 7_000 * MS;
    let ledger = TurnLedger::new();
    assert_eq!(ledger.close(Breadcrumb::AboutToWait, T0), None, "arms only");
    assert_eq!(ledger.close(Breadcrumb::WindowEvent, T0 + MS), Some(MS));
    assert_eq!(
        ledger.close(Breadcrumb::AboutToWaitWork, T0 + 3 * MS),
        Some(2 * MS)
    );
    assert_eq!(
        ledger.close(Breadcrumb::AboutToWait, T0 + 3 * MS + 600_000 * MS),
        None
    );
    let c = ledger.snapshot();
    assert_eq!((c.turns, c.busy_ns), (2, 3 * MS));
    assert_eq!(c.max_owner.map(Breadcrumb::as_str), Some("about_to_wait"));
    assert!((c.busy_ms_per_s(3_000 * MS) - 1.0).abs() < 1e-9);
    assert_eq!(c.busy_ms_per_s(0), 0.0, "an empty window is idle");
    ledger.reset();
    assert_eq!(ledger.snapshot().busy_ns, 0, "a window stat clears");
}

#[test]
fn a_fork_waits_for_the_main_thread_to_park() {
    let watch = MainWatch::new();
    let _serial = beat_serial();
    for root in [
        Breadcrumb::UserEvent,
        Breadcrumb::WindowEvent,
        Breadcrumb::NewEvents,
        Breadcrumb::AboutToWaitWork,
    ] {
        beat_on(&watch, root);
        assert!(!watch.parked(), "{root:?} is a turn");
        assert!(!watch.wait_for_park(Duration::ZERO), "{root:?}");
    }
    for park in [
        Breadcrumb::AboutToWait,
        Breadcrumb::Startup,
        Breadcrumb::Modal,
        Breadcrumb::UpdateHandoff,
        Breadcrumb::Launching,
    ] {
        beat_on(&watch, park);
        assert!(watch.parked(), "{park:?} lets a fork go");
        assert!(watch.wait_for_park(Duration::from_secs(60)), "{park:?}");
    }
}

#[test]
fn a_fork_waits_for_the_park_only_inside_a_spawn_that_asks() {
    let watch = MainWatch::new();
    let _serial = beat_serial();
    let bound = Duration::from_millis(30);
    let ambient = || FORK_PARK_BOUND.get();
    beat_on(&watch, Breadcrumb::UserEvent);
    assert_eq!(ambient(), Duration::ZERO, "no spawn asked: no wait");
    assert!(!park_before_fork_on(&watch));
    let inside = forks_wait_for_park(bound, || {
        let t0 = std::time::Instant::now();
        (
            ambient(),
            park_before_fork_on(&watch),
            t0.elapsed() >= bound,
        )
    });
    assert_eq!(
        inside,
        (bound, false, true),
        "a turn holds the fork for its bound"
    );
    assert_eq!(ambient(), Duration::ZERO, "the bound went with the spawn");
    let unwound =
        std::panic::catch_unwind(|| forks_wait_for_park(bound, || panic!("the spawn panicked")));
    assert!(unwound.is_err());
    assert_eq!(
        ambient(),
        Duration::ZERO,
        "the bound went with the unwound spawn"
    );
    beat_on(&watch, Breadcrumb::AboutToWait);
    assert!(forks_wait_for_park(Duration::from_secs(60), || {
        park_before_fork_on(&watch)
    }));
}

#[test]
fn about_to_wait_s_guard_parks_only_as_the_handler_returns() {
    static WATCH: MainWatch = MainWatch::new();
    let _serial = beat_serial();
    beat_on(&WATCH, Breadcrumb::UserEvent);
    let before = WATCH.heartbeat.load(Ordering::Relaxed);
    {
        let _root = enter_about_to_wait_in(&WATCH);
        assert_eq!(WATCH.current(), Breadcrumb::AboutToWaitWork);
        assert!(!WATCH.returned.load(Ordering::Relaxed));
    }
    assert_eq!(WATCH.current(), Breadcrumb::AboutToWait);
    assert!(WATCH.returned.load(Ordering::Relaxed));
    assert_eq!(WATCH.heartbeat.load(Ordering::Relaxed), before + 2);
}

#[test]
fn an_off_cpu_block_in_the_about_to_wait_body_is_a_named_stall() {
    let t0 = Instant::now();
    let step = Duration::from_millis(250);
    let quiet = |tick: u32| Some(Duration::from_millis(tick.into()));
    let mut body = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
    let mut idle = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
    let mut hit = None;
    for tick in 1..=8u32 {
        let now = t0 + step * tick;
        hit = hit.or(body.poll_with(now, 3, Breadcrumb::AboutToWaitWork, quiet(tick)));
        assert_eq!(
            idle.poll_with(now, 3, Breadcrumb::AboutToWait, quiet(tick)),
            None,
            "the OS wait is a designed park"
        );
    }
    let hit = hit.expect("the blocked body is reported");
    assert_eq!((hit.root, hit.busy), (Breadcrumb::AboutToWaitWork, false));
    let line = stall_message(1, hit.frozen, hit.root, Phase::None, Locus::Inside, false);
    assert!(line.contains("`AboutToWaitWork`"), "{line}");
    let beat_ns = REFUSAL_T0;
    assert_eq!(
        main_stall(
            beat_ns + 3_000_000,
            beat_ns,
            Breadcrumb::AboutToWaitWork,
            false,
            Some(beat_ns - 1_000_000_000),
        ),
        None
    );
}

#[test]
fn a_reported_stall_that_ends_says_so_once_and_an_unreported_one_never_does() {
    let t0 = Instant::now();
    let mut s = Sampler::with_threshold(t0, 7, STALL_THRESHOLD);
    assert_eq!(
        s.poll(t0 + Duration::from_millis(300), 7, Breadcrumb::UserEvent),
        None
    );
    assert_eq!(
        s.poll(t0 + Duration::from_millis(400), 8, Breadcrumb::UserEvent),
        None
    );
    assert_eq!(s.take_ended(), None);
    let from = t0 + Duration::from_millis(400);
    assert_eq!(
        s.poll(from + Duration::from_millis(600), 8, Breadcrumb::UserEvent),
        Some(Breadcrumb::UserEvent)
    );
    assert_eq!(s.take_ended(), None, "still frozen: nothing has ended");
    assert_eq!(
        s.poll(from + Duration::from_secs(9), 9, Breadcrumb::AboutToWait),
        None
    );
    let ended = Ended {
        frozen: Duration::from_secs(9),
        root: Breadcrumb::UserEvent,
        by: EndedBy::Beat,
    };
    assert_eq!(s.take_ended(), Some(ended));
    assert_eq!(s.take_ended(), None);
    #[cfg(unix)]
    for by in [EndedBy::Beat, EndedBy::Quiet] {
        assert!(
            stall_ended_message(Ended { by, ..ended })
                .starts_with(crate::recovery_census::STALL_ENDED_LINE)
        );
    }
    for locus in [Locus::Inside, Locus::Returned] {
        for reports in [1, 2] {
            assert!(
                !stall_message(
                    reports,
                    Duration::from_secs(5),
                    Breadcrumb::UserEvent,
                    Phase::None,
                    locus,
                    true
                )
                .contains("ENDED")
            );
        }
    }
}

/// NEGATIVE CONTROL: `"off"`, `"0"`, and blank seam values disarm the watchdog, while `None` (including every shipped build) keeps it armed.
#[test]
fn the_watchdog_is_armed_unless_explicitly_switched_off() {
    assert!(
        enabled_for(None),
        "a shipped build must arm the stall watchdog: silence is what \
         cost five hours on 2026-08-30"
    );
    for on in ["1", "on", "ON", "abort", "ABORT", "custom"] {
        assert!(enabled_for(Some(on)), "seam `{on}` must stay armed");
    }
    for off in ["off", "OFF", "Off", "0", "", "   "] {
        assert!(!enabled_for(Some(off)), "seam `{off}` must disarm");
    }

    assert_eq!(threshold_for(false, false), RELEASE_STALL_THRESHOLD);
    assert_eq!(threshold_for(true, false), STALL_THRESHOLD);
    assert_eq!(threshold_for(false, true), STALL_THRESHOLD);
    assert_eq!(threshold_for(true, true), STALL_THRESHOLD);
    assert!(STALL_THRESHOLD < RELEASE_STALL_THRESHOLD);
    assert!(RELEASE_STALL_THRESHOLD < Duration::from_secs(30));

    assert!(!abort_on_stall_for(None));
    assert!(!abort_on_stall_for(Some("1")));
    assert!(!abort_on_stall_for(Some("off")));
    assert!(abort_on_stall_for(Some("abort")));
    assert!(abort_on_stall_for(Some(" ABORT ")));
}

#[test]
fn a_persisting_stall_is_reported_again_on_the_repeat_cadence() {
    let t0 = Instant::now();
    let mut s = Sampler::with_threshold(t0, 1, STALL_THRESHOLD);
    assert_eq!(
        s.poll(t0 + Duration::from_secs(1), 1, Breadcrumb::UserEvent),
        Some(Breadcrumb::UserEvent),
        "the first crossing must report"
    );
    assert!(
        s.poll(t0 + Duration::from_secs(30), 1, Breadcrumb::UserEvent)
            .is_none(),
        "inside the repeat interval it stays quiet — no flood"
    );
    assert_eq!(
        s.poll(
            t0 + Duration::from_secs(1) + STALL_REPEAT_INTERVAL,
            1,
            Breadcrumb::UserEvent
        ),
        Some(Breadcrumb::UserEvent),
        "a stall still live one interval later must say so again"
    );
    assert!(
        s.poll(t0 + Duration::from_secs(200), 2, Breadcrumb::UserEvent)
            .is_none(),
        "a heartbeat means the main thread is back"
    );
}

#[test]
fn a_second_stall_after_recovery_reports_as_a_first_again() {
    let t0 = Instant::now();
    let mut s = Sampler::with_threshold(t0, 1, STALL_THRESHOLD);
    assert_eq!(
        s.poll(t0 + Duration::from_secs(1), 1, Breadcrumb::UserEvent),
        Some(Breadcrumb::UserEvent)
    );
    assert_eq!(s.reports, 1, "the first line of the first stall");
    assert!(
        s.poll(t0 + Duration::from_secs(2), 2, Breadcrumb::UserEvent)
            .is_none()
    );
    assert_eq!(s.reports, 0, "recovery re-arms the report count");
    assert_eq!(
        s.poll(t0 + Duration::from_secs(9000), 2, Breadcrumb::WindowEvent),
        Some(Breadcrumb::WindowEvent)
    );
    assert_eq!(
        s.reports, 1,
        "a separate wedge must announce itself in full, not as a continuation"
    );
}

#[test]
fn the_2026_09_26_spin_is_named_as_outside_aterm_and_on_cpu() {
    static WATCH: MainWatch = MainWatch::new();
    let _serial = beat_serial();
    {
        let _root = enter_in(&WATCH, Breadcrumb::NewEvents);
        assert!(
            !WATCH.returned.load(Ordering::Relaxed),
            "inside the handler"
        );
    }
    assert!(
        WATCH.returned.load(Ordering::Relaxed),
        "the handler returned"
    );
    assert_eq!(WATCH.current(), Breadcrumb::NewEvents);

    let t0 = Instant::now();
    let mut s = Sampler::with_threshold(t0, 9, RELEASE_STALL_THRESHOLD);
    let mut hit = None;
    for tick in 1..=4u32 {
        let at = t0 + Duration::from_millis(2_500) * tick;
        let cpu = Duration::from_millis(2_500) * tick;
        hit = hit.or(s.poll_with(at, 9, Breadcrumb::NewEvents, Some(cpu)));
    }
    let hit = hit.expect("a frozen work root past the bar is a stall");
    assert!(hit.busy, "on-CPU across the samples: a spin, not a park");
    let line = stall_message(
        1,
        hit.frozen,
        hit.root,
        Phase::None,
        Locus::Returned,
        hit.busy,
    );
    assert!(line.contains("since `NewEvents` returned"), "{line}");
    assert!(line.contains("ON-CPU"), "{line}");
    assert!(!line.contains("while inside"), "{line}");
    {
        let _root = enter_in(&WATCH, Breadcrumb::UserEvent);
        drop(park_modal_in(&WATCH));
        assert!(
            !WATCH.returned.load(Ordering::Relaxed),
            "back inside the outer root"
        );
    }
}

#[test]
fn a_spin_at_the_idle_park_is_a_stall_and_a_quiet_park_is_not() {
    let t0 = Instant::now();
    let step = Duration::from_millis(250);
    let run = |bc: Breadcrumb, busy: bool| {
        let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
        let mut hits = Vec::new();
        for tick in 1..=12u32 {
            let cpu = if busy {
                step * tick
            } else {
                Duration::from_millis(1) * tick
            };
            if let Some(hit) = s.poll_with(t0 + step * tick, 3, bc, Some(cpu)) {
                hits.push((tick, hit));
            }
        }
        hits
    };
    for park in [Breadcrumb::AboutToWait, Breadcrumb::ShutdownWait] {
        let spun = run(park, true);
        assert_eq!(spun.len(), 1, "one report per contiguous spin: {spun:?}");
        let (tick, hit) = spun[0];
        assert!(hit.busy && hit.root == park);
        assert!(hit.frozen >= STALL_THRESHOLD, "{hit:?}");
        assert_eq!(
            tick, 3,
            "busy from the first interval, the bar crossed at the third sample"
        );
        assert!(
            run(park, false).is_empty(),
            "a quiet {park:?} is not a stall"
        );
        let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
        for tick in 1..=12u32 {
            assert!(s.poll_with(t0 + step * tick, 3, park, None).is_none());
        }
    }
    for designed in [
        Breadcrumb::Startup,
        Breadcrumb::Modal,
        Breadcrumb::UpdateHandoff,
    ] {
        assert!(
            run(designed, true).is_empty(),
            "{designed:?} does real work by design"
        );
    }
}

#[test]
fn a_process_gap_is_named_and_restarts_the_stall_clock() {
    let sample = RELEASE_STALL_THRESHOLD / 2;
    assert_eq!(
        process_gap(sample, sample + Duration::from_secs(95_674)),
        Some(Duration::from_secs(95_674))
    );
    assert_eq!(process_gap(sample, sample + Duration::from_secs(3)), None);
    assert_eq!(process_gap(sample, Duration::ZERO), None);

    let t0 = Instant::now();
    let mut s = Sampler::with_threshold(t0, 5, RELEASE_STALL_THRESHOLD);
    let back = t0 + Duration::from_secs(95_676);
    s.resync(back, 5, Duration::from_secs(95_674));
    assert!(
        s.poll_with(
            back + Duration::from_secs(1),
            5,
            Breadcrumb::UserEvent,
            None
        )
        .is_none(),
        "the gap is not a stall of the main thread"
    );
    assert!(
        s.poll_with(
            back + Duration::from_secs(6),
            5,
            Breadcrumb::UserEvent,
            None
        )
        .is_some(),
        "a real wedge after the gap still reports"
    );
}

fn wake_logged(
    s: &mut Sampler,
    lines: &mut Vec<String>,
    now: Instant,
    slept: Duration,
    beat: u64,
    bc: Breadcrumb,
) -> SamplerWake {
    let elapsed = Elapsed {
        mono: slept,
        wall: Some(slept),
        pass: Duration::ZERO,
    };
    wake_logged_on(s, lines, now, elapsed, beat, bc)
}

fn wake_logged_on(
    s: &mut Sampler,
    lines: &mut Vec<String>,
    now: Instant,
    elapsed: Elapsed,
    beat: u64,
    bc: Breadcrumb,
) -> SamplerWake {
    let wake = s.wake(now, s.threshold / 2, elapsed, beat, bc, None);
    lines.extend(wake.sleep_note());
    lines.extend(wake.notices());
    if let Some(hit) = wake.hit {
        lines.push(stall_message(
            s.reports,
            hit.frozen,
            hit.root,
            Phase::None,
            Locus::Inside,
            hit.busy,
        ));
    }
    wake
}

fn awake_gap(late: Duration) -> Option<Gap> {
    Some(Gap {
        unscheduled: Some(late),
        wall: Wall::Awake,
        pass: Duration::ZERO,
    })
}

#[test]
fn the_2026_09_28_gap_inside_a_report_pass_is_not_charged_to_the_main_thread() {
    let step = RELEASE_STALL_THRESHOLD / 2;
    let pass = Duration::from_secs(12_130);
    let t0 = Instant::now();
    let reported = |lines: &mut Vec<String>| {
        let mut s = Sampler::with_threshold(t0, 5, RELEASE_STALL_THRESHOLD);
        let w = wake_logged(&mut s, lines, t0 + step, step, 5, Breadcrumb::NewEvents);
        assert_eq!(w.hit, None, "under the bar");
        let w = wake_logged(&mut s, lines, t0 + step * 2, step, 5, Breadcrumb::NewEvents);
        assert_eq!(w.hit.map(|h| h.root), Some(Breadcrumb::NewEvents));
        s
    };
    let back = t0 + step * 3 + pass;

    let mut lines = Vec::new();
    let mut s = reported(&mut lines);
    let w = wake_logged_on(
        &mut s,
        &mut lines,
        back,
        Elapsed {
            mono: pass + step,
            wall: Some(pass + step),
            pass,
        },
        6,
        Breadcrumb::AboutToWait,
    );
    assert_eq!(
        w.gap,
        Some(Gap {
            unscheduled: Some(pass),
            wall: Wall::Awake,
            pass,
        })
    );
    assert_eq!(
        w.ended.map(|e| e.frozen),
        Some(step * 3),
        "the main thread's own 7.5 s, not the pass's 12130 s"
    );
    assert!(
        lines
            .last()
            .is_some_and(|l| l.starts_with("MAIN-THREAD STALL ENDED:")),
        "{lines:#?}"
    );
    #[cfg(unix)]
    assert_eq!(
        crate::recovery_census::stall_at_end(&lines),
        aterm_update::recovery_ledger::Tri::No,
        "{lines:#?}"
    );

    let mut lines = Vec::new();
    let mut s = reported(&mut lines);
    let w = wake_logged(&mut s, &mut lines, back, step, 6, Breadcrumb::AboutToWait);
    assert_eq!(w.gap, None, "a gap outside the timed sleep is invisible");
    assert_eq!(
        w.ended.map(|e| e.frozen),
        Some(step * 3 + pass),
        "and the main thread is charged with all of it"
    );

    let mut lines = Vec::new();
    let mut s = reported(&mut lines);
    let w = wake_logged_on(
        &mut s,
        &mut lines,
        t0 + step * 3,
        Elapsed {
            mono: step,
            wall: Some(step + pass),
            pass: Duration::ZERO,
        },
        6,
        Breadcrumb::AboutToWait,
    );
    assert_eq!(
        w.gap,
        Some(Gap {
            unscheduled: None,
            wall: Wall::Slept(pass),
            pass: Duration::ZERO,
        })
    );
    assert_eq!(w.ended.map(|e| e.frozen), Some(step * 3));
}

#[test]
fn a_wake_that_finds_the_wall_ahead_says_the_wall_moved() {
    let step = RELEASE_STALL_THRESHOLD / 2;
    let hour = Duration::from_secs(3_600);
    let wake = |mono: Duration, wall: Option<Duration>| SamplerWake {
        gap: classify_gap(
            step,
            Elapsed {
                mono,
                wall,
                pass: Duration::ZERO,
            },
        ),
        ended: None,
        hit: None,
    };
    assert!(wake(step, Some(step + hour)).wall_moved(), "a sleep");
    assert!(
        wake(step + hour, Some(step + hour * 3)).wall_moved(),
        "a sleep beside awake time lost"
    );
    assert!(!wake(step, Some(step)).wall_moved(), "on time");
    assert!(
        !wake(step + hour, Some(step + hour)).wall_moved(),
        "awake time lost alone: the wall moved with the monotonic clock"
    );
    assert!(!wake(step, None).wall_moved(), "the wall went backwards");
    assert!(
        !wake(step, Some(step + PROCESS_GAP - Duration::from_secs(1))).wall_moved(),
        "a lead short of the gap"
    );
}

#[test]
fn a_gap_is_read_on_both_clocks() {
    let step = RELEASE_STALL_THRESHOLD / 2;
    let hour = Duration::from_secs(3_600);
    let lost = Duration::from_secs(100);
    let on = |mono: Duration, wall: Option<Duration>| {
        classify_gap(
            step,
            Elapsed {
                mono,
                wall,
                pass: Duration::ZERO,
            },
        )
    };
    let gap = |unscheduled: Option<Duration>, wall: Wall| {
        Some(Gap {
            unscheduled,
            wall,
            pass: Duration::ZERO,
        })
    };
    assert_eq!(
        on(step + hour, Some(step + hour)),
        gap(Some(hour), Wall::Awake)
    );
    assert_eq!(on(step, Some(step + hour)), gap(None, Wall::Slept(hour)));
    assert_eq!(
        on(step + lost, Some(step + lost + hour)),
        gap(Some(lost), Wall::Slept(hour))
    );
    let jitter = Duration::from_secs(3);
    assert_eq!(on(step + jitter, Some(step + jitter * 4)), None);
    assert_eq!(
        on(step, Some(step + PROCESS_GAP - Duration::from_millis(1))),
        None
    );
    assert_eq!(on(step, None), None);
    assert_eq!(on(step + hour, None), gap(Some(hour), Wall::Unknown));
    assert_eq!(on(step + hour, Some(step)), gap(Some(hour), Wall::Awake));

    let t0 = Instant::now();
    let w0 = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    let reported_for = Duration::from_millis(2);
    let earlier = Clocks { mono: t0, wall: w0 };
    let later = Clocks {
        mono: t0 + reported_for + step,
        wall: w0 + reported_for + step + hour,
    };
    assert_eq!(
        later.since(earlier, t0 + reported_for),
        Elapsed {
            mono: reported_for + step,
            wall: Some(reported_for + step + hour),
            pass: reported_for,
        }
    );
    let set_back = Clocks {
        mono: t0 + step,
        wall: w0 - hour,
    };
    assert_eq!(
        set_back.since(earlier, t0),
        Elapsed {
            mono: step,
            wall: None,
            pass: Duration::ZERO,
        }
    );

    let huge = Duration::from_secs(u64::MAX);
    for line in [
        gap_message(huge, Wall::Slept(huge), huge),
        gap_message(huge, Wall::Awake, huge),
        gap_message(huge, Wall::Unknown, huge),
    ] {
        assert!(
            line.len() <= aterm_log::record_cap(aterm_log::Level::Warn),
            "{} bytes: {line}",
            line.len()
        );
        assert!(!line.contains("MAIN-THREAD STALL"), "{line}");
    }
    let asleep = slept_message(huge);
    assert!(
        asleep.len() <= aterm_log::record_cap(aterm_log::Level::Info),
        "{} bytes: {asleep}",
        asleep.len()
    );
    assert!(!asleep.contains("MAIN-THREAD STALL"), "{asleep}");
}

#[test]
fn the_stack_is_logged_whole_in_numbered_parts_under_the_error_cap() {
    let cap = aterm_log::record_cap(aterm_log::Level::Error);
    let mut frames =
        vec!["load address 0x1004c8000, UUID 4C4C4453-5555-3144-A1B2-C3D4E5F60718".to_string()];
    frames.extend((0..64).map(|i| {
        format!(
            "#{i} AppKit + {} (-[NSSceneStatusItem _setupScene:] + {}) \
             [a synthetic AppKit frame]",
            1_000_000 + 4_096 * i,
            60 + i
        )
    }));
    let captured = UNIX_EPOCH + Duration::from_millis(1_790_191_963_664);

    let old = format!("{STACK_LINE} {}", frames.join(FRAME_SEPARATOR));
    let logged = aterm_log::sanitize_record_for(aterm_log::Level::Error, &old);
    assert!(
        old.contains("| #11 ") && logged.contains("| #10 ") && !logged.contains("| #11 "),
        "{logged}"
    );

    let lines = stack_lines(&frames, captured, cap);
    let parts = lines.len();
    assert!(parts > 1, "{lines:#?}");
    let mut rejoined: Vec<String> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let head = format!(
            "MAIN-THREAD STALL stack: (part {}/{parts}, captured at 1790191963.664) ",
            i + 1
        );
        let body = line
            .strip_prefix(head.as_str())
            .unwrap_or_else(|| panic!("numbered and stamped: {line}"));
        assert!(line.len() <= cap, "{} bytes: {line}", line.len());
        assert_eq!(
            aterm_log::sanitize_record_for(aterm_log::Level::Error, line),
            line.as_str(),
            "the logger passes every part untouched"
        );
        rejoined.extend(body.split(FRAME_SEPARATOR).map(str::to_string));
    }
    assert_eq!(rejoined, frames, "every frame, in order, none split");
}

#[test]
fn an_oversized_frame_is_clipped_into_a_part_of_its_own() {
    let cap = aterm_log::record_cap(aterm_log::Level::Error);
    let captured = UNIX_EPOCH + Duration::from_millis(1_790_191_963_664);
    let frames = [
        "#0 libsystem_kernel.dylib + 4660 (mach_msg2_trap + 8)".to_string(),
        format!("#1 {}", "é".repeat(1_100)),
        format!("#2 {}", "\u{1}".repeat(600)),
        "#3 dyld + 24680 (start + 2360)".to_string(),
    ];
    let lines = stack_lines(&frames, captured, cap);
    assert_eq!(lines.len(), 4, "{lines:#?}");
    for line in &lines {
        assert!(
            logged_len(line) <= cap,
            "{} bytes: {line}",
            logged_len(line)
        );
        assert_eq!(
            aterm_log::sanitize_record_for(aterm_log::Level::Error, line).len(),
            logged_len(line),
            "the logger replaces control characters and cuts nothing: {line}"
        );
    }
    assert!(lines[0].ends_with(") #0 libsystem_kernel.dylib + 4660 (mach_msg2_trap + 8)"));
    assert!(
        lines[1].contains("(part 2/4, ") && lines[1].ends_with("é…"),
        "{}",
        lines[1]
    );
    assert!(lines[2].ends_with("\u{1}…"), "{}", lines[2]);
    assert!(lines[3].ends_with(") #3 dyld + 24680 (start + 2360)"));
    assert!(stack_lines(&[], captured, cap).is_empty());

    assert_eq!(epoch_stamp(captured), "1790191963.664");
    assert_eq!(epoch_stamp(UNIX_EPOCH + Duration::from_millis(5)), "0.005");
    assert!(epoch_stamp(UNIX_EPOCH - Duration::from_secs(1)).starts_with("an unknown time"));
}

#[test]
fn a_stall_reported_before_a_process_gap_is_still_ended_after_it() {
    let step = RELEASE_STALL_THRESHOLD / 2;
    let gap = Duration::from_secs(95_674);
    let t0 = Instant::now();
    let reported = |lines: &mut Vec<String>| {
        let mut s = Sampler::with_threshold(t0, 5, RELEASE_STALL_THRESHOLD);
        let w = wake_logged(&mut s, lines, t0 + step, step, 5, Breadcrumb::UserEvent);
        assert_eq!(w.hit, None, "under the bar");
        let w = wake_logged(&mut s, lines, t0 + step * 2, step, 5, Breadcrumb::UserEvent);
        assert_eq!(w.hit.map(|h| h.root), Some(Breadcrumb::UserEvent));
        s
    };
    let back = t0 + step * 3 + gap;

    let mut lines = Vec::new();
    let mut s = reported(&mut lines);
    let w = wake_logged(
        &mut s,
        &mut lines,
        back,
        step + gap,
        5,
        Breadcrumb::UserEvent,
    );
    assert_eq!((w.gap, w.ended, w.hit), (awake_gap(gap), None, None));
    let w = wake_logged(
        &mut s,
        &mut lines,
        back + step,
        step,
        5,
        Breadcrumb::UserEvent,
    );
    assert_eq!((w.ended, w.hit), (None, None), "under the bar again");
    let w = wake_logged(
        &mut s,
        &mut lines,
        back + step * 2,
        step,
        6,
        Breadcrumb::AboutToWait,
    );
    assert_eq!(
        w.ended,
        Some(Ended {
            frozen: step * 5,
            root: Breadcrumb::UserEvent,
            by: EndedBy::Beat,
        }),
        "the stall reported before the gap ends after it"
    );
    assert!(
        lines
            .last()
            .is_some_and(|l| l.starts_with("MAIN-THREAD STALL ENDED:")),
        "{lines:#?}"
    );
    assert_eq!(s.take_ended(), None, "said once");
    #[cfg(unix)]
    assert_eq!(
        crate::recovery_census::stall_at_end(&lines),
        aterm_update::recovery_ledger::Tri::No,
        "a stall the run survived is not its last word: {lines:#?}"
    );

    let mut lines = Vec::new();
    let mut s = reported(&mut lines);
    let w = wake_logged(
        &mut s,
        &mut lines,
        back,
        step + gap,
        6,
        Breadcrumb::AboutToWait,
    );
    assert_eq!(w.gap, awake_gap(gap));
    assert_eq!(
        w.ended,
        Some(Ended {
            frozen: step * 3,
            root: Breadcrumb::UserEvent,
            by: EndedBy::Beat,
        })
    );
    assert_eq!(w.notices().len(), 2, "the gap line, then the ENDED line");
    #[cfg(unix)]
    assert_eq!(
        crate::recovery_census::stall_at_end(&lines),
        aterm_update::recovery_ledger::Tri::No,
        "{lines:#?}"
    );
    assert_eq!(
        wake_logged(
            &mut s,
            &mut lines,
            back + step,
            step,
            7,
            Breadcrumb::UserEvent
        )
        .ended,
        None,
        "ended once, not again at the next beat"
    );

    let mut lines = Vec::new();
    let mut s = reported(&mut lines);
    wake_logged(
        &mut s,
        &mut lines,
        back,
        step + gap,
        5,
        Breadcrumb::UserEvent,
    );
    #[cfg(unix)]
    assert_eq!(
        crate::recovery_census::stall_at_end(&lines),
        aterm_update::recovery_ledger::Tri::Yes,
        "{lines:#?}"
    );
}

#[test]
fn a_process_gap_does_not_invent_a_stall() {
    let step = RELEASE_STALL_THRESHOLD / 2;
    let gap = Duration::from_secs(95_674);
    let t0 = Instant::now();
    let mut lines = Vec::new();
    let mut s = Sampler::with_threshold(t0, 5, RELEASE_STALL_THRESHOLD);
    let w = wake_logged(
        &mut s,
        &mut lines,
        t0 + step,
        step,
        5,
        Breadcrumb::UserEvent,
    );
    assert_eq!(w.hit, None);
    let back = t0 + step * 2 + gap;
    let w = wake_logged(
        &mut s,
        &mut lines,
        back,
        step + gap,
        5,
        Breadcrumb::UserEvent,
    );
    assert_eq!((w.gap, w.ended, w.hit), (awake_gap(gap), None, None));
    let w = wake_logged(
        &mut s,
        &mut lines,
        back + step,
        step,
        5,
        Breadcrumb::UserEvent,
    );
    assert_eq!(
        (w.ended, w.hit),
        (None, None),
        "the gap is not charged to the main thread"
    );
    let w = wake_logged(
        &mut s,
        &mut lines,
        back + step * 2,
        step,
        6,
        Breadcrumb::UserEvent,
    );
    assert_eq!((w.ended, w.hit), (None, None));
    assert_eq!(lines.len(), 1, "the gap line alone: {lines:#?}");
    #[cfg(unix)]
    assert_eq!(
        crate::recovery_census::stall_at_end(&lines),
        aterm_update::recovery_ledger::Tri::No,
        "{lines:#?}"
    );

    for beat_after in [8, 9] {
        let mut lines = Vec::new();
        let mut s = Sampler::with_threshold(t0, 7, RELEASE_STALL_THRESHOLD);
        wake_logged(
            &mut s,
            &mut lines,
            t0 + step * 2,
            step,
            7,
            Breadcrumb::UserEvent,
        );
        let w = wake_logged(
            &mut s,
            &mut lines,
            t0 + step * 3,
            step,
            8,
            Breadcrumb::UserEvent,
        );
        assert!(w.ended.is_some());
        let w = wake_logged(
            &mut s,
            &mut lines,
            t0 + step * 4 + gap,
            step + gap,
            beat_after,
            Breadcrumb::UserEvent,
        );
        assert_eq!((w.ended, w.hit), (None, None), "beat {beat_after}");
        #[cfg(unix)]
        assert_eq!(
            crate::recovery_census::stall_at_end(&lines),
            aterm_update::recovery_ledger::Tri::No,
            "{lines:#?}"
        );
    }
}

#[test]
fn a_spin_reported_at_the_idle_park_ends_when_a_reading_finds_it_quiet() {
    let t0 = Instant::now();
    let step = Duration::from_millis(250);
    let spin_to_report = |s: &mut Sampler| {
        let mut hit = None;
        for tick in 1..=3u32 {
            hit = hit.or(s.poll_with(
                t0 + step * tick,
                3,
                Breadcrumb::AboutToWait,
                Some(step * tick),
            ));
        }
        assert!(hit.is_some_and(|h| h.busy), "{hit:?}");
        assert_eq!(s.take_ended(), None);
    };
    let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
    spin_to_report(&mut s);
    let cpu = step * 3 + Duration::from_millis(1);
    assert_eq!(
        s.poll_with(t0 + step * 4, 3, Breadcrumb::AboutToWait, Some(cpu)),
        None
    );
    assert_eq!(
        s.take_ended(),
        Some(Ended {
            frozen: step * 3,
            root: Breadcrumb::AboutToWait,
            by: EndedBy::Quiet,
        })
    );
    assert!(
        s.poll_with(t0 + step * 5, 4, Breadcrumb::NewEvents, Some(cpu))
            .is_none()
            && s.take_ended().is_none(),
        "ended once: the next heartbeat ends nothing"
    );
    let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
    spin_to_report(&mut s);
    for tick in 4..=40u32 {
        assert_eq!(
            s.poll_with(t0 + step * tick, 3, Breadcrumb::AboutToWait, None),
            None
        );
        assert_eq!(s.take_ended(), None, "no reading is no evidence it ended");
    }
    s.poll_with(t0 + step * 41, 4, Breadcrumb::NewEvents, None);
    assert_eq!(
        s.take_ended().map(|e| (e.root, e.by)),
        Some((Breadcrumb::AboutToWait, EndedBy::Beat))
    );
}

#[test]
fn a_work_root_stall_ends_by_its_beat_never_as_a_quiet_spin() {
    let t0 = Instant::now();
    let step = Duration::from_millis(250);
    let quiet = |tick: u32| Some(Duration::from_millis(tick.into()));
    let mut s = Sampler::with_threshold(t0, 3, STALL_THRESHOLD);
    let mut hit = None;
    for tick in 1..=3u32 {
        hit = hit.or(s.poll_with(t0 + step * tick, 3, Breadcrumb::UserEvent, quiet(tick)));
    }
    assert!(
        hit.is_some_and(|h| h.root == Breadcrumb::UserEvent && !h.busy),
        "a park on a lock inside `UserEvent`, reported: {hit:?}"
    );
    assert_eq!(
        s.poll_with(t0 + step * 4, 3, Breadcrumb::AboutToWait, quiet(4)),
        None
    );
    assert_eq!(s.take_ended(), None, "not a spin that went quiet");
    assert_eq!(
        s.poll_with(t0 + step * 5, 4, Breadcrumb::AboutToWait, quiet(5)),
        None
    );
    assert_eq!(
        s.take_ended(),
        Some(Ended {
            frozen: step * 5,
            root: Breadcrumb::UserEvent,
            by: EndedBy::Beat,
        })
    );
    assert_eq!(s.take_ended(), None);
}

const REFUSAL_T0: u64 = 40_000_000_000;

const REFUSAL_TICK_NS: u64 = 2_500_000_000;

fn secs(n: u64) -> u64 {
    n * 1_000_000_000
}

#[test]
fn a_work_root_with_no_beat_for_the_bar_is_refused_and_says_where() {
    let beat = REFUSAL_T0;
    assert_eq!(
        main_stall(beat + secs(4), beat, Breadcrumb::NewEvents, true, None),
        None,
        "under the bar is a slow turn, not a stall"
    );
    let stall = main_stall(beat + secs(12_138), beat, Breadcrumb::NewEvents, true, None)
        .expect("a work root frozen past the bar is stalled");
    assert_eq!(stall.stalled, Duration::from_secs(12_138));
    let inside = main_stall(beat + secs(7), beat, Breadcrumb::UserEvent, false, None)
        .expect("a handler still on the stack is stalled too");
    assert_eq!(inside.stalled, Duration::from_secs(7));
}

#[test]
fn the_idle_park_is_refused_only_once_a_hop_has_waited_the_bar() {
    let beat = REFUSAL_T0;
    let idle = beat + secs(600);
    let park = Breadcrumb::AboutToWait;
    assert_eq!(main_stall(idle, beat, park, true, None), None, "idle");
    assert_eq!(
        main_stall(idle, beat, park, true, Some(idle)),
        None,
        "a hop just posted"
    );
    assert_eq!(
        main_stall(idle + secs(4), beat, park, true, Some(idle)),
        None
    );
    let stall = main_stall(idle + secs(5), beat, park, true, Some(idle))
        .expect("a hop that waited the bar on an unmoved heartbeat");
    assert_eq!(
        stall.stalled,
        Duration::from_secs(5),
        "counted from the hop, not from the idle before it"
    );
    assert_eq!(
        main_stall(idle + secs(6), idle + secs(2), park, true, Some(idle)),
        None
    );
    assert!(main_stall(idle, beat, park, true, Some(1)).is_some());
}

#[test]
fn a_designed_freeze_or_a_thread_that_never_beat_is_never_refused() {
    let beat = REFUSAL_T0;
    let late = beat + secs(3_600);
    for root in [
        Breadcrumb::Modal,
        Breadcrumb::UpdateHandoff,
        Breadcrumb::Startup,
        Breadcrumb::Launching,
    ] {
        assert_eq!(
            main_stall(late, beat, root, false, Some(beat)),
            None,
            "{root:?}"
        );
    }
    assert_eq!(
        main_stall(late, 0, Breadcrumb::UserEvent, false, Some(beat)),
        None,
        "no beat yet is no evidence"
    );
}

struct RefusalLockstep<'a> {
    /// This lockstep's own main-thread watch, which the writer stamps.
    watch: &'a MainWatch,
    ledger: &'a TurnLedger,
    phase: &'a AtomicU8,
    /// `'static`, as the process's own is, so a real `Wake::Hop` can
    /// carry its mark.
    hops: &'static crate::control::control_media::Hops,
    /// The worker's side of the posted hop, alive while it waits.
    hop: Option<crate::control::control_media::HopGuard<'static>>,
    /// When `hop` was posted.
    posted: u64,
    now: u64,
    root: Breadcrumb,
    returned: bool,
    /// The census defect: the reader takes the turn census's open stamp,
    /// which `reset` zeroes, for the heartbeat's time.
    census: bool,
    /// The heartbeat-only defect: a waiting hop is judged as if it had
    /// waited forever, so only the heartbeat's age counts.
    beat_only: bool,
    /// The count-until-reply defect: a hop counts from its post for as
    /// long as its worker waits, taken by the main thread or not.
    until_reply: bool,
}

impl RefusalLockstep<'_> {
    fn enter_root(&mut self, root: Breadcrumb, returned: bool) {
        beat_into(self.watch, self.ledger, self.phase, root, self.now);
        self.root = root;
        self.returned = returned;
    }

    fn take(&self) {
        let hop = self.hop.as_ref().expect("a posted hop to take");
        let request = crate::control::control_media::take_hop(crate::Wake::Hop {
            mark: hop.mark(),
            wake: Box::new(crate::Wake::WarmupResult),
        });
        assert!(matches!(request, crate::Wake::WarmupResult), "{request:?}");
    }

    fn stamp(&self) -> u64 {
        if self.census {
            self.ledger.open_ns.load(Ordering::Relaxed)
        } else {
            self.ledger.last_beat_ns.load(Ordering::Relaxed)
        }
    }

    fn refuses(&self) -> bool {
        let since = if self.until_reply {
            self.hop.as_ref().map(|_| self.posted)
        } else {
            self.hops.since_ns()
        };
        let since = since.map(|since| if self.beat_only { 1 } else { since });
        main_stall(self.now, self.stamp(), self.root, self.returned, since).is_some()
    }

    fn step(&mut self, action: &str) -> Option<bool> {
        match action {
            "BeatIdle" => self.enter_root(Breadcrumb::AboutToWait, true),
            "BeatWork" => self.enter_root(Breadcrumb::NewEvents, true),
            "BeatPark" => self.enter_root(Breadcrumb::Modal, false),
            "Tick" => self.now += REFUSAL_TICK_NS,
            "Post" => {
                let hops: &'static crate::control::control_media::Hops = self.hops;
                self.hop = Some(hops.post(self.now));
                self.posted = self.now;
            }
            "Answer" => {
                self.enter_root(Breadcrumb::UserEvent, false);
                self.take();
                self.hop = None;
            }
            "Take" => {
                self.enter_root(Breadcrumb::UserEvent, false);
                self.take();
            }
            "Reply" => {
                self.enter_root(Breadcrumb::UserEvent, false);
                self.hop = None;
            }
            "GiveUp" => self.hop = None,
            "Reset" => self.ledger.reset(),
            "Probe" => return Some(self.refuses()),
            other => panic!("no real step for model action `{other}`"),
        }
        None
    }
}

fn refusal_lockstep(
    model: &aterm_spec::derive::Model,
    mutation: Option<&'static str>,
    schedule: &[&'static str],
) -> Vec<(i64, i64, bool)> {
    let ledger = TurnLedger::new();
    let phase = AtomicU8::new(Phase::None as u8);
    let watch = MainWatch::new();
    let hops: &'static crate::control::control_media::Hops =
        Box::leak(Box::new(crate::control::control_media::Hops::new()));
    let mut real = RefusalLockstep {
        watch: &watch,
        ledger: &ledger,
        phase: &phase,
        hops,
        hop: None,
        posted: 0,
        now: REFUSAL_T0,
        root: Breadcrumb::Startup,
        returned: false,
        census: mutation == Some("MutateCensusStamp"),
        beat_only: mutation == Some("MutateHeartbeatOnly"),
        until_reply: mutation == Some("MutateCountsUntilReply"),
    };
    let mut state = model.init_state();
    if let Some(mutation) = mutation {
        assert!(model.fire(mutation, &mut state), "{mutation}");
    }
    let mut probes = Vec::new();
    for &action in schedule {
        assert!(
            model.fire(action, &mut state),
            "{action} is not admitted at {state:?} ({schedule:?})"
        );
        let verdict = real.step(action);
        assert_eq!(
            state["waiting"],
            i64::from(real.hop.is_some()),
            "{action} in {schedule:?}: the worker's live guard projects onto `waiting`"
        );
        assert_eq!(
            state["hop"],
            i64::from(hops.since_ns().is_some()),
            "{action} in {schedule:?}: the hop count projects onto `hop`"
        );
        assert_eq!(
            state["stamp"],
            i64::from(real.stamp() != 0),
            "{action} in {schedule:?}: the reader's stamp projects onto `stamp`"
        );
        if let Some(refuses) = verdict {
            assert_eq!(
                state["refused"],
                i64::from(refuses),
                "{action} in {schedule:?}: the real reader and the model disagree"
            );
            probes.push((state["refused"], state["stalled"], refuses));
        }
    }
    probes
}

#[test]
fn main_stall_conforms_to_the_stall_refusal_model() {
    let model = aterm_models_gui::main_thread_stall_refusal_model();
    assert!(model.consts.contains(&("Bar", 2)), "two ticks are the bar");
    assert_eq!(Duration::from_nanos(2 * REFUSAL_TICK_NS), MAIN_STALL_BAR);

    let _serial = beat_serial();
    let schedules: &[&[&'static str]] = &[
        &[
            "BeatWork", "Probe", "Tick", "Probe", "Tick", "Probe", "Post", "Tick", "Probe",
        ],
        &[
            "BeatIdle", "Tick", "Tick", "Tick", "Post", "Probe", "Tick", "Probe", "Tick", "Probe",
        ],
        &[
            "BeatIdle", "Tick", "Tick", "Post", "Tick", "Answer", "Probe", "BeatIdle", "Tick",
            "Tick", "Probe",
        ],
        &[
            "BeatWork", "BeatPark", "Post", "Tick", "Tick", "Tick", "Probe",
        ],
        &[
            "BeatWork", "Tick", "Reset", "Tick", "Probe", "Reset", "Probe",
        ],
        &[
            "BeatIdle", "Post", "Tick", "Tick", "Probe", "GiveUp", "Probe",
        ],
        &[
            "BeatIdle", "Post", "Tick", "BeatIdle", "Tick", "Probe", "Tick", "Probe",
        ],
        &["Tick", "Tick", "Post", "Tick", "Tick", "Probe"],
        &[
            "BeatIdle", "Post", "Take", "BeatIdle", "Tick", "Tick", "Probe", "Tick", "Probe",
            "Reply", "Probe",
        ],
        &["BeatIdle", "Post", "Take", "Tick", "Tick", "Probe"],
    ];
    let mut refused = 0;
    for schedule in schedules {
        for (model_refused, stalled, _) in refusal_lockstep(&model, None, schedule) {
            assert_eq!(model_refused, stalled, "{schedule:?}");
            refused += model_refused;
        }
    }
    assert!(refused > 0, "the schedules reach a refusal");

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let reset_under_stall: &[&'static str] = &["BeatWork", "Tick", "Reset", "Tick", "Probe"];
    assert_eq!(
        refusal_lockstep(&model, None, reset_under_stall),
        [(1, 1, true)]
    );
    assert_eq!(
        refusal_lockstep(&buggy, Some("MutateCensusStamp"), reset_under_stall),
        [(0, 1, false)],
        "read through the census stamp, a reset lets a stalled thread's verbs through"
    );
    let fresh_hop: &[&'static str] = &["BeatIdle", "Tick", "Tick", "Tick", "Post", "Probe"];
    assert_eq!(refusal_lockstep(&model, None, fresh_hop), [(0, 0, false)]);
    assert_eq!(
        refusal_lockstep(&buggy, Some("MutateHeartbeatOnly"), fresh_hop),
        [(1, 0, true)],
        "judged on the heartbeat alone, a fresh hop after idleness is refused"
    );
    let queued_reply: &[&'static str] = &[
        "BeatIdle", "Post", "Take", "BeatIdle", "Tick", "Tick", "Probe",
    ];
    assert_eq!(
        refusal_lockstep(&model, None, queued_reply),
        [(0, 0, false)]
    );
    assert_eq!(
        refusal_lockstep(&buggy, Some("MutateCountsUntilReply"), queued_reply),
        [(1, 0, true)],
        "counted until its reply, a queued settings write makes a healthy idle thread \
         read as stuck"
    );
}

#[cfg(any(unix, windows))]
/// A private watch, ledger and phase cell, so these loops share no cell with the beat tests.
fn private_watch() -> (&'static MainWatch, &'static TurnLedger, &'static AtomicU8) {
    (
        Box::leak(Box::new(MainWatch::new())),
        Box::leak(Box::new(TurnLedger::new())),
        Box::leak(Box::new(AtomicU8::new(Phase::None as u8))),
    )
}

#[cfg(any(unix, windows))]
fn beat_private(
    watch: (&'static MainWatch, &'static TurnLedger, &'static AtomicU8),
    bc: Breadcrumb,
) {
    let (main, turns, phase) = watch;
    beat_into(main, turns, phase, bc, crate::metrics::now_ns());
}

#[cfg(any(unix, windows))]
/// A test pace: a 100 ms bar sampled every 50 ms (the launch's too), no CPU reading and no
/// stack; the launch's stage reads `TEST_STAGE` and a launch leaves by returning.
const TEST_JUDGE: Judge = Judge {
    threshold: Duration::from_millis(100),
    sample: Duration::from_millis(50),
    abort: false,
    cpu: || None,
    stack: || None,
    launch_bar: Duration::from_millis(100),
    stage: || Some(TEST_STAGE),
    leave: |_| {},
};

#[cfg(any(unix, windows))]
const TEST_STAGE: &str = "inside a test's launch";

#[cfg(any(unix, windows))]
/// A loop running on `main` with `judge` and a wall watch told by hand (or none): what it saw,
/// each `on_wall_moved`, and the watch to tell. Dropping the receiver ends the loop at its next
/// wait.
struct Loop {
    seen: std::sync::mpsc::Receiver<Seen>,
    moved: std::sync::mpsc::Receiver<()>,
    wall: Option<&'static aterm_uds::wall_watch::WallWatch>,
}

#[cfg(any(unix, windows))]
impl Loop {
    fn start(main: &'static MainWatch, judge: Option<Judge>, wall: bool) -> Self {
        Self::start_with(main, judge, wall, |_| {})
    }

    /// [`Loop::start`], with `step` run on the loop's own thread at each event it sees, before
    /// the event is sent: what `step` does lands at that point of the loop, with no timing.
    fn start_with(
        main: &'static MainWatch,
        judge: Option<Judge>,
        wall: bool,
        step: impl Fn(Seen) + Send + 'static,
    ) -> Self {
        let wall: Option<&'static aterm_uds::wall_watch::WallWatch> = wall.then(|| {
            &*Box::leak(Box::new(
                aterm_uds::wall_watch::WallWatch::by_hand().expect("doorbell"),
            ))
        });
        let (seen_tx, seen) = std::sync::mpsc::channel();
        let (moved_tx, moved) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            run(
                main,
                judge,
                wall,
                &move || {
                    let _ = moved_tx.send(());
                },
                &move |event| {
                    step(event);
                    seen_tx.send(event).is_ok()
                },
            );
        });
        Self { seen, moved, wall }
    }

    /// The next event the loop saw (a one-minute hang detector).
    fn next(&self) -> Seen {
        self.seen
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the watchdog loop goes on")
    }

    /// The loop settles at rest: exactly [`QUIET_SAMPLES`] timed samples that find no beat, then
    /// a wait with no deadline.
    fn settles(&self) {
        let quiet: Vec<Seen> = (0..QUIET_SAMPLES)
            .flat_map(|_| [Seen::Waits { rest: false }, Seen::Sampled])
            .collect();
        assert_eq!(
            self.until(Seen::Waits { rest: true }),
            quiet,
            "rests after the quiet samples"
        );
    }

    /// Events until `want`, which is returned with what came before it: one one-minute hang
    /// detector for the whole wait, not one per event, so a loop that samples on and on without
    /// `want` fails instead of hanging.
    fn until(&self, want: Seen) -> Vec<Seen> {
        let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let mut before = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let event = self.seen.recv_timeout(left).unwrap_or_else(|error| {
                let last = &before[before.len().saturating_sub(8)..];
                panic!(
                    "the watchdog loop shows {want:?} within a minute ({error}), after {} other \
                     events, the last {last:?}",
                    before.len()
                )
            });
            if event == want {
                return before;
            }
            before.push(event);
        }
    }
}

#[test]
fn the_sampler_rests_only_where_nothing_could_be_reported_without_a_beat() {
    for bc in Breadcrumb::ALL {
        let work = !bc.is_park_point();
        assert_eq!(rests_at(bc, false), !work, "{bc:?} with no CPU reading");
        let judged = bc == Breadcrumb::ShutdownWait || work;
        assert_eq!(rests_at(bc, true), !judged, "{bc:?} with a CPU reading");
    }
}

#[cfg(any(unix, windows))]
#[test]
fn a_beat_takes_the_rest_claim_and_rings_once() {
    let watch = private_watch();
    let main = watch.0;
    let bell = main.bell().expect("doorbell");
    beat_private(watch, Breadcrumb::WindowEvent);
    assert!(!main.claim_rest(true), "no rest inside a turn");
    assert!(
        !main.resting.load(Ordering::SeqCst),
        "the claim is withdrawn"
    );
    beat_private(watch, Breadcrumb::AboutToWait);
    assert!(!bell.clear(), "a beat with no rest claimed rings nothing");
    assert!(main.claim_rest(true));
    beat_private(watch, Breadcrumb::NewEvents);
    assert!(
        !main.resting.load(Ordering::SeqCst),
        "the beat took the claim"
    );
    assert!(bell.clear(), "and rang");
    beat_private(watch, Breadcrumb::AboutToWait);
    assert!(!bell.clear(), "once: the next beat finds no claim");
}

#[cfg(any(unix, windows))]
#[test]
fn at_rest_the_sampler_waits_with_no_deadline_and_samples_once_per_ring() {
    let watch = private_watch();
    beat_private(watch, Breadcrumb::AboutToWait);
    let looped = Loop::start(watch.0, Some(TEST_JUDGE), true);
    looped.settles();
    beat_private(watch, Breadcrumb::UpdateHandoff);
    assert_eq!(looped.next(), Seen::Sampled, "the beat rang one sample");
    // And the sampler rests again over a designed park once it is quiet again.
    looped.settles();
}

#[cfg(any(unix, windows))]
#[test]
fn with_no_wall_watch_the_sampler_never_rests_and_reads_the_drift() {
    let watch = private_watch();
    beat_private(watch, Breadcrumb::AboutToWait);
    let looped = Loop::start(watch.0, Some(TEST_JUDGE), false);
    for _ in 0..3 {
        assert_eq!(looped.next(), Seen::Waits { rest: false });
        assert_eq!(looped.next(), Seen::Sampled);
    }
}

#[cfg(any(unix, windows))]
#[test]
fn a_main_thread_wedged_inside_a_turn_is_caught_from_rest() {
    let watch = private_watch();
    beat_private(watch, Breadcrumb::AboutToWait);
    let looped = Loop::start(watch.0, Some(TEST_JUDGE), true);
    looped.settles();
    // The main thread leaves its wait and wedges in the turn: it never beats again.
    beat_private(watch, Breadcrumb::WindowEvent);
    let before = looped.until(Seen::Hit(Breadcrumb::WindowEvent));
    assert!(
        !before.contains(&Seen::Waits { rest: true }),
        "armed throughout the wedged turn: {before:?}"
    );
    assert_eq!(
        looped.next(),
        Seen::Waits { rest: false },
        "and still armed"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn a_told_wake_or_step_drives_on_wall_moved_while_the_sampler_rests() {
    let watch = private_watch();
    beat_private(watch, Breadcrumb::AboutToWait);
    let looped = Loop::start(watch.0, Some(TEST_JUDGE), true);
    let wall = looped.wall.expect("a wall watch");
    looped.settles();
    for moved in [
        aterm_uds::wall_watch::Moved {
            woke: true,
            slept: Some(Duration::from_secs(3_600)),
            ..Default::default()
        },
        aterm_uds::wall_watch::Moved {
            stepped: true,
            ..Default::default()
        },
    ] {
        wall.tell(moved);
        assert_eq!(looped.next(), Seen::Sampled);
        assert_eq!(looped.next(), Seen::WallMoved, "{moved:?}");
        looped
            .moved
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("on_wall_moved ran");
        assert_eq!(looped.next(), Seen::Waits { rest: true });
    }
}

#[cfg(any(unix, windows))]
#[test]
fn with_the_judging_switched_off_a_wall_move_still_drives_on_wall_moved() {
    let watch = private_watch();
    let looped = Loop::start(watch.0, None, true);
    assert_eq!(looped.next(), Seen::Waits { rest: true });
    looped
        .wall
        .expect("a wall watch")
        .tell(aterm_uds::wall_watch::Moved {
            stepped: true,
            ..Default::default()
        });
    assert_eq!(looped.next(), Seen::WallMoved);
    looped
        .moved
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("on_wall_moved ran");
    assert_eq!(looped.next(), Seen::Waits { rest: true });
}

/// MEASUREMENT, not a check: the whole test process's interrupt wakeups over 65 s
/// (`ATERM_MEASURE_REST_SECS`) with the process's own watchdog started over a main thread
/// parked in `about_to_wait`'s wait (this thread registered as the main thread, so its CPU time
/// reads as the main thread's, as in a window). Run alone:
/// `targo --unverified test -p aterm-gui --lib watchdog_rest_wakeups -- --ignored --nocapture`.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "a 65 s measurement, run by hand"]
fn watchdog_rest_wakeups() {
    fn wakeups() -> (u64, u64) {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: a V4 buffer of the right size for this process's own usage.
        let rc = unsafe {
            libc::proc_pid_rusage(
                i32::try_from(std::process::id()).expect("a pid"),
                libc::RUSAGE_INFO_V4,
                info.as_mut_ptr().cast(),
            )
        };
        assert_eq!(rc, 0);
        // SAFETY: proc_pid_rusage filled it.
        let info = unsafe { info.assume_init() };
        (info.ri_interrupt_wkups, info.ri_pkg_idle_wkups)
    }
    let span = Duration::from_secs(
        std::env::var("ATERM_MEASURE_REST_SECS")
            .ok()
            .and_then(|secs| secs.parse().ok())
            .unwrap_or(65),
    );
    beat_on(&MAIN, Breadcrumb::AboutToWait);
    start(|| {});
    std::thread::sleep(Duration::from_secs(2));
    let (wake0, idle0) = wakeups();
    std::thread::sleep(span);
    let (wake1, idle1) = wakeups();
    eprintln!(
        "watchdog at rest: span_s={} interrupt_wkups={} pkg_idle_wkups={}",
        span.as_secs(),
        wake1 - wake0,
        idle1 - idle0
    );
}

/// A MAIN THREAD THAT WAKES BETWEEN SAMPLES IS NEVER RUNG: while every sample finds a beat since
/// the last, the sampler waits on its interval alone (no rest claimed, so no beat rings, and no
/// ring adds a wake to the interval's); once [`QUIET_SAMPLES`] samples in a row find none, it
/// rests. The beats are made inside the loop's own waits, so no timing decides the sequence.
/// NEGATIVE CONTROL: `run` resting without the quiet count (`quiet >= QUIET_SAMPLES` dropped)
/// rests at its first wait and fails "never rests while the main thread beats".
#[cfg(any(unix, windows))]
#[test]
fn a_main_thread_waking_between_samples_is_sampled_on_the_interval_and_never_rung() {
    const BEATING: usize = 12;
    let watch = private_watch();
    beat_private(watch, Breadcrumb::AboutToWait);
    let bell = watch.0.bell().expect("doorbell");
    let (seen_tx, seen) = std::sync::mpsc::channel();
    let waits = std::sync::atomic::AtomicUsize::new(0);
    std::thread::spawn(move || {
        run(
            watch.0,
            Some(TEST_JUDGE),
            Some(&*Box::leak(Box::new(
                aterm_uds::wall_watch::WallWatch::by_hand().expect("doorbell"),
            ))),
            &|| {},
            &move |event| {
                // Each of the first waits, a short turn: the main thread wakes, works and parks.
                if matches!(event, Seen::Waits { .. })
                    && waits.fetch_add(1, Ordering::SeqCst) < BEATING
                {
                    beat_private(watch, Breadcrumb::NewEvents);
                    beat_private(watch, Breadcrumb::AboutToWait);
                }
                seen_tx.send(event).is_ok()
            },
        );
    });
    let next = || {
        seen.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the watchdog loop goes on")
    };
    for _ in 0..BEATING {
        assert_eq!(
            next(),
            Seen::Waits { rest: false },
            "never rests while the main thread beats"
        );
        assert_eq!(next(), Seen::Sampled);
    }
    assert!(!bell.clear(), "no beat rang");
    // The beats stop: the quiet samples, then the rest.
    let mut events = Vec::new();
    loop {
        let event = next();
        if event == (Seen::Waits { rest: true }) {
            break;
        }
        events.push(event);
    }
    let quiet: Vec<Seen> = (0..QUIET_SAMPLES)
        .flat_map(|_| [Seen::Waits { rest: false }, Seen::Sampled])
        .collect();
    assert_eq!(events, quiet, "rests after the quiet samples");
}

/// AN EXIT ASKED INSIDE `about_to_wait` PARKS AT `ShutdownWait`: winit beats no `NewEvents` once
/// it is exiting, so the handler's return must not leave a park the sampler rests at.
/// NEGATIVE CONTROL: `RootGuard::loop_ending` doing nothing leaves `AboutToWait` and fails "the
/// ending loop parks at ShutdownWait".
#[cfg(any(unix, windows))]
#[test]
fn an_exit_inside_about_to_wait_parks_where_the_sampler_judges() {
    let watch = private_watch();
    let main = watch.0;
    drop(enter_about_to_wait_in(main));
    assert_eq!(main.current(), Breadcrumb::AboutToWait);
    assert!(main.claim_rest(true), "an ordinary return rests");
    main.resting.store(false, Ordering::SeqCst);
    let mut root = enter_about_to_wait_in(main);
    root.loop_ending();
    drop(root);
    assert_eq!(
        main.current(),
        Breadcrumb::ShutdownWait,
        "the ending loop parks at ShutdownWait"
    );
    assert!(
        !main.claim_rest(true),
        "where a spin is judged, never rested"
    );
}

/// THE LAUNCH IS JUDGED AT ITS OWN BAR: a main thread that has not left the launch two seconds
/// after it began is reported, under the release stall threshold, once per repeat interval, and a
/// beat (the loop's first root) ends it. NEGATIVE CONTROL, in the test: the identical frozen
/// schedule under `Startup`, the breadcrumb a launch had before the launch watch, reports nothing,
/// even ten minutes on, which is how every 0.187.0 successor on m16 hung unseen. Reverting the fix
/// (`Launching` a park point, or judged at the stall threshold) fails "reported at its bar".
#[test]
fn a_launch_frozen_past_its_bar_is_reported_and_the_same_schedule_at_startup_is_not() {
    let schedule = [
        (Duration::from_millis(1_900), false),
        (LAUNCH_STALL_BAR, true),
        (Duration::from_secs(3), false),
        (Duration::from_secs(600), true),
    ];
    let t0 = Instant::now();
    let mut launching = Sampler::with_threshold(t0, 1, RELEASE_STALL_THRESHOLD);
    let mut startup = Sampler::with_threshold(t0, 1, RELEASE_STALL_THRESHOLD);
    assert!(LAUNCH_STALL_BAR < RELEASE_STALL_THRESHOLD, "PRECONDITION");
    for (at, reported) in schedule {
        assert_eq!(
            launching.poll(t0 + at, 1, Breadcrumb::Launching),
            reported.then_some(Breadcrumb::Launching),
            "reported at its bar, once per repeat interval: at {at:?}"
        );
        assert_eq!(
            startup.poll(t0 + at, 1, Breadcrumb::Startup),
            None,
            "the same frozen schedule at Startup is never judged: at {at:?}"
        );
    }
    assert_eq!(
        launching.poll(
            t0 + Duration::from_secs(601),
            2,
            Breadcrumb::AboutToWaitWork
        ),
        None,
        "the loop's first root ends it"
    );
    assert_eq!(
        launching.take_ended().map(|ended| (ended.root, ended.by)),
        Some((Breadcrumb::Launching, EndedBy::Beat)),
        "and the end is said"
    );
}

/// A LAUNCH WITH AN EXIT BOUND LEAVES AT IT, ONCE; ONE WITHOUT NEVER DOES: the exit is decided
/// only at `Launching`, only with an exit bound (a handoff successor's whose parent reads no
/// progress), only once the launch has gone the bound without a beat, and once per launch.
/// NEGATIVE CONTROLS, in the test: the same schedule with no bound (a launch that only reports)
/// and at `Startup` with a bound never leave. Reverting the fix (`leave_due` answering `None`)
/// fails "leaves at its bound".
#[test]
fn a_launch_unfinished_at_its_exit_bound_leaves_once_and_one_without_never() {
    let bound = Some(LAUNCH_EXIT_BOUND);
    let before = LAUNCH_EXIT_BOUND - Duration::from_secs(1);
    let t0 = Instant::now();
    let mut s = Sampler::with_threshold(t0, 1, RELEASE_STALL_THRESHOLD);
    let at = |s: &mut Sampler, after: Duration, beat: u64, bc: Breadcrumb, exit| {
        let now = t0 + after;
        let _ = s.poll(now, beat, bc);
        s.leave_due(now, beat, bc, exit)
    };
    assert_eq!(at(&mut s, before, 1, Breadcrumb::Launching, bound), None);
    assert_eq!(
        at(&mut s, LAUNCH_EXIT_BOUND, 1, Breadcrumb::Launching, bound),
        Some(LAUNCH_EXIT_BOUND),
        "leaves at its bound"
    );
    assert_eq!(
        at(
            &mut s,
            LAUNCH_EXIT_BOUND * 2,
            1,
            Breadcrumb::Launching,
            bound
        ),
        None,
        "once"
    );
    let resumed = LAUNCH_EXIT_BOUND * 3;
    assert_eq!(at(&mut s, resumed, 2, Breadcrumb::Launching, bound), None);
    assert_eq!(
        at(
            &mut s,
            resumed + LAUNCH_EXIT_BOUND,
            2,
            Breadcrumb::Launching,
            bound
        ),
        Some(LAUNCH_EXIT_BOUND),
        "a beat starts the next launch's count"
    );

    for (bc, exit, case) in [
        (
            Breadcrumb::Launching,
            None,
            "a launch without an exit bound only reports",
        ),
        (Breadcrumb::Startup, bound, "Startup is not the launch"),
    ] {
        let mut s = Sampler::with_threshold(t0, 1, RELEASE_STALL_THRESHOLD);
        for after in [LAUNCH_EXIT_BOUND, Duration::from_secs(3_600)] {
            assert_eq!(at(&mut s, after, 1, bc, exit), None, "{case}");
        }
    }
}

/// THE LAUNCH'S POLICY AND ITS END: `launching` stamps `Launching` with its exit bound, and
/// `launch_finished` goes back to `Startup` and forgets the bound, so the synchronous launch after
/// it (the dial, the claim) is unjudged as before and nothing later leaves by it.
#[test]
fn a_finished_launch_goes_back_to_startup_and_forgets_its_exit() {
    let watch = MainWatch::new();
    let _serial = beat_serial();
    let exit = OnLaunchStall::Exit {
        after: LAUNCH_EXIT_BOUND,
        code: 72,
    };
    launching_on(&watch, exit);
    assert_eq!(watch.current(), Breadcrumb::Launching);
    assert_eq!(watch.on_launch_stall(), exit);
    let beat = watch.heartbeat.load(Ordering::Relaxed);
    launch_finished_on(&watch);
    assert_eq!(watch.current(), Breadcrumb::Startup);
    assert_eq!(watch.on_launch_stall(), OnLaunchStall::Log);
    assert_eq!(
        watch.heartbeat.load(Ordering::Relaxed),
        beat + 1,
        "the end is a beat"
    );
    launching_on(&watch, OnLaunchStall::Log);
    assert_eq!(
        (watch.current(), watch.on_launch_stall()),
        (Breadcrumb::Launching, OnLaunchStall::Log)
    );
}

/// THE REPORT NAMES WHERE AND WHAT NEXT: the AppKit stage the launch reached, and either that it
/// only reports or the code and bound it leaves at; the exit line names the code.
#[test]
fn the_launch_report_names_the_stage_and_what_comes_next() {
    let stage = "inside AppKit's own launch work";
    let frozen = Duration::from_secs(2);
    let logged = launch_stall_message(1, frozen, Some(stage), OnLaunchStall::Log, false);
    assert!(logged.starts_with("LAUNCH STALL:"), "{logged}");
    assert!(logged.contains(stage), "{logged}");
    assert!(logged.contains("only reports"), "{logged}");
    let exit = OnLaunchStall::Exit {
        after: LAUNCH_EXIT_BOUND,
        code: 72,
    };
    let leaving = launch_stall_message(1, frozen, Some(stage), exit, false);
    assert!(
        leaving.contains("exits with 72") && leaving.contains("20s"),
        "{leaving}"
    );
    let again = launch_stall_message(2, frozen * 30, None, exit, true);
    assert!(
        again.starts_with("LAUNCH STALL CONTINUES:") && again.contains("ON-CPU"),
        "{again}"
    );
    let left = launch_exit_message(LAUNCH_EXIT_BOUND, Some(stage), 72);
    assert!(
        left.starts_with("aterm could not start:")
            && left.contains(stage)
            && left.contains("exits with 72"),
        "{left}"
    );
}

/// The code [`LEAVING_JUDGE`]'s `leave` was last called with (0: not called).
#[cfg(any(unix, windows))]
static LEFT_WITH: AtomicI32 = AtomicI32::new(0);

/// [`TEST_JUDGE`] whose `leave` records its code in [`LEFT_WITH`] and returns.
#[cfg(any(unix, windows))]
const LEAVING_JUDGE: Judge = Judge {
    leave: |code| LEFT_WITH.store(code, Ordering::SeqCst),
    ..TEST_JUDGE
};

/// THE SAMPLER CATCHES A STALLED LAUNCH FROM REST AND ONE WITH AN EXIT BOUND LEAVES: started over
/// `Startup` it rests (nothing there is judged); the launch's beat rings it, it samples at the
/// launch's pace, reports `Launching` at the bar and, with an exit bound, leaves through the
/// judge's exit with its code. NEGATIVE CONTROLS, in the test: a launch that only reports (an
/// ordinary one, or a successor's whose parent reads progress) is reported and never leaves;
/// reverting the fix (no `leave(code)` in the loop, or `leave_due` answering `None`) fails "the
/// exit itself ran with the code" or "shows Left(72) within a minute", never hangs.
#[cfg(any(unix, windows))]
#[test]
fn a_stalled_launch_is_caught_from_rest_and_only_one_with_an_exit_bound_leaves() {
    for (on_stall, leaves) in [
        (
            OnLaunchStall::Exit {
                after: Duration::from_millis(300),
                code: 72,
            },
            true,
        ),
        (OnLaunchStall::Log, false),
    ] {
        LEFT_WITH.store(0, Ordering::SeqCst);
        let watch = private_watch();
        let looped = Loop::start(watch.0, Some(LEAVING_JUDGE), true);
        looped.settles();
        watch.0.set_launch_policy(on_stall);
        beat_private(watch, Breadcrumb::Launching);
        let before = looped.until(Seen::Hit(Breadcrumb::Launching));
        assert!(
            !before.contains(&Seen::Waits { rest: true }),
            "{on_stall:?}: armed throughout the stalled launch: {before:?}"
        );
        if leaves {
            let before = looped.until(Seen::Left(72));
            assert!(!before.contains(&Seen::Waits { rest: true }), "{before:?}");
            assert_eq!(
                LEFT_WITH.load(Ordering::SeqCst),
                72,
                "the exit itself ran with the code"
            );
        } else {
            // 16 samples at the 50 ms pace: well past where the first case's 300 ms bound left.
            let mut sampled = 0_usize;
            while sampled < 16 {
                let event = looped.next();
                assert!(
                    !matches!(event, Seen::Left(_)),
                    "a launch that only reports never leaves: {event:?}"
                );
                sampled += usize::from(event == Seen::Sampled);
            }
            assert_eq!(LEFT_WITH.load(Ordering::SeqCst), 0, "and never exits");
        }
    }
}

/// THE LAUNCH IS SAMPLED AT HALF ITS BAR: at `Launching` the pace is the sample interval or half
/// the launch's bar, whichever is shorter (release's 2.5 s sample against the 2 s bar: 1 s, so,
/// counted from the sample the launch's own beat rings, the report comes at 2 s, not 2.5 s);
/// every other root keeps the sample interval. NEGATIVE CONTROL: `pace` answering the sample
/// interval at `Launching` fails "half the bar".
#[test]
fn the_launch_is_sampled_at_half_its_bar_and_every_other_root_at_the_sample() {
    let release = Judge {
        threshold: RELEASE_STALL_THRESHOLD,
        sample: RELEASE_STALL_THRESHOLD / 2,
        abort: false,
        cpu: || None,
        stack: || None,
        launch_bar: LAUNCH_STALL_BAR,
        stage: || None,
        leave: |_| {},
    };
    assert_eq!(
        release.pace(Breadcrumb::Launching),
        Duration::from_secs(1),
        "half the bar"
    );
    for bc in Breadcrumb::ALL {
        if bc != Breadcrumb::Launching {
            assert_eq!(release.pace(bc), release.sample, "{bc:?}");
        }
    }
    let fast = Judge {
        sample: Duration::from_millis(250),
        ..release
    };
    assert_eq!(
        fast.pace(Breadcrumb::Launching),
        fast.sample,
        "a sample already under half the bar stays"
    );
}

/// THE EXIT BOUND COUNTS FROM THE PROCESS'S BIRTH, AND A LATE LAUNCH STILL GETS THE FLOOR: a
/// launch that begins `age` into the process gets what is left of the bound, but never less than
/// the floor (four report bars, about a hundred of m16's healthy launches). 0.177.0's 30 s dial
/// clock starts at its launch request, before this birth, and the bound leaves the rest of its
/// 30 s, 10 s, for LaunchServices' start of this process (before the birth) and the parent's
/// notice of the exit (after it). Up to a 12 s start
/// (the bound less the floor) the exit lands 20 s after the birth, the same point of the outgoing
/// process's dial wait however long the start took; from 12 s to 22 s (the 30 s less the floor)
/// it moves later, up to 30 s after the birth, eating that margin, so it can come after 0.177.0's
/// dial bound; past 22 s it lands more than 30 s after the birth, always after that bound, where
/// 0.177.0 books TimedOut as before the launch watch. An unread birth counts from the launch's
/// start. NEGATIVE CONTROLS: answering the whole bound whatever the age (the launch-anchored
/// bound) fails "what is left since the birth"; the floor at the report bar (the 2 s floor before
/// this) fails "a launch that begins at the bound still gets the floor".
#[test]
fn the_launch_exit_bound_counts_from_the_process_birth_and_never_undercuts_the_floor() {
    let cases = [
        (
            Some(Duration::ZERO),
            LAUNCH_EXIT_BOUND,
            "a launch at the birth",
        ),
        (
            Some(Duration::from_secs(5)),
            LAUNCH_EXIT_BOUND - Duration::from_secs(5),
            "what is left since the birth",
        ),
        (
            Some(LAUNCH_EXIT_BOUND - LAUNCH_EXIT_FLOOR),
            LAUNCH_EXIT_FLOOR,
            "the floor's edge",
        ),
        (
            Some(LAUNCH_EXIT_BOUND),
            LAUNCH_EXIT_FLOOR,
            "a launch that begins at the bound still gets the floor",
        ),
        (
            Some(Duration::from_secs(25)),
            LAUNCH_EXIT_FLOOR,
            "never under the floor",
        ),
        (
            None,
            LAUNCH_EXIT_BOUND,
            "an unread birth counts from the launch",
        ),
    ];
    for (age, after, case) in cases {
        assert_eq!(launch_exit_after(age), after, "{case}: age {age:?}");
    }
    for ms in (0..=30_000).step_by(250) {
        let age = Duration::from_millis(ms);
        let after = launch_exit_after(Some(age));
        assert!(after >= LAUNCH_EXIT_FLOOR, "the floor: {age:?}");
        if age + LAUNCH_EXIT_FLOOR <= LAUNCH_EXIT_BOUND {
            assert_eq!(
                age + after,
                LAUNCH_EXIT_BOUND,
                "at the bound from the birth: {age:?}"
            );
        }
        if age + LAUNCH_EXIT_FLOOR <= OLDEST_DIAL_BOUND {
            assert!(
                age + after <= OLDEST_DIAL_BOUND,
                "within 30 s of the birth while the start took at most 22 s (0.177.0's own dial \
                 clock starts earlier, at its launch request): {age:?}"
            );
        }
    }
}

/// A SUCCESSOR LEAVES ITS STALLED LAUNCH TO A PARENT THAT READS PROGRESS: such a parent books a
/// hang before the dial itself (Stalled, an unexplained failure whose repeats converge), so the
/// launch only reports, however late it began; a parent that reads no progress (0.177.0) is sent
/// the exit, with its code, after what is left of the bound since the birth, never under the
/// floor. NEGATIVE CONTROL: `launch_stall_policy` answering the exit whatever the parent (the
/// policy before this fix) fails "a parent that reads progress judges the hang itself".
#[cfg(unix)]
#[test]
fn a_successor_leaves_its_stalled_launch_to_a_parent_that_reads_progress() {
    for age in [
        None,
        Some(Duration::ZERO),
        Some(Duration::from_secs(5)),
        Some(Duration::from_secs(25)),
    ] {
        assert_eq!(
            launch_stall_policy(true, age),
            OnLaunchStall::Log,
            "a parent that reads progress judges the hang itself: age {age:?}"
        );
        assert_eq!(
            launch_stall_policy(false, age),
            OnLaunchStall::Exit {
                after: launch_exit_after(age),
                code: crate::seamless::EXIT_SUCCESSOR_LAUNCH_STALLED,
            },
            "a parent that reads no progress is sent the exit: age {age:?}"
        );
    }
    assert_eq!(
        launch_stall_policy(false, Some(Duration::from_secs(25))),
        OnLaunchStall::Exit {
            after: LAUNCH_EXIT_FLOOR,
            code: 72,
        },
        "a start past the bound still leaves the launch the floor"
    );
}

/// THE LAUNCH'S BEAT WAKES A SAMPLER BETWEEN SAMPLES, WALL WATCH OR NOT: a sampler about to enter
/// a timed wait (an hour long here, so nothing but a ring ends it within the test; no sample has
/// found the main thread quiet yet, so it does not rest and a beat alone rings nothing) samples as
/// soon as `launching` stamps the launch, so the launch's clock starts at its beat and the
/// launch's pace (half its 100 ms bar) applies from there: the report follows. The beat lands
/// inside the loop's own step, after the pace is chosen and before the wait begins, so no timing
/// decides the sequence: the doorbell must already hold the ring when the wait looks, with no wall
/// watch too. NEGATIVE CONTROLS: `launching_on` without its ring leaves the sampler in its hour's
/// wait and fails "the launch's beat woke the sampler" at the one-minute hang detector, wall watch
/// or not; the code before this fix (the doorbell made up front only beside a wall watch, and a
/// ring that finds only a doorbell already made) fails the same with no wall watch.
#[cfg(any(unix, windows))]
#[test]
fn the_launchs_beat_wakes_a_sampler_between_samples() {
    let _serial = beat_serial();
    for wall in [true, false] {
        let watch = private_watch();
        let hourly = Judge {
            sample: Duration::from_secs(3_600),
            ..TEST_JUDGE
        };
        let first = std::sync::atomic::AtomicBool::new(true);
        let looped = Loop::start_with(watch.0, Some(hourly), wall, move |event| {
            if event == (Seen::Waits { rest: false }) && first.swap(false, Ordering::SeqCst) {
                launching_on(watch.0, OnLaunchStall::Log);
            }
        });
        assert_eq!(
            looped.next(),
            Seen::Waits { rest: false },
            "a timed wait at Startup (wall watch: {wall})"
        );
        let woke = looped
            .seen
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .unwrap_or_else(|error| {
                panic!("the launch's beat woke the sampler (wall watch: {wall}): {error}")
            });
        assert_eq!(woke, Seen::Sampled, "wall watch: {wall}");
        let before = looped.until(Seen::Hit(Breadcrumb::Launching));
        assert!(
            !before.contains(&Seen::Waits { rest: true }),
            "armed throughout the stalled launch (wall watch: {wall}): {before:?}"
        );
    }
}

/// WITH THE JUDGING SWITCHED OFF, A LAUNCH'S RING WAKES NOTHING: `launching` rings the doorbell
/// whatever the sampler, and the loop that judges nothing hears the wall alone. It makes no
/// doorbell, so none it never clears can wake it on and on, and after a launch's ring a wall move
/// is still the next thing it sees, then it waits again. NEGATIVE CONTROL: that loop polling the
/// doorbell (the code before this fix, which made it at the first wait and never cleared it)
/// fails "the loop that judges nothing makes no doorbell".
#[cfg(any(unix, windows))]
#[test]
fn with_the_judging_switched_off_a_launchs_ring_wakes_nothing() {
    let _serial = beat_serial();
    let watch = private_watch();
    let looped = Loop::start(watch.0, None, true);
    let wall = looped.wall.expect("a wall watch");
    let stepped = aterm_uds::wall_watch::Moved {
        stepped: true,
        ..Default::default()
    };
    assert_eq!(looped.next(), Seen::Waits { rest: true });
    wall.tell(stepped);
    assert_eq!(looped.next(), Seen::WallMoved);
    assert_eq!(looped.next(), Seen::Waits { rest: true });
    assert!(
        watch.0.bell.get().is_none(),
        "the loop that judges nothing makes no doorbell: it has waited once"
    );
    launching_on(watch.0, OnLaunchStall::Log);
    wall.tell(stepped);
    assert_eq!(
        looped.next(),
        Seen::WallMoved,
        "the wall, not the ring, wakes the loop that judges nothing"
    );
    assert_eq!(looped.next(), Seen::Waits { rest: true });
}

/// Windows' std `Instant` counts a suspend, so the span it counted less the time the machine was
/// awake is the suspend; under a second it is the two clocks' grain, and unread it is nothing.
#[test]
fn a_suspend_is_what_the_counting_clock_ran_past_the_awake_one() {
    let hour = Duration::from_secs(3_600);
    let step = Duration::from_millis(2_500);
    assert_eq!(suspended(step + hour, Some(step)), hour);
    assert_eq!(
        suspended(step, Some(step - Duration::from_millis(16))),
        Duration::ZERO,
        "the interrupt time's grain is no suspend"
    );
    assert_eq!(
        suspended(step + SUSPEND_FLOOR, Some(step)),
        SUSPEND_FLOOR,
        "the floor itself counts"
    );
    assert_eq!(
        suspended(step, Some(step + hour)),
        Duration::ZERO,
        "an awake clock that ran further says nothing"
    );
    assert_eq!(suspended(step + hour, None), Duration::ZERO, "unread");
}

/// A forward step of the wall clock is the wall running past a clock that counts a sleep as it
/// does; a sleep moves both, so it is no step, and neither is a wall set back or a small drift.
#[test]
fn a_forward_step_is_the_wall_running_past_a_clock_that_counts_sleep() {
    let hour = Duration::from_secs(3_600);
    let step = Duration::from_millis(2_500);
    assert_eq!(stepped_forward(Some(step + hour), step), Some(hour));
    assert_eq!(
        stepped_forward(Some(step + hour), step + hour),
        None,
        "a sleep: both clocks counted it"
    );
    assert_eq!(stepped_forward(None, step), None, "set back");
    assert_eq!(
        stepped_forward(Some(step + PROCESS_GAP - Duration::from_millis(1)), step),
        None,
        "under the bar"
    );
    assert_eq!(
        stepped_forward(Some(step + PROCESS_GAP), step),
        Some(PROCESS_GAP)
    );
    assert_eq!(stepped_forward(Some(step), step + hour), None);
    let line = stepped_message(Duration::from_secs(u64::MAX));
    assert!(
        line.len() <= aterm_log::record_cap(aterm_log::Level::Info),
        "{line}"
    );
    assert!(!line.contains("MAIN-THREAD STALL"), "{line}");
}

/// A rest has no deadline only where the wall watch rings for a step of the wall clock; where
/// it does not (Windows) a rest wakes after [`STEP_POLL`] to read the clocks for one, so a step
/// set while the main thread is parked is never waited out until its next beat. Off a rest the
/// wait is the next sample's either way. NEGATIVE CONTROL: with steps announced the rest is
/// unbounded, as on macOS and Linux.
#[test]
fn a_rest_is_bounded_where_no_step_of_the_wall_clock_is_announced() {
    let asleep = Instant::now();
    let sample = Duration::from_millis(250);
    for steps_announced in [false, true] {
        assert_eq!(
            wait_until(false, asleep, sample, steps_announced),
            Some(asleep + sample),
            "{steps_announced}"
        );
    }
    assert_eq!(
        wait_until(true, asleep, sample, false),
        Some(asleep + STEP_POLL)
    );
    assert_eq!(wait_until(true, asleep, sample, true), None);
    // A step is read sooner than the least one `stepped_forward` names.
    const { assert!(STEP_POLL.as_secs() < PROCESS_GAP.as_secs()) };
    #[cfg(any(unix, windows))]
    assert_eq!(STEPS_ANNOUNCED, !cfg!(windows), "where the bound applies");
}

/// A suspend the sampler's clock counted is skipped: a main thread that was inside a turn for
/// 40 ms when the machine slept an hour is not a stall at the wake, and becomes one only once it
/// has been frozen the threshold in awake time. NEGATIVE CONTROL: without the skip the same
/// sample reports the hour as a stall.
#[test]
fn a_suspend_the_clock_counted_is_no_stall() {
    let threshold = Duration::from_millis(100);
    let hour = Duration::from_secs(3_600);
    let t0 = Instant::now();
    let inside = Duration::from_millis(40);
    let at_wake = t0 + inside + hour;

    let mut unskipped = Sampler::with_threshold(t0, 7, threshold);
    assert_eq!(
        unskipped
            .poll(at_wake, 7, Breadcrumb::WindowEvent)
            .map(|_| ()),
        Some(()),
        "the control: the sleep read as a frozen hour"
    );

    let mut s = Sampler::with_threshold(t0, 7, threshold);
    s.skip(hour);
    assert_eq!(s.poll(at_wake, 7, Breadcrumb::WindowEvent), None);
    assert_eq!(
        s.poll(
            at_wake + threshold - inside - Duration::from_millis(1),
            7,
            Breadcrumb::WindowEvent
        ),
        None,
        "still under the bar in awake time"
    );
    assert_eq!(
        s.poll(at_wake + threshold - inside, 7, Breadcrumb::WindowEvent),
        Some(Breadcrumb::WindowEvent),
        "frozen the threshold in awake time"
    );
}

/// The system's own announcement reaches the running loop: a resume told to the real
/// registration's callback (a real suspend cannot be asked for) wakes the resting sampler,
/// which runs `on_wall_moved`.
#[cfg(windows)]
#[test]
fn a_resume_the_power_manager_announces_drives_on_wall_moved_at_rest() {
    use aterm_sysprobe::power::windows::{
        PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, PowerEvent, listen,
    };
    let watch = private_watch();
    beat_private(watch, Breadcrumb::AboutToWait);
    let looped = Loop::start(watch.0, Some(TEST_JUDGE), true);
    let wall = looped.wall.expect("a wall watch");
    looped.settles();
    // What the wall watch's own listener does with a resume, through a registration of this
    // test's: the callback's decode, then the watch told a wake.
    let listening = listen(Box::new(move |event| {
        if event == PowerEvent::Resume {
            wall.tell(aterm_uds::wall_watch::Moved {
                woke: true,
                ..Default::default()
            });
        }
    }))
    .expect("registered");
    listening.deliver(PBT_APMSUSPEND);
    listening.deliver(PBT_APMRESUMEAUTOMATIC);
    assert_eq!(looped.next(), Seen::Sampled);
    assert_eq!(looped.next(), Seen::WallMoved);
    looped
        .moved
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("on_wall_moved ran");
    assert_eq!(looped.next(), Seen::Waits { rest: true });
}
