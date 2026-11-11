// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The live box (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3): its tick is the largest
/// reading of the last 10 s and its cap the share of their read time over, the larger
/// series' on a two-series row; a reading older than the window or ahead of now counts for
/// neither.
/// CONTROL: those left out are the minute's largest, so counting them would change both.
#[test]
fn the_live_box_ticks_its_ten_second_max_and_caps_its_over_share() {
    const NOW: i64 = 1_792_000_000_000;
    let reading = |back_s: i64, v: f64, over: bool| Reading {
        at_ms: NOW - back_s * 1000,
        v,
        over,
        w_ms: 1_000,
    };
    let recent: Arc<[Reading]> = vec![
        reading(40, 9.5, true),
        reading(12, 9.0, true),
        reading(9, 2.0, false),
        reading(6, 6.0, true),
        reading(3, 3.0, false),
        reading(0, 4.0, true),
        reading(-3, 9.9, true),
    ]
    .into();
    let view = ResourceView {
        capacity: Some(10.0),
        recent,
        ..ResourceView::default()
    };
    let (peak, over) = view.live_box(NOW);
    assert!(
        peak.is_some_and(|p| (p - 0.6).abs() < 1e-6),
        "the 10 s max: {peak:?}"
    );
    assert!(
        (over - 0.5).abs() < 1e-6,
        "two of four readings over: {over}"
    );
    let (earlier, earlier_over) = view.live_box(NOW - 2_000);
    assert!(
        earlier.is_some_and(|p| (p - 0.6).abs() < 1e-6),
        "the window ending 2 s ago: {earlier:?}"
    );
    assert!(
        (earlier_over - 1.0 / 3.0).abs() < 1e-6,
        "one of three: {earlier_over}"
    );
    let mut two = view.clone();
    two.second = Some(Series {
        capacity: Some(4.0),
        recent: vec![reading(2, 3.0, true)].into(),
        ..Series::default()
    });
    let (peak2, over2) = two.live_box(NOW);
    assert!(
        peak2.is_some_and(|p| (p - 0.75).abs() < 1e-6),
        "the larger series' max: {peak2:?}"
    );
    assert!(
        (over2 - 1.0).abs() < 1e-6,
        "the larger series' over share: {over2}"
    );
}

/// A1 §3 as F4′ amends it: the live box's over cap is the share of its 10 s's read time that
/// was over, each reading weighed by the time it stands for, as its column (the 10 s mean)
/// and every other box are: a 5 s reading over beside a 1 s one not over caps five sixths.
/// CONTROL: the same two readings each standing for a second cap half, so the law tells the
/// time share from the count share.
#[test]
fn the_live_box_caps_the_share_of_its_read_time_over() {
    const NOW: i64 = 1_792_000_000_000;
    let view = |w_over: u32, w_not: u32| ResourceView {
        capacity: Some(10.0),
        recent: vec![
            Reading {
                at_ms: NOW - 6_000,
                v: 9.0,
                over: true,
                w_ms: w_over,
            },
            Reading {
                at_ms: NOW,
                v: 1.0,
                over: false,
                w_ms: w_not,
            },
        ]
        .into(),
        ..ResourceView::default()
    };
    let (_, over) = view(5_000, 1_000).live_box(NOW);
    assert!(
        (over - 5.0 / 6.0).abs() < 1e-6,
        "five of six seconds: {over}"
    );
    let (_, count) = view(1_000, 1_000).live_box(NOW);
    assert!((count - 0.5).abs() < 1e-6, "CONTROL: one of two: {count}");
}

/// A kernel record's cut name (16 bytes) and a client's are the sweep's whole one or its
/// marked one, and the record's cut through a character is too; a whole name that begins
/// another, or a cut one that begins another program's, is not. NEGATIVE CONTROL: the rule
/// the birth reader held before whole names (equal, or the record's first 15 bytes) fails
/// the whole name the sweep now gives.
#[test]
fn two_readers_names_match_whole_cut_or_marked() {
    assert!(same_process_name("mds_stores", "mds_stores"));
    assert!(same_process_name(
        "com.apple.WebKit",
        "com.apple.WebKit.WebContent"
    ));
    assert!(same_process_name(
        "com.apple.WebKit.WebContent",
        "com.apple.WebKit"
    ));
    assert!(same_process_name("SetStoreUpdateSe", "SetStoreUpdateS"));
    assert!(same_process_name(
        "com.apple.WebKit",
        "com.apple.WebKi\u{2026}"
    ));
    let record = String::from_utf8_lossy(&"aПриложение".as_bytes()[..16]).into_owned();
    assert_eq!(record, "aПриложе\u{FFFD}");
    assert!(same_process_name(&record, "aПриложение"));
    assert!(same_process_name("Приложен", "Приложе\u{2026}"));
    assert!(same_process_name("Приложен", "Приложе\u{FFFD}"));
    assert!(!same_process_name("Приложен", "Приложе"));
    assert!(!same_process_name("mds_stores", "mds"));
    assert!(!same_process_name("mds", "mds_stores"));
    assert!(!same_process_name("launchd", "aterm_gui-0123456789abcdef"));
    assert!(!same_process_name(
        "com.apple.WebKi\u{2026}",
        "com.apple.Safari"
    ));
    let before = |record: &str, swept: &str| {
        record == swept || record.get(..15).is_some_and(|cut| cut == swept)
    };
    assert!(
        !before("com.apple.WebKit", "com.apple.WebKit.WebContent"),
        "CONTROL"
    );
}

fn proc(pid: u32, ppid: u32, cpu_ms: u64, foot_mib: u64, name: &str) -> ProcSample {
    ProcSample {
        pid,
        start: Some(1),
        ppid,
        cpu_ns: Some(cpu_ms * 1_000_000),
        footprint: Some(foot_mib << 20),
        name: name.into(),
        ..ProcSample::default()
    }
}

fn tab(key: u64, shell: u32, label: &str, program: &str) -> TabRoot {
    TabRoot {
        key,
        tab: key,
        shell_pid: shell,
        label: label.into(),
        program: program.into(),
        tab_id: Some(key),
        agent_leader: None,
    }
}

#[test]
fn delegated_jobs_charge_two_tabs_once_without_their_shared_daemon() {
    use crate::delegated::ProcessKey;
    let member = |pid| ProcessKey {
        pid,
        start: u64::from(pid),
    };
    let processes = |second: bool| {
        [
            (900, 1, 800, 300),
            (901, 900, 50, 10),
            (902, 901, 200, 20),
            (903, 900, 100, 30),
            (904, 903, 400, 40),
        ]
        .into_iter()
        .map(|(pid, ppid, cpu, mem)| {
            let mut sample = proc(pid, ppid, if second { cpu } else { 0 }, mem, "work");
            sample.start = Some(u64::from(pid));
            sample
        })
        .collect()
    };
    let mut sample = Sample {
        at_ns: 1_000_000_000,
        at_unix_ms: 1000,
        mono_ms: 1000,
        cadence_ms: 1000,
        self_pid: 100,
        tabs: vec![
            tab(1, 200, "Tab 1", "codex"),
            tab(2, 300, "Tab 2", "claude"),
        ],
        procs: Some(processes(false)),
        delegated: vec![
            DelegatedJob {
                root: member(901),
                tab: Some(1),
                members: vec![member(901), member(902)],
            },
            DelegatedJob {
                root: member(903),
                tab: Some(2),
                members: vec![member(903), member(904)],
            },
        ],
        ..Sample::default()
    };
    let mut vitals = Vitals::new();
    vitals.absorb(&sample);
    sample.at_ns += 1_000_000_000;
    sample.at_unix_ms += 1000;
    sample.mono_ms += 1000;
    sample.procs = Some(processes(true));
    let snapshot = vitals.absorb(&sample);
    assert_eq!(snapshot.aterm.as_ref().unwrap().cores.raw, 0.75);
    assert_eq!(snapshot.aterm.as_ref().unwrap().footprint, Some(100 << 20));
    assert_eq!(snapshot.tab(1).unwrap().scope.cores.raw, 0.25);
    assert_eq!(snapshot.tab(2).unwrap().scope.cores.raw, 0.5);
    let cpu = &snapshot.resources[Resource::Cpu.index()];
    assert_eq!(cpu.aterm.now, Some(0.75));
    assert_eq!(snapshot.resource(Resource::Cpu, &[1]).tab.now, Some(0.25));
    assert_eq!(snapshot.resource(Resource::Cpu, &[2]).tab.now, Some(0.5));
    assert_eq!(
        snapshot.resources[Resource::Memory.index()].aterm.now,
        Some((100 << 20) as f64)
    );
    assert_eq!(
        snapshot.resources[Resource::Processes.index()].aterm.now,
        Some(4.0)
    );
    assert_eq!(
        snapshot.delegated.iter().map(|job| job.procs).sum::<u32>(),
        4
    );
    assert_eq!(
        snapshot.delegated.iter().map(|job| job.cores).sum::<f32>(),
        0.75
    );
    assert_eq!(heat_of(Some(f64::NAN), Some(10.0)), None);
    assert_eq!(heat_of(Some(f64::INFINITY), Some(f64::INFINITY)), None);
}

#[test]
fn reused_pid_inherits_neither_delegation_nor_previous_cpu() {
    use crate::delegated::ProcessKey;
    let mut process = proc(900, 1, 1000, 10, "old");
    let root = ProcessKey { pid: 900, start: 1 };
    let mut sample = Sample {
        at_ns: 1,
        self_pid: 100,
        procs: Some(vec![process.clone()]),
        delegated: vec![DelegatedJob {
            root,
            tab: None,
            members: vec![root],
        }],
        ..Sample::default()
    };
    let mut vitals = Vitals::new();
    vitals.absorb(&sample);
    process.start = Some(2);
    process.cpu_ns = Some(2_000_000_000);
    sample.at_ns += 1_000_000_000;
    sample.procs = Some(vec![process]);
    let snapshot = vitals.absorb(&sample);
    assert_eq!(snapshot.aterm.unwrap().cores.raw, 0.0);
    assert!(
        snapshot.delegated.is_empty(),
        "reused PID is not a zero reading of the old job"
    );
    assert!(snapshot.top.is_empty());
}

#[test]
fn process_self_rate_keeps_reaped_cpu_and_generation_separate() {
    let mut p = proc(900, 100, 100, 10, "worker");
    p.child_cpu_ns = Some(0);
    p.reaps_unwaited = Some(false);
    p.version = Some(11);
    p.at_ns = Some(1_000_000_000);
    let mut s = Sample {
        at_ns: 1_000_000_000,
        self_pid: 100,
        procs: Some(vec![p.clone()]),
        ..Sample::default()
    };
    let mut vitals = Vitals::new();
    vitals.absorb(&s);
    s.at_ns += 1_000_000_000;
    p.at_ns = Some(s.at_ns);
    p.cpu_ns = Some(200_000_000);
    p.child_cpu_ns = Some(5_000_000_000);
    s.procs = Some(vec![p.clone()]);
    let snapshot = vitals.absorb(&s);
    let row = &snapshot.procs[0];
    assert_eq!(row.cores, Some(0.1));
    assert_eq!(row.cpu_accounting.self_ns, Some(200_000_000));
    assert_eq!(row.cpu_accounting.reaped_children_ns, Some(5_000_000_000));
    assert_eq!(row.cpu_accounting.auto_reaps_children, Some(false));
    assert_eq!(row.cpu_accounting.sampled_at_ns, Some(s.at_ns));
    assert_eq!(row.cpu_accounting.exec_version, Some(11));

    s.at_ns += 1_000_000_000;
    p.start = Some(2);
    p.version = Some(12);
    p.cpu_ns = Some(50_000_000);
    p.child_cpu_ns = None;
    p.reaps_unwaited = None;
    p.at_ns = Some(s.at_ns);
    s.procs = Some(vec![p]);
    let replacement = vitals.absorb(&s);
    let row = &replacement.procs[0];
    assert_eq!(row.cores, None);
    assert_eq!(row.cpu_accounting.start, Some(2));
    assert_eq!(row.cpu_accounting.exec_version, Some(12));
    assert_eq!(row.cpu_accounting.self_ns, Some(50_000_000));
    assert_eq!(row.cpu_accounting.reaped_children_ns, None);
    assert_eq!(row.cpu_accounting.auto_reaps_children, None);
}

#[test]
fn conflicting_explicit_claims_are_not_counted_twice() {
    use crate::delegated::ProcessKey;
    let process = proc(900, 1, 1, 1, "shared");
    let key = ProcessKey { pid: 900, start: 1 };
    let jobs = vec![
        DelegatedJob {
            root: key,
            tab: Some(1),
            members: vec![key],
        },
        DelegatedJob {
            root: key,
            tab: Some(2),
            members: vec![key],
        },
    ];
    let (membership, jobs) = attribute_delegated(&[process], 100, &[], &jobs);
    assert_eq!(membership, vec![Membership::default()]);
    assert_eq!(jobs, vec![None]);
}

#[test]
fn attribution_follows_parent_chains_to_aterm_and_the_nearest_tab() {
    let procs = vec![
        proc(1, 0, 0, 0, "launchd"),
        proc(100, 1, 0, 0, "aterm"),
        proc(200, 100, 0, 0, "zsh"),
        proc(201, 200, 0, 0, "cargo"),
        proc(202, 201, 0, 0, "rustc"),
        proc(300, 100, 0, 0, "zsh"),
        proc(301, 300, 0, 0, "claude"),
        proc(50, 1, 0, 0, "keeper"),
        proc(400, 50, 0, 0, "zsh"),
        proc(900, 1, 0, 0, "Safari"),
    ];
    let tabs = vec![
        tab(7, 200, "Tab 1", "cargo"),
        tab(8, 300, "Tab 2", "claude"),
        tab(9, 400, "Tab 3", "zsh"),
    ];
    let m = attribute(&procs, 100, &tabs);
    let by_pid: HashMap<u32, Membership> =
        procs.iter().map(|p| p.pid).zip(m.iter().copied()).collect();
    assert_eq!(by_pid[&1], Membership::default());
    assert_eq!(
        by_pid[&100],
        Membership {
            aterm: true,
            tab: None
        }
    );
    assert_eq!(
        by_pid[&202],
        Membership {
            aterm: true,
            tab: Some(0)
        }
    );
    assert_eq!(
        by_pid[&301],
        Membership {
            aterm: true,
            tab: Some(1)
        }
    );
    assert_eq!(
        by_pid[&400],
        Membership {
            aterm: true,
            tab: Some(2)
        }
    );
    assert_eq!(
        by_pid[&50],
        Membership::default(),
        "the keeper is not a tab"
    );
    assert_eq!(by_pid[&900], Membership::default());
}

#[test]
fn a_parent_cycle_from_pid_reuse_terminates() {
    let procs = vec![proc(10, 11, 0, 0, "a"), proc(11, 10, 0, 0, "b")];
    let m = attribute(&procs, 100, &[]);
    assert_eq!(m, vec![Membership::default(); 2]);
}

fn sample(at_s: u64, busy: &[u64], procs: Vec<ProcSample>) -> Sample {
    Sample {
        at_ns: at_s * 1_000_000_000,
        cores: Some(
            busy.iter()
                .map(|&b| Ticks {
                    busy: b,
                    total: at_s * 100,
                })
                .collect(),
        ),
        clusters: vec![
            ClusterSpec {
                name: "Performance".into(),
                logical: vec![2, 3],
            },
            ClusterSpec {
                name: "Efficiency".into(),
                logical: vec![0, 1],
            },
        ],
        mem: Some(MemSample {
            total: 32 << 30,
            app: 10 << 30,
            wired: 3 << 30,
            compressed: Some(1 << 30),
            cached: 8 << 30,
            free: 10 << 30,
            swap_used: 0,
            swap_total: 0,
            pressure: Pressure::Normal,
            commit: None,
            ..MemSample::default()
        }),
        gpu: Some(GpuSample {
            device_pct: Some(4),
            cores: Some(10),
            ..GpuSample::default()
        }),
        volume: None,
        procs: Some(procs),
        self_pid: 100,
        tabs: vec![tab(7, 200, "Tab 1", "cargo")],
        ..Sample::default()
    }
}

#[test]
fn each_process_is_rated_over_its_own_reads() {
    let stamped = |cpu_ms, at_ms: u64| ProcSample {
        at_ns: Some(at_ms * 1_000_000),
        ..proc(201, 200, cpu_ms, 300, "cargo")
    };
    let mut v = Vitals::new();
    v.absorb(&sample(
        10,
        &[0, 0, 0, 0],
        vec![stamped(0, 9_000), proc(900, 1, 0, 900, "Safari")],
    ));
    let s = v.absorb(&sample(
        11,
        &[0, 0, 100, 100],
        vec![stamped(1_900, 10_900), proc(900, 1, 1_900, 900, "Safari")],
    ));
    let cores = |pid| {
        s.procs
            .iter()
            .find(|p| p.pid == pid)
            .and_then(|p| p.cores)
            .expect("a rate")
    };
    assert!((cores(201) - 1.0).abs() < 1e-6, "{}", cores(201));
    assert!((cores(900) - 1.9).abs() < 1e-6, "{}", cores(900));
}

#[test]
fn the_busiest_processes_carry_their_scope_and_cores_keep_a_minute() {
    let mut v = Vitals::new();
    let p0 = vec![
        proc(100, 1, 1000, 200, "aterm"),
        proc(200, 100, 0, 5, "zsh"),
        proc(201, 200, 5000, 300, "cargo"),
        proc(900, 1, 0, 900, "Safari"),
        proc(901, 1, 0, 9, "idle"),
    ];
    let _ = v.absorb(&sample(10, &[0, 0, 0, 0], p0));
    let p1 = vec![
        proc(100, 1, 1200, 200, "aterm"),
        proc(200, 100, 0, 5, "zsh"),
        proc(201, 200, 5900, 300, "cargo"),
        proc(900, 1, 300, 900, "Safari"),
        proc(901, 1, 0, 9, "idle"),
    ];
    let s = v.absorb(&sample(11, &[0, 0, 100, 50], p1));
    let names: Vec<&str> = s.top.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        ["cargo", "Safari", "aterm"],
        "busiest first, idle left out"
    );
    assert_eq!(s.top[0].scope, ProcScope::Tab(7));
    assert_eq!(s.top[1].scope, ProcScope::Other);
    assert_eq!(s.top[2].scope, ProcScope::Aterm);
    assert_eq!(s.top[0].footprint, Some(300 << 20));
    let cpu = s.cpu.as_ref().expect("an interval");
    assert_eq!(cpu.core_history.len(), 4);
    assert_eq!(cpu.core_history[2].last().copied().flatten(), Some(1.0));
    for at in 12..100 {
        let _ = v.absorb(&sample(
            at,
            &[0, 0, 100 * (at - 10), 50 * (at - 10)],
            Vec::new(),
        ));
    }
    let s = v.absorb(&sample(100, &[0, 0, 9000, 4500], Vec::new()));
    let cpu = s.cpu.as_ref().expect("an interval");
    assert!(cpu.core_history.iter().all(|h| h.len() <= CORE_HISTORY_LEN));
}

#[test]
fn the_busiest_program_counts_every_process_of_its_name() {
    let procs = |rustc_ms: u64, cargo_ms: u64| {
        let mut p = vec![
            proc(100, 1, 0, 200, "aterm"),
            proc(200, 100, 0, 5, "zsh"),
            proc(201, 200, cargo_ms, 300, "cargo"),
        ];
        p.extend((0..9).map(|i| proc(300 + i, 201, rustc_ms, 100, "rustc")));
        p
    };
    let mut v = Vitals::new();
    let _ = v.absorb(&sample(10, &[0, 0, 0, 0], procs(0, 0)));
    let s = v.absorb(&sample(11, &[0, 0, 100, 100], procs(600, 2_000)));
    assert_eq!(s.top.len(), TOP_PROCS);
    assert_eq!(s.top[0].name, "cargo");
    let b = s.facts.busiest.expect("a busy program");
    assert_eq!(
        (b.name.as_str(), b.n, b.scope),
        ("rustc", 9, Some(ProcScope::Tab(7)))
    );
    assert_eq!((b.born_ms, b.unborn), (None, 9), "no birth read, never 0");
    assert!((b.cores - 5.4).abs() < 1e-4, "{b:?}");
    let mut mixed = procs(1_200, 4_000);
    mixed[3].ppid = 1;
    let s = v.absorb(&sample(12, &[0, 0, 200, 200], mixed));
    let b = s.facts.busiest.expect("a busy program");
    assert_eq!((b.n, b.scope), (9, None));
}

