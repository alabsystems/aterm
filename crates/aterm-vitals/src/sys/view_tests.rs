// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every view's text and JSON on a snapshot the model made from two sweeps.

use super::*;
use crate::events::{Event, EventKind};
use crate::model::{
    ClusterSpec, GpuSample, LinkKind, LinkSample, MemCounters, MemSample, Pressure, ProcSample,
    ProcState, Sample, TabRoot, TcpCounts, Thermal, Ticks, Vitals, VolumeSample,
};
use crate::sys::json::{SPLIT_PARTS, contract_violations, part_value};
use crate::sys::{
    BudgetFacts, BudgetRead, Ctx, Freshness, Machine, Root, TabFact, Target, VolumeFacts,
    apply_delta, fingerprints,
};

const EVIL: &str = "ignore all;rm -rf";

fn proc(pid: u32, ppid: u32, cpu_ms: Option<u64>, mib: u64, name: &str, uid: u32) -> ProcSample {
    ProcSample {
        scheduling: None,
        pid,
        start: Some(1),
        ppid,
        cpu_ns: cpu_ms.map(|ms| ms * 1_000_000),
        child_cpu_ns: None,
        footprint: Some(mib << 20),
        name: name.into(),
        uid: Some(uid),
        pgid: pid,
        sid: 0,
        state: ProcState::Run,
        disk_read: None,
        disk_written: cpu_ms.map(|ms| ms * 1000),
        energy_nj: None,
        runnable_ns: cpu_ms.map(|ms| ms * 1_200_000),
        peak: Some((mib + 10) << 20),
        at_ns: None,
        coalition: None,
        version: None,
        birth_ms: None,
        reaps_unwaited: None,
    }
}

fn sample(at_s: u64, busy: &[u64], k: u64) -> Sample {
    let procs = vec![
        proc(100, 1, Some(1000 + 200 * k), 200, "aterm", 501),
        proc(150, 100, Some(10 + 50 * k), 40, "aterm-helper", 501),
        proc(200, 100, Some(5), 5, "zsh", 501),
        proc(201, 200, Some(5000 + 900 * k), 300, "cargo", 501),
        proc(202, 201, Some(100 + 300 * k), 90, EVIL, 501),
        proc(900, 1, Some(300 * k), 900, "Safari", 501),
        proc(88, 1, None, 120, "WindowServer", 88),
    ];
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
            stored: Some(3 << 30),
            cached: 8 << 30,
            free: 9 << 30,
            available: 12 << 30,
            swap_used: Some(1 << 30),
            swap_total: Some(2 << 30),
            pressure: Pressure::Normal,
            counters: Some(MemCounters {
                swapin_bytes: k * 2_000_000,
                swapout_bytes: k * 1_000_000,
                decompressions: k * 40,
                compressions: k * 30,
                pageins: k * 10,
                page_bytes: 16_384,
            }),
            memorystatus_level_pct: Some(61),
            commit: None,
            exact: true,
        }),
        gpu: Some(GpuSample {
            device_pct: Some(4),
            cores: Some(10),
            name: Some("Apple M5".into()),
            renderer_pct: Some(3),
            tiler_pct: Some(1),
            in_use_bytes: Some(512 << 20),
            unified: true,
            ..GpuSample::default()
        }),
        volume: Some(VolumeSample {
            total: 1_000_000_000_000,
            free: 380_000_000_000,
            reserved: 20_000_000_000,
            mount: "/System/Volumes/Data".into(),
        }),
        procs: Some(procs),
        self_pid: 100,
        tabs: vec![TabRoot {
            key: 7,
            tab: 7,
            shell_pid: 200,
            label: "Tab 1".into(),
            program: "cargo".into(),
            tab_id: Some(7),
            agent_leader: None,
        }],
        self_uid: Some(501),
        thermal: Some(Thermal {
            level: crate::model::Throttle::None,
            raw: 0,
            low_power: Some(false),
            exact: true,
        }),
        load: Some([2.5, 2.0, 1.5]),
        links: Some(vec![
            LinkSample {
                byte_counter: crate::net_counter::ByteCounter::Cumulative64,
                name: "en0".into(),
                kind: LinkKind::Wifi,
                up: true,
                counts: true,
                rx: 2_000_000 * k,
                tx: 500_000 * k,
                rx_packets: 1_500 * k,
                tx_packets: 400 * k,
                rx_errors: 0,
                tx_errors: 0,
                rx_drops: 2 * k,
                tx_drops: None,
                rate_bps: Some(1_000_000_000),
                ..LinkSample::default()
            },
            LinkSample {
                byte_counter: crate::net_counter::ByteCounter::Cumulative64,
                name: "lo0".into(),
                kind: LinkKind::Other,
                up: true,
                loopback: true,
                counts: false,
                rx: 100_000 * k,
                tx: 100_000 * k,
                rx_packets: 200 * k,
                tx_packets: 200 * k,
                ..LinkSample::default()
            },
        ]),
        tcp: Some(TcpCounts {
            established: 12,
            listen: 4,
            time_wait: 2,
            close_wait: 1,
            syn_sent: 1,
            ..TcpCounts::default()
        }),
        procs_truncated: false,
        at_unix_ms: 1_760_000_000_000 + i64::try_from(at_s).unwrap_or(0) * 1000,
        mono_ms: at_s * 1000,
        cadence_ms: 1000,
        ..Sample::default()
    }
}

fn snapshot() -> Snapshot {
    let mut v = Vitals::new();
    let _ = v.absorb(&sample(10, &[0, 0, 0, 0], 0));
    let mut s = v.absorb(&sample(11, &[20, 30, 100, 90], 1));
    s.set_left(Some(std::sync::Arc::new(
        crate::sys::left::tests::view_fixture(),
    )));
    s
}

#[test]
fn global_monitor_summaries_show_used_available_and_total_capacity() {
    let mut s = snapshot();
    s.volume = Some(VolumeSample {
        total: 8_000_000_000_000,
        free: 240_000_000_000,
        reserved: 160_000_000_000,
        mount: "/".into(),
    });
    let w = world();
    let c = ctx(&w, Some(&s), true, true);
    for view in [View::Overview, View::Global] {
        let text = all_text(&render_text(&query(view, false), &c));
        for figure in [
            "7.76 TB used",
            "240 GB free",
            "8.00 TB total",
            "12.0 GiB available",
            "32 GiB total",
        ] {
            assert!(text.contains(figure), "{view:?} lacks {figure}: {text}");
        }
    }
    let json = global_json(&s);
    // Used is the total less what this user can still write: the 160 GB the volume keeps
    // back from them counts as used, so used and free add up to the total.
    assert_eq!(json.get("disk_used_bytes"), Some(&Jv::u(7_760_000_000_000)));
    assert_eq!(json.get("disk_free_bytes"), Some(&Jv::u(240_000_000_000)));
    assert_eq!(
        json.get("disk_available_bytes"),
        None,
        "free is the one name"
    );
    assert_eq!(
        json.get("disk_total_bytes"),
        Some(&Jv::u(8_000_000_000_000))
    );
    // Available is the reader's own figure, never free plus the file cache (17 GiB here).
    assert_eq!(json.get("mem_available_bytes"), Some(&Jv::u(12 << 30)));
    assert_ne!(json.get("mem_available_bytes"), Some(&Jv::u(17 << 30)));
    assert_eq!(json.get("mem_available_basis"), None);
    assert_eq!(json.get("mem_total_bytes"), Some(&Jv::u(32 << 30)));
    assert_eq!(c2(f32::INFINITY), "0.00c");
    assert_eq!(gpu_pm(f32::INFINITY), 0);
    assert_eq!(gpu_ppm(f32::INFINITY), 0);
    assert_eq!(gpu_share(f32::INFINITY), "0.0%");
    assert_eq!(bps_word(f64::INFINITY), "0 B/s");
    assert_eq!(covered_whole_s(f64::INFINITY), 0);
}

#[test]
fn scheduling_values_errors_and_active_executable_survive_rendering() {
    use crate::scheduling::Scheduling;
    let mut s = snapshot();
    let mut w = world();
    w.machine.executable = Some("/Applications/aterm.app/Contents/MacOS/aterm".into());
    s.procs[0].scheduling = Some(Scheduling {
        nice: Ok(0),
        application: true,
        background_internal: false,
        background_external: true,
        suppressed: false,
        role: Ok(0),
        qos_cpu_ns: Some(Ok([0, 0, 0, 0, 123, 0, 0])),
    });
    let reading = scheduling_json(s.procs[0].scheduling, "macOS");
    assert_eq!(reading.get("nice"), Some(&Jv::Int(0)));
    assert_eq!(reading.get("darwin_application"), Some(&Jv::Bool(true)));
    let background = reading.get("darwin_background_requested").unwrap();
    assert_eq!(background.get("internal"), Some(&Jv::Bool(false)));
    assert_eq!(background.get("external"), Some(&Jv::Bool(true)));
    assert_eq!(reading.get("darwin_suppressed"), Some(&Jv::Bool(false)));
    assert_eq!(reading.get("darwin_role"), Some(&Jv::Int(0)));
    assert_eq!(
        reading.get("qos_lifetime_cpu_ns").unwrap().get("legacy"),
        Some(&Jv::Int(123))
    );
    assert!(contract_violations(&reading).is_empty());
    let failed = scheduling_json(
        Some(Scheduling {
            nice: Err(1),
            application: false,
            background_internal: true,
            background_external: false,
            suppressed: true,
            role: Err(5),
            qos_cpu_ns: Some(Err(13)),
        }),
        "macOS",
    );
    assert_eq!(failed.get("nice").unwrap().get("errno"), Some(&Jv::Int(1)));
    assert_eq!(
        failed.get("darwin_role").unwrap().get("unavailable"),
        Some(&Jv::why("os_error:5"))
    );
    assert_eq!(scheduling_json(None, "Linux"), Jv::why("unsupported"));
    assert_eq!(scheduling_json(None, "macOS"), Jv::why("not_sampled"));
    let mut clock_missing = s.procs[0].scheduling.unwrap();
    clock_missing.qos_cpu_ns = None;
    assert_eq!(
        scheduling_json(Some(clock_missing), "macOS").get("qos_lifetime_cpu_ns"),
        Some(&Jv::why("not_sampled"))
    );
    assert!(contract_violations(&failed).is_empty());
    let c = ctx(&w, Some(&s), true, true);
    let text = all_text(&render_text(&query(View::Procs, false), &c));
    assert!(text.contains("/Applications/aterm.app/Contents/MacOS/aterm"));
    assert!(text.contains("older development copy"));
    let json = render_json(&query(View::Procs, true), &c);
    assert_eq!(
        json.get("scheduling_context")
            .unwrap()
            .get("diagnostic_executable"),
        Some(&Jv::s("/Applications/aterm.app/Contents/MacOS/aterm"))
    );
    assert!(contract_violations(&json).is_empty());
}

#[test]
fn reported_cpu_counter_difference_never_qualifies_physical_wait_with_idle_cores() {
    let mut s = snapshot();
    let w = world();
    s.global.wait = Some(4.0);
    for core in &mut s.cpu.as_mut().unwrap().cores {
        core.load.raw = 0.0;
    }
    let c = ctx(&w, Some(&s), true, true);
    for view in [View::Cores, View::Procs] {
        let json = render_json(&query(view, true), &c);
        assert_eq!(
            json.get("scheduling_context").unwrap().get("finding"),
            Some(&Jv::why("unavailable:execution_window_unknown"))
        );
        assert!(
            !all_text(&render_text(&query(view, false), &c))
                .contains("runnable-wait-with-idle-performance")
        );
        assert!(contract_violations(&json).is_empty());
    }
}

#[test]
fn cpu_accounting_scope_identity_and_unknowns_survive_both_reports() {
    use crate::model::CpuAccounting;
    let mut s = snapshot();
    let w = world();
    let c = CpuAccounting {
        self_ns: Some(0),
        reaped_children_ns: Some(u64::MAX),
        auto_reaps_children: Some(true),
        sampled_at_ns: Some(17),
        start: Some(23),
        exec_version: Some(42),
        published: crate::published_cpu::CpuAccounting::PUBLISHED_COUNTERS,
    };
    s.procs
        .iter_mut()
        .find(|p| p.pid == 201)
        .unwrap()
        .cpu_accounting = c;
    let ctx = ctx(&w, Some(&s), true, true);
    let json = render_json(&query(View::Procs, true), &ctx);
    let text = all_text(&render_text(&query(View::Procs, false), &ctx));
    assert!(text.contains("cpu_mc_scope=process_self"));
    let accounting = cpu_accounting_json(&c, "macOS", None);
    let procs = json.get("procs").unwrap();
    assert_eq!(procs.get("cpu_mc_scope"), Some(&Jv::s("process_self")));
    let Some(Jv::Arr(rows)) = procs.get("rows") else {
        panic!("missing process rows");
    };
    let worker = rows
        .iter()
        .find(|p| p.get("pid") == Some(&Jv::u(201)))
        .unwrap();
    assert_eq!(worker.get("cpu_mc_scope"), Some(&Jv::s("process_self")));
    assert_eq!(worker.get("cpu_accounting"), Some(&accounting));
    assert_eq!(accounting.get("self_ns"), Some(&Jv::s("0")));
    assert_eq!(
        accounting.get("reaped_children_ns"),
        Some(&Jv::s("18446744073709551615"))
    );
    assert_eq!(accounting.get("auto_reaps_children"), Some(&Jv::Bool(true)));
    assert_eq!(accounting.get("counter_at_ns"), Some(&Jv::s("17")));
    assert_eq!(accounting.get("pid_start"), Some(&Jv::s("23")));
    assert_eq!(accounting.get("exec_version"), Some(&Jv::Int(42)));
    let report = crate::sys::hardware::current_report_json(&s);
    let Some(Jv::Arr(workers)) = report.get("workers") else {
        panic!("missing report workers");
    };
    let worker = workers
        .iter()
        .find(|p| p.get("pid") == Some(&Jv::u(201)))
        .unwrap();
    assert_eq!(worker.get("cpu_accounting"), Some(&accounting));
    let unknown = cpu_accounting_json(&CpuAccounting::default(), "macOS", None);
    assert_eq!(unknown.get("self_ns"), Some(&Jv::why("refused")));
    assert_eq!(
        unknown.get("reaped_children_ns"),
        Some(&Jv::why("not_sampled"))
    );
    let unsupported = cpu_accounting_json(&CpuAccounting::default(), "Linux", None);
    assert_eq!(
        unsupported.get("reaped_children_ns"),
        Some(&Jv::why("unsupported"))
    );
    let held = cpu_accounting_json(
        &CpuAccounting::default(),
        "macOS",
        Some(crate::lanes::KPROC_TABLE),
    );
    assert_eq!(
        held.get("auto_reaps_children"),
        Some(&Jv::stalled(crate::lanes::KPROC_TABLE))
    );
    assert_eq!(
        unknown.get("auto_reaps_children"),
        Some(&Jv::why("not_sampled"))
    );
    assert!(contract_violations(&json).is_empty());
}

#[test]
fn scheduling_held_core_is_unavailable_in_core_cluster_and_global_views() {
    let mut model = Vitals::new();
    model.absorb(&sample(10, &[0, 0, 0, 0], 0));
    let previous = sample(11, &[20, 30, 100, 90], 1);
    let measured = model.absorb(&previous);
    assert!(
        measured.why.is_some(),
        "a current split was previously available"
    );
    let p = measured.cpu.as_ref().unwrap().clusters[0].cores[0];
    let mut held = sample(12, &[40, 60, 120, 180], 2);
    held.cores.as_mut().unwrap()[p] = previous.cores.as_ref().unwrap()[p];
    let s = model.absorb(&held);
    let w = world();
    assert!(!s.cpu.as_ref().unwrap().cores[p].measured);
    assert!(
        s.why.is_none(),
        "the unavailable current split retires prior subtraction facts"
    );
    let json = cores_json(&s);
    let Jv::Arr(clusters) = json.get("clusters").unwrap() else {
        panic!("clusters")
    };
    assert_eq!(
        clusters[0].get("busy_mc"),
        Some(&Jv::why("no_counter_delta"))
    );
    assert_eq!(clusters[0].get("exact"), Some(&Jv::why("no_counter_delta")));
    assert_eq!(clusters[1].get("exact"), Some(&Jv::s("exact")));
    let Jv::Arr(cores) = json.get("cores").unwrap() else {
        panic!("cores")
    };
    assert_eq!(cores[p].get("busy_pm"), Some(&Jv::why("no_counter_delta")));
    assert_eq!(
        global_json(&s).get("cpu_mc"),
        Some(&Jv::why("no_counter_delta"))
    );
    assert_eq!(s.total_cores(), Some(4));
    let c = ctx(&w, Some(&s), true, true);
    let overview = all_text(&render_text(&query(View::Overview, false), &c));
    assert!(overview.contains("4 cores total"), "{overview}");
    let text = all_text(&render_text(&query(View::Cores, false), &c));
    assert!(text.contains("exact=unavailable(no_counter_delta)"));
    // This fixture is swapping, so bare `why` correctly prefers memory.
    let mut why_query = query(View::Why, true);
    why_query.subject = Some(crate::explain::WhySubject::Cpu);
    let why = render_json(&why_query, &c);
    let figure = why.get("figure").unwrap();
    assert_eq!(figure.get("cpu_mc"), Some(&Jv::why("no_counter_delta")));
    assert_eq!(figure.get("trend"), Some(&Jv::why("no_counter_delta")));
    assert_eq!(
        why.get("pass").unwrap().get("trend"),
        Some(&Jv::why("no_counter_delta"))
    );
    why_query.json = false;
    let why_text = all_text(&render_text(&why_query, &c));
    assert!(why_text.contains("pass trend=unavailable(no_counter_delta)"));
    assert!(contract_violations(&json).is_empty());
    assert!(contract_violations(&why).is_empty());
    let mut calm = s.clone();
    calm.mem.as_mut().unwrap().rates = Some(Default::default());
    calm.global.wait = Some(0.0);
    let calm_ctx = ctx(&w, Some(&calm), true, true);
    let bare = render_json(&query(View::Why, true), &calm_ctx);
    assert_ne!(bare.get("calm"), Some(&Jv::Bool(true)));
    assert_eq!(bare.get("subject").unwrap().get("res"), Some(&Jv::s("cpu")));
    let recovered = model.absorb(&sample(13, &[50, 70, 130, 190], 3));
    assert!(recovered.cpu.as_ref().unwrap().current_busy().is_some());
    assert!(
        recovered.why.is_some(),
        "fresh counter deltas restore current split facts"
    );
    let recovered_ctx = ctx(&w, Some(&recovered), true, true);
    let mut recovered_query = query(View::Why, true);
    recovered_query.subject = Some(crate::explain::WhySubject::Cpu);
    let recovered_why = render_json(&recovered_query, &recovered_ctx);
    assert!(matches!(
        recovered_why.get("figure").unwrap().get("cpu_mc"),
        Some(Jv::Int(_))
    ));
}

struct World {
    machine: Machine,
    target: Target,
    tabs: Vec<TabFact>,
    volume: VolumeFacts,
    roots: Vec<Root>,
    disk: Option<crate::sys::DiskTotals>,
    stores: Vec<crate::sys::StoreFact>,
    events: Vec<Event>,
}

fn world() -> World {
    World {
        machine: Machine {
            executable: None,
            cpu: Some("Apple M5".into()),
            os: Some("macOS 27.0".into()),
            family: "macOS",
        },
        target: Target {
            key: 7,
            sid: "s-ab12".into(),
            window: Some(1),
            tab: Some(1),
            panes: vec![7],
            window_sessions: vec![7],
            cwd: Some("/Users//a/my proj".into()),
            fg_pgid: Some(201),
            request: None,
        },
        tabs: vec![TabFact {
            key: 7,
            sid: "s-ab12".into(),
            window: Some(1),
            tab: Some(1),
        }],
        volume: VolumeFacts {
            mount: "/System/Volumes/Data".into(),
            fs: "apfs".into(),
            total: 1_000_000_000_000,
            free: 380_000_000_000,
            reserved: 20_000_000_000,
            level: Some("ok".into()),
            budget: BudgetRead::NotSampled,
        },
        roots: vec![Root {
            kinds: vec!["cwd"],
            path: "/Users//a/my proj".into(),
            keys: vec![7],
            tab: Some(1),
            ..Root::default()
        }],
        disk: None,
        stores: Vec::new(),
        events: vec![Event {
            seq: 2,
            at_ns: 11_000_000_000,
            kind: EventKind::Pressure {
                from: Pressure::Normal,
                to: Pressure::Warn,
            },
        }],
    }
}

fn ctx<'a>(w: &'a World, s: Option<&'a Snapshot>, owner: bool, own_tab: bool) -> Ctx<'a> {
    Ctx {
        snap: s,
        epoch: 0x7f3a,
        as_of_unix_ms: 1_792_185_697_412,
        age_ms: 120,
        state: if s.is_some() {
            Freshness::Fresh
        } else {
            Freshness::Warming
        },
        machine: &w.machine,
        target: &w.target,
        tabs: &w.tabs,
        caller_panes: Some(if own_tab { &w.target.panes } else { &[99] }),
        owner_class: owner,
        volume: Some(&w.volume),
        roots: &w.roots,
        disk: w.disk.as_ref(),
        stores: &w.stores,
        home: Some("/Users//a"),
        events: &w.events,
        gap: None,
        head_seq: 0,
        held: None,
    }
}

/// A STALE VIEW SAYS SO IN ITS OWN LINES: its first line names when its figures were read and how
/// long ago, so a reader of the lines alone never takes an old `cores busy … easing` for the
/// present (measured 2026-11-13 on 0.199.0, ATERM-KI-1112-153: a two-hour-old overview read as
/// current, its only mark `state=stale` in the header). CONTROL: a fresh view has no such line.
#[test]
fn a_stale_view_says_when_its_figures_were_read_in_its_first_line() {
    let w = world();
    let s = snapshot();
    let mut stale = ctx(&w, Some(&s), true, true);
    stale.state = Freshness::Stale;
    stale.age_ms = 2 * 3_600_000 + 5 * 60_000;
    let read_at = format!("at {} UTC", clock_ms(stale.as_of_unix_ms));
    for view in [View::Overview, View::Global, View::Tab, View::Aterm] {
        let reply = render_text(&query(view, false), &stale);
        let first = &reply.lines.first().expect("a line").text;
        assert!(
            first.starts_with("state  stale: these figures were read 2 h 5 min ago, ")
                && first.contains(&read_at),
            "{view:?}: {first}"
        );
    }
    let fresh = ctx(&w, Some(&s), true, true);
    for view in [View::Overview, View::Global, View::Tab, View::Aterm] {
        let text = all_text(&render_text(&query(view, false), &fresh));
        assert!(!text.contains("state  stale"), "CONTROL {view:?}: {text}");
    }
}

fn query(view: View, json: bool) -> SysQuery {
    let mut q = SysQuery::parse(view.word()).unwrap();
    q.json = json;
    q
}

