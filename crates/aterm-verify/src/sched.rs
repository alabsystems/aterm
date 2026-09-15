// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Running stages concurrently while the ladder still reads top to bottom.
//!
//! The script was sequential because bash is. Most of these stages are genuinely
//! independent — the grep guards, the license headers, the install-channel and
//! start-compare harnesses share nothing with the build — so they run at the same
//! time here. What they are NOT free to share is a resource, and the scheduler
//! models exactly two kinds:
//!
//! * A [`Lane`] is a contended resource — in practice a cargo target directory.
//!   Two cargo invocations against the same target dir do not corrupt anything
//!   (cargo takes a file lock), they just queue — so "running them concurrently"
//!   would buy nothing and cost the ability to reason about the run. Stages in a
//!   lane run in declared order, one at a time. Tippy has its own target dir
//!   (`target-tippy`), the L0 gate has its own workspace, and the libc oracle
//!   owns its nested workspace's two target dirs, so all are real lanes that
//!   genuinely overlap the main build.
//!
//! * An EXCLUSIVE stage runs with nothing else in flight. The two smokes MEASURE:
//!   frames per second, input→present latency, sync timeout-releases. A gate that
//!   decided "present starvation — frames=12 (< 15)" while a lint saturated the
//!   other cores would be reporting the gate, not the build. Exclusivity is what
//!   keeps a ported stage's decision identical to the sequential one.
//!
//! * An AFTER-LANES stage (2026-09-13; today only the test run) also waits for
//!   every stage of the lanes it names — except stages behind an exclusive
//!   barrier declared after it, which cannot overlap it anyway: that barrier
//!   waits for it, and they wait for the barrier. Those are its AWAITED stages
//!   ([`awaited`]).
//!
//! Scheduling rule, in full: stage *i* may start when every earlier stage in its
//! own lane has finished, no earlier exclusive stage is unfinished, every stage
//! *i* awaits has finished, and — if *i* is itself exclusive — nothing at all is
//! running.
//!
//! That rule cannot deadlock, given what [`check_after_lanes`] enforces: an
//! awaited stage is never exclusive, never waits on lanes itself, and is never
//! in the awaiting stage's own lane. Take the lowest-indexed unfinished stage
//! *i*: every earlier stage is finished, so its lane predecessors are done and
//! no earlier exclusive stage is pending; if it is exclusive, anything still
//! running would have to be a LATER stage, which could not have started while
//! an earlier exclusive stage was unfinished. If *i* is blocked at all, it is
//! blocked on an unfinished awaited stage. Take the lowest such *j*: nothing
//! exclusive lies between *i* and *j* (that is what "not behind a barrier"
//! means), its lane predecessors after *i* would be lower awaited stages, and it
//! awaits nothing — so *j* is running or startable. Progress is guaranteed.
//! (`no_starvation_and_no_deadlock` below model-checks that against the real
//! plans.)

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

/// Can stage `i` start right now? Pure, so the rule above is testable without
/// threads.
#[must_use]
fn ready(specs: &[StageSpec], started: &[bool], done: &[bool], running: usize, i: usize) -> bool {
    if started[i] {
        return false;
    }
    let me = &specs[i];
    let blocked = specs.iter().take(i).zip(done).any(|(earlier, finished)| {
        !finished
            // I am exclusive, so nothing earlier may still be outstanding.
            && (me.exclusive
                // An earlier exclusive stage is a barrier in BOTH directions:
                // nothing after it starts until it has finished. Without this a
                // steady trickle of pure stages could starve it forever.
                || earlier.exclusive
                // An earlier stage in my lane still owes me the resource.
                // `Pure` is the absence of a resource, not a resource shared by
                // everything holding it: the guards, the license headers and the
                // two shell harnesses contend for NOTHING, so they run beside
                // each other as well as beside the build.
                || (earlier.lane == me.lane && me.lane != Lane::Pure))
    });
    !blocked && !(me.exclusive && running > 0) && awaited(specs, i).all(|j| done[j])
}

