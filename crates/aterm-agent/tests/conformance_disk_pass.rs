// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests for `DiskReclaimPass` against `disk::pass_plan` and `disk::apply_auto`,
//! and for `DiskBudgetPass` against `disk::budget`'s eligibility and memo.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use aterm_agent::harness::cli;
use aterm_agent::harness::disk::{
    self, Applied, Class, Config, Planned, Refusal, Removed, Report, Row, Step, Tier, Trigger,
    Witness, discover,
};
use aterm_models_harness::{disk_budget_pass_model, disk_reclaim_pass_model};
use aterm_spec::derive::Model;

type S = std::collections::BTreeMap<&'static str, i64>;

const NOW: i64 = 1_792_195_200;

const GIB_BYTES: u64 = 1 << 30;

fn abs(posix: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{posix}"))
    } else {
        PathBuf::from(posix)
    }
}

/// What the real code did, in order: a re-measure (the bytes it answered) or a
/// step it told its journal.
#[derive(Debug, Clone)]
#[allow(
    dead_code,
    reason = "the paths are read in a failure's dump of the events"
)]
enum Event {
    Read(u64),
    Removing(PathBuf, Option<Tier>),
    /// The real primitive took every lock of a profile (`Stage::Locked`):
    /// the row, the profile.
    Locked(PathBuf, PathBuf),
    /// The real primitive starts deleting under them (`Stage::Deleting`).
    Deleting(PathBuf, PathBuf),
    Removed(PathBuf, Option<Tier>),
    Failed(PathBuf),
    Stopped(Option<u8>, u64),
}

fn levels() -> [(&'static str, u64); 4] {
    [
        ("MeasureOk", 500 * GIB_BYTES),
        ("MeasureLow", 20 * GIB_BYTES),
        ("MeasureShort", 6 * GIB_BYTES),
        ("MeasureEmergency", 2 * GIB_BYTES),
    ]
}

fn row(path: &str, witness: Witness) -> Row {
    Row {
        class: Class::CargoTargets,
        path: abs(path),
        bytes: 3 * GIB_BYTES,
        bytes_partial: false,
        witness,
        removable: true,
        blocked: None,
    }
}

fn gone(path: &str, last_used: i64) -> Row {
    row(
        path,
        Witness::OwnerGone {
            marker: "CACHEDIR.TAG".to_owned(),
            evidence: "a workspace that is gone".to_owned(),
            owner: discover::Owner::BuildCache {
                workspace: abs("/h/ws"),
                mirror: abs("/h/cache/h/ws"),
                home: Some(abs("/h")),
            },
            last_used,
            age_s: NOW - last_used,
            quiet_s: 600,
            floor_gib: 40,
            until_gib: 40,
            device: 1,
            projected: None,
        },
    )
}

fn idle(path: &str, idle_days: i64) -> Row {
    row(
        path,
        Witness::StaleBuildDir {
            marker: "CACHEDIR.TAG".to_owned(),
            profiles: 1,
            idle_days,
            threshold_days: 1,
            leaves: Vec::new(),
        },
    )
}

fn whole_idle(path: &str, last_used: i64) -> Row {
    row(
        path,
        Witness::IdleWhole {
            marker: "CACHEDIR.TAG".to_owned(),
            checkout: abs(path)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            profiles: 2,
            last_used,
            age_s: NOW - last_used,
            window_days: 7,
            device: 1,
        },
    )
}

fn recent(path: &str, last_used: i64) -> Row {
    row(
        path,
        Witness::LeastRecentlyUsed {
            marker: "CACHEDIR.TAG".to_owned(),
            build_dir: abs(path)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            last_used,
            age_s: NOW - last_used,
            threshold_days: 1,
            floor_gib: 10,
            until_gib: 15,
            device: 1,
            projected: None,
        },
    )
}

/// A projection standing at 2 MB/s, about 13.4 GiB over the horizon (§8.2).
fn projection(free: u64) -> disk::fill::Projection {
    let floor = Config::default().auto_free_gib * GIB_BYTES;
    disk::fill::Projection::of(2_000_000, free, floor, None)
}

fn report_at(free: u64, demand: Option<u64>) -> Report {
    report_projected(free, demand, None)
}

fn report_projected(
    free: u64,
    demand: Option<u64>,
    projected: Option<disk::fill::Projection>,
) -> Report {
    let config = Config::default();
    let rows = vec![
        gone("/h/cache/a/.cargo-build", NOW - 9 * 86_400),
        gone("/h/cache/refused/.cargo-build", NOW - 8 * 86_400),
        idle("/h/x/target", 9),
        idle("/h/y/target", 3),
        whole_idle("/h/v/target", NOW - 20 * 86_400),
        whole_idle("/h/refused-too/target", NOW - 9 * 86_400),
        recent("/h/z/target/debug", NOW - 7_200),
        recent("/h/w/target/debug", NOW - 3_600),
    ];
    let mut survey = disk::Survey::new(NOW);
    survey.free_bytes = Some(free);
    survey.trigger = Trigger::Tick;
    survey.demand = demand;
    survey.projected = projected;
    let mut rep = disk::report(&survey, config);
    rep.rows = rows;
    rep
}

fn word(free: u64, target: u64) -> &'static str {
    let t = Config::default().thresholds(None);
    if free >= target {
        "ReadAtTarget"
    } else if free < t.emergency {
        "ReadBelowEmergency"
    } else if free < t.floor {
        "ReadBelowFloor"
    } else {
        "ReadBelowTarget"
    }
}

