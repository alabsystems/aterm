// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use super::fixtures::{Scene, calm_1510, liar_1032, spotlight_1032, spotlight_family_1115, tab};
use super::*;
use crate::model::{MemSample, Pressure, ProcSample, Thermal, Throttle, Vitals};
use crate::published_cpu::{CpuAccounting, CpuExecutionWindow};

fn read_calm(scene: Scene) -> Scene {
    let calm = calm_1510();
    Scene {
        mem: calm.mem,
        thermal: calm.thermal,
        ..scene
    }
}

/// Two readings of one stable app process and one tab. Only the published tab counter changes;
/// the caller specifies that growth independently of execution the producer does not expose.
fn reported_tab_growth(growth_ns: u64) -> ScopeCpu {
    reported_tab_read(Some(growth_ns), false)
}

fn reported_tab_read(growth_ns: Option<u64>, missing_identity: bool) -> ScopeCpu {
    let mut state = WhyState::default();
    let mut sample = Sample {
        at_ns: 1_000_000_000,
        at_unix_ms: 2_000_000,
        self_pid: 100,
        self_uid: Some(501),
        tabs: vec![tab(1, 200)],
        procs: Some(vec![
            ProcSample {
                pid: 100,
                ppid: 1,
                start: Some(1),
                birth_ms: Some(1),
                uid: Some(501),
                cpu_ns: Some(0),
                child_cpu_ns: Some(0),
                ..ProcSample::default()
            },
            ProcSample {
                pid: 200,
                ppid: 100,
                start: Some(2),
                birth_ms: Some(2),
                uid: Some(501),
                cpu_ns: Some(10_000_000),
                child_cpu_ns: Some(0),
                ..ProcSample::default()
            },
        ]),
        ..Sample::default()
    };
    let members = crate::model::attribute(sample.procs.as_deref().unwrap(), 100, &sample.tabs);
    let owners = [None, None];
    assert!(
        state
            .scope_cpu(&sample, Some((&members, &owners)))
            .is_none()
    );
    sample.at_ns += 1_000_000_000;
    sample.at_unix_ms += 1_000;
    sample.procs.as_mut().unwrap()[1].cpu_ns = growth_ns.map(|growth| 10_000_000 + growth);
    if missing_identity {
        sample.procs.as_mut().unwrap()[1].start = None;
    }
    state
        .scope_cpu(&sample, Some((&members, &owners)))
        .expect("consecutive observations")
}

#[test]
fn reported_cpu_delayed_publication_is_never_an_actual_window_floor() {
    // Four ms executed before the first read remain unpublished; one ms executes after it.
    // Publishing all five ms at the second read is indistinguishable from five fresh ms.
    let actual_inside_ns = 1_000_000_u64;
    let cpu = reported_tab_growth(4_000_000 + actual_inside_ns);
    assert_eq!(cpu.window_ns, 1_000_000_000);
    assert!(!cpu.missed);
    assert_eq!(cpu.tabs.len(), 1);
    assert!(!cpu.tabs[0].ambiguous && !cpu.tabs[0].missed);
    assert_eq!(cpu.tabs[0].reported, 0.005);
    assert_eq!(cpu.process_reported(), 0.005);
    assert!(cpu.tabs[0].reported > actual_inside_ns as f64 / cpu.window_ns as f64);
    assert_eq!(
        cpu.cpu_accounting.execution_window,
        CpuExecutionWindow::Unavailable
    );
}

#[test]
fn reported_cpu_zero_publication_does_not_establish_zero_execution() {
    // Execution can remain in unpublished tails at both observations. The native DTO has no
    // execution-epoch field that distinguishes this from a truly idle tab.
    let cpu = reported_tab_growth(0);
    assert!(!cpu.missed);
    assert_eq!((cpu.process_reported(), cpu.tabs[0].reported), (0.0, 0.0));
    assert_eq!(
        cpu.cpu_accounting.execution_window,
        CpuExecutionWindow::Unavailable
    );
    let split = CoalSplit {
        coverage_complete: true,
        tabs: vec![(1, 0)],
        tab_reported: vec![(1, 0)],
        ..CoalSplit::default()
    };
    assert_eq!(split.cpu_accounting, cpu.cpu_accounting);
    assert!(split.coverage_complete && !split.ownership_incomplete);
}

