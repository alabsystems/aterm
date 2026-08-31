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
//! Scheduling rule, in full: stage *i* may start when every earlier stage in its
//! own lane has finished, no earlier exclusive stage is unfinished, and — if *i*
//! is itself exclusive — nothing at all is running.
//!
//! That rule cannot deadlock. Take the lowest-indexed unfinished stage: every
//! earlier stage is finished, so its lane predecessors are done and no earlier
//! exclusive stage is pending; if it is exclusive, anything still running would
//! have to be a LATER stage, which could not have started while an earlier
//! exclusive stage was unfinished. So the lowest unfinished stage is always
//! startable, and progress is guaranteed. (`no_starvation_and_no_deadlock` below
//! runs that argument against the real plan shapes.)

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
    !blocked && !(me.exclusive && running > 0)
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
        }
    }

    fn plan_shape() -> Vec<StageSpec> {
        vec![
            spec(StageId::Build, "build", Lane::MainTarget, false),
            spec(StageId::Test, "test", Lane::MainTarget, false),
            spec(StageId::Tippy, "tippy", Lane::TippyTarget, false),
            spec(StageId::GrepGuards, "grep", Lane::Pure, false),
            spec(StageId::LicenseHeaders, "license", Lane::Pure, false),
            spec(StageId::LibcOracle, "libc", Lane::LibcOracleTarget, false),
            spec(StageId::FreezeGate, "l0", Lane::FreezeGateTarget, false),
            spec(StageId::ControlSocketSmoke, "smoke", Lane::MainTarget, true),
            spec(StageId::GuiSmoke, "gui", Lane::MainTarget, true),
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
    }

    #[test]
    fn no_starvation_and_no_deadlock() {
        // The lowest-indexed unfinished stage is always startable — check the
        // invariant directly over every reachable (started, done) shape of a
        // plan that has lanes, a barrier, and stages on both sides of it.
        let specs = plan_shape();
        let n = specs.len();
        for finished in 0..=n {
            let done: Vec<bool> = (0..n).map(|i| i < finished).collect();
            let started = done.clone();
            if finished < n {
                assert!(
                    ready(&specs, &started, &done, 0, finished),
                    "stage {finished} must be startable when everything before it is done"
                );
            }
        }
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
    fn an_empty_plan_is_not_a_hang() {
        let reports = run_stages(&[], |_| Report::new("never"), |_, _| unreachable!());
        assert!(reports.is_empty());
    }
}
