// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verification stage definitions, lane assignments, and execution order.

use std::path::PathBuf;

use aterm_toolchain::build_cache::noindex;

use crate::Ctx;
use crate::scope::Scope;

/// Target directory name under `<root>/libc-oracle` ([`Lane::LibcOracleTarget`]).
pub const LIBC_ORACLE_TARGET: &str = "target.noindex";

/// Contended target-directory lane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    /// Touches no target directory.
    Pure,
    /// Caller's target directory ([`crate::Ctx::main_build_dir`]).
    MainTarget,
    /// Target directory for stages awaiting [`StageId::TestCompile`] alongside [`StageId::Test`].
    AfterCompile,
    /// `<target>/verify/.ubd/tippy.noindex` for lint builds ([`lanes_dir`]).
    TippyTarget,
    /// `<target>/verify/.ubd/checkers.noindex` for `--features aterm-spec/checkers` test builds.
    CheckersTarget,
    /// `<target>/verify/.ubd/trust.noindex` for `ship check --export` release tools.
    TrustTarget,
    /// `<target>/verify/.ubd/cells.noindex` for cross-target cells, wasm modules, and browser runs.
    CellsTarget,
    /// `<root>/libc-oracle/target.noindex` for the per-checkout libc oracle workspace.
    LibcOracleTarget,
}

/// Verification stage identifier.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StageId {
    /// Compiles test binaries at `t0` so doctests can run beside [`Self::Test`].
    TestCompile,
    Test,
    Doctests,
    Tippy,
    Formatting,
    /// Publication engine export guards (`ship check --export`).
    Export,
    LibcOracle,
    /// Source guards and L0 censuses ([`aterm_census::CENSUSES`]).
    Census,
    /// Trust checkers over registered formal models (`spec-check`).
    SpecCheck,
    /// Holds this host's GPU arm to its CPU twins, with the record of wgpu's frames a signal,
    /// after [`Self::Test`], on its build in the main target.
    GpuOracle,
    /// Compiles `aterm-spec/checkers` tests (`--no-run`) in [`Lane::CheckersTarget`].
    CheckersCompile,
    /// Runs `aterm-spec/checkers` tests after [`Self::Test`] and [`Self::GpuOracle`].
    SpecCheckers,
    /// Prebuilds measuring test binaries (`--no-run`) before the first exclusive barrier.
    MeasurePrebuild,
    /// Paint and spin matrices over the release bundle, run exclusively.
    MeasuringTests,
    GuiSmoke,
    RedrawConformance,
    /// macOS Objective-C live-window drivers (`stages::OBJC_DRIVES`).
    ObjcDrives,
    /// Cross-target cells linted for their own triples ([`crate::cells`]).
    ForeignCells,
    /// Web modules' host boundary in headless Chromium ([`crate::wasm_smoke`]).
    WasmSmoke,
    /// JS glue generator on `aterm-wasm-abi`'s toy module ([`crate::wasm_glue`]).
    WasmGlue,
    /// Web modules in headless Chromium, Firefox, and WebKit (`crate::browser`).
    BrowserLane,
    Forge,
    /// End-to-end `codex-alab` native harness (`lab codex-alab-proof`), run exclusively.
    CodexAlabProof,
    /// Linux runtime lane (`lab linux proof`) in a Lima VM, run exclusively.
    LinuxLane,
}

/// Stages that open a desktop window, paired with their `--no-windows` skip names.
pub const WINDOWED: [(StageId, &str); 3] = [
    (StageId::GuiSmoke, "gui smoke"),
    (StageId::RedrawConformance, "redraw conformance"),
    (StageId::ObjcDrives, "objc drives"),
];

/// Stages that run under `--no-windows` and emit named skips for their windowed rows.
pub const WINDOWED_ROWS: [StageId; 2] = [StageId::BrowserLane, StageId::MeasuringTests];

/// Stages whose verdicts depend on host timing: each runs on the quiet machine, a claim in the
/// per-user capacity ledger (`docs/DESIGN-adaptive-build-2026-10-24.md`, QUIET-ADMIT).
pub const TIMED: [StageId; 4] = [
    StageId::MeasuringTests,
    StageId::GuiSmoke,
    StageId::RedrawConformance,
    StageId::ObjcDrives,
];

