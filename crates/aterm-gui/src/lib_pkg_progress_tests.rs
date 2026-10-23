// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_models_atpkg::atpkg_index_wake_highwater_model;
use atpkg::progress::{Overall, Phase, ProgramProgress, ProgressFile};

fn file_with(pid: Option<u32>, heartbeat_unix: u64) -> ProgressFile {
    let mut programs = std::collections::BTreeMap::new();
    programs.insert(
        "trust".to_string(),
        ProgramProgress {
            phase: Phase::Download,
            bytes_done: 5,
            bytes_total: 100,
            build: Some(210),
            bumped: false,
            error: None,
        },
    );
    ProgressFile {
        v: atpkg::progress::PROGRESS_VERSION,
        pid,
        pass: "net".to_string(),
        started_unix: 1,
        generation: None,
        heartbeat_unix,
        overall: Overall {
            programs_done: 0,
            programs_total: 2,
            bytes_done: 5,
            bytes_total: 200,
        },
        queue: vec!["trust".to_string()],
        programs,
        ended_unix: None,
    }
}

#[test]
fn running_verdict_fails_safe() {
    let now = pkg_unix_now();
    let fresh = file_with(Some(7), now);
    assert!(pkg_progress_running(&fresh, 7, true, now, |_| {
        panic!("own child needs no probe")
    }));
    assert!(
        !pkg_progress_running(&fresh, 7, false, now, |_| panic!("no probe")),
        "our child exited ⇒ not running even with a warm heartbeat"
    );
    let stale = file_with(Some(7), now.saturating_sub(11));
    assert!(
        !pkg_progress_running(&stale, 7, true, now, |_| true),
        "heartbeat >10s old ⇒ not running"
    );
    let future = file_with(Some(7), now + 11);
    assert!(
        !pkg_progress_running(&future, 7, true, now, |_| true),
        "an absurd future stamp must not pin the file live"
    );
    let ended = file_with(None, now);
    assert!(
        !pkg_progress_running(&ended, 7, true, now, |_| true),
        "cleared pid = clean pass end ⇒ not running"
    );
    let foreign = file_with(Some(9), now);
    assert!(
        pkg_progress_running(&foreign, 7, true, now, |pid| pid == 9),
        "a foreign writer is answered by the probe"
    );
}

#[test]
fn malformed_and_oversized_progress_ignored() {
    let prefix =
        std::env::temp_dir().join(format!("aterm-pkg-progress-bad-{}", std::process::id()));
    std::fs::create_dir_all(&prefix).unwrap();
    let layout = atpkg::store::Layout {
        prefix: prefix.clone(),
    };
    let mut probe = ForeignProbe::default();
    assert!(
        read_pkg_progress_snapshot(&layout, 1, true, 0, &mut probe).is_none(),
        "absent file reads as no data"
    );
    std::fs::write(layout.progress_file(), b"{not json").unwrap();
    assert!(
        read_pkg_progress_snapshot(&layout, 1, true, 0, &mut probe).is_none(),
        "malformed reads as no data"
    );
    let oversized = " ".repeat(300 * 1024);
    std::fs::write(layout.progress_file(), oversized).unwrap();
    assert!(
        read_pkg_progress_snapshot(&layout, 1, true, 0, &mut probe).is_none(),
        "oversized reads as no data — never partially parsed"
    );
    let _ = std::fs::remove_dir_all(&prefix);
}

#[test]
fn a_pass_that_ended_before_this_child_is_not_this_childs_pass() {
    let prefix =
        std::env::temp_dir().join(format!("aterm-pkg-progress-stale-{}", std::process::id()));
    std::fs::create_dir_all(&prefix).unwrap();
    let layout = atpkg::store::Layout {
        prefix: prefix.clone(),
    };
    std::fs::write(
        layout.progress_file(),
        br#"{"v":1,"pid":null,"pass":"net","started_unix":1789065336,
                 "heartbeat_unix":1789065347,
                 "overall":{"programs_done":2,"programs_total":2,
                 "bytes_done":312816252,"bytes_total":312816252},
                 "queue":[],"programs":{},"ended_unix":1789065347}"#,
    )
    .unwrap();
    let mut probe = ForeignProbe::default();
    assert!(
        read_pkg_progress_snapshot(&layout, 1, true, 1_789_161_957, &mut probe).is_none(),
        "a pass that ended 27 hours ago is not this child's pass"
    );
    assert!(
        read_pkg_progress_snapshot(&layout, 1, true, 1_789_065_000, &mut probe).is_some(),
        "a pass that ended after this tailer began is this child's own"
    );
    let _ = std::fs::remove_dir_all(&prefix);
}

