// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `sys` host: the workers' copy, pulls, deltas and cursors, and the
//! machine wait, each on a private [`Shared`] (never the process's).

use super::*;
use aterm_vitals::model::{
    ClusterView, CoreView, CpuView, MemSample, MemView, Pressure, ScopeView,
};
use aterm_vitals::sys::json::contract_violations;

/// A snapshot two readings of this process made (the live machine).
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

/// A small synthetic snapshot: 4 cores at `busy` each, memory normal, no GPU.
fn synthetic(seq: u64, busy: f32) -> Snapshot {
    let flat = aterm_vitals::TrendView::flat;
    Snapshot {
        at_ns: seq * 1_000_000_000,
        seq,
        cpu: Some(CpuView {
            cores: (0..4).map(|_| CoreView { load: flat(busy) }).collect(),
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
        },
        tabs: vec![TabFact {
            key: 0,
            sid: "s-test".into(),
            window: Some(0),
            tab: Some(1),
        }],
        roots: Vec::new(),
        disk: None,
        volume: None,
        caller_panes: None,
        home: None,
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
    let shared = Shared::new();
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
        assert!(body.starts_with("{\"schema\":1,\"kind\":\"sys."), "{body}");
        let q = SysQuery::parse(&format!("{} --json", view.word())).unwrap();
        let (held, state) = shared.fresh(Duration::from_secs(3600), false);
        let g = gathered();
        let ctx = Ctx {
            snap: held.as_ref().map(|h| h.snap.as_ref()),
            epoch: *EPOCH,
            as_of_unix_ms: 0,
            age_ms: 0,
            state,
            machine: &Machine::default(),
            target: &g.target,
            tabs: &g.tabs,
            caller_panes: None,
            owner_class: true,
            volume: None,
            roots: &[],
            home: None,
            events: &[],
            gap: None,
            disk: None,
            head_seq: 0,
        };
        let v = contract_violations(&render_json(&q, &ctx));
        assert!(v.is_empty(), "{view:?}: {v:?}");
    }
}

#[test]
fn a_since_cursor_answers_a_delta_and_its_gaps() {
    let shared = Shared::new();
    shared.publish_for_test(synthetic(3, 0.1), Vec::new());
    let first = ask(&shared, "global");
    let cursor = cursor_of(&first);
    assert_eq!(cursor, format!("{:x}.3", *EPOCH));
    // The same snapshot again: every value line folds into one `same` line.
    let again = ask(&shared, &format!("global since={cursor}"));
    let header = again.lines().next().unwrap();
    assert!(header.ends_with(" delta=1"), "{again}");
    assert!(
        again.lines().nth(1).unwrap().starts_with("same "),
        "{again}"
    );
    // A moved figure stays; the others fold.
    shared.publish_for_test(synthetic(4, 0.6), Vec::new());
    let moved = ask(&shared, &format!("global since={cursor}"));
    assert!(moved.lines().any(|l| l.starts_with("cpu ")), "{moved}");
    assert!(moved.lines().any(|l| l.starts_with("same ")), "{moved}");
    // Another view's reply at that cursor is not kept: a full reply, gap evicted.
    let other = ask(&shared, &format!("cores since={cursor}"));
    assert!(other.lines().any(|l| l == "gap why=evicted"), "{other}");
    assert!(!other.lines().next().unwrap().contains("delta=1"));
    // …nor is the same view printed to another shape (`top=`).
    let shorter = ask(&shared, &format!("global top=1 since={cursor}"));
    assert!(shorter.lines().any(|l| l == "gap why=evicted"), "{shorter}");
    // Another start's cursor: reset.
    let reset = ask(&shared, &format!("global since={:x}.3", *EPOCH ^ 1));
    assert!(reset.lines().any(|l| l == "gap why=reset"), "{reset}");
    // A cursor ahead of the head.
    let ahead = ask(&shared, &format!("global since={:x}.99", *EPOCH));
    assert_eq!(ahead, format!("ERR bad since head={:x}.4\n", *EPOCH));
}

#[test]
fn events_since_a_cursor_print_and_the_ring_says_when_it_lost_them() {
    let shared = Shared::new();
    let mut s = synthetic(2, 0.1);
    s.events = vec![aterm_vitals::events::Event {
        seq: 2,
        at_ns: s.at_ns,
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
    // Fill the ring past its size: a cursor before what it lost gets no events.
    for seq in 3..(3 + EVENTS_CAP as u64 + 1) {
        let mut s = synthetic(seq, 0.1);
        s.events = vec![aterm_vitals::events::Event {
            seq,
            at_ns: s.at_ns,
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

/// The pull's order of things, on a copy whose gap is an hour (so a second
/// read is certainly within it) and whose wait is a one-minute hang detector:
/// nothing here depends on how fast the probe thread is scheduled.
#[test]
fn a_pull_asks_the_probe_once_per_gap_and_takes_its_reading() {
    let shared = Arc::new(Shared::paced(
        Duration::from_secs(3600),
        Duration::from_secs(60),
    ));
    // No probe: what is held, at once.
    let (held, state) = shared.fresh(Duration::ZERO, true);
    assert!(held.is_none());
    assert_eq!(state, Freshness::Warming);
    shared.publish_for_test(synthetic(1, 0.1), Vec::new());
    let (held, state) = shared.fresh(Duration::ZERO, true);
    assert_eq!(held.map(|h| h.snap.seq), Some(1));
    assert_eq!(state, Freshness::Stale);
    // A probe that answers each pull with the next snapshot.
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
    // Within the gap a second forced read takes the reading that pull left.
    let (held, _) = shared.fresh(Duration::ZERO, true);
    assert_eq!(held.map(|h| h.snap.seq), Some(2));
    // A fresh enough snapshot asks for nothing.
    let (held, _) = shared.fresh(Duration::from_secs(60), false);
    assert_eq!(held.map(|h| h.snap.seq), Some(2));
    shared.lock().tx = None;
    assert_eq!(probe.join().unwrap(), 1, "one pull for three reads");
}

#[test]
fn the_machine_wait_meets_refuses_times_out_and_sees_its_session_exit() {
    let shared = Shared::new();
    shared.publish_for_test(
        synthetic(3, 0.1),
        vec![PaneFact {
            key: 0,
            window: 0,
            tab: 1,
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
        quiet.contains(" skipped=gpu:unsupported,throttle:unsupported,swap:not_sampled "),
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

    // The last low-CPU reading cannot meet a wait once it is stale and no
    // probe answers. A fresh replacement may meet it, on its own cursor.
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

    // A failed refresh also breaks a hold. Advance the producer at each
    // cancellation check: fresh 10, stale 12, fresh 13, fresh 15. Only the
    // last two may supply the two-second hold; this uses no wall-clock wait.
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

/// A snapshot whose jobs view is `view`.
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
    })
}

/// §13.7: a named job that left the engine's view after it exited answers
/// from the event ring (its exit meets `done`; any other wait is `ERR
/// gone`), and one neither the view nor the ring knows is gone once the
/// engine passed twice more, never parked to the timeout. NEGATIVE CONTROL:
/// a job still in the view, and a pass that did not happen (the same view
/// again), park.
#[test]
fn a_named_job_that_left_the_view_answers_from_the_ring_or_is_gone() {
    use aterm_vitals::events::{Event, EventKind};
    use aterm_vitals::jobs::{Class, Verdict};
    // The ring's `exited` event, through the copy the workers read.
    let shared = Shared::new();
    let empty = jobs_view(Vec::new());
    let mut s = with_jobs(2, &empty);
    s.events = vec![Event {
        seq: 2,
        at_ns: s.at_ns,
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
    // A job nobody knows: the arm's pass and one more park; the second new
    // pass says gone. The same view again is not a pass.
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
    // A job in the view parks however many passes go by.
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

/// `scope=aterm` reads every pane; the default reads the caller's tab.
#[test]
fn a_machine_wait_reads_its_tab_or_with_scope_aterm_every_tab() {
    let panes = [
        PaneFact {
            key: 1,
            window: 1,
            tab: 1,
        },
        PaneFact {
            key: 2,
            window: 1,
            tab: 1,
        },
        PaneFact {
            key: 3,
            window: 1,
            tab: 2,
        },
        PaneFact {
            key: 4,
            window: 2,
            tab: 1,
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

/// THE SLOTS WAIT over the snapshots the probe thread fills (`vitals_host`'s
/// one slot reader): parked while the one slot is held, met on the snapshot
/// that frees it (`slots=1/1`), refused where the reader could not read the
/// slots, and `for=` held only across stamps that span it. NEGATIVE CONTROL:
/// a free slot an orphaned build still fills (discounted) parks.
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

/// A wait arms the slots' few-second cadence only when it names them; the
/// gauge counts every armed wait and is at rest once the last is dropped.
/// NEGATIVE CONTROL: a wait on the machine's other terms arms nothing.
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
    let shared = Arc::new(Shared::new());
    shared.publish_for_test(synthetic(1, 0.1), Vec::new());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let feeder = {
        let (shared, stop) = (Arc::clone(&shared), Arc::clone(&stop));
        std::thread::spawn(move || {
            // Snapshots whose stamps are a second apart, until the wait answers.
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

/// The seam's honest arm: each pane's folder is one `cwd` root, two panes in
/// one folder are one root, and no size or balance is claimed.
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
    let (roots, disk) = disk_roots(ScopeArg::Aterm, &[1, 2, 3], &cwds, &|_| None);
    assert_eq!(disk, None);
    assert_eq!(roots.len(), 2);
    assert!(
        roots
            .iter()
            .all(|r| r.kinds == ["cwd"] && r.size == RootSize::NotSampled)
    );
    assert_eq!(roots[0].keys, [1, 2], "both panes own the shared folder");
    assert_eq!(roots[1].tab, Some(2));
}

/// THE WALKER'S ARM: a pane's report becomes `sys` roots: a complete walk
/// with its subtrees and growth, a running one as a lower bound beside its
/// last complete figure, a refused and a too-broad root by their unavailable
/// words; who claims each folder (for the per-root path rule) and the tab
/// numbers; the report's own balance. A pane the walker was not told of yet
/// reads its folder as queued.
#[test]
fn the_walkers_report_becomes_roots_sizes_and_a_balance() {
    use crate::diskwalk_host::{
        Claim, Complete, Detail, DirRow, DiskReport, DiskScope, Growth, RootKind, RootRecord,
        RootRow, RootState, WalkerState,
    };
    use aterm_vitals::sys::RootSizes;
    let now = Instant::now();
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
    let row =
        |n: usize, path: &str, kinds: &[RootKind], state, complete, shared: Vec<u64>| RootRow {
            n,
            claim: Claim {
                path: path.into(),
                kinds: kinds.iter().copied().collect(),
                linked_worktree: false,
            },
            inside: None,
            shared_with: shared,
            counted_in: Some(1),
            shared_alloc: 0,
            record: RootRecord {
                path: path.into(),
                state,
                id: None,
                partial: None,
                walked_entries: 0,
                complete,
                level: Vec::new(),
                read_from: None,
                asked: None,
            },
        };
    let mut walking = row(
        2,
        "/w/repo/target",
        &[RootKind::Build],
        RootState::Walking,
        Some(done(1_000, Vec::new())),
        Vec::new(),
    );
    walking.inside = Some(1);
    walking.record.walked_entries = 120;
    walking.record.partial = Some(aterm_diskwalk::Sums {
        alloc_fair: 400,
        ..aterm_diskwalk::Sums::default()
    });
    let report = DiskReport {
        scope: DiskScope::Tab(1),
        label: Some("Tab 1".into()),
        roots: vec![
            row(
                1,
                "/w/repo",
                &[RootKind::Cwd, RootKind::Repo],
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
                vec![2],
            ),
            walking,
            row(
                3,
                "/Users//a/Documents",
                &[RootKind::Work],
                RootState::NotMeasured(aterm_diskwalk::Refusal::Protected),
                None,
                Vec::new(),
            ),
            row(
                4,
                "/",
                &[RootKind::Work],
                RootState::OneLevel,
                None,
                Vec::new(),
            ),
        ],
        total_bytes: 9_000,
        exclusive_bytes: 4_000,
        shared_bytes: 5_000,
        walking: true,
        warnings: Vec::new(),
        growth: None,
        unknown_tab: false,
        walker: WalkerState::Running,
    };
    let tab_of = |k: u64| Some(usize::try_from(k).unwrap_or(0));
    let (roots, totals) = walker_roots(std::slice::from_ref(&report), &[], &tab_of, now);
    assert_eq!(roots.len(), 4);
    assert_eq!(roots[0].kinds, ["cwd", "repo"]);
    assert_eq!(
        roots[0].keys,
        [1, 2],
        "the claimers: the pane and who shares it"
    );
    assert_eq!(
        (roots[0].tab, roots[0].shared_with.as_slice()),
        (Some(1), &[2][..])
    );
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
    assert_eq!(roots[1].inside, Some(1));
    match &roots[1].size {
        RootSize::Walking {
            walked_entries: 120,
            partial_alloc: Some(400),
            last: Some(last),
        } => assert_eq!(last.alloc, 1_000),
        other => panic!("{other:?}"),
    }
    assert_eq!(roots[2].size, RootSize::NotMeasured("protected"));
    assert_eq!(roots[3].size, RootSize::NotMeasured("too_broad"));
    let t = totals.expect("a walker's balance");
    assert_eq!(
        (t.total, t.exclusive, t.shared, t.walking),
        (9_000, 4_000, 5_000, true)
    );
    // A pane whose facts have not reached the walker: its folder, queued.
    let unknown = DiskReport {
        scope: DiskScope::Tab(5),
        roots: Vec::new(),
        unknown_tab: true,
        total_bytes: 0,
        exclusive_bytes: 0,
        shared_bytes: 0,
        walking: false,
        ..report
    };
    let cwds = [PaneCwd {
        key: 5,
        tab: Some(3),
        path: "/w/new".into(),
    }];
    let (roots, totals) = walker_roots(&[unknown], &cwds, &tab_of, now);
    assert_eq!(roots.len(), 1);
    assert_eq!(
        (roots[0].path.as_str(), roots[0].keys.as_slice()),
        ("/w/new", &[5][..])
    );
    assert!(matches!(
        roots[0].size,
        RootSize::Walking {
            walked_entries: 0,
            ..
        }
    ));
    assert!(totals.is_some_and(|t| t.walking && t.total == 0));
}

/// `EVENT <local> job …`'s words: the verdict, the id, the class and the rev,
/// then the task id (a plain word only) and the exit code when known.
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
    // A task id that is not a plain word is left out, never printed.
    assert_eq!(
        job_event_payload(Verdict::Quiet, "a1", Class::Waiting, 2, Some("x y;z"), None),
        "quiet job=a1 class=waiting rev=2"
    );
}

/// The brief's jobs line goes last before the trailer's `gap`/`more=`, and
/// the header counts it, keeps the budget asked and says the whole reply's
/// token estimate.
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
    // No trailer (the one-line form): unchanged.
    let one = "OK 1 brief sid=s-1 v=2 as_of=9 ep=1 budget=5 used=12\n- sh r1 · -";
    assert_eq!(splice_brief_line(one, "jobs 1 job", 5), one);
}

/// A jobs view that is not parked speaks for now only while it is at most a
/// minute old; a parked one (demand idle) for as long as its reader runs.
/// NEGATIVE CONTROL: the parked view with its reader stopped says nothing
/// past the minute.
#[test]
fn a_jobs_view_older_than_a_minute_says_nothing_of_now_unless_parked() {
    let view = Arc::new(aterm_vitals::jobs::JobsView {
        demand: aterm_vitals::jobs::Demand::Fast,
        ..aterm_vitals::jobs::JobsView::default()
    });
    let now = Instant::now();
    assert!(fresh_jobs(Some((Arc::clone(&view), now)), now, true).is_some());
    let old = now
        .checked_sub(crate::vitals_jobs::JOBS_FRESH + Duration::from_secs(1))
        .expect("a clock past a minute");
    assert!(fresh_jobs(Some((view, old)), now, true).is_none());
    assert!(fresh_jobs(None, now, true).is_none());
    let parked = Arc::new(aterm_vitals::jobs::JobsView::default());
    assert!(fresh_jobs(Some((Arc::clone(&parked), old)), now, true).is_some());
    assert!(fresh_jobs(Some((parked, old)), now, false).is_none());
}
