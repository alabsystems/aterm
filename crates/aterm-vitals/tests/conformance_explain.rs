// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `ExplainHonesty` (`aterm_models_misc::explain_honesty_model`): each scene of the
//! cause engine's tests (`aterm_vitals::explain::fixtures`) is read through the real
//! `Vitals::absorb` (the split, `detect`, the episodes and the clause) and `explain::account`, and
//! projected onto the model.

use std::collections::BTreeMap;

use aterm_models_misc::explain_honesty_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_vitals::explain::fixtures::{
    Scene, USERSPACE_MS, calm_1510, finished_1032, lane_off_1032, liar_1032, reaped_1510,
    spotlight_1032, spotlight_late_1032, tab2_build_1048, wrapped_1218,
};
use aterm_vitals::explain::{
    ActivityId, CauseId, ClauseHold, CoalSplit, Context, Fault, OpenCause, Role, WhySubject,
    account, deck_cause,
};
use aterm_vitals::{Sample, Snapshot, Vitals};

type S = BTreeMap<&'static str, i64>;

/// A scene's facts in the model's terms (`explain_honesty_model`'s doc).
#[derive(Clone, Copy, Debug)]
struct Truth {
    claims: i64,
    vouched: i64,
    terms: i64,
    above: i64,
    tab: i64,
    young: i64,
    bycpu: i64,
    bycount: i64,
    swept: i64,
    live: i64,
    bound: i64,
    /// The birth of the lowest pid above 1, unix ms.
    lowpid_ms: i64,
    /// The earliest birth of every other process above pid 1.
    other_ms: i64,
    lost: i64,
    settled: i64,
}

/// The 10:32 moment's facts: Spotlight's two coalitions are Apple's, hold
/// their terms (1420 mc, 722 ‰ deferred) over the floor, and are led by
/// processes born in start-up's first second; tab 1's 0.20 is under the
/// floor and minor whatever finished; `logd` (357) and `mds` (387) were
/// born together.
const T1032: Truth = Truth {
    claims: 1,
    vouched: 1,
    terms: 1,
    above: 1,
    tab: 1,
    young: 1,
    bycpu: 1,
    bycount: 1,
    swept: 1,
    live: 1,
    bound: 1,
    lowpid_ms: USERSPACE_MS,
    other_ms: USERSPACE_MS,
    lost: 0,
    settled: 0,
};

/// The calm Mac's facts: nothing holds its terms or the floor, userspace is
/// 4 h 41 min old, WindowServer was born half a second after `logd` and
/// `mds`, and memory and the thermal state read settled.
const T1510: Truth = Truth {
    terms: 0,
    above: 0,
    young: 0,
    other_ms: USERSPACE_MS + 500,
    settled: 1,
    ..T1032
};

/// Tab 2's build with `mds` at 0.60, 80 % deferred: over 500 mc, under a
/// tenth of the 8.50 busy.
fn tab2_build_mds_1048() -> Scene {
    let mut scene = tab2_build_1048();
    scene.busy_mc += 300;
    for c in scene.coalitions.iter_mut().filter(|c| c.id == 307) {
        c.mc += 300;
        c.deferred_mc = 480;
    }
    for p in scene.procs.iter_mut().filter(|p| p.pid == 387) {
        p.mc += 300;
        p.fast_mc += 300;
    }
    scene
}

/// The 10:32 moment with `mds`'s QoS buckets 640 mc past its CPU time:
/// Spotlight's members cover 1420 mc, over the floor, but run 498 ‰
/// deferred, under its terms' 500.
fn terms_failing_1032() -> Scene {
    let mut scene = spotlight_1032();
    for c in scene.coalitions.iter_mut().filter(|c| c.id == 307) {
        c.qos_over_mc = 640;
    }
    scene
}

/// `scene` with the calm Mac's memory and thermal readings.
fn read_calm(scene: Scene) -> Scene {
    let calm = calm_1510();
    Scene {
        mem: calm.mem,
        thermal: calm.thermal,
        ..scene
    }
}

