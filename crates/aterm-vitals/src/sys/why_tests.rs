// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `sys why` cause attribution, split accounting, and text/JSON formatting.

use super::*;
use crate::events::{Event, EventKind};
use crate::explain::fixtures::{
    Coal, Proc, Scene, USERSPACE_MS, app_work_1510, apple, build_start_1032, calm_1510,
    delegated_1032, delegated_in_aterm_1032, finished_1032, lane_off_1032, liar_1032, reaped_1032,
    spotlight_1032, spotlight_late_1032, tab, tab_ahead_1032, tab2_build_1048, two_coalitions_1421,
    wrapped_1218,
};
use crate::explain::{CoalLane, LeadIdentity, Voucher};
use crate::model::{MemCounters, MemSample, Pressure, Thermal, Vitals};
use crate::sys::json::{SPLIT_PARTS, contract_violations};
use crate::sys::view::{Reply, render_json, render_text};
use crate::sys::{Ctx, Freshness, Machine, TabFact, Target, apply_delta, fingerprints};

struct World {
    machine: Machine,
    target: Target,
    tabs: Vec<TabFact>,
}

fn world(family: &'static str, tabs: u64) -> World {
    World {
        machine: Machine {
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
        home: None,
        events: &[],
        gap: None,
        head_seq: 0,
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
        .and_then(|v| v.trim_start_matches(['≥', '≤']).parse().ok())
        .unwrap_or_else(|| panic!("no {key} in {line}"))
}

const EXAMPLE_A: &str = "\
OK 16 sys v=1 view=why subject=global.cpu as_of=17:32:41.200 age_ms=120 window_ms=5000 next=7f3a.7
why     global cpu: 3.3 of 18 cores busy, easing; mostly macOS services (2.17c); Spotlight, after start-up; this tab is not the cause
split tab_mc=200 aterm_rest_mc=200 others_mc=730 services_mc=2170 unaccounted_mc=0 exact=exact via=coalitions n=9
you tab=1 role=minor cpu_mc=200 share_pm=61
who coalition=307 pid=387 birth_ms=1792517318000 exe=mds voucher=apple uid=0 part=services cpu_mc=1290 read_bps=22900000 deferred_pm=722 finished_included=1 cause=spotlight
who coalition=1092 pid=916 birth_ms=1792517325000 exe=siriactionsd voucher=apple uid=501 part=others cpu_mc=660 cause=-
who coalition=327 pid=397 birth_ms=1792517318000 exe=contextstored voucher=apple uid=0 part=services cpu_mc=460 finished_included=1 cause=-
who coalition=1342 pid=1187 birth_ms=1792517515000 exe=aterm voucher=developer_id team=A66A9P66Z7 scope=aterm cpu_mc=400 finished_included=1 cause=-
who coalition=1 pid=1 birth_ms=1792512374000 exe=launchd voucher=apple uid=0 part=services cpu_mc=170 finished_included=1 cause=-
who coalition=491 pid=533 birth_ms=1792517319000 exe=mds_stores voucher=apple uid=308 part=services cpu_mc=130 finished_included=1 cause=spotlight
more who=2 cpu_mc=190
cause id=spotlight covers_mc=1420 since=17:28:38 opened=17:32:36 deferred_pm=722 sure=held src=signature,kernel
cause id=startup since=17:28:38 wake=17:28:33 wake_reason=RTP.multi-touch slept_s=3715 sure=held src=kernel
pass trend=easing src=kernel
do class=none may=agent destructive=0
cannot what=index_progress why=unsupported
cannot what=index_folder why=needs_privilege:root
";

#[test]
fn example_a_spotlight_after_start_up() {
    let w = mac();
    let s = example_a();
    let c = ctx(&w, &s, true);
    let a = why("why top=6", &c);
    assert_eq!(a, EXAMPLE_A);
    assert!(a.len() <= REPLY_MAX, "{} bytes", a.len());
    let bare = why("why", &c);
    assert!(bare.starts_with("OK 17 sys v=1 view=why subject=global.cpu "));
    assert_eq!(lines(&bare, "who").len(), 8);
    assert!(lines(&bare, "more").is_empty());
    assert!(bare.len() <= REPLY_MAX, "{} bytes", bare.len());
}

#[test]
fn the_rows_sum_to_the_split_and_a_scoped_caller_gets_the_sums() {
    let w = mac();
    let s = example_a();
    for (owner, top) in [(true, "why"), (true, "why top=2"), (false, "why")] {
        let c = ctx(&w, &s, owner);
        let t = why(top, &c);
        let split = lines(&t, "split")[0];
        let parts: i64 = [
            "tab_mc",
            "aterm_rest_mc",
            "others_mc",
            "services_mc",
            "unaccounted_mc",
        ]
        .iter()
        .map(|k| field(split, k))
        .sum();
        assert_eq!(parts, 3300, "{t}");
        let rows: i64 = ["who", "more", "withheld"]
            .iter()
            .flat_map(|k| lines(&t, k))
            .map(|l| field(l, "cpu_mc"))
            .sum();
        assert_eq!(rows, 3300, "{t}");
        assert_eq!(lines(&t, "who").is_empty(), !owner, "{t}");
    }
    let c = ctx(&w, &s, false);
    let scoped = why("why", &c);
    assert_eq!(lines(&scoped, "withheld"), ["withheld who=8 cpu_mc=3300"]);
    assert!(lines(&scoped, "more").is_empty());
    let two = why("why top=2", &ctx(&w, &s, true));
    assert_eq!(lines(&two, "who").len(), 2);
    assert_eq!(lines(&two, "more"), ["more who=6 cpu_mc=1350"]);
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
}

#[test]
fn example_b_a_calm_mac_is_two_lines() {
    let w = mac();
    let s = read_at(&calm_1510(), 3);
    let c = ctx(&w, &s, true);
    assert_eq!(
        why("why", &c),
        "OK 2 sys v=1 view=why subject=global as_of=22:10:02.120 age_ms=120 window_ms=5000 next=7f3a.7
why     calm: 0.4 of 18 cores busy; memory 16.6 of 128 GiB used, pressure normal; swap 0; no cause open
you tab=1 role=minor basis=machine_calm cpu_mc=120 share_pm=300
"
    );
    let j = render_json(&q("why --json"), &c);
    assert_eq!(j.get("calm"), Some(&Jv::Bool(true)));
    assert_eq!(
        j.get("you").and_then(|y| y.get("basis")),
        Some(&Jv::s("machine_calm"))
    );
    let cpu = why("why cpu", &c);
    assert!(cpu.contains(" view=why subject=global.cpu "), "{cpu}");
    assert_eq!(lines(&cpu, "split").len(), 1, "{cpu}");
    assert_eq!(lines(&cpu, "pass"), ["pass trend=steady src=kernel"]);
}

#[test]
fn a_bare_why_is_never_calm_without_the_thermal_state() {
    let w = world("Linux", 1);
    let scene = Scene {
        lane: CoalLane::Unsupported,
        thermal: None,
        ..calm_1510()
    };
    let s = read(&scene, 3);
    let c = ctx(&w, &s, true);
    let t = why("why", &c);
    assert!(!t.contains("subject=global "), "{t}");
    assert!(lines(&t, "you")[0].find("basis=").is_none(), "{t}");
    assert_eq!(subject_word(&q("why"), Some(&s)), Some("global.mem"));
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

#[test]
fn the_memory_reply_reads_its_classes_and_footprints() {
    let w = mac();
    let s = read_at(&mem_1142(), 3);
    let c = ctx(&w, &s, true);
    let t = why("why mem top=3", &c);
    assert_eq!(
        t,
        "OK 13 sys v=1 view=why subject=global.mem as_of=22:10:02.120 age_ms=120 window_ms=5000 next=7f3a.7
why     global memory: 16.5 of 128 GiB used; pressure normal; swap 0; nothing compressed; no cause open; this tab is not the cause
classes total_bytes=137438953472 app_bytes=12545392640 wired_bytes=5132959744 compressed_bytes=0 used_bytes=17678352384 cache_bytes=27967258624 free_bytes=89733971968 src=kernel
classes not_in_vm_bytes=2059370496 src=derived
you tab=1 role=minor fp_bytes=33554432 share_pm=2
owners fp_bytes=3451115275 coalitions=6 live_only=1 src=kernel
who coalition=1342 pid=1187 birth_ms=1792517515000 exe=aterm voucher=developer_id team=A66A9P66Z7 scope=aterm fp_bytes=1630032364 live=3
who coalition=1393 pid=1631 birth_ms=1792517638000 exe=Safari voucher=apple uid=501 part=others fp_bytes=846525891 live=1
who coalition=393 pid=430 birth_ms=1792517318500 exe=WindowServer voucher=apple uid=88 part=services fp_bytes=773230428 live=1
more who=3 fp_bytes=201326592
pass trend=steady src=kernel
do class=none may=agent destructive=0
cannot what=wired_by_process why=needs_privilege:root
cannot what=uncharged_memory why=unsupported
"
    );
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

#[test]
fn another_tabs_work_is_named_only_to_the_owner() {
    let w = world("macOS", 2);
    let s = read(&tab2_build_1048(), 3);
    let owner = why("why", &ctx(&w, &s, true));
    assert!(
        owner.contains("; Tab 2's work; this tab is not the cause\n"),
        "{owner}"
    );
    assert_eq!(
        lines(&owner, "cause")[0].split(' ').nth(1),
        Some("id=tab-work")
    );
    assert!(lines(&owner, "cause")[0].contains(" tab=2 "), "{owner}");
    let scoped = why("why", &ctx(&w, &s, false));
    assert!(
        scoped.contains("; another tab's work; this tab is not the cause\n"),
        "{scoped}"
    );
    assert!(!scoped.contains("tab-work"), "{scoped}");
    assert!(!scoped.contains("tab=2"), "{scoped}");
    let j = render_json(&q("why --json"), &ctx(&w, &s, false)).to_text();
    assert!(!j.contains("tab-work"), "{j}");
}

#[test]
fn app_work_names_its_coalition_only_to_the_owner() {
    let w = mac();
    let s = read(&app_work_1510(), 3);
    for owner in [true, false] {
        let c = ctx(&w, &s, owner);
        let t = why("why cpu", &c);
        assert!(
            t.contains("; an app running as you; this tab is not the cause\n"),
            "{t}"
        );
        let cause = lines(&t, "cause")[0];
        assert!(
            cause.starts_with("cause id=app-work covers_mc=1500 "),
            "{t}"
        );
        assert_eq!(cause.contains(" coalition=896 "), owner, "{cause}");
        let j = render_json(&q("why cpu --json"), &c);
        let Some(Jv::Arr(causes)) = j.get("causes") else {
            panic!("no causes in {}", j.to_text());
        };
        let named = causes[0].get("evidence").is_some_and(|e| {
            matches!(e, Jv::Arr(facts) if facts.iter().any(|f| f.get("coalition") == Some(&Jv::Int(896))))
        });
        assert_eq!(named, owner, "{}", j.to_text());
        let first = render_text(&q("why cpu"), &c);
        let mut again = render_text(&q("why cpu"), &c);
        apply_delta(&mut again, &fingerprints(&first));
        let same = &again.lines[0].text;
        assert!(same.contains(",cause_app-work,"), "{same}");
        if !owner {
            assert!(!t.contains("coalition="), "{t}");
            assert!(!j.to_text().contains("\"coalition\""), "{}", j.to_text());
            assert!(!same.contains("896"), "{same}");
        }
    }
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

#[test]
fn a_split_tab_is_its_panes_summed() {
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
    assert_eq!(figure(&one, &apart, "why cpu", "split", "tab_mc"), 200);
    assert_eq!(figure(&two, &apart, "why cpu", "split", "tab_mc"), 300);
    for key in [1, 2] {
        let both = split_tab(key);
        assert_eq!(figure(&both, &joined, "why cpu", "split", "tab_mc"), 500);
        assert_eq!(figure(&both, &joined, "why cpu", "you", "cpu_mc"), 500);
        assert_eq!(
            figure(&both, &joined, "why mem", "you", "fp_bytes"),
            figure(&one, &apart, "why mem", "you", "fp_bytes")
                + figure(&two, &apart, "why mem", "you", "fp_bytes")
        );
    }

    let span = |mut scene: Scene| {
        scene.busy_mc += 400;
        scene.coalitions.push(Coal::new(7000, 400));
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
fn a_split_tabs_work_is_one_cause() {
    let tab_work = |t: &str| -> Vec<String> {
        lines(t, "cause")
            .into_iter()
            .filter(|l| l.starts_with("cause id=tab-work "))
            .map(str::to_owned)
            .collect()
    };
    let apart = read(&two_sessions_1032(), 3);
    let t = why("why cpu", &ctx(&world("macOS", 2), &apart, true));
    assert!(tab_work(&t).is_empty(), "{t}");
    let s = read(&split_1032(), 3);
    for key in [1, 2] {
        let t = why("why cpu", &ctx(&split_tab(key), &s, false));
        let work = tab_work(&t);
        assert_eq!(work.len(), 1, "{t}");
        assert_eq!(field(&work[0], "covers_mc"), 500, "{t}");
        assert_eq!(field(&work[0], "tab"), 1, "{t}");
        assert_eq!(field(lines(&t, "you")[0], "cpu_mc"), 500, "{t}");
    }
    let mut busy = split_1032();
    for (pid, mc) in [(1210, 800), (1310, 800)] {
        let p = busy
            .procs
            .iter_mut()
            .find(|p| p.pid == pid)
            .expect("a pane's claude");
        let more = mc - p.mc;
        (p.mc, p.fast_mc) = (mc, mc);
        busy.busy_mc += more;
        for c in busy.coalitions.iter_mut().filter(|c| c.id == 1342) {
            c.mc += more;
        }
    }
    let s = read(&busy, 3);
    let t = why("why cpu", &ctx(&split_tab(2), &s, true));
    let work = tab_work(&t);
    assert_eq!(work.len(), 1, "{t}");
    assert_eq!(field(&work[0], "covers_mc"), 1600, "{t}");
    assert!(
        t.contains("; Tab 1's work; this tab is part of the cause\n"),
        "{t}"
    );
    assert_eq!(field(lines(&t, "you")[0], "cpu_mc"), 1600, "{t}");
}

#[test]
fn a_scoped_caller_never_sees_aterms_coalitions() {
    let w = world("macOS", 2);
    let s = read(&two_coalitions_1421(), 3);
    let tabs = s
        .why
        .as_deref()
        .and_then(|f| f.split.rows.iter().find(|r| r.id == 55954))
        .map(|r| (r.part, r.tabs.clone()));
    assert_eq!(tabs, Some((Part::Aterm, Some(vec![1]))));
    let owner = why("why cpu", &ctx(&w, &s, true));
    assert!(
        lines(&owner, "who")
            .iter()
            .any(|l| l.starts_with("who coalition=55954 pid=6100 ") && l.contains(" scope=aterm ")),
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
            level: Throttle::Light,
            raw: 1,
            low_power: Some(false),
            exact: true,
        }),
        ..calm_1510()
    };
    let s = read(&hot, 3);
    let cpu = why("why cpu", &ctx(&w, &s, true));
    assert!(cpu.contains("; throttled: light; "), "{cpu}");
    assert_eq!(
        lines(&cpu, "cause"),
        ["cause id=throttled level=light low_power=0 sure=held src=kernel"]
    );
    assert!(
        cpu.contains("\ncannot what=remedy why=unsupported\n"),
        "{cpu}"
    );
}

#[test]
fn each_cause_says_its_own_words() {
    let w = mac();
    let why_of = |scene: &Scene, text: &str| why(text, &ctx(&w, &read(scene, 3), true));
    let cause = |t: &str, id: &str| -> String {
        let head = format!("cause id={id} ");
        lines(t, "cause")
            .into_iter()
            .find(|l| l.starts_with(&head))
            .unwrap_or_else(|| panic!("no {id} in {t}"))
            .to_owned()
    };

    let t = why_of(&spotlight_late_1032(), "why cpu");
    assert!(t.contains("; Spotlight, since wake; "), "{t}");
    assert_eq!(
        cause(&t, "wake"),
        "cause id=wake wake=17:28:33 wake_reason=RTP.multi-touch slept_s=3715 sure=held src=kernel"
    );

    let t = why_of(&finished_1032(), "why cpu");
    assert!(t.contains("; no cause aterm can name: aterm; "), "{t}");
    let unexplained = cause(&t, "unexplained");
    assert!(
        unexplained.starts_with("cause id=unexplained covers_mc=3400 "),
        "{t}"
    );
    assert!(
        unexplained.ends_with(" part=aterm sure=held src=derived"),
        "{t}"
    );

    let mut busy = calm_1510();
    busy.busy_mc += 1420;
    for c in busy.coalitions.iter_mut().filter(|c| c.id == 1342) {
        c.mc += 1420;
    }
    for p in busy.procs.iter_mut().filter(|p| p.pid == 1187) {
        *p = Proc::new(p.pid, p.ppid, p.uid, p.coalition, p.birth_ms, "aterm", 1500);
    }
    let t = why_of(&busy, "why cpu");
    assert!(t.contains("; aterm itself; "), "{t}");
    assert!(
        cause(&t, "aterm-work").starts_with("cause id=aterm-work covers_mc=1500 "),
        "{t}"
    );

    let low_power = Scene {
        thermal: Some(Thermal {
            level: Throttle::None,
            raw: 0,
            low_power: Some(true),
            exact: true,
        }),
        ..calm_1510()
    };
    let t = why_of(&low_power, "why cpu");
    assert!(t.contains("; throttled: low power; "), "{t}");
    assert_eq!(
        cause(&t, "throttled"),
        "cause id=throttled level=none low_power=1 sure=held src=kernel"
    );

    let t = why_of(&tab2_build_1048(), "why");
    assert!(t.contains("; a tab's work; "), "{t}");
    let work = cause(&t, "tab-work");
    assert!(work.contains(" local=2 "), "{work}");
    assert!(!work.contains(" tab="), "{work}");

    let mut squeezed = mem_1142();
    if let Some(m) = squeezed.mem.as_mut() {
        m.compressed = Some(3 << 29);
    }
    let t = why_of(&squeezed, "why mem");
    assert!(t.contains("; 1.5 GiB compressed; "), "{t}");
}

#[test]
fn with_the_lane_off_the_split_is_by_subtraction() {
    let off = Scene {
        lane: CoalLane::Unsupported,
        ..spotlight_1032()
    };
    for (w, scene) in [(mac(), lane_off_1032()), (world("Linux", 1), off)] {
        let s = read(&scene, 3);
        let c = ctx(&w, &s, true);
        let t = why("why cpu", &c);
        let split = lines(&t, "split")[0];
        assert!(
            split.contains(
                " unaccounted_mc=unavailable(lane_off:coalitions) exact=heuristic via=subtraction"
            ),
            "{t}"
        );
        assert!(!split.contains(" n="), "{t}");
        assert!(lines(&t, "who").is_empty(), "{t}");
        assert!(lines(&t, "more").is_empty(), "{t}");
        assert!(
            t.contains("\ncannot what=coalitions why=lane_off:coalitions\n"),
            "{t}"
        );
        let mem = why("why mem", &c);
        assert!(lines(&mem, "owners").is_empty(), "{mem}");
        let j = render_json(&q("why --json"), &c);
        assert_eq!(
            j.get("split_mc").and_then(|p| p.get("unaccounted")),
            Some(&Jv::why("lane_off:coalitions"))
        );
    }
}

/// A tab holding two thirds of used memory is the main part only where every
/// part beside it is read: with the lane off no coalition's footprint is, so
/// its role is unread by the lane's word, yet its footprint and share still
/// print (they were read), the sentence says what is not, and `cannot` names
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
            ["you tab=1 role=unavailable(lane_off:coalitions) fp_bytes=4311744512 share_pm=669"],
            "{t}"
        );
        assert!(
            lines(&t, "why")[0].ends_with("; whether this tab is the largest part is not read now"),
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
        assert_eq!(you.get("share_pm"), Some(&Jv::u(669)));
    }
    let s = two_thirds(spotlight_1032());
    let t = why("why mem", &ctx(&mac(), &s, true));
    assert_eq!(
        lines(&t, "you"),
        ["you tab=1 role=main fp_bytes=4311744512 share_pm=669"],
        "CONTROL: every part read: {t}"
    );
    assert!(
        lines(&t, "why")[0].ends_with("; this tab is the main cause"),
        "{t}"
    );
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
    assert!(lines(&with, "who").len() < shown, "{with}");
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
        let s = read(scene, 3);
        for owner in [true, false] {
            for text in ["why", "why cpu", "why mem"] {
                let t = why(text, &ctx(&w, &s, owner));
                assert!(t.len() <= REPLY_MAX, "{} bytes\n{t}", t.len());
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

#[test]
fn unattributed_work_bounds_the_tab_pair() {
    let w = mac();
    let s = read(&finished_1032(), 3);
    let c = ctx(&w, &s, true);
    let t = why("why", &c);
    assert!(
        lines(&t, "split")[0].starts_with("split tab_mc=≥200 aterm_rest_mc=≤3200 others_mc=730 "),
        "{t}"
    );
    assert_eq!(
        lines(&t, "you"),
        ["you tab=1 role=unavailable(not_sampled) cpu_at_least_mc=200 unattributed_mc=3000"]
    );
    assert!(
        t.contains("\ncannot what=finished_tab_work why=not_sampled\n"),
        "{t}"
    );
    assert!(t.contains("; this tab's share is not read now\n"), "{t}");
    let j = render_json(&q("why --json"), &c);
    let split = j.get("split_mc").expect("split_mc");
    assert_eq!(split.get("tab"), Some(&Jv::at_least(200, "not_sampled")));
    assert_eq!(
        split.get("aterm_rest"),
        Some(&Jv::at_most(3200, "not_sampled"))
    );
    let you = j.get("you").expect("you");
    assert_eq!(you.get("cpu_at_least_mc"), Some(&Jv::Int(200)));
    assert_eq!(you.get("role"), Some(&Jv::why("not_sampled")));
}

#[test]
fn delegated_work_bounds_the_tab_from_below() {
    let w = mac();
    let s = read(&delegated_1032(500, 100), 3);
    let t = why("why", &ctx(&w, &s, true));
    let split = lines(&t, "split")[0];
    assert!(
        split.starts_with("split tab_mc=≥700 aterm_rest_mc=200 "),
        "{t}"
    );
    assert!(split.contains(" delegated_mc=500 "), "{t}");
    let row = lines(&t, "who")
        .into_iter()
        .find(|l| l.starts_with("who coalition=4000 "))
        .expect("coalition 4000");
    assert!(
        row.contains(" part=others cpu_mc=600 delegated_mc=500 "),
        "{row}"
    );
}

#[test]
fn a_cold_why_names_its_cause_with_no_opened() {
    let w = mac();
    let s = read(&spotlight_1032(), 1);
    let t = why("why", &ctx(&w, &s, true));
    assert!(
        t.starts_with("OK 17 sys v=1 view=why subject=global.cpu "),
        "{t}"
    );
    let spotlight = lines(&t, "cause")[0];
    assert!(
        spotlight.starts_with("cause id=spotlight covers_mc=1420 since=17:28:38 deferred_pm=722 ")
    );
    assert!(!t.contains("opened="), "{t}");
    let first = last(&spotlight_1032().series(1)[..1]);
    let t = why("why", &ctx(&w, &first, true));
    assert!(t.contains("\nsplit unavailable(warming)\n"), "{t}");
    assert!(t.contains("\nyou tab=1 role=unavailable(warming)\n"), "{t}");
    assert!(
        t.contains("; causes not read yet; this tab's share is not read now\n"),
        "{t}"
    );
    assert!(!t.contains("no cause open"), "{t}");
}

/// Before the lane's first window no cause is known not to hold, so the
/// CPU's reply never says nothing is needed, though memory's pressure and
/// the thermal state read calm (the CPU account's half of the bare `sys
/// why` that is never calm there); the first window's reply says it.
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
        t.contains("\ndo class=none may=agent destructive=0\n"),
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
        t.starts_with("OK 17 sys v=1 view=why subject=global.cpu as_of=17:32:41.200 age_ms=1120 window_ms=5000 "),
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
                if let Some(Jv::Obj(parts)) = j.get("split_mc").filter(|p| p.get("why").is_none()) {
                    let keys: Vec<&str> = parts.iter().map(|(k, _)| k.as_str()).collect();
                    assert_eq!(keys, SPLIT_PARTS);
                }
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

const EXAMPLE_A_JSON: &str = r#"{"schema":1,
"kind":"sys.why",
"as_of":{"unix_ms":1792517561200,"age_ms":120,"window_ms":5000,"cursor":"7f3a.7","state":"fresh"},
"subject":{"scope":"global","res":"cpu"},
"figure":{"cpu_mc":3300,"capacity_mc":18000,"trend":"easing","exact":"exact","via":"coalitions","n":9},
"split_mc":{"tab":200,"aterm_rest":200,"others":730,"services":2170,"unaccounted":0},
"you":{"tab":1,"role":"minor","cpu_mc":200,"share_pm":61},
"who":[{"coalition":307,"pid":387,"birth_ms":1792517318000,"exe":"mds","voucher":"apple","uid":0,"part":"services","cpu_mc":1290,"read_bps":22900000,"deferred_pm":722,"finished_included":true,"cause":"spotlight"},
{"coalition":1092,"pid":916,"birth_ms":1792517325000,"exe":"siriactionsd","voucher":"apple","uid":501,"part":"others","cpu_mc":660,"cause":"-"},
{"coalition":327,"pid":397,"birth_ms":1792517318000,"exe":"contextstored","voucher":"apple","uid":0,"part":"services","cpu_mc":460,"finished_included":true,"cause":"-"},
{"coalition":1342,"pid":1187,"birth_ms":1792517515000,"exe":"aterm","voucher":"developer_id","team":"A66A9P66Z7","scope":"aterm","cpu_mc":400,"finished_included":true,"cause":"-"},
{"coalition":1,"pid":1,"birth_ms":1792512374000,"exe":"launchd","voucher":"apple","uid":0,"part":"services","cpu_mc":170,"finished_included":true,"cause":"-"},
{"coalition":491,"pid":533,"birth_ms":1792517319000,"exe":"mds_stores","voucher":"apple","uid":308,"part":"services","cpu_mc":130,"finished_included":true,"cause":"spotlight"}],
"more":{"who":2,"cpu_mc":190},
"causes":[{"cause":"spotlight","sure":"held","since_ms":1792517318000,"opened_ms":1792517556200,"covers_mc":1420,"evidence":[{"fact":"deferred","deferred_pm":722,"src":"kernel"},{"fact":"read","read_bps":22900000,"src":"kernel"},{"fact":"signature","coalitions":2,"src":"signature"}]},
{"cause":"startup","sure":"held","since_ms":1792517318000,"evidence":[{"fact":"userspace_start","at_unix_ms":1792517318000,"src":"kernel"},{"fact":"wake","at_unix_ms":1792517313000,"src":"kernel"},{"fact":"wake_reason","reason":"RTP.multi-touch","src":"kernel"},{"fact":"slept","slept_s":3715,"src":"kernel"}]}],
"pass":{"trend":"easing","src":"kernel"},
"do":[{"class":"none","may":"agent","destructive":0}],
"cannot":[{"what":"index_progress","reason":{"why":"unsupported"}},
{"what":"index_folder","reason":{"why":"needs_privilege:root"}}],
"events":[]}"#;

const MEM_1142_JSON: &str = r#"{"schema":1,
"kind":"sys.why",
"as_of":{"unix_ms":1792534202120,"age_ms":120,"window_ms":5000,"cursor":"7f3a.7","state":"fresh"},
"subject":{"scope":"global","res":"mem"},
"figure":{"used_bytes":17678352384,"total_bytes":137438953472,"trend":"steady","exact":"exact","via":"coalitions","n":6},
"classes":[{"class":"app","bytes":12545392640},
{"class":"wired","bytes":5132959744},
{"class":"compressed","bytes":0},
{"class":"cache","bytes":27967258624},
{"class":"free","bytes":89733971968},
{"class":"not_in_vm","bytes":2059370496}],
"owners":{"fp_bytes":3451115275,"coalitions":6,"live_only":true},
"you":{"tab":1,"role":"minor","fp_bytes":33554432,"share_pm":2},
"who":[{"coalition":1342,"pid":1187,"birth_ms":1792517515000,"exe":"aterm","voucher":"developer_id","team":"A66A9P66Z7","scope":"aterm","fp_bytes":1630032364,"live":3},
{"coalition":1393,"pid":1631,"birth_ms":1792517638000,"exe":"Safari","voucher":"apple","uid":501,"part":"others","fp_bytes":846525891,"live":1},
{"coalition":393,"pid":430,"birth_ms":1792517318500,"exe":"WindowServer","voucher":"apple","uid":88,"part":"services","fp_bytes":773230428,"live":1}],
"more":{"who":3,"fp_bytes":201326592},
"causes":[],
"pass":{"trend":"steady","src":"kernel"},
"do":[{"class":"none","may":"agent","destructive":0}],
"cannot":[{"what":"wired_by_process","reason":{"why":"needs_privilege:root"}},
{"what":"uncharged_memory","reason":{"why":"unsupported"}}],
"events":[]}"#;

#[test]
fn the_json_goldens() {
    let w = mac();
    let a = example_a();
    let j = render_json(&q("why top=6 --json"), &ctx(&w, &a, true)).to_text();
    assert_eq!(j, EXAMPLE_A_JSON.replace('\n', ""));
    let m = read_at(&mem_1142(), 3);
    let j = render_json(&q("why mem top=3 --json"), &ctx(&w, &m, true)).to_text();
    assert_eq!(j, MEM_1142_JSON.replace('\n', ""));
}

fn int(j: &Jv) -> Option<i64> {
    match j {
        Jv::Int(n) => Some(*n),
        _ => match j.get("v") {
            Some(Jv::Int(n)) => Some(*n),
            _ => None,
        },
    }
}

fn rows_sum(j: &Jv, key: &str) -> i64 {
    let who: i64 = match j.get("who") {
        Some(Jv::Arr(rows)) => rows.iter().filter_map(|r| r.get(key).and_then(int)).sum(),
        _ => 0,
    };
    who + ["more", "withheld"]
        .iter()
        .filter_map(|k| j.get(k).and_then(|m| m.get(key)).and_then(int))
        .sum::<i64>()
}

#[test]
fn the_json_sums_close_as_the_text_does() {
    let scenes = [
        example_a(),
        read(&finished_1032(), 3),
        read(&delegated_1032(500, 100), 3),
        read(&reaped_1032(), 3),
        read(&lane_off_1032(), 3),
        read(&mem_1142(), 3),
    ];
    let w = mac();
    for s in &scenes {
        for owner in [true, false] {
            let c = ctx(&w, s, owner);
            let j = render_json(&q("why cpu --json"), &c);
            let figure = j
                .get("figure")
                .and_then(|f| f.get("cpu_mc"))
                .and_then(int)
                .expect("cpu_mc");
            let split = j.get("split_mc").expect("split_mc");
            let text = why("why cpu", &c);
            let line = lines(&text, "split")[0];
            let mut parts = 0;
            for (part, key) in SPLIT_PARTS.iter().zip([
                "tab_mc",
                "aterm_rest_mc",
                "others_mc",
                "services_mc",
                "unaccounted_mc",
            ]) {
                if let Some(v) = split.get(part).and_then(int) {
                    assert_eq!(v, field(line, key), "{part}: {line}");
                    parts += v;
                }
            }
            assert_eq!(parts, figure, "{}", j.to_text());
            if let Some(unaccounted) = split.get("unaccounted").and_then(int) {
                assert_eq!(
                    rows_sum(&j, "cpu_mc") + unaccounted,
                    figure,
                    "{}",
                    j.to_text()
                );
            }
            let j = render_json(&q("why mem --json"), &c);
            if let Some(owners) = j
                .get("owners")
                .and_then(|o| o.get("fp_bytes"))
                .and_then(int)
            {
                assert_eq!(rows_sum(&j, "fp_bytes"), owners, "{}", j.to_text());
            }
        }
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
        let t = why("why cpu", &c);
        let j = render_json(&q("why cpu --json"), &c).to_text();
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
            row.contains(" exe=- voucher=developer_id uid=501 "),
            "{row}"
        );
        assert!(t.contains(" wake_reason=- "), "{t}");
    }
    let owner = why("why cpu", &ctx(&w, &s, true));
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
        split.exact = false;
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
    let split = lines(&t, "split")[0];
    assert!(
        split.contains(" unaccounted_mc=460 exact=heuristic "),
        "{t}"
    );
    let rows: i64 = lines(&t, "who")
        .into_iter()
        .filter(|l| !l.contains(unread))
        .map(|l| field(l, "cpu_mc"))
        .sum();
    assert_eq!(rows + field(split, "unaccounted_mc"), 3300, "{t}");
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
    let unaccounted = j
        .get("split_mc")
        .and_then(|p| p.get("unaccounted"))
        .and_then(int);
    assert_eq!(
        Some(rows_sum(&j, "cpu_mc"))
            .zip(unaccounted)
            .map(|(r, u)| r + u),
        Some(3300)
    );
}

#[test]
fn since_folds_every_unchanged_record() {
    let w = mac();
    let s = example_a();
    let c = ctx(&w, &s, true);
    for text in ["why top=6", "why mem", "why"] {
        let first = render_text(&q(text), &c);
        let keys: Vec<&str> = first.lines.iter().map(|l| l.key.as_str()).collect();
        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "{keys:?}");
        assert!(first.lines.iter().all(|l| l.value), "{keys:?}");
        let mut again = render_text(&q(text), &c);
        let folded = apply_delta(&mut again, &fingerprints(&first));
        assert_eq!(folded.len(), keys.len());
        assert_eq!(again.lines.len(), 1);
        assert!(
            again.lines[0].text.starts_with("same why,"),
            "{}",
            again.lines[0].text
        );
    }
    let first = render_text(&q("why top=6"), &c);
    let mut cut = render_text(&q("why top=5"), &c);
    apply_delta(&mut cut, &fingerprints(&first));
    let kept: Vec<&str> = cut.lines.iter().map(|l| l.key.as_str()).collect();
    assert_eq!(kept, ["same", "more"]);
}

#[test]
fn the_subject_word_is_the_replys() {
    let calm = read(&calm_1510(), 3);
    let a = example_a();
    assert_eq!(subject_word(&q("why"), Some(&calm)), Some("global"));
    assert_eq!(subject_word(&q("why"), Some(&a)), Some("global.cpu"));
    assert_eq!(subject_word(&q("why mem"), Some(&a)), Some("global.mem"));
    assert_eq!(
        subject_word(&q("why global.cpu"), Some(&calm)),
        Some("global.cpu")
    );
    assert_eq!(subject_word(&q("why"), None), Some("global.cpu"));
    assert_eq!(subject_word(&q("global"), Some(&a)), None);
}
