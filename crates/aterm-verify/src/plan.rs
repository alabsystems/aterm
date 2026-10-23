// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stages, as data: which ones a selection of checks runs, in which order,
//! contending for what. Concurrency changes when a stage runs, never where its
//! ladder prints in the log.

use std::path::PathBuf;

use aterm_toolchain::build_cache::noindex;

use crate::Ctx;
use crate::scope::Scope;

/// The libc oracle's lane under `<root>/libc-oracle` ([`Lane::LibcOracleTarget`]),
/// which `aterm-libc-oracle` defaults to as well.
pub const LIBC_ORACLE_TARGET: &str = "target.noindex";

/// A contended resource. Two stages in one lane run in declared order; lanes
/// overlap freely. A lane is a cargo target directory, which two cargo
/// invocations would queue on anyway.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    /// Touches no target directory.
    Pure,
    /// The caller's target dir, shared with plain `targo` (and its build cache,
    /// [`crate::Ctx::main_build_dir`]): the test compile and the test run, the
    /// measuring rows, and what `appkit` and `codex-alab` build and drive.
    MainTarget,
    /// The same directory, for the cargo runs that start once the test compile
    /// is done (the doctests, `spec-check`): they await it
    /// ([`StageSpec::after_stages`]) and then run beside the test run, which
    /// compiles nothing more. Beside it, not after it: a whole `verify --all`
    /// measured shorter that way (the commit that set it carries the figures).
    AfterCompile,
    /// `<target>/verify/tippy.noindex`: the lint. Its check-mode units share no
    /// artifact with the test build, so a dir of its own lets the two compile
    /// at once instead of queueing on cargo's lock.
    TippyTarget,
    /// `<target>/verify/checkers.noindex`: the workspace's tests under
    /// `--features aterm-spec/checkers`, a feature build the main cache never holds.
    CheckersTarget,
    /// `<target>/verify/gpu-oracle.noindex`: aterm-gpu's tests under `--features
    /// wgpu-oracle`, a feature build the main cache never holds.
    GpuOracleTarget,
    /// `<target>/verify/trust.noindex`: the release tools (aterm-release) that
    /// `ship check --export` runs.
    TrustTarget,
    /// `<target>/verify/cells.noindex`: the `cells` check, one directory per cell,
    /// and the `wasm-smoke`, `wasm-glue` and `browser` modules, each built by
    /// a rustup toolchain no other lane uses.
    CellsTarget,
    /// `<root>/libc-oracle/target.noindex`: the libc oracle's own workspace and its
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
    Tippy,
    Formatting,
    /// The publication engine's export guards (`ship check --export`).
    Export,
    LibcOracle,
    /// The source guards and the five L0 censuses, in process
    /// ([`crate::stages::CENSUS_WALKS`]).
    Census,
    /// The Trust checkers over every registered formal model (`spec-check`).
    SpecCheck,
    /// The `wgpu-oracle` feature build's compile (`--no-run`), in its own lane
    /// beside the test run.
    GpuOracleCompile,
    /// aterm-gpu's tests with `wgpu-oracle` on: the renderer against wgpu. After
    /// the test run, which drives the same GPU.
    GpuOracle,
    /// The `checkers` feature build's compile (`--no-run`), in its own lane
    /// beside the test run.
    CheckersCompile,
    /// The workspace's tests with `aterm-spec/checkers` on: the tiered tests'
    /// per-step `ty` agreement. After the test run and the gpu oracle's, never
    /// beside either: test suites run at once judge each other's load, and
    /// both hold aterm-gpu's GPU suites.
    SpecCheckers,
    /// The measuring rows' compile (`--no-run`), before the barrier, so the
    /// exclusive stage that runs them compiles nothing.
    MeasurePrebuild,
    /// The paint and spin matrices over the handed release bundle, run alone.
    MeasuringTests,
    GuiSmoke,
    RedrawConformance,
    /// The objc drivers (`stages::OBJC_DRIVES`), one row each, on macOS.
    ObjcDrives,
    /// The forge cells no fleet machine hosts (or each `--cell`), each linted
    /// for its own triple ([`crate::cells`]).
    ForeignCells,
    /// The two web modules' host boundary in a headless Chromium
    /// ([`crate::wasm_smoke`]).
    WasmSmoke,
    /// The JS glue generator on aterm-wasm-abi's toy module
    /// ([`crate::wasm_glue`]).
    WasmGlue,
    /// Both web modules in headless Chromium, Firefox and WebKit: a WebGL2 boot,
    /// GPU vs CPU pixels, on the page and in a worker (`crate::browser`).
    BrowserLane,
    Forge,
    /// The codex-alab native harness end to end (`lab codex-alab-proof`): the
    /// real package in a private headless aterm, run alone.
    CodexAlabProof,
    /// The Linux runtime lane (`lab linux proof`): a lima VM on this Mac,
    /// run alone.
    LinuxLane,
}

