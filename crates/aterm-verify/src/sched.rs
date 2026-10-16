// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running stages concurrently while the ladder still reads top to bottom.
//!
//! The scheduler models two kinds of contention:
//!
//! * A [`Lane`] is a contended resource, in practice a cargo target directory.
//!   Two cargo invocations on one target dir only queue on cargo's lock, so a
//!   lane's stages run one at a time in declared order. The side lanes have
//!   target dirs of their own, so they overlap the main lane, bar
//!   [`Lane::AfterCompile`], whose stages share its dir and await the test
//!   compile. [`Lane::Pure`] is no resource at all.
//! * An exclusive stage runs with nothing else in flight. The measuring tests
//!   and the two smokes are judged by the clock (paint takes, frame rate,
//!   input-to-present latency), which a lint saturating the other cores would
//!   skew; the Codex live upgrade runs alone too.
//!
//! A stage also waits for the stages it awaits ([`awaited`]): every stage of
//! the lanes its `after_lanes` names and every stage its `after_stages` names
//! (the doctests await the test compile, the trust lane the builds and test
//! runs: [`crate::plan::trust_overlap`]). Stages behind an exclusive barrier
//! declared after the waiter are not awaited (the barrier orders them), nor,
//! through a lane, the [`crate::plan::NOT_AWAITED`] rows.
//!
//! Stage *i* may start when every earlier stage in its lane has finished, no
//! earlier exclusive stage is unfinished, every stage it awaits has finished,
//! and, if *i* is exclusive, nothing at all is running.
//!
//! That rule cannot deadlock given what [`check_after_lanes`] enforces. Take
//! the lowest unfinished stage *i*: everything before it has finished, so only
//! an unfinished awaited stage can block it. An awaited stage is never
//! exclusive or in *i*'s lane, and waits (beyond stages declared before *i*)
//! only on stages *i* awaits that wait on nothing, so some unfinished awaited
//! stage *j* has its own waits met. Nothing exclusive lies between *i* and *j*,
//! and *j*'s unfinished lane predecessors lie after *i* and wait on nothing, so
//! the lowest of them, or *j* itself, is running or startable.
//! `no_starvation_and_no_deadlock` below model-checks this against the real
//! plans.

use std::sync::{Condvar, Mutex};

use crate::ladder::Report;
use crate::plan::{Lane, StageSpec};

/// Live scheduler bookkeeping.
#[derive(Debug)]
struct State {
    started: Vec<bool>,
    done: Vec<bool>,
    running: usize,
    results: Vec<Option<Report>>,
}

/// Can stage `i` start right now? Pure, so the rule is testable without
/// threads; `plan`'s tests ask it directly.
#[must_use]
pub(crate) fn ready(
    specs: &[StageSpec],
    started: &[bool],
    done: &[bool],
    running: usize,
    i: usize,
) -> bool {
    if started[i] {
        return false;
    }
    let me = &specs[i];
    let blocked = specs.iter().take(i).zip(done).any(|(earlier, finished)| {
        !finished
            // I am exclusive, so nothing earlier may be outstanding.
            && (me.exclusive
                // An earlier exclusive stage is a barrier in both directions;
                // without this a steady trickle of pure stages could starve it.
                || earlier.exclusive
                // An earlier stage in my lane still holds the resource. `Pure`
                // is no resource, so pure stages run beside each other.
                || (earlier.lane == me.lane && me.lane != Lane::Pure))
    });
    !blocked && !(me.exclusive && running > 0) && awaited(specs, i).all(|j| done[j])
}