/// A PROGRAM'S AGE IS ITS OLDEST MEMBER'S, WITHIN ITS SCOPE: nine rustc in Tab 1 born a
/// minute apart and two older rustc outside aterm are two groups, and the tab's age is its
/// own oldest member's though that one sits idle. The busiest program's birth is its busy
/// members' earliest. NEGATIVE CONTROLS: a member with no birth read counts as unborn and
/// leaves the others' oldest; a group with none read has no age, never 0; a birth after the
/// reading (a wall-clock step) reads 0, not a wrap.
#[test]
fn a_program_groups_age_is_its_oldest_members_within_its_scope() {
    const T0: i64 = 1_790_000_000_000;
    const MIN: i64 = 60_000;
    let born = |p: ProcSample, birth_ms: Option<i64>| ProcSample { birth_ms, ..p };
    let procs = |rustc_ms: u64, other_ms: u64, unread_ms: u64| {
        let mut p = vec![
            proc(100, 1, 0, 200, "aterm"),
            proc(200, 100, 0, 5, "zsh"),
            born(proc(201, 200, 0, 300, "cargo"), Some(T0 - MIN)),
        ];
        p.extend((0..9u32).map(|i| {
            let cpu = if i == 0 { 0 } else { rustc_ms };
            let p = proc(300 + i, 201, cpu, 100, "rustc");
            born(p, Some(T0 + i64::from(i) * MIN))
        }));
        p.push(born(
            proc(900, 1, other_ms, 50, "rustc"),
            Some(T0 - 60 * MIN),
        ));
        p.push(born(proc(901, 1, unread_ms, 50, "rustc"), None));
        p.push(born(proc(950, 1, 0, 10, "mds"), None));
        p
    };
    let at = T0 + 47 * MIN;
    let reading = |at_s: u64, busy: &[u64], procs: Vec<ProcSample>, at_unix_ms: i64| Sample {
        at_unix_ms,
        ..sample(at_s, busy, procs)
    };
    let mut v = Vitals::new();
    let _ = v.absorb(&reading(10, &[0, 0, 0, 0], procs(0, 0, 0), at - 1000));
    let s = v.absorb(&reading(11, &[0, 0, 100, 100], procs(600, 600, 0), at));
    let group = |s: &Snapshot, name: &str, scope: ProcScope| {
        s.programs
            .iter()
            .find(|g| g.name == name && g.scope == scope)
            .cloned()
            .unwrap_or_else(|| panic!("{name} in {scope:?}: {:?}", s.programs))
    };
    let tab = group(&s, "rustc", ProcScope::Tab(7));
    assert_eq!((tab.n, tab.born_ms, tab.unborn), (9, Some(T0), 0));
    assert_eq!(tab.age_ms(at), Some(47 * 60_000), "its idle oldest counts");
    assert!((tab.cores - 4.8).abs() < 1e-4, "{tab:?}");
    assert_eq!(tab.footprint, Some(900 << 20));
    let other = group(&s, "rustc", ProcScope::Other);
    assert_eq!(
        (other.n, other.born_ms, other.unborn),
        (2, Some(T0 - 60 * MIN), 1),
        "its own scope; the unread member is unborn"
    );
    let mds = group(&s, "mds", ProcScope::Other);
    assert_eq!((mds.born_ms, mds.unborn, mds.age_ms(at)), (None, 1, None));
    assert_eq!(tab.age_ms(T0 - 1), Some(0), "a birth after the reading");
    assert_eq!(
        group(&s, "cargo", ProcScope::Tab(7)).born_ms,
        Some(T0 - MIN)
    );
    assert_eq!(
        s.programs[0].name, "rustc",
        "busiest first: {:?}",
        s.programs
    );
    assert_eq!(
        s.procs
            .iter()
            .find(|p| p.pid == 305)
            .and_then(|p| p.age_ms(at)),
        Some(42 * 60_000),
        "a row carries its birth"
    );
    let b = s.facts.busiest.clone().expect("a busy program");
    assert_eq!(
        (b.name.as_str(), b.n, b.scope, b.born_ms, b.unborn),
        ("rustc", 9, None, Some(T0 - 60 * MIN), 0)
    );
    let s = v.absorb(&reading(
        12,
        &[0, 0, 200, 200],
        procs(1_200, 600, 0),
        at + 1000,
    ));
    let b = s.facts.busiest.expect("a busy program");
    assert_eq!(
        (b.n, b.scope, b.born_ms, b.unborn, b.age_ms(at + 1000)),
        (
            8,
            Some(ProcScope::Tab(7)),
            Some(T0 + MIN),
            0,
            Some(46 * 60_000 + 1000)
        ),
        "its busy members' earliest: the idle oldest and the idle outsider are not busy"
    );
    let s = v.absorb(&reading(
        13,
        &[0, 0, 300, 300],
        procs(1_800, 600, 600),
        at + 2000,
    ));
    let b = s.facts.busiest.expect("a busy program");
    assert_eq!(
        (b.n, b.scope, b.born_ms, b.unborn),
        (9, None, Some(T0 + MIN), 1),
        "a busy member whose birth went unread is counted unborn"
    );
}

#[test]
fn the_programs_kept_are_every_aterm_group_and_the_machines_busiest_and_largest() {
    let group = |name: String, scope, cores: f32, footprint: usize| ProgramGroup {
        name,
        scope,
        n: 1,
        cores,
        footprint: Some(footprint as u64),
        born_ms: None,
        unborn: 1,
    };
    let mut all: Vec<ProgramGroup> = (0..3 * TOP_ROWS)
        .map(|i| {
            let cores = i as f32 / 100.0;
            group(
                format!("other{i}"),
                ProcScope::Other,
                cores,
                3 * TOP_ROWS - i,
            )
        })
        .collect();
    all.extend((0..5).map(|i| group(format!("tab{i}"), ProcScope::Tab(7), 0.0, 1)));
    all.push(group("aterm".into(), ProcScope::Aterm, 0.0, 0));
    let kept = keep_programs(all);
    let ours = kept.iter().filter(|g| g.scope != ProcScope::Other).count();
    assert_eq!(ours, 6, "every aterm and tab group");
    let others: Vec<&str> = kept
        .iter()
        .filter(|g| g.scope == ProcScope::Other)
        .map(|g| g.name.as_str())
        .collect();
    assert_eq!(others.len(), 2 * TOP_ROWS);
    let busiest = format!("other{}", 3 * TOP_ROWS - 1);
    assert_eq!(kept[0].name, busiest, "busiest first");
    assert!(others.contains(&"other0"), "the largest of the rest");
    let neither = format!("other{}", TOP_ROWS + 5);
    assert!(
        !others.contains(&neither.as_str()),
        "neither busy nor large"
    );
}

#[test]
fn scheduling_zero_total_is_held_drawing_data_not_current_utilization() {
    for prior_busy in [0, 100] {
        let mut v = Vitals::new();
        v.absorb(&sample(10, &[0; 4], Vec::new()));
        let measured = v.absorb(&sample(11, &[0, 0, prior_busy, prior_busy], Vec::new()));
        let cpu = measured.cpu.as_ref().unwrap();
        assert!(cpu.cores.iter().all(|c| c.measured));
        assert_eq!(cpu.current_busy(), Some(2.0 * prior_busy as f32 / 100.0));

        let mut stopped = sample(12, &[0, 0, prior_busy, prior_busy], Vec::new());
        stopped.cores.as_mut().unwrap()[2].total = 1100;
        let mut held = v.absorb(&stopped);
        held.global.wait = Some(2.0);
        let cpu = held.cpu.as_ref().unwrap();
        assert_eq!(cpu.cores[2].load.raw, prior_busy as f32 / 100.0);
        assert_eq!(cpu.cores[2].current(), None);
        assert_eq!(cpu.current_busy(), None);
        assert_eq!(cpu.current_cluster_busy(&cpu.clusters[0]), None);
        assert_eq!(cpu.current_cluster_busy(&cpu.clusters[1]), Some(0.0));
        assert_eq!(cpu.core_history[2].last(), Some(&None));
        assert_eq!(cpu.history.last(), Some(&None));
        assert!(held.split.is_none());
        assert!(crate::scheduling::idle_performance(&held).is_none());

        let recovered = v.absorb(&sample(13, &[0, 0, prior_busy, prior_busy], Vec::new()));
        assert_eq!(recovered.cpu.as_ref().unwrap().current_busy(), Some(0.0));
    }
}

#[test]
fn scheduling_wait_requires_matching_sweep_windows() {
    let reading = |t_s: u64, busy: &[u64], cpu_ms, run_ns| {
        let mut s = sample(
            t_s,
            busy,
            vec![ProcSample {
                runnable_ns: Some(run_ns),
                ..proc(201, 200, cpu_ms, 1, "worker")
            }],
        );
        s.at_unix_ms = T0 + t_s as i64 * 1000;
        s.mono_ms = t_s * 1000;
        s.cadence_ms = 1000;
        s
    };
    let mut v = Vitals::new();
    v.absorb(&reading(10, &[0, 0, 0, 0], 0, 0));
    let mut hidden = reading(11, &[0, 0, 100, 100], 0, 0);
    hidden.procs = None;
    hidden.machine_only = true;
    v.absorb(&hidden);

    // Historical wait must not be paired with only the latest interval's idle cores.
    let resumed = v.absorb(&reading(12, &[0, 0, 100, 100], 2000, 6_000_000_000));
    assert_eq!(resumed.procs_window_ns, Some(2_000_000_000));
    assert_eq!(resumed.cores_window_ns, Some(1_000_000_000));
    assert_eq!(resumed.global.wait, Some(2.0));
    assert_eq!(resumed.cpu.as_ref().unwrap().cores[2].load.raw, 0.0);
    assert_eq!(resumed.cpu.as_ref().unwrap().cores[3].load.raw, 0.0);
    assert!(crate::scheduling::idle_performance(&resumed).is_none());

    let aligned = v.absorb(&reading(13, &[0, 0, 100, 100], 2000, 8_000_000_000));
    assert_eq!(aligned.procs_window_ns, aligned.cores_window_ns);
    assert_eq!(aligned.global.wait, Some(2.0));
    assert!(crate::scheduling::idle_performance(&aligned).is_some());
}

#[test]
fn cores_and_scopes_come_from_deltas_over_the_interval() {
    let mut v = Vitals::new();
    let p0 = vec![
        proc(100, 1, 1000, 200, "aterm"),
        proc(200, 100, 0, 5, "zsh"),
        proc(201, 200, 5000, 300, "cargo"),
        proc(900, 1, 0, 900, "Safari"),
    ];
    let first = v.absorb(&sample(10, &[0, 0, 0, 0], p0));
    assert!(first.cpu.is_none(), "one reading has no interval");
    assert!(first.aterm.is_none());
    assert!(first.mem.is_some() && first.gpu.is_some());

    let p1 = vec![
        proc(100, 1, 1200, 200, "aterm"),
        proc(200, 100, 0, 5, "zsh"),
        proc(201, 200, 5900, 300, "cargo"),
        proc(900, 1, 300, 900, "Safari"),
    ];
    let s = v.absorb(&sample(11, &[0, 0, 100, 50], p1.clone()));
    let cpu = s.cpu.as_ref().expect("an interval");
    assert_eq!(cpu.total(), 4);
    assert!((cpu.busy.raw - 1.5).abs() < 1e-6, "{:?}", cpu.busy);
    assert_eq!(cpu.cores[2].load.raw, 1.0);
    assert_eq!(cpu.clusters[0].short, "P");
    assert_eq!(cpu.clusters[0].cores, vec![2, 3]);
    assert_eq!(cpu.clusters[1].short, "E");

    let aterm = s.aterm.as_ref().expect("two sweeps");
    assert!((aterm.cores.raw - 1.1).abs() < 1e-5, "{aterm:?}");
    assert_eq!(aterm.procs, 3);
    assert_eq!(aterm.footprint, Some(505 << 20));
    assert_eq!(aterm.top.as_ref().map(|t| t.0.as_str()), Some("cargo"));
    let t = s.tab(7).expect("tab 1");
    assert!((t.scope.cores.raw - 0.9).abs() < 1e-5, "{t:?}");
    assert_eq!(t.scope.procs, 2);
    assert_eq!(s.global.procs, 4);
    assert_eq!(s.global.footprint, Some(14 << 30));

    for at in [12, 13] {
        let mut missing = sample(at, &[0; 4], Vec::new());
        missing.cores = None;
        missing.gpu = None;
        missing.mem = None;
        missing.procs = None;
        if at == 13 {
            missing.cores = Some(Vec::new());
            missing.gpu = Some(GpuSample::default());
            missing.mem = Some(MemSample::default());
        }
        let gap = v.absorb(&missing);
        assert!(gap.cpu.is_none() && gap.gpu.is_none() && gap.mem.is_none());
        assert!(gap.aterm.is_none() && gap.tabs.is_empty());
    }
    let mut recovered = sample(15, &[0, 0, 100, 50], p1.clone());
    recovered.gpu.as_mut().expect("GPU").device_pct = Some(84);
    recovered.mem.as_mut().expect("memory").app = 18 << 30;
    let recovered = v.absorb(&recovered);
    let decay = (-4.0 / crate::trend::FAST_TAU_S).exp();
    let cpu = recovered.cpu.as_ref().expect("CPU recovered");
    assert_eq!(cpu.busy.raw, 0.0, "no new busy ticks");
    assert!((cpu.busy.level - 1.5 * decay).abs() < 1e-5, "{cpu:?}");
    assert!((cpu.cores[2].load.level - decay).abs() < 1e-5);
    let gpu = recovered.gpu.as_ref().expect("GPU recovered");
    let gpu_level = 0.84 + (0.04 - 0.84) * decay;
    assert!((gpu.load.level - gpu_level).abs() < 1e-5, "{gpu:?}");
    let mem = recovered.mem.as_ref().expect("memory recovered");
    let mem_level = 22.0 + (14.0 - 22.0) * decay;
    assert!((mem.used_gib.level - mem_level).abs() < 1e-5, "{mem:?}");
    assert!((recovered.global.footprint_gib.level - mem_level).abs() < 1e-5);
    for history in [&cpu.history, &gpu.history, &mem.history] {
        assert_eq!(history.len(), 6);
        assert_eq!(&history[2..4], &[None, None]);
        assert_eq!(history[4], history[5], "14 holds the reading that spans it");
    }
    let aterm = recovered.aterm.as_ref().expect("aterm recovered");
    let tab = recovered.tab(7).expect("tab recovered");
    assert!((aterm.cores.level - 1.1 * decay).abs() < 1e-5);
    assert!((tab.scope.cores.level - 0.9 * decay).abs() < 1e-5);
    for history in [&aterm.history, &tab.scope.history] {
        assert_eq!(history.len(), 5);
        assert_eq!(&history[1..], &[None, None, Some(0.0), Some(0.0)]);
    }

    for at in [16, 17] {
        let mut unreadable = p1.clone();
        for process in &mut unreadable {
            process.footprint = None;
        }
        let unknown = v.absorb(&sample(at, &[0, 0, 100, 50], unreadable));
        assert_eq!(unknown.aterm.as_ref().expect("aterm sweep").footprint, None);
        assert_eq!(unknown.tab(7).expect("tab sweep").scope.footprint, None);
    }
    let mut footprints = p1;
    for process in &mut footprints {
        process.footprint = process.footprint.map(|bytes| bytes * 2);
    }
    let recovered = v.absorb(&sample(19, &[0, 0, 100, 50], footprints));
    let aterm = recovered.aterm.as_ref().expect("aterm footprint");
    let tab = recovered.tab(7).expect("tab footprint");
    let aterm_level = (1010.0 + (505.0 - 1010.0) * decay) / 1024.0;
    let tab_level = (610.0 + (305.0 - 610.0) * decay) / 1024.0;
    assert_eq!(aterm.footprint, Some(1010 << 20));
    assert_eq!(tab.scope.footprint, Some(610 << 20));
    assert!(
        (aterm.footprint_gib.level - aterm_level).abs() < 1e-5,
        "{aterm:?}"
    );
    assert!(
        (tab.scope.footprint_gib.level - tab_level).abs() < 1e-5,
        "{tab:?}"
    );
}

#[test]
fn a_tab_with_no_program_word_is_named_from_its_shells_biggest_child() {
    let mut v = Vitals::new();
    let procs = || {
        vec![
            proc(200, 100, 0, 5, "zsh"),
            proc(201, 200, 0, 400, "claude"),
            proc(202, 200, 0, 2, "less"),
        ]
    };
    let mut s0 = sample(1, &[0; 4], procs());
    s0.tabs[0].program.clear();
    let _ = v.absorb(&s0);
    let mut s1 = sample(2, &[0; 4], procs());
    s1.tabs[0].program.clear();
    let snap = v.absorb(&s1);
    assert_eq!(snap.tab(7).map(|t| t.program.as_str()), Some("claude"));
    let mut s2 = sample(3, &[0; 4], vec![proc(200, 100, 0, 5, "zsh")]);
    s2.tabs[0].program.clear();
    let snap = v.absorb(&s2);
    assert_eq!(snap.tab(7).map(|t| t.program.as_str()), Some("zsh"));
}

#[test]
fn a_process_seen_once_counts_from_its_second_sweep() {
    let mut v = Vitals::new();
    let _ = v.absorb(&sample(1, &[0; 4], vec![proc(100, 1, 0, 1, "aterm")]));
    let s = v.absorb(&sample(
        2,
        &[0; 4],
        vec![
            proc(100, 1, 0, 1, "aterm"),
            proc(201, 100, 99_000, 1, "new"),
        ],
    ));
    assert_eq!(s.aterm.as_ref().map(|a| a.cores.raw), Some(0.0));
}

#[test]
fn a_closed_tab_drops_its_state() {
    let mut v = Vitals::new();
    let _ = v.absorb(&sample(1, &[0; 4], vec![proc(200, 100, 0, 1, "zsh")]));
    let _ = v.absorb(&sample(2, &[0; 4], vec![proc(200, 100, 0, 1, "zsh")]));
    assert_eq!(v.tabs.len(), 1);
    let mut s = sample(3, &[0; 4], vec![]);
    s.tabs.clear();
    let snap = v.absorb(&s);
    assert!(snap.tabs.is_empty());
    assert!(v.tabs.is_empty());
}

#[test]
fn a_core_count_change_restarts_the_cores_instead_of_misreading_them() {
    let mut v = Vitals::new();
    let _ = v.absorb(&sample(1, &[0; 4], vec![]));
    assert!(v.absorb(&sample(2, &[0; 4], vec![])).cpu.is_some());
    assert!(v.absorb(&sample(3, &[0; 6], vec![])).cpu.is_none());
    assert!(v.absorb(&sample(4, &[0; 6], vec![])).cpu.is_some());
}

#[test]
fn cluster_views_cover_every_core_once() {
    let specs = vec![
        ClusterSpec {
            name: "Super".into(),
            logical: vec![0, 1, 9, 1],
        },
        ClusterSpec {
            name: "Efficiency".into(),
            logical: vec![1, 2],
        },
    ];
    let v = cluster_views(&specs, 4);
    assert_eq!(v[0].cores, vec![0, 1]);
    assert_eq!(v[0].short, "S");
    assert_eq!(v[1].cores, vec![2]);
    assert_eq!(v[2].name, "CPU");
    assert_eq!(v[2].cores, vec![3]);
}

#[test]
fn memory_used_is_app_wired_and_compressed_capped_at_physical() {
    let m = MemSample {
        total: 10,
        app: 6,
        wired: 3,
        compressed: Some(4),
        ..MemSample::default()
    };
    assert_eq!(m.used(), 10);
    let unread = MemSample {
        compressed: None,
        ..m
    };
    assert_eq!(unread.used(), 9);
    assert_eq!(gib(1 << 30), 1.0);
}

#[test]
fn short_labels_keep_numbered_levels_apart() {
    let spec = |name: &str, logical: Vec<u16>| ClusterSpec {
        name: name.into(),
        logical,
    };
    let v = cluster_views(
        &[
            spec("Performance", vec![0]),
            spec("Mid 1", vec![1]),
            spec("Mid 2", vec![2]),
            spec("Efficiency", vec![3]),
        ],
        4,
    );
    let shorts: Vec<&str> = v.iter().map(|c| c.short.as_str()).collect();
    assert_eq!(shorts, ["P", "M1", "M2", "E"]);
    assert_eq!(cluster_views(&[spec("CPU", vec![0, 1])], 2)[0].short, "CPU");
    assert_eq!(cluster_views(&[spec("7", vec![0])], 1)[0].short, "7");
}

#[test]
fn commit_is_near_its_limit_only_where_the_limit_is_enforced() {
    let c = |charge, limit, exact| CommitSample {
        charge,
        limit,
        exact,
    };
    assert_eq!(c(93, 100, true).near_limit(), Some(93));
    assert_eq!(c(90, 100, true).near_limit(), Some(90));
    assert_eq!(c(899, 1000, true).near_limit(), None);
    assert_eq!(c(150, 100, false).near_limit(), None);
    assert_eq!(c(150, 100, false).pct(), Some(150));
    assert_eq!(c(5, 0, true).pct(), None);
    assert_eq!(c(5, 0, true).near_limit(), None);
}

use Resource::{Cpu, Disk, Gpu, Memory, Network, Processes, Tokens};

const T0: i64 = 1_792_000_800_000;

fn rig(mono_ms: u64, wall_ms: i64, cadence_ms: u32) -> Sample {
    let ticks = mono_ms / 10;
    let mine = |p: ProcSample, written: u64| ProcSample {
        disk_written: Some(written),
        uid: Some(501),
        ..p
    };
    Sample {
        at_ns: mono_ms * 1_000_000,
        cores: Some(vec![
            Ticks {
                busy: ticks,
                total: ticks,
            },
            Ticks {
                busy: ticks / 2,
                total: ticks,
            },
        ]),
        mem: Some(MemSample {
            total: 32 << 30,
            app: 8 << 30,
            ..MemSample::default()
        }),
        gpu: Some(GpuSample {
            device_pct: Some(12),
            ..GpuSample::default()
        }),
        volume: Some(VolumeSample {
            total: 2_000_000_000_000,
            free: 1_000_000_000_000,
            reserved: 0,
            mount: "/".into(),
        }),
        procs: Some(vec![
            mine(proc(100, 1, mono_ms / 10, 200, "aterm"), 0),
            mine(proc(200, 100, 0, 5, "zsh"), 0),
            mine(proc(201, 200, mono_ms / 2, 300, "cargo"), 2_000 * mono_ms),
        ]),
        self_pid: 100,
        tabs: vec![tab(7, 200, "Tab 1", "claude")],
        at_unix_ms: T0 + wall_ms,
        mono_ms,
        cadence_ms,
        load1: Some(0.5),
        links: Some(vec![LinkSample {
            name: "en0".into(),
            kind: LinkKind::Wifi,
            up: true,
            counts: true,
            rx: 1_000 * mono_ms,
            tx: 250 * mono_ms,
            rate_bps: Some(100_000_000),
            ..LinkSample::default()
        }]),
        room: TaskRoom {
            procs: Some(900),
            procs_max: Some(16_000),
            user_procs: Some(500),
            user_procs_max: Some(10_666),
            ..TaskRoom::default()
        },
        self_uid: Some(501),
        tokens: vec![TabTokens {
            key: 7,
            session: "s1".into(),
            out_total: 600 * mono_ms / 1000,
            in_total: 6_000 * mono_ms / 1000,
            complete: true,
            ..TabTokens::default()
        }],
        ..Sample::default()
    }
}

