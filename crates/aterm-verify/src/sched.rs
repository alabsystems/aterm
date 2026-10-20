// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running stages concurrently while the ladder still reads top to bottom.
//!
//! The scheduler models two kinds of contention:
//!
//! * A [`Lane`] is a contended resource — in practice a cargo target directory.
//!   Two cargo invocations against the same target dir do not corrupt anything
//!   (cargo takes a file lock), they just queue — so "running them concurrently"
//!   would buy nothing and cost the ability to reason about the run. Stages in a
//!   lane run in declared order, one at a time. The side lanes have target dirs
//!   of their own under the run's, so they genuinely overlap the main lane's
//!   compile.
//!
//! * An EXCLUSIVE stage runs with nothing else in flight. The measuring tests
//!   and the GUI smoke are judged by the clock: paint takes, frames per
//!   second, input→present latency, sync timeout-releases. A run that
//!   decided "present starvation — frames=12 (< 15)" while a lint saturated the
//!   other cores would be reporting the run, not the build. The two lab lanes
//!   (the codex-alab proof and the Linux VM) load the machine, so they run
//!   alone too, the Linux lane as the last barrier.
//!
//! * An AFTER-STAGES stage waits for every stage it names
//!   ([`StageSpec::after_stages`]): the doctests and `spec-check`, which run in a
//!   lane of their own beside the test run, await the test compile
//!   ([`crate::plan::Lane::AfterCompile`]). Those are its AWAITED stages
//!   ([`awaited`]), and they are always declared BEFORE it ([`check_waits`]).
//!
//! Scheduling rule, in full: stage *i* may start when every earlier stage in its
//! own lane has finished, no earlier exclusive stage is unfinished, every stage
//! *i* awaits has finished, and — if *i* is itself exclusive — nothing at all is
//! running.
//!
//! That rule cannot deadlock. Take the lowest-indexed unfinished stage *i*: every
//! earlier stage has finished, so its lane predecessors are done, no earlier
//! exclusive stage is pending, and every stage it awaits — each declared before it
//! — is done. If *i* is exclusive, nothing else is running either: a later stage
//! could not have started while *i*, an earlier exclusive stage, was unfinished.
//! So *i* is startable, and progress is guaranteed
//! (`no_starvation_and_no_deadlock` below model-checks that against the real
//! plans).

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

/// The stages `i` waits for: every stage its [`StageSpec::after_stages`] names,
/// all declared before it ([`check_waits`]).
pub fn awaited(specs: &[StageSpec], i: usize) -> impl Iterator<Item = usize> + '_ {
    let me = &specs[i];
    (0..i).filter(move |&j| me.after_stages.contains(&specs[j].id))
}

/// What keeps the no-deadlock argument above true: a stage awaits only stages
/// declared before it.
///
/// # Errors
/// Names the first stage that awaits itself or a stage declared after it.
pub fn check_waits(specs: &[StageSpec]) -> Result<(), String> {
    for (i, me) in specs.iter().enumerate() {
        if let Some(later) = specs[i..].iter().find(|s| me.after_stages.contains(&s.id)) {
            return Err(format!(
                "{} awaits {}, which is not declared before it",
                me.title, later.title
            ));
        }
    }
    Ok(())
}

