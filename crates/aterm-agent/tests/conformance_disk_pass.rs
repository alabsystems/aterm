// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `DiskReclaimPass` (`aterm_models_harness::disk_reclaim_pass_model`):
//! the real tier planner (`disk::pass_plan`) and the real pass
//! (`disk::apply_auto`), driven with scripted re-measures and removals on
//! synthetic reports at every measured level, with and without an adopted
//! demand. Every step the real pass takes — the tier it enters, each re-measure
//! against its tier's target, each intent row, each lock and removal, each stop
//! — is projected onto the model and must be an ENABLED action there, every
//! invariant holding after it. And the planner is held to the model's
//! admission both ways: a tier has rows in the plan exactly when the model
//! admits it at that level.
//!
//! The reserve is bound too: the real `disk_reserve_look` releases it exactly
//! when the model lets `ReleaseReserve` fire.
//!
//! The report holds rows of every tier at every level (the planner's own
//! admission is what is held), tier 3's among them. A WHOLE unit (tiers 1 and
//! 3) is removed by the REAL primitive (`target::reclaim_whole_with`,
//! `target::reclaim_profiles_with`) on a cargo build directory laid for it,
//! and its locks are projected from the primitive's own `Stage::Locked`
//! (`LockHeld`) and its delete from `Stage::Deleting` (`Remove`): the model's
//! `WholeOnlyHeld` is held to what the real removal does, never inserted by
//! the test.
//!
//! Negative controls: a planner that admits tier 4 for a demand at a measured
//! `low` (the defect `AdmitRecentOnDemand` stands for), and a whole removal
//! that deletes before it locks (`RemoveUnheld`), are projected the same way,
//! and the checker must reject each — so a checker that accepts everything
//! fails this test.

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

/// Bytes in a GiB.
const GIB_BYTES: u64 = 1 << 30;

/// `posix`, a fixture's absolute POSIX path (`/h/x/target`), as one this
/// host reads as absolute — a removal's guard refuses any other: itself on
/// Unix, the same path on drive `C:` on Windows.
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

/// One measured level: `(model action, free bytes the look read)` for the
/// default config's lines (warn 40 GiB, floor 10 GiB, emergency 4 GiB).
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
        },
    )
}

/// A report at `free` bytes, `demand` adopted, holding rows of EVERY tier
/// whatever the level (the real report makes an ended owner's whole row only
/// from `low` and recent profiles' only below the floor; here the planner's
/// own admission is what is held to the model), one of the whole rows a
/// removal refuses.
fn report_at(free: u64, demand: Option<u64>) -> Report {
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
    let mut rep = disk::report(&survey, config);
    rep.rows = rows;
    rep
}

/// The model's free-space word for `free` against a tier's `target`.
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

/// Fire `action` on `s`, which must be enabled, every invariant holding after.
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

/// The model state after the look: its level measured, a demand adopted.
fn looked(m: &Model, measure: &str, demand: bool) -> Result<S, String> {
    let mut s = m.init_state();
    step(m, &mut s, measure)?;
    if demand {
        step(m, &mut s, "Adopt")?;
    }
    Ok(s)
}

/// THE PROJECTION: replay `events` (what the real pass did, in order) onto `s`,
/// each tier entered when its first event comes, each re-measure classified
/// against the target of the tier the next event names.
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
    // The unit in flight: its locks seen held, its removal begun, and each
    // profile whose locks the primitive took.
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
                // A whole unit the remover refused: its locks, or its recheck.
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

/// A cargo build directory at `dir`, as cargo lays one: a signed
/// `CACHEDIR.TAG`, `.rustc_info.json`, and two profiles (`debug/`,
/// `release/`), each with every build lock, `.fingerprint/`, `deps/` (an
/// rlib), `build/` and `incremental/`. Answered canonical (macOS's temp dir
/// sits behind the `/var` link, and the primitives reach nothing through a
/// link).
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

/// A scratch folder of this test's own, removed when it drops.
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
    #[cfg(unix)]
    DeletesBeforeLocking,
}

/// Run the real pass on `rep`, re-measures answering `script` in turn (its
/// last figure after that); a whole unit removed by the real primitive on a
/// build directory laid for it (`whole`), its stages told as events; every
/// other removal releasing what its row says; the `refused` rows refused
/// before any lock: what it did, as events.
fn run(rep: &Report, script: &[u64]) -> (Option<Applied>, Vec<Event>) {
    run_with(rep, script, Whole::Real)
}

