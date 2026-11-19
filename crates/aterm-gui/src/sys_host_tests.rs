// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the `sys` host on a private [`Shared`].

use super::*;
use aterm_vitals::model::{
    ClusterView, CoreView, CpuView, MemSample, MemView, Pressure, ScopeView,
};
use aterm_vitals::sys::json::contract_violations;
// A fixture's absolute path on every host (`C:` before it on Windows, where `/Users//a` is
// relative and so no home `under=` could name).
use crate::test_host::abs;

thread_local! {
    static MEMORY_CLOCK: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    static MEMORY_INSTANT: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

fn memory_clock() -> Option<u64> {
    MEMORY_CLOCK.with(std::cell::Cell::get)
}

fn memory_instant() -> Instant {
    MEMORY_INSTANT.with(|clock| clock.get().unwrap())
}

fn memory_receipt(snapshot: &Snapshot, observed_at: ObservedAt) -> Arc<memory::Receipt> {
    memory_receipt_of(
        snapshot,
        observed_at,
        &aterm_sysprobe::Sweep {
            procs: Vec::new(),
            truncated: false,
        },
    )
}

fn memory_receipt_of(
    snapshot: &Snapshot,
    observed_at: ObservedAt,
    sweep: &aterm_sysprobe::Sweep,
) -> Arc<memory::Receipt> {
    let probes = memory::Probes {
        continuous_ns: memory_clock,
        context: || {
            Some(memory::Context {
                scope: aterm_agent::harness::native_act::BootStamp {
                    host: "memory-clock-test".into(),
                    boot: "same-boot".into(),
                },
                namespace: None,
            })
        },
        ..memory::Probes::INERT
    };
    Arc::new(
        memory::Pending::begin(probes, observed_at)
            .finish(sweep)
            .publish(snapshot, Vec::new()),
    )
}

fn groups_frame(seq: u64) -> aterm_vitals::groups::Frame {
    aterm_vitals::groups::Frame {
        seq,
        taken: aterm_vitals::ring::Taken::at(1_800_000_000_000 + seq as i64 * 1000, 1000),
        mono_ms: seq * 1000,
        at_ns: seq * 1_000_000_000,
        processes_observed: true,
        coalitions_observed: true,
        processes_complete: true,
        coalitions_complete: true,
    }
}

fn groups_observation(
    cpu: aterm_vitals::groups::Value<f64>,
) -> aterm_vitals::groups::Observation<'static> {
    use aterm_vitals::groups::{GroupKey, Observation, Rate, ReadWindow, Scope, Value};
    let mut observation = Observation::new(
        GroupKey::Tab {
            pane: 0,
            root: aterm_vitals::delegated::ProcessKey {
                pid: 400,
                start: 500,
            },
        },
        Scope::Tab(0),
        "groups-source-root",
        1,
    );
    observation.cpu = Rate {
        value: cpu,
        window: ReadWindow::Reported {
            end_ns: 1_000_000_000,
            window_ns: 1_000_000_000,
        },
    };
    observation.footprint = Value::Read(4096);
    observation.membership_complete = true;
    observation
}

fn groups_snapshot(seq: u64, book: &aterm_vitals::groups::GroupBook) -> Snapshot {
    let mut snapshot = synthetic(seq, 0.1);
    snapshot.at_unix_ms = groups_frame(seq).taken.at_ms;
    snapshot.groups = book.view();
    // Groups is its own source view, not a CPU warm pair or a retained Why explanation.
    snapshot.cpu = None;
    snapshot.aterm = None;
    snapshot.procs_window_ns = None;
    snapshot.why = None;
    snapshot.coal_lane = aterm_vitals::explain::CoalLane::On;
    snapshot
}

fn groups_receipt(
    snapshot: &Snapshot,
    observed_at: ObservedAt,
    truncated: bool,
) -> Arc<memory::Receipt> {
    memory_receipt_of(
        snapshot,
        observed_at,
        &aterm_sysprobe::Sweep {
            procs: vec![aterm_sysprobe::ProcessReading {
                pid: 400,
                start: Some(500),
                name: "groups-source-root".into(),
                footprint: Some(4096),
                ..aterm_sysprobe::ProcessReading::default()
            }],
            truncated,
        },
    )
}

#[test]
fn sys_groups_freshness_keeps_original_census_age_across_new_machine_frames() {
    use aterm_vitals::groups::{GroupBook, Value};
    const SECOND: u64 = 1_000_000_000;
    const SLEEP: u64 = 6 * 60 * 60 * SECOND;
    let base = Instant::now();
    MEMORY_INSTANT.with(|clock| clock.set(Some(base)));
    MEMORY_CLOCK.with(|clock| clock.set(Some(0)));
    let shared = Shared::with_clocks(memory_instant, memory_clock);
    let first = ObservedAt::capture(memory_instant, memory_clock);
    let mut book = GroupBook::new();
    book.absorb(groups_frame(1), [groups_observation(Value::Read(0.75))]);
    let publish = |snapshot: Snapshot, observed_at, receipt| {
        shared.publish(
            Arc::new(snapshot),
            &Machine::default(),
            observed_at,
            None,
            None,
            true,
            receipt,
        );
    };
    let snapshot = groups_snapshot(1, &book);
    publish(
        snapshot.clone(),
        first,
        Some(groups_receipt(&snapshot, first, false)),
    );
    let query = SysQuery::parse("groups --json fresh=60000").expect("groups query");
    let read = || {
        shared.fresh_for(
            &query,
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
        )
    };
    let (held, state) = read();
    assert_eq!(
        state,
        Freshness::Fresh,
        "current groups need no CPU warm-up or Why cache"
    );
    assert_eq!(
        held.as_ref().unwrap().snap.groups.rows()[0]
            .cpu()
            .value
            .value(),
        Some(0.75)
    );
    let reply = answer(
        &shared,
        &query,
        held.as_ref(),
        state,
        &Machine::default(),
        &gathered(),
        true,
    );
    assert!(
        reply.contains("groups-source-root"),
        "a real group is rendered: {reply}"
    );

    // A new global publication carrying the old book cannot make that book current, even
    // while all clocks and its newly paired process receipt would otherwise be recent.
    let snapshot = groups_snapshot(2, &book);
    publish(
        snapshot.clone(),
        first,
        Some(groups_receipt(&snapshot, first, false)),
    );
    assert_eq!(read().1, Freshness::Stale);

    MEMORY_INSTANT.with(|clock| clock.set(Some(base + Duration::from_secs(1))));
    MEMORY_CLOCK.with(|clock| clock.set(Some(SLEEP + SECOND)));
    let current = ObservedAt::capture(memory_instant, memory_clock);
    book.absorb(groups_frame(3), [groups_observation(Value::Read(0.25))]);
    let snapshot = groups_snapshot(3, &book);
    let old_receipt = groups_receipt(&snapshot, first, false);
    publish(snapshot, current, Some(Arc::clone(&old_receipt)));
    let (held, state) = read();
    assert_eq!(
        state,
        Freshness::Stale,
        "an older in-flight census keeps its original age"
    );
    assert!(Arc::ptr_eq(
        held.as_ref().unwrap().processes.as_ref().unwrap(),
        &old_receipt
    ));
    let reply = answer(
        &shared,
        &query,
        held.as_ref(),
        state,
        &Machine::default(),
        &gathered(),
        true,
    );
    let body: aterm_json::Value = aterm_json::from_str(reply.lines().nth(1).unwrap()).unwrap();
    assert_eq!(body["as_of"]["state"].as_str(), Some("stale"));
    assert!(body["as_of"]["age_ms"].as_u64().unwrap() >= (SLEEP + SECOND) / 1_000_000);

    let mut machine_only = groups_snapshot(4, &book);
    machine_only.machine_only = true;
    publish(machine_only, current, None);
    let (held, state) = read();
    assert_eq!((held.unwrap().snap.seq, state), (3, Freshness::Stale));
    MEMORY_CLOCK.with(|clock| clock.set(None));
    MEMORY_INSTANT.with(|clock| clock.set(None));
}

#[test]
fn sys_groups_freshness_requires_matching_observed_sources_and_preserves_partial_values() {
    use aterm_vitals::groups::{GroupBook, Value};
    MEMORY_INSTANT.with(|clock| clock.set(Some(Instant::now())));
    MEMORY_CLOCK.with(|clock| clock.set(Some(0)));
    let shared = Shared::with_clocks(memory_instant, memory_clock);
    let at = ObservedAt::capture(memory_instant, memory_clock);
    let query = SysQuery::parse("groups fresh=60000").unwrap();
    let publish = |snapshot: Snapshot, receipt| {
        shared.publish(
            Arc::new(snapshot),
            &Machine::default(),
            at,
            None,
            None,
            true,
            receipt,
        );
        shared
            .fresh_for(
                &query,
                &Arming::new(),
                &Arming::new(),
                &Arming::new(),
                &Arming::new(),
                &Arming::new(),
            )
            .1
    };
    let mut book = GroupBook::new();
    book.absorb(groups_frame(1), [groups_observation(Value::Read(0.75))]);
    let mut partial = groups_frame(2);
    partial.processes_complete = false;
    book.absorb(
        partial,
        [groups_observation(Value::Partial {
            value: 0.25,
            unread: 1,
        })],
    );
    let snapshot = groups_snapshot(2, &book);
    let row = &snapshot.groups.rows()[0];
    assert_eq!(row.cpu().value.value(), Some(0.25));
    assert!(
        row.cpu().value.unread() > 0,
        "neither the older 0.75 nor an invented zero"
    );
    assert_eq!(
        publish(snapshot.clone(), Some(groups_receipt(&snapshot, at, true))),
        Freshness::Partial
    );

    for (seq, field) in [(3, "process"), (4, "coalition"), (5, "seq"), (6, "time")] {
        let mut frame = groups_frame(seq);
        match field {
            "process" => {
                frame.processes_observed = false;
                frame.processes_complete = false;
            }
            "coalition" => {
                frame.coalitions_observed = false;
                frame.coalitions_complete = false;
            }
            "seq" => frame.seq -= 1,
            _ => frame.at_ns -= 1,
        }
        book.absorb(frame, std::iter::empty());
        let snapshot = groups_snapshot(seq, &book);
        assert!(
            snapshot
                .groups
                .rows()
                .iter()
                .all(|row| row.cpu().value.value().is_none()),
            "a missing current observation cannot retain the old live value: {field}"
        );
        assert_eq!(
            publish(snapshot.clone(), Some(groups_receipt(&snapshot, at, false))),
            Freshness::Stale,
            "{field}"
        );
    }
    book.absorb(groups_frame(7), [groups_observation(Value::Read(0.5))]);
    assert_eq!(
        publish(groups_snapshot(7, &book), None),
        Freshness::Stale,
        "missing original census"
    );
    let snapshot = groups_snapshot(8, &GroupBook::new());
    assert_eq!(
        publish(snapshot.clone(), Some(groups_receipt(&snapshot, at, false))),
        Freshness::Warming,
        "never sampled"
    );
    MEMORY_CLOCK.with(|clock| clock.set(None));
    MEMORY_INSTANT.with(|clock| clock.set(None));
}

#[test]
fn sys_groups_forced_pull_arms_split_without_restamping_its_process_source() {
    use aterm_vitals::groups::{GroupBook, Value};
    const HANG: Duration = Duration::from_secs(60);
    const NOW_NS: u64 = 10_000_000_000;
    let base = Instant::now();
    let original = ObservedAt {
        instant: base,
        continuous_ns: Some(0),
    };
    let current = ObservedAt {
        instant: base + Duration::from_secs(10),
        continuous_ns: Some(NOW_NS),
    };
    MEMORY_INSTANT.with(|clock| clock.set(Some(current.instant)));
    MEMORY_CLOCK.with(|clock| clock.set(Some(NOW_NS)));
    for old in [true, false] {
        let shared = Arc::new(Shared::paced_with_clocks(
            HANG,
            HANG,
            memory_instant,
            memory_clock,
        ));
        let armed = Arc::new(Arming::new());
        let mut book = GroupBook::new();
        book.absorb(groups_frame(1), [groups_observation(Value::Read(0.75))]);
        let snapshot = groups_snapshot(1, &book);
        shared.publish(
            Arc::new(snapshot.clone()),
            &Machine::default(),
            original,
            None,
            None,
            true,
            Some(groups_receipt(&snapshot, original, false)),
        );
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        shared.install(tx);
        let producer = {
            let shared = Arc::clone(&shared);
            let armed = Arc::clone(&armed);
            std::thread::spawn(move || {
                MEMORY_CLOCK.with(|clock| clock.set(Some(NOW_NS)));
                assert!(matches!(
                    rx.recv_timeout(HANG).expect("one requested pull"),
                    Msg::Pull
                ));
                assert!(
                    armed.armed(),
                    "Groups requests the existing coalition/process lane"
                );
                book.absorb(groups_frame(2), [groups_observation(Value::Read(0.25))]);
                let snapshot = groups_snapshot(2, &book);
                let receipt =
                    groups_receipt(&snapshot, if old { original } else { current }, false);
                shared.publish(
                    Arc::new(snapshot),
                    &Machine::default(),
                    current,
                    None,
                    None,
                    true,
                    Some(Arc::clone(&receipt)),
                );
                receipt
            })
        };
        let query = SysQuery::parse("groups fresh=0").unwrap();
        let (held, state) = shared.fresh_for(
            &query,
            &armed,
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
        );
        assert_eq!(
            state,
            if old {
                Freshness::Stale
            } else {
                Freshness::Fresh
            }
        );
        assert_eq!(held.as_ref().unwrap().snap.seq, 2);
        assert!(!armed.armed());
        shared.uninstall();
        let receipt = producer.join().unwrap();
        assert!(Arc::ptr_eq(
            held.as_ref().unwrap().processes.as_ref().unwrap(),
            &receipt
        ));
        assert_eq!(receipt.observed_at, if old { original } else { current });
    }
    MEMORY_CLOCK.with(|clock| clock.set(None));
    MEMORY_INSTANT.with(|clock| clock.set(None));
}

#[test]
fn one_engine_processes_peer_age_keeps_old_census_across_sleep_and_new_machine_frames() {
    const BEFORE: u64 = 10_000_000_000;
    const SLEEP: u64 = 6 * 60 * 60 * 1_000_000_000;
    MEMORY_CLOCK.with(|clock| clock.set(Some(BEFORE)));
    MEMORY_INSTANT.with(|clock| clock.set(Some(Instant::now())));
    let shared = Shared::with_clocks(memory_instant, memory_clock);
    let first = ObservedAt::capture(memory_instant, memory_clock);
    let snapshot = synthetic(1, 0.1);
    let receipt = memory_receipt(&snapshot, first);
    shared.publish(
        Arc::new(snapshot),
        &Machine::default(),
        first,
        None,
        None,
        true,
        Some(receipt),
    );
    let query = SysQuery::parse("aterm --json fresh=60000").unwrap();
    let read = || {
        let (held, state) = shared.fresh_for(
            &query,
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
        );
        let text = answer(
            &shared,
            &query,
            held.as_ref(),
            state,
            &Machine::default(),
            &gathered(),
            true,
        );
        let json: aterm_json::Value = aterm_json::from_str(text.lines().nth(1).unwrap()).unwrap();
        (held.unwrap().snap.seq, json)
    };
    let (_, fresh) = read();
    assert_eq!(fresh["as_of"]["state"].as_str(), Some("fresh"));
    assert!(fresh["as_of"]["age_ms"].as_u64().unwrap() < 60_000);

    MEMORY_CLOCK.with(|clock| clock.set(Some(BEFORE + SLEEP)));
    let after_sleep = ObservedAt::capture(memory_instant, memory_clock);
    let mut machine_only = synthetic(2, 0.1);
    machine_only.machine_only = true;
    shared.publish(
        Arc::new(machine_only),
        &Machine::default(),
        after_sleep,
        None,
        None,
        true,
        None,
    );
    let (seq, stale) = read();
    assert_eq!(
        seq, 1,
        "a machine-only frame retains the original whole census"
    );
    assert_eq!(stale["as_of"]["state"].as_str(), Some("stale"));
    assert!(stale["as_of"]["age_ms"].as_u64().unwrap() >= SLEEP / 1_000_000);

    let snapshot = synthetic(3, 0.1);
    let older_receipt = memory_receipt(&snapshot, first);
    shared.publish(
        Arc::new(snapshot),
        &Machine::default(),
        after_sleep,
        None,
        None,
        true,
        Some(older_receipt),
    );
    let (_, older) = read();
    assert_eq!(older["as_of"]["state"].as_str(), Some("stale"));
    assert!(
        older["as_of"]["age_ms"].as_u64().unwrap() >= SLEEP / 1_000_000,
        "a late native-lane answer is not timestamped with the newer frame"
    );

    let snapshot = synthetic(4, 0.1);
    let receipt = memory_receipt(&snapshot, after_sleep);
    shared.publish(
        Arc::new(snapshot),
        &Machine::default(),
        after_sleep,
        None,
        None,
        true,
        Some(receipt),
    );
    let (_, recovered) = read();
    assert_eq!(recovered["as_of"]["state"].as_str(), Some("fresh"));
    assert!(recovered["as_of"]["age_ms"].as_u64().unwrap() < 60_000);
    MEMORY_CLOCK.with(|clock| clock.set(None));
    MEMORY_INSTANT.with(|clock| clock.set(None));
}

#[test]
fn one_engine_processes_peer_refuses_unknown_and_regressing_age_without_pull_spin() {
    MEMORY_CLOCK.with(|clock| clock.set(Some(100)));
    MEMORY_INSTANT.with(|clock| clock.set(Some(Instant::now())));
    let shared = Shared::with_clocks(memory_instant, memory_clock);
    let observed_at = ObservedAt::capture(memory_instant, memory_clock);
    let snapshot = synthetic(1, 0.1);
    let receipt = memory_receipt(&snapshot, observed_at);
    shared.publish(
        Arc::new(snapshot),
        &Machine::default(),
        observed_at,
        None,
        None,
        true,
        Some(receipt),
    );
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    shared.install(tx);
    let query = SysQuery::parse("aterm --json fresh=60000").unwrap();
    MEMORY_CLOCK.with(|clock| clock.set(None));
    let (held, state) = shared.fresh_for(
        &query,
        &Arming::new(),
        &Arming::new(),
        &Arming::new(),
        &Arming::new(),
        &Arming::new(),
    );
    assert!(matches!(
        rx.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    let reply = answer(
        &shared,
        &query,
        held.as_ref(),
        state,
        &Machine::default(),
        &gathered(),
        true,
    );
    assert!(
        reply.starts_with("ERR "),
        "missing age is not a numeric zero: {reply}"
    );
    MEMORY_CLOCK.with(|clock| clock.set(Some(99)));
    let held = shared.held_now();
    let reply = answer(
        &shared,
        &query,
        held.as_ref(),
        Freshness::Fresh,
        &Machine::default(),
        &gathered(),
        true,
    );
    assert!(
        reply.starts_with("ERR "),
        "a reset is not zero age: {reply}"
    );
    MEMORY_CLOCK.with(|clock| clock.set(None));
    MEMORY_INSTANT.with(|clock| clock.set(None));
}

#[test]
fn one_engine_processes_memory_age_does_not_stale_independent_assessments() {
    let now = Instant::now() + Duration::from_secs(600);
    MEMORY_INSTANT.with(|clock| clock.set(Some(now)));
    MEMORY_CLOCK.with(|clock| clock.set(None));
    let shared = Shared::with_clocks(memory_instant, memory_clock);
    let old_frame = ObservedAt {
        instant: now - Duration::from_secs(600),
        continuous_ns: None,
    };
    let publish = |seq, assessed_at| {
        let mut snapshot = synthetic(seq, 0.1);
        snapshot.jobs = Some(Arc::new(aterm_vitals::jobs::JobsView::default()));
        snapshot.left = Some(Arc::new(aterm_vitals::left::LeftView::default()));
        shared.publish(
            Arc::new(snapshot),
            &Machine::default(),
            old_frame,
            Some(assessed_at),
            Some(assessed_at),
            true,
            None,
        );
    };
    publish(1, now - Duration::from_secs(1));
    for view in ["jobs", "left"] {
        let query = SysQuery::parse(view).unwrap();
        let (held, state) = shared.fresh_for(
            &query,
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
        );
        assert_eq!(
            state,
            Freshness::Fresh,
            "{view}: its pass is newer than the memory frame"
        );
        assert_eq!(
            held.as_ref().unwrap().observed_at,
            old_frame,
            "assessment freshness never restamps the machine or its memory"
        );
        let reply = answer(
            &shared,
            &query,
            held.as_ref(),
            state,
            &Machine::default(),
            &gathered(),
            true,
        );
        assert!(
            reply.starts_with("OK "),
            "{view}: memory clock is not its age authority: {reply}"
        );
        assert!(
            !reply.lines().next().unwrap().contains("state=stale"),
            "{reply}"
        );
    }
    publish(2, now - Duration::from_secs(120));
    for view in ["jobs", "left"] {
        let query = SysQuery::parse(view).unwrap();
        let (_, state) = shared.fresh_for(
            &query,
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
        );
        assert_eq!(
            state,
            Freshness::Stale,
            "{view}: the independent old pass is still stale"
        );
    }
    assert!(
        shared
            .fresh_memory(Duration::from_secs(60), false)
            .is_none(),
        "independent assessment freshness never qualifies memory"
    );
    MEMORY_INSTANT.with(|clock| clock.set(None));
}

#[test]
fn one_engine_processes_forced_wrappers_bound_new_arrivals_by_their_view_default() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    static ELAPSED_NS: AtomicU64 = AtomicU64::new(0);
    fn instant() -> Instant {
        *BASE.get().unwrap() + Duration::from_nanos(ELAPSED_NS.load(Ordering::SeqCst))
    }
    fn continuous() -> Option<u64> {
        Some(ELAPSED_NS.load(Ordering::SeqCst))
    }
    let _ = BASE.set(Instant::now());
    MEMORY_CLOCK.with(|clock| clock.set(Some(0)));
    let run = |view: View, fresh: Duration, force: bool, seconds: u64, expected: bool| {
        ELAPSED_NS.store(0, Ordering::SeqCst);
        let shared = Arc::new(Shared::paced_with_clocks(
            Duration::from_secs(60),
            Duration::from_secs(60),
            instant,
            continuous,
        ));
        let observed_at = ObservedAt::capture(instant, continuous);
        let mut snapshot = with_hardware(2, true);
        snapshot.jobs = Some(Arc::new(aterm_vitals::jobs::JobsView::default()));
        snapshot.left = Some(Arc::new(aterm_vitals::left::LeftView::default()));
        let receipt = memory_receipt(&snapshot, observed_at);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        shared.install(tx);
        let probe = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let message = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).unwrap();
                assert!(match message {
                    Msg::MemoryPull => view == View::Mem,
                    Msg::Pull => view != View::Mem,
                    _ => false,
                });
                ELAPSED_NS.store(seconds * 1_000_000_000, Ordering::SeqCst);
                shared.publish(
                    Arc::new(snapshot),
                    &Machine::default(),
                    observed_at,
                    Some(observed_at.instant),
                    Some(observed_at.instant),
                    true,
                    Some(receipt),
                );
            })
        };
        if view == View::Mem {
            assert_eq!(
                shared.fresh_memory(fresh, force).map(|h| h.snap.seq),
                expected.then_some(2),
                "{view:?}, age={seconds}s, fresh={fresh:?}, force={force}"
            );
        } else {
            let (held, state) = match view {
                View::Overview => shared.fresh(fresh, force),
                // As a `sys why mem` is, which prints no self CPU line.
                View::Why => shared.fresh_why(fresh, force, false),
                View::Left => shared.fresh_left(fresh, force),
                _ => unreachable!(),
            };
            assert_eq!(held.map(|h| h.snap.seq), Some(2));
            assert_eq!(
                state,
                if expected {
                    Freshness::Fresh
                } else {
                    Freshness::Stale
                },
                "{view:?}, age={seconds}s, fresh={fresh:?}, force={force}"
            );
        }
        shared.uninstall();
        probe.join().unwrap();
    };
    for view in [View::Overview, View::Why, View::Left, View::Mem] {
        run(view, Duration::ZERO, true, 1, true);
        run(view, Duration::ZERO, true, 10, false);
    }
    run(View::Mem, Duration::from_secs(60), true, 10, true);
    run(View::Mem, Duration::ZERO, false, 1, false);
    MEMORY_CLOCK.with(|clock| clock.set(None));
}

