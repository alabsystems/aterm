// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::native_settings::OwnerAnswer;

fn probe(mode: DebuggerMode) -> DebuggerProbe {
    DebuggerProbe {
        mode,
        member: DebuggerMember::Admin,
        label: if mode == DebuggerMode::Unknown {
            DebuggerProbeLabel::StatusUnrecognized
        } else {
            DebuggerProbeLabel::StatusOk
        },
    }
}

fn child(pid: u32) -> DevtoolsChild {
    DevtoolsChild {
        pid,
        birth: Some(ProcessBirth {
            seconds: 1_790_000_000,
            microseconds: 42,
        }),
    }
}

fn scratch(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "aterm-debugger-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn job(change: DebuggerChange, id: u64, slot: &Arc<ChangeSlot>) -> DevtoolsJob {
    DevtoolsJob::new(
        ChangeTicket::for_test(change, id),
        ProbeGate::on(),
        Arc::clone(slot),
        None,
        Arc::new(|| {}),
        |_| false,
    )
}

#[test]
fn markers_round_trip_and_refuse_what_this_build_did_not_write() {
    let marker = ChangeMarker::new(DebuggerChange::On, child(4242), 1_790_000_005).unwrap();
    assert_eq!(marker.encode(), "on 4242 1790000000.000042 1790000005");
    assert_eq!(ChangeMarker::parse(&marker.encode()), Some(marker));
    assert!(marker.encode().len() <= MAX_MARKER_BYTES);
    let off = ChangeMarker::new(
        DebuggerChange::Off,
        DevtoolsChild {
            pid: u32::MAX,
            birth: Some(ProcessBirth {
                seconds: u64::MAX,
                microseconds: 999_999,
            }),
        },
        u64::MAX,
    )
    .unwrap();
    assert!(off.encode().len() <= MAX_MARKER_BYTES, "{}", off.encode());
    assert_eq!(ChangeMarker::parse(&off.encode()), Some(off));
    for bad in [
        "",
        "enable 1 2.3 4",
        "on 0 1.2 3",
        "on 1 2 3",
        "on 1 2.3",
        "on 1 2.3 4 5",
        "on x 2.3 4",
    ] {
        assert_eq!(ChangeMarker::parse(bad), None, "{bad:?}");
    }
    assert_eq!(
        ChangeMarker::new(
            DebuggerChange::On,
            DevtoolsChild {
                pid: 7,
                birth: None
            },
            1
        ),
        None
    );
    let observed = ObservedMarker {
        mode: DebuggerMode::Enabled,
        since: 1_790_000_000,
        origin: Origin::Aterm,
    };
    assert_eq!(observed.encode(), "enabled 1790000000 aterm");
    assert_eq!(ObservedMarker::parse(&observed.encode()), Some(observed));
    for bad in [
        "",
        "unknown 1 aterm",
        "unrecognized 1 aterm",
        "enabled x aterm",
        "enabled 1 someone",
        "enabled 1 aterm extra",
    ] {
        assert_eq!(ObservedMarker::parse(bad), None, "{bad:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_busy_change_lock_is_waited_out_only_for_its_patience() {
    let dir = scratch("patience");
    let config = dir.join("aterm.toml");
    let LockTry::Held(held) = try_change_lock(&config) else {
        panic!("a fresh lock is free");
    };
    assert!(matches!(
        take_change_lock(&config, std::time::Duration::from_millis(1)),
        LockTry::Busy
    ));
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(60));
        drop(held);
    });
    assert!(matches!(
        take_change_lock(&config, std::time::Duration::from_secs(60)),
        LockTry::Held(_)
    ));
    releaser.join().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn the_change_lock_is_exclusive_released_on_drop_and_never_followed() {
    let dir = scratch("lock");
    let config = dir.join("aterm.toml");
    let LockTry::Held(held) = try_change_lock(&config) else {
        panic!("a free lock is taken");
    };
    assert!(matches!(try_change_lock(&config), LockTry::Busy));
    drop(held);
    assert!(matches!(try_change_lock(&config), LockTry::Held(_)));
    #[cfg(unix)]
    {
        let linked = scratch("lock-link");
        std::os::unix::fs::symlink(dir.join("elsewhere"), linked.join(CHANGE_LOCK)).unwrap();
        assert!(matches!(
            try_change_lock(&linked.join("aterm.toml")),
            LockTry::Unavailable(_)
        ));
        assert!(
            !dir.join("elsewhere").exists(),
            "nothing was created through the link"
        );
        let _ = std::fs::remove_dir_all(&linked);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn the_workers_lock_is_free_while_a_child_forked_under_it_has_yet_to_exec() {
    use crate::parked_fork::ParkedChild;
    let dir = scratch("fork-window");
    let config = dir.join("aterm.toml");
    let slot = Arc::new(ChangeSlot::default());
    slot.arm(1);
    let mut marked = job(DebuggerChange::On, 1, &slot);
    marked.config_path = Some(config.clone());
    let mut forked = None;
    run_devtools_job(marked, |_, _, on_spawn| {
        on_spawn(child(77));
        forked = Some(ParkedChild::fork());
        DevtoolsChangeRun::Exited {
            code: Some(0),
            stdout_head: String::new(),
            stderr_head: String::new(),
        }
    });
    let forked = forked.expect("the runner forked under the worker's lock");
    assert!(dir.join(CHANGE_MARKER).exists(), "the worker's marker");
    assert!(
        matches!(try_change_lock(&config), LockTry::Held(_)),
        "the ended worker's lock is free in one try"
    );
    settle_markers(&config, None, Some(&child(77)));
    assert!(
        !dir.join(CHANGE_MARKER).exists(),
        "the park's removal took the lock and removed the marker"
    );
    assert!(
        forked.still_parked(),
        "…while the child forked under the lock had yet to exec"
    );
    forked.release();

    let closed_only = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join(CHANGE_LOCK))
        .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the negative control: a lock let go by the close alone"
    )]
    closed_only.try_lock().unwrap();
    let forked = ParkedChild::fork();
    drop(closed_only);
    assert!(
        matches!(try_change_lock(&config), LockTry::Busy),
        "control: closed, not unlocked, the child's copy holds it"
    );
    forked.release();
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn the_change_marker_stays_durable_and_the_observed_mode_is_only_ordered() {
    use aterm_durable::{Flush, seam::Recording};
    let dir = scratch("flush-tiers");
    let config = dir.join("aterm.toml");
    let slot = Arc::new(ChangeSlot::default());
    slot.arm(1);
    let mut marked = job(DebuggerChange::On, 1, &slot);
    marked.config_path = Some(config.clone());
    let recording = Recording::start();
    run_devtools_job(marked, |_, _, on_spawn| {
        on_spawn(child(77));
        DevtoolsChangeRun::Exited {
            code: Some(0),
            stdout_head: String::new(),
            stderr_head: String::new(),
        }
    });
    assert!(dir.join(CHANGE_MARKER).exists(), "the worker's marker");
    assert_eq!(
        recording.take(),
        [Flush::Durable, Flush::Durable],
        "the change marker, then its folder's entry"
    );
    let observed = ObservedMarker {
        mode: DebuggerMode::Disabled,
        since: 5,
        origin: Origin::Unknown,
    };
    settle_markers(&config, Some(observed), Some(&child(77)));
    assert_eq!(recording.take(), [Flush::Ordered], "the observed mode");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn the_records_reader_keeps_a_live_child_names_a_stale_one_and_drops_an_unreadable_one() {
    let _patience = patience_for_test(Duration::ZERO);
    let dir = scratch("records");
    let config = dir.join("aterm.toml");
    let marker = ChangeMarker::new(DebuggerChange::On, child(4242), 7).unwrap();
    write_debugger_marker(
        &config,
        CHANGE_MARKER,
        &marker.encode(),
        crate::durable::Flush::Durable,
    )
    .unwrap();
    let observed = ObservedMarker {
        mode: DebuggerMode::Disabled,
        since: 5,
        origin: Origin::Unknown,
    };
    write_debugger_marker(
        &config,
        OBSERVED_MARKER,
        &observed.encode(),
        crate::durable::Flush::Ordered,
    )
    .unwrap();
    let alive = read_records(&config, |_| true);
    assert_eq!(alive.inherited, Some(marker));
    assert_eq!(alive.stale, None);
    assert_eq!(alive.observed, Some(observed));
    assert!(
        dir.join(CHANGE_MARKER).exists(),
        "a live child's marker stays"
    );
    let dead = read_records(&config, |_| false);
    assert_eq!(dead.inherited, None);
    assert_eq!(dead.stale, Some(marker), "its direction and child are kept");
    assert_eq!(dead.observed, Some(observed));
    assert!(
        dir.join(CHANGE_MARKER).exists(),
        "a stale marker waits for its change to be recorded"
    );
    write_debugger_marker(
        &config,
        CHANGE_MARKER,
        "on not-a-pid",
        crate::durable::Flush::Durable,
    )
    .unwrap();
    let LockTry::Held(held) = try_change_lock(&config) else {
        panic!("free");
    };
    assert_eq!(read_records(&config, |_| false).stale, None);
    assert!(
        dir.join(CHANGE_MARKER).exists(),
        "not removed from under a holder"
    );
    drop(held);
    assert_eq!(
        read_records(&config, |_| true),
        RecordsRead {
            observed: Some(observed),
            ..RecordsRead::default()
        }
    );
    assert!(
        !dir.join(CHANGE_MARKER).exists(),
        "an unreadable marker is removed"
    );
    write_debugger_marker(
        &config,
        CHANGE_MARKER,
        &marker.encode(),
        crate::durable::Flush::Durable,
    )
    .unwrap();
    settle_markers(&config, None, Some(&child(9)));
    assert!(
        dir.join(CHANGE_MARKER).exists(),
        "a marker naming another child stays"
    );
    settle_markers(&config, None, Some(&child(4242)));
    assert!(!dir.join(CHANGE_MARKER).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_change_lock_held_before_its_marker_is_written_is_read_again() {
    let _patience = patience_for_test(Duration::ZERO);
    let dir = scratch("unsettled");
    let config = dir.join("aterm.toml");
    assert_eq!(read_records(&config, |_| true), RecordsRead::default());
    assert!(
        !dir.join(CHANGE_LOCK).exists(),
        "a read where no change ever ran leaves no lock file"
    );
    let LockTry::Held(worker) = try_change_lock(&config) else {
        panic!("free");
    };
    let early = read_records(&config, |_| true);
    assert!(early.unsettled, "a held lock with no marker yet");
    let mut lane = DebuggerChangeLane::with_liveness(|_| true);
    assert!(lane.records_due(100), "the launch read");
    lane.settle_records(early);
    assert!(!lane.records_due(100), "one read out at a time");
    assert_eq!(lane.poll(100), LaneStep::default());
    assert!(!lane.flow().records_read(), "nothing was settled");
    assert!(lane.flow().busy(), "so no button is live");
    assert_eq!(
        lane.begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .map(|_| ()),
        Err(Refusal::RecordsUnread)
    );
    assert!(lane.needs_poll(), "the park keeps its RESULT_RECHECK");
    assert!(!lane.records_due(104), "not before RESULT_RECHECK");
    assert!(lane.records_due(105), "read again after it");
    let marker = ChangeMarker::new(DebuggerChange::On, child(77), 101).unwrap();
    write_debugger_marker(
        &config,
        CHANGE_MARKER,
        &marker.encode(),
        crate::durable::Flush::Durable,
    )
    .unwrap();
    drop(worker);
    let again = read_records(&config, |_| true);
    assert_eq!(again.inherited, Some(marker));
    assert!(!again.unsettled);
    lane.settle_records(again);
    lane.poll(105);
    assert_eq!(
        lane.flow()
            .in_flight()
            .map(|(change, _, _, inherited)| (change, inherited)),
        Some((DebuggerChange::On, true)),
        "the orphaned child is this aterm's change in flight"
    );
    assert_eq!(
        lane.begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .map(|_| ()),
        Err(Refusal::InFlight)
    );
    assert!(!lane.records_due(200), "settled: never read again");
    std::fs::remove_file(dir.join(CHANGE_MARKER)).unwrap();
    assert_eq!(read_records(&config, |_| true), RecordsRead::default());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_host_gates_do_not_require_a_hardened_aterm() {
    let open = BeginInputs {
        windowed: true,
        macos: true,
        privacy: true,
        self_hardened: SelfHardening::Hardened,
    };
    for hardening in [
        SelfHardening::Hardened,
        SelfHardening::NotHardened,
        SelfHardening::Unknown,
    ] {
        assert_eq!(
            admit_begin(BeginInputs {
                self_hardened: hardening,
                ..open
            }),
            Ok(()),
            "O8: {hardening:?}"
        );
    }
    assert_eq!(
        admit_begin(BeginInputs {
            windowed: false,
            ..open
        }),
        Err(Refusal::NoWindow)
    );
    assert_eq!(
        admit_begin(BeginInputs {
            macos: false,
            ..open
        }),
        Err(Refusal::NotMacos)
    );
    assert_eq!(
        admit_begin(BeginInputs {
            privacy: false,
            ..open
        }),
        Err(Refusal::PrivacyOff)
    );
}

#[test]
fn the_inert_arm_starts_nothing() {
    let slot = Arc::new(ChangeSlot::default());
    slot.arm(1);
    let mut inert = job(DebuggerChange::On, 1, &slot);
    inert.wake = Arc::new(|| panic!("the inert arm posts nothing"));
    let gestures = crate::native_settings::ConsentGestures::for_instance(true);
    assert_eq!(gestures.run_devtools(inert), DevtoolsSpawn::Inert);
    assert_eq!(slot.take(1), SlotRead::default());
    let mut inert = job(DebuggerChange::Off, 1, &slot);
    inert.wake = Arc::new(|| panic!("the inert arm posts nothing"));
    assert_eq!(
        crate::native_settings::ConsentGestures::default().run_devtools(inert),
        DevtoolsSpawn::Inert
    );
}

#[cfg(unix)]
#[test]
fn the_worker_holds_the_lock_publishes_the_child_then_the_result_and_leaves_the_marker() {
    let dir = scratch("worker");
    let config = dir.join("aterm.toml");
    let slot = Arc::new(ChangeSlot::default());
    slot.arm(3);
    let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let mut marked = job(DebuggerChange::On, 3, &slot);
    marked.config_path = Some(config.clone());
    marked.wake = Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let seen = std::cell::Cell::new((false, false));
    run_devtools_job(marked, |change, gate, on_spawn| {
        assert_eq!((change, gate), (DebuggerChange::On, ProbeGate::on()));
        let locked = matches!(try_change_lock(&config), LockTry::Busy);
        on_spawn(child(77));
        let names = read_debugger_marker(&config, CHANGE_MARKER)
            .and_then(|t| ChangeMarker::parse(&t))
            .is_some_and(|m| m.child == child(77) && m.change == DebuggerChange::On);
        seen.set((locked, names));
        DevtoolsChangeRun::Exited {
            code: Some(0),
            stdout_head: String::new(),
            stderr_head: String::new(),
        }
    });
    assert_eq!(
        seen.get(),
        (true, true),
        "locked, and the marker names the child"
    );
    assert!(
        matches!(try_change_lock(&config), LockTry::Held(_)),
        "the lock is free, in one try, once the worker ends"
    );
    assert!(
        dir.join(CHANGE_MARKER).exists(),
        "the marker waits for the park to take the result"
    );
    let read = slot.take(3);
    assert_eq!(read.spawned, Some(child(77)));
    assert!(matches!(
        read.result,
        Some(DevtoolsChangeRun::Exited { .. })
    ));
    assert!(read.gone);
    assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 2);

    slot.arm(4);
    let mut over_live = job(DebuggerChange::On, 4, &slot);
    over_live.config_path = Some(config.clone());
    over_live.alive = |c| c.pid == 77;
    run_devtools_job(over_live, |_, _, _| {
        panic!("nothing runs over a live change")
    });
    assert_eq!(slot.take(4).refused, Some(WorkerRefusal::PeerChanging));

    let LockTry::Held(held) = try_change_lock(&config) else {
        panic!("free");
    };
    slot.arm(5);
    let mut blocked = job(DebuggerChange::On, 5, &slot);
    blocked.config_path = Some(config.clone());
    let mut clock_reads = 0;
    run_devtools_job_with_clock(
        blocked,
        |_, _, _| panic!("nothing runs under a held lock"),
        || {
            clock_reads += 1;
            std::time::Instant::now()
                - if clock_reads == 1 {
                    Duration::from_secs(61)
                } else {
                    Duration::ZERO
                }
        },
    );
    let refused = slot.take(5);
    assert_eq!(refused.refused, Some(WorkerRefusal::PeerChanging));
    assert!(refused.gone && refused.spawned.is_none() && refused.result.is_none());
    drop(held);

    slot.arm(6);
    slot.publish_result(5, DevtoolsChangeRun::SpawnFailed);
    slot.publish_refused(5, WorkerRefusal::NoLock);
    assert_eq!(slot.take(6), SlotRead::default());
    slot.arm(7);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_devtools_job(job(DebuggerChange::Off, 7, &slot), |_, _, _| {
            panic!("stand-in runner fails")
        });
    }));
    assert!(panicked.is_err());
    assert_eq!(
        slot.take(7),
        SlotRead {
            gone: true,
            ..SlotRead::default()
        }
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn required_debugger_change_waits_for_its_native_predecessor() {
    for age in [Duration::from_secs(2), Duration::from_secs(61)] {
        let expired = age >= aterm_spec::wait::MUST_HAPPEN;
        let dir = scratch("required-writer");
        let config = dir.join("aterm.toml");
        let LockTry::Held(held) = try_change_lock(&config) else {
            panic!("native predecessor");
        };
        let mut held = Some(held);
        let slot = Arc::new(ChangeSlot::default());
        slot.arm(11);
        let mut request = job(DebuggerChange::On, 11, &slot);
        request.config_path = Some(config.clone());
        let ran = std::cell::Cell::new(false);
        let mut observed = 0;
        run_devtools_job_with_clock(
            request,
            |change, gate, on_spawn| {
                assert_eq!((change, gate), (DebuggerChange::On, ProbeGate::on()));
                assert!(matches!(try_change_lock(&config), LockTry::Busy));
                on_spawn(child(177));
                let marker = read_debugger_marker(&config, CHANGE_MARKER)
                    .and_then(|text| ChangeMarker::parse(&text))
                    .unwrap();
                assert_eq!(marker.child, child(177));
                ran.set(true);
                DevtoolsChangeRun::Exited {
                    code: Some(0),
                    stdout_head: String::new(),
                    stderr_head: String::new(),
                }
            },
            || {
                observed += 1;
                if observed == 2 && !expired {
                    drop(held.take());
                }
                std::time::Instant::now() - if observed == 1 { age } else { Duration::ZERO }
            },
        );
        if expired {
            assert!(
                matches!(try_change_lock(&config), LockTry::Busy),
                "expiry leaves the real predecessor held"
            );
            assert!(!dir.join(CHANGE_MARKER).exists());
        }
        drop(held);
        let read = slot.take(11);
        if expired {
            assert_eq!(read.refused, Some(WorkerRefusal::PeerChanging));
            assert!(!ran.get());
            assert!(read.spawned.is_none() && read.result.is_none());
        } else {
            assert_eq!(
                read.refused, None,
                "a released native predecessor is not a peer changing access"
            );
            assert!(ran.get());
            assert_eq!(read.spawned, Some(child(177)));
            assert!(matches!(
                read.result,
                Some(DevtoolsChangeRun::Exited { code: Some(0), .. })
            ));
        }
        assert!(read.gone);
        assert!(
            observed >= 2,
            "the reader observed native contention before release"
        );
        assert!(matches!(try_change_lock(&config), LockTry::Held(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn drift_records_only_transitions_names_no_cause_and_never_asks_to_re_enable() {
    let mut watch = DriftWatch::default();
    assert_eq!(
        watch.on_observed(DebuggerMode::Disabled, None, 1),
        DriftStep::default()
    );
    let was_on = ObservedMarker {
        mode: DebuggerMode::Enabled,
        since: 100,
        origin: Origin::Aterm,
    };
    let step = watch.seed(Some(was_on), 200, false);
    assert_eq!(step.record, Some(DebuggerRecord::WasOnNowOff));
    assert_eq!(step.write.map(|m| m.mode), Some(DebuggerMode::Disabled));
    assert_eq!(watch.drift_since(), Some(100));
    assert_eq!(
        watch.on_observed(DebuggerMode::Disabled, None, 300),
        DriftStep::default()
    );
    for mode in [DebuggerMode::Unrecognized, DebuggerMode::Unknown] {
        assert_eq!(watch.on_observed(mode, None, 301), DriftStep::default());
    }
    let on = watch.on_observed(DebuggerMode::Enabled, Some(DebuggerChange::On), 400);
    assert_eq!(on.record, Some(DebuggerRecord::On));
    assert_eq!(on.write.map(|m| m.origin), Some(Origin::Aterm));
    assert_eq!(watch.drift_since(), None);
    let off = watch.on_observed(DebuggerMode::Disabled, Some(DebuggerChange::Off), 500);
    assert_eq!(off.record, Some(DebuggerRecord::Off));
    assert_eq!(watch.drift_since(), None);
    let on_disk = watch.recorded();
    assert_eq!(
        watch.on_observed(DebuggerMode::Enabled, None, 600),
        DriftStep::default()
    );
    assert!(!watch.seeded(), "it waits for a fresh read");
    let outside = watch.seed(on_disk, 601, false);
    assert_eq!(outside.record, Some(DebuggerRecord::OnOutside));
    assert_eq!(outside.write.map(|m| m.origin), Some(Origin::Outside));
    let mut fresh = DriftWatch::default();
    assert_eq!(fresh.seed(None, 1, false), DriftStep::default());
    assert_eq!(
        fresh.on_observed(DebuggerMode::Enabled, None, 2),
        DriftStep::default()
    );
    let first = fresh.seed(None, 3, false);
    assert_eq!(first.record, None);
    assert_eq!(first.write.map(|m| m.origin), Some(Origin::Unknown));
}

#[test]
fn a_peer_aterms_change_is_judged_against_the_shared_record() {
    let dir = scratch("peer-drift");
    let config = dir.join("aterm.toml");
    let write = |marker: ObservedMarker| {
        write_debugger_marker(
            &config,
            OBSERVED_MARKER,
            &marker.encode(),
            crate::durable::Flush::Ordered,
        )
        .unwrap();
    };
    let reread = |lane: &mut DebuggerChangeLane, now: u64| {
        assert!(lane.records_due(now), "a fresh read of the record is asked");
        lane.settle_records(read_records(&config, |_| false));
        lane.poll(now)
    };
    write(ObservedMarker {
        mode: DebuggerMode::Disabled,
        since: 100,
        origin: Origin::Unknown,
    });
    let mut b = DebuggerChangeLane::with_liveness(|_| false);
    reread(&mut b, 200);
    assert_eq!(
        b.observe(probe(DebuggerMode::Disabled), 201),
        LaneStep::default()
    );
    assert!(!b.records_due(201), "nothing new: nothing to read");
    write(ObservedMarker {
        mode: DebuggerMode::Enabled,
        since: 302,
        origin: Origin::Aterm,
    });
    let seen = b.observe(probe(DebuggerMode::Enabled), 400);
    assert_eq!(
        seen.drift,
        DriftStep::default(),
        "not judged from a stale copy"
    );
    assert!(b.wants_poll(), "the park takes the read it asks for");
    assert_eq!(
        reread(&mut b, 400).drift,
        DriftStep::default(),
        "already on record"
    );
    let facts = b.facts(true, probe(DebuggerMode::Enabled), SelfHardening::Hardened);
    assert_eq!(facts.recorded.map(|m| m.origin), Some(Origin::Aterm));
    assert_eq!(
        read_debugger_marker(&config, OBSERVED_MARKER)
            .and_then(|text| ObservedMarker::parse(&text))
            .map(|m| (m.since, m.origin)),
        Some((302, Origin::Aterm)),
        "the peer's record stands"
    );
    write(ObservedMarker {
        mode: DebuggerMode::Disabled,
        since: 600,
        origin: Origin::Aterm,
    });
    assert_eq!(
        b.observe(probe(DebuggerMode::Disabled), 700).drift,
        DriftStep::default()
    );
    assert_eq!(reread(&mut b, 700).drift, DriftStep::default());
    let facts = b.facts(true, probe(DebuggerMode::Disabled), SelfHardening::Hardened);
    assert_eq!(facts.drift_since, None, "no \"it is off now\" line");
    assert_eq!(
        b.observe(probe(DebuggerMode::Enabled), 800).drift,
        DriftStep::default()
    );
    let outside = reread(&mut b, 800).drift;
    assert_eq!(outside.record, Some(DebuggerRecord::OnOutside));
    assert_eq!(outside.write.map(|m| m.origin), Some(Origin::Outside));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_peer_reading_between_a_changes_result_and_its_record_files_it_as_aterms() {
    let dir = scratch("result-window");
    let config = dir.join("aterm.toml");
    let apply = |step: &LaneStep| {
        settle_markers(
            &config,
            step.drift.write,
            step.remove_change_marker.as_ref(),
        );
    };
    let reread = |lane: &mut DebuggerChangeLane, now: u64| {
        lane.settle_records(read_records(&config, |_| false));
        let step = lane.poll(now);
        apply(&step);
        step
    };
    let before = ObservedMarker {
        mode: DebuggerMode::Disabled,
        since: 100,
        origin: Origin::Unknown,
    };
    write_debugger_marker(
        &config,
        OBSERVED_MARKER,
        &before.encode(),
        crate::durable::Flush::Ordered,
    )
    .unwrap();
    let mut a = DebuggerChangeLane::with_liveness(|_| false);
    let mut b = DebuggerChangeLane::with_liveness(|_| false);
    for lane in [&mut a, &mut b] {
        reread(lane, 200);
        apply(&lane.observe(probe(DebuggerMode::Disabled), 201));
    }
    let ticket = a
        .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
        .unwrap();
    let worker = DevtoolsJob::new(
        ticket,
        ProbeGate::on(),
        a.slot(),
        Some(config.clone()),
        Arc::new(|| {}),
        |_| false,
    );
    run_devtools_job(worker, |_, _, on_spawn| {
        on_spawn(child(41));
        DevtoolsChangeRun::Exited {
            code: Some(0),
            stdout_head: String::new(),
            stderr_head: String::new(),
        }
    });
    apply(&a.poll(300));
    assert!(a.awaiting_probe(), "A took the result; its re-probe is out");

    apply(&b.observe(probe(DebuggerMode::Enabled), 301));
    assert!(b.records_due(301), "B asks for the shared records");
    let read = reread(&mut b, 301);
    assert_ne!(
        read.drift.record,
        Some(DebuggerRecord::OnOutside),
        "A's change is not \"set outside aterm\""
    );
    assert!(
        b.awaiting_probe(),
        "B found A's marker, its child gone, and re-reads that change as aterm's"
    );
    let b_ended = b.observe(probe(DebuggerMode::Enabled), 302);
    apply(&b_ended);
    assert_eq!(b_ended.drift.record, Some(DebuggerRecord::On));
    let a_ended = a.observe(probe(DebuggerMode::Enabled), 303);
    apply(&a_ended);
    assert_eq!(a_ended.outcome, Some(Outcome::Changed(DebuggerChange::On)));
    assert_eq!(a_ended.remove_change_marker, Some(child(41)));
    for lane in [&a, &b] {
        let facts = lane.facts(true, probe(DebuggerMode::Enabled), SelfHardening::Hardened);
        assert_eq!(facts.recorded.map(|m| m.origin), Some(Origin::Aterm));
    }
    assert_eq!(
        read_debugger_marker(&config, OBSERVED_MARKER)
            .and_then(|text| ObservedMarker::parse(&text))
            .map(|m| (m.mode, m.origin)),
        Some((DebuggerMode::Enabled, Origin::Aterm))
    );
    assert!(
        !dir.join(CHANGE_MARKER).exists(),
        "the marker went once the change was recorded"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_released_changes_marker_read_before_its_removal_is_not_adopted_again() {
    let dir = scratch("released-marker");
    let config = dir.join("aterm.toml");
    let apply = |step: &LaneStep| {
        settle_markers(
            &config,
            step.drift.write,
            step.remove_change_marker.as_ref(),
        );
    };
    write_debugger_marker(
        &config,
        OBSERVED_MARKER,
        &ObservedMarker {
            mode: DebuggerMode::Disabled,
            since: 100,
            origin: Origin::Unknown,
        }
        .encode(),
        crate::durable::Flush::Ordered,
    )
    .unwrap();
    let mut lane = DebuggerChangeLane::with_liveness(|_| false);
    lane.settle_records(read_records(&config, |_| false));
    apply(&lane.poll(200));
    apply(&lane.observe(probe(DebuggerMode::Disabled), 201));
    let ticket = lane
        .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
        .unwrap();
    let worker = DevtoolsJob::new(
        ticket,
        ProbeGate::on(),
        lane.slot(),
        Some(config.clone()),
        Arc::new(|| {}),
        |_| false,
    );
    run_devtools_job(worker, |_, _, on_spawn| {
        on_spawn(child(81));
        DevtoolsChangeRun::Exited {
            code: Some(1),
            stdout_head: String::new(),
            stderr_head: String::new(),
        }
    });
    apply(&lane.poll(300));
    let ended = lane.observe(probe(DebuggerMode::Disabled), 301);
    assert_eq!(ended.outcome, Some(Outcome::NotChanged(DebuggerChange::On)));
    assert_eq!(ended.remove_change_marker, Some(child(81)));
    let out = lane.ask_records();
    let read = read_records(&config, |_| false);
    assert_eq!(read.stale.map(|m| m.child), Some(child(81)));
    *out.lock().unwrap_or_else(|p| p.into_inner()) = Some(read);
    let again = lane.poll(302);
    assert_eq!(again.outcome, None, "no second outcome");
    assert_eq!(
        again.remove_change_marker, None,
        "the removal already asked for is not asked again"
    );
    assert!(
        !lane.awaiting_probe(),
        "the finished change is not re-opened"
    );
    apply(&ended);
    assert!(!dir.join(CHANGE_MARKER).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_records_read_naming_the_jobs_own_exited_child_keeps_its_marker() {
    let dir = scratch("own-marker");
    let config = dir.join("aterm.toml");
    let apply = |step: &LaneStep| {
        settle_markers(
            &config,
            step.drift.write,
            step.remove_change_marker.as_ref(),
        );
    };
    write_debugger_marker(
        &config,
        OBSERVED_MARKER,
        &ObservedMarker {
            mode: DebuggerMode::Disabled,
            since: 100,
            origin: Origin::Unknown,
        }
        .encode(),
        crate::durable::Flush::Ordered,
    )
    .unwrap();
    let mut lane = DebuggerChangeLane::with_liveness(|_| false);
    lane.settle_records(read_records(&config, |_| false));
    apply(&lane.poll(200));
    apply(&lane.observe(probe(DebuggerMode::Disabled), 201));
    let out = lane.ask_records();
    let ticket = lane
        .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
        .unwrap();
    let worker = DevtoolsJob::new(
        ticket,
        ProbeGate::on(),
        lane.slot(),
        Some(config.clone()),
        Arc::new(|| {}),
        |_| false,
    );
    run_devtools_job(worker, |_, _, on_spawn| {
        on_spawn(child(71));
        DevtoolsChangeRun::Exited {
            code: Some(0),
            stdout_head: String::new(),
            stderr_head: String::new(),
        }
    });
    let read = read_records(&config, |_| false);
    assert_eq!(read.stale.map(|m| m.child), Some(child(71)));
    *out.lock().unwrap_or_else(|p| p.into_inner()) = Some(read);

    let step = lane.poll(300);
    apply(&step);
    assert_eq!(
        step.remove_change_marker, None,
        "the change's own marker is not a stranger's"
    );
    assert!(
        dir.join(CHANGE_MARKER).exists(),
        "it outlives the taken result"
    );
    assert!(lane.awaiting_probe(), "the change waits for its re-probe");
    let ended = lane.observe(probe(DebuggerMode::Enabled), 301);
    apply(&ended);
    assert_eq!(ended.outcome, Some(Outcome::Changed(DebuggerChange::On)));
    assert_eq!(ended.drift.record, Some(DebuggerRecord::On));
    assert_eq!(ended.remove_change_marker, Some(child(71)));
    assert!(
        !dir.join(CHANGE_MARKER).exists(),
        "and goes once the re-probe is recorded"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_re_probe_judged_at_a_pending_records_read_keeps_the_marker_until_then() {
    let dir = scratch("drift-pending");
    let config = dir.join("aterm.toml");
    let apply = |step: &LaneStep| {
        settle_markers(
            &config,
            step.drift.write,
            step.remove_change_marker.as_ref(),
        );
    };
    let reread = |lane: &mut DebuggerChangeLane, now: u64| {
        lane.settle_records(read_records(&config, |_| false));
        let step = lane.poll(now);
        apply(&step);
        step
    };
    write_debugger_marker(
        &config,
        OBSERVED_MARKER,
        &ObservedMarker {
            mode: DebuggerMode::Enabled,
            since: 100,
            origin: Origin::Unknown,
        }
        .encode(),
        crate::durable::Flush::Ordered,
    )
    .unwrap();
    let mut a = DebuggerChangeLane::with_liveness(|_| false);
    let mut b = DebuggerChangeLane::with_liveness(|_| false);
    for lane in [&mut a, &mut b] {
        reread(lane, 200);
        apply(&lane.observe(probe(DebuggerMode::Enabled), 201));
    }
    apply(&b.observe(probe(DebuggerMode::Disabled), 210));
    assert!(b.records_due(210));
    assert_eq!(
        reread(&mut b, 210).drift.record,
        Some(DebuggerRecord::WasOnNowOff)
    );
    apply(&a.observe(probe(DebuggerMode::Disabled), 211));
    assert!(a.records_due(211), "A judges it against a fresh read");
    let out = a.ask_records();
    let ticket = a
        .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
        .unwrap();
    let worker = DevtoolsJob::new(
        ticket,
        ProbeGate::on(),
        a.slot(),
        Some(config.clone()),
        Arc::new(|| {}),
        |_| false,
    );
    run_devtools_job(worker, |_, _, on_spawn| {
        on_spawn(child(61));
        DevtoolsChangeRun::Exited {
            code: Some(0),
            stdout_head: String::new(),
            stderr_head: String::new(),
        }
    });
    apply(&a.poll(300));
    let ended = a.observe(probe(DebuggerMode::Enabled), 301);
    apply(&ended);
    assert_eq!(ended.outcome, Some(Outcome::Changed(DebuggerChange::On)));
    assert_eq!(ended.drift.write, None, "judged at the pending read");
    assert_eq!(
        ended.remove_change_marker, None,
        "the marker waits for that read too"
    );
    assert!(dir.join(CHANGE_MARKER).exists());

    apply(&b.observe(probe(DebuggerMode::Enabled), 302));
    let read = reread(&mut b, 302);
    assert_ne!(
        read.drift.record,
        Some(DebuggerRecord::OnOutside),
        "A's change is not \"set outside aterm\""
    );
    assert!(b.awaiting_probe(), "B re-reads it as aterm's change");
    let b_ended = b.observe(probe(DebuggerMode::Enabled), 303);
    apply(&b_ended);
    assert_eq!(b_ended.drift.record, Some(DebuggerRecord::On));

    *out.lock().unwrap_or_else(|p| p.into_inner()) = Some(read_records(&config, |_| false));
    let settled = a.poll(304);
    apply(&settled);
    assert_eq!(settled.remove_change_marker, Some(child(61)));
    let facts = a.facts(true, probe(DebuggerMode::Enabled), SelfHardening::Hardened);
    assert_eq!(
        facts.recorded.map(|m| (m.mode, m.origin)),
        Some((DebuggerMode::Enabled, Origin::Aterm))
    );
    assert!(!dir.join(CHANGE_MARKER).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_stale_change_marker_is_re_read_as_this_aterms_change() {
    for ((change, before, after, want), early) in [
        (
            DebuggerChange::On,
            DebuggerMode::Disabled,
            DebuggerMode::Enabled,
            DebuggerRecord::On,
        ),
        (
            DebuggerChange::Off,
            DebuggerMode::Enabled,
            DebuggerMode::Disabled,
            DebuggerRecord::Off,
        ),
    ]
    .into_iter()
    .flat_map(|case| [(case, false), (case, true)])
    {
        let mut lane = DebuggerChangeLane::with_liveness(|_| false);
        if early {
            assert_eq!(
                lane.observe(probe(after), 150).drift,
                DriftStep::default(),
                "{change:?}: nothing is judged before the records"
            );
        }
        let stale = ChangeMarker::new(change, child(31), 1).unwrap();
        lane.settle_records(RecordsRead {
            stale: Some(stale),
            observed: Some(ObservedMarker {
                mode: before,
                since: 100,
                origin: Origin::Unknown,
            }),
            ..RecordsRead::default()
        });
        let step = lane.poll(200);
        assert!(step.invalidate_probe, "{change:?}");
        assert_eq!(
            step.drift,
            DriftStep::default(),
            "{change:?} (early {early}): the change's own re-probe judges it, never a probe from before"
        );
        assert_eq!(
            step.remove_change_marker, None,
            "{change:?}: its marker stays until its change is recorded"
        );
        assert!(lane.awaiting_probe(), "{change:?}: only a probe ends it");
        assert!(lane.needs_poll(), "{change:?}");
        let ended = lane.observe(probe(after), 300);
        assert_eq!(ended.outcome, Some(Outcome::Changed(change)));
        assert_eq!(
            ended.remove_change_marker,
            Some(child(31)),
            "{change:?}: and goes after the record that step writes"
        );
        assert_eq!(ended.drift.record, Some(want), "{change:?} (early {early})");
        assert_eq!(
            ended.drift.write.map(|m| m.origin),
            Some(Origin::Aterm),
            "{change:?} (early {early})"
        );
        assert_eq!(lane.drift.drift_since(), None, "{change:?}: no drift line");
    }
    let mut lane = DebuggerChangeLane::with_liveness(|_| false);
    lane.settle_records(RecordsRead {
        stale: ChangeMarker::new(DebuggerChange::On, child(32), 1),
        ..RecordsRead::default()
    });
    lane.poll(1);
    let ended = lane.observe(probe(DebuggerMode::Disabled), 2);
    assert_eq!(ended.outcome, Some(Outcome::NotChanged(DebuggerChange::On)));
}

#[test]
fn a_lost_debugger_result_is_re_read() {
    let mut lane = DebuggerChangeLane::with_liveness(|_| false);
    lane.settle_records(RecordsRead::default());
    assert!(lane.needs_poll(), "the records are waiting");
    assert_eq!(lane.poll(1), LaneStep::default());
    assert!(!lane.needs_poll());
    assert_eq!(
        lane.next_deadline(Instant::now()),
        None,
        "idle parks forever"
    );
    lane.observe(probe(DebuggerMode::Disabled), 2);
    assert!(lane.records_due(2));
    lane.settle_records(RecordsRead::default());
    assert_eq!(
        lane.poll(2).drift.write.map(|m| m.mode),
        Some(DebuggerMode::Disabled)
    );
    let job = lane
        .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
        .unwrap()
        .job();
    let now = Instant::now();
    assert_eq!(lane.next_deadline(now), Some(now + RESULT_RECHECK));
    let slot = lane.slot();
    slot.publish_spawned(job, child(11));
    assert_eq!(lane.poll(3), LaneStep::default());
    assert!(lane.flow().in_flight().is_some());
    let now = Instant::now();
    assert_eq!(
        lane.next_deadline(now),
        Some(now + RESULT_RECHECK),
        "a child in flight keeps the park re-reading"
    );
    assert!(!lane.result_unread());
    slot.publish_result(
        job,
        DevtoolsChangeRun::Exited {
            code: Some(0),
            stdout_head: String::new(),
            stderr_head: String::new(),
        },
    );
    slot.worker_gone(job);
    assert!(lane.result_unread(), "the result waits unread");
    let step = lane.poll(4);
    assert!(
        step.invalidate_probe,
        "the rule may have changed under the cache"
    );
    assert_eq!(
        step.remove_change_marker, None,
        "the marker outlives the taken result until the change is recorded"
    );
    assert!(lane.awaiting_probe(), "only a probe ends it");
    assert!(
        lane.needs_poll(),
        "and the park keeps re-reading until it does"
    );
    let ended = lane.observe(probe(DebuggerMode::Enabled), 5);
    assert_eq!(ended.outcome, Some(Outcome::Changed(DebuggerChange::On)));
    assert_eq!(ended.drift.record, Some(DebuggerRecord::On));
    assert_eq!(
        ended.remove_change_marker,
        Some(child(11)),
        "it goes with the record, and only if it names the child"
    );
    assert!(!lane.needs_poll());

    static ALIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
    let mut successor =
        DebuggerChangeLane::with_liveness(|_| ALIVE.load(std::sync::atomic::Ordering::SeqCst));
    let marker = ChangeMarker::new(DebuggerChange::On, child(12), 5).unwrap();
    let was_off = ObservedMarker {
        mode: DebuggerMode::Disabled,
        since: 1,
        origin: Origin::Unknown,
    };
    successor.settle_records(RecordsRead {
        inherited: Some(marker),
        observed: Some(was_off),
        ..RecordsRead::default()
    });
    successor.poll(6);
    assert_eq!(
        successor
            .flow()
            .in_flight()
            .map(|(c, _, _, inherited)| (c, inherited)),
        Some((DebuggerChange::On, true))
    );
    assert_eq!(
        successor.begin(DebuggerChange::On, OwnerAnswer::yes_for_test()),
        Err(Refusal::InFlight)
    );
    assert!(
        !successor.poll(7).invalidate_probe,
        "still running: still in flight"
    );
    ALIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    let step = successor.poll(8);
    assert!(step.invalidate_probe);
    assert_eq!(step.remove_change_marker, None);
    assert!(successor.awaiting_probe());
    let ended = successor.observe(probe(DebuggerMode::Disabled), 9);
    assert_eq!(ended.outcome, Some(Outcome::NotChanged(DebuggerChange::On)));
    assert_eq!(ended.remove_change_marker, Some(child(12)));
}

#[test]
fn a_worker_that_ends_without_a_result_is_lost_and_a_not_due_read_is_observed() {
    let mut lane = DebuggerChangeLane::with_liveness(|_| false);
    lane.settle_records(RecordsRead {
        observed: Some(ObservedMarker {
            mode: DebuggerMode::Disabled,
            since: 1,
            origin: Origin::Unknown,
        }),
        ..RecordsRead::default()
    });
    lane.poll(1);
    lane.observe(probe(DebuggerMode::Disabled), 1);
    let job = lane
        .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
        .unwrap()
        .job();
    lane.slot().worker_gone(job);
    let step = lane.poll(2);
    assert_eq!(step.outcome, Some(Outcome::NotStarted(DebuggerChange::On)));
    assert_eq!(lane.flow().phase(), Phase::Idle);
    let job = lane
        .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
        .unwrap()
        .job();
    lane.slot().publish_spawned(job, child(5));
    lane.poll(3);
    lane.slot().worker_gone(job);
    assert_eq!(lane.poll(4).remove_change_marker, None);
    assert!(lane.awaiting_probe());
    assert_eq!(
        lane.observe(probe(DebuggerMode::Disabled), 5)
            .remove_change_marker,
        Some(child(5))
    );
    let job = lane
        .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
        .unwrap()
        .job();
    lane.slot()
        .publish_refused(job, WorkerRefusal::PeerChanging);
    assert_eq!(
        lane.poll(6).outcome,
        Some(Outcome::PeerInFlight(DebuggerChange::On))
    );
    assert_eq!(lane.flow().phase(), Phase::Idle);
    let job = lane
        .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
        .unwrap()
        .job();
    lane.slot().publish_refused(job, WorkerRefusal::NoLock);
    assert_eq!(
        lane.poll(7).outcome,
        Some(Outcome::NotStarted(DebuggerChange::On))
    );
    let job = lane
        .begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
        .unwrap()
        .job();
    lane.slot().publish_result(
        job,
        DevtoolsChangeRun::NotDue {
            observed: probe(DebuggerMode::Unrecognized),
        },
    );
    let step = lane.poll(8);
    assert_eq!(
        step.outcome,
        Some(Outcome::NotDue(
            DebuggerChange::On,
            DebuggerMode::Unrecognized
        ))
    );
    assert!(step.invalidate_probe);
    assert_eq!(lane.flow().observed(), DebuggerMode::Unrecognized);
    assert_eq!(
        lane.begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
            .map(|_| ()),
        Err(Refusal::State(
            DebuggerChange::On,
            DebuggerMode::Unrecognized
        ))
    );
}

#[test]
fn the_facts_offer_a_button_only_where_pressing_can_help() {
    let zero = DebuggerFacts::default();
    assert!(!zero.offers(DebuggerChange::On) && !zero.offers(DebuggerChange::Off));
    let mut lane = DebuggerChangeLane::with_liveness(|_| false);
    let disabled = probe(DebuggerMode::Disabled);
    let unread = lane.facts(true, disabled, SelfHardening::Hardened);
    assert!(
        unread.busy && !unread.offers(DebuggerChange::On),
        "records unread"
    );
    lane.settle_records(RecordsRead::default());
    lane.poll(1);
    let facts = lane.facts(true, disabled, SelfHardening::NotHardened);
    assert!(
        facts.offers(DebuggerChange::On),
        "O8: offered unhardened too"
    );
    assert!(facts.not_hardened());
    assert!(!facts.offers(DebuggerChange::Off));
    assert!(
        !lane
            .facts(false, disabled, SelfHardening::Hardened)
            .offers(DebuggerChange::On)
    );
    let outsider = DebuggerProbe {
        member: DebuggerMember::NotMember,
        ..disabled
    };
    assert!(
        !lane
            .facts(true, outsider, SelfHardening::Hardened)
            .offers(DebuggerChange::On)
    );
    let enabled = probe(DebuggerMode::Enabled);
    let on = lane.facts(
        true,
        DebuggerProbe {
            member: DebuggerMember::NotMember,
            ..enabled
        },
        SelfHardening::Hardened,
    );
    assert!(
        on.offers(DebuggerChange::Off),
        "turning off only reduces exposure"
    );
    for mode in [DebuggerMode::Unrecognized, DebuggerMode::Unknown] {
        let facts = lane.facts(true, probe(mode), SelfHardening::Hardened);
        assert!(!facts.offers(DebuggerChange::On) && !facts.offers(DebuggerChange::Off));
    }
    lane.observe(disabled, 2);
    lane.begin(DebuggerChange::On, OwnerAnswer::yes_for_test())
        .unwrap();
    lane.slot().publish_spawned(1, child(3));
    lane.poll(9);
    let waiting = lane.facts(true, disabled, SelfHardening::Hardened);
    assert!(waiting.busy && !waiting.offers(DebuggerChange::On));
    assert_eq!(
        waiting.in_flight,
        Some(InFlightFacts {
            change: DebuggerChange::On,
            since: 9,
            inherited: false
        })
    );
}