/// The scenes: name, the readings, the caller's tab, the facts.
fn cases() -> Vec<(&'static str, Vec<Sample>, u64, Truth)> {
    let window = |scene: Scene| scene.samples().to_vec();
    let mut cut = spotlight_1032().samples();
    cut[2].procs_truncated = true;
    vec![
        ("10:32", window(spotlight_1032()), 1, T1032),
        (
            "10:32 with memory and the thermal state read",
            window(read_calm(spotlight_1032())),
            1,
            Truth {
                settled: 1,
                ..T1032
            },
        ),
        (
            "the liar",
            window(liar_1032()),
            1,
            Truth {
                vouched: 0,
                ..T1032
            },
        ),
        (
            "tab 2's build 20 minutes after start-up",
            window(tab2_build_1048()),
            2,
            Truth {
                above: 0,
                tab: 2,
                live: 3,
                bound: 3,
                ..T1032
            },
        ),
        (
            "Spotlight's terms failing over the floor",
            window(terms_failing_1032()),
            1,
            Truth { terms: 0, ..T1032 },
        ),
        (
            "Spotlight under a tenth of a build",
            window(tab2_build_mds_1048()),
            2,
            Truth {
                above: 0,
                tab: 2,
                live: 3,
                bound: 3,
                ..T1032
            },
        ),
        (
            "Spotlight with corespotlightd restarted",
            window(spotlight_late_1032()),
            1,
            Truth { bycpu: 0, ..T1032 },
        ),
        (
            "the wrapped pid space",
            window(wrapped_1218()),
            1,
            Truth {
                young: 0,
                lowpid_ms: USERSPACE_MS + 6_449_000,
                ..T1032
            },
        ),
        ("the calm Mac", window(calm_1510()), 1, T1510),
        (
            "a coalition reaped after 1.50 cores on the calm Mac",
            window(reaped_1510()),
            1,
            Truth { lost: 1, ..T1510 },
        ),
        (
            "a build that finished between sweeps",
            window(finished_1032()),
            1,
            Truth { bound: 3, ..T1032 },
        ),
        (
            "a sweep cut at its cap",
            cut.to_vec(),
            1,
            Truth { swept: 0, ..T1032 },
        ),
        (
            "the lane off",
            window(lane_off_1032()),
            1,
            Truth { claims: 0, ..T1032 },
        ),
    ]
}

/// The last snapshot of `samples`, read through a model with `fault`.
fn read(samples: &[Sample], fault: Option<Fault>) -> Snapshot {
    let mut v = match fault {
        Some(f) => Vitals::new().with_fault(f),
        None => Vitals::new(),
    };
    let mut last = None;
    for s in samples {
        last = Some(v.absorb(s));
    }
    last.expect("readings")
}

fn role(r: Role) -> i64 {
    match r {
        Role::None => 0,
        Role::Minor => 1,
        Role::Part => 2,
        Role::Main => 3,
        Role::Unavailable("truncated") => 4,
        Role::Unavailable("not_sampled") => 5,
        Role::Unavailable(_) => 6,
    }
}

/// A birth's rank against the truth's two: 0 the first, 1 the later, 2
/// not read, 3 neither.
fn rank(at: Option<i64>, t: Truth) -> i64 {
    let (first, later) = (t.lowpid_ms.min(t.other_ms), t.lowpid_ms.max(t.other_ms));
    match at {
        None => 2,
        Some(x) if x == first => 0,
        Some(x) if x == later => 1,
        Some(_) => 3,
    }
}

/// The states before `Detect`, after it, and after `Attach`.
fn project(m: &Model, t: Truth, snap: &Snapshot, tab: u64) -> [S; 3] {
    let facts = snap.why.as_ref().expect("a window's facts");
    let held = |id: CauseId| facts.causes.iter().find(|c| c.id == id);
    let flag = |b: bool| i64::from(b);
    let spotlight = held(CauseId::Activity(ActivityId::Spotlight));
    let tab_work = held(CauseId::TabWork(tab));
    let starts = |c: Option<&aterm_vitals::explain::Candidate>| {
        flag(c.is_some_and(|c| c.contexts.contains(&Context::Startup)))
    };
    let mut pre = m.init_state();
    pre.extend([
        ("claims", t.claims),
        ("vouched", t.vouched),
        ("terms", t.terms),
        ("above", t.above),
        ("tab", t.tab),
        ("young", t.young),
        ("bycpu", t.bycpu),
        ("bycount", t.bycount),
        ("swept", t.swept),
        ("live", t.live),
        ("bound", t.bound),
        ("lowpid", rank(Some(t.lowpid_ms), t)),
        ("other", rank(Some(t.other_ms), t)),
        ("lost", t.lost),
        ("settled", t.settled),
        ("fresh", 0),
    ]);
    let cpu = account(WhySubject::Cpu, Some(tab), snap);
    let mut mid = pre.clone();
    mid.extend([
        ("shown", flag(spotlight.is_some())),
        ("tabshown", flag(tab_work.is_some())),
        ("role", role(cpu.you.role)),
        ("none", flag(cpu.nothing_needed)),
        ("start", rank(facts.userspace_ms, t)),
        ("fresh", 1),
    ]);
    let mut post = mid.clone();
    post.extend([
        ("actstart", starts(spotlight)),
        ("tabstart", starts(tab_work)),
        ("fresh", 2),
    ]);
    [pre, mid, post]
}

