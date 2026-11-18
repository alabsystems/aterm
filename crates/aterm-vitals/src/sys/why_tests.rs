// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for reported CPU accounts, memory attribution, privacy, and text/JSON boundaries.

use super::*;
use crate::events::{Event, EventKind};
use crate::explain::fixtures::{
    Coal, Proc, Scene, USERSPACE_MS, app_work_1510, apple, build_start_1032, calm_1510,
    delegated_1032, delegated_in_aterm_1032, finished_1032, lane_off_1032, liar_1032, reaped_1032,
    spotlight_1032, spotlight_late_1032, tab, tab_ahead_1032, tab2_build_1048, two_coalitions_1421,
    wrapped_1218,
};
use crate::explain::{CoalLane, LeadIdentity, Voucher};
use crate::model::{MemCounters, MemSample, Pressure, Thermal, Throttle, Vitals};
use crate::sys::json::contract_violations;
use crate::sys::view::{Reply, render_json, render_text};
use crate::sys::{Ctx, Freshness, Machine, TabFact, Target, apply_delta, fingerprints};

fn published_reading(tab_mc: i64, machine_mc: i64) -> Snapshot {
    let mut snapshot = read(&calm_1510(), 3);
    snapshot.procs_window_ns = Some(2_000_000_000);
    let facts = std::sync::Arc::make_mut(snapshot.why.as_mut().expect("counter observations"));
    facts.window_ns = 1_000_000_000;
    facts.split.cpu_accounting = crate::published_cpu::CpuAccounting::PUBLISHED_COUNTERS;
    facts.split.coverage_complete = true;
    facts.split.ownership_incomplete = false;
    facts.split.global_mc = machine_mc;
    facts.split.tabs = vec![(1, tab_mc + 100)];
    facts.split.tab_reported = vec![(1, tab_mc)];
    facts.split.own_reported_mc = 4;
    // Coalition and process reports are independent; neither caps the other's growth.
    facts.split.aterm_mc = 50_000;
    snapshot
}

#[test]
fn reported_cpu_late_growth_keeps_rates_read_span_and_unknown_execution_window() {
    // Four ms from before the first read publish with the one ms executed inside it.
    // One other ms executes inside. The reported tab rate is 5 mc, above the 2 mc machine
    // observation, although its execution inside was only 1 mc. Coverage is complete.
    let snapshot = published_reading(5, 2);
    let world = mac();
    for owner in [true, false] {
        let context = ctx(&world, &snapshot, owner);
        let json = render_json(&q("why cpu --json"), &context);
        assert!(contract_violations(&json).is_empty(), "{}", json.to_text());
        assert!(json.get("split_mc").is_none());
        let figure = json.get("figure").unwrap();
        assert_eq!(figure.get("cpu_mc"), Some(&Jv::Int(2)));
        assert_eq!(
            figure.get("accounting_basis"),
            Some(&Jv::s("machine_ticks"))
        );
        assert!(figure.get("execution_window").is_none());
        let reported = json.get("reported_cpu_mc").unwrap();
        assert_eq!(reported.get("tab"), Some(&Jv::Int(5)));
        assert_eq!(reported.get("aterm_rest"), Some(&Jv::Int(4)));
        assert_eq!(reported.get("basis"), Some(&Jv::s("published_counters")));
        assert_eq!(reported.get("coalition_coverage"), Some(&Jv::s("complete")));
        assert_eq!(reported.get("read_span_ns"), Some(&Jv::u(1_000_000_000)));
        assert!(reported.get("unaccounted").is_none());
        assert_eq!(
            reported.get("execution_window"),
            Some(&Jv::why("unavailable:execution_window_unknown"))
        );
        let you = json.get("you").unwrap();
        assert_eq!(you.get("reported_cpu_mc"), Some(&Jv::Int(5)));
        assert_eq!(
            you.get("role"),
            Some(&Jv::why("unavailable:execution_window_unknown"))
        );
        assert!(you.get("cpu_at_least_mc").is_none() && you.get("share_pm").is_none());
        assert_eq!(json.get("causes"), Some(&Jv::Arr(Vec::new())));
        let text = why("why cpu", &context);
        let rates = lines(&text, "reported_cpu")[0];
        assert_eq!(field(rates, "tab_mc"), 5);
        assert_eq!(field(rates, "read_span_ns"), 1_000_000_000);
        assert!(!text.contains('≥') && !text.contains('≤'));
        assert!(lines(&text, "cause").is_empty());
    }
}

#[test]
fn reported_cpu_zero_publication_never_says_no_execution_or_nothing_needed() {
    // Positive execution can be unpublished at both reads. These observations cannot
    // distinguish it from an idle process, even with complete owner coverage.
    let snapshot = published_reading(0, 0);
    let world = mac();
    let context = ctx(&world, &snapshot, true);
    let text = why("why cpu", &context);
    assert_eq!(field(lines(&text, "reported_cpu")[0], "tab_mc"), 0);
    assert!(!text.contains("uses none") && !text.contains("role=none"));
    assert!(lines(&text, "do").is_empty());
    let json = render_json(&q("why cpu --json"), &context);
    assert_eq!(
        json.get("you").and_then(|you| you.get("reported_cpu_mc")),
        Some(&Jv::Int(0))
    );
    assert_eq!(
        json.get("you").and_then(|you| you.get("role")),
        Some(&Jv::why("unavailable:execution_window_unknown"))
    );
    assert_eq!(json.get("do"), Some(&Jv::Arr(Vec::new())));
    assert!(contract_violations(&json).is_empty(), "{}", json.to_text());
}

struct World {
    machine: Machine,
    target: Target,
    tabs: Vec<TabFact>,
}

fn world(family: &'static str, tabs: u64) -> World {
    World {
        machine: Machine {
            executable: None,
            cpu: Some("Apple M5 Max".into()),
            os: Some("macOS 27.0".into()),
            family,
        },
        target: Target {
            key: 1,
            sid: "s-ab12".into(),
            window: Some(1),
            tab: Some(1),
            panes: vec![1],
            window_sessions: (1..=tabs).collect(),
            ..Target::default()
        },
        tabs: (1..=tabs)
            .map(|k| TabFact {
                sampled_nonce: None,
                key: k,
                sid: format!("s-{k}"),
                window: Some(1),
                tab: usize::try_from(k).ok(),
            })
            .collect(),
    }
}

fn mac() -> World {
    world("macOS", 1)
}

fn ctx<'a>(w: &'a World, s: &'a Snapshot, owner: bool) -> Ctx<'a> {
    Ctx {
        snap: Some(s),
        epoch: 0x7f3a,
        as_of_unix_ms: u64::try_from(s.at_unix_ms).unwrap_or(0),
        age_ms: 120,
        state: Freshness::Fresh,
        machine: &w.machine,
        target: &w.target,
        tabs: &w.tabs,
        caller_panes: Some(&w.target.panes),
        owner_class: owner,
        volume: None,
        roots: &[],
        disk: None,
        stores: &[],
        home: None,
        events: &[],
        gap: None,
        head_seq: 0,
        held: None,
    }
}

fn last(readings: &[crate::model::Sample]) -> Snapshot {
    let mut v = Vitals::new();
    let mut snaps: Vec<Snapshot> = readings.iter().map(|r| v.absorb(r)).collect();
    snaps.pop().expect("readings")
}

fn read(scene: &Scene, windows: u64) -> Snapshot {
    last(&scene.series(windows))
}

fn read_at(scene: &Scene, windows: u64) -> Snapshot {
    let later = i64::try_from((windows - 1) * scene.window_ms).unwrap_or(0);
    let scene = Scene {
        at_ms: scene.at_ms.map(|at| at - later),
        ..scene.clone()
    };
    read(&scene, windows)
}

fn read_calm(scene: Scene) -> Scene {
    let calm = calm_1510();
    Scene {
        mem: calm.mem,
        thermal: calm.thermal,
        ..scene
    }
}

fn example_a() -> Snapshot {
    let scene = Scene {
        at_ms: Some(1_792_517_551_200),
        ..read_calm(spotlight_1032())
    };
    let mut s = read(&scene, 3);
    if let Some(cpu) = s.cpu.as_mut() {
        cpu.busy.slow = cpu.busy.level + 1.0;
    }
    s
}

fn q(text: &str) -> SysQuery {
    SysQuery::parse(text).expect("query")
}