fn at(t_ms: u64, cadence_ms: u32) -> Sample {
    rig(t_ms, t_ms as i64, cadence_ms)
}

fn open(s: &Snapshot, r: Resource) -> Option<f64> {
    s.resources[r.index()].ring.last().and_then(|c| c.mean)
}

fn close(a: Option<f64>, b: f64) -> bool {
    a.is_some_and(|a| (a - b).abs() <= 1e-6 * b.abs().max(1.0))
}

#[test]
fn unknown_is_never_zero() {
    let mut v = Vitals::new();
    let blank = |t: i64| Sample {
        at_unix_ms: T0 + t * 1000,
        mono_ms: t as u64 * 1000,
        cadence_ms: 1000,
        tabs: vec![tab(7, 200, "Tab 1", "zsh")],
        ..Sample::default()
    };
    let mut s = Snapshot::default();
    for t in 0..25 {
        s = v.absorb(&blank(t));
    }
    for res in Resource::ALL {
        let r = s.resource(res, &[7]);
        assert_eq!((r.now, r.heat_now), (None, None), "{res:?}");
        assert!(r.ring.iter().all(|c| c.mean.is_none()), "{res:?}");
        for per in [10, 20, 30] {
            assert!(
                r.heat_columns(per, 20, s.at_unix_ms)
                    .iter()
                    .all(|c| c.heat.is_none() && c.max.is_none()),
                "{res:?}"
            );
        }
        assert_eq!(r.aterm.now, None);
        assert_eq!(r.tab.now, None);
    }
    let mut idle = blank(25);
    idle.gpu = Some(GpuSample {
        device_pct: Some(0),
        ..GpuSample::default()
    });
    let s = v.absorb(&idle);
    let gpu = &s.resources[Gpu.index()];
    assert_eq!((gpu.now, gpu.heat_now), (Some(0.0), Some(0.0)));
}

#[test]
fn a_gap_is_a_baseline_not_a_rate() {
    let baseline = Stamp {
        wall_ms: 0,
        mono_ms: 0,
        cadence_ms: 1,
    };
    let rate_at = |wall_ms| {
        Stamp {
            wall_ms,
            mono_ms: 1,
            cadence_ms: 1,
        }
        .rate_dt(Some(baseline))
    };
    for wall in [i64::MIN, i64::MIN + 1, i64::MAX] {
        assert_eq!(rate_at(wall), None, "extreme clock jump {wall}");
    }
    for wall in [-999, 1001] {
        assert_eq!(rate_at(wall), Some(0.001));
    }
    for wall in [-1000, 1002] {
        assert_eq!(rate_at(wall), None);
    }

    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for t in 0..5 {
        s = v.absorb(&at(t * 1000, 1000));
    }
    assert!(close(open(&s, Cpu), 1.5));
    assert!(close(open(&s, Network), 1e6));
    assert!(close(open(&s, Tokens), 36_000.0));
    assert!(close(s.resources[Disk.index()].aterm.now, 2e6));

    let s = v.absorb(&rig(14_000, 604_000, 1000));
    for res in [Cpu, Network, Tokens] {
        assert_eq!(open(&s, res), None, "{res:?}");
        assert_eq!(s.resources[res.index()].now, None, "{res:?}");
    }
    let tx = s.resources[Network.index()].second.as_ref().expect("send");
    assert_eq!(tx.ring.last().and_then(|c| c.mean), None);
    assert_eq!(s.resources[Disk.index()].aterm.now, None, "bytes written");
    assert_eq!(s.resource(Disk, &[7]).tab.now, None);
    assert!(close(open(&s, Memory), (8u64 << 30) as f64));
    assert!(close(open(&s, Disk), 1e12));

    let s = v.absorb(&rig(15_000, 605_000, 1000));
    assert!(close(open(&s, Cpu), 1.5));
    assert!(close(open(&s, Network), 1e6));
    assert!(close(open(&s, Tokens), 36_000.0));
    assert!(close(s.resources[Disk.index()].aterm.now, 2e6));
    assert!(close(s.resource(Disk, &[7]).tab.now, 2e6));

    let renamed = |mono_ms: u64| {
        let mut r = rig(mono_ms, mono_ms as i64 + 590_000, 10_000);
        if let Some(l) = r.links.as_mut().and_then(|l| l.first_mut()) {
            l.name = "en1".into();
        }
        r
    };
    let s = v.absorb(&renamed(25_000));
    assert!(close(open(&s, Cpu), 1.5), "the interval is admitted");
    assert_eq!(open(&s, Network), None);
    let tx = s.resources[Network.index()].second.as_ref().expect("send");
    assert_eq!(tx.ring.last().and_then(|c| c.mean), None);
    let s = v.absorb(&renamed(35_000));
    assert!(close(open(&s, Network), 1e6));
}

#[test]
fn heat_formulas() {
    let day = day_of(T0);
    let learned = |peak: f64, read_s: u32| DailyMax {
        days: vec![(day, peak)],
        read_s,
    };
    let mut v = Vitals::new();
    v.restore_peaks(Peaks {
        net_rx: learned(50e6, NET_LEARN_S),
        net_tx: learned(1e6, NET_LEARN_S),
        tokens: learned(72_000.0, TOKENS_LEARN_S),
        ..Peaks::default()
    });
    let mut s = Snapshot::default();
    for t in 0..6 {
        s = v.absorb(&at(t * 1000, 1000));
    }
    let heat = |s: &Snapshot, r: Resource| s.resources[r.index()].heat_now.map(f64::from);
    assert_eq!(s.resources[Cpu.index()].capacity, Some(2.0));
    assert!(close(heat(&s, Cpu), 0.75));
    assert!(close(heat(&s, Gpu), 0.12));
    let mem = &s.resources[Memory.index()];
    assert!(close(heat(&s, Memory), 0.25));
    assert!(close(
        heat(&s, Memory),
        mem.now.expect("read") / mem.capacity.expect("read")
    ));
    assert!(close(heat(&s, Disk), 0.5));
    let net = &s.resources[Network.index()];
    assert_eq!(net.basis, CapBasis::Peak);
    assert_eq!(net.capacity, Some(12.5e6), "50 MB/s clamped to the link");
    let tx = net.second.as_ref().expect("send");
    assert_eq!(tx.capacity, Some(1e6));
    assert!(close(heat(&s, Network), 0.25), "{net:?}");
    assert!(tx.binds && !net.first().binds);
    assert_eq!(net.binding().now, tx.now);
    assert_eq!(s.resources[Tokens.index()].basis, CapBasis::Peak);
    assert!(close(heat(&s, Tokens), 0.5));
    // With no start counter the row is the count against the machine's limit, and the
    // user's limit is Details' to say: the row carries no second series.
    assert!(close(heat(&s, Processes), 900.0 / 16_000.0));
    assert_eq!(s.resources[Processes.index()].basis, CapBasis::Fixed);
    assert!(s.resources[Processes.index()].second.is_none());

    let col = |s: &Snapshot| {
        s.resources[Tokens.index()].heat_columns(10, 1, T0 + 6_000)[0]
            .heat
            .map(f64::from)
    };
    assert!(close(col(&s), 0.5));
    let mut raised = v.peaks().clone();
    raised.tokens = learned(144_000.0, TOKENS_LEARN_S);
    v.restore_peaks(raised);
    let s = v.absorb(&at(7000, 1000));
    assert!(close(col(&s), 0.25));

    let mut burst = at(8000, 1000);
    if let Some(l) = burst.links.as_mut().and_then(|l| l.first_mut()) {
        l.rx += 1_000_000_000;
    }
    let s = v.absorb(&burst);
    let net = &s.resources[Network.index()];
    assert_eq!(net.heat_now, Some(1.0));
    assert_eq!(net.capacity, Some(12.5e6));
    assert!(net.first().binds);
    let cells = net.heat_columns(10, 60, T0 + 8000);
    assert!(
        cells
            .iter()
            .all(|c| c.heat.is_none_or(|h| h <= 1.0) && c.max.is_none_or(|m| m <= 1.0)),
        "{cells:?}"
    );
    assert_eq!(cells.last().and_then(|c| c.max), Some(1.0));

    let mut fresh = Vitals::new();
    let mut s = Snapshot::default();
    for t in 0..3 {
        s = fresh.absorb(&at(t * 1000, 1000));
    }
    let net = &s.resources[Network.index()];
    assert_eq!((net.basis, net.capacity), (CapBasis::Link, Some(12.5e6)));
    assert!(close(heat(&s, Network), 0.08));
    let tok = &s.resources[Tokens.index()];
    assert_eq!((tok.basis, tok.capacity), (CapBasis::Learning, None));
    assert_eq!(tok.heat_now, None);
    assert!(
        tok.now.is_some(),
        "the rate is read while the peak is learned"
    );
}

/// A1 §7 (the R3 lens fixes and their review, 2026-11-06): each column of a two-series row's
/// lens trace is one series' minute, the series whose mean is the larger share of its own
/// capacity: its mean the column and its largest reading the tick, each with its own share as
/// its heat, held at the basis's ceiling. The column's heat and over share are therefore the
/// deck row's ([`ResourceView::heat_columns`]) and the tick's heat at most the row's peak heat,
/// cooler where the other series' largest reading is the larger share. With no capacity, the
/// series whose raw mean is the larger, with no heat; a one-series row's are its columns. With
/// capacities far apart (receiving 100, sending 10) a column's tick never stands below it.
/// With no capacity, where one series' mean is the larger and the other's largest reading, the
/// column is the larger mean's minute. NEGATIVE CONTROLS: the larger raw figure takes
/// sending's minute in column 0; each figure from the series it is the larger share of (the
/// R3 fix as first built) takes sending's largest reading in column 0 and, against the
/// far-apart capacities, ticks 7 under a column of 50 and 9 under one of 30; with no capacity,
/// the larger raw max takes the minute of 5 (largest 20) beside one of 8 (largest 10).
#[test]
fn busier_columns_take_each_column_from_its_busier_series() {
    let k = 1_000_000_i64;
    // Four readings of a second each: the read time and its over seconds, in ms.
    let cell = |v: Option<(f64, f64)>, over_s: f64| Cell {
        mean: v.map(|v| v.0),
        max: v.map(|v| v.1),
        tau_ms: if v.is_some() { 4_000.0 } else { 0.0 },
        over_ms: over_s * 1_000.0,
    };
    // Receiving against 10, sending against 20.
    let rx: Arc<[Cell]> = vec![
        cell(Some((5.0, 6.0)), 1.0),
        cell(Some((2.0, 9.0)), 0.0),
        cell(Some((1.0, 1.0)), 0.0),
        cell(None, 0.0),
        cell(Some((9.0, 9.5)), 0.0),
    ]
    .into();
    let tx: Arc<[Cell]> = vec![
        cell(Some((8.0, 16.0)), 2.0),
        cell(Some((6.0, 10.0)), 0.0),
        cell(None, 0.0),
        cell(Some((3.0, 4.0)), 0.0),
        cell(Some((19.0, 30.0)), 0.0),
    ]
    .into();
    let view = |rx: &Arc<[Cell]>, tx: &Arc<[Cell]>, rx_cap: Option<f64>, tx_cap: Option<f64>| {
        ResourceView {
            capacity: rx_cap,
            basis: CapBasis::Peak,
            ring: Arc::clone(rx),
            ring_k: k,
            second: Some(Series {
                capacity: tx_cap,
                ring: Arc::clone(tx),
                ..Series::default()
            }),
            ..ResourceView::default()
        }
    };
    let now = k * BUCKET_MS + 5_000;
    let shared = view(&rx, &tx, Some(10.0), Some(20.0));
    let (cells, heat) = shared.busier_columns(10, 5, now);
    let figures: Vec<(Option<f64>, Option<f64>, f64)> = cells
        .iter()
        .map(|c| (c.mean, c.max, c.over_share()))
        .collect();
    assert_eq!(
        figures,
        [
            (Some(5.0), Some(6.0), 0.5),
            (Some(6.0), Some(10.0), 0.0),
            (Some(1.0), Some(1.0), 0.0),
            (Some(3.0), Some(4.0), 0.0),
            (Some(19.0), Some(30.0), 0.0),
        ],
        "each column the minute of the series whose mean is the larger share"
    );
    let shares: Vec<(Option<f32>, Option<f32>)> = heat.iter().map(|h| (h.heat, h.max)).collect();
    assert_eq!(
        shares,
        [
            (Some(0.5), Some(0.6)),
            (Some(0.3), Some(0.5)),
            (Some(0.1), Some(0.1)),
            (Some(0.15), Some(0.2)),
            (Some(0.95), Some(1.0)),
        ],
        "each with its own share, held at the peak basis's ceiling"
    );
    let row = shared.heat_columns(10, 5, now);
    for (j, (h, r)) in heat.iter().zip(&row).enumerate() {
        assert_eq!(
            (h.heat, h.over),
            (r.heat, r.over),
            "column {j}: the deck row's column heat and over share"
        );
        assert!(h.max <= r.max, "column {j}: {h:?} against the row's {r:?}");
    }
    assert_eq!(
        (heat[0].max, row[0].max),
        (Some(0.6), Some(0.8)),
        "CONTROL: column 0's tick is receiving's, cooler than sending's peak the row takes"
    );
    let (cells, heat) = view(&rx, &tx, None, None).busier_columns(10, 5, now);
    assert_eq!(
        cells.iter().map(|c| (c.mean, c.max)).collect::<Vec<_>>(),
        [
            (Some(8.0), Some(16.0)),
            (Some(6.0), Some(10.0)),
            (Some(1.0), Some(1.0)),
            (Some(3.0), Some(4.0)),
            (Some(19.0), Some(30.0)),
        ],
        "the minute of the larger raw mean with no capacity"
    );
    assert!(heat.iter().all(|h| h.heat.is_none() && h.max.is_none()));
    // The larger raw mean in one series and the larger raw max in the other, each way round
    // (the second review, 2026-11-06): the column is the mean's minute.
    let mean_or_max = |first: (f64, f64), second: (f64, f64)| {
        let rx: Arc<[Cell]> = vec![cell(Some(first), 0.0), cell(Some(second), 0.0)].into();
        let tx: Arc<[Cell]> = vec![cell(Some(second), 0.0), cell(Some(first), 0.0)].into();
        let (cells, _) = view(&rx, &tx, None, None).busier_columns(10, 2, now);
        cells.iter().map(|c| (c.mean, c.max)).collect::<Vec<_>>()
    };
    assert_eq!(
        mean_or_max((5.0, 20.0), (8.0, 10.0)),
        [(Some(8.0), Some(10.0)), (Some(8.0), Some(10.0))],
        "the minute of the larger raw mean, not of the larger raw max"
    );
    let one = ResourceView {
        second: None,
        ..view(&rx, &tx, Some(10.0), None)
    };
    let (cells, heat) = one.busier_columns(10, 5, now);
    assert_eq!(cells, one.columns(10, 5, now));
    assert_eq!(heat, one.heat_cells(one.columns(10, 5, now)));

    // Receiving against 100, sending against 10: the review's column (50/60 against 1/7), one
    // whose sending mean binds and one whose receiving mean binds over a hotter sending tick.
    let rx: Arc<[Cell]> = vec![
        cell(Some((50.0, 60.0)), 0.0),
        cell(Some((40.0, 95.0)), 0.0),
        cell(Some((30.0, 80.0)), 0.0),
    ]
    .into();
    let tx: Arc<[Cell]> = vec![
        cell(Some((1.0, 7.0)), 0.0),
        cell(Some((6.0, 9.0)), 0.0),
        cell(Some((2.0, 9.0)), 0.0),
    ]
    .into();
    let apart = view(&rx, &tx, Some(100.0), Some(10.0));
    let (cells, heat) = apart.busier_columns(10, 3, now);
    assert_eq!(
        cells.iter().map(|c| (c.mean, c.max)).collect::<Vec<_>>(),
        [
            (Some(50.0), Some(60.0)),
            (Some(6.0), Some(9.0)),
            (Some(30.0), Some(80.0)),
        ],
        "far-apart capacities"
    );
    for (j, c) in cells.iter().enumerate() {
        assert!(
            c.max >= c.mean,
            "column {j}: the tick {:?} under its column {:?}",
            c.max,
            c.mean
        );
    }
    assert_eq!(
        heat.iter().map(|h| (h.heat, h.max)).collect::<Vec<_>>(),
        [
            (Some(0.5), Some(0.6)),
            (Some(0.6), Some(0.9)),
            (Some(0.3), Some(0.8)),
        ]
    );
}

/// The R3 lens fixes' review (2026-11-06): where neither series' share is known, the second
/// binds where its finite raw "now" is the larger, the rule `busier_columns` takes each column
/// by on its minute's mean, so with rates that hold through the minute, as here, the amount,
/// the deck's no-heat fill and the Network lens's Traffic line name the series its trace's
/// newest column draws. Read through [`Vitals::absorb`] in the first day of learning, on a
/// link that reports no speed: sending 14 MB/s against receiving's 2.1 binds; equal figures
/// keep the first. (Processes has one series since F4′, which retired its yours, record §8.1.)
/// NEGATIVE CONTROL: the old arm, which bound the first series whenever no share was known,
/// binds receiving.
#[test]
fn with_no_share_known_the_larger_raw_figure_binds() {
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for t in 0..6_u64 {
        let mut sample = at(t * 1000, 1000);
        sample.room.procs_max = None;
        sample.room.user_procs_max = None;
        if let Some(l) = sample.links.as_mut().and_then(|l| l.first_mut()) {
            l.rate_bps = None;
            l.rx = 2_100 * t * 1000;
            l.tx = 14_000 * t * 1000;
        }
        s = v.absorb(&sample);
    }
    let net = &s.resources[Network.index()];
    let tx = net.second.as_ref().expect("send");
    assert_eq!(
        (net.basis, net.capacity, tx.capacity),
        (CapBasis::Learning, None, None),
        "PRECONDITION: no share is known"
    );
    assert!(close(net.now, 2.1e6) && close(tx.now, 14e6), "{net:?}");
    assert!(tx.binds && !net.first().binds, "{net:?}");
    assert_eq!(net.binding().now, tx.now);
    let (cells, _) = net.busier_columns(10, 1, s.at_unix_ms);
    assert!(close(cells[0].mean, 14e6), "the trace's column: {cells:?}");
    let even = |b: f64| Series {
        now: Some(b),
        ..Series::default()
    };
    let tie = ResourceView::of(even(3.0), Some(even(3.0)), 0, false, CapBasis::Learning);
    assert!(
        !tie.second.as_ref().expect("second").binds,
        "a tie keeps the first"
    );
    let unread = ResourceView::of(
        Series::default(),
        Some(even(f64::NAN)),
        0,
        false,
        CapBasis::Learning,
    );
    assert!(
        !unread.second.as_ref().expect("second").binds,
        "a figure that is not finite is unread"
    );
}

#[test]
fn gap_rule_across_cadence_switches() {
    let mut readings: Vec<(u64, u32, bool)> = (0..10).map(|t| (t * 1000, 1000, false)).collect();
    readings.push((14_000, 5_000, true));
    readings.push((19_000, 5_000, false));
    readings.push((20_000, 500, true));
    readings.extend((1..20).map(|i| (20_000 + i * 500, 500, false)));
    readings.push((39_500, 10_000, true));
    readings.push((49_500, 10_000, false));
    readings.push((58_500, 1_000, true));
    readings.push((59_500, 1_000, false));
    let mut v = Vitals::new();
    for (t, cadence, first) in readings {
        let s = v.absorb(&at(t, cadence));
        if first {
            for res in [Cpu, Network, Tokens] {
                assert!(
                    open(&s, res).is_some(),
                    "{res:?} after the switch at {t} ms"
                );
            }
            assert!(s.resources[Disk.index()].aterm.now.is_some(), "{t} ms");
        }
    }
    let s = v.absorb(&at(69_500, 1_000));
    for res in [Cpu, Network, Tokens] {
        assert_eq!(open(&s, res), None, "{res:?} after missed readings");
    }
    let s = v.absorb(&at(79_500, 10_000));
    assert!(open(&s, Cpu).is_some());
    let s = v.absorb(&rig(89_500, 669_500, 10_000));
    for res in [Cpu, Network, Tokens] {
        assert_eq!(open(&s, res), None, "{res:?} across a sleep");
    }
}