fn step(m: &Model, s: &mut S, action: &str) -> Result<(), String> {
    if !m.action_enabled(action, s) {
        return Err(format!("`{action}` is not enabled in {s:?}"));
    }
    assert!(m.fire(action, s));
    for inv in &m.invariants {
        if !m.check_invariant(inv.name, s) {
            return Err(format!("`{}` broken after `{action}`: {s:?}", inv.name));
        }
    }
    Ok(())
}

fn looked(m: &Model, measure: &str, demand: bool) -> Result<S, String> {
    looked_projected(m, measure, demand, false)
}

fn looked_projected(m: &Model, measure: &str, demand: bool, projected: bool) -> Result<S, String> {
    let mut s = m.init_state();
    step(m, &mut s, measure)?;
    if demand {
        step(m, &mut s, "Adopt")?;
    }
    if projected {
        step(m, &mut s, "Project")?;
    }
    Ok(s)
}

fn project(m: &Model, s: &mut S, plan: &[Planned], events: &[Event]) -> Result<(), String> {
    let target_of = |tier: Option<Tier>| {
        plan.iter()
            .find(|p| p.row.tier() == tier)
            .and_then(|p| p.target)
    };
    let mut running: u8 = 0;
    let mut enter = |s: &mut S, tier: u8| -> Result<(), String> {
        if running < tier {
            step(m, s, &format!("Admit{tier}"))?;
            running = tier;
        }
        Ok(())
    };
    let mut pending: Option<u64> = None;
    let mut locked = false;
    let mut removed = false;
    let mut held: Vec<&PathBuf> = Vec::new();
    for e in events {
        match e {
            Event::Read(free) => pending = Some(*free),
            Event::Removing(_, tier) => {
                let n = tier.map_or(0, Tier::number);
                enter(s, n)?;
                if let Some(free) = pending.take() {
                    let target =
                        target_of(*tier).ok_or("a re-measure for a tier with no target")?;
                    step(m, s, word(free, target))?;
                }
                step(m, s, "Journal")?;
                (locked, removed) = (false, false);
                held.clear();
            }
            Event::Locked(_, profile) => {
                held.push(profile);
                if !locked {
                    step(m, s, "LockHeld")?;
                    locked = true;
                }
            }
            Event::Deleting(_, profile) => {
                // Each profile is deleted under its own locks, the first one
                // the unit's removal.
                if !held.contains(&profile) {
                    return Err(format!(
                        "{} is deleted without its locks held",
                        profile.display()
                    ));
                }
                if !removed {
                    step(m, s, "Remove")?;
                    removed = true;
                }
            }
            Event::Removed(..) => {
                if !removed {
                    step(m, s, "Remove")?;
                    removed = true;
                }
            }
            Event::Failed(_) => {
                step(m, s, "LockRefused")?;
            }
            Event::Stopped(tier, target) => {
                enter(s, tier.unwrap_or(0))?;
                let free = pending.take().ok_or("a stop with no re-measure")?;
                step(m, s, word(free, *target))?;
                step(m, s, "Stop")?;
            }
        }
    }
    Ok(())
}

fn lay(dir: &Path) -> PathBuf {
    for profile in ["debug", "release"] {
        let p = dir.join(profile);
        for d in [
            "deps",
            ".fingerprint/lay-2bcb77ac5af7c69c",
            "build",
            "incremental/s-1",
        ] {
            std::fs::create_dir_all(p.join(d)).expect("profile dirs");
        }
        for lock in disk::CARGO_LOCKS {
            std::fs::write(p.join(lock), b"").expect("lock");
        }
        std::fs::write(p.join("deps/liblay-0123456789abcdef.rlib"), vec![7u8; 4096]).expect("rlib");
        std::fs::write(p.join(".fingerprint/lay-2bcb77ac5af7c69c/lib-lay"), b"fp")
            .expect("fingerprint");
        std::fs::write(p.join("incremental/s-1/query-cache.bin"), vec![9u8; 4096])
            .expect("query cache");
    }
    std::fs::write(
        dir.join("CACHEDIR.TAG"),
        format!("{}\n# a cargo target directory\n", disk::CACHEDIR_SIGNATURE),
    )
    .expect("tag");
    std::fs::write(dir.join(".rustc_info.json"), b"{}").expect("rustc info");
    std::fs::canonicalize(dir).expect("canonical")
}

struct Scratch(
    PathBuf,
    #[expect(dead_code, reason = "RAII cleanup guard")] aterm_tempfile::TempDir,
);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-conformance-disk-{name}-"))
            .suffix(".noindex")
            .tempdir()
            .expect("scratch");
        Self(dir.path().to_path_buf(), dir)
    }
}