fn live_snapshot() -> Snapshot {
    let mut reader = crate::vitals_host::Reader::new(std::sync::Arc::default());
    let req = crate::vitals_host::Request {
        self_pid: std::process::id(),
        ..crate::vitals_host::Request::default()
    };
    let _ = reader.read(&req);
    let mut x = 0u64;
    for i in 0..2_000_000u64 {
        x = x.wrapping_mul(31).wrapping_add(i);
    }
    assert_ne!(x, 1);
    reader.read(&req)
}

fn synthetic(seq: u64, busy: f32) -> Snapshot {
    let flat = aterm_vitals::TrendView::flat;
    Snapshot {
        at_ns: seq * 1_000_000_000,
        seq,
        cpu: Some(CpuView {
            cores: (0..4)
                .map(|_| CoreView {
                    load: flat(busy),
                    measured: true,
                })
                .collect(),
            clusters: vec![ClusterView {
                name: "CPU".into(),
                short: "CPU".into(),
                cores: (0..4).collect(),
            }],
            busy: flat(busy * 4.0),
            history: Vec::new(),
            core_history: Vec::new(),
        }),
        mem: Some(MemView {
            sample: MemSample {
                total: 8 << 30,
                pressure: Pressure::Normal,
                ..MemSample::default()
            },
            ..MemView::default()
        }),
        global: ScopeView {
            wait: Some(0.05),
            ..ScopeView::default()
        },
        aterm: Some(ScopeView::default()),
        procs_window_ns: Some(1_000_000_000),
        ..Snapshot::default()
    }
}

fn gathered() -> Gathered {
    Gathered {
        target: Target {
            key: 0,
            sid: "s-test".into(),
            window: Some(0),
            tab: Some(1),
            panes: vec![0],
            window_sessions: vec![0],
            cwd: None,
            fg_pgid: None,
            request: None,
        },
        tabs: vec![TabFact {
            sampled_nonce: None,
            key: 0,
            sid: "s-test".into(),
            window: Some(0),
            tab: Some(1),
        }],
        roots: Vec::new(),
        disk: None,
        stores: Vec::new(),
        volume: None,
        caller_panes: None,
        event_caller: None,
        home: None,
        held: None,
    }
}

fn ask(shared: &Shared, request: &str) -> String {
    let q = SysQuery::parse(request).expect("a request the grammar takes");
    let (held, state) = shared.fresh(Duration::from_secs(3600), false);
    answer(
        shared,
        &q,
        held.as_ref(),
        state,
        &Machine::default(),
        &gathered(),
        true,
    )
}

fn cursor_of(reply: &str) -> String {
    reply
        .lines()
        .next()
        .and_then(|h| h.split_whitespace().find_map(|t| t.strip_prefix("next=")))
        .expect("a header with next=")
        .to_owned()
}

#[test]
fn every_view_of_a_live_snapshot_answers_and_its_json_keeps_the_contract() {
    let shared = Shared::with_clock(|| Some(0));
    shared.publish_for_test(live_snapshot(), Vec::new());
    for view in View::ALL {
        let text = ask(&shared, view.word());
        let header = text.lines().next().unwrap();
        let n: usize = header.split_whitespace().nth(1).unwrap().parse().unwrap();
        assert_eq!(text.lines().count(), n + 1, "{view:?}: {text}");
        assert!(
            header.contains(&format!(" sys v=1 view={} ", view.word())),
            "{header}"
        );
        let json = ask(&shared, &format!("{} --json", view.word()));
        let mut lines = json.lines();
        assert!(lines.next().unwrap().starts_with("OK 1 sys v=1 "));
        let body = lines.next().expect("one JSON line");
        assert!(lines.next().is_none());
        let schema = if view == View::Disk { 2 } else { 1 };
        assert!(
            body.starts_with(&format!("{{\"schema\":{schema},\"kind\":\"sys.")),
            "{body}"
        );
        let q = SysQuery::parse(&format!("{} --json", view.word())).unwrap();
        let (held, state) = shared.fresh(Duration::from_secs(3600), false);
        let g = gathered();
        let ctx = Ctx {
            snap: held.as_ref().map(|h| h.snap.as_ref()),
            epoch: *EPOCH,
            as_of_unix_ms: 0,
            age_ms: Some(0),
            state,
            machine: &Machine::default(),
            target: &g.target,
            tabs: &g.tabs,
            caller_panes: None,
            event_caller: None,
            owner_class: true,
            volume: None,
            roots: &[],
            home: None,
            events: &[],
            gap: None,
            disk: None,
            stores: &[],
            head_seq: 0,
            held: None,
        };
        let v = contract_violations(&render_json(&q, &ctx));
        assert!(v.is_empty(), "{view:?}: {v:?}");
    }
}

#[test]
fn a_since_cursor_answers_a_delta_and_its_gaps() {
    static AT: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    fn instant() -> Instant {
        *AT.get().unwrap()
    }
    let _ = AT.set(Instant::now());
    let shared = Shared::with_clocks(instant, || Some(0));
    shared.publish_for_test(synthetic(3, 0.1), Vec::new());
    let first = ask(&shared, "global");
    let cursor = cursor_of(&first);
    assert_eq!(cursor, format!("{:x}.3", *EPOCH));
    let again = ask(&shared, &format!("global since={cursor}"));
    let header = again.lines().next().unwrap();
    assert!(header.ends_with(" delta=1"), "{again}");
    assert!(
        again.lines().nth(1).unwrap().starts_with("same "),
        "{again}"
    );
    shared.publish_for_test(synthetic(4, 0.6), Vec::new());
    let moved = ask(&shared, &format!("global since={cursor}"));
    assert!(moved.lines().any(|l| l.starts_with("cpu ")), "{moved}");
    assert!(moved.lines().any(|l| l.starts_with("same ")), "{moved}");
    let other = ask(&shared, &format!("cores since={cursor}"));
    assert!(
        other.lines().any(|l| l == "gap why=other-request"),
        "{other}"
    );
    assert!(!other.lines().next().unwrap().contains("delta=1"));
    let shorter = ask(&shared, &format!("global top=1 since={cursor}"));
    assert!(
        shorter.lines().any(|l| l == "gap why=other-request"),
        "{shorter}"
    );
    shared.publish_for_test(synthetic(5, 0.6), Vec::new());
    let json = cursor_of(&ask(&shared, "global --json"));
    let json_again = ask(&shared, &format!("global --json since={json}"));
    assert!(!json_again.contains("\"gap\""), "{json_again}");
    let text_of_json = ask(&shared, &format!("global since={json}"));
    assert!(
        text_of_json.lines().any(|l| l == "gap why=other-request"),
        "{text_of_json}"
    );
    let gone = ask(&shared, &format!("global since={:x}.1", *EPOCH));
    assert!(gone.lines().any(|l| l == "gap why=evicted"), "{gone}");
    let reset = ask(&shared, &format!("global since={:x}.3", *EPOCH ^ 1));
    assert!(reset.lines().any(|l| l == "gap why=reset"), "{reset}");
    let ahead = ask(&shared, &format!("global since={:x}.99", *EPOCH));
    assert_eq!(ahead, format!("ERR bad since head={:x}.5\n", *EPOCH));
}