#[test]
fn now_is_one_value() {
    let mut v = Vitals::new();
    let used = |i: u64| (8u64 << 30) + i * (64 << 20);
    let mut s = Snapshot::default();
    for i in 0..75u64 {
        let mut r = at(i * 1000, 1000);
        r.mem.as_mut().expect("memory").app = used(i);
        if let Some(p) = r.procs.as_mut().and_then(|p| p.first_mut()) {
            p.footprint = Some((200 + 10 * i) << 20);
        }
        s = v.absorb(&r);
    }
    let want = (65..75).map(|i| used(i) as f64).sum::<f64>() / 10.0;
    let mem = s.resource(Memory, &[7]);
    assert!(close(mem.now, want), "{:?} vs {want}", mem.now);
    assert!(close(
        mem.heat_now.map(f64::from),
        want / (32u64 << 30) as f64
    ));
    let want_aterm = (65..75u64)
        .map(|i| ((200 + 10 * i + 5 + 300) << 20) as f64)
        .sum::<f64>()
        / 10.0;
    assert!(close(mem.aterm.now, want_aterm));
    assert!(close(mem.tab.now, (305u64 << 20) as f64));
    let opens: Vec<Option<f64>> = [(10, 60), (20, 30), (30, 20)]
        .into_iter()
        .map(|(per, n)| {
            mem.columns(per, n, s.at_unix_ms)
                .last()
                .and_then(|c| c.mean)
        })
        .collect();
    for o in &opens {
        assert!(o.is_some_and(|o| (o - want).abs() > 1.0), "{opens:?}");
    }
    assert_ne!(opens[0], opens[1]);
    assert_eq!(s.resource(Memory, &[]).now, mem.now);
    assert_eq!(s.resource(Memory, &[99]).aterm, mem.aterm);
}

/// A reading at `t_ms` whose memory is `(GiB in use, pressure)`; `None` reads no memory.
fn pressed(t_ms: u64, cadence_ms: u32, reading: Option<(u64, Pressure)>) -> Sample {
    let mut s = at(t_ms, cadence_ms);
    match reading {
        Some((used_gib, pressure)) => {
            let m = s.mem.as_mut().expect("the rig reads memory");
            m.app = used_gib << 30;
            m.pressure = pressure;
        }
        None => s.mem = None,
    }
    s
}

#[test]
fn memory_keeps_the_worst_pressure_of_each_bucket_and_an_unread_bucket_hollow() {
    use Pressure::{Critical, Normal, Unknown, Warn};
    let reading = |t: u64| match t {
        3 => Some(Critical),
        5 | 40 => Some(Warn),
        30..40 => Some(Unknown),
        41 => None,
        _ => Some(Normal),
    };
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for t in (0..43u64).filter(|t| !(20..30).contains(t)) {
        s = v.absorb(&pressed(t * 1000, 1000, reading(t).map(|p| (8, p))));
    }
    let mem = &s.resources[Memory.index()];
    assert_eq!(
        means(&mem.ring)
            .iter()
            .map(Option::is_some)
            .collect::<Vec<_>>(),
        [true, true, false, true, true],
        "the run behind it"
    );
    assert_eq!(
        *mem.pressure,
        [Some(Critical), Some(Normal), None, None, Some(Warn)],
        "the worst of 0-9 s is its one critical reading; 20-29 s read nothing; 30-39 s read \
         memory but no pressure; 40-42 s keeps its warn past an unread and a normal reading"
    );
}

#[test]
fn without_a_pressure_reading_the_pressure_run_is_hollow_never_normal() {
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for t in 0..95u64 {
        s = v.absorb(&pressed(t * 1000, 1000, Some((8, Pressure::Unknown))));
    }
    let mem = &s.resources[Memory.index()];
    assert!(mem.ring.iter().all(|c| c.mean.is_some()), "memory is read");
    assert_eq!(mem.pressure.len(), mem.ring.len());
    assert!(mem.pressure.iter().all(Option::is_none));
    for res in Resource::ALL {
        let r = &s.resources[res.index()];
        assert!(res == Memory || r.pressure.is_empty(), "{res:?} keeps none");
    }
}

#[test]
fn hidden_reads_fill_global_rows() {
    let mut v = Vitals::new();
    let hidden = |t_s: u64| Sample {
        procs: None,
        machine_only: true,
        ..at(t_s * 1000, 10_000)
    };
    let mut s = Snapshot::default();
    for i in 0..=60 {
        s = v.absorb(&hidden(10 * i));
    }
    for res in Resource::ALL {
        let r = s.resource(res, &[7]);
        let read: Vec<bool> = r.ring.iter().map(|c| c.mean.is_some()).collect();
        assert_eq!(read.len(), 61, "{res:?}: one bucket per 10 s");
        let counter = matches!(res, Cpu | Network | Tokens);
        assert_eq!(read[0], !counter, "{res:?}");
        assert!(read[1..].iter().all(|&r| r), "{res:?}: {read:?}");
        assert!(r.now.is_some(), "{res:?}");
        assert_eq!((r.aterm.now, r.tab.now), (None, None), "{res:?}");
    }
    let _ = v.absorb(&at(601_000, 1000));
    let s = v.absorb(&at(602_000, 1000));
    for res in [Cpu, Memory, Disk, Tokens, Processes] {
        let r = s.resource(res, &[7]);
        assert!(r.aterm.now.is_some() && r.tab.now.is_some(), "{res:?}");
    }
    for res in [Gpu, Network] {
        let r = s.resource(res, &[7]);
        assert_eq!(r.aterm.missing, Some(NoFigure::NotPerApp));
        assert_eq!(r.tab.missing, Some(NoFigure::NotPerApp));
    }
}

/// GPU per scope from two client reads: each client counts in its process's scope, a reused
/// pid's time stays with services as unclaimed, the rows hold this tab ≤ all tabs ≤ the Mac's
/// (a device reading of 0 included), each scope's row is its client time times κ (here the
/// clients ran 67 % against a device 12 % or 0 % busy), while the sweep's figures stay raw
/// client time, and "not per app" shows only while no client read is held.
#[test]
fn gpu_time_per_scope_comes_from_the_client_reads() {
    use crate::gpu_clients::{GpuClientRow, GpuClientsSample};
    const MS: u64 = 1_000_000;
    let row = |entry: u64, pid: u32, name: &str, gpu_ms: u64| GpuClientRow {
        entry,
        pid,
        name: name.into(),
        gpu_ns: gpu_ms * MS,
    };
    let reading = |t_ms: u64, device: u8, ran: Option<u64>| {
        let mut s = at(t_ms, 1000);
        s.gpu.as_mut().expect("GPU").device_pct = Some(device);
        s.procs.as_mut().expect("sweep").push(ProcSample {
            uid: Some(88),
            ..proc(601, 1, 0, 400, "WindowServer")
        });
        s.gpu_clients = ran.map(|k| GpuClientsSample {
            at_ns: s.at_ns,
            complete: true,
            entries: vec![1, 2, 3, 4],
            rows: vec![
                row(1, 100, "aterm", 200 * k),
                row(2, 201, "cargo", 100 * k),
                row(3, 601, "WindowServer", 40 * k),
                row(4, 200, "bash", 1000 * k),
            ],
        });
        s
    };
    for device in [12u8, 0] {
        let mut v = Vitals::new();
        let first = v.absorb(&reading(1000, device, Some(0)));
        let r = first.resource(Gpu, &[7]);
        assert_eq!(
            r.aterm.missing,
            Some(NoFigure::NotPerApp),
            "a baseline only"
        );
        assert_eq!(first.aterm.as_ref().and_then(|a| a.gpu), None);
        let _ = v.absorb(&reading(2000, device, None));
        let s = v.absorb(&reading(3000, device, Some(1)));
        let gpu_of = |pid: u32| s.procs.iter().find(|p| p.pid == pid).and_then(|p| p.gpu);
        let near = |a: Option<f32>, b: f32| a.is_some_and(|a| (a - b).abs() < 1e-5);
        assert!(near(gpu_of(100), 0.10), "{:?}", s.procs);
        assert!(near(gpu_of(201), 0.05));
        assert!(near(gpu_of(601), 0.02));
        assert_eq!(gpu_of(200), Some(0.0), "a reused pid takes no GPU time");
        assert!(near(s.aterm.as_ref().and_then(|a| a.gpu), 0.15));
        assert!(near(s.tab(7).and_then(|t| t.scope.gpu), 0.05));
        let split = s.gpu.as_ref().and_then(|g| g.split).expect("a split");
        assert!((split.clients - 0.67).abs() < 1e-5, "{split:?}");
        assert!((split.aterm - 0.15).abs() < 1e-5, "{split:?}");
        // WindowServer's 0.02 and the reused pid's unclaimed 0.5.
        assert!(split.others.abs() < 1e-6 && (split.services - 0.52).abs() < 1e-5);

        let r = s.resource(Gpu, &[7]);
        assert_eq!((r.aterm.missing, r.tab.missing), (None, None));
        let (mac, all, tab) = (r.now.expect("Mac"), r.aterm.now.expect("all"), r.tab.now);
        let tab = tab.expect("tab");
        assert!(tab <= all && all <= mac, "{tab} ≤ {all} ≤ {mac}");
        // ū is the device's mean over the window's readings (2000 and 3000 ms), R 0.67.
        let kappa = f64::from(device) / 67.0;
        assert!((all - 15.0 * kappa).abs() < 1e-4, "{all}");
        assert!((tab - 5.0 * kappa).abs() < 1e-4, "{tab}");

        let last = |run: &[Cell]| run.last().and_then(|c| c.mean).expect("a cell");
        let (mac_run, all_run) = (last(&r.ring), last(&r.aterm.run));
        let tab_run = last(&r.tab.run);
        assert!(
            tab_run <= all_run && all_run <= mac_run,
            "each scope's 10 s cell: {tab_run} ≤ {all_run} ≤ {mac_run}"
        );

        let max_ms = crate::explain::WINDOW_MAX_NS / MS;
        let later = v.absorb(&reading(3000 + max_ms + 1000, device, None));
        let r = later.resource(Gpu, &[7]);
        assert_eq!(r.aterm.missing, Some(NoFigure::NotPerApp), "expired");
        assert_eq!(later.aterm.as_ref().and_then(|a| a.gpu), None);
    }
}

/// The owner's case, read live (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §3): the device
/// 5 % busy while the clients ran 5.8 % between them, aterm's 5.3 % and WindowServer's
/// 0.5 %. Each scope's share is its client time times κ = ū / R, so aterm keeps its
/// proportion of the busy time (4.57 %) and the rest of the device (0.43 %) is WindowServer's.
/// Where the clients ran less than the device was busy, κ is 1 and aterm's share is its
/// client time. CONTROL: holding the raw client time at the device's busy alone, as before,
/// gives aterm the whole 5 % and leaves WindowServer none.
#[test]
fn gpu_scopes_share_busy_time_in_proportion_when_clients_overlap() {
    use crate::gpu_clients::{GpuClientRow, GpuClientsSample};
    const MS: u64 = 1_000_000;
    let reading = |t_ms: u64, device: u8, ran: Option<u64>| {
        let mut s = at(t_ms, 1000);
        s.gpu.as_mut().expect("GPU").device_pct = Some(device);
        s.procs.as_mut().expect("sweep").push(ProcSample {
            uid: Some(88),
            ..proc(601, 1, 0, 400, "WindowServer")
        });
        // Over the 2 s window: aterm 106 ms (5.3 %), WindowServer 10 ms (0.5 %).
        s.gpu_clients = ran.map(|k| GpuClientsSample {
            at_ns: s.at_ns,
            complete: true,
            entries: vec![1, 3],
            rows: vec![
                GpuClientRow {
                    entry: 1,
                    pid: 100,
                    name: "aterm".into(),
                    gpu_ns: 106 * k * MS,
                },
                GpuClientRow {
                    entry: 3,
                    pid: 601,
                    name: "WindowServer".into(),
                    gpu_ns: 10 * k * MS,
                },
            ],
        });
        s
    };
    for (device, want) in [(5_u8, 5.3 * 5.0 / 5.8), (50, 5.3)] {
        let mut v = Vitals::new();
        let _ = v.absorb(&reading(1000, device, Some(0)));
        let _ = v.absorb(&reading(2000, device, None));
        let s = v.absorb(&reading(3000, device, Some(1)));
        let r = s.resource(Gpu, &[7]);
        let (mac, all) = (r.now.expect("Mac"), r.aterm.now.expect("aterm"));
        assert!(
            (all - want).abs() < 1e-4,
            "device {device}%: aterm {all}, want {want}"
        );
        assert!(all <= mac, "{all} ≤ {mac}");
        let aterm_raw = s.aterm.as_ref().and_then(|a| a.gpu).expect("raw");
        assert!(
            (aterm_raw - 0.053).abs() < 1e-6,
            "sys keeps the raw client time: {aterm_raw}"
        );
        if device == 5 {
            let rest = mac - all;
            assert!(
                (rest - 0.5 * 5.0 / 5.8).abs() < 1e-4,
                "WindowServer keeps its proportion: {rest}"
            );
            let held_alone = (f64::from(aterm_raw) * 100.0).min(mac);
            assert!(
                (held_alone - mac).abs() < 1e-9,
                "CONTROL: without κ aterm takes the whole device ({held_alone} of {mac})"
            );
        }
    }
}

use crate::ring::RING_BUCKETS;

const MIB: f64 = (1u64 << 20) as f64;

fn scope_ring(v: &Vitals, key: Option<u64>, res: Resource) -> Option<&BucketRing> {
    match key {
        Some(key) => v.rows.tabs.get(&key)?.ring(res),
        None => v.rows.aterm.ring(res),
    }
}

fn means(cells: &[Cell]) -> Vec<Option<f64>> {
    cells.iter().map(|c| c.mean).collect()
}

fn whole(t_s: u64) -> Sample {
    at(t_s * 1000, 10_000)
}

#[test]
fn each_scope_keeps_the_hour_and_a_snapshot_carries_its_last_ten_minutes() {
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for i in 0..=420 {
        s = v.absorb(&whole(10 * i));
    }
    for (res, tab, aterm) in [
        (Cpu, 0.5, 0.6),
        (Memory, 305.0 * MIB, 505.0 * MIB),
        (Processes, 2.0, 3.0),
        (Disk, 2e6, 2e6),
    ] {
        let r = s.resource(res, &[7]);
        for (key, fig, want) in [(Some(7), &r.tab, tab), (None, &r.aterm, aterm)] {
            assert_eq!(
                scope_ring(&v, key, res).map(BucketRing::len),
                Some(RING_BUCKETS),
                "{res:?} {key:?}: the model keeps the hour"
            );
            assert_eq!(fig.run.len(), SCOPE_RUN_BUCKETS, "{res:?} {key:?}");
            assert_eq!(fig.run_k, r.ring_k, "{res:?} {key:?}: ends with the Mac's");
            assert!(
                fig.run.iter().all(|c| close(c.mean, want)),
                "{res:?} {key:?}: {:?}",
                means(&fig.run)
            );
            assert!(close(fig.now, want), "{res:?} {key:?}: {:?}", fig.now);
        }
    }
}

#[test]
fn a_scope_run_never_exceeds_its_container() {
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    let (mut cargo_ms, mut helper_ms, mut child_ms) = (0, 0, 0);
    for i in 0..90u64 {
        cargo_ms += (i * 7 % 5) * 800;
        helper_ms += (i * 3 % 4) * 500;
        child_ms += i % 2 * 1_500;
        let mut r = at(i * 2_000, 2_000);
        let procs = r.procs.as_mut().expect("swept");
        if let Some(cargo) = procs.iter_mut().find(|p| p.pid == 201) {
            cargo.cpu_ns = Some(cargo_ms * 1_000_000);
            cargo.footprint = Some((100 + i * 13 % 9 * 50) << 20);
        }
        if i % 3 != 0 {
            procs.push(ProcSample {
                disk_written: Some(i * 4_000_000),
                uid: Some(501),
                ..proc(202, 201, child_ms, 40 + i % 5 * 20, "rustc")
            });
        }
        if i % 4 != 1 {
            procs.push(ProcSample {
                disk_written: Some(i * 1_000_000),
                uid: Some(501),
                ..proc(101, 100, helper_ms, 70 + i % 7 * 10, "helper")
            });
        }
        s = v.absorb(&r);
    }
    for res in [Cpu, Memory, Processes, Disk] {
        let r = s.resource(res, &[7]);
        let tab = r.tab.columns(10, 60, s.at_unix_ms);
        let aterm = r.aterm.columns(10, 60, s.at_unix_ms);
        let both: Vec<(f64, f64)> = tab
            .iter()
            .zip(&aterm)
            .filter_map(|(t, a)| t.mean.zip(a.mean))
            .collect();
        assert!(both.len() >= 15, "{res:?}: {both:?}");
        let mut seen: Vec<f64> = both.iter().map(|&(t, _)| t).collect();
        seen.dedup();
        assert!(seen.len() > 1, "{res:?}: the load varies: {both:?}");
        for (t, a) in both {
            assert!(
                t <= a + 1e-6 * a.abs().max(1.0),
                "{res:?}: tab {t} > all tabs {a}"
            );
        }
    }
}

#[test]
fn hidden_time_is_hollow_in_scope_runs_never_zero() {
    let mut v = Vitals::new();
    let hidden = |t_s: u64| Sample {
        procs: None,
        machine_only: true,
        ..whole(t_s)
    };
    let mut s = Snapshot::default();
    for i in 0..60 {
        let t_s = 10 * i;
        s = v.absorb(&if (30..48).contains(&i) {
            hidden(t_s)
        } else {
            whole(t_s)
        });
        if i == 40 {
            let r = s.resource(Memory, &[7]);
            assert_eq!(r.tab.now, None, "no figure while hidden");
            assert_eq!(r.tab.run.len(), 29, "the run is carried while hidden");
        }
    }
    let now = s.at_unix_ms;
    assert!(
        s.resources[Memory.index()]
            .columns(10, 60, now)
            .iter()
            .all(|c| c.mean.is_some()),
        "the Mac reads throughout"
    );
    for res in [Cpu, Memory, Disk, Processes] {
        let r = s.resource(res, &[7]);
        let back = if matches!(res, Cpu | Disk) { 49 } else { 48 };
        let hidden_span: Vec<usize> = std::iter::once(0).chain(30..back).collect();
        for fig in [&r.tab, &r.aterm] {
            let cols = fig.columns(10, 60, now);
            let hollow: Vec<usize> = (0..cols.len())
                .filter(|&j| cols[j].mean.is_none())
                .collect();
            assert_eq!(
                hollow, hidden_span,
                "{res:?}: a scope reads from its second sweep, unread is hollow, and a \
                 rate's first sweep back is a baseline"
            );
            assert!(
                cols.iter().all(|c| c.mean != Some(0.0)),
                "{res:?}: {cols:?}"
            );
        }
    }
}

#[test]
fn a_closed_tab_takes_its_run_with_it() {
    let mut v = Vitals::new();
    for i in 0..6 {
        let _ = v.absorb(&whole(10 * i));
    }
    assert!(v.rows.tabs.contains_key(&7));
    let mut gone = whole(60);
    gone.tabs.clear();
    let s = v.absorb(&gone);
    assert!(!v.rows.tabs.contains_key(&7));
    assert!(s.resources.iter().all(|r| r.tabs.is_empty()));
    assert!(s.resource(Memory, &[7]).tab.run.is_empty());
    let s = v.absorb(&whole(70));
    let back = s.resource(Memory, &[7]);
    assert_eq!(
        means(&back.tab.run),
        [Some(305.0 * MIB)],
        "a returning key starts hollow"
    );
    assert_eq!(back.aterm.run.len(), 7, "aterm's run goes on");
}

