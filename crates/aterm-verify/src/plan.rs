// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stages, as data: which ones a selection of checks runs, in which order,
//! contending for what. Concurrency changes when a stage runs, never where its
//! ladder prints in the log.

use std::path::PathBuf;

use crate::Ctx;
use crate::scope::Scope;

/// One GiB, in bytes.
pub const GIB: u64 = 1 << 30;

/// A contended resource. Two stages in one lane run in declared order; lanes
/// overlap freely. A lane is a cargo target directory, which two cargo
/// invocations would queue on anyway.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    /// Touches no target directory.
    Pure,
    /// The caller's target dir: the test compile and the test run.
    MainTarget,
    /// The same directory, for the cargo runs that reuse the test compile's
    /// build (the doctests, `spec-check`): they await it
    /// ([`StageSpec::after_stages`]) and then run beside the test run, which
    /// compiles nothing more.
    AfterCompile,
    /// `<target>/verify/tippy`: the lint. Its check-mode units share no
    /// artifact with the test build, so a dir of its own lets the two compile
    /// at once instead of queueing on cargo's lock.
    TippyTarget,
    /// `<target>/verify/drivers`: every binary a stage drives (the smokes'
    /// aterm-gui, the redraw harness, the objc drivers, the Codex lane's
    /// aterm), so the test build never relinks one under a stage driving it.
    DriverTarget,
    /// `<target>/verify/trust`: the release tools (aterm-release) the Trust
    /// lane and `ship check --export` run and, under `slots/`, the Trust
    /// lane's checks, built with flags no other lane shares.
    TrustTarget,
    /// `<target>/verify/cells`: the `cells` check, one directory per cell,
    /// and the `wasm-smoke` and `wasm-glue` modules, each built by a rustup
    /// toolchain no other lane uses.
    CellsTarget,
    /// `<root>/libc-oracle/target`: the libc oracle's own workspace and its
    /// gate. One per checkout, never under a shared target dir: the oracle's
    /// shim build is keyed by paths relative to its workspace, so a dir shared
    /// across checkouts can serve another checkout's conformance build.
    LibcOracleTarget,
}

/// Every stage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StageId {
    /// The test stage's compile, at t0, so the doctests can start when it ends
    /// and run beside the test run.
    TestCompile,
    Test,
    Doctests,
    /// The sealed cross-host rung of the fabric bridge (`--features sealed`),
    /// in the driver lane: its suites boot the `aterm` it builds first.
    SealedLane,
    Tippy,
    Formatting,
    /// The source guards (`aterm-census --guards`).
    Guards,
    /// The publication engine's export guards (`ship check --export`).
    Export,
    /// The Trust verification lane over the selection's libraries.
    TrustAdvisory,
    LibcOracle,
    /// The L0 censuses ([`crate::stages::L0_CENSUSES`]).
    Census,
    /// The Trust checkers over every registered formal model (`spec-check`).
    SpecCheck,
    /// The hand-written specs' Tier-1 conformances, which only `ty` can
    /// discharge ([`crate::stages::TY_ONLY_CONFORMANCES`]), in the main lane
    /// after the test run so they reuse its build.
    TyConformances,
    /// The smokes' `aterm-gui`/`aterm-ctl`, prebuilt in the driver lane.
    DriverBuilds,
    /// What only the rows behind the first barrier drive, prebuilt; nothing
    /// before the barrier waits for it ([`NOT_AWAITED`]).
    TailDriverBuilds,
    /// The paint and spin matrices over the handed release bundle, run alone.
    MeasuringTests,
    ControlSocketSmoke,
    GuiSmoke,
    RedrawConformance,
    /// The objc drivers (`stages::OBJC_DRIVES`), one row each, on macOS.
    ObjcDrives,
    /// The forge cells no fleet machine hosts (or each `--cell`), each linted
    /// for its own triple ([`crate::cells`]).
    ForeignCells,
    /// The two web modules' host boundary under node ([`crate::wasm_smoke`]).
    WasmSmoke,
    /// The JS glue generator on aterm-wasm-abi's toy module
    /// ([`crate::wasm_glue`]).
    WasmGlue,
    Forge,
    /// The Codex live upgrade: reads this machine's managed store, run alone.
    CodexLiveUpgrade,
}

/// The rows an after-lanes stage does not wait for: compile-only, last in
/// their lane before the first barrier, and everything they build is driven
/// behind that barrier.
pub const NOT_AWAITED: [StageId; 1] = [StageId::TailDriverBuilds];

