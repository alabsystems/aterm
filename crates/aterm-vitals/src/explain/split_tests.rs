// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use super::fixtures::{
    Coal, Proc, Scene, build_start_1032, delegated_1032, delegated_in_aterm_1032, reaped_1032,
    shared_launcher_1032, spotlight_1032, tab, tab_ahead_1032, two_coalitions_1421,
};
use super::*;
use crate::events::EventKind;
use crate::model::{MemSample, Split, TabView, Vitals};

fn facts(scene: &Scene) -> Arc<WhyFacts> {
    facts_edited(scene, |_, _| {})
}

fn facts_edited(scene: &Scene, edit: impl FnOnce(&mut Sample, &mut Sample)) -> Arc<WhyFacts> {
    let mut v = Vitals::new();
    let [mut base, sweep, mut now] = scene.samples();
    edit(&mut base, &mut now);
    assert_eq!(
        v.absorb(&base).why,
        None,
        "one tick is a base, not a window"
    );
    v.absorb(&sweep);
    v.absorb(&now).why.expect("a window's facts")
}

fn parts(tab: i64, aterm_rest: i64, others: i64, services: i64, unaccounted: i64) -> Parts {
    Parts {
        tab,
        aterm_rest,
        others,
        services,
        unaccounted,
    }
}

fn later(s: &Sample, k: u64) -> Sample {
    let mut s = s.clone();
    s.at_ns += k * 1_000_000_000;
    s.mono_ms += k * 1000;
    s.at_unix_ms += i64::try_from(k * 1000).unwrap_or(0);
    for c in s.cores.iter_mut().flatten() {
        c.total += k * 1000;
    }
    s
}

fn row(f: &WhyFacts, id: u64) -> &CoalRow {
    f.split
        .rows
        .iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("no row for coalition {id}"))
}

#[test]
fn the_1032_window_splits_into_its_five_parts() {
    let scene = spotlight_1032();
    let f = facts(&scene);
    assert_eq!(f.window_ns, 5_000_000_000);
    assert_eq!(f.at_unix_ms, 1_792_517_561_200);
    assert_eq!(f.timeline, scene.timeline, "the split tick's timeline");
    assert_eq!(
        f.userspace_ms,
        Some(1_792_517_318_000),
        "logd's and mds's birth, never launchd's"
    );
    assert_eq!(f.lane, CoalLane::On);
    assert_eq!((f.split.via, f.split.exact), (Via::Coalitions, true));
    assert_eq!(f.split.global_mc, 3300);
    assert_eq!(f.split.parts(Some(1)), parts(200, 200, 730, 2170, 0));
    assert_eq!(f.split.own_mc, 200, "aterm itself and aterm-link");
    let rows: i64 = f.split.rows.iter().map(|r| r.cpu_mc).sum();
    assert_eq!(rows, 3300);
    assert!(f.split.rows.iter().all(|r| r.measured));
    assert_eq!(
        f.split.rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        [307, 1092, 327, 1342, 1, 491, 247, 1100],
        "busiest first"
    );
    let mds = row(&f, 307);
    assert_eq!((mds.deferred_mc, mds.read_bps), (931, 22_900_000));
    assert_eq!((row(&f, 491).deferred_mc, row(&f, 491).read_bps), (94, 0));
    let finished: Vec<u64> = f
        .split
        .rows
        .iter()
        .filter(|r| r.finished)
        .map(|r| r.id)
        .collect();
    assert_eq!(
        finished,
        [307, 327, 1342, 1, 491],
        "rows with exited members"
    );
    let all_live = facts_edited(&scene, |base, now| {
        for c in [base, now].into_iter().flat_map(|s| s.coalitions.as_mut()) {
            for r in c.rows.iter_mut().filter(|r| r.id == 1092) {
                r.tasks_started = 3;
            }
        }
    });
    assert!(!row(&all_live, 1092).finished, "three tasks, none exited");
    let aterm = row(&f, 1342);
    assert_eq!(
        (aterm.part, aterm.cpu_mc, aterm.live),
        (Part::Aterm, 400, 4)
    );
    assert_eq!(aterm.lead.as_ref().map(|l| l.pid), Some(1187));
    assert_eq!(
        row(&f, 491).part,
        Part::Services,
        "mds_stores runs as _spotlight"
    );
    assert_eq!(
        row(&f, 1092).part,
        Part::Others,
        "siriactionsd runs as the user"
    );
    assert_eq!(f.split.parts(None), parts(0, 400, 730, 2170, 0));
    let cut = facts_edited(&scene, |_, now| now.procs_truncated = true);
    assert_eq!(cut.userspace_ms, None);
}