/// How a whole unit (tiers 1 and 3) is removed in a run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Whole {
    /// By the real primitive, on a build directory laid for it.
    Real,
    /// NEGATIVE CONTROL: its contents deleted first, the real primitive (and
    /// its locks) after.
    DeletesBeforeLocking,
}

fn run(rep: &Report, script: &[u64]) -> (Option<Applied>, Vec<Event>) {
    run_with(rep, script, Whole::Real)
}

fn run_with(rep: &Report, script: &[u64], whole: Whole) -> (Option<Applied>, Vec<Event>) {
    let events = RefCell::new(Vec::new());
    let reads = RefCell::new(0usize);
    let scratch = Scratch::new("whole");
    let laid = RefCell::new(0usize);
    let now =
        aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX)) + 8 * 86_400;
    let judge = disk::Judge {
        now,
        threshold_days: 1,
        device: None,
        volume: None,
    };
    let done = disk::apply_auto(
        rep,
        None,
        &mut |row: &Row| {
            if row.path.to_string_lossy().contains("refused") {
                return Err(std::io::Error::other(
                    "a build took a lock since the survey",
                ));
            }
            let tier = row.tier();
            if matches!(tier, Some(Tier::EndedOwners | Tier::IdleWhole)) {
                let dir = {
                    let mut n = laid.borrow_mut();
                    *n += 1;
                    lay(&scratch.0.join(format!("unit-{n}/target")))
                };
                if whole != Whole::Real {
                    let first = dir.join("debug");
                    events
                        .borrow_mut()
                        .push(Event::Deleting(row.path.clone(), first.clone()));
                    std::fs::remove_dir_all(first.join("deps")).expect("deleted first");
                }
                let mut hook = |stage: disk::target::Stage, at: &Path| {
                    let e = match stage {
                        disk::target::Stage::Locked => {
                            Event::Locked(row.path.clone(), at.to_path_buf())
                        }
                        disk::target::Stage::Deleting => {
                            Event::Deleting(row.path.clone(), at.to_path_buf())
                        }
                        disk::target::Stage::Released => return,
                    };
                    events.borrow_mut().push(e);
                };
                return if tier == Some(Tier::EndedOwners) {
                    disk::target::reclaim_whole_with(
                        &dir,
                        &judge,
                        now,
                        600,
                        &mut || Ok(()),
                        &mut hook,
                    )
                } else {
                    disk::target::reclaim_profiles_with(
                        &dir,
                        &judge,
                        now,
                        7 * 86_400,
                        &mut || Ok(()),
                        &mut || Ok(()),
                        &mut hook,
                    )
                };
            }
            Ok(Removed {
                bytes: row.bytes,
                units: vec![row.path.clone()],
                ..Removed::default()
            })
        },
        &mut || {
            let mut n = reads.borrow_mut();
            let got = script.get(*n).or(script.last()).copied();
            *n += 1;
            events.borrow_mut().push(Event::Read(got.unwrap_or(0)));
            got
        },
        &mut |s: &Step<'_>| {
            let e = match s {
                Step::Removing(r) => Event::Removing(r.path.clone(), r.tier()),
                Step::Removed(r, _) => Event::Removed(r.path.clone(), r.tier()),
                Step::Denied(Refusal::RemoveFailed(p, _)) => Event::Failed(p.clone()),
                Step::Denied(other) => panic!("an unexpected refusal: {other:?}"),
                Step::Stopped(stop) => Event::Stopped(stop.tier, stop.target),
            };
            events.borrow_mut().push(e);
            true
        },
    );
    (done, events.into_inner())
}

#[test]
fn the_real_pass_takes_only_the_models_steps_at_every_level() {
    let m = disk_reclaim_pass_model();
    let need = 30 * GIB_BYTES;
    let scripts: [&[u64]; 4] = [
        &[GIB_BYTES],
        &[20 * GIB_BYTES, 45 * GIB_BYTES],
        &[3 * GIB_BYTES, 9 * GIB_BYTES, 16 * GIB_BYTES, 60 * GIB_BYTES],
        &[500 * GIB_BYTES],
    ];
    let mut passes = 0;
    for (measure, free) in levels() {
        for (demand, projected) in [
            (None, None),
            (Some(need), None),
            (None, Some(projection(free))),
            (Some(need), Some(projection(free))),
        ] {
            let rep = report_projected(free, demand, projected);
            let start = looked_projected(&m, measure, demand.is_some(), projected.is_some())
                .expect("the look");
            let plan = disk::pass_plan(&rep).unwrap_or_default();
            for tier in [
                Tier::EndedOwners,
                Tier::IdleIncremental,
                Tier::IdleWhole,
                Tier::RecentIncremental,
            ] {
                let has_rows = rep.rows.iter().any(|r| r.tier() == Some(tier));
                if !has_rows {
                    continue;
                }
                let planned = plan.iter().any(|p| p.row.tier() == Some(tier));
                let admits = m.action_enabled(&format!("Admit{}", tier.number()), &start);
                assert_eq!(
                    planned,
                    admits,
                    "{measure} demand={demand:?} projected={}: tier {} planned={planned}, the model admits={admits}",
                    projected.is_some(),
                    tier.number()
                );
            }
            for script in scripts {
                let (done, events) = run(&rep, script);
                let mut s = start.clone();
                project(&m, &mut s, &plan, &events).unwrap_or_else(|why| {
                    panic!(
                        "{measure} demand={demand:?} projected={} script={script:?}: {why}\n{events:#?}",
                        projected.is_some()
                    )
                });
                if let Some(done) = done {
                    passes += 1;
                    assert!(
                        done.removed
                            .iter()
                            .all(|(r, _)| r.tier() != Some(Tier::RecentIncremental))
                            || measure == "MeasureShort"
                            || measure == "MeasureEmergency"
                            || projected.is_some(),
                        "{measure}: tier 4 above the floor with no projection"
                    );
                }
            }
        }
    }
    assert!(passes >= 24, "the passes ran: {passes}");
}