/// The stages that put a window on the desktop, each with the name its
/// `--no-windows` skip carries. Every other stage drives a headless instance,
/// a stand-in, or no binary at all.
pub const WINDOWED: [(StageId, &str); 3] = [
    (StageId::GuiSmoke, "gui smoke"),
    (StageId::RedrawConformance, "redraw conformance"),
    (StageId::ObjcDrives, "objc drives"),
];

/// The stages whose verdicts read the machine's timing: two runs of them on
/// one machine at once would judge each other. They take a machine-wide
/// advisory lock while they run ([`crate::default_timing_lock`]).
pub const TIMED: [StageId; 5] = [
    StageId::MeasuringTests,
    StageId::ControlSocketSmoke,
    StageId::GuiSmoke,
    StageId::RedrawConformance,
    StageId::ObjcDrives,
];

impl StageId {
    /// The named skip `--no-windows` answers for this stage, with its fix:
    /// `Some` exactly for a [`WINDOWED`] stage.
    #[must_use]
    pub fn no_windows_skip(self) -> Option<String> {
        WINDOWED
            .iter()
            .find(|(id, _)| *id == self)
            .map(|(_, name)| {
                format!(
                    "{name} (--no-windows: it opens a window on the desktop — fix: run without \
                 --no-windows when the desktop is free)"
                )
            })
    }

    /// Whether a stage whose [`StageSpec::after_lanes`] names this stage's lane
    /// waits for it: every stage but the [`NOT_AWAITED`] ones.
    #[must_use]
    pub fn awaited(self) -> bool {
        !NOT_AWAITED.contains(&self)
    }

    /// Does this stage judge timings ([`TIMED`])?
    #[must_use]
    pub fn timed(self) -> bool {
        TIMED.contains(&self)
    }
}

/// A stage's identity, its log header, and its scheduling constraints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageSpec {
    pub id: StageId,
    /// The `=== … ===` header, resolved against the selection where it varies.
    pub title: String,
    pub lane: Lane,
    /// Runs with nothing else in flight: the stages whose verdict depends on
    /// how busy the machine is.
    pub exclusive: bool,
    /// Lanes whose stages must all have finished before this one starts
    /// (barring later barriers and [`NOT_AWAITED`] rows; `crate::sched`).
    pub after_lanes: Vec<Lane>,
    /// Stages that must have finished before this one starts.
    pub after_stages: Vec<StageId>,
}

/// The rows that compile at workspace scale: each builds the `aterm-gui` graph.
pub const WORKSPACE_BUILDS: [StageId; 4] = [
    StageId::TestCompile,
    StageId::Tippy,
    StageId::DriverBuilds,
    StageId::TailDriverBuilds,
];

/// The memory one concurrent workspace build needs at its peak.
pub const WORKSPACE_BUILD_BYTES: u64 = 2 * GIB;

/// What the host keeps for everything that is not a build.
pub const HOST_RESERVE_BYTES: u64 = 2 * GIB;

/// How many workspace builds `ram` bytes hold at once.
#[must_use]
pub fn builds_held(ram: u64) -> u64 {
    ram.saturating_sub(HOST_RESERVE_BYTES) / WORKSPACE_BUILD_BYTES
}

/// The rows that can compile while the test run runs: the lint, the driver
/// lane's builds (the sealed rung's among them) and the doctests, each counted
/// as one build (the fail-closed figure).
#[must_use]
pub fn builds_beside_tests(plan: &[StageSpec]) -> usize {
    plan.iter()
        .filter(|s| {
            matches!(
                s.id,
                StageId::Tippy
                    | StageId::DriverBuilds
                    | StageId::SealedLane
                    | StageId::TailDriverBuilds
                    | StageId::Doctests
            )
        })
        .count()
}

/// The trust lane waits for the workspace builds, the test runs and the
/// doctests in every plan that has them: the verifier's verdict budgets are
/// wall-clock budgets, so beside those builds it loses proofs it keeps alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustOverlap {
    /// No trust lane, or nothing to hold it behind.
    Overlaps,
    /// The trust lane waits.
    AfterTests,
}

/// Decide [`TrustOverlap`] for `plan`.
#[must_use]
pub fn trust_overlap(plan: &[StageSpec]) -> TrustOverlap {
    let builds = plan.iter().any(|s| {
        WORKSPACE_BUILDS.contains(&s.id)
            || matches!(s.id, StageId::Doctests | StageId::TyConformances)
    });
    if builds && plan.iter().any(|s| s.id == StageId::TrustAdvisory) {
        TrustOverlap::AfterTests
    } else {
        TrustOverlap::Overlaps
    }
}