/// Tab 1 split in two panes, one opened 200 s in, with a 50 s gap in the sweeps: "this tab"
/// is both panes summed cell by cell, hollow only where no pane read, never past all tabs.
#[test]
fn a_split_tab_reads_its_panes_summed() {
    let split = |i: u64| {
        let mut s = whole(10 * i);
        if i >= 20 {
            let ms = 10_000 * i;
            s.tabs.push(TabRoot {
                tab: 7,
                ..tab(8, 300, "Tab 1", "")
            });
            let procs = s.procs.as_mut().expect("swept");
            for (pid, ppid, cpu_ms, mib, name, wrote) in [
                (300, 100, 0, 5, "zsh", 0),
                (301, 300, ms / 4, 100, "node", 500 * ms),
            ] {
                procs.push(ProcSample {
                    disk_written: Some(wrote),
                    uid: Some(501),
                    ..proc(pid, ppid, cpu_ms, mib, name)
                });
            }
        }
        if (40..45).contains(&i) {
            s.procs = None;
            s.machine_only = true;
        }
        s
    };
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for i in 0..60 {
        s = v.absorb(&split(i));
    }
    let now = s.at_unix_ms;
    let same = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(a), Some(b)) => close(Some(a), b),
        (a, b) => a == b,
    };
    for (res, focused, other) in [
        (Cpu, 0.5, 0.25),
        (Memory, 305.0 * MIB, 105.0 * MIB),
        (Processes, 2.0, 2.0),
        (Disk, 2e6, 0.5e6),
    ] {
        let own = |key: u64| {
            s.resources[res.index()]
                .tabs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, f)| f.clone())
        };
        let (seven, eight) = (s.resource(res, &[7]).tab, s.resource(res, &[8]).tab);
        assert_eq!(
            Some(&seven),
            own(7).as_ref(),
            "{res:?}: one pane is its own"
        );
        assert_eq!(Some(&eight), own(8).as_ref(), "{res:?}");
        assert!(close(seven.now, focused), "{res:?}: {:?}", seven.now);
        assert!(close(eight.now, other), "{res:?}: {:?}", eight.now);
        let r = s.resource(res, &[7, 8]);
        assert!(
            close(r.tab.now, focused + other),
            "{res:?}: {:?}",
            r.tab.now
        );
        assert_eq!(s.resource(res, &[8, 7]).tab, r.tab, "{res:?}: in any order");
        assert!(r.tab.now <= r.aterm.now, "{res:?}: this tab ≤ all tabs");

        let cols = |f: &ScopeFig| f.columns(10, 60, now);
        let (a, b, tab, all) = (cols(&seven), cols(&eight), cols(&r.tab), cols(&r.aterm));
        let back = if matches!(res, Cpu | Disk) { 46 } else { 45 };
        let hollow: Vec<usize> = (0..60).filter(|&j| tab[j].mean.is_none()).collect();
        let unread: Vec<usize> = std::iter::once(0).chain(40..back).collect();
        assert_eq!(hollow, unread, "{res:?}: hollow only where no pane read");
        for j in 0..60 {
            let sum = plus(a[j].mean, b[j].mean, |x, y| x + y);
            assert!(
                same(tab[j].mean, sum),
                "{res:?} {j}: {:?} vs {sum:?}",
                tab[j]
            );
            if let (Some(t), Some(all)) = (tab[j].max, all[j].max) {
                assert!(t <= all, "{res:?} {j}: tab {t} > all tabs {all}");
            }
        }
        assert!(
            (1..20).all(|j| tab[j] == a[j]),
            "{res:?}: before pane 8 opened"
        );
        assert!(
            (21..40).all(|j| tab[j].mean > a[j].mean),
            "{res:?}: the focused pane alone reads less: {:?} vs {:?}",
            means(&tab[21..40]),
            means(&a[21..40])
        );
    }

    let tokens = s.resource(Tokens, &[7, 8]).tab;
    assert_eq!(
        s.resource(Tokens, &[8]).tab.missing,
        Some(NoFigure::NoAgent)
    );
    assert_eq!(tokens.missing, None, "an agent in either pane is the tab's");
    assert!(tokens.now.is_some() && tokens.now == s.resource(Tokens, &[7]).tab.now);
    let net = s.resource(Network, &[7, 8]).tab;
    assert_eq!(net.missing, Some(NoFigure::NotPerApp));

    assert_eq!(
        [s.tab(7), s.tab(8)].map(|t| t.map(|t| t.tab)),
        [Some(7), Some(7)],
        "each pane carries its tab's key"
    );
    let t = s.tab_of(&[7, 8]).expect("the tab");
    assert_eq!((t.key, t.label.as_str()), (7, "Tab 1"));
    assert_eq!(t.scope.procs, 4);
    assert!(
        (t.scope.cores.raw - 0.75).abs() < 1e-6,
        "{:?}",
        t.scope.cores
    );
    assert_eq!(t.scope.footprint, Some(410 << 20));
    assert!(close(t.scope.write_bps, 2.5e6), "{:?}", t.scope.write_bps);
    assert_eq!(t.scope.top.as_ref().map(|t| t.0.as_str()), Some("cargo"));
    let aterm = s.aterm.as_ref().expect("aterm");
    assert!(t.scope.cores.raw <= aterm.cores.raw && t.scope.procs <= aterm.procs);
    assert_eq!(s.tab_of(&[7]), s.tab(7).cloned(), "one pane is itself");
    assert_eq!(
        s.tab_of(&[8, 7]).map(|t| (t.key, t.scope.procs)),
        Some((8, 4)),
        "the focused pane names it"
    );
    assert_eq!(s.tab_of(&[99]), None);
}

/// Two panes at 10% each, each within the Mac's 12%, sum past it, so the split tab's GPU
/// figure is held at all tabs'.
#[test]
fn a_split_tabs_gpu_stays_inside_all_tabs() {
    use crate::gpu_clients::{GpuClientRow, GpuClientsSample};
    let reading = |t_ms: u64, ran: Option<u64>| {
        let mut s = at(t_ms, 1000);
        s.gpu.as_mut().expect("GPU").device_pct = Some(12);
        s.tabs.push(TabRoot {
            tab: 7,
            ..tab(8, 300, "Tab 1", "")
        });
        let procs = s.procs.as_mut().expect("sweep");
        procs.push(proc(300, 100, 0, 5, "zsh"));
        procs.push(proc(301, 300, 0, 50, "node"));
        s.gpu_clients = ran.map(|k| GpuClientsSample {
            at_ns: s.at_ns,
            complete: true,
            entries: vec![1, 2],
            rows: [(1, 201, "cargo"), (2, 301, "node")]
                .into_iter()
                .map(|(entry, pid, name)| GpuClientRow {
                    entry,
                    pid,
                    name: name.into(),
                    gpu_ns: 200_000_000 * k,
                })
                .collect(),
        });
        s
    };
    let mut v = Vitals::new();
    let _ = v.absorb(&reading(1000, Some(0)));
    let _ = v.absorb(&reading(2000, None));
    let s = v.absorb(&reading(3000, Some(1)));
    // Each client ran 10 % of the window, together 20 % of a device busy 12 %: each tab's
    // share is its 10 % scaled by 12/20.
    for pane in [7, 8] {
        let fig = s.resource(Gpu, &[pane]).tab;
        assert!(close(fig.now, 6.0), "pane {pane}: {:?}", fig.now);
    }
    let r = s.resource(Gpu, &[7, 8]);
    let (mac, all) = (r.now.expect("Mac"), r.aterm.now.expect("all tabs"));
    assert!(close(Some(all), 12.0), "{all}");
    assert!(close(r.tab.now, all), "held at all tabs': {:?}", r.tab.now);
    assert!(all <= mac);
    let last = |run: &[Cell]| run.last().and_then(|c| c.mean).expect("a cell");
    assert!(
        last(&r.tab.run) <= last(&r.aterm.run),
        "{:?}",
        r.tab.run.last()
    );
}

/// rustc in both panes of a split tab is one program of Tab 1 among the busiest, the top
/// processes and the writers; the same panes as two tabs (the control) leave its scope unnamed.
#[test]
fn a_split_tabs_processes_are_the_tabs() {
    let read = |tab_of_8: u64| {
        let procs = |t: u64| {
            let wrote = |p: ProcSample, mb: u64| ProcSample {
                disk_written: Some(t * mb * 1_000_000),
                ..p
            };
            vec![
                proc(100, 1, 0, 200, "aterm"),
                proc(200, 100, 0, 5, "zsh"),
                wrote(proc(201, 200, t * 500, 100, "rustc"), 1),
                proc(300, 100, 0, 5, "zsh"),
                wrote(proc(301, 300, t * 1000, 100, "rustc"), 2),
            ]
        };
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for t in [10, 11] {
            let mut swept = Sample {
                mono_ms: t * 1000,
                at_unix_ms: T0 + t as i64 * 1000,
                cadence_ms: 1000,
                ..sample(t, &[0, 0, 0, 0], procs(t))
            };
            swept.tabs.push(TabRoot {
                tab: tab_of_8,
                ..tab(8, 300, "Tab 1", "")
            });
            s = v.absorb(&swept);
        }
        s
    };
    let s = read(7);
    let b = s.facts.busiest.as_ref().expect("a busy program");
    assert_eq!(
        (b.name.as_str(), b.n, b.scope),
        ("rustc", 2, Some(ProcScope::Tab(7)))
    );
    assert!((b.cores - 1.5).abs() < 1e-4, "{b:?}");
    let scopes = |names: Vec<(&str, ProcScope)>| -> Vec<ProcScope> {
        names
            .into_iter()
            .filter(|(n, _)| *n == "rustc")
            .map(|(_, s)| s)
            .collect()
    };
    let top = scopes(s.top.iter().map(|t| (t.name.as_str(), t.scope)).collect());
    assert_eq!(top, [ProcScope::Tab(7); 2], "{:?}", s.top);
    let writers: Vec<_> = s
        .facts
        .writers
        .iter()
        .filter(|w| w.name == "rustc")
        .map(|w| (w.scope, w.n))
        .collect();
    assert_eq!(writers, [(ProcScope::Tab(7), 2)], "one program of Tab 1");
    assert_eq!(s.tab(7).map(|t| t.label.as_str()), Some("Tab 1"));

    let apart = read(8);
    let b = apart.facts.busiest.as_ref().expect("a busy program");
    assert_eq!((b.n, b.scope), (2, None), "CONTROL: two tabs");
}

#[test]
fn gpu_and_network_scope_runs_stay_empty() {
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for i in 0..6 {
        s = v.absorb(&whole(10 * i));
    }
    for res in [Gpu, Network] {
        let r = s.resource(res, &[7]);
        for fig in [&r.aterm, &r.tab] {
            assert_eq!(fig.missing, Some(NoFigure::NotPerApp), "{res:?}");
            assert!(fig.run.is_empty() && fig.now.is_none(), "{res:?}");
        }
        assert!(scope_ring(&v, None, res).is_none(), "{res:?}");
        assert!(scope_ring(&v, Some(7), res).is_none(), "{res:?}");
    }
    assert_eq!(s.resource(Memory, &[7]).tab.run.len(), 5);
}

/// `t_ms` on both clocks at a 1 s cadence, reading `forks` on the start counter.
fn forks_at(t_ms: u64, wall_ms: i64, forks: Option<u64>) -> Sample {
    Sample {
        starts: forks.map(crate::starts::StartCount::Forks),
        ..rig(t_ms, wall_ms, 1000)
    }
}

/// The flow is the start counter's growth over the reading's interval, from the second
/// reading that reads one; a reading that reads none leaves the next without a base.
#[test]
fn the_flow_is_the_start_counters_growth_over_each_readings_interval() {
    let mut v = Vitals::new();
    let flow = |s: &Snapshot| s.facts.flow.map(|f| (f.starts_ps, f.exits_ps));
    assert_eq!(flow(&v.absorb(&forks_at(1_000, 1_000, Some(100)))), None);
    assert_eq!(
        flow(&v.absorb(&forks_at(3_000, 3_000, Some(140)))),
        Some((20.0, None))
    );
    assert_eq!(flow(&v.absorb(&forks_at(4_000, 4_000, None))), None);
    assert_eq!(
        flow(&v.absorb(&forks_at(5_000, 5_000, Some(150)))),
        None,
        "no base: the reading before read no counter"
    );
    assert_eq!(
        flow(&v.absorb(&forks_at(6_000, 6_000, Some(157)))),
        Some((7.0, None))
    );
}

/// NO RATE ACROSS A SLEEP: the wall clock ran 60 s while the monotonic one (which stops
/// while the Mac sleeps) ran 1 s, so the counter's growth spans time no interval measures.
/// NEGATIVE CONTROL: the same growth with the clocks together is a rate.
#[test]
fn a_reading_across_sleep_gives_no_start_rate() {
    let mut v = Vitals::new();
    v.absorb(&forks_at(1_000, 1_000, Some(100)));
    let slept = v.absorb(&forks_at(2_000, 61_000, Some(400)));
    assert_eq!(slept.facts.flow, None);
    let awake = v.absorb(&forks_at(3_000, 62_000, Some(430)));
    assert_eq!(awake.facts.flow.map(|f| f.starts_ps), Some(30.0));

    let mut control = Vitals::new();
    control.absorb(&forks_at(1_000, 1_000, Some(100)));
    let s = control.absorb(&forks_at(2_000, 2_000, Some(400)));
    assert_eq!(s.facts.flow.map(|f| f.starts_ps), Some(300.0));
}

/// Record §9: a base and the tail lines written after it restore what a whole checkpoint
/// at the newest line would: every ring's closed buckets, the ten-minute buckets each folded
/// once, the capacities, the gaps and events, and the newest wall clock. A line at or before
/// the base's epoch replays nothing. CONTROL: a line put past the fold mark folds twice.
#[test]
fn a_base_and_its_replayed_lines_restore_the_whole_strip() {
    let mut v = Vitals::new();
    for t in 0..60 {
        v.absorb(&at(t * 1000, 1000));
    }
    let base = v.saved_strip(false);
    assert_eq!(Some(base.through_k + 1), v.strip_open_k());
    let mut lines: Vec<StripLine> = Vec::new();
    let mut through = base.through_k;
    for t in 60..700 {
        v.absorb(&at(t * 1000, 1000));
        if t == 300 {
            v.record_time_gap(TimeGap {
                start_ms: T0 + 200_000,
                end_ms: T0 + 230_000,
                kind: GapKind::Paused,
            });
        }
        let open = v.strip_open_k().expect("read");
        for k in through + 1..open {
            if let Some(mut line) = v.strip_line(k) {
                if k + 1 == open {
                    (line.time_gaps, line.strip_events) = v.strip_marks();
                }
                lines.push(line);
            }
        }
        through = through.max(open - 1);
    }
    assert!(
        v.strip_line(through + 1).is_none(),
        "the open epoch is no line"
    );
    let whole = v.saved_strip(false);
    let mut replayed = base.clone();
    for line in lines.iter().cloned() {
        assert!(replayed.replay(line));
    }
    assert!(!replayed.replay(lines[0].clone()), "a line the strip holds");
    assert_eq!(replayed.through_k, through);
    assert_eq!(replayed.last_wall_ms, lines.last().expect("lines").wall_ms);
    assert_eq!(replayed.caps, whole.caps);
    let closed = |r: &crate::ring::SavedRing| {
        r.buckets
            .iter()
            .filter(|(k, _)| *k <= through)
            .copied()
            .collect::<Vec<_>>()
    };
    for slot in 0..STRIP_RINGS {
        let (mut a, mut b) = (replayed.clone(), whole.clone());
        let (a, b) = (
            a.ring_mut(slot).expect("slot").clone(),
            b.ring_mut(slot).expect("slot").clone(),
        );
        assert_eq!(closed(&a), closed(&b), "slot {slot}");
        assert_eq!(a.long, b.long, "slot {slot}");
    }
    // Restored after a crash, the strip's gaps are the whole one's, and the crash starts at
    // the newest line: the readings after it were in no closed epoch.
    let now = T0 + 701_000;
    let last_wall = lines.last().expect("lines").wall_ms;
    let (mut x, mut y) = (Vitals::new(), Vitals::new());
    x.restore_strip_history(replayed, now);
    y.restore_strip_history(whole.clone(), now);
    let gaps = |v: &Vitals, kind: GapKind| -> Vec<TimeGap> {
        v.strip_marks()
            .0
            .into_iter()
            .filter(|g| g.kind == kind)
            .collect()
    };
    assert_eq!(gaps(&x, GapKind::Paused), gaps(&y, GapKind::Paused));
    assert_eq!(gaps(&x, GapKind::Paused).len(), 1);
    assert_eq!(
        gaps(&x, GapKind::Crash)
            .iter()
            .map(|g| (g.start_ms, g.end_ms))
            .collect::<Vec<_>>(),
        [(last_wall, now)]
    );

    let mut doubled = whole.clone();
    let first = lines[0].clone();
    for ring in &mut doubled.rings {
        ring.folded_below = i64::MIN;
    }
    for (slot, agg) in first.rings {
        if let Some(r) = doubled.ring_mut(usize::from(slot)) {
            r.put_closed(first.k, agg);
        }
    }
    assert_ne!(doubled.rings[0].long, whole.rings[0].long, "CONTROL");
}

/// Record §9: a ring that read nothing while the strip went on (aterm's while the deck is
/// hidden, whose readings are the machine's alone) holds its open bucket past the base's
/// epoch, unfolded; when it reads again, the live ring folds that bucket as a later one opens,
/// and the base with its tail lines replayed folds it too, before the line's own, so every
/// slot's 10 s and ten-minute buckets restore as the live model holds them. Negative control:
/// `SavedRing::put_closed` folding only the line's bucket leaves aterm's ten-minute buckets
/// short of the lagging bucket's read time (the mutant the F4′ review ran).
#[test]
fn a_ring_that_lagged_the_strip_restores_its_old_bucket_folded_once() {
    let mut v = Vitals::new();
    for t in 0..25 {
        v.absorb(&at(t * 1000, 1000));
    }
    for t in 25..100 {
        let mut s = at(t * 1000, 1000);
        s.machine_only = true;
        v.absorb(&s);
    }
    let base = v.saved_strip(false);
    let lagging = base.aterm[Cpu.index()].clone();
    assert!(
        lagging
            .buckets
            .last()
            .is_some_and(|&(k, _)| k <= base.through_k && k >= lagging.folded_below),
        "aterm's CPU ring holds its open bucket behind the base's epoch, unfolded"
    );
    let mut through = base.through_k;
    let mut lines: Vec<StripLine> = Vec::new();
    for t in 100..140 {
        v.absorb(&at(t * 1000, 1000));
        let open = v.strip_open_k().expect("read");
        lines.extend((through + 1..open).filter_map(|k| v.strip_line(k)));
        through = through.max(open - 1);
    }
    let whole = v.saved_strip(false);
    let mut replayed = base;
    for line in lines {
        assert!(replayed.replay(line));
    }
    for slot in 0..STRIP_RINGS {
        let (a, b) = (
            replayed.ring(slot).expect("slot"),
            whole.ring(slot).expect("slot"),
        );
        let closed = |r: &crate::ring::SavedRing| -> Vec<_> {
            r.buckets
                .iter()
                .filter(|(k, _)| *k <= through)
                .copied()
                .collect()
        };
        assert_eq!(closed(a), closed(b), "slot {slot}");
        assert_eq!(a.long, b.long, "slot {slot}");
    }
    let cpu = &replayed.aterm[Cpu.index()];
    let tau = |b: &[crate::ring::SavedBucket]| b.iter().map(|(_, a)| a.tau_ms).sum::<u32>();
    let closed: Vec<_> = cpu
        .buckets
        .iter()
        .filter(|(k, _)| *k <= through)
        .copied()
        .collect();
    assert_eq!(
        tau(&cpu.long),
        tau(&closed),
        "aterm's CPU folds every closed second it read, the lagging bucket's among them"
    );
}

/// `forks_at` reading `i`, one a second.
fn starts_at(i: u64, forks: Option<u64>) -> Sample {
    forks_at(1_000 * (i + 1), 1_000 * (i as i64 + 1), forks)
}

/// The Processes ring's read buckets' largest readings, oldest first.
fn proc_held(v: &Vitals) -> Vec<f64> {
    v.saved_strip(false).rings[Processes.index()]
        .buckets
        .iter()
        .map(|(_, b)| b.max)
        .collect()
}

/// Record §8.1: where a start counter reads, the Processes row is starts a second against
/// the 7-day peak of sustained-30 s starts (`CapBasis::Peak`): its % is the 10 s mean over
/// that peak, its strip each box's mean starts a second over the same peak, and it carries
/// no second series (the count is the run's words, kept in its own ring). CONTROL: the same
/// machine read with no start counter is the count against the machine's limit.
#[test]
fn the_processes_row_heats_by_starts_against_its_seven_day_peak() {
    let peaks = || Peaks {
        proc_starts: DailyMax {
            days: vec![(day_of(T0), 200.0)],
            read_s: PROC_LEARN_S,
        },
        ..Peaks::default()
    };
    let mut v = Vitals::new();
    v.restore_peaks(peaks());
    let mut s = Snapshot::default();
    for i in 0..12 {
        s = v.absorb(&starts_at(i, Some(50 * i)));
    }
    let p = &s.resources[Processes.index()];
    assert_eq!(p.basis, CapBasis::Peak);
    assert_eq!(p.capacity, Some(200.0));
    assert!(close(p.now, 50.0), "{p:?}");
    assert!(close(p.heat_now.map(f64::from), 0.25), "{p:?}");
    assert!(
        p.ring
            .iter()
            .filter_map(|c| c.mean)
            .all(|m| (m - 50.0).abs() < 1e-9),
        "{:?}",
        p.ring
    );
    assert!(p.second.is_none());
    let kept = v.saved_strip(false);
    assert!(
        kept.proc_count.buckets.iter().any(|(_, b)| b.max == 900.0),
        "the count is kept beside the starts"
    );
    assert_eq!(kept.proc_starts, Some(true));

    let mut counts = Vitals::new();
    counts.restore_peaks(peaks());
    let mut c = Snapshot::default();
    for i in 0..12 {
        c = counts.absorb(&starts_at(i, None));
    }
    let p = &c.resources[Processes.index()];
    assert_eq!(
        (p.basis, p.capacity),
        (CapBasis::Fixed, Some(16_000.0)),
        "CONTROL"
    );
    assert!(close(p.heat_now.map(f64::from), 900.0 / 16_000.0));
}

/// Record §8.1: for its first 10 minutes ever (`PROC_LEARN_S`) the row learns its start
/// peak, and its strip's place and its sentence say how far it has read: "learning its start
/// peak: 4 of 10 min". The peak is fed at each bucket's close with the sustained-30 s
/// starts, as Network's is; once learned the row heats against it. CONTROL: a row with no
/// start counter counts and never learns.
#[test]
fn the_processes_row_learns_for_ten_minutes_and_says_so() {
    let reading = |i: u64, forks: Option<u64>| Sample {
        cadence_ms: 10_000,
        ..forks_at(10_000 * (i + 1), 10_000 * (i as i64 + 1), forks)
    };
    let mut v = Vitals::new();
    let mut mid = Snapshot::default();
    let mut s = Snapshot::default();
    for i in 0..64 {
        s = v.absorb(&reading(i, Some(300 * i)));
        if i == 30 {
            mid = s.clone();
        }
        if i == 62 {
            assert_eq!(
                s.resources[Processes.index()].basis,
                CapBasis::Learning,
                "590 s read"
            );
        }
    }
    let p = &mid.resources[Processes.index()];
    assert_eq!(
        (p.basis, p.capacity, p.heat_now),
        (CapBasis::Learning, None, None)
    );
    assert_eq!(mid.facts.proc_read_s, 270);
    assert_eq!(
        crate::words::strip_words(Processes, &mid).and_then(|f| f.first().cloned()),
        Some("learning its start peak: 4 of 10 min".to_string())
    );
    let said = crate::words::row_sentence(Processes, &mid, &[], &crate::words::Locale::default());
    assert!(
        said.starts_with("Processes: learning its start peak, 4 of 10 minutes."),
        "{said}"
    );
    let p = &s.resources[Processes.index()];
    assert_eq!(s.facts.proc_read_s, PROC_LEARN_S);
    assert_eq!((p.basis, p.capacity), (CapBasis::Peak, Some(30.0)));
    assert!(close(p.heat_now.map(f64::from), 1.0));
    assert_eq!(crate::words::strip_words(Processes, &s), None);

    let mut counts = Vitals::new();
    let mut c = Snapshot::default();
    for i in 0..64 {
        c = counts.absorb(&reading(i, None));
    }
    assert_eq!(
        c.resources[Processes.index()].basis,
        CapBasis::Fixed,
        "CONTROL"
    );
    assert_eq!(c.facts.proc_read_s, 0);
    assert_eq!(crate::words::strip_words(Processes, &c), None);
}

