// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;

use super::fixtures::{
    Coal, Proc, Scene, build_start_1032, delegated_1032, delegated_in_aterm_1032, reaped_1032,
    spotlight_1032, tab, tab_ahead_1032, two_coalitions_1421,
};
use super::*;
use crate::events::EventKind;
use crate::model::{MemSample, Split, TabView, Vitals};

/// The facts after reading `scene` through a fresh model.
fn facts(scene: &Scene) -> Arc<WhyFacts> {
    facts_edited(scene, |_, _| {})
}

/// The same, with the window's two split ticks edited by `edit`.
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

/// `s`, `k` seconds on, its cores' ticks moved so it has a window.
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

/// RD's 10:32 moment: 3300 = 200 + 200 + 730 + 2170 + 0, every part over the
/// one 5 s window ending 17:32:41.200Z, and the rows add up to the figure.
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
    // A caller in no tab: all of aterm is the rest.
    assert_eq!(f.split.parts(None), parts(0, 400, 730, 2170, 0));
    // A sweep cut at its cap misses the oldest processes (the kernel lists
    // the newest first): no start of userspace is read from it.
    let cut = facts_edited(&scene, |_, now| now.procs_truncated = true);
    assert_eq!(cut.userspace_ms, None);
}

/// Coalition 2 is listed and answers nothing: the split's count is the
/// list's, not its rows' (that it never reads as a zero row is the reader's,
/// `aterm-sysprobe`'s coalitions tests).
#[test]
fn a_listed_coalition_that_answers_nothing_is_counted_as_listed() {
    let f = facts(&spotlight_1032());
    assert_eq!(f.split.listed, 9);
    assert_eq!(f.split.rows.len(), 8);
}

/// SA's 14:21 table: aterm is TWO coalitions, its own and the predecessor's
/// that still holds the adopted tab 1. NEGATIVE CONTROL: with tab 1 not this
/// instance's, its coalition is the user's other work.
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

/// A coalition whose last member exited inside the window keeps the part and
/// the name it had; one never seen with a member is a service with no name.
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

/// A coalition made inside the window (numbered above every one the base
/// listed) counts from 0; one reaped inside it ran work no part holds, so
/// the residual shows it, signed.
#[test]
fn a_coalition_made_or_reaped_inside_the_window() {
    let f = facts(&reaped_1032());
    let made = row(&f, 90_000);
    assert_eq!((made.cpu_mc, made.measured), (300, true));
    assert!(f.split.rows.iter().all(|r| r.id != 80_000));
    assert_eq!(f.split.parts(Some(1)), parts(200, 200, 1030, 2170, 250));
    assert!(f.split.exact);

    // The cores' ticks behind the counters: a negative residual, kept.
    let mut behind = spotlight_1032();
    behind.busy_mc -= 50;
    let f = facts(&behind);
    assert_eq!(f.split.parts(Some(1)), parts(200, 200, 730, 2170, -50));
}

/// A tab's delegated job under a launcher outside aterm (`resource-run`
/// under an app server): its member counts in the tab and leaves the
/// launcher's part, once; read ahead of its launcher's counter, it takes no
/// more than the launcher's own figure, so no part goes below 0.
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

    // Its tab closed: the member's work is aterm's own, in the rest, still
    // once; aterm's own work is then of aterm's coalitions and the delegated
    // work, past aterm's coalitions alone.
    let mut closed = delegated_1032(500, 100);
    closed.delegated[0].tab = None;
    let f = facts(&closed);
    assert_eq!(f.split.parts(Some(1)), parts(200, 700, 830, 2170, 0));
    assert_eq!(
        (f.split.aterm_mc, f.split.delegated_mc, f.split.own_mc),
        (400, 500, 700)
    );
}

/// The tab's live figure read past aterm's coalition (the sweep read it after
/// the coalition's counter): the rest of aterm is 0, never below, and the
/// residual takes the 0.05, signed.
#[test]
fn a_tab_read_past_aterms_coalitions_leaves_the_residual_negative() {
    let f = facts(&tab_ahead_1032());
    assert_eq!(f.split.aterm_mc, 400);
    assert_eq!(f.split.parts(Some(1)), parts(450, 0, 730, 2170, -50));
}