#[test]
fn history_and_events_do_not_fill_the_delta_cache_or_displace_a_usable_reply() {
    use aterm_vitals::events::EventScope;
    use aterm_vitals::model::{Sample, StripEvent, StripEventKind, Ticks, Vitals};
    let base = Instant::now();
    MEMORY_INSTANT.with(|clock| clock.set(Some(base)));
    MEMORY_CLOCK.with(|clock| clock.set(Some(0)));
    let shared = Shared::with_clocks(memory_instant, memory_clock);
    let mut model = Vitals::new();
    let mut input = Sample {
        at_ns: 1_000_000_000,
        at_unix_ms: 1_760_000_001_000,
        mono_ms: 1000,
        cores: Some(vec![Ticks {
            busy: 20,
            total: 100,
        }]),
        mem: Some(MemSample {
            total: 8 << 30,
            app: 1 << 30,
            wired: 1 << 30,
            free: 6 << 30,
            available: 6 << 30,
            pressure: Pressure::Normal,
            exact: true,
            ..MemSample::default()
        }),
        ..Sample::default()
    };
    shared.publish_for_test(model.absorb(&input), Vec::new());
    let ordinary = ask(&shared, "mem");
    let cursor = cursor_of(&ordinary);
    assert_eq!(
        shared.lock().replies.len(),
        1,
        "ordinary replies keep a real delta base"
    );
    for step in 2..=5 {
        model.record_strip_event(StripEvent {
            at_unix_ms: input.at_unix_ms,
            kind: StripEventKind::Restart,
            label: format!("uncached-marker-{step}"),
            scope: EventScope::Machine,
            span: None,
        });
        input.at_ns += 1_000_000_000;
        input.at_unix_ms += 1000;
        input.mono_ms += 1000;
        input.cores = Some(vec![Ticks {
            busy: step * 20,
            total: step * 100,
        }]);
        shared.publish_for_test(model.absorb(&input), Vec::new());
        for request in [
            "history cpu mem",
            "history cpu mem --json",
            "events",
            "events --json",
        ] {
            let reply = ask(&shared, request);
            assert!(reply.starts_with("OK "), "{request}: {reply}");
            if request.starts_with("history") {
                if request.ends_with("--json") {
                    let body: aterm_json::Value =
                        aterm_json::from_str(reply.lines().nth(1).unwrap()).unwrap();
                    let rows = body["history"]["rows"].as_array().unwrap();
                    assert_eq!(rows.len(), 2);
                    assert!(
                        rows.iter().any(|row| row["boxes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|cell| cell["primary"]["mean"].as_f64().is_some())),
                        "history contains actual retained measurements"
                    );
                } else {
                    assert!(
                        reply.contains("history row=mem") && reply.contains("box row=cpu"),
                        "{reply}"
                    );
                }
            } else {
                assert!(
                    reply.contains(&format!("uncached-marker-{step}")),
                    "events remain usable: {reply}"
                );
            }
            assert_eq!(
                shared.lock().replies.len(),
                1,
                "{request} has no reply-delta consumer"
            );
        }
    }
    let delta = ask(&shared, &format!("mem since={cursor}"));
    assert!(
        delta.lines().next().unwrap().ends_with(" delta=1"),
        "the original normal-view base remains usable: {delta}"
    );
    assert!(
        delta.lines().any(|line| line.starts_with("same ")),
        "real unchanged measurements still fold: {delta}"
    );
    assert!(!delta.contains("gap why="), "{delta}");
    MEMORY_INSTANT.with(|clock| clock.set(None));
    MEMORY_CLOCK.with(|clock| clock.set(None));
}

#[test]
fn a_why_since_cursor_folds_only_against_the_same_reply() {
    let shared = Shared::with_clock(|| Some(0));
    shared.publish_for_test(synthetic(3, 0.1), Vec::new());
    let other = |reply: &str| reply.lines().any(|l| l == "gap why=other-request");
    let mem = cursor_of(&ask(&shared, "why mem"));
    let cpu = ask(&shared, &format!("why cpu since={mem}"));
    assert!(other(&cpu), "{cpu}");
    let bare = cursor_of(&ask(&shared, "why"));
    let top = ask(&shared, &format!("why top=8 since={bare}"));
    assert!(other(&top), "{top}");
    let again = ask(&shared, &format!("why since={bare}"));
    assert!(
        again.lines().next().unwrap().ends_with(" delta=1"),
        "{again}"
    );
    assert!(!other(&again), "{again}");
    let procs = cursor_of(&ask(&shared, "procs top=8"));
    let folded = ask(&shared, &format!("procs since={procs}"));
    assert!(
        folded.lines().next().unwrap().ends_with(" delta=1"),
        "{folded}"
    );
}

#[test]
fn events_since_a_cursor_print_and_the_ring_says_when_it_lost_them() {
    let shared = Shared::with_clock(|| Some(0));
    let mut s = synthetic(2, 0.1);
    s.events = vec![aterm_vitals::events::Event {
        seq: 2,
        at_ns: s.at_ns,
        at_unix_ms: Some(s.at_unix_ms),
        scope: aterm_vitals::events::EventScope::Machine,
        kind: aterm_vitals::events::EventKind::Pressure {
            from: Pressure::Normal,
            to: Pressure::Warn,
        },
    }];
    shared.publish_for_test(synthetic(1, 0.1), Vec::new());
    let before = cursor_of(&ask(&shared, "overview"));
    shared.publish_for_test(s, Vec::new());
    let after = ask(&shared, &format!("overview since={before}"));
    assert!(
        after
            .lines()
            .any(|l| l.starts_with("e seq=2 ") && l.contains("kind=pressure from=normal to=warn")),
        "{after}"
    );
    let (events, covered) = shared.events_since(1);
    assert_eq!(events.len(), 1);
    assert!(covered);
    for seq in 3..(3 + EVENTS_CAP as u64 + 1) {
        let mut s = synthetic(seq, 0.1);
        s.events = vec![aterm_vitals::events::Event {
            seq,
            at_ns: s.at_ns,
            at_unix_ms: Some(s.at_unix_ms),
            scope: aterm_vitals::events::EventScope::Machine,
            kind: aterm_vitals::events::EventKind::Lane {
                lane: "gpu",
                available: seq % 2 == 0,
            },
        }];
        shared.publish_for_test(s, Vec::new());
    }
    let (_, covered) = shared.events_since(1);
    assert!(!covered, "the oldest events left the ring");
}

#[test]
fn sys_events_cursor_is_one_atomic_retained_cut_without_reply_or_same_sequence_loss() {
    use aterm_vitals::events::{Event, EventKind, EventScope};
    use aterm_vitals::model::{Sample, StripEvent, StripEventKind, Vitals};
    let shared = Shared::with_clock(|| Some(0));
    let mut model = Vitals::new();
    let mut input = Sample {
        at_ns: 1_000_000_000,
        at_unix_ms: 1_760_000_001_000,
        mono_ms: 1000,
        ..Sample::default()
    };
    let first = model.absorb(&input);
    shared.publish_for_test(first.clone(), Vec::new());
    let cursor = Cursor {
        epoch: *EPOCH,
        seq: first.seq,
    };
    for i in 0..20 {
        model.record_strip_event(StripEvent {
            at_unix_ms: input.at_unix_ms + i,
            kind: StripEventKind::Restart,
            label: format!("batch-{i}"),
            scope: EventScope::Machine,
            span: None,
        });
    }
    input.at_ns += 1_000_000_000;
    input.mono_ms += 1000;
    input.at_unix_ms -= 100_000;
    let batch = model.absorb(&input);
    assert_eq!(batch.events.len(), 20, "accepted source batch");
    shared.publish_for_test(batch.clone(), Vec::new());
    let cut = shared.event_cut(Some(cursor), *EPOCH).unwrap();
    assert_eq!(cut.head.seq, batch.seq);
    assert_eq!(cut.held.as_ref().unwrap().snap.seq, batch.seq);
    assert_eq!(cut.events.len(), 20);
    assert_eq!(cut.gap, None);
    assert!(cut.events.iter().all(|event| event.seq == batch.seq));
    let text = ask(&shared, &format!("events since={cursor}"));
    assert_eq!(
        text.lines().filter(|line| line.starts_with("e ")).count(),
        20,
        "{text}"
    );
    assert!(
        !text.contains("gap why="),
        "no remembered reply is required: {text}"
    );
    let json = ask(&shared, &format!("events since={cursor} --json"));
    let parsed: aterm_json::Value = aterm_json::from_str(json.lines().nth(1).unwrap()).unwrap();
    let events = parsed["events"].as_array().unwrap();
    assert_eq!(
        events.len(),
        20,
        "no top cut may discard same-sequence events: {json}"
    );
    for i in 0..20 {
        assert!(
            events
                .iter()
                .any(|event| event["label"].as_str() == Some(format!("batch-{i}").as_str()))
        );
    }
    let head = cut.head;
    shared.publish_for_test(batch.clone(), Vec::new());
    let repeat = shared.event_cut(Some(head), *EPOCH).unwrap();
    assert_eq!(repeat.head, head);
    assert!(repeat.events.is_empty());
    assert_eq!(repeat.gap, None, "identical publication is idempotent");
    let mut invalid = batch.clone();
    invalid.events[0] = Event {
        seq: head.seq,
        at_ns: batch.at_ns,
        at_unix_ms: Some(input.at_unix_ms),
        scope: EventScope::Machine,
        kind: EventKind::Lane {
            lane: "late-new-event",
            available: false,
        },
    };
    shared.publish_for_test(invalid, Vec::new());
    let rejected = shared.event_cut(Some(head), *EPOCH).unwrap();
    assert!(rejected.events.is_empty());
    assert_eq!(
        rejected.gap,
        Some(Gap::Evicted),
        "a later different same-sequence batch cannot silently disappear behind a successful cursor"
    );
    assert!(!ask(&shared, "events --json").contains("late-new-event"));
    let next_seq = head.seq + 1;
    let mut next = synthetic(next_seq, 0.1);
    next.events = vec![Event {
        seq: next_seq,
        at_ns: next.at_ns,
        at_unix_ms: Some(42),
        scope: EventScope::Machine,
        kind: EventKind::Lane {
            lane: "gpu",
            available: true,
        },
    }];
    shared.publish_for_test(next.clone(), Vec::new());
    let recovery = shared.event_cut(Some(head), *EPOCH).unwrap();
    assert_eq!(recovery.events.len(), 1);
    assert_eq!(recovery.events[0].at_unix_ms, Some(42));
    assert_eq!(recovery.gap, Some(Gap::Evicted));
    assert_eq!(
        shared.event_cut(Some(recovery.head), *EPOCH).unwrap().gap,
        None
    );
    let reset = shared
        .event_cut(
            Some(Cursor {
                epoch: EPOCH.wrapping_add(1),
                seq: u64::MAX,
            }),
            *EPOCH,
        )
        .unwrap();
    assert_eq!(reset.gap, Some(Gap::Reset));
    assert_eq!(reset.events.len(), 21);
    assert!(
        shared
            .event_cut(
                Some(Cursor {
                    epoch: *EPOCH,
                    seq: next_seq + 1
                }),
                *EPOCH
            )
            .is_err()
    );
    next.seq += 1;
    next.events.clear();
    next.events_lost = 1;
    shared.publish_for_test(next, Vec::new());
    assert_eq!(
        shared.event_cut(Some(recovery.head), *EPOCH).unwrap().gap,
        Some(Gap::Evicted)
    );
}

#[test]
fn sys_events_reads_retained_sources_without_pulling_on_empty_stale_or_unread_clocks() {
    use aterm_vitals::events::EventScope;
    use aterm_vitals::model::{Sample, StripEvent, StripEventKind, Vitals};
    let base = Instant::now();
    MEMORY_INSTANT.with(|clock| clock.set(Some(base)));
    MEMORY_CLOCK.with(|clock| clock.set(Some(0)));
    let shared = Shared::with_clocks(memory_instant, memory_clock);
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    shared.lock().tx = Some(tx);
    let arming = Arming::new();
    let read = |request: &str, expected| {
        let q = SysQuery::parse(request).unwrap();
        let (held, state) = shared.fresh_for(&q, &arming, &arming, &arming, &arming, &arming);
        assert_eq!(state, expected, "{request}");
        let reply = answer(
            &shared,
            &q,
            held.as_ref(),
            state,
            &Machine::default(),
            &gathered(),
            true,
        );
        assert!(
            matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
            "reading Events must send no pull: {request}"
        );
        assert!(!arming.armed());
        (held, reply)
    };
    let (empty, text) = read("events fresh=0", Freshness::Warming);
    assert!(empty.is_none());
    assert!(
        text.contains("events retained=0") && text.contains("state=warming"),
        "{text}"
    );
    let (_, json) = read("events fresh=0 --json", Freshness::Warming);
    let body: aterm_json::Value = aterm_json::from_str(json.lines().nth(1).unwrap()).unwrap();
    assert!(body["events"].as_array().unwrap().is_empty());
    assert!(json.lines().next().unwrap().contains("age_ms=-"));
    let mut model = Vitals::new();
    model.record_strip_event(StripEvent {
        at_unix_ms: 1_760_000_000_000,
        kind: StripEventKind::Restart,
        label: "retained-real-marker".into(),
        scope: EventScope::Machine,
        span: None,
    });
    let snapshot = model.absorb(&Sample {
        at_ns: 1_000_000_000,
        at_unix_ms: 1_760_000_001_000,
        mono_ms: 1000,
        machine_only: true,
        ..Sample::default()
    });
    assert_eq!(snapshot.events.len(), 1);
    MEMORY_CLOCK.with(|clock| clock.set(Some(5_000_000_000)));
    shared.publish_for_test(snapshot.clone(), Vec::new());
    let (fresh, text) = read("events fresh=0", Freshness::Fresh);
    assert_eq!(fresh.unwrap().snap.seq, snapshot.seq);
    assert!(text.contains("retained-real-marker"), "{text}");
    MEMORY_INSTANT.with(|clock| clock.set(Some(base + Duration::from_secs(11))));
    MEMORY_CLOCK.with(|clock| clock.set(Some(16_000_000_000)));
    for request in ["events", "events fresh=0", "events fresh=0 --json"] {
        let (old, reply) = read(request, Freshness::Stale);
        assert_eq!(
            old.unwrap().observed_at.instant,
            base,
            "no read can renew the original frame"
        );
        assert!(
            reply.contains("retained-real-marker") && reply.contains("age_ms=11000"),
            "{reply}"
        );
    }
    MEMORY_CLOCK.with(|clock| clock.set(None));
    let (old, reply) = read("events fresh=0 --json", Freshness::Stale);
    assert_eq!(old.unwrap().snap.seq, snapshot.seq);
    assert!(
        reply.contains("retained-real-marker"),
        "missing clock cannot force a source read: {reply}"
    );
    assert!(
        !reply.starts_with("ERR"),
        "retained original event timestamps remain readable: {reply}"
    );
    assert!(
        reply.lines().next().unwrap().contains("age_ms=-"),
        "unknown inclusive age is not an awake-time substitute: {reply}"
    );
    let body: aterm_json::Value = aterm_json::from_str(reply.lines().nth(1).unwrap()).unwrap();
    assert_eq!(
        body["as_of"]["unix_ms"].as_u64(),
        Some(snapshot.at_unix_ms as u64)
    );
    assert_eq!(
        body["as_of"]["age_ms"]["why"].as_str(),
        Some("not_sampled:observation_age")
    );
    for continuous in [None, Some(4_000_000_000)] {
        MEMORY_CLOCK.with(|clock| clock.set(continuous));
        let (_, text) = read("events fresh=0", Freshness::Stale);
        assert!(
            text.contains("retained-real-marker") && text.contains("age_ms=-"),
            "{text}"
        );
        assert!(
            text.contains("observation age is unavailable(not_sampled:observation_age)"),
            "{text}"
        );
        assert!(
            text.contains(
                "onset_unix_ms=unavailable(not_sampled) end_unix_ms=unavailable(not_sampled)"
            ),
            "point intervals stay unavailable: {text}"
        );
    }
    MEMORY_CLOCK.with(|clock| clock.set(Some(16_000_000_000)));
    let (_, recovered) = read("events --json", Freshness::Stale);
    let body: aterm_json::Value = aterm_json::from_str(recovered.lines().nth(1).unwrap()).unwrap();
    assert_eq!(body["as_of"]["age_ms"].as_u64(), Some(11_000));
    assert_eq!(
        body["as_of"]["unix_ms"].as_u64(),
        Some(snapshot.at_unix_ms as u64)
    );
    MEMORY_INSTANT.with(|clock| clock.set(None));
    MEMORY_CLOCK.with(|clock| clock.set(None));
}

#[test]
fn sys_history_uses_new_machine_only_rings_without_renewing_a_process_frame() {
    use aterm_vitals::model::{Resource, Sample, Ticks, Vitals};
    let shared = Shared::with_clock(|| Some(0));
    let mut model = Vitals::new();
    let mut input = Sample {
        at_ns: 1_000_000_000,
        at_unix_ms: 1_760_000_001_000,
        mono_ms: 1000,
        cadence_ms: 1000,
        cores: Some(vec![Ticks {
            busy: 10,
            total: 100,
        }]),
        mem: Some(MemSample {
            total: 8 << 30,
            app: 1 << 30,
            wired: 1 << 30,
            free: 6 << 30,
            available: 6 << 30,
            pressure: Pressure::Normal,
            exact: true,
            ..MemSample::default()
        }),
        ..Sample::default()
    };
    let mut whole = model.absorb(&input);
    // Populate earlier boxes from original readings before the pause.
    for step in 2..=40 {
        input.at_ns += 1_000_000_000;
        input.at_unix_ms += 1000;
        input.mono_ms += 1000;
        input.cores = Some(vec![Ticks {
            busy: step * 20 - 10,
            total: step * 100,
        }]);
        whole = model.absorb(&input);
    }
    let old_cpu = whole.resources[Resource::Cpu.index()]
        .now
        .expect("the declared one-second cadence admits real strip CPU rates");
    assert!(old_cpu > 0.0);
    let old_slots = whole.resources[Resource::Cpu.index()].history_slots(
        aterm_vitals::MultiScaleSpec::FULL_6H,
        whole.at_unix_ms,
        &whole.time_gaps,
    );
    assert!(
        old_slots.cells.iter().any(|cell| {
            !cell.primary.slot.is_live_now && cell.primary.cell.mean.is_some_and(|cpu| cpu > 0.0)
        }),
        "fixture setup must populate a positive canonical historical CPU box"
    );
    shared.publish_for_test(whole.clone(), Vec::new());
    input.at_ns += 20_000_000_000;
    input.at_unix_ms += 20_000;
    input.mono_ms += 20_000;
    input.machine_only = true;
    input.cores = None;
    let mem = input.mem.as_mut().unwrap();
    mem.app = 2 << 30;
    mem.free = 5 << 30;
    mem.available = 5 << 30;
    let machine_only = model.absorb(&input);
    assert!(machine_only.time_gaps.iter().any(|gap| {
        gap.kind == aterm_vitals::GapKind::Paused
            && gap.start_ms == whole.at_unix_ms
            && gap.end_ms == machine_only.at_unix_ms
    }));
    assert!(
        machine_only.cpu.is_none(),
        "current unread CPU is not a carried live reading"
    );
    let slots = machine_only.resources[Resource::Cpu.index()].history_slots(
        aterm_vitals::MultiScaleSpec::FULL_6H,
        machine_only.at_unix_ms,
        &machine_only.time_gaps,
    );
    assert!(
        slots.cells[0].primary.cell.mean.is_none(),
        "old point is past the strip's original support"
    );
    assert!(
        slots
            .cells
            .iter()
            .any(|cell| cell.primary.cell.mean.is_some_and(|cpu| cpu > 0.0)),
        "retained earlier history remains readable"
    );
    let before_pause = slots
        .cells
        .iter()
        .find(|cell| cell.primary.slot.age_lo_ms == 20_000 && cell.primary.slot.age_hi_ms == 30_000)
        .expect("the canonical ten-second box immediately before the pause");
    assert_eq!(before_pause.primary.gap, None);
    assert!(
        before_pause
            .primary
            .cell
            .mean
            .is_some_and(|cpu| (cpu - old_cpu).abs() < 1e-6),
        "the earlier box keeps the original positive CPU reading"
    );
    let memory = &machine_only.resources[Resource::Memory.index()];
    assert!(
        memory.now.unwrap() > whole.resources[Resource::Memory.index()].now.unwrap(),
        "the current machine-only memory reading differs from the held whole frame"
    );
    let memory_slots = memory.history_slots(
        aterm_vitals::MultiScaleSpec::FULL_6H,
        machine_only.at_unix_ms,
        &machine_only.time_gaps,
    );
    assert_eq!(memory_slots.cells[0].primary.cell.mean, memory.now);
    shared.publish_for_test(machine_only.clone(), Vec::new());
    let q = SysQuery::parse("history cpu mem --json").unwrap();
    let arming = Arming::new();
    let (held, state) = shared.fresh_for(&q, &arming, &arming, &arming, &arming, &arming);
    let held = held.unwrap();
    assert_eq!(held.snap.seq, machine_only.seq);
    assert_eq!(held.snap.at_unix_ms, machine_only.at_unix_ms);
    assert_eq!(held.snap.resources, machine_only.resources);
    assert_eq!(
        shared.held_now().unwrap().snap.seq,
        whole.seq,
        "whole-process authority is unchanged"
    );
    let reply = answer(
        &shared,
        &q,
        Some(&held),
        state,
        &Machine::default(),
        &gathered(),
        true,
    );
    let expected = render_json(
        &q,
        &Ctx {
            snap: Some(&machine_only),
            as_of_unix_ms: held.unix_ms,
            age_ms: Some(0),
            epoch: *EPOCH,
            state,
            machine: &Machine::default(),
            target: &gathered().target,
            tabs: &[],
            caller_panes: None,
            event_caller: None,
            owner_class: true,
            volume: None,
            roots: &[],
            disk: None,
            stores: &[],
            home: None,
            events: &[],
            gap: None,
            head_seq: machine_only.seq,
            held: None,
        },
    );
    assert!(
        reply.contains(&expected.get("history").unwrap().to_text()),
        "latest canonical body: {reply}"
    );
    assert_eq!(
        shared
            .event_cut(None, *EPOCH)
            .unwrap()
            .held
            .unwrap()
            .snap
            .seq,
        machine_only.seq
    );
}

#[test]
fn sys_events_admits_only_the_live_grant_without_process_receipt_or_caller_spoofing() {
    use aterm_vitals::events::{EventScope, SessionScope};
    use aterm_vitals::model::{Sample, StripEvent, StripEventKind, Vitals};
    let store = crate::session_store::new_store();
    let own = crate::session_store::test_handle(71);
    let sibling = crate::session_store::test_handle(72);
    let foreign = crate::session_store::test_handle(73);
    for handle in [&own, &sibling, &foreign] {
        store.write().unwrap().register(handle.clone());
    }
    let layout = vec![
        PaneFact {
            key: own.local_id,
            window: 1,
            tab: 1,
            ..PaneFact::default()
        },
        PaneFact {
            key: sibling.local_id,
            window: 1,
            tab: 1,
            ..PaneFact::default()
        },
        PaneFact {
            key: foreign.local_id,
            window: 1,
            tab: 2,
            ..PaneFact::default()
        },
    ];
    let mut model = Vitals::new();
    for (handle, label) in [
        (&own, "own-private-marker"),
        (&sibling, "sibling-private-marker"),
        (&foreign, "foreign-private-marker"),
    ] {
        model.record_strip_event(StripEvent {
            at_unix_ms: 1_760_000_000_000,
            kind: StripEventKind::Restart,
            label: label.into(),
            scope: EventScope::Session(SessionScope {
                local: handle.local_id,
                sid: handle.sid.as_str().into(),
                nonce: handle.nonce.to_hex(),
            }),
            span: None,
        });
    }
    model.record_strip_event(StripEvent {
        at_unix_ms: 1_760_000_000_000,
        kind: StripEventKind::Restart,
        label: "machine-marker".into(),
        scope: EventScope::Machine,
        span: None,
    });
    let snapshot = model.absorb(&Sample {
        at_ns: 1_000_000_000,
        at_unix_ms: 1_760_000_001_000,
        mono_ms: 1000,
        machine_only: true,
        ..Sample::default()
    });
    let shared = Shared::with_clock(|| Some(0));
    shared.publish_for_test(snapshot.clone(), layout.clone());
    let q = SysQuery::parse(&format!("events --json caller={}", foreign.sid.as_str())).unwrap();
    let admitted = AuthorizedTarget {
        session: own.local_id,
        term: &own.term,
        master: -1,
        ctx: &own.ctx,
    };
    let nonowner = gather(&store, &admitted, &layout, &q, Some(&snapshot), None, false).unwrap();
    assert_eq!(
        nonowner.caller_panes.as_deref(),
        Some([own.local_id].as_slice())
    );
    assert!(
        nonowner.tabs.iter().all(|tab| tab.sampled_nonce.is_none()),
        "no process receipt exists"
    );
    assert_eq!(
        nonowner.event_caller.as_ref().unwrap().nonce,
        own.nonce.to_hex()
    );
    let arming = Arming::new();
    let (held, state) = shared.fresh_for(&q, &arming, &arming, &arming, &arming, &arming);
    let reply = answer(
        &shared,
        &q,
        held.as_ref(),
        state,
        &Machine::default(),
        &nonowner,
        false,
    );
    assert!(
        reply.contains("own-private-marker") && reply.contains("machine-marker"),
        "{reply}"
    );
    assert!(
        !reply.contains("sibling-private-marker") && !reply.contains("foreign-private-marker"),
        "{reply}"
    );
    assert!(
        !reply.contains(&own.nonce.to_hex()),
        "internal proof must not be serialized: {reply}"
    );
    let owner = gather(&store, &admitted, &layout, &q, Some(&snapshot), None, true).unwrap();
    let wide = answer(
        &shared,
        &q,
        held.as_ref(),
        state,
        &Machine::default(),
        &owner,
        true,
    );
    assert!(
        wide.contains("sibling-private-marker") && wide.contains("foreign-private-marker"),
        "owner positive: {wide}"
    );
}

#[test]
fn a_pull_asks_the_probe_once_per_gap_and_takes_its_reading() {
    let shared = Arc::new(Shared::paced(
        Duration::from_secs(3600),
        Duration::from_secs(60),
    ));
    let (held, state) = shared.fresh(Duration::ZERO, true);
    assert!(held.is_none());
    assert_eq!(state, Freshness::Warming);
    shared.publish_for_test(synthetic(1, 0.1), Vec::new());
    let (held, state) = shared.fresh(Duration::ZERO, true);
    assert_eq!(held.map(|h| h.snap.seq), Some(1));
    assert_eq!(state, Freshness::Stale);
    let (tx, rx) = std::sync::mpsc::sync_channel(2);
    shared.install(tx);
    let probe = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut seq = 1;
            let mut pulls = 0;
            while let Ok(msg) = rx.recv() {
                assert!(matches!(msg, Msg::Pull));
                pulls += 1;
                seq += 1;
                shared.publish_for_test(synthetic(seq, 0.1), Vec::new());
            }
            pulls
        })
    };
    let (held, state) = shared.fresh(Duration::ZERO, true);
    assert_eq!(held.map(|h| h.snap.seq), Some(2));
    assert_eq!(state, Freshness::Fresh);
    let (held, _) = shared.fresh(Duration::ZERO, true);
    assert_eq!(held.map(|h| h.snap.seq), Some(2));
    let (held, _) = shared.fresh(Duration::from_secs(60), false);
    assert_eq!(held.map(|h| h.snap.seq), Some(2));
    shared.lock().tx = None;
    assert_eq!(probe.join().unwrap(), 1, "one pull for three reads");
}

/// PRIVACY'S COUNT PULLS A READING WHEN NONE SPEAKS FOR NOW (leftover §9): with no jobs view
/// held, the count asks the probe for one and answers from it; with no probe running it answers
/// nothing.
#[test]
fn privacys_count_pulls_a_reading_when_none_speaks_for_now() {
    use aterm_vitals::jobs::{Findings, JobsView, Named};
    let shared = Arc::new(Shared::paced(
        Duration::from_secs(3600),
        Duration::from_secs(60),
    ));
    assert_eq!(shared.program_check_now(), None, "no probe: nothing read");
    let (tx, rx) = std::sync::mpsc::sync_channel(2);
    shared.install(tx);
    let probe = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut pulls = 0;
            while let Ok(msg) = rx.recv() {
                assert!(matches!(msg, Msg::Pull));
                pulls += 1;
                let mut snap = synthetic(1 + pulls, 0.1);
                snap.jobs = Some(Arc::new(JobsView {
                    findings: Findings {
                        program_check: vec![Named {
                            pid: 25311,
                            exec_version: None,
                            name: "gh".into(),
                            tab: None,
                            job: None,
                            for_ms: 35_940_000,
                        }],
                        ..Findings::default()
                    },
                    ..JobsView::default()
                }));
                shared.publish_for_test(snap, Vec::new());
            }
            pulls
        })
    };
    let count = shared
        .program_check_now()
        .expect("the pulled reading's count");
    assert_eq!(count.held, 1);
    assert!(count.oldest_ms >= 35_940_000, "{count:?}");
    assert_eq!(
        shared.program_check_now().map(|c| c.held),
        Some(1),
        "the reading now held speaks for now"
    );
    shared.lock().tx = None;
    assert_eq!(probe.join().unwrap(), 1, "one pull");
}

fn with_split(seq: u64, split: Option<u64>) -> Snapshot {
    let mut s = synthetic(seq, 0.1);
    s.coal_lane = aterm_vitals::explain::CoalLane::On;
    s.why = split.map(|at| {
        Arc::new(aterm_vitals::explain::WhyFacts {
            at_ns: at * 1_000_000_000,
            window_ns: 5_000_000_000,
            lane: aterm_vitals::explain::CoalLane::On,
            ..aterm_vitals::explain::WhyFacts::default()
        })
    });
    s
}

#[test]
fn a_why_is_as_fresh_as_its_split() {
    let probe = |shared: &Arc<Shared>, moves_from: u64| {
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        shared.install(tx);
        let shared = Arc::clone(shared);
        std::thread::spawn(move || {
            let mut pulls = 0;
            while let Ok(msg) = rx.recv() {
                assert!(matches!(msg, Msg::Pull));
                pulls += 1;
                let seq = 7300 + pulls;
                let split = if pulls >= moves_from { seq } else { 1 };
                shared.publish_for_test(with_split(seq, Some(split)), Vec::new());
            }
            pulls
        })
    };
    let hour = Duration::from_secs(3600);
    let shared = Arc::new(Shared::paced(hour, Duration::from_secs(60)));
    let pulls = probe(&shared, 1);
    shared.publish_for_test(with_split(1, Some(1)), Vec::new());
    // Judged by its split alone, as a `sys why mem` is, which prints no self CPU line.
    let (held, state) = shared.fresh_why(hour, false, false);
    assert_eq!(
        (held.map(|h| h.snap.seq), state),
        (Some(1), Freshness::Fresh)
    );
    shared.publish_for_test(with_split(7300, Some(1)), Vec::new());
    let (held, _) = shared.fresh(hour, false);
    assert_eq!(held.map(|h| h.snap.seq), Some(7300));
    let (held, state) = shared.fresh_why(hour, false, false);
    assert_eq!(
        (held.map(|h| h.snap.seq), state),
        (Some(7301), Freshness::Fresh)
    );
    shared.lock().tx = None;
    assert_eq!(pulls.join().unwrap(), 1);

    let shared = Arc::new(Shared::paced(hour, Duration::from_secs(60)));
    let pulls = probe(&shared, 2);
    shared.publish_for_test(with_split(7300, Some(1)), Vec::new());
    let (held, _) = shared.fresh(Duration::ZERO, true);
    assert_eq!(held.map(|h| h.snap.seq), Some(7301));
    let (held, state) = shared.fresh_why(hour, false, false);
    assert_eq!(
        (held.map(|h| h.snap.seq), state),
        (Some(7302), Freshness::Fresh)
    );
    shared.lock().tx = None;
    assert_eq!(pulls.join().unwrap(), 2);

    let shared = Arc::new(Shared::paced(hour, Duration::from_millis(200)));
    let pulls = probe(&shared, u64::MAX);
    shared.publish_for_test(with_split(7300, Some(1)), Vec::new());
    let (held, state) = shared.fresh_why(hour, false, false);
    assert!(held.is_some_and(|h| h.snap.seq >= 7300));
    assert_eq!(state, Freshness::Stale);
    shared.lock().tx = None;
    assert_eq!(pulls.join().unwrap(), 1);
    let mut off = synthetic(7400, 0.1);
    off.coal_lane = aterm_vitals::explain::CoalLane::Off;
    shared.publish_for_test(off, Vec::new());
    let (held, state) = shared.fresh_why(hour, false, false);
    assert_eq!(
        (held.map(|h| h.snap.seq), state),
        (Some(7400), Freshness::Fresh)
    );
}

#[test]
fn a_why_arms_the_split_while_it_waits() {
    static ARMED: Arming = Arming::new();
    static SCHEDULING: Arming = Arming::new();
    static SELF_CPU: Arming = Arming::new();
    static AT: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    fn instant() -> Instant {
        *AT.get().unwrap()
    }
    fn publish(shared: &Shared, snapshot: Snapshot) {
        let observed_at = ObservedAt::capture(instant, || Some(0));
        // This independent raw census is paired with the frame even when no row is needed
        // by the arming assertion. The worker's helper clock is explicitly inert and known.
        let previous = MEMORY_CLOCK.with(|clock| clock.replace(Some(0)));
        let receipt = memory_receipt(&snapshot, observed_at);
        MEMORY_CLOCK.with(|clock| clock.set(previous));
        shared.publish(
            Arc::new(snapshot),
            &Machine::default(),
            observed_at,
            None,
            None,
            true,
            Some(receipt),
        );
    }
    let _ = AT.set(Instant::now());
    for (text, armed) in [
        ("why fresh=0", true),
        ("global fresh=0", true),
        ("aterm fresh=0", true),
        ("tab fresh=0", true),
        ("mem fresh=0", true),
        ("fresh=0", true),
        ("procs fresh=0", false),
    ] {
        let shared = Arc::new(Shared::paced_with_clocks(
            Duration::from_secs(3600),
            Duration::from_secs(60),
            instant,
            || Some(0),
        ));
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        shared.install(tx);
        let probe = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                while let Ok(msg) = rx.recv() {
                    assert!(matches!(msg, Msg::Pull));
                    seen.push(ARMED.armed());
                    // A forced `sys why` waits for a CPU Stats sample too.
                    let mut snap = with_split(7301, Some(7301));
                    snap.hardware = Some(aterm_vitals::hardware::Sample::unavailable("warming"));
                    // A `sys procs` waits for a reading whose sweep read scheduling too.
                    snap.scheduling_read = SCHEDULING.armed();
                    // A `sys why` waits for one that read aterm's own threads too.
                    snap.self_cpu_read = SELF_CPU.armed();
                    publish(&shared, snap);
                }
                seen
            })
        };
        publish(&shared, with_split(7300, Some(1)));
        let q = SysQuery::parse(text).expect("query");
        let (held, state) = shared.fresh_for(
            &q,
            &ARMED,
            &Arming::new(),
            &Arming::new(),
            &SCHEDULING,
            &SELF_CPU,
        );
        assert_eq!(
            (held.map(|h| h.snap.seq), state),
            (Some(7301), Freshness::Fresh),
            "{text}"
        );
        assert!(!ARMED.armed(), "{text}");
        shared.lock().tx = None;
        assert_eq!(probe.join().unwrap(), [armed], "{text}");
    }
}