/// Record §8.1: a peak basis has no maximum to exceed, so the Processes row never draws an
/// over cap: a burst far past its peak heats the row to the top of its ramp and no further,
/// and no box holds over time. CONTROL: the burst is fifty times the peak.
#[test]
fn the_processes_row_never_sets_over() {
    let mut v = Vitals::new();
    v.restore_peaks(Peaks {
        proc_starts: DailyMax {
            days: vec![(day_of(T0), 10.0)],
            read_s: PROC_LEARN_S,
        },
        ..Peaks::default()
    });
    let mut s = Snapshot::default();
    for i in 0..25 {
        s = v.absorb(&starts_at(i, Some(500 * i)));
    }
    let p = &s.resources[Processes.index()];
    assert_eq!(p.heat_now, Some(1.0));
    assert!(!p.over);
    assert!(
        p.ring
            .iter()
            .all(|c| c.over_ms == 0.0 && c.over_share() == 0.0)
    );
    assert_eq!(p.live_box(s.at_unix_ms).1, 0.0);
    assert!(
        p.now.zip(p.capacity).is_some_and(|(n, c)| n / c >= 50.0),
        "CONTROL: {p:?}"
    );
}

/// What the Processes rings hold follows the start counter (record §8.2): a run whose
/// readings carry one reads starts; a reading that misses it in such a run is unread, not a
/// count; and where the meaning moves, the rings that held the other are cleared and the
/// strip's checkpoint is told to write a whole base. A run that reads no counter counts over
/// a restored starts strip. CONTROL: a run that always counts never clears.
#[test]
fn the_processes_rings_never_mix_starts_and_counts() {
    let mut v = Vitals::new();
    for i in 0..19 {
        v.absorb(&starts_at(i, None));
    }
    let rewrites = v.strip_rewrites();
    assert_eq!(proc_held(&v), [900.0, 900.0], "counts");
    v.absorb(&starts_at(19, Some(0)));
    assert_eq!(v.strip_rewrites(), rewrites + 1);
    assert!(proc_held(&v).is_empty(), "the counts are cleared");
    v.absorb(&starts_at(20, Some(40)));
    let s = v.absorb(&starts_at(21, None));
    assert_eq!(
        v.strip_rewrites(),
        rewrites + 1,
        "a missed counter changes nothing"
    );
    assert_eq!(proc_held(&v), [40.0]);
    assert_ne!(s.resources[Processes.index()].basis, CapBasis::Fixed);
    assert_eq!(
        s.resources[Processes.index()].recent.last().map(|r| r.v),
        Some(40.0),
        "the missed reading is unread"
    );

    let saved = v.saved_strip(true);
    let mut later = Vitals::new();
    later.restore_strip_history(saved, T0 + 25_000);
    let after = later.strip_rewrites();
    later.absorb(&starts_at(25, None));
    assert_eq!(later.strip_rewrites(), after + 1);
    assert_eq!(
        proc_held(&later),
        [900.0],
        "a run with no counter counts over restored starts"
    );

    let mut counts = Vitals::new();
    for i in 0..30 {
        counts.absorb(&starts_at(i, None));
    }
    assert_eq!(counts.strip_rewrites(), 0, "CONTROL");
}

/// With no start counter the row is the count against the machine's limit even where the
/// user's own limit is nearer (record §8.1 retires meter §3's binding per-user limit), and a
/// scope's writes run is carried raw.
#[test]
fn the_processes_row_counts_against_the_machines_limit_and_a_writes_run_is_carried_raw() {
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for i in 0..12 {
        let mut r = whole(10 * i);
        r.room.user_procs_max = Some(1_000);
        s = v.absorb(&r);
    }
    assert_eq!(
        s.resources[Processes.index()].binding().capacity,
        Some(16_000.0),
        "the machine's limit, not the user's"
    );
    assert!(
        s.resource(Disk, &[7])
            .tab
            .run
            .iter()
            .any(|c| c.mean > Some(0.0)),
        "the writes run itself is carried raw"
    );
}

#[test]
fn token_rate_hygiene() {
    let mut v = Vitals::new();
    let tok = |i: i64, out: u64, unread: usize, more: bool, complete: bool| Sample {
        at_unix_ms: T0 + i * 10_000,
        mono_ms: i as u64 * 10_000,
        cadence_ms: 10_000,
        tabs: vec![tab(7, 200, "Tab 1", "claude"), tab(8, 300, "Tab 2", "zsh")],
        tokens: vec![TabTokens {
            key: 7,
            session: "s".into(),
            out_total: out,
            in_total: out * 10,
            unread,
            unread_more: more,
            complete,
            read_ms: None,
        }],
        ..Sample::default()
    };
    let steps: [(u64, usize, bool, bool, Option<f64>); 11] = [
        (0, 0, false, true, None),
        (6_000, 0, false, true, Some(36_000.0)),
        (12_000, 1, false, true, None),
        (18_000, 1, false, true, Some(36_000.0)),
        (24_000, 1, true, true, None),
        (30_000, 1, true, true, Some(36_000.0)),
        (36_000, 1, true, false, None),
        (42_000, 1, true, true, None),
        (48_000, 1, true, true, Some(36_000.0)),
        (108_000, 1, true, true, None),
        (114_000, 1, true, true, Some(36_000.0)),
    ];
    for (i, (out, unread, more, complete, want)) in steps.into_iter().enumerate() {
        let s = v.absorb(&tok(i as i64, out, unread, more, complete));
        match want {
            Some(w) => assert!(
                close(open(&s, Tokens), w),
                "step {i}: {:?}",
                open(&s, Tokens)
            ),
            None => assert_eq!(open(&s, Tokens), None, "step {i}"),
        }
        assert_eq!(s.facts.agents, 1);
        let t2 = s.resource(Tokens, &[8]).tab;
        assert_eq!(t2.missing, Some(NoFigure::NoAgent), "Tab 2 runs no agent");
    }
    let mut out = 114_000;
    for i in 11..22 {
        out += if i == 16 { 30_000 } else { 6_000 };
        let _ = v.absorb(&tok(i, out, 1, true, true));
    }
    assert_eq!(v.peaks().tokens.peak(day_of(T0)), Some(36_000.0));
}

#[test]
fn token_rates_span_folds_not_readings() {
    let mut v = Vitals::new();
    let reading = |i: u64, k: u64, fold_s: i64, slept_ms: i64| Sample {
        at_unix_ms: T0 + i as i64 * 1000 + slept_ms,
        mono_ms: i * 1000,
        cadence_ms: 1000,
        tabs: vec![tab(7, 200, "Tab 1", "claude")],
        tokens: vec![TabTokens {
            key: 7,
            session: "s".into(),
            out_total: 45_000 * k,
            in_total: 450_000 * k,
            complete: true,
            read_ms: Some(fold_s * 1000),
            ..TabTokens::default()
        }],
        ..Sample::default()
    };
    let per_min = 3_000.0 * 60.0;
    for i in 0..=100u64 {
        let k = (i / 15).min(2);
        let s = v.absorb(&reading(i, k, k as i64 * 15, 0));
        let now = s.resource(Tokens, &[]).now;
        match i {
            0..15 => assert_eq!(open(&s, Tokens), None, "{i}: one fold"),
            15..=90 => {
                assert!(
                    close(open(&s, Tokens), per_min),
                    "{i}: {:?}",
                    open(&s, Tokens)
                );
                assert!(close(now, per_min), "{i}: now {now:?}");
            }
            100 => assert_eq!(open(&s, Tokens), None, "past the hold"),
            _ => {}
        }
    }
    let since = 45_000.0 / 71.0 * 60.0;
    let s = v.absorb(&reading(101, 3, 101, 0));
    assert!(close(open(&s, Tokens), since), "{:?}", open(&s, Tokens));
    let s = v.absorb(&reading(102, 3, 101, 0));
    assert!(close(open(&s, Tokens), since), "held");
    let s = v.absorb(&reading(103, 3, 101, 120_000));
    assert_eq!(open(&s, Tokens), None, "a sleep drops the rate");
    let s = v.absorb(&reading(104, 3, 101, 120_000));
    assert_eq!(open(&s, Tokens), None, "no new fold since the sleep");
    let spaced = |i, k, fold_s, slept_ms| Sample {
        cadence_ms: 10_000,
        ..reading(i, k, fold_s, slept_ms)
    };
    let s = v.absorb(&spaced(111, 4, 111, 120_000));
    assert_eq!(open(&s, Tokens), None, "the first fresh post-sleep fold");
    let s = v.absorb(&spaced(121, 5, 121, 120_000));
    assert!(close(open(&s, Tokens), 4_500.0 * 60.0), "new interval");

    let mut v = Vitals::new();
    let _ = v.absorb(&spaced(0, 0, 0, 0));
    let s = v.absorb(&spaced(10, 1, 10, 0));
    assert!(close(open(&s, Tokens), 4_500.0 * 60.0));
    let s = v.absorb(&spaced(20, 2, 10, 0));
    assert_eq!(open(&s, Tokens), None, "changed totals at the same ms");
    let s = v.absorb(&spaced(30, 3, 30, 0));
    assert!(
        close(open(&s, Tokens), 2_250.0 * 60.0),
        "new totals baseline"
    );
    let s = v.absorb(&spaced(40, 3, 30, 0));
    assert!(
        close(open(&s, Tokens), 2_250.0 * 60.0),
        "identical fold held"
    );
    let mut incomplete = spaced(50, 3, 30, 0);
    incomplete.tokens[0].complete = false;
    let s = v.absorb(&incomplete);
    assert_eq!(open(&s, Tokens), None, "incomplete at the same ms");
    let mut repeated = spaced(60, 3, 30, 0);
    repeated.tokens[0].complete = false;
    let s = v.absorb(&repeated);
    assert_eq!(open(&s, Tokens), None, "an incomplete fold holds no rate");
    let s = v.absorb(&spaced(70, 4, 70, 0));
    assert_eq!(open(&s, Tokens), None, "both folds must be complete");
    let s = v.absorb(&spaced(80, 5, 80, 0));
    assert!(close(open(&s, Tokens), 4_500.0 * 60.0), "complete again");
}

fn disk_reading(i: i64, available: u64, facts: Option<DiskFacts>) -> Sample {
    Sample {
        at_unix_ms: T0 + i * 10_000,
        mono_ms: i as u64 * 10_000,
        cadence_ms: 10_000,
        volume: Some(VolumeSample {
            total: 2_000_000_000_000,
            free: available,
            reserved: 0,
            mount: "/".into(),
        }),
        disk: facts,
        ..Sample::default()
    }
}

fn watched(level: DiskLevel) -> Option<DiskFacts> {
    Some(DiskFacts {
        level: Some(level),
        watch: true,
        ..DiskFacts::default()
    })
}

#[test]
fn disk_history_and_rates() {
    let mut v = Vitals::new();
    let mut free = 500_000_000_000u64;
    let mut s = Snapshot::default();
    for i in 0..420 {
        s = v.absorb(&disk_reading(i, free, watched(DiskLevel::Ok)));
        match i {
            356 => assert_eq!(s.facts.disk_rates.rate60, None),
            357 => {
                assert!(close(s.facts.disk_rates.rate60, 1e7));
                assert!(close(s.facts.disk_rates.rate10, 1e7));
                assert!(close(s.facts.disk_rates.rate5, 1e7));
                assert_eq!(s.facts.filling, None);
            }
            416 => assert_eq!(s.facts.filling, None),
            _ => {}
        }
        free -= 100_000_000;
    }
    let f = s.facts.filling.expect("time to full under a day");
    assert!(!f.burst && !f.outranks_level());
    assert!((f.secs - 458.1e9 / 1e7).abs() < 1.0, "{f:?}");
    assert_eq!(s.resources[Disk.index()].state, None);
    for i in 420..450 {
        free -= 2_000_000_000;
        s = v.absorb(&disk_reading(i, free, None));
    }
    let f = s.facts.filling.expect("a burst");
    assert!(f.burst && f.outranks_level(), "{f:?}");
    assert_eq!(s.resources[Disk.index()].state, Some(State::Warn));
    let w = s.free_history.window(s.at_unix_ms);
    let read: Vec<u64> = w.iter().flatten().copied().collect();
    assert_eq!(read.len(), 8, "75 minutes span 8 ten-minute buckets");
    assert_eq!(read.last(), Some(&free), "the least free space");
    assert!(read.windows(2).all(|p| p[1] < p[0]));
    for i in 450..480 {
        s = v.absorb(&disk_reading(i, free, None));
    }
    let f = s.facts.filling.expect("full within a day");
    assert!(!f.burst && !f.outranks_level(), "{f:?}");
    assert_eq!(s.resources[Disk.index()].state, None);
    let off = DiskFacts {
        level: Some(DiskLevel::Low),
        stuck: 2,
        watch: false,
        ..DiskFacts::default()
    };
    let s = v.absorb(&disk_reading(480, free, Some(off)));
    assert_eq!(s.resources[Disk.index()].state, None);
    let s = v.absorb(&disk_reading(481, free, watched(DiskLevel::Low)));
    assert_eq!(s.resources[Disk.index()].state, Some(State::Warn));
    let s = v.absorb(&disk_reading(482, free, watched(DiskLevel::EmergencyFiles)));
    assert_eq!(s.resources[Disk.index()].state, Some(State::Critical));
}

#[test]
fn missed_readings_and_clock_steps_keep_the_filling_clause() {
    let mut v = Vitals::new();
    let free = |i: i64| 500_000_000_000 - i as u64 * 100_000_000;
    let missed = |i: i64| i == 100 || (200..204).contains(&i);
    for i in (0..640).filter(|&i| !missed(i)) {
        let s = v.absorb(&disk_reading(i, free(i), None));
        if i < 417 {
            continue;
        }
        let unknown = (559..561).contains(&i);
        assert_eq!(s.facts.disk_rates.rate60.is_none(), unknown, "{i}");
        assert_eq!(s.facts.filling.is_some(), !unknown, "{i}");
    }
    let mut back = disk_reading(640, free(640), None);
    back.at_unix_ms -= 7_200_000;
    let s = v.absorb(&back);
    assert!(close(s.facts.disk_rates.rate60, 1e7));
    assert!(s.facts.filling.is_some());
    assert_eq!(
        s.free_history
            .window(s.at_unix_ms)
            .last()
            .copied()
            .flatten(),
        Some(free(640))
    );
}

#[test]
fn a_restored_free_history_reads_on_only_from_its_volume() {
    let mut before = Vitals::new();
    for i in 0..120 {
        let _ = before.absorb(&disk_reading(i, 400_000_000_000 - i as u64, None));
    }
    let saved = before.free_history().saved().expect("20 minutes read");
    assert_eq!(saved.volume.mount, "/");
    let after = disk_reading(200, 1, None);
    let read = |s: &Snapshot| {
        s.free_history
            .window(s.at_unix_ms)
            .into_iter()
            .flatten()
            .count()
    };

    let mut v = Vitals::new();
    v.restore_free_history(FreeHistory::restored(&saved, after.at_unix_ms));
    assert_eq!(
        read(&v.absorb(&after)),
        3,
        "two kept ten minutes and the new one"
    );
    let mut moved = after.clone();
    if let Some(vol) = moved.volume.as_mut() {
        vol.mount = "/Volumes/Backup".into();
    }
    let mut v = Vitals::new();
    v.restore_free_history(FreeHistory::restored(&saved, after.at_unix_ms));
    assert_eq!(read(&v.absorb(&moved)), 1, "another volume starts afresh");
}

#[test]
fn over_is_a_queue_on_the_cores_or_heavy_swapping() {
    let run = |load1: f64, busy_half: bool, swap_kib_per_s: u64| {
        let mut v = Vitals::new();
        let mut s = Snapshot::default();
        for t in 0..5u64 {
            let mut r = at(t * 1000, 1000);
            r.load1 = Some(load1);
            if !busy_half && let Some(c) = r.cores.as_mut() {
                c[1].busy = c[1].total;
            }
            r.swapped_kib = Some(swap_kib_per_s * t);
            s = v.absorb(&r);
        }
        (
            s.resources[Cpu.index()].over,
            s.resources[Memory.index()].over,
        )
    };
    assert_eq!(run(2.5, false, 0), (true, false));
    assert_eq!(run(1.5, false, 0), (false, false), "no queue");
    assert_eq!(run(2.5, true, 0), (false, false), "1.5 of 2 cores busy");
    assert_eq!(run(0.5, true, 4 * 1024), (false, true));
    assert_eq!(run(0.5, true, 3 * 1024), (false, false));
}

/// The CPU row's well at each reading's thermal state (A2 §3.2), through the model and the
/// well's words, as the deck draws it: the mark and its ink, or none.
fn cpu_well(
    v: &mut Vitals,
    t_s: u64,
    thermal: Option<Thermal>,
) -> Option<(crate::words::Mark, crate::words::InkKind)> {
    let s = v.absorb(&Sample {
        thermal,
        ..at(t_s * 1000, 1000)
    });
    crate::words::well(Cpu, &s.resources[Cpu.index()])
}

fn os_level(raw: u8, low_power: Option<bool>) -> Thermal {
    let level = [
        Throttle::None,
        Throttle::Light,
        Throttle::Heavy,
        Throttle::Severe,
    ][usize::from(raw)];
    Thermal {
        level,
        raw,
        low_power,
        exact: true,
    }
}

/// The CPU well draws the existing ● at the OS's exact serious (in `warn`) and critical (in
/// `critical`), and nothing at nominal or fair, with or without Low Power Mode, or where no
/// level was read; it follows the latest reading both ways. NEGATIVE CONTROL: the same row at
/// fair, one reading before serious and one after it, draws none.
#[test]
fn the_cpu_well_draws_a_dot_only_at_an_exact_serious_or_critical_thermal_state() {
    use crate::words::{InkKind, Mark};
    let mut v = Vitals::new();
    let mut t = 0;
    let mut well = |thermal| {
        t += 1;
        cpu_well(&mut v, t, thermal)
    };
    assert_eq!(well(None), None, "no level read");
    for low_power in [None, Some(false), Some(true)] {
        assert_eq!(well(Some(os_level(0, low_power))), None, "nominal");
        assert_eq!(well(Some(os_level(1, low_power))), None, "fair");
    }
    assert_eq!(
        well(Some(os_level(2, None))),
        Some((Mark::Dot, InkKind::Warn)),
        "serious"
    );
    assert_eq!(well(Some(os_level(1, None))), None, "back to fair");
    assert_eq!(
        well(Some(os_level(3, Some(true)))),
        Some((Mark::Dot, InkKind::Critical)),
        "critical"
    );
    assert_eq!(
        well(Some(os_level(2, Some(false)))),
        Some((Mark::Dot, InkKind::Warn)),
        "down to serious"
    );
    assert_eq!(well(None), None, "unread again");
    let s = v.absorb(&Sample {
        thermal: Some(os_level(3, None)),
        ..at(1_000_000, 1000)
    });
    assert_eq!(s.resources[Cpu.index()].state, Some(State::Critical));
    for res in [Gpu, Memory, Network, Tokens] {
        assert_eq!(s.resources[res.index()].state, None, "{res:?}");
    }
}

/// A level the counter heuristic estimates (Linux's thermal zones and throttle counters,
/// `exact: false`) draws no ●, at any rank. NEGATIVE CONTROL: the same ranks reported exactly
/// by the OS draw one at heavy (serious) and severe (critical).
#[test]
fn a_heuristic_thermal_level_draws_no_cpu_dot() {
    let mut v = Vitals::new();
    let mut t = 0;
    for level in [
        Throttle::None,
        Throttle::Light,
        Throttle::Heavy,
        Throttle::Severe,
    ] {
        let heuristic = Thermal {
            level,
            raw: 0,
            low_power: None,
            exact: false,
        };
        t += 1;
        assert_eq!(cpu_well(&mut v, t, Some(heuristic)), None, "{level:?}");
        assert_eq!(heuristic.state(), None, "{level:?}");
        let exact = Thermal {
            exact: true,
            raw: u8::try_from(level.rank()).expect("a rank"),
            ..heuristic
        };
        t += 1;
        let drawn = cpu_well(&mut v, t, Some(exact));
        assert_eq!(
            drawn.is_some(),
            level >= Throttle::Heavy,
            "CONTROL: exact {level:?} draws {drawn:?}"
        );
    }
}

