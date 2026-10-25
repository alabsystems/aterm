// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fixtures::{
    Coal, Proc, Scene, USERSPACE_MS, app_work_1510, apple, calm_1510, exited_skewed_1510,
    finished_1032, idle_skewed_1510, lane_off_1032, liar_1032, reaped_1510, spotlight_1032,
    spotlight_late_1032, tab, tab2_build_1048, wrapped_1218,
};
use super::*;
use crate::model::{MemRates, MemSample, MemView, Pressure, Thermal, Throttle, Vitals};

/// The snapshot of `scene`'s second split tick, through a fresh model.
fn snap(scene: &Scene) -> Snapshot {
    let mut v = Vitals::new();
    let [base, sweep, now] = scene.samples();
    v.absorb(&base);
    v.absorb(&sweep);
    v.absorb(&now)
}

/// The snapshot of each split tick with a window, over `windows` windows of
/// `scene`'s work, through a model with `fault` (if any).
fn ticks(scene: &Scene, windows: u64, fault: Option<Fault>) -> Vec<Snapshot> {
    let mut v = match fault {
        Some(f) => Vitals::new().with_fault(f),
        None => Vitals::new(),
    };
    let snaps: Vec<Snapshot> = scene.series(windows).iter().map(|s| v.absorb(s)).collect();
    snaps.into_iter().skip(2).step_by(2).collect()
}

/// `scene` with process `pid`, its coalition and the busy figure `mc`
/// millicores busier.
fn busier(mut scene: Scene, pid: u32, mc: u64) -> Scene {
    scene.busy_mc += mc;
    let mut coalition = None;
    for p in scene.procs.iter_mut().filter(|p| p.pid == pid) {
        p.mc += mc;
        p.fast_mc += mc;
        coalition = Some(p.coalition);
    }
    for c in scene
        .coalitions
        .iter_mut()
        .filter(|c| Some(c.id) == coalition)
    {
        c.mc += mc;
    }
    scene
}