/// A snapshot with a CPU Stats sample where `sampled`.
fn with_hardware(seq: u64, sampled: bool) -> Snapshot {
    let mut s = synthetic(seq, 0.1);
    s.hardware = sampled.then(|| aterm_vitals::hardware::Sample::unavailable("warming"));
    s
}

/// `sys cores`, `sys report` and a forced `sys why` print CPU hardware, which no deck reading
/// samples any more (A2 §6.4): over a held snapshot without a sample they pull, whatever its age
/// or their `fresh=`, with CPU Stats armed while they wait, and answer from the pull's reading,
/// never from a deck reading without a sample that lands first; over one with a sample, the two
/// unforced views answer at once. An unforced `sys why` pulls for no sample; every other view
/// arms nothing. NEGATIVE CONTROL: judge `sys cores` by the snapshot's landing, as the host before
/// A2 did, and it answers the held snapshot at once, fresh, with no pull and no hardware; judge a
/// forced `sys why` by its split alone, as the S2 commit did, and it answers the deck reading
/// (7301), which has none.
#[test]
fn sys_cores_and_report_force_a_pull_when_the_held_snapshot_has_no_hardware() {
    static HARDWARE: Arming = Arming::new();
    static SCHEDULING: Arming = Arming::new();
    static SELF_CPU: Arming = Arming::new();
    let run = |text: &str, held_sampled: bool| {
        let shared = Arc::new(Shared::paced(
            Duration::from_secs(3600),
            Duration::from_secs(60),
        ));
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        shared.install(tx);
        let probe = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                let mut seq = 7300;
                while let Ok(msg) = rx.recv() {
                    assert!(matches!(msg, Msg::Pull));
                    let armed = HARDWARE.armed();
                    seen.push(armed);
                    if armed {
                        // A deck reading already in flight lands first, with no sample.
                        seq += 1;
                        shared.publish_for_test(with_hardware(seq, false), Vec::new());
                    }
                    seq += 1;
                    // A `sys report`'s pull reads each process's scheduling too, and a
                    // `sys why`'s aterm's own threads.
                    let mut snap = with_hardware(seq, armed);
                    snap.scheduling_read = SCHEDULING.armed();
                    snap.self_cpu_read = SELF_CPU.armed();
                    shared.publish_for_test(snap, Vec::new());
                }
                seen
            })
        };
        // A held snapshot with a sample read each process's scheduling too; every held one read
        // aterm's own threads, as an earlier `sys why`'s pull does, so a `sys why` is judged by
        // its CPU Stats need alone.
        let mut held = with_hardware(7300, held_sampled);
        held.scheduling_read = held_sampled;
        held.self_cpu_read = true;
        shared.publish_for_test(held, Vec::new());
        let q = SysQuery::parse(text).expect("query");
        let (held, state) = shared.fresh_for(
            &q,
            &Arming::new(),
            &Arming::new(),
            &HARDWARE,
            &SCHEDULING,
            &SELF_CPU,
        );
        assert!(!HARDWARE.armed(), "{text}: disarmed once answered");
        shared.lock().tx = None;
        let sampled = held.as_ref().is_some_and(|h| h.snap.hardware.is_some());
        (
            held.map(|h| h.snap.seq),
            state,
            sampled,
            probe.join().unwrap(),
        )
    };
    let pulled = (Some(7302), Freshness::Fresh, true, vec![true]);
    for text in ["cores", "report", "cores fresh=60000", "report fresh=60000"] {
        assert_eq!(run(text, false), pulled, "{text}: pulled, CPU Stats armed");
    }
    assert_eq!(run("cores fresh=0", true), pulled, "forced");
    let held = (Some(7300), Freshness::Fresh, true, vec![]);
    assert_eq!(run("cores", true), held, "a held sample answers at once");
    assert_eq!(run("report", true), held);
    let unsampled = (Some(7300), Freshness::Fresh, false, vec![]);
    assert_eq!(run("why", false), unsampled, "a why forces no pull for it");
    assert_eq!(run("global", false), unsampled);
    assert_eq!(
        run("why fresh=0", false),
        pulled,
        "a forced why arms CPU Stats and waits for a sample"
    );
    assert_eq!(
        run("global fresh=0", false),
        (Some(7301), Freshness::Fresh, false, vec![false]),
        "another forced view arms nothing"
    );
}

/// A reply that needs a CPU Stats sample the held snapshot lacks (`sys cores`, `sys report`, a
/// forced `sys why`) waits as a cold pull does, since its pull reads a pair where no sample is
/// warm: allowed no wait at all for a warm pull, each still answers from its pull. CONTROL: a
/// forced `sys global` over the same warm snapshot needs no sample, waits as a warm pull, and so
/// answers stale at once, as the S2 commit's forced `sys why` did.
#[test]
fn a_reply_that_needs_cpu_stats_waits_as_a_cold_pull() {
    static HARDWARE: Arming = Arming::new();
    static SCHEDULING: Arming = Arming::new();
    static SELF_CPU: Arming = Arming::new();
    let run = |text: &str| {
        let shared = Arc::new(Shared::paced(
            Duration::from_secs(3600),
            Duration::from_secs(60),
        ));
        shared.lock().pacing.wait = Duration::ZERO;
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        shared.install(tx);
        let probe = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                while rx.recv().is_ok() {
                    let mut snap = with_hardware(7301, HARDWARE.armed());
                    snap.scheduling_read = SCHEDULING.armed();
                    snap.self_cpu_read = SELF_CPU.armed();
                    shared.publish_for_test(snap, Vec::new());
                }
            })
        };
        shared.publish_for_test(with_hardware(7300, false), Vec::new());
        let q = SysQuery::parse(text).expect("query");
        let (held, state) = shared.fresh_for(
            &q,
            &Arming::new(),
            &Arming::new(),
            &HARDWARE,
            &SCHEDULING,
            &SELF_CPU,
        );
        shared.lock().tx = None;
        probe.join().unwrap();
        (held.map(|h| h.snap.seq), state)
    };
    for text in ["cores", "report", "why fresh=0"] {
        assert_eq!(run(text), (Some(7301), Freshness::Fresh), "{text}");
    }
    assert_eq!(
        run("global fresh=0"),
        (Some(7300), Freshness::Stale),
        "control: a warm pull's wait"
    );
}

/// A snapshot whose sweep read each process's scheduling where `read`.
fn with_scheduling(seq: u64, read: bool) -> Snapshot {
    let mut s = synthetic(seq, 0.1);
    s.scheduling_read = read;
    s
}

/// NO SNAPSHOT WITHOUT SCHEDULING ANSWERS A VIEW THAT PRINTS IT (C4): `sys procs`, `sys report`
/// and the JSON of `sys tab`, `sys global` and `sys mem` over a held snapshot whose sweep read no
/// scheduling pull, whatever its age or their `fresh=`, with the scheduling armed while they
/// wait, and answer from the pull's reading, never from a deck reading without it that lands
/// first; over one that read it, `sys procs` answers at once. A view that prints none (`sys
/// global` in text, `sys cores`) arms nothing for it and answers the held one. NEGATIVE CONTROL:
/// with no scheduling need in `fresh_by`, `sys procs` answers the held snapshot read without it.
#[test]
fn a_view_that_prints_scheduling_answers_only_a_reading_that_read_it() {
    static SCHEDULING: Arming = Arming::new();
    let run = |text: &str, held_read: bool| {
        let shared = Arc::new(Shared::paced(
            Duration::from_secs(3600),
            Duration::from_secs(60),
        ));
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        shared.install(tx);
        let probe = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                let mut seq = 7300;
                while let Ok(msg) = rx.recv() {
                    assert!(matches!(msg, Msg::Pull));
                    let armed = SCHEDULING.armed();
                    seen.push(armed);
                    if armed {
                        // A deck reading already in flight lands first, its sweep asked none.
                        seq += 1;
                        shared.publish_for_test(with_scheduling(seq, false), Vec::new());
                    }
                    seq += 1;
                    let mut snap = with_scheduling(seq, armed);
                    snap.hardware = Some(aterm_vitals::hardware::Sample::unavailable("warming"));
                    shared.publish_for_test(snap, Vec::new());
                }
                seen
            })
        };
        let mut held = with_scheduling(7300, held_read);
        held.hardware = Some(aterm_vitals::hardware::Sample::unavailable("warming"));
        shared.publish_for_test(held, Vec::new());
        let q = SysQuery::parse(text).expect("query");
        let (held, state) = shared.fresh_for(
            &q,
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &SCHEDULING,
            &Arming::new(),
        );
        assert!(!SCHEDULING.armed(), "{text}: disarmed once answered");
        shared.lock().tx = None;
        let read = held.as_ref().is_some_and(|h| h.snap.scheduling_read);
        (held.map(|h| h.snap.seq), state, read, probe.join().unwrap())
    };
    let pulled = (Some(7302), Freshness::Fresh, true, vec![true]);
    for text in [
        "procs",
        "procs fresh=60000",
        "procs --json",
        "report",
        "tab --json",
        "global --json",
        "mem --json",
    ] {
        assert_eq!(run(text, false), pulled, "{text}: pulled, scheduling armed");
    }
    let held = (Some(7300), Freshness::Fresh, true, vec![]);
    assert_eq!(
        run("procs", true),
        held,
        "a reading that read it answers at once"
    );
    let unread = (Some(7300), Freshness::Fresh, false, vec![]);
    assert_eq!(run("global", false), unread, "text global prints none");
    assert_eq!(run("cores", false), unread, "nor do the cores");
}

/// INSIDE ITS GAP A VIEW THAT PRINTS SCHEDULING ANSWERS THE PULL'S READING (C4 review): a
/// `sys procs` pulls and answers from its pull's reading; a deck reading whose sweep read no
/// scheduling then lands, and a second `sys procs` or `sys tab --json` inside that pull's gap
/// answers the pull's reading at once, fresh, sending no pull of its own. NEGATIVE CONTROL: judge
/// it by the newest snapshot (`State::judged` returning `held`), as C4 did, and the second
/// query, allowed no wait, answers the deck reading stale, its scheduling unread.
#[test]
fn inside_its_gap_a_view_that_prints_scheduling_answers_the_pulls_reading() {
    static SCHEDULING: Arming = Arming::new();
    for second in ["procs", "tab --json"] {
        let shared = Arc::new(Shared::paced(
            Duration::from_secs(3600),
            Duration::from_secs(60),
        ));
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        shared.install(tx);
        let probe = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                while let Ok(msg) = rx.recv() {
                    assert!(matches!(msg, Msg::Pull));
                    seen.push(SCHEDULING.armed());
                    shared.publish_for_test(with_scheduling(7301, SCHEDULING.armed()), Vec::new());
                }
                seen
            })
        };
        shared.publish_for_test(with_scheduling(7300, false), Vec::new());
        let ask = |text: &str| {
            let q = SysQuery::parse(text).expect("query");
            let (held, state) = shared.fresh_for(
                &q,
                &Arming::new(),
                &Arming::new(),
                &Arming::new(),
                &SCHEDULING,
                &Arming::new(),
            );
            let read = held.as_ref().is_some_and(|h| h.snap.scheduling_read);
            (held.map(|h| h.snap.seq), state, read)
        };
        assert_eq!(ask("procs"), (Some(7301), Freshness::Fresh, true), "pulled");
        // A deck reading lands inside the pull's gap, its sweep asked for no scheduling.
        shared.publish_for_test(with_scheduling(7302, false), Vec::new());
        {
            let mut g = shared.lock();
            g.pacing.wait = Duration::ZERO;
            g.pacing.wait_cold = Duration::ZERO;
        }
        assert_eq!(
            ask(second),
            (Some(7301), Freshness::Fresh, true),
            "{second}: the pull's reading answers inside its gap"
        );
        shared.lock().tx = None;
        assert_eq!(probe.join().unwrap(), [true], "{second}: one pull, armed");
    }
}

/// A snapshot whose reading read aterm's own thread CPU where `read`.
fn with_self_cpu(seq: u64, read: bool) -> Snapshot {
    let mut s = synthetic(seq, 0.1);
    s.self_cpu_read = read;
    s
}

/// NO SNAPSHOT WITHOUT ATERM'S OWN THREADS ANSWERS A `sys why` THAT PRINTS THEM: a bare `sys
/// why`, `sys why cpu`, `sys why --json` and `sys why fresh=60000` over a held snapshot whose
/// reading read none pull, whatever its age or their `fresh=`, with the read armed while they
/// wait, and answer from the pull's reading, never from a deck reading without it that lands
/// first; over one that read them, a bare `sys why` answers at once; `sys why mem`, which prints
/// none, arms nothing for them and answers the held one. Inside the pull's gap, once a deck
/// reading without them has landed, a second `sys why` answers the pull's reading at once and
/// sends no pull. Where no reader reads aterm's threads (off macOS, `Shared::self_cpu_reads`
/// false), every reading answers why, so a bare `sys why` or `sys why cpu` over a held snapshot
/// whose reading read none answers it at once and sends no pull. NEGATIVE CONTROLS: with no
/// self-CPU need (`Reads::of` never `SelfCpu`), a bare `sys why` answers the held snapshot read
/// without them; with the need wherever `sys why` prints them, whatever reads them, it pulls
/// where no reader reads them.
#[test]
fn a_why_that_prints_self_cpu_answers_only_a_reading_that_read_it() {
    static SELF_CPU: Arming = Arming::new();
    let probe = |shared: &Arc<Shared>, rx: std::sync::mpsc::Receiver<Msg>| {
        let shared = Arc::clone(shared);
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut seq = 7300;
            while let Ok(msg) = rx.recv() {
                assert!(matches!(msg, Msg::Pull));
                let armed = SELF_CPU.armed();
                seen.push(armed);
                if armed {
                    // A deck reading already in flight lands first, reading none.
                    seq += 1;
                    shared.publish_for_test(with_self_cpu(seq, false), Vec::new());
                }
                seq += 1;
                shared.publish_for_test(with_self_cpu(seq, armed), Vec::new());
            }
            seen
        })
    };
    let ask = |shared: &Shared, text: &str| {
        let q = SysQuery::parse(text).expect("query");
        let (held, state) = shared.fresh_for(
            &q,
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &SELF_CPU,
        );
        assert!(!SELF_CPU.armed(), "{text}: disarmed once answered");
        let read = held.as_ref().is_some_and(|h| h.snap.self_cpu_read);
        (held.map(|h| h.snap.seq), state, read)
    };
    let run_where = |text: &str, held_read: bool, self_cpu_reads: bool| {
        let mut shared = Shared::paced(Duration::from_secs(3600), Duration::from_secs(60));
        shared.self_cpu_reads = self_cpu_reads;
        let shared = Arc::new(shared);
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        shared.install(tx);
        let pulls = probe(&shared, rx);
        shared.publish_for_test(with_self_cpu(7300, held_read), Vec::new());
        let answer = ask(&shared, text);
        shared.lock().tx = None;
        (answer, pulls.join().unwrap())
    };
    let run = |text: &str, held_read: bool| run_where(text, held_read, true);
    let pulled = ((Some(7302), Freshness::Fresh, true), vec![true]);
    for text in ["why", "why cpu", "why --json", "why fresh=60000"] {
        assert_eq!(run(text, false), pulled, "{text}: pulled, its read armed");
    }
    assert_eq!(
        run("why", true),
        ((Some(7300), Freshness::Fresh, true), vec![]),
        "a reading that read them answers at once"
    );
    assert_eq!(
        run("why mem", false),
        ((Some(7300), Freshness::Fresh, false), vec![]),
        "why mem prints none"
    );
    for text in ["why", "why cpu"] {
        assert_eq!(
            run_where(text, false, false),
            ((Some(7300), Freshness::Fresh, false), vec![]),
            "{text}: no reader reads them, so the held reading answers"
        );
    }

    let mut shared = Shared::paced(Duration::from_secs(3600), Duration::from_secs(60));
    shared.self_cpu_reads = true;
    let shared = Arc::new(shared);
    let (tx, rx) = std::sync::mpsc::sync_channel(2);
    shared.install(tx);
    let pulls = probe(&shared, rx);
    shared.publish_for_test(with_self_cpu(7300, false), Vec::new());
    assert_eq!(ask(&shared, "why"), pulled.0, "pulled");
    // A deck reading lands inside the pull's gap, reading none.
    shared.publish_for_test(with_self_cpu(7303, false), Vec::new());
    {
        let mut g = shared.lock();
        g.pacing.wait = Duration::ZERO;
        g.pacing.wait_cold = Duration::ZERO;
    }
    assert_eq!(
        ask(&shared, "why"),
        pulled.0,
        "the pull's reading answers inside its gap"
    );
    shared.lock().tx = None;
    assert_eq!(pulls.join().unwrap(), [true], "one pull, armed");
}

/// A SCHEDULING READING ANSWERS ONLY WITHIN `fresh=`, AND NONE WITHOUT THE CLOCK (C4 on the
/// paired census's clocks): `sys procs fresh=60000` over a reading whose sweep read scheduling,
/// now 120 s old, answers nothing at once but pulls, its scheduling armed, and answers the pull's
/// reading; inside that pull's gap, once that reading is 120 s old too, a second one sends no pull
/// and answers it stale; a pull whose scheduling reading was observed 120 s before it lands
/// answers that reading stale; and with the observation clock unavailable, `sys procs` sends no
/// pull and answers the newest whole snapshot (a deck reading without scheduling), stale, never
/// the older reading that read it. NEGATIVE CONTROLS: without `fresh_age` in `fresh_by`'s early
/// answer the first answers the old reading fresh, with no pull; without it in the gap answer the
/// second answers its reading fresh; without it in the wait the third answers fresh; answering
/// the judged reading where the clock is unavailable (`stale(have)`) the fourth answers the
/// reading that read scheduling; and without that early return the fourth sends a pull.
#[test]
fn a_view_that_prints_scheduling_answers_its_reading_only_within_fresh() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SCHEDULING: Arming = Arming::new();
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    static ELAPSED_NS: AtomicU64 = AtomicU64::new(0);
    const GAP: Duration = Duration::from_secs(3600);
    const WAIT: Duration = Duration::from_secs(60);
    const OLD: Duration = Duration::from_secs(120);
    fn instant() -> Instant {
        *BASE.get().unwrap() + Duration::from_nanos(ELAPSED_NS.load(Ordering::SeqCst))
    }
    fn continuous() -> Option<u64> {
        Some(ELAPSED_NS.load(Ordering::SeqCst))
    }
    fn advance(by: Duration) {
        ELAPSED_NS.fetch_add(u64::try_from(by.as_nanos()).unwrap(), Ordering::SeqCst);
    }
    /// Now less `back` on both clocks.
    fn observed(back: Duration) -> ObservedAt {
        let back_ns = u64::try_from(back.as_nanos()).unwrap();
        ObservedAt {
            instant: instant() - back,
            continuous_ns: continuous().map(|ns| ns - back_ns),
        }
    }
    fn publish(shared: &Shared, snap: Snapshot, observed_at: ObservedAt) {
        shared.publish(
            Arc::new(snap),
            &Machine::default(),
            observed_at,
            None,
            None,
            true,
            None,
        );
    }
    // Each pull's reading reads scheduling where the query armed it, observed `back` before it
    // lands.
    let probe = |shared: &Arc<Shared>, rx: std::sync::mpsc::Receiver<Msg>, back: Duration| {
        let shared = Arc::clone(shared);
        std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut seq = 7300;
            while let Ok(msg) = rx.recv() {
                assert!(matches!(msg, Msg::Pull));
                let armed = SCHEDULING.armed();
                seen.push(armed);
                seq += 1;
                publish(&shared, with_scheduling(seq, armed), observed(back));
            }
            seen
        })
    };
    let ask = |shared: &Shared| {
        let q = SysQuery::parse("procs fresh=60000").expect("query");
        let (held, state) = shared.fresh_for(
            &q,
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &SCHEDULING,
            &Arming::new(),
        );
        let read = held.as_ref().is_some_and(|h| h.snap.scheduling_read);
        (held.map(|h| h.snap.seq), state, read)
    };
    let _ = BASE.set(Instant::now());

    let shared = Arc::new(Shared::paced_with_clocks(GAP, WAIT, instant, continuous));
    let (tx, rx) = std::sync::mpsc::sync_channel(2);
    shared.install(tx);
    let pulls = probe(&shared, rx, Duration::ZERO);
    publish(
        &shared,
        with_scheduling(7300, true),
        observed(Duration::ZERO),
    );
    advance(OLD);
    assert_eq!(
        ask(&shared),
        (Some(7301), Freshness::Fresh, true),
        "early: a scheduling reading older than fresh= pulls"
    );
    advance(OLD);
    {
        let mut g = shared.lock();
        g.pacing.wait = Duration::ZERO;
        g.pacing.wait_cold = Duration::ZERO;
    }
    assert_eq!(
        ask(&shared),
        (Some(7301), Freshness::Stale, true),
        "gap: the pull's reading, now older than fresh=, answers stale"
    );
    shared.lock().tx = None;
    assert_eq!(pulls.join().unwrap(), [true], "one pull, armed");

    let shared = Arc::new(Shared::paced_with_clocks(GAP, WAIT, instant, continuous));
    let (tx, rx) = std::sync::mpsc::sync_channel(2);
    shared.install(tx);
    let pulls = probe(&shared, rx, OLD);
    publish(
        &shared,
        with_scheduling(7300, false),
        observed(Duration::ZERO),
    );
    assert_eq!(
        ask(&shared),
        (Some(7301), Freshness::Stale, true),
        "wait: a pull's scheduling reading observed past fresh= answers stale"
    );
    shared.lock().tx = None;
    assert_eq!(pulls.join().unwrap(), [true], "one pull, armed");

    let shared = Shared::paced_with_clocks(GAP, Duration::ZERO, instant, || None);
    let (tx, rx) = std::sync::mpsc::sync_channel(2);
    shared.install(tx);
    let unclocked = || ObservedAt::capture(instant, || None);
    publish(&shared, with_scheduling(7300, true), unclocked());
    publish(&shared, with_scheduling(7301, false), unclocked());
    assert_eq!(
        ask(&shared),
        (Some(7301), Freshness::Stale, false),
        "no clock: the newest whole snapshot, stale"
    );
    assert!(
        matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)),
        "no clock: no pull"
    );
}