impl StageId {
    /// Named skip reason under `--no-windows` for a [`WINDOWED`] stage.
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

    /// Whether `--no-windows` alters this stage ([`WINDOWED`] or [`WINDOWED_ROWS`]).
    #[must_use]
    pub fn opens_windows(self) -> bool {
        self.no_windows_skip().is_some() || WINDOWED_ROWS.contains(&self)
    }

    /// Whether this stage measures host timing ([`TIMED`]).
    #[must_use]
    pub fn timed(self) -> bool {
        TIMED.contains(&self)
    }
}

/// Stage identity, log header, and scheduling constraints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageSpec {
    pub id: StageId,
    /// Stage log header.
    pub title: String,
    pub lane: Lane,
    /// Runs with no other stage in flight.
    pub exclusive: bool,
    /// Earlier stages that must finish before this stage starts ([`crate::sched::check_waits`]).
    pub after_stages: Vec<StageId>,
    /// The memory it claims, bytes ([`crate::memory::need`]): no stage that claims any starts
    /// beside others it would take the run past its budget with ([`crate::sched::ready`]). `0`
    /// claims none, and a run that measures nothing ([`Ctx::memory`] `None`) plans `0` for every
    /// stage.
    pub mem: u64,
}

/// The side lanes' build directories ([`SIDE_LANES`]), each `*.noindex`, live in
/// `<target>/verify/.ubd`, a folder whose file events fseventsd drops (`aterm_tempfile::QUIET`):
/// one checkout's lint lane gave it 4,983 file events in a 10-minute census, and a lane under
/// `.ubd` gives it none.
#[must_use]
pub fn lanes_dir(ctx: &Ctx) -> PathBuf {
    ctx.state_dir().join(aterm_tempfile::QUIET)
}

/// The side lanes' names, each a `<name>.noindex` directory in [`lanes_dir`].
pub const SIDE_LANES: [&str; 4] = ["tippy", "checkers", "trust", "cells"];

/// Target directory for `lane`'s cargo invocations, or `None` for [`Lane::Pure`].
#[must_use]
pub fn lane_dir(ctx: &Ctx, lane: Lane) -> Option<PathBuf> {
    let side = |name: &str| Some(lanes_dir(ctx).join(noindex(name)));
    match lane {
        Lane::Pure => None,
        Lane::MainTarget | Lane::AfterCompile => Some(ctx.target_dir.clone()),
        Lane::TippyTarget => side("tippy"),
        Lane::CheckersTarget => side("checkers"),
        Lane::TrustTarget => side("trust"),
        Lane::CellsTarget => Some(cells_dir(ctx)),
        Lane::LibcOracleTarget => Some(ctx.root.join("libc-oracle").join(LIBC_ORACLE_TARGET)),
    }
}

/// Directory for [`Lane::CellsTarget`] builds and browser frames.
#[must_use]
pub fn cells_dir(ctx: &Ctx) -> PathBuf {
    lanes_dir(ctx).join(noindex("cells"))
}

/// THE SIDE LANES' OLD PLACE, emptied: each `<target>/verify/<lane>.noindex` the lanes built in
/// before they moved into [`lanes_dir`] goes whole once no build holds its cargo locks, and what
/// an earlier run set aside of it and could not finish (`<lane>.noindex.removing`,
/// `<lane>.noindex.removing-<n>`) is tried again, whether or not the lane is still there
/// (`atpkg::build_cache::remove_unbuilt`): one line each for the log, and none for a lane with
/// nothing of it left; one a build holds stays for a later run. Nothing of this version builds
/// there.
#[cfg(unix)]
#[must_use]
pub fn retire_old_lanes(ctx: &Ctx) -> Vec<String> {
    retire_old_lanes_with(ctx, &atpkg::build_cache::remove_unbuilt)
}

