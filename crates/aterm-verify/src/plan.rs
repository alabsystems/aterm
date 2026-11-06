// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verification stage definitions, lane assignments, and execution order.

use std::path::PathBuf;

use aterm_toolchain::build_cache::noindex;

use crate::Ctx;
use crate::memory::StageKey as _;
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
    /// `tippy.noindex` under [`lanes_dir`] for lint builds.
    TippyTarget,
    /// `checkers.noindex` under [`lanes_dir`] for `--features aterm-spec/checkers` test builds.
    CheckersTarget,
    /// `trust.noindex` under [`lanes_dir`] for `ship check --export` release tools.
    TrustTarget,
    /// `cells.noindex` under [`lanes_dir`] for cross-target cells, wasm modules, and browser runs.
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
    /// Compiles `aterm-spec/checkers` tests (`--no-run`) in [`Lane::CheckersTarget`].
    CheckersCompile,
    /// Runs `aterm-spec/checkers` tests after [`Self::Test`].
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
    /// Web modules in headless Chromium, Firefox, WebKit, and the system Safari (`crate::browser`).
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
    /// The call of a shaped test compile it is ([`crate::shape::Part`]), or why its test compile
    /// was not shaped (a part of one call); `None` for every other stage.
    pub part: Option<crate::shape::Part>,
}

