// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Concurrent stage scheduler ([`Lane`], [`StageSpec::after_stages`],
//! [`crate::plan::StageId::opens_windows`], [`crate::plan::Lane::AfterCompile`], [`awaited`],
//! [`check_waits`]), within the run's memory budget ([`StageSpec::mem`], [`memory_holders`]).

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

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

/// Can stage `i` start right now, within a run whose stages may claim `budget` bytes of memory
/// together? Pure, so the rule is testable without threads: free of every other constraint
/// ([`unconstrained`]) and held by no stage's memory ([`memory_holders`]).
#[must_use]
pub(crate) fn ready(
    specs: &[StageSpec],
    started: &[bool],
    done: &[bool],
    running: usize,
    i: usize,
    budget: u64,
) -> bool {
    unconstrained(specs, started, done, running, i)
        && memory_holders(specs, started, done, running, i, budget).is_empty()
}

/// Could stage `i` start right now, memory aside? Not started, no unfinished earlier stage it
/// shares a lane with, no exclusive one either side, no two that open desktop windows at once, and
/// every stage it awaits done.
fn unconstrained(
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
            && (me.exclusive
                || earlier.exclusive
                || (earlier.lane == me.lane && me.lane != Lane::Pure)
                || (earlier.id.opens_windows() && me.id.opens_windows()))
    });
    !blocked && !(me.exclusive && running > 0) && awaited(specs, i).all(|j| done[j])
}

/// THE STAGES WHOSE MEMORY HOLDS STAGE `i` BACK, by index; none for a stage that claims no memory
/// ([`StageSpec::mem`] `0`), which memory never holds. A stage that claims some does not start
///
/// - while the stages that claim some and run would claim more than `budget` with it: the ones
///   running are its holders. With none running it starts whatever it claims, so a stage that
///   claims more than the whole budget runs, and runs with no other that claims memory;
/// - nor while one declared before it that claims some is free of every other constraint
///   ([`unconstrained`]) and not yet started: that one is its holder, so a stage that fits never
///   overtakes an earlier one that waits for room, and the earlier one starts as soon as there
///   is room for it.
///
/// The scheduler alone never deadlocks on this: with nothing running, the first stage declared
/// that claims memory and is free of every other constraint has no holder, and stages that claim
/// none have none, so a stage is ready whenever one is free of every other constraint, as it is
/// without memory ([`check_waits`]). What a started stage waits for outside it is not this
/// rule's: its claims in the capacity ledger, which wait on other runs' claims, and which hold
/// nothing while they wait but a timed stage's quiet machine, its run's across its timed
/// stages, with memory claimed only on it (`crate::staged`; the ledger's FIFO and ALONE rules
/// hold its waits to an end).
#[must_use]
pub fn memory_holders(
    specs: &[StageSpec],
    started: &[bool],
    done: &[bool],
    running: usize,
    i: usize,
    budget: u64,
) -> Vec<usize> {
    let me = &specs[i];
    if me.mem == 0 || started[i] {
        return Vec::new();
    }
    let claiming: Vec<usize> = (0..specs.len())
        .filter(|&j| started[j] && !done[j] && specs[j].mem > 0)
        .collect();
    let claimed = claiming
        .iter()
        .fold(0u64, |sum, &j| sum.saturating_add(specs[j].mem));
    if !claiming.is_empty() && claimed.saturating_add(me.mem) > budget {
        return claiming;
    }
    (0..i)
        .filter(|&j| specs[j].mem > 0 && unconstrained(specs, started, done, running, j))
        .take(1)
        .collect()
}

/// What memory kept a stage from starting once nothing else did ([`memory_holders`]): for how
/// long, and behind which stages (by index, each named once, in the order first seen).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Held {
    /// The time it was held by memory alone.
    pub waited: Duration,
    /// The stages that held it.
    pub behind: Vec<usize>,
}

/// Stages `i` waits for ([`StageSpec::after_stages`], [`check_waits`]).
pub fn awaited(specs: &[StageSpec], i: usize) -> impl Iterator<Item = usize> + '_ {
    let me = &specs[i];
    (0..i).filter(move |&j| me.after_stages.contains(&specs[j].id))
}