/// `scene` with the calm Mac's memory and thermal readings: pressure normal,
/// swap still, nominal, so what "nothing needed" rests on is read.
fn read_calm(scene: Scene) -> Scene {
    let calm = calm_1510();
    Scene {
        mem: calm.mem,
        thermal: calm.thermal,
        ..scene
    }
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

fn cause(s: &Snapshot, id: CauseId) -> Candidate {
    s.why
        .as_ref()
        .and_then(|f| f.causes.iter().find(|c| c.id == id).cloned())
        .unwrap_or_else(|| panic!("no {id:?} in {:?}", causes(s)))
}

const SPOTLIGHT: CauseId = CauseId::Activity(ActivityId::Spotlight);
const STARTUP: CauseId = CauseId::Context(Context::Startup);

/// Spotlight covers `mds` and `mds_stores`, 1420 of 3300, 722 ‰ deferred, and runs after start-up.
#[test]
fn spotlight_holds_on_its_vouched_members_after_start_up() {
    let s = snap(&spotlight_1032());
    assert_eq!(causes(&s), [SPOTLIGHT, STARTUP]);
    let c = cause(&s, SPOTLIGHT);
    assert_eq!(
        (c.covers_mc, c.coalitions.as_slice()),
        (1420, &[307, 491][..])
    );
    assert_eq!(
        c.evidence,
        Evidence::Activity {
            deferred_pm: Some(722),
            read_bps: 22_900_000
        }
    );
    assert_eq!(
        c.since_ms,
        Some(USERSPACE_MS),
        "by the evidence, not the tick"
    );
    assert_eq!(c.contexts, [Context::Startup]);
    assert_eq!(c.src(), ["signature", "kernel"]);
    let start = cause(&s, STARTUP);
    assert_eq!(
        start.evidence,
        Evidence::Startup {
            userspace_ms: USERSPACE_MS,
            wake: Some(WakeFact {
                at_ms: USERSPACE_MS - 5_000,
                reason: Some("RTP.multi-touch".into()),
                slept_s: Some(3715),
            }),
        },
        "the wake folds into start-up's evidence"
    );
    assert_eq!(start.covers_mc, 0);
}

/// The liar: `mds` and `mds_stores` under ad-hoc signatures with Apple's
/// signing ids.
#[test]
fn a_claimed_signing_id_never_names_an_activity() {
    let s = snap(&liar_1032());
    assert_eq!(causes(&s), [CauseId::Unexplained, STARTUP]);
    let c = cause(&s, CauseId::Unexplained);
    assert_eq!(c.covers_mc, 2170);
    assert_eq!(
        c.evidence,
        Evidence::Uncovered {
            part: Part::Services
        }
    );
    assert_eq!(c.src(), ["derived"]);

    let mut unread = spotlight_1032();
    unread.leads.clear();
    assert!(
        !causes(&snap(&unread)).contains(&SPOTLIGHT),
        "unknown is not Apple"
    );

    let mut own = spotlight_1032();
    own.busy_mc += 2000;
    own.coalitions.push(Coal {
        deferred_mc: 1500,
        ..Coal::new(1335, 2000)
    });
    own.procs.push(Proc::new(
        1160,
        1,
        501,
        1335,
        USERSPACE_MS + 160_000,
        "mds",
        2000,
    ));
    own.leads.push(LeadIdentity {
        voucher: Voucher::AdHoc,
        ..apple(1160, USERSPACE_MS + 160_000, "mds", "com.apple.mds")
    });
    let s = snap(&own);
    assert_eq!(causes(&s), [CauseId::AppWork(1335), SPOTLIGHT, STARTUP]);
    assert_eq!(cause(&s, SPOTLIGHT).coalitions, [307, 491]);
    let row = s
        .why
        .as_ref()
        .and_then(|f| f.split.rows.iter().find(|r| r.id == 1335).cloned());
    assert_eq!(
        row.and_then(|r| r.lead).map(|l| l.voucher),
        Some(Voucher::AdHoc)
    );
    assert_eq!(
        account(WhySubject::Cpu, Some(1), &s).headline,
        Some((CauseId::AppWork(1335), None))
    );
}

/// Tab 2's build 20 minutes after start-up, the boot daemons idle-busy:
/// tab 2's work is the cause and never "after start-up", though userspace
/// is young and every daemon was born with it; `mds` at 0.30 and 80 %
/// deferred holds Spotlight's terms but not the floor.
#[test]
fn a_tabs_work_is_its_cause_and_never_after_start_up() {
    let s = snap(&tab2_build_1048());
    assert_eq!(causes(&s), [CauseId::TabWork(2), CauseId::Unexplained]);
    let c = cause(&s, CauseId::TabWork(2));
    assert_eq!((c.covers_mc, c.contexts.as_slice()), (6000, &[][..]));
    let u = cause(&s, CauseId::Unexplained);
    assert_eq!(
        (u.covers_mc, u.contexts.as_slice(), u.coalitions.len()),
        (1700, &[][..], 5)
    );
    assert_eq!(
        u.evidence,
        Evidence::Uncovered {
            part: Part::Services
        }
    );
    assert!(!CauseId::TabWork(2).takes_start() && !CauseId::AtermWork.takes_start());
    let a = account(WhySubject::Cpu, Some(2), &s);
    assert_eq!(a.headline, Some((CauseId::TabWork(2), None)));
    assert!(!a.nothing_needed);
    assert_eq!(a.cannot.first(), Some(&("remedy", "unsupported")));
    assert!(!causes(&snap(&spotlight_1032())).contains(&CauseId::TabWork(1)));

    let tab1 = snap(&busier(tab2_build_1048(), 1210, 400));
    assert!(!causes(&tab1).contains(&CauseId::TabWork(1)), "600 of 8600");
    let mut mds = busier(tab2_build_1048(), 387, 300);
    for c in mds.coalitions.iter_mut().filter(|c| c.id == 307) {
        c.deferred_mc = 480;
    }
    assert!(!causes(&snap(&mds)).contains(&SPOTLIGHT), "600 of 8500");
}

/// A tab's work above the floor beside a larger load no cause names:
/// services' uncovered 3.79 cores are unexplained and the headline, never
/// the tab's 0.72, which is minor.
#[test]
fn a_small_cause_never_stands_for_a_large_uncovered_load() {
    let s = snap(&busier(busier(calm_1510(), 1210, 600), 430, 3600));
    assert_eq!(causes(&s), [CauseId::Unexplained, CauseId::TabWork(1)]);
    assert_eq!(cause(&s, CauseId::Unexplained).covers_mc, 3790);
    let a = account(WhySubject::Cpu, Some(1), &s);
    assert_eq!(a.headline, Some((CauseId::Unexplained, None)));
    assert_eq!(a.you.role, Role::Minor);
    assert!(!a.nothing_needed);

    let s = snap(&finished_1032());
    assert_eq!(causes(&s), [CauseId::Unexplained, SPOTLIGHT, STARTUP]);
    let u = cause(&s, CauseId::Unexplained);
    assert_eq!(
        (u.covers_mc, u.coalitions.as_slice(), u.contexts.as_slice()),
        (3400, &[][..], &[][..])
    );
    assert_eq!(u.evidence, Evidence::Uncovered { part: Part::Aterm });
    assert_eq!(
        account(WhySubject::Cpu, Some(1), &s).headline,
        Some((CauseId::Unexplained, None))
    );
}

/// aterm itself as the largest part above the floor.
#[test]
fn aterm_work_holds_when_aterm_itself_is_the_largest_part() {
    let mut scene = calm_1510();
    scene.busy_mc += 1420;
    for c in &mut scene.coalitions {
        if c.id == 1342 {
            c.mc += 1420;
        }
    }
    for p in &mut scene.procs {
        if p.pid == 1187 {
            *p = Proc::new(p.pid, p.ppid, p.uid, p.coalition, p.birth_ms, "aterm", 1500);
        }
    }
    let s = snap(&scene);
    assert_eq!(causes(&s), [CauseId::AtermWork]);
    assert_eq!(cause(&s, CauseId::AtermWork).covers_mc, 1500);
    assert!(!causes(&snap(&spotlight_1032())).contains(&CauseId::AtermWork));
    let under = snap(&busier(scene, 430, 1910));
    assert_eq!(causes(&under), [CauseId::Unexplained]);
}

/// The busiest coalition outside aterm's, led by the user's app: app-work,
/// since its lead's birth.
#[test]
fn app_work_is_the_busiest_coalition_outside_aterm_led_by_the_user() {
    let mut scene = app_work_1510();
    let s = snap(&scene);
    assert_eq!(causes(&s), [CauseId::AppWork(896)]);
    let c = cause(&s, CauseId::AppWork(896));
    assert_eq!(
        (c.covers_mc, c.since_ms),
        (1500, Some(USERSPACE_MS + 8_000))
    );
    assert_eq!(c.coalitions, [896]);
    assert!(
        causes(&snap(&spotlight_1032()))
            .iter()
            .all(|c| !matches!(c, CauseId::AppWork(_)))
    );
    scene.at_ms = Some(USERSPACE_MS + 600_000);
    let young = snap(&scene);
    assert_eq!(causes(&young), [CauseId::AppWork(896)]);
    assert_eq!(cause(&young, CauseId::AppWork(896)).contexts, []);
}

/// The calm Mac: 0.40 busy, Caffeine's 0.01 and every other coalition under
/// the floor, so nothing holds, not even unexplained (under a core).
#[test]
fn a_calm_mac_holds_no_cause() {
    let s = snap(&calm_1510());
    assert_eq!(causes(&s), []);
    assert_eq!(bare_subject(&s), Bare::Calm);
    let a = account(WhySubject::Cpu, Some(1), &s);
    assert_eq!((a.headline, a.nothing_needed), (None, true));
    assert_eq!(
        (a.you.role, a.you.calm, a.you.figure, a.you.share_pm),
        (Role::Minor, true, Some(120), Some(300)),
        "0.12 of 0.40 is a third, under the floor: minor on a calm machine"
    );
}

/// Work no coalition held past a core (coalition 2000 reaped inside the
/// calm Mac's window after 1.50 cores): `unexplained` covers the residual,
/// with no coalition and no context, so the reply never says nothing is
/// needed and a bare `sys why` names the CPU. NEGATIVE CONTROLS: 0.90 cores
/// of it holds nothing and nothing needs doing; and `unexplained` blind to
/// the residual (`Fault::IgnoreUnaccounted`) leaves 1.50 cores uncovered
/// beside "nothing needed", the defect the ruling closes.
#[test]
fn work_no_coalition_held_past_a_core_is_never_nothing_needed() {
    let s = snap(&reaped_1510());
    let f = s.why.as_deref().expect("a window");
    assert_eq!((f.split.global_mc, f.split.unaccounted_mc), (1900, 1500));
    assert_eq!(causes(&s), [CauseId::Unexplained]);
    let u = cause(&s, CauseId::Unexplained);
    assert_eq!(
        (u.covers_mc, u.coalitions.as_slice(), u.contexts.as_slice()),
        (1500, &[][..], &[][..])
    );
    assert_eq!(u.src(), ["derived"]);
    assert_eq!(u.evidence, Evidence::Unaccounted);
    let a = account(WhySubject::Cpu, Some(1), &s);
    assert_eq!(a.headline, Some((CauseId::Unexplained, None)));
    assert!(!a.nothing_needed);
    assert_eq!(a.cannot.first(), Some(&("remedy", "unsupported")));
    assert_eq!(bare_subject(&s), Bare::Subject(WhySubject::Cpu));

    let mut under = reaped_1510();
    under.busy_mc -= 600;
    for c in under.coalitions.iter_mut().filter(|c| c.id == 2000) {
        c.mc = 900;
    }
    let s = snap(&under);
    assert_eq!(s.why.as_deref().map(|f| f.split.unaccounted_mc), Some(900));
    assert_eq!(causes(&s), []);
    assert!(account(WhySubject::Cpu, Some(1), &s).nothing_needed);

    let blind = ticks(&reaped_1510(), 1, Some(Fault::IgnoreUnaccounted))
        .pop()
        .expect("a window");
    assert_eq!(causes(&blind), []);
    assert!(account(WhySubject::Cpu, Some(1), &blind).nothing_needed);
    assert_eq!(bare_subject(&blind), Bare::Calm);
}

/// `wake`: 2 minutes after a wake that restarted nothing, with a core of
/// Spotlight's work deferred (931 + 94 mc).
#[test]
fn wake_holds_after_a_wake_when_start_up_does_not() {
    let mut scene = wrapped_1218();
    let at = scene.at_ms.expect("a wall clock");
    if let Some(t) = scene.timeline.as_mut() {
        t.sleep_ms = Some(at - 3_600_000);
        t.wake_ms = Some(at - 120_000);
    }
    let s = snap(&scene);
    assert_eq!(causes(&s), [SPOTLIGHT, CauseId::Context(Context::Wake)]);
    assert_eq!(cause(&s, SPOTLIGHT).contexts, [Context::Wake]);
    let w = cause(&s, CauseId::Context(Context::Wake));
    assert_eq!(
        w.evidence,
        Evidence::Wake(WakeFact {
            at_ms: at - 120_000,
            reason: Some("RTP.multi-touch".into()),
            slept_s: Some(3480),
        })
    );
    assert_eq!(w.since_ms, None, "the wake's time is its evidence");
    let old = snap(&wrapped_1218());
    assert_eq!(causes(&old), [SPOTLIGHT], "neither start-up nor a wake");
    assert!(!causes(&snap(&spotlight_1032())).contains(&CauseId::Context(Context::Wake)));
    for c in scene.coalitions.iter_mut().filter(|c| c.id == 307) {
        c.deferred_mc = 880;
    }
    assert_eq!(causes(&snap(&scene)), [SPOTLIGHT]);
}

/// `startup` is weighed by CPU, never by count: Spotlight with
/// `corespotlightd` restarted after start-up's first minute covers three
/// coalitions, two of them start-up's, but only 1.42 of its 3.42 cores, so
/// it runs since the wake instead.
#[test]
fn start_up_is_weighed_by_cpu_never_by_count() {
    let s = snap(&spotlight_late_1032());
    assert_eq!(causes(&s), [SPOTLIGHT, CauseId::Context(Context::Wake)]);
    let c = cause(&s, SPOTLIGHT);
    assert_eq!(
        (c.covers_mc, c.coalitions.as_slice(), c.contexts.as_slice()),
        (3420, &[1330, 307, 491][..], &[Context::Wake][..])
    );
}

/// The wrapped pid space: pid 159, born at 12:16, changes neither when
/// userspace started nor aterm's coalition's name, and start-up does not
/// hold 110 minutes on.
#[test]
fn a_wrapped_pid_space_moves_neither_the_start_nor_a_name() {
    let s = snap(&wrapped_1218());
    let f = s.why.as_ref().expect("facts");
    assert_eq!(f.userspace_ms, Some(USERSPACE_MS));
    let aterm = f.split.rows.iter().find(|r| r.id == 1342).expect("aterm's");
    assert_eq!(
        aterm.lead.as_ref().map(|l| (l.pid, l.exe.as_deref())),
        Some((1187, Some("aterm")))
    );
    assert!(!causes(&s).contains(&STARTUP));
}

/// The machine's contexts hold for every covering cause, and alone: memory
/// pressure at warn, and a thermal throttle or Low Power Mode.
#[test]
fn pressure_and_throttle_are_contexts_of_the_machine() {
    let mut scene = spotlight_1032();
    scene.mem = Some(MemSample {
        total: 64 << 30,
        app: 40 << 30,
        pressure: Pressure::Warn,
        ..MemSample::default()
    });
    scene.thermal = Some(Thermal {
        level: Throttle::Light,
        ..Thermal::default()
    });
    let s = snap(&scene);
    assert_eq!(
        causes(&s),
        [
            SPOTLIGHT,
            STARTUP,
            CauseId::Context(Context::Throttled),
            CauseId::Context(Context::Pressure)
        ]
    );
    assert_eq!(
        cause(&s, SPOTLIGHT).contexts,
        [Context::Startup, Context::Throttled, Context::Pressure]
    );
    assert_eq!(
        cause(&s, CauseId::Context(Context::Pressure)).evidence,
        Evidence::Pressure {
            level: Pressure::Warn,
            swap_bps: None
        }
    );
    let mem = account(WhySubject::Mem, Some(1), &s);
    assert_eq!(
        mem.causes.iter().map(|c| c.cause.id).collect::<Vec<_>>(),
        [CauseId::Context(Context::Pressure)]
    );
    assert_eq!(
        mem.headline,
        Some((CauseId::Context(Context::Pressure), None))
    );
    assert_eq!(mem.cannot.first(), Some(&("remedy", "unsupported")));
    assert!(!account(WhySubject::Cpu, Some(1), &s).nothing_needed);
    assert_eq!(bare_subject(&s), Bare::Subject(WhySubject::Mem));

    scene.mem = None;
    scene.thermal = Some(Thermal {
        low_power: Some(true),
        ..Thermal::default()
    });
    assert!(causes(&snap(&scene)).contains(&CauseId::Context(Context::Throttled)));
    scene.thermal = Some(Thermal::default());
    assert_eq!(causes(&snap(&scene)), [SPOTLIGHT, STARTUP]);
}

/// With the lane off no coalition is read: the facts are by subtraction, no
/// covering cause can hold, and the reply says why, on macOS and on an OS
/// with no coalitions alike; the machine's contexts still hold, and the
/// first is the headline.
#[test]
fn with_the_lane_off_only_the_machines_contexts_hold() {
    for lane in [CoalLane::Off, CoalLane::Unsupported] {
        let s = snap(&read_calm(Scene {
            lane,
            ..lane_off_1032()
        }));
        assert_eq!(s.why.as_ref().map(|f| f.split.via), Some(Via::Subtraction));
        assert_eq!(causes(&s), []);
        let a = account(WhySubject::Cpu, Some(1), &s);
        assert_eq!(
            a.cannot,
            [("coalitions", "lane_off:coalitions")],
            "{lane:?}"
        );
        assert_eq!((a.headline, a.nothing_needed), (None, true));
    }
    let linux = read_calm(Scene {
        lane: CoalLane::Unsupported,
        ..lane_off_1032()
    });
    let edits: [fn(&mut Scene); 2] = [
        |s| {
            if let Some(m) = s.mem.as_mut() {
                m.pressure = Pressure::Unknown;
            }
        },
        |s| s.thermal = None,
    ];
    for edit in edits {
        let mut scene = linux.clone();
        edit(&mut scene);
        let s = snap(&scene);
        let a = account(WhySubject::Cpu, Some(1), &s);
        assert_eq!((a.headline, a.nothing_needed), (None, false));
        assert_eq!(a.cannot.first(), Some(&("remedy", "unsupported")));
    }
    let mut scene = lane_off_1032();
    scene.thermal = Some(Thermal {
        level: Throttle::Heavy,
        ..Thermal::default()
    });
    let s = snap(&scene);
    assert_eq!(causes(&s), [CauseId::Context(Context::Throttled)]);
    let a = account(WhySubject::Cpu, Some(1), &s);
    assert_eq!(
        a.headline,
        Some((CauseId::Context(Context::Throttled), None))
    );
    assert_eq!(
        a.cannot,
        [
            ("remedy", "unsupported"),
            ("coalitions", "lane_off:coalitions")
        ]
    );
}

/// With no split the machine's contexts still hold: Windows reads no
/// process's owner, so no split is ever made, by coalition or by
/// subtraction, and no facts are; memory pressure at warn and a light
/// throttle are the CPU's causes all the same, nothing is known needless,
/// and the lane is said.
#[test]
fn with_no_split_the_machines_contexts_still_hold() {
    let scene = Scene {
        lane: CoalLane::Unsupported,
        mem: Some(MemSample {
            total: 64 << 30,
            app: 40 << 30,
            pressure: Pressure::Warn,
            ..MemSample::default()
        }),
        thermal: Some(Thermal {
            level: Throttle::Light,
            ..Thermal::default()
        }),
        ..lane_off_1032()
    };
    let mut v = Vitals::new();
    let [_, _, s] = scene.samples().map(|mut sample| {
        sample.self_uid = None;
        v.absorb(&sample)
    });
    assert!(s.why.is_none() && s.split.is_none());
    let ids = |a: &Account| a.causes.iter().map(|c| c.cause.id).collect::<Vec<_>>();
    let throttled = CauseId::Context(Context::Throttled);
    let pressure = CauseId::Context(Context::Pressure);
    let a = account(WhySubject::Cpu, Some(1), &s);
    assert_eq!(ids(&a), [throttled, pressure]);
    assert_eq!(a.headline, Some((throttled, None)));
    assert!(!a.nothing_needed);
    assert_eq!(
        a.cannot,
        [
            ("remedy", "unsupported"),
            ("coalitions", "lane_off:coalitions")
        ]
    );
    assert_eq!(
        (a.parts, a.you.role),
        (None, Role::Unavailable("not_sampled"))
    );
    let m = account(WhySubject::Mem, Some(1), &s);
    assert_eq!(
        (ids(&m), m.headline),
        (vec![pressure], Some((pressure, None)))
    );
    assert!(!m.nothing_needed);
    assert_eq!(bare_subject(&s), Bare::Subject(WhySubject::Mem));

    for subject in [WhySubject::Cpu, WhySubject::Mem] {
        let none = account(subject, None, &Snapshot::default());
        assert_eq!(
            (none.causes.len(), none.headline, none.nothing_needed),
            (0, None, false),
            "{subject:?}"
        );
        assert_eq!(none.cannot.first(), Some(&("remedy", "unsupported")));
    }
    assert_eq!(
        account(WhySubject::Cpu, None, &Snapshot::default())
            .you
            .role,
        Role::Unavailable("warming")
    );
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

/// macOS 27.0.1's QoS buckets sum 0.7–6 % past a coalition's CPU time, so a
/// deferred share is of the buckets, never of the CPU time, and never past
/// 1000 ‰: Spotlight's 1025 mc deferred of 1420 mc is 722 ‰ with the
/// buckets equal to the time, 681 ‰ with them 6 % past it, and 498 ‰ (no
/// Spotlight) with them 640 mc past it, where a share of the time would
/// read 722 ‰ and open it.
#[test]
fn a_deferred_share_is_of_the_qos_buckets_and_never_past_one() {
    let over = |mds: u64, stores: u64| {
        let mut scene = spotlight_1032();
        for c in &mut scene.coalitions {
            match c.id {
                307 => c.qos_over_mc = mds,
                491 => c.qos_over_mc = stores,
                _ => {}
            }
        }
        snap(&scene)
    };
    let s = over(77, 8);
    assert_eq!(
        cause(&s, SPOTLIGHT).evidence,
        Evidence::Activity {
            deferred_pm: Some(681),
            read_bps: 22_900_000
        }
    );
    let mds = s
        .why
        .as_ref()
        .and_then(|f| f.split.rows.iter().find(|r| r.id == 307).cloned());
    assert_eq!(
        mds.map(|r| (r.qos_mc, r.deferred_pm())),
        Some((1367, Some(681)))
    );
    assert!(!causes(&over(640, 0)).contains(&SPOTLIGHT));
    assert_eq!(deferred_pm(1100, 1000), Some(1000));
    assert_eq!(deferred_pm(5, 0), None);
    assert_eq!(deferred_pm(-5, 100), Some(0));
}

/// An episode opens on its second consecutive split tick, `since` from its
/// evidence; a cold pull's first window prints its cause with no `opened=`.
#[test]
fn spotlight_opens_on_its_second_tick_and_the_clause_ten_seconds_on() {
    let snaps = ticks(&spotlight_1032(), 5, None);
    let opened = |s: &Snapshot| {
        account(WhySubject::Cpu, Some(1), s)
            .causes
            .iter()
            .find(|c| c.cause.id == SPOTLIGHT)
            .map(|c| c.opened_unix_ms)
    };
    assert_eq!(opened(&snaps[0]), Some(None), "cold: holds, not open");
    let second = snaps[1].at_unix_ms;
    for s in &snaps[1..] {
        assert_eq!(opened(s), Some(Some(second)));
    }
    let start = account(WhySubject::Cpu, Some(1), &snaps[1]);
    assert_eq!(
        start
            .causes
            .iter()
            .find(|c| c.cause.id == STARTUP)
            .map(|c| c.opened_unix_ms),
        Some(None),
        "a context says when by its evidence"
    );
    let open = &snaps[1].why.as_ref().expect("facts").open;
    assert_eq!(
        open.iter().map(|o| (o.id, o.since_ms)).collect::<Vec<_>>(),
        [(SPOTLIGHT, Some(USERSPACE_MS))],
        "a context keeps no episode"
    );
    let clause: Vec<Option<DeckCause>> = snaps.iter().map(deck_cause).collect();
    let shown = Some(DeckCause {
        activity: ActivityId::Spotlight,
        word: "Spotlight",
        context: Some(Context::Startup),
    });
    assert_eq!(clause, [None, None, None, shown, shown]);
    assert_eq!(bare_subject(&snaps[0]), Bare::Subject(WhySubject::Cpu));
}

fn held(id: CauseId, covers_mc: i64) -> Candidate {
    Candidate {
        id,
        covers_mc,
        coalitions: Vec::new(),
        since_ms: None,
        evidence: Evidence::Measured,
        contexts: Vec::new(),
    }
}

/// An open episode closes once its cause has failed for 30 s; a gap of more
/// than a minute between split ticks ends every episode.
#[test]
fn an_episode_closes_after_failing_for_thirty_seconds_or_a_gap() {
    let mut e = Episodes::default();
    let c = [held(CauseId::TabWork(2), 900)];
    assert_eq!(e.step(0, 0, &c, false), []);
    let open = e.step(5_000, 5, &c, false);
    assert_eq!((open[0].opened_unix_ms, open[0].holds), (5, true));
    for at in [10_000, 25_000, 39_999] {
        let open = e.step(at, 0, &[], false);
        assert_eq!(open.len(), 1, "open at {at}");
        assert!(!open[0].holds);
    }
    assert_eq!(
        e.step(40_000, 0, &[], false),
        [],
        "30 s after it first failed"
    );

    let mut e = Episodes::default();
    e.step(0, 0, &c, false);
    e.step(5_000, 0, &c, false);
    assert_eq!(
        e.step(66_000, 0, &c, false),
        [],
        "after a gap it starts cold"
    );
    assert_eq!(e.step(71_000, 7, &c, false)[0].opened_unix_ms, 7);
}

/// An open episode's `since` is the earliest its evidence said over the
/// episode: a later tick whose earliest lead is gone does not move it.
#[test]
fn an_episodes_since_is_the_earliest_over_it() {
    let at = |since_ms| Candidate {
        since_ms: Some(since_ms),
        ..held(SPOTLIGHT, 1420)
    };
    let mut e = Episodes::default();
    e.step(0, 0, &[at(USERSPACE_MS)], false);
    let open = e.step(5_000, 5, &[at(USERSPACE_MS + 150_000)], false);
    assert_eq!(open[0].since_ms, Some(USERSPACE_MS));
}

/// On a tick whose sweep stopped at its cap a tab's work neither opens nor
/// closes, however long the sweep stays cut; other causes run on.
#[test]
fn a_cut_sweep_neither_opens_nor_closes_a_tabs_work() {
    let both = [
        held(CauseId::TabWork(2), 900),
        held(CauseId::AppWork(1335), 700),
    ];
    let tab = CauseId::TabWork(2);
    let ids = |open: &[OpenCause]| open.iter().map(|o| o.id).collect::<Vec<_>>();
    let mut e = Episodes::default();
    e.step(0, 0, &both, false);
    let open = e.step(5_000, 0, &both, true);
    assert_eq!(ids(&open), [CauseId::AppWork(1335)], "not opened while cut");
    assert_eq!(ids(&e.step(10_000, 0, &both[..1], false)).len(), 2);
    for at in (15_000..=50_000).step_by(5_000) {
        let open = e.step(at, 0, &[], true);
        assert!(open.iter().any(|o| o.id == tab), "open at {at}, cut");
    }
    assert!(e.step(55_000, 0, &[], false).iter().any(|o| o.id == tab));
    assert_eq!(e.step(85_000, 0, &[], false), [], "30 s failing, whole");

    let mut v = Vitals::new();
    let mut series = tab2_build_1048().series(3);
    series[4].procs_truncated = true;
    let snaps: Vec<Snapshot> = series.iter().map(|s| v.absorb(s)).collect();
    let open = |k: usize| ids(&snaps[2 * k].why.as_ref().expect("facts").open);
    assert_eq!(open(2), [CauseId::Unexplained]);
    assert_eq!(open(3), [CauseId::TabWork(2), CauseId::Unexplained]);
}

/// Ten minutes with no split tick: the first after the gap only sets the base,
/// evaluates nothing and carries no open episode and no clause; Spotlight
/// then reopens on two evaluated ticks, and the clause shows 10 s after.
#[test]
fn after_a_gap_the_clause_waits_for_a_new_episode_and_its_hold() {
    let all = spotlight_1032().series(130);
    let kept = [&all[..=10], &all[251..]].concat();
    let mut v = Vitals::new();
    let snaps: Vec<Snapshot> = kept.iter().map(|s| v.absorb(s)).collect();
    let tick = |k: usize| &snaps[if k <= 5 { 2 * k } else { 2 * (k - 120) }];
    assert!(deck_cause(tick(5)).is_some());
    let base = tick(126).why.as_ref().expect("the last facts");
    assert_eq!((base.open.as_slice(), base.clause), (&[][..], None));
    assert_eq!(deck_cause(tick(126)), None);
    let opened = |k: usize| {
        account(WhySubject::Cpu, Some(1), tick(k))
            .causes
            .iter()
            .find(|c| c.cause.id == SPOTLIGHT)
            .map(|c| c.opened_unix_ms.is_some())
    };
    assert_eq!(opened(127), Some(false), "cold: holds, not open");
    assert_eq!(opened(128), Some(true));
    let clause: Vec<bool> = (127..=130).map(|k| deck_cause(tick(k)).is_some()).collect();
    assert_eq!(clause, [false, false, false, true]);
}

fn open_as(id: CauseId, covers_mc: i64, contexts: &[Context]) -> OpenCause {
    OpenCause {
        id,
        opened_unix_ms: 0,
        since_ms: None,
        holds: true,
        covers_mc,
        contexts: contexts.to_vec(),
    }
}

fn open(covers_mc: i64, contexts: &[Context]) -> OpenCause {
    open_as(SPOTLIGHT, covers_mc, contexts)
}

/// The 10:32 split's outside-aterm busy: 2900 mc.
fn outside() -> CoalSplit {
    CoalSplit {
        others_mc: 730,
        services_mc: 2170,
        ..CoalSplit::default()
    }
}

/// The clause holds 10 s before it shows and 30 s before it goes: held 8 s,
/// none; 10 s, the clause; a 29 s failure keeps it, 30 s drops it.
#[test]
fn the_clause_shows_after_ten_seconds_and_goes_after_thirty() {
    let mut h = ClauseHold::default();
    let o = [open(1420, &[Context::Startup])];
    let split = outside();
    assert_eq!(h.step(0, &o, &split), None);
    assert_eq!(h.step(8_000, &o, &split), None, "8 s");
    let shown = h.step(10_000, &o, &split).expect("10 s");
    assert_eq!(
        (shown.word, shown.context),
        ("Spotlight", Some(Context::Startup))
    );
    for at in [15_000, 25_000, 35_000, 43_999] {
        assert_eq!(h.step(at, &[], &split), Some(shown), "{at}");
    }
    assert_eq!(h.step(45_000, &[], &split), None, "30 s failing");

    let mut h = ClauseHold::default();
    for at in [0, 5_000, 10_000] {
        h.step(at, &o, &split);
    }
    let plain = [open(1420, &[])];
    assert_eq!(h.step(15_000, &plain, &split), Some(shown));
    assert_eq!(h.step(20_000, &plain, &split), Some(shown));
    let now = h.step(25_000, &plain, &split).expect("the new words");
    assert_eq!(now.context, None);

    let mut h = ClauseHold::default();
    for at in [0, 5_000, 10_000] {
        h.step(at, &o, &split);
    }
    assert_eq!(h.step(26_000, &o, &split), None);
}

/// The clause's floor: a core and 40 % of the busy CPU outside aterm.
#[test]
fn the_clause_needs_a_core_and_two_fifths_and_names_one_cause() {
    let shown = |o: &[OpenCause], split: &CoalSplit| {
        let mut h = ClauseHold::default();
        for at in [0, 5_000] {
            h.step(at, o, split);
        }
        h.step(10_000, o, split)
    };
    let split = outside();
    assert_eq!(shown(&[open(1131, &[])], &split), None, "39 %");
    assert!(shown(&[open(1160, &[])], &split).is_some(), "40 %");
    let small = CoalSplit {
        others_mc: 0,
        services_mc: 1000,
        ..CoalSplit::default()
    };
    assert_eq!(shown(&[open(900, &[])], &small), None, "0.9 core");
    let app = |covers_mc| open_as(CauseId::AppWork(1335), covers_mc, &[]);
    for two in [
        [open(1218, &[Context::Wake]), app(1305)],
        [app(1305), open(1218, &[Context::Wake])],
    ] {
        assert_eq!(shown(&two, &split), None, "an app's work is the largest");
    }
    for two in [
        [open(1305, &[Context::Wake]), app(1218)],
        [app(1218), open(1305, &[Context::Wake])],
    ] {
        assert_eq!(
            shown(&two, &split).map(|c| c.context),
            Some(Some(Context::Wake)),
            "Spotlight is the largest"
        );
    }
    let residual = |services_mc: i64, unaccounted_mc: i64| CoalSplit {
        others_mc: 730,
        services_mc,
        unaccounted_mc,
        ..CoalSplit::default()
    };
    assert!(
        shown(&[open(1100, &[])], &residual(1870, 0)).is_some(),
        "42 %"
    );
    assert_eq!(
        shown(&[open(1100, &[])], &residual(1870, 300)),
        None,
        "38 %"
    );
    assert_eq!(
        shown(&[open(1100, &[])], &residual(2170, -300)),
        None,
        "38 %, the negative residual left out"
    );
    let mut failing = open(1420, &[]);
    failing.holds = false;
    assert_eq!(
        shown(&[failing], &split),
        None,
        "an open episode that fails"
    );
}

/// The clause rides the facts while they are at most 15 s old.
#[test]
fn a_stale_clause_is_not_shown() {
    let snaps = ticks(&spotlight_1032(), 4, None);
    let mut s = snaps[3].clone();
    assert!(deck_cause(&s).is_some());
    s.at_ns += 15_000_000_000;
    assert!(deck_cause(&s).is_some(), "15 s");
    s.at_ns += 1_000_000;
    assert_eq!(deck_cause(&s), None, "older than 15 s");
}

/// Every role by its share, its rank and the floor.
#[test]
fn a_tabs_role_is_by_its_share_its_rank_and_the_floor() {
    assert_eq!(you_role(0, 3300, false, false), (Role::None, false));
    assert_eq!(you_role(200, 3300, false, false), (Role::Minor, false));
    assert_eq!(you_role(120, 400, false, false), (Role::Minor, true));
    assert_eq!(you_role(700, 3300, false, true), (Role::Part, false));
    assert_eq!(
        you_role(1500, 3300, true, true),
        (Role::Part, false),
        "the largest, under half"
    );
    assert_eq!(
        you_role(2000, 3300, false, true),
        (Role::Part, false),
        "over half, not the largest"
    );
    assert_eq!(you_role(2000, 3300, true, true), (Role::Main, false));
    assert_eq!(
        you_role(600, 3300, false, true),
        (Role::Minor, false),
        "under a fifth"
    );
}

/// The `you` line on each scene: minor at 10:32 (6.1 %, no basis), main for
/// tab 2's build, none for a tab with no work, `unavailable(truncated)` on
/// a cut sweep (never none), and `unavailable(not_sampled)` where aterm's
/// finished work could make the tab the cause: 0.20 live is minor, 3.20 with
/// the 3.00 no live process holds is main.
#[test]
fn the_you_role_needs_a_whole_sweep_and_its_bound() {
    let you = |scene: &Scene, tab: u64| account(WhySubject::Cpu, Some(tab), &snap(scene));
    let a = you(&read_calm(spotlight_1032()), 1);
    assert_eq!(
        (a.you.role, a.you.calm, a.you.figure, a.you.share_pm),
        (Role::Minor, false, Some(200), Some(61))
    );
    assert_eq!(
        a.parts
            .map(|p| (p.tab, p.aterm_rest, p.others, p.services, p.unaccounted)),
        Some((200, 200, 730, 2170, 0))
    );
    assert_eq!(a.headline, Some((SPOTLIGHT, Some(Context::Startup))));
    assert!(a.nothing_needed);
    assert_eq!(a.you.unattributed_mc, None);
    assert_eq!(
        a.cannot,
        [
            ("index_progress", "unsupported"),
            ("index_folder", "needs_privilege:root")
        ]
    );
    let b = you(&tab2_build_1048(), 2);
    assert_eq!(
        (b.you.role, b.you.figure, b.you.share_pm),
        (Role::Main, Some(6000), Some(732))
    );
    assert_eq!(you(&wrapped_1218(), 2).you.role, Role::None);

    let mut v = Vitals::new();
    let [base, sweep, mut now] = spotlight_1032().samples();
    now.procs_truncated = true;
    v.absorb(&base);
    v.absorb(&sweep);
    let s = v.absorb(&now);
    let t = account(WhySubject::Cpu, Some(1), &s);
    assert_eq!(
        (t.you.role, t.you.figure),
        (Role::Unavailable("truncated"), None)
    );
    assert_eq!(
        account(WhySubject::Mem, Some(1), &s).you.role,
        Role::Unavailable("truncated")
    );
    let mut series = spotlight_1032().series(2);
    series[2].procs_truncated = true;
    let s = last(&series[..4]);
    assert!(!s.procs_truncated);
    assert_eq!(
        account(WhySubject::Cpu, Some(1), &s).you.role,
        Role::Unavailable("truncated")
    );

    let mut unread = snap(&spotlight_1032());
    unread.aterm = None;
    for subject in [WhySubject::Cpu, WhySubject::Mem] {
        assert_eq!(
            account(subject, Some(1), &unread).you.role,
            Role::Unavailable("truncated"),
            "{subject:?} with aterm unread"
        );
    }

    let f = you(&finished_1032(), 1);
    assert_eq!(
        (f.you.role, f.you.figure, f.you.unattributed_mc),
        (Role::Unavailable("not_sampled"), Some(200), Some(3000))
    );
    assert!(f.cannot.contains(&("finished_tab_work", "not_sampled")));
    assert!(!a.cannot.contains(&("finished_tab_work", "not_sampled")));
    let mut build = tab2_build_1048();
    build.busy_mc += 1000;
    for c in build.coalitions.iter_mut().filter(|c| c.id == 1342) {
        c.mc += 1000;
        c.reaped = 10;
    }
    let g = you(&build, 2);
    assert_eq!(
        (g.you.role, g.you.figure, g.you.unattributed_mc),
        (Role::Main, Some(6000), Some(1000))
    );
    assert!(!g.cannot.contains(&("finished_tab_work", "not_sampled")));
}

/// An idle tab beside aterm's coalition and its live work 0.30 mc apart
/// (aterm's coalition 200.50 mc, aterm itself 200.20, a member made and
/// reaped inside the window, so the gap may be finished work): rounded on
/// their own they read a millicore apart, rounded once from their
/// nanoseconds none, so the role stays `none` and no `finished_tab_work` is
/// claimed. NEGATIVE CONTROL: 0.80 mc apart is a millicore no live process
/// holds, and the bound rule reads `not_sampled`.
#[test]
fn the_parts_rounding_never_reads_as_unattributed_work() {
    let idle = |coalition_ns: u64| {
        let mut scene = calm_1510();
        for c in scene.coalitions.iter_mut().filter(|c| c.id == 1342) {
            c.mc = 200;
        }
        for p in scene.procs.iter_mut() {
            match p.pid {
                1187 => *p = Proc::new(p.pid, p.ppid, p.uid, p.coalition, p.birth_ms, "aterm", 200),
                1210 => *p = Proc::new(p.pid, p.ppid, p.uid, p.coalition, p.birth_ms, "claude", 0),
                _ => {}
            }
        }
        let mut v = Vitals::new();
        let [base, sweep, mut now] = scene.samples();
        for r in now.coalitions.iter_mut().flat_map(|c| c.rows.iter_mut()) {
            if r.id == 1342 {
                r.cpu_ns += coalition_ns;
                r.tasks_started += 1;
                r.tasks_exited += 1;
            }
        }
        for p in now.procs.iter_mut().flatten().filter(|p| p.pid == 1187) {
            p.cpu_ns = p.cpu_ns.map(|ns| ns + 1_000_000);
        }
        v.absorb(&base);
        v.absorb(&sweep);
        let s = v.absorb(&now);
        let f = s.why.as_deref().expect("a window");
        assert_eq!(
            (f.split.aterm_mc - f.split.own_mc, f.split.tabs.as_slice()),
            (1, &[(1, 0)][..]),
            "{coalition_ns} ns: the parts rounded on their own"
        );
        (
            f.split.unattributed_mc,
            account(WhySubject::Cpu, Some(1), &s),
        )
    };
    let (unattributed, a) = idle(2_500_000);
    assert_eq!(unattributed, 0);
    assert_eq!((a.you.role, a.you.unattributed_mc), (Role::None, None));
    assert!(!a.cannot.contains(&("finished_tab_work", "not_sampled")));
    let (unattributed, a) = idle(5_000_000);
    assert_eq!(unattributed, 1);
    assert_eq!(
        (a.you.role, a.you.unattributed_mc),
        (Role::Unavailable("not_sampled"), Some(1))
    );
    assert!(a.cannot.contains(&("finished_tab_work", "not_sampled")));
}

/// An idle tab over a window in which aterm's coalition's counter moved
/// 3.26 ms past its members' times (`idle_skewed_1510`, the headless run's
/// flip): its task counters show no member made or reaped and every member
/// is swept and in a live figure, so none of it is finished work; it reads
/// as the counters' skew and the role stays `none`. NEGATIVE CONTROLS: the
/// same window with a member made and reaped, with a member in no live
/// figure (a process of aterm's coalition reparented to launchd), with a
/// member whose coalition went unread (the sweep found fewer than the
/// counters hold), with a member whose CPU was not read at both ends (a
/// root `sudo` in tab 1 at 500 mc), or with one whose user went unread at
/// the window's end (tab 1's `claude` at 300 mc) keeps what no tab's figure
/// holds as work, and the role `not_sampled`; so does a `git` that exited
/// after 3 mc (`exited_skewed_1510`).
#[test]
fn the_counters_skew_never_reads_as_finished_work() {
    let read = |scene: &Scene, tweak: fn(&mut Sample)| {
        let mut v = Vitals::new();
        let [base, sweep, mut now] = scene.samples();
        tweak(&mut now);
        v.absorb(&base);
        v.absorb(&sweep);
        let s = v.absorb(&now);
        let split = s.why.as_deref().expect("a window").split.clone();
        (split, account(WhySubject::Cpu, Some(1), &s))
    };
    let (split, a) = read(&idle_skewed_1510(), |_| {});
    assert_eq!(
        (
            split.aterm_mc,
            split.own_mc,
            split.tabs.as_slice(),
            split.skew_mc,
            split.unattributed_mc
        ),
        (201, 200, &[(1, 0)][..], 1, 0)
    );
    assert_eq!((a.you.role, a.you.unattributed_mc), (Role::None, None));
    assert!(!a.cannot.contains(&("finished_tab_work", "not_sampled")));

    let (split, a) = read(&idle_skewed_1510(), |now| {
        for r in now.coalitions.iter_mut().flat_map(|c| c.rows.iter_mut()) {
            if r.id == 1342 {
                r.tasks_started += 1;
                r.tasks_exited += 1;
            }
        }
    });
    assert_eq!(
        (split.skew_mc, split.unattributed_mc),
        (0, 1),
        "a member reaped"
    );
    assert_eq!(
        (a.you.role, a.you.unattributed_mc),
        (Role::Unavailable("not_sampled"), Some(1))
    );
    let mut stray = idle_skewed_1510();
    stray.procs.push(Proc::new(
        1230,
        1,
        501,
        1342,
        USERSPACE_MS + 400_000,
        "ssh-agent",
        0,
    ));
    for c in stray.coalitions.iter_mut().filter(|c| c.id == 1342) {
        c.started += 1;
    }
    let (split, a) = read(&stray, |_| {});
    assert_eq!(
        (split.skew_mc, split.unattributed_mc),
        (0, 1),
        "a member in no live figure"
    );
    assert_eq!(a.you.role, Role::Unavailable("not_sampled"));
    let (split, a) = read(&idle_skewed_1510(), |now| {
        for p in now.procs.iter_mut().flatten().filter(|p| p.pid == 1210) {
            p.coalition = None;
        }
    });
    assert_eq!(
        (split.skew_mc, split.unattributed_mc),
        (0, 1),
        "a member whose coalition went unread"
    );
    assert_eq!(a.you.role, Role::Unavailable("not_sampled"));
    let mut root = idle_skewed_1510();
    root.busy_mc += 500;
    root.procs.push(Proc::new(
        1240,
        1200,
        0,
        1342,
        USERSPACE_MS + 300_000,
        "sudo",
        500,
    ));
    for c in root.coalitions.iter_mut().filter(|c| c.id == 1342) {
        c.mc += 500;
        c.started += 1;
    }
    let (split, a) = read(&root, |_| {});
    assert_eq!(
        (split.skew_mc, split.unattributed_mc),
        (0, 501),
        "a member whose CPU was not read at both ends"
    );
    assert_eq!(
        (a.you.role, a.you.unattributed_mc),
        (Role::Unavailable("not_sampled"), Some(501))
    );
    let mut busy = idle_skewed_1510();
    busy.busy_mc += 300;
    for p in busy.procs.iter_mut().filter(|p| p.pid == 1210) {
        *p = Proc::new(p.pid, p.ppid, p.uid, p.coalition, p.birth_ms, "claude", 300);
    }
    for c in busy.coalitions.iter_mut().filter(|c| c.id == 1342) {
        c.mc += 300;
    }
    let (split, a) = read(&busy, |now| {
        for p in now.procs.iter_mut().flatten().filter(|p| p.pid == 1210) {
            p.uid = None;
        }
    });
    assert_eq!(
        (split.skew_mc, split.unattributed_mc),
        (0, 301),
        "a member whose user went unread at the window's end"
    );
    assert_eq!(
        (a.you.role, a.you.unattributed_mc),
        (Role::Unavailable("not_sampled"), Some(301))
    );

    let (split, a) = read(&exited_skewed_1510(), |_| {});
    assert_eq!(
        (split.aterm_mc, split.skew_mc, split.unattributed_mc),
        (204, 0, 4),
        "the git's 15 ms and the counters' 3.26 ms, with a member reaped"
    );
    assert_eq!(
        (a.you.role, a.you.figure, a.you.unattributed_mc),
        (Role::Unavailable("not_sampled"), Some(0), Some(4))
    );
    assert!(a.cannot.contains(&("finished_tab_work", "not_sampled")));
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
        (Role::Minor, Some(32 << 20), Some(2))
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

/// The bare `sys why`: memory under pressure or swapping first; then the CPU
/// throttled (Low Power Mode too) or with a core-sized cause, held or open;
/// calm under a quarter of the cores with no cause and every deviation read;
/// else the larger share of its capacity.
#[test]
fn a_bare_why_answers_for_what_deviates_most() {
    assert_eq!(bare_subject(&snap(&calm_1510())), Bare::Calm);
    assert_eq!(
        bare_subject(&Snapshot::default()),
        Bare::Subject(WhySubject::Cpu),
        "unread is never calm"
    );
    let unread = |edit: fn(&mut Scene)| {
        let mut scene = calm_1510();
        edit(&mut scene);
        bare_subject(&snap(&scene))
    };
    assert_eq!(unread(|s| s.mem = None), Bare::Subject(WhySubject::Cpu));
    let edits: [fn(&mut Scene); 3] = [
        |s| {
            if let Some(m) = s.mem.as_mut() {
                m.pressure = Pressure::Unknown;
            }
        },
        |s| {
            if let Some(m) = s.mem.as_mut() {
                m.counters = None;
            }
        },
        |s| s.thermal = None,
    ];
    for edit in edits {
        assert_eq!(unread(edit), Bare::Subject(WhySubject::Mem));
    }
    assert_eq!(
        bare_subject(&snap(&spotlight_1032())),
        Bare::Subject(WhySubject::Cpu)
    );
    let mut hot = calm_1510();
    hot.thermal = Some(Thermal {
        level: Throttle::Light,
        ..Thermal::default()
    });
    assert_eq!(bare_subject(&snap(&hot)), Bare::Subject(WhySubject::Cpu));
    let mut small = calm_1510();
    small.cores = 1;
    let used = |app: u64| MemSample {
        total: 100 << 30,
        app: app << 30,
        pressure: Pressure::Normal,
        ..MemSample::default()
    };
    small.mem = Some(used(60));
    assert_eq!(bare_subject(&snap(&small)), Bare::Subject(WhySubject::Mem));
    small.mem = Some(used(10));
    assert_eq!(bare_subject(&snap(&small)), Bare::Subject(WhySubject::Cpu));

    let mut swapping = snap(&calm_1510());
    swapping.mem = Some(MemView {
        sample: used(10),
        rates: Some(MemRates {
            swapout_bps: 2_000_000.0,
            ..MemRates::default()
        }),
        ..MemView::default()
    });
    assert_eq!(bare_subject(&swapping), Bare::Subject(WhySubject::Mem));
    let mut lpm = calm_1510();
    lpm.thermal = Some(Thermal {
        low_power: Some(true),
        ..Thermal::default()
    });
    assert_eq!(bare_subject(&snap(&lpm)), Bare::Subject(WhySubject::Cpu));
    let held = snap(&busier(calm_1510(), 1210, 600));
    assert_eq!(causes(&held), [CauseId::TabWork(1)]);
    assert_eq!(bare_subject(&held), Bare::Subject(WhySubject::Mem));

    let mut scene = spotlight_1032();
    let calm = calm_1510();
    (scene.mem, scene.thermal) = (calm.mem, calm.thermal);
    let mut after = ticks(&scene, 3, None).pop().expect("a split tick");
    let f = Arc::make_mut(after.why.as_mut().expect("facts"));
    assert_eq!(f.open.len(), 1);
    f.causes.clear();
    for o in &mut f.open {
        o.holds = false;
    }
    assert_eq!(
        bare_subject(&after),
        Bare::Subject(WhySubject::Cpu),
        "open, failing"
    );
    Arc::make_mut(after.why.as_mut().expect("facts"))
        .open
        .clear();
    assert_eq!(bare_subject(&after), Bare::Calm, "closed");
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

/// The machine's contexts are the snapshot's on every reading, never the
/// last split tick's: a throttle that starts after the tick holds for the
/// CPU's causes at once, as the bare `sys why` that answers for the CPU by
/// it reads it, so the reply never says nothing is needed beside it; one
/// that ended since the tick holds no more.
#[test]
fn the_machines_contexts_are_the_snapshots_not_the_windows() {
    let light = Some(Thermal {
        level: Throttle::Light,
        ..Thermal::default()
    });
    let throttled = CauseId::Context(Context::Throttled);
    let contexts = |a: &Account| {
        a.causes
            .iter()
            .find(|c| c.cause.id == SPOTLIGHT)
            .map(|c| c.cause.contexts.clone())
    };
    let mut series = read_calm(spotlight_1032()).series(2);
    series[3].thermal = light;
    let s = last(&series[..4]);
    assert_eq!(bare_subject(&s), Bare::Subject(WhySubject::Cpu));
    let a = account(WhySubject::Cpu, Some(1), &s);
    assert!(a.causes.iter().any(|c| c.cause.id == throttled));
    assert_eq!(
        contexts(&a),
        Some(vec![Context::Startup, Context::Throttled])
    );
    assert!(!a.nothing_needed);

    let mut series = read_calm(spotlight_1032()).series(2);
    series[2].thermal = light;
    let a = account(WhySubject::Cpu, Some(1), &last(&series[..4]));
    assert!(a.causes.iter().all(|c| c.cause.id != throttled));
    assert_eq!(contexts(&a), Some(vec![Context::Startup]));
    assert!(a.nothing_needed);
}

/// With the lane off the facts are each whole reading's split by
/// subtraction: a whole reading that makes none carries none, so the split
/// and the tab's share read `not_sampled`.
#[test]
fn with_the_lane_off_a_reading_with_no_split_carries_no_facts() {
    let mut series = read_calm(Scene {
        lane: CoalLane::Unsupported,
        ..lane_off_1032()
    })
    .series(2);
    for p in series[3]
        .procs
        .iter_mut()
        .flatten()
        .filter(|p| p.pid == 387)
    {
        p.uid = None;
    }
    assert!(last(&series[..3]).why.is_some());
    let s = last(&series[..4]);
    assert!(s.split.is_none() && s.why.is_none());
    let a = account(WhySubject::Cpu, Some(1), &s);
    assert_eq!(
        (a.parts, a.you.role),
        (None, Role::Unavailable("not_sampled"))
    );
    assert_eq!((bare_subject(&s), a.nothing_needed), (Bare::Calm, true));
}

/// A tab the readings never read has no figure yet: on the CPU one opened
/// after the last split tick, in memory one no sweep holds.
#[test]
fn a_tab_no_reading_read_is_unread_never_none() {
    let mut series = read_calm(spotlight_1032()).series(2);
    series[3].tabs.push(tab(2, 1300));
    let s = last(&series[..4]);
    let a = account(WhySubject::Cpu, Some(2), &s);
    assert_eq!(
        (a.parts, a.you.role, a.you.figure, a.you.share_pm),
        (None, Role::Unavailable("warming"), None, None)
    );
    assert_eq!(account(WhySubject::Cpu, Some(1), &s).you.role, Role::Minor);
    let m = account(WhySubject::Mem, Some(3), &s);
    assert_eq!(
        (m.you.role, m.you.figure, m.you.share_pm),
        (Role::Unavailable("warming"), None, None)
    );
    series[3].procs_truncated = true;
    let s = last(&series[..4]);
    let a = account(WhySubject::Cpu, Some(2), &s);
    assert_eq!(
        (a.parts, a.you.role),
        (None, Role::Unavailable("truncated"))
    );
    assert_eq!(
        account(WhySubject::Mem, Some(2), &s).you.role,
        Role::Unavailable("truncated")
    );
}

/// A misread turns the lane off for good, and with it every episode and the
/// clause.
#[test]
fn a_misread_ends_the_episodes_and_the_clause() {
    let mut series = read_calm(spotlight_1032()).series(5);
    for r in series[10]
        .coalitions
        .iter_mut()
        .flat_map(|c| &mut c.rows)
        .filter(|r| r.id == 307)
    {
        r.footprint = Some(1 << 41);
    }
    assert!(deck_cause(&last(&series[..9])).is_some());
    let s = last(&series);
    assert_eq!(s.coal_lane, CoalLane::Off);
    let f = s.why.as_ref().expect("facts by subtraction");
    assert_eq!(f.split.via, Via::Subtraction);
    assert_eq!((f.open.as_slice(), f.clause), (&[][..], None));
    assert_eq!(deck_cause(&s), None);
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
        let _ = name(&mut names, &read, &procs, 1, 501, &[], ids);
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