/// Run every stage under the rule above, calling `on_report` in DECLARED order as
/// each stage's turn to be printed arrives, and returning all reports in that
/// same order.
///
/// `run` is invoked on a worker thread per stage; it must not assume it is alone
/// unless its spec says `exclusive`.
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
    if let Err(why) = check_waits(specs) {
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
            after_stages: Vec::new(),
        }
    }

    fn plan_shape() -> Vec<StageSpec> {
        vec![
            spec(StageId::TestCompile, "compile", Lane::MainTarget, false),
            spec(StageId::Test, "test", Lane::MainTarget, false),
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
            spec(StageId::LibcOracle, "libc", Lane::LibcOracleTarget, false),
            spec(StageId::Forge, "forge", Lane::Pure, false),
            spec(StageId::ForeignCells, "cells", Lane::CellsTarget, false),
            // A side-lane feature build at t0, beside the test compile.
            spec(
                StageId::CheckersCompile,
                "checkers",
                Lane::CheckersTarget,
                false,
            ),
            spec(
                StageId::MeasurePrebuild,
                "prebuild",
                Lane::MainTarget,
                false,
            ),
            spec(StageId::MeasuringTests, "measure", Lane::MainTarget, true),
            spec(StageId::GuiSmoke, "gui", Lane::Pure, true),
            spec(
                StageId::RedrawConformance,
                "redraw",
                Lane::MainTarget,
                false,
            ),
            spec(StageId::LinuxLane, "linux", Lane::MainTarget, true),
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
    fn independent_stages_really_do_run_at_the_same_time() {
        let specs = plan_shape();
        let times = met_run(
            &specs,
            &[
                ("compile", "tippy"),
                ("compile", "l0"),
                ("compile", "libc"),
                ("compile", "forge"),
                ("forge", "l0"),
                ("compile", "fmt"),
                ("compile", "checkers"),
                ("compile", "cells"),
            ],
        );
        let idx = |t: &str| specs.iter().position(|s| s.title == t).expect("stage");
        let at = |i: usize| {
            let (_, s, e) = times.iter().find(|(k, _, _)| *k == i).expect("timed");
            (*s, *e)
        };
        assert!(
            overlaps(at(idx("compile")), at(idx("forge"))),
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
            overlaps(at(idx("forge")), at(idx("l0"))),
            "`Pure` is the ABSENCE of a contended resource: pure stages must \
             overlap each other too, not queue behind one another"
        );
        // The side lanes start at t0, beside the test compile.
        for side in ["cells", "checkers"] {
            assert!(
                overlaps(at(idx("compile")), at(idx(side))),
                "{side} has its own target dir and must overlap the test compile"
            );
        }
    }

    /// Drive the scheduler's rule without threads: start every stage that is
    /// ready (re-checking after each start, as the real waiters do), then finish
    /// one running stage chosen by `pick`. Panics on a state where stages remain
    /// but nothing runs and nothing can start — a deadlock — and on any start
    /// beside an exclusive stage: one that measures timing runs on an idle
    /// machine.
    fn model_check(specs: &[StageSpec], mut pick: impl FnMut(&[usize]) -> usize) {
        let n = specs.len();
        let (mut started, mut done) = (vec![false; n], vec![false; n]);
        let mut running: Vec<usize> = Vec::new();
        while done.iter().any(|d| !d) {
            loop {
                let next = (0..n).find(|&i| ready(specs, &started, &done, running.len(), i));
                let Some(i) = next else { break };
                assert!(
                    running.iter().all(|&r| !specs[r].exclusive)
                        && (!specs[i].exclusive || running.is_empty()),
                    "{} started beside an exclusive stage, or beside {running:?} while exclusive",
                    specs[i].title
                );
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
        // Every reachable interleaving makes progress, and nothing runs beside
        // an exclusive stage ([`model_check`]), over the shape above and every
        // real plan, finishing the running stages oldest-first, newest-first
        // and in a scrambled order.
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
            check_waits(specs).expect("a plan that cannot deadlock");
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
    }

    /// A stage that awaits itself or a later stage is refused: the two could
    /// wait for each other. Each wait the CLI plans points back.
    #[test]
    fn a_plan_that_could_deadlock_is_refused() {
        let mut specs = plan_shape();
        let fmt = specs.iter().position(|s| s.title == "fmt").expect("fmt");
        specs[fmt].after_stages = vec![StageId::Census];
        assert!(check_waits(&specs).is_err(), "a wait on a later stage");
        let mut specs = plan_shape();
        specs[fmt].after_stages = vec![StageId::Formatting];
        assert!(check_waits(&specs).is_err(), "a wait on itself");
        // Two stages each waiting on the other's lane mate would never start;
        // the check refuses the plan before a run could hang on it.
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
        assert!(check_waits(&cycle).is_err());
        let mut specs = plan_shape();
        specs[fmt].after_stages = vec![StageId::Tippy];
        check_waits(&specs).expect("a wait on an earlier stage");
        model_check(&specs, |_| 0);
        model_check(&specs, |r| r.len() - 1);
    }
}