/// A snapshot carrying a jobs view the engine's pass made `age_ms` before its reading.
fn with_jobs_aged(seq: u64, age_ms: u64) -> Snapshot {
    let mut s = synthetic(seq, 0.1);
    s.jobs = Some(Arc::new(aterm_vitals::jobs::JobsView::default()));
    s.jobs_age_ms = age_ms;
    s
}

/// `sys jobs` is as fresh as its jobs view, not as the snapshot that carries it: a snapshot that
/// just landed with the 25 s old view of the engine's last pass is past the default 2 s, so the
/// reply pulls with the engine armed, and answers from the reading whose own pass made a view;
/// a pull whose reading still carries the old view times out to `state=stale`. Every other view
/// reads the same snapshot as fresh, arming nothing. NEGATIVE CONTROL: judge `sys jobs` by the
/// snapshot's landing, and it answers the carried view at once, fresh, with no pull.
#[test]
fn sys_jobs_is_as_fresh_as_its_jobs_view_and_arms_the_engine_while_it_waits() {
    static JOBS: Arming = Arming::new();
    let run = |text: &str, wait: Duration, makes: bool| {
        let left = text.starts_with("left");
        let shared = Arc::new(Shared::paced(Duration::from_secs(3600), wait));
        let (tx, rx) = std::sync::mpsc::sync_channel(2);
        shared.install(tx);
        let probe = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || {
                let mut seen = Vec::new();
                while let Ok(msg) = rx.recv() {
                    assert!(matches!(msg, Msg::Pull));
                    seen.push(JOBS.armed());
                    let observed = Instant::now();
                    let age = if makes { 1 } else { 0 };
                    let jobs_at = if makes {
                        Some(observed)
                    } else {
                        observed.checked_sub(Duration::from_millis(25_100))
                    };
                    let mut offered = with_jobs_aged(7301, age);
                    if left {
                        offered.left = Some(Arc::new(aterm_vitals::left::LeftView::default()));
                    }
                    shared.publish(
                        Arc::new(offered),
                        &Machine::default(),
                        ObservedAt {
                            instant: observed,
                            continuous_ns: Some(0),
                        },
                        jobs_at,
                        if left { jobs_at } else { None },
                        true,
                        None,
                    );
                }
                seen
            })
        };
        let mut initial = with_jobs_aged(7300, 25_000);
        if left {
            initial.left = Some(Arc::new(aterm_vitals::left::LeftView::default()));
        }
        let now = Instant::now();
        let old = now.checked_sub(Duration::from_secs(25));
        shared.publish(
            Arc::new(initial),
            &Machine::default(),
            ObservedAt {
                instant: now,
                continuous_ns: Some(0),
            },
            old,
            if left { old } else { None },
            true,
            None,
        );
        let q = SysQuery::parse(text).expect("query");
        let (held, state) = shared.fresh_for(
            &q,
            &Arming::new(),
            &JOBS,
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
        );
        assert!(!JOBS.armed(), "{text}: disarmed once answered");
        shared.lock().tx = None;
        (held.map(|h| h.snap.seq), state, probe.join().unwrap())
    };
    let hang = Duration::from_secs(60);
    for text in ["left", "left fresh=0"] {
        assert_eq!(
            run(text, hang, true),
            (Some(7301), Freshness::Fresh, vec![true])
        );
    }
    assert_eq!(
        run("left fresh=60000", hang, true),
        (Some(7300), Freshness::Fresh, vec![])
    );
    assert_eq!(
        run("left", Duration::from_millis(200), false).1,
        Freshness::Stale
    );
    assert_eq!(
        run("jobs", hang, true),
        (Some(7301), Freshness::Fresh, vec![true]),
        "the default 2 s: pulled, the engine armed"
    );
    assert_eq!(
        run("jobs fresh=0", hang, true),
        (Some(7301), Freshness::Fresh, vec![true]),
        "forced"
    );
    assert_eq!(
        run("jobs fresh=60000", hang, true),
        (Some(7300), Freshness::Fresh, vec![]),
        "a 25 s view is within a minute"
    );
    assert_eq!(
        run("global", hang, true),
        (Some(7300), Freshness::Fresh, vec![]),
        "the snapshot itself just landed"
    );
    // The 200 ms is this leg's subject (a pull that never makes a view times out), so the probe
    // may read the arm after the wait ended: the legs above prove the arming, this one that the
    // one pull went out (the buffered Pull reaches the probe even once `tx` is dropped).
    let (seq, state, seen) = run("jobs", Duration::from_millis(200), false);
    assert_eq!(
        (state, seen.len()),
        (Freshness::Stale, 1),
        "the pull's reading carried the old view: {seq:?}"
    );
}

/// `privacy`'s count tells a held program's age on from the pass that made the jobs view, not
/// from the snapshot that carries it. NEGATIVE CONTROL: age the view from the snapshot's
/// landing, and a program held 2 s at a pass 25 s ago reads 2 s.
#[test]
fn privacys_count_ages_a_carried_view_from_its_pass() {
    use aterm_vitals::jobs::{Findings, JobsView, Named};
    let shared = Shared::with_clock(|| Some(0));
    let mut snap = synthetic(2, 0.1);
    snap.jobs = Some(Arc::new(JobsView {
        findings: Findings {
            program_check: vec![Named {
                pid: 25311,
                exec_version: None,
                name: "gh".into(),
                tab: None,
                job: None,
                for_ms: 2_000,
            }],
            ..Findings::default()
        },
        ..JobsView::default()
    }));
    snap.jobs_age_ms = 25_000;
    shared.publish_for_test(snap, Vec::new());
    let count = shared
        .program_check_now()
        .expect("a view that speaks for now");
    assert_eq!(count.held, 1);
    assert!(count.oldest_ms >= 27_000, "{count:?}");
}

#[test]
fn the_machine_wait_meets_refuses_times_out_and_sees_its_session_exit() {
    let shared = Shared::with_clock(|| Some(0));
    shared.publish_for_test(
        synthetic(3, 0.1),
        vec![PaneFact {
            key: 0,
            window: 0,
            tab: 1,
            tab_id: None,
        }],
    );
    let wait = |s: &str| match AwaitSys::parse(s).unwrap() {
        Parsed::Machine(w) => w,
        Parsed::Volume(_) => panic!("{s}"),
    };
    let alive = || false;
    let met = await_machine(&shared, 0, &wait("sys cpu<25% and mem=normal"), &alive);
    assert!(
        met.starts_with("OK sys cpu=10% mem=normal held_ms=0 as_of="),
        "{met}"
    );
    assert!(
        met.trim_end().ends_with(&format!("next={:x}.3", *EPOCH)),
        "{met}"
    );
    let quiet = await_machine(&shared, 0, &wait("sys quiet"), &alive);
    assert!(
        quiet.contains(" skipped=gpu:unsupported,thermal:unsupported,swap:not_sampled "),
        "{quiet}"
    );
    assert_eq!(
        await_machine(&shared, 0, &wait("sys gpu<10%"), &alive),
        "ERR unavailable gpu why=unsupported\n"
    );
    assert_eq!(
        await_machine(&shared, 0, &wait("sys cpu>90% timeout=0"), &alive),
        "OK timeout\n"
    );
    let gone = || true;
    assert_eq!(
        await_machine(&shared, 0, &wait("sys cpu>90% timeout=60000"), &gone),
        "ERR exited\n"
    );

    let make_stale = |shared: &Shared| {
        shared.lock().held.as_mut().unwrap().at = Instant::now()
            .checked_sub(AWAIT_PERIOD + Duration::from_secs(1))
            .unwrap();
    };
    make_stale(&shared);
    assert_eq!(
        await_machine(&shared, 0, &wait("sys cpu<25% timeout=0"), &alive),
        "OK timeout\n"
    );
    let replaced = std::cell::Cell::new(false);
    let replace = || {
        assert!(!replaced.replace(true), "the fresh replacement should meet");
        shared.publish_for_test(synthetic(4, 0.1), Vec::new());
        false
    };
    let met = await_machine(&shared, 0, &wait("sys cpu<25% timeout=60000"), &replace);
    assert!(replaced.get(), "the stale reading did not meet");
    assert!(
        met.trim_end().ends_with(&format!("next={:x}.4", *EPOCH)),
        "{met}"
    );

    shared.publish_for_test(synthetic(10, 0.1), Vec::new());
    let stage = std::cell::Cell::new(0);
    let produce = || {
        let next = stage.get() + 1;
        stage.set(next);
        match next {
            1 => {
                shared.publish_for_test(synthetic(12, 0.1), Vec::new());
                make_stale(&shared);
            }
            2 => shared.publish_for_test(synthetic(13, 0.1), Vec::new()),
            3 => shared.publish_for_test(synthetic(15, 0.1), Vec::new()),
            _ => panic!("the final two fresh readings should meet"),
        }
        false
    };
    let met = await_machine(
        &shared,
        0,
        &wait("sys cpu<25% for=2s timeout=60000"),
        &produce,
    );
    assert_eq!(stage.get(), 3, "a stale span cannot supply the hold: {met}");
    assert!(met.contains(" held_ms=2000 "), "{met}");
    assert!(
        met.trim_end().ends_with(&format!("next={:x}.15", *EPOCH)),
        "{met}"
    );
}

fn machine(s: &str) -> AwaitSys {
    match AwaitSys::parse(s).unwrap() {
        Parsed::Machine(w) => w,
        Parsed::Volume(_) => panic!("{s} routed to the volume"),
    }
}

fn with_jobs(seq: u64, view: &Arc<aterm_vitals::jobs::JobsView>) -> Snapshot {
    let mut s = synthetic(seq, 0.1);
    s.jobs = Some(Arc::clone(view));
    s
}

fn jobs_view(rows: Vec<aterm_vitals::jobs::JobRow>) -> Arc<aterm_vitals::jobs::JobsView> {
    Arc::new(aterm_vitals::jobs::JobsView {
        rows,
        findings: aterm_vitals::jobs::Findings::default(),
        demand: aterm_vitals::jobs::Demand::Fast,
        lanes_off: Vec::new(),
        parked_tabs: Vec::new(),
        at_ms: 0,
        zombie_facts: None,
    })
}

#[test]
fn a_named_job_that_left_the_view_answers_from_the_ring_or_is_gone() {
    use aterm_vitals::events::{Event, EventKind};
    use aterm_vitals::jobs::{Class, Verdict};
    let shared = Shared::with_clock(|| Some(0));
    let empty = jobs_view(Vec::new());
    let mut s = with_jobs(2, &empty);
    s.events = vec![Event {
        seq: 2,
        at_ns: s.at_ns,
        at_unix_ms: Some(s.at_unix_ms),
        scope: aterm_vitals::events::EventScope::Session(aterm_vitals::events::SessionScope {
            local: 7,
            sid: "s-test".into(),
            nonce: "test-nonce".into(),
        }),
        kind: EventKind::Job {
            tab: 7,
            id: "a1".into(),
            from: Some(Verdict::Fine),
            to: Verdict::Exited,
            class: Class::Exited,
            task: None,
            exit: Some(2),
        },
    }];
    shared.publish_for_test(
        s,
        vec![PaneFact {
            key: 7,
            window: 1,
            tab: 1,
            tab_id: None,
        }],
    );
    let ended = shared.ended_jobs();
    assert_eq!(ended.len(), 1);
    let met = await_machine(
        &shared,
        7,
        &machine("sys job=a1 done timeout=60000"),
        &|| false,
    );
    assert!(
        met.starts_with("OK sys job=a1 verdict=exited class=exited exit=2 held_ms=0 "),
        "{met}"
    );
    assert_eq!(
        await_machine(
            &shared,
            7,
            &machine("sys job=a1 stuck timeout=60000"),
            &|| false
        ),
        "ERR gone job=a1\n"
    );
    let wait = machine("sys job=a9 done");
    let mut w = MachineWait::default();
    let (v1, v2, v3) = (
        jobs_view(Vec::new()),
        jobs_view(Vec::new()),
        jobs_view(Vec::new()),
    );
    assert_eq!(w.step(&wait, &with_jobs(1, &v1), 0, &[7], &ended), None);
    assert_eq!(w.step(&wait, &with_jobs(2, &v1), 0, &[7], &ended), None);
    assert_eq!(w.step(&wait, &with_jobs(3, &v2), 0, &[7], &ended), None);
    assert_eq!(
        w.step(&wait, &with_jobs(4, &v3), 0, &[7], &ended)
            .as_deref(),
        Some("ERR gone job=a9\n")
    );
    let live = || {
        jobs_view(vec![crate::stats_paint::tests::job_row(
            "a9",
            Verdict::Fine,
            None,
            None,
        )])
    };
    let mut w = MachineWait::default();
    for seq in 1..6 {
        assert_eq!(
            w.step(&wait, &with_jobs(seq, &live()), 0, &[7], &ended),
            None
        );
    }
}

#[test]
fn a_machine_wait_reads_its_tab_or_with_scope_aterm_every_tab() {
    let panes = [
        PaneFact {
            key: 1,
            window: 1,
            tab: 1,
            tab_id: None,
        },
        PaneFact {
            key: 2,
            window: 1,
            tab: 1,
            tab_id: None,
        },
        PaneFact {
            key: 3,
            window: 1,
            tab: 2,
            tab_id: None,
        },
        PaneFact {
            key: 4,
            window: 2,
            tab: 1,
            tab_id: None,
        },
    ];
    assert_eq!(wait_keys(&panes, 2, false), [1, 2]);
    assert_eq!(wait_keys(&panes, 2, true), [1, 2, 3, 4]);
    assert_eq!(
        wait_keys(&panes, 9, false),
        [9],
        "an unknown caller reads itself"
    );
}

#[test]
fn a_slots_wait_latches_on_the_snapshot_that_frees_a_slot() {
    use aterm_vitals::model::SlotsView;
    let one_slot = |seq: u64, held: u16, free: u16, discounted: u16| {
        let mut s = synthetic(seq, 0.1);
        s.slots = Some(SlotsView::Read {
            n: 1,
            held,
            free,
            pending: 0,
            unknown: 0,
            discounted,
        });
        s
    };
    let wait = machine("sys slots>=1");
    let mut w = MachineWait::default();
    assert_eq!(w.step(&wait, &one_slot(1, 1, 0, 0), 0, &[], &[]), None);
    assert_eq!(
        w.step(&wait, &one_slot(2, 0, 1, 1), 0, &[], &[]),
        None,
        "an orphan's slot is no free one"
    );
    let met = w
        .step(&wait, &one_slot(3, 0, 1, 0), 0, &[], &[])
        .expect("the snapshot that frees the slot meets it");
    assert!(
        met.starts_with("OK sys slots=1/1 held_ms=0 as_of="),
        "{met}"
    );
    let mut unread = synthetic(4, 0.1);
    unread.slots = Some(SlotsView::Unread(
        aterm_vitals::model::SlotsUnread::UnreadFormat,
    ));
    assert_eq!(
        MachineWait::default()
            .step(&wait, &unread, 0, &[], &[])
            .as_deref(),
        Some("ERR unavailable slots why=unread_format\n")
    );
    let wait = machine("sys slots>=1 for=20s");
    let mut w = MachineWait::default();
    assert_eq!(w.step(&wait, &one_slot(1, 0, 1, 0), 0, &[], &[]), None);
    assert_eq!(w.step(&wait, &one_slot(20, 0, 1, 0), 0, &[], &[]), None);
    let met = w
        .step(&wait, &one_slot(21, 0, 1, 0), 0, &[], &[])
        .expect("held twenty seconds");
    assert!(met.contains(" held_ms=20000 "), "{met}");
}

#[test]
fn a_wait_naming_the_slots_arms_their_cadence_while_it_stands() {
    for (text, names) in [
        ("sys slots>=1", true),
        ("sys load<0.5 and slots>=2 for=20m", true),
        ("sys load<0.5 and avail>=8GiB", false),
        ("sys quiet", false),
    ] {
        assert_eq!(names_slots(&machine(text)), names, "{text}");
    }
    let gauge = crate::occupancy_host::Arming::new();
    assert!(!gauge.armed());
    let one = gauge.arm();
    let two = gauge.arm();
    assert!(gauge.armed());
    drop(one);
    assert!(gauge.armed(), "one wait still stands");
    drop(two);
    assert!(!gauge.armed());
}

#[test]
fn for_holds_across_snapshots_before_the_wait_answers() {
    let shared = Arc::new(Shared::with_clock(|| Some(0)));
    shared.publish_for_test(synthetic(1, 0.1), Vec::new());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let feeder = {
        let (shared, stop) = (Arc::clone(&shared), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut seq = 1;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
                seq += 1;
                shared.publish_for_test(synthetic(seq, 0.1), Vec::new());
            }
        })
    };
    let w = match AwaitSys::parse("sys cpu<25% for=2s timeout=60000").unwrap() {
        Parsed::Machine(w) => w,
        Parsed::Volume(_) => unreachable!(),
    };
    let reply = await_machine(&shared, 0, &w, &|| false);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    feeder.join().unwrap();
    let held: u64 = reply
        .split_whitespace()
        .find_map(|t| t.strip_prefix("held_ms="))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("{reply}"));
    assert!(held >= 2000, "{reply}");
    assert!(reply.starts_with("OK sys cpu=10% held_ms="), "{reply}");
}

fn pane_fact(key: u64, tab: usize, id: u64) -> PaneFact {
    PaneFact {
        key,
        window: 1,
        tab,
        tab_id: Some(crate::tab_model::TabId::from_stored(id)),
    }
}

fn disk_ask<'a>(
    scope: ScopeArg,
    panes: &'a [PaneFact],
    keys: &'a [u64],
    tab: Option<u64>,
    lines: bool,
) -> DiskAsk<'a> {
    let mut tabs = Vec::new();
    for p in panes.iter().filter(|p| keys.contains(&p.key)) {
        if let Some(id) = p.tab_id
            && !tabs.contains(&id)
        {
            tabs.push(id);
        }
    }
    DiskAsk {
        scope,
        tab: tab.map(crate::tab_model::TabId::from_stored),
        tabs,
        panes,
        keys,
        lines,
    }
}

#[test]
fn without_a_walker_the_seam_names_each_panes_folder_and_claims_no_size() {
    let cwds = [
        PaneCwd {
            key: 1,
            tab: Some(1),
            path: "/w/a".into(),
        },
        PaneCwd {
            key: 2,
            tab: Some(1),
            path: "/w/a".into(),
        },
        PaneCwd {
            key: 3,
            tab: Some(2),
            path: "/w/b".into(),
        },
    ];
    let panes = [pane_fact(1, 1, 1), pane_fact(2, 1, 1), pane_fact(3, 2, 2)];
    let ask = disk_ask(ScopeArg::Aterm, &panes, &[1, 2, 3], Some(1), true);
    assert_ne!(
        crate::diskwalk_host::DISKWALK.walker_state(),
        crate::diskwalk_host::WalkerState::Running,
        "a test process starts no walker of its own"
    );
    let (roots, disk) = disk_roots(&ask, &cwds);
    assert_eq!(disk, None);
    assert_eq!(roots.len(), 2);
    assert!(
        roots
            .iter()
            .all(|r| r.kinds == ["folder"] && r.size == RootSize::NotSampled)
    );
    assert_eq!(roots[0].keys, [1, 2], "both panes own the shared folder");
    assert_eq!(roots[1].tab, Some(2));
}

/// AN ASK OF A WALKER NOT STARTED DEMANDS ITS SCOPES: a headless instance starts its walker
/// only once a scope is demanded, so `sys disk` there demands what it asked at look pace, and
/// the loop starts the walker for the next ask to read its sizes. The walker is a private one
/// (`diskwalk_host::Shared`), demanded at a set instant. NEGATIVE CONTROLS: a walker nobody
/// asked is not demanded, and an ask of no scope demands nothing.
#[test]
fn an_ask_of_a_walker_not_started_demands_what_it_asked() {
    use crate::diskwalk_host::{DiskScope, WalkerState};
    let walk = crate::diskwalk_host::Shared::new();
    let now = Instant::now();
    assert_eq!(walk.walker_state(), WalkerState::NotStarted);
    assert!(!walk.demanded(now), "nobody has asked");
    assert!(!ask_unstarted(&walk, &[], now));
    assert!(!walk.demanded(now), "an ask of no scope");
    let panes = [pane_fact(1, 1, 1)];
    let ask = disk_ask(ScopeArg::Aterm, &panes, &[1], Some(1), true);
    assert!(ask_unstarted(&walk, &ask.asked(), now));
    assert!(walk.demanded(now), "the walker is demanded");
    assert!(ask.asked().contains(&DiskScope::Aterm));
}