/// Whether `m` takes `states` through `steps`; the laws a state on the way
/// breaks.
fn replay(m: &Model, states: &[S], steps: &[&str]) -> Result<Vec<&'static str>, String> {
    for (pair, step) in states.windows(2).zip(steps) {
        if interp::admits(m, &pair[0], &pair[1]) != Some(*step) {
            return Err(format!(
                "`{step}` does not take {:?} to {:?}",
                pair[0], pair[1]
            ));
        }
    }
    Ok(m.invariants
        .iter()
        .filter(|i| states.iter().any(|s| !m.check_invariant(i.name, s)))
        .map(|i| i.name)
        .collect())
}

#[test]
fn every_scene_is_a_run_of_the_model() {
    let m = interp::with_buggy(&explain_honesty_model(), 0);
    let mut bad = Vec::new();
    for (name, samples, tab, truth) in cases() {
        let states = project(&m, truth, &read(&samples, None), tab);
        match replay(&m, &states, &["Detect", "Attach"]) {
            Ok(broken) if broken.is_empty() => {}
            Ok(broken) => bad.push(format!("{name}: breaks {broken:?}")),
            Err(e) => bad.push(format!("{name}: {e}")),
        }
    }
    assert!(
        bad.is_empty(),
        "{} scene(s) the model refuses:\n  {}",
        bad.len(),
        bad.join("\n  ")
    );
}

/// Activities matched by signing id alone: the liar reads as Spotlight.
#[test]
fn a_signing_id_matched_alone_is_caught_by_claim_never_vouches() {
    let (_, samples, tab, truth) = cases()
        .into_iter()
        .find(|c| c.0 == "the liar")
        .expect("the liar");
    let model = explain_honesty_model();
    let snap = read(&samples, Some(Fault::MatchBySigningIdOnly));
    let healthy = interp::with_buggy(&model, 0);
    let states = project(&healthy, truth, &snap, tab);
    assert!(
        replay(&healthy, &states, &["Detect", "Attach"]).is_err(),
        "the healthy model admits the liar as Spotlight"
    );
    let mutant = interp::with_buggy(&model, 2);
    let broken = replay(&mutant, &states, &["Detect", "Attach"])
        .unwrap_or_else(|e| panic!("Buggy 2 does not admit the liar: {e}"));
    assert_eq!(broken, ["ClaimNeverVouches"]);
}

/// `unexplained` blind to the residual: the calm Mac whose reaped coalition
/// left 1.50 cores no coalition held reads "nothing needed".
#[test]
fn a_residual_left_uncovered_is_caught_by_lost_work_never_nothing_needed() {
    let (_, samples, tab, truth) = cases()
        .into_iter()
        .find(|c| c.0.starts_with("a coalition reaped"))
        .expect("the reaped coalition");
    let model = explain_honesty_model();
    let snap = read(&samples, Some(Fault::IgnoreUnaccounted));
    let healthy = interp::with_buggy(&model, 0);
    let states = project(&healthy, truth, &snap, tab);
    assert!(
        replay(&healthy, &states, &["Detect", "Attach"]).is_err(),
        "the healthy model admits nothing needed beside a core no coalition held"
    );
    let mutant = interp::with_buggy(&model, 10);
    let broken = replay(&mutant, &states, &["Detect", "Attach"])
        .unwrap_or_else(|e| panic!("Buggy 10 does not admit the blind residual: {e}"));
    assert_eq!(broken, ["LostWorkNeverNothingNeeded"]);
}