#[test]
fn a_routine_pass_posts_only_the_reads_that_could_paint() {
    let read = |bytes_done: u64, bytes_total: u64, running: bool| {
        let mut file = file_with(Some(7), 1);
        file.overall.bytes_done = bytes_done;
        file.overall.bytes_total = bytes_total;
        PkgProgressSnapshot { file, running }
    };
    let mut routine = PkgPostGate::new(aterm_messages::Waiter::Nobody);
    assert!(routine.admits(&read(1, 100, true)), "the first read");
    for done in 2..50 {
        assert!(
            !routine.admits(&read(done, 100, true)),
            "a light routine pass's byte counters are no wake"
        );
    }
    assert!(routine.admits(&read(100, 100, false)), "running moved");
    let heavy = crate::toolchain_words::HEAVY_PASS_BYTES;
    let mut big = PkgPostGate::new(aterm_messages::Waiter::Nobody);
    assert!(big.admits(&read(1, 100, true)));
    assert!(!big.admits(&read(2, 100, true)));
    assert!(
        big.admits(&read(3, heavy, true)),
        "the read that turns heavy"
    );
    assert!(big.admits(&read(4, heavy, true)), "then every change");
    let mut first = PkgPostGate::new(aterm_messages::Waiter::Person);
    for done in 1..5 {
        assert!(
            first.admits(&read(done, 100, true)),
            "a first run, and a person's Settings verb, paint every read"
        );
    }
}

