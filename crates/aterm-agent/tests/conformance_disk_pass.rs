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
use aterm_models_harness::{
    disk_budget_pass_model, disk_emergency_unmet_model, disk_reclaim_pass_model,
};
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
    /// A rustc holds the lock of the died build's piece being removed (the model's
    /// `WriterHolds`), told as its removal begins.
    Writing(PathBuf),
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
    // Tier 0's steps are its own: admitted, journalled, its writer's lock probed, removed.
    let mut current: Option<Tier> = None;
    let word_of = |tier: Option<Tier>, died: &'static str, other: &'static str| {
        if tier == Some(Tier::DiedBuild) {
            died
        } else {
            other
        }
    };
    for e in events {
        match e {
            Event::Read(free) => pending = Some(*free),
            Event::Removing(_, tier) if *tier == Some(Tier::DiedBuild) => {
                current = *tier;
                if s.get("died") == Some(&0) {
                    step(m, s, "AdmitDied")?;
                }
                step(m, s, "JournalDied")?;
                (locked, removed) = (false, false);
                held.clear();
            }
            Event::Writing(_) => step(m, s, "WriterHolds")?,
            Event::Removing(_, tier) => {
                current = *tier;
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
                    step(m, s, word_of(current, "ProbeHeld", "LockHeld"))?;
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
                    step(m, s, word_of(current, "RemoveDied", "Remove"))?;
                    removed = true;
                }
            }
            Event::Removed(..) => {
                if !removed {
                    step(m, s, word_of(current, "RemoveDied", "Remove"))?;
                    removed = true;
                }
            }
            Event::Failed(_) => {
                step(m, s, word_of(current, "ProbeRefused", "LockRefused"))?;
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

/// A build that died, laid in a build directory under `dir`: the build directory, its profile,
/// the unfinished session folder (its lock file beside it) and a metadata temporary folder in
/// `deps/`, each piece dated `at`.
#[cfg(unix)]
fn lay_died(dir: &Path, at: i64) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let target = lay(&dir.join("target"));
    let profile = target.join("debug");
    let krate = profile.join("incremental/lay-1");
    let session = krate.join("s-old1-died1-working");
    std::fs::create_dir_all(&session).expect("session");
    std::fs::write(session.join("query-cache.bin"), vec![1u8; 4096]).expect("cache");
    std::fs::write(krate.join("s-old1-died1.lock"), b"").expect("lock");
    let meta = profile.join("deps/rmetaAb12Cd");
    std::fs::create_dir_all(&meta).expect("rmeta");
    std::fs::write(meta.join("lib.rmeta"), vec![2u8; 4096]).expect("rmeta file");
    let when = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(at).expect("after the epoch"));
    for piece in [&session, &meta] {
        std::fs::File::open(piece)
            .and_then(|f| f.set_modified(when))
            .expect("dated");
    }
    (target, profile, session, meta)
}

/// The tier 0 row of the piece `entry` [`lay_died`] laid in `profile` of `target`, dated `at`:
/// a session (its lock beside it) or a metadata folder.
#[cfg(unix)]
fn died_row(target: &Path, profile: &Path, entry: &Path, at: i64) -> Row {
    let session_lock = entry
        .to_str()
        .is_some_and(|p| p.ends_with("-working"))
        .then(|| entry.with_file_name("s-old1-died1.lock"));
    Row {
        class: Class::CargoTargets,
        path: entry.to_path_buf(),
        bytes: 4096,
        bytes_partial: false,
        witness: Witness::DiedBuild {
            marker: "CACHEDIR.TAG".to_owned(),
            build_dir: target.to_path_buf(),
            profile: profile.to_path_buf(),
            session_lock,
            modified: at,
            age_s: 3_600,
            device: 0,
        },
        removable: true,
        blocked: None,
    }
}

/// A rustc still writing its session: it holds the session's lock (`flock`) until dropped.
#[cfg(unix)]
struct Rustc(std::fs::File);

#[cfg(unix)]
impl Rustc {
    fn holds(lock: &Path) -> Self {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock)
            .expect("the lock");
        #[expect(
            clippy::disallowed_methods,
            reason = "the test's rustc: its Drop lets the lock go"
        )]
        let taken = file.try_lock();
        taken.expect("the test's rustc takes its lock");
        Self(file)
    }
}