fn wire(r: &Reply, c: &Ctx<'_>) -> String {
    wire_as(r, c, false)
}

fn wire_as(r: &Reply, c: &Ctx<'_>, delta: bool) -> String {
    let mut t = r.header(c, delta);
    for l in &r.lines {
        t.push('\n');
        t.push_str(&l.text);
    }
    t.push('\n');
    t
}

fn why(text: &str, c: &Ctx<'_>) -> String {
    wire(&render_text(&q(text), c), c)
}

fn lines<'t>(text: &'t str, key: &str) -> Vec<&'t str> {
    let head = format!("{key} ");
    text.lines().filter(|l| l.starts_with(&head)).collect()
}

fn field(line: &str, key: &str) -> i64 {
    let head = format!("{key}=");
    line.split(' ')
        .find_map(|f| f.strip_prefix(&head))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no {key} in {line}"))
}

fn mem_1142() -> Scene {
    let mut scene = calm_1510();
    scene.mem = Some(MemSample {
        total: 128 << 30,
        app: 12_545_392_640,
        wired: 5_132_959_744,
        compressed: Some(0),
        cached: 27_967_258_624,
        free: 89_733_971_968,
        swap_used: Some(0),
        swap_total: Some(0),
        exact: true,
        pressure: Pressure::Normal,
        counters: Some(MemCounters::default()),
        ..MemSample::default()
    });
    for c in &mut scene.coalitions {
        c.footprint = match c.id {
            1342 => Some(1_630_032_364),
            393 => Some(773_230_428),
            _ => None,
        };
    }
    let safari = USERSPACE_MS + 320_000;
    scene.coalitions.push(Coal {
        footprint: Some(846_525_891),
        ..Coal::new(1393, 0)
    });
    scene
        .procs
        .push(Proc::new(1631, 1, 501, 1393, safari, "Safari", 0));
    scene
        .leads
        .push(apple(1631, safari, "Safari", "com.apple.Safari"));
    scene
}

/// [`mem_1142`] with fseventsd holding 44.4 GiB, as on 2026-10-22, its lead signed as
/// `signing_id`.
fn fseventsd_1142(signing_id: &str) -> Scene {
    let mut scene = mem_1142();
    let born = USERSPACE_MS + 2_000;
    scene.coalitions.push(Coal {
        footprint: Some(47_675_000_000),
        ..Coal::new(777, 0)
    });
    scene
        .procs
        .push(Proc::new(537, 1, 0, 777, born, "fseventsd", 0));
    scene.leads.push(apple(537, born, "fseventsd", signing_id));
    scene
}

/// `scene` with the kernel's memory pressure past normal.
fn pressed(mut scene: Scene) -> Scene {
    if let Some(mem) = scene.mem.as_mut() {
        mem.pressure = Pressure::Warn;
    }
    scene
}

/// The service-memory cause lines of a reply.
fn service_causes(t: &str) -> Vec<&str> {
    lines(t, "cause")
        .into_iter()
        .filter(|l| l.contains("id=service-memory"))
        .collect()
}

fn split_tab(key: u64) -> World {
    let mut w = world("macOS", 2);
    w.target.key = key;
    w.target.panes = vec![1, 2];
    w.target.window_sessions = vec![1, 2];
    for t in &mut w.tabs {
        t.tab = Some(1);
    }
    w
}

fn two_sessions_1032() -> Scene {
    let born = USERSPACE_MS + 210_000;
    let mut scene = read_calm(spotlight_1032());
    scene.tabs.push(tab(2, 1300));
    scene.busy_mc += 300;
    for c in &mut scene.coalitions {
        if c.id == 1342 {
            c.mc += 300;
            c.started += 2;
        }
    }
    scene
        .procs
        .push(Proc::new(1300, 1187, 501, 1342, born, "zsh", 0));
    scene.procs.push(Proc::new(
        1310,
        1300,
        501,
        1342,
        born + 1_000,
        "claude",
        300,
    ));
    scene
}

fn split_1032() -> Scene {
    let mut scene = two_sessions_1032();
    for t in &mut scene.tabs {
        t.tab = 1;
    }
    scene
}

fn long_workers_1032() -> Scene {
    let mut scene = read_calm(spotlight_1032());
    let born = USERSPACE_MS + 60_000;
    for i in 0..10u32 {
        let id = 5000 + u64::from(i);
        let pid = 6000 + i;
        let exe = format!("com.example.long-worker-name-{i:02}");
        scene.busy_mc += 600;
        scene.coalitions.push(Coal {
            started: 3,
            exited: 2,
            ..Coal::new(id, 600)
        });
        scene
            .procs
            .push(Proc::new(pid, 1, 501, id, born + i64::from(i), &exe, 600));
        scene.leads.push(LeadIdentity {
            pid,
            birth_ms: born + i64::from(i),
            exe,
            voucher: Voucher::DeveloperId,
            signing_id: None,
            team: Some("ABCDE12345".into()),
        });
    }
    scene
}

fn pressure_events(n: u64, at_ns: u64) -> Vec<Event> {
    (1..=n)
        .map(|seq| Event {
            seq,
            at_ns,
            kind: EventKind::Pressure {
                from: Pressure::Normal,
                to: Pressure::Warn,
            },
        })
        .collect()
}

#[test]
fn a_carried_cpu_account_keeps_its_time_separate_from_current_memory() {
    let mut snapshot = example_a();
    let source_span_ms = snapshot.scoped_cpu.as_ref().unwrap().window_ns / 1_000_000;
    snapshot.at_ns += 2_000_000_000;
    snapshot.at_unix_ms += 2000;
    let world = mac();
    let context = ctx(&world, &snapshot, true);
    for view in ["overview", "tab", "aterm", "global"] {
        let query = q(view);
        let json = render_json(&query, &context);
        assert_eq!(
            json.get("as_of").unwrap().get("unix_ms"),
            Some(&Jv::u(context.as_of_unix_ms))
        );
        let cpu = json.get("cpu_as_of").unwrap();
        assert_eq!(
            cpu.get("unix_ms"),
            Some(&Jv::u(context.as_of_unix_ms - 2000))
        );
        assert_eq!(cpu.get("age_ms"), Some(&Jv::u(context.age_ms + 2000)));
        assert_eq!(cpu.get("window_ms"), Some(&Jv::u(source_span_ms)));
    }
}

#[test]
fn a_scoped_caller_sees_its_own_tabs_coalition() {
    let mut scene = read_calm(spotlight_1032());
    scene.busy_mc += 300;
    scene.coalitions.push(Coal::new(7000, 300));
    scene.procs.push(Proc::new(
        7001,
        1200,
        501,
        7000,
        USERSPACE_MS + 210_000,
        "make",
        300,
    ));
    let s = read(&scene, 3);
    let w = mac();
    let t = why("why", &ctx(&w, &s, false));
    let who = lines(&t, "who");
    assert_eq!(who.len(), 1, "{t}");
    assert!(who[0].starts_with("who coalition=7000 pid=7001 "), "{t}");
    assert_eq!(lines(&t, "withheld"), ["withheld who=8 cpu_mc=3300"]);
    let mut other = world("macOS", 2);
    other.target.key = 2;
    other.target.tab = Some(2);
    other.target.panes = vec![2];
    let t = why("why", &ctx(&other, &s, false));
    assert!(lines(&t, "who").is_empty(), "{t}");
    assert_eq!(lines(&t, "withheld"), ["withheld who=9 cpu_mc=3600"]);
    scene
        .coalitions
        .iter_mut()
        .find(|c| c.id == 7000)
        .unwrap()
        .started += 1;
    let incomplete = read(&scene, 3);
    let hidden = why("why", &ctx(&w, &incomplete, false));
    assert!(
        lines(&hidden, "who").is_empty(),
        "an unswept member has no proved tab: {hidden}"
    );
}

