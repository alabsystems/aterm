// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests for `DiskReclaimPass` against `disk::pass_plan` and `disk::apply_auto`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use aterm_agent::harness::disk::{
    self, Applied, Class, Config, Planned, Refusal, Removed, Report, Row, Step, Tier, Trigger,
    Witness, discover,
};
use aterm_models_harness::disk_reclaim_pass_model;
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

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "aterm-conformance-disk-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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
    let tmp = std::env::temp_dir().join(format!(
        "aterm-conformance-disk-reserve-{}",
        std::process::id()
    ));
    let state = tmp.join("state");
    let t = Config::default().thresholds(None);
    for (measure, free, level) in [
        ("MeasureLow", 20 * GIB_BYTES, disk::Level::Low),
        ("MeasureShort", 6 * GIB_BYTES, disk::Level::Short),
        ("MeasureEmergency", 2 * GIB_BYTES, disk::Level::Emergency),
    ] {
        let reserve = tmp.join(format!("{measure}/reserve"));
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
    let _ = std::fs::remove_dir_all(&tmp);
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