#[cfg(unix)]
impl Drop for Rustc {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// How a run removes a tier 0 row.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum DiedBy {
    /// The real primitive.
    Real,
    /// NEGATIVE CONTROL: the folder deleted outright, no lock taken.
    IgnoringTheLock,
}

/// A pass over `rep` at `now` whose tier 0 rows are removed `by` the way named, every other row
/// by a stand-in, each step told as [`run_with`] tells it; the piece at `writing` is one a rustc
/// holds, told ([`Event::Writing`]) as its removal begins, after its journal row.
#[cfg(unix)]
fn run_died(
    rep: &Report,
    script: &[u64],
    now: i64,
    by: DiedBy,
    writing: Option<&Path>,
) -> Vec<Event> {
    let events = RefCell::new(Vec::new());
    let reads = RefCell::new(0usize);
    let judge = disk::Judge {
        now,
        threshold_days: 1,
        device: None,
        volume: None,
    };
    disk::apply_auto(
        rep,
        None,
        &mut |row: &Row| {
            let Witness::DiedBuild {
                build_dir,
                profile,
                modified,
                ..
            } = &row.witness
            else {
                return Ok(Removed {
                    bytes: row.bytes,
                    units: vec![row.path.clone()],
                    ..Removed::default()
                });
            };
            if writing == Some(row.path.as_path()) {
                events.borrow_mut().push(Event::Writing(row.path.clone()));
            }
            if by == DiedBy::IgnoringTheLock {
                std::fs::remove_dir_all(&row.path)?;
                return Ok(Removed {
                    bytes: row.bytes,
                    units: vec![row.path.clone()],
                    ..Removed::default()
                });
            }
            disk::target::reclaim_died_with(
                build_dir,
                profile,
                &row.path,
                &judge,
                *modified,
                &mut || Ok(()),
                &mut |stage, at| {
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
                },
            )
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
    )
    .expect("short: a pass runs");
    events.into_inner()
}

/// The model's state once a build died leaving `pieces` pieces and the volume was measured
/// short.
#[cfg(unix)]
fn died_and_short(m: &Model, pieces: usize) -> Result<S, String> {
    let mut s = m.init_state();
    for _ in 0..pieces {
        step(m, &mut s, "BuildDied")?;
    }
    step(m, &mut s, "MeasureShort")?;
    Ok(s)
}

/// A report measured short whose rows are `rows`, in the order given.
#[cfg(unix)]
fn short_with(rows: Vec<Row>) -> Report {
    let mut rep = report_at(levels()[2].1, None);
    rep.rows = rows;
    rep
}

/// THE TIER 0 CONFORMANCE: the real primitive takes each of a died build's two pieces (an
/// unfinished session and a metadata folder) under its locks, journalled, the locks taken, then
/// deleted, each taken whole; where a rustc holds the session's lock the session is refused and
/// kept, and the pass goes on to the metadata folder; the model accepts both runs step for step,
/// a finished unit's tier admitted only once both pieces were dealt with. The order of the
/// tiers is the real report's, which exit test 2 holds (`disk_pressure_tests`).
/// NEGATIVE CONTROL: the two tests below, a plan that takes a finished unit first and a remover
/// that deletes past the held lock, each rejected by the model; and a primitive that skipped
/// the session's lock would delete the held session (the `session.exists()` line fails).
#[cfg(unix)]
#[test]
fn the_real_pass_takes_a_died_builds_pieces_under_their_locks_as_the_model_says() {
    let m = disk_reclaim_pass_model();
    let now = NOW;
    for writing in [false, true] {
        let scratch = Scratch::new(if writing { "died-held" } else { "died" });
        let (target, profile, session, meta) = lay_died(&scratch.0, now - 3_600);
        let lock = session.with_file_name("s-old1-died1.lock");
        let rustc = writing.then(|| Rustc::holds(&lock));
        let rep = short_with(vec![
            died_row(&target, &profile, &session, now - 3_600),
            died_row(&target, &profile, &meta, now - 3_600),
            idle("/h/x/target", 9),
            recent("/h/z/target/debug", NOW - 7_200),
        ]);
        let plan = disk::pass_plan(&rep).expect("short: a plan");
        assert!(
            plan.iter()
                .filter(|p| p.row.tier() == Some(Tier::DiedBuild))
                .all(|p| p.target.is_none()),
            "taken whole: {plan:?}"
        );
        let held = writing.then_some(session.as_path());
        let events = run_died(&rep, &[levels()[2].1], now, DiedBy::Real, held);
        let mut s = died_and_short(&m, 2).expect("the model's start");
        project(&m, &mut s, &plan, &events).unwrap_or_else(|e| panic!("{e}: {events:?}"));
        assert_eq!(
            s.get("partial"),
            Some(&0),
            "both pieces dealt with: {events:?}"
        );
        assert_eq!(session.exists(), writing, "{events:?}");
        assert!(!meta.exists(), "{events:?}");
        assert!(lock.exists(), "the lock is rustc's own to collect");
        drop(rustc);
    }
}

/// NEGATIVE CONTROL of the tier 0 order: a plan that takes a finished unit (tier 2) before the
/// died build's session is rejected: the model admits no finished unit's tier while a died
/// build's piece is still to be dealt with at a short.
#[cfg(unix)]
#[test]
fn a_plan_that_takes_a_finished_unit_before_the_died_build_is_rejected() {
    let m = disk_reclaim_pass_model();
    let now = NOW;
    let scratch = Scratch::new("died-late");
    let (target, profile, session, _) = lay_died(&scratch.0, now - 3_600);
    let died = died_row(&target, &profile, &session, now - 3_600);
    let rep = short_with(vec![idle("/h/x/target", 9), died]);
    let plan = disk::pass_plan(&rep).expect("short: a plan");
    let events = run_died(&rep, &[levels()[2].1], now, DiedBy::Real, None);
    let mut s = died_and_short(&m, 1).expect("the model's start");
    let refused = project(&m, &mut s, &plan, &events).expect_err("a finished unit first");
    assert!(refused.contains("Admit2"), "{refused}");
}

/// NEGATIVE CONTROL of the session's lock: a remover that deletes a session whose lock a rustc
/// holds, without taking it, is rejected: the model removes a died build's piece only with its
/// lock taken.
#[cfg(unix)]
#[test]
fn a_remover_that_deletes_a_session_rustc_holds_is_rejected() {
    let m = disk_reclaim_pass_model();
    let now = NOW;
    let scratch = Scratch::new("died-robbed");
    let (target, profile, session, _) = lay_died(&scratch.0, now - 3_600);
    let rustc = Rustc::holds(&session.with_file_name("s-old1-died1.lock"));
    let rep = short_with(vec![died_row(&target, &profile, &session, now - 3_600)]);
    let plan = disk::pass_plan(&rep).expect("short: a plan");
    let events = run_died(
        &rep,
        &[levels()[2].1],
        now,
        DiedBy::IgnoringTheLock,
        Some(&session),
    );
    assert!(!session.exists(), "the control deleted it");
    let mut s = died_and_short(&m, 1).expect("the model's start");
    let refused = project(&m, &mut s, &plan, &events).expect_err("deleted past a held lock");
    assert!(refused.contains("RemoveDied"), "{refused}");
    drop(rustc);
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
        let moved = disk_reserve_look(&state, &reserve, level, Some(free), &t, None, NOW);
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

/// How a host plans its passes at the emergency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plans {
    /// The real hosts' way: [`disk::cadence::Emergency::due`].
    Real,
    /// NEGATIVE CONTROL: on the interval alone ([`disk::cadence::due`]), the wait ignored.
    Interval,
    /// NEGATIVE CONTROL: the wait cleared at every look, whatever moved.
    Blind,
}

/// One look of the emergency's script: the level and free bytes read at unix second `now`,
/// whether an agent waits for room (`demand`), the live sessions' digest, whether something the
/// wait reads moved since the last look (`moved`, the script's own word, never the code's), and
/// whether the host restarted just before it (`restart`).
#[derive(Debug, Clone, Copy)]
struct Seen {
    level: disk::Level,
    free: u64,
    now: i64,
    demand: bool,
    live: u64,
    moved: bool,
    restart: bool,
}

/// A host's look at `disk=emergency` over the real episode's end ([`cli::disk_emergency_look`]),
/// the real wait ([`disk::cadence::Emergency::due`]) and the real pass end
/// ([`cli::disk_emergency_end`]), each pass recorded as the hosts record it
/// ([`disk::cadence::ran`]), its passes planned the way `plans` names.
struct EmergencyHost<'a> {
    at: cli::EmergencyAt<'a>,
    done: Applied,
    emergency: disk::cadence::Emergency,
    last: Option<disk::cadence::Pass>,
    started: std::time::Instant,
    plans: Plans,
}