/// The stages that put a window on the desktop, each with the name its
/// `--no-windows` skip carries. A [`WINDOWED_ROWS`] stage opens one in some
/// of its rows; every other stage drives a headless instance, a stand-in, or
/// no binary at all.
pub const WINDOWED: [(StageId, &str); 3] = [
    (StageId::GuiSmoke, "gui smoke"),
    (StageId::RedrawConformance, "redraw conformance"),
    (StageId::ObjcDrives, "objc drives"),
];

/// The stages that run under `--no-windows` but answer a named skip, of their
/// own, for the rows that would open a window: the browser lane's WebKit
/// session keeps its headless pages in windows (`crate::browser`).
pub const WINDOWED_ROWS: [StageId; 1] = [StageId::BrowserLane];

/// The stages whose verdicts read the machine's timing: two runs of them on
/// one machine at once would judge each other. They take a machine-wide
/// advisory lock while they run ([`crate::default_timing_lock`]).
pub const TIMED: [StageId; 4] = [
    StageId::MeasuringTests,
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

    /// Does `--no-windows` change what this stage does ([`WINDOWED`] or
    /// [`WINDOWED_ROWS`])? A rerun of its check keeps the flag.
    #[must_use]
    pub fn opens_windows(self) -> bool {
        self.no_windows_skip().is_some() || WINDOWED_ROWS.contains(&self)
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
    /// how busy the machine is, and the lab lanes, which load it.
    pub exclusive: bool,
    /// Stages declared EARLIER that must have FINISHED before this one starts
    /// ([`crate::sched::check_waits`] refuses any other).
    pub after_stages: Vec<StageId>,
}

/// The target directory a lane's cargo children use; `None` for [`Lane::Pure`].
/// The side lanes live under the run's target dir, never beside it, each in a
/// folder named `*.noindex`, which Spotlight skips with all it holds
/// (`aterm_toolchain::build_cache::NOINDEX`): a side lane's builds are
/// intermediates nobody opens.
#[must_use]
pub fn lane_dir(ctx: &Ctx, lane: Lane) -> Option<PathBuf> {
    let side = |name: &str| Some(ctx.state_dir().join(noindex(name)));
    match lane {
        Lane::Pure => None,
        Lane::MainTarget | Lane::AfterCompile => Some(ctx.target_dir.clone()),
        Lane::TippyTarget => side("tippy"),
        Lane::CheckersTarget => side("checkers"),
        Lane::GpuOracleTarget => side("gpu-oracle"),
        Lane::TrustTarget => side("trust"),
        Lane::CellsTarget => Some(cells_dir(ctx)),
        Lane::LibcOracleTarget => Some(ctx.root.join("libc-oracle").join(LIBC_ORACLE_TARGET)),
    }
}

/// The cells lane's directory ([`Lane::CellsTarget`]): every cell's target dir,
/// the wasm modules' builds and the browser sessions' frames live under it.
#[must_use]
pub fn cells_dir(ctx: &Ctx) -> PathBuf {
    ctx.state_dir().join(noindex("cells"))
}

/// The run's stages: every declared stage the selected checks need
/// ([`crate::checks::stages_of`]), in declared order, waiting only on stages
/// that are planned.
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
    v
}