/// One clause tick in the model's terms: whether the candidate held, and
/// whether the clause showed.
fn clause_states(m: &Model, ticks: &[(bool, bool)]) -> (Vec<S>, Vec<&'static str>) {
    let mut states = vec![m.init_state()];
    let mut steps = Vec::new();
    for &(cand, shown) in ticks {
        let prev = states.last().expect("a state").clone();
        let was = prev["clause"] == 1;
        let held = if cand && prev["cand"] == 1 {
            (prev["held"] + 1).min(2)
        } else {
            0
        };
        let fail = if was && !cand {
            if prev["cand"] == 1 {
                0
            } else {
                (prev["fail"] + 1).min(6)
            }
        } else {
            0
        };
        let mut next = prev.clone();
        next.extend([
            ("cand", i64::from(cand)),
            ("held", held),
            ("fail", fail),
            ("clause", i64::from(shown)),
            ("was", i64::from(was)),
        ]);
        states.push(next);
        steps.push(if cand { "Hold" } else { "Fail" });
    }
    (states, steps)
}

/// Spotlight's moment over five windows: the episode opens on the second
/// split tick, so the clause's candidate holds from then on (written from
/// the scene); the clause each tick is the real Deck's.
fn spotlight_ticks(fault: Option<Fault>) -> Vec<(bool, bool)> {
    let mut v = match fault {
        Some(f) => Vitals::new().with_fault(f),
        None => Vitals::new(),
    };
    let snaps: Vec<Snapshot> = spotlight_1032()
        .series(5)
        .iter()
        .map(|s| v.absorb(s))
        .collect();
    snaps
        .iter()
        .skip(2)
        .step_by(2)
        .enumerate()
        .map(|(k, s)| (k >= 1, deck_cause(s).is_some()))
        .collect()
}

#[test]
fn the_clause_over_spotlights_split_ticks_is_a_run_of_the_model() {
    let m = interp::with_buggy(&explain_honesty_model(), 0);
    let ticks = spotlight_ticks(None);
    assert_eq!(
        ticks.iter().filter(|t| t.1).count(),
        2,
        "shown from the fourth tick"
    );
    let (states, steps) = clause_states(&m, &ticks);
    assert_eq!(replay(&m, &states, &steps), Ok(vec![]));
}

/// The real `ClauseHold` over a scripted run: Spotlight's candidate holds
/// four 5 s ticks, fails seven (30 s), and holds once more.
#[test]
fn the_clause_failing_for_thirty_seconds_is_a_run_of_the_model() {
    let split = CoalSplit {
        others_mc: 730,
        services_mc: 2170,
        ..CoalSplit::default()
    };
    let spotlight = [OpenCause {
        id: CauseId::Activity(ActivityId::Spotlight),
        opened_unix_ms: 0,
        since_ms: Some(USERSPACE_MS),
        holds: true,
        covers_mc: 1420,
        contexts: vec![Context::Startup],
    }];
    let script: Vec<bool> = [[true; 4].as_slice(), &[false; 7], &[true]].concat();
    let mut hold = ClauseHold::default();
    let ticks: Vec<(bool, bool)> = script
        .iter()
        .zip((0..).step_by(5_000))
        .map(|(&cand, at)| {
            let open: &[OpenCause] = if cand { &spotlight } else { &[] };
            (cand, hold.step(at, open, &split).is_some())
        })
        .collect();
    assert_eq!(
        ticks.iter().map(|t| t.1).collect::<Vec<_>>(),
        [
            false, false, true, true, true, true, true, true, true, true, false, false
        ],
        "in at 10 s held, out at 30 s failing"
    );
    let m = interp::with_buggy(&explain_honesty_model(), 0);
    let (states, steps) = clause_states(&m, &ticks);
    assert_eq!(replay(&m, &states, &steps), Ok(vec![]));
}

/// The clause with no hold: it shows the tick Spotlight's episode opens.
#[test]
fn a_clause_with_no_hold_is_caught_by_clause_never_flickers() {
    let model = explain_honesty_model();
    let ticks = spotlight_ticks(Some(Fault::NoHold));
    let healthy = interp::with_buggy(&model, 0);
    let (states, steps) = clause_states(&healthy, &ticks);
    assert!(
        replay(&healthy, &states, &steps).is_err(),
        "the healthy model admits a clause with no hold"
    );
    let mutant = interp::with_buggy(&model, 9);
    let broken = replay(&mutant, &states, &steps)
        .unwrap_or_else(|e| panic!("Buggy 9 does not admit the clause: {e}"));
    assert_eq!(broken, ["ClauseNeverFlickers"]);
}