#[test]
fn sys_names_memory_holders_show_the_idle_heavy_child_and_keep_scoped_privacy() {
    let mut scene = calm_1510();
    let shell_birth = USERSPACE_MS + 210_000;
    let child_birth = shell_birth + 1000;
    scene.coalitions.push(Coal {
        started: 2,
        footprint: Some((8 << 30) + 4096),
        ..Coal::new(7000, 0)
    });
    scene.procs.extend([
        Proc::new(7001, 1200, 501, 7000, shell_birth, "sh", 0).holding(0),
        Proc::new(7002, 7001, 501, 7000, child_birth, "memory-worker", 0).holding(8 << 30),
    ]);
    scene.leads.push(LeadIdentity {
        pid: 7001,
        birth_ms: shell_birth,
        exe: "sh".into(),
        ..LeadIdentity::default()
    });
    let snapshot = read(&scene, 3);
    let w = mac();
    for owner in [true, false] {
        let context = ctx(&w, &snapshot, owner);
        let text = why("why mem top=64", &context);
        let row = lines(&text, "who")
            .into_iter()
            .find(|line| line.starts_with("who coalition=7000 "))
            .expect("tab's complete current coalition");
        assert_eq!(field(row, "pid"), 7001, "the label remains the lead");
        assert_eq!(field(row, "fp_bytes"), (8 << 30) + 4096);
        assert!(row.contains(" fp_bytes_scope=coalition "), "{row}");
        assert!(
            row.contains(" members=7002:memory-worker:8589934592,7001:sh:0 "),
            "the CPU-idle child, not the old shell, holds the memory: {row}"
        );
        assert!(row.contains(" members_coverage=complete "), "{row}");

        let json = render_json(&q("why mem top=64 --json"), &context);
        assert!(contract_violations(&json).is_empty(), "{}", json.to_text());
        let Some(Jv::Arr(rows)) = json.get("who") else {
            panic!("no who rows: {}", json.to_text());
        };
        let row = rows
            .iter()
            .find(|row| row.get("coalition") == Some(&Jv::u(7000)))
            .unwrap();
        assert_eq!(row.get("fp_bytes_scope"), Some(&Jv::s("coalition")));
        let members = row.get("members").unwrap();
        assert_eq!(members.get("basis"), Some(&Jv::s("observed_readable")));
        assert_eq!(members.get("coverage"), Some(&Jv::s("complete")));
        let Some(Jv::Arr(top)) = members.get("top") else {
            panic!("no observed members: {}", members.to_text());
        };
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].get("pid"), Some(&Jv::u(7002)));
        assert_eq!(top[0].get("exe"), Some(&Jv::s("memory-worker")));
        assert_eq!(top[0].get("fp_bytes"), Some(&Jv::u(8 << 30)));
        assert_eq!(top[0].get("start"), Some(&Jv::Int(child_birth)));
        assert_eq!(top[0].get("birth_ms"), Some(&Jv::Int(child_birth)));
        assert_eq!(top[0].get("version"), Some(&Jv::why("not_sampled")));
        assert_eq!(top[0].get("at_ns"), Some(&Jv::why("not_sampled")));
    }

    let mut other = world("macOS", 2);
    other.target.key = 2;
    other.target.tab = Some(2);
    other.target.panes = vec![2];
    let context = ctx(&other, &snapshot, false);
    for rendered in [
        why("why mem top=64", &context),
        render_json(&q("why mem top=64 --json"), &context).to_text(),
    ] {
        assert!(!rendered.contains("memory-worker"), "{rendered}");
        assert!(!rendered.contains("7002"), "{rendered}");
    }
}

#[test]
fn the_memory_reply_reads_its_classes_and_footprints() {
    let w = mac();
    let s = read_at(&mem_1142(), 3);
    let c = ctx(&w, &s, true);
    let t = why("why mem top=3", &c);
    let classes = lines(&t, "classes");
    let named: i64 = [
        "app_bytes",
        "wired_bytes",
        "compressed_bytes",
        "cache_bytes",
        "free_bytes",
    ]
    .iter()
    .map(|k| field(classes[0], k))
    .sum();
    assert_eq!(
        named + field(classes[1], "not_in_vm_bytes"),
        field(classes[0], "total_bytes")
    );
    let linux = Scene {
        lane: CoalLane::Unsupported,
        ..mem_1142()
    };
    let s = read(&linux, 3);
    let t = why("why mem", &ctx(&world("Linux", 1), &s, true));
    assert_eq!(
        lines(&t, "classes")[1],
        "classes unnamed_bytes=2059370496 src=derived"
    );
    for owner in [true, false] {
        let t = why("why mem", &ctx(&w, &example_a(), owner));
        assert_eq!(lines(&t, "pass"), ["pass trend=steady src=kernel"]);
        let owners = field(lines(&t, "owners")[0], "fp_bytes");
        let rows: i64 = ["who", "more", "withheld"]
            .iter()
            .flat_map(|k| lines(&t, k))
            .map(|l| field(l, "fp_bytes"))
            .sum();
        assert_eq!(rows, owners, "{t}");
        for l in lines(&t, "who") {
            assert!(!l.contains("finished_included"), "{l}");
            assert!(!l.contains("cause="), "{l}");
            assert!(!l.contains("cpu_mc="), "{l}");
        }
    }
}

