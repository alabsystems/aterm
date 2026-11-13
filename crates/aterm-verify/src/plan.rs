// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Verification stage definitions, lane assignments, and execution order.

use std::path::PathBuf;

use aterm_toolchain::build_cache::noindex;

use crate::Ctx;
use crate::memory::StageKey as _;
use crate::scope::Scope;

/// Target directory name under `<root>/libc-oracle` ([`Lane::LibcOracleTarget`]).
pub const LIBC_ORACLE_TARGET: &str = ".ubd/target.noindex";

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
    /// `cells.noindex` under [`lanes_dir`] for cross-target cells, wasm modules, and browser runs.
    CellsTarget,
    /// `<root>/libc-oracle/.ubd/target.noindex` for the per-checkout libc oracle workspace.
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
    /// The engine's content guards over the export of the selection ([`crate::export_scan`]).
    ExportScan,
    /// Installed public shim and helper against the candidate bootstrap inputs.
    ReleasedShim,
    /// Trust checkers over registered formal models (`spec-check`).
    SpecCheck,
    /// Reruns compiled tests with external checkers after [`Self::Test`].
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

/// Stages that run over the run's crate selection: over an empty one each only says the change
/// cone holds no crate (`stages::EMPTY_CONE`), so it is a named skip ([`named_skip`]).
pub const OVER_SELECTION: [StageId; 5] = [
    StageId::TestCompile,
    StageId::Test,
    StageId::Doctests,
    StageId::Tippy,
    StageId::SpecCheckers,
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

    /// Whether this stage runs over the crate selection ([`OVER_SELECTION`]).
    #[must_use]
    pub fn over_selection(self) -> bool {
        OVER_SELECTION.contains(&self)
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
pub const SIDE_LANES: [&str; 2] = ["tippy", "cells"];

/// Every side lane that once built in the pre-[`lanes_dir`] location. The retired trust lane
/// stays here until no checkout can retain one of its old directories or removal asides.
const OLD_SIDE_LANES: [&str; 3] = ["tippy", "trust", "cells"];

/// Target directory for `lane`'s cargo invocations, or `None` for [`Lane::Pure`].
#[must_use]
pub fn lane_dir(ctx: &Ctx, lane: Lane) -> Option<PathBuf> {
    let side = |name: &str| Some(lanes_dir(ctx).join(noindex(name)));
    match lane {
        Lane::Pure => None,
        Lane::MainTarget | Lane::AfterCompile => Some(ctx.target_dir.clone()),
        Lane::TippyTarget => side("tippy"),
        Lane::CellsTarget => Some(cells_dir(ctx)),
        Lane::LibcOracleTarget => Some(ctx.root.join("libc-oracle").join(LIBC_ORACLE_TARGET)),
    }
}

/// Directory for [`Lane::CellsTarget`] builds and browser frames.
#[must_use]
pub fn cells_dir(ctx: &Ctx) -> PathBuf {
    lanes_dir(ctx).join(noindex("cells"))
}

/// THE SIDE LANES' OLD PLACES, emptied: each `<target>/verify/<lane>.noindex` the lanes built in
/// before they moved into [`lanes_dir`], and the trust lane in its later place under
/// [`lanes_dir`], go whole once no build holds their cargo locks. What an earlier run set aside
/// and could not finish (`<lane>.noindex.removing`, `<lane>.noindex.removing-<n>`) is tried again,
/// whether or not the lane is still there (`atpkg::build_cache::remove_unbuilt`): one line each
/// for the log, and none for a lane with nothing of it left; one a build or process holds, or
/// retention keeps, stays for a later run. Nothing of this version builds in any of these paths;
/// export now builds its release tools in the main target.
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
    let mut retire = |name: &str, old: PathBuf, retired: bool| {
        let there = std::fs::symlink_metadata(&old).is_ok_and(|m| m.is_dir());
        let line = match remove(&old) {
            Ok(true) if retired => Some(format!(
                "removed {}: export builds its release tools in the main target now",
                old.display()
            )),
            Ok(true) => Some(format!(
                "removed {}: the {name} lane builds in {} now, out of fseventsd's stream",
                old.display(),
                lanes_dir(ctx).display()
            )),
            Ok(false) if there => Some(format!(
                "kept {} or what was set aside of it: a build holds its cargo locks, a process \
                 uses it or cannot be ruled out, or retention keeps it; a later run checks it again",
                old.display()
            )),
            Ok(false) => None,
            Err(e) => Some(format!(
                "{}: not all of it could be removed ({e}); a later run tries again",
                old.display()
            )),
        };
        if let Some(line) = line {
            said.push(line);
        }
    };
    for name in OLD_SIDE_LANES {
        retire(name, ctx.state_dir().join(noindex(name)), name == "trust");
    }
    retire("trust", lanes_dir(ctx).join(noindex("trust")), true);
    said
}

/// Whether stage `id` is a named skip in `ctx` before it starts anything: a windowed stage
/// ([`WINDOWED`]) under `--no-windows`, one that measures the handed bundle (the measuring
/// tests, the gui smoke) with no `--app`, or one that runs over the crate selection
/// ([`OVER_SELECTION`]) when the selection is empty. It claims no memory, takes no quiet machine
/// and compiles nothing ([`has_compiler_stage`]).
#[must_use]
pub fn named_skip(ctx: &Ctx, id: StageId) -> bool {
    let needs_app = matches!(id, StageId::MeasuringTests | StageId::GuiSmoke);
    (ctx.no_windows && id.no_windows_skip().is_some())
        || (needs_app && ctx.measure_artifact.is_none())
        || (id.over_selection() && ctx.scope.selects_nothing())
}

/// The stages `ctx` runs, unordered: its checks' ([`crate::checks::stages_of`]), only the
/// compiles under `--compile-only`, only the burst under `--cursor-pacing`, no test compile over
/// an empty selection, and no measuring rows' compile where the test compile builds them or no
/// matrix will run ([`folds_measuring`], [`named_skip`]).
fn wanted(ctx: &Ctx) -> Vec<StageId> {
    let mut wanted = crate::checks::stages_of(&ctx.checks);
    if ctx.compile_only {
        wanted.retain(|id| matches!(id, StageId::TestCompile | StageId::MeasurePrebuild));
    }
    if ctx.cursor_pacing {
        wanted.retain(|id| *id == StageId::GuiSmoke);
    }
    if ctx.scope.selects_nothing() {
        wanted.retain(|id| *id != StageId::TestCompile);
    }
    if folds(ctx, &wanted) || (!ctx.compile_only && named_skip(ctx, StageId::MeasuringTests)) {
        wanted.retain(|id| *id != StageId::MeasurePrebuild);
    }
    wanted
}

/// Whether the selected, unskipped plan can compile. This is decided before disk admission,
/// without resolving the unit graph or consulting the stage-memory planner.
pub(crate) fn has_compiler_stage(ctx: &Ctx) -> bool {
    wanted(ctx)
        .into_iter()
        .any(|id| !named_skip(ctx, id) && crate::memory::compiles(id))
}

/// Whether the test compile also builds the measuring matrices' benches
/// ([`crate::stages::measuring_bench_args`]): it is the workspace's and the release's own
/// (`release`, `--as-release`: [`Ctx::checkers_in_tests`]). `ship release` runs that compile
/// first (`verify test --all --compile-only --as-release`), so the release's `verify` finds the
/// benches built, and a red row of it reruns as `--as-release`, which builds them again. Their
/// units are the test compile's: `test --bench` and `bench --profile test` build a bench under the
/// same profile in the same compile mode, the workspace's features unify alike for both. So no
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

/// Planned stages for `ctx.checks` ([`wanted`]), with source and type preflight first,
/// each with the memory it claims
/// by the run's book ([`StageSpec::mem`]): none for a named skip ([`named_skip`]), which starts
/// nothing.
#[must_use]
pub fn plan(ctx: &Ctx) -> Vec<StageSpec> {
    let wanted = wanted(ctx);
    // Pooling bounds resource use but does not replace early-error ordering.
    let pooled = ctx.pool.is_some() && !ctx.background;
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
        // Source guards and lint expose errors before test code generation, including
        // when a shared pool could otherwise give the compile every CPU token.
        for prerequisite in [
            StageId::ReleasedShim,
            StageId::Formatting,
            StageId::Census,
            StageId::ExportScan,
            StageId::Tippy,
        ] {
            if s.id == prerequisite {
                break;
            }
            if !s.after_stages.contains(&prerequisite) {
                s.after_stages.push(prerequisite);
            }
        }
        s.after_stages.retain(|id| present.contains(id));
        if measures && matches!(s.id, StageId::RedrawConformance | StageId::ObjcDrives) {
            s.exclusive = true;
        }
        if let (Some(book), Some((records, plain))) = (&ctx.memory, &records)
            && !named_skip(ctx, s.id)
        {
            let key = crate::memory::Key::of_stage(&s, ctx);
            s.mem = crate::memory::need_for_jobs(
                records,
                plain,
                &s,
                &key,
                book.physical,
                crate::compile_width::jobs_for(ctx, s.id),
            )
            .bytes;
            if s.id == StageId::TestCompile {
                let workspace;
                let selection = if ctx.scope.is_workspace() {
                    workspace = (ctx.workspace_members)(ctx).specs();
                    workspace.as_slice()
                } else {
                    ctx.scope.crates()
                };
                if selection.is_empty() {
                    planned.push(s);
                    continue;
                }
                let checkout = ctx.root.display().to_string();
                let shaping = crate::shape::Shaping {
                    records,
                    plain,
                    checkout: &checkout,
                    budget: book.shape_budget(),
                };
                match crate::shape::shape(selection, &shaping, &mut |p, closing, interner| {
                    (ctx.unit_graph)(ctx, p, closing, interner)
                }) {
                    crate::shape::Shaped::Calls(calls) => {
                        planned.extend(calls_of(ctx, &s, calls));
                        continue;
                    }
                    crate::shape::Shaped::Whole(Some(e)) => at_least(selection, &mut s, &e, book),
                    crate::shape::Shaped::Whole(None) => {}
                }
            }
        }
        planned.push(s);
    }
    for s in &mut planned {
        // Unpooled independent lanes would each spend the managed CPU grant. Background
        // plans also retain their existing one-stage policy.
        if (ctx.env.build_slot_held.is_some() && !pooled) || ctx.background {
            s.exclusive = true;
        }
    }
    planned
}