impl TrustOverlap {
    /// Hold the trust lane behind every workspace build, the test runs, the
    /// doctests, and whatever the test run itself awaits (which
    /// [`crate::sched::check_after_lanes`] requires).
    pub fn apply(self, plan: &mut [StageSpec]) {
        if self == TrustOverlap::Overlaps {
            return;
        }
        let run = plan.iter().position(|s| s.id == StageId::Test);
        let waits: Vec<StageId> = plan
            .iter()
            .enumerate()
            .filter(|&(j, s)| {
                WORKSPACE_BUILDS.contains(&s.id)
                    || matches!(
                        s.id,
                        StageId::Test | StageId::Doctests | StageId::TyConformances
                    )
                    || run.is_some_and(|t| crate::sched::awaited(plan, t).any(|a| a == j))
            })
            .map(|(_, s)| s.id)
            .collect();
        for s in plan.iter_mut() {
            if s.id == StageId::TrustAdvisory {
                s.after_stages.clone_from(&waits);
            }
        }
    }
}

/// The target directory a lane's cargo children use; `None` for [`Lane::Pure`].
/// The side lanes live under the run's target dir, never beside it.
#[must_use]
pub fn lane_dir(ctx: &Ctx, lane: Lane) -> Option<PathBuf> {
    let side = |name: &str| Some(ctx.target_dir.join("verify").join(name));
    match lane {
        Lane::Pure => None,
        Lane::MainTarget | Lane::AfterCompile => Some(ctx.target_dir.clone()),
        Lane::TippyTarget => side("tippy"),
        Lane::DriverTarget => side("drivers"),
        Lane::TrustTarget => side("trust"),
        Lane::CellsTarget => side("cells"),
        Lane::LibcOracleTarget => Some(ctx.root.join("libc-oracle/target")),
    }
}

/// The run's stages: every declared stage the selected checks need
/// ([`crate::checks::stages_of`]), in declared order, waiting only on stages
/// that are planned; then the trust lane's hold ([`trust_overlap`]).
#[must_use]
pub fn plan(ctx: &Ctx) -> Vec<StageSpec> {
    let mut wanted = crate::checks::stages_of(&ctx.checks);
    // An empty cone compiles nothing: the test stage says so once.
    if ctx.scope.selects_nothing() {
        wanted.retain(|id| *id != StageId::TestCompile);
    }
    let mut v: Vec<StageSpec> = declared(&ctx.scope)
        .into_iter()
        .filter(|s| wanted.contains(&s.id))
        .collect();
    let present: Vec<StageId> = v.iter().map(|s| s.id).collect();
    for s in &mut v {
        s.after_stages.retain(|id| present.contains(id));
    }
    trust_overlap(&v).apply(&mut v);
    v
}