#[test]
fn a_listed_coalition_that_answers_nothing_is_counted_as_listed() {
    let f = facts(&spotlight_1032());
    assert_eq!(f.split.listed, 9);
    assert_eq!(f.split.rows.len(), 8);
}

#[test]
fn aterm_is_every_coalition_holding_it_or_a_tab_shell() {
    let scene = two_coalitions_1421();
    let f = facts(&scene);
    assert_eq!(f.split.global_mc, 10_000);
    assert_eq!(f.split.aterm_mc, 9760);
    assert_eq!(row(&f, 55954).part, Part::Aterm);
    assert_eq!(row(&f, 55990).part, Part::Aterm);
    assert_eq!(f.split.parts(Some(1)), parts(3350, 6410, 30, 180, 30));
    assert_eq!(f.split.parts(Some(2)), parts(80, 9680, 30, 180, 30));
    assert_eq!(f.split.own_mc, 20);

    let alone = Scene {
        tabs: vec![tab(2, 7100)],
        ..scene
    };
    let f = facts(&alone);
    assert_eq!(row(&f, 55954).part, Part::Others);
    assert_eq!(f.split.aterm_mc, 5640);
    assert_eq!(f.split.parts(Some(2)), parts(80, 5560, 4150, 180, 30));
}

#[test]
fn a_coalition_with_no_member_left_keeps_its_name_and_part() {
    let mut scene = spotlight_1032();
    scene.busy_mc += 160;
    scene.coalitions.push(Coal::new(70_000, 120));
    scene.coalitions.push(Coal::new(71_000, 40));
    let born = 1_792_517_400_000;
    scene
        .procs
        .push(Proc::new(3000, 1, 501, 70_000, born, "Preview", 120).gone());
    scene.leads.push(LeadIdentity {
        pid: 3000,
        birth_ms: born,
        exe: "Preview".into(),
        voucher: Voucher::Apple,
        signing_id: Some("com.apple.Preview".into()),
        team: None,
    });
    let f = facts(&scene);
    let kept = row(&f, 70_000);
    assert_eq!((kept.part, kept.cpu_mc, kept.live), (Part::Others, 120, 0));
    let lead = kept.lead.as_ref().expect("the name it had");
    assert_eq!((lead.pid, lead.exe.as_deref()), (3000, Some("Preview")));
    assert_eq!(lead.voucher, Voucher::Apple);
    let unseen = row(&f, 71_000);
    assert_eq!((unseen.part, unseen.lead.as_ref()), (Part::Services, None));
    assert_eq!(f.split.parts(Some(1)), parts(200, 200, 850, 2210, 0));
}

#[test]
fn a_coalition_made_or_reaped_inside_the_window() {
    let f = facts(&reaped_1032());
    let made = row(&f, 90_000);
    assert_eq!((made.cpu_mc, made.measured), (300, true));
    assert!(f.split.rows.iter().all(|r| r.id != 80_000));
    assert_eq!(f.split.parts(Some(1)), parts(200, 200, 1030, 2170, 250));
    assert!(f.split.exact);

    let mut behind = spotlight_1032();
    behind.busy_mc -= 50;
    let f = facts(&behind);
    assert_eq!(f.split.parts(Some(1)), parts(200, 200, 730, 2170, -50));
}