#[cfg(unix)]
fn retire_old_lanes_with(
    ctx: &Ctx,
    remove: &dyn Fn(&std::path::Path) -> std::io::Result<bool>,
) -> Vec<String> {
    let mut said = Vec::new();
    for name in SIDE_LANES {
        let old = ctx.state_dir().join(noindex(name));
        let there = std::fs::symlink_metadata(&old).is_ok_and(|m| m.is_dir());
        said.push(match remove(&old) {
            Ok(true) => format!(
                "removed {}: the {name} lane builds in {} now, out of fseventsd's stream",
                old.display(),
                lanes_dir(ctx).display()
            ),
            Ok(false) if there => format!(
                "kept {}: a build or process still uses it, or ownership is unreadable; a later run checks it again",
                old.display()
            ),
            Ok(false) => continue,
            Err(e) => format!(
                "{}: not all of it could be removed ({e}); a later run tries again",
                old.display()
            ),
        });
    }
    said
}

/// Whether stage `id` is a named skip in `ctx` before it starts anything: a windowed stage
/// ([`WINDOWED`]) under `--no-windows`, or one that measures the handed bundle (the measuring
/// tests, the gui smoke) with no `--app`. It claims no memory and takes no quiet machine.
#[must_use]
pub fn named_skip(ctx: &Ctx, id: StageId) -> bool {
    let needs_app = matches!(id, StageId::MeasuringTests | StageId::GuiSmoke);
    (ctx.no_windows && id.no_windows_skip().is_some())
        || (needs_app && ctx.measure_artifact.is_none())
}

/// Planned stages for `ctx.checks` ([`crate::checks::stages_of`]) in declared order, each with
/// the memory it claims by the run's book ([`StageSpec::mem`]): none for a named skip
/// ([`named_skip`]), which starts nothing.
#[must_use]
pub fn plan(ctx: &Ctx) -> Vec<StageSpec> {
    let mut wanted = crate::checks::stages_of(&ctx.checks);
    if ctx.scope.selects_nothing() {
        wanted.retain(|id| *id != StageId::TestCompile);
    }
    let mut v: Vec<StageSpec> = declared(&ctx.scope)
        .into_iter()
        .filter(|s| wanted.contains(&s.id))
        .collect();
    let present: Vec<StageId> = v.iter().map(|s| s.id).collect();
    let records = ctx.memory.as_ref().map(crate::memory::Book::records);
    for s in &mut v {
        s.after_stages.retain(|id| present.contains(id));
        if let (Some(book), Some(records)) = (&ctx.memory, &records)
            && !named_skip(ctx, s.id)
        {
            let key = crate::memory::Key::of_run(s.id, ctx);
            s.mem = crate::memory::need(records, s, &key, book.physical).bytes;
        }
        // Independent build/test lanes would each spend the same CPU grant.
        // Keep every stage and its bounds, but let a managed run spend it once.
        if ctx.env.build_slot_held.is_some() {
            s.exclusive = true;
        }
    }
    v
}