/// Every stage, in the order the log prints them.
///
/// The exclusive stages are barriers: everything declared before one finishes
/// first, and the driver-lane rows behind it run after it. In the driver lane
/// the sealed rung precedes the tail driver builds: it relinks
/// `<drivers>/debug/aterm` with `--features sealed`, and the tail relinks the
/// plain one the Codex lane drives.
#[must_use]
pub fn declared(scope: &Scope) -> Vec<StageSpec> {
    let label = scope.label();
    vec![
        spec(
            StageId::TestCompile,
            format!("test compile ({label})"),
            Lane::MainTarget,
        ),
        spec(StageId::Test, format!("test ({label})"), Lane::MainTarget),
        StageSpec {
            after_stages: vec![StageId::TestCompile],
            ..spec(
                StageId::Doctests,
                format!("doctests ({label})"),
                Lane::AfterCompile,
            )
        },
        spec(
            StageId::Tippy,
            format!("tippy lint ({label})"),
            Lane::TippyTarget,
        ),
        spec(
            StageId::Formatting,
            "formatting (trustfmt over every tracked .rs, at its crate's edition)",
            Lane::Pure,
        ),
        spec(StageId::Guards, "source guards (aterm-census)", Lane::Pure),
        spec(StageId::Census, "L0 censuses (aterm-census)", Lane::Pure),
        StageSpec {
            after_stages: vec![StageId::TestCompile],
            ..spec(
                StageId::SpecCheck,
                "spec checkers (spec-check: ty, ay, trust-ir over every registered model)",
                Lane::AfterCompile,
            )
        },
        spec(
            StageId::TyConformances,
            "ty-only conformances (the hand-written specs' Tier-1 tests, under ty)",
            Lane::MainTarget,
        ),
        spec(
            StageId::Export,
            "export guards (ship check --export: the publication engine's dry run)",
            Lane::TrustTarget,
        ),
        spec(
            StageId::TrustAdvisory,
            "trust lane (ship trust run: targo trust check --lib over the selection's libraries, \
             held to their floors)",
            Lane::TrustTarget,
        ),
        spec(
            StageId::LibcOracle,
            "libc ABI oracle (this host's native cell)",
            Lane::LibcOracleTarget,
        ),
        spec(
            StageId::Forge,
            "third-party surface (aterm-forge check)",
            Lane::Pure,
        ),
        spec(
            StageId::ForeignCells,
            "cells (each linted for its own triple on rustup's toolchain)",
            Lane::CellsTarget,
        ),
        spec(
            StageId::WasmSmoke,
            "wasm smoke (both web modules' host boundary under node)",
            Lane::CellsTarget,
        ),
        spec(
            StageId::WasmGlue,
            "wasm glue (the generator on aterm-wasm-abi's toy, its goldens, under node)",
            Lane::CellsTarget,
        ),
        spec(
            StageId::DriverBuilds,
            "driver builds (the smokes' aterm-gui and aterm-ctl)",
            Lane::DriverTarget,
        ),
        spec(
            StageId::SealedLane,
            "sealed fabric lane (aterm --features sealed)",
            Lane::DriverTarget,
        ),
        spec(
            StageId::TailDriverBuilds,
            "tail driver builds (what the rows behind the barrier drive)",
            Lane::DriverTarget,
        ),
        exclusive(
            StageId::MeasuringTests,
            "measuring tests (paint and spin over the handed bundle; run alone)",
            Lane::MainTarget,
        ),
        exclusive(
            StageId::ControlSocketSmoke,
            "control-socket smoke",
            Lane::DriverTarget,
        ),
        exclusive(
            StageId::GuiSmoke,
            "gui typing-pacing smoke",
            Lane::DriverTarget,
        ),
        spec(
            StageId::RedrawConformance,
            "control redraw conformance (a select repaints a real window)",
            Lane::DriverTarget,
        ),
        spec(
            StageId::ObjcDrives,
            "objc drives (live-class audit, IME, toolbar, window and event, on a real AppKit)",
            Lane::DriverTarget,
        ),
        exclusive(
            StageId::CodexLiveUpgrade,
            "Codex live upgrade (the managed store's older Codex moved onto its current one; \
             run alone)",
            Lane::DriverTarget,
        ),
    ]
}

fn spec(id: StageId, title: impl Into<String>, lane: Lane) -> StageSpec {
    StageSpec {
        id,
        title: title.into(),
        lane,
        exclusive: false,
        after_lanes: Vec::new(),
        after_stages: Vec::new(),
    }
}