#[test]
fn a_delegated_member_counts_once_in_its_tab_capped_at_its_launcher() {
    let job = |mc: u64, launcher_mc: u64| facts(&delegated_1032(mc, launcher_mc));
    let f = job(500, 100);
    let launcher = row(&f, 4000);
    assert_eq!((launcher.part, launcher.delegated_mc), (Part::Others, 500));
    assert_eq!(f.split.delegated_mc, 500);
    assert_eq!(f.split.parts(Some(1)), parts(700, 200, 830, 2170, 0));

    let f = job(700, 0);
    assert_eq!(
        row(&f, 4000).delegated_mc,
        600,
        "capped at the launcher's 600"
    );
    let p = f.split.parts(Some(1));
    assert_eq!(p, parts(900, 100, 730, 2170, 0));
    assert_eq!(
        p.tab + p.aterm_rest + p.others + p.services + p.unaccounted,
        3900
    );

    let mut closed = delegated_1032(500, 100);
    closed.delegated[0].tab = None;
    let f = facts(&closed);
    assert_eq!(f.split.parts(Some(1)), parts(200, 700, 830, 2170, 0));
    assert_eq!(
        (f.split.aterm_mc, f.split.delegated_mc, f.split.own_mc),
        (400, 500, 700)
    );
}

#[test]
fn two_tabs_delegated_jobs_under_one_launcher_count_each_member_once_in_its_own_tab() {
    let scene = shared_launcher_1032();
    let f = facts(&scene);
    let launcher = row(&f, 4000);
    assert_eq!(
        (launcher.part, launcher.cpu_mc, launcher.delegated_mc),
        (Part::Others, 900, 800),
        "both members leave the launcher's part; its own 100 stays"
    );
    assert_eq!(launcher.tabs, None, "the app server is in no tab");
    assert_eq!(f.split.delegated_mc, 800);
    assert_eq!(
        f.split.tabs,
        [(1, 700), (2, 300)],
        "each member in its own tab"
    );
    assert_eq!((f.split.own_mc, f.split.unattributed_mc), (200, 0));
    assert_eq!(f.split.global_mc, 4200);
    assert_eq!(f.split.parts(Some(1)), parts(700, 500, 830, 2170, 0));
    assert_eq!(f.split.parts(Some(2)), parts(300, 900, 830, 2170, 0));
    for tab in [1, 2] {
        let p = f.split.parts(Some(tab));
        assert_eq!(
            p.tab + p.aterm_rest + p.others + p.services + p.unaccounted,
            4200,
            "tab {tab}'s view counts every member once"
        );
    }

    let mut claimed_twice = scene.clone();
    claimed_twice.busy_mc += 150;
    for c in claimed_twice.coalitions.iter_mut().filter(|c| c.id == 4000) {
        c.mc += 150;
    }
    let rustc = Proc::new(4030, 4001, 501, 4000, 1_792_517_500_030, "rustc", 150);
    let key = crate::delegated::ProcessKey {
        pid: rustc.pid,
        start: u64::try_from(rustc.birth_ms).unwrap_or(0),
    };
    claimed_twice.procs.push(rustc);
    for job in &mut claimed_twice.delegated {
        job.members.push(key);
    }
    let f = facts(&claimed_twice);
    assert_eq!(
        (f.split.tabs.clone(), f.split.delegated_mc),
        (vec![(1, 700), (2, 300)], 800),
        "a member both jobs claim is neither tab's"
    );
    assert_eq!(f.split.parts(Some(1)), parts(700, 500, 980, 2170, 0));

    let mut exited = scene;
    exited.busy_mc += 200;
    for c in exited.coalitions.iter_mut().filter(|c| c.id == 4000) {
        c.mc += 200;
    }
    let rustc = Proc::new(4040, 4001, 501, 4000, 1_792_517_500_040, "rustc", 200).gone();
    let key = crate::delegated::ProcessKey {
        pid: rustc.pid,
        start: u64::try_from(rustc.birth_ms).unwrap_or(0),
    };
    exited.procs.push(rustc);
    for job in exited.delegated.iter_mut().filter(|j| j.tab == Some(2)) {
        job.members.push(key);
    }
    let f = facts(&exited);
    assert_eq!(
        (f.split.tabs.clone(), f.split.delegated_mc),
        (vec![(1, 700), (2, 300)], 800)
    );
    let launcher = row(&f, 4000);
    assert_eq!((launcher.cpu_mc, launcher.delegated_mc), (1100, 800));
    assert_eq!(
        f.split.parts(Some(2)),
        parts(300, 900, 1030, 2170, 0),
        "a member that exited inside the window stays in its launcher's part"
    );
}