/// Every stage, in the order the log prints them.
///
/// The exclusive stages are barriers: everything declared before one finishes
/// first, and everything behind it starts after it. The measuring rows' compile
/// is the main lane's last row before the first barrier, so the stage that runs
/// them alone compiles nothing; the Linux lane is the last barrier, so it has
/// the machine to itself whatever else the run planned.
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
            "formatting (trustfmt over every tracked or cargo-built .rs, at its crate's edition)",
            Lane::Pure,
        ),
        spec(
            StageId::Census,
            "source guards and L0 censuses (aterm-census)",
            Lane::Pure,
        ),
        StageSpec {
            after_stages: vec![StageId::TestCompile],
            ..spec(
                StageId::SpecCheck,
                "spec checkers (spec-check: ty over every registered model, the ay bundles)",
                Lane::AfterCompile,
            )
        },
        spec(
            StageId::GpuOracleCompile,
            "gpu oracle compile (aterm-gpu's tests with wgpu-oracle, --no-run)",
            Lane::GpuOracleTarget,
        ),
        StageSpec {
            after_stages: vec![StageId::Test],
            ..spec(
                StageId::GpuOracle,
                "gpu oracle (aterm-gpu's tests with wgpu-oracle: the renderer against wgpu; \
                 after the test run)",
                Lane::GpuOracleTarget,
            )
        },
        spec(
            StageId::CheckersCompile,
            "checkers compile (the workspace's tests with aterm-spec/checkers, --no-run)",
            Lane::CheckersTarget,
        ),
        StageSpec {
            after_stages: vec![StageId::Test, StageId::GpuOracle],
            ..spec(
                StageId::SpecCheckers,
                "checkers (the workspace's tests with aterm-spec/checkers: per-step ty \
                 agreement; after the test run and the gpu oracle's)",
                Lane::CheckersTarget,
            )
        },
        spec(
            StageId::Export,
            "export guards (ship check --export: the publication engine's dry run)",
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
            "wasm smoke (both web modules' host boundary in a browser)",
            Lane::CellsTarget,
        ),
        spec(
            StageId::WasmGlue,
            "wasm glue (the generator on aterm-wasm-abi's toy, its goldens, in a browser)",
            Lane::CellsTarget,
        ),
        spec(
            StageId::BrowserLane,
            "browser lane (both web modules in headless Chromium, Firefox and WebKit: WebGL2, GPU vs CPU pixels)",
            Lane::CellsTarget,
        ),
        spec(
            StageId::MeasurePrebuild,
            "measuring rows' compile (paint and spin, --no-run)",
            Lane::MainTarget,
        ),
        exclusive(
            StageId::MeasuringTests,
            "measuring tests (paint and spin over the handed bundle; run alone)",
            Lane::MainTarget,
        ),
        exclusive(StageId::GuiSmoke, "gui typing-pacing smoke", Lane::Pure),
        spec(
            StageId::RedrawConformance,
            "control redraw conformance (a select repaints a real window)",
            Lane::MainTarget,
        ),
        spec(
            StageId::ObjcDrives,
            "objc drives (live-class audit, IME, toolbar, window and event, on a real AppKit)",
            Lane::MainTarget,
        ),
        exclusive(
            StageId::CodexAlabProof,
            "codex-alab proof (the native harness end to end on the real package; run alone)",
            Lane::MainTarget,
        ),
        exclusive(
            StageId::LinuxLane,
            "Linux runtime lane (a lima VM: tests on aarch64 and x86_64, aterm-gui under four \
             display servers; run alone)",
            Lane::MainTarget,
        ),
    ]
}