impl EmergencyHost<'_> {
    /// One look as `seen`, told as the model's actions, in order: `Leave` where it read the
    /// episode's end, `Enter` where the model is out of one and the level reads the emergency,
    /// `Restart` where the host restarted, `Move` where the script says something moved while
    /// the model waits, `Wake` where the real wait went before the look's pass, `PassUnmet` where
    /// the look's pass left a wait standing.
    fn look(&mut self, s: &S, seen: Seen) -> Vec<&'static str> {
        let mut took = Vec::new();
        if seen.restart {
            self.emergency = disk::cadence::Emergency::default();
            self.last = None;
            took.push("Restart");
        }
        let line = self.at.line;
        let over = disk::cadence::over(seen.level, Some(seen.free), line);
        cli::disk_emergency_look(
            self.at.state,
            &mut self.emergency,
            seen.level,
            Some(seen.free),
            line,
        );
        let waiting = s.get("waiting") == Some(&1) && !seen.restart;
        if over && s.get("emergency") == Some(&1) {
            took.push("Leave");
        } else if !over && seen.level == disk::Level::Emergency && s.get("emergency") == Some(&0) {
            took.push("Enter");
        }
        if seen.moved && waiting && !over {
            took.push("Move");
        }
        let elapsed = u64::try_from(seen.now - NOW).expect("the script runs forward");
        let look = disk::cadence::Look {
            at: self.started + std::time::Duration::from_secs(elapsed),
            unix: seen.now,
            level: seen.level,
            free: Some(seen.free),
            room: 16 * GIB_BYTES,
            poked: false,
            demand: seen.demand,
        };
        let stood = self.emergency.unmet.is_some();
        let due = match self.plans {
            Plans::Real => self
                .emergency
                .due(&mut self.last, &look, || Some(seen.live)),
            Plans::Interval => disk::cadence::due(
                &mut self.last,
                look.at,
                look.level,
                look.free,
                look.poked,
                look.room,
            ),
            Plans::Blind => {
                self.emergency.unmet = None;
                self.emergency
                    .due(&mut self.last, &look, || Some(seen.live))
            }
        };
        if stood && self.emergency.unmet.is_none() {
            took.push("Wake");
        }
        if due {
            let at = cli::EmergencyAt {
                now: seen.now,
                level: seen.level,
                live: seen.live,
                ..self.at
            };
            cli::disk_emergency_end(
                &at,
                &mut self.emergency,
                Some(&self.done),
                0,
                Some(seen.free),
            );
            let fruitless = seen.level == disk::Level::Emergency;
            disk::cadence::ran(
                &mut self.last,
                look.at,
                look.free,
                fruitless,
                seen.level,
                false,
            );
            if self.emergency.unmet.is_some() {
                took.push("PassUnmet");
            }
        }
        took
    }
}