#[test]
fn a_tab_read_past_aterms_coalitions_leaves_the_residual_negative() {
    let f = facts(&tab_ahead_1032());
    assert_eq!(f.split.aterm_mc, 400);
    assert_eq!(f.split.parts(Some(1)), parts(450, 0, 730, 2170, -50));
}

#[test]
fn a_part_is_rounded_once_not_per_coalition() {
    let mut scene = spotlight_1032();
    scene.busy_mc += 8;
    let f = facts_edited(&scene, |base, now| {
        for (s, ns) in [(base, 1_000_000), (now, 5_000_000)] {
            let c = s.coalitions.as_mut().expect("a split tick");
            c.rows.extend((600..610).map(|id| CoalUsage {
                id,
                tasks_started: 1,
                cpu_ns: ns,
                ..CoalUsage::default()
            }));
        }
    });
    assert_eq!(f.split.parts(Some(1)), parts(200, 200, 730, 2178, 0));
    assert_eq!(
        (600..610).map(|id| row(&f, id).cpu_mc).collect::<Vec<_>>(),
        [1, 1, 1, 1, 1, 1, 1, 1, 0, 0],
        "ten 0.80s rounded as a set, the lower ids up first"
    );
    assert_eq!(
        f.split.rows.iter().map(|r| r.cpu_mc).sum::<i64>(),
        3308,
        "the rows add up to the parts, where one by one they would read 3310"
    );

    let calm = Scene {
        busy_mc: 2,
        coalitions: Vec::new(),
        unanswered: 0,
        procs: vec![Proc::new(800, 1, 501, 700, 1_792_517_400_000, "Notes", 0)],
        ..spotlight_1032()
    };
    let f = facts_edited(&calm, |base, now| {
        for (s, end) in [(base, 0), (now, 1)] {
            let c = s.coalitions.as_mut().expect("a split tick");
            c.rows.extend(
                [(700, 2_000_000), (600, 8_000_000)].map(|(id, ns)| CoalUsage {
                    id,
                    tasks_started: 1,
                    cpu_ns: ns * end,
                    ..CoalUsage::default()
                }),
            );
        }
    });
    assert_eq!(f.split.global_mc, 2);
    assert_eq!(f.split.parts(None), parts(0, 0, 0, 2, 0));
}

#[test]
fn the_tab_is_read_over_the_splits_window_not_the_last_sweeps() {
    let mut v = Vitals::new();
    let [base, sweep, now] = build_start_1032().samples();
    v.absorb(&base);
    v.absorb(&sweep);
    let snap = v.absorb(&now);
    let sweep_mc = millicores(snap.tabs[0].scope.cores.raw);
    assert_eq!(sweep_mc, 3000, "the Deck's figure, over the sweep");
    let f = snap.why.expect("a window's facts");
    assert_eq!(f.split.parts(Some(1)), parts(1000, 200, 730, 2170, 0));
}

#[test]
fn a_delegated_member_in_aterms_coalition_is_not_moved_again() {
    let f = facts(&delegated_in_aterm_1032(500));
    let aterm = row(&f, 1342);
    assert_eq!((aterm.cpu_mc, aterm.delegated_mc), (900, 0));
    assert_eq!(f.split.delegated_mc, 0);
    assert_eq!(f.split.parts(Some(1)), parts(700, 200, 730, 2170, 0));
}