/// The unshaped test compile `s` claims at least its estimate `e` ([`crate::shape::Shaped::Whole`])
/// and a fifth more: its stage's lookup ([`crate::memory::need`]) reads its checkout's record
/// first, however light (a run after a plain build had compiled it), and the narrower selections
/// it holds only where nothing of its own selection is kept and they name every crate of it, by
/// their least cover ([`crate::memory::composed`]), or a wider selection's record in another
/// checkout where that claims less ([`crate::memory::wider_elsewhere`]), where the estimate reads
/// every checkout's record and the heaviest disjoint narrower records summed. So the lesser of
/// the lookup's two bounds decides the test compile's claim only where it exceeds that estimate;
/// the lint's, the doctests' and the pulled cells', which nothing here raises, it always decides.
/// Where the estimate raises its claim, its time line says by what.
fn at_least(
    selection: &[String],
    s: &mut StageSpec,
    e: &crate::shape::Estimate,
    book: &crate::memory::Book,
) {
    let floor = aterm_capacity::peak_claim(e.peak);
    if floor <= s.mem {
        return;
    }
    s.part = Some(crate::shape::Part::whole(
        crate::shape::Piece::of(selection, false),
        format!(
            "by {} and a fifth more, more than the {} its stage's lookup found; under the run's {} \
             shaping budget, so not shaped",
            e.from,
            aterm_capacity::size(s.mem),
            aterm_capacity::size(book.shape_budget())
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
    vec![
        spec(
            StageId::ReleasedShim,
            "released-shim (installed release builds aterm-time offline)",
            Lane::Pure,
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
        spec(
            StageId::ExportScan,
            "export scan (the publication engine's content guards)",
            Lane::Pure,
        ),
        spec(
            StageId::Tippy,
            format!("tippy lint ({})", scope.lint_label()),
            Lane::TippyTarget,
        ),
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
        StageSpec {
            after_stages: vec![StageId::TestCompile],
            ..spec(
                StageId::SpecCheck,
                "spec checkers (spec-check: ty over every registered model, the ay bundles)",
                Lane::AfterCompile,
            )
        },
        StageSpec {
            after_stages: vec![StageId::TestCompile, StageId::Test],
            ..spec(
                StageId::SpecCheckers,
                format!("checkers ({label}: per-step ty agreement; after the test run)"),
                Lane::MainTarget,
            )
        },
        StageSpec {
            after_stages: vec![StageId::TestCompile],
            ..spec(
                StageId::Export,
                "export guards (ship check --export: the publication engine's dry run)",
                Lane::MainTarget,
            )
        },
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
            "cells (each linted for its own triple on the checkout's stock pin)",
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
#[path = "plan_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "plan_cursor_pacing_tests.rs"]
mod cursor_pacing_tests;