/// Hundreds of coalitions each run under a millicore on a calm Mac: each
/// part is summed before it is rounded, so the residual holds none of their
/// rounding. Ten services at 0.8 mc each read 1 apiece and 8 together.
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
    assert_eq!(row(&f, 600).cpu_mc, 1, "a row is rounded on its own");
    assert_eq!(f.split.parts(Some(1)), parts(200, 200, 730, 2178, 0));

    // Each part rounds half up, as the cores' figure does, so a window that
    // balances leaves no residual: 2.0 mc busy, the user's 0.4 and a
    // service's 1.6 (truncated, 0 and 1, the residual would read 1).
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

/// Every part is over the split's window: as a build starts, the tab reads
/// its 1.00 over the 5 s window, never the 3.00 of the last 1 s sweep the
/// Deck shows.
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

/// A delegated member in aterm's own coalition (a detached job a tab started
/// keeps the coalition it was made in): aterm's counter already holds its
/// work, so nothing moves and it counts once, in its tab.
#[test]
fn a_delegated_member_in_aterms_coalition_is_not_moved_again() {
    let f = facts(&delegated_in_aterm_1032(500));
    let aterm = row(&f, 1342);
    assert_eq!((aterm.cpu_mc, aterm.delegated_mc), (900, 0));
    assert_eq!(f.split.delegated_mc, 0);
    assert_eq!(f.split.parts(Some(1)), parts(700, 200, 730, 2170, 0));
}

/// With the lane off the facts are the built split by subtraction, over the
/// sweep's window, with the same parts.
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
    assert_eq!(CoalLane::Unsupported.word(), Some("unsupported"));
    assert_eq!(CoalLane::Off.word(), Some("lane_off:coalitions"));
    assert_eq!(CoalLane::On.word(), None);
}

/// A base older than a minute starts the next window and splits nothing; so
/// does one over another count of cores. The reading that found it stale is
/// the next window's base: a split tick 5 s after it has a window.
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

/// By subtraction, aterm's own work is aterm's less its tabs', never below 0
/// where the tabs' rounded figures pass aterm's.
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
    let s = CoalSplit::by_subtraction(&built, &[tab(1, 0.2), tab(2, 0.2)]);
    assert_eq!((s.aterm_mc, s.tabs.len(), s.own_mc), (300, 2, 0));
}

/// A footprint counts each compressed page at its full size, swapped out or
/// not, so on a Mac under pressure the coalitions' sum passes physical
/// memory: 19 GiB on a 16 GiB Mac keeps the lane on. A sum no such machine
/// holds (a cycle count read from the footprint's slot) is a record the
/// readers misread: the facts fall back to the split by subtraction and the
/// lane is off for good, so the coalitions' lane event says so once, never
/// again on the sane reads after it.
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
    // Memory and swap together: 150 GiB is under 8 × 20 GiB.
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

/// The kernel numbers coalitions in order and never reuses an id, so only
/// one listed above the base's newest was made inside the window. One the
/// base did not read, one whose counter went back (a misread), and every new
/// one after a base list that stopped short read 0 over the window, never a
/// lifetime charged to one window: their work is the residual's, and the
/// split is heuristic. So is one whose list or sweep stopped short at
/// either end.
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

    // The base's newest row unread, or every row: the list still says they
    // were there, so none counts from 0.
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

/// A tab's pid reused inside the window by a new process (a new kernel
/// start) is not the process the base read; an unread start at either end
/// cannot prove that identity either. The tab does not count it, and aterm's
/// coalition, whose counter holds the work, keeps it in the rest.
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

/// The kernel lists the newest process first, so a coalition's first-swept
/// member is seldom what settles its part: a later tab shell makes it
/// aterm's, and a later process of the user's makes it others' though another
/// user's helper was swept first. Pid 159, made after the pid space wrapped
/// and swept first, never names aterm's coalition 1342 (RD §2.4).
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

/// A lead read on an earlier tick keeps its identity while the same process
/// is swept, though the host sent it only once: on a tick whose coalition
/// answered nothing, whose lead's birth was not read, or that made no base;
/// a new process under the pid (another birth) is not vouched for by it.
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

    // A sweep cut at its cap that missed the lead: it keeps its identity
    // while it leads, for the next whole sweep that finds it.
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

    // Live members none of whose births were read: the part, and no name.
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

/// A reading with no split tick carries the last facts.
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

/// A coalition is named by its earliest-born live member, never its lowest
/// pid: pid 159, born at 12:16 after the pid space wrapped, never names
/// aterm's coalition 1342, whose lead is aterm (pid 1187, born 10:31:55;
/// RD §2.4). Ties go to the lower pid; pid 0 never leads.
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