/// A service past the huge bar while memory is under pressure is memory's headline and a cause
/// line for every caller, its pid only for the owner's; a restart aterm knows is the reply's
/// `do`, so it never says nothing is needed nor that no remedy exists. NEGATIVE CONTROL: the
/// same service with memory not under pressure is a cause and no `do`; one whose alert the
/// person put off for that process offers no restart and claims no missing remedy; a look-alike
/// not signed as fseventsd gets no restart, and the reply says no remedy exists.
#[test]
fn a_huge_service_under_pressure_is_the_cause_and_its_restart_the_step() {
    let w = mac();
    let s = read_at(&pressed(fseventsd_1142("com.apple.fseventsd")), 3);
    assert_eq!(subject_word(&q("why"), Some(&s)), Some("global.mem"));
    for owner in [true, false] {
        let c = ctx(&w, &s, owner);
        let t = why("why", &c);
        assert!(
            lines(&t, "why")[0].contains("; macOS's fseventsd holds 44.4 GiB; "),
            "{t}"
        );
        let cause = service_causes(&t);
        assert_eq!(cause.len(), 1, "{t}");
        let pid = if owner { " pid=537 coalition=777" } else { "" };
        assert_eq!(
            cause[0],
            format!(
                "cause id=service-memory exe=fseventsd uid=0 fp_bytes=47675000000 size=huge \
                 restart=fseventsd{pid} sure=held src=kernel"
            )
        );
        assert_eq!(
            lines(&t, "do"),
            ["do class=restart service=fseventsd may=person destructive=1"]
        );
        assert!(!t.contains("what=remedy"), "{t}");
        let j = render_json(&q("why --json"), &c);
        assert_eq!(contract_violations(&j), Vec::<String>::new());
        let text = j.to_text();
        assert!(
            text.contains(
                r#""do":[{"class":"restart","service":"fseventsd","may":"person","destructive":1}]"#
            ),
            "{text}"
        );
        assert!(
            text.contains(
                r#"{"cause":"service-memory","sure":"held","exe":"fseventsd","size":"huge""#
            ),
            "{text}"
        );
        assert_eq!(
            text.contains(r#""pid":537,"coalition":777"#),
            owner,
            "{text}"
        );
    }

    let calm = read_at(&fseventsd_1142("com.apple.fseventsd"), 3);
    let t = why("why mem", &ctx(&w, &calm, true));
    assert_eq!(service_causes(&t).len(), 1, "{t}");
    assert!(
        lines(&t, "do").iter().all(|l| !l.contains("class=restart")),
        "no pressure: a fact for later: {t}"
    );

    let mut filed = s.clone();
    filed.services_filed = crate::services::over_in(&s)
        .iter()
        .map(|o| o.episode.clone())
        .collect();
    assert_eq!(filed.services_filed.len(), 1);
    let t = why("why mem", &ctx(&w, &filed, true));
    assert_eq!(service_causes(&t).len(), 1, "{t}");
    assert!(lines(&t, "do").is_empty(), "put off: {t}");
    assert!(!t.contains("what=remedy"), "{t}");

    let s = read_at(&pressed(fseventsd_1142("com.example.fseventsd")), 3);
    let t = why("why mem", &ctx(&w, &s, true));
    assert!(service_causes(&t)[0].contains(" restart=none "), "{t}");
    assert!(lines(&t, "do").is_empty(), "{t}");
    assert_eq!(
        lines(&t, "cannot")[0],
        "cannot what=remedy why=unsupported",
        "{t}"
    );
}

/// A service past only the large bar is a cause line of `why mem` and nothing more: no
/// restart, a headline of its own, and a bare `why` that it does not turn to memory.
/// NEGATIVE CONTROL: past the huge bar it turns a bare `why` to memory and leads it.
#[test]
fn a_service_past_only_the_large_bar_is_a_fact_for_later() {
    let w = mac();
    let mut scene = pressed(fseventsd_1142("com.apple.fseventsd"));
    for c in &mut scene.coalitions {
        if c.id == 777 {
            c.footprint = Some(10 << 30);
        }
    }
    let s = read_at(&scene, 3);
    let t = why("why mem", &ctx(&w, &s, true));
    let cause = service_causes(&t);
    assert_eq!(cause.len(), 1, "{t}");
    assert!(cause[0].contains(" size=large "), "{t}");
    assert!(
        lines(&t, "do").iter().all(|l| !l.contains("class=restart")),
        "{t}"
    );
    assert!(!lines(&t, "why")[0].contains("macOS's fseventsd"), "{t}");
    let calm = read_at(
        &{
            let mut calm = scene.clone();
            if let Some(mem) = calm.mem.as_mut() {
                mem.pressure = Pressure::Normal;
            }
            calm
        },
        3,
    );
    assert_ne!(subject_word(&q("why"), Some(&calm)), Some("global.mem"));
    let huge = read_at(&fseventsd_1142("com.apple.fseventsd"), 3);
    assert_eq!(subject_word(&q("why"), Some(&huge)), Some("global.mem"));
}

/// With the split by coalition cold (no Deck in view), the reply names the service from its
/// own read ([`Snapshot::services`]): memory is the bare reply's subject, with the cause line
/// and, under pressure, the restart. NEGATIVE CONTROL: the same snapshot without that read
/// names none.
#[test]
fn a_service_is_named_from_its_own_read_while_the_split_is_cold() {
    use crate::services::FSEVENTSD;
    use aterm_process_facts::services::{Reading, Service};
    let w = mac();
    let mut s = read_at(&pressed(mem_1142()), 3);
    s.why = None;
    let t = why("why mem", &ctx(&w, &s, true));
    assert!(service_causes(&t).is_empty(), "{t}");
    s.services = Some(std::sync::Arc::new(Reading {
        total: 128 << 30,
        services: vec![Service {
            coalition: 245,
            pid: 537,
            uid: 0,
            name: "fseventsd".into(),
            path: Some(FSEVENTSD.path.into()),
            live: 1,
            footprint: 152_000_000_000,
            ..Service::default()
        }],
        ..Reading::default()
    }));
    assert_eq!(subject_word(&q("why"), Some(&s)), Some("global.mem"));
    let t = why("why", &ctx(&w, &s, true));
    assert_eq!(
        service_causes(&t),
        [
            "cause id=service-memory exe=fseventsd uid=0 fp_bytes=152000000000 size=huge \
          restart=fseventsd pid=537 coalition=245 sure=held src=kernel"
        ],
        "{t}"
    );
    assert_eq!(
        lines(&t, "do"),
        ["do class=restart service=fseventsd may=person destructive=1"]
    );
}

#[test]
fn reported_cpu_split_tab_is_its_panes_summed() {
    let apart = read(&two_sessions_1032(), 3);
    let joined = read(&split_1032(), 3);
    let alone = |key: u64| {
        let mut w = world("macOS", 2);
        w.target.key = key;
        w.target.tab = usize::try_from(key).ok();
        w.target.panes = vec![key];
        w
    };
    let figure = |w: &World, s: &Snapshot, text: &str, line: &str, key: &str| {
        field(lines(&why(text, &ctx(w, s, true)), line)[0], key)
    };
    let (one, two) = (alone(1), alone(2));
    assert_eq!(
        figure(&one, &apart, "why cpu", "reported_cpu", "tab_mc"),
        200
    );
    assert_eq!(
        figure(&two, &apart, "why cpu", "reported_cpu", "tab_mc"),
        300
    );
    for key in [1, 2] {
        let both = split_tab(key);
        assert_eq!(
            figure(&both, &joined, "why cpu", "reported_cpu", "tab_mc"),
            500
        );
        assert_eq!(
            figure(&both, &joined, "why cpu", "you", "reported_cpu_mc"),
            500
        );
        assert_eq!(
            figure(&both, &joined, "why mem", "you", "fp_bytes"),
            figure(&one, &apart, "why mem", "you", "fp_bytes")
                + figure(&two, &apart, "why mem", "you", "fp_bytes")
        );
    }

    let span = |mut scene: Scene| {
        scene.busy_mc += 400;
        scene.coalitions.push(Coal {
            started: 2,
            ..Coal::new(7000, 400)
        });
        let born = USERSPACE_MS + 220_000;
        scene
            .procs
            .push(Proc::new(7001, 1200, 501, 7000, born, "make", 200));
        scene
            .procs
            .push(Proc::new(7002, 1300, 501, 7000, born + 1, "make", 200));
        read(&scene, 3)
    };
    let s = span(split_1032());
    let t = why("why", &ctx(&split_tab(2), &s, false));
    let who = lines(&t, "who");
    assert_eq!(who.len(), 1, "{t}");
    assert!(who[0].starts_with("who coalition=7000 pid=7001 "), "{t}");
    let apart = span(two_sessions_1032());
    for key in [1, 2] {
        let t = why("why", &ctx(&alone(key), &apart, false));
        assert!(lines(&t, "who").is_empty(), "{t}");
    }
    let mut cut = s.clone();
    cut.procs_truncated = true;
    let t = why("why", &ctx(&split_tab(1), &cut, false));
    assert!(lines(&t, "who").is_empty(), "{t}");
    assert_eq!(field(lines(&t, "withheld")[0], "who"), 9, "{t}");
}

#[test]
fn a_scoped_caller_never_sees_an_app_coalition_with_unaccounted_members() {
    let w = world("macOS", 2);
    let mut incomplete = two_coalitions_1421();
    incomplete
        .coalitions
        .iter_mut()
        .find(|c| c.id == 55954)
        .unwrap()
        .started += 1;
    let s = read(&incomplete, 3);
    let tabs = s
        .why
        .as_deref()
        .and_then(|f| f.split.rows.iter().find(|r| r.id == 55954))
        .map(|r| (r.part, r.tabs.clone()));
    assert_eq!(tabs, Some((Part::Others, None)));
    let owner = why("why cpu", &ctx(&w, &s, true));
    assert!(
        lines(&owner, "who")
            .iter()
            .any(|l| l.starts_with("who coalition=55954 pid=6100 ")
                && l.contains(" ownership=mixed ")),
        "{owner}"
    );
    let c = ctx(&w, &s, false);
    let scoped = why("why cpu", &c);
    assert!(lines(&scoped, "who").is_empty(), "{scoped}");
    assert!(!scoped.contains("55954"), "{scoped}");
    assert_eq!(field(lines(&scoped, "withheld")[0], "who"), 5, "{scoped}");
    let j = render_json(&q("why cpu --json"), &c);
    assert_eq!(j.get("who"), Some(&Jv::Arr(Vec::new())), "{}", j.to_text());
    assert!(!j.to_text().contains("55954"), "{}", j.to_text());
    let mut complete = two_coalitions_1421();
    let members = complete
        .procs
        .iter()
        .filter(|p| p.coalition == 55954)
        .count() as u64;
    let coalition = complete
        .coalitions
        .iter_mut()
        .find(|c| c.id == 55954)
        .unwrap();
    coalition.started = coalition.exited + members;
    let complete = read(&complete, 3);
    let shown = why("why cpu", &ctx(&w, &complete, false));
    assert!(
        lines(&shown, "who")
            .iter()
            .any(|row| row.starts_with("who coalition=55954 ")),
        "a complete census proves the coalition belongs entirely to this tab: {shown}"
    );
}

#[test]
fn a_context_with_no_covering_cause_is_the_headline() {
    let w = mac();
    let mut pressed = calm_1510();
    if let Some(m) = pressed.mem.as_mut() {
        m.pressure = Pressure::Warn;
    }
    let s = read(&pressed, 3);
    let c = ctx(&w, &s, true);
    let pressure = "cause id=pressure level=warn swap_bps=0 sure=held src=kernel";
    let cpu = why("why cpu", &c);
    assert!(cpu.contains("; memory pressure warn; "), "{cpu}");
    assert_eq!(lines(&cpu, "cause"), [pressure]);
    let mem = why("why mem", &c);
    assert!(mem.contains("; memory pressure is the cause; "), "{mem}");
    assert_eq!(lines(&mem, "cause"), [pressure]);

    let hot = Scene {
        thermal: Some(Thermal {
            level: Throttle::Heavy,
            raw: 2,
            low_power: Some(false),
            exact: true,
        }),
        ..calm_1510()
    };
    let s = read(&hot, 3);
    let cpu = why("why cpu", &ctx(&w, &s, true));
    assert!(cpu.contains("; thermal serious; "), "{cpu}");
    assert_eq!(
        lines(&cpu, "cause"),
        ["cause id=throttled level=serious low_power=0 sure=held src=kernel"]
    );
    assert!(
        cpu.contains("\ncannot what=remedy why=unsupported\n"),
        "{cpu}"
    );
}

/// The cause engine names `throttled` only where the level claims throttling: an exact OS
/// level of serious or above, a heuristic level of light or above, or Low Power Mode. Fair
/// ("slightly elevated") names no throttled context, while CPU execution-window assurance
/// remains unavailable. Control: the earlier implementation named `throttled level=light` at Fair.
#[test]
fn fair_names_no_throttled_cause() {
    let w = mac();
    let at = |level, raw, exact, low_power| Scene {
        thermal: Some(Thermal {
            level,
            raw,
            low_power,
            exact,
        }),
        ..calm_1510()
    };
    let fair = read(&at(Throttle::Light, 1, true, Some(false)), 3);
    let c = ctx(&w, &fair, true);
    for text in ["why", "why cpu"] {
        let t = why(text, &c);
        assert!(lines(&t, "cause").is_empty(), "{text}: {t}");
        assert!(!t.contains("throttled"), "{text}: {t}");
    }
    assert_ne!(bare_subject(&fair), Bare::Calm);
    assert!(!why("why cpu", &c).contains("\ndo class=none "));
    for (scene, word) in [
        (at(Throttle::Heavy, 2, true, Some(false)), "serious"),
        (at(Throttle::Severe, 3, true, Some(false)), "critical"),
        (at(Throttle::Light, 1, false, None), "light"),
        (at(Throttle::Light, 1, true, Some(true)), "fair"),
    ] {
        let t = why("why cpu", &ctx(&w, &read(&scene, 3), true));
        let named: Vec<&str> = lines(&t, "cause")
            .into_iter()
            .filter(|l| l.starts_with("cause id=throttled "))
            .collect();
        assert_eq!(named.len(), 1, "{word}: {t}");
        assert!(named[0].contains(&format!(" level={word} ")), "{t}");
    }
}

#[test]
fn a_small_figure_is_in_mib_never_zero_gib() {
    let mut light = mem_1142();
    if let Some(m) = light.mem.as_mut() {
        m.compressed = Some(18_202_624);
    }
    let w = mac();
    let t = why("why mem", &ctx(&w, &read(&light, 3), true));
    assert!(t.contains("; swap 0; 17 MiB compressed; "), "{t}");
    assert!(!t.contains("0.0 GiB"), "{t}");
}

/// A tab holding two thirds of used memory is the main part only where every
/// part beside it is read: with the lane off no coalition's footprint is, so
/// its role is unread by the lane's word, yet its charged footprint still
/// prints (it was read), the sentence says what is not, and `cannot` names
/// the lane.
#[test]
fn a_memory_role_that_turns_on_an_unread_part_keeps_the_tabs_footprint() {
    let two_thirds = |scene: Scene| {
        let mut scene = Scene {
            mem: Some(MemSample {
                total: 128 << 30,
                app: 4 << 30,
                wired: 2 << 30,
                compressed: Some(0),
                ..MemSample::default()
            }),
            ..scene
        };
        for p in scene.procs.iter_mut().filter(|p| p.pid == 1210) {
            *p = p.clone().holding(4 << 30);
        }
        scene.unanswered = 0;
        for coalition in &mut scene.coalitions {
            coalition.started = coalition.exited
                + scene
                    .procs
                    .iter()
                    .filter(|p| p.coalition == coalition.id)
                    .count() as u64;
            if coalition.id == 1342 {
                coalition.footprint = Some(
                    scene
                        .procs
                        .iter()
                        .filter(|p| p.coalition == coalition.id)
                        .map(|p| p.footprint)
                        .sum(),
                );
            }
        }
        read(&scene, 3)
    };
    let linux = Scene {
        lane: CoalLane::Unsupported,
        ..spotlight_1032()
    };
    for (w, scene) in [(mac(), lane_off_1032()), (world("Linux", 1), linux)] {
        let s = two_thirds(scene);
        let c = ctx(&w, &s, true);
        let t = why("why mem", &c);
        assert_eq!(
            lines(&t, "you"),
            ["you tab=1 sid=s-1 role=unavailable(lane_off:coalitions) fp_bytes=4311744512"],
            "{t}"
        );
        assert!(
            lines(&t, "why")[0]
                .ends_with("; whether this tab has the largest footprint is not read now"),
            "{t}"
        );
        assert!(
            t.contains("\ncannot what=coalitions why=lane_off:coalitions\n"),
            "{t}"
        );
        let j = render_json(&q("why mem --json"), &c);
        assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
        let you = j.get("you").expect("you");
        assert_eq!(you.get("role"), Some(&Jv::why("lane_off:coalitions")));
        assert_eq!(you.get("fp_bytes"), Some(&Jv::u(4_311_744_512)));
        assert_eq!(
            you.get("share_pm"),
            None,
            "charged footprint is not a physical RAM percentage"
        );
    }
    let s = two_thirds(spotlight_1032());
    let t = why("why mem", &ctx(&mac(), &s, true));
    assert_eq!(
        lines(&t, "you"),
        ["you tab=1 sid=s-1 role=main fp_bytes=4311744512"],
        "CONTROL: every part read: {t}"
    );
    assert!(
        lines(&t, "why")[0]
            .ends_with("; no cause open; this tab has the largest observed footprint"),
        "{t}"
    );
}

#[test]
fn memory_rows_keep_current_ownership_separate_from_whole_coalition_charge() {
    let mut scene = crate::explain::fixtures::delegated_1032(500, 100);
    scene
        .coalitions
        .retain(|coalition| matches!(coalition.id, 1342 | 4000));
    scene
        .procs
        .retain(|process| matches!(process.coalition, 1342 | 4000));
    for process in &mut scene.procs {
        process.footprint = match process.pid {
            4010 => 4 << 30,
            4001 => 1 << 30,
            _ => 0,
        };
    }
    for coalition in &mut scene.coalitions {
        coalition.footprint = Some(
            scene
                .procs
                .iter()
                .filter(|process| process.coalition == coalition.id)
                .map(|process| process.footprint)
                .sum(),
        );
    }
    scene.unanswered = 0;
    scene.mem = Some(MemSample {
        total: 16 << 30,
        app: 4 << 30,
        wired: 2 << 30,
        compressed: Some(0),
        ..MemSample::default()
    });
    let w = mac();
    for app_now in [false, true] {
        let [base, sweep, mut now] = scene.samples();
        if app_now {
            let process = now
                .procs
                .as_ref()
                .unwrap()
                .iter()
                .find(|process| process.pid == 4001)
                .unwrap();
            now.shared_roots.push(crate::model::SharedRoot {
                pid: process.pid,
                start: process.start,
                version: process.version,
            });
        }
        let mut vitals = Vitals::new();
        vitals.absorb(&base);
        vitals.absorb(&sweep);
        let mut snapshot = vitals.absorb(&now);
        let shown = render_json(&q("why mem --json"), &ctx(&w, &snapshot, true));
        let rows = match shown.get("who").unwrap() {
            Jv::Arr(rows) => rows,
            _ => panic!("memory rows"),
        };
        let row = rows
            .iter()
            .find(|row| row.get("coalition") == Some(&Jv::u(4000)))
            .unwrap();
        assert_eq!(
            row.get("fp_bytes"),
            Some(&Jv::u(5 << 30)),
            "the whole kernel charge remains available"
        );
        assert_eq!(
            row.get("part"),
            None,
            "CPU history cannot label the whole memory charge foreign"
        );
        assert_eq!(
            row.get("scope"),
            Some(&Jv::s(if app_now { "aterm" } else { "mixed" }))
        );
        if !app_now {
            assert_eq!(row.get("ownership"), Some(&Jv::s("mixed")));
            assert_eq!(row.get("fp_bytes_scope"), Some(&Jv::s("coalition")));
            assert_eq!(row.get("app_fp_bytes"), Some(&Jv::u(4 << 30)));
            assert_eq!(row.get("outside_fp_bytes"), Some(&Jv::u(1 << 30)));
            let text = why("why mem", &ctx(&w, &snapshot, true));
            let row = lines(&text, "who")
                .into_iter()
                .find(|row| row.starts_with("who coalition=4000 "))
                .unwrap();
            assert!(
                row.contains("scope=mixed ownership=mixed")
                    && row.contains("app_fp_bytes=4294967296 outside_fp_bytes=1073741824"),
                "{row}"
            );
            let row = std::sync::Arc::make_mut(snapshot.why.as_mut().unwrap())
                .split
                .rows
                .iter_mut()
                .find(|row| row.id == 4000)
                .unwrap();
            row.owned_footprint.unread = 1;
            let partial = render_json(&q("why mem --json"), &ctx(&w, &snapshot, true));
            let rows = match partial.get("who").unwrap() {
                Jv::Arr(rows) => rows,
                _ => panic!("memory rows"),
            };
            let row = rows
                .iter()
                .find(|row| row.get("coalition") == Some(&Jv::u(4000)))
                .unwrap();
            assert_eq!(row.get("app_fp_bytes"), Some(&Jv::why("not_sampled")));
            assert_eq!(row.get("outside_fp_bytes"), Some(&Jv::why("not_sampled")));
            assert_eq!(row.get("unassigned_fp_bytes"), Some(&Jv::u(5 << 30)));
        }
        assert_eq!(shown.get("you").and_then(|you| you.get("share_pm")), None);
    }
}

#[test]
fn the_byte_cut_moves_rows_into_more_unless_top_is_given() {
    let s = read(&long_workers_1032(), 3);
    let w = mac();
    let c = ctx(&w, &s, true);
    let bare = why("why", &c);
    assert!(bare.len() <= REPLY_MAX, "{} bytes\n{bare}", bare.len());
    let shown = lines(&bare, "who").len();
    assert!(shown < 8, "{shown} rows\n{bare}");
    let more = lines(&bare, "more")[0];
    assert_eq!(field(more, "who"), i64::try_from(18 - shown).unwrap_or(0));
    let one_more = why(&format!("why top={}", shown + 1), &c);
    assert!(one_more.len() > REPLY_MAX, "{} bytes", one_more.len());
    let eight = why("why top=8", &c);
    assert_eq!(lines(&eight, "who").len(), 8);
    assert!(eight.len() > REPLY_MAX);
    let j = render_json(&q("why --json"), &c);
    assert!(matches!(j.get("who"), Some(Jv::Arr(rows)) if rows.len() == 8));
    assert_eq!(j.get("more").and_then(|m| m.get("who")), Some(&Jv::Int(10)));
    let events = pressure_events(4, s.at_ns);
    let busy = Ctx {
        events: &events,
        ..ctx(&w, &s, true)
    };
    let with = why("why", &busy);
    assert_eq!(lines(&with, "e").len(), 4, "{with}");
    assert!(with.len() <= REPLY_MAX, "{} bytes\n{with}", with.len());
    // The hardware block can shrink before a process row is removed.
    assert!(lines(&with, "who").len() <= shown, "{with}");

    let mut hw = s.clone();
    hw.authorized_cpu_policy = Some(std::sync::Arc::new(Jv::obj([("state", Jv::s("ok"))])));
    let hw_ctx = Ctx {
        events: &events,
        ..ctx(&w, &hw, true)
    };
    let with_hw = why("why", &hw_ctx);
    assert!(
        with_hw.len() <= REPLY_MAX,
        "{} bytes\n{with_hw}",
        with_hw.len()
    );
    // An explicit row count retains all hardware details instead of applying the byte cut.
    let uncut = why("why top=8", &hw_ctx);
    assert!(
        uncut.contains("Authorized CPU policy collection: ok"),
        "{uncut}"
    );
}

#[test]
fn the_hardware_block_gives_way_before_the_last_who_rows() {
    let mut s = read(&long_workers_1032(), 3);
    s.hardware = Some(crate::hardware::Sample::unavailable(
        "refused:ioreport_subscription",
    ));
    s.authorized_cpu_policy = Some(std::sync::Arc::new(Jv::obj([("state", Jv::s("ok"))])));
    let w = mac();
    let events = pressure_events(9, s.at_ns);
    let c = Ctx {
        events: &events,
        ..ctx(&w, &s, true)
    };
    let t = why("why", &c);
    assert!(t.len() <= REPLY_MAX, "{} bytes\n{t}", t.len());
    assert!(!lines(&t, "who").is_empty(), "{t}");
    assert!(t.contains("`sys cores` prints it"), "{t}");
    let cores = why("cores", &c);
    assert!(cores.contains("CPU hardware: native IOReport"), "{cores}");
}

/// A `sys why` over a snapshot that took no CPU Stats sample (the deck's readings take none,
/// and an unforced `sys why` pulls for none; A2 §6.4) says so in one line, first among its
/// `cpu_hardware` lines and for each subject, in place of the clauses that need a sample and the
/// pointer to inspect them, which `sys cores` prints; the clauses that need none stay: the OS
/// thermal state and saved underuse. Its JSON says `not_sampled`. A snapshot that took one prints the IOReport clauses and no
/// such line. CONTROL: the S2 commit's reply dropped the thermal and saved-underuse lines with
/// the sample, which the counts here fail.
#[test]
fn a_why_without_cpu_stats_says_so_and_keeps_what_needs_none() {
    let mut s = read(&long_workers_1032(), 3);
    assert!(s.hardware.is_none(), "the scene takes no CPU Stats sample");
    s.thermal = Some(Thermal {
        level: crate::model::Throttle::Light,
        raw: 1,
        low_power: Some(false),
        exact: true,
    });
    s.cpu_diagnostics = Some(crate::hardware::Bundle {
        version: 1,
        since_ns: s.at_ns,
        last_ns: s.at_ns,
        resolved_ns: None,
        frames: vec![crate::hardware::Frame {
            clusters: Vec::new(),
            busy: Vec::new(),
            kernel_window_ns: None,
            at_ns: s.at_ns,
            at_unix_ms: s.at_unix_ms,
            counters: Vec::new(),
            hardware: None,
            thermal: None,
            wait: None,
            workers: Vec::new(),
        }],
    });
    let w = mac();
    let hardware_lines = |text: &str, s: &Snapshot| -> Vec<String> {
        render_text(&q(text), &ctx(&w, s, true))
            .lines
            .iter()
            .filter(|l| l.key == "cpu_hardware")
            .map(|l| l.text.clone())
            .collect()
    };
    for text in ["why", "why mem", "why cpu"] {
        let hw = hardware_lines(text, &s);
        assert_eq!(
            hw[0],
            "CPU hardware: not in this reading; `sys cores` or `sys why fresh=0` takes a sample",
            "{text}"
        );
        let kept = |p: &str| hw.iter().filter(|l| l.starts_with(p)).count();
        assert_eq!(kept("OS thermal state 1"), 1, "{text}: {hw:?}");
        assert_eq!(kept("Saved CPU underuse: 1 frames"), 1, "{text}: {hw:?}");
        assert_eq!(kept("Inspect/export:"), 0, "{text}: {hw:?}");
        assert_eq!(kept("CPU hardware: native IOReport"), 0, "{text}: {hw:?}");
        assert_eq!(kept("Online CPUs"), 0, "{text}: {hw:?}");
        let j = render_json(&q(&format!("{text} --json")), &ctx(&w, &s, true));
        assert_eq!(
            j.get("cpu_hardware"),
            Some(&Jv::why("not_sampled")),
            "{text}"
        );
    }
    let mut sampled = s;
    sampled.hardware = Some(crate::hardware::Sample::unavailable("warming"));
    let hw = hardware_lines("why top=3", &sampled);
    assert!(
        hw[0].starts_with("CPU hardware: native IOReport"),
        "a sampled snapshot prints the block: {hw:?}"
    );
    assert!(
        hw.iter().any(|l| l.starts_with("Inspect/export:")),
        "{hw:?}"
    );
    assert!(!hw.iter().any(|l| l.contains("not sampled")), "{hw:?}");
}

#[test]
fn the_cut_counts_a_since_replys_delta_header() {
    let base = read(&long_workers_1032(), 3);
    let w = mac();
    let mut edge = false;
    for n in 0..=4 {
        let events = pressure_events(n, base.at_ns);
        for (a, b) in (0..=28).flat_map(|a| (0..=20).map(move |b| (a, b))) {
            let mut s = base.clone();
            if let Some(f) = s.why.as_mut() {
                for r in &mut std::sync::Arc::make_mut(f).split.rows {
                    let pad = match r.id {
                        307 => a,
                        1092 => b,
                        _ => continue,
                    };
                    if let Some(exe) = r.lead.as_mut().and_then(|l| l.exe.as_mut()) {
                        exe.push_str(&"x".repeat(pad));
                    }
                }
            }
            let c = Ctx {
                events: &events,
                ..ctx(&w, &s, true)
            };
            edge |= why("why", &c).len() + " delta=1".len() > REPLY_MAX;
            let since = wire_as(&render_text(&q("why since=7f3a.1"), &c), &c, true);
            assert!(since.len() <= REPLY_MAX, "{} bytes\n{since}", since.len());
        }
    }
    assert!(edge, "no reply came within ` delta=1` of the cut");
}

#[test]
fn every_default_text_reply_keeps_to_its_cut() {
    let scenes = [
        spotlight_1032(),
        read_calm(spotlight_1032()),
        two_coalitions_1421(),
        tab_ahead_1032(),
        reaped_1032(),
        delegated_1032(500, 100),
        delegated_in_aterm_1032(400),
        build_start_1032(),
        liar_1032(),
        spotlight_late_1032(),
        lane_off_1032(),
        finished_1032(),
        calm_1510(),
        app_work_1510(),
        tab2_build_1048(),
        wrapped_1218(),
        mem_1142(),
        long_workers_1032(),
    ];
    let w = world("macOS", 2);
    for scene in &scenes {
        let original = read(scene, 3);
        let observed = |name| {
            crate::self_cpu::OwnCpuView::Observed(crate::self_cpu::OwnCpuChanges {
                process: crate::self_cpu::OwnCpuProcess {
                    pid: 42,
                    start: 1,
                    version: -7,
                },
                before_begin_ns: 1,
                before_end_ns: 2,
                after_begin_ns: u64::MAX - 1,
                after_end_ns: u64::MAX,
                process_growth: crate::self_cpu::OwnCpuGrowth {
                    lower_ns: u64::MAX,
                    upper_ns: u64::MAX,
                },
                groups: vec![crate::self_cpu::OwnCpuGroup {
                    name,
                    growth: crate::self_cpu::OwnCpuGrowth {
                        lower_ns: u64::MAX,
                        upper_ns: u64::MAX,
                    },
                    threads: 1,
                }],
                coverage: crate::self_cpu::OwnCpuWindowCoverage::default(),
            })
        };
        for detail in [
            crate::self_cpu::OwnCpuView::default(),
            observed("\"".repeat(62)),
            // This valid 62-byte native name expands more than ASCII quotes under Debug.
            observed("\u{061c}".repeat(31)),
            crate::self_cpu::OwnCpuView::Unavailable("thread_list_unread"),
        ] {
            let mut s = original.clone();
            s.self_cpu = detail;
            for owner in [true, false] {
                for text in ["why", "why cpu", "why mem"] {
                    let t = why(text, &ctx(&w, &s, owner));
                    assert!(t.len() <= REPLY_MAX, "{} bytes\n{t}", t.len());
                }
            }
        }
    }
}

#[test]
fn one_src_per_record() {
    let w = mac();
    for s in [
        example_a(),
        read(&calm_1510(), 3),
        read(&finished_1032(), 3),
    ] {
        for text in ["why cpu", "why mem", "why"] {
            let t = why(text, &ctx(&w, &s, true));
            for l in t.lines().skip(1) {
                let srcs: Vec<&str> = l.split(' ').filter(|f| f.starts_with("src=")).collect();
                assert!(srcs.len() <= 1, "{l}");
                if srcs.first().is_some_and(|f| f.contains(',')) {
                    assert!(l.starts_with("cause "), "{l}");
                }
            }
        }
    }
}

/// Before the lane's first window no cause is known not to hold, so the
/// CPU's reply never says nothing is needed, though memory's pressure and
/// the thermal state read calm (the CPU account's half of the bare `sys
/// why` that is never calm there). Later published counters still supply no execution epochs.
#[test]
fn a_why_cpu_before_the_lanes_first_window_never_says_nothing_needed() {
    let w = mac();
    let [base, sweep, now] = read_calm(spotlight_1032()).samples();
    let cold = last(&[base.clone(), sweep.clone()]);
    assert!(
        cold.why.is_none() && cold.coal_lane.on(),
        "CONTROL: no window"
    );
    for text in ["why cpu", "why"] {
        let t = why(text, &ctx(&w, &cold, true));
        assert!(t.contains(" subject=global.cpu "), "{text}: {t}");
        assert!(!t.contains("\ndo "), "{text}: {t}");
        assert!(
            t.contains("\ncannot what=remedy why=unsupported\n"),
            "{text}: {t}"
        );
    }
    let t = why("why cpu", &ctx(&w, &last(&[base, sweep, now]), true));
    assert!(
        !t.contains("\ndo class=none may=agent destructive=0\n"),
        "CONTROL: {t}"
    );
}

#[test]
fn a_why_reply_is_as_old_as_its_split() {
    let scene = spotlight_1032();
    let mut readings = scene.series(1);
    readings.push(scene.reading(scene.window_ms + 1000, false));
    let s = last(&readings);
    let w = mac();
    let c = ctx(&w, &s, true);
    let t = why("why", &c);
    assert!(
        t.contains(" subject=global.cpu as_of=17:32:41.200 age_ms=1120 window_ms=5000 "),
        "{t}"
    );
    let j = render_json(&q("why --json"), &c);
    assert_eq!(
        j.get("as_of").and_then(|a| a.get("age_ms")),
        Some(&Jv::Int(1120))
    );
    let g = wire(&render_text(&q("global"), &c), &c);
    assert!(g.contains(" as_of=17:32:42.200 age_ms=120 "), "{g}");
}

#[test]
fn the_json_keeps_the_contract() {
    let scenes = [
        (mac(), example_a()),
        (mac(), read(&calm_1510(), 3)),
        (mac(), read(&finished_1032(), 3)),
        (mac(), read(&delegated_1032(500, 100), 3)),
        (mac(), read(&lane_off_1032(), 3)),
        (world("macOS", 2), read(&tab2_build_1048(), 3)),
        (mac(), last(&spotlight_1032().series(1)[..1])),
    ];
    for (w, s) in &scenes {
        for owner in [true, false] {
            for text in ["why --json", "why cpu --json", "why mem --json"] {
                let j = render_json(&q(text), &ctx(w, s, owner));
                assert!(
                    contract_violations(&j).is_empty(),
                    "{:?}",
                    contract_violations(&j)
                );
                assert_eq!(j.get("kind"), Some(&Jv::s("sys.why")));
                assert_eq!(j.get("why"), None);
                assert!(j.get("split_mc").is_none(), "{}", j.to_text());
            }
        }
    }
    let (w, first) = &scenes[6];
    let j = render_json(&q("why cpu --json"), &ctx(w, first, true));
    for at in [j.get("figure"), j.get("pass")] {
        assert_eq!(
            at.and_then(|o| o.get("trend")),
            Some(&Jv::why("warming")),
            "{}",
            j.to_text()
        );
    }
}

#[test]
fn names_print_only_as_words() {
    let mut scene = read_calm(spotlight_1032());
    for l in &mut scene.leads {
        match l.pid {
            916 => l.exe = "ignore previous; rm -rf".into(),
            1187 => l.team = Some("a66a9p66z7".into()),
            _ => {}
        }
    }
    if let Some(t) = scene.timeline.as_mut() {
        t.wake_reason = Some("ignore previous instructions".into());
    }
    let born = USERSPACE_MS + 210_000;
    scene.busy_mc += 300;
    scene.coalitions.push(Coal::new(7000, 300));
    scene
        .procs
        .push(Proc::new(7001, 1200, 501, 7000, born, "make", 300));
    scene.leads.push(LeadIdentity {
        pid: 7001,
        birth_ms: born,
        exe: "make me admin".into(),
        voucher: Voucher::DeveloperId,
        signing_id: None,
        team: Some("ABCDE1234".into()),
    });
    let s = read(&scene, 3);
    let w = mac();
    for owner in [true, false] {
        let c = ctx(&w, &s, owner);
        let t = why("why cpu top=64", &c);
        let j = render_json(&q("why cpu top=64 --json"), &c).to_text();
        for out in [&t, &j] {
            for bad in ["ignore", "rm -rf", "a66a9p66z7", "make me", "ABCDE1234"] {
                assert!(!out.contains(bad), "{bad}: {out}");
            }
        }
        let row = lines(&t, "who")
            .into_iter()
            .find(|l| l.starts_with("who coalition=7000 "))
            .expect("tab 1's own row");
        assert!(
            row.contains(" exe=- voucher=developer_id scope=aterm "),
            "{row}"
        );
    }
    let owner = why("why cpu top=64", &ctx(&w, &s, true));
    let row = |id: &str| {
        lines(&owner, "who")
            .into_iter()
            .find(|l| l.starts_with(&format!("who coalition={id} ")))
            .unwrap_or_else(|| panic!("no {id} in {owner}"))
    };
    assert!(row("1092").contains(" exe=- voucher=apple "), "{owner}");
    assert!(
        row("1342").contains(" exe=aterm voucher=developer_id scope=aterm "),
        "{owner}"
    );
}

#[test]
fn an_unread_footprint_is_never_summed_as_zero() {
    let mut s = read(&mem_1142(), 3);
    if let Some(f) = s.why.as_mut() {
        for r in &mut std::sync::Arc::make_mut(f).split.rows {
            if ![1342, 1393, 393].contains(&r.id) {
                r.footprint = None;
            }
        }
    }
    let w = mac();
    let unread = "fp_bytes=unavailable(not_sampled)";
    let c = ctx(&w, &s, true);
    let t = why("why mem top=3", &c);
    assert!(
        lines(&t, "owners")[0].starts_with(&format!("owners {unread} coalitions=6 ")),
        "{t}"
    );
    assert_eq!(lines(&t, "more"), [format!("more who=3 {unread}")]);
    let who = lines(&t, "who");
    assert_eq!(who.len(), 3, "{t}");
    assert!(who.iter().all(|l| !l.contains(unread)), "{t}");
    let all = why("why mem top=6", &c);
    assert_eq!(
        lines(&all, "who")
            .iter()
            .filter(|l| l.contains(unread))
            .count(),
        3,
        "{all}"
    );
    let scoped = why("why mem", &ctx(&w, &s, false));
    assert_eq!(
        lines(&scoped, "withheld"),
        [format!("withheld who=6 {unread}")]
    );
    for owner in [true, false] {
        let j = render_json(&q("why mem top=3 --json"), &ctx(&w, &s, owner));
        assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
        let sum = if owner { "more" } else { "withheld" };
        for at in ["owners", sum] {
            assert_eq!(
                j.get(at).and_then(|o| o.get("fp_bytes")),
                Some(&Jv::why("not_sampled")),
                "{at}: {}",
                j.to_text()
            );
        }
    }
}

#[test]
fn an_unmeasured_coalition_reads_no_cpu() {
    let mut s = example_a();
    if let Some(f) = s.why.as_mut() {
        let split = &mut std::sync::Arc::make_mut(f).split;
        let row = split
            .rows
            .iter_mut()
            .find(|r| r.id == 327)
            .expect("contextstored");
        let mc = std::mem::take(&mut row.cpu_mc);
        row.measured = false;
        split.services_mc -= mc;
        split.unaccounted_mc += mc;
        split.coverage_complete = false;
    }
    let w = mac();
    let c = ctx(&w, &s, true);
    let t = why("why top=8", &c);
    let unread = "cpu_mc=unavailable(not_sampled)";
    let row = lines(&t, "who")
        .into_iter()
        .find(|l| l.starts_with("who coalition=327 "))
        .expect("contextstored's row");
    assert!(row.contains(&format!(" {unread} ")), "{row}");
    let reported = lines(&t, "reported_cpu")[0];
    assert!(reported.contains(" coalition_coverage=partial "), "{t}");
    assert!(!reported.contains("unaccounted"), "{t}");
    let j = render_json(&q("why top=8 --json"), &c);
    assert!(contract_violations(&j).is_empty(), "{}", j.to_text());
    let Some(Jv::Arr(who)) = j.get("who") else {
        panic!("no who in {}", j.to_text());
    };
    let row = who
        .iter()
        .find(|r| r.get("coalition") == Some(&Jv::Int(327)))
        .expect("contextstored's row");
    assert_eq!(row.get("cpu_mc"), Some(&Jv::why("not_sampled")));
    assert!(j.get("split_mc").is_none());
    assert_eq!(
        j.get("reported_cpu_mc")
            .and_then(|r| r.get("coalition_coverage")),
        Some(&Jv::s("partial"))
    );
}

#[test]
/// An unchanged reply folds every value line into one `same` line. The CPU-hardware block's
/// notes are evidence, not values: a `since=` reply sends them again, here the OS thermal state.
fn since_folds_every_unchanged_record() {
    let w = mac();
    let s = example_a();
    let c = ctx(&w, &s, true);
    for text in ["why top=6", "why mem", "why"] {
        let first = render_text(&q(text), &c);
        let (values, notes): (Vec<_>, Vec<_>) = first.lines.iter().partition(|l| l.value);
        let keys: Vec<&str> = values.iter().map(|l| l.key.as_str()).collect();
        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "{keys:?}");
        let notes: Vec<&str> = notes.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            notes,
            [
                "OS thermal state 0 (nominal); low power off; nominal thermal state does not exclude another limit"
            ],
            "{text}"
        );
        let mut again = render_text(&q(text), &c);
        let folded = apply_delta(&mut again, &fingerprints(&first));
        assert_eq!(folded.len(), keys.len());
        let left: Vec<&str> = again.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(left[0].starts_with("same why,"), "{left:?}");
        assert_eq!(left[1], notes[0]);
    }
    let first = render_text(&q("why top=6"), &c);
    let mut cut = render_text(&q("why top=5"), &c);
    apply_delta(&mut cut, &fingerprints(&first));
    let kept: Vec<&str> = cut.lines.iter().map(|l| l.key.as_str()).collect();
    assert_eq!(kept, ["same", "more", "cpu_hardware"]);
}