fn all_text(r: &Reply) -> String {
    r.lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn every_view_answers_in_both_shapes_and_the_json_keeps_the_contract() {
    let s = snapshot();
    let w = world();
    for owner in [true, false] {
        for own in [true, false] {
            let c = ctx(&w, Some(&s), owner, own);
            for view in View::ALL {
                let reply = render_text(&query(view, false), &c);
                assert!(!reply.lines.is_empty(), "{view:?}");
                for l in &reply.lines {
                    assert!(!l.text.contains('\n'), "{view:?}: {}", l.text);
                    assert!(
                        !l.text.contains("NaN") && !l.text.contains("inf"),
                        "{}",
                        l.text
                    );
                    assert!(!l.text.trim().is_empty(), "{view:?}");
                }
                let j = render_json(&query(view, true), &c);
                let v = contract_violations(&j);
                assert!(v.is_empty(), "{view:?}: {v:?}\n{}", j.to_text());
                let schema = if view == View::Disk { 2 } else { 1 };
                assert_eq!(j.get("schema"), Some(&Jv::Int(schema)), "{view:?}");
                assert_eq!(j.get("kind"), Some(&Jv::s(format!("sys.{}", view.word()))));
                assert!(j.get("as_of").is_some());
                assert!(matches!(j.get("events"), Some(Jv::Arr(e)) if e.len() == 1));
                let header = reply.header(&c, false);
                assert!(
                    header.starts_with(&format!(
                        "OK {} sys v=1 view={}",
                        reply.lines.len(),
                        view.word()
                    )),
                    "{header}"
                );
                assert!(
                    header.contains(" as_of=21:21:37.412 age_ms=120 window_ms=1000 next=7f3a.")
                );
            }
        }
    }
}

#[test]
fn memory_off_macos_says_its_remainder_unread_classes_and_commit() {
    let mut s = snapshot();
    let w = world();
    let mac = all_text(&render_text(
        &query(View::Mem, false),
        &ctx(&w, Some(&s), true, true),
    ));
    assert!(mac.contains("classes add up to physical memory"), "{mac}");
    assert!(mac.contains(" · not in VM "), "{mac}");
    assert!(!mac.contains("commit"), "{mac}");

    let m = s.mem.as_mut().expect("memory");
    m.sample.compressed = None;
    m.sample.commit = Some(crate::model::CommitSample {
        charge: 29 << 30,
        limit: 32 << 30,
        exact: true,
    });
    let mut win = world();
    win.machine.family = "Windows";
    let c = ctx(&win, Some(&s), true, true);
    let text = all_text(&render_text(&query(View::Mem, false), &c));
    assert!(
        text.contains("the classes leave memory no class names"),
        "{text}"
    );
    assert!(
        text.contains("+ compressed unavailable(not_sampled) ·"),
        "{text}"
    );
    assert!(text.contains(" · no class "), "{text}");
    assert!(
        text.contains(
            "commit  29.00 of 32.00 GiB (90%) · enforced: an allocation fails at the limit"
        ),
        "{text}"
    );
    let global = all_text(&render_text(&query(View::Global, false), &c));
    assert!(
        global.contains("commit   29.00 of 32.00 GiB (90%)"),
        "{global}"
    );
    let j = render_json(&query(View::Mem, true), &c);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let jt = j.to_text();
    assert!(jt.contains(r#""partition":"remainder""#), "{jt}");
    assert!(
        jt.contains(r#"{"class":"compressed","bytes":{"why":"not_sampled"}}"#),
        "{jt}"
    );
    assert!(jt.contains(r#""class":"unnamed""#), "{jt}");
    assert!(
        jt.contains(&format!(
            r#""commit":{{"charge_bytes":{},"limit_bytes":{},"enforced":true}}"#,
            29u64 << 30,
            32u64 << 30
        )),
        "{jt}"
    );

    for (counters, why) in [
        (None, "not_sampled"),
        (Some(MemCounters::default()), "warming"),
    ] {
        let mut pending = s.clone();
        let m = pending.mem.as_mut().unwrap();
        m.sample.counters = counters;
        m.rates = None;
        let c = ctx(&win, Some(&pending), true, true);
        let text = render_text(&query(View::Mem, false), &c);
        assert!(text.lines.iter().any(|l| {
            l.key == "paging" && l.text.contains(&format!("rates unavailable({why})"))
        }));
        assert!(text.lines.iter().any(|l| {
            l.key == "mem" && l.text.contains(&format!("thrashing: unavailable({why})"))
        }));
        let j = render_json(&query(View::Mem, true), &c);
        assert_eq!(j.get("per_s"), Some(&Jv::why(why)));
        assert_eq!(j.get("thrashing"), Some(&Jv::why(why)));
        assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    }
    let measured = render_json(&query(View::Mem, true), &ctx(&win, Some(&s), true, true));
    assert_eq!(
        measured.get("per_s").and_then(|p| p.get("decompressions")),
        Some(&Jv::u(40))
    );
    assert_eq!(measured.get("thrashing"), Some(&Jv::Bool(false)));
    let mut unknown = s.clone();
    unknown.mem.as_mut().unwrap().sample.pressure = Pressure::Unknown;
    let c = ctx(&win, Some(&unknown), true, true);
    let j = render_json(&query(View::Mem, true), &c);
    assert_eq!(j.get("per_s"), measured.get("per_s"));
    assert_eq!(j.get("thrashing"), Some(&Jv::why("unsupported")));
    assert!(
        render_text(&query(View::Mem, false), &c)
            .lines
            .iter()
            .any(|l| { l.key == "mem" && l.text.contains("thrashing: unavailable(unsupported)") })
    );
}

#[test]
fn reported_cpu_accounts_keep_unknown_owners_without_machine_closure() {
    let s = snapshot();
    let w = world();
    let c = ctx(&w, Some(&s), true, true);
    let j = render_json(&query(View::Overview, true), &c);
    let reported = j
        .get("reported_cpu_mc")
        .expect("published counter accounts");
    let busy = match j.get("global").and_then(|g| g.get("cpu_mc")) {
        Some(Jv::Int(n)) => *n,
        other => panic!("{other:?}"),
    };
    assert_eq!(busy, 2400);
    assert_eq!(reported.get("tab"), Some(&Jv::Int(1200)));
    assert_eq!(reported.get("aterm"), Some(&Jv::Int(1450)));
    assert_eq!(reported.get("others"), Some(&Jv::Int(300)));
    assert_eq!(
        reported.get("cpu_basis"),
        Some(&Jv::s("published_counters"))
    );
    assert_eq!(
        reported.get("cpu_execution_window"),
        Some(&Jv::why("unavailable:execution_window_unknown"))
    );
    assert!(reported.get("unaccounted").is_none());
    assert!(j.get("split_mc").is_none());
    let counters: i64 = ["aterm", "others", "services"]
        .into_iter()
        .map(|key| reported.get(key).and_then(part_value).unwrap())
        .sum();
    assert_ne!(
        counters, busy,
        "independent published counters need no machine closure"
    );

    for self_uid in [None, Some(501)] {
        let input = |at, busy: &[u64], k| {
            let mut s = sample(at, busy, k);
            s.self_uid = self_uid;
            for p in s.procs.as_mut().unwrap() {
                if self_uid.is_none() || p.pid == 900 {
                    p.uid = None;
                }
            }
            s
        };
        let mut model = Vitals::new();
        let warming = model.absorb(&input(10, &[0, 0, 0, 0], 0));
        assert_eq!(
            render_json(
                &query(View::Overview, true),
                &ctx(&w, Some(&warming), true, true)
            )
            .get("reported_cpu_mc"),
            Some(&Jv::why("warming"))
        );
        let unknown = model.absorb(&input(11, &[20, 30, 100, 90], 1));
        assert!(
            unknown.split.is_some(),
            "unknown ownership must not erase known CPU"
        );
        let row = |pid| unknown.procs.iter().find(|p| p.pid == pid).unwrap();
        assert_eq!(row(900).scope, RowScope::Unknown);
        assert_eq!(row(900).cores, Some(0.3));
        assert_eq!(row(900).footprint, Some(900 << 20));
        assert!((unknown.split.unwrap().unknown_owner_reported - 0.3).abs() < 1e-6);
        assert_eq!(row(100).scope, RowScope::SelfProc);
        assert_eq!(row(150).scope, RowScope::Helper);
        assert_eq!(row(201).scope, RowScope::Tab(7));
        assert_eq!(
            unknown.aterm, s.aterm,
            "ownership does not change tree totals"
        );
        let c = ctx(&w, Some(&unknown), true, true);
        for view in [View::Overview, View::Global] {
            let j = render_json(&query(view, true), &c);
            let reported = j.get("reported_cpu_mc").unwrap();
            assert_eq!(reported.get("tab"), Some(&Jv::Int(1200)));
            assert_eq!(reported.get("process_unknown_owner"), Some(&Jv::Int(300)));
            assert!(reported.get("unaccounted").is_none());
            assert_eq!(
                reported.get("cpu_execution_window"),
                Some(&Jv::why("unavailable:execution_window_unknown"))
            );
            assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
            let text = render_text(&query(view, false), &c);
            assert!(text.lines.iter().any(|l| {
                l.key == "reported_cpu"
                    && l.text
                        .contains("execution_window=unavailable:execution_window_unknown")
                    && !l.text.contains('≥')
                    && !l.text.contains('≤')
            }));
        }
        let global = render_json(&query(View::Global, true), &c);
        assert_eq!(global.get("aterm_mc").and_then(part_value), Some(1450));
        let procs = render_json(
            &SysQuery {
                scope: Some(ScopeArg::Global),
                ..query(View::Procs, true)
            },
            &c,
        );
        let rows = match procs.get("procs").and_then(|p| p.get("rows")) {
            Some(Jv::Arr(rows)) => rows,
            other => panic!("{other:?}"),
        };
        let outside = rows
            .iter()
            .find(|r| r.get("pid") == Some(&Jv::u(900)))
            .unwrap();
        assert_eq!(outside.get("scope"), Some(&Jv::s("unknown")));
        assert_eq!(outside.get("cpu_mc"), Some(&Jv::u(300)));
        let mem = render_json(&query(View::Mem, true), &c);
        let expected_unknown = if self_uid.is_none() {
            1020 << 20
        } else {
            900 << 20
        };
        assert_eq!(
            mem.get("owners")
                .and_then(|o| o.get("unknown"))
                .and_then(|o| o.get("footprint_bytes")),
            Some(&Jv::u(expected_unknown))
        );
        let scoped = render_json(
            &query(View::Global, true),
            &ctx(&w, Some(&unknown), false, true),
        );
        assert_eq!(scoped.get("top"), Some(&Jv::why("needs_privilege:owner")));
    }

    let mut model = Vitals::new();
    let mut known = Snapshot::default();
    for (at, k) in [(10, 0), (11, 1)] {
        let mut input = sample(at, &[0, 0, 0, 0], k);
        input.cores = None;
        known = model.absorb(&input);
    }
    assert!(known.cpu.is_none() && known.split.is_none());
    assert!(known.procs_window_ns.is_some());
    assert!(
        known
            .procs
            .iter()
            .any(|p| p.pid == 900 && p.scope == RowScope::Others)
    );
    let mem = render_text(&query(View::Mem, false), &ctx(&w, Some(&known), true, true));
    let owners = mem.lines.iter().find(|l| l.key == "charges").unwrap();
    assert!(
        owners
            .text
            .contains(&format!("others {}", words::mem(900 << 20))),
        "{}",
        owners.text
    );
}

#[test]
fn memory_coverage_is_independent_of_cpu_and_survives_the_scope_mean() {
    let w = world();
    let mut model = Vitals::new();
    let input = |at, unread| {
        let mut s = sample(at, &[0, 0, at * 10, at * 10], at);
        let members = s.procs.as_mut().unwrap();
        members.iter_mut().find(|p| p.pid == 201).unwrap().footprint =
            if unread { None } else { Some(300 << 20) };
        members.iter_mut().find(|p| p.pid == 202).unwrap().cpu_ns = None;
        s.at_unix_ms = (at * 1000) as i64;
        s
    };
    model.absorb(&input(10, true));
    let s = model.absorb(&input(11, true));
    let tab = s.tab(7).unwrap();
    assert_eq!(tab.scope.refused, 1);
    assert_eq!(tab.scope.footprint_unread, 1);
    assert_eq!(tab.scope.footprint, Some(95 << 20));
    assert_eq!(
        s.resource(crate::model::Resource::Memory, &[7]).tab.unread,
        1
    );
    let json = render_json(&query(View::Mem, true), &ctx(&w, Some(&s), true, true));
    assert_eq!(
        json.get("tab").and_then(|o| o.get("footprint_bytes")),
        Some(&Jv::at_least_refused(95 << 20, 1))
    );
    let complete = model.absorb(&input(12, false));
    assert_eq!(complete.tab(7).unwrap().scope.footprint_unread, 0);
    assert_eq!(
        complete
            .resource(crate::model::Resource::Memory, &[7])
            .tab
            .unread,
        1,
        "an unread contributor remains in the trailing mean"
    );
    let complete = model.absorb(&input(30, false));
    assert_eq!(
        complete
            .resource(crate::model::Resource::Memory, &[7])
            .tab
            .unread,
        0
    );
}

#[test]
fn memory_ownership_totals_include_processes_past_the_display_row_cap() {
    let mut model = Vitals::new();
    let input = |at| {
        let mut s = sample(at, &[0, 0, 0, 0], at);
        for pid in 1000..1300 {
            s.procs
                .as_mut()
                .unwrap()
                .push(proc(pid, 1, Some(0), 1, "idle", 501));
        }
        s
    };
    model.absorb(&input(10));
    let s = model.absorb(&input(11));
    assert!(s.procs.len() < 300);
    let all = s.memory_owners.unwrap();
    assert_eq!(all.others.bytes, 1200 << 20);
    assert_eq!(all.others.members, 301);
    assert_eq!(all.others.unread, 0);
    let w = world();
    let json = render_json(&query(View::Mem, true), &ctx(&w, Some(&s), true, true));
    let owners = json.get("owners").unwrap();
    assert_eq!(owners.get("via"), Some(&Jv::s("processes")));
    assert_eq!(
        owners.get("others").and_then(|o| o.get("footprint_bytes")),
        Some(&Jv::u(1200 << 20))
    );
}

#[test]
fn memory_ownership_uses_complete_coalition_charges_with_their_own_age() {
    use crate::explain::{CoalLane, CoalRow, CoalSplit, Part, WhyFacts};
    let mut s = snapshot();
    s.why = Some(std::sync::Arc::new(WhyFacts {
        at_ns: s.at_ns - 2_000_000_000,
        lane: CoalLane::On,
        split: CoalSplit {
            listed: 2,
            rows: vec![
                CoalRow {
                    part: Part::Aterm,
                    footprint_aterm: true,
                    footprint: Some(123),
                    ..CoalRow::default()
                },
                CoalRow {
                    part: Part::Services,
                    footprint_outside: Some(Part::Services),
                    footprint: Some(456),
                    live: 2,
                    ..CoalRow::default()
                },
            ],
            ..CoalSplit::default()
        },
        ..WhyFacts::default()
    }));
    let read = MemoryReading::of(&s).unwrap();
    assert_eq!(read.via, "coalitions");
    assert_eq!(read.age_ms, 2000);
    assert_eq!(read.owners.aterm.bytes, 123);
    assert_eq!(read.owners.services.bytes, 456);
    assert!(!read.owners.truncated);
    {
        let row = &mut std::sync::Arc::make_mut(s.why.as_mut().unwrap()).split.rows[1];
        row.owned_footprint = crate::model::MemoryCharge {
            bytes: 56,
            members: 1,
            unread: 0,
        };
    }
    let moved = MemoryReading::of(&s).unwrap();
    assert_eq!(moved.owners.aterm.bytes, 179);
    assert_eq!(moved.owners.services.bytes, 400);
    assert_eq!(moved.owners.unknown.bytes, 0);
    {
        let row = &mut std::sync::Arc::make_mut(s.why.as_mut().unwrap()).split.rows[1];
        row.owned_footprint.unread = 1;
    }
    let mixed = MemoryReading::of(&s).unwrap();
    assert_eq!(mixed.owners.aterm.bytes, 123);
    assert_eq!(mixed.owners.services.bytes, 0);
    assert_eq!(
        mixed.owners.unknown.bytes, 456,
        "an unread member leaves the complete charge explicitly mixed rather than inventing an owner"
    );
    std::sync::Arc::make_mut(s.why.as_mut().unwrap()).split.rows[1].owned_footprint =
        crate::model::MemoryCharge::default();
    std::sync::Arc::make_mut(s.why.as_mut().unwrap())
        .split
        .listed = 3;
    let read = MemoryReading::of(&s).unwrap();
    assert!(read.owners.truncated);
    assert_eq!(
        read.json()
            .get("services")
            .and_then(|o| o.get("footprint_bytes")),
        Some(&Jv::at_least(456, "truncated"))
    );
    s.memory_owners.as_mut().unwrap().ownership_incomplete = true;
    std::sync::Arc::make_mut(s.why.as_mut().unwrap())
        .split
        .listed = 2;
    let read = MemoryReading::of(&s).unwrap();
    assert!(!read.owners.truncated);
    let json = read.json();
    assert_eq!(json.get("ownership_incomplete"), Some(&Jv::Bool(true)));
    assert_eq!(
        json.get("services").and_then(|o| o.get("footprint_bytes")),
        Some(&Jv::why("ownership_incomplete"))
    );
    assert_eq!(
        json.get("services")
            .and_then(|o| o.get("observed_footprint_bytes")),
        Some(&Jv::u(456)),
        "the kernel charge stays known while its owner is unresolved"
    );
}

#[test]
fn native_wait_totals_remain_known_when_the_final_cpu_split_is_missing() {
    use crate::model::{NativeCpuCut, NativeCpuWindow};
    let mut s = snapshot();
    s.native_terminals.push(crate::model::NativeTerminal {
        timing: NativeCpuWindow {
            previous: NativeCpuCut {
                continuous_ns: Some(10),
                boot: None,
            },
            current: NativeCpuCut {
                continuous_ns: Some(20),
                boot: None,
            },
            exited_ns: Some(15),
        },
        host: crate::delegated::ProcessKey { pid: 100, start: 1 },
        seq: 1,
        process: ProcSample {
            pid: 9000,
            start: Some(999),
            ppid: 100,
            state: ProcState::Zombie,
            ..ProcSample::default()
        },
        wait_cpu_ns: Some(12345),
        tab: Some(7),
    });
    let json = native_waits_json(&s);
    assert_eq!(json.get("scope"), Some(&Jv::s("owning_wait_total")));
    assert_eq!(json.get("wait_cpu_ns"), Some(&Jv::u(12345)));
    let rows = match json.get("rows").unwrap() {
        Jv::Arr(rows) => rows,
        _ => unreachable!(),
    };
    assert_eq!(rows[0].get("own_cpu_ns"), Some(&Jv::why("not_sampled")));
    assert_eq!(rows[0].get("child_cpu_ns"), Some(&Jv::why("not_sampled")));
    let timing = rows[0].get("timing").unwrap();
    assert_eq!(timing.get("clock"), Some(&Jv::s("continuous_boot_ns")));
    assert_eq!(timing.get("exited_ns"), Some(&Jv::u(15)));
    assert_eq!(timing.get("previous_cut_ns"), Some(&Jv::u(10)));
    assert_eq!(timing.get("current_cut_ns"), Some(&Jv::u(20)));
    s.native_terminals[0].timing.exited_ns = None;
    let json = native_waits_json(&s);
    let Jv::Arr(rows) = json.get("rows").unwrap() else {
        unreachable!()
    };
    assert_eq!(
        rows[0].get("timing").unwrap().get("exited_ns"),
        Some(&Jv::why("not_sampled"))
    );
}

#[test]
fn incomplete_mixed_coalition_memory_stays_unknown_and_never_becomes_foreign_memory() {
    use crate::explain::{CoalLane, CoalRow, CoalSplit, Part, WhyFacts};
    let mut s = snapshot();
    let mixed = CoalRow {
        part: Part::Others,
        footprint_outside: Some(Part::Others),
        footprint: Some(100),
        ownership_mixed: true,
        owned_footprint: crate::model::MemoryCharge {
            bytes: 30,
            members: 1,
            unread: 0,
        },
        live: 2,
        ..CoalRow::default()
    };
    s.why = Some(std::sync::Arc::new(WhyFacts {
        at_ns: s.at_ns,
        lane: CoalLane::On,
        split: CoalSplit {
            listed: 1,
            rows: vec![mixed],
            ..CoalSplit::default()
        },
        ..WhyFacts::default()
    }));
    let read = MemoryReading::of(&s).unwrap();
    assert_eq!(
        (
            read.owners.aterm.bytes,
            read.owners.others.bytes,
            read.owners.unknown.bytes
        ),
        (30, 70, 0)
    );
    {
        let row = &mut std::sync::Arc::make_mut(s.why.as_mut().unwrap()).split.rows[0];
        row.footprint_ownership_incomplete = true;
    }
    let incomplete = MemoryReading::of(&s).unwrap();
    assert_eq!(
        (
            incomplete.owners.aterm.bytes,
            incomplete.owners.others.bytes,
            incomplete.owners.unknown.bytes
        ),
        (0, 0, 100)
    );
    assert!(
        incomplete.owners.ownership_incomplete,
        "known unassigned memory must not produce an exact zero for the app"
    );
    {
        let row = &mut std::sync::Arc::make_mut(s.why.as_mut().unwrap()).split.rows[0];
        row.footprint_ownership_incomplete = false;
        row.owned_footprint = crate::model::MemoryCharge::default();
        row.footprint_outside = None;
    }
    let missing = MemoryReading::of(&s).unwrap();
    assert_eq!(
        missing.owners.unknown.bytes, 100,
        "a mixed coalition with no currently proved app subset must not become entirely foreign"
    );
    {
        let row = &mut std::sync::Arc::make_mut(s.why.as_mut().unwrap()).split.rows[0];
        row.owned_footprint = crate::model::MemoryCharge {
            bytes: 30,
            members: 1,
            unread: 0,
        };
        row.live = 1;
    }
    let only_owned = MemoryReading::of(&s).unwrap();
    assert_eq!(
        (
            only_owned.owners.aterm.bytes,
            only_owned.owners.others.bytes,
            only_owned.owners.unknown.bytes
        ),
        (30, 0, 70),
        "with no current foreign member, an unexplained remainder is not proof of foreign memory"
    );
    std::sync::Arc::make_mut(s.why.as_mut().unwrap()).memory_provenance_incomplete = true;
    assert!(
        !s.memory_owners.as_ref().unwrap().ownership_incomplete,
        "the newer process reading repaired its identities"
    );
    assert!(
        MemoryReading::of(&s).unwrap().owners.ownership_incomplete,
        "that repair cannot upgrade the provenance of carried coalition charges"
    );
}

#[test]
fn overlapping_or_unread_vm_classes_do_not_claim_an_exact_partition() {
    let w = world();
    let mut s = snapshot();
    s.mem.as_mut().unwrap().sample.app = 64 << 30;
    let c = ctx(&w, Some(&s), true, true);
    assert!(!exact_partition(&c));
    let json = render_json(&query(View::Mem, true), &c);
    assert_eq!(json.get("partition"), Some(&Jv::s("remainder")));
    assert!(matches!(json.get("class_overflow_bytes"), Some(Jv::Int(n)) if *n > 0));
    s.mem.as_mut().unwrap().sample.compressed = None;
    assert!(!exact_partition(&ctx(&w, Some(&s), true, true)));
}

#[test]
fn physical_memory_sys_keeps_the_shared_classes_and_platform_proof() {
    let mut s = snapshot();
    s.mem.as_mut().unwrap().sample = MemSample {
        total: 32 << 30,
        app: 12 << 30,
        wired: 4 << 30,
        compressed: Some(2 << 30),
        cached: 8 << 30,
        free: 4 << 30,
        exact: true,
        ..MemSample::default()
    };
    for (family, platform) in [
        ("macOS", Some(words::Platform::MacOs)),
        ("Linux", Some(words::Platform::Linux)),
        ("Windows", Some(words::Platform::Windows)),
        ("unknown", None),
    ] {
        let mut w = world();
        w.machine.family = family;
        let c = ctx(&w, Some(&s), true, true);
        let exact = exact_partition(&c);
        assert_eq!(exact, family == "macOS");
        let m = s.mem.as_ref().unwrap();
        let projected = m.sample.physical_classes(platform);
        let classes = mem_classes(m, exact);
        assert_eq!(
            classes.map(|(_, bytes)| bytes),
            projected.classes.map(|c| c.bytes)
        );
        assert_eq!(classes[5].1, Some(2 << 30));
        assert_eq!(classes[5].0, if exact { "not_in_vm" } else { "unnamed" });
        let json = render_json(&query(View::Mem, true), &c);
        assert_eq!(
            json.get("partition"),
            Some(&Jv::s(if exact { "exact" } else { "remainder" }))
        );
        assert_eq!(json.get("class_overflow_bytes"), Some(&Jv::Int(0)));
        assert_eq!(
            json.get("classes"),
            Some(&classes_json(m, exact, class_why(&c)))
        );
    }
}

#[test]
fn physical_memory_sys_preserves_unknown_compressed_bytes() {
    let mut s = snapshot();
    s.mem.as_mut().unwrap().sample.compressed = None;
    let mut w = world();
    w.machine.family = "Linux";
    let c = ctx(&w, Some(&s), true, true);
    let json = render_json(&query(View::Mem, true), &c);
    let Some(Jv::Arr(classes)) = json.get("classes") else {
        panic!("the physical classes must remain available");
    };
    assert_eq!(classes.len(), 6);
    assert_eq!(classes[2].get("bytes"), Some(&Jv::why("not_enabled:zswap")));
    assert_eq!(classes[5].get("class"), Some(&Jv::s("unnamed")));
    assert_eq!(classes[5].get("bytes"), Some(&Jv::u(2 << 30)));
    assert_eq!(json.get("partition"), Some(&Jv::s("remainder")));
}

#[test]
fn physical_memory_sys_detects_zero_capacity_and_unsaturated_excess() {
    let mut s = snapshot();
    let w = world();
    s.mem.as_mut().unwrap().sample = MemSample {
        total: u64::MAX,
        app: u64::MAX,
        wired: 1,
        compressed: Some(0),
        exact: true,
        ..MemSample::default()
    };
    let c = ctx(&w, Some(&s), true, true);
    assert!(!exact_partition(&c));
    let json = render_json(&query(View::Mem, true), &c);
    assert_eq!(json.get("class_overflow_bytes"), Some(&Jv::Int(1)));

    let m = &mut s.mem.as_mut().unwrap().sample;
    m.wired = u64::MAX;
    m.compressed = Some(u64::MAX);
    m.cached = u64::MAX;
    m.free = u64::MAX;
    let json = render_json(&query(View::Mem, true), &ctx(&w, Some(&s), true, true));
    assert_eq!(
        json.get("class_overflow_bytes"),
        Some(&Jv::at_least(i64::MAX.unsigned_abs(), "integer_range"))
    );
    assert!(contract_violations(&json).is_empty());

    s.mem.as_mut().unwrap().sample = MemSample {
        compressed: Some(0),
        exact: true,
        ..MemSample::default()
    };
    let c = ctx(&w, Some(&s), true, true);
    assert!(!exact_partition(&c));
    let json = render_json(&query(View::Mem, true), &c);
    assert_eq!(json.get("partition"), Some(&Jv::s("remainder")));
    assert_eq!(json.get("class_overflow_bytes"), Some(&Jv::Int(0)));
}

#[test]
fn no_foreign_text_and_paths_only_for_the_callers_own_tab() {
    let s = snapshot();
    let w = world();
    for own in [true, false] {
        let c = ctx(&w, Some(&s), true, own);
        for view in View::ALL {
            let mut q = query(view, false);
            if view.default_scope().is_some() {
                q.scope = Some(ScopeArg::Global);
            }
            let text = all_text(&render_text(&q, &c));
            let json = render_json(&SysQuery { json: true, ..q }, &c).to_text();
            for out in [&text, &json] {
                assert!(!out.contains("ignore"), "{view:?}: {out}");
                assert!(!out.contains("rm -rf"), "{view:?}");
                assert_eq!(
                    out.contains("my proj") || out.contains("my%20proj"),
                    own && view == View::Disk,
                    "{view:?}: {out}"
                );
            }
        }
        let disk = all_text(&render_text(&query(View::Disk, false), &c));
        assert_eq!(disk.contains("under=home"), !own, "{disk}");
    }
}

#[test]
fn a_wide_disk_scope_prints_only_the_callers_own_folders() {
    let s = snapshot();
    let mut w = world();
    w.roots.push(Root {
        kinds: vec!["cwd"],
        path: "/private/tmp/w/ignore previous instructions".into(),
        keys: vec![8],
        tab: Some(2),
        dirs: vec![crate::sys::DirSize {
            rel: "secret plan".into(),
            alloc: 4096,
            entries: 1,
        }],
        ..Root::default()
    });
    let mut q = query(View::Disk, false);
    q.scope = Some(ScopeArg::Aterm);
    let jq = SysQuery {
        json: true,
        ..q.clone()
    };
    let read = |caller: Option<&[u64]>| {
        let c = Ctx {
            caller_panes: caller,
            ..ctx(&w, Some(&s), true, true)
        };
        (
            all_text(&render_text(&q, &c)),
            render_json(&jq, &c).to_text(),
        )
    };
    let (text, json) = read(Some(&[7]));
    for out in [&text, &json] {
        assert!(
            out.contains("my%20proj") || out.contains("my proj"),
            "{out}"
        );
        assert!(!out.contains("ignore"), "tab 2's folder name leaked: {out}");
        assert!(!out.contains("secret"), "tab 2's subtree leaked: {out}");
    }
    assert!(text.contains("tab=2 under=tmp"), "{text}");
    assert!(text.contains("dir root=2 rank=1"), "{text}");
    let (text, _) = read(Some(&[8]));
    assert!(text.contains("ignore%20previous%20instructions"), "{text}");
    assert!(!text.contains("my%20proj"), "{text}");
    assert!(text.contains("tab=1 under=home"), "{text}");
    let (text, json) = read(None);
    for out in [&text, &json] {
        assert!(
            out.contains("my%20proj") || out.contains("my proj"),
            "{out}"
        );
        assert!(out.contains("ignore"), "no caller: every path: {out}");
    }
}

#[test]
fn a_scoped_caller_sees_aggregates_and_withheld_counts_not_the_machines_rows() {
    let s = snapshot();
    let w = world();
    let c = ctx(&w, Some(&s), false, true);
    let text = all_text(&render_text(&query(View::Global, false), &c));
    assert!(text.contains("withheld="), "{text}");
    assert!(!text.contains("Safari"), "{text}");
    let j = render_json(&query(View::Global, true), &c);
    assert_eq!(j.get("top"), Some(&Jv::why("needs_privilege:owner")));
    let mem = all_text(&render_text(&query(View::Mem, false), &c));
    assert!(
        !mem.contains("Safari") && mem.contains("withheld="),
        "{mem}"
    );
    assert!(mem.contains("exe=cargo"), "the tab's own rows print: {mem}");
    let owner = ctx(&w, Some(&s), true, true);
    let text = all_text(&render_text(&query(View::Global, false), &owner));
    assert!(text.contains("exe=Safari"), "{text}");

    let mut window = s.clone();
    let mut foreign = window.procs.iter().find(|p| p.pid == 201).unwrap().clone();
    foreign.pid = 777;
    foreign.name = "foreign-writer".into();
    foreign.scope = RowScope::Tab(8);
    foreign.write_bps = Some(1_800_000.0);
    window.procs.push(foreign);
    window.tabs.push(crate::model::TabView {
        key: 8,
        scope: ScopeView {
            write_bps: Some(1_800_000.0),
            ..ScopeView::default()
        },
        ..crate::model::TabView::default()
    });
    let mut w = world();
    w.target.window_sessions.push(8);
    w.roots.push(Root {
        keys: vec![8],
        path: "/w/other-tab".into(),
        ..Root::default()
    });
    let q = SysQuery::parse("disk scope=window").unwrap();
    assert!(q.target_rows_only(), "the parser admits this scoped form");
    let scoped = ctx(&w, Some(&window), false, true);
    let owner = ctx(&w, Some(&window), true, true);
    let text = all_text(&render_text(&q, &scoped));
    assert!(text.contains("writer pid=201 "), "{text}");
    assert!(
        !text.contains("pid=777") && !text.contains("foreign-writer"),
        "{text}"
    );
    let owner_text = all_text(&render_text(&q, &owner));
    assert!(
        owner_text.contains("writer pid=777 exe=foreign-writer "),
        "{owner_text}"
    );
    let j = render_json(&q, &scoped);
    let owner_j = render_json(&q, &owner);
    let disk = j.get("disk").unwrap();
    let owner_disk = owner_j.get("disk").unwrap();
    let has_writer = |disk: &Jv, pid| match disk.get("writers") {
        Some(Jv::Arr(rows)) => rows.iter().any(|r| r.get("pid") == Some(&Jv::u(pid))),
        other => panic!("{other:?}"),
    };
    assert!(has_writer(disk, 201) && !has_writer(disk, 777));
    assert!(has_writer(owner_disk, 201) && has_writer(owner_disk, 777));
    assert_eq!(disk.get("withheld"), Some(&Jv::u(1)));
    assert_eq!(
        disk.get("write_bytes_per_s"),
        owner_disk.get("write_bytes_per_s")
    );
    assert_eq!(disk.get("roots"), owner_disk.get("roots"));
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    assert!(
        contract_violations(&owner_j).is_empty(),
        "{}",
        owner_j.to_text()
    );
}

#[test]
fn procs_rows_carry_labels_states_and_base_units() {
    let s = snapshot();
    let w = world();
    let c = ctx(&w, Some(&s), true, true);
    let r = render_text(&query(View::Procs, false), &c);
    let text = all_text(&r);
    assert!(
        text.contains("proc pid=201 exe=cargo label=fg pgid=201 cpu_mc=900"),
        "{text}"
    );
    assert!(text.contains("proc pid=200 exe=zsh label=root"), "{text}");
    assert!(text.contains("proc pid=202 exe=- label=job"), "{text}");
    assert!(text.contains("stat=run"), "{text}");
    assert!(text.contains("write_bps=900000"), "{text}");
    let mut q = query(View::Procs, false);
    q.top = 1;
    let r = render_text(&q, &c);
    assert!(all_text(&r).contains("more=2 rows"), "{}", all_text(&r));
}

#[test]
fn modulo_network_diagnostics_name_raw_counter_provenance_and_the_unknown_rate() {
    use crate::net_counter::{ByteCounter, DeltaError};
    let mut s = snapshot();
    let net = s.net.as_mut().expect("fixture network");
    net.rates = None;
    net.rates_why = Some(DeltaError::AmbiguousWrap);
    net.links[0].sample.byte_counter = ByteCounter::DarwinModulo32;
    net.links[0].sample.rx = 4096;
    net.links[0].rates = None;
    net.links[0].rates_why = Some(DeltaError::AmbiguousWrap);
    let w = world();
    let c = ctx(&w, Some(&s), true, true);
    let text = all_text(&render_text(&query(View::Net, false), &c));
    assert!(
        text.contains("unavailable(counter_wrap_ambiguous)"),
        "{text}"
    );
    assert!(text.contains("rx_bytes=4096") && text.contains("byte_counter=darwin_iflist2_modulo32 byte_counter_bits=32 byte_counter_quantum_bytes=1024"), "{text}");
    let j = render_json(&query(View::Net, true), &c);
    assert!(contract_violations(&j).is_empty(), "{j:?}");
    assert!(j.to_text().contains("counter_wrap_ambiguous"));
    assert_eq!(
        global_json(&s).get("net_rx_bytes_per_s"),
        Some(&Jv::why("counter_wrap_ambiguous"))
    );
}

#[test]
fn unknown_swap_is_not_zero_in_memory_global_and_explanation_views() {
    let mut s = snapshot();
    let used = s.mem.as_ref().unwrap().sample.used();
    let w = world();
    for (measurement, total, expected, expected_total, words) in [
        (
            None,
            None,
            Jv::why("not_read"),
            Jv::why("not_read"),
            "swap unknown",
        ),
        (
            None,
            Some(1 << 30),
            Jv::why("not_read"),
            Jv::u(1 << 30),
            "swap unknown",
        ),
        (Some(0), Some(0), Jv::u(0), Jv::u(0), "swap 0"),
    ] {
        let memory = &mut s.mem.as_mut().unwrap().sample;
        memory.swap_used = measurement;
        memory.swap_total = total;
        let c = ctx(&w, Some(&s), true, true);
        assert_eq!(global_json(&s).get("swap_used_bytes"), Some(&expected));
        let json = render_json(&query(View::Mem, true), &c);
        assert!(contract_violations(&json).is_empty(), "{json:?}");
        let swap = json.get("swap").expect("mem JSON fields are top-level");
        assert_eq!(swap.get("used_bytes"), Some(&expected));
        assert_eq!(swap.get("total_bytes"), Some(&expected_total));
        let text = all_text(&render_text(&query(View::Mem, false), &c));
        if measurement.is_none() {
            assert!(
                text.contains("swap unavailable(not_read) used of"),
                "{text}"
            );
        } else {
            assert!(text.contains("swap 0 KiB used of 0 KiB"), "{text}");
        }
        if total == Some(1 << 30) {
            assert!(text.contains("used of 1.0 GiB"), "{text}");
        }
        assert!(all_text(&render_text(&query(View::Overview, false), &c)).contains(words));
        assert!(all_text(&render_text(&query(View::Why, false), &c)).contains(words));
        assert_eq!(s.mem.as_ref().unwrap().sample.used(), used);
    }
}

/// A process's wait, peak and write rate say why each is unread on its OS: Windows samples no
/// wait or disk writes (`not_sampled`), Linux keeps no peak footprint (`unsupported`) and reads
/// wait and disk writes for this user's processes (`warming` until a second sweep, `refused` for
/// `Services`), and macOS reads all three (`warming` for rates, `refused` for peak or a refused
/// row). NEGATIVE CONTROL: a service row on Linux never stays `warming`.
#[test]
fn a_process_figure_no_sweep_reads_is_not_sampled() {
    let mut s = snapshot();
    let row = &mut s.procs[0];
    (row.wait, row.peak, row.write_bps, row.refused) = (None, None, None, false);
    let mut refused = row.clone();
    refused.refused = true;
    let mut service = row.clone();
    service.scope = RowScope::Services;
    let row = row.clone();
    let words = |family: &'static str, r: &ProcView| {
        let mut w = world();
        w.machine.family = family;
        let c = ctx(&w, Some(&s), true, true);
        let j = proc_json(&c, &s, r);
        ["wait_mc", "peak_bytes", "write_bytes_per_s"]
            .map(|key| j.get(key).cloned().expect("a figure or why"))
    };
    let why = |words: [&str; 3]| words.map(Jv::why);
    for r in [&row, &refused, &service] {
        assert_eq!(words("Windows", r), why(["not_sampled"; 3]));
    }
    assert_eq!(
        words("Linux", &row),
        why(["warming", "unsupported", "warming"])
    );
    for r in [&refused, &service] {
        assert_eq!(
            words("Linux", r),
            why(["refused", "unsupported", "refused"])
        );
    }
    assert_eq!(words("macOS", &row), why(["warming", "refused", "warming"]));
    for r in [&refused, &service] {
        assert_eq!(words("macOS", r), why(["refused"; 3]));
    }
    let mut w = world();
    w.machine.family = "Windows";
    let c = ctx(&w, Some(&s), true, true);
    let text = all_text(&render_text(&query(View::Procs, false), &c));
    let pid = row.pid;
    assert!(
        text.contains(&format!("proc pid={pid} "))
            && text.contains("wait_mc=unavailable(not_sampled) fp_bytes=",)
            && text
                .contains("peak_bytes=unavailable(not_sampled) write_bps=unavailable(not_sampled)"),
        "{text}"
    );
}

/// Each process row carries its age at the reading, and each program its oldest member's, a
/// lower bound while a member's birth went unread. NEGATIVE CONTROL: with no birth read, every
/// age reads unavailable, never 0.
#[test]
fn procs_carry_each_age_and_each_programs_oldest() {
    const MIN: i64 = 60_000;
    let at = sample(11, &[0, 0, 0, 0], 1).at_unix_ms;
    let born = |at_s: u64, busy: &[u64], k: u64| {
        let mut input = sample(at_s, busy, k);
        let procs = input.procs.as_mut().expect("a sweep");
        procs.push(proc(203, 201, Some(7 * k), 10, "cargo", 501));
        for p in procs.iter_mut() {
            p.birth_ms = match p.pid {
                200 => Some(at - 60 * MIN),
                201 => Some(at - 47 * MIN),
                _ => None,
            };
        }
        input
    };
    let mut v = Vitals::new();
    let _ = v.absorb(&born(10, &[0, 0, 0, 0], 0));
    let s = v.absorb(&born(11, &[20, 30, 100, 90], 1));
    let w = world();
    let c = ctx(&w, Some(&s), true, true);
    let text = all_text(&render_text(&query(View::Procs, false), &c));
    let row = |pid: u32| {
        text.lines()
            .find(|l| l.starts_with(&format!("proc pid={pid} ")))
            .unwrap_or_else(|| panic!("{pid}: {text}"))
    };
    assert!(row(201).ends_with(" age_ms=2820000"), "{}", row(201));
    assert!(
        row(202).ends_with(" age_ms=unavailable(not_sampled)"),
        "{}",
        row(202)
    );
    let j = render_json(&query(View::Procs, true), &c);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let procs = j.get("procs").expect("procs");
    let rows = match procs.get("rows") {
        Some(Jv::Arr(rows)) => rows,
        other => panic!("{other:?}"),
    };
    let age_of = |pid: u32| {
        rows.iter()
            .find(|r| r.get("pid") == Some(&Jv::u(u64::from(pid))))
            .and_then(|r| r.get("age_ms"))
            .cloned()
    };
    assert_eq!(age_of(200), Some(Jv::u(3_600_000)));
    assert_eq!(age_of(203), Some(Jv::why("not_sampled")));
    let programs = match procs.get("programs") {
        Some(Jv::Arr(programs)) => programs,
        other => panic!("{other:?}"),
    };
    let program = |exe: &str| {
        programs
            .iter()
            .find(|g| g.get("exe") == Some(&Jv::s(exe)))
            .unwrap_or_else(|| panic!("{exe}: {}", j.to_text()))
    };
    let cargo = program("cargo");
    assert_eq!(cargo.get("n"), Some(&Jv::u(2)));
    assert_eq!(
        (cargo.get("scope"), cargo.get("tab")),
        (Some(&Jv::s("tab")), Some(&Jv::u(1)))
    );
    assert_eq!(
        cargo.get("age_ms"),
        Some(&Jv::at_least(2_820_000, "not_sampled")),
        "one member unread: its oldest may be older"
    );
    assert_eq!(program("zsh").get("age_ms"), Some(&Jv::u(3_600_000)));
    assert_eq!(program("-").get("age_ms"), Some(&Jv::why("not_sampled")));
    assert!(
        programs
            .iter()
            .all(|g| g.get("scope") == Some(&Jv::s("tab"))),
        "the tab's programs only: {}",
        j.to_text()
    );

    let s = snapshot();
    let c = ctx(&w, Some(&s), true, true);
    let text = all_text(&render_text(&query(View::Procs, false), &c));
    let rows: Vec<&str> = text.lines().filter(|l| l.starts_with("proc ")).collect();
    assert!(
        !rows.is_empty()
            && rows
                .iter()
                .all(|l| l.ends_with(" age_ms=unavailable(not_sampled)")),
        "{text}"
    );
    let j = render_json(&query(View::Procs, true), &c);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    match j.get("procs").and_then(|p| p.get("programs")) {
        Some(Jv::Arr(programs)) => assert!(
            !programs.is_empty()
                && programs
                    .iter()
                    .all(|g| g.get("age_ms") == Some(&Jv::why("not_sampled"))),
            "{}",
            j.to_text()
        ),
        other => panic!("{other:?}"),
    }
}

/// A pane's programs are its tab's: a program groups by tab, under its least pane's key, so a
/// split tab's other pane lists them too. NEGATIVE CONTROL: another tab's program stays out.
#[test]
fn a_panes_programs_are_its_tabs() {
    let group = |name: &str, tab: u64| ProgramGroup {
        name: name.into(),
        scope: ProcScope::Tab(tab),
        n: 1,
        cores: 0.5,
        footprint: None,
        footprint_unread: 1,
        born_ms: None,
        unborn: 1,
    };
    let mut s = snapshot();
    s.programs = vec![group("cargo", 7), group("vim", 12)];
    let mut w = world();
    w.target.key = 9;
    w.target.panes = vec![7, 9];
    let c = ctx(&w, Some(&s), true, true);
    for scope in [ScopeArg::Pane, ScopeArg::Tab] {
        let mut q = query(View::Procs, true);
        q.scope = Some(scope);
        let j = render_json(&q, &c);
        let exes: Vec<Jv> = match j.get("procs").and_then(|p| p.get("programs")) {
            Some(Jv::Arr(programs)) => programs
                .iter()
                .filter_map(|g| g.get("exe").cloned())
                .collect(),
            other => panic!("{other:?}"),
        };
        assert_eq!(exes, [Jv::s("cargo")], "{scope:?}: {}", j.to_text());
    }
}

#[test]
fn unmeasured_figures_read_unavailable_never_zero() {
    let s = snapshot();
    let w = world();
    let c = ctx(&w, Some(&s), true, true);
    let jobs = all_text(&render_text(&query(View::Jobs, false), &c));
    assert!(jobs.contains("unavailable(not_sampled)"), "{jobs}");
    let j = render_json(&query(View::Jobs, true), &c);
    assert_eq!(j.get("jobs"), Some(&Jv::why("not_sampled")));
    let disk = render_json(&query(View::Disk, true), &c);
    let root = match disk.get("disk").and_then(|d| d.get("roots")) {
        Some(Jv::Arr(r)) => r[0].clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(root.get("alloc_bytes"), Some(&Jv::why("not_sampled")));
    let cores = all_text(&render_text(&query(View::Cores, false), &c));
    assert!(
        cores.contains("req_mhz=unavailable(not_sampled:requested_frequency)"),
        "{cores}"
    );
    assert!(cores.contains("core name=P1 cpu=2 busy_pm=1000"), "{cores}");
}

/// A Linux machine's unavailable words: with zswap off the compressed class says
/// `not_enabled:zswap` (not sampling), lifetime peak footprint is `unsupported` (never refused),
/// and volume purgeable space off APFS is `unsupported` (not sampling). NEGATIVE CONTROL: the
/// Mac's world keeps `not_sampled` for purgeable space and `refused`/`not_sampled` for peak.
#[test]
fn a_linux_machine_reads_its_own_unavailable_words() {
    let mut w = world();
    w.machine = Machine {
        executable: None,
        cpu: None,
        os: Some("Ubuntu 24.04.5 LTS".into()),
        family: "Linux",
    };
    let s = snapshot();
    let mut off = s.clone();
    if let Some(m) = off.mem.as_mut() {
        m.sample.compressed = None;
    }
    for p in &mut off.procs {
        p.peak = None;
    }
    let c = ctx(&w, Some(&off), true, true);
    let mem = all_text(&render_text(&query(View::Mem, false), &c));
    assert!(
        mem.contains("compressed unavailable(not_enabled:zswap)"),
        "{mem}"
    );
    assert!(mem.contains("peak_bytes=unavailable(unsupported)"), "{mem}");
    let j = render_json(&query(View::Mem, true), &c);
    let compressed = match j.get("classes") {
        Some(Jv::Arr(a)) => a
            .iter()
            .find(|x| x.get("class") == Some(&Jv::s("compressed")))
            .and_then(|x| x.get("bytes"))
            .cloned(),
        other => panic!("{other:?}"),
    };
    assert_eq!(compressed, Some(Jv::why("not_enabled:zswap")));
    // No lifetime peak on Linux: unsupported, never refused.
    let procs = all_text(&render_text(&query(View::Procs, false), &c));
    assert!(
        procs.contains("peak_bytes=unavailable(unsupported)")
            && !procs.contains("peak_bytes=unavailable(refused"),
        "{procs}"
    );
    // No purgeable space off APFS: unsupported, not a sampling still to come.
    let disk = all_text(&render_text(&query(View::Disk, false), &c));
    assert!(
        disk.contains("purgeable_bytes=unavailable(unsupported)"),
        "{disk}"
    );
    let j = render_json(&query(View::Disk, true), &c);
    let purgeable = j
        .get("disk")
        .and_then(|d| d.get("volume"))
        .and_then(|v| v.get("purgeable_bytes"))
        .cloned();
    assert_eq!(purgeable, Some(Jv::why("unsupported")));

    let mac = world();
    let c = ctx(&mac, Some(&off), true, true);
    let disk = all_text(&render_text(&query(View::Disk, false), &c));
    assert!(
        disk.contains("purgeable_bytes=unavailable(not_sampled)"),
        "{disk}"
    );
    let mem = all_text(&render_text(&query(View::Mem, false), &c));
    assert!(mem.contains("compressed unavailable(not_sampled)"), "{mem}");
}

#[test]
fn sys_disk_prints_what_the_walker_measured_through_the_seam() {
    use crate::sys::{DirSize, DiskTotals, HugeDirFact, RootSize, RootSizes};
    let s = snapshot();
    let mut w = world();
    w.roots = vec![
        Root {
            kinds: vec!["cwd", "repo"],
            path: "/Users//a/my proj".into(),
            keys: vec![7],
            tab: Some(1),
            size: RootSize::Complete(RootSizes {
                alloc: 7_050_000_000,
                logical: 6_900_000_000,
                entries: 63_039,
                direct_entries_max: 4_100,
                growth_bps: Some(7_400_000),
                age_s: Some(3),
            }),
            dirs: vec![
                DirSize {
                    rel: "target".into(),
                    alloc: 5_000_000_000,
                    entries: 40_000,
                },
                DirSize {
                    rel: "target/debug/incremental/deep/er".into(),
                    alloc: 1_080_000_000,
                    entries: 9_000,
                },
            ],
            shared_with: vec![2],
            counted_in: Some(1),
            ..Root::default()
        },
        Root {
            kinds: vec!["scratch"],
            path: "/private/tmp/claude-502/x".into(),
            keys: vec![7],
            tab: Some(1),
            size: RootSize::Walking {
                walked_entries: 12_000,
                partial_alloc: Some(2_100_000_000),
                last: None,
            },
            ..Root::default()
        },
        Root {
            kinds: vec!["work"],
            path: "/Users//a/Documents/x".into(),
            keys: vec![7],
            tab: Some(1),
            size: RootSize::NotMeasured("protected"),
            ..Root::default()
        },
    ];
    w.disk = Some(DiskTotals {
        total: 9_150_000_000,
        exclusive: 9_150_000_000,
        shared: 0,
        walking: true,
        not_measured: vec!["protected"],
        growth_bps: Some(7_400_000),
        warnings: vec![HugeDirFact {
            path: "/Users//a/my proj/target/debug/deps".into(),
            entries: 120_000,
            root: Some(1),
        }],
        ..DiskTotals::default()
    });
    let c = ctx(&w, Some(&s), true, true);
    let text = all_text(&render_text(&query(View::Disk, false), &c));
    assert!(
        text.contains(
            "\u{2265}9.2 GB exclusive (walking; 1 not measured: protected) · growing 7.4 MB/s"
        ),
        "{text}"
    );
    assert!(
        text.contains("root n=1 kinds=cwd,repo tab=1 path=/Users//a/my%20proj state=complete alloc_bytes=7050000000 entries=63039 growth_bps=7400000 walked_s=3 shared_with=2 counted_in=tab1"),
        "{text}"
    );
    assert!(
        text.contains("dir root=1 path=target alloc_bytes=5000000000 entries=40000 walked_s=3"),
        "{text}"
    );
    assert!(!text.contains("deep/er"), "below depth=3: {text}");
    assert!(
        text.contains("state=walking alloc_bytes=\u{2265}2100000000 entries=unavailable(walking) growth_bps=unavailable(walking) walked_entries=12000"),
        "{text}"
    );
    assert!(
        text.contains("state=protected alloc_bytes=unavailable(protected)"),
        "{text}"
    );
    assert!(
        text.contains("summary warnings 1 folder past 100,000 entries (largest 120000) · not measured protected · still walking"),
        "{text}"
    );
    let mut deep = query(View::Disk, false);
    deep.depth = Some(8);
    assert!(all_text(&render_text(&deep, &c)).contains("deep/er"));
    let foreign = ctx(&w, Some(&s), true, false);
    let text = all_text(&render_text(&query(View::Disk, false), &foreign));
    assert!(
        text.contains("dir root=1 rank=1 alloc_bytes=5000000000"),
        "{text}"
    );
    assert!(!text.contains("target"), "{text}");
    let j = render_json(&query(View::Disk, true), &c);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let d = j.get("disk").expect("disk");
    // The volume says free (what this user may write) and the reserve apart, under one name
    // each: this world's volume has 400 GB of free blocks, 20 GB of them kept back.
    let volume = d.get("volume").expect("volume");
    assert_eq!(volume.get("free_bytes"), Some(&Jv::u(380_000_000_000)));
    assert_eq!(volume.get("reserved_bytes"), Some(&Jv::u(20_000_000_000)));
    assert_eq!(volume.get("available_bytes"), None);
    assert_eq!(
        d.get("exclusive_bytes"),
        Some(&Jv::at_least(9_150_000_000, "walking"))
    );
    let Some(Jv::Arr(roots)) = d.get("roots") else {
        panic!("{}", j.to_text());
    };
    assert_eq!(roots[0].get("alloc_bytes"), Some(&Jv::u(7_050_000_000)));
    assert_eq!(roots[0].get("complete"), Some(&Jv::Bool(true)));
    let Some(Jv::Arr(dirs)) = roots[0].get("dirs") else {
        panic!("missing subtree rows")
    };
    assert_eq!(dirs[0].get("walked_age_s"), Some(&Jv::u(3)));
    assert_eq!(roots[1].get("walked_entries"), Some(&Jv::u(12_000)));
    let tab = all_text(&render_text(&query(View::Tab, false), &c));
    assert!(
        tab.contains(
            "disk   3 roots: cwd, repo, scratch, work · \u{2265}9.2 GB exclusive (walking; 1 not measured: protected)"
        ),
        "{tab}"
    );
    let tj = render_json(&query(View::Tab, true), &c);
    assert!(contract_violations(&tj).is_empty(), "{}", tj.to_text());
}

/// SYS DISK WITHHOLDS OTHER TABS' PANES AND PATHS FROM A TAB CALLER: a caller that is not of
/// owner class reads its own tab's panes and `withheld=N`; a caller in a session of either class
/// reads another tab's folders as `under=`. Negative control: an owner caller reads every pane.
#[test]
fn sys_disk_withholds_other_tabs_panes_and_paths_from_a_tab_caller() {
    use crate::sys::{DiskTotals, PaneLine, ScopeBytes, ScopeLine};
    let s = snapshot();
    let mut w = world();
    let pane = |key: u64, tab: usize, live: &str, why: Option<&'static str>| PaneLine {
        key,
        tab: Some(tab),
        tab_name: None,
        pane: 1,
        from: Some("shell"),
        exe: Some("zsh".into()),
        host: None,
        live: Some(live.into()),
        project: Some(live.into()),
        member: why.is_none(),
        why,
    };
    w.disk = Some(DiskTotals {
        scopes: vec![
            ScopeLine::Global {
                pool: Some("container:disk3".into()),
                used: Some(620_000_000_000),
                total: Some(1_000_000_000_000),
            },
            ScopeLine::Bytes(ScopeBytes {
                name: "aterm",
                pool: Some("container:disk3".into()),
                bytes: 9_000,
                drawn: true,
                complete: true,
                roots: 2,
                counted: 2,
                oldest_walk_s: Some(34),
                ..ScopeBytes::default()
            }),
            ScopeLine::Bytes(ScopeBytes {
                name: "tab",
                tab: Some(1),
                keys: vec![7],
                pool: Some("container:disk3".into()),
                why: Some("walking"),
                roots: 1,
                ..ScopeBytes::default()
            }),
        ],
        panes: vec![
            pane(7, 1, "/Users//a/my proj", None),
            pane(8, 2, "/Users//a/secret", None),
            pane(9, 3, "/Users//a", Some("too_broad")),
        ],
        ..DiskTotals::default()
    });
    let tab_caller = ctx(&w, Some(&s), false, true);
    let text = all_text(&render_text(&query(View::Disk, false), &tab_caller));
    assert!(
        text.contains("scope name=global pool=container:disk3 used_bytes=620000000000 total_bytes=1000000000000"),
        "{text}"
    );
    assert!(
        text.contains("scope name=aterm pool=container:disk3 bytes=9000 drawn=yes complete=yes roots=2 counted=2 oldest_walk_s=34 clamped=no"),
        "{text}"
    );
    assert!(
        text.contains("scope name=tab tab=1 pool=container:disk3 bytes=0 drawn=no why=walking complete=no roots=1 counted=0 oldest_walk_s=unavailable(not_sampled) clamped=no"),
        "{text}"
    );
    assert!(
        text.contains("pane tab=1 pane=1 from=shell exe=zsh live=/Users//a/my%20proj project=/Users//a/my%20proj member=yes"),
        "{text}"
    );
    assert!(
        !text.contains("pane tab=2") && !text.contains("secret"),
        "{text}"
    );
    assert!(
        text.contains("withheld=2 (pane rows outside this tab"),
        "{text}"
    );
    let j = render_json(&query(View::Disk, true), &tab_caller);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    assert_eq!(j.get("schema"), Some(&Jv::Int(2)));
    let d = j.get("disk").expect("disk");
    let Some(Jv::Arr(panes)) = d.get("panes") else {
        panic!("{}", j.to_text());
    };
    assert_eq!(panes.len(), 1);
    assert_eq!(d.get("panes_withheld"), Some(&Jv::u(2)));
    let Some(Jv::Arr(scopes)) = d.get("scopes") else {
        panic!("{}", j.to_text());
    };
    assert_eq!(scopes[2].get("why_not"), Some(&Jv::s("walking")));
    // An owner caller in another tab reads every pane, its paths outside its tab as `under=`.
    let owner_elsewhere = ctx(&w, Some(&s), true, false);
    let text = all_text(&render_text(&query(View::Disk, false), &owner_elsewhere));
    assert!(
        text.contains("pane tab=2 pane=1 from=shell exe=zsh under=home member=yes"),
        "{text}"
    );
    assert!(
        text.contains("pane tab=3 pane=1 from=shell exe=zsh under=home member=no why=too_broad"),
        "{text}"
    );
    assert!(
        !text.contains("withheld=") && !text.contains("secret"),
        "{text}"
    );
    let j = render_json(&query(View::Disk, true), &owner_elsewhere);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
}

/// A tab whose only folder no walk measures (a scratch HOME is too broad) reads its size as
/// unavailable in `sys disk` and `sys tab`, never `0 B`; beside a measured one, a lower bound.
#[test]
fn a_root_no_walk_measured_reads_unavailable_in_sys_disk_and_sys_tab_never_zero() {
    use crate::sys::{DiskTotals, RootSize, RootSizes};
    let s = snapshot();
    let mut w = world();
    w.roots[0].size = RootSize::NotMeasured("too_broad");
    w.disk = Some(DiskTotals {
        not_measured: vec!["too_broad"],
        ..DiskTotals::default()
    });
    let c = ctx(&w, Some(&s), true, true);
    let disk = all_text(&render_text(&query(View::Disk, false), &c));
    assert!(
        disk.contains(" · 1 root · sizes unavailable(too_broad) · writes "),
        "{disk}"
    );
    assert!(!disk.contains("0 B exclusive"), "{disk}");
    let tab = all_text(&render_text(&query(View::Tab, false), &c));
    assert!(
        tab.contains("disk   1 root: cwd · sizes unavailable(too_broad) · writes "),
        "{tab}"
    );
    let j = render_json(&query(View::Disk, true), &c);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let d = j.get("disk").expect("disk");
    assert_eq!(d.get("exclusive_bytes"), Some(&Jv::why("too_broad")));
    assert_eq!(d.get("total_bytes"), Some(&Jv::why("too_broad")));
    let tj = render_json(&query(View::Tab, true), &c);
    assert!(contract_violations(&tj).is_empty(), "{}", tj.to_text());
    assert_eq!(
        tj.get("tab").and_then(|t| t.get("root_bytes")),
        Some(&Jv::why("too_broad"))
    );

    w.roots.push(Root {
        kinds: vec!["work"],
        path: "/w/repo".into(),
        keys: vec![7],
        tab: Some(1),
        size: RootSize::Complete(RootSizes {
            alloc: 7_050_000_000,
            ..RootSizes::default()
        }),
        ..Root::default()
    });
    w.disk = Some(DiskTotals {
        total: 7_050_000_000,
        exclusive: 7_050_000_000,
        not_measured: vec!["too_broad"],
        ..DiskTotals::default()
    });
    let c = ctx(&w, Some(&s), true, true);
    let disk = all_text(&render_text(&query(View::Disk, false), &c));
    assert!(
        disk.contains(
            " · 2 roots · \u{2265}7.0 GB exclusive (1 not measured: too_broad) · writes "
        ),
        "{disk}"
    );
    let tj = render_json(&query(View::Tab, true), &c);
    assert_eq!(
        tj.get("tab").and_then(|t| t.get("root_bytes")),
        Some(&Jv::at_least(7_050_000_000, "too_broad"))
    );
}

#[test]
fn before_any_snapshot_every_view_says_warming() {
    let w = world();
    let c = ctx(&w, None, true, true);
    for view in View::ALL {
        let r = render_text(&query(view, false), &c);
        assert!(all_text(&r).contains("warming"), "{view:?}");
        assert!(
            r.header(&c, false)
                .contains("as_of=- age_ms=- window_ms=- next=7f3a.0 state=warming")
        );
        let j = render_json(&query(view, true), &c);
        assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    }
}

#[test]
fn a_repeat_reply_folds_to_one_same_line_and_events_stay() {
    let s = snapshot();
    let w = world();
    let c = ctx(&w, Some(&s), true, true);
    let first = render_text(&query(View::Overview, false), &c);
    let base = fingerprints(&first);
    let mut again = render_text(&query(View::Overview, false), &c);
    let folded = apply_delta(&mut again, &base);
    assert!(folded.contains(&"global".to_owned()), "{folded:?}");
    assert_eq!(again.lines[0].key, "same");
    assert!(again.lines.iter().any(|l| {
        l.text
            .starts_with("e seq=2 at=21:21:37 kind=pressure from=normal to=warn")
    }));
    assert!(again.header(&c, true).ends_with(" delta=1"));

    let mut w = world();
    w.events = [
        (1, s.at_ns - 2_250_000_000),
        (2, s.at_ns),
        (3, s.at_ns + 2_250_000_000),
    ]
    .into_iter()
    .map(|(seq, at_ns)| Event {
        seq,
        at_ns,
        kind: EventKind::Pressure {
            from: Pressure::Normal,
            to: Pressure::Warn,
        },
    })
    .collect();
    let c = Ctx {
        head_seq: 3,
        ..ctx(&w, Some(&s), true, true)
    };
    let mut delta = render_text(&query(View::Overview, false), &c);
    apply_delta(&mut delta, &base);
    assert!(
        delta
            .lines
            .iter()
            .any(|l| l.text.starts_with("e seq=1 at=21:21:35 "))
    );
    assert!(
        delta
            .lines
            .iter()
            .any(|l| l.text.starts_with("e seq=2 at=21:21:37 "))
    );
    assert!(
        delta
            .lines
            .iter()
            .any(|l| l.text.starts_with("e seq=3 at=21:21:39 "))
    );
    assert!(delta.header(&c, true).contains(" next=7f3a.3 "));
    let j = render_json(&query(View::Overview, true), &c);
    let times: Vec<&Jv> = match j.get("events") {
        Some(Jv::Arr(events)) => events
            .iter()
            .map(|e| e.get("at_unix_ms").unwrap())
            .collect(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        times,
        [
            &Jv::u(c.as_of_unix_ms - 2_250),
            &Jv::u(c.as_of_unix_ms),
            &Jv::u(c.as_of_unix_ms + 2_250)
        ]
    );
    let zero = Ctx {
        as_of_unix_ms: 0,
        ..c.clone()
    };
    assert_eq!(event_unix_ms(&zero, &s, 0), 0);
    let end = Ctx {
        as_of_unix_ms: u64::MAX - 1,
        ..c
    };
    assert_eq!(event_unix_ms(&end, &s, u64::MAX), u64::MAX);
}

#[test]
fn another_tabs_events_and_machine_names_print_only_for_the_owner() {
    use crate::events::LoadScope;
    use crate::jobs::{Class, Verdict};
    let s = snapshot();
    let mut w = world();
    let at_ns = 11_000_000_000;
    let job = |tab: u64, id: &str| Event {
        seq: 2,
        at_ns,
        kind: EventKind::Job {
            tab,
            id: id.into(),
            from: Some(Verdict::Fine),
            to: Verdict::Stuck,
            class: Class::Waiting,
            task: None,
            exit: None,
        },
    };
    let load = |scope: LoadScope| Event {
        seq: 2,
        at_ns,
        kind: EventKind::Load {
            scope,
            from_mc: 100,
            to_mc: 3000,
            top: Some("cc1plus".into()),
        },
    };
    w.events.extend([
        job(7, "a1"),
        job(99, "a9"),
        load(LoadScope::Global),
        load(LoadScope::Tab(7)),
        load(LoadScope::Tab(99)),
        Event {
            seq: 2,
            at_ns,
            kind: EventKind::Orphan {
                key: 99,
                label: "Tab 2".into(),
                procs: 3,
                top: Some("node".into()),
            },
        },
    ]);
    let events = |owner: bool| -> Vec<String> {
        let c = ctx(&w, Some(&s), owner, true);
        render_text(&query(View::Overview, false), &c)
            .lines
            .into_iter()
            .filter(|l| l.text.starts_with("e "))
            .map(|l| l.text)
            .collect()
    };
    let scoped = events(false).join("\n");
    assert!(scoped.contains("kind=pressure"), "{scoped}");
    assert!(scoped.contains("job=a1 "), "{scoped}");
    assert!(!scoped.contains("job=a9"), "{scoped}");
    assert!(!scoped.contains("local=99"), "{scoped}");
    assert!(
        scoped.contains("kind=load scope=global from_mc=100 to_mc=3000"),
        "{scoped}"
    );
    assert!(
        !scoped.contains("scope=global from_mc=100 to_mc=3000 top="),
        "{scoped}"
    );
    assert!(
        scoped.contains("scope=tab tab=1 from_mc=100 to_mc=3000 top=cc1plus"),
        "{scoped}"
    );
    assert!(!scoped.contains("node"), "{scoped}");
    assert_eq!(events(false).len(), 4, "{scoped}");
    let j = render_json(
        &query(View::Overview, true),
        &ctx(&w, Some(&s), false, true),
    );
    assert!(
        matches!(j.get("events"), Some(Jv::Arr(e)) if e.len() == 4),
        "{}",
        j.to_text()
    );
    let owner = events(true).join("\n");
    assert_eq!(events(true).len(), 7, "{owner}");
    assert!(
        owner.contains("job=a9") && owner.contains("top=node"),
        "{owner}"
    );
    assert!(
        owner.contains("scope=global from_mc=100 to_mc=3000 top=cc1plus"),
        "{owner}"
    );
}

fn snapshot_with_jobs() -> Snapshot {
    use crate::jobs::{Class, Demand, Findings, JobsView, Named, Verdict};
    use crate::sys::jobs_view::fixtures::{row, stuck};
    let mut s = snapshot();
    let mut running = row("a44120", 7, Class::Running, Verdict::Fine);
    running.exe = "targo".into();
    s.jobs = Some(std::sync::Arc::new(JobsView {
        rows: vec![stuck("a40977", 7), running, stuck("a9", 99)],
        findings: Findings {
            stopped: vec![
                Named {
                    pid: 10,
                    exec_version: None,
                    name: "vim".into(),
                    tab: Some(7),
                    job: None,
                    for_ms: 0,
                },
                Named {
                    pid: 11,
                    exec_version: None,
                    name: "mail".into(),
                    tab: Some(99),
                    job: None,
                    for_ms: 0,
                },
            ],
            own_procs: 300,
            process_limit: Some(5_000),
            ..Findings::default()
        },
        demand: Demand::Fast,
        lanes_off: vec!["lane_off:kqueue_detail"],
        parked_tabs: Vec::new(),
        at_ms: s.at_ns / 1_000_000,
    }));
    s
}

/// A reading the engine did not run on (a pull, a reading inside the engine's cadence) carries
/// the last pass's view, so `sys jobs` says that pass's time and age in its header, in both
/// shapes: a held finding's `for_ms` and a row's `quiet_ms` are as of it, as status's
/// `program_check=` tells the same `for_ms` on by the view's age. The age is the snapshot's own
/// [`Snapshot::jobs_age_ms`], never its clock against the view's: a recorded scene's snapshot
/// keeps the scene's clock. NEGATIVE CONTROL: a view the snapshot's own reading made reads the
/// snapshot's time, and every other view keeps it.
#[test]
fn sys_jobs_speaks_as_of_the_pass_that_made_its_view() {
    use crate::jobs::Named;
    let w = world();
    let carried = |older_ms: u64| {
        let mut s = snapshot_with_jobs();
        s.jobs_age_ms = older_ms;
        let mut v = s.jobs.as_deref().cloned().unwrap();
        v.at_ms = 0;
        v.findings.program_check = vec![Named {
            pid: 401,
            exec_version: None,
            name: "gh".into(),
            tab: Some(7),
            job: None,
            for_ms: 2_000,
        }];
        s.jobs = Some(std::sync::Arc::new(v));
        s
    };
    let read = |s: &Snapshot, view: View| {
        let c = ctx(&w, Some(s), false, true);
        let reply = render_text(&query(view, false), &c);
        let age = render_json(&query(view, true), &c)
            .get("as_of")
            .and_then(|a| a.get("age_ms"))
            .cloned();
        (reply.header(&c, false), all_text(&reply), age)
    };

    let late = carried(10_000);
    let (header, text, age) = read(&late, View::Jobs);
    assert!(
        header.contains(" as_of=21:21:27.412 age_ms=10120 "),
        "{header}"
    );
    assert_eq!(age, Some(Jv::Int(10_120)));
    assert!(
        text.contains("finding kind=program-check pid=401 exe=gh for_ms=2000"),
        "{text}"
    );
    assert!(text.contains("quiet_ms=1152000"), "{text}");
    let (header, _, _) = read(&late, View::Overview);
    assert!(
        header.contains(" as_of=21:21:37.412 age_ms=120 "),
        "{header}"
    );

    let (header, _, age) = read(&carried(0), View::Jobs);
    assert!(
        header.contains(" as_of=21:21:37.412 age_ms=120 "),
        "{header}"
    );
    assert_eq!(age, Some(Jv::Int(120)));
}

#[test]
fn sys_jobs_reads_the_engines_rows_of_the_scope_in_both_shapes() {
    let s = snapshot_with_jobs();
    let w = world();
    let c = ctx(&w, Some(&s), false, true);
    let text = all_text(&render_text(&query(View::Jobs, false), &c));
    let first = text.lines().next().unwrap();
    assert_eq!(
        first,
        "jobs  tab 1 · 2 jobs · 1 running · 1 stuck · worst: silent 19m12s, about 19× the assumed 60 s gap, φ 8.34"
    );
    for (id, fields) in [
        (
            "a40977",
            &["exe=curl", "class=waiting", "wait=tcp", "verdict=stuck"][..],
        ),
        (
            "a44120",
            &["exe=targo", "class=running", "verdict=fine"][..],
        ),
    ] {
        let line = text
            .lines()
            .find(|line| line.starts_with(&format!("job id={id} ")))
            .unwrap();
        for field in fields {
            assert!(line.split_whitespace().any(|word| word == *field), "{line}");
        }
    }
    assert!(!text.contains("a9 "), "{text}");
    assert!(
        text.contains("finding kind=stopped pid=10 exe=vim"),
        "{text}"
    );
    assert!(!text.contains("pid=11"), "{text}");
    assert!(text.contains("lanes lane_off:kqueue_detail"), "{text}");
    assert!(text.contains("await sys job=a40977 done,stuck"), "{text}");
    let tab = all_text(&render_text(&query(View::Tab, false), &c));
    assert!(
        tab.contains("jobs   2 jobs · 1 running · 1 stuck · worst: silent 19m12s"),
        "{tab}"
    );

    let j = render_json(&query(View::Jobs, true), &c);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let summary = j.get("summary").unwrap();
    assert_eq!(summary.get("jobs"), Some(&Jv::Int(2)));
    assert_eq!(summary.get("stuck"), Some(&Jv::Int(1)));
    assert_eq!(summary.get("worst_phi_m"), Some(&Jv::Int(8_338)));
    match j.get("jobs") {
        Some(Jv::Arr(rows)) => {
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].get("id"), Some(&Jv::s("a40977")));
            assert_eq!(rows[0].get("verdict"), Some(&Jv::s("stuck")));
            assert_eq!(rows[0].get("tab"), Some(&Jv::Int(1)));
            assert_eq!(rows[0].get("cpu_mc"), None);
        }
        other => panic!("{other:?}"),
    }
    for view in View::ALL {
        let j = render_json(&query(view, true), &c);
        let v = contract_violations(&j);
        assert!(v.is_empty(), "{view:?}: {v:?}\n{}", j.to_text());
    }
    let t = render_json(&query(View::Tab, true), &c);
    assert_eq!(
        t.get("tab")
            .and_then(|t| t.get("jobs"))
            .and_then(|j| j.get("running")),
        Some(&Jv::Int(1))
    );
}

#[test]
fn jobs_capacity_keeps_machine_and_slot_ages_and_unknowns_separate() {
    use crate::model::{SlotsUnread, SlotsView};
    let mut s = snapshot_with_jobs();
    s.jobs_age_ms = 10_000;
    s.mem
        .get_or_insert_with(crate::model::MemView::default)
        .sample
        .available = 77 << 30;
    s.slots_age_ms = Some(60_000);
    s.slots = Some(SlotsView::Read {
        n: 5,
        held: 1,
        free: 3,
        pending: 1,
        unknown: 0,
        discounted: 2,
    });
    let w = world();
    let c = ctx(&w, Some(&s), false, true);
    let j = render_json(&query(View::Jobs, true), &c);
    let machine = j.get("machine").unwrap();
    assert_eq!(machine.get("age_ms"), Some(&Jv::Int(120)));
    assert_eq!(machine.get("available_bytes"), Some(&Jv::u(77 << 30)));
    let slots = machine.get("slots").unwrap();
    assert_eq!(slots.get("free"), Some(&Jv::Int(1)));
    assert_eq!(slots.get("pending"), Some(&Jv::Int(1)));
    assert_eq!(slots.get("age_ms"), Some(&Jv::Int(60_120)));
    let text = all_text(&render_text(&query(View::Jobs, false), &c));
    assert!(
        text.contains("1/5 confirmed free, 1 awaiting confirmation, 0 unknown, age_ms=60120"),
        "{text}"
    );

    // Even when the job engine has not sampled, missing machine counters are not zero.
    s.jobs = None;
    s.cpu = None;
    s.mem = None;
    s.slots = Some(SlotsView::Unread(SlotsUnread::Refused));
    let c = ctx(&w, Some(&s), false, true);
    let j = render_json(&query(View::Jobs, true), &c);
    let machine = j.get("machine").unwrap();
    assert_eq!(
        machine.get("cpu_busy_mc"),
        Some(&Jv::why("no_counter_delta"))
    );
    assert_eq!(
        machine.get("available_bytes"),
        Some(&Jv::why("not_sampled"))
    );
    assert_eq!(machine.get("slots"), Some(&Jv::why("refused")));
    assert!(
        all_text(&render_text(&query(View::Jobs, false), &c))
            .contains("build_slots=unavailable(refused)")
    );
}

#[test]
fn jobs_capacity_unconfirmed_slots_are_not_an_admission_queue() {
    use crate::model::SlotsView;
    let mut s = snapshot_with_jobs();
    s.slots_age_ms = Some(36_093);
    s.slots = Some(SlotsView::Read {
        n: 4,
        held: 0,
        free: 0,
        pending: 4,
        unknown: 0,
        discounted: 0,
    });
    let w = world();
    let c = ctx(&w, Some(&s), false, true);
    let text = all_text(&render_text(&query(View::Jobs, false), &c));
    assert!(
        text.contains("0/4 confirmed free, 4 awaiting confirmation"),
        "{text}"
    );
    assert!(text.contains("age_ms=36213"), "{text}");
    assert!(
        text.contains("paired slot observations; admission queue not measured"),
        "{text}"
    );

    // The existing wire counts still describe slot observations, not ticket leases.
    let j = render_json(&query(View::Jobs, true), &c);
    let slots = j.get("machine").unwrap().get("slots").unwrap();
    assert_eq!(slots.get("free"), Some(&Jv::Int(0)));
    assert_eq!(slots.get("pending"), Some(&Jv::Int(4)));
    assert_eq!(slots.get("age_ms"), Some(&Jv::Int(36_213)));
}

#[test]
fn sys_jobs_scope_global_is_every_row_and_finding() {
    let s = snapshot_with_jobs();
    let w = world();
    let c = ctx(&w, Some(&s), true, true);
    let mut q = SysQuery::parse("jobs scope=global").unwrap();
    q.json = false;
    let text = all_text(&render_text(&q, &c));
    assert!(text.starts_with("jobs  global · 3 jobs"), "{text}");
    assert!(
        text.contains("finding kind=stopped pid=11 exe=mail"),
        "{text}"
    );
    assert!(text.contains("verdict=stuck tab=1"), "{text}");
    assert!(
        text.lines()
            .any(|l| l.starts_with("job id=a9 ") && l.ends_with("verdict=stuck")),
        "{text}"
    );
    assert!(
        text.contains(
            "more=aterm ctl @self await sys job=a40977 done,stuck (wait for one) · sys procs"
        ),
        "{text}"
    );
    let mut q = SysQuery::parse("jobs scope=global top=1").unwrap();
    q.json = false;
    let text = all_text(&render_text(&q, &c));
    let more: Vec<&str> = text.lines().filter(|l| l.starts_with("more=")).collect();
    assert_eq!(
        more,
        [
            "more=2 jobs · top=3 · aterm ctl @self await sys job=a40977 done,stuck (wait for one) · sys procs"
        ],
        "{text}"
    );

    let mut other = snapshot_with_jobs();
    let mut view = (**other.jobs.as_ref().expect("jobs")).clone();
    view.rows.retain(|r| r.tab == 99);
    other.jobs = Some(std::sync::Arc::new(view));
    let mut w2 = world();
    w2.tabs.push(TabFact {
        key: 99,
        sid: "s-cd34".into(),
        window: Some(1),
        tab: Some(2),
    });
    let c2 = ctx(&w2, Some(&other), true, true);
    let mut q = SysQuery::parse("jobs scope=aterm").unwrap();
    q.json = false;
    let text = all_text(&render_text(&q, &c2));
    assert!(
        text.contains("more=aterm ctl @s-cd34 await sys job=a9 done,stuck (wait for one) · sys jobs scope=global (owner) · sys procs"),
        "{text}"
    );
}

#[test]
fn reported_cpu_counter_differences_never_become_wait_fractions() {
    for (cpu, difference) in [
        (0.0, Some(0.0)),
        (1.0, Some(1.0)),
        (1.0, Some(-1.5)),
        (1.0, None),
    ] {
        let fig = Fig {
            present: true,
            cores: cpu,
            wait: difference,
            ..Fig::default()
        };
        let json = fig_json(&fig);
        assert_eq!(
            json.get("starved_pm"),
            Some(&Jv::why("unavailable:execution_window_unknown"))
        );
        assert_eq!(
            json.get("wait_basis"),
            Some(&Jv::s("runnable_minus_cpu_counter_growth"))
        );
        if difference == Some(-1.5) {
            assert_eq!(json.get("wait_mc"), Some(&Jv::Int(-1500)));
        }
    }
}

#[test]
fn reported_cpu_growth_is_not_clipped_to_the_old_u32_rate_ceiling() {
    let fig = Fig {
        present: true,
        cores: 5_000_000.0,
        ..Fig::default()
    };
    assert_eq!(fig_json(&fig).get("cpu_mc"), Some(&Jv::Int(5_000_000_000)));
}

#[test]
fn reported_cpu_timing_uses_its_process_read_span_and_endpoint() {
    let mut scene = crate::explain::fixtures::claude_reaped_1032();
    scene.unanswered = 0;
    let mut vitals = Vitals::new();
    vitals.absorb(&scene.reading(0, true));
    vitals.absorb(&scene.reading(4000, false));
    vitals.absorb(&scene.reading(5000, true));
    let current_reading = scene.reading(6000, false);
    let expected_at = current_reading.at_ns;
    let mut snap = vitals.absorb(&current_reading);
    let cpu = snap.scoped_cpu.as_ref().unwrap();
    assert_eq!(
        (cpu.read_at_ns, cpu.window_ns),
        (expected_at, 1_000_000_000)
    );
    assert_eq!(
        snap.why.as_ref().unwrap().at_ns,
        scene.reading(5000, true).at_ns
    );
    assert_eq!(
        cpu_timing(&snap),
        Some((expected_at, 1_000_000_000, "process_and_reap"))
    );
    snap.at_ns += 2_000_000_000;
    snap.at_unix_ms += 2000;
    assert_eq!(
        cpu_timing(&snap),
        Some((expected_at, 1_000_000_000, "process_and_reap"))
    );
}

#[test]
fn reported_cpu_process_json_uses_the_rows_chosen_read_span() {
    for process_marks in [false, true] {
        let mut before = sample(10, &[0; 4], 0);
        let mut after = sample(11, &[1; 4], 1);
        for (reading, mark) in [(&mut before, 10_500_000_000), (&mut after, 11_750_000_000)] {
            reading
                .procs
                .as_mut()
                .unwrap()
                .iter_mut()
                .find(|p| p.pid == 201)
                .unwrap()
                .at_ns = process_marks.then_some(mark);
        }
        let mut vitals = Vitals::new();
        vitals.absorb(&before);
        let snapshot = vitals.absorb(&after);
        let w = world();
        let c = ctx(&w, Some(&snapshot), true, true);
        let row = snapshot.procs.iter().find(|row| row.pid == 201).unwrap();
        let json = proc_json(&c, &snapshot, row);
        assert_eq!(snapshot.procs_window_ns, Some(1_000_000_000));
        assert_eq!(
            json.get("cpu_mc"),
            Some(&Jv::Int(if process_marks { 720 } else { 900 }))
        );
        assert_eq!(
            json.get("cpu_read_span_ns"),
            Some(&Jv::u(if process_marks {
                1_250_000_000
            } else {
                1_000_000_000
            }))
        );
        assert_eq!(
            json.get("cpu_execution_window"),
            Some(&Jv::why("unavailable:execution_window_unknown"))
        );
        let mut unavailable = row.clone();
        unavailable.cores = None;
        unavailable.cpu_read_span_ns = None;
        assert_eq!(
            proc_json(&c, &snapshot, &unavailable).get("cpu_read_span_ns"),
            Some(&Jv::why("warming")),
            "a row without a valid rate cannot borrow the sweep's span"
        );
    }
}

#[test]
fn a_first_sweeps_procs_are_warming_never_zero() {
    let first = Vitals::new().absorb(&sample(10, &[0, 0, 0, 0], 0));
    assert_eq!(first.total_cores(), Some(4));
    let w = world();
    let c = ctx(&w, Some(&first), true, true);
    assert_eq!(machine_json(&c, &first).get("cpus"), Some(&Jv::u(4)));
    let overview = all_text(&render_text(&query(View::Overview, false), &c));
    assert!(overview.contains("4 cores total"), "{overview}");
    let text = all_text(&render_text(&query(View::Procs, false), &c));
    assert!(
        text.starts_with("procs  tab 1 · unavailable(warming): one process sweep has no interval"),
        "{text}"
    );
    let j = render_json(&query(View::Procs, true), &c);
    assert_eq!(j.get("procs"), Some(&Jv::why("warming")), "{}", j.to_text());
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let t = render_json(&query(View::Tab, true), &c);
    assert_eq!(t.get("procs"), Some(&Jv::why("warming")), "{}", t.to_text());
    let s = snapshot();
    let c = ctx(&w, Some(&s), true, true);
    let j = render_json(&query(View::Procs, true), &c);
    assert!(
        matches!(
            j.get("procs").and_then(|p| p.get("cpu_mc")),
            Some(Jv::Int(_))
        ),
        "{}",
        j.to_text()
    );
}

/// [`snapshot`] with two GPU client reads 1 s apart: aterm's own client ran 15 ms, the tab's
/// cargo 5 ms, Safari (another of the user's) 2 ms, WindowServer (another user's) 8 ms, against
/// the device's 4 %.
fn gpu_snapshot() -> Snapshot {
    use crate::gpu_clients::{GpuClientRow, GpuClientsSample};
    let read = |s: &mut Sample, k: u64| {
        let row = |entry: u64, pid: u32, name: &str, ms: u64| GpuClientRow {
            entry,
            pid,
            name: name.into(),
            gpu_ns: ms * k * 1_000_000,
        };
        s.gpu_clients = Some(GpuClientsSample {
            at_ns: s.at_ns,
            complete: true,
            entries: vec![1, 2, 3, 4],
            rows: vec![
                row(1, 100, "aterm", 15),
                row(2, 201, "cargo", 5),
                row(3, 900, "Safari", 2),
                row(4, 88, "WindowServer", 8),
            ],
        });
    };
    let mut v = Vitals::new();
    let mut first = sample(10, &[0, 0, 0, 0], 0);
    read(&mut first, 0);
    let _ = v.absorb(&first);
    let mut second = sample(11, &[20, 30, 100, 90], 1);
    read(&mut second, 1);
    v.absorb(&second)
}

#[test]
fn gpu_client_time_fills_the_lanes_slots_in_text_and_json() {
    let w = world();
    let mut model = Vitals::new();
    model.absorb(&sample(10, &[0; 4], 0));
    let mut reading = sample(11, &[20, 30, 100, 90], 1);
    reading.gpu.as_mut().unwrap().device_pct = None;
    let unread = model.absorb(&reading);
    assert!(unread.gpu.is_none(), "no utilization-bearing view was made");
    let unread_ctx = ctx(&w, Some(&unread), true, true);
    for view in [View::Overview, View::Global, View::Gpu] {
        let text = all_text(&render_text(&query(view, false), &unread_ctx));
        assert!(text.contains("Apple M5"), "{view:?}: {text}");
        assert!(text.contains("10 cores"), "{view:?}: {text}");
        assert!(
            text.contains("unavailable(not_sampled:device_utilization)"),
            "{view:?}: {text}"
        );
        let json = render_json(&query(view, true), &unread_ctx);
        assert!(contract_violations(&json).is_empty(), "{json:?}");
    }
    assert_eq!(
        machine_json(&unread_ctx, &unread).get("gpu_cores"),
        Some(&Jv::u(10))
    );
    assert_eq!(
        machine_json(&unread_ctx, &unread).get("gpu_name"),
        Some(&Jv::s("Apple M5"))
    );
    assert_eq!(
        global_json(&unread).get("gpu_pm"),
        Some(&Jv::why("not_sampled:device_utilization"))
    );
    let gpu = gpu_json(&query(View::Gpu, true), &unread_ctx, &unread);
    assert_eq!(
        gpu.get("device_pm"),
        Some(&Jv::why("not_sampled:device_utilization"))
    );
    assert_eq!(gpu.get("cores"), Some(&Jv::u(10)));
    assert_eq!(gpu.get("name"), Some(&Jv::s("Apple M5")));

    reading.at_ns += 1_000_000_000;
    reading.gpu.as_mut().unwrap().device_pct = Some(0);
    let zero = model.absorb(&reading);
    assert_eq!(global_json(&zero).get("gpu_pm"), Some(&Jv::u(0)));
    assert!(gpu_words(&zero).starts_with("0% busy"));
    reading.at_ns += 1_000_000_000;
    reading.gpu = None;
    let missing = model.absorb(&reading);
    let missing_ctx = ctx(&w, Some(&missing), true, true);
    assert_eq!(gpu_words(&missing), unavailable("unsupported"));
    assert_eq!(
        gpu_json(&query(View::Gpu, true), &missing_ctx, &missing),
        Jv::why("unsupported")
    );
    let off = snapshot();
    let c = ctx(&w, Some(&off), true, true);
    let gpu = all_text(&render_text(&query(View::Gpu, false), &c));
    assert!(
        gpu.contains("per-scope GPU time unavailable(lane_off:gpu_clients)")
            && !gpu.contains("client pid="),
        "{gpu}"
    );
    let tab = all_text(&render_text(&query(View::Tab, false), &c));
    assert!(
        tab.contains("per-tab GPU time unavailable(lane_off:gpu_clients)"),
        "{tab}"
    );
    let j = render_json(&query(View::Gpu, true), &c);
    let g = j.get("gpu").expect("gpu");
    assert_eq!(g.get("split_pm"), Some(&Jv::why("lane_off:gpu_clients")));
    assert_eq!(g.get("clients"), Some(&Jv::why("lane_off:gpu_clients")));
    let procs = all_text(&render_text(&query(View::Procs, false), &c));
    assert!(
        procs.contains("gpu_ppm=unavailable(lane_off:gpu_clients)"),
        "{procs}"
    );

    let on = gpu_snapshot();
    let c = ctx(&w, Some(&on), true, true);
    for view in View::ALL {
        let j = render_json(&query(view, true), &c);
        let v = contract_violations(&j);
        assert!(v.is_empty(), "{view:?}: {v:?}\n{}", j.to_text());
    }
    let gpu = all_text(&render_text(&query(View::Gpu, false), &c));
    assert!(
        gpu.contains(
            "split   tab 0.5% · rest of aterm 1.5% · others 0.2% · macOS services 0.8% · client \
             time 3.0%, within 2% of the device's whole percent, so no residual"
        ),
        "{gpu}"
    );
    let rows: Vec<&str> = gpu.lines().filter(|l| l.starts_with("client ")).collect();
    assert_eq!(
        rows,
        [
            "client pid=100 exe=aterm scope=self share_ppm=15000",
            "client pid=88 exe=WindowServer scope=services share_ppm=8000",
            "client pid=201 exe=cargo scope=tab tab=1 share_ppm=5000",
            "client pid=900 exe=Safari scope=others share_ppm=2000",
        ],
        "{gpu}"
    );
    let tab = all_text(&render_text(&query(View::Tab, false), &c));
    assert!(
        tab.contains("per-tab GPU time 0.5% · the device is 4% busy"),
        "{tab}"
    );
    let aterm = all_text(&render_text(&query(View::Aterm, false), &c));
    assert!(
        aterm.contains("aterm's GPU time 2.0% of the GPU"),
        "{aterm}"
    );
    let procs = all_text(&render_text(&query(View::Procs, false), &c));
    assert!(
        procs.contains("exe=cargo") && procs.contains("gpu_ppm=5000 "),
        "{procs}"
    );

    let j = render_json(&query(View::Gpu, true), &c);
    let g = j.get("gpu").expect("gpu");
    let split = g.get("split_pm").expect("split_pm");
    let part = |k: &str| split.get(k).and_then(part_value);
    assert_eq!(
        SPLIT_PARTS.map(part),
        [Some(5), Some(15), Some(2), Some(8), Some(10)],
        "{}",
        j.to_text()
    );
    match g.get("clients") {
        Some(Jv::Arr(rows)) => {
            assert_eq!(rows.len(), 4);
            assert_eq!(rows[0].get("share_ppm"), Some(&Jv::u(15_000)));
            assert_eq!(rows[2].get("tab"), Some(&Jv::u(1)));
        }
        other => panic!("{other:?}"),
    }
    let j = render_json(&query(View::Tab, true), &c);
    assert_eq!(j.get("tab").and_then(|t| t.get("gpu_pm")), Some(&Jv::u(5)));
    let j = render_json(&query(View::Aterm, true), &c);
    assert_eq!(
        j.get("aterm").and_then(|a| a.get("gpu_pm")),
        Some(&Jv::u(20))
    );

    let scoped = ctx(&w, Some(&on), false, true);
    let gpu = all_text(&render_text(&query(View::Gpu, false), &scoped));
    let rows: Vec<&str> = gpu.lines().filter(|l| l.starts_with("client ")).collect();
    assert_eq!(
        rows,
        ["client pid=201 exe=cargo scope=tab tab=1 share_ppm=5000"]
    );
    assert!(
        gpu.contains("withheld=3") && !gpu.contains("Safari") && !gpu.contains("WindowServer"),
        "{gpu}"
    );
    let j = render_json(&query(View::Gpu, true), &scoped);
    let g = j.get("gpu").expect("gpu");
    assert_eq!(g.get("withheld"), Some(&Jv::u(3)));
    assert!(matches!(g.get("clients"), Some(Jv::Arr(rows)) if rows.len() == 1));
}

/// The net view prints the host's network place as it came: its words on a `place` line ahead
/// of the counters, and its fields as the top-level `place` object of the JSON (nested objects
/// kept), so `sys` cannot word the place apart from every other reporter. With no place the
/// text has no `place` line and the JSON says it was not sampled.
/// NEGATIVE CONTROL: dropping the `place` line from `net_text` fails the first assertion.
#[test]
fn sys_net_prints_the_hosts_network_place_as_it_came() {
    use crate::model::{PlaceValue, PlaceView};
    let w = world();
    let mut s = snapshot();
    let words = "words the host was handed";
    s.place = Some(PlaceView {
        line: words.to_owned(),
        fields: vec![
            ("state", PlaceValue::Text("remote".to_owned())),
            ("expensive", PlaceValue::Flag(true)),
            ("since_ms", PlaceValue::Count(42)),
            ("gateway", PlaceValue::Absent("not_sampled")),
            (
                "internet",
                PlaceValue::Object(vec![("state", PlaceValue::Text("path-up".to_owned()))]),
            ),
        ],
        kind: crate::model::PlaceKind::Hotspot,
    });
    let c = ctx(&w, Some(&s), true, true);
    let text = all_text(&render_text(&query(View::Net, false), &c));
    let place: Vec<&str> = text.lines().filter(|l| l.starts_with("place ")).collect();
    assert_eq!(place.len(), 1, "{text}");
    assert!(place[0].ends_with(words), "{text}");
    for view in [View::Net, View::Global] {
        let j = render_json(&query(view, true), &c);
        assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
        let place = j.get("place").expect("place");
        assert_eq!(place.get("state"), Some(&Jv::s("remote")));
        assert_eq!(place.get("expensive"), Some(&Jv::Bool(true)));
        assert_eq!(place.get("since_ms"), Some(&Jv::u(42)));
        assert_eq!(place.get("gateway"), Some(&Jv::why("not_sampled")));
        assert_eq!(
            place.get("internet").and_then(|i| i.get("state")),
            Some(&Jv::s("path-up"))
        );
    }
    let bare = snapshot();
    let c = ctx(&w, Some(&bare), true, true);
    let text = all_text(&render_text(&query(View::Net, false), &c));
    assert!(!text.lines().any(|l| l.starts_with("place ")), "{text}");
    let j = render_json(&query(View::Net, true), &c);
    assert_eq!(j.get("place"), Some(&Jv::why("not_sampled")));
}

/// A Linux reading with only the kernel's counters read (no all-states dump yet, or its
/// self-check retired it) gives every other TCP state as not sampled, in both shapes, not as 0.
/// NEGATIVE CONTROL: the whole reading in the test below gives them as numbers.
#[test]
fn sys_net_gives_tcp_states_only_a_dump_reads_as_not_sampled_before_one() {
    let mut s = snapshot();
    s.net.as_mut().expect("net view").tcp = Some(TcpCounts {
        established: 12,
        time_wait: 2,
        counters_only: true,
        ..TcpCounts::default()
    });
    let w = world();
    let c = ctx(&w, Some(&s), true, true);
    let text = all_text(&render_text(&query(View::Net, false), &c));
    assert!(
        text.contains(
            "tcp 12 established or close_wait, 2 time_wait or orphaned fin_wait2 (other states not_sampled)"
        ),
        "{text}"
    );
    assert!(
        text.contains("tcp     total=unavailable(not_sampled) established=\u{2264}12 listen=unavailable(not_sampled) time_wait=\u{2264}2 close_wait=unavailable(not_sampled)"),
        "{text}"
    );
    let overview = all_text(&render_text(&query(View::Overview, false), &c));
    assert!(
        overview.contains("12 TCP established or close-wait"),
        "{overview}"
    );
    let j = render_json(&query(View::Net, true), &c);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let tcp = j.get("net").and_then(|n| n.get("tcp")).expect("tcp");
    assert_eq!(
        tcp.get("established"),
        Some(&Jv::at_most(12, "close_wait_counted")),
        "the counter may hold close-wait too"
    );
    assert_eq!(
        tcp.get("time_wait"),
        Some(&Jv::at_most(2, "fin_wait2_counted")),
        "the counter holds orphaned fin-wait-2 too"
    );
    for key in [
        "total",
        "listen",
        "close_wait",
        "connecting",
        "closing_total",
    ] {
        assert_eq!(tcp.get(key), Some(&Jv::why("not_sampled")), "{key}");
    }
    let j = render_json(&query(View::Global, true), &c);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    assert_eq!(
        j.get("global").and_then(|g| g.get("tcp_established")),
        Some(&Jv::at_most(12, "close_wait_counted"))
    );
}

#[test]
fn sys_net_reports_links_rates_and_tcp_connections_in_both_shapes() {
    let s = snapshot();
    let w = world();
    let c = ctx(&w, Some(&s), true, true);
    let text = all_text(&render_text(&query(View::Net, false), &c));
    assert!(
        text.contains("net     down 2.0 MB/s (1500 pkt/s), up 500 KB/s (400 pkt/s) · err 0.0/s, drop 2.0/s · primary en0 (wifi), link 125.0 MB/s · tcp 20 total (12 established, 4 listen, 2 time_wait, 1 close_wait, 1 connecting)"),
        "{text}"
    );
    assert!(
        text.contains("tcp     total=20 established=12 listen=4 time_wait=2 close_wait=1 syn_sent=1 syn_recv=0 fin_wait1=0 fin_wait2=0 closing=0 last_ack=0 close=0"),
        "{text}"
    );
    assert!(
        text.contains("link name=en0 kind=wifi up=1 loopback=0 counted=1 primary=1 rx_bps=2000000 tx_bps=500000 rx_pps=1500 tx_pps=400 rx_err_ps=0 tx_err_ps=0 rx_drop_ps=2 tx_drop_ps=unavailable(unsupported) rx_bytes=2000000 tx_bytes=500000 byte_counter=cumulative64 byte_counter_bits=64 byte_counter_quantum_bytes=1 rx_packets=1500 tx_packets=400 rx_errors=0 tx_errors=0 rx_drops=2 tx_drops=unavailable(unsupported) link_bps=1000000000"),
        "{text}"
    );
    assert!(
        text.contains("link name=lo0 kind=other up=1 loopback=1 counted=0 primary=0 rx_bps=100000 tx_bps=100000"),
        "{text}"
    );

    let overview = all_text(&render_text(&query(View::Overview, false), &c));
    assert!(
        overview.contains("net down 2.0 MB/s, up 500 KB/s, 20 TCP (12 established, 4 listen)"),
        "{overview}"
    );
    let global = all_text(&render_text(&query(View::Global, false), &c));
    assert!(
        global.contains("net      down 2.0 MB/s (1500 pkt/s), up 500 KB/s (400 pkt/s)"),
        "{global}"
    );

    let j = render_json(&query(View::Net, true), &c);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let net = j.get("net").expect("net");
    assert_eq!(net.get("primary_link"), Some(&Jv::s("en0")));
    assert_eq!(
        net.get("per_s").and_then(|p| p.get("rx_bytes_per_s")),
        Some(&Jv::u(2_000_000))
    );
    assert_eq!(
        net.get("per_s").and_then(|p| p.get("tx_bytes_per_s")),
        Some(&Jv::u(500_000))
    );
    assert_eq!(
        net.get("per_s").and_then(|p| p.get("rx_drops_per_s")),
        Some(&Jv::u(2))
    );
    let tcp = net.get("tcp").expect("tcp");
    assert_eq!(tcp.get("total"), Some(&Jv::u(20)));
    assert_eq!(tcp.get("established"), Some(&Jv::u(12)));
    assert_eq!(tcp.get("listen"), Some(&Jv::u(4)));
    assert_eq!(tcp.get("time_wait"), Some(&Jv::u(2)));
    assert_eq!(tcp.get("close_wait"), Some(&Jv::u(1)));
    assert_eq!(tcp.get("connecting"), Some(&Jv::u(1)));
    assert_eq!(tcp.get("closing_total"), Some(&Jv::u(3)));
    assert_eq!(net.get("state"), Some(&Jv::s("degraded")));
    assert_eq!(net.get("srtt_us"), Some(&Jv::why("not_sampled")));
    assert_eq!(net.get("aterm_sockets"), Some(&Jv::why("not_sampled")));

    let mut with_rtt = s.clone();
    let nv = with_rtt.net.as_mut().expect("net view");
    nv.state = crate::NetState::Online;
    nv.srtt_ms = Some(14.2);
    nv.aterm_sockets = Some(18);
    let c_rtt = ctx(&w, Some(&with_rtt), true, true);
    let j_rtt = render_json(&query(View::Net, true), &c_rtt);
    assert_eq!(
        j_rtt.get("net").and_then(|n| n.get("state")),
        Some(&Jv::s("online"))
    );
    let text_rtt = all_text(&render_text(&query(View::Net, false), &c_rtt));
    assert!(
        text_rtt.contains("rtt 14.2 ms · aterm 18 sockets") && text_rtt.contains("srtt_us=14200"),
        "{text_rtt}"
    );
    let j_rtt = render_json(&query(View::Net, true), &c_rtt);
    assert!(
        contract_violations(&j_rtt).is_empty(),
        "{}",
        j_rtt.to_text()
    );
    let net_rtt = j_rtt.get("net").expect("net");
    assert_eq!(net_rtt.get("srtt_us"), Some(&Jv::u(14_200)));
    assert_eq!(net_rtt.get("aterm_sockets"), Some(&Jv::u(18)));
}

/// Seventy one-second sweeps of [`sample`]: past the minute a program's hour needs.
fn hour_snapshot() -> Snapshot {
    let mut v = Vitals::new();
    let mut s = Snapshot::default();
    for k in 0..=70 {
        s = v.absorb(&sample(10 + k, &[0, 0, 0, 0], k));
    }
    s
}

#[test]
fn sys_disk_names_each_programs_hour_with_its_coverage() {
    let s = hour_snapshot();
    let w = world();
    let owner = ctx(&w, Some(&s), true, true);
    let q = SysQuery::parse("disk scope=global").unwrap();
    let text = all_text(&render_text(&q, &owner));
    let programs: Vec<&str> = text.lines().filter(|l| l.starts_with("program ")).collect();
    assert_eq!(
        programs,
        [
            "program exe=cargo n=1 scope=tab tab=1 write_bytes_per_h=3240000000 covered_s=70",
            "program exe=Safari n=1 scope=others write_bytes_per_h=1080000000 covered_s=70",
            "program exe=- n=1 scope=tab tab=1 write_bytes_per_h=1080000000 covered_s=70",
            "program exe=aterm n=1 scope=aterm write_bytes_per_h=720000000 covered_s=70",
            "program exe=aterm-helper n=1 scope=aterm write_bytes_per_h=180000000 covered_s=70",
        ],
        "{text}"
    );
    let mut jq = q.clone();
    jq.json = true;
    let j = render_json(&jq, &owner);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let first = match j.get("disk").and_then(|d| d.get("programs")) {
        Some(Jv::Arr(rows)) => rows[0].clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(first.get("exe"), Some(&Jv::s("cargo")));
    assert_eq!(first.get("tab"), Some(&Jv::u(1)));
    assert_eq!(first.get("write_bytes_per_h"), Some(&Jv::u(3_240_000_000)));
    assert_eq!(first.get("covered_s"), Some(&Jv::u(70)));

    // NEGATIVE CONTROL: one second read is no rate: the figure is unavailable, never a zero.
    let one = snapshot();
    let c = ctx(&w, Some(&one), true, true);
    let text = all_text(&render_text(&q, &c));
    assert!(
        text.contains(
            "program exe=cargo n=1 scope=tab tab=1 write_bytes_per_h=unavailable(warming) covered_s=1"
        ),
        "{text}"
    );
    let j = render_json(&jq, &c);
    let first = match j.get("disk").and_then(|d| d.get("programs")) {
        Some(Jv::Arr(rows)) => rows[0].clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(first.get("write_bytes_per_h"), Some(&Jv::why("warming")));
}

/// A FIGURE AT ITS MINUTE: the covered seconds print rounded down, so a program still warming
/// never reads `covered_s=60` (a live instance showed 59.6 s printed as 60 beside `warming`).
/// NEGATIVE CONTROL: at the minute itself the rate is given.
#[test]
fn covered_seconds_never_claim_the_minute_a_warming_figure_lacks() {
    let mut s = hour_snapshot();
    let w = world();
    let q = SysQuery::parse("disk scope=global").unwrap();
    let first_program = |s: &Snapshot| -> String {
        let owner = ctx(&w, Some(s), true, true);
        let text = all_text(&render_text(&q, &owner));
        text.lines()
            .find(|l| l.starts_with("program "))
            .expect("a program row")
            .to_owned()
    };
    s.facts.writes_covered_s = 59.6;
    assert_eq!(
        first_program(&s),
        "program exe=cargo n=1 scope=tab tab=1 write_bytes_per_h=unavailable(warming) covered_s=59"
    );
    s.facts.writes_covered_s = 60.0;
    assert_eq!(
        first_program(&s),
        "program exe=cargo n=1 scope=tab tab=1 write_bytes_per_h=3780000000 covered_s=60"
    );
}

#[test]
fn a_tab_scoped_caller_sees_its_own_tabs_programs_and_a_withheld_count() {
    let mut s = hour_snapshot();
    s.facts.hour_writers.insert(
        0,
        crate::writes::ProgramHour {
            name: "foreign-writer".into(),
            scope: crate::model::ProcScope::Tab(8),
            bytes: 1 << 30,
            n_max: 3,
        },
    );
    let mut w = world();
    w.target.window_sessions.push(8);
    let q = SysQuery::parse("disk scope=window").unwrap();
    let scoped = ctx(&w, Some(&s), false, true);
    let owner = ctx(&w, Some(&s), true, true);
    let text = all_text(&render_text(&q, &scoped));
    assert!(
        text.contains("program exe=cargo n=1 scope=tab tab=1 "),
        "{text}"
    );
    assert!(!text.contains("foreign-writer"), "{text}");
    assert!(!text.contains("exe=Safari"), "outside the window: {text}");
    assert!(
        text.contains("withheld=1 (program rows outside this tab are the owner's)"),
        "{text}"
    );
    let owner_text = all_text(&render_text(&q, &owner));
    assert!(
        owner_text.contains("program exe=foreign-writer n=3 scope=tab local=8 "),
        "{owner_text}"
    );
    assert!(!owner_text.contains("program rows outside"), "{owner_text}");
    let mut jq = q.clone();
    jq.json = true;
    let j = render_json(&jq, &scoped);
    let disk = j.get("disk").unwrap();
    assert_eq!(disk.get("programs_withheld"), Some(&Jv::u(1)));
    let exes = |disk: &Jv| match disk.get("programs") {
        Some(Jv::Arr(rows)) => rows
            .iter()
            .filter_map(|r| match r.get("exe") {
                Some(Jv::Str(e)) => Some(e.clone()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    assert_eq!(exes(disk), ["cargo", "-"]);
    let owner_j = render_json(&jq, &owner);
    let owner_disk = owner_j.get("disk").unwrap();
    assert_eq!(exes(owner_disk), ["foreign-writer", "cargo", "-"]);
    assert_eq!(owner_disk.get("programs_withheld"), None);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
}

/// A PANE READS ITS TAB'S PROGRAMS: the hour is kept per tab, under the tab's least pane's key,
/// so a caller in the second pane of a split tab asking for its pane gets the tab's programs,
/// not none, and nothing is withheld from it. NEGATIVE CONTROL: a pane of another tab gets none.
#[test]
fn a_pane_of_a_split_tab_reads_its_tabs_programs() {
    let s = hour_snapshot();
    let mut w = world();
    w.target.key = 8;
    w.target.panes = vec![7, 8];
    w.target.window_sessions = vec![7, 8];
    let q = SysQuery::parse("disk scope=pane").unwrap();
    for owner in [true, false] {
        let text = all_text(&render_text(&q, &ctx(&w, Some(&s), owner, true)));
        assert!(
            text.contains("program exe=cargo n=1 scope=tab tab=1 "),
            "owner={owner}: {text}"
        );
        assert!(
            !text.contains("program rows outside"),
            "owner={owner}: {text}"
        );
    }
    let mut apart = world();
    apart.target.key = 9;
    apart.target.panes = vec![9];
    apart.target.window_sessions = vec![9];
    let text = all_text(&render_text(&q, &ctx(&apart, Some(&s), true, true)));
    assert!(!text.contains("program exe=cargo"), "{text}");
}

/// A3 L4 (D7): `sys` says a GPU uses unified system memory where its reader says it works in the
/// system's memory (`GpuSample::unified`), as NVML says of a GB10, not by its name. NEGATIVE
/// CONTROL: an Apple-named GPU whose reading carries no such fact says nothing of it.
#[test]
fn unified_memory_words_key_on_the_readers_fact_not_the_platform() {
    let w = world();
    let said = |name: &str, unified: bool| {
        let mut s = snapshot();
        let g = s.facts.gpu.as_mut().expect("fixture GPU");
        g.name = Some(name.into());
        g.unified = unified;
        all_text(&render_text(
            &query(View::Gpu, false),
            &ctx(&w, Some(&s), true, true),
        ))
    };
    for (name, unified) in [
        ("NVIDIA GB10", true),
        ("Apple M5", true),
        ("Apple M5", false),
    ] {
        let text = said(name, unified);
        assert!(text.contains(name), "{text}");
        assert_eq!(
            text.contains("(unified system memory)"),
            unified,
            "{name} {unified}: {text}"
        );
    }
}

#[test]
fn the_gpu_view_names_the_device_and_core_count_once() {
    let s = snapshot();
    let w = world();
    let c = ctx(&w, Some(&s), true, true);
    let reply = render_text(&query(View::Gpu, false), &c);
    let gpu = &reply
        .lines
        .iter()
        .find(|line| line.key == "gpu")
        .expect("GPU line")
        .text;
    assert_eq!(gpu.matches("Apple M5").count(), 1, "{gpu}");
    assert_eq!(gpu.matches("10 cores").count(), 1, "{gpu}");
}

/// An exact OS level prints the OS's own word everywhere `sys` names the thermal state (the
/// global clause, the `thermal` line, `sys cores`, every JSON `thermal` key, an event, the strip
/// label), and "throttled" is left to the counter heuristic, which counts throttle events. The
/// `throttle` key is retired. Control: the code before A2 S3 printed "throttled: light" at Fair
/// and kept the `throttle` key.
#[test]
fn sys_prints_the_os_thermal_word_and_reserves_throttled_for_counted_throttling() {
    let fair = Thermal {
        level: crate::model::Throttle::Light,
        raw: 1,
        low_power: Some(false),
        exact: true,
    };
    let mut s = snapshot();
    s.thermal = Some(fair);
    let mut w = world();
    w.events = vec![Event {
        seq: 2,
        at_ns: 11_000_000_000,
        kind: EventKind::Thermal {
            from: Thermal {
                level: crate::model::Throttle::None,
                raw: 0,
                ..fair
            },
            to: fair,
        },
    }];
    let c = ctx(&w, Some(&s), true, true);
    let overview = all_text(&render_text(&query(View::Overview, false), &c));
    assert!(overview.contains("; thermal fair; "), "{overview}");
    assert!(
        overview.contains(" kind=thermal from=nominal to=fair"),
        "{overview}"
    );
    let global = render_text(&query(View::Global, false), &c);
    let line = |r: &Reply, key: &str| {
        r.lines
            .iter()
            .find(|l| l.text.starts_with(key))
            .map(|l| l.text.clone())
    };
    assert_eq!(
        line(&global, "thermal").as_deref(),
        Some("thermal  fair (OS level 1) · Low Power Mode off")
    );
    let cores = render_text(&query(View::Cores, false), &c);
    assert_eq!(
        line(&cores, "thermal").as_deref(),
        Some("thermal fair (OS level 1) · Low Power Mode off")
    );
    for r in [&global, &cores] {
        assert!(
            !all_text(r).contains("throttle"),
            "Fair claims no throttling: {}",
            all_text(r)
        );
    }
    assert!(!overview.contains("throttle"), "{overview}");

    let j = render_json(&query(View::Global, true), &c);
    assert_eq!(j.get("throttle"), None);
    assert_eq!(
        j.get("thermal"),
        Some(&Jv::obj([
            ("word", Jv::s("fair")),
            ("raw", Jv::u(1)),
            ("exact", Jv::Bool(true)),
            ("low_power", Jv::Bool(false)),
        ]))
    );
    let g = j.get("global").expect("global");
    assert_eq!(
        (g.get("thermal"), g.get("throttle")),
        (Some(&Jv::s("fair")), None)
    );
    let j = render_json(&query(View::Cores, true), &c);
    assert_eq!(
        (j.get("thermal"), j.get("throttle")),
        (Some(&Jv::s("fair")), None)
    );
    let j = render_json(&query(View::Overview, true), &c);
    let e = match j.get("events") {
        Some(Jv::Arr(e)) => e[0].clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        (e.get("kind"), e.get("from"), e.get("to")),
        (
            Some(&Jv::s("thermal")),
            Some(&Jv::s("nominal")),
            Some(&Jv::s("fair"))
        )
    );

    let counted = Thermal {
        level: crate::model::Throttle::Light,
        raw: 1,
        low_power: None,
        exact: false,
    };
    s.thermal = Some(counted);
    let c = ctx(&w, Some(&s), true, true);
    let overview = all_text(&render_text(&query(View::Overview, false), &c));
    assert!(overview.contains("; throttled: light; "), "{overview}");
    let global = render_text(&query(View::Global, false), &c);
    assert!(
        line(&global, "thermal")
            .is_some_and(|l| l.starts_with("thermal  throttled: light (estimate: ")),
        "{}",
        all_text(&global)
    );
    let j = render_json(&query(View::Global, true), &c);
    assert_eq!(
        j.get("thermal"),
        Some(&Jv::obj([
            ("word", Jv::s("light")),
            ("raw", Jv::u(1)),
            ("exact", Jv::Bool(false)),
            ("low_power", Jv::why("unsupported")),
        ]))
    );

    let mut v = Vitals::new();
    let _ = v.absorb(&sample(10, &[0, 0, 0, 0], 0));
    let mut hot = sample(11, &[20, 30, 100, 90], 1);
    hot.thermal = Some(Thermal {
        level: crate::model::Throttle::Severe,
        raw: 3,
        ..fair
    });
    let labels: Vec<String> = v
        .absorb(&hot)
        .strip_events
        .into_iter()
        .map(|e| e.label)
        .collect();
    assert_eq!(labels, ["thermal critical"]);
}

/// `scope=request` reads the request's tabs, says whose they are, and states its totals with
/// each unread figure `unavailable`, never 0. NEGATIVE CONTROL: `scope=tab` has no request line.
#[test]
fn scope_request_names_its_request_and_its_unread_totals_are_unavailable() {
    let s = snapshot();
    let mut w = world();
    w.target.request = Some(crate::sys::RequestFacts {
        id: "r-20261111-1200-87be5228-01".into(),
        sessions: vec![7, 8],
        tabs: 2,
        cpu_s: Some(12),
        peak_mb: None,
        written_mb: Some(3),
        claims: 1,
        claimed_mb: None,
    });
    let c = ctx(&w, Some(&s), true, true);
    for view in ["procs", "jobs", "disk"] {
        let reply = render_text(
            &SysQuery::parse(&format!("{view} scope=request")).unwrap(),
            &c,
        );
        let text = all_text(&reply);
        assert!(
            text.contains(
                "request=r-20261111-1200-87be5228-01 tabs=2 cpu_s=12 peak_mb=unavailable(not_sampled) \
                 disk_written_mb=3 claims=1 claimed_mb=unavailable(not_sampled)"
            ),
            "{view}: {text}"
        );
        assert!(
            text.contains("request r-20261111-1200-87be5228-01 (2 tabs)"),
            "{view}: {text}"
        );
        let json = render_json(
            &SysQuery::parse(&format!("{view} scope=request --json")).unwrap(),
            &c,
        );
        assert_eq!(json.get("scope"), Some(&Jv::s("request")), "{view}");
        let request = json.get("request").expect("the request's totals");
        assert_eq!(request.get("cpu_s"), Some(&Jv::u(12)));
        assert_eq!(request.get("peak_mb"), Some(&Jv::why("not_sampled")));
        let v = contract_violations(&json);
        assert!(v.is_empty(), "{view}: {v:?}");
        let tab = all_text(&render_text(
            &SysQuery::parse(&format!("{view} scope=tab")).unwrap(),
            &c,
        ));
        assert!(!tab.contains("request="), "{view}: {tab}");
    }
    assert_eq!(scope_keys(&c, ScopeArg::Request), vec![7, 8]);
    // A target no request owns reads its own tab: the host refuses the scope before this.
    w.target.request = None;
    let c = ctx(&w, Some(&s), true, true);
    assert_eq!(scope_keys(&c, ScopeArg::Request), vec![7]);
}

/// `sys disk` with no walker's sizes says what each platform measures: on Windows that the
/// walker has measured nothing yet and that no process's disk writes are read; on macOS (and
/// Linux) the sentence it always said. NEGATIVE CONTROL: the macOS reading never says the
/// Windows sentence, nor the Windows one that writers are measured.
#[test]
fn an_unmeasured_disk_says_what_its_platform_measures() {
    let s = snapshot();
    let mut w = world();
    let mac = all_text(&render_text(
        &query(View::Disk, false),
        &ctx(&w, Some(&s), true, true),
    ));
    assert!(
        mac.contains(
            "are unavailable(not_sampled) here (no disk walker measures them); free space and \
             writers are measured"
        ),
        "{mac}"
    );
    assert!(!mac.contains("disk writes are not"), "{mac}");
    w.machine.family = "Windows";
    let win = all_text(&render_text(
        &query(View::Disk, false),
        &ctx(&w, Some(&s), true, true),
    ));
    assert!(
        win.contains(
            "are unavailable(not_sampled) here (the disk walker has measured none yet; a \
             headless instance starts it at the first ask); free space is measured, each \
             process's disk writes are not"
        ),
        "{win}"
    );
    assert!(!win.contains("writers are measured"), "{win}");
}

/// The process sweep's lane as a reading found it stalled: in `proc_pid_rusage` of the held
/// `aterm-verify` (its name the last answered sweep's) since 21:18:20.000, 3m12s before.
fn procs_stall() -> crate::lanes::LaneStall {
    crate::lanes::LaneStall {
        lane: crate::lanes::PROCS,
        call: "proc_pid_rusage",
        pid: Some(4321),
        name: Some("aterm-verify".into()),
        since_unix_ms: 1_792_185_500_000,
        for_ms: 192_000,
    }
}

/// [`snapshot`]'s model one reading on, whose sweep stalled: the machine's counters read, the
/// processes not.
fn stalled_snapshot() -> Snapshot {
    let mut v = Vitals::new();
    let _ = v.absorb(&sample(10, &[0, 0, 0, 0], 0));
    let _ = v.absorb(&sample(11, &[20, 30, 100, 90], 1));
    let mut stalled = sample(12, &[40, 60, 200, 180], 2);
    stalled.procs = None;
    stalled.stalled = vec![procs_stall()];
    v.absorb(&stalled)
}

/// A stalled sweep's reading keeps the machine's figures, and every view names the read it is
/// held in and since when, its own figures unavailable(stalled), never warming or zero; a reading
/// whose lanes answered prints no stall.
#[test]
fn a_stalled_lane_prints_its_read_and_since_in_overview_procs_why_and_jobs() {
    let s = stalled_snapshot();
    assert!(s.cpu.is_some() && s.mem.is_some() && s.volume.is_some());
    assert!(s.aterm.is_none(), "no sweep, no aterm figures");
    let w = world();
    let mut c = ctx(&w, Some(&s), true, true);
    c.state = Freshness::Partial;
    let head = "stalled procs: proc_pid_rusage pid=4321 (aterm-verify) since 21:18:20.000 for \
                3m12s · figures aterm,tab,procs,wait,writes,split,top,why,jobs,left";
    for view in [
        View::Overview,
        View::Procs,
        View::Why,
        View::Jobs,
        View::Tab,
    ] {
        let reply = render_text(&query(view, false), &c);
        let text = all_text(&reply);
        assert!(text.contains(head), "{view:?}: {text}");
        let header = reply.header(&c, false);
        assert!(
            header.contains(" state=partial stalled=procs"),
            "{view:?}: {header}"
        );
    }
    let overview = all_text(&render_text(&query(View::Overview, false), &c));
    assert!(
        overview.contains("aterm   unavailable(stalled)"),
        "{overview}"
    );
    assert!(
        overview.contains("tab 1   unavailable(stalled)"),
        "{overview}"
    );
    assert!(
        overview.contains("memory "),
        "the machine's figures stay: {overview}"
    );
    assert!(!overview.contains("warming"), "{overview}");
    assert!(!overview.contains("measuring"), "{overview}");
    let procs = all_text(&render_text(&query(View::Procs, false), &c));
    assert!(
        procs.contains("procs  tab 1 · unavailable(stalled): the process sweep did not answer"),
        "{procs}"
    );
    let jobs = all_text(&render_text(&query(View::Jobs, false), &c));
    assert!(jobs.contains("unavailable(stalled)"), "{jobs}");
    // The negative control: a reading whose lanes answered names no stall.
    let fresh = snapshot();
    let c = ctx(&w, Some(&fresh), true, true);
    for view in View::ALL {
        let reply = render_text(&query(view, false), &c);
        assert!(!all_text(&reply).contains("stalled"), "{view:?}");
        assert!(!reply.header(&c, false).contains("stalled"), "{view:?}");
    }
}

/// `--json` lists each stalled lane with its read, pid, name and since, and each figure the
/// lane leaves unknown is `{"why":"stalled","lane":…}`; the contract holds both, and a reading
/// whose lanes answered carries no `lanes`.
#[test]
fn sys_json_carries_each_stalled_lane_and_marks_its_figures() {
    let s = stalled_snapshot();
    let w = world();
    let mut c = ctx(&w, Some(&s), true, true);
    c.state = Freshness::Partial;
    for view in View::ALL {
        let j = render_json(&query(view, true), &c);
        let v = contract_violations(&j);
        assert!(v.is_empty(), "{view:?}: {v:?}\n{}", j.to_text());
        let Some(Jv::Arr(lanes)) = j.get("lanes") else {
            panic!("{view:?}: no lanes: {}", j.to_text());
        };
        assert_eq!(lanes.len(), 1);
        let lane = &lanes[0];
        assert_eq!(lane.get("lane"), Some(&Jv::s("procs")));
        assert_eq!(lane.get("state"), Some(&Jv::s("stalled")));
        assert_eq!(lane.get("call"), Some(&Jv::s("proc_pid_rusage")));
        assert_eq!(lane.get("pid"), Some(&Jv::u(4321)));
        assert_eq!(lane.get("name"), Some(&Jv::s("aterm-verify")));
        assert_eq!(lane.get("since"), Some(&Jv::s("21:18:20.000")));
        assert_eq!(lane.get("for_ms"), Some(&Jv::u(192_000)));
    }
    let overview = render_json(&query(View::Overview, true), &c);
    assert_eq!(
        overview.get("aterm"),
        Some(&Jv::stalled("procs")),
        "{}",
        overview.to_text()
    );
    assert_eq!(
        overview.get("tab").and_then(|t| t.get("cpu_mc")),
        Some(&Jv::stalled("procs")),
        "{}",
        overview.to_text()
    );
    assert_eq!(overview.get("reported_cpu_mc"), Some(&Jv::stalled("procs")));
    assert!(overview.get("split_mc").is_none());
    let procs = render_json(&query(View::Procs, true), &c);
    assert_eq!(procs.get("procs"), Some(&Jv::stalled("procs")));
    let memory = render_json(&query(View::Mem, true), &c);
    assert_eq!(
        memory.get("owners"),
        Some(&Jv::stalled("procs")),
        "missing memory-owner evidence keeps the stalled sweep reason: {}",
        memory.to_text()
    );
    // The negative control.
    let fresh = snapshot();
    let c = ctx(&w, Some(&fresh), true, true);
    for view in View::ALL {
        assert!(
            render_json(&query(view, true), &c).get("lanes").is_none(),
            "{view:?}"
        );
    }
}

/// [`stalled_snapshot`]'s readings with the third sweep answered: the same machine, its
/// processes read.
fn answered_snapshot() -> Snapshot {
    let mut v = Vitals::new();
    let _ = v.absorb(&sample(10, &[0, 0, 0, 0], 0));
    let _ = v.absorb(&sample(11, &[20, 30, 100, 90], 1));
    let mut s = v.absorb(&sample(12, &[40, 60, 200, 180], 2));
    s.set_left(Some(std::sync::Arc::new(
        crate::sys::left::tests::view_fixture(),
    )));
    s
}

/// The words an unknown is read by that are not `stalled`.
const UNSTALLED_UNKNOWNS: [&str; 3] = ["warming", "not_sampled", "measuring"];

/// Every `{"why": w}` under `v` with `w` one of [`UNSTALLED_UNKNOWNS`], by path.
fn unstalled_unknowns(v: &Jv, path: &str, out: &mut Vec<(String, String)>) {
    match v {
        Jv::Obj(pairs) => {
            if let Some(Jv::Str(w)) = v.get("why")
                && UNSTALLED_UNKNOWNS.contains(&w.as_str())
            {
                out.push((path.to_owned(), w.clone()));
            }
            for (k, x) in pairs {
                unstalled_unknowns(x, &format!("{path}.{k}"), out);
            }
        }
        Jv::Arr(xs) => {
            for (i, x) in xs.iter().enumerate() {
                unstalled_unknowns(x, &format!("{path}[{i}]"), out);
            }
        }
        Jv::Int(_) | Jv::Str(_) | Jv::Bool(_) => {}
    }
}

/// A stalled sweep leaves no figure warming, not sampled or measuring that the same reading
/// with its sweep answered reads: every figure the stall leaves unknown says it stalled, in
/// every view's text and JSON. The answered reading is the control.
#[test]
fn a_stall_leaves_no_figure_it_feeds_warming_or_unsampled() {
    let stalled = stalled_snapshot();
    let answered = answered_snapshot();
    let w = world();
    let mut c = ctx(&w, Some(&stalled), true, true);
    c.state = Freshness::Partial;
    let a = ctx(&w, Some(&answered), true, true);
    let mut wrong = Vec::new();
    for view in View::ALL {
        let held = render_text(&query(view, false), &c);
        let read = render_text(&query(view, false), &a);
        for l in &held.lines {
            let twin = read.lines.iter().find(|r| r.key == l.key);
            for word in UNSTALLED_UNKNOWNS {
                let n = l.text.matches(word).count();
                let m = twin.map_or(0, |r| r.text.matches(word).count());
                if n > m {
                    wrong.push(format!("{view:?} text {}: {}", l.key, l.text));
                }
            }
        }
        let mut held_json = Vec::new();
        unstalled_unknowns(&render_json(&query(view, true), &c), "", &mut held_json);
        let mut read_json = Vec::new();
        unstalled_unknowns(&render_json(&query(view, true), &a), "", &mut read_json);
        for u in held_json {
            if !read_json.contains(&u) {
                wrong.push(format!("{view:?} json {}: {}", u.0, u.1));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    let global = render_json(&query(View::Global, true), &c);
    let g = global.get("global").expect("global");
    for key in ["procs", "wait_mc", "disk_write_bytes_per_s"] {
        assert_eq!(
            g.get(key),
            Some(&Jv::stalled("procs")),
            "{key}: {}",
            global.to_text()
        );
    }
    assert_eq!(
        global.get("top"),
        Some(&Jv::stalled("procs")),
        "{}",
        global.to_text()
    );
    let text = all_text(&render_text(&query(View::Global, false), &c));
    assert!(text.contains("top      unavailable(stalled)"), "{text}");
    let aterm = all_text(&render_text(&query(View::Aterm, false), &c));
    assert!(aterm.contains("cpu     unavailable(stalled)"), "{aterm}");
    let aterm = render_json(&query(View::Aterm, true), &c);
    assert_eq!(aterm.get("parts"), Some(&Jv::stalled("procs")));
    let mem = all_text(&render_text(&query(View::Why, false), &c));
    assert!(mem.contains("role=unavailable(stalled)"), "{mem}");
    // A sweep stalled before any answered: why's split has no account to carry, and says the
    // sweep stalled rather than warming.
    let mut v = Vitals::new();
    let _ = v.absorb(&sample(10, &[0, 0, 0, 0], 0));
    let mut first = sample(11, &[20, 30, 100, 90], 1);
    first.procs = None;
    first.stalled = vec![procs_stall()];
    let cold = v.absorb(&first);
    let mut c = ctx(&w, Some(&cold), true, true);
    c.state = Freshness::Partial;
    let mut cpu = query(View::Why, false);
    cpu.subject = Some(crate::explain::WhySubject::Cpu);
    let why = all_text(&render_text(&cpu, &c));
    assert!(why.contains("reported_cpu unavailable(stalled)"), "{why}");
    assert!(why.contains("role=unavailable(stalled)"), "{why}");
    assert!(!why.contains("warming"), "{why}");
    cpu.json = true;
    let why = render_json(&cpu, &c).to_text();
    assert!(why.contains(r#"{"why":"stalled","lane":"procs"}"#), "{why}");
    assert!(!why.contains("warming"), "{why}");
}

/// Every `age_ms` under `v`.
fn ages(v: &Jv, out: &mut Vec<Jv>) {
    match v {
        Jv::Obj(pairs) => {
            for (k, x) in pairs {
                if k == "age_ms" {
                    out.push(x.clone());
                } else {
                    ages(x, out);
                }
            }
        }
        Jv::Arr(xs) => xs.iter().for_each(|x| ages(x, out)),
        Jv::Int(_) | Jv::Str(_) | Jv::Bool(_) => {}
    }
}

/// While the process table's lane is stalled, an age no birth was read for says so, in text and
/// JSON; the same reading with the table answering reads it unsampled.
#[test]
fn a_stalled_table_leaves_ages_stalled_not_unsampled() {
    let w = world();
    let answered = answered_snapshot();
    let mut held = answered_snapshot();
    held.stalled = vec![crate::lanes::LaneStall {
        lane: crate::lanes::KPROC_TABLE,
        call: "sysctl KERN_PROC_ALL",
        pid: None,
        name: None,
        since_unix_ms: 1_792_185_500_000,
        for_ms: 1_500,
    }];
    let mut c = ctx(&w, Some(&held), true, true);
    c.state = Freshness::Partial;
    let mut got = Vec::new();
    ages(&render_json(&query(View::Procs, true), &c), &mut got);
    let stalled = Jv::stalled(crate::lanes::KPROC_TABLE);
    assert!(got.contains(&stalled), "{got:?}");
    assert!(
        got.iter().all(|a| *a == stalled || matches!(a, Jv::Int(_))),
        "{got:?}"
    );
    let text = all_text(&render_text(&query(View::Procs, false), &c));
    assert!(text.contains("age_ms=unavailable(stalled)"), "{text}");
    assert!(text.contains("figures ages,reaping"), "{text}");
    let a = ctx(&w, Some(&answered), true, true);
    let mut got = Vec::new();
    ages(&render_json(&query(View::Procs, true), &a), &mut got);
    assert!(got.contains(&Jv::why("not_sampled")), "{got:?}");
    assert!(
        got.iter()
            .all(|a| *a == Jv::why("not_sampled") || matches!(a, Jv::Int(_))),
        "{got:?}"
    );
}

/// SYS DISK PRINTS THE REBUILDABLE BUDGET AS ONE RECORD OF ITS OWN, IN TEXT AND IN JSON, AND
/// THE VOLUME'S RECORD DOES NOT MOVE. A volume 313 GiB over the target its 466 GiB cap leaves
/// prints the cap, where it came from, the kept bytes, the target, the rebuildable bytes, `over_bytes`
/// and the sizing's age, while its `volume` line is byte for byte the line of the same volume
/// with no sizing and still says `level=ok`. One of `over_bytes` and `under_bytes` prints,
/// never both. CONTROLS: a volume the watch does not size prints `unavailable(not_sampled)`,
/// one it has not sized yet `unavailable(warming)`, a budget turned off
/// `unavailable(not_enabled:disk.cap_used_gib)`.
#[test]
fn sys_disk_prints_the_budget_record_in_text_and_json() {
    const GIB: u64 = 1 << 30;
    fn budget_lines(w: &World, s: &Snapshot) -> Vec<String> {
        render_text(&query(View::Disk, false), &ctx(w, Some(s), true, true))
            .lines
            .iter()
            .filter(|l| l.key == "budget")
            .map(|l| l.text.clone())
            .collect()
    }
    fn volume_line(w: &World, s: &Snapshot) -> String {
        render_text(&query(View::Disk, false), &ctx(w, Some(s), true, true))
            .lines
            .iter()
            .find(|l| l.key == "volume")
            .map(|l| l.text.clone())
            .expect("a volume line")
    }
    fn disk_reply(w: &World, s: &Snapshot) -> Jv {
        let j = render_json(&query(View::Disk, true), &ctx(w, Some(s), true, true));
        let v = contract_violations(&j);
        assert!(v.is_empty(), "{v:?}\n{}", j.to_text());
        j.get("disk").cloned().expect("disk")
    }
    let s = snapshot();
    let mut w = world();

    // CONTROL: nothing sized.
    assert_eq!(w.volume.budget, BudgetRead::NotSampled);
    let plain_volume = volume_line(&w, &s);
    assert!(plain_volume.contains(" level=ok "), "{plain_volume}");
    assert_eq!(
        budget_lines(&w, &s),
        ["budget unavailable(not_sampled)"],
        "one budget line, and no figure before a sizing"
    );
    let plain = disk_reply(&w, &s);
    assert_eq!(plain.get("budget"), Some(&Jv::why("not_sampled")));

    // Over: 313 GiB past the target.
    w.volume.budget = BudgetRead::Sized(BudgetFacts {
        cap: 466 * GIB,
        cap_written: true,
        kept: 398 * GIB,
        target: 68 * GIB,
        rebuildable: 381 * GIB,
        partial: false,
        sized_s: 1800,
    });
    assert_eq!(
        budget_lines(&w, &s),
        [format!(
            "budget cap_bytes={} cap_from=aterm.toml kept_bytes={} target_bytes={} \
             rebuildable_bytes={} over_bytes={} walk=complete sized_s=1800",
            466 * GIB,
            398 * GIB,
            68 * GIB,
            381 * GIB,
            313 * GIB
        )]
    );
    assert_eq!(
        volume_line(&w, &s),
        plain_volume,
        "the budget moves nothing in the volume's record"
    );
    let over = disk_reply(&w, &s);
    assert_eq!(
        over.get("budget"),
        Some(&Jv::obj([
            ("cap_bytes", Jv::u(466 * GIB)),
            ("cap_from", Jv::s("aterm.toml")),
            ("kept_bytes", Jv::u(398 * GIB)),
            ("target_bytes", Jv::u(68 * GIB)),
            ("rebuildable_bytes", Jv::u(381 * GIB)),
            ("over_bytes", Jv::u(313 * GIB)),
            ("walk", Jv::s("complete")),
            ("sized_age_s", Jv::u(1800)),
        ]))
    );
    assert_eq!(
        over.get("budget").and_then(|b| b.get("under_bytes")),
        None,
        "over or under, never both"
    );
    assert_eq!(over.get("volume"), plain.get("volume"));
    assert_eq!(
        over.get("volume").and_then(|v| v.get("level")),
        Some(&Jv::s("ok"))
    );

    // Under, from a sizing that was cut short, against the volume's own share.
    w.volume.budget = BudgetRead::Sized(BudgetFacts {
        cap: 466 * GIB,
        cap_written: false,
        kept: 342 * GIB,
        target: 124 * GIB,
        rebuildable: 24 * GIB,
        partial: true,
        sized_s: 7,
    });
    assert_eq!(
        budget_lines(&w, &s),
        [format!(
            "budget cap_bytes={} cap_from=share kept_bytes={} target_bytes={} \
             rebuildable_bytes={} under_bytes={} walk=partial sized_s=7",
            466 * GIB,
            342 * GIB,
            124 * GIB,
            24 * GIB,
            100 * GIB
        )]
    );
    let under = disk_reply(&w, &s);
    let budget = under.get("budget").expect("budget");
    assert_eq!(budget.get("under_bytes"), Some(&Jv::u(100 * GIB)));
    assert_eq!(budget.get("over_bytes"), None);
    assert_eq!(budget.get("cap_from"), Some(&Jv::s("share")));
    assert_eq!(budget.get("walk"), Some(&Jv::s("partial")));

    // At the target: under by 0.
    w.volume.budget = BudgetRead::Sized(BudgetFacts {
        cap: 466 * GIB,
        cap_written: true,
        kept: 400 * GIB,
        target: 66 * GIB,
        rebuildable: 66 * GIB,
        partial: false,
        sized_s: 0,
    });
    let at = budget_lines(&w, &s);
    assert!(
        matches!(at.as_slice(), [l] if l.contains(" under_bytes=0 ") && !l.contains("over_bytes")),
        "{at:?}"
    );

    // CONTROL: the budget turned off.
    w.volume.budget = BudgetRead::Off;
    assert_eq!(
        budget_lines(&w, &s),
        ["budget unavailable(not_enabled:disk.cap_used_gib)"]
    );
    assert_eq!(
        disk_reply(&w, &s).get("budget"),
        Some(&Jv::why("not_enabled:disk.cap_used_gib"))
    );
    assert_eq!(volume_line(&w, &s), plain_volume);

    // CONTROL: not sized yet.
    w.volume.budget = BudgetRead::Warming;
    assert_eq!(budget_lines(&w, &s), ["budget unavailable(warming)"]);
    assert_eq!(disk_reply(&w, &s).get("budget"), Some(&Jv::why("warming")));

    // No volume read: no figure either.
    let mut none = ctx(&w, Some(&s), true, true);
    none.volume = None;
    let reply = render_text(&query(View::Disk, false), &none);
    let lines: Vec<&str> = reply
        .lines
        .iter()
        .filter(|l| l.key == "budget")
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(lines, ["budget unavailable(not_sampled)"]);
}

/// The value at `path` (keys joined by `.`) of `j`.
fn at<'a>(j: &'a Jv, path: &str) -> &'a Jv {
    path.split('.').fold(j, |v, key| {
        v.get(key)
            .unwrap_or_else(|| panic!("no `{key}` of `{path}` in {}", j.to_text()))
    })
}

/// Whether `j` holds a number or a boolean anywhere: a figure something read.
fn holds_a_figure(j: &Jv) -> bool {
    match j {
        Jv::Int(_) | Jv::Bool(_) => true,
        Jv::Str(_) => false,
        Jv::Arr(items) => items.iter().any(holds_a_figure),
        Jv::Obj(pairs) => pairs.iter().any(|(_, v)| holds_a_figure(v)),
    }
}

/// A snapshot holding what N2's readers read on a MacBook Air at pressure Heavy: the heat chain
/// with a cut Super clock, P10, three zombie piles (one under another app, one under this tab's
/// cargo, one under another tab's program whose name no `exe=` word takes), two processes in an
/// uninterruptible wait, the lid, AC with a charging battery, an
/// external display, one sleep, a hotspot place and the computer's type.
fn n2_snapshot() -> Snapshot {
    use crate::blockage::{DStateView, DStuck, ZombiesView};
    use crate::heat::{
        Chain, ClockRange, Counters, HeatView, Judged, ThermalPressure, Verdict, Watts,
    };
    use crate::model::{PlaceKind, PlaceValue, PlaceView};
    use crate::runq::{QueueBasis, RunQueue};
    use crate::situation::{
        Awake, Battery, Displays, Lid, LidReading, MachineType, Power, PowerSource, Situation,
        SleepSpan,
    };
    let mut s = snapshot();
    s.heat = HeatView {
        pressure: Ok(ThermalPressure::Heavy),
        watts: Watts {
            cpu: Ok(9.4321),
            gpu: Ok(1.25),
            ane: Ok(0.0),
        },
        chain: Ok(Chain {
            window_ns: 1_000_000_000,
            counters: Counters::default(),
            range: Ok(ClockRange {
                p_mhz: (1260.0, 4464.0),
                e_mhz: (972.0, 3048.0),
            }),
            judged: Judged {
                p_mhz: Some(2512.4),
                e_mhz: Some(3048.0),
                ipj: Some(2_345_678_901.4),
                p_time_share: Some(0.384),
                p_instruction_share: Some(0.57),
                cpu_w: 9.4321,
                gpu_w: 1.25,
                ..Judged::default()
            },
            decile: Some(9),
            usual_mhz: Some(4113.0),
            p_verdict: Verdict::Pass,
            cut: Cut::Cut(4113.0 / 2512.4),
            e_verdict: Verdict::Fail,
        }),
    };
    s.facts.queue = Some(RunQueue {
        threads: 12.25,
        basis: QueueBasis::Runnable,
    });
    s.facts.zombies = Zombies::Read(ZombiesView {
        count: 19,
        piles: vec![
            ZombieParent {
                ppid: 1476,
                name: "Safari".into(),
                count: 14,
                per_min_deci: 36,
                tab: None,
            },
            ZombieParent {
                ppid: 201,
                name: "cargo".into(),
                count: 3,
                per_min_deci: 30,
                tab: Some(7),
            },
            ZombieParent {
                ppid: 310,
                name: EVIL.into(),
                count: 2,
                per_min_deci: 10,
                tab: Some(8),
            },
        ],
        growing: false,
        whole: true,
        age_ms: 0,
    });
    s.facts.dstate = DState::Read(DStateView {
        scope: DScope::Own,
        procs: vec![
            DStuck {
                pid: 202,
                name: EVIL.into(),
                threads: Some(1),
                threads_cut: false,
                held: false,
            },
            DStuck {
                pid: 900,
                name: "Safari".into(),
                threads: Some(2),
                threads_cut: false,
                held: true,
            },
        ],
        blocked: None,
        age_ms: 4_000,
        whole: true,
        pass: None,
    });
    s.situation = Situation {
        machine: Reading::Read(std::sync::Arc::new(MachineType {
            name: Some("MacBook Air".into()),
            family: None,
            model: Some("MacBook Air (15-inch, M5)".into()),
            model_id: Some("Mac17,4".into()),
        })),
        lid: Reading::Read(LidReading {
            lid: Lid::Open,
            changed_ms: None,
        }),
        power: Reading::Read(Power {
            source: PowerSource::Ac,
            battery: Some(Battery {
                pct: 53,
                charging: true,
            }),
            adapter_w: Some(70),
            changed_ms: None,
        }),
        displays: Reading::Read(Displays { external: 1 }),
        awake: Reading::Read(Awake {
            boot_ms: 1_759_000_000_000,
            last_sleep: Some(SleepSpan {
                from_ms: 1_759_990_000_000,
                to_ms: 1_759_991_000_000,
                cause: Some("Clamshell Sleep".into()),
                wake: None,
            }),
        }),
    };
    s.place = Some(PlaceView {
        line: "hotspot".into(),
        fields: vec![
            ("state", PlaceValue::Text("remote".into())),
            ("kind", PlaceValue::Text("hotspot".into())),
            ("since_ms", PlaceValue::Count(42)),
        ],
        kind: PlaceKind::Hotspot,
    });
    s
}

/// THE OVERVIEW PRINTS WHAT N2'S READERS READ (A4 §12.1, SD-01): `sys --json` carries the heat
/// chain, the run queue beside `load_m`, the zombie piles and uninterruptible waits, the
/// situation with the place beside it, and the computer's type, each figure a whole number in the
/// unit its key names. A row that names a process outside the target's tab prints only for the
/// owner class: a scoped caller reads its own tab's pile, the machine's counts and how many rows
/// were withheld. `sys cores` no longer prints `threads.runnable`, the hard-coded `not_sampled`:
/// `global.queue` carries the run queue. CONTROLS: the queue on the load average says `load1`; a
/// sweep that stopped at its cap bounds the zombie count; a Linux sweep's waits are the
/// machine's, with `procs_blocked` and no thread counts.
#[test]
fn the_overview_json_carries_heat_queue_blockage_situation_and_type() {
    use crate::blockage::{DStateView, DStuck};
    use crate::runq::{QueueBasis, RunQueue};
    let w = world();
    let s = n2_snapshot();
    let owner = ctx(&w, Some(&s), true, true);
    let j = render_json(&query(View::Overview, true), &owner);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());

    assert_eq!(
        at(&j, "heat"),
        &Jv::obj([
            ("pressure", Jv::s("heavy")),
            ("thermal_state", Jv::s("nominal")),
            (
                "clock",
                Jv::obj([
                    ("p_mhz", Jv::u(2512)),
                    ("e_mhz", Jv::u(3048)),
                    ("e_verdict", Jv::s("fail")),
                    ("basis", Jv::s("coalition_counters")),
                    ("p_usual_mhz", Jv::u(4113)),
                    (
                        "cut",
                        Jv::obj([("state", Jv::s("cut")), ("usual_over_clock_m", Jv::u(1637)),]),
                    ),
                ]),
            ),
            (
                "watts",
                Jv::obj([
                    ("cpu_mw", Jv::u(9432)),
                    ("gpu_mw", Jv::u(1250)),
                    ("ane_mw", Jv::u(0)),
                ]),
            ),
            ("ipj", Jv::u(2_345_678_901)),
            (
                "p_share",
                Jv::obj([("time_pm", Jv::u(384)), ("instructions_pm", Jv::u(570))]),
            ),
        ]),
    );
    assert_eq!(at(&j, "heat.thermal_state"), at(&j, "global.thermal"));

    let queue = Jv::obj([
        ("threads_m", Jv::u(12_250)),
        ("basis", Jv::s("runnable_10s")),
    ]);
    assert_eq!(at(&j, "global.queue"), &queue);
    let Some(Jv::Obj(global)) = j.get("global") else {
        panic!("{}", j.to_text());
    };
    let place_of = |key: &str| global.iter().position(|(k, _)| k == key);
    assert_eq!(
        place_of("queue"),
        place_of("load_m").map(|i| i + 1),
        "the queue stands beside load_m"
    );

    assert_eq!(
        at(&j, "procs"),
        &Jv::obj([
            (
                "zombies",
                Jv::Arr(vec![
                    Jv::obj([
                        ("parent", Jv::u(1476)),
                        ("exe", Jv::s("Safari")),
                        ("count", Jv::u(14)),
                        ("per_min_deci", Jv::u(36)),
                    ]),
                    Jv::obj([
                        ("parent", Jv::u(201)),
                        ("exe", Jv::s("cargo")),
                        ("count", Jv::u(3)),
                        ("per_min_deci", Jv::u(30)),
                        ("local", Jv::u(7)),
                        ("tab", Jv::u(1)),
                    ]),
                    Jv::obj([
                        ("parent", Jv::u(310)),
                        ("exe", Jv::s("-")),
                        ("count", Jv::u(2)),
                        ("per_min_deci", Jv::u(10)),
                        ("local", Jv::u(8)),
                    ]),
                ]),
            ),
            ("zombie_count", Jv::u(19)),
            ("zombies_age_ms", Jv::u(0)),
            (
                "dstate",
                Jv::Arr(vec![
                    Jv::obj([
                        ("pid", Jv::u(900)),
                        ("exe", Jv::s("Safari")),
                        ("threads", Jv::u(2)),
                        ("held", Jv::Bool(true)),
                    ]),
                    Jv::obj([
                        ("pid", Jv::u(202)),
                        ("exe", Jv::s("-")),
                        ("threads", Jv::u(1)),
                        ("held", Jv::Bool(false)),
                    ]),
                ]),
            ),
            ("dstate_count", Jv::u(2)),
            ("dstate_held", Jv::u(1)),
            ("dstate_age_ms", Jv::u(4_000)),
            ("dstate_scope", Jv::s("own")),
        ]),
    );

    assert_eq!(
        at(&j, "situation"),
        &Jv::obj([
            ("lid", Jv::s("open")),
            (
                "power",
                Jv::obj([
                    ("source", Jv::s("ac")),
                    ("battery_pct", Jv::u(53)),
                    ("charging", Jv::Bool(true)),
                ]),
            ),
            ("docked", Jv::Bool(true)),
            ("awake_since_ms", Jv::Int(1_759_991_000_000)),
            (
                "last_sleep",
                Jv::obj([
                    ("from_ms", Jv::Int(1_759_990_000_000)),
                    ("to_ms", Jv::Int(1_759_991_000_000)),
                    ("cause", Jv::s("Clamshell Sleep")),
                    ("wake", Jv::why("not_sampled")),
                ]),
            ),
        ]),
        "the place and Low Power Mode are not copied into it",
    );
    // The place stands beside the situation, as the global view's own `place` prints it.
    let global_view = render_json(&query(View::Global, true), &owner);
    assert_eq!(at(&j, "place.kind"), &Jv::s("hotspot"));
    assert_eq!(j.get("place"), global_view.get("place"));

    for view in [&j, &global_view] {
        assert_eq!(at(view, "machine.type"), &Jv::s("MacBook Air"));
        assert_eq!(
            at(view, "machine.model"),
            &Jv::s("MacBook Air (15-inch, M5)")
        );
        assert_eq!(at(view, "machine.model_id"), &Jv::s("Mac17,4"));
        assert_eq!(at(view, "machine.cpu_name"), &Jv::s("Apple M5"));
        assert_eq!(at(view, "global.queue"), &queue);
    }
    assert!(contract_violations(&global_view).is_empty());

    // `sys cores`: the hard-coded figure is gone, the waits it stood beside stay.
    let cores = render_json(&query(View::Cores, true), &owner);
    assert!(at(&cores, "threads").get("runnable").is_none());
    assert!(at(&cores, "threads").get("wait_mc").is_some());
    assert!(at(&cores, "threads").get("tab_wait_mc").is_some());

    // A scoped caller: its own tab's pile, the machine's counts, and how many rows it may not
    // read. No name of a process outside the tab is in the reply.
    // The rule is the target's tab, whichever tab the caller sits in (its own, then another).
    for own_tab in [true, false] {
        let scoped = ctx(&w, Some(&s), false, own_tab);
        let j = render_json(&query(View::Overview, true), &scoped);
        assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
        assert_eq!(
            at(&j, "procs.zombies"),
            &Jv::Arr(vec![Jv::obj([
                ("parent", Jv::u(201)),
                ("exe", Jv::s("cargo")),
                ("count", Jv::u(3)),
                ("per_min_deci", Jv::u(30)),
                ("local", Jv::u(7)),
                ("tab", Jv::u(1)),
            ])]),
            "own_tab={own_tab}",
        );
        assert_eq!(at(&j, "procs.zombies_withheld"), &Jv::u(2));
        assert_eq!(at(&j, "procs.zombie_count"), &Jv::u(19));
        assert_eq!(at(&j, "procs.dstate"), &Jv::Arr(Vec::new()));
        assert_eq!(at(&j, "procs.dstate_withheld"), &Jv::u(2));
        assert_eq!(at(&j, "procs.dstate_count"), &Jv::u(2));
        assert_eq!(at(&j, "procs.dstate_held"), &Jv::u(1));
        assert!(!j.to_text().contains("Safari"), "{}", j.to_text());
    }

    // `top=` cuts each list as it cuts every other, largest pile and held wait first, and says
    // how many rows it left out.
    let mut one = query(View::Overview, true);
    one.top = 1;
    let j = render_json(&one, &owner);
    let first = |key: &str| match at(&j, key) {
        Jv::Arr(rows) if rows.len() == 1 => rows[0].clone(),
        other => panic!("{key}: {other:?}"),
    };
    assert_eq!(first("procs.zombies").get("parent"), Some(&Jv::u(1476)));
    assert_eq!(at(&j, "procs.zombies_more"), &Jv::u(2));
    assert_eq!(first("procs.dstate").get("pid"), Some(&Jv::u(900)));
    assert_eq!(at(&j, "procs.dstate_more"), &Jv::u(1));
    assert_eq!(at(&j, "procs.zombie_count"), &Jv::u(19));
    assert_eq!(at(&j, "procs.dstate_count"), &Jv::u(2));

    // CONTROL: P10 unread, so the over rule reads the load average and the queue says so.
    let mut load = s.clone();
    load.facts.queue = Some(RunQueue {
        threads: 2.5,
        basis: QueueBasis::Load1,
    });
    let j = render_json(
        &query(View::Overview, true),
        &ctx(&w, Some(&load), true, true),
    );
    assert_eq!(
        at(&j, "global.queue"),
        &Jv::obj([("threads_m", Jv::u(2500)), ("basis", Jv::s("load1"))]),
    );

    // CONTROL: a sweep that stopped at its cap counts at least these.
    let mut capped = s.clone();
    if let Zombies::Read(v) = &mut capped.facts.zombies {
        v.whole = false;
        v.age_ms = 2_000;
    }
    // CONTROL: Linux's sweep reads every user's process states and no thread counts, and the
    // same cut sweep leaves its `D` processes a lower bound too.
    capped.facts.dstate = DState::Read(DStateView {
        scope: DScope::Machine,
        procs: vec![DStuck {
            pid: 900,
            name: "Safari".into(),
            threads: None,
            threads_cut: false,
            held: true,
        }],
        blocked: Some(3),
        age_ms: 0,
        whole: false,
        pass: None,
    });
    let j = render_json(
        &query(View::Overview, true),
        &ctx(&w, Some(&capped), true, true),
    );
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    assert_eq!(at(&j, "procs.zombie_count"), &Jv::at_least(19, "truncated"));
    assert_eq!(at(&j, "procs.zombies_age_ms"), &Jv::u(2_000));
    assert_eq!(at(&j, "procs.dstate_scope"), &Jv::s("machine"));
    assert_eq!(at(&j, "procs.dstate_blocked"), &Jv::u(3));
    assert_eq!(at(&j, "procs.dstate_count"), &Jv::at_least(1, "truncated"));
    assert_eq!(at(&j, "procs.dstate_held"), &Jv::at_least(1, "truncated"));
    assert_eq!(
        at(&j, "procs.dstate"),
        &Jv::Arr(vec![Jv::obj([
            ("pid", Jv::u(900)),
            ("exe", Jv::s("Safari")),
            ("threads", Jv::why("unsupported")),
            ("held", Jv::Bool(true)),
        ])]),
    );
    // A `/proc/stat` that gave no `procs_blocked` says so, never 0.
    if let DState::Read(v) = &mut capped.facts.dstate {
        v.blocked = None;
    }
    let j = render_json(
        &query(View::Overview, true),
        &ctx(&w, Some(&capped), true, true),
    );
    assert_eq!(at(&j, "procs.dstate_blocked"), &Jv::why("not_sampled"));

    // CONTROL: a census pass that did not read every process it listed (its cover is open, or
    // took two passes) counts at least what it found; one that was its whole cover is exact,
    // unless a read of it was cut at a cap, which counts at least too. The open cover, the wider
    // gap, is the cause named where both hold.
    for (cover_done, passes_in_cover, whole, bound) in [
        (false, 0, true, Some("partial_census")),
        (true, 2, true, Some("partial_census")),
        (true, 1, true, None),
        (true, 1, false, Some("truncated")),
        (true, 2, false, Some("partial_census")),
    ] {
        let mut census = s.clone();
        if let DState::Read(v) = &mut census.facts.dstate {
            v.whole = whole;
            v.pass = Some(std::sync::Arc::new(crate::blockage::CensusPass {
                cover_done,
                passes_in_cover,
                ..crate::blockage::CensusPass::default()
            }));
        }
        let j = render_json(
            &query(View::Overview, true),
            &ctx(&w, Some(&census), true, true),
        );
        assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
        let figure = |n| bound.map_or_else(|| Jv::u(n), |cause| Jv::at_least(n, cause));
        let case = format!("{cover_done} {passes_in_cover} {whole}");
        assert_eq!(at(&j, "procs.dstate_count"), &figure(2), "{case}");
        assert_eq!(at(&j, "procs.dstate_held"), &figure(1), "{case}");
    }

    // A process with more threads than the census reads counts at least the threads read; the
    // counts of processes stay exact, since it was found. CONTROL: the row beside it is exact.
    let mut cut = s.clone();
    if let DState::Read(v) = &mut cut.facts.dstate {
        v.procs[1].threads = Some(256);
        v.procs[1].threads_cut = true;
    }
    let j = render_json(
        &query(View::Overview, true),
        &ctx(&w, Some(&cut), true, true),
    );
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let Jv::Arr(rows) = at(&j, "procs.dstate") else {
        panic!("{}", j.to_text())
    };
    let threads: Vec<&Jv> = rows.iter().map(|r| at(r, "threads")).collect();
    assert_eq!(threads, [&Jv::at_least(256, "truncated"), &Jv::u(1)]);
    assert_eq!(at(&j, "procs.dstate_count"), &Jv::u(2));
    assert_eq!(at(&j, "procs.dstate_held"), &Jv::u(1));
}

/// A VALUE THAT IS NOT A READING PRINTS ITS WORD, NEVER A ZERO (A4 §8, §12.1): every heat,
/// queue, blockage, situation and type value says `not_sampled` where its reader gave nothing and
/// `unsupported` where this system has none, with the reader's reason where it names one, and the
/// reply holds no number or boolean in their place. What a machine lacks is a reading of its own
/// (`none`), not a word. CONTROL: a reader that read nothing prints 0 and an empty list.
#[test]
fn an_unread_heat_or_situation_reading_prints_its_word_never_zero() {
    use crate::blockage::{DStateView, ZombiesView};
    use crate::heat::{HeatView, Watts};
    use crate::model::Unread;
    use crate::situation::{Awake, Displays, Lid, LidReading, MachineType, Power, PowerSource};
    let w = world();
    let overview = |s: &Snapshot| {
        let j = render_json(&query(View::Overview, true), &ctx(&w, Some(s), true, true));
        assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
        j
    };
    let no_figure = |j: &Jv| {
        for part in ["heat", "procs", "situation", "place", "global.queue"] {
            assert!(!holds_a_figure(at(j, part)), "{part}: {}", j.to_text());
        }
    };
    let read = n2_snapshot();
    // CONTROL: the snapshot the readers read holds figures in each of them.
    for part in ["heat", "procs", "situation", "place", "global.queue"] {
        assert!(holds_a_figure(at(&overview(&read), part)), "CONTROL {part}");
    }

    // No reader has answered yet.
    let mut s = read.clone();
    s.heat = HeatView::default();
    s.thermal = None;
    s.facts.queue = None;
    s.facts.runnable = Runnable::Unread(Unread::NotSampled);
    s.facts.zombies = Zombies::default();
    s.facts.dstate = DState::default();
    s.situation = crate::situation::Situation::default();
    s.place = None;
    let j = overview(&s);
    no_figure(&j);
    let unread = || Jv::why("not_sampled");
    let not_read = || Jv::why("not_sampled:not_read");
    assert_eq!(
        at(&j, "heat"),
        &Jv::obj([
            ("pressure", not_read()),
            ("thermal_state", Jv::why("unsupported")),
            ("clock", not_read()),
            (
                "watts",
                Jv::obj([
                    ("cpu_mw", not_read()),
                    ("gpu_mw", not_read()),
                    ("ane_mw", not_read()),
                ]),
            ),
            ("ipj", not_read()),
            ("p_share", not_read()),
        ]),
    );
    assert_eq!(at(&j, "global.queue"), &unread());
    assert_eq!(
        at(&j, "procs"),
        &Jv::obj([
            ("zombies", unread()),
            ("zombie_count", unread()),
            ("dstate", unread()),
            ("dstate_count", unread()),
            ("dstate_held", unread()),
            ("dstate_scope", unread()),
        ]),
    );
    assert_eq!(
        at(&j, "situation"),
        &Jv::obj([
            ("lid", unread()),
            ("power", unread()),
            ("docked", unread()),
            ("awake_since_ms", unread()),
            ("last_sleep", unread()),
        ]),
    );
    assert_eq!(at(&j, "place"), &unread());
    for key in ["machine.type", "machine.model", "machine.model_id"] {
        assert_eq!(at(&j, key), &unread(), "{key}");
    }

    // A system with no reader of them (Windows): each says so, with the reader's reason.
    let none = |reason| NoHeat::NotSupported(reason);
    s.heat = HeatView {
        pressure: Err(none("no_pressure_level")),
        watts: Watts {
            ane: Err(none("no_neural_engine")),
            ..Watts::none(none("no_power_reader"))
        },
        chain: Err(none("no_coalitions")),
    };
    s.facts.runnable = Runnable::Unread(Unread::Unsupported);
    s.facts.zombies = Zombies::Unread(Unread::Unsupported);
    s.facts.dstate = DState::Unread(Unread::Unsupported);
    s.situation = crate::situation::Situation {
        machine: Reading::Unsupported,
        lid: Reading::Unsupported,
        power: Reading::Unsupported,
        displays: Reading::Unsupported,
        awake: Reading::Unsupported,
    };
    let j = overview(&s);
    no_figure(&j);
    let unsupported = || Jv::why("unsupported");
    assert_eq!(
        at(&j, "heat.pressure"),
        &Jv::why("unsupported:no_pressure_level")
    );
    assert_eq!(at(&j, "heat.clock"), &Jv::why("unsupported:no_coalitions"));
    assert_eq!(at(&j, "heat.ipj"), &Jv::why("unsupported:no_coalitions"));
    assert_eq!(
        at(&j, "heat.watts.cpu_mw"),
        &Jv::why("unsupported:no_power_reader")
    );
    assert_eq!(
        at(&j, "heat.watts.ane_mw"),
        &Jv::why("unsupported:no_neural_engine")
    );
    assert_eq!(at(&j, "global.queue"), &unsupported());
    for key in [
        "procs.zombies",
        "procs.zombie_count",
        "procs.dstate",
        "procs.dstate_count",
        "procs.dstate_held",
        "procs.dstate_scope",
        "situation.lid",
        "situation.power",
        "situation.docked",
        "situation.awake_since_ms",
        "situation.last_sleep",
        "machine.type",
        "machine.model",
        "machine.model_id",
    ] {
        assert_eq!(at(&j, key), &unsupported(), "{key}");
    }

    // A census whose process list the OS refused says refused, never an empty census.
    s.facts.dstate = DState::Unread(Unread::Refused);
    // A reason no unavailable word takes is left off: the reply keeps its contract.
    s.heat.pressure = Err(NoHeat::Unread("no such word!"));
    let j = overview(&s);
    no_figure(&j);
    assert_eq!(at(&j, "procs.dstate"), &Jv::why("refused"));
    assert_eq!(at(&j, "heat.pressure"), &unread());

    // A chain that read, with values it could not give: each says which.
    let mut partial = read.clone();
    let Ok(chain) = partial.heat.chain.as_mut() else {
        panic!("the fixture's chain");
    };
    chain.range = Err(none("no_clock_table"));
    chain.judged = crate::heat::Judged::default();
    chain.decile = None;
    chain.usual_mhz = None;
    chain.cut = Cut::Unread;
    partial.heat.watts.cpu = Ok(f64::NAN);
    let j = overview(&partial);
    let too_little = Jv::why("not_sampled:too_little_core_time");
    for key in ["heat.clock.p_mhz", "heat.clock.e_mhz"] {
        assert_eq!(at(&j, key), &Jv::why("unsupported:no_clock_table"), "{key}");
    }
    assert_eq!(
        at(&j, "heat.clock.p_usual_mhz"),
        &Jv::why("not_sampled:no_busy_decile")
    );
    assert_eq!(at(&j, "heat.clock.cut"), &unread());
    assert_eq!(at(&j, "heat.ipj"), &too_little);
    assert_eq!(at(&j, "heat.p_share.time_pm"), &too_little);
    assert_eq!(at(&j, "heat.p_share.instructions_pm"), &too_little);
    assert_eq!(at(&j, "heat.watts.cpu_mw"), &Jv::why("unread_format"));
    // With a table, an idle window gives no clock, and an Efficiency clock past its table is
    // refused alone; a decile that has not learned its usual clock says so.
    let Ok(chain) = partial.heat.chain.as_mut() else {
        panic!("the fixture's chain");
    };
    chain.range = read.heat.chain.expect("the fixture's chain").range;
    chain.judged.e_refused = true;
    chain.decile = Some(3);
    chain.cut = Cut::Gated(crate::heat::Verdict::Unmeasured);
    let j = overview(&partial);
    assert_eq!(at(&j, "heat.clock.p_mhz"), &too_little);
    assert_eq!(
        at(&j, "heat.clock.e_mhz"),
        &Jv::why("not_sampled:past_table")
    );
    assert_eq!(
        at(&j, "heat.clock.p_usual_mhz"),
        &Jv::why("not_sampled:unlearned")
    );
    assert_eq!(
        at(&j, "heat.clock.cut"),
        &Jv::obj([("state", Jv::s("gated")), ("verdict", Jv::s("unmeasured")),]),
    );
    // Each other judgement of the cut says its own state, and a share past the whole (the Super
    // time and the QoS time are two accounts) is the whole.
    let state = |word: &str| Jv::obj([("state", Jv::s(word))]);
    for (cut, want) in [
        (Cut::NotJudged, state("not_judged")),
        (Cut::Unlearned, state("unlearned")),
        (
            Cut::Within(1.02),
            Jv::obj([
                ("state", Jv::s("within")),
                ("usual_over_clock_m", Jv::u(1020)),
            ]),
        ),
    ] {
        let Ok(chain) = partial.heat.chain.as_mut() else {
            panic!("the fixture's chain");
        };
        chain.cut = cut;
        chain.judged.p_time_share = Some(1.004);
        let j = overview(&partial);
        assert_eq!(at(&j, "heat.clock.cut"), &want, "{cut:?}");
        assert_eq!(at(&j, "heat.p_share.time_pm"), &Jv::u(1000));
    }

    // What a machine lacks is a reading: a desktop that has not slept, whose firmware names no
    // model, says `none` for its lid, battery, docked and last sleep, and a word for no field.
    let mut desktop = read.clone();
    desktop.situation = crate::situation::Situation {
        machine: Reading::Read(std::sync::Arc::new(MachineType {
            name: Some("Mac mini".into()),
            ..MachineType::default()
        })),
        lid: Reading::Read(LidReading {
            lid: Lid::None,
            changed_ms: None,
        }),
        power: Reading::Read(Power {
            source: PowerSource::Ac,
            battery: None,
            adapter_w: None,
            changed_ms: None,
        }),
        displays: Reading::Read(Displays { external: 2 }),
        awake: Reading::Read(Awake {
            boot_ms: 1_759_000_000_000,
            last_sleep: None,
        }),
    };
    let j = overview(&desktop);
    let none_word = Jv::s("none");
    assert_eq!(
        at(&j, "situation"),
        &Jv::obj([
            ("lid", none_word.clone()),
            (
                "power",
                Jv::obj([
                    ("source", Jv::s("ac")),
                    ("battery_pct", none_word.clone()),
                    ("charging", none_word.clone()),
                ]),
            ),
            ("docked", none_word.clone()),
            ("awake_since_ms", Jv::Int(1_759_000_000_000)),
            ("last_sleep", none_word),
        ]),
    );
    assert_eq!(at(&j, "machine.type"), &Jv::s("Mac mini"));
    for key in ["machine.model", "machine.model_id"] {
        assert_eq!(
            at(&j, key),
            &Jv::why("not_sampled:firmware_gave_none"),
            "{key}"
        );
    }

    // CONTROL: a reader that read nothing prints its 0 and its empty list.
    let mut quiet = read;
    quiet.facts.zombies = Zombies::Read(ZombiesView {
        whole: true,
        ..ZombiesView::default()
    });
    quiet.facts.dstate = DState::Read(DStateView {
        scope: DScope::Own,
        procs: Vec::new(),
        blocked: None,
        age_ms: 0,
        whole: true,
        pass: None,
    });
    quiet.facts.queue = Some(crate::runq::RunQueue {
        threads: 0.0,
        basis: crate::runq::QueueBasis::Runnable,
    });
    let j = overview(&quiet);
    assert_eq!(at(&j, "procs.zombies"), &Jv::Arr(Vec::new()));
    assert_eq!(at(&j, "procs.zombie_count"), &Jv::u(0));
    assert_eq!(at(&j, "procs.dstate"), &Jv::Arr(Vec::new()));
    assert_eq!(at(&j, "procs.dstate_count"), &Jv::u(0));
    assert_eq!(at(&j, "global.queue.threads_m"), &Jv::u(0));
    assert_eq!(at(&j, "heat.watts.ane_mw"), &Jv::u(0));
}