/// What keeps the no-deadlock argument above true: a stage awaits only stages declared before it.
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

/// Run every stage under the rule above, its stages that claim memory within `budget` bytes
/// together ([`ready`]); `run` is told what memory held each stage back ([`Held`]).
pub fn run_stages<R, P>(specs: &[StageSpec], budget: u64, run: R, mut on_report: P) -> Vec<Report>
where
    R: Fn(&StageSpec, &Held) -> Report + Sync,
    P: FnMut(usize, &Report),
{
    let n = specs.len();
    if n == 0 {
        return Vec::new();
    }
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
                let mut held = Held::default();
                {
                    let mut g = state.lock().expect("scheduler mutex");
                    let mut since: Option<Instant> = None;
                    while !ready(specs, &g.started, &g.done, g.running, i, budget) {
                        let holders =
                            memory_holders(specs, &g.started, &g.done, g.running, i, budget);
                        if unconstrained(specs, &g.started, &g.done, g.running, i)
                            && !holders.is_empty()
                        {
                            since.get_or_insert_with(Instant::now);
                            for h in holders {
                                if !held.behind.contains(&h) {
                                    held.behind.push(h);
                                }
                            }
                        } else if let Some(at) = since.take() {
                            held.waited += at.elapsed();
                        }
                        g = cv.wait(g).expect("scheduler condvar");
                    }
                    if let Some(at) = since {
                        held.waited += at.elapsed();
                    }
                    g.started[i] = true;
                    g.running += 1;
                }
                // A stage held back only until this one started may start now
                // ([`memory_holders`]).
                cv.notify_all();
                let report = run(&specs[i], &held);
                {
                    let mut g = state.lock().expect("scheduler mutex");
                    g.done[i] = true;
                    g.running -= 1;
                    g.results[i] = Some(report);
                }
                cv.notify_all();
            });
        }

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
            mem: 0,
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
            u64::MAX,
            |s, _| {
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
            u64::MAX,
            |s, _| {
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
            u64::MAX,
            |s, _| {
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
        for side in ["cells", "checkers"] {
            assert!(
                overlaps(at(idx("compile")), at(idx(side))),
                "{side} has its own target dir and must overlap the test compile"
            );
        }
    }

    #[test]
    fn windowed_stages_serialize_without_blocking_independent_headless_work() {
        let wanted = crate::checks::stages_of(&[
            crate::Check::Lint,
            crate::Check::Census,
            crate::Check::Browser,
            crate::Check::Appkit,
        ]);
        let specs: Vec<_> = crate::plan::declared(&crate::Scope::workspace())
            .into_iter()
            .filter(|s| wanted.contains(&s.id))
            .collect();
        let at = |id| specs.iter().position(|s| s.id == id).expect("planned");
        let browser = at(StageId::BrowserLane);
        let redraw = at(StageId::RedrawConformance);
        let objc = at(StageId::ObjcDrives);
        let tippy = at(StageId::Tippy);
        let census = at(StageId::Census);
        let mut started = vec![false; specs.len()];
        let mut done = vec![false; specs.len()];

        assert!(ready(&specs, &started, &done, 0, browser, u64::MAX));
        assert!(!ready(&specs, &started, &done, 0, redraw, u64::MAX));
        started[browser] = true;
        assert!(!ready(&specs, &started, &done, 1, redraw, u64::MAX));
        assert!(!ready(&specs, &started, &done, 1, objc, u64::MAX));
        assert!(ready(&specs, &started, &done, 1, tippy, u64::MAX));
        assert!(ready(&specs, &started, &done, 1, census, u64::MAX));

        started[tippy] = true;
        done[browser] = true;
        assert!(ready(&specs, &started, &done, 1, redraw, u64::MAX));
        started[redraw] = true;
        assert!(!ready(&specs, &started, &done, 2, objc, u64::MAX));
        assert!(ready(&specs, &started, &done, 2, census, u64::MAX));
        done[redraw] = true;
        assert!(ready(&specs, &started, &done, 1, objc, u64::MAX));
    }

    /// Drive the scheduler's rule without threads: start every stage that is
    /// ready (re-checking after each start, as the real waiters do), then finish
    /// one running stage chosen by `pick`.
    fn model_check(specs: &[StageSpec], mut pick: impl FnMut(&[usize]) -> usize) {
        model_check_limit(specs, &mut pick, usize::MAX);
    }

    fn model_check_limit(specs: &[StageSpec], pick: impl FnMut(&[usize]) -> usize, maximum: usize) {
        model_check_within(specs, u64::MAX, pick, maximum);
    }

    /// [`model_check_limit`] in a run whose stages may claim `budget` bytes of memory
    /// together, holding the budget's laws at each start: the stages that claim memory and run
    /// claim no more than `budget` together unless one runs alone among them, and none starts
    /// while one declared before it that claims memory could start but for memory.
    fn model_check_within(
        specs: &[StageSpec],
        budget: u64,
        mut pick: impl FnMut(&[usize]) -> usize,
        maximum: usize,
    ) {
        let n = specs.len();
        let (mut started, mut done) = (vec![false; n], vec![false; n]);
        let mut running: Vec<usize> = Vec::new();
        while done.iter().any(|d| !d) {
            loop {
                let next =
                    (0..n).find(|&i| ready(specs, &started, &done, running.len(), i, budget));
                let Some(i) = next else { break };
                let claiming: Vec<usize> = running
                    .iter()
                    .copied()
                    .chain([i])
                    .filter(|&r| specs[r].mem > 0)
                    .collect();
                let claimed: u64 = claiming.iter().map(|&r| specs[r].mem).sum();
                assert!(
                    claiming.len() <= 1 || claimed <= budget,
                    "{} started beside {running:?}: {claimed} bytes claimed of {budget}",
                    specs[i].title
                );
                assert!(
                    specs[i].mem == 0
                        || !(0..i).any(|j| specs[j].mem > 0
                            && unconstrained(specs, &started, &done, running.len(), j)),
                    "{} overtook an earlier stage waiting for memory",
                    specs[i].title
                );
                assert!(
                    running.len() < maximum,
                    "{} would spend an admitted CPU share twice",
                    specs[i].title
                );
                assert!(
                    running.iter().all(|&r| !specs[r].exclusive)
                        && (!specs[i].exclusive || running.is_empty()),
                    "{} started beside an exclusive stage, or beside {running:?} while exclusive",
                    specs[i].title
                );
                assert!(
                    !specs[i].id.opens_windows()
                        || running.iter().all(|&r| !specs[r].id.opens_windows()),
                    "{} started beside another stage that opens desktop windows",
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
    fn managed_plans_spend_their_share_on_one_stage_without_losing_checks() {
        for checks in selections() {
            for scope in [
                crate::Scope::workspace(),
                crate::Scope::crate_only("aterm-grid"),
                crate::Scope::changed("main", vec![], true),
            ] {
                let plain = crate::plan::plan(&ctx(&checks, scope.clone()));
                let mut managed = ctx(&checks, scope);
                managed.env.build_slot_held = Some("1".into());
                managed.env.cargo_build_jobs = Some("6".into());
                let managed = crate::plan::plan(&managed);
                assert_eq!(plain.len(), managed.len());
                for (old, new) in plain.iter().zip(&managed) {
                    assert_eq!(
                        (old.id, &old.title, old.lane, &old.after_stages),
                        (new.id, &new.title, new.lane, &new.after_stages)
                    );
                }
                model_check_limit(&managed, |_| 0, 1);
                model_check_limit(&managed, |running| running.len() - 1, 1);
            }
        }
    }

    #[test]
    fn no_starvation_and_no_deadlock() {
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
    /// wait for each other.
    #[test]
    fn a_plan_that_could_deadlock_is_refused() {
        let mut specs = plan_shape();
        let fmt = specs.iter().position(|s| s.title == "fmt").expect("fmt");
        specs[fmt].after_stages = vec![StageId::Census];
        assert!(check_waits(&specs).is_err(), "a wait on a later stage");
        let mut specs = plan_shape();
        specs[fmt].after_stages = vec![StageId::Formatting];
        assert!(check_waits(&specs).is_err(), "a wait on itself");
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

    const GIB: u64 = 1 << 30;

    /// This 24 GiB Mac's budget: what can ever be available here, 2.5 GiB of it wired.
    const BUDGET: u64 = 43 * GIB / 2;

    /// Stage `id` titled `title` in `lane`, claiming `mem` bytes.
    fn claiming(id: StageId, title: &str, lane: Lane, mem: u64) -> StageSpec {
        StageSpec {
            mem,
            ..spec(id, title, lane, false)
        }
    }

    /// The stages of `specs` that are ready now, by title.
    fn ready_now<'a>(
        specs: &'a [StageSpec],
        started: &[bool],
        done: &[bool],
        budget: u64,
    ) -> Vec<&'a str> {
        let running = (0..specs.len()).filter(|&j| started[j] && !done[j]).count();
        (0..specs.len())
            .filter(|&i| ready(specs, started, done, running, i, budget))
            .map(|i| specs[i].title.as_str())
            .collect()
    }

    /// The stages of `specs` a waiter starts now, by title, in the order it starts them: each
    /// stage ready once the ones before it started ([`model_check_within`]'s rounds).
    fn start_all<'a>(
        specs: &'a [StageSpec],
        started: &mut [bool],
        done: &[bool],
        budget: u64,
    ) -> Vec<&'a str> {
        let mut titles = Vec::new();
        loop {
            let running = (0..specs.len()).filter(|&j| started[j] && !done[j]).count();
            let Some(i) =
                (0..specs.len()).find(|&i| ready(specs, started, done, running, i, budget))
            else {
                return titles;
            };
            started[i] = true;
            titles.push(specs[i].title.as_str());
        }
    }

    /// The incident this rule answers (2026-10-28: one `verify -p aterm-gui` here reached 28 GB):
    /// a test compile, a lint and a cells build that each fit alone start in turn, never three at
    /// once. Claiming 15, 11 and 10 GiB in a 21.5 GiB budget, the compile starts alone; once it
    /// ends, the lint and the cells build (21 GiB together) start together. Claiming their peaks
    /// and margins (18, 13.2 and 12 GiB), each runs alone. A stage that claims nothing is never
    /// held. NEGATIVE CONTROL: with no budget all three start at once.
    #[test]
    fn stages_that_fit_alone_but_not_together_take_turns() {
        let specs = vec![
            claiming(StageId::TestCompile, "compile", Lane::MainTarget, 15 * GIB),
            claiming(StageId::Tippy, "tippy", Lane::TippyTarget, 11 * GIB),
            claiming(StageId::ForeignCells, "cells", Lane::CellsTarget, 10 * GIB),
            spec(StageId::Formatting, "fmt", Lane::Pure, false),
        ];
        assert_eq!(
            start_all(&specs, &mut [false; 4], &[false; 4], u64::MAX),
            ["compile", "tippy", "cells", "fmt"]
        );
        assert_eq!(
            start_all(&specs, &mut [false; 4], &[false; 4], BUDGET),
            ["compile", "fmt"]
        );
        let (mut started, mut done) = (vec![false; 4], vec![false; 4]);
        assert_eq!(
            ready_now(&specs, &started, &done, BUDGET),
            ["compile", "fmt"]
        );
        started[0] = true;
        assert_eq!(ready_now(&specs, &started, &done, BUDGET), ["fmt"]);
        for (waiting, why) in [(1, "the lint"), (2, "the cells build")] {
            assert_eq!(
                memory_holders(&specs, &started, &done, 1, waiting, BUDGET),
                [0],
                "{why} waits for room behind the compile"
            );
        }
        done[0] = true;
        assert_eq!(ready_now(&specs, &started, &done, BUDGET), ["tippy", "fmt"]);
        assert_eq!(
            memory_holders(&specs, &started, &done, 0, 2, BUDGET),
            [1],
            "the cells build waits behind the lint, declared before it, until it starts"
        );
        started[1] = true;
        assert_eq!(ready_now(&specs, &started, &done, BUDGET), ["cells", "fmt"]);

        let peaks: Vec<StageSpec> = specs
            .iter()
            .map(|s| StageSpec {
                mem: if s.mem == 0 {
                    0
                } else {
                    aterm_capacity::peak_claim(s.mem)
                },
                ..s.clone()
            })
            .collect();
        let mut pairs = Vec::new();
        model_check_within(&peaks, BUDGET, |r| r.len() - 1, usize::MAX);
        let (mut started, mut done) = (vec![false; 4], vec![false; 4]);
        for heavy in 0..3 {
            assert_eq!(
                ready_now(&peaks, &started, &done, BUDGET),
                [peaks[heavy].title.as_str(), "fmt"]
            );
            started[heavy] = true;
            pairs.push(ready_now(&peaks, &started, &done, BUDGET).len());
            done[heavy] = true;
        }
        assert_eq!(pairs, [1, 1, 1], "only fmt beside each");
    }

    /// A stage that claims more than the whole budget still runs, and runs with no other that
    /// claims memory: 30 GiB in 21.5 waits for the 5 GiB lint to end, then starts, and the 5 GiB
    /// cells build after it waits until it ends; the formatter runs beside it. NEGATIVE CONTROL:
    /// with no budget the cells build starts beside it.
    #[test]
    fn a_stage_above_the_budget_runs_with_no_other_that_claims_memory() {
        let specs = vec![
            claiming(StageId::Tippy, "tippy", Lane::TippyTarget, 5 * GIB),
            claiming(StageId::TestCompile, "compile", Lane::MainTarget, 30 * GIB),
            claiming(StageId::ForeignCells, "cells", Lane::CellsTarget, 5 * GIB),
            spec(StageId::Formatting, "fmt", Lane::Pure, false),
        ];
        let (mut started, mut done) = (vec![false; 4], vec![false; 4]);
        assert_eq!(ready_now(&specs, &started, &done, BUDGET), ["tippy", "fmt"]);
        started[0] = true;
        assert_eq!(ready_now(&specs, &started, &done, BUDGET), ["fmt"]);
        done[0] = true;
        assert_eq!(
            ready_now(&specs, &started, &done, BUDGET),
            ["compile", "fmt"]
        );
        started[1] = true;
        assert_eq!(ready_now(&specs, &started, &done, BUDGET), ["fmt"]);
        assert_eq!(memory_holders(&specs, &started, &done, 1, 2, BUDGET), [1]);
        assert_eq!(
            ready_now(&specs, &started, &done, u64::MAX),
            ["cells", "fmt"]
        );
        done[1] = true;
        assert_eq!(ready_now(&specs, &started, &done, BUDGET), ["cells", "fmt"]);
    }

    /// Declared order holds for memory: with a 10 GiB stage running, a 15 GiB one declared next
    /// waits for room, and a 5 GiB one declared after it, which would fit, waits behind it rather
    /// than overtake it; once the 10 GiB stage ends, the 15 GiB one starts, and then the 5 GiB
    /// one beside it. A stage held by something other than memory (the doctests, after the
    /// compile) holds nothing back. NEGATIVE CONTROL: the 5 GiB stage alone after the 10 GiB one
    /// starts beside it.
    #[test]
    fn a_stage_that_fits_never_overtakes_an_earlier_one_waiting_for_room() {
        let specs = vec![
            claiming(
                StageId::LibcOracle,
                "libc",
                Lane::LibcOracleTarget,
                10 * GIB,
            ),
            claiming(StageId::TestCompile, "compile", Lane::MainTarget, 15 * GIB),
            claiming(StageId::Tippy, "tippy", Lane::TippyTarget, 5 * GIB),
        ];
        let (mut started, mut done) = (vec![false; 3], vec![false; 3]);
        started[0] = true;
        assert_eq!(
            ready_now(&specs, &started, &done, BUDGET),
            Vec::<&str>::new()
        );
        assert_eq!(memory_holders(&specs, &started, &done, 1, 2, BUDGET), [1]);
        done[0] = true;
        assert_eq!(ready_now(&specs, &started, &done, BUDGET), ["compile"]);
        started[1] = true;
        assert_eq!(ready_now(&specs, &started, &done, BUDGET), ["tippy"]);

        let alone = vec![specs[0].clone(), specs[2].clone()];
        assert_eq!(
            ready_now(&alone, &[true, false], &[false, false], BUDGET),
            ["tippy"]
        );

        let after = vec![
            claiming(
                StageId::LibcOracle,
                "libc",
                Lane::LibcOracleTarget,
                10 * GIB,
            ),
            claiming(StageId::TestCompile, "compile", Lane::MainTarget, 5 * GIB),
            StageSpec {
                after_stages: vec![StageId::TestCompile],
                ..claiming(StageId::Doctests, "doc", Lane::AfterCompile, 15 * GIB)
            },
            claiming(StageId::Tippy, "tippy", Lane::TippyTarget, 5 * GIB),
        ];
        assert_eq!(
            ready_now(&after, &[true, true, false, false], &[false; 4], BUDGET),
            ["tippy"],
            "the doctests wait for the compile, not for room, and hold the lint back from nothing"
        );
    }

    /// Memory stages free of every other constraint: each in a lane of its own, beside a pure
    /// one, in declared order.
    fn wide() -> Vec<StageSpec> {
        vec![
            claiming(StageId::TestCompile, "compile", Lane::MainTarget, 0),
            claiming(StageId::Tippy, "tippy", Lane::TippyTarget, 0),
            claiming(StageId::Export, "export", Lane::TrustTarget, 0),
            claiming(StageId::LibcOracle, "libc", Lane::LibcOracleTarget, 0),
            claiming(StageId::ForeignCells, "cells", Lane::CellsTarget, 0),
            claiming(
                StageId::CheckersCompile,
                "checkers",
                Lane::CheckersTarget,
                0,
            ),
            spec(StageId::Formatting, "fmt", Lane::Pure, false),
        ]
    }

    /// The claims each test below assigns stages from, and the budgets it runs them in: below,
    /// at and above what two of them claim.
    const CLAIMS: [u64; 4] = [0, 5 * GIB, 15 * GIB, 30 * GIB];
    const BUDGETS: [u64; 3] = [8 * GIB, BUDGET, 44 * GIB];

    /// A xorshift step: the assignment of claims below is varied, and the same on every run.
    fn step(seed: &mut u32) -> u32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        *seed
    }

    /// THE BUDGET'S LAWS ON SAMPLED PLANS: every plan a selection makes (each check alone, both
    /// sets, every check at once, over three scopes) and the plan's shape, its stages claiming
    /// twelve assignments of 0, 5, 15 and 30 GiB each (four rotations and eight drawn from a
    /// fixed seed: a sample, not every assignment), in budgets of 8, 21.5 and 44 GiB, finished in
    /// three orders, never deadlocks, never claims past the budget but for one stage alone
    /// among those that claim, and never lets a stage overtake an earlier one waiting for room
    /// ([`model_check_within`]); every assignment of those claims to the wide plan's six memory
    /// stages too. NEGATIVE CONTROL: a rule that lets a stage start whenever it fits alone, or
    /// lets one that fits overtake an earlier one waiting for room, fails a law here.
    #[test]
    fn sampled_plans_keep_the_budget_and_finish() {
        let mut shapes = vec![plan_shape()];
        for checks in selections() {
            for scope in [
                crate::Scope::workspace(),
                crate::Scope::crate_only("aterm-grid"),
                crate::Scope::changed("main", vec![], true),
            ] {
                let p = crate::plan::plan(&ctx(&checks, scope));
                if !shapes.contains(&p) {
                    shapes.push(p);
                }
            }
        }
        let mut seed = 0x2545_f491_u32;
        for shape in &shapes {
            let n = shape.len();
            let mut assignments: Vec<Vec<u64>> = (0..4)
                .map(|o| (0..n).map(|i| CLAIMS[(i + o) % 4]).collect())
                .collect();
            for _ in 0..8 {
                assignments.push(
                    (0..n)
                        .map(|_| CLAIMS[step(&mut seed) as usize % 4])
                        .collect(),
                );
            }
            for claims in assignments {
                let specs: Vec<StageSpec> = shape
                    .iter()
                    .zip(&claims)
                    .map(|(s, &mem)| StageSpec { mem, ..s.clone() })
                    .collect();
                for budget in BUDGETS {
                    model_check_within(&specs, budget, |_| 0, usize::MAX);
                    model_check_within(&specs, budget, |r| r.len() - 1, usize::MAX);
                    model_check_within(
                        &specs,
                        budget,
                        |r| step(&mut seed) as usize % r.len(),
                        usize::MAX,
                    );
                }
            }
        }
        let base = wide();
        for code in 0..4usize.pow(6) {
            let mut digits = code;
            let specs: Vec<StageSpec> = base
                .iter()
                .map(|s| {
                    let mem = if s.lane == Lane::Pure {
                        0
                    } else {
                        CLAIMS[digits % 4]
                    };
                    digits /= 4;
                    StageSpec { mem, ..s.clone() }
                })
                .collect();
            for budget in BUDGETS {
                model_check_within(&specs, budget, |_| 0, usize::MAX);
                model_check_within(&specs, budget, |r| r.len() - 1, usize::MAX);
            }
        }
    }

    /// THE THREADED SCHEDULER KEEPS THE BUDGET AND FINISHES: the plan's shape with its memory
    /// stages claiming varied assignments of 0, 5, 15 and 30 GiB, in each budget and in one of
    /// zero bytes (where no two stages that claim memory ever run together), run on real threads,
    /// ends within the hang detector; at no stage's start do the stages in flight claim past the
    /// budget but for one alone among those that claim, and a stage is held only behind others
    /// that claim memory. NEGATIVE CONTROL: a rule that holds a stage above the budget back even
    /// with nothing else claiming never finishes, and the hang detector fails it by name.
    #[test]
    fn the_threaded_scheduler_keeps_the_budget_and_finishes() {
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let shape = plan_shape();
            let mut seed = 0x1234_5679_u32;
            for round in 0..12 {
                let specs: Vec<StageSpec> = shape
                    .iter()
                    .map(|s| StageSpec {
                        mem: if s.lane == Lane::Pure {
                            0
                        } else {
                            CLAIMS[step(&mut seed) as usize % 4]
                        },
                        ..s.clone()
                    })
                    .collect();
                for budget in BUDGETS.into_iter().chain([0]) {
                    let flight: StdMutex<Vec<usize>> = StdMutex::new(Vec::new());
                    let index = |t: &str| specs.iter().position(|s| s.title == t).expect("stage");
                    let reports = run_stages(
                        &specs,
                        budget,
                        |s, held| {
                            let me = index(&s.title);
                            {
                                let mut f = flight.lock().expect("flight");
                                f.push(me);
                                let claiming: Vec<usize> =
                                    f.iter().copied().filter(|&r| specs[r].mem > 0).collect();
                                let claimed: u64 = claiming.iter().map(|&r| specs[r].mem).sum();
                                assert!(
                                    claiming.len() <= 1 || claimed <= budget,
                                    "round {round}: {claiming:?} claim {claimed} of {budget}"
                                );
                            }
                            assert!(
                                held.behind.iter().all(|&h| specs[h].mem > 0 && h != me),
                                "{}: held behind {:?}",
                                s.title,
                                held.behind
                            );
                            flight.lock().expect("flight").retain(|&r| r != me);
                            Report::new(s.title.clone())
                        },
                        |_, _| {},
                    );
                    assert_eq!(reports.len(), specs.len());
                }
            }
            let _ = tx.send(());
        });
        match rx.recv_timeout(HANG_DETECTOR) {
            Ok(()) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                std::panic::resume_unwind(worker.join().expect_err("the runs panicked"));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("hang detector: the threaded scheduler did not finish its runs in a minute")
            }
        }
    }
}