/// The stages `i` waits for through [`StageSpec::after_lanes`]: every stage in
/// one of those lanes, unless an exclusive stage is declared after `i` and at
/// or before it. Such a stage cannot overlap `i` — the barrier waits for `i`
/// and the stage waits for the barrier — and waiting for it would deadlock.
pub fn awaited(specs: &[StageSpec], i: usize) -> impl Iterator<Item = usize> + '_ {
    let me = &specs[i];
    let barrier = specs
        .iter()
        .enumerate()
        .skip(i + 1)
        .find(|(_, s)| s.exclusive)
        .map_or(specs.len(), |(k, _)| k);
    (0..barrier).filter(move |&j| j != i && me.after_lanes.contains(&specs[j].lane))
}

/// What keeps the no-deadlock argument above true. An awaited stage must not be
/// exclusive (it would wait for everything, the waiter included), must not wait
/// on lanes itself (two waiters could wait for each other), and the waiter must
/// not name its own lane or `Pure` (its own lane already orders it, and `Pure`
/// is the absence of a resource).
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
            if s.exclusive || !s.after_lanes.is_empty() {
                return Err(format!(
                    "{} awaits {}, which is exclusive or waits on lanes itself",
                    me.title, s.title
                ));
            }
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
    // A plan that could deadlock is a gate defect, caught by the plan tests;
    // failing loudly here beats a gate that hangs silently.
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
        // fast pure stage never jumps the build it was scheduled beside.
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