/// The snapshot carries the Neural Engine reader's latest reading as it is, never a level
/// over a window (A2 §3.1: the tile draws now only), and the check that retired it.
#[test]
fn the_snapshot_carries_the_neural_engines_latest_reading() {
    let mut v = Vitals::new();
    let reading = |powered| Some(Ok(AneSample { cores: 16, powered }));
    let mut t = 0;
    let mut ane = |ane| {
        t += 1;
        v.absorb(&Sample {
            ane,
            ..at(t * 1000, 1000)
        })
        .ane
    };
    assert_eq!(ane(None), None, "no reader");
    assert_eq!(ane(reading(true)), reading(true));
    assert_eq!(ane(reading(false)), reading(false), "off at once");
    assert_eq!(ane(reading(true)), reading(true));
    assert_eq!(ane(Some(Err(AneCheck::Cost))), Some(Err(AneCheck::Cost)));
}

#[test]
fn the_processes_state_follows_the_jobs_well() {
    use crate::jobs::{Findings, JobsView, ZombieParent};
    let zombie = |tab| ZombieParent {
        ppid: 9,
        name: "server".into(),
        count: 3,
        per_min_deci: 0,
        tab,
    };
    let mut s = Snapshot::default();
    s.set_jobs(Some(Arc::new(JobsView::default())));
    assert_eq!(s.resources[Processes.index()].state, None, "a calm machine");
    let view = |tab| JobsView {
        findings: Findings {
            zombies: vec![zombie(tab)],
            ..Findings::default()
        },
        ..JobsView::default()
    };
    s.set_jobs(Some(Arc::new(view(Some(7)))));
    assert_eq!(s.resources[Processes.index()].state, Some(State::Zombies));
    s.set_jobs(Some(Arc::new(view(None))));
    assert_eq!(s.resources[Processes.index()].state, None);
    assert!(State::Critical > State::Warn && State::Warn > State::Stuck);
    assert!(State::Stuck > State::Zombies && State::Quiet > State::Waiting);
    assert!(State::Waiting > State::Stopped);
}

#[test]
fn the_measured_link_holds_while_the_mix_holds() {
    let link = |name: &str, kind, total: u64, rate_bps| LinkSample {
        name: name.into(),
        kind,
        up: true,
        counts: true,
        rx: total,
        tx: 0,
        rate_bps: Some(rate_bps),
        ..LinkSample::default()
    };
    let wifi = |total| link("en0", LinkKind::Wifi, total, 54_000_000);
    let wired = |total| link("en7", LinkKind::Ethernet, total, 1_000_000_000);
    let mut v = Vitals::new();
    let mut read = |t: u64, links: Vec<LinkSample>| {
        let mut s = at(t * 1000, 1000);
        s.links = Some(links);
        let snap = v.absorb(&s);
        let l = snap.facts.link.expect("a counted link");
        let cap = snap.resources[Network.index()].capacity;
        (l.name, cap)
    };
    let (mut w, mut e) = (1_000u64, 1_000u64);
    assert_eq!(
        read(0, vec![wifi(w), wired(e)]),
        ("en7".into(), Some(125e6))
    );
    for t in 1..40 {
        if t % 3 == 0 {
            w += 300;
        } else {
            e += 200;
        }
        assert_eq!(
            read(t, vec![wifi(w), wired(e)]).0,
            "en7",
            "idle reading {t}"
        );
    }
    for t in 40..40 + u64::from(LINK_LEAD_READINGS) {
        w += 2_000_000;
        let want = if t + 1 < 40 + u64::from(LINK_LEAD_READINGS) {
            "en7"
        } else {
            "en0"
        };
        assert_eq!(read(t, vec![wifi(w), wired(e)]).0, want, "busy reading {t}");
    }
    assert_eq!(
        read(45, vec![wifi(w + 2_000_000), wired(e)]),
        ("en0".into(), Some(6.75e6))
    );
    let mut t = 46;
    for burst in [true, true, false, true, true, true, true] {
        if burst {
            e += 10_000_000;
        }
        assert_eq!(read(t, vec![wifi(w + 2_000_000), wired(e)]).0, "en0");
        t += 1;
    }
    assert_eq!(read(t, vec![wired(e + 10_000_000)]).0, "en7");
}

/// A process of tab 7's `cargo` (pid 201), own user's, with its own start.
fn child(pid: u32, start: u64, name: &str, written: u64) -> ProcSample {
    ProcSample {
        start: Some(start),
        disk_written: Some(written),
        uid: Some(501),
        ..proc(pid, 201, 10, 20, name)
    }
}

fn write_bps(s: &Snapshot, pid: u32) -> Option<f64> {
    s.procs
        .iter()
        .find(|p| p.pid == pid)
        .and_then(|p| p.write_bps)
}

fn writer<'a>(s: &'a Snapshot, name: &str) -> Option<&'a Writer> {
    s.facts.writers.iter().find(|w| w.name == name)
}

#[test]
fn a_process_born_between_sweeps_counts_what_it_wrote_since_birth() {
    let mut v = Vitals::new();
    let _ = v.absorb(&at(0, 1000));
    let _ = v.absorb(&at(1000, 1000));
    let mut r = at(2000, 1000);
    r.procs
        .as_mut()
        .expect("swept")
        .push(child(300, 300, "rustc", 5_000_000));
    let s = v.absorb(&r);
    assert!(close(write_bps(&s, 300), 5e6), "{:?}", write_bps(&s, 300));
    let w = writer(&s, "rustc").expect("a writer");
    assert_eq!((w.scope, w.n, w.bytes), (ProcScope::Tab(7), 1, 5_000_000));
    assert!(
        close(s.tab(7).and_then(|t| t.scope.write_bps), 7e6),
        "cargo's and rustc's"
    );
}

/// NEGATIVE CONTROL: across a gap no window forms, so even a newborn counts nothing.
#[test]
fn the_first_sweep_after_a_gap_counts_nothing() {
    let mut v = Vitals::new();
    let _ = v.absorb(&at(0, 1000));
    let _ = v.absorb(&at(1000, 1000));
    let mut r = at(14_000, 1000);
    r.procs
        .as_mut()
        .expect("swept")
        .push(child(300, 300, "rustc", 5_000_000));
    let s = v.absorb(&r);
    assert_eq!(write_bps(&s, 300), None);
    assert_eq!(write_bps(&s, 201), None, "nor the old cargo");
    assert!(s.facts.writers.is_empty(), "{:?}", s.facts.writers);
    assert!(
        (s.facts.writes_covered_s - 1.0).abs() < 1e-9,
        "only the first window"
    );
    let s = v.absorb(&at(15_000, 1000));
    assert!(close(write_bps(&s, 201), 2e6));
    assert!((s.facts.writes_covered_s - 2.0).abs() < 1e-9);
}

/// NEGATIVE CONTROL: a sweep that stopped at its cap may have missed any process, so the
/// next counts none as born since it.
#[test]
fn after_a_truncated_sweep_no_process_counts_as_newborn() {
    let mut v = Vitals::new();
    let _ = v.absorb(&at(0, 1000));
    let mut cut = at(1000, 1000);
    cut.procs_truncated = true;
    let _ = v.absorb(&cut);
    let mut r = at(2000, 1000);
    r.procs
        .as_mut()
        .expect("swept")
        .push(child(300, 300, "rustc", 5_000_000));
    let s = v.absorb(&r);
    assert_eq!(write_bps(&s, 300), None);
    assert!(
        close(write_bps(&s, 201), 2e6),
        "one it read last time still counts"
    );
    let mut r = at(3000, 1000);
    r.procs
        .as_mut()
        .expect("swept")
        .push(child(300, 300, "rustc", 6_000_000));
    let s = v.absorb(&r);
    assert!(close(write_bps(&s, 300), 1e6), "from the next sweep on");
}

/// A LIVE PROCESS ONE SWEEP MISSED IS NO NEWBORN: a long-lived writer (its start older than
/// the newest the last sweep read) that one whole sweep skipped reads, on its return, nothing
/// for that window, never its life's 50 GB as one second's writes. NEGATIVE CONTROL: a process
/// truly born in the same window (its start the newest) counts its whole counter.
#[test]
fn a_live_process_one_sweep_missed_never_counts_its_life_as_one_window() {
    let mut v = Vitals::new();
    let old = |written| child(500, 5, "postgres", written);
    let mut r = at(0, 1000);
    let procs = r.procs.as_mut().expect("swept");
    procs.push(old(50_000_000_000));
    procs.push(child(300, 300, "rustc", 1_000_000));
    let _ = v.absorb(&r);
    let mut r = at(1000, 1000);
    r.procs
        .as_mut()
        .expect("swept")
        .push(child(300, 300, "rustc", 2_000_000));
    let _ = v.absorb(&r);
    let mut r = at(2000, 1000);
    let procs = r.procs.as_mut().expect("swept");
    procs.push(old(50_000_001_000));
    procs.push(child(300, 300, "rustc", 3_000_000));
    procs.push(child(600, 600, "cc", 3_000_000));
    let s = v.absorb(&r);
    assert_eq!(write_bps(&s, 500), None, "{:?}", write_bps(&s, 500));
    assert!(writer(&s, "postgres").is_none(), "{:?}", s.facts.writers);
    assert!(
        s.facts.hour_writers.iter().all(|p| p.name != "postgres"),
        "{:?}",
        s.facts.hour_writers
    );
    assert!(close(write_bps(&s, 600), 3e6), "{:?}", write_bps(&s, 600));
    assert_eq!(writer(&s, "cc").map(|w| w.bytes), Some(3_000_000));
    let mut r = at(3000, 1000);
    let procs = r.procs.as_mut().expect("swept");
    procs.push(old(50_000_003_000));
    let s = v.absorb(&r);
    assert!(
        close(write_bps(&s, 500), 2_000.0),
        "from the next sweep on: {:?}",
        write_bps(&s, 500)
    );
}

#[test]
fn a_reused_pid_never_reads_as_the_old_processs_delta() {
    let mut v = Vitals::new();
    for t in [0, 1000] {
        let mut r = at(t, 1000);
        r.procs
            .as_mut()
            .expect("swept")
            .push(child(300, 300, "rustc", 400_000_000));
        let _ = v.absorb(&r);
    }
    let mut r = at(2000, 1000);
    r.procs
        .as_mut()
        .expect("swept")
        .push(child(300, 301, "cp", 500_000_000));
    let s = v.absorb(&r);
    assert!(
        close(write_bps(&s, 300), 5e8),
        "the new process's own bytes, not 100 MB over the old one's: {:?}",
        write_bps(&s, 300)
    );
    assert!(writer(&s, "rustc").is_none());
    assert_eq!(writer(&s, "cp").map(|w| w.bytes), Some(500_000_000));
}

#[test]
fn a_zombie_row_gives_the_writes_after_its_last_live_sweep() {
    let mut v = Vitals::new();
    for (t, written) in [(0, 10_000_000), (1000, 20_000_000)] {
        let mut r = at(t, 1000);
        r.procs
            .as_mut()
            .expect("swept")
            .push(child(300, 300, "rustc", written));
        let _ = v.absorb(&r);
    }
    let mut r = at(2000, 1000);
    r.procs.as_mut().expect("swept").push(ProcSample {
        cpu_ns: None,
        footprint: None,
        state: ProcState::Zombie,
        ..child(300, 300, "rustc", 68_000_000)
    });
    let s = v.absorb(&r);
    assert!(close(write_bps(&s, 300), 48e6), "{:?}", write_bps(&s, 300));
    let s = v.absorb(&at(3000, 1000));
    assert_eq!(write_bps(&s, 300), None, "reaped");
}

#[test]
fn a_program_split_over_many_processes_is_one_writer() {
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for t in [0u64, 1000] {
        let mut r = at(t, 1000);
        let procs = r.procs.as_mut().expect("swept");
        for i in 0..9 {
            procs.push(child(400 + i, 400 + u64::from(i), "rustc", 3_400 * t));
        }
        procs.push(ProcSample {
            start: Some(900),
            disk_written: Some(10_000 * t),
            uid: Some(501),
            ..proc(900, 1, 10, 20, "cp")
        });
        s = v.absorb(&r);
    }
    let rustc = &s.facts.writers[0];
    assert_eq!(
        (rustc.name.as_str(), rustc.scope, rustc.n),
        ("rustc", ProcScope::Tab(7), 9),
        "{:?}",
        s.facts.writers
    );
    assert!(close(Some(rustc.bytes_per_s), 30.6e6), "{rustc:?}");
    assert_eq!(rustc.bytes, 30_600_000);
    let cp = writer(&s, "cp").expect("cp");
    assert_eq!((cp.scope, cp.n), (ProcScope::Other, 1));
    // NEGATIVE CONTROL: process by process, cp outranks every rustc.
    assert!(
        (400..409).all(|pid| write_bps(&s, pid).is_some_and(|r| r < cp.bytes_per_s)),
        "{:?}",
        s.procs
    );
}

/// EACH SCOPE KEEPS ITS OWN FIRST WRITERS THIS SWEEP: five programs of the rest of the Mac,
/// each writing more than tab 7's cargo, leave cargo among the writers (the lens's "this
/// tab" finds it), and the rest of the Mac keeps its first four. NEGATIVE CONTROL: by rate
/// alone cargo is sixth, past a single first four.
#[test]
fn each_scope_keeps_its_own_first_writers_this_sweep() {
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for t in [0u64, 1000] {
        let mut r = at(t, 1000);
        let procs = r.procs.as_mut().expect("swept");
        for i in 0..5u32 {
            procs.push(ProcSample {
                start: Some(900 + u64::from(i)),
                disk_written: Some((10_000 + u64::from(i)) * t),
                uid: Some(501),
                ..proc(900 + i, 1, 10, 20, &format!("cp{i}"))
            });
        }
        s = v.absorb(&r);
    }
    let others: Vec<&str> = s
        .facts
        .writers
        .iter()
        .filter(|w| w.scope == ProcScope::Other)
        .map(|w| w.name.as_str())
        .collect();
    assert_eq!(
        others,
        ["cp4", "cp3", "cp2", "cp1"],
        "{:?}",
        s.facts.writers
    );
    let cargo = writer(&s, "cargo").expect("tab 7's writer");
    assert_eq!(cargo.scope, ProcScope::Tab(7));
    assert!(
        (900..905).all(|pid| write_bps(&s, pid).is_some_and(|r| r > cargo.bytes_per_s)),
        "{:?}",
        s.procs
    );
}

#[test]
fn a_tabs_hour_names_its_top_writer_with_its_coverage() {
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for t in 0..=30u64 {
        s = v.absorb(&at(t * 1000, 1000));
    }
    assert!((s.facts.writes_covered_s - 30.0).abs() < 1e-9);
    let cargo = &s.facts.hour_writers[0];
    assert_eq!(
        (cargo.name.as_str(), cargo.scope),
        ("cargo", ProcScope::Tab(7))
    );
    assert_eq!(
        cargo.per_hour(s.facts.writes_covered_s),
        None,
        "half a minute read"
    );
    for t in 31..=90u64 {
        s = v.absorb(&at(t * 1000, 1000));
    }
    let cargo = &s.facts.hour_writers[0];
    assert_eq!((cargo.bytes, cargo.n_max), (180_000_000, 1));
    let per_hour = cargo.per_hour(s.facts.writes_covered_s);
    assert!(close(per_hour, 7.2e9), "{per_hour:?}");
    // Hidden for ten minutes (no sweep): the hour keeps what it read and its coverage.
    for t in 91..=690u64 {
        let mut r = at(t * 1000, 1000);
        r.procs = None;
        r.machine_only = true;
        s = v.absorb(&r);
    }
    assert!((s.facts.writes_covered_s - 90.0).abs() < 1e-9);
    assert!(close(s.facts.hour_writers[0].per_hour(90.0), 7.2e9));
}

#[test]
fn no_disk_counter_means_no_coverage() {
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for t in 0..90u64 {
        let mut r = at(t * 1000, 1000);
        for p in r.procs.as_mut().expect("swept") {
            p.disk_written = None;
        }
        s = v.absorb(&r);
    }
    assert_eq!(s.facts.writes_covered_s, 0.0);
    assert!(s.facts.hour_writers.is_empty());
    assert!(s.facts.writers.is_empty());
    assert_eq!(
        s.resources[Disk.index()].aterm.missing,
        Some(NoFigure::NotReadHere)
    );
}

/// Readings asked on the half-slot (`due_on_phase`, record §5.2) stand each in its own second
/// of the wall clock, which every snapshot's slot grid now is: each reading of a second's
/// cadence opens one slot, and each ten-second bucket weighs its whole ten seconds. CONTROL:
/// readings paced from the moment each was asked drift through the second, and on the wall
/// clock's seconds they open none and then two, which the adaptive grid this replaced hid.
#[test]
fn readings_on_the_half_slot_stand_each_in_its_own_second() {
    let blank = |at_ms: i64| Sample {
        at_unix_ms: at_ms,
        cadence_ms: 1000,
        gpu: Some(GpuSample {
            device_pct: Some(10),
            ..GpuSample::default()
        }),
        ..Sample::default()
    };
    let mut v = Vitals::new();
    let mut prev: Option<i64> = None;
    let mut snap = Snapshot::default();
    for i in 0..40 {
        let t = T0 + 500 + i * 1_000;
        snap = v.absorb(&blank(t));
        assert_eq!(snap.slot_grid, SlotGrid::SECONDS);
        if let Some(p) = prev {
            assert_eq!(snap.slot_grid.opened(p, t), 1, "{t}");
        }
        prev = Some(t);
    }
    let gpu = &snap.resources[Gpu.index()];
    let closed = &gpu.ring[gpu.ring.len() - 2];
    assert_eq!(
        closed.tau_ms, 10_000.0,
        "a closed bucket weighs its ten seconds"
    );
    let drift: Vec<i64> = (0..400)
        .map(|i| T0 + 900 + i * 1_003 + ((i * 7_919) % 41) - 20)
        .collect();
    let opened: Vec<i64> = drift
        .windows(2)
        .map(|w| SlotGrid::SECONDS.opened(w[0], w[1]))
        .collect();
    assert!(
        opened.contains(&0) && opened.contains(&2),
        "CONTROL: drifting readings stutter on the wall clock's seconds"
    );
}

/// A1 §9 (F4′): a control worker's pull between two of the deck's half-slot readings stands
/// for an instant, so each deck reading keeps its own second in its bucket, in the 10 s mean
/// and in the 1 s band, and an over reading just after the pull keeps its over time; the
/// pull's cadence still serves the gap rule, so the swap rate across it reads. CONTROL: the
/// pull weighed by its own 3 s cadence takes the over reading's second, which keeps 1 ms.
#[test]
fn a_pull_between_deck_readings_leaves_each_its_second_and_its_over_time() {
    let read = |t: i64, pull: bool, kib: u64, cadence_ms: u32| Sample {
        at_unix_ms: T0 + t,
        mono_ms: t as u64,
        cadence_ms,
        pull,
        mem: Some(MemSample {
            total: 32 << 30,
            app: 8 << 30,
            ..MemSample::default()
        }),
        swapped_kib: Some(kib),
        ..Sample::default()
    };
    let run = |pull: bool| {
        let mut v = Vitals::new();
        let mut snap = Snapshot::default();
        for s in [
            read(500, false, 0, 1_000),
            read(1_500, false, 0, 1_000),
            read(1_750, pull, 0, 3_000),
            read(2_500, false, 100_000, 1_000),
            read(3_500, false, 100_000, 1_000),
        ] {
            snap = v.absorb(&s);
        }
        let mem = &snap.resources[Memory.index()];
        let open = *mem.ring.last().expect("the open bucket");
        let over_w = mem
            .recent
            .iter()
            .find(|r| r.at_ms == T0 + 2_500)
            .map(|r| (r.over, r.w_ms));
        (open.tau_ms, open.over_ms, over_w)
    };
    assert_eq!(
        run(true),
        (4_001.0, 1_000.0, Some((true, 1_000))),
        "four deck seconds and the pull's instant; the over reading's whole second"
    );
    assert_eq!(
        run(false),
        (4_001.0, 1.0, Some((true, 1))),
        "CONTROL: a pull weighed by its cadence leaves the over reading 1 ms"
    );
}

/// A snapshot carries each row's last minute of readings, a reading a second in order: the
/// strip's finest boxes read them ([`ResourceView::heat_multi_scale`]).
#[test]
fn a_snapshot_carries_its_last_minute_of_readings_in_order() {
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for t in 0..12 {
        s = v.absorb(&at(t * 1000, 1000));
    }
    let gpu = &s.resources[Gpu.index()];
    assert_eq!(gpu.recent.len(), 12, "a reading a second, all kept");
    assert!(gpu.recent.windows(2).all(|w| w[0].at_ms < w[1].at_ms));
}

#[test]
fn sleep_jumps_and_crash_markers_record_time_gaps_and_strip_events() {
    let mut v = Vitals::new();
    let mut s0 = at(0, 1_000);
    s0.note_events.push(StripEvent {
        at_unix_ms: s0.at_unix_ms - 5_000,
        kind: StripEventKind::Crash,
        label: "previous instance SIGSEGV".into(),
    });
    let _ = v.absorb(&s0);

    // Simulate 30s of wall-clock sleep while mono_ms advances by only 1s.
    let mut s1 = at(30_000, 1_000);
    s1.mono_ms = s0.mono_ms + 1_000;
    s1.mem = Some(MemSample {
        total: 32 << 30,
        app: 30 << 30,
        pressure: Pressure::Critical,
        ..MemSample::default()
    });
    let snap = v.absorb(&s1);
    assert!(
        snap.time_gaps.iter().any(|g| g.kind == GapKind::Sleep
            && g.start_ms == s0.at_unix_ms
            && g.end_ms == s1.at_unix_ms),
        "sleep skew records a Sleep TimeGap: {:?}",
        snap.time_gaps
    );
    assert!(
        snap.strip_events
            .iter()
            .any(|e| e.kind == StripEventKind::Crash),
        "crash event preserved: {:?}",
        snap.strip_events
    );
    assert!(
        snap.strip_events
            .iter()
            .any(|e| e.kind == StripEventKind::SleepWake),
        "wake event emitted: {:?}",
        snap.strip_events
    );
    assert!(
        snap.strip_events
            .iter()
            .any(|e| e.kind == StripEventKind::PressureSpike),
        "critical pressure spike emitted: {:?}",
        snap.strip_events
    );
}