#[test]
fn with_the_lane_off_the_split_is_the_built_one_by_subtraction() {
    for lane in [CoalLane::Unsupported, CoalLane::Off] {
        let scene = Scene {
            lane,
            ..spotlight_1032()
        };
        let mut v = Vitals::new();
        let [base, sweep, now] = scene.samples();
        v.absorb(&base);
        v.absorb(&sweep);
        let snap = v.absorb(&now);
        let built = snap.split.expect("the built split");
        let f = snap.why.expect("facts by subtraction");
        assert_eq!(f.lane, lane);
        assert_eq!(f.timeline, scene.timeline);
        assert_eq!(f.window_ns, 1_000_000_000, "the sweep's window");
        let mc = |cores: f32| (f64::from(cores) * 1000.0).round() as i64;
        let s = &f.split;
        assert_eq!((s.via, s.exact, s.listed), (Via::Subtraction, false, 0));
        assert_eq!(
            (s.global_mc, s.aterm_mc, s.others_mc, s.services_mc),
            (
                mc(built.global),
                mc(built.aterm),
                mc(built.others),
                mc(built.services)
            )
        );
        assert!((s.unaccounted_mc - mc(built.unaccounted)).abs() <= 2);
        assert_eq!(s.tabs, [(1, 200)]);
        let p = s.parts(Some(1));
        assert_eq!(
            p.tab + p.aterm_rest + p.others + p.services + p.unaccounted,
            s.global_mc
        );
        assert!(s.rows.is_empty());
    }
    assert_eq!(CoalLane::Unsupported.word(), Some("lane_off:coalitions"));
    assert_eq!(CoalLane::Off.word(), Some("lane_off:coalitions"));
    assert_eq!(CoalLane::On.word(), None);
}

#[test]
fn a_base_older_than_a_minute_only_starts_the_next_window() {
    let next = |v: &mut Vitals, now: &Sample| {
        let f = v.absorb(&later(now, 5)).why.expect("the next window");
        assert_eq!(f.window_ns, 5_000_000_000);
    };
    let scene = Scene {
        window_ms: 61_000,
        ..spotlight_1032()
    };
    let mut v = Vitals::new();
    let samples = scene.samples();
    for s in &samples {
        assert_eq!(v.absorb(s).why, None);
    }
    next(&mut v, &samples[2]);

    let mut v = Vitals::new();
    let [mut base, sweep, now] = spotlight_1032().samples();
    base.cores.iter_mut().for_each(|c| c.truncate(17));
    for s in [&base, &sweep, &now] {
        assert_eq!(v.absorb(s).why, None);
    }
    next(&mut v, &now);
}

#[test]
fn by_subtraction_aterms_own_work_is_never_below_zero() {
    let tab = |key: u64, cores: f32| {
        let mut t = TabView {
            key,
            ..TabView::default()
        };
        t.scope.cores.raw = cores;
        t
    };
    let built = Split {
        global: 1.0,
        aterm: 0.3,
        others: 0.2,
        services: 0.5,
        ..Split::default()
    };
    let s = CoalSplit::by_subtraction(&built, &[tab(1, 0.2), tab(2, 0.2)], &[]);
    assert_eq!((s.aterm_mc, s.tabs.len(), s.own_mc), (300, 2, 0));
}