/// The side lanes' build directories ([`SIDE_LANES`]), each `*.noindex`, live under
/// the managed build cache when the shim selects it, so removing a worktree does not
/// discard its expensive compiler artifacts. Caller-selected layouts keep them in
/// `<target>/verify/.ubd`. Both use a folder whose file events fseventsd drops (`aterm_tempfile::QUIET`):
/// one checkout's lint lane gave it 4,983 file events in a 10-minute census, and a lane under
/// `.ubd` gives it none.
#[must_use]
pub fn lanes_dir(ctx: &Ctx) -> PathBuf {
    ctx.cached_lanes_dir
        .clone()
        .unwrap_or_else(|| ctx.state_dir().join(aterm_tempfile::QUIET))
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
/// nothing of it left; one a build or process holds, or retention keeps, stays for a later run.
/// Nothing of this version builds there.
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
                "kept {} or what was set aside of it: a build holds its cargo locks, a process \
                 uses it or cannot be ruled out, or retention keeps it; a later run checks it again",
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

/// The stages `ctx` runs, unordered: its checks' ([`crate::checks::stages_of`]), only the
/// compiles under `--compile-only`, only the burst under `--cursor-pacing`, no test compile over
/// an empty selection, and no measuring rows' compile where the test compile builds them or no
/// matrix will run ([`folds_measuring`], [`named_skip`]).
fn wanted(ctx: &Ctx) -> Vec<StageId> {
    let mut wanted = crate::checks::stages_of(&ctx.checks);
    if ctx.compile_only {
        wanted.retain(|id| matches!(id, StageId::TestCompile | StageId::CheckersCompile));
    }
    if ctx.cursor_pacing {
        wanted.retain(|id| *id == StageId::GuiSmoke);
    }
    if ctx.scope.selects_nothing() {
        wanted.retain(|id| *id != StageId::TestCompile);
    }
    if folds(ctx, &wanted) || named_skip(ctx, StageId::MeasuringTests) {
        wanted.retain(|id| *id != StageId::MeasurePrebuild);
    }
    wanted
}

/// Whether the test compile also builds the measuring matrices' benches
/// ([`crate::stages::measuring_bench_args`]): it is the workspace's and the release's own
/// (`release`, `--as-release`: [`Ctx::checkers_in_tests`]). `ship release` runs that compile
/// first (`verify test --all --compile-only --as-release`), so the release's `verify` finds the
/// benches built, and a red row of it reruns as `--as-release`, which builds them again. Their
/// units are the test compile's: `test --bench` and `bench --profile test` build a bench under the
/// same profile in the same compile mode, the workspace's features unify alike for both, and the
/// `aterm-spec/checkers` feature the release's compile carries reaches none of them. So no
/// prebuild row is planned, and the measuring stage, alone on the quiet machine, finds its benches
/// built or does not run ([`crate::stages`]' `measuring_tests`). Any other plan that measures
/// leaves them to the prebuild row ([`StageId::MeasurePrebuild`]): a dev run's red test compile
/// reruns as `verify test`, which builds no bench.
#[must_use]
pub fn folds_measuring(ctx: &Ctx) -> bool {
    folds(ctx, &wanted(ctx))
}

fn folds(ctx: &Ctx, wanted: &[StageId]) -> bool {
    ctx.checkers_in_tests && ctx.scope.is_workspace() && wanted.contains(&StageId::TestCompile)
}

/// Planned stages for `ctx.checks` ([`wanted`]) in declared order, each with the memory it claims
/// by the run's book ([`StageSpec::mem`]): none for a named skip ([`named_skip`]), which starts
/// nothing.
#[must_use]
pub fn plan(ctx: &Ctx) -> Vec<StageSpec> {
    let wanted = wanted(ctx);
    let v: Vec<StageSpec> = declared(&ctx.scope)
        .into_iter()
        .filter(|s| wanted.contains(&s.id))
        .collect();
    let present: Vec<StageId> = v.iter().map(|s| s.id).collect();
    let measures = present.contains(&StageId::MeasuringTests);
    let records = ctx
        .memory
        .as_ref()
        .map(|b| (b.records(), b.plain_records()));
    let mut planned = Vec::with_capacity(v.len());
    for mut s in v {
        s.after_stages.retain(|id| present.contains(id));
        if measures && matches!(s.id, StageId::RedrawConformance | StageId::ObjcDrives) {
            s.exclusive = true;
        }
        if let (Some(book), Some((records, plain))) = (&ctx.memory, &records)
            && !named_skip(ctx, s.id)
        {
            let key = crate::memory::Key::of_stage(&s, ctx);
            s.mem = crate::memory::need(records, plain, &s, &key, book.physical).bytes;
            if s.id == StageId::TestCompile && !ctx.scope.is_workspace() {
                let checkout = ctx.root.display().to_string();
                let shaping = crate::shape::Shaping {
                    records,
                    plain,
                    checkout: &checkout,
                    budget: book.budget(),
                };
                match crate::shape::shape(ctx.scope.crates(), &shaping, &mut |p| {
                    (ctx.unit_graph)(ctx, p)
                }) {
                    crate::shape::Shaped::Calls(calls) => {
                        planned.extend(calls_of(ctx, &s, calls));
                        continue;
                    }
                    crate::shape::Shaped::Whole(Some(e)) => at_least(ctx, &mut s, &e, book),
                    crate::shape::Shaped::Whole(None) => {}
                }
            }
        }
        planned.push(s);
    }
    for s in &mut planned {
        // Independent build/test lanes would each spend the same CPU grant.
        // Keep every stage and its bounds, but let a managed run spend it once.
        if ctx.env.build_slot_held.is_some() {
            s.exclusive = true;
        }
    }
    planned
}

/// The unshaped test compile `s` claims at least its estimate `e` ([`crate::shape::Shaped::Whole`])
/// and a fifth more: its stage's lookup ([`crate::memory::need`]) reads its checkout's record
/// first, however light (a run after a plain build had compiled it), and never the narrower
/// selections it holds, where the estimate reads every checkout's record and their cover. Where
/// that raises its claim, its time line says by what.
fn at_least(ctx: &Ctx, s: &mut StageSpec, e: &crate::shape::Estimate, book: &crate::memory::Book) {
    let floor = aterm_capacity::peak_claim(e.peak);
    if floor <= s.mem {
        return;
    }
    s.part = Some(crate::shape::Part::whole(
        crate::shape::Piece::of(ctx.scope.crates(), false),
        format!(
            "by {} and a fifth more, more than the {} its stage's lookup found; under the run's {} \
             budget, so not shaped",
            e.from,
            aterm_capacity::size(s.mem),
            aterm_capacity::size(book.budget())
        ),
    ));
    s.mem = floor;
}

/// The test compile `s` as the calls its planner made ([`crate::shape::shape`]), in its place and
/// lane: each its own stage, with its own claim and part, titled `test compile (<its selection>),
/// call i of n`; one call keeps the stage's title, with the claim and the reason it was not
/// shaped.
fn calls_of(ctx: &Ctx, s: &StageSpec, calls: Vec<crate::shape::Call>) -> Vec<StageSpec> {
    calls
        .into_iter()
        .map(|call| {
            let title = if call.part.shaped() {
                let label = if call.part.closing() {
                    ctx.scope.label()
                } else {
                    call.part.piece.label()
                };
                format!(
                    "test compile ({label}), call {} of {}",
                    call.part.index + 1,
                    call.part.of
                )
            } else {
                s.title.clone()
            };
            StageSpec {
                title,
                mem: call.mem,
                part: Some(call.part),
                ..s.clone()
            }
        })
        .collect()
}

/// Every declared stage in log order.
#[must_use]
pub fn declared(scope: &Scope) -> Vec<StageSpec> {
    let label = scope.label();
    let checker_label = crate::stages::spec_checkers_scope(scope).label();
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
            format!("tippy lint ({})", scope.lint_label()),
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
            StageId::CheckersCompile,
            format!("checkers compile ({checker_label} with aterm-spec/checkers, --no-run)"),
            Lane::CheckersTarget,
        ),
        StageSpec {
            after_stages: vec![StageId::Test],
            ..spec(
                StageId::SpecCheckers,
                format!(
                    "checkers ({checker_label} with aterm-spec/checkers: per-step ty \
                 agreement; after the test run)"
                ),
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
            "browser lane (both web modules in headless Chromium, Firefox and WebKit, and in Safari: WebGL2, GPU vs CPU pixels)",
            Lane::CellsTarget,
        ),
        spec(
            StageId::MeasurePrebuild,
            "measuring rows' compile (paint, spin, aos and rewind, --no-run, where the release's \
             test compile does not build them)",
            Lane::MainTarget,
        ),
        // Report live UI defects before the longer performance matrices. The
        // planner isolates these when the run also selects measurements.
        spec(
            StageId::RedrawConformance,
            "control redraw conformance (a select repaints a real window)",
            Lane::MainTarget,
        ),
        spec(
            StageId::ObjcDrives,
            "objc drives (live-class audit, IME, toolbar, window, event and frame clock, on a real \
             AppKit)",
            Lane::MainTarget,
        ),
        exclusive(
            StageId::MeasuringTests,
            "measuring tests (paint, spin, aos and rewind over the handed bundle; run alone)",
            Lane::MainTarget,
        ),
        exclusive(StageId::GuiSmoke, "gui typing-pacing smoke", Lane::Pure),
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
        part: None,
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

    pub(super) fn ctx(checks: &[Check], scope: Scope) -> Ctx {
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

    #[test]
    fn compile_only_uses_the_normal_test_compile_without_running_tests() {
        let normal = ctx(&[Check::Test], Scope::Workspace);
        let expected = plan(&normal)
            .into_iter()
            .find(|s| s.id == StageId::TestCompile)
            .unwrap();
        let focused = normal.with_compile_only(true);
        let planned = plan(&focused);
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].id, expected.id);
        assert_eq!(planned[0].mem, expected.mem);
        assert_eq!(
            focused.rerun(Check::Test),
            "targo --unverified verify test --all --compile-only"
        );
        assert!(
            plan(&ctx(&[Check::Test], Scope::Workspace))
                .iter()
                .any(|s| s.id == StageId::Test)
        );
    }

    /// A release's red `test` or `doc` row prints a command the CLI takes and that runs the check
    /// as the release did: without `--as-release` it carries no feature and owes no golden test,
    /// so what failed there would pass. CONTROL: a plain run's and another check's name no flag.
    #[test]
    fn a_releases_red_test_row_reruns_as_the_release_ran() {
        let release =
            ctx(&[Check::Test, Check::Doc, Check::Lint], Scope::Workspace).with_release(true);
        for check in [Check::Test, Check::Doc] {
            let printed = release.rerun(check);
            assert_eq!(
                printed,
                format!(
                    "targo --unverified verify {} --all --as-release",
                    check.name()
                )
            );
            let words = printed
                .strip_prefix("targo --unverified verify ")
                .expect("the verb")
                .split(' ');
            let parsed = crate::cli::parse(words).expect("the CLI takes what the row prints");
            assert!(parsed.as_release && parsed.all && parsed.checks == [check]);
            assert_eq!(
                parsed.wanted(|p| Scope::Workspace.includes_crate(p)),
                [check],
                "no release runs what `test` would pull"
            );
        }
        let without = crate::cli::parse(["test", "--all"]).expect("parses");
        assert_eq!(
            without.wanted(|p| Scope::Workspace.includes_crate(p)),
            [Check::Test, Check::WasmSmoke],
            "a plain `test --all` still pulls it"
        );
        let compiled =
            crate::cli::parse(["test", "--all", "--compile-only", "--as-release"]).expect("parses");
        assert_eq!(
            compiled.wanted(|p| Scope::Workspace.includes_crate(p)),
            [Check::Test]
        );
        assert!(!release.rerun(Check::Lint).contains("--as-release"));
        let plain = ctx(&[Check::Test], Scope::Workspace);
        assert_eq!(
            plain.rerun(Check::Test),
            "targo --unverified verify test --all"
        );
        assert_eq!(
            plain
                .with_checkers_in_tests(true)
                .with_compile_only(true)
                .rerun(Check::Test),
            "targo --unverified verify test --all --compile-only --as-release"
        );
    }

    /// A cone's red `lint` row reruns the cone by its base, which runs the same passes: `-p` over
    /// its crates would lint every target of each, and the crates the cone's lint left out.
    /// CONTROL: `test` and `doc` take every crate of the cone, so theirs name them; `-p` and the
    /// workspace rerun as they ran.
    #[test]
    fn a_cones_red_lint_row_reruns_the_cone_by_its_base() {
        let cone = Scope::changed(
            "origin/main",
            vec!["dev".to_string(), "seed".to_string()],
            true,
        )
        .with_all_targets(vec!["seed".to_string()])
        .with_unlinted(vec!["dev".to_string()]);
        let plain = ctx(&[Check::Lint, Check::Test, Check::Doc], cone);
        let printed = plain.rerun(Check::Lint);
        assert_eq!(printed, "targo --unverified verify lint --base origin/main");
        let words = printed
            .strip_prefix("targo --unverified verify ")
            .expect("the verb")
            .split(' ');
        let parsed = crate::cli::parse(words).expect("the CLI takes what the row prints");
        assert_eq!(parsed.checks, [Check::Lint]);
        assert_eq!(parsed.base.as_deref(), Some("origin/main"));
        assert!(parsed.packages.is_empty() && !parsed.all && parsed.narrows());
        for (check, name) in [(Check::Test, "test"), (Check::Doc, "doc")] {
            assert_eq!(
                plain.rerun(check),
                format!("targo --unverified verify {name} -p dev -p seed")
            );
        }
        assert_eq!(
            ctx(&[Check::Lint], Scope::crate_only("seed")).rerun(Check::Lint),
            "targo --unverified verify lint -p seed"
        );
        assert_eq!(
            ctx(&[Check::Lint], Scope::Workspace).rerun(Check::Lint),
            "targo --unverified verify lint --all"
        );
    }

    #[test]
    fn compile_only_keeps_both_test_modes_and_neither_runtime() {
        let full = ctx(&[Check::Test, Check::Checkers], Scope::Workspace);
        let expected: Vec<_> = plan(&full)
            .into_iter()
            .filter(|s| matches!(s.id, StageId::TestCompile | StageId::CheckersCompile))
            .map(|s| (s.id, s.mem, s.lane))
            .collect();
        let focused = full.with_compile_only(true);
        let actual: Vec<_> = plan(&focused)
            .into_iter()
            .map(|s| (s.id, s.mem, s.lane))
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 2);
        assert!(focused.rerun(Check::Checkers).ends_with(" --compile-only"));
        assert!(
            plan(&ctx(&[Check::Test, Check::Checkers], Scope::Workspace))
                .iter()
                .any(|s| s.id == StageId::SpecCheckers)
        );
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

    /// A HEAVY TEST COMPILE IS PLANNED AS ITS CALLS, IN ITS PLACE: on 10:23's book the test
    /// compile over S1 is two stages in the main lane where it stood, before the test run, each
    /// with its own claim and part: S1 less aterm-gui (15.3 GiB), then S1 (at least the budget),
    /// titled `call i of n` with the closing call's title over the run's own selection; the doctests,
    /// which await the test compile, await both. NEGATIVE CONTROL: a run that measures nothing
    /// plans one test compile, whole and unshaped.
    #[test]
    fn a_heavy_test_compile_is_planned_as_its_calls_in_its_place() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let path = dir.path().join("stage-memory.json");
        crate::shape::tests::book_at_1023()
            .save(&path)
            .expect("saved");
        let book = crate::memory::Book::at(
            Some(path),
            Some(24 << 30),
            Some(crate::shape::tests::BUDGET),
        );
        let scope = Scope::packages(&crate::shape::tests::s1());
        let checks = [Check::Test, Check::Doc];
        let mut c = ctx(&checks, scope.clone()).with_memory(book);
        c.unit_graph = crate::shape::tests::fixture_graph;
        let planned = plan(&c);
        let shown: Vec<(StageId, &str, Lane)> = planned
            .iter()
            .map(|s| (s.id, s.title.as_str(), s.lane))
            .collect();
        assert_eq!(
            shown,
            [
                (
                    StageId::TestCompile,
                    "test compile (-p aterm-agent -p aterm-models -p aterm-models-harness -p \
                     aterm-types), call 1 of 2",
                    Lane::MainTarget
                ),
                (
                    StageId::TestCompile,
                    "test compile (-p aterm-agent -p aterm-gui -p aterm-models-harness -p \
                     aterm-models -p aterm-types), call 2 of 2",
                    Lane::MainTarget
                ),
                (
                    StageId::Test,
                    "test (-p aterm-agent -p aterm-gui -p aterm-models-harness -p aterm-models \
                     -p aterm-types)",
                    Lane::MainTarget
                ),
                (
                    StageId::Doctests,
                    "doctests (-p aterm-agent -p aterm-gui -p aterm-models-harness -p \
                     aterm-models -p aterm-types)",
                    Lane::AfterCompile
                ),
            ]
        );
        assert_eq!(
            planned[0].mem,
            aterm_capacity::peak_claim((1272u64 << 30) / 100)
        );
        assert!(planned[1].mem >= crate::shape::tests::BUDGET);
        assert!(
            planned[0]
                .part
                .as_ref()
                .is_some_and(|p| p.shaped() && !p.closing())
        );
        assert!(
            planned[1]
                .part
                .as_ref()
                .is_some_and(crate::shape::Part::closing)
        );
        assert_eq!(
            crate::sched::awaited(&planned, 3).collect::<Vec<_>>(),
            [0, 1]
        );
        crate::sched::check_waits(&planned).expect("each awaits only earlier stages");

        let whole = plan(&ctx(&checks, scope));
        assert_eq!(
            whole
                .iter()
                .filter(|s| s.id == StageId::TestCompile)
                .count(),
            1
        );
        assert!(whole.iter().all(|s| s.part.is_none()));
    }

    /// AN UNSHAPED TEST COMPILE CLAIMS AT LEAST ITS ESTIMATE: `-p aterm-gui -p aterm-lab`, whose
    /// only kept figure is the `-p aterm-gui` it holds (17.63 GiB in aterm-vmem), is not shaped
    /// (21.16 GiB, under the 21.3 GiB budget) and claims that, not the cold 12 GiB its stage's
    /// lookup found; `-p aterm-gui`, whose record here is a fresh run's 32 MiB, claims by
    /// aterm-vmem's 17.63 GiB and not 1.03 GiB, and its time line says so. NEGATIVE CONTROL, by
    /// hand: with the estimate dropped, they claimed 12 GiB and 1.03 GiB.
    #[test]
    fn an_unshaped_test_compile_claims_at_least_its_estimate() {
        const GIB: u64 = 1 << 30;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let path = dir.path().join("stage-memory.json");
        let key = |checkout: &str, crates: &[&str]| crate::memory::Key {
            stage: "TestCompile".to_owned(),
            checkout: checkout.to_owned(),
            crates: Some(crates.iter().map(|c| (*c).to_owned()).collect()),
            variant: None,
        };
        let mut r = crate::memory::Records::default();
        r.push(key("/vmem", &["aterm-gui"]), 1763 * GIB / 100, 1);
        r.save(&path).expect("saved");
        let claim = |crates: &[&str]| -> (u64, Option<String>) {
            let book = crate::memory::Book::at(
                Some(path.clone()),
                Some(24 * GIB),
                Some(crate::shape::tests::BUDGET),
            );
            let crates: Vec<String> = crates.iter().map(|c| (*c).to_owned()).collect();
            let c = ctx(&[Check::Test], Scope::packages(&crates)).with_memory(book);
            let p = plan(&c);
            let s = p
                .iter()
                .find(|s| s.id == StageId::TestCompile)
                .expect("planned");
            (s.mem, s.part.as_ref().map(|p| p.said.clone()))
        };
        let gui = aterm_capacity::peak_claim(1763 * GIB / 100);
        assert!(gui < crate::shape::tests::BUDGET);

        let (mem, said) = claim(&["aterm-gui", "aterm-lab"]);
        assert_eq!(mem, gui);
        assert!(
            said.as_deref()
                .is_some_and(|s| s.contains("the narrower selections it holds, summed")
                    && s.contains("more than the 12 GiB its stage's lookup found")),
            "{said:?}"
        );

        r.push(key("/repo", &["aterm-gui"]), 32 << 20, 2);
        r.save(&path).expect("saved");
        let (mem, said) = claim(&["aterm-gui"]);
        assert_eq!(mem, gui);
        assert!(
            said.as_deref()
                .is_some_and(|s| s.contains("the most `-p aterm-gui` kept in /vmem")),
            "{said:?}"
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
            for app in [false, true] {
                let c = if app {
                    with_app(&checks, Scope::workspace())
                } else {
                    ctx(&checks, Scope::workspace())
                };
                let p = plan(&c);
                crate::sched::check_waits(&p)
                    .unwrap_or_else(|e| panic!("{checks:?} could deadlock: {e}"));
                let mut want = crate::checks::stages_of(&checks);
                // The measuring rows' compile is planned only where their matrices run and the
                // release's test compile does not build them ([`folds_measuring`]).
                want.retain(|id| *id != StageId::MeasurePrebuild || app);
                want.sort_by_key(|id| *id as u8);
                let mut got: Vec<StageId> = p.iter().map(|s| s.id).collect();
                got.sort_by_key(|id| *id as u8);
                assert_eq!(got, want, "{checks:?}, app {app}");
            }
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

    fn with_app(checks: &[Check], scope: Scope) -> Ctx {
        ctx(checks, scope)
            .with_measure_artifact(Some(crate::MeasureArtifact::unnamed("/release/aterm.app")))
    }

    #[test]
    fn the_measuring_compile_precedes_the_barrier_and_appkit_drives_the_main_target() {
        let p = plan(&with_app(&RELEASE, Scope::workspace()).with_release(true));
        let at = |id| p.iter().position(|s| s.id == id).expect("planned");
        let barrier = p.iter().position(|s| s.exclusive).expect("a barrier");
        assert_eq!(p[barrier].id, StageId::RedrawConformance);
        assert_eq!(p[barrier].lane, Lane::MainTarget);
        assert!(
            !p.iter().any(|s| s.id == StageId::MeasurePrebuild),
            "the release's test compile builds the measuring benches"
        );
        assert!(at(StageId::TestCompile) < barrier && !p[at(StageId::TestCompile)].exclusive);
        for id in [StageId::RedrawConformance, StageId::ObjcDrives] {
            assert!(barrier <= at(id), "{id:?} runs at or behind the barrier");
            assert!(at(id) < at(StageId::MeasuringTests));
            assert!(p[at(id)].exclusive);
            assert_eq!(p[at(id)].lane, Lane::MainTarget, "{id:?}");
        }
        let alone = plan(&with_app(&[Check::Measure], Scope::workspace()));
        let alone_ids: Vec<StageId> = alone.iter().map(|s| s.id).collect();
        assert_eq!(
            alone_ids,
            [
                StageId::MeasurePrebuild,
                StageId::MeasuringTests,
                StageId::GuiSmoke
            ]
        );
        let barrier = alone.iter().position(|s| s.exclusive).expect("a barrier");
        assert_eq!(alone[barrier].id, StageId::MeasuringTests);
        assert!(!alone[0].exclusive && alone[0].lane == Lane::MainTarget);
        assert_eq!(
            ids(&ctx(&[Check::Measure], Scope::workspace())),
            [StageId::MeasuringTests, StageId::GuiSmoke],
            "with no bundle no matrix runs, so nothing compiles them"
        );
    }

    #[test]
    fn only_the_releases_test_compile_builds_the_measuring_benches() {
        let prebuilds = |c: &Ctx| ids(c).contains(&StageId::MeasurePrebuild);
        let folding = [
            // A prebuild would be planned here without the fold.
            with_app(&RELEASE, Scope::workspace()).with_release(true),
            // `verify release` with no bundle builds what the release's does.
            ctx(&RELEASE, Scope::workspace()).with_release(true),
            // The compile `ship release` runs first, so its `verify` finds them built, and the
            // command a release's red test row prints.
            ctx(&[Check::Test], Scope::workspace())
                .with_checkers_in_tests(true)
                .with_compile_only(true),
            ctx(&[Check::Test], Scope::workspace()).with_checkers_in_tests(true),
        ];
        for c in &folding {
            assert!(folds_measuring(c), "{:?}", c.checks);
            assert!(!prebuilds(c), "{:?}", c.checks);
        }
        let narrowed = Scope::packages(&["aterm-verify".to_string()]);
        let elsewhere = [
            // A dev run that tests and measures: its red compile reruns as `verify test`.
            (
                with_app(&[Check::Test, Check::Measure], Scope::workspace()),
                true,
            ),
            (with_app(&[Check::Test, Check::Measure], narrowed), true),
            (with_app(&[Check::Measure], Scope::workspace()), true),
            // No bundle, so no matrix runs: nothing compiles them.
            (
                ctx(&[Check::Test, Check::Measure], Scope::workspace()),
                false,
            ),
            (ctx(&[Check::Test], Scope::workspace()), false),
            (
                with_app(&[Check::Measure], Scope::workspace()).with_cursor_pacing(true),
                false,
            ),
        ];
        for (c, prebuild) in &elsewhere {
            assert!(!folds_measuring(c), "{:?}", c.checks);
            assert_eq!(prebuilds(c), *prebuild, "{:?}", c.checks);
        }
    }

    #[test]
    fn appkit_failure_is_observed_before_measurements_and_all_checks_still_run() {
        let stages = plan(&ctx(&[Check::Appkit, Check::Measure], Scope::workspace()));
        let seen = std::sync::Mutex::new(Vec::new());
        let reports = crate::sched::run_stages(
            &stages,
            u64::MAX,
            |stage, _| {
                seen.lock().unwrap().push(stage.id);
                let mut report = crate::ladder::Report::new(stage.title.clone());
                if stage.id == StageId::ObjcDrives {
                    report.fail("fixture: toolbar state missing");
                } else {
                    report.pass("fixture");
                }
                report
            },
            |_, _| {},
        );
        let seen = seen.into_inner().unwrap();
        let position = |id| seen.iter().position(|seen| *seen == id).unwrap();
        assert!(position(StageId::ObjcDrives) < position(StageId::MeasuringTests));
        assert_eq!(reports.len(), stages.len());
        assert_eq!(seen.len(), stages.len());
    }

    #[test]
    fn the_feature_builds_compile_beside_the_test_run_and_test_after_it() {
        use crate::sched::{awaited, ready};
        let p = plan(&ctx(&[Check::Test, Check::Checkers], Scope::workspace()));
        let at = |id| p.iter().position(|s| s.id == id).expect("planned");
        let none = vec![false; p.len()];
        assert!(
            ready(&p, &none, &none, 0, at(StageId::CheckersCompile), u64::MAX),
            "the checkers compile at t0"
        );
        let (test, checkers) = (at(StageId::Test), at(StageId::SpecCheckers));
        assert_eq!(awaited(&p, checkers).collect::<Vec<_>>(), [test]);
        let mut done = none.clone();
        for id in [StageId::TestCompile, StageId::CheckersCompile] {
            done[at(id)] = true;
        }
        assert!(
            !ready(&p, &done, &done, 0, checkers, u64::MAX),
            "the checkers wait for the test run"
        );
        done[test] = true;
        assert!(ready(&p, &done, &done, 0, checkers, u64::MAX));
        let alone = plan(&ctx(&[Check::Checkers], Scope::workspace()));
        assert!(alone.iter().all(|s| s.after_stages.is_empty()));
        // The release runs its suite once, with the feature, in the main lane.
        let release = plan(&ctx(&RELEASE, Scope::workspace()));
        assert!(release.iter().all(|s| {
            !matches!(s.id, StageId::CheckersCompile | StageId::SpecCheckers)
                && s.lane != Lane::CheckersTarget
        }));
    }
}

#[cfg(test)]
mod cursor_pacing_tests {
    #[test]
    fn focused_pacing_builds_no_conformance_matrices() {
        let mut context = super::tests::ctx(
            &[crate::checks::Check::Measure],
            crate::scope::Scope::workspace(),
        );
        context.cursor_pacing = true;
        let stages = super::plan(&context);
        assert_eq!(
            stages.iter().map(|stage| stage.id).collect::<Vec<_>>(),
            [super::StageId::GuiSmoke]
        );
    }
}