#[test]
fn bar_shares_nest_tab_inside_aterm_inside_global_across_all_five_resources() {
    let mut v = Vitals::new();
    let mut s0 = at(0, 1_000);
    s0.self_pid = 100;
    s0.cpu_brand = Some("Apple M5 Max".into());
    s0.tabs = vec![tab(1, 200, "Tab 1", "cargo")];
    s0.procs = Some(vec![
        ProcSample {
            pid: 100,
            ppid: 1,
            start: Some(1),
            cpu_ns: Some(1_000_000_000),
            footprint: Some(1 << 30),
            disk_read: Some(10_000_000),
            disk_written: Some(5_000_000),
            name: "aterm".into(),
            ..ProcSample::default()
        },
        ProcSample {
            pid: 200,
            ppid: 100,
            start: Some(2),
            cpu_ns: Some(2_000_000_000),
            footprint: Some(3 << 30),
            disk_read: Some(40_000_000),
            disk_written: Some(20_000_000),
            name: "cargo".into(),
            ..ProcSample::default()
        },
    ]);
    s0.links = Some(vec![LinkSample {
        name: "en0".into(),
        kind: LinkKind::Wifi,
        up: true,
        counts: true,
        rx: 1_000_000,
        tx: 500_000,
        rate_bps: Some(800_000_000),
        ..LinkSample::default()
    }]);
    s0.tcp = Some(TcpCounts {
        established: 10,
        listen: 2,
        ..TcpCounts::default()
    });
    s0.tcp_srtt_ms = Some(14.5);
    s0.aterm_sockets = Some(6);
    s0.tab_sockets = vec![(1, 4)];
    let _ = v.absorb(&s0);

    let mut s1 = s0.clone();
    s1.at_ns += 1_000_000_000;
    s1.at_unix_ms += 1_000;
    s1.mono_ms += 1_000;
    for c in s1.cores.as_mut().unwrap() {
        c.busy += 500;
        c.total += 1_000;
    }
    let procs = s1.procs.as_mut().unwrap();
    procs[0].cpu_ns = Some(1_500_000_000);
    procs[0].disk_read = Some(25_000_000);
    procs[0].disk_written = Some(15_000_000);
    procs[1].cpu_ns = Some(4_000_000_000);
    procs[1].disk_read = Some(90_000_000);
    procs[1].disk_written = Some(50_000_000);
    s1.links.as_mut().unwrap()[0].rx += 20_000_000;
    s1.links.as_mut().unwrap()[0].tx += 5_000_000;
    let snap = v.absorb(&s1);

    assert_eq!(snap.facts.cpu_brand.as_deref(), Some("Apple M5 Max"));
    let net = snap.net.as_ref().expect("net view");
    assert_eq!(net.state, NetState::Online);
    assert_eq!(net.srtt_ms, Some(14.5));
    for res in [
        Resource::Cpu,
        Resource::Memory,
        Resource::Disk,
        Resource::Network,
        Resource::Processes,
    ] {
        let [g, a, t] = snap
            .bar_shares(res, &[1])
            .unwrap_or_else(|| panic!("missing bar_shares for {res:?}"));
        assert!(
            g > 0.0 && a > 0.0 && t > 0.0 && t <= a && a <= g && g <= 1.0,
            "{res:?}: expected 0 < t <= a <= g <= 1, got [{g}, {a}, {t}]"
        );
    }
}

#[test]
fn saved_strip_preserves_multi_scale_history_across_restarts_and_crashes() {
    let mut before = Vitals::new();
    let mut prev_busy = 0u64;
    let mut prev_total = 0u64;
    for step in 0..=60u64 {
        let t_ms = step * 10_000;
        let mut s = at(t_ms, 10_000);
        prev_busy += 600;
        prev_total += 1_000;
        for c in s.cores.as_mut().unwrap() {
            c.busy = prev_busy;
            c.total = prev_total;
        }
        s.mem = Some(MemSample {
            total: 64 << 30,
            app: 24 << 30,
            wired: 8 << 30,
            free: 32 << 30,
            pressure: Pressure::Normal,
            ..MemSample::default()
        });
        let _ = before.absorb(&s);
    }
    let last_ms = before.saved_strip(true).last_wall_ms;
    assert_eq!(last_ms, T0 + 600_000);

    // 1. Graceful restart 30 s later preserves pre-restart 10m..1m buckets and marks Paused gap.
    let clean_ckpt = before.saved_strip(true);
    let relaunch_ms = last_ms + 30_000;
    let mut after_clean = Vitals::new();
    after_clean.restore_strip_history(clean_ckpt, relaunch_ms);
    let mut post = rig(1_000, 630_000, 1_000);
    post.mem = Some(MemSample {
        total: 64 << 30,
        app: 20 << 30,
        wired: 8 << 30,
        free: 36 << 30,
        pressure: Pressure::Normal,
        ..MemSample::default()
    });
    let snap_clean = after_clean.absorb(&post);
    let mem_slots = snap_clean.resources[Resource::Memory.index()].heat_multi_scale(
        MultiScaleSpec::FULL_6H,
        snap_clean.at_unix_ms,
        2_500,
        &snap_clean.time_gaps,
    );
    assert!(
        mem_slots
            .iter()
            .any(|hs| hs.slot.age_lo_ms >= 60_000 && hs.cell.heat.is_some_and(|h| h > 0.3)),
        "pre-restart 1m..15m multi-scale boxes must retain memory heat: {mem_slots:?}"
    );
    assert!(
        snap_clean
            .time_gaps
            .iter()
            .any(|g| g.kind == GapKind::Paused && g.start_ms == last_ms),
        "expected Paused downtime gap after clean restart: {:?}",
        snap_clean.time_gaps
    );

    // 2. Abrupt SIGKILL (no crash file) synthesizes Crash gap + Crash/Restart markers while
    //    preserving pre-crash CPU & Memory buckets; a crash-file marker coalesces cleanly.
    let crash_ckpt = before.saved_strip(false);
    let crash_relaunch_ms = last_ms + 40_000;
    let mut after_sigkill = Vitals::new();
    after_sigkill.restore_strip_history(crash_ckpt.clone(), crash_relaunch_ms);
    let snap_sigkill = after_sigkill.absorb(&rig(1_000, 640_000, 1_000));
    assert_eq!(
        snap_sigkill
            .strip_events
            .iter()
            .filter(|e| e.kind == StripEventKind::Crash)
            .count(),
        1,
        "unclean checkpoint alone must synthesize a Crash marker"
    );
    let cpu_slots = snap_sigkill.resources[Resource::Cpu.index()].heat_multi_scale(
        MultiScaleSpec::FULL_6H,
        snap_sigkill.at_unix_ms,
        2_500,
        &snap_sigkill.time_gaps,
    );
    assert!(
        cpu_slots
            .iter()
            .any(|hs| hs.slot.age_lo_ms >= 60_000 && hs.cell.heat.is_some_and(|h| h > 0.4)),
        "pre-crash CPU multi-scale boxes must survive crash recovery: {cpu_slots:?}"
    );

    let mut after_crash_file = Vitals::new();
    after_crash_file.restore_strip_history(crash_ckpt, crash_relaunch_ms + 150);
    let mut first_post = rig(1_000, 640_150, 1_000);
    let exact_crash_ms = last_ms + 4_200;
    first_post.note_gaps.push(TimeGap {
        start_ms: exact_crash_ms,
        end_ms: crash_relaunch_ms,
        kind: GapKind::Crash,
    });
    first_post.note_events.push(StripEvent {
        at_unix_ms: exact_crash_ms,
        kind: StripEventKind::Crash,
        label: "SIGSEGV".into(),
    });
    first_post.note_events.push(StripEvent {
        at_unix_ms: crash_relaunch_ms,
        kind: StripEventKind::Restart,
        label: "restarted".into(),
    });
    let snap_coalesced = after_crash_file.absorb(&first_post);
    let crashes: Vec<&StripEvent> = snap_coalesced
        .strip_events
        .iter()
        .filter(|e| e.kind == StripEventKind::Crash)
        .collect();
    assert_eq!(
        crashes.len(),
        1,
        "checkpoint Crash and crash-file Crash must coalesce: {crashes:?}"
    );
    assert_eq!(crashes[0].at_unix_ms, exact_crash_ms);
    assert_eq!(crashes[0].label, "SIGSEGV");
    assert_eq!(
        snap_coalesced
            .strip_events
            .iter()
            .filter(|e| e.kind == StripEventKind::Restart)
            .count(),
        1,
        "Restart markers must deduplicate: {:?}",
        snap_coalesced.strip_events
    );

    // 3. Delayed post-wake userspace sample + kernel `TimelineSample` wake timestamp coalesce
    //    into one Sleep gap and one SleepWake marker at the kernel's exact `wake_ms`.
    let mut sleep_v = Vitals::new();
    let _ = sleep_v.absorb(&rig(10_000, 1_000_000, 1_000));
    let mut woke = rig(15_000, 1_906_500, 5_000);
    woke.timeline = Some(crate::explain::TimelineSample {
        boot_ms: T0 + 100_000,
        sleep_ms: Some(T0 + 1_000_400),
        wake_ms: Some(T0 + 1_900_000),
        wake_reason: None,
    });
    let snap_woke = sleep_v.absorb(&woke);
    let wakes: Vec<&StripEvent> = snap_woke
        .strip_events
        .iter()
        .filter(|e| e.kind == StripEventKind::SleepWake)
        .collect();
    assert_eq!(
        wakes.len(),
        1,
        "skew detector and kernel timeline wake must coalesce into one SleepWake marker: {wakes:?}"
    );
    assert_eq!(wakes[0].at_unix_ms, T0 + 1_900_000);
    let sleep_gaps: Vec<&TimeGap> = snap_woke
        .time_gaps
        .iter()
        .filter(|g| g.kind == GapKind::Sleep)
        .collect();
    assert_eq!(sleep_gaps.len(), 1, "{sleep_gaps:?}");

    // 4. Transitive bridging gap merge, chronological ordering, and unsampled startup idle
    //    after restore (clean restart at +30s followed by first sample at +90s, plus immediate
    //    dirty checkpoint before first sample preserving prior Paused span):
    let mut bridged = Vitals::new();
    bridged.record_time_gap(TimeGap {
        start_ms: T0 + 40_000,
        end_ms: T0 + 60_000,
        kind: GapKind::Paused,
    });
    bridged.record_time_gap(TimeGap {
        start_ms: T0 + 10_000,
        end_ms: T0 + 30_000,
        kind: GapKind::Crash,
    });
    bridged.record_time_gap(TimeGap {
        start_ms: T0 + 25_000,
        end_ms: T0 + 45_000,
        kind: GapKind::Paused,
    });
    assert_eq!(
        bridged.time_gaps.len(),
        1,
        "bridging span must transitively fold both overlapping gaps: {:?}",
        bridged.time_gaps
    );
    assert_eq!(
        bridged.time_gaps[0],
        TimeGap {
            start_ms: T0 + 10_000,
            end_ms: T0 + 60_000,
            kind: GapKind::Crash,
        }
    );

    let clean_ckpt2 = before.saved_strip(true);
    let mut run2 = Vitals::new();
    run2.restore_strip_history(clean_ckpt2, last_ms + 30_000);
    // Dirty checkpoint before any sample is taken in run2 must advance `last_wall_ms` to
    // `last_ms + 30_000` so an early SIGKILL in run2 never rewrites run1's clean downtime:
    let run2_dirty = run2.saved_strip(false);
    assert_eq!(run2_dirty.last_wall_ms, last_ms + 30_000);
    let mut run3 = Vitals::new();
    run3.restore_strip_history(run2_dirty, last_ms + 50_000);
    assert_eq!(run3.time_gaps.len(), 2, "{:?}", run3.time_gaps);
    assert_eq!(run3.time_gaps[0].kind, GapKind::Paused);
    assert_eq!(run3.time_gaps[1].kind, GapKind::Crash);

    // When run2 stays alive and takes its first sample 60s after restore (`last_ms + 90_000`),
    // the unsampled startup interval merges into the Paused span `[last_ms, last_ms + 90_000)`:
    let snap_delayed = run2.absorb(&rig(60_000, 690_000, 1_000));
    assert_eq!(
        snap_delayed.time_gaps,
        vec![TimeGap {
            start_ms: last_ms,
            end_ms: last_ms + 90_000,
            kind: GapKind::Paused,
        }]
    );

    assert_eq!(
        crate::words::compact_cpu_brand("13th Gen Intel(R) Core(TM) i9-13900K"),
        "Core i9-13900K"
    );
    assert_eq!(
        crate::words::compact_cpu_brand("1st Gen Intel(R) Core(TM) Ultra 9 185H"),
        "Core Ultra 9 185H"
    );
}

#[test]
fn distinct_and_sleep_spanning_crashes_preserve_timestamps_and_deduplicate() {
    use crate::jobs::{Class, Consumer, JobKind, JobRow, JobsView, Tick, Verdict};

    let job_row = |id: &str, exe: &str, verdict: Verdict, exit: Option<i32>| JobRow {
        id: id.into(),
        tab: 1,
        kind: JobKind::Shell,
        orphan_of: None,
        consumer: Consumer::PersonFg,
        promoted_ms: None,
        task: None,
        pgid: 100,
        leader: (100, Some(1)),
        exe: exe.into(),
        members: 0,
        class: Class::Exited,
        wait: None,
        named: None,
        stop: None,
        input: None,
        cpu_mc: None,
        zombies: None,
        standing: None,
        quiet_ms: None,
        phi_milli: None,
        mu_ms: None,
        mu_from: None,
        verdict,
        no_progress: false,
        working_descendant: None,
        episode: 0,
        exit,
        tick: Tick::default(),
    };

    let apply_host_jobs = |v: &mut Vitals, snap: &mut Snapshot, rows: Vec<JobRow>| {
        let prev = snap.strip_events.len();
        snap.set_jobs(Some(Arc::new(JobsView {
            rows,
            ..JobsView::default()
        })));
        let added: Vec<StripEvent> = snap.strip_events[prev..].to_vec();
        for ev in added {
            v.record_strip_event(ev);
        }
    };

    // 1. Two distinct crashed jobs within 15s keep both labels and exact timestamps across
    //    the 60s retention window; non-Exited rows and negative/non-crash exit codes emit none.
    let mut v = Vitals::new();
    let mut s1 = v.absorb(&Sample {
        at_unix_ms: T0 + 100_000,
        mono_ms: 1_000,
        cadence_ms: 1_000,
        ..Sample::default()
    });
    apply_host_jobs(
        &mut v,
        &mut s1,
        vec![
            job_row("j1", "cargo", Verdict::Exited, Some(139)),
            job_row("jlive", "live", Verdict::Fine, Some(139)),
            job_row("jneg", "neg", Verdict::Exited, Some(i32::MIN)),
            job_row("jzero", "zero", Verdict::Exited, Some(128)),
        ],
    );
    let mut s2 = v.absorb(&Sample {
        at_unix_ms: T0 + 103_000,
        mono_ms: 4_000,
        cadence_ms: 1_000,
        ..Sample::default()
    });
    apply_host_jobs(
        &mut v,
        &mut s2,
        vec![
            job_row("j1", "cargo", Verdict::Exited, Some(139)),
            job_row("j2", "node", Verdict::Exited, Some(134)),
        ],
    );
    for step in 5..=60_u64 {
        let mut st = v.absorb(&Sample {
            at_unix_ms: T0 + 99_000 + i64::try_from(step * 1_000).unwrap(),
            mono_ms: step * 1_000,
            cadence_ms: 1_000,
            ..Sample::default()
        });
        apply_host_jobs(
            &mut v,
            &mut st,
            vec![
                job_row("j1", "cargo", Verdict::Exited, Some(139)),
                job_row("j2", "node", Verdict::Exited, Some(134)),
            ],
        );
        assert_eq!(
            st.strip_events,
            vec![
                StripEvent {
                    at_unix_ms: T0 + 100_000,
                    kind: StripEventKind::Crash,
                    label: "cargo (SIGSEGV)".into(),
                },
                StripEvent {
                    at_unix_ms: T0 + 103_000,
                    kind: StripEventKind::Crash,
                    label: "node (SIGABRT)".into(),
                },
            ],
            "tick {step} must neither drop the second crash nor drag the first timestamp"
        );
    }

    // 2. System sleep >60s while an exited crashed job is still <60s old monotonically does
    //    not emit a duplicate marker on wake, until awake elapsed time passes 60s.
    let mut v_sleep = Vitals::new();
    let mut pre_sleep = v_sleep.absorb(&Sample {
        at_unix_ms: T0 + 100_000,
        mono_ms: 10_000,
        cadence_ms: 1_000,
        ..Sample::default()
    });
    apply_host_jobs(
        &mut v_sleep,
        &mut pre_sleep,
        vec![job_row("j2", "node", Verdict::Exited, Some(134))],
    );
    let mut before_sleep = v_sleep.absorb(&Sample {
        at_unix_ms: T0 + 140_000,
        mono_ms: 50_000,
        cadence_ms: 1_000,
        ..Sample::default()
    });
    apply_host_jobs(
        &mut v_sleep,
        &mut before_sleep,
        vec![job_row("j2", "node", Verdict::Exited, Some(134))],
    );
    let mut woke_sample = Sample {
        at_unix_ms: T0 + 745_000,
        mono_ms: 55_000,
        cadence_ms: 1_000,
        ..Sample::default()
    };
    woke_sample.timeline = Some(crate::explain::TimelineSample {
        boot_ms: T0,
        sleep_ms: Some(T0 + 140_000),
        wake_ms: Some(T0 + 740_000),
        wake_reason: None,
    });
    let mut s_woke = v_sleep.absorb(&woke_sample);
    apply_host_jobs(
        &mut v_sleep,
        &mut s_woke,
        vec![job_row("j2", "node", Verdict::Exited, Some(134))],
    );
    let node_crashes: Vec<i64> = s_woke
        .strip_events
        .iter()
        .filter(|e| e.label == "node (SIGABRT)")
        .map(|e| e.at_unix_ms)
        .collect();
    assert_eq!(
        node_crashes,
        vec![T0 + 100_000],
        "40s of awake time across a 10m sleep must not re-emit the retained exited crash"
    );
    let mut s_later = v_sleep.absorb(&Sample {
        at_unix_ms: T0 + 766_000,
        mono_ms: 76_000,
        cadence_ms: 1_000,
        ..Sample::default()
    });
    apply_host_jobs(
        &mut v_sleep,
        &mut s_later,
        vec![job_row("j3", "node", Verdict::Exited, Some(134))],
    );
    let node_crashes_later: Vec<i64> = s_later
        .strip_events
        .iter()
        .filter(|e| e.label == "node (SIGABRT)")
        .map(|e| e.at_unix_ms)
        .collect();
    assert_eq!(
        node_crashes_later,
        vec![T0 + 100_000, T0 + 766_000],
        "a second crash after >60s of awake time must be recorded"
    );

    // 3. Post-relaunch job crash and rapid crash loop never overwrite a prior run's
    //    `"aterm crash"`, and extreme i64 timestamps never overflow.
    let mut v_relaunch = Vitals::new();
    let _ = v_relaunch.absorb(&Sample {
        at_unix_ms: T0 + 100_000,
        mono_ms: 1_000,
        cadence_ms: 1_000,
        ..Sample::default()
    });
    let dirty1 = v_relaunch.saved_strip(false);
    let mut run2 = Vitals::new();
    run2.restore_strip_history(dirty1, T0 + 105_000);
    run2.record_strip_event(StripEvent {
        at_unix_ms: T0 + 105_500,
        kind: StripEventKind::Crash,
        label: "cargo (SIGSEGV)".into(),
    });
    let crash_labels: Vec<(&str, i64)> = run2
        .strip_events
        .iter()
        .filter(|e| e.kind == StripEventKind::Crash)
        .map(|e| (e.label.as_str(), e.at_unix_ms))
        .collect();
    assert_eq!(
        crash_labels,
        vec![
            ("aterm crash", T0 + 100_000),
            ("cargo (SIGSEGV)", T0 + 105_500),
        ],
        "post-relaunch job crash must not overwrite prior run's checkpoint crash"
    );
    let dirty2 = run2.saved_strip(false);
    let mut run3 = Vitals::new();
    run3.restore_strip_history(dirty2, T0 + 112_000);
    let loop_crashes: Vec<(&str, i64)> = run3
        .strip_events
        .iter()
        .filter(|e| e.kind == StripEventKind::Crash && e.label == "aterm crash")
        .map(|e| (e.label.as_str(), e.at_unix_ms))
        .collect();
    assert_eq!(
        loop_crashes,
        vec![("aterm crash", T0 + 100_000), ("aterm crash", T0 + 105_000)],
        "rapid crash loop across disjoint Crash gaps must keep both checkpoint crashes"
    );

    run3.record_strip_event(StripEvent {
        at_unix_ms: i64::MIN,
        kind: StripEventKind::Crash,
        label: "extreme (SIGSEGV)".into(),
    });
    run3.record_strip_event(StripEvent {
        at_unix_ms: i64::MAX,
        kind: StripEventKind::Crash,
        label: "extreme (SIGSEGV)".into(),
    });
}