#[test]
fn under_a_projection_each_tier_stops_where_the_fill_no_longer_crosses() {
    let (_, free) = levels()[1];
    let p = projection(free);
    let plan = disk::pass_plan(&report_projected(free, None, Some(p))).expect("pressed");
    let c = Config::default();
    let horizon = c.projected_target(&p);
    assert_eq!(horizon, c.pressure_target_bytes() + p.need);
    for planned in &plan {
        let line = match planned.row.tier() {
            Some(Tier::EndedOwners) => None,
            Some(Tier::IdleIncremental | Tier::IdleWhole) => Some(c.idle_target_bytes(true)),
            _ => Some(c.pressure_target_bytes()),
        };
        assert_eq!(
            planned.target,
            line.map(|l| l.max(horizon)),
            "the line each tier stops at under pressure, raised to the horizon's: {planned:?}"
        );
    }
    assert!(
        plan.iter()
            .any(|p| p.row.tier() == Some(Tier::RecentIncremental)),
        "a projection at low runs tier 4"
    );
}

#[test]
fn the_reserve_is_released_exactly_where_the_model_lets_it_go() {
    use aterm_agent::harness::cli::{ReserveMove, disk_reserve_look};
    let m = disk_reclaim_pass_model();
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-conformance-disk-reserve-")
        .suffix(".noindex")
        .tempdir()
        .expect("scratch");
    let state = tmp.path().join("state");
    let t = Config::default().thresholds(None);
    for (measure, free, level) in [
        ("MeasureLow", 20 * GIB_BYTES, disk::Level::Low),
        ("MeasureShort", 6 * GIB_BYTES, disk::Level::Short),
        ("MeasureEmergency", 2 * GIB_BYTES, disk::Level::Emergency),
    ] {
        let reserve = tmp.path().join(format!("{measure}/reserve"));
        std::fs::create_dir_all(reserve.parent().expect("folder")).expect("folder");
        std::fs::write(&reserve, vec![7u8; 4096]).expect("a small reserve");
        let moved = disk_reserve_look(&state, &reserve, level, Some(free), &t, NOW);
        let released = matches!(moved, ReserveMove::Released(_));
        // A projection is a forecast: the look that projects a crossing reads the reserve by its
        // measured level alone, so the model's look projects too.
        let mut s = looked_projected(&m, measure, false, true).expect("the look");
        assert_eq!(
            released,
            m.action_enabled("ReleaseReserve", &s),
            "{measure}: {moved:?}"
        );
        if released {
            step(&m, &mut s, "ReleaseReserve").expect("conforms");
            assert!(!reserve.exists());
        } else {
            assert!(reserve.exists());
        }
    }
}

#[test]
fn a_planner_that_takes_recent_caches_for_a_demand_is_rejected() {
    let m = disk_reclaim_pass_model();
    let (measure, free) = levels()[1];
    let rep = report_at(free, Some(30 * GIB_BYTES));
    let real = disk::pass_plan(&rep).expect("a demand admits tiers 1-3");
    assert!(
        real.iter()
            .all(|p| p.row.tier() != Some(Tier::RecentIncremental))
    );
    let mut buggy = real.clone();
    buggy.push(Planned {
        row: recent("/h/z/target/debug", NOW - 7_200),
        target: Some(15 * GIB_BYTES),
    });
    let projected = |plan: &[Planned]| {
        let mut s = looked(&m, measure, true)?;
        let events: Vec<Event> = plan
            .iter()
            .flat_map(|p| {
                let mut e = Vec::new();
                if p.target.is_some() {
                    e.push(Event::Read(free));
                }
                e.push(Event::Removing(p.row.path.clone(), p.row.tier()));
                if matches!(p.row.tier(), Some(Tier::EndedOwners | Tier::IdleWhole)) {
                    e.push(Event::Locked(p.row.path.clone(), p.row.path.clone()));
                    e.push(Event::Deleting(p.row.path.clone(), p.row.path.clone()));
                }
                e.push(Event::Removed(p.row.path.clone(), p.row.tier()));
                e
            })
            .filter(|e| !matches!(e, Event::Removing(p, _) | Event::Locked(p, _) | Event::Deleting(p, _) | Event::Removed(p, _) if p.to_string_lossy().contains("refused")))
            .collect();
        project(&m, &mut s, plan, &events)
    };
    projected(&real).expect("the real plan conforms");
    let caught = projected(&buggy).expect_err("the checker must reject tier 4 for a demand");
    assert!(caught.contains("Admit4"), "{caught}");
}