/// A board of two tabs: Tab 1's panes 1 and 2 at one repository, its build mirror walking
/// behind its prior figure; Tab 2's pane 3 at home (too broad, named) and pane 4 on another
/// machine; aterm's state folder; an opt-in inside a protected folder.
fn sys_board(now: Instant) -> crate::diskwalk_host::Published {
    use crate::diskwalk_host::{
        Claim, Complete, Detail, DirRow, Growth, LiveFrom, PaneView, PaneWhy, Pool, Published,
        RootKind, RootRecord, RootState, TabFacts,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;
    use std::sync::Arc;
    let done = |alloc: u64, biggest: Vec<DirRow>| Complete {
        sums: aterm_diskwalk::Sums {
            alloc_fair: alloc,
            logical: alloc / 2,
            files: 9,
            dirs: 1,
            ..aterm_diskwalk::Sums::default()
        },
        detail: Detail::Sizes,
        links: None,
        at: now.checked_sub(Duration::from_secs(4)).unwrap_or(now),
        took: Duration::from_millis(5),
        direct_entries_max: 7,
        biggest,
        most_entries: Vec::new(),
        huge: Vec::new(),
        not_measured: Vec::new(),
        growth: Some(Growth {
            bytes: 6_000,
            over: Duration::from_secs(3),
        }),
    };
    let claim = |p: &str, k: RootKind| Claim {
        path: PathBuf::from(p),
        kinds: BTreeSet::from([k]),
        linked_worktree: false,
    };
    let rec = |p: &str, state: RootState, complete: Option<Complete>| {
        Arc::new(RootRecord {
            path: PathBuf::from(p),
            state,
            id: None,
            partial: None,
            walked_entries: 0,
            complete,
            level: Vec::new(),
            read_from: None,
            asked: None,
        })
    };
    let repo = PathBuf::from(abs("/Users//a/w/repo"));
    let mirror = PathBuf::from(abs(
        "/Users//a/Library/Caches/aterm/.ubd/cargo-build.noindex/Users//a/w/repo",
    ));
    let tab_pane = |key: u64, from: LiveFrom, live: &str| PaneView {
        key,
        from: Some(from),
        exe: Some("zsh".into()),
        host: None,
        live: Some(PathBuf::from(live)),
        project: Some(repo.clone()),
        why: None,
        members: vec![
            claim(&abs("/Users//a/w/repo"), RootKind::Project),
            claim(&mirror.display().to_string(), RootKind::Build),
        ],
    };
    let mut walking = (*rec(
        &mirror.display().to_string(),
        RootState::Walking,
        Some(done(1_000, Vec::new())),
    ))
    .clone();
    walking.walked_entries = 120;
    walking.partial = Some(aterm_diskwalk::Sums {
        alloc_fair: 400_000,
        ..aterm_diskwalk::Sums::default()
    });
    let id = crate::tab_model::TabId::from_stored;
    Published {
        tabs: vec![
            TabFacts {
                id: id(1),
                label: "Tab 1".into(),
                number: Some(1),
                window: 1,
                panes: vec![1, 2],
            },
            TabFacts {
                id: id(2),
                label: "Tab 2".into(),
                number: Some(2),
                window: 1,
                panes: vec![3, 4],
            },
        ],
        panes: BTreeMap::from([
            (1, tab_pane(1, LiveFrom::Fg, &abs("/Users//a/w/repo/crates"))),
            (2, tab_pane(2, LiveFrom::Shell, &abs("/Users//a/w/repo"))),
            (
                3,
                PaneView {
                    key: 3,
                    from: Some(LiveFrom::Shell),
                    exe: Some("zsh".into()),
                    host: None,
                    live: Some(PathBuf::from(abs("/Users//a"))),
                    project: None,
                    why: Some(PaneWhy::Refused(aterm_diskwalk::Refusal::TooBroad)),
                    members: Vec::new(),
                },
            ),
            (
                4,
                PaneView {
                    key: 4,
                    from: Some(LiveFrom::Osc7),
                    exe: None,
                    host: Some("build-host".into()),
                    live: None,
                    project: None,
                    why: Some(PaneWhy::Elsewhere),
                    members: Vec::new(),
                },
            ),
        ]),
        own_claims: vec![claim(
            &abs("/Users//a/Library/Application Support/aterm"),
            RootKind::Own,
        )],
        opt_in: vec![claim(&abs("/Users//a/Documents/x"), RootKind::OptIn)],
        pools: [
            abs("/Users//a/w/repo"),
            mirror.display().to_string(),
            abs("/Users//a/Library/Application Support/aterm"),
            abs("/Users//a/Documents/x"),
        ]
        .iter()
        .map(|p| (PathBuf::from(p), Pool::Same))
        .collect(),
        home_pool: Some(aterm_sysprobe::volumes::VolumeKey::Container(
            "disk3".into(),
        )),
        records: BTreeMap::from([
            (
                repo.clone(),
                rec(
                    &abs("/Users//a/w/repo"),
                    RootState::Complete,
                    Some(done(
                        9_000,
                        vec![DirRow {
                            rel: "target".into(),
                            alloc: 5_000,
                            logical: 4_000,
                            entries: 30,
                            direct_entries: 3,
                        }],
                    )),
                ),
            ),
            (mirror.clone(), Arc::new(walking)),
            (
                PathBuf::from(abs("/Users//a/Library/Application Support/aterm")),
                rec(
                    &abs("/Users//a/Library/Application Support/aterm"),
                    RootState::Complete,
                    Some(done(2_000, Vec::new())),
                ),
            ),
            (
                PathBuf::from(abs("/Users//a/Documents/x")),
                rec(
                    &abs("/Users//a/Documents/x"),
                    RootState::NotMeasured(aterm_diskwalk::Refusal::Protected),
                    None,
                ),
            ),
        ]),
        discovered: BTreeSet::from([crate::diskwalk_host::DiskScope::Aterm]),
        fold_case: true,
        ..Published::default()
    }
}

fn sys_reports(
    board: &crate::diskwalk_host::Published,
    ask: &DiskAsk<'_>,
) -> Vec<crate::diskwalk_host::DiskReport> {
    ask.asked()
        .into_iter()
        .map(|s| {
            crate::diskwalk_host::build_report(s, board, crate::diskwalk_host::WalkerState::Running)
        })
        .collect()
}

#[test]
fn the_walkers_report_becomes_roots_sizes_and_a_balance() {
    use aterm_vitals::sys::RootSizes;
    let now = Instant::now();
    let board = sys_board(now);
    let panes = [
        pane_fact(1, 1, 1),
        pane_fact(2, 1, 1),
        pane_fact(3, 2, 2),
        pane_fact(4, 2, 2),
    ];
    let ask = disk_ask(ScopeArg::Tab, &panes, &[1, 2], Some(1), false);
    let reports = sys_reports(&board, &ask);
    let (roots, t) = disk_view(&board, &reports, &ask, &[], None, now);
    assert_eq!(roots.len(), 2, "{roots:#?}");
    assert_eq!(roots[0].path, abs("/Users//a/w/repo"));
    assert_eq!(roots[0].kinds, ["project"]);
    assert_eq!(roots[0].keys, [1, 2], "the tab's panes");
    assert_eq!(roots[0].tab, Some(1));
    assert_eq!(roots[0].scopes, ["tab:1", "aterm"]);
    assert_eq!(roots[0].pool, Some("same"));
    match &roots[0].size {
        RootSize::Complete(RootSizes {
            alloc: 9_000,
            entries: 10,
            growth_bps: Some(2_000),
            age_s: Some(4),
            ..
        }) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(roots[0].dirs[0].rel, "target");
    assert_eq!(roots[1].kinds, ["build"]);
    match &roots[1].size {
        RootSize::Walking {
            walked_entries: 120,
            partial_alloc: Some(400_000),
            last: Some(last),
        } => assert_eq!(last.alloc, 1_000),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        (t.total, t.walking),
        (10_000, true),
        "the rewalk's prior figure, never its partial"
    );
    assert!(
        t.scopes.is_empty() && t.panes.is_empty(),
        "`sys tab` prints none"
    );
    let aterm = disk_ask(ScopeArg::Aterm, &panes, &[1, 2, 3, 4], Some(1), false);
    let reports = sys_reports(&board, &aterm);
    let (roots, t) = disk_view(&board, &reports, &aterm, &[], None, now);
    let protected = roots
        .iter()
        .find(|r| r.path == abs("/Users//a/Documents/x"))
        .expect("the opt-in's row");
    assert_eq!(protected.size, RootSize::NotMeasured("protected"));
    assert!(protected.scopes.is_empty(), "an opt-in is in no scope");
    assert_eq!(t.total, 12_000);
    assert_eq!(t.not_measured, ["protected"]);
}

/// The root `sys disk` lists at `path`.
#[cfg(unix)]
fn root_at<'a>(roots: &'a [Root], path: &std::path::Path) -> Option<&'a Root> {
    roots.iter().find(|r| std::path::Path::new(&r.path) == path)
}

/// SYS DISK ATTRIBUTES A BUILD BY PATH TO ITS SESSION'S TAB (DC-01): Tab 1's pane sits in one
/// repository and a build started in it compiled a linked worktree elsewhere, leaving its
/// build-start line under the pane's session. `sys disk scope=tab` for that tab lists the
/// worktree as a project and its build mirror as build output, each the tab's; the line's
/// build folder lies inside the mirror and is no row. Tab 2 lists neither. NEGATIVE CONTROLS:
/// with no line, and with the same line under a session no pane has, Tab 1 lists its pane's
/// repository alone. The board is one real discovery over a scratch home.
#[cfg(unix)]
#[test]
fn sys_disk_attributes_a_build_by_path_to_its_sessions_tab() {
    use crate::diskwalk_host::{
        DiskScope, Facts, PaneFacts, PaneReads, Published, TabFacts, build_mirror, discovered_board,
    };
    use crate::tab_model::TabId;
    use aterm_sysprobe::volumes::VolumeKey;
    let tmp = aterm_tempfile::TempDir::new().expect("tempdir");
    let home = aterm_diskwalk::canonical(tmp.path())
        .expect("absolute temp folder")
        .join("home");
    let (main, wt, two) = (home.join("main"), home.join("wt"), home.join("two"));
    for folder in [main.join(".git"), wt.join("src"), two.clone()] {
        std::fs::create_dir_all(folder).expect("mkdir");
    }
    std::fs::write(wt.join(".git"), "gitdir: elsewhere\n").expect("the worktree's link file");
    let cache = atpkg::build_cache::cache_root(&home);
    let build = atpkg::build_cache::build_dir(&cache, &wt);
    std::fs::create_dir_all(&build).expect("mkdir");
    let mirror = build_mirror(&cache, &wt);
    let session = |key: u64| format!("s-{key:020x}");
    let line = |session: String| atpkg::build_start::Start {
        session,
        workspace: wt.clone(),
        build: Some(build.clone()),
        pid: 4242,
        at_ms: 1_731_450_000_000,
    };
    let now = Instant::now();
    let discovered = |lines: Vec<atpkg::build_start::Start>| -> Published {
        let cwds = [(101_u32, main.clone()), (102, two.clone())];
        let facts = Facts {
            panes: [(1_u64, 101_u32), (2, 102)]
                .into_iter()
                .map(|(key, pid)| PaneFacts {
                    key,
                    session: session(key),
                    shell_pid: pid,
                    front_pid: pid,
                    report: None,
                })
                .collect(),
            tabs: [(1_u64, 1_usize), (2, 2)]
                .into_iter()
                .map(|(key, number)| TabFacts {
                    id: TabId::from_stored(key),
                    label: format!("Tab {number}"),
                    number: Some(number),
                    window: 1,
                    panes: vec![key],
                })
                .collect(),
            home: Some(home.clone()),
            ..Facts::default()
        };
        let reads = PaneReads {
            cwd: Box::new(move |pid| {
                cwds.iter()
                    .find(|(shell, _)| *shell == pid)
                    .map(|(_, at)| at.clone())
            }),
            exe: Box::new(|pid| Some(format!("p{pid}"))),
            pool: Box::new(|_, _| Some(VolumeKey::Fsid(1))),
            starts: Box::new(move || lines.clone()),
        };
        let scopes = [
            DiskScope::Tab(TabId::from_stored(1)),
            DiskScope::Tab(TabId::from_stored(2)),
            DiskScope::Aterm,
        ];
        discovered_board(facts, reads, &scopes, now)
    };
    let panes = [pane_fact(1, 1, 1), pane_fact(2, 2, 2)];
    let listed = |board: &Published, key: u64| -> Vec<Root> {
        let keys = [key];
        let ask = disk_ask(ScopeArg::Tab, &panes, &keys, Some(key), false);
        let reports = sys_reports(board, &ask);
        disk_view(board, &reports, &ask, &[], None, now).0
    };

    let board = discovered(vec![line(session(1))]);
    let roots = listed(&board, 1);
    let worktree = root_at(&roots, &wt).expect("the worktree the tab's build compiled");
    assert_eq!(worktree.kinds, ["project"], "{roots:#?}");
    assert_eq!(worktree.tab, Some(1));
    assert_eq!(worktree.keys, [1], "the tab's pane");
    assert!(worktree.scopes.iter().any(|s| s == "tab:1"), "{roots:#?}");
    let output = root_at(&roots, &mirror).expect("the worktree's build mirror");
    assert_eq!(output.kinds, ["build"], "{roots:#?}");
    assert_eq!(output.tab, Some(1));
    assert!(
        build.starts_with(&mirror) && root_at(&roots, &build).is_none(),
        "the line's build folder is counted in the mirror: {roots:#?}"
    );
    assert!(root_at(&roots, &main).is_some(), "{roots:#?}");
    assert_eq!(roots.len(), 3, "{roots:#?}");
    let other = listed(&board, 2);
    assert_eq!(
        other.iter().map(|r| r.path.clone()).collect::<Vec<_>>(),
        [two.display().to_string()],
        "the other tab lists its own folder alone"
    );

    for (what, lines) in [
        ("no line", Vec::new()),
        ("another session's line", vec![line(session(9))]),
    ] {
        let roots = listed(&discovered(lines), 1);
        assert_eq!(
            roots.iter().map(|r| r.path.clone()).collect::<Vec<_>>(),
            [main.display().to_string()],
            "{what}"
        );
    }
}

/// SYS DISK'S SCOPE LINES AND JSON AGREE WITH THE BAR: every figure from one board, the bytes
/// after the clamp to the Mac's used.
#[test]
fn sys_disk_scope_lines_and_json_agree_with_the_bar() {
    use crate::diskwalk_host::{DiskScope, clamp};
    use aterm_vitals::sys::{ScopeBytes, ScopeLine};
    let now = Instant::now();
    let board = sys_board(now);
    let panes = [
        pane_fact(1, 1, 1),
        pane_fact(2, 1, 1),
        pane_fact(3, 2, 2),
        pane_fact(4, 2, 2),
    ];
    let tab = crate::tab_model::TabId::from_stored(1);
    for used in [50_000, 11_000, 9_500] {
        let ask = disk_ask(ScopeArg::Tab, &panes, &[1, 2], Some(1), true);
        let reports = sys_reports(&board, &ask);
        let (_, t) = disk_view(&board, &reports, &ask, &[], Some((used, 100_000)), now);
        let a = board.figure(DiskScope::Aterm);
        let f = board.figure(DiskScope::Tab(tab));
        let c = clamp(Some(used), a.bytes, f.bytes);
        let [
            ScopeLine::Global {
                pool,
                used: u,
                total,
            },
            ScopeLine::Bytes(aterm),
            ScopeLine::Bytes(tab_line),
        ] = t.scopes.as_slice()
        else {
            panic!("{:#?}", t.scopes);
        };
        assert_eq!(
            (pool.as_deref(), *u, *total),
            (Some("container:disk3"), Some(used), Some(100_000))
        );
        assert_eq!(
            (aterm.bytes, aterm.clamped, aterm.drawn, aterm.complete),
            (c.aterm, c.aterm_clamped, true, true),
            "a rewalk behind its prior figure still counts"
        );
        assert_eq!(
            (tab_line.bytes, tab_line.clamped, tab_line.tab),
            (c.tab, c.tab_clamped, Some(1))
        );
        assert_eq!((aterm.roots, aterm.counted), (a.roots, a.counted));
        assert!(tab_line.bytes <= aterm.bytes && aterm.bytes <= used);
        let text = ScopeLine::Bytes(tab_line.clone())
            .text(&aterm_vitals::sys::pct, &|w| format!("unavailable({w})"));
        assert!(
            text.starts_with(&format!(
                "scope name=tab tab=1 pool=container:disk3 bytes={} drawn=yes complete=yes roots=2 counted=2",
                c.tab
            )),
            "{text}"
        );
        assert!(text.ends_with(&format!(
            " clamped={}",
            if c.tab_clamped { "yes" } else { "no" }
        )));
        let json = ScopeLine::Bytes(tab_line.clone()).json();
        assert!(contract_violations(&json).is_empty(), "{}", json.to_text());
        assert_eq!(
            json.get("bytes"),
            Some(&aterm_vitals::sys::json::Jv::u(c.tab))
        );
        let _: &ScopeBytes = tab_line;
    }
    // The pane lines: each pane in its tab's layout order, with its why.
    let ask = disk_ask(ScopeArg::Aterm, &panes, &[1, 2, 3, 4], Some(1), true);
    let reports = sys_reports(&board, &ask);
    let (_, t) = disk_view(&board, &reports, &ask, &[], None, now);
    let words: Vec<_> = t
        .panes
        .iter()
        .map(|p| (p.tab, p.pane, p.from, p.why, p.member))
        .collect();
    assert_eq!(
        words,
        [
            (Some(1), 1, Some("fg"), None, true),
            (Some(1), 2, Some("shell"), None, true),
            (Some(2), 1, Some("shell"), Some("too_broad"), false),
            (Some(2), 2, Some("osc7"), Some("elsewhere"), false),
        ]
    );
    let line = t.panes[0].text(
        true,
        Some(abs("/Users//a").as_str()),
        &aterm_vitals::sys::pct,
        &|e| aterm_vitals::sys::exe_word(e).to_owned(),
    );
    assert_eq!(
        line,
        format!(
            "pane tab=1 pane=1 from=fg exe=zsh live={} project={} member=yes",
            abs("/Users//a/w/repo/crates"),
            abs("/Users//a/w/repo")
        )
    );
    let hidden = t.panes[0].text(
        false,
        Some(abs("/Users//a").as_str()),
        &aterm_vitals::sys::pct,
        &|e| aterm_vitals::sys::exe_word(e).to_owned(),
    );
    assert_eq!(
        hidden,
        "pane tab=1 pane=1 from=fg exe=zsh under=home member=yes"
    );
    for p in &t.panes {
        let j = p.json(true, Some(abs("/Users//a").as_str()), &|e| {
            aterm_vitals::sys::exe_word(e).to_owned()
        });
        assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    }
}

/// A tab whose only root the walker does not walk balances as unavailable, never `0 B
/// exclusive`; summed with a measured tab, a lower bound.
#[test]
fn a_report_whose_only_root_is_not_measured_balances_as_unavailable_never_zero() {
    use aterm_vitals::sys::disk::{totals_clause, totals_json};
    let now = Instant::now();
    let mut board = sys_board(now);
    board.panes.retain(|k, _| *k >= 3);
    board.own_claims.clear();
    let panes = [pane_fact(3, 2, 2), pane_fact(4, 2, 2)];
    let ask = disk_ask(ScopeArg::Aterm, &panes, &[3, 4], None, false);
    let reports = sys_reports(&board, &ask);
    let (roots, t) = disk_view(&board, &reports, &ask, &[], None, now);
    assert_eq!(roots.len(), 1, "the opt-in alone: {roots:#?}");
    assert_eq!(roots[0].size, RootSize::NotMeasured("protected"));
    assert_eq!((t.total, t.walking), (0, false));
    assert_eq!(t.not_measured, ["protected"]);
    let bps = |b: f64| format!("{b} B/s");
    assert_eq!(totals_clause(Some(&t), bps), "sizes unavailable(protected)");
    let mut o = aterm_vitals::sys::json::Jv::obj::<&str>([]);
    totals_json(Some(&t), &mut o);
    assert!(contract_violations(&o).is_empty(), "{}", o.to_text());
    assert_eq!(
        o.get("exclusive_bytes"),
        Some(&aterm_vitals::sys::json::Jv::why("protected"))
    );
    let all = sys_board(now);
    let panes = [
        pane_fact(1, 1, 1),
        pane_fact(2, 1, 1),
        pane_fact(3, 2, 2),
        pane_fact(4, 2, 2),
    ];
    let ask = disk_ask(ScopeArg::Window, &panes, &[1, 2, 3, 4], Some(1), false);
    let reports = sys_reports(&all, &ask);
    let (_, t) = disk_view(&all, &reports, &ask, &[], None, now);
    assert_eq!(t.total, 10_000);
    assert!(t.walking, "the mirror's rewalk runs");
    assert_eq!(
        totals_clause(Some(&t), bps),
        "\u{2265}10 KB exclusive (walking) · growing 4000 B/s"
    );
}

/// A scope the walker has not discovered yet (the first `sys disk` a tab makes) reads as its
/// panes' cwds still walking, sizes unknown: never "0 roots".
#[test]
fn an_undiscovered_scope_reads_as_its_cwds_walking_never_as_no_roots() {
    let now = Instant::now();
    let board = crate::diskwalk_host::Published::default();
    let panes = [pane_fact(5, 3, 5), pane_fact(6, 4, 6)];
    let cwds = [
        PaneCwd {
            key: 5,
            tab: Some(3),
            path: "/w/new".into(),
        },
        PaneCwd {
            key: 6,
            tab: Some(4),
            path: "/w/other".into(),
        },
    ];
    let ask = disk_ask(ScopeArg::Tab, &panes, &[5], Some(5), false);
    let reports = sys_reports(&board, &ask);
    assert!(reports.iter().all(|r| r.unknown_tab || r.undiscovered));
    let (roots, t) = disk_view(&board, &reports, &ask, &cwds, None, now);
    assert_eq!(roots.len(), 1, "the pane's own cwd only: {roots:?}");
    assert_eq!(
        (
            roots[0].path.as_str(),
            roots[0].keys.as_slice(),
            roots[0].tab
        ),
        ("/w/new", &[5][..], Some(3))
    );
    assert_eq!(roots[0].size.state_word(), Some("walking"));
    assert_eq!(roots[0].size.alloc_text(), "unavailable(walking)");
    assert!(t.walking && t.total == 0);
    let ask = disk_ask(ScopeArg::Aterm, &panes, &[5, 6], Some(5), false);
    let reports = sys_reports(&board, &ask);
    let (roots, t) = disk_view(&board, &reports, &ask, &cwds, None, now);
    let paths: Vec<&str> = roots.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, ["/w/new", "/w/other"]);
    assert!(t.walking);
}