#[test]
fn the_subject_word_is_the_replys() {
    let calm = read(&calm_1510(), 3);
    let a = example_a();
    assert_eq!(subject_word(&q("why"), Some(&calm)), Some("global.cpu"));
    assert_eq!(subject_word(&q("why"), Some(&a)), Some("global.cpu"));
    assert_eq!(subject_word(&q("why mem"), Some(&a)), Some("global.mem"));
    assert_eq!(
        subject_word(&q("why global.cpu"), Some(&calm)),
        Some("global.cpu")
    );
    assert_eq!(subject_word(&q("why"), None), Some("global.cpu"));
    assert_eq!(subject_word(&q("global"), Some(&a)), None);
}

/// Published CPU observations do not witness every lifetime or socket delegation.
/// Memory and an unread first CPU window are controls: neither claims sampled attribution.
#[test]
fn sampled_cpu_names_the_work_its_attribution_cannot_witness() {
    let w = mac();
    for scene in [spotlight_1032(), calm_1510(), finished_1032()] {
        let s = read(&scene, 3);
        let c = ctx(&w, &s, true);
        for query in ["why", "why cpu"] {
            let t = why(query, &c);
            let j = render_json(&q(&format!("{query} --json")), &c).to_text();
            for (what, reason) in [
                ("every_process_start", "not_sampled"),
                ("between_sweep_orphans", "not_sampled"),
                ("socket_delegation", "unsupported"),
            ] {
                assert!(
                    t.contains(&format!("cannot what={what} why={reason}\n")),
                    "{t}"
                );
                assert!(
                    j.contains(&format!(
                        r#"{{"what":"{what}","reason":{{"why":"{reason}"}}}}"#
                    )),
                    "{j}"
                );
                let mem = why("why mem", &c);
                assert!(!mem.contains(what), "{mem}");
            }
        }
    }
    let s = Snapshot::default();
    let c = ctx(&w, &s, true);
    assert!(!why("why cpu", &c).contains("not_sampled"));
}
