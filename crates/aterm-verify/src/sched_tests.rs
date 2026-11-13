// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
/// alone, both sets, the release's two halves, and every check at once.
fn selections() -> Vec<Vec<crate::checks::Check>> {
    let every: Vec<crate::checks::Check> = crate::checks::CHECKS.iter().map(|s| s.check).collect();
    let mut v: Vec<Vec<crate::checks::Check>> = every.iter().map(|c| vec![*c]).collect();
    v.push(crate::checks::DEV.to_vec());
    v.push(crate::checks::RELEASE.to_vec());
    v.push(crate::checks::RELEASE_TREE.to_vec());
    v.push(crate::checks::RELEASE_BUNDLE.to_vec());
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
        part: None,
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
        StageSpec {
            after_stages: vec![StageId::TestCompile],
            ..spec(StageId::Export, "export", Lane::MainTarget, false)
        },
        spec(StageId::LibcOracle, "libc", Lane::LibcOracleTarget, false),
        spec(StageId::Forge, "forge", Lane::Pure, false),
        spec(StageId::ForeignCells, "cells", Lane::CellsTarget, false),
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
        &Halt::off(),
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
        &Halt::off(),
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
        &Halt::off(),
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

/// Run [`plan_shape`] under `halt` where the test compile, the first stage, meets a red
/// ([`Halt::red`]) and then, if `wait`, holds its slot until the test run (its own lane,
/// after it) and the measure tail (exclusive, after everything) have started or the hang
/// detector runs out: whether both had started while it still ran, and every stage's report.
fn red_while_the_compile_runs(halt: &Halt, wait: bool) -> (bool, Vec<Report>) {
    let started: StdMutex<Vec<String>> = StdMutex::new(Vec::new());
    let arrived = std::sync::Condvar::new();
    let both = ["test", "measure"];
    let met = std::sync::atomic::AtomicBool::new(false);
    let reports = run_stages(
        &plan_shape(),
        u64::MAX,
        halt,
        |s, _| {
            let mut g = started.lock().expect("started");
            g.push(s.title.clone());
            arrived.notify_all();
            if s.title == "compile" {
                halt.red("test");
                let deadline = Instant::now() + HANG_DETECTOR;
                while wait && !both.iter().all(|b| g.iter().any(|t| t == b)) {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    g = arrived.wait_timeout(g, left).expect("started").0;
                }
                met.store(
                    both.iter().all(|b| g.iter().any(|t| t == b)),
                    std::sync::atomic::Ordering::SeqCst,
                );
            }
            Report::new(s.title.clone())
        },
        |_, _| {},
    );
    (met.into_inner(), reports)
}

/// The words each halt below was given to end its run with.
static ENDED: StdMutex<Vec<String>> = StdMutex::new(Vec::new());

fn end(words: &str) {
    ENDED.lock().expect("ended").push(words.to_owned());
}

/// A HALTED RUN HOLDS NOTHING BACK: once the test compile halts the run, the test run behind
/// it in its lane and the exclusive measure tail start while it still runs (each to say what
/// the halt kept from running), every stage still reports once and in declared order, the
/// halt's words name the check, only the first red sets them, and the run's end is given them
/// once; reverted, the waiting stages start only after the compile, past the hang detector.
/// CONTROL: in a run that never halts the same red sets nothing and ends nothing, and every
/// stage reports (its lanes and barriers are the other tests' here: lanes in
/// `stages_sharing_a_lane_never_overlap_and_keep_their_order`, the rest in
/// `no_starvation_and_no_deadlock`).
#[test]
fn a_halted_run_starts_every_waiting_stage_at_once() {
    let halt = Halt::at_first_red("the release", end);
    let (met, reports) = red_while_the_compile_runs(&halt, true);
    assert!(
        met,
        "the waiting stages started while the halting stage ran"
    );
    assert_eq!(
        reports.iter().map(|r| r.title.clone()).collect::<Vec<_>>(),
        plan_shape()
            .iter()
            .map(|s| s.title.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(halt.words(), Some("stopped: the release failed at test"));
    assert!(!halt.red("lint"), "only the first red halts");
    assert_eq!(halt.words(), Some("stopped: the release failed at test"));
    let ended = |words: &str| {
        ENDED
            .lock()
            .expect("ended")
            .iter()
            .filter(|w| *w == words)
            .count()
    };
    assert_eq!(ended("stopped: the release failed at test"), 1);

    let bare = Halt::off();
    let (_, reports) = red_while_the_compile_runs(&bare, false);
    assert_eq!(reports.len(), plan_shape().len());
    assert_eq!(
        (bare.armed(), bare.red("test"), bare.words()),
        (false, false, None)
    );
}

#[test]
fn ordinary_preflight_halts_without_shortening_test_failure_evidence() {
    for check in ["fmt", "census", "export-scan", "lint"] {
        let halt = Halt::after_preflight(|_| {});
        assert!(!halt.armed(), "tests still run their complete sweep");
        assert!(!halt.red("test"));
        assert!(!halt.red("doc"));
        assert!(halt.words().is_none());
        assert!(halt.red(check));
        assert_eq!(
            halt.words(),
            Some(format!("stopped: the run failed at {check}").as_str())
        );
        assert!(!halt.red(check));
    }
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
    assert!(
        overlaps(at(idx("compile")), at(idx("cells"))),
        "cells has its own target dir and must overlap the test compile"
    );
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
            let next = (0..n).find(|&i| ready(specs, &started, &done, running.len(), i, budget));
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

/// The adopted FIFO bounds compiler jobs and binary charges together, so it replaces
/// whole-stage exclusion after preflight. The main target's compilation still
/// precedes its readers.
#[cfg(unix)]
#[test]
fn pooled_tree_checks_overlap_without_losing_the_compile_dependency() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let pool = aterm_capacity::Jobserver::new(&dir.path().join("jobs"), 4).unwrap();
    let mut c = ctx(&crate::checks::RELEASE_TREE, crate::Scope::workspace()).with_pool(Some(pool));
    c.env.build_slot_held = Some("1".into());
    c.env.cargo_build_jobs = Some("4".into());
    let specs = crate::plan::plan(&c);
    assert_eq!(specs[0].id, StageId::Formatting);
    assert!(specs.iter().all(|s| !s.exclusive));
    let at = |id| specs.iter().position(|s| s.id == id).unwrap();
    let title = |id| specs[at(id)].title.as_str();
    let pairs = [
        (title(StageId::Test), title(StageId::Doctests)),
        (title(StageId::Test), title(StageId::SpecCheck)),
    ];
    let spans = met_run(&specs, &pairs);
    let span = |id| {
        let (_, start, end) = spans.iter().find(|(i, _, _)| *i == at(id)).unwrap();
        (*start, *end)
    };
    for (a, b) in pairs {
        let a = specs.iter().find(|s| s.title == a).unwrap().id;
        let b = specs.iter().find(|s| s.title == b).unwrap().id;
        assert!(overlaps(span(a), span(b)));
    }
    assert!(span(StageId::Tippy).1 <= span(StageId::TestCompile).0);
    for id in [StageId::Test, StageId::Doctests, StageId::SpecCheck] {
        assert!(span(StageId::TestCompile).1 <= span(id).0);
    }
    assert!(span(StageId::Doctests).1 <= span(StageId::SpecCheck).0);
    let mut expected = crate::checks::stages_of(&crate::checks::RELEASE_TREE);
    let mut actual: Vec<_> = specs.iter().map(|s| s.id).collect();
    expected.sort_by_key(|id| *id as u8);
    actual.sort_by_key(|id| *id as u8);
    assert_eq!(actual, expected, "every check retains every stage");
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
            ..spec(StageId::Formatting, "a", Lane::MainTarget, false)
        },
        StageSpec {
            after_stages: vec![StageId::LibcOracle],
            ..spec(StageId::Tippy, "b", Lane::TippyTarget, false)
        },
        spec(StageId::Forge, "c", Lane::TippyTarget, false),
        spec(StageId::LibcOracle, "d", Lane::CellsTarget, false),
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
        let Some(i) = (0..specs.len()).find(|&i| ready(specs, started, done, running, i, budget))
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
        claiming(StageId::Export, "export", Lane::MainTarget, 0),
        claiming(StageId::Tippy, "tippy", Lane::TippyTarget, 0),
        claiming(StageId::LibcOracle, "libc", Lane::LibcOracleTarget, 0),
        claiming(StageId::ForeignCells, "cells", Lane::CellsTarget, 0),
        claiming(StageId::SpecCheckers, "checkers", Lane::AfterCompile, 0),
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

/// Plan `p` with its test compile shaped into `n` calls ([`crate::shape::Part`]), in its place
/// and lane, as [`crate::plan::plan`] plans one; `p` itself when it has none.
fn shaped(p: &[StageSpec], n: usize) -> Vec<StageSpec> {
    p.iter()
        .flat_map(|s| {
            if s.id != StageId::TestCompile {
                return vec![s.clone()];
            }
            (0..n)
                .map(|index| StageSpec {
                    title: format!("{}, call {} of {n}", s.title, index + 1),
                    part: Some(crate::shape::Part {
                        piece: crate::shape::Piece::of(&[format!("c{index}")], false),
                        index,
                        of: n,
                        after: Vec::new(),
                        said: String::new(),
                        planned: None,
                        refused: Vec::new(),
                    }),
                    ..s.clone()
                })
                .collect()
        })
        .collect()
}

/// THE BUDGET'S LAWS ON SAMPLED PLANS: every plan a selection makes (each check alone, both
/// sets, every check at once, over three scopes), each with its test compile shaped into two
/// and three calls too (VERIFY-SHAPE: each call its own stage in the main lane, its own
/// claim), and the plan's shape, its stages claiming
/// twelve assignments of 0, 5, 15 and 30 GiB each (four rotations and eight drawn from a
/// fixed seed: a sample, not every assignment), in budgets of 8, 21.5 and 44 GiB, finished in
/// three orders, never deadlocks, never claims past the budget but for one stage alone
/// among those that claim, and never lets a stage overtake an earlier one waiting for room
/// ([`model_check_within`]); every assignment of those claims to the wide plan's five memory
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
            for p in [shaped(&p, 2), shaped(&p, 3), p] {
                if !shapes.contains(&p) {
                    shapes.push(p);
                }
            }
        }
    }
    assert!(
        shapes
            .iter()
            .any(|p| p.iter().filter(|s| s.id == StageId::TestCompile).count() == 3),
        "a test compile shaped into three calls is sampled"
    );
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
    for code in 0..4usize.pow(5) {
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
                    &Halt::off(),
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

/// A stage whose `run` panics still ends its stage. The stage behind it in its
/// lane, and the one behind that, never start, the collector stops waiting for the report it will not get, and the panic
/// reaches the caller with its own payload once every stage thread has ended, rather than every
/// thread parking in the scheduler's wait for ever.
#[test]
fn a_panicking_stage_ends_the_run_with_its_panic_instead_of_parking_the_stages_behind_it() {
    let (tx, rx) = std::sync::mpsc::channel();
    let ran: std::sync::Arc<StdMutex<Vec<String>>> = std::sync::Arc::default();
    let seen = std::sync::Arc::clone(&ran);
    let worker = std::thread::spawn(move || {
        let specs = vec![
            spec(StageId::TestCompile, "compile", Lane::MainTarget, false),
            spec(StageId::Test, "test", Lane::MainTarget, false),
            // Behind a stage that never starts, so it waits on no stage's end.
            spec(
                StageId::MeasurePrebuild,
                "prebuild",
                Lane::MainTarget,
                false,
            ),
            spec(StageId::Formatting, "fmt", Lane::Pure, false),
        ];
        let reports = run_stages(
            &specs,
            u64::MAX,
            &Halt::off(),
            |s, _| {
                seen.lock().expect("ran").push(s.title.clone());
                assert!(s.title != "compile", "planted stage panic");
                Report::new(s.title.clone())
            },
            |_, _| {},
        );
        let _ = tx.send(reports.len());
    });
    match rx.recv_timeout(HANG_DETECTOR) {
        Ok(n) => panic!("the run returned {n} reports past a panicking stage"),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            let payload = worker.join().expect_err("the run raised the stage's panic");
            let words = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_default();
            assert!(words.contains("planted stage panic"), "{words}");
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("hang detector: a panicking stage parked the scheduler for a minute")
        }
    }
    let ran = ran.lock().expect("ran");
    assert!(ran.contains(&"compile".to_string()), "{ran:?}");
    for later in ["test", "prebuild"] {
        assert!(
            !ran.contains(&later.to_string()),
            "no stage starts after a stage panicked: {ran:?}"
        );
    }
}