#[test]
fn child_exit_interrupts_a_parked_progress_tailer() {
    let prefix = std::env::temp_dir().join(format!(
        "aterm-pkg-progress-finish-wake-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&prefix).unwrap();
    let layout = atpkg::store::Layout {
        prefix: prefix.clone(),
    };
    let file = file_with(Some(std::process::id()), pkg_unix_now());
    std::fs::write(layout.progress_file(), aterm_json::to_vec(&file).unwrap()).unwrap();
    let (park_tx, park_rx) = std::sync::mpsc::channel::<()>();
    let (post_tx, post_rx) = std::sync::mpsc::channel();
    let tailer = PkgProgressTailer::spawn_with_ticks(
        layout,
        std::process::id(),
        aterm_messages::Waiter::Nobody,
        move |snapshot| {
            let _ = post_tx.send(snapshot);
        },
        PkgProgressTicks {
            visible: PKG_PROGRESS_VISIBLE_TICK,
            quiet: Duration::from_secs(600),
        },
        move |_| {
            let _ = park_tx.send(());
        },
        || {},
    )
    .expect("tailer spawns");
    assert_eq!(
        park_rx.recv_timeout(Duration::from_secs(60)),
        Ok(()),
        "the worker passed its stop check before finish"
    );
    let first = post_rx
        .recv_timeout(Duration::from_secs(60))
        .unwrap()
        .unwrap();
    assert!(first.running);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let finisher = std::thread::spawn(move || {
        let _ = done_tx.send(tailer.finish());
    });
    assert!(
        done_rx.recv_timeout(Duration::from_secs(60)).is_ok(),
        "finish must interrupt the ten-minute wait"
    );
    finisher.join().unwrap();
    let last = post_rx
        .recv_timeout(Duration::from_secs(60))
        .unwrap()
        .unwrap();
    assert!(!last.running, "exit makes the final read immediately");
    let _ = std::fs::remove_dir_all(&prefix);
}

#[test]
fn bump_rate_floor_and_failure_filter() {
    let now = Instant::now();
    assert!(bump_pass_allowed(now, None), "first trigger is free");
    assert!(
        !bump_pass_allowed(now + Duration::from_secs(299), Some(now)),
        "inside the floor ⇒ refused"
    );
    assert!(
        bump_pass_allowed(now + Duration::from_secs(300), Some(now)),
        "floor lapsed ⇒ allowed"
    );
    let mut failed = file_with(None, 0);
    failed.programs.get_mut("trust").expect("fixture row").phase = Phase::Failed;
    let names = vec!["trust".to_string()];
    assert!(
        !bump_should_trigger(&names, Some(&failed)),
        "a failure already recorded in the current pass does not re-trigger"
    );
    assert!(
        bump_should_trigger(&names, None),
        "no pass record ⇒ the wish stands"
    );
    let names2 = vec!["trust".to_string(), "ty".to_string()];
    assert!(
        bump_should_trigger(&names2, Some(&failed)),
        "any un-failed name still triggers"
    );
    assert!(
        !bump_should_trigger(&[], Some(&failed)),
        "no admitted names ⇒ nothing to do"
    );
}

#[test]
fn published_index_wakes_the_package_park_without_cutting_failure_backoff() {
    let layout = atpkg::store::Layout {
        prefix: std::env::temp_dir().join("aterm-pkg-park-probe-absent"),
    };
    let now = Instant::now();
    let mut watch = BumpWatch {
        last_index_probe: Some(now),
        ..BumpWatch::default()
    };
    let mut calls = 0;
    assert_eq!(
        published_index_wakes(&layout, &mut watch, true, now, |_| {
            calls += 1;
            atpkg::index_probe::Probe::Published(44)
        }),
        None
    );
    assert_eq!(calls, 0, "the probe waits for its own cadence");

    let due = now + atpkg::index_probe::INTERVAL;
    assert_eq!(
        published_index_wakes(&layout, &mut watch, false, due, |_| panic!(
            "a contended pass leaves the probe to its holder"
        )),
        None
    );
    assert_eq!(watch.last_index_probe, Some(now));

    assert_eq!(
        published_index_wakes(&layout, &mut watch, true, due, |_| {
            atpkg::index_probe::Probe::Missing
        }),
        None
    );
    assert_eq!(watch.last_index_probe, Some(due));

    assert_eq!(
        published_index_wakes(
            &layout,
            &mut watch,
            true,
            due + atpkg::index_probe::INTERVAL,
            |_| {
                calls += 1;
                atpkg::index_probe::Probe::Published(44)
            },
        ),
        Some(44)
    );
    assert_eq!(calls, 1);
}

#[test]
fn first_eligible_park_probes_now_then_keeps_completion_cadence() {
    let layout = atpkg::store::Layout {
        prefix: std::env::temp_dir().join("aterm-index-first-park-fake"),
    };
    let mut watch = BumpWatch::default();
    let now = Instant::now();
    assert!(first_index_probe_wakes_park(true, Some(&layout), &watch));
    assert!(!first_index_probe_wakes_park(false, Some(&layout), &watch));
    assert!(!first_index_probe_wakes_park(true, None, &watch));
    start_index_probe(&layout, &mut watch, false, now, |_| {
        panic!("automatic updates off cannot probe")
    });
    assert!(watch.pending_index_probe.is_none());
    assert!(watch.last_index_probe.is_none());

    watch.note_full_pass_started();
    start_index_probe(&layout, &mut watch, true, now, |_| {
        atpkg::index_probe::Probe::Published(44)
    });
    assert!(watch.pending_index_probe.is_some(), "no launch warmup");
    assert!(
        !first_index_probe_wakes_park(true, Some(&layout), &watch),
        "no zero-wait spin"
    );
    wait_for_what_must_happen("fake HEAD answered", || index_probe_answer_ready(&watch));
    assert_eq!(take_ready_index_probe(&layout, &mut watch), Some(44));
    let completed = watch.last_index_probe.expect("completion clock");

    start_index_probe(
        &layout,
        &mut watch,
        true,
        completed + atpkg::index_probe::INTERVAL - Duration::from_nanos(1),
        |_| panic!("local cooldown still holds"),
    );
    assert!(watch.pending_index_probe.is_none());
    start_index_probe(
        &layout,
        &mut watch,
        true,
        completed + atpkg::index_probe::INTERVAL,
        |_| atpkg::index_probe::Probe::Missing,
    );
    assert!(watch.pending_index_probe.is_some(), "next probe is due");
    watch
        .pending_index_probe
        .take()
        .unwrap()
        .worker
        .join()
        .unwrap();
}

#[test]
fn an_index_the_pass_did_not_land_is_not_a_wake_at_every_cooldown() {
    let layout = atpkg::store::Layout {
        prefix: std::env::temp_dir().join("aterm-pkg-park-unlanded-absent"),
    };
    let minute = atpkg::index_probe::INTERVAL;
    let t0 = Instant::now();
    let mut watch = BumpWatch {
        last_index_probe: Some(t0),
        unlanded: Some(44),
        ..BumpWatch::default()
    };
    let published =
        |build| move |_: &atpkg::store::Layout| atpkg::index_probe::Probe::Published(build);
    for k in 1..=12 {
        assert_eq!(
            published_index_wakes(&layout, &mut watch, true, t0 + minute * k, published(44)),
            None,
            "minute {k}: the index the pass did not land"
        );
    }
    assert_eq!(watch.unlanded, Some(44), "a deferred answer keeps it");
    assert_eq!(
        published_index_wakes(&layout, &mut watch, true, t0 + minute * 13, |_| {
            atpkg::index_probe::Probe::Deferred
        }),
        None
    );
    assert_eq!(watch.unlanded, Some(44));
    assert_eq!(
        published_index_wakes(&layout, &mut watch, true, t0 + minute * 14, published(45)),
        Some(45),
        "the next index is a new question"
    );
    assert_eq!(watch.unlanded, None);
    let mut again = BumpWatch {
        last_index_probe: Some(t0),
        unlanded: Some(44),
        ..BumpWatch::default()
    };
    assert_eq!(
        published_index_wakes(&layout, &mut again, true, t0 + minute, |_| {
            atpkg::index_probe::Probe::Missing
        }),
        None
    );
    assert_eq!(
        published_index_wakes(&layout, &mut again, true, t0 + minute * 2, published(44)),
        Some(44),
        "the negative control: forgotten, it is a wake"
    );
}

#[test]
fn failed_index_highwater_and_failure_park_refine_the_derived_model() {
    let model = atpkg_index_wake_highwater_model();
    let mut state = model.init_state();
    assert!(model.fire("FailMiddle", &mut state));
    let layout = atpkg::store::Layout {
        prefix: std::env::temp_dir().join("aterm-pkg-highwater-absent"),
    };
    let minute = atpkg::index_probe::INTERVAL;
    let t0 = Instant::now();
    let mut watch = BumpWatch {
        last_index_probe: Some(t0),
        unlanded: Some(49),
        ..BumpWatch::default()
    };
    let lower = published_index_wakes(&layout, &mut watch, true, t0 + minute, |_| {
        atpkg::index_probe::Probe::Published(45)
    });
    assert!(model.fire("HintLow", &mut state));
    assert_eq!(lower, None, "a near 45 cannot replay after far 49 failed");
    assert_eq!(
        (watch.unlanded, state["unlanded"], state["wake"]),
        (Some(49), 2, 0)
    );

    let deferred = published_index_wakes(&layout, &mut watch, true, t0 + minute * 2, |_| {
        atpkg::index_probe::Probe::Deferred
    });
    assert!(model.fire("Deferred", &mut state));
    assert_eq!(deferred, None);
    assert_eq!((watch.unlanded, state["unlanded"]), (Some(49), 2));

    let wall = std::time::SystemTime::now();
    let higher = package_park_tick_wakes(
        &layout,
        &mut watch,
        true,
        wall + Duration::from_secs(3600),
        |_, _| false,
        || (t0 + minute * 3, wall),
        |_| atpkg::index_probe::Probe::Published(50),
    );
    assert!(model.fire("HintHigh", &mut state));
    assert_eq!(higher, Some(ParkEnd::IndexPublished(50)));
    assert_eq!(
        (watch.unlanded, state["unlanded"], state["wake"]),
        (None, 0, 3)
    );

    let mut unlanded = Some(50);
    assert!(model.fire("FailHigh", &mut state));
    assert!(note_pass_ending(
        PassVerb::Update,
        PassWhy::Published(45),
        PassEnding::Failed,
        false,
        1_790_000_000,
        &mut OwnPasses::default(),
        &mut unlanded,
    ));
    assert!(model.fire("FailLow", &mut state));
    assert_eq!((unlanded, state["unlanded"]), (Some(50), 3));
    assert!(model.check_invariant("FailureMarkNeverDowngrades", &state));
}

#[test]
fn package_park_checks_bump_and_deadline_before_remote_probe() {
    let layout = atpkg::store::Layout {
        prefix: std::env::temp_dir().join("aterm-pkg-park-priority-absent"),
    };
    let last_probe = Instant::now();
    let now = last_probe + atpkg::index_probe::INTERVAL;
    let wall = std::time::SystemTime::now();
    let mut watch = BumpWatch {
        last_index_probe: Some(last_probe),
        ..BumpWatch::default()
    };
    assert_eq!(
        package_park_tick_wakes(
            &layout,
            &mut watch,
            true,
            wall + Duration::from_secs(1),
            |_, _| true,
            || panic!("a local bump must win before consulting the clock"),
            |_| panic!("a local bump must win before network I/O"),
        ),
        Some(ParkEnd::Bumped)
    );
    assert_eq!(watch.last_index_probe, Some(last_probe));

    assert_eq!(
        package_park_tick_wakes(
            &layout,
            &mut watch,
            true,
            wall,
            |_, _| false,
            || (now, wall),
            |_| panic!("an elapsed park deadline must win before network I/O"),
        ),
        None
    );
    assert_eq!(watch.last_index_probe, Some(last_probe));

    assert_eq!(
        package_park_tick_wakes(
            &layout,
            &mut watch,
            true,
            wall + Duration::from_secs(1),
            |_, _| false,
            || (now, wall),
            |_| atpkg::index_probe::Probe::Published(44),
        ),
        Some(ParkEnd::IndexPublished(44))
    );
    assert_eq!(watch.last_index_probe, Some(now));

    let next = now + atpkg::index_probe::INTERVAL;
    let bump_reads = std::cell::Cell::new(0);
    assert_eq!(
        package_park_tick_wakes(
            &layout,
            &mut watch,
            true,
            wall + Duration::from_secs(1),
            |_, _| {
                bump_reads.set(bump_reads.get() + 1);
                bump_reads.get() == 2
            },
            || (next, wall),
            |_| atpkg::index_probe::Probe::Missing,
        ),
        Some(ParkEnd::Bumped)
    );
    assert_eq!(
        bump_reads.get(),
        2,
        "a bump arriving during HEAD is read immediately"
    );
}

#[test]
fn a_wake_past_the_deadline_runs_the_pass_twenty_seconds_after_it() {
    let t0 = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    let five = Duration::from_secs(5);
    let until = t0 + Duration::from_secs(60);
    assert_eq!(
        park_deadline_after_slice(until, t0, Some(five), five),
        until
    );
    assert_eq!(
        park_deadline_after_slice(until, t0, Some(Duration::from_secs(20)), five),
        until,
        "a small step is no wake"
    );
    assert_eq!(
        park_deadline_after_slice(until, t0, None, five),
        until,
        "set back"
    );
    let woke = t0 + Duration::from_secs(8 * 3600);
    assert_eq!(
        park_deadline_after_slice(until, woke, Some(Duration::from_secs(8 * 3600)), five),
        woke + atpkg::vendor_direct::watch::AFTER_WAKE,
        "slept through the deadline"
    );
    let later = woke + Duration::from_secs(3600);
    assert_eq!(
        park_deadline_after_slice(later, woke, Some(Duration::from_secs(8 * 3600)), five),
        later,
        "the negative control: a wake short of the deadline keeps it"
    );
}

#[test]
fn a_park_keeps_a_deadline_within_the_wake_gap_of_its_length() {
    let t0 = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    let five = Duration::from_secs(5);
    let ten_minutes = Duration::from_secs(600);
    let three = Duration::from_secs(3);
    assert_eq!(park_slice(t0 + ten_minutes, t0, ten_minutes), Some(five));
    assert_eq!(park_slice(t0 + three, t0, three), Some(three));
    assert_eq!(
        park_slice(t0 + atpkg::vendor_direct::watch::AFTER_WAKE, t0, three),
        Some(five),
        "the twenty seconds after a wake, on a three-second park"
    );
    assert_eq!(
        park_slice(t0 + ten_minutes + Duration::from_secs(2), t0, ten_minutes),
        Some(five),
        "a clock stepped back two seconds early in the park"
    );
    assert_eq!(
        park_slice(t0 + Duration::from_secs(3600), t0, Duration::from_secs(300)),
        None,
        "an hour out on a five-minute park: the clock went back"
    );
    assert_eq!(park_slice(t0, t0, ten_minutes), None, "due");
    assert_eq!(park_slice(t0 - five, t0, ten_minutes), None, "past");
}

#[test]
fn contention_backoff_doubles_once_then_wedges_and_resets() {
    let mut b = ContentionBackoff::default();
    let applied: Vec<u64> = (1..CONTENTION_WEDGE_CYCLES)
        .map(|_| b.park(false).as_secs())
        .collect();
    assert_eq!(applied, [30, 60], "the parks the loop consults");
    assert!(!b.wedged(), "two timeouts are not yet a wedge");
    b.park(false);
    assert!(b.wedged(), "the third consecutive timeout is");
    b.reset();
    assert!(!b.wedged());
    assert_eq!(b.park(false).as_secs(), 30, "reset starts over");
    let mut d = ContentionBackoff::default();
    d.park(false);
    d.park(false);
    assert_eq!(
        d.park(true).as_secs(),
        30,
        "advanced: not a cycle, the first park again"
    );
    assert!(
        !d.wedged(),
        "…and no wedge on what would have been the third"
    );
    d.park(false);
    d.park(false);
    assert!(!d.wedged(), "two with nothing moving since the advance");
    d.park(false);
    assert!(d.wedged(), "the third is the wedge");
    assert_eq!(d.park(true).as_secs(), 30, "movement un-wedges");
    assert!(!d.wedged());
}

#[test]
fn a_dead_sibling_writer_is_not_posted_over_our_own_waiting_child() {
    let dead_sibling = PkgProgressSnapshot {
        file: file_with(Some(15359), 0),
        running: false,
    };
    assert!(foreign_writer_died_mid_pass(&dead_sibling, 15441));
    assert!(
        !foreign_writer_died_mid_pass(&dead_sibling, 15359),
        "our own dead child's file is our outcome"
    );
    let live_sibling = PkgProgressSnapshot {
        file: file_with(Some(15359), pkg_unix_now()),
        running: true,
    };
    assert!(!foreign_writer_died_mid_pass(&live_sibling, 15441));
    let mut ended = file_with(Some(15359), 0);
    ended.ended_unix = Some(1);
    let ended_sibling = PkgProgressSnapshot {
        file: ended,
        running: false,
    };
    assert!(
        !foreign_writer_died_mid_pass(&ended_sibling, 15441),
        "a sibling's finished pass is a real outcome"
    );
    let no_pid = PkgProgressSnapshot {
        file: file_with(None, 0),
        running: false,
    };
    assert!(!foreign_writer_died_mid_pass(&no_pid, 15441));
}

#[test]
fn a_running_siblings_work_moving_on_is_an_advance_and_a_heartbeat_alone_is_not() {
    let ours = 15441;
    let sibling = 15359;
    let running = |file: &ProgressFile| PkgProgressSnapshot {
        file: file.clone(),
        running: true,
    };
    let mut last = None;
    let mut file = file_with(Some(sibling), pkg_unix_now());
    assert!(
        !holder_work_advanced(&running(&file), ours, &mut last),
        "the first read is the baseline, not an advance"
    );
    let baseline_heartbeat = file.heartbeat_unix;
    file.heartbeat_unix += 5;
    assert!(
        !holder_work_advanced(&running(&file), ours, &mut last),
        "a heartbeat alone is liveness, not progress"
    );
    assert_eq!(
        last.as_ref().map(|f| f.heartbeat_unix),
        Some(baseline_heartbeat),
        "…and is not copied: the baseline is refreshed only on an advance"
    );
    file.programs.get_mut("trust").unwrap().bytes_done = 50;
    assert!(
        holder_work_advanced(&running(&file), ours, &mut last),
        "bytes landing is progress"
    );
    assert_eq!(
        last.as_ref().map(|f| f.heartbeat_unix),
        Some(file.heartbeat_unix),
        "an advance refreshes the whole baseline"
    );
    assert!(
        !holder_work_advanced(&running(&file), ours, &mut last),
        "…counted once"
    );
    file.overall.programs_done += 1;
    assert!(
        holder_work_advanced(&running(&file), ours, &mut last),
        "a program finishing is progress"
    );
    file.queue.push("ay".to_string());
    assert!(
        holder_work_advanced(&running(&file), ours, &mut last),
        "the queue moving is progress"
    );
    let mut own_last = None;
    let mut own = file_with(Some(ours), pkg_unix_now());
    assert!(!holder_work_advanced(&running(&own), ours, &mut own_last));
    own.programs.get_mut("trust").unwrap().bytes_done = 99;
    assert!(!holder_work_advanced(&running(&own), ours, &mut own_last));
    assert!(own_last.is_none());
    let mut dead_last = None;
    let dead = PkgProgressSnapshot {
        file: file_with(Some(sibling), 0),
        running: false,
    };
    assert!(!holder_work_advanced(&dead, ours, &mut dead_last));
    assert!(dead_last.is_none());
}

#[test]
fn the_tailer_reports_a_siblings_advance_at_finish() {
    let prefix =
        std::env::temp_dir().join(format!("aterm-pkg-progress-advance-{}", std::process::id()));
    std::fs::create_dir_all(&prefix).unwrap();
    let layout = atpkg::store::Layout {
        prefix: prefix.clone(),
    };
    let write = |file: &ProgressFile| {
        std::fs::write(layout.progress_file(), aterm_json::to_vec(file).unwrap()).unwrap();
    };
    let mut file = file_with(Some(std::process::id()), pkg_unix_now());
    write(&file);
    let (tx, rx) = std::sync::mpsc::channel::<Option<Box<PkgProgressSnapshot>>>();
    let baseline = |which: &str| {
        let first = rx
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("the {which} tailer posted no running first read"))
            .unwrap_or_else(|| panic!("the {which} tailer's first post is a clear"));
        assert!(first.running, "the {which} tailer's baseline: {first:?}");
    };
    let sender = tx.clone();
    let idle = PkgProgressTailer::spawn(
        layout.clone(),
        1,
        aterm_messages::Waiter::Person,
        move |s| {
            let _ = sender.send(s);
        },
    )
    .expect("tailer spawns");
    baseline("idle");
    file.heartbeat_unix = (file.heartbeat_unix + 1).max(pkg_unix_now());
    write(&file);
    assert!(
        !idle.finish(),
        "heartbeat refreshes alone are not an advance"
    );
    while rx.try_recv().is_ok() {}
    file.heartbeat_unix = file.heartbeat_unix.max(pkg_unix_now());
    write(&file);
    let sender = tx;
    let busy = PkgProgressTailer::spawn(
        layout.clone(),
        1,
        aterm_messages::Waiter::Person,
        move |s| {
            let _ = sender.send(s);
        },
    )
    .expect("tailer spawns");
    baseline("busy");
    file.programs.get_mut("trust").unwrap().bytes_done = 60;
    file.heartbeat_unix = pkg_unix_now();
    write(&file);
    assert!(
        busy.finish(),
        "the sibling's bytes landing during the tailer's life is"
    );
    let _ = std::fs::remove_dir_all(&prefix);
}