fn exclusive(id: StageId, title: impl Into<String>, lane: Lane) -> StageSpec {
    StageSpec {
        exclusive: true,
        ..spec(id, title, lane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvSnapshot;
    use crate::checks::{CHECKS, Check, DEV, RELEASE};
    use std::path::PathBuf;

    fn ctx(checks: &[Check], scope: Scope) -> Ctx {
        Ctx::new(
            PathBuf::from("/repo"),
            scope,
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
        .with_checks(checks)
    }

    fn ids(ctx: &Ctx) -> Vec<StageId> {
        plan(ctx).into_iter().map(|s| s.id).collect()
    }

    fn every_check() -> Vec<Check> {
        CHECKS.iter().map(|s| s.check).collect()
    }

    /// Each check plans its own stages and its helpers, nothing else; every
    /// combination the CLI offers is deadlock-free.
    #[test]
    fn a_selection_plans_exactly_its_checks_stages_and_never_deadlocks() {
        assert_eq!(
            ids(&ctx(&[Check::Fmt], Scope::workspace())),
            [StageId::Formatting]
        );
        assert_eq!(
            ids(&ctx(&[Check::Test, Check::Doc], Scope::workspace())),
            [StageId::TestCompile, StageId::Test, StageId::Doctests]
        );
        assert_eq!(
            ids(&ctx(&[Check::Doc], Scope::workspace())),
            [StageId::Doctests]
        );
        let lone_doc = plan(&ctx(&[Check::Doc], Scope::workspace()));
        assert!(
            lone_doc[0].after_stages.is_empty(),
            "a stage never waits on one the run does not plan"
        );
        let mut selections: Vec<Vec<Check>> = every_check().into_iter().map(|c| vec![c]).collect();
        selections.extend([DEV.to_vec(), RELEASE.to_vec(), every_check()]);
        for checks in selections {
            let p = plan(&ctx(&checks, Scope::workspace()));
            crate::sched::check_after_lanes(&p)
                .unwrap_or_else(|e| panic!("{checks:?} could deadlock: {e}"));
            let mut want = crate::checks::stages_of(&checks);
            want.sort_by_key(|id| *id as u8);
            let mut got: Vec<StageId> = p.iter().map(|s| s.id).collect();
            got.sort_by_key(|id| *id as u8);
            assert_eq!(got, want, "{checks:?}");
        }
    }

    /// The test compile starts at t0 and the test run as soon as it ends: no
    /// other lane holds either.
    #[test]
    fn the_test_run_follows_its_compile_and_waits_for_nothing_else() {
        let p = plan(&ctx(&RELEASE, Scope::workspace()));
        let at = |id| p.iter().position(|s| s.id == id).expect("planned");
        let (compile, test) = (at(StageId::TestCompile), at(StageId::Test));
        assert!(compile < test);
        assert!(p[compile].after_lanes.is_empty() && p[test].after_lanes.is_empty());
        let nothing = vec![false; p.len()];
        assert!(crate::sched::ready(&p, &nothing, &nothing, 0, compile));
        assert!(!crate::sched::ready(&p, &nothing, &nothing, 0, test));
        let mut done = nothing.clone();
        done[compile] = true;
        assert!(crate::sched::ready(&p, &done, &done, 0, test));
    }

    /// The doctests await exactly the test compile, then run beside the test
    /// run.
    #[test]
    fn the_doctests_start_when_the_test_compile_ends_and_run_beside_the_test_run() {
        use crate::sched::{awaited, ready};
        let c = ctx(&RELEASE, Scope::workspace());
        let p = plan(&c);
        let at = |id| p.iter().position(|s| s.id == id).expect("planned");
        let (compile, test, doc) = (
            at(StageId::TestCompile),
            at(StageId::Test),
            at(StageId::Doctests),
        );
        assert_eq!(
            lane_dir(&c, Lane::AfterCompile),
            lane_dir(&c, Lane::MainTarget)
        );
        assert_eq!(awaited(&p, doc).collect::<Vec<_>>(), [compile]);
        let none = vec![false; p.len()];
        assert!(!ready(&p, &none, &none, 0, doc));
        let mut done = none.clone();
        done[compile] = true;
        let mut started = done.clone();
        assert!(ready(&p, &started, &done, 0, test));
        assert!(ready(&p, &started, &done, 0, doc));
        started[test] = true;
        assert!(ready(&p, &started, &done, 1, doc));
    }

    /// The tail driver builds are the driver lane's last row before the first
    /// barrier and follow the sealed rung, so the plain `aterm` the Codex lane
    /// drives is the last one linked.
    #[test]
    fn the_tail_driver_builds_follow_the_sealed_rung_and_the_barrier_waits_for_them() {
        let p = plan(&ctx(&RELEASE, Scope::workspace()));
        let at = |id| p.iter().position(|s| s.id == id).expect("planned");
        let tail = at(StageId::TailDriverBuilds);
        let barrier = p.iter().position(|s| s.exclusive).expect("a barrier");
        assert!(tail < barrier);
        assert!(
            p[tail + 1..barrier]
                .iter()
                .all(|s| s.lane != Lane::DriverTarget)
        );
        let sealed = at(StageId::SealedLane);
        assert_eq!(p[sealed].lane, p[tail].lane);
        assert!(
            sealed < tail,
            "the plain aterm is relinked after the sealed one"
        );
        for id in [StageId::RedrawConformance, StageId::ObjcDrives] {
            assert!(barrier < at(id), "{id:?} drives what the tail built");
        }
    }

    /// Only the stages that judge timings own the machine; the Codex upgrade
    /// is last, so nothing waits behind it.
    #[test]
    fn only_the_stages_that_judge_timings_are_exclusive() {
        let p = plan(&ctx(&every_check(), Scope::workspace()));
        let exclusive: Vec<StageId> = p.iter().filter(|s| s.exclusive).map(|s| s.id).collect();
        assert_eq!(
            exclusive,
            [
                StageId::MeasuringTests,
                StageId::ControlSocketSmoke,
                StageId::GuiSmoke,
                StageId::CodexLiveUpgrade,
            ]
        );
        assert_eq!(p.last().map(|s| s.id), Some(StageId::CodexLiveUpgrade));
        assert!(
            ids(&ctx(&DEV, Scope::workspace()))
                .iter()
                .all(|id| !id.timed()),
            "the dev set takes no machine-wide lock"
        );
    }

    /// Every lane lives in the run's target dir (the caller's own, or
    /// `<root>/target`), the side lanes under its `verify/`; the libc oracle's
    /// in its own checkout, whatever the caller's target dir.
    #[test]
    fn each_cargo_lane_lives_under_the_runs_target_dir() {
        let c = ctx(&DEV, Scope::workspace());
        let dir = |c: &Ctx, l| lane_dir(c, l).map(|p| p.display().to_string());
        assert_eq!(dir(&c, Lane::Pure), None);
        for l in [Lane::MainTarget, Lane::AfterCompile] {
            assert_eq!(dir(&c, l).as_deref(), Some("/repo/target"), "{l:?}");
        }
        assert_eq!(
            dir(&c, Lane::TippyTarget).as_deref(),
            Some("/repo/target/verify/tippy")
        );
        assert_eq!(
            dir(&c, Lane::DriverTarget).as_deref(),
            Some("/repo/target/verify/drivers")
        );
        assert_eq!(
            dir(&c, Lane::TrustTarget).as_deref(),
            Some("/repo/target/verify/trust")
        );
        assert_eq!(
            dir(&c, Lane::CellsTarget).as_deref(),
            Some("/repo/target/verify/cells")
        );
        assert_eq!(
            dir(&c, Lane::LibcOracleTarget).as_deref(),
            Some("/repo/libc-oracle/target")
        );
        let theirs = Ctx::new(
            PathBuf::from("/repo"),
            Scope::workspace(),
            EnvSnapshot {
                cargo_target_dir: Some(PathBuf::from("/fast/tgt")),
                ..EnvSnapshot::default()
            },
            PathBuf::from("/tmp"),
        );
        assert_eq!(dir(&theirs, Lane::MainTarget).as_deref(), Some("/fast/tgt"));
        assert_eq!(
            dir(&theirs, Lane::DriverTarget).as_deref(),
            Some("/fast/tgt/verify/drivers")
        );
        assert_eq!(
            dir(&theirs, Lane::LibcOracleTarget).as_deref(),
            Some("/repo/libc-oracle/target")
        );
    }

    /// The trust lane waits for the builds, the test runs and the doctests, and
    /// for what the test run itself awaits; with nothing to wait for it starts
    /// at t0.
    #[test]
    fn the_trust_lane_verifies_after_the_tests_and_alone_starts_at_once() {
        let p = plan(&ctx(
            &[Check::Test, Check::Doc, Check::Lint, Check::Trust],
            Scope::workspace(),
        ));
        assert_eq!(trust_overlap(&p), TrustOverlap::AfterTests);
        let trust = p
            .iter()
            .position(|s| s.id == StageId::TrustAdvisory)
            .expect("planned");
        let mut held: Vec<StageId> = crate::sched::awaited(&p, trust).map(|j| p[j].id).collect();
        held.sort_by_key(|id| *id as u8);
        assert_eq!(
            held,
            [
                StageId::TestCompile,
                StageId::Test,
                StageId::Doctests,
                StageId::Tippy
            ]
        );
        crate::sched::check_after_lanes(&p).expect("deadlock-free");
        let spec = plan(&ctx(&[Check::Spec, Check::Trust], Scope::workspace()));
        assert_eq!(trust_overlap(&spec), TrustOverlap::AfterTests);
        let trust = spec
            .iter()
            .position(|s| s.id == StageId::TrustAdvisory)
            .expect("planned");
        assert!(crate::sched::awaited(&spec, trust).any(|j| spec[j].id == StageId::TyConformances));
        crate::sched::check_after_lanes(&spec).expect("deadlock-free");
        let alone = plan(&ctx(&[Check::Trust], Scope::workspace()));
        assert_eq!(trust_overlap(&alone), TrustOverlap::Overlaps);
        assert!(alone[0].after_stages.is_empty());
    }
}