/// The stages `i` waits for: every stage in a lane its [`StageSpec::after_lanes`]
/// names, bar the [`crate::plan::NOT_AWAITED`] rows, and every stage its
/// [`StageSpec::after_stages`] names, except those at or behind the first
/// exclusive stage declared after `i`: that barrier waits for `i` and they wait
/// for the barrier, so waiting for them would deadlock.
pub fn awaited(specs: &[StageSpec], i: usize) -> impl Iterator<Item = usize> + '_ {
    let me = &specs[i];
    let barrier = specs
        .iter()
        .enumerate()
        .skip(i + 1)
        .find(|(_, s)| s.exclusive)
        .map_or(specs.len(), |(k, _)| k);
    (0..barrier).filter(move |&j| {
        j != i
            && ((me.after_lanes.contains(&specs[j].lane) && specs[j].id.awaited())
                || me.after_stages.contains(&specs[j].id))
    })
}

/// The rules the no-deadlock argument above needs. A stage names neither its own
/// lane nor `Pure` in `after_lanes`. A stage it awaits is not exclusive, is not
/// in its lane, and waits only on stages declared before it or on stages it
/// awaits too that wait on nothing; a stage of the awaited one's lane declared
/// between the two waits on nothing, since the awaited stage queues behind it.
/// A wait on a stage declared before the waiter counts as none: when the waiter
/// is the lowest unfinished stage, that stage has finished.
///
/// # Errors
/// Names the first stage that breaks one of those rules.
pub fn check_after_lanes(specs: &[StageSpec]) -> Result<(), String> {
    for (i, me) in specs.iter().enumerate() {
        if me.after_lanes.contains(&me.lane) || me.after_lanes.contains(&Lane::Pure) {
            return Err(format!(
                "{} waits on its own lane or on `pure`: {:?}",
                me.title, me.after_lanes
            ));
        }
        for j in awaited(specs, i) {
            let s = &specs[j];
            if s.lane == me.lane && me.lane != Lane::Pure {
                return Err(format!(
                    "{} awaits {}, a stage of its own lane",
                    me.title, s.title
                ));
            }
            if s.exclusive {
                return Err(format!(
                    "{} awaits {}, which is exclusive",
                    me.title, s.title
                ));
            }
            // One level of waits, through lanes or stages.
            if let Some(k) = awaited(specs, j).find(|&k| {
                k >= i && (!awaited(specs, i).any(|a| a == k) || !waits_before(specs, k, i))
            }) {
                return Err(format!(
                    "{} awaits {}, which awaits {}, a stage that waits itself or that {} does not await",
                    me.title, s.title, specs[k].title, me.title
                ));
            }
            if s.lane != Lane::Pure
                && let Some(k) =
                    (i + 1..j).find(|&k| specs[k].lane == s.lane && !waits_before(specs, k, i))
            {
                return Err(format!(
                    "{} awaits {}, which queues behind {}, a stage that waits itself",
                    me.title, s.title, specs[k].title
                ));
            }
        }
    }
    Ok(())
}

/// Does stage `k` wait only on stages declared before stage `i`? Those have
/// all finished by the time `i` is the lowest unfinished stage, so for the
/// no-deadlock argument such a `k` waits on nothing.
fn waits_before(specs: &[StageSpec], k: usize, i: usize) -> bool {
    awaited(specs, k).all(|m| m < i)
}

