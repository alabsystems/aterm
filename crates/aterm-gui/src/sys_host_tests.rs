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
fn a_why_since_cursor_folds_only_against_the_same_reply() {
    let shared = Shared::new();
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
    let (held, state) = shared.fresh_why(hour, false);
    assert_eq!(
        (held.map(|h| h.snap.seq), state),
        (Some(1), Freshness::Fresh)
    );
    shared.publish_for_test(with_split(7300, Some(1)), Vec::new());
    let (held, _) = shared.fresh(hour, false);
    assert_eq!(held.map(|h| h.snap.seq), Some(7300));
    let (held, state) = shared.fresh_why(hour, false);
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
    let (held, state) = shared.fresh_why(hour, false);
    assert_eq!(
        (held.map(|h| h.snap.seq), state),
        (Some(7302), Freshness::Fresh)
    );
    shared.lock().tx = None;
    assert_eq!(pulls.join().unwrap(), 2);

    let shared = Arc::new(Shared::paced(hour, Duration::from_millis(200)));
    let pulls = probe(&shared, u64::MAX);
    shared.publish_for_test(with_split(7300, Some(1)), Vec::new());
    let (held, state) = shared.fresh_why(hour, false);
    assert!(held.is_some_and(|h| h.snap.seq >= 7300));
    assert_eq!(state, Freshness::Stale);
    shared.lock().tx = None;
    assert_eq!(pulls.join().unwrap(), 1);
    let mut off = synthetic(7400, 0.1);
    off.coal_lane = aterm_vitals::explain::CoalLane::Off;
    shared.publish_for_test(off, Vec::new());
    let (held, state) = shared.fresh_why(hour, false);
    assert_eq!(
        (held.map(|h| h.snap.seq), state),
        (Some(7400), Freshness::Fresh)
    );
}

#[test]
fn a_why_arms_the_split_while_it_waits() {
    static ARMED: Arming = Arming::new();
    for (text, armed) in [("why fresh=0", true), ("global fresh=0", false)] {
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
                    seen.push(ARMED.armed());
                    // A forced `sys why` waits for a CPU Stats sample too.
                    let mut snap = with_split(7301, Some(7301));
                    snap.hardware = Some(aterm_vitals::hardware::Sample::unavailable("warming"));
                    shared.publish_for_test(snap, Vec::new());
                }
                seen
            })
        };
        shared.publish_for_test(with_split(7300, Some(1)), Vec::new());
        let q = SysQuery::parse(text).expect("query");
        let (held, state) = shared.fresh_for(&q, &ARMED, &Arming::new(), &Arming::new());
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
                    shared.publish_for_test(with_hardware(seq, armed), Vec::new());
                }
                seen
            })
        };
        shared.publish_for_test(with_hardware(7300, held_sampled), Vec::new());
        let q = SysQuery::parse(text).expect("query");
        let (held, state) = shared.fresh_for(&q, &Arming::new(), &Arming::new(), &HARDWARE);
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
                    shared.publish_for_test(with_hardware(7301, HARDWARE.armed()), Vec::new());
                }
            })
        };
        shared.publish_for_test(with_hardware(7300, false), Vec::new());
        let q = SysQuery::parse(text).expect("query");
        let (held, state) = shared.fresh_for(&q, &Arming::new(), &Arming::new(), &HARDWARE);
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
                        observed,
                        jobs_at,
                        if left { jobs_at } else { None },
                        true,
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
            now,
            old,
            if left { old } else { None },
            true,
        );
        let q = SysQuery::parse(text).expect("query");
        let (held, state) = shared.fresh_for(&q, &Arming::new(), &JOBS, &Arming::new());
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
    let shared = Shared::new();
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
    let shared = Shared::new();
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
    })
}

#[test]
fn a_named_job_that_left_the_view_answers_from_the_ring_or_is_gone() {
    use aterm_vitals::events::{Event, EventKind};
    use aterm_vitals::jobs::{Class, Verdict};
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
    let shared = Arc::new(Shared::new());
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
            snap: Arc::new(Snapshot::default()),
            unix_ms: 0,
            at: Instant::now(),
            jobs_at: None,
            left_at: None,
        })
    };
    let reading = || {
        Some(aterm_vitals::services::Reading {
            total: 128 << 30,
            ..aterm_vitals::services::Reading::default()
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
            snap: Arc::new(warm.clone()),
            unix_ms: 0,
            at: Instant::now(),
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