fn spec(id: StageId, title: impl Into<String>, lane: Lane) -> StageSpec {
    StageSpec {
        id,
        title: title.into(),
        lane,
        exclusive: false,
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

    /// EVERY SIDE LANE IS A `*.noindex` FOLDER Spotlight skips: under the run's
    /// `<target>/verify` (the caller's `CARGO_TARGET_DIR` moves it), the libc
    /// oracle's under the checkout whatever the target is; the main lanes share
    /// the target dir itself.
    #[test]
    fn every_side_lane_is_a_folder_spotlight_skips() {
        for (env, target) in [
            (EnvSnapshot::default(), PathBuf::from("/repo/target")),
            (
                EnvSnapshot {
                    cargo_target_dir: Some(PathBuf::from("/fast/t")),
                    ..EnvSnapshot::default()
                },
                PathBuf::from("/fast/t"),
            ),
        ] {
            let c = Ctx::new(
                PathBuf::from("/repo"),
                Scope::workspace(),
                env,
                PathBuf::from("/tmp"),
            );
            let lane = |l| lane_dir(&c, l).expect("a directory");
            for (l, name) in [
                (Lane::TippyTarget, "tippy.noindex"),
                (Lane::CheckersTarget, "checkers.noindex"),
                (Lane::GpuOracleTarget, "gpu-oracle.noindex"),
                (Lane::TrustTarget, "trust.noindex"),
                (Lane::CellsTarget, "cells.noindex"),
            ] {
                assert_eq!(lane(l), target.join("verify").join(name), "{l:?}");
            }
            assert_eq!(
                lane(Lane::LibcOracleTarget),
                PathBuf::from("/repo/libc-oracle/target.noindex")
            );
            assert_eq!(lane(Lane::MainTarget), target);
            assert_eq!(lane_dir(&c, Lane::Pure), None);
        }
    }

    /// Each check plans its own stages, nothing else; every combination the
    /// CLI offers waits only on earlier stages.
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
            crate::sched::check_waits(&p)
                .unwrap_or_else(|e| panic!("{checks:?} could deadlock: {e}"));
            let mut want = crate::checks::stages_of(&checks);
            want.sort_by_key(|id| *id as u8);
            let mut got: Vec<StageId> = p.iter().map(|s| s.id).collect();
            got.sort_by_key(|id| *id as u8);
            assert_eq!(got, want, "{checks:?}");
        }
    }

    /// The test compile starts at t0 and the test run as soon as it ends: no
    /// other lane holds either. The doctests await exactly the test compile,
    /// then run beside the test run.
    #[test]
    fn the_test_run_follows_its_compile_and_waits_for_nothing_else() {
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
        assert!(
            ready(&p, &none, &none, 0, compile),
            "the compile starts at t0"
        );
        assert!(p[compile].after_stages.is_empty() && p[test].after_stages.is_empty());
        assert!(
            !ready(&p, &none, &none, 0, test),
            "the test run follows its compile"
        );
        let mut done = none.clone();
        done[compile] = true;
        let mut started = done.clone();
        assert!(ready(&p, &started, &done, 0, test));
        assert!(ready(&p, &started, &done, 0, doc));
        started[test] = true;
        assert!(ready(&p, &started, &done, 1, doc));
    }

    /// The measuring rows compile in the main lane before the first barrier,
    /// so the exclusive stage that runs them compiles nothing; appkit's rows
    /// build and drive from the main lane behind it.
    #[test]
    fn the_measuring_compile_precedes_the_barrier_and_appkit_drives_the_main_target() {
        let p = plan(&ctx(&RELEASE, Scope::workspace()));
        let at = |id| p.iter().position(|s| s.id == id).expect("planned");
        let prebuild = at(StageId::MeasurePrebuild);
        let barrier = p.iter().position(|s| s.exclusive).expect("a barrier");
        assert_eq!(p[barrier].id, StageId::MeasuringTests);
        assert!(prebuild < barrier && !p[prebuild].exclusive);
        assert_eq!(p[prebuild].lane, Lane::MainTarget);
        assert_eq!(p[barrier].lane, Lane::MainTarget);
        for id in [StageId::RedrawConformance, StageId::ObjcDrives] {
            assert!(barrier < at(id), "{id:?} runs behind the barrier");
            assert_eq!(p[at(id)].lane, Lane::MainTarget, "{id:?}");
        }
        assert_eq!(
            ids(&ctx(&[Check::Measure], Scope::workspace())),
            [
                StageId::MeasurePrebuild,
                StageId::MeasuringTests,
                StageId::GuiSmoke
            ]
        );
    }

    /// The release's feature builds compile beside the test run and run their
    /// tests after it, one suite at a time: the gpu oracle's, then the
    /// checkers'. Alone, each runs straight after its compile.
    #[test]
    fn the_feature_builds_compile_beside_the_test_run_and_test_after_it() {
        use crate::sched::{awaited, ready};
        let p = plan(&ctx(&RELEASE, Scope::workspace()));
        let at = |id| p.iter().position(|s| s.id == id).expect("planned");
        let none = vec![false; p.len()];
        for compile in [StageId::GpuOracleCompile, StageId::CheckersCompile] {
            assert!(ready(&p, &none, &none, 0, at(compile)), "{compile:?} at t0");
        }
        let (test, oracle, checkers) = (
            at(StageId::Test),
            at(StageId::GpuOracle),
            at(StageId::SpecCheckers),
        );
        assert_eq!(awaited(&p, oracle).collect::<Vec<_>>(), [test]);
        assert_eq!(awaited(&p, checkers).collect::<Vec<_>>(), [test, oracle]);
        let mut done = none.clone();
        for id in [
            StageId::TestCompile,
            StageId::GpuOracleCompile,
            StageId::CheckersCompile,
        ] {
            done[at(id)] = true;
        }
        assert!(
            !ready(&p, &done, &done, 0, oracle),
            "the oracle waits for the test run"
        );
        done[test] = true;
        assert!(ready(&p, &done, &done, 0, oracle));
        assert!(
            !ready(&p, &done, &done, 0, checkers),
            "the checkers wait for the oracle"
        );
        done[oracle] = true;
        assert!(ready(&p, &done, &done, 0, checkers));
        for alone in [Check::Checkers, Check::GpuOracle] {
            let p = plan(&ctx(&[alone], Scope::workspace()));
            assert!(p.iter().all(|s| s.after_stages.is_empty()), "{alone:?}");
        }
    }
}