#[test]
fn reported_cpu_process_coalition_and_inherited_accounts_remain_independent() {
    let mut cpu = reported_tab_growth(5_000_000);
    cpu.own_charge = 20.0;
    cpu.tabs[0].charge = 30.0;
    for coalition_mc in [0, 1, 50_000] {
        let facts = PreparedWhy {
            lane: CoalLane::On,
            facts: Some(WhyFacts {
                lane: CoalLane::On,
                at_ns: cpu.read_at_ns,
                window_ns: cpu.window_ns,
                split: CoalSplit {
                    coverage_complete: true,
                    aterm_mc: coalition_mc,
                    delegated_mc: 40_000,
                    ..CoalSplit::default()
                },
                ..WhyFacts::default()
            }),
        };
        facts.apply_app(&Sample::default(), &mut cpu);
        assert_eq!(cpu.coalition_reported, Some(coalition_mc as f64 / 1000.0));
        assert!(!cpu.coalition_incomplete);
        assert_eq!(cpu.process_reported(), 0.005);
        assert_eq!(cpu.all_charge(), 50.0);
        assert_eq!(cpu.cpu_accounting, CpuAccounting::PUBLISHED_COUNTERS);
        let mut later = cpu.clone();
        later.read_at_ns += 1;
        later.coalition_reported = None;
        facts.apply_app(&Sample::default(), &mut later);
        assert_eq!(
            later.coalition_reported, None,
            "equal read spans do not prove equal observation endpoints"
        );
    }
}

#[test]
fn reported_cpu_complete_or_missing_observations_never_open_cpu_causes() {
    for incomplete in [false, true] {
        let mut scene = spotlight_1032();
        scene.unanswered = u32::from(incomplete);
        scene.mem = None;
        scene.thermal = None;
        let mut model = Vitals::new();
        let mut readings = 0;
        let mut complete = false;
        for sample in scene.series(8) {
            let snapshot = model.absorb(&sample);
            if let Some(facts) = snapshot.why {
                readings += 1;
                complete |= facts.split.coverage_complete;
                assert!(facts.causes.is_empty());
                assert!(facts.open.is_empty());
                assert!(facts.clause.is_none());
                assert_eq!(
                    facts.split.cpu_accounting.execution_window,
                    CpuExecutionWindow::Unavailable
                );
            }
        }
        assert!(readings > 0);
        assert_eq!(complete, !incomplete);
    }
}

#[test]
fn machine_contexts_follow_current_readings_without_cpu_covering_claims() {
    let mut samples = spotlight_1032().series(2);
    let mut model = Vitals::new();
    for sample in &samples[..3] {
        model.absorb(sample);
    }
    samples[3].mem = Some(MemSample {
        total: 64 << 30,
        pressure: Pressure::Warn,
        ..MemSample::default()
    });
    samples[3].thermal = Some(Thermal {
        level: Throttle::Light,
        ..Thermal::default()
    });
    let current = model.absorb(&samples[3]);
    let account = account(WhySubject::Cpu, Some(1), &current);
    assert_eq!(
        account
            .causes
            .iter()
            .map(|c| c.cause.id)
            .collect::<Vec<_>>(),
        [
            CauseId::Context(Context::Throttled),
            CauseId::Context(Context::Pressure)
        ]
    );
    assert!(!account.nothing_needed);
    samples[4].mem = None;
    samples[4].thermal = None;
    let current = model.absorb(&samples[4]);
    assert!(causes(&current).is_empty());
}

/// The four Spotlight members main counted before SD-93.
const MAIN_FOUR: &[&str] = &[
    "com.apple.mds",
    "com.apple.mds_stores",
    "com.apple.mdworker_shared",
    "com.apple.corespotlightd",
];