/// [`run`], whole units removed as `whole` says.
fn run_with(rep: &Report, script: &[u64], whole: Whole) -> (Option<Applied>, Vec<Event>) {
    let events = RefCell::new(Vec::new());
    let reads = RefCell::new(0usize);
    let scratch = Scratch::new("whole");
    let laid = RefCell::new(0usize);
    // Eight days after the fixtures are written: every profile idle, past
    // tier 3's seven-day window and tier 1's quiet one.
    let now =
        aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX)) + 8 * 86_400;
    let judge = disk::Judge {
        now,
        threshold_days: 1,
        device: None,
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

/// EVERY LEVEL, WITH AND WITHOUT A DEMAND, EACH OF SEVERAL RE-MEASURE SCRIPTS:
/// the real planner admits exactly the tiers the model admits, and every step
/// the real pass takes is one the model enables, its invariants holding after.
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
        for demand in [None, Some(need)] {
            let rep = report_at(free, demand);
            let start = looked(&m, measure, demand.is_some()).expect("the look");
            // Admission, both ways.
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
                    "{measure} demand={demand:?}: tier {} planned={planned}, the model admits={admits}",
                    tier.number()
                );
            }
            for script in scripts {
                let (done, events) = run(&rep, script);
                let mut s = start.clone();
                project(&m, &mut s, &plan, &events).unwrap_or_else(|why| {
                    panic!("{measure} demand={demand:?} script={script:?}: {why}\n{events:#?}")
                });
                if let Some(done) = done {
                    passes += 1;
                    assert!(
                        done.removed
                            .iter()
                            .all(|(r, _)| r.tier() != Some(Tier::RecentIncremental))
                            || measure == "MeasureShort"
                            || measure == "MeasureEmergency",
                        "{measure}: tier 4 above the floor"
                    );
                }
            }
        }
    }
    assert!(passes >= 12, "the passes ran: {passes}");
}

/// THE RESERVE: the real look releases it at a measured emergency and at no
/// other level, exactly where the model lets `ReleaseReserve` fire. (Levels at
/// or above the floor would write the reserve, which needs room this test does
/// not take; below it nothing is written.)
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
        ("MeasureShort", 6 * GIB_BYTES, disk::Level::Short),
        ("MeasureEmergency", 2 * GIB_BYTES, disk::Level::Emergency),
    ] {
        let reserve = tmp.join(format!("{measure}/reserve"));
        std::fs::create_dir_all(reserve.parent().expect("folder")).expect("folder");
        std::fs::write(&reserve, vec![7u8; 4096]).expect("a small reserve");
        let moved = disk_reserve_look(&state, &reserve, level, Some(free), &t, NOW);
        let released = matches!(moved, ReserveMove::Released(_));
        let mut s = looked(&m, measure, false).expect("the look");
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

/// NEGATIVE CONTROL: a planner that admits tier 4 for a demand at a measured
/// `low` — recent caches taken for an agent's wait — is caught: projected the
/// same way, its first recent row's tier is one the model does not admit. And
/// the same projection of the real planner's plan at that level passes.
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
                // A whole unit's locks, as its primitive tells them (this
                // control is about the planner).
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

/// NEGATIVE CONTROL for the locks: a whole removal that deletes a profile's
/// contents BEFORE the real primitive takes its locks (the defect
/// `RemoveUnheld` stands for) is caught — its first delete projects onto
/// `Remove` with no `LockHeld` before it. And the same report through the
/// real primitive alone conforms, every whole unit's `LockHeld` read from its
/// own `Stage::Locked`.
// The real primitive locks and removes through Unix fds; elsewhere it refuses.
#[cfg(unix)]
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

/// NEGATIVE CONTROLS for the rest of the pass's invariants, each projected as
/// the real pass's events are and each caught: tier 3 admitted at a measured
/// `low` with no demand (`NoWholeAtLow`); a row journalled after the
/// re-measure read its tier's target (`StopsAtTarget`); the reserve released
/// below the floor but above the emergency (`ReserveOnlyMeasuredEmergency`).
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