#[test]
fn a_planner_that_takes_recent_caches_at_low_with_no_projection_is_rejected() {
    let m = disk_reclaim_pass_model();
    let (measure, free) = levels()[1];
    let p = projection(free);
    let real = disk::pass_plan(&report_projected(free, None, Some(p))).expect("pressed");
    let events = |plan: &[Planned]| -> Vec<Event> {
        plan.iter()
            .filter(|p| !p.row.path.to_string_lossy().contains("refused"))
            .flat_map(|p| {
                let mut e = Vec::new();
                if p.target.is_some() {
                    e.push(Event::Read(free));
                }
                e.push(Event::Removing(p.row.path.clone(), p.row.tier()));
                if matches!(p.row.tier(), Some(Tier::EndedOwners | Tier::IdleWhole)) {
                    e.push(Event::Locked(p.row.path.clone(), p.row.path.clone()));
                    e.push(Event::Deleting(p.row.path.clone(), p.row.path.clone()));
                }
                e.push(Event::Removed(p.row.path.clone(), p.row.tier()));
                e
            })
            .collect()
    };
    let mut s = looked_projected(&m, measure, false, true).expect("the look");
    project(&m, &mut s, &real, &events(&real)).expect("the projected plan conforms");
    // NEGATIVE CONTROL: the same plan run at a look that projected nothing, as a planner that
    // read `low` as pressure would run it.
    let mut s = looked(&m, measure, false).expect("the look");
    let caught = project(&m, &mut s, &real, &events(&real))
        .expect_err("the checker must reject tiers 3 and 4 at low with no projection");
    assert!(
        caught.contains("Admit3") || caught.contains("Admit4"),
        "{caught}"
    );
}

#[test]
fn a_whole_removal_that_deletes_before_it_locks_is_rejected() {
    let m = disk_reclaim_pass_model();
    let (measure, free) = levels()[2];
    let rep = report_at(free, None);
    let plan = disk::pass_plan(&rep).expect("short admits tiers 1-4");
    let script: &[u64] = &[3 * GIB_BYTES];
    let (_, events) = run(&rep, script);
    assert!(
        events.iter().any(|e| matches!(e, Event::Locked(..))),
        "the real primitives told their locks: {events:#?}"
    );
    let mut s = looked(&m, measure, false).expect("the look");
    project(&m, &mut s, &plan, &events)
        .unwrap_or_else(|why| panic!("the real removals conform: {why}\n{events:#?}"));
    let (_, events) = run_with(&rep, script, Whole::DeletesBeforeLocking);
    let mut s = looked(&m, measure, false).expect("the look");
    let caught = project(&m, &mut s, &plan, &events)
        .expect_err("the checker must reject a delete before the locks");
    assert!(
        caught.contains("Remove") || caught.contains("without its locks"),
        "{caught}"
    );
}

#[test]
fn a_pass_that_breaks_any_other_invariant_is_rejected() {
    let m = disk_reclaim_pass_model();
    let (measure, free) = levels()[1];
    let rep = report_at(free, None);
    let real = disk::pass_plan(&rep).expect("low admits tiers 1-2");
    assert!(real.iter().all(|p| p.row.tier() != Some(Tier::IdleWhole)));
    let whole = whole_idle("/h/v/target", NOW - 20 * 86_400);
    let mut buggy = real.clone();
    buggy.push(Planned {
        row: whole.clone(),
        target: Some(45 * GIB_BYTES),
    });
    let events = vec![
        Event::Read(free),
        Event::Removing(whole.path.clone(), whole.tier()),
        Event::Locked(whole.path.clone(), whole.path.clone()),
        Event::Deleting(whole.path.clone(), whole.path.clone()),
        Event::Removed(whole.path.clone(), whole.tier()),
    ];
    let mut s = looked(&m, measure, false).expect("the look");
    let caught = project(&m, &mut s, &buggy, &events).expect_err("tier 3 at low");
    assert!(caught.contains("Admit3"), "{caught}");

    let idle_row = idle("/h/x/target", 9);
    let target = real
        .iter()
        .find(|p| p.row.tier() == Some(Tier::IdleIncremental))
        .and_then(|p| p.target)
        .expect("tier 2's target");
    let past = vec![
        Event::Read(target),
        Event::Removing(idle_row.path.clone(), idle_row.tier()),
        Event::Removed(idle_row.path.clone(), idle_row.tier()),
    ];
    let mut s = looked(&m, measure, false).expect("the look");
    let caught = project(&m, &mut s, &real, &past).expect_err("a row past its target");
    assert!(caught.contains("Journal"), "{caught}");

    let s = looked(&m, "MeasureShort", false).expect("the look");
    assert!(
        !m.action_enabled("ReleaseReserve", &s),
        "a reserve released at `short` is one the model refuses"
    );
}

/// The model's state after `trace`, each action [`step`]ped from its start.
fn replay(m: &Model, trace: &[&str]) -> Result<S, String> {
    let mut s = m.init_state();
    for action in trace {
        step(m, &mut s, action)?;
    }
    Ok(s)
}