/// Every declared stage in log order.
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
        StageSpec {
            after_stages: vec![StageId::Test],
            ..spec(
                StageId::GpuOracle,
                "gpu oracle (aterm-gpu's frame record: this host's arm against wgpu's \
                 recorded frames; after the test run, on its build)",
                Lane::MainTarget,
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
            "measuring rows' compile (paint, spin, aos and rewind, --no-run)",
            Lane::MainTarget,
        ),
        exclusive(
            StageId::MeasuringTests,
            "measuring tests (paint, spin, aos and rewind over the handed bundle; run alone)",
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
        mem: 0,
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
                (Lane::TrustTarget, "trust.noindex"),
                (Lane::CellsTarget, "cells.noindex"),
            ] {
                let quiet = target.join("verify").join(".ubd").join(name);
                assert_eq!(lane(l), quiet, "{l:?}");
                assert!(aterm_tempfile::is_quiet(&quiet), "{quiet:?}");
            }
            assert_eq!(
                lane(Lane::LibcOracleTarget),
                PathBuf::from("/repo/libc-oracle/target.noindex")
            );
            assert_eq!(lane(Lane::MainTarget), target);
            assert!(
                !aterm_tempfile::is_quiet(&target),
                "the caller's target is theirs"
            );
            assert_eq!(lane_dir(&c, Lane::Pure), None);
        }
    }

    /// WHAT AN EARLIER RUN SET ASIDE OF AN OLD LANE IS TRIED AGAIN, THE LANE GONE OR NOT: an aside
    /// of the old tippy lane keeping one entry its owner flagged `uchg` (the remover never clears
    /// a flag) loses the rest and is said, naming it, though no `tippy.noindex` is left; once the
    /// flag is off, the next run removes it and says nothing. NEGATIVE CONTROL: the flag refuses
    /// `std::fs::remove_file`.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_old_lanes_stuck_aside_is_tried_again_once_the_lane_is_gone() {
        /// Clears the flag when dropped, so a failed test leaves its scratch removable.
        struct Unflag(PathBuf);
        impl Drop for Unflag {
            fn drop(&mut self) {
                let _ = atpkg::platform::set_file_flags(&self.0, 0);
            }
        }
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let target = std::fs::canonicalize(dir.path())
            .expect("resolved")
            .join("target");
        let c = Ctx::new(
            PathBuf::from("/repo"),
            Scope::workspace(),
            EnvSnapshot {
                cargo_target_dir: Some(target),
                ..EnvSnapshot::default()
            },
            PathBuf::from("/tmp"),
        );
        let aside = c.state_dir().join("tippy.noindex.removing");
        let stuck = aside.join("debug/stuck");
        std::fs::create_dir_all(stuck.parent().expect("its folder")).expect("a folder");
        std::fs::write(&stuck, "x").expect("written");
        std::fs::write(aside.join("debug/free"), "x").expect("written");
        let _unflag = Unflag(stuck.clone());
        atpkg::platform::set_file_flags(&stuck, libc::UF_IMMUTABLE).expect("uchg");
        assert!(std::fs::remove_file(&stuck).is_err(), "the flag refuses it");
        assert!(
            c.state_dir()
                .join("tippy.noindex")
                .symlink_metadata()
                .is_err()
        );

        let retire = || {
            retire_old_lanes_with(&c, &|path| {
                atpkg::build_cache::remove_unbuilt_with_processes(path, &|_, _| {
                    Ok(aterm_uds::holders::PathScan::default())
                })
            })
        };
        let said = retire();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(said[0].contains(&*aside.to_string_lossy()), "{said:?}");
        assert!(stuck.is_file() && !aside.join("debug/free").exists());
        atpkg::platform::set_file_flags(&stuck, 0).expect("the flag off");
        assert_eq!(retire(), Vec::<String>::new());
        assert!(aside.symlink_metadata().is_err(), "the aside went");
    }

    /// A run that measures memory plans each stage's claim from its book: the test compile its
    /// kept peak and margin, the lint (never measured) the cold figure, the test run (which
    /// compiles nothing) and the formatter (which touches no target directory) nothing.
    /// NEGATIVE CONTROL: a run that measures nothing plans no claim for any stage.
    #[test]
    fn a_run_that_measures_memory_plans_each_stages_claim_from_its_book() {
        const GIB: u64 = 1 << 30;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let book = crate::memory::Book::at(
            Some(dir.path().join("stage-memory.json")),
            Some(24 * GIB),
            Some(43 * GIB / 2),
        );
        let scope = Scope::crate_only("aterm-gui");
        let compile = declared(&scope)
            .into_iter()
            .find(|s| s.id == StageId::TestCompile)
            .expect("declared");
        let checks = [Check::Test, Check::Lint, Check::Fmt];
        let key = crate::memory::Key::of_run(compile.id, &ctx(&checks, scope.clone()));
        book.keep(&compile, &key, 10 * GIB).expect("kept");
        let claims = |c: &Ctx| -> Vec<(StageId, u64)> {
            plan(c).into_iter().map(|s| (s.id, s.mem)).collect()
        };
        assert_eq!(
            claims(&ctx(&checks, scope.clone()).with_memory(book)),
            [
                (StageId::TestCompile, 12 * GIB),
                (StageId::Test, 0),
                (StageId::Tippy, 12 * GIB),
                (StageId::Formatting, 0),
            ]
        );
        assert!(
            claims(&ctx(&checks, scope))
                .iter()
                .all(|(_, mem)| *mem == 0)
        );
    }

    /// A named skip plans no memory: under `--no-windows` the redraw conformance and the objc
    /// drives, which would compile and so claim the cold figure, are skipped before they start
    /// anything and claim nothing. NEGATIVE CONTROL: the same run with windows plans the cold 12
    /// GiB of a 24 GiB Mac for each.
    #[test]
    fn a_named_skip_plans_no_memory() {
        const GIB: u64 = 1 << 30;
        let book = || crate::memory::Book::at(None, Some(24 * GIB), Some(43 * GIB / 2));
        let claims = |no_windows: bool| -> Vec<(StageId, u64)> {
            let c = ctx(&[Check::Appkit], Scope::workspace())
                .with_no_windows(no_windows)
                .with_memory(book());
            plan(&c).into_iter().map(|s| (s.id, s.mem)).collect()
        };
        assert_eq!(
            claims(true),
            [(StageId::RedrawConformance, 0), (StageId::ObjcDrives, 0)]
        );
        assert_eq!(
            claims(false),
            [
                (StageId::RedrawConformance, 12 * GIB),
                (StageId::ObjcDrives, 12 * GIB)
            ]
        );
    }

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
        assert!(!ready(&p, &none, &none, 0, doc, u64::MAX));
        assert!(
            ready(&p, &none, &none, 0, compile, u64::MAX),
            "the compile starts at t0"
        );
        assert!(p[compile].after_stages.is_empty() && p[test].after_stages.is_empty());
        assert!(
            !ready(&p, &none, &none, 0, test, u64::MAX),
            "the test run follows its compile"
        );
        let mut done = none.clone();
        done[compile] = true;
        let mut started = done.clone();
        assert!(ready(&p, &started, &done, 0, test, u64::MAX));
        assert!(ready(&p, &started, &done, 0, doc, u64::MAX));
        started[test] = true;
        assert!(ready(&p, &started, &done, 1, doc, u64::MAX));
    }

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

    #[test]
    fn the_feature_builds_compile_beside_the_test_run_and_test_after_it() {
        use crate::sched::{awaited, ready};
        let p = plan(&ctx(&RELEASE, Scope::workspace()));
        let at = |id| p.iter().position(|s| s.id == id).expect("planned");
        let none = vec![false; p.len()];
        assert!(
            ready(&p, &none, &none, 0, at(StageId::CheckersCompile), u64::MAX),
            "the checkers compile at t0"
        );
        assert_eq!(
            p[at(StageId::GpuOracle)].lane,
            Lane::MainTarget,
            "the oracle runs on the test run's build"
        );
        let (test, oracle, checkers) = (
            at(StageId::Test),
            at(StageId::GpuOracle),
            at(StageId::SpecCheckers),
        );
        assert_eq!(awaited(&p, oracle).collect::<Vec<_>>(), [test]);
        assert_eq!(awaited(&p, checkers).collect::<Vec<_>>(), [test, oracle]);
        let mut done = none.clone();
        for id in [StageId::TestCompile, StageId::CheckersCompile] {
            done[at(id)] = true;
        }
        assert!(
            !ready(&p, &done, &done, 0, oracle, u64::MAX),
            "the oracle waits for the test run"
        );
        done[test] = true;
        assert!(ready(&p, &done, &done, 0, oracle, u64::MAX));
        assert!(
            !ready(&p, &done, &done, 0, checkers, u64::MAX),
            "the checkers wait for the oracle"
        );
        done[oracle] = true;
        assert!(ready(&p, &done, &done, 0, checkers, u64::MAX));
        for alone in [Check::Checkers, Check::GpuOracle] {
            let p = plan(&ctx(&[alone], Scope::workspace()));
            assert!(p.iter().all(|s| s.after_stages.is_empty()), "{alone:?}");
        }
    }
}