/// Spotlight's family is read whole: plain `mdworker`, `managedcorespotlightd` and
/// `spotlightknowledged.updater`, each under the 200-millicore bar alone, read indexing
/// together at 540 millicores of reported growth in the reading the `sys` line, its history and
/// `await sys spotlight=` take, and the activity's members are the reading's nine. No CPU cause
/// is claimed: published counter growth establishes none (SD-91).
/// NEGATIVE CONTROL: the same split's rows read with the four members main counted leave the
/// three out: no share, no growth, idle.
#[test]
fn spotlights_cause_sums_every_process_of_its_family() {
    let s = snap(&spotlight_family_1115());
    assert_eq!(
        ActivityId::Spotlight.activity().members,
        crate::spotlight::MEMBERS
    );
    let r = s.spotlight.as_ref().expect("a split reading");
    assert_eq!(
        (r.cpu_mc, r.read_bps, r.footprint_bytes(), r.shares.len()),
        (540, 3_000_000, Some(3 * (20 << 20)), 3)
    );
    assert_eq!(r.state, crate::spotlight::State::Indexing);
    let mut ids: Vec<&str> = r.shares.iter().map(|m| m.signing_id).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        [
            "com.apple.managedcorespotlightd",
            "com.apple.mdworker",
            "com.apple.spotlightknowledged.updater"
        ]
    );
    assert!(causes(&s).is_empty(), "{:?}", causes(&s));

    let f = s.why.as_ref().expect("facts");
    let control = crate::spotlight::reading_of(&f.split.rows, MAIN_FOUR, None, 0, 0, 5000);
    assert_eq!(
        (control.shares.len(), control.cpu_mc, control.state),
        (0, 0, crate::spotlight::State::Idle),
        "the control: main's four leave the three out"
    );
}

/// The liar: `mds` and `mds_stores` under ad-hoc signatures with Apple's signing ids, which
/// the family's reading (the `sys` line, its history and `await sys spotlight=`) does not
/// count. CONTROL: vouched, the same two are shares.
#[test]
fn a_claimed_signing_id_never_joins_the_spotlight_family() {
    let liar = snap(&liar_1032());
    let r = liar.spotlight.as_ref().expect("a split reading");
    assert_eq!((r.shares.len(), r.cpu_mc, r.read_bps), (0, 0, 0));
    let honest = snap(&spotlight_1032());
    let r = honest.spotlight.as_ref().expect("a split reading");
    let mut ids: Vec<&str> = r.shares.iter().map(|m| m.signing_id).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        ["com.apple.mds", "com.apple.mds_stores"],
        "the control"
    );
}

/// The snapshot of `scene`'s second split tick, through a fresh model.
fn snap(scene: &Scene) -> Snapshot {
    let mut v = Vitals::new();
    let [base, sweep, now] = scene.samples();
    v.absorb(&base);
    v.absorb(&sweep);
    v.absorb(&now)
}

/// The snapshot of the last of `readings`, through a fresh model.
fn last(readings: &[Sample]) -> Snapshot {
    let mut v = Vitals::new();
    let mut snaps: Vec<Snapshot> = readings.iter().map(|r| v.absorb(r)).collect();
    snaps.pop().expect("readings")
}

fn causes(s: &Snapshot) -> Vec<CauseId> {
    s.why
        .as_ref()
        .map_or_else(Vec::new, |f| f.causes.iter().map(|c| c.id).collect())
}

/// A coalition list cut short at either end is said; a sweep cut at its cap
/// is not that limit.
#[test]
fn a_cut_coalition_list_is_said() {
    let cannot = |edit: fn(&mut Sample, &mut Sample)| {
        let mut v = Vitals::new();
        let [mut base, sweep, mut now] = spotlight_1032().samples();
        edit(&mut base, &mut now);
        v.absorb(&base);
        v.absorb(&sweep);
        account(WhySubject::Cpu, Some(1), &v.absorb(&now)).cannot
    };
    let cut = ("coalition_list", "truncated");
    let edits: [fn(&mut Sample, &mut Sample); 2] = [
        |base, _| {
            if let Some(c) = base.coalitions.as_mut() {
                c.truncated = true;
            }
        },
        |_, now| {
            if let Some(c) = now.coalitions.as_mut() {
                c.truncated = true;
            }
        },
    ];
    for edit in edits {
        assert!(cannot(edit).contains(&cut));
    }
    assert!(!cannot(|_, now| now.procs_truncated = true).contains(&cut));
}