/// A budget page as the model reads it: the volume read over and the pass admitted, each
/// removal a `Take` (`TakeLive` where it is under `live`), and how it ended.
fn budget_trace(done: &Applied, live: &Path, end: &disk::budget::End) -> Vec<&'static str> {
    let mut trace = vec!["ReadOver", "Admit"];
    for (row, _) in &done.removed {
        trace.push(if row.path.starts_with(live) {
            "TakeLive"
        } else {
            "Take"
        });
    }
    trace.push(match end {
        disk::budget::End::Met => "Met",
        disk::budget::End::StoodDown(_) | disk::budget::End::Unfinished => "StandDown",
        disk::budget::End::CannotMeet { .. } => "CannotMeet",
    });
    trace
}

/// What a host's budget page runs against: the sizing, the cap in GiB and the live sessions.
struct Against<'a> {
    sizing: &'a disk::budget::Sizing,
    cap: u64,
    live: &'a disk::budget::Live,
}

/// One budget page as a host runs it at `now`, its state in `dir`, over a volume over its cap
/// with every row it may take already taken (`takes` of them, as the model counts them), as the
/// model reads it: nothing where the memo plans none ([`disk::budget::plans`], the host's gate),
/// else `Admit`, a `Take` for each of `takes`, and how the real page ended
/// ([`cli::disk_tick_budget`]): `CannotMeet` where it wrote its `cannot-meet` row,
/// `Exhausted` where it did not. Whether a page was planned.
fn memo_pass(
    dir: &Path,
    against: &Against<'_>,
    now: i64,
    takes: usize,
    trace: &mut Vec<&str>,
) -> bool {
    let config = Config {
        cap_used_gib: disk::Cap::Written(against.cap),
        ..Config::default()
    };
    let (sizing, live) = (against.sizing, against.live);
    let free = 100 * GIB_BYTES;
    let over = disk::budget::over(sizing, config.cap_used_gib, &sizing.volume, free)
        .expect("the volume reads over its cap");
    let partial = sizing.sized.partial;
    let inputs = disk::budget::Inputs::of(sizing, &over.budget, live.digest(), partial);
    if !disk::budget::plans(disk::budget::load(dir).as_ref(), &inputs, now) {
        return false;
    }
    trace.push("Admit");
    trace.extend(std::iter::repeat_n("Take", takes));
    std::fs::create_dir_all(dir).expect("the state directory");
    let look = cli::DiskLook {
        state: dir,
        volume: dir,
        free: Some(free),
        targets: &[],
        discover: None,
        transcripts: None,
        now,
        room: None,
        table: None,
        demand: None,
        projected: None,
        session_cwds: &[],
        session_cwd_unknown: None,
        link_dirs: &[],
    };
    let page = cli::disk_tick_budget(
        &look,
        config,
        &cli::BudgetLook {
            sizing,
            over,
            live,
            page: 1,
        },
        &mut || Ok(()),
        &mut |row: &Row| {
            panic!(
                "nothing is left to take, and it took {}",
                row.path.display()
            )
        },
        &mut || Some(free),
        &mut discover::Cursor::new(),
    );
    trace.push(match page.end {
        cli::BudgetEnd::CannotMeet { row: true, .. } => "CannotMeet",
        cli::BudgetEnd::CannotMeet { row: false, .. } => "Exhausted",
        cli::BudgetEnd::Met => "Met",
        cli::BudgetEnd::StoodDown(_) | cli::BudgetEnd::Unfinished => "StandDown",
    });
    true
}

/// The `cannot-meet` rows in `dir`'s disk ledger.
fn cannot_meet_rows(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join(cli::DISK_LEDGER))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter(|row| {
            row.get("kind").and_then(aterm_json::Value::as_str) == Some(disk::CANNOT_MEET_ROW_KIND)
        })
        .count()
}