/// Human-readable lane name for diagnostics.
#[must_use]
pub fn lane_name(lane: Lane) -> &'static str {
    match lane {
        Lane::Pure => "pure",
        Lane::MainTarget => "target/",
        Lane::TippyTarget => "target-tippy/",
        Lane::FreezeGateTarget => "tools/freeze-safety-gate/target/",
        Lane::LibcOracleTarget => "libc-oracle/{target,target-symgate}/",
        Lane::RegexTarget => "target-regex/",
        Lane::SealedTarget => "target-sealed/",
        Lane::XtaskTarget => "target-xtask/",
        Lane::DriverTarget => "target-drivers/",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::StageId;
    use std::sync::Mutex as StdMutex;
    use std::time::{Duration, Instant};

    fn spec(id: StageId, title: &str, lane: Lane, exclusive: bool) -> StageSpec {
        StageSpec {
            id,
            title: title.to_string(),
            lane,
            exclusive,
            after_lanes: Vec::new(),
        }
    }

    const SIDE_LANES: [Lane; 4] = [
        Lane::RegexTarget,
        Lane::SealedTarget,
        Lane::XtaskTarget,
        Lane::DriverTarget,
    ];

    fn plan_shape() -> Vec<StageSpec> {
        vec![
            spec(StageId::Build, "build", Lane::MainTarget, false),
            StageSpec {
                after_lanes: SIDE_LANES.to_vec(),
                ..spec(StageId::Test, "test", Lane::MainTarget, false)
            },
            spec(StageId::Doctests, "doctests", Lane::MainTarget, false),
            spec(StageId::RegexLane, "regex", Lane::RegexTarget, false),
            spec(StageId::SealedLane, "sealed", Lane::SealedTarget, false),
            spec(StageId::Tippy, "tippy", Lane::TippyTarget, false),
            spec(StageId::Formatting, "fmt", Lane::XtaskTarget, false),
            spec(StageId::GrepGuards, "grep", Lane::Pure, false),
            spec(StageId::LicenseHeaders, "license", Lane::Pure, false),
            spec(StageId::FeatureGates, "gates", Lane::XtaskTarget, false),
            spec(StageId::LibcOracle, "libc", Lane::LibcOracleTarget, false),
            spec(StageId::FreezeGate, "l0", Lane::FreezeGateTarget, false),
            spec(StageId::DriverBuilds, "drivers", Lane::DriverTarget, false),
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

    #[test]
    fn output_order_is_the_declared_order_however_the_stages_finish() {
        let specs = plan_shape();
        let mut seen = Vec::new();
        let reports = run_stages(
            &specs,
            |s| {
                // Pure stages finish instantly; the build takes its time. The
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
                        "{} and {} share {} and must not overlap",
                        specs[*i].title,
                        specs[*j].title,
                        lane_name(specs[*i].lane)
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
        // Otherwise this is just a slower script with more lines.
        let specs = plan_shape();
        let times = timed_run(&specs);
        let idx = |t: &str| specs.iter().position(|s| s.title == t).expect("stage");
        let at = |i: usize| {
            let (_, s, e) = times.iter().find(|(k, _, _)| *k == i).expect("timed");
            (*s, *e)
        };
        assert!(
            overlaps(at(idx("build")), at(idx("grep"))),
            "the pure guards must not wait for the build"
        );
        assert!(
            overlaps(at(idx("build")), at(idx("tippy"))),
            "tippy has its own target dir and must overlap the build"
        );
        assert!(
            overlaps(at(idx("build")), at(idx("l0"))),
            "the L0 gate builds in its own workspace and must overlap the build"
        );
        assert!(
            overlaps(at(idx("build")), at(idx("libc"))),
            "the libc oracle builds in its own workspace and must overlap the build"
        );
        assert!(
            overlaps(at(idx("grep")), at(idx("license"))),
            "`Pure` is the ABSENCE of a contended resource: pure stages must \
             overlap each other too, not queue behind one another"
        );
        // 2026-09-13: the three side lanes start at t0, beside the build.
        for side in ["regex", "fmt", "drivers"] {
            assert!(
                overlaps(at(idx("build")), at(idx(side))),
                "{side} has its own target dir and must overlap the build"
            );
        }
    }

    /// The test run measures (paint, spin), and before 2026-09-13 the regex
    /// lane, the xtask verbs and the driver builds queued behind it in
    /// `target/`. In their own lanes they must still never overlap it — and the
    /// driver stages behind the smokes' barrier must still come after it.
    #[test]
    fn the_test_run_never_overlaps_a_side_lane() {
        let specs = plan_shape();
        let times = timed_run(&specs);
        let test = specs.iter().position(|s| s.title == "test").expect("test");
        let (_, t_start, t_end) = *times.iter().find(|(k, _, _)| *k == test).expect("timed");
        for (j, s_j, e_j) in &times {
            if SIDE_LANES.contains(&specs[*j].lane) {
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
    fn model_check(specs: &[StageSpec], mut pick: impl FnMut(&[usize]) -> usize) {
        let n = specs.len();
        let (mut started, mut done) = (vec![false; n], vec![false; n]);
        let mut running: Vec<usize> = Vec::new();
        while done.iter().any(|d| !d) {
            loop {
                let next = (0..n).find(|&i| ready(specs, &started, &done, running.len(), i));
                let Some(i) = next else { break };
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
        // Before after-lanes, the lowest unfinished stage was always startable.
        // The test run now waits on LATER stages, so check the property that
        // matters instead: every reachable interleaving makes progress. Run
        // over the shape above and over every real plan, finishing the running
        // stages oldest-first, newest-first, and in a scrambled order.
        let ctx = |mode, scope| {
            crate::Ctx::new(
                std::path::PathBuf::from("/repo"),
                mode,
                scope,
                false,
                crate::EnvSnapshot::default(),
                std::path::PathBuf::from("/tmp"),
            )
        };
        let mut shapes = vec![plan_shape()];
        for mode in [crate::Mode::Fast, crate::Mode::Full] {
            for scope in [
                crate::Scope::workspace(),
                crate::Scope::crate_only("aterm-grid"),
                crate::Scope::changed("main", vec![], true),
            ] {
                shapes.push(crate::plan::plan(&ctx(mode, scope)));
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

    #[test]
    fn a_plan_that_could_deadlock_is_refused() {
        // An EXCLUSIVE stage in an awaited lane, ahead of any barrier. One
        // declared after the test run is excluded by the barrier rule; one
        // declared before it is refused too, conservatively — the argument in
        // the module doc assumes no awaited stage is exclusive.
        let mut specs = plan_shape();
        specs.insert(
            0,
            spec(StageId::GuiSmoke, "early", Lane::DriverTarget, true),
        );
        assert!(check_after_lanes(&specs).is_err());
        // …and so would one awaiting a stage that waits on lanes itself.
        let mut specs = plan_shape();
        let regex = specs
            .iter()
            .position(|s| s.title == "regex")
            .expect("regex");
        specs[regex].after_lanes = vec![Lane::DriverTarget];
        assert!(check_after_lanes(&specs).is_err());
        // …or one naming its own lane.
        let mut specs = plan_shape();
        specs[1].after_lanes.push(Lane::MainTarget);
        assert!(check_after_lanes(&specs).is_err());
    }

    #[test]
    fn an_empty_plan_is_not_a_hang() {
        let reports = run_stages(&[], |_| Report::new("never"), |_, _| unreachable!());
        assert!(reports.is_empty());
    }
}