/// The `cannot-meet` rows in the ledger under `state`.
fn cannot_meet_in(state: &Path) -> i64 {
    i64::try_from(cannot_meet_rows(state)).unwrap_or(i64::MAX)
}

/// The emergency's script ([`Seen`]): a pass that takes nothing; a look past the ten-minute
/// interval with nothing moved; free space moving by the drop; a restart; the clock reaching
/// the hour's bound; an agent's wait for room; the live sessions changing; a look at `short`
/// inside the drop over the line (the same episode); the volume read out of it by the drop; and
/// the emergency again, its interval past.
fn emergency_script() -> Vec<Seen> {
    use disk::cadence::{EMERGENCY_DROP, PASS_EVERY, UNMET_WAIT_MAX_S};
    let emergency = disk::Level::Emergency;
    let every = i64::try_from(PASS_EVERY.as_secs()).expect("seconds");
    let first = Seen {
        level: emergency,
        free: GIB_BYTES,
        now: NOW,
        demand: false,
        live: 7,
        moved: false,
        restart: false,
    };
    let moved = GIB_BYTES + EMERGENCY_DROP + (64 << 20);
    let restarted = NOW + every + 120;
    let ripe = restarted + UNMET_WAIT_MAX_S;
    vec![
        first,
        Seen {
            now: NOW + every + 60,
            ..first
        },
        Seen {
            free: moved,
            now: NOW + every + 90,
            moved: true,
            ..first
        },
        Seen {
            free: moved,
            now: restarted,
            restart: true,
            ..first
        },
        Seen {
            free: moved,
            now: ripe,
            moved: true,
            ..first
        },
        Seen {
            free: moved,
            now: ripe + 60,
            demand: true,
            moved: true,
            ..first
        },
        Seen {
            free: moved,
            now: ripe + 120,
            live: 8,
            moved: true,
            ..first
        },
        Seen {
            level: disk::Level::Short,
            free: 4 * GIB_BYTES + (64 << 20),
            now: ripe + 180,
            live: 8,
            moved: true,
            ..first
        },
        Seen {
            level: disk::Level::Ok,
            free: 10 * GIB_BYTES,
            now: ripe + 240,
            live: 8,
            moved: true,
            ..first
        },
        Seen {
            now: ripe + 240 + every,
            live: 8,
            ..first
        },
    ]
}