#[test]
fn a_job_event_carries_join_keys_and_closed_words_only() {
    use aterm_vitals::jobs::{Class, Verdict};
    assert_eq!(
        job_event_payload(
            Verdict::Stuck,
            "a40977",
            Class::Waiting,
            3,
            Some("bq7f2k1za"),
            None
        ),
        "stuck job=a40977 class=waiting rev=3 task=bq7f2k1za"
    );
    assert_eq!(
        job_event_payload(Verdict::Exited, "j12", Class::Exited, 1, None, Some(0)),
        "exited job=j12 class=exited rev=1 code=0"
    );
    assert_eq!(
        job_event_payload(Verdict::Quiet, "a1", Class::Waiting, 2, Some("x y;z"), None),
        "quiet job=a1 class=waiting rev=2"
    );
}

#[test]
fn the_jobs_line_lands_before_the_trailer_and_the_header_counts_it() {
    let text = "OK 3 brief sid=s-1 v=2 as_of=9 ep=1 budget=500 used=40\n\
                - sh r1 2m00s · -\n\
                gap why=evicted lost=2 resync_to=4\n\
                more=-";
    let out = splice_brief_line(text, "jobs 1 job · 1 stuck", 600);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 5, "{out}");
    assert!(
        lines[0].starts_with("OK 4 brief sid=s-1 v=2 as_of=9 ep=1 budget=600 used="),
        "{out}"
    );
    assert_eq!(lines[2], "jobs 1 job · 1 stuck");
    assert!(lines[3].starts_with("gap why="));
    assert_eq!(lines[4], "more=-");
    let used: usize = lines[0]
        .rsplit("used=")
        .next()
        .and_then(|u| u.parse().ok())
        .expect("used=");
    assert_eq!(used, aterm_slog::tokens(out.len()), "{out}");
    let one = "OK 1 brief sid=s-1 v=2 as_of=9 ep=1 budget=5 used=12\n- sh r1 · -";
    assert_eq!(splice_brief_line(one, "jobs 1 job", 5), one);
}

/// A jobs view older than a minute says nothing of now when no window's reading is due again for it
/// on a timer that has not come (a suspect agent call, a program that has not started, its
/// snapshot's leftover view offering a closed tab's row): its reader stopped delivering. One read
/// again only on an edge, the disk watch's look or a pull (a parked tab, or a live job's own
/// demand, which arms nothing with no window) speaks while its reader runs. NEGATIVE CONTROLS: the
/// rule before the idle-wake fix (only a parked view speaks past a minute) leaves the running job's
/// old view unspoken, and the third block fails; `fresh_jobs` handing `speaks_for_now` no leftover
/// view lets the offered snapshot's old view speak, and the last block fails.
#[test]
fn a_jobs_view_older_than_a_minute_says_nothing_of_now_unless_read_only_at_edges() {
    use aterm_vitals::jobs::{Demand, Findings, JobsView, Named, Standing};
    let snap = |jobs: &Arc<JobsView>| {
        Arc::new(Snapshot {
            jobs: Some(Arc::clone(jobs)),
            ..Snapshot::default()
        })
    };
    let timed = |rows, findings| {
        Arc::new(JobsView {
            demand: Demand::Fast,
            rows,
            findings,
            ..JobsView::default()
        })
    };
    let suspect = aterm_vitals::jobs::JobRow {
        standing: Some(Standing::Capped),
        ..crate::vitals_jobs::tests::call_row(
            0,
            aterm_vitals::jobs::Consumer::AgentBg,
            true,
            31,
            27098,
            "python3",
        )
    };
    let not_started = Findings {
        not_started: vec![Named {
            pid: 25311,
            exec_version: None,
            name: "gh".into(),
            tab: Some(0),
            job: None,
            for_ms: 1_000,
        }],
        ..Findings::default()
    };
    let now = Instant::now();
    let old = now
        .checked_sub(crate::vitals_jobs::JOBS_FRESH + Duration::from_secs(1))
        .expect("a clock past a minute");
    for view in [
        timed(vec![suspect], Findings::default()),
        timed(Vec::new(), not_started),
    ] {
        assert!(fresh_jobs(Some((snap(&view), now)), now, true).is_some());
        assert!(fresh_jobs(Some((snap(&view), old)), now, true).is_none());
    }
    assert!(fresh_jobs(None, now, true).is_none());
    assert!(
        fresh_jobs(Some((Arc::new(Snapshot::default()), now)), now, true).is_none(),
        "no jobs view"
    );
    let parked = Arc::new(JobsView::default());
    assert!(fresh_jobs(Some((snap(&parked), old)), now, true).is_some());
    assert!(fresh_jobs(Some((snap(&parked), old)), now, false).is_none());
    let running = timed(
        vec![aterm_vitals::jobs::JobRow {
            tab: 0,
            ..crate::stats_paint::tests::job_row(
                "j41",
                aterm_vitals::jobs::Verdict::Fine,
                None,
                Some(1_000),
            )
        }],
        Findings::default(),
    );
    assert!(
        fresh_jobs(Some((snap(&running), old)), now, true).is_some(),
        "a running job's view, read again only at its edges: it speaks while its reader runs"
    );
    assert!(fresh_jobs(Some((snap(&running), old)), now, false).is_none());
    for view in [parked, running] {
        let offered = Arc::new(Snapshot {
            jobs: Some(Arc::clone(&view)),
            left: Some(Arc::new(crate::vitals_jobs::tests::offered_left())),
            ..Snapshot::default()
        });
        assert!(fresh_jobs(Some((Arc::clone(&offered), now)), now, true).is_some());
        assert!(
            fresh_jobs(Some((offered, old)), now, true).is_none(),
            "a leftover offer is read again within 30 s: this old view's reader stopped: {view:?}"
        );
    }
}

/// A `sys why` that may be of memory reads macOS's services on their own while the split by
/// coalition is cold, so the reply can name one with no Deck in view, and carries the episodes
/// whose restart the person put off, warm or cold. NEGATIVE CONTROL: a CPU reply, another view
/// and a warm split read nothing more, and a CPU reply carries no filing.
#[test]
fn a_why_with_the_split_cold_reads_the_services_on_their_own() {
    let held = || {
        Some(Held {
            processes: None,
            snap: Arc::new(Snapshot::default()),
            unix_ms: 0,
            at: Instant::now(),
            observed_at: ObservedAt::capture(Instant::now, || Some(0)),
            jobs_at: None,
            left_at: None,
        })
    };
    let reading = || {
        Some(aterm_process_facts::services::Reading {
            total: 128 << 30,
            ..aterm_process_facts::services::Reading::default()
        })
    };
    let read_for = |text: &str, held: Option<Held>| {
        let q = SysQuery::parse(text).expect("query");
        with_services(&q, held, reading, Vec::new())
            .and_then(|h| h.snap.services.clone())
            .is_some()
    };
    let filed_for = |text: &str, held: Option<Held>| {
        let q = SysQuery::parse(text).expect("query");
        with_services(&q, held, || None, vec!["537-1790000000".to_owned()])
            .map(|h| h.snap.services_filed.clone())
            .unwrap_or_default()
    };
    assert_eq!(filed_for("why mem", held()), ["537-1790000000"]);
    assert!(filed_for("why cpu", held()).is_empty());
    assert!(read_for("why", held()));
    assert!(read_for("why mem", held()));
    assert!(!read_for("why cpu", held()));
    assert!(!read_for("mem", held()));
    let warm = Snapshot {
        why: Some(Arc::new(aterm_vitals::explain::WhyFacts {
            lane: aterm_vitals::explain::CoalLane::On,
            split: aterm_vitals::explain::CoalSplit {
                rows: vec![aterm_vitals::explain::CoalRow::default()],
                ..aterm_vitals::explain::CoalSplit::default()
            },
            ..aterm_vitals::explain::WhyFacts::default()
        })),
        ..Snapshot::default()
    };
    let warm = || {
        Some(Held {
            processes: None,
            snap: Arc::new(warm.clone()),
            unix_ms: 0,
            at: Instant::now(),
            observed_at: ObservedAt::capture(Instant::now, || Some(0)),
            jobs_at: None,
            left_at: None,
        })
    };
    assert!(!read_for("why mem", warm()));
    assert_eq!(filed_for("why", warm()), ["537-1790000000"]);
}

/// A `sys left` that names no `@` target sees every session's rows: Owner's alone, read as
/// `scope=aterm` when it names no scope. A targeted one, and every other view, is read as
/// written. NEGATIVE CONTROL: read an untargeted `left` as the front tab's, and an edge reads
/// it.
#[test]
fn a_request_with_no_selector_needs_owner() {
    use super::untargeted_left;
    assert_eq!(untargeted_left("sys", "left", false), Err(()));
    assert_eq!(
        untargeted_left("sys", "left scope=tab --json", false),
        Err(())
    );
    assert_eq!(
        untargeted_left("sys", "left", true),
        Ok(Some("left scope=aterm".to_owned()))
    );
    assert_eq!(untargeted_left("sys", "left scope=pane", true), Ok(None));
    assert_eq!(untargeted_left("sys", "jobs", false), Ok(None));
    assert_eq!(
        untargeted_left("sys", "left scope=moon", false),
        Ok(None),
        "usage"
    );
    assert_eq!(untargeted_left("status", "left", false), Ok(None));
}

/// `scope=request` finds the request that owns the target's tab, its tabs here and its totals,
/// each unread figure `None`; a tab no request owns has none, which `cmd_sys` refuses.
#[test]
fn scope_request_reads_the_request_owning_the_tab() {
    let folder = aterm_tempfile::tempdir().expect("a folder");
    crate::requests_host::set_dir_for_test(Some(folder.path().to_path_buf()));
    let store = crate::session_store::new_store();
    let tab = crate::session_store::test_handle(61);
    let (key, sid) = (tab.local_id, tab.sid.as_str().to_owned());
    store.write().unwrap().register(tab);
    // NEGATIVE first: no request owns it.
    assert_eq!(request_facts(&store, &sid), None);
    let id = crate::requests_host::opened_for_test(
        true,
        "87be5228c0ffee0087be5228c0ffee00",
        "m114",
        "fleet:87be5228:-",
        crate::requests_host::Made::Shell,
        &sid,
    );
    let facts = request_facts(&store, &sid).expect("its request");
    assert_eq!(
        (facts.id.as_str(), facts.sessions, facts.tabs, facts.claims),
        (id.as_str(), vec![key], 1, 0)
    );
    assert_eq!(
        (facts.cpu_s, facts.peak_mb, facts.written_mb),
        (None, None, None),
        "an unread tree is unread, never 0"
    );
    crate::requests_host::set_dir_for_test(None);
}

/// The production Store lookup binds disclosure to the admitted session before any advisory
/// caller or row limit is applied. A visual sibling has no implicit scoped-read grant.
#[test]
fn sys_groups_host_binds_disclosure_to_the_authorized_session() {
    use aterm_vitals::groups::{GroupBook, GroupKey, Scope, Value};
    let store = crate::session_store::new_store();
    let own = crate::session_store::test_handle(61);
    let sibling = crate::session_store::test_handle(62);
    let foreign = crate::session_store::test_handle(63);
    let unknown = crate::session_store::test_handle(64);
    for handle in [&own, &sibling, &foreign] {
        store.write().unwrap().register(handle.clone());
    }
    let layout = [
        PaneFact {
            key: own.local_id,
            window: 1,
            tab: 1,
            tab_id: None,
        },
        PaneFact {
            key: sibling.local_id,
            window: 1,
            tab: 1,
            tab_id: None,
        },
        PaneFact {
            key: foreign.local_id,
            window: 1,
            tab: 2,
            tab_id: None,
        },
    ];
    let mut book = GroupBook::new();
    let observations = [
        (&own, "authorized-process", 0.25),
        (&sibling, "sibling-private-process", 50.0),
        (&foreign, "foreign-private-process", 100.0),
    ]
    .into_iter()
    .map(|(handle, name, cpu)| {
        let mut observation = groups_observation(Value::Read(cpu));
        observation.key = GroupKey::Tab {
            pane: handle.local_id,
            root: aterm_vitals::delegated::ProcessKey {
                pid: 400 + handle.local_id as u32,
                start: 500,
            },
        };
        observation.scope = Scope::Tab(handle.local_id);
        observation.name = name;
        observation
    });
    book.absorb(groups_frame(1), observations);
    let snapshot = groups_snapshot(1, &book);
    let at = ObservedAt {
        instant: Instant::now(),
        continuous_ns: Some(snapshot.at_ns),
    };
    MEMORY_CLOCK.with(|clock| clock.set(at.continuous_ns));
    MEMORY_INSTANT.with(|clock| clock.set(Some(at.instant)));
    let shared = Shared::paced_with_clocks(
        Duration::from_secs(60),
        Duration::from_secs(60),
        memory_instant,
        memory_clock,
    );
    shared.publish(
        Arc::new(snapshot.clone()),
        &Machine::default(),
        at,
        None,
        None,
        true,
        Some(groups_receipt(&snapshot, at, false)),
    );
    let authorized = AuthorizedTarget {
        session: own.local_id,
        term: &own.term,
        master: -1,
        ctx: &own.ctx,
    };
    let read = |request: &str, owner: bool| {
        let q = SysQuery::parse(request).expect("the public query grammar");
        let (held, state) = shared.fresh_for(
            &q,
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
            &Arming::new(),
        );
        let g = gather(&store, &authorized, &layout, &q, None, None, owner)
            .expect("the admitted target is still the registered target");
        assert!(
            held.is_some(),
            "a nonempty current source is the positive control"
        );
        let reply = answer(
            &shared,
            &q,
            held.as_ref(),
            state,
            &Machine::default(),
            &g,
            owner,
        );
        (g.caller_panes, reply)
    };
    for caller in [
        None,
        Some(&own),
        Some(&sibling),
        Some(&foreign),
        Some(&unknown),
    ] {
        for scope in ["pane", "tab", "window", "aterm", "global"] {
            for json in [false, true] {
                let mut request = format!("groups scope={scope} top=1 fresh=60000");
                if let Some(caller) = caller {
                    request.push_str(&format!(" caller={}", caller.sid.as_str()));
                }
                if json {
                    request.push_str(" --json");
                }
                let (panes, reply) = read(&request, false);
                assert_eq!(
                    panes.as_deref(),
                    Some([own.local_id].as_slice()),
                    "{request}"
                );
                assert!(reply.contains("authorized-process"), "{request}: {reply}");
                for private in ["sibling-private-process", "foreign-private-process"] {
                    assert!(!reply.contains(private), "{request}: {reply}");
                }
                if json {
                    let body: aterm_json::Value =
                        aterm_json::from_str(reply.lines().nth(1).unwrap()).unwrap();
                    assert_eq!(body["groups"]["rows"].as_array().unwrap().len(), 1);
                    assert_eq!(body["groups"]["withheld"].as_u64(), Some(2));
                } else {
                    assert!(reply.contains("withheld=2"), "{reply}");
                }
            }
        }
    }
    for json in [false, true] {
        let shape = if json { " --json" } else { "" };
        let (panes, all) = read(
            &format!("groups scope=global top=32 fresh=60000{shape}"),
            true,
        );
        assert!(panes.is_none());
        for name in [
            "authorized-process",
            "sibling-private-process",
            "foreign-private-process",
        ] {
            assert!(
                all.contains(name),
                "the owner's nonempty positive control: {all}"
            );
        }
        let (panes, filtered) = read(
            &format!(
                "groups scope=global top=32 fresh=60000 caller={}{shape}",
                foreign.sid.as_str()
            ),
            true,
        );
        assert_eq!(panes.as_deref(), Some([foreign.local_id].as_slice()));
        assert!(filtered.contains("foreign-private-process"), "{filtered}");
        assert!(
            !filtered.contains("authorized-process")
                && !filtered.contains("sibling-private-process")
        );
        let (panes, empty) = read(
            &format!(
                "groups scope=global top=32 fresh=60000 caller={}{shape}",
                unknown.sid.as_str()
            ),
            true,
        );
        assert_eq!(panes.as_deref(), Some([].as_slice()));
        for name in [
            "authorized-process",
            "sibling-private-process",
            "foreign-private-process",
        ] {
            assert!(
                !empty.contains(name),
                "an unresolved owner filter cannot widen: {empty}"
            );
        }
    }
    MEMORY_CLOCK.with(|clock| clock.set(None));
    MEMORY_INSTANT.with(|clock| clock.set(None));
}

/// A pull may finish after the admitted session was removed or replaced. The captured SID,
/// nonce and terminal identity must still match under the Store lock used by disclosure.
#[test]
fn sys_groups_host_refuses_an_admitted_target_replaced_before_gather() {
    for changed in ["sid", "nonce", "terminal", "missing"] {
        let store = crate::session_store::new_store();
        let original = crate::session_store::test_handle(71);
        store.write().unwrap().register(original.clone());
        let authorized = AuthorizedTarget {
            session: original.local_id,
            term: &original.term,
            master: -1,
            ctx: &original.ctx,
        };
        let q = SysQuery::parse("groups scope=global").unwrap();
        assert!(gather(&store, &authorized, &[], &q, None, None, false).is_ok());
        if changed == "missing" {
            let _ = store.write().unwrap().deregister_local(original.local_id);
        } else {
            let mut replacement = original.clone();
            match changed {
                "sid" => replacement.sid = crate::session_store::test_handle(72).sid,
                "nonce" => {
                    let bytes =
                        if original.nonce == aterm_session::LaunchNonce::from_bytes([42; 16]) {
                            [43; 16]
                        } else {
                            [42; 16]
                        };
                    replacement.nonce = aterm_session::LaunchNonce::from_bytes(bytes);
                }
                "terminal" => replacement.term = Arc::new(Mutex::new(Terminal::new(24, 80))),
                _ => unreachable!(),
            }
            store.write().unwrap().register(replacement);
        }
        for owner in [false, true] {
            let refusal = match gather(&store, &authorized, &[], &q, None, None, owner) {
                Ok(_) => panic!("the {changed} replacement retained the old admission"),
                Err(refusal) => refusal,
            };
            assert_eq!(
                refusal, "ERR sys target changed after authorization\n",
                "{changed}"
            );
        }
    }
}

#[test]
fn one_engine_processes_windowless_session_identity_does_not_depend_on_layout() {
    let store = crate::session_store::new_store();
    let visible = crate::session_store::test_handle(60);
    let hidden = crate::session_store::test_handle(61);
    store.write().unwrap().register(visible.clone());
    store.write().unwrap().register(hidden.clone());
    let snapshot = synthetic(1, 0.1);
    let receipt = crate::vitals_host::memory::Pending::begin(
        crate::vitals_host::memory::Probes::INERT,
        ObservedAt::capture(Instant::now, || None),
    )
    .finish(&aterm_sysprobe::Sweep {
        procs: Vec::new(),
        truncated: false,
    })
    .publish(
        &snapshot,
        vec![crate::vitals_host::memory::PaneSource {
            local: hidden.local_id,
            sid: hidden.sid.clone(),
            nonce: hidden.nonce,
        }],
    );
    let layout = [PaneFact {
        key: visible.local_id,
        window: 1,
        tab: 1,
        tab_id: None,
    }];
    let q = SysQuery::parse("aterm --json").unwrap();
    let gather_now = || {
        gather(
            &store,
            &AuthorizedTarget {
                session: visible.local_id,
                term: &visible.term,
                master: -1,
                ctx: &visible.ctx,
            },
            &layout,
            &q,
            Some(&snapshot),
            Some(&receipt),
            true,
        )
        .expect("the same admitted session is still live")
    };
    let read = gather_now();
    let fact = read
        .tabs
        .iter()
        .find(|fact| fact.key == hidden.local_id)
        .expect("its source identity survives without a visible tab");
    assert_eq!(fact.sid, hidden.sid.as_str());
    assert_eq!(fact.sampled_nonce, Some(hidden.nonce.to_hex()));
    assert_eq!((fact.window, fact.tab), (None, None));

    let mut relaunched = hidden.clone();
    let first = aterm_session::LaunchNonce::from_bytes([42; 16]);
    relaunched.nonce = if hidden.nonce == first {
        aterm_session::LaunchNonce::from_bytes([43; 16])
    } else {
        first
    };
    assert_ne!(relaunched.nonce, hidden.nonce);
    store.write().unwrap().register(relaunched);
    let read = gather_now();
    assert!(
        !read.tabs.iter().any(|fact| fact.key == hidden.local_id),
        "a current live nonce cannot relabel the older sample"
    );
}

/// The row of `held` at the fixture path `path`.
#[cfg(unix)]
fn row_at(
    held: &aterm_vitals::sys::held::Holdings,
    path: &str,
) -> aterm_vitals::sys::held::Holding {
    let path = abs(path);
    let Some(h) = held.rows.iter().find(|h| h.path == path) else {
        panic!("no {path} row: {:#?}", held.rows);
    };
    h.clone()
}