#[test]
fn a_footprint_no_memory_holds_turns_the_lane_off_for_good() {
    const GIB: u64 = 1 << 30;
    let reads = |s: &mut Sample, footprint: u64| {
        s.mem = Some(MemSample {
            total: 16 * GIB,
            swap_total: 4 * GIB,
            ..MemSample::default()
        });
        s.coalitions.as_mut().expect("a split tick").rows[0].footprint = Some(footprint);
    };
    let later = |now: &Sample, k: u64, footprint: u64| {
        let mut s = later(now, k);
        reads(&mut s, footprint);
        s
    };
    let lane_events = |snap: &Snapshot| {
        snap.events
            .iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    EventKind::Lane {
                        lane: "coalitions",
                        ..
                    }
                )
            })
            .count()
    };
    let mut v = Vitals::new();
    let [base, sweep, mut now] = spotlight_1032().samples();
    reads(&mut now, 19 * GIB);
    v.absorb(&base);
    v.absorb(&sweep);
    let busy = v.absorb(&now);
    assert_eq!(busy.why.as_ref().map(|f| f.lane), Some(CoalLane::On));
    assert_eq!(lane_events(&busy), 0, "the first facts seed the lane");
    let edge = v.absorb(&later(&now, 1, 150 * GIB));
    assert_eq!(edge.why.as_ref().map(|f| f.lane), Some(CoalLane::On));

    let off = v.absorb(&later(&now, 2, 3_500_000_000_000));
    let f = off.why.as_ref().expect("facts by subtraction");
    assert_eq!((f.lane, f.split.via), (CoalLane::Off, Via::Subtraction));
    assert!(off.events.iter().any(|e| e.kind
        == EventKind::Lane {
            lane: "coalitions",
            available: false
        }));

    let sane = v.absorb(&later(&now, 3, 19 * GIB));
    assert_eq!(sane.why.as_ref().map(|f| f.lane), Some(CoalLane::Off));
    assert_eq!(lane_events(&sane), 0, "no flapping");
}

#[test]
fn a_coalition_the_base_did_not_read_has_no_figure_over_the_window() {
    let drop_327 = facts_edited(&spotlight_1032(), |base, _| {
        if let Some(c) = base.coalitions.as_mut() {
            c.rows.retain(|r| r.id != 327);
        }
    });
    let unread = row(&drop_327, 327);
    assert_eq!((unread.cpu_mc, unread.measured), (0, false));
    assert_eq!(
        drop_327.split.parts(Some(1)),
        parts(200, 200, 730, 1710, 460)
    );
    assert!(!drop_327.split.exact);

    let drop_newest = facts_edited(&spotlight_1032(), |base, _| {
        if let Some(c) = base.coalitions.as_mut() {
            c.rows.retain(|r| r.id != 1342);
        }
    });
    assert_eq!(
        (
            row(&drop_newest, 1342).cpu_mc,
            row(&drop_newest, 1342).measured
        ),
        (0, false)
    );
    assert_eq!(
        drop_newest.split.parts(Some(1)),
        parts(200, 0, 730, 2170, 200)
    );
    assert!(!drop_newest.split.exact);
    let drop_all = facts_edited(&spotlight_1032(), |base, _| {
        if let Some(c) = base.coalitions.as_mut() {
            c.rows.clear();
        }
    });
    assert!(drop_all.split.rows.iter().all(|r| !r.measured));
    assert_eq!(drop_all.split.parts(Some(1)), parts(200, 0, 0, 0, 3100));
    assert!(!drop_all.split.exact);

    let went_back = facts_edited(&spotlight_1032(), |base, now| {
        let before = base.coalitions.as_ref().expect("a split tick");
        let at_base = before.rows.iter().find(|r| r.id == 307).map(|r| r.cpu_ns);
        for r in now.coalitions.iter_mut().flat_map(|c| &mut c.rows) {
            if r.id == 307 {
                r.cpu_ns = at_base.unwrap_or(1) - 1;
            }
        }
    });
    assert_eq!(
        (row(&went_back, 307).cpu_mc, row(&went_back, 307).measured),
        (0, false)
    );
    assert_eq!(
        went_back.split.parts(Some(1)),
        parts(200, 200, 730, 880, 1290)
    );
    assert!(!went_back.split.exact);

    let short = facts_edited(&reaped_1032(), |base, _| {
        if let Some(c) = base.coalitions.as_mut() {
            c.truncated = true;
        }
    });
    let made = row(&short, 90_000);
    assert_eq!((made.cpu_mc, made.measured), (0, false));
    assert_eq!(short.split.parts(Some(1)), parts(200, 200, 730, 2170, 550));
    assert!(!short.split.exact);

    let edits: [fn(&mut Sample, &mut Sample); 2] = [
        |_, now| {
            if let Some(c) = now.coalitions.as_mut() {
                c.truncated = true;
            }
        },
        |base, _| base.procs_truncated = true,
    ];
    for edit in edits {
        let f = facts_edited(&spotlight_1032(), edit);
        assert_eq!(f.split.parts(Some(1)), parts(200, 200, 730, 2170, 0));
        assert!(!f.split.exact);
    }
}