/// Run every stage under the rule above, calling `on_report` in declared order
/// as each stage's turn to be printed arrives, and return the reports in that
/// order. `run` runs on a worker thread per stage; it must not assume it is
/// alone unless its spec says `exclusive`.
pub fn run_stages<R, P>(specs: &[StageSpec], run: R, mut on_report: P) -> Vec<Report>
where
    R: Fn(&StageSpec) -> Report + Sync,
    P: FnMut(usize, &Report),
{
    let n = specs.len();
    if n == 0 {
        return Vec::new();
    }
    // A plan that could deadlock is a verify defect, caught by the plan tests;
    // failing loudly here beats a run that hangs silently.
    if let Err(why) = check_after_lanes(specs) {
        panic!("aterm-verify plan defect: {why}");
    }
    let state = Mutex::new(State {
        started: vec![false; n],
        done: vec![false; n],
        running: 0,
        results: (0..n).map(|_| None).collect(),
    });
    let cv = Condvar::new();
    let run = &run;

    let mut ordered: Vec<Report> = Vec::with_capacity(n);
    std::thread::scope(|scope| {
        for i in 0..n {
            let state = &state;
            let cv = &cv;
            scope.spawn(move || {
                {
                    let mut g = state.lock().expect("scheduler mutex");
                    while !ready(specs, &g.started, &g.done, g.running, i) {
                        g = cv.wait(g).expect("scheduler condvar");
                    }
                    g.started[i] = true;
                    g.running += 1;
                }
                let report = run(&specs[i]);
                {
                    let mut g = state.lock().expect("scheduler mutex");
                    g.done[i] = true;
                    g.running -= 1;
                    g.results[i] = Some(report);
                }
                cv.notify_all();
            });
        }

        // The printer: hand out finished stages strictly in declared order, so a
        // fast pure stage never jumps the compile it was scheduled beside.
        for i in 0..n {
            let mut g = state.lock().expect("scheduler mutex");
            while g.results[i].is_none() {
                g = cv.wait(g).expect("scheduler condvar");
            }
            let report = g.results[i].take().expect("just checked");
            drop(g);
            on_report(i, &report);
            ordered.push(report);
        }
    });
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::StageId;
    use std::sync::Mutex as StdMutex;
    use std::time::{Duration, Instant};

    /// A wait for something that must happen is a hang detector, never a latency budget.
    const HANG_DETECTOR: Duration = Duration::from_secs(60);

    /// A context over a fake root that runs `checks`.
    fn ctx(checks: &[crate::checks::Check], scope: crate::Scope) -> crate::Ctx {
        crate::Ctx::new(
            std::path::PathBuf::from("/repo"),
            scope,
            crate::EnvSnapshot::default(),
            std::path::PathBuf::from("/tmp"),
        )
        .with_checks(checks)
    }

    /// The selections whose plans the laws below are checked over: each check
    /// alone, both sets, and every check at once.
    fn selections() -> Vec<Vec<crate::checks::Check>> {
        let every: Vec<crate::checks::Check> =
            crate::checks::CHECKS.iter().map(|s| s.check).collect();
        let mut v: Vec<Vec<crate::checks::Check>> = every.iter().map(|c| vec![*c]).collect();
        v.push(crate::checks::DEV.to_vec());
        v.push(crate::checks::RELEASE.to_vec());
        v.push(every);
        v
    }

    fn spec(id: StageId, title: &str, lane: Lane, exclusive: bool) -> StageSpec {
        StageSpec {
            id,
            title: title.to_string(),
            lane,
            exclusive,
            after_lanes: Vec::new(),
            after_stages: Vec::new(),
        }
    }

    /// The lanes the test run waits for.
    const AWAITED_LANES: [Lane; 1] = [Lane::DriverTarget];

    fn plan_shape() -> Vec<StageSpec> {
        vec![
            spec(StageId::TestCompile, "compile", Lane::MainTarget, false),
            StageSpec {
                after_lanes: AWAITED_LANES.to_vec(),
                ..spec(StageId::Test, "test", Lane::MainTarget, false)
            },
            StageSpec {
                after_stages: vec![StageId::TestCompile],
                ..spec(StageId::Doctests, "doctests", Lane::AfterCompile, false)
            },
            spec(StageId::Tippy, "tippy", Lane::TippyTarget, false),
            spec(StageId::Formatting, "fmt", Lane::Pure, false),
            spec(StageId::Census, "l0", Lane::Pure, false),
            StageSpec {
                after_stages: vec![StageId::TestCompile],
                ..spec(StageId::SpecCheck, "spec", Lane::AfterCompile, false)
            },
            spec(StageId::Export, "export", Lane::TrustTarget, false),
            spec(StageId::Guards, "guards", Lane::Pure, false),
            spec(StageId::LibcOracle, "libc", Lane::LibcOracleTarget, false),
            spec(StageId::Forge, "forge", Lane::Pure, false),
            spec(StageId::ForeignCells, "cells", Lane::CellsTarget, false),
            spec(StageId::DriverBuilds, "drivers", Lane::DriverTarget, false),
            spec(StageId::SealedLane, "sealed", Lane::DriverTarget, false),
            spec(
                StageId::ControlSocketSmoke,
                "smoke",
                Lane::DriverTarget,
                true,
            ),
            spec(StageId::GuiSmoke, "gui", Lane::DriverTarget, true),
            spec(
                StageId::RedrawConformance,
                "redraw",
                Lane::DriverTarget,
                false,
            ),
        ]
    }

    /// Record when each stage held the machine, so overlaps can be checked.
    fn timed_run(specs: &[StageSpec]) -> Vec<(usize, Instant, Instant)> {
        let log: StdMutex<Vec<(usize, Instant, Instant)>> = StdMutex::new(Vec::new());
        let index = |t: &str| specs.iter().position(|s| s.title == t).expect("stage");
        let reports = run_stages(
            specs,
            |s| {
                let start = Instant::now();
                std::thread::sleep(Duration::from_millis(30));
                let end = Instant::now();
                log.lock().expect("log").push((index(&s.title), start, end));
                Report::new(s.title.clone())
            },
            |_, _| {},
        );
        assert_eq!(reports.len(), specs.len(), "every stage produced a report");
        log.into_inner().expect("log")
    }

    fn overlaps(a: (Instant, Instant), b: (Instant, Instant)) -> bool {
        a.0 < b.1 && b.0 < a.1
    }

    /// [`timed_run`] where each stage named in `meet` holds its slot until every
    /// partner it is paired with there has started, bounded by [`HANG_DETECTOR`]:
    /// a rendezvous, not a sleep, so pairs that must overlap do however late
    /// their threads start, and a scheduler that serialises a pair times the first
    /// out and the pair reads disjoint.
    fn met_run(specs: &[StageSpec], meet: &[(&str, &str)]) -> Vec<(usize, Instant, Instant)> {
        let log: StdMutex<Vec<(usize, Instant, Instant)>> = StdMutex::new(Vec::new());
        let entered: StdMutex<Vec<String>> = StdMutex::new(Vec::new());
        let arrived = std::sync::Condvar::new();
        let index = |t: &str| specs.iter().position(|s| s.title == t).expect("stage");
        let reports = run_stages(
            specs,
            |s| {
                let start = Instant::now();
                let partners: Vec<&str> = meet
                    .iter()
                    .filter_map(|&(a, b)| match s.title.as_str() {
                        t if t == a => Some(b),
                        t if t == b => Some(a),
                        _ => None,
                    })
                    .collect();
                let deadline = start + HANG_DETECTOR;
                let mut g = entered.lock().expect("entered");
                g.push(s.title.clone());
                arrived.notify_all();
                while !partners.iter().all(|p| g.iter().any(|e| e == p)) {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    g = arrived.wait_timeout(g, left).expect("entered").0;
                }
                drop(g);
                std::thread::sleep(Duration::from_millis(30));
                let end = Instant::now();
                log.lock().expect("log").push((index(&s.title), start, end));
                Report::new(s.title.clone())
            },
            |_, _| {},
        );
        assert_eq!(reports.len(), specs.len(), "every stage produced a report");
        log.into_inner().expect("log")
    }

    #[test]
    fn output_order_is_the_declared_order_however_the_stages_finish() {
        let specs = plan_shape();
        let mut seen = Vec::new();
        let reports = run_stages(
            &specs,
            |s| {
                // Pure stages finish instantly; the compile takes its time. The
                // ladder must not reorder because of it.
                if s.lane == Lane::MainTarget {
                    std::thread::sleep(Duration::from_millis(40));
                }
                Report::new(s.title.clone())
            },
            |i, r| seen.push((i, r.title.clone())),
        );
        let want: Vec<String> = specs.iter().map(|s| s.title.clone()).collect();
        assert_eq!(
            reports.iter().map(|r| r.title.clone()).collect::<Vec<_>>(),
            want
        );
        assert_eq!(
            seen.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            (0..specs.len()).collect::<Vec<_>>()
        );
        assert_eq!(seen.into_iter().map(|(_, t)| t).collect::<Vec<_>>(), want);
    }

    #[test]
    fn stages_sharing_a_lane_never_overlap_and_keep_their_order() {
        let specs = plan_shape();
        let times = timed_run(&specs);
        for (i, s_i, e_i) in &times {
            for (j, s_j, e_j) in &times {
                if i < j && specs[*i].lane == specs[*j].lane && specs[*i].lane != Lane::Pure {
                    assert!(
                        !overlaps((*s_i, *e_i), (*s_j, *e_j)),
                        "{} and {} share {:?} and must not overlap",
                        specs[*i].title,
                        specs[*j].title,
                        specs[*i].lane
                    );
                    assert!(s_i < s_j, "lane order follows declared order");
                }
            }
        }
    }

    #[test]
    fn an_exclusive_stage_measures_an_idle_machine() {
        let specs = plan_shape();
        let times = timed_run(&specs);
        for (i, s_i, e_i) in &times {
            if !specs[*i].exclusive {
                continue;
            }
            for (j, s_j, e_j) in &times {
                if i == j {
                    continue;
                }
                assert!(
                    !overlaps((*s_i, *e_i), (*s_j, *e_j)),
                    "{} measures timing and ran beside {}",
                    specs[*i].title,
                    specs[*j].title
                );
            }
        }
    }

    #[test]
    fn independent_stages_really_do_run_at_the_same_time() {
        let specs = plan_shape();
        let times = met_run(
            &specs,
            &[
                ("compile", "tippy"),
                ("compile", "l0"),
                ("compile", "libc"),
                ("compile", "guards"),
                ("guards", "l0"),
                ("compile", "fmt"),
                ("compile", "cells"),
                ("compile", "drivers"),
            ],
        );
        let idx = |t: &str| specs.iter().position(|s| s.title == t).expect("stage");
        let at = |i: usize| {
            let (_, s, e) = times.iter().find(|(k, _, _)| *k == i).expect("timed");
            (*s, *e)
        };
        assert!(
            overlaps(at(idx("compile")), at(idx("guards"))),
            "the pure stages must not wait for the test compile"
        );
        assert!(
            overlaps(at(idx("compile")), at(idx("tippy"))),
            "tippy is a lane of its own and must overlap the test compile"
        );
        assert!(
            overlaps(at(idx("compile")), at(idx("l0"))),
            "the censuses are pure and must overlap the test compile"
        );
        assert!(
            overlaps(at(idx("compile")), at(idx("libc"))),
            "the libc oracle builds in its own workspace and must overlap the test compile"
        );
        assert!(
            overlaps(at(idx("guards")), at(idx("l0"))),
            "`Pure` is the ABSENCE of a contended resource: pure stages must \
             overlap each other too, not queue behind one another"
        );
        for side in ["cells", "drivers"] {
            assert!(
                overlaps(at(idx("compile")), at(idx(side))),
                "{side} has its own target dir and must overlap the test compile"
            );
        }
    }

    /// The doctests await only the test compile, so they overlap the test run,
    /// which still waits for the driver lane's rows.
    #[test]
    fn the_doctests_run_beside_the_test_run_after_its_compile() {
        let specs = plan_shape();
        let times = met_run(&specs, &[("test", "doctests")]);
        let at = |t: &str| {
            let i = specs.iter().position(|s| s.title == t).expect("stage");
            let (_, s, e) = times.iter().find(|(k, _, _)| *k == i).expect("timed");
            (*s, *e)
        };
        assert!(at("compile").1 <= at("doctests").0, "after the compile");
        assert!(
            overlaps(at("test"), at("doctests")),
            "the doctests must run beside the test run"
        );
        for driver in ["drivers", "sealed"] {
            assert!(at(driver).1 <= at("test").0, "{driver} before the test run");
        }
    }

    /// The test run never overlaps the driver lane: the stages it awaits finish
    /// before it starts, and the driver stages behind the smokes' barrier start
    /// after it ends.
    #[test]
    fn the_test_run_never_overlaps_a_side_lane() {
        let specs = plan_shape();
        let times = timed_run(&specs);
        let test = specs.iter().position(|s| s.title == "test").expect("test");
        let (_, t_start, t_end) = *times.iter().find(|(k, _, _)| *k == test).expect("timed");
        for (j, s_j, e_j) in &times {
            if AWAITED_LANES.contains(&specs[*j].lane) {
                assert!(
                    !overlaps((t_start, t_end), (*s_j, *e_j)),
                    "the test run overlapped {}",
                    specs[*j].title
                );
                if *j < test || crate::sched::awaited(&specs, test).any(|a| a == *j) {
                    assert!(*e_j <= t_start, "{} must finish first", specs[*j].title);
                } else {
                    assert!(*s_j >= t_end, "{} is behind the barrier", specs[*j].title);
                }
            }
        }
    }

    /// Drive the scheduler's rule without threads: start every stage that is
    /// ready (re-checking after each start, as the real waiters do), then finish
    /// one running stage chosen by `pick`. Panics on a state where stages remain
    /// but nothing runs and nothing can start — a deadlock.
    fn model_check(specs: &[StageSpec], pick: impl FnMut(&[usize]) -> usize) {
        model_check_observed(specs, pick, |_, _, _| {});
    }

    /// [`model_check`], calling `on_start(i, done, running)` just before stage
    /// `i` starts — `done` as it stands then, `running` without `i`.
    fn model_check_observed(
        specs: &[StageSpec],
        mut pick: impl FnMut(&[usize]) -> usize,
        mut on_start: impl FnMut(usize, &[bool], &[usize]),
    ) {
        let n = specs.len();
        let (mut started, mut done) = (vec![false; n], vec![false; n]);
        let mut running: Vec<usize> = Vec::new();
        while done.iter().any(|d| !d) {
            loop {
                let next = (0..n).find(|&i| ready(specs, &started, &done, running.len(), i));
                let Some(i) = next else { break };
                on_start(i, &done, &running);
                started[i] = true;
                running.push(i);
            }
            assert!(
                !running.is_empty(),
                "deadlock: nothing runs and nothing can start; done={done:?}"
            );
            let k = pick(&running);
            let finished = running.remove(k);
            done[finished] = true;
        }
    }

    #[test]
    fn no_starvation_and_no_deadlock() {
        // Every reachable interleaving makes progress, over the shape above and
        // every real plan, finishing the running stages oldest-first,
        // newest-first and in a scrambled order.
        let mut shapes = vec![plan_shape()];
        for checks in selections() {
            for scope in [
                crate::Scope::workspace(),
                crate::Scope::crate_only("aterm-grid"),
                crate::Scope::changed("main", vec![], true),
            ] {
                shapes.push(crate::plan::plan(&ctx(&checks, scope)));
            }
        }
        for specs in &shapes {
            check_after_lanes(specs).expect("a plan that cannot deadlock");
            model_check(specs, |_| 0);
            model_check(specs, |r| r.len() - 1);
            let mut seed = 0x9e37_79b9_u32;
            model_check(specs, |r| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as usize % r.len()
            });
        }

        let specs = plan_shape();
        let n = specs.len();
        // And a running exclusive stage blocks every other stage. Its own
        // start rule guarantees everything before it is already finished, so
        // that is the state to check.
        let ex = specs
            .iter()
            .position(|s| s.exclusive)
            .expect("an exclusive stage");
        let done: Vec<bool> = (0..n).map(|i| i < ex).collect();
        let mut started = done.clone();
        started[ex] = true;
        for i in 0..n {
            if i == ex {
                continue;
            }
            assert!(
                !ready(&specs, &started, &done, 1, i),
                "nothing may run beside the exclusive stage {}",
                specs[ex].title
            );
        }
    }

    /// A driver-lane row started before the driver builds finish meets its
    /// binary mid-link. Over every real plan, the driver builds are the lane's
    /// first row, every later lane row before the first barrier is not exclusive
    /// and waits on no lane, and the test run, in a target dir of its own, waits
    /// on no lane either; over every interleaving, a lane row starts only after
    /// the driver builds finish and never beside another stage of its lane.
    #[test]
    fn every_driver_lane_row_runs_behind_the_driver_builds() {
        let mut lane_rows_seen = 0;
        for checks in selections() {
            for scope in [
                crate::Scope::workspace(),
                crate::Scope::crate_only("aterm-link"),
                crate::Scope::crate_only("aterm-grid"),
                crate::Scope::changed("main", vec!["aterm-gui".into()], true),
                crate::Scope::changed("main", vec![], true),
            ] {
                let what = format!("{checks:?} / {}", scope.label());
                let specs = crate::plan::plan(&ctx(&checks, scope));
                let lane = Lane::DriverTarget;
                let (Some(builds), Some(barrier)) = (
                    specs.iter().position(|s| s.id == StageId::DriverBuilds),
                    specs.iter().position(|s| s.exclusive),
                ) else {
                    continue;
                };
                assert!(
                    specs[..builds].iter().all(|s| s.lane != lane),
                    "{what}: the driver builds are the lane's first row"
                );
                if let Some(test) = specs.iter().position(|s| s.id == StageId::Test) {
                    assert!(specs[test].after_lanes.is_empty(), "{what}");
                }
                for row in (builds + 1..barrier).filter(|&i| specs[i].lane == lane) {
                    let title = &specs[row].title;
                    assert!(!specs[row].exclusive, "{what}: {title}");
                    assert!(specs[row].after_lanes.is_empty(), "{what}: {title}");
                    lane_rows_seen += 1;
                }
                for order in 0..3 {
                    let mut seed = 0x9e37_79b9_u32;
                    model_check_observed(
                        &specs,
                        |r| match order {
                            0 => 0,
                            1 => r.len() - 1,
                            _ => {
                                seed ^= seed << 13;
                                seed ^= seed >> 17;
                                seed ^= seed << 5;
                                seed as usize % r.len()
                            }
                        },
                        |i, done, running| {
                            if specs[i].lane == lane {
                                let title = &specs[i].title;
                                assert!(
                                    i <= builds || done[builds],
                                    "{what}: {title} started before the driver builds finished"
                                );
                                assert!(
                                    running.iter().all(|&j| specs[j].lane != lane),
                                    "{what}: {title} started beside a stage of its own lane"
                                );
                            }
                        },
                    );
                }
            }
        }
        // Not vacuous: the sealed rung and the tail driver builds sit before
        // the barrier in the plans that hold them.
        assert!(lane_rows_seen >= 10, "{lane_rows_seen}");
    }

    #[test]
    fn a_plan_that_could_deadlock_is_refused() {
        // An exclusive stage in an awaited lane, ahead of any barrier, is
        // refused: the no-deadlock argument assumes no awaited stage is
        // exclusive.
        let mut specs = plan_shape();
        specs.insert(
            0,
            spec(StageId::GuiSmoke, "early", Lane::DriverTarget, true),
        );
        assert!(check_after_lanes(&specs).is_err());
        // …and so would one awaiting a stage that waits on lanes itself, here
        // ones the waiter does not await (the test run awaits the driver
        // lane, whose first row now waits on a side lane).
        let mut specs = plan_shape();
        let drivers = specs
            .iter()
            .position(|s| s.title == "drivers")
            .expect("drivers");
        specs[drivers].after_lanes = vec![Lane::TrustTarget];
        assert!(check_after_lanes(&specs).is_err());
        // The same wait on the lane's last row before the barrier, so nothing
        // awaited queues behind it: refused while the test run does not await
        // the side-lane rows, accepted once it awaits every one of them, none of
        // which waits (the shape of the trust lane awaiting the test run).
        let mut specs = plan_shape();
        specs[drivers + 1].after_lanes = vec![Lane::TrustTarget];
        assert!(check_after_lanes(&specs).is_err());
        specs[1].after_stages = vec![StageId::Export];
        check_after_lanes(&specs).expect("one level, inside the waiter's own");
        model_check(&specs, |_| 0);
        model_check(&specs, |r| r.len() - 1);
        // …or one naming its own lane.
        let mut specs = plan_shape();
        specs[1].after_lanes.push(Lane::MainTarget);
        assert!(check_after_lanes(&specs).is_err());
        // …or one naming a later stage of its own lane: the two wait for
        // each other (only the own-lane rule sees this — nothing lies between).
        let mut specs = plan_shape();
        let sealed = drivers + 1;
        assert_eq!(specs[sealed].lane, specs[drivers].lane);
        specs[drivers].after_stages = vec![specs[sealed].id];
        assert!(check_after_lanes(&specs).is_err());
        // …or one awaiting a stage that names stages itself (the test run
        // awaits the driver lane, whose first row now waits on fmt).
        let mut specs = plan_shape();
        specs[drivers].after_stages = vec![StageId::Formatting];
        assert!(check_after_lanes(&specs).is_err());
        // Accepted once the waiter awaits fmt too, fmt waits on nothing and
        // no awaited stage queues behind the one that waits (the trust lane
        // awaiting the test run and its driver rows): here the sealed rung,
        // the driver lane's last row before the barrier…
        let mut specs = plan_shape();
        specs[sealed].after_stages = vec![StageId::Formatting];
        assert!(check_after_lanes(&specs).is_err());
        specs[1].after_stages = vec![StageId::Formatting];
        check_after_lanes(&specs).expect("one level, inside the waiter's own");
        model_check(&specs, |_| 0);
        model_check(&specs, |r| r.len() - 1);
        // …but not when fmt waits itself.
        let fmt = specs.iter().position(|s| s.title == "fmt").expect("fmt");
        specs[fmt].after_stages = vec![StageId::Guards];
        assert!(check_after_lanes(&specs).is_err());
        // …or one awaiting a stage that queues behind a waiter it does not
        // await. Each waits on a stage of the other's lane: 0 on 2, 2 behind
        // 1 (its lane), 1 on 3, 3 behind 0 (its lane) — a cycle in which no
        // stage is ever ready.
        let cycle = vec![
            StageSpec {
                after_stages: vec![StageId::Forge],
                ..spec(StageId::Formatting, "a", Lane::TrustTarget, false)
            },
            StageSpec {
                after_stages: vec![StageId::LibcOracle],
                ..spec(StageId::Tippy, "b", Lane::TippyTarget, false)
            },
            spec(StageId::Forge, "c", Lane::TippyTarget, false),
            spec(StageId::LibcOracle, "d", Lane::TrustTarget, false),
        ];
        let none = vec![false; cycle.len()];
        assert!((0..cycle.len()).all(|i| !ready(&cycle, &none, &none, 0, i)));
        assert!(check_after_lanes(&cycle).is_err());
    }

    #[test]
    fn an_empty_plan_is_not_a_hang() {
        let reports = run_stages(&[], |_| Report::new("never"), |_, _| unreachable!());
        assert!(reports.is_empty());
    }
}