/// SYS DISK HELD GATHERS ITS ROWS FROM THE WALKER'S BOARD AND THE BUILD-START LINES (DC-09a):
/// read as aterm's scope, each root on home's volume is a row, its pinned children's bytes out
/// of it. Out of the root that holds each, a row of its own for a project's `target` its walk
/// listed, aterm's build cache, the root that cache moved from and its tooling cache, and each
/// build mirror in the build cache that a build-start line names, a nested one apart from the
/// one around it; a listed folder that holds a root row counts in none of them, and aterm's
/// caches folder keeps the rest (its reserve, its virtual machines), kept. Each row carries the
/// mark of the sessions that claim it (a mirror's, those its own lines name), the tab and
/// program of the nearest at work, and the next pass the passes that are on give it: a verify
/// run's end only where a run kept its record. The reply prints the rows gathered.
/// NEGATIVE CONTROLS: with no line the build cache keeps the mirrors' bytes; with no session in
/// aterm's table every claimed row is gone; a line of a session at work marks its mirror at
/// work, and run-end only once a run kept its record there; with cleanup off every pass but a
/// verify run's end is none; a linked worktree splits its `target` and a folder none; a root
/// on another volume is no row; with every walk complete the walk is complete, with no walk
/// demanded an unwalked root is not sampled and the walk idle, and with no walker it is
/// stopped; a reply with no rows gathered says so.
#[cfg(unix)]
#[test]
fn sys_disk_held_gathers_its_rows_from_build_start_lines_and_the_walkers_board() {
    use crate::diskwalk_host::{
        Claim, Complete, DirRow, DiskScope, Pool, RootKind, RootRecord, RootState, WalkerState,
        build_report,
    };
    use aterm_vitals::sys::held::{Bytes, Class, Holding, Holdings, Mark, Next, Walk, Worktree};
    use std::collections::BTreeSet;
    const REPO: &str = "/Users//a/w/repo";
    const TARGET: &str = "/Users//a/w/repo/target";
    const CACHES: &str = "/Users//a/Library/Caches/aterm";
    const BUILD: &str = "/Users//a/Library/Caches/aterm/.ubd/cargo-build.noindex";
    const MOVED: &str = "/Users//a/Library/Caches/aterm/cargo-build.noindex";
    const TOOLING: &str = "/Users//a/Library/Caches/aterm/tooling.noindex";
    const MIRROR: &str = "/Users//a/Library/Caches/aterm/.ubd/cargo-build.noindex/Users//a/w/repo";
    const OLD: &str = "/Users//a/Library/Caches/aterm/.ubd/cargo-build.noindex/Users//a/w/old";
    const DEV: &str = "/Users//a/Library/Caches/aterm/.ubd/cargo-build.noindex/Users//a/dev";
    const WT: &str = "/Users//a/Library/Caches/aterm/.ubd/cargo-build.noindex/Users//a/dev/.ubd/wt";
    const STATE: &str = "/Users//a/Library/Application Support/aterm";
    const OPT_IN: &str = "/Users//a/Documents/x";
    let now = Instant::now();
    let mut board = sys_board(now);
    let caches = PathBuf::from(abs(CACHES));
    board.own_claims.push(Claim {
        path: caches.clone(),
        kinds: BTreeSet::from([RootKind::Own]),
        linked_worktree: false,
    });
    board.pools.insert(caches.clone(), Pool::Same);
    let repo = PathBuf::from(abs(REPO));
    let Some(walk) = board.records[&repo].complete.clone() else {
        panic!("the fixture's project is walked");
    };
    let dir = |rel: &str, alloc: u64| DirRow {
        rel: rel.into(),
        alloc,
        logical: alloc,
        entries: 9,
        direct_entries: 1,
    };
    // The build cache's own folder holds the repository's mirror, a root row: it counts in no
    // part. Below it: two mirrors of gone sessions' workspaces (one nested in the other's
    // folder with a line of a session at work), and one no line names.
    let cache_walk = Complete {
        sums: aterm_diskwalk::Sums {
            alloc_fair: 50_000,
            ..walk.sums
        },
        biggest: vec![
            dir(".ubd/cargo-build.noindex", 32_000),
            dir(".ubd/cargo-build.noindex/Users//a/w/old", 20_000),
            dir(".ubd/cargo-build.noindex/Users//a/w/old/debug", 15_000),
            dir(".ubd/cargo-build.noindex/Users//a/w/stray", 3_000),
            dir(".ubd/cargo-build.noindex/Users//a/dev", 8_000),
            dir(".ubd/cargo-build.noindex/Users//a/dev/.ubd/wt", 6_000),
            dir("cargo-build.noindex", 4_000),
            dir("tooling.noindex", 2_500),
            dir("lima.noindex", 7_000),
        ],
        ..walk
    };
    let mut record = (*board.records[&repo]).clone();
    record.path = caches.clone();
    record.complete = Some(cache_walk);
    board.records.insert(caches, Arc::new(record));
    board.panes.get_mut(&1).expect("pane 1").exe = Some("claude".into());
    let start = |session: &str, workspace: &str| atpkg::build_start::Start {
        session: session.into(),
        workspace: PathBuf::from(abs(workspace)),
        build: None,
        pid: 4242,
        at_ms: 1_731_450_000_000,
    };
    let lines = [
        start("s-live", REPO),
        start("s-gone", "/Users//a/w/old"),
        start("s-gone", "/Users//a/dev"),
        start("s-live", "/Users//a/dev/.ubd/wt"),
    ];
    let live: [(u64, String); 2] = [(1, "s-live".into()), (2, "s-live2".into())];
    let home = PathBuf::from(abs("/Users//a"));
    let on = Passes {
        low_disk: true,
        daily_days: Some(3),
        run_end: true,
        outside_days: Some(7),
    };
    let mirror_path = PathBuf::from(abs(MIRROR));
    let kept_in_mirror = |m: &Path| m == mirror_path.as_path();
    let facts = HeldFacts {
        starts: &lines,
        live: &live,
        home: Some(home.as_path()),
        passes: on,
        kept: &kept_in_mirror,
        now,
    };
    let report = build_report(DiskScope::Aterm, &board, WalkerState::Running);
    let held = held_rows(&board, &report, true, &facts);
    let none = |project: &str, worktree, path: &str, bytes, class, next| Holding {
        project: project.into(),
        worktree,
        path: abs(path),
        keys: Vec::new(),
        mark: Mark::NoSession,
        tab: None,
        exe: None,
        bytes,
        class,
        next,
    };
    let at_work = |keys: Vec<u64>, h: Holding| Holding {
        keys,
        mark: Mark::AtWork,
        tab: Some(1),
        exe: Some("claude".into()),
        ..h
    };
    let gone = |h: Holding| Holding {
        mark: Mark::Gone,
        ..h
    };
    let daily = Next::DailyCache { stale_days: 3 };
    let run_end = Next::RunEnd {
        stale_days: Some(3),
    };
    let unused = |after: Duration| Next::DailyUnused {
        after_s: after.as_secs(),
    };
    let (rebuilt, kept) = (Class::Rebuildable, Class::Kept);
    let mut want = vec![
        none(
            "x",
            Worktree::OptIn,
            OPT_IN,
            Bytes::Unmeasured("protected"),
            kept,
            Next::NotAterms,
        ),
        none(
            "aterm",
            Worktree::Aterm,
            STATE,
            Bytes::Measured(2_000),
            kept,
            Next::Nothing,
        ),
        none(
            "aterm",
            Worktree::Aterm,
            CACHES,
            Bytes::Measured(11_500),
            kept,
            Next::Nothing,
        ),
        none(
            "aterm",
            Worktree::Cache,
            BUILD,
            Bytes::AtLeast(3_000),
            rebuilt,
            daily,
        ),
        none(
            "aterm",
            Worktree::Cache,
            MOVED,
            Bytes::AtLeast(4_000),
            rebuilt,
            unused(atpkg::build_cache::MOVED_AFTER),
        ),
        none(
            "aterm",
            Worktree::Tooling,
            TOOLING,
            Bytes::AtLeast(2_500),
            Class::Costly,
            unused(atpkg::build_cache::RECLAIM_IDLE),
        ),
        gone(none(
            "old",
            Worktree::Build,
            OLD,
            Bytes::AtLeast(20_000),
            rebuilt,
            daily,
        )),
        gone(none(
            "dev",
            Worktree::Build,
            DEV,
            Bytes::AtLeast(2_000),
            rebuilt,
            daily,
        )),
        at_work(
            vec![1],
            none(
                "wt",
                Worktree::Build,
                WT,
                Bytes::AtLeast(6_000),
                rebuilt,
                daily,
            ),
        ),
        at_work(
            vec![1, 2],
            none(
                "repo",
                Worktree::Build,
                MIRROR,
                Bytes::Measured(1_000),
                rebuilt,
                run_end,
            ),
        ),
        at_work(
            vec![1, 2],
            none(
                "repo",
                Worktree::Checkout,
                REPO,
                Bytes::Measured(4_000),
                kept,
                Next::Nothing,
            ),
        ),
        at_work(
            vec![1, 2],
            none(
                "repo",
                Worktree::Build,
                TARGET,
                Bytes::AtLeast(5_000),
                rebuilt,
                Next::TierPass,
            ),
        ),
    ];
    want.sort_by(|a, b| a.path.cmp(&b.path));
    let sorted = |h: &Holdings| {
        let mut rows = h.rows.clone();
        rows.sort_by(|a, b| a.path.cmp(&b.path));
        rows
    };
    assert_eq!(sorted(&held), want);
    assert_eq!(
        held.walk,
        Walk::Walking,
        "the repository's build mirror is walking"
    );
    assert_eq!(held.oldest_walk_s, Some(4));
    // The reply prints the rows gathered.
    let shared = Shared::with_clock(|| Some(0));
    shared.publish_for_test(synthetic(1, 0.1), Vec::new());
    let (snap, state) = shared.fresh(Duration::from_secs(3600), false);
    let q = SysQuery::parse("disk held").expect("disk held");
    let wired = Gathered {
        held: Some(held.clone()),
        ..gathered()
    };
    let reply = answer(
        &shared,
        &q,
        snap.as_ref(),
        state,
        &Machine::default(),
        &wired,
        true,
    );
    assert!(reply.contains(" view=disk form=held "), "{reply}");
    assert!(reply.contains("\nholding n=1 "), "{reply}");
    let unwired = answer(
        &shared,
        &q,
        snap.as_ref(),
        state,
        &Machine::default(),
        &gathered(),
        true,
    );
    assert!(
        unwired.contains("held unavailable(not_sampled)"),
        "CONTROL: {unwired}"
    );
    // No line: the build cache keeps its mirrors' bytes.
    let bare = held_rows(
        &board,
        &report,
        true,
        &HeldFacts {
            starts: &[],
            ..facts
        },
    );
    for path in [OLD, DEV, WT] {
        assert!(
            bare.rows.iter().all(|h| h.path != abs(path)),
            "CONTROL: {:#?}",
            bare.rows
        );
    }
    let build = row_at(&bare, BUILD);
    assert_eq!(build.bytes, Bytes::AtLeast(31_000), "CONTROL: {build:?}");
    // No session in aterm's table: every claimed row is gone, a mirror the daily pass's.
    let nobody = held_rows(&board, &report, true, &HeldFacts { live: &[], ..facts });
    for path in [REPO, TARGET, MIRROR, WT] {
        let h = row_at(&nobody, path);
        assert_eq!(
            (h.mark, h.tab, h.exe.as_deref()),
            (Mark::Gone, None, None),
            "CONTROL: {h:?}"
        );
        assert!(h.keys.is_empty(), "CONTROL: {h:?}");
    }
    assert_eq!(row_at(&nobody, MIRROR).next, daily, "CONTROL");
    // A line of a session at work marks its mirror at work, and run-end only once a verify
    // run kept its record in it.
    let mine = [start("s-live", "/Users//a/w/old")];
    let held_live = held_rows(
        &board,
        &report,
        true,
        &HeldFacts {
            starts: &mine,
            ..facts
        },
    );
    let old = row_at(&held_live, OLD);
    assert_eq!(
        (old.mark, old.tab),
        (Mark::AtWork, Some(1)),
        "CONTROL: {old:?}"
    );
    assert_eq!(old.keys, [1], "CONTROL: {old:?}");
    assert_eq!(old.next, daily, "CONTROL: no run kept its record there");
    let old_path = PathBuf::from(abs(OLD));
    let kept_in_old = |m: &Path| m == old_path.as_path();
    let ran = HeldFacts {
        starts: &mine,
        kept: &kept_in_old,
        ..facts
    };
    let old = row_at(&held_rows(&board, &report, true, &ran), OLD);
    assert_eq!(old.next, run_end, "CONTROL: {old:?}");
    // Cleanup off: every pass but a verify run's end is none.
    let off = Passes {
        low_disk: false,
        daily_days: None,
        run_end: true,
        outside_days: None,
    };
    let held_off = held_rows(
        &board,
        &report,
        true,
        &HeldFacts {
            passes: off,
            ..facts
        },
    );
    for path in [TARGET, OLD, DEV, WT, BUILD, MOVED, TOOLING] {
        let h = row_at(&held_off, path);
        assert_eq!(h.next, Next::CleanupOff, "CONTROL: {h:?}");
    }
    let mirror = row_at(&held_off, MIRROR);
    assert_eq!(mirror.next, Next::RunEnd { stale_days: None }, "CONTROL");
    // A linked worktree splits its `target` too; a folder no repository holds splits none.
    let mut linked = sys_board(now);
    for pane in linked.panes.values_mut() {
        for m in &mut pane.members {
            m.linked_worktree |= m.path == repo;
        }
    }
    let linked_report = build_report(DiskScope::Aterm, &linked, WalkerState::Running);
    let held_linked = held_rows(&linked, &linked_report, true, &facts);
    assert_eq!(
        row_at(&held_linked, REPO).worktree,
        Worktree::Linked,
        "CONTROL"
    );
    assert_eq!(
        row_at(&held_linked, TARGET).bytes,
        Bytes::AtLeast(5_000),
        "CONTROL"
    );
    let mut folder = sys_board(now);
    for pane in folder.panes.values_mut() {
        for m in pane.members.iter_mut().filter(|m| m.path == repo) {
            m.kinds = BTreeSet::from([RootKind::Folder]);
        }
    }
    let folder_report = build_report(DiskScope::Aterm, &folder, WalkerState::Running);
    let held_folder = held_rows(&folder, &folder_report, true, &facts);
    let plain = row_at(&held_folder, REPO);
    assert_eq!(plain.worktree, Worktree::Folder, "CONTROL: {plain:?}");
    assert_eq!(plain.bytes, Bytes::Measured(9_000), "CONTROL: {plain:?}");
    assert!(
        held_folder.rows.iter().all(|h| h.path != abs(TARGET)),
        "CONTROL"
    );
    // A root on another volume is no row.
    let mut far = sys_board(now);
    far.pools.insert(PathBuf::from(abs(OPT_IN)), Pool::Other);
    let far_report = build_report(DiskScope::Aterm, &far, WalkerState::Running);
    let held_far = held_rows(&far, &far_report, true, &facts);
    assert!(
        held_far.rows.iter().all(|h| h.path != abs(OPT_IN)),
        "CONTROL: {:#?}",
        held_far.rows
    );
    // The walk: complete once every walk is; with no walk demanded an unwalked root is not
    // sampled and the walk idle, else walking; with no walker, stopped.
    let mut unwalked = (*board.records[&mirror_path]).clone();
    unwalked.complete = None;
    let mut owed = sys_board(now);
    owed.records.insert(mirror_path.clone(), Arc::new(unwalked));
    let owed_report = build_report(DiskScope::Aterm, &owed, WalkerState::Running);
    let asked = held_rows(&owed, &owed_report, true, &facts);
    assert_eq!(
        row_at(&asked, MIRROR).bytes,
        Bytes::Unmeasured("walking"),
        "CONTROL"
    );
    assert_eq!(asked.walk, Walk::Walking, "CONTROL");
    let unasked = held_rows(&owed, &owed_report, false, &facts);
    assert_eq!(
        row_at(&unasked, MIRROR).bytes,
        Bytes::Unmeasured("not_sampled")
    );
    assert_eq!(unasked.walk, Walk::Idle);
    let walked = RootRecord {
        state: RootState::Complete,
        partial: None,
        ..(*board.records[&mirror_path]).clone()
    };
    board.records.insert(mirror_path.clone(), Arc::new(walked));
    let report = build_report(DiskScope::Aterm, &board, WalkerState::Running);
    let done = held_rows(&board, &report, false, &facts);
    assert_eq!(done.walk, Walk::Complete, "CONTROL: every walk is complete");
    let report = build_report(DiskScope::Aterm, &board, WalkerState::NotStarted);
    let idle = held_rows(&board, &report, true, &facts);
    assert_eq!(idle.walk, Walk::Stopped, "CONTROL: no walker measures");
}

/// A HOLDING NAMES THE TAB AND PROGRAM OF THE SESSION THAT CLAIMS IT MOST NEARLY: of the panes
/// at work whose folders hold it, the one whose folder lies deepest, not the least key; every
/// one of them may read its path. NEGATIVE CONTROL: a folder only the pane above claims names
/// that pane.
#[cfg(unix)]
#[test]
fn a_holding_names_the_tab_and_program_of_its_nearest_claimer() {
    use crate::diskwalk_host::{Claim, RootKind};
    use aterm_vitals::sys::held::Mark;
    use std::collections::BTreeSet;
    let mut board = sys_board(Instant::now());
    board.panes.get_mut(&1).expect("pane 1").exe = Some("claude".into());
    let mut above = board.panes[&1].clone();
    above.key = 0;
    above.exe = Some("zsh".into());
    above.members = vec![Claim {
        path: PathBuf::from(abs("/Users//a/w")),
        kinds: BTreeSet::from([RootKind::Folder]),
        linked_worktree: false,
    }];
    board.panes.insert(0, above);
    board.tabs[1].panes.push(0);
    let live: [(u64, String); 3] = [(0, "s-0".into()), (1, "s-1".into()), (2, "s-2".into())];
    let repo = HeldBy::of_path(&board, &live, Path::new(&abs("/Users//a/w/repo")));
    assert_eq!(repo.mark, Mark::AtWork);
    assert_eq!(repo.keys, [0, 1, 2]);
    assert_eq!((repo.tab, repo.exe.as_deref()), (Some(1), Some("claude")));
    let other = HeldBy::of_path(&board, &live, Path::new(&abs("/Users//a/w/other")));
    assert_eq!(other.keys, [0], "CONTROL");
    assert_eq!(
        (other.tab, other.exe.as_deref()),
        (Some(2), Some("zsh")),
        "CONTROL"
    );
}

/// THE PASSES `sys disk held` NAMES FOLLOW aterm.toml: the tier pass while
/// `disk.auto_free_gib` is above 0; the daily pass only on macOS, with the tier pass on and
/// `disk.dead_build_dirs`, at the cache's stale days; a verify run's trim on macOS whatever the
/// file says; the outside-cache step with the tier pass on and `disk.idle_build_days` at least
/// 1, at those days, off Windows. NEGATIVE CONTROLS: each condition off turns its pass off.
#[test]
fn the_passes_sys_disk_names_follow_aterm_toml() {
    use aterm_agent::harness::disk::Config;
    let on = Config {
        auto_free_gib: 20,
        dead_build_dirs: true,
        ..Config::default()
    };
    let days = atpkg::build_cache::RECLAIM_STALE.as_secs() / (24 * 60 * 60);
    let idle = u64::try_from(on.idle_build_days).expect("a positive default");
    let all = Passes {
        low_disk: true,
        daily_days: Some(days),
        run_end: true,
        outside_days: (!cfg!(windows)).then_some(idle),
    };
    assert_eq!(Passes::of(&on, true), all);
    let elsewhere = Passes {
        daily_days: None,
        run_end: false,
        ..all
    };
    assert_eq!(Passes::of(&on, false), elsewhere, "CONTROL: not macOS");
    let off = Config {
        auto_free_gib: 0,
        ..on
    };
    let none = Passes {
        low_disk: false,
        daily_days: None,
        outside_days: None,
        ..all
    };
    assert_eq!(Passes::of(&off, true), none, "CONTROL: cleanup off");
    let kept = Config {
        dead_build_dirs: false,
        ..on
    };
    let no_daily = Passes {
        daily_days: None,
        ..all
    };
    assert_eq!(
        Passes::of(&kept, true),
        no_daily,
        "CONTROL: no dead build dirs"
    );
    let no_window = Config {
        idle_build_days: 0,
        ..on
    };
    let no_outside = Passes {
        outside_days: None,
        ..all
    };
    assert_eq!(
        Passes::of(&no_window, true),
        no_outside,
        "CONTROL: no idle window, no outside-cache step"
    );
}

/// A HELD READ DEMANDS NO WALK: it reads the walker's last board and asks the walker for
/// nothing, and a walker that has not started reads as stopped. NEGATIVE CONTROL: a demand of
/// the same walker is seen.
#[test]
fn a_held_read_demands_no_walk() {
    use crate::diskwalk_host::{Detail, DiskScope, Pace};
    use aterm_vitals::sys::held::Walk;
    let walk = crate::diskwalk_host::Shared::new();
    let now = Instant::now();
    let facts = HeldFacts {
        starts: &[],
        live: &[],
        home: None,
        passes: Passes {
            low_disk: true,
            daily_days: Some(3),
            run_end: true,
            outside_days: Some(7),
        },
        kept: &|_: &Path| false,
        now,
    };
    let held = held_read(&walk, &facts);
    assert!(!walk.demanded(now), "a held read demands no walk");
    assert_eq!(held.walk, Walk::Stopped);
    assert!(held.rows.is_empty(), "{:#?}", held.rows);
    walk.demand(DiskScope::Aterm, Detail::Sizes, Pace::Look, now);
    assert!(walk.demanded(now), "CONTROL: a demand is seen");
}

/// A reading whose process sweep stalled is whole for a pull: it carries every machine figure and
/// names the stall, so `sys` answers it at once as `state=partial`, never waiting out the pull to
/// call it warming. CONTROLS: the same reading with its sweep answered is fresh, and one with no
/// aterm figures that names no stall is warming.
#[test]
fn a_partial_snapshot_answers_a_pull_without_waiting_it_out() {
    let shared = Shared::paced(Duration::from_secs(3600), aterm_spec::wait::MUST_HAPPEN);
    let stall = aterm_vitals::lanes::LaneStall {
        lane: aterm_vitals::lanes::PROCS,
        call: "proc_listallpids",
        pid: None,
        name: None,
        since_unix_ms: 1_792_185_500_000,
        for_ms: 4_000,
    };
    shared.publish_for_test(
        Snapshot {
            aterm: None,
            procs_window_ns: None,
            stalled: vec![stall],
            ..synthetic(3, 0.1)
        },
        Vec::new(),
    );
    let (held, state) = shared.fresh(Duration::from_secs(60), false);
    assert_eq!(state, Freshness::Partial);
    assert!(held.is_some_and(|h| h.snap.cpu.is_some() && h.snap.mem.is_some()));
    let reply = ask(&shared, "global");
    let header = reply.lines().next().unwrap();
    assert!(header.contains(" state=partial stalled=procs"), "{reply}");
    assert!(
        reply
            .lines()
            .any(|l| l.starts_with("stalled procs: proc_listallpids since ")),
        "{reply}"
    );
    shared.publish_for_test(synthetic(4, 0.1), Vec::new());
    assert_eq!(
        shared.fresh(Duration::from_secs(60), false).1,
        Freshness::Fresh
    );
    shared.publish_for_test(
        Snapshot {
            aterm: None,
            procs_window_ns: None,
            ..synthetic(5, 0.1)
        },
        Vec::new(),
    );
    assert_eq!(
        shared.fresh(Duration::from_secs(60), false).1,
        Freshness::Warming
    );
}