#[test]
fn a_tab_pid_reused_inside_the_window_is_not_the_tabs_live_work() {
    let scene = spotlight_1032();
    assert_eq!(
        facts(&scene).split.parts(Some(1)),
        parts(200, 200, 730, 2170, 0),
        "matching known starts preserve the tab's live work"
    );
    let f = facts_edited(&scene, |_, now| {
        for p in now.procs.iter_mut().flatten() {
            if p.pid == 1210 {
                p.start = p.start.map(|s| s + 1);
            }
        }
    });
    assert_eq!(f.split.parts(Some(1)), parts(0, 400, 730, 2170, 0));

    for (missing_base, missing_now) in [(true, false), (false, true), (true, true)] {
        let f = facts_edited(&scene, |base, now| {
            for (sample, missing) in [(base, missing_base), (now, missing_now)] {
                if missing {
                    for p in sample.procs.iter_mut().flatten().filter(|p| p.pid == 1210) {
                        p.start = None;
                    }
                }
            }
        });
        assert_eq!(
            f.split.parts(Some(1)),
            parts(0, 400, 730, 2170, 0),
            "an unknown start cannot prove the same live process across the window"
        );
    }
}

#[test]
fn a_coalition_takes_its_part_from_every_member_in_the_kernels_order() {
    let late = 1_792_517_560_000;
    let mut scene = spotlight_1032();
    scene
        .procs
        .push(Proc::new(2000, 1, 0, 1092, late, "helper", 0));
    scene
        .procs
        .push(Proc::new(159, 1210, 501, 1342, late + 500, "git", 0));
    let f = facts(&scene);
    assert_eq!(row(&f, 1092).part, Part::Others);
    let aterm = row(&f, 1342);
    assert_eq!((aterm.part, aterm.live), (Part::Aterm, 5));
    assert_eq!(aterm.lead.as_ref().map(|l| l.pid), Some(1187));
    assert_eq!(f.split.parts(Some(1)), parts(200, 200, 730, 2170, 0));
}