/// THE EMERGENCY'S RECORD CONFORMANCE: the real episode's end, wait and pass end, through the
/// script of [`emergency_script`] (two episodes, a restart in the first), take only the model's
/// steps, and after each look the ledger holds as many `cannot-meet` rows as the model's
/// episodes wrote: one in the first, across its restart (the memo is a file), and one in the
/// second. Whether something moved is the script's word, so a wait that went with nothing moved
/// is a `Wake` the model refuses. The clock is frozen: every look's time is the script's.
/// NEGATIVE CONTROLS: a host that plans on the interval alone runs a pass past the ten minutes
/// with the wait standing and nothing moved (`PassUnmet` is not enabled while the model waits);
/// one whose wait goes at every look wakes with nothing moved (`Wake` is not enabled).
#[test]
fn the_emergency_record_takes_only_the_models_steps() {
    let m = disk_emergency_unmet_model();
    for plans in [Plans::Real, Plans::Interval, Plans::Blind] {
        let scratch = Scratch::new("emergency");
        let mut host = EmergencyHost {
            at: cli::EmergencyAt {
                state: &scratch.0,
                now: NOW,
                level: disk::Level::Emergency,
                volume: "/",
                total: 100 * GIB_BYTES,
                line: 4 * GIB_BYTES,
                live: 7,
            },
            done: Applied {
                seen: disk::Rebuildable {
                    bytes: 3 * GIB_BYTES,
                    partial: false,
                },
                ..Applied::default()
            },
            emergency: disk::cadence::Emergency::default(),
            last: None,
            started: std::time::Instant::now(),
            plans,
        };
        let mut s = m.init_state();
        let mut written = 0;
        let mut ran = Ok(());
        let mut trace = Vec::new();
        for seen in emergency_script() {
            for action in host.look(&s, seen) {
                trace.push(action);
                let rows = s.get("rows").copied();
                ran = ran.and_then(|()| step(&m, &mut s, action));
                written += i64::from(s.get("rows").copied() > rows);
            }
            if ran.is_ok() {
                assert_eq!(cannot_meet_in(&scratch.0), written, "{plans:?}: {trace:?}");
            }
        }
        match plans {
            Plans::Real => {
                ran.unwrap_or_else(|e| panic!("{e}: {trace:?}"));
                assert_eq!(written, 2, "one row an episode, two episodes: {trace:?}");
                assert_eq!(
                    trace.iter().filter(|a| **a == "PassUnmet").count(),
                    7,
                    "the first pass, after the move, the restart, the clock, the ask, the live \
                     sessions, and the second episode's: {trace:?}"
                );
            }
            Plans::Interval => {
                let refused = ran.expect_err("a pass on the interval alone");
                assert!(refused.contains("`PassUnmet` is not enabled"), "{refused}");
            }
            Plans::Blind => {
                let refused = ran.expect_err("a wake with nothing moved");
                assert!(refused.contains("`Wake` is not enabled"), "{refused}");
            }
        }
    }
}