/// THE BUDGET PASS'S THREE LAWS: it takes nothing a live session's builds keep
/// (`BudgetTakesNoLive`), writes one `cannot-meet` row an episode (`CannotMeetOnce`), and
/// strands no row a hold released: a day after the episode's last pass one is planned
/// (`NothingStranded`).
///
/// - The model holds every law, and each mutant breaks its own: one that takes a live session's
///   row breaks the first; one that writes its row again, and one whose memo is forgotten at a
///   restart, break the second; one that lets the day pass with no pass planned breaks the third.
/// - The real scan, ask and pass over two idle build directories and one a session works in
///   take the two and say the target cannot be met: the model's steps. A refused removal leaves
///   its row for a later page: the page is unfinished, which the model reads as a stand-down.
///   NEGATIVE CONTROLS: an ask that makes every row eligible takes the live one, which the model
///   refuses, and its live-taking mutant fires it and breaks `BudgetTakesNoLive`; the refused
///   page read as though nothing were refused says it cannot meet the target with a row left,
///   which the model refuses.
/// - The real page and memo, on disk across a restart, plan a page exactly where the model
///   admits one, write the row once (the ledger holds one a `CannotMeet`), and plan again when
///   the live sessions change, the rebuildable bytes grow, a day passes or the cap moves.
///   NEGATIVE CONTROLS: a memo lost at the restart writes a second row, which the model refuses
///   and its forgetting mutant breaks `CannotMeetOnce` on; a gate whose clock does not move
///   plans nothing a day on, which the model refuses and its time-blind mutant breaks
///   `NothingStranded` on.
#[test]
fn the_budget_laws_hold_and_each_mutant_breaks_its_own() {
    use aterm_spec::interp::{violated_invariants, with_buggy};
    use std::collections::BTreeSet;

    let m = disk_budget_pass_model();
    assert!(violated_invariants(&m).is_empty());
    assert_eq!(
        violated_invariants(&with_buggy(&m, 1)),
        BTreeSet::from(["BudgetTakesNoLive"])
    );
    for buggy in [2, 3] {
        assert_eq!(
            violated_invariants(&with_buggy(&m, buggy)),
            BTreeSet::from(["CannotMeetOnce"]),
            "Buggy {buggy}"
        );
    }
    assert_eq!(
        violated_invariants(&with_buggy(&m, 4)),
        BTreeSet::from(["NothingStranded"])
    );

    // The real eligibility and plan.
    let scratch = Scratch::new("budget");
    let home = scratch.0.join("home");
    let [a, b, live] = ["a", "b", "live"].map(|name| lay(&home.join(name).join("target")));
    let live_checkout = live.parent().expect("its checkout").to_path_buf();
    let now =
        aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX)) + 3 * 86_400;
    let config = Config::default();
    let (free, free_target) = (100 * GIB_BYTES, 200 * GIB_BYTES);
    let roots = disk::Roots {
        volume: Some(scratch.0.clone()),
        targets: vec![a.clone(), b.clone(), live.clone()],
        budget: Some(free_target),
        ..disk::Roots::default()
    };
    let survey = disk::scan(&roots, now, Trigger::Tick, config, Some(free));
    let sessions = disk::budget::Live {
        cwds: vec![live_checkout.clone()],
        ..disk::budget::Live::default()
    };
    // The volume table every host's look reads with: without one no session's folder is
    // resolved, and on Windows a session whose folder and volume are unplaced may work in any
    // row, so every row is held.
    let table = aterm_sysprobe::volumes::table();
    let ask = disk::budget::ask(&survey, &sessions, free_target, Some(&table), now);
    assert_eq!(
        ask.eligible,
        BTreeSet::from([a.clone(), b.clone()]),
        "held: {:?}",
        ask.held
    );
    let pass_refusing = |ask: disk::budget::Ask, refused: Option<&Path>| -> Applied {
        let mut rep = disk::report(&survey, config);
        rep.budget = Some(ask);
        disk::apply_auto(
            &rep,
            None,
            &mut |row: &Row| {
                if refused == Some(row.path.as_path()) {
                    return Err(std::io::Error::other("a build holds its lock"));
                }
                Ok(Removed {
                    bytes: row.bytes,
                    units: vec![row.path.clone()],
                    ..Removed::default()
                })
            },
            &mut || Some(free),
            &mut |_: &Step<'_>| true,
        )
        .expect("a budget report plans its pass")
    };
    let pass = |ask: disk::budget::Ask| pass_refusing(ask, None);
    let end_of = |done: &Applied| disk::budget::end_of(done, Some(free), free_target, 1, false);
    let done = pass(ask.clone());
    let end = end_of(&done);
    assert_eq!(end, disk::budget::End::CannotMeet { partial: false });
    replay(&m, &budget_trace(&done, &live_checkout, &end))
        .unwrap_or_else(|why| panic!("the real pass conforms: {why}"));

    // A refused removal: the page is unfinished, a stand-down with that row still eligible.
    let done = pass_refusing(ask.clone(), Some(a.as_path()));
    assert_eq!((done.removed.len(), done.denials.len()), (1, 1), "{done:?}");
    let end = end_of(&done);
    assert_eq!(end, disk::budget::End::Unfinished);
    let trace = budget_trace(&done, &live_checkout, &end);
    assert_eq!(trace, ["ReadOver", "Admit", "Take", "StandDown"]);
    replay(&m, &trace).unwrap_or_else(|why| panic!("the refused page conforms: {why}"));
    // NEGATIVE CONTROL: read as though nothing were refused, it says so with a row left.
    let unrefused = Applied {
        denials: Vec::new(),
        ..done
    };
    let trace = budget_trace(&unrefused, &live_checkout, &end_of(&unrefused));
    let caught = replay(&m, &trace).expect_err("the model says it only with nothing left");
    assert!(caught.contains("`CannotMeet` is not enabled"), "{caught}");

    // NEGATIVE CONTROL: an ask that keeps nothing for a live session.
    let careless = disk::budget::Ask {
        eligible: BTreeSet::from([a, b, live]),
        held: Vec::new(),
        ..ask
    };
    let done = pass(careless);
    let trace = budget_trace(&done, &live_checkout, &end_of(&done));
    assert!(trace.contains(&"TakeLive"), "{trace:?}");
    let caught = replay(&m, &trace).expect_err("the model refuses a live row");
    assert!(caught.contains("`TakeLive` is not enabled"), "{caught}");
    let caught = replay(&with_buggy(&m, 1), &trace).expect_err("its mutant breaks the law");
    assert!(caught.contains("BudgetTakesNoLive"), "{caught}");

    // The real page and memo, in a scratch state directory, as a host runs them.
    let sizing = disk::budget::Sizing {
        volume: "v".to_owned(),
        total: 1000 * GIB_BYTES,
        used: 900 * GIB_BYTES,
        sized: disk::Rebuildable {
            bytes: 50 * GIB_BYTES,
            partial: false,
        },
        sized_unix: NOW,
    };
    let grown = disk::budget::Sizing {
        sized: disk::Rebuildable {
            bytes: 60 * GIB_BYTES,
            partial: false,
        },
        ..sizing.clone()
    };
    let nobody = disk::budget::Live::default();
    let one = Against {
        sizing: &sizing,
        cap: 800,
        live: &sessions,
    };
    let ended = Against {
        live: &nobody,
        ..one
    };
    let moved = Against { cap: 700, ..ended };
    let more = Against {
        sizing: &grown,
        ..moved
    };
    let day = NOW + disk::budget::REPLAN_AFTER_S;
    // Each page planned where, and only where, the model admits one.
    let page = |dir: &Path, against: &Against<'_>, now: i64, takes, trace: &mut Vec<&str>| {
        let s = replay(&m, trace).expect("the trace so far");
        let admits = m.action_enabled("Admit", &s);
        assert_eq!(
            memo_pass(dir, against, now, takes, trace),
            admits,
            "{trace:?}"
        );
    };
    let dir = scratch.0.join("state");
    let mut trace = vec!["ReadOver"];
    page(&dir, &one, NOW, 2, &mut trace);
    // A restart: nothing but the memo on disk is read again.
    page(&dir, &one, NOW, 0, &mut trace);
    trace.push("SessionEnds");
    page(&dir, &ended, NOW, 1, &mut trace);
    page(&dir, &ended, NOW, 0, &mut trace);
    // A hold lapses with nothing the memo compares changed: nothing until a day has passed.
    trace.push("Ages");
    page(&dir, &ended, day - 1, 0, &mut trace);
    trace.push("DayPasses");
    page(&dir, &ended, day, 1, &mut trace);
    trace.push("CapMoves");
    page(&dir, &moved, day, 0, &mut trace);
    trace.push("Replans");
    page(&dir, &more, day, 0, &mut trace);
    page(&dir, &more, day, 0, &mut trace);
    assert_eq!(
        trace,
        [
            "ReadOver",
            "Admit",
            "Take",
            "Take",
            "CannotMeet",
            "SessionEnds",
            "Admit",
            "Take",
            "Exhausted",
            "Ages",
            "DayPasses",
            "Admit",
            "Take",
            "Exhausted",
            "CapMoves",
            "Admit",
            "CannotMeet",
            "Replans",
            "Admit",
            "Exhausted",
        ]
    );
    replay(&m, &trace).unwrap_or_else(|why| panic!("the real memo conforms: {why}"));
    let said = trace.iter().filter(|a| **a == "CannotMeet").count();
    assert_eq!(cannot_meet_rows(&dir), said, "one row a CannotMeet");

    // NEGATIVE CONTROL: a memo lost at a restart (kept in memory, say) writes a second row.
    let lost = scratch.0.join("lost");
    let mut trace = vec!["ReadOver"];
    memo_pass(&lost, &one, NOW, 2, &mut trace);
    std::fs::remove_file(lost.join(disk::budget::UNMET_FILE)).expect("the memo");
    let restart = trace.len();
    memo_pass(&lost, &one, NOW, 0, &mut trace);
    assert_eq!(
        trace.iter().filter(|a| **a == "CannotMeet").count(),
        2,
        "{trace:?}"
    );
    assert_eq!(cannot_meet_rows(&lost), 2);
    let caught = replay(&m, &trace).expect_err("the model writes one row");
    assert!(caught.contains("is not enabled"), "{caught}");
    let mut forgets = trace.clone();
    forgets.insert(restart, "RestartForgets");
    let caught = replay(&with_buggy(&m, 3), &forgets).expect_err("its mutant breaks the law");
    assert!(caught.contains("CannotMeetOnce"), "{caught}");

    // NEGATIVE CONTROL: a gate whose clock does not move plans nothing a day on, and the row a
    // lapsed hold left stays.
    let stuck = scratch.0.join("stuck");
    let mut trace = vec!["ReadOver"];
    memo_pass(&stuck, &ended, NOW, 2, &mut trace);
    trace.push("Ages");
    assert!(!memo_pass(&stuck, &ended, NOW, 0, &mut trace), "{trace:?}");
    trace.push("DayPassesBlind");
    let caught = replay(&m, &trace).expect_err("the model plans a page a day on");
    assert!(
        caught.contains("`DayPassesBlind` is not enabled"),
        "{caught}"
    );
    let caught = replay(&with_buggy(&m, 4), &trace).expect_err("its mutant breaks the law");
    assert!(caught.contains("NothingStranded"), "{caught}");
}