#[test]
fn a_lead_keeps_its_identity_while_the_same_process_leads() {
    let mut scene = spotlight_1032();
    scene.leads.push(LeadIdentity {
        pid: 387,
        birth_ms: 1_792_517_318_000,
        exe: "mds".into(),
        voucher: Voucher::Apple,
        signing_id: Some("com.apple.mds".into()),
        team: None,
    });
    let mds = |f: &WhyFacts| {
        let lead = row(f, 307).lead.clone().expect("a lead");
        assert_eq!(
            (
                lead.exe.as_deref(),
                lead.voucher,
                lead.signing_id.as_deref()
            ),
            (Some("mds"), Voucher::Apple, Some("com.apple.mds"))
        );
    };
    mds(&facts_edited(&scene, |_, now| now.leads.clear()));
    mds(&facts_edited(&scene, |base, now| {
        if let Some(c) = base.coalitions.as_mut() {
            c.rows.retain(|r| r.id != 307);
        }
        now.leads.clear();
    }));
    mds(&facts_edited(&scene, |base, now| {
        for p in base.procs.iter_mut().flatten().filter(|p| p.pid == 387) {
            p.birth_ms = None;
        }
        now.leads.clear();
    }));
    let mut v = Vitals::new();
    let [mut base, sweep, mut now] = scene.samples();
    let mut first = base.clone();
    first.cores = None;
    first.at_ns -= 1_000_000_000;
    first.mono_ms -= 1000;
    first.at_unix_ms -= 1000;
    base.leads.clear();
    now.leads.clear();
    for s in [&first, &base, &sweep] {
        v.absorb(s);
    }
    mds(&v.absorb(&now).why.expect("a window's facts"));

    let mut v = Vitals::new();
    let [base, sweep, now] = scene.samples();
    let mut cut = now.clone();
    cut.leads.clear();
    cut.procs_truncated = true;
    if let Some(procs) = cut.procs.as_mut() {
        procs.retain(|p| p.pid != 387);
    }
    let mut found = later(&now, 5);
    found.leads.clear();
    for s in [&base, &sweep, &cut] {
        v.absorb(s);
    }
    mds(&v.absorb(&found).why.expect("a window's facts"));

    let reborn = facts_edited(&scene, |_, now| {
        now.leads.clear();
        for p in now.procs.iter_mut().flatten() {
            if p.pid == 387 {
                p.birth_ms = p.birth_ms.map(|b| b + 1);
            }
        }
    });
    let lead = row(&reborn, 307).lead.clone().expect("a lead");
    assert_eq!((lead.exe, lead.voucher), (None, Voucher::Unknown));

    let mut v = Vitals::new();
    let [base, sweep, now] = scene.samples();
    let mut unreadable = now.clone();
    unreadable.leads = vec![LeadIdentity {
        pid: 387,
        birth_ms: 1_792_517_318_000,
        ..LeadIdentity::default()
    }];
    for s in [&base, &sweep] {
        v.absorb(s);
    }
    let unknown = v.absorb(&unreadable).why.expect("a window's facts");
    let lead = row(&unknown, 307).lead.clone().expect("a lead");
    assert_eq!(
        (lead.exe, lead.voucher, lead.signing_id, lead.team),
        (None, Voucher::Unknown, None, None)
    );
    let recovered = later(&now, 5);
    mds(&v.absorb(&recovered).why.expect("a current identity"));

    let unborn = facts_edited(&scene, |base, now| {
        for p in [base, now]
            .into_iter()
            .flat_map(|s| s.procs.iter_mut().flatten())
            .filter(|p| p.pid == 940)
        {
            p.birth_ms = None;
        }
    });
    let finder = row(&unborn, 1100);
    assert_eq!((finder.part, finder.lead.as_ref()), (Part::Others, None));
}

#[test]
fn a_reading_between_split_ticks_carries_the_last_facts() {
    let mut v = Vitals::new();
    let [base, sweep, now] = spotlight_1032().samples();
    v.absorb(&base);
    v.absorb(&sweep);
    let facts = v.absorb(&now).why.expect("a window's facts");
    let mut later = sweep.clone();
    later.at_ns = now.at_ns + 1_000_000_000;
    later.mono_ms = now.mono_ms + 1000;
    later.at_unix_ms = now.at_unix_ms + 1000;
    later.cores = now.cores.clone();
    let carried = v.absorb(&later).why.expect("carried");
    assert!(Arc::ptr_eq(&facts, &carried));
}

#[test]
fn leads_are_the_earliest_born_never_the_lowest_pid() {
    let at = |h: i64, m: i64, s: i64| ((h * 60 + m) * 60 + s) * 1000;
    let members = [
        (159, 1342, at(19, 16, 0)),
        (1187, 1342, at(17, 31, 55)),
        (1210, 1342, at(17, 32, 0)),
        (387, 307, at(17, 28, 38)),
        (533, 491, at(17, 28, 39)),
        (532, 491, at(17, 28, 39)),
        (0, 1, 0),
        (1, 1, at(16, 6, 14)),
    ];
    assert_eq!(
        leads(&members),
        [(1, 1), (307, 387), (491, 532), (1342, 1187)]
    );
}