/// Memory's `you`: the tab's footprint of used memory, minor under the
/// floor; memory's limits are said.
#[test]
fn the_memory_account_reads_the_tabs_footprint() {
    let mut scene = spotlight_1032();
    scene.mem = Some(MemSample {
        total: 128 << 30,
        app: 12 << 30,
        wired: 4 << 30,
        compressed: Some(0),
        pressure: Pressure::Normal,
        ..MemSample::default()
    });
    let a = account(WhySubject::Mem, Some(1), &snap(&scene));
    assert_eq!(
        (a.you.role, a.you.figure, a.you.share_pm),
        (Role::Minor, Some(32 << 20), None)
    );
    assert_eq!((a.parts, a.headline), (None, None));
    assert_eq!(
        a.cannot,
        [
            ("wired_by_process", "needs_privilege:root"),
            ("uncharged_memory", "unsupported")
        ]
    );

    let role = |fp: u64, own: u64, used_gib: u64| {
        let mut scene = spotlight_1032();
        for p in &mut scene.procs {
            match p.pid {
                1210 => *p = p.clone().holding(fp),
                1187 => *p = p.clone().holding(own),
                _ => {}
            }
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
        scene.mem = Some(MemSample {
            total: 128 << 30,
            app: (used_gib - 2) << 30,
            wired: 2 << 30,
            compressed: Some(0),
            ..MemSample::default()
        });
        let you = account(WhySubject::Mem, Some(1), &snap(&scene)).you;
        (you.role, you.calm)
    };
    let mib = 1 << 20;
    assert_eq!(
        role(4 << 30, 16 * mib, 6),
        (Role::Main, false),
        "two thirds"
    );
    assert_eq!(
        role(4 << 30, 16 * mib, 16),
        (Role::Part, false),
        "a quarter"
    );
    assert_eq!(
        role(900 * mib, 16 * mib, 6),
        (Role::Minor, true),
        "under 1 GiB"
    );
    assert_eq!(
        role(4 << 30, 16 * mib, 64),
        (Role::Minor, true),
        "under a tenth"
    );
    assert_eq!(
        role(4 << 30, 8 << 30, 64),
        (Role::Minor, false),
        "not the largest"
    );

    let readings = |tab: u64| {
        let mut scene = spotlight_1032();
        for p in scene.procs.iter_mut().filter(|p| p.pid == 1210) {
            *p = p.clone().holding(tab);
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
        scene.mem = Some(MemSample {
            total: 128 << 30,
            app: 4 << 30,
            wired: 2 << 30,
            compressed: Some(0),
            ..MemSample::default()
        });
        scene.samples()
    };
    let beside = |tab: u64, a: Option<u64>, b: Option<u64>| {
        let mut readings = readings(tab);
        for r in readings[2].coalitions.iter_mut().flat_map(|c| &mut c.rows) {
            match r.id {
                1092 => r.footprint = a,
                1100 => r.footprint = b,
                _ => {}
            }
        }
        account(WhySubject::Mem, Some(1), &last(&readings)).you.role
    };
    let both = |fp| beside(4 << 30, fp, fp);
    assert_eq!(
        both(Some(5 << 29)),
        Role::Main,
        "2.5 GiB each, 5 GiB in all"
    );
    assert_eq!(both(Some(9 << 29)), Role::Part, "4.5 GiB, larger");
    assert_eq!(both(None), Role::Unavailable("not_sampled"), "unread");
    assert_eq!(
        beside(4 << 30, Some(5 << 29), None),
        Role::Unavailable("not_sampled"),
        "one unread"
    );
    assert_eq!(
        beside(4 << 30, Some(9 << 29), None),
        Role::Part,
        "one read larger"
    );
    assert_eq!(
        beside(900 << 20, None, None),
        Role::Minor,
        "under 1 GiB either way"
    );
    let cold = last(&readings(4 << 30)[..2]);
    assert!(
        cold.why.is_none() && cold.coal_lane.on(),
        "CONTROL: no window"
    );
    let you = account(WhySubject::Mem, Some(1), &cold).you;
    assert_eq!(
        (you.role, you.figure.is_some()),
        (Role::Unavailable("warming"), true),
        "before the first window"
    );
}

#[test]
fn memory_competition_uses_current_foreign_charge_without_counting_the_tab_twice() {
    let mut scene = super::fixtures::delegated_1032(500, 100);
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
            1187 => 1 << 20,
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
    for launcher_now_owned in [false, true] {
        let mut vitals = Vitals::new();
        vitals.absorb(&scene.reading(0, true));
        vitals.absorb(&scene.reading(4_000, false));
        let mut now = scene.reading(5_000, true);
        if launcher_now_owned {
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
        let snapshot = vitals.absorb(&now);
        let foreign = snapshot
            .why
            .as_ref()
            .unwrap()
            .split
            .rows
            .iter()
            .find(|row| row.id == 4000)
            .unwrap();
        assert_eq!(
            foreign.part,
            Part::Others,
            "CPU history remains foreign or mixed"
        );
        assert_eq!(foreign.footprint, Some(5 << 30));
        assert_eq!(
            foreign.footprint_aterm, launcher_now_owned,
            "memory follows the current census"
        );
        let you = account(WhySubject::Mem, Some(1), &snapshot).you;
        assert_eq!(
            (you.role, you.figure, you.share_pm),
            (Role::Main, Some(4 << 30), None)
        );
        if !launcher_now_owned {
            for (owned_bytes, unread) in [(4 << 30, 1), (6 << 30, 0)] {
                let mut partial = snapshot.clone();
                let row = Arc::make_mut(partial.why.as_mut().unwrap())
                    .split
                    .rows
                    .iter_mut()
                    .find(|row| row.id == 4000)
                    .unwrap();
                row.owned_footprint.bytes = owned_bytes;
                row.owned_footprint.unread = unread;
                let partial = account(WhySubject::Mem, Some(1), &partial).you;
                assert_eq!(
                    partial.role,
                    Role::Unavailable("not_sampled"),
                    "unread or inconsistent app subsets cannot prove a foreign remainder"
                );
                assert_eq!(partial.figure, Some(4 << 30));
            }
        }
        let stale = vitals.absorb(&scene.reading(6_000, false));
        assert!(stale.why.as_ref().unwrap().at_ns < stale.at_ns);
        let stale = account(WhySubject::Mem, Some(1), &stale).you;
        assert_eq!(
            stale.role,
            Role::Unavailable("not_sampled"),
            "old coalition charges cannot establish the current footprint ranking"
        );
        assert_eq!(stale.figure, Some(4 << 30));
    }
}

/// A bare `sys why` with the lane on and no window read yet is never calm:
/// the coalitions are unread, so no cause is known not to hold.
#[test]
fn a_bare_why_before_the_lanes_first_window_is_never_calm() {
    let [base, sweep, _] = read_calm(spotlight_1032()).samples();
    let s = last(&[base, sweep]);
    assert!(s.why.is_none() && s.coal_lane.on());
    assert_eq!(bare_subject(&s), Bare::Subject(WhySubject::Cpu));
}

/// A lead kept from the last tick is the one built anew from the same reads
/// and no other: every field tells. NEGATIVE CONTROL: each read moved alone
/// reads as another lead.
#[test]
fn a_kept_lead_is_the_one_built_from_the_same_reads() {
    let id = LeadIdentity {
        pid: 387,
        birth_ms: 2_000,
        exe: "mds".into(),
        voucher: Voucher::Apple,
        signing_id: Some("com.apple.mds".into()),
        team: None,
    };
    let unread = LeadIdentity {
        exe: String::new(),
        ..id.clone()
    };
    for read in [None, Some(&id), Some(&unread)] {
        let lead = Lead::of(387, 2_000, Some(0), read);
        assert!(lead.is(387, 2_000, Some(0), read), "{read:?}");
        assert!(!lead.is(388, 2_000, Some(0), read), "{read:?}");
        assert!(!lead.is(387, 2_001, Some(0), read), "{read:?}");
        assert!(!lead.is(387, 2_000, Some(501), read), "{read:?}");
    }
    let lead = Lead::of(387, 2_000, Some(0), Some(&id));
    for other in [
        LeadIdentity {
            exe: "mdx".into(),
            ..id.clone()
        },
        unread.clone(),
        LeadIdentity {
            voucher: Voucher::AdHoc,
            ..id.clone()
        },
        LeadIdentity {
            signing_id: None,
            ..id.clone()
        },
        LeadIdentity {
            team: Some("ABCDE12345".into()),
            ..id.clone()
        },
    ] {
        assert!(!lead.is(387, 2_000, Some(0), Some(&other)), "{other:?}");
        assert_ne!(lead, Lead::of(387, 2_000, Some(0), Some(&other)));
    }
    assert!(!lead.is(387, 2_000, Some(0), None));
}

/// Naming keeps last tick's lead only where it is this tick's: a lead whose
/// identity was read since, or whose identity moved, is named anew.
/// NEGATIVE CONTROL: with nothing moved, the lead is the one kept.
#[test]
fn a_coalitions_lead_is_named_anew_once_its_identity_moves() {
    let read = CoalSample {
        listed: 1,
        newest: Some(307),
        truncated: false,
        rows: vec![CoalUsage {
            id: 307,
            tasks_started: 1,
            ..CoalUsage::default()
        }],
    };
    let procs = vec![crate::model::ProcSample {
        pid: 387,
        coalition: Some(307),
        birth_ms: Some(2_000),
        uid: Some(0),
        ..crate::model::ProcSample::default()
    }];
    let mut names = IdMap::default();
    let mut ids = IdMap::default();
    let mut lead = |ids: &IdMap<(u32, i64), LeadIdentity>| {
        let _ = name(
            &mut names,
            &read,
            &procs,
            501,
            &crate::model::attribute(&procs, 1, &[]),
            &vec![None; procs.len()],
            ids,
        );
        assert_eq!(names[&307].part, Part::Services);
        names[&307].lead.clone().expect("a lead")
    };
    assert_eq!(lead(&ids).exe, None, "no identity read yet");
    ids.insert(
        (387, 2_000),
        LeadIdentity {
            pid: 387,
            birth_ms: 2_000,
            exe: "mds".into(),
            voucher: Voucher::Apple,
            ..LeadIdentity::default()
        },
    );
    let named = lead(&ids);
    assert_eq!(
        (named.exe.as_deref(), named.voucher),
        (Some("mds"), Voucher::Apple),
        "its identity read since"
    );
    assert_eq!(lead(&ids), named, "nothing moved");
    if let Some(id) = ids.get_mut(&(387, 2_000)) {
        id.voucher = Voucher::AdHoc;
    }
    assert_eq!(lead(&ids).voucher, Voucher::AdHoc, "its identity moved");
}

#[test]
fn reported_cpu_unread_owned_counter_or_identity_never_claims_complete_coverage() {
    for cpu in [
        reported_tab_read(None, false),
        reported_tab_read(Some(5_000_000), true),
    ] {
        assert!(cpu.missed, "{cpu:?}");
        assert!(cpu.tabs[0].missed, "{cpu:?}");
        assert_eq!(cpu.tabs[0].reported, 0.0);
        assert_eq!(
            cpu.cpu_accounting.execution_window,
            crate::published_cpu::CpuExecutionWindow::Unavailable
        );
    }
    let complete = reported_tab_growth(0);
    assert!(!complete.missed && !complete.tabs[0].missed);
}
