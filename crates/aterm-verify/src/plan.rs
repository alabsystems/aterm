// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LADDER, as data: which stages run, in which order, contending for what.
//!
//! The order is the script's order and is part of the contract — reviewers and
//! agents read these runs top to bottom and know where to look. Concurrency
//! changes when a stage RUNS, never where it PRINTS.

use std::path::PathBuf;

use crate::Ctx;
use crate::scope::Scope;

/// A contended resource. Two stages in the same lane are serialised in declared
/// order; different lanes overlap freely. In practice a lane is a cargo target
/// directory, which is exactly the thing two concurrent cargo invocations would
/// queue on anyway.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    /// Shells out to nothing that touches a target dir.
    Pure,
    /// `target/` — the workspace's test compile, its tests and doctests, and
    /// the `--full` tiers that compile. (Until 2026-09-13 also the xtask verbs
    /// and every driven binary; those have their own lanes below.)
    MainTarget,
    /// `target-tippy/` — the lint keeps a SEPARATE target dir: a `check`
    /// compile and a test compile share no artifacts, and in one directory
    /// each would queue on the other's cargo lock instead of overlapping.
    TippyTarget,
    /// `tools/freeze-safety-gate/` is its own workspace with its own target dir.
    FreezeGateTarget,
    /// `libc-oracle/{target,target-symgate}/` — the nested reference workspace
    /// and its emitted-symbol gate. The oracle owns both directories and may
    /// run beside the main workspace without contending for Cargo's lock.
    LibcOracleTarget,
    /// `target-xtask/` — the xtask-verb stages (formatting, forge, the foreign
    /// cells). The only lock they take in it is the one `targo run -p xtask`
    /// takes to build xtask; `cells-foreign` compiles its cells elsewhere.
    XtaskTarget,
    /// `target-drivers/` — every binary the gate DRIVES rather than tests: the
    /// smokes' `aterm-gui`/`aterm-ctl`, the redraw harness, the objc drivers,
    /// the `driver builds` stage that pre-compiles them, and the rows whose
    /// suites drive a binary their own stage builds first — the sealed fabric
    /// rung (`--full`), the atpkg pack and the live lanes' `aterm`. Only
    /// stages in this lane write its binaries, so no other stage can relink
    /// one under a stage that is driving it (in `target/` the test stage
    /// relinked `aterm-gui` with dev features).
    DriverTarget,
    /// `target/conformance-release/` — the RELEASE `aterm` the paint and spin
    /// suites judge, built here and HANDED to them (`ATERM_PAINT_BIN`). It is
    /// the directory those suites' own helper builds into when run by hand, so
    /// a hand run and the gate share one warm artifact.
    ///
    /// It lives UNDER `target/` and is still a lane of its own, because a lane
    /// is a cargo target directory and cargo's build lock is taken per target
    /// directory: the main lane's children never open this one, so priming it
    /// contends with nothing the test compile is doing.
    ConformanceRelease,
}

/// Every stage of the gate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StageId {
    /// The test stage's compile, at t0: the main lane's first compile, and the
    /// row that says COULD NOT RUN when there is no pinned `targo`.
    TestCompile,
    Test,
    Doctests,
    /// `--full` only: the sealed cross-host rung of the fabric bridge,
    /// `--features sealed`. A DRIVER-lane stage: its suite boots real
    /// `aterm-gui`s (see `plan`).
    SealedLane,
    Tippy,
    Formatting,
    GrepGuards,
    /// The offline suites over the scripts that build, sign, install and
    /// publish the app and its toolchain packages (`stages::DELIVERY_SUITES`):
    /// every one runs on stubs of its own making.
    DeliveryTooling,
    /// The atpkg end-to-end pack (`stages::ATPKG_DRIVEN_SUITE`), which drives
    /// the `atpkg` this stage builds first. A DRIVER-lane stage.
    AtpkgTooling,
    TrustContractProbe,
    /// `--full` only: the startup-comparison harness's own test.
    StartCompare,
    LibcOracle,
    FreezeGate,
    DriverBuilds,
    /// The RELEASE `aterm` the paint and spin suites judge, built in its own
    /// lane at t0 so the measuring stage finds it warm. MEASURE tier.
    ConformanceRelease,
    /// The tests the parallel run skips because their DEADLINES are seconds
    /// long (`stages::DEADLINE_TESTS`, aterm-update's), run with nothing else
    /// in flight. LAND tier: they decide correctness.
    DeadlineTests,
    /// The tests the parallel run skips because they MEASURE the machine or
    /// the release artifact (`stages::MEASURING_TESTS`, aterm-conformance's
    /// paint and spin), run with nothing else in flight. MEASURE tier.
    MeasuringTests,
    ControlSocketSmoke,
    GuiSmoke,
    RedrawConformance,
    ObjcClassAudit,
    ObjcImeDrive,
    ObjcToolbarDrive,
    ObjcWindowDrive,
    ObjcEventDrive,
    ObjcBoundDrive,
    /// The foreground handback lane (`stages::FOREGROUND_HANDBACK_SUITE`): a
    /// real shell's job control, in a private headless `aterm` this run built.
    ForegroundHandback,
    KaniFloor,
    CrossCells,
    /// `xtask gate forge`, every tier (2026-09-25).
    Forge,
    /// `xtask gate cells-foreign`, every tier (2026-09-25).
    ForeignCells,
    /// `--full` ONLY, and run alone: the Codex live upgrade
    /// (`stages::CODEX_LIVE_UPGRADE_SUITE`), which moves this machine's
    /// managed-store Codex from an older build to its current one — so its
    /// verdict reads the machine and the vendor as well as the tree, which is
    /// why it is not in the per-commit contract (see `plan`).
    CodexLiveUpgrade,
}

/// THE TWO TIERS AND THE `--full` EXTRAS (2026-09-26).
///
/// * LAND — the merge contract, what a bare `tools/verify.sh` runs: whether the
///   tree builds, passes its tests, lints, formats, proves its obligations and
///   drives its binaries. Its verdict is about the change.
/// * MEASURE — [`MEASURE_TIER`]: the stages that measure the MACHINE or the
///   RELEASE ARTIFACT rather than decide correctness — frame timings, input
///   latencies, and the fat-LTO release build the paint and spin matrices
///   judge. Run by `--measure` and `--full`; a release cut requires a green one
///   for the tree it cuts.
/// * FULL — [`FULL_ONLY`]: the sealed fabric rung, the trust-mc / Kani floor,
///   the cross-cell type-check, the startup comparison and the Codex live
///   upgrade, `--full`'s own.
///
/// WHY THE SPLIT (the 2026-09-26 gate audit, measured on two loaded runs):
/// the release build took 3351 s and 3525 s beside every push's gate, and the
/// same load turned correctness verdicts red on timing. The measuring work
/// still runs — before anything ships — just not as a condition of landing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    Land,
    Measure,
    Full,
}

/// THE MEASURE TIER, DECLARED ONCE. Every stage in it, and no other: the one
/// list the plan filters on ([`StageId::tier`]), the verdict of a run that did
/// not run it names ([`tier_titles`]), and a MEASURE receipt's `measured yes`
/// covers. Adding a stage here moves it out of the merge contract.
pub const MEASURE_TIER: [StageId; 3] = [
    StageId::ConformanceRelease,
    StageId::MeasuringTests,
    StageId::GuiSmoke,
];

/// The `--full`-only stages, in their ladder order: the sealed fabric rung,
/// declared in the driver lane before the first barrier so the test run waits
/// for it, then the tail — the Codex live upgrade, exclusive, last.
///
/// THE SEALED RUNG MOVED HERE ON 2026-09-27. aterm-link's `sealed` is "OFF BY
/// DEFAULT — so a default build, and every shipped binary, carries no cipher
/// tree at all" (its `[features]`), so its one test guards a transport no
/// release carries, while every per-commit test run waited behind it. The
/// fabric's authorization semantics stay per commit through the plaintext
/// end-to-end suites.
pub const FULL_ONLY: [StageId; 5] = [
    StageId::SealedLane,
    StageId::KaniFloor,
    StageId::CrossCells,
    StageId::StartCompare,
    StageId::CodexLiveUpgrade,
];

impl StageId {
    /// The tier this stage belongs to: [`MEASURE_TIER`], [`FULL_ONLY`], or
    /// the merge contract's LAND tier.
    #[must_use]
    pub fn tier(self) -> Tier {
        if MEASURE_TIER.contains(&self) {
            Tier::Measure
        } else if FULL_ONLY.contains(&self) {
            Tier::Full
        } else {
            Tier::Land
        }
    }
}

/// A stage's identity, its ladder header, and its scheduling constraints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageSpec {
    pub id: StageId,
    /// The `=== … ===` header, resolved against the scope where it varies.
    pub title: String,
    pub lane: Lane,
    /// Runs with nothing else in flight. Only the deadline tests, the
    /// measuring tests, the two smokes and the Codex live upgrade, and only
    /// because they judge deadlines, frame rates and latencies: a stage whose
    /// verdict depends on how busy the machine is must own the machine.
    pub exclusive: bool,
    /// Lanes whose stages must all have FINISHED before this one starts —
    /// apart from stages behind an exclusive barrier declared after it, which
    /// cannot overlap it anyway (see `crate::sched`). Only the test run uses
    /// it, to wait for the driver lane (see `plan`).
    pub after_lanes: Vec<Lane>,
}

/// The target directory a lane's cargo children use. `None` for [`Lane::Pure`],
/// which has none.
///
/// `MainTarget` is `<root>/target`, cargo's own default: its children are
/// handed no directory and inherit none — a caller's `CARGO_TARGET_DIR` never
/// reaches a child ([`crate::CHILD_ENV_REMOVED`]). The side lanes are absolute
/// paths under the root, because they ARE handed to children whose cwd is the
/// root and a relative one would be read twice.
#[must_use]
pub fn lane_dir(ctx: &Ctx, lane: Lane) -> Option<PathBuf> {
    let under_root = |rel: &str| {
        let p = ctx.root.join(rel);
        Some(std::path::absolute(&p).unwrap_or(p))
    };
    match lane {
        Lane::Pure => None,
        Lane::MainTarget => Some(ctx.root.join("target")),
        // Spelled as `stages::tippy_cmd` spells it, which is unchanged.
        Lane::TippyTarget => Some(ctx.root.join("target-tippy")),
        Lane::FreezeGateTarget => under_root("tools/freeze-safety-gate/target"),
        Lane::LibcOracleTarget => under_root("libc-oracle/target"),
        Lane::XtaskTarget => under_root("target-xtask"),
        Lane::DriverTarget => under_root("target-drivers"),
        // Spelled as the suites' own helper spells it (`release_bin`,
        // `root.join("target/conformance-release")` in
        // `crates/aterm-conformance/tests/support/mod.rs`), so a hand run of
        // paint or spin and the gate share one warm artifact.
        Lane::ConformanceRelease => under_root("target/conformance-release"),
    }
}

/// Does this run measure paint and spin? They are `aterm-conformance`'s, so
/// the measuring stage — and the release artifact it hands them, and so its
/// prime — has something to run exactly when the selection compiles that
/// crate. Whole-tree always does. A narrowing that does not REMOVES all
/// three rather than skipping them: `--test paint` names no target outside
/// that crate (cargo refuses it), and a minutes-long release build for
/// nobody is the opposite of what the prime is for.
#[must_use]
fn measures_paint_and_spin(scope: &Scope) -> bool {
    scope.includes_crate("aterm-conformance")
}

/// Does this run hold the deadline tests? They are `aterm-update`'s lib unit
/// tests (`stages::DEADLINE_TESTS`), so exactly when the selection compiles
/// that crate.
#[must_use]
fn holds_deadline_tests(scope: &Scope) -> bool {
    scope.includes_crate("aterm-update")
}

/// Build the run's stage list: every stage [`declared`] for its scope whose
/// tier its mode runs ([`crate::cli::Mode::runs`]).
///
/// Two things can remove a stage entirely (as opposed to skipping it):
///  * a narrowing that does not compile a stage's crate drops it, because
///    there is nothing for it to run: `aterm-link` for the sealed fabric
///    lane, `aterm-update` for the deadline tests ([`holds_deadline_tests`]),
///    `aterm-conformance` for the measuring tests and their release prime
///    ([`measures_paint_and_spin`]);
///  * the MODE picks the tiers ([`Tier`]): the default runs the LAND tier and
///    not the MEASURE tier — and its verdict NAMES every MEASURE stage it left
///    out ([`tier_titles`]), so the removal is never silent — `--measure` runs
///    the MEASURE tier alone, and `--full` both plus its own ([`FULL_ONLY`]).
///
/// Nothing else is conditional here. Absent TOOLS produce skips inside a stage,
/// never a missing stage, because a stage that vanishes is a stage nobody
/// notices was not run.
#[must_use]
pub fn plan(ctx: &Ctx) -> Vec<StageSpec> {
    declared(&ctx.scope)
        .into_iter()
        .filter(|s| ctx.mode.runs(s.id.tier()))
        .collect()
}

/// The titles of the `tier` stages `scope` plans, in ladder order — what the
/// verdict of a run that did not run `tier` names, so a tier a mode leaves out
/// is never a silent skip.
#[must_use]
pub fn tier_titles(scope: &Scope, tier: Tier) -> Vec<String> {
    declared(scope)
        .into_iter()
        .filter(|s| s.id.tier() == tier)
        .map(|s| s.title)
        .collect()
}

/// Every stage of every tier `scope` plans, in the ladder's order.
fn declared(scope: &Scope) -> Vec<StageSpec> {
    let label = scope.label();
    let mut v = vec![
        // THE TEST COMPILE, AT t0 (2026-09-27). The test run below waits for the
        // driver lane, and its compile does not need to: a compile is exactly the
        // work that overlapped the driver builds when a `targo build --workspace`
        // row held this slot. That row compiled a second, non-test variant of
        // the workspace which no stage ran; this one compiles what the test run
        // runs, so the test run's own `--no-run` child is a fingerprint check.
        // It is also where a missing or refused `targo` is said: COULD NOT RUN,
        // never a skip, because nothing about the tree can be decided without it.
        spec(
            StageId::TestCompile,
            format!("test compile ({label})"),
            Lane::MainTarget,
        ),
        // THE TEST RUN WAITS FOR THE DRIVER LANE. Its stages start at t0; the
        // driven binaries are built there, the sealed rung and the atpkg pack
        // run there, and the aterm-link harness refuses daemons it did not
        // start, so the run owns the machine's driven processes the way it did
        // when they queued behind it in `target/`. The sealed fabric rung is one
        // of the driver lane's stages, so it is waited for through
        // `DriverTarget`.
        //
        // NOT FOR TIPPY (2026-09-23) OR THE XTASK LANE (2026-09-27). Both only
        // compile; the tests whose verdicts depend on how busy the box is run
        // in exclusive stages of their own (`StageId::DeadlineTests`, and the
        // MEASURE tier's `StageId::MeasuringTests`), so the run no longer holds
        // its start for a compile.
        StageSpec {
            after_lanes: vec![Lane::DriverTarget],
            ..spec(StageId::Test, format!("test ({label})"), Lane::MainTarget)
        },
        spec(
            StageId::Doctests,
            format!("doctests ({label})"),
            Lane::MainTarget,
        ),
    ];
    v.push(spec(
        StageId::Tippy,
        format!("tippy lint ({label})"),
        Lane::TippyTarget,
    ));
    // FORMATTING, right after the lint it belongs beside. It is `xtask gate
    // lint --fmt-only`: both passes of the formatter lane — `targo-fmt --all`
    // over the workspace and the per-file sweep over the sources `--all` cannot
    // reach — and no other lane. `XtaskTarget` because it runs through the xtask
    // binary; the check itself needs no compiler and cost 7.5 s over 1,761 files
    // on two measured runs.
    //
    // WHY THE CONTRACT NOW CHECKS FORMATTING. It did not until 2026-08-31, and
    // the limit was stated rather than hidden — but `.githooks/pre-push` was
    // advisory from 2026-08-24, so nothing whatsoever ran the formatter
    // unless a human chose to. MEASURED consequence: three consecutive rebases
    // of `main` arrived with drift (5 files, 2 files, 1 file), and one of those
    // files sat in a crate `targo-fmt --all` structurally cannot see. A rule the
    // tree is held to by nobody is not a rule.
    v.push(spec(
        StageId::Formatting,
        "formatting (targo-fmt --all + the per-file sweep)",
        Lane::XtaskTarget,
    ));
    v.push(spec(
        StageId::GrepGuards,
        "grep guards and license headers",
        Lane::Pure,
    ));
    // DELIVERY TOOLING — every offline suite over the scripts that take the
    // app and its toolchain packages to someone's machine, each on stubs of its
    // own making (`stages.rs` section 3.5 has the roster and the reasons). The
    // atpkg index and publish producers' ten joined on 2026-09-27 from the
    // driver lane, where they had sat beside the one suite that drives a real
    // `atpkg` (the pack, below) and held the test run's start with it.
    v.push(spec(
        StageId::DeliveryTooling,
        "delivery tooling (installer channel and guards, cargo pin, export policy, release preflight, shape and after-cut, site sync, dev signing identity, fabric demo cleanup, atpkg index and publish producers; offline)",
        Lane::Pure,
    ));
    v.push(spec(
        StageId::TrustContractProbe,
        "trust contract probe (self-field ensures proves; off-switch no ICE)",
        Lane::Pure,
    ));
    v.push(spec(
        StageId::LibcOracle,
        "libc ABI oracle (this host's native cell; the others are decided where they are native)",
        Lane::LibcOracleTarget,
    ));
    v.push(spec(
        StageId::FreezeGate,
        "L0 temporal-safety gate (freeze/data-loss/deadlock — 6 obligations)",
        Lane::FreezeGateTarget,
    ));
    // THE TWO GATE VERBS THAT HAD NO AUTOMATIC CALLER (decided 2026-09-25
    // under the owner's standing direction). Both are xtask verbs, so both sit
    // in the xtask lane, which only compiles and so runs beside the test run.
    // `forge` is a flat ~9-14 s with no compiler and no network; `cells-foreign`
    // (five cells) is 8 s warm and 278 s after a core edit, 432 s cold into
    // 2.6 GiB of the snapshot's cells lane (re-measured 2026-09-27 on a loaded
    // M5 Max, with the gate's CARGO_INCREMENTAL=0). Nothing waits for it, so it
    // costs a run only where it outlasts the test run — and it did not delay
    // even the test run's START while that still waited for this lane: in a
    // merge-contract run after an edit to aterm-grid's lib.rs it ended 238 s
    // before the build and 657 s before the test run started, which the
    // driver lane held (2026-09-27, `--timings`). It is the only
    // automatic compile of the cells no box in the fleet hosts — the Windows
    // binary shipped unbuildable (v0.82.0) while that compile lived behind
    // `--full`, and a Unix-only harness change reached main red for both
    // Windows cells on 2026-09-26. `stages::forge_gate` and
    // `stages::foreign_cells` carry the rest.
    v.push(spec(
        StageId::Forge,
        "third-party surface (xtask gate forge)",
        Lane::XtaskTarget,
    ));
    v.push(spec(
        StageId::ForeignCells,
        "foreign-cell type-check (xtask gate cells-foreign: the cells no fleet box hosts)",
        Lane::XtaskTarget,
    ));
    // DRIVER BUILDS (2026-09-13): the objc, smoke and redraw binaries and the
    // live lanes' `aterm`, compiled at t0 in their own lane. Every driver
    // stage below still runs its own build argv — a fingerprint no-op after
    // this — so nothing is proven from this row's binaries that its own stage
    // did not ask cargo for. What it removes is those compiles running one
    // after another inside the exclusive tail. Declared here, before the
    // smokes, because a stage after an exclusive barrier could not start until
    // the barrier finished.
    v.push(spec(
        StageId::DriverBuilds,
        "driver builds (objc, smoke and redraw binaries, and the live lanes' aterm)",
        Lane::DriverTarget,
    ));
    // THE CONFORMANCE RELEASE ARTIFACT (2026-09-22; the MEASURE tier's since
    // 2026-09-26) — a fat-LTO, one-codegen-unit `--release -p aterm`, 326 s
    // cold, built at t0 in its own lane so the exclusive measuring stage that
    // judges it finds it warm (its own first child is the same build, a
    // fingerprint check by then). Nothing waits on this lane: the point is to
    // overlap everything else, and the measuring stage, being exclusive,
    // cannot start before it has finished anyway.
    if measures_paint_and_spin(scope) {
        v.push(spec(
            StageId::ConformanceRelease,
            "conformance release artifact (paint/spin build it otherwise)",
            Lane::ConformanceRelease,
        ));
    }
    // THE SEALED FABRIC RUNG (2026-09-14; `--full` only since 2026-09-27, see
    // [`FULL_ONLY`]). `tests/two_nodes_sealed.rs` is `#![cfg(feature =
    // "sealed")]`, so the workspace test run compiles it to nothing and this
    // row is its only cadence. Its suite boots real `aterm-gui --headless`
    // processes, which its harness FINDS — first in the target dir the test
    // binary was built into — and refuses when older than their sources. So it
    // is a DRIVER-lane row: its first child builds that `aterm-gui` into this
    // lane's dir and its suite starts only if that succeeded (`stages.rs`),
    // and it is declared before the smokes' barrier, so the test run waits for
    // it and never relinks the binary under it (`sched.rs` holds every row of
    // this lane to that).
    if scope.includes_sealed_lane() {
        v.push(spec(
            StageId::SealedLane,
            "sealed fabric lane (aterm-link --features sealed)",
            Lane::DriverTarget,
        ));
    }
    // THE ATPKG END-TO-END PACK (2026-09-16), the same shape for the same
    // reason. `tools/test-atpkg-pack-one-compiler.sh` section D PACKS a
    // sysroot bundle with a real `atpkg`, resolved as `$ATPKG`, else
    // `<root>/target/debug/atpkg` — which, from a stage at t0, is a PREVIOUS
    // run's (measured on 86381efbf: FAIL on a cold snapshot; three earlier
    // gates passed only on a stale binary). So its first child builds `-p
    // atpkg` into this lane's dir and the suite is handed exactly that. Its
    // ten stub-only siblings, which drive no real binary, run with the
    // delivery tooling at t0 (since 2026-09-27).
    v.push(spec(
        StageId::AtpkgTooling,
        "atpkg end-to-end pack (the atpkg this stage builds)",
        Lane::DriverTarget,
    ));
    // THE DEADLINE TESTS (2026-09-26), the LAND tier's first exclusive stage:
    // aterm-update's launchd copy tests (`stages::DEADLINE_TESTS`), which the
    // test run skips. Their deadlines are seconds long, and they are
    // CORRECTNESS tests — the ty models of the copy's deadline, publication
    // and reaper, and the real wrapper — so they stay in the merge contract and
    // run alone.
    if holds_deadline_tests(scope) {
        v.push(exclusive(
            StageId::DeadlineTests,
            format!("deadline tests ({label}; run alone)"),
            Lane::MainTarget,
        ));
    }
    // THE MEASURING TESTS (2026-09-23; the MEASURE tier's since 2026-09-26):
    // the paint and spin matrices, which the test run skips
    // (`stages::MEASURING_TESTS`). They judge the RELEASE artifact's frame
    // timings, so like the smokes below they run with nothing else in flight.
    if measures_paint_and_spin(scope) {
        v.push(exclusive(
            StageId::MeasuringTests,
            format!("measuring tests ({label}; run alone)"),
            Lane::MainTarget,
        ));
    }
    // THE HEADLESS SMOKE, LAND tier and exclusive: the `cursor` round trip
    // every `aterm drive` verb preflights with, then a typing burst over the
    // control socket that must heal no lost output wake, a reading only an idle
    // machine can make (`smoke_stages`). Being exclusive it is also the LAND
    // tier's barrier in every scope — the deadline tests above are removed by a
    // narrowing that drops aterm-update — so the driver-lane rows declared
    // after it run with nothing else in flight.
    v.push(exclusive(
        StageId::ControlSocketSmoke,
        "control-socket smoke",
        Lane::DriverTarget,
    ));
    // THE PACING SMOKE, MEASURE tier since 2026-09-26: it gates a real
    // window's frames, input->present, key->write and present->glass — the
    // machine's latencies, which the headless smoke above cannot see.
    v.push(exclusive(
        StageId::GuiSmoke,
        "gui typing-pacing smoke",
        Lane::DriverTarget,
    ));
    // After the two smokes in the driver lane: it builds aterm-gui under a
    // non-default feature, so it runs after the two smokes have finished with
    // the default binaries. Never conditional on the scope — the claim it makes
    // is about the shipped GUI, and a gate that a narrowing can remove is a gate
    // that stops running exactly when someone is in a hurry.
    v.push(spec(
        StageId::RedrawConformance,
        "control redraw conformance (a select repaints a real window)",
        Lane::DriverTarget,
    ));
    // AND THE OTHER THING ONLY A `fn main` CAN SEE. `vendor/winit`'s
    // `WinitWindowDelegate` is declared by `aterm_objc::declare_class!` since
    // W3, and NOTHING IN CI READ THE REGISTERED CLASS: two compile-verified
    // plants — an argument retyped `Id` -> `Bool`, and `NSWindowDelegate`
    // deleted from the `protocols:` list — each left `cargo build` at exit 0,
    // `cargo test -p aterm-objc --test winit_seam` at 6/6, and the DRIVEN EVENT
    // LOG byte-identical to clean head. libtest cannot host AppKit
    // (`pthread_main_np()` is 0 on every worker, `--test-threads=1` included),
    // so the auditor is an `[[example]]` and this is what invokes it. Never
    // conditional on the scope, for the same reason the redraw gate is not.
    v.push(spec(
        StageId::ObjcClassAudit,
        "objc live-class audit (the registered WinitWindowDelegate and WinitView, against the runtime)",
        Lane::DriverTarget,
    ));
    // The auditor's twin, and a SEPARATE stage because it asks a separate
    // question. The audit proves the ported `WinitView` is SHAPED right — 44
    // registered encodings against the runtime's own authority. It cannot prove
    // the class BEHAVES right, and `view.rs`'s eleven `NSTextInputClient` rows
    // are a state machine an input method drives: a port that registers all
    // eleven correctly and still drops a preedit, mis-clamps a cursor range or
    // forwards UTF-16 indices where winit's API promises UTF-8 byte offsets
    // passes every shape check in the tree. This drives a whole composition
    // through the registered IMPs and reads the `WindowEvent::Ime` sequence that
    // comes out.
    v.push(spec(
        StageId::ObjcImeDrive,
        "objc IME drive (a composition through the ported WinitView's NSTextInputClient rows)",
        Lane::DriverTarget,
    ));
    // AND THE SAME OBLIGATION, ONE FILE OVER. Both stages above audit
    // `vendor/winit`; `crates/aterm-gui/src/toolbar.rs` is the LARGEST ported
    // file in the tree — four declared classes, and after W7 every AppKit
    // binding call in it — and until this stage NOTHING IN THE TREE DROVE IT.
    // Its classes were checked by `#[cfg(test)] mod objc_tests`, whose central
    // case is thirty-two registered encodings against a literal written in the
    // same file: a plant that registered `controlTextDidChange:` as `v@:B` AND
    // edited the table to agree left that test GREEN. The driver installs the
    // real toolbar in a real NSWindow, enters the registered IMPs through
    // AppKit's own dispatch, captures 27 drawing states with
    // `-cacheDisplayInRect:`, and reads all four classes off the live objects.
    // On its first run it found a defect no encoding check could see — the
    // rename editor posting a spurious commit at open, which killed both of its
    // exits — so it is not conditional on the scope either.
    v.push(spec(
        StageId::ObjcToolbarDrive,
        "objc toolbar drive (the real tab strip: 27 drawn states, and all four declared classes off live objects)",
        Lane::DriverTarget,
    ));
    // THE WINDOW DRIVER (W8), and it is unconditional for the same reason its
    // three siblings are. `window_delegate.rs` is the largest file in the
    // mac-arm endgame and W8 moved all 177 of its AppKit binding calls off
    // `objc2-app-kit`; on its first run it SEGFAULTED on a use-after-free that
    // had compiled clean and passed every test in the tree. Nothing else in the
    // ladder touches the window surface.
    v.push(spec(
        StageId::ObjcWindowDrive,
        "objc window drive (the real window: title, style mask, geometry, limits, theme, tabs, drag-and-drop, close and fullscreen)",
        Lane::DriverTarget,
    ));
    // THE EVENT DRIVER, unconditional for the same reason as its three
    // siblings — and it is the row they were missing. The auditor proved the
    // ported `WinitView` was SHAPED right, the IME drive composed through it,
    // the window drive resized and focused it, and v0.72.0 still aborted on
    // the FIRST `mouseMoved:` AppKit delivered: `update_modifiers` sent
    // `-keyCode` to a mouse event, AppKit raised, and the trampoline's
    // `catch_unwind` cannot catch a foreign exception. No gate had ever sent
    // a mouse event through a mouse IMP. This one builds a real `NSEvent` of
    // every type each event-taking row can receive, enters the IMP as AppKit
    // does, sends the same shapes through `-[NSApplication sendEvent:]`, and
    // re-executes itself as a control child that makes the v0.72.0 send
    // inside a trampoline and must die by SIGABRT. Measured against the
    // v0.72.0 `view.rs`: exit 134 at the first `mouseMoved:`.
    v.push(spec(
        StageId::ObjcEventDrive,
        "objc event drive (every NSEvent-taking WinitView row and sendEvent:, with a real NSEvent of every type AppKit can deliver)",
        Lane::DriverTarget,
    ));
    // (The modal and swizzle drivers were rows here until 2026-09-27. The
    // swizzle's one live proof — the IMP's image leaving AppKit — is the
    // window drive's stage 15, and the event drive sends every NSEvent shape
    // through the swizzled `sendEvent:`; the modal driver sent every message
    // as a raw `objc_msgSend`, so it drove AppKit's facts, not aterm's.)
    //
    // THE CONTAINER DRIVER (W12), unconditional because its obligation has no
    // type-system half. `MainThreadBound<T>` is `Send + Sync` for every `T`
    // on the strength of a `Drop` that reschedules to the main thread, and
    // libtest cannot exercise that in either direction: it runs every test on
    // a worker and parks its main thread. This one measures the thread the
    // destructor lands on — against an UNSOUND twin declared in the same file
    // that lands it on the worker — with a plain `T` and with a declared
    // class whose `-dealloc` carries a Rust destructor, and proves the
    // `needs_drop` short-circuit is load-bearing with a child that must hang.
    v.push(spec(
        StageId::ObjcBoundDrive,
        "objc bound drive (MainThreadBound's main-thread drop against an unsound twin, a declared class's -dealloc, and the needs_drop hang differential)",
        Lane::DriverTarget,
    ));
    // THE FOREGROUND HANDBACK (2026-09-26), the last row of the driver lane.
    // `tools/test-foreground-handback.sh` drives a private headless `aterm` —
    // THE one binary, which no other stage builds — and until this row nothing
    // ran it: by hand it found no `<root>/target/debug/aterm` and answered with
    // a code that is not a failure (2). The stage builds `-p aterm --bin aterm`
    // in this lane (the driver builds row has compiled it at t0) and hands the
    // lane that binary. HERE, behind the smokes' barrier, because every stage
    // after the barrier in a `--fast` run is in this lane, so nothing else is
    // in flight while it waits on real shells under deadlines (`ctl await …
    // timeout=`), and a compile beside it turns a slow machine into a red row.
    // (In a `--full` run the tiers below run after the same barrier and do
    // overlap it, as they overlap the redraw gate and the
    // drives; the lane's deadlines are 10-20 s against a ~20 s run.) Declared
    // after the barrier, it is awaited by nothing. Never removed by a
    // narrowing.
    //
    // Its twin, the Codex live upgrade, ran in this same row for one day and
    // left it for the `--full` tier below — the reason is written there.
    v.push(spec(
        StageId::ForegroundHandback,
        "foreground handback (a real shell's job control, driving a private headless aterm built this run)",
        Lane::DriverTarget,
    ));
    // THE `--full`-ONLY STAGES ([`FULL_ONLY`]).
    v.push(spec(
        StageId::KaniFloor,
        "trust-mc / Kani BMC floor (config-free parser harnesses)",
        Lane::MainTarget,
    ));
    // THE WHOLE MATRIX, COMPILED. Every tier type-checks the host's own
    // cell (the test and lint lanes) and, since 2026-09-25, the five
    // cells no fleet box hosts (`ForeignCells` above); forge measures all
    // eight and compiles none. `xtask gate cells` adds the rest — `mac-x64`,
    // and `mac-arm`/`linux` where the box is not that host — with a real
    // compiler per triple. It is `--full`-only because each of those is
    // native on some box, where that box's own lanes compile it, and its
    // cross verdict differs from the native one (gate.rs,
    // `FLEET_HOST_TRIPLES`). (Until 2026-09-25 this comment read "`--fast`
    // type-checks exactly one of the five targets", true then.)
    //
    // WHAT `--fast` DOES NOT SEE, corrected 2026-09-01 because the previous
    // version of this sentence was FALSE IN BOTH DIRECTIONS. It listed
    // "every `#[cfg(unix)]`" among the blind spots: macOS IS a unix, so the
    // mac-arm cell `--fast` runs compiles every `#[cfg(unix)]` block in
    // every crate it builds, and always did. And it implied `--full` covered
    // the rest, which was not true either — `ring` and `zstd-sys` bundle C,
    // their build scripts could not run for the Linux and Windows triples on
    // this box, and until the `cshim` rows landed in
    // `tools/cross-cell-gate.tsv` those two cells reached NONE of aterm's
    // own eighteen compiled crates. A bare `E0308` under
    // `#[cfg(target_os = "linux")]` in `crates/aterm-gui/src/control.rs`
    // left `gate cells` GREEN.
    //
    // What `--fast` did not see before `ForeignCells`: `#[cfg(windows)]`,
    // `#[cfg(target_os = "linux")]`, the `unix` arms that exclude Apple,
    // `#[cfg(target_arch = "wasm32")]`, and every third-party crate that
    // only appears on a non-native cell. `--fast` now sees the Windows,
    // wasm32 and ARM-Linux arms through the foreign cells; the x86_64
    // `linux` cell stays here, and `--full` sees all four of those: measured over the 3,245 platform `cfg` attribute sites under
    // `crates/`, some cell reaches 2,894 of them, against 2,143 before the
    // shim rows. Of the 351 left, 232 are in crates no cell's graph carries
    // at all (`aterm-release`, `atpkg-keys`, `aterm-conformance`,
    // `aterm-nest`, `aterm-effects-web`, …) and 119 are predicates no cell's
    // triple can satisfy. Neither `--fast` nor `--full` LINKS or RUNS a
    // cross artifact.
    //
    // `MainTarget`, even though every cell compiles into a target directory
    // OUTSIDE this repo: the stage reaches the verb through
    // `targo run -p xtask`, and THAT holds the workspace lock like any other
    // gate. The lane describes what the stage contends for, not where the
    // work it launches ends up.
    v.push(spec(
        StageId::CrossCells,
        "cross-cell type-check (every forge cell, each for its own triple)",
        Lane::MainTarget,
    ));
    // THE STARTUP-COMPARISON HARNESS'S OWN TEST (2026-09-27: per commit
    // until then). It guards the evidence path a publishable startup
    // comparison takes, which only `--full`-grade work produces.
    v.push(spec(
        StageId::StartCompare,
        "startup comparison scheduler",
        Lane::Pure,
    ));
    // THE CODEX LIVE UPGRADE (2026-09-26) — `--full` only, and run alone.
    //
    // `tools/test-codex-live-upgrade.sh` starts the managed store's OLDER
    // Codex in a private headless `aterm` and watches that aterm's harness
    // host move it onto the store's CURRENT build. It ran in the per-commit
    // ladder for one day, beside the foreground handback, and left it because
    // its verdict is not a function of the tree alone. Two inputs are the
    // MACHINE's and the VENDOR's:
    //
    //  * THE STORE. With no Codex older than `current` in it, the lane cannot
    //    run (exit 77). atpkg keeps the live build plus one rollback
    //    (`crates/atpkg/src/gc.rs`), so that is every Mac that installed Codex once,
    //    installed it fresh, or rolled back to its oldest build — and there
    //    the per-commit gate could never claim the contract, whatever the
    //    commit.
    //  * THE VENDOR. Codex is a default-set, vendor-fetched package that atpkg
    //    moves forward on its own, and the lane asserts one release's internals
    //    (0.157's daemon, its `auto-update-version` pin). An unchanged commit
    //    could go from PASS to FAIL overnight.
    //
    // `lib.rs` states the rule this broke: "A contract that measures a
    // different thing depending on who typed the command is not a contract."
    // Pinning two Codex builds for the lane to upgrade between was the other
    // way out, and was not taken: the gate is offline, the pins would be a
    // vendor download per machine, and a pinned pair stops measuring the
    // Codex people actually run, which is the lane's whole point.
    //
    // So it is in the tier that already holds world-dependent evidence (the
    // trust-mc floor), where a missing prerequisite is a NAMED SKIP —
    // counted, printed with the lane's own reason, and
    // forfeiting that run's contract claim, never a pass — and a FAIL after a
    // vendor update with an unchanged tree is exactly what the tier is for:
    // aterm's host (or the lane's release-specific checks) has to catch up
    // with the Codex people now run. EXCLUSIVE and LAST: it waits on real
    // idle points and ten-second ledger polls for ~10 minutes (581 s on
    // 2026-09-26), and in `--full` the tiers above run after the smokes'
    // barrier, so without exclusivity it would run beside them. Last, so
    // nothing waits behind it. Its own first child is the handback row's
    // build (a fingerprint no-op by then).
    v.push(exclusive(
        StageId::CodexLiveUpgrade,
        "Codex live upgrade (the managed store's older Codex moved onto its current one by a private headless aterm; reads this machine's store and the vendor's Codex; run alone)",
        Lane::DriverTarget,
    ));

    v
}

fn spec(id: StageId, title: impl Into<String>, lane: Lane) -> StageSpec {
    StageSpec {
        id,
        title: title.into(),
        lane,
        exclusive: false,
        after_lanes: Vec::new(),
    }
}

fn exclusive(id: StageId, title: impl Into<String>, lane: Lane) -> StageSpec {
    StageSpec {
        id,
        title: title.into(),
        lane,
        exclusive: true,
        after_lanes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvSnapshot;
    use crate::cli::Mode;
    use crate::scope::Scope;
    use std::path::PathBuf;

    const MODES: [Mode; 3] = [Mode::Fast, Mode::Measure, Mode::Full];

    fn ctx(mode: Mode, scope: Scope) -> Ctx {
        Ctx::new(
            PathBuf::from("/repo"),
            mode,
            scope,
            EnvSnapshot::default(),
            PathBuf::from("/tmp"),
        )
    }

    fn ids(ctx: &Ctx) -> Vec<StageId> {
        plan(ctx).into_iter().map(|s| s.id).collect()
    }

    #[test]
    fn the_fast_ladder_is_the_documented_stages_in_the_documented_order() {
        assert_eq!(
            ids(&ctx(Mode::Fast, Scope::workspace())),
            [
                StageId::TestCompile,
                StageId::Test,
                StageId::Doctests,
                StageId::Tippy,
                StageId::Formatting,
                StageId::GrepGuards,
                StageId::DeliveryTooling,
                StageId::TrustContractProbe,
                StageId::LibcOracle,
                StageId::FreezeGate,
                StageId::Forge,
                StageId::ForeignCells,
                StageId::DriverBuilds,
                StageId::AtpkgTooling,
                StageId::DeadlineTests,
                StageId::ControlSocketSmoke,
                StageId::RedrawConformance,
                StageId::ObjcClassAudit,
                StageId::ObjcImeDrive,
                StageId::ObjcToolbarDrive,
                StageId::ObjcWindowDrive,
                StageId::ObjcEventDrive,
                StageId::ObjcBoundDrive,
                StageId::ForegroundHandback,
            ]
        );
    }

    /// THE MEASURE LADDER IS THE MEASURE TIER, IN LADDER ORDER (2026-09-26):
    /// `--measure` plans exactly [`MEASURE_TIER`] — the release artifact, the
    /// measuring tests and the pacing smoke — and not one stage of the merge
    /// contract.
    #[test]
    fn the_measure_ladder_is_the_measure_tier_in_ladder_order() {
        assert_eq!(ids(&ctx(Mode::Measure, Scope::workspace())), MEASURE_TIER);
        for id in MEASURE_TIER {
            assert_eq!(id.tier(), Tier::Measure, "{id:?}");
        }
        for id in FULL_ONLY {
            assert_eq!(id.tier(), Tier::Full, "{id:?}");
        }
    }

    /// THE MEASURE TIER IS DECLARED ONCE, AND A RUN THAT LEAVES IT OUT NAMES IT
    /// (2026-09-26). In every mode and scope, a stage is planned iff its tier
    /// is one the mode runs — nothing else decides it — and [`tier_titles`]
    /// names exactly the MEASURE stages the same scope would plan, which is
    /// what the default's verdict lists as `not part of the merge contract`.
    /// A scope without `aterm-conformance` names no release prime: it would
    /// not have built one either.
    #[test]
    fn a_stage_is_planned_iff_the_mode_runs_its_tier_and_the_left_out_tier_is_named() {
        for scope in [
            Scope::workspace(),
            Scope::crate_only("aterm-grid"),
            Scope::changed("main", vec!["aterm-conformance".into()], true),
        ] {
            let full = plan(&ctx(Mode::Full, scope.clone()));
            for mode in MODES {
                let got = ids(&ctx(mode, scope.clone()));
                let want: Vec<StageId> = full
                    .iter()
                    .map(|s| s.id)
                    .filter(|id| mode.runs(id.tier()))
                    .collect();
                assert_eq!(got, want, "{mode:?} / {}", scope.label());
            }
            let measured: Vec<String> = full
                .iter()
                .filter(|s| s.id.tier() == Tier::Measure)
                .map(|s| s.title.clone())
                .collect();
            assert_eq!(tier_titles(&scope, Tier::Measure), measured);
            assert_eq!(
                tier_titles(&scope, Tier::Measure)
                    .iter()
                    .any(|t| t.starts_with("conformance release artifact")),
                scope.includes_crate("aterm-conformance"),
                "{}",
                scope.label()
            );
        }
        assert_eq!(
            tier_titles(&Scope::workspace(), Tier::Measure),
            [
                "conformance release artifact (paint/spin build it otherwise)",
                "measuring tests (--workspace; run alone)",
                "gui typing-pacing smoke",
            ]
        );
    }

    /// The test run waits for the driver lane — whose stages build and drive
    /// the binaries the run's own suites would otherwise meet mid-link — and
    /// nothing in the plan can deadlock on that wait. Its compile does not
    /// wait: the test compile row starts at t0.
    #[test]
    fn the_test_run_waits_for_the_driver_lane_and_its_compile_does_not() {
        for mode in [Mode::Fast, Mode::Full] {
            let p = plan(&ctx(mode, Scope::workspace()));
            let at = |id| p.iter().position(|s| s.id == id).expect("planned");
            let (compile, test) = (at(StageId::TestCompile), at(StageId::Test));
            assert!(
                p[test].after_lanes.contains(&Lane::DriverTarget),
                "{mode:?}: {:?}",
                p[test].after_lanes
            );
            assert!(compile < test, "{mode:?}");
            assert!(p[compile].after_lanes.is_empty(), "{mode:?}");
            let nothing = vec![false; p.len()];
            assert!(
                crate::sched::ready(&p, &nothing, &nothing, 0, compile),
                "{mode:?}: the test compile starts at t0"
            );
            crate::sched::check_after_lanes(&p).expect("the plan cannot deadlock");
        }
    }

    /// A GATE NOBODY INVOKES IS NOT A GATE, for the two gate verbs that had no
    /// automatic caller until 2026-09-25. `cells-foreign` is the only automatic compile of the
    /// cells no fleet box hosts, and `forge` the only automatic read of the
    /// third-party surface; a narrowing is exactly when someone is in a hurry,
    /// so neither may be scoped away.
    #[test]
    fn forge_and_the_foreign_cells_run_in_every_tier_and_every_scope() {
        for mode in [Mode::Fast, Mode::Full] {
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-grid"),
                Scope::changed("main", vec![], true),
            ] {
                let got = ids(&ctx(mode, scope.clone()));
                for id in [StageId::Forge, StageId::ForeignCells] {
                    assert!(
                        got.contains(&id),
                        "{mode:?} / {} lost {id:?}",
                        scope.label()
                    );
                }
            }
        }
    }

    /// `--full` is BOTH TIERS plus its own ([`FULL_ONLY`]), in ladder order:
    /// take the MEASURE and FULL stages out of it and it is the default
    /// ladder; its FULL stages are [`FULL_ONLY`], the Codex live upgrade last;
    /// and the sealed rung, the one of them declared mid-ladder, sits in the
    /// driver lane behind the driver builds and before the first barrier, so
    /// the test run waits for it.
    #[test]
    fn full_is_both_tiers_plus_its_own_stages() {
        let fast = ids(&ctx(Mode::Fast, Scope::workspace()));
        let full = plan(&ctx(Mode::Full, Scope::workspace()));
        let of = |tier: Tier| -> Vec<StageId> {
            full.iter()
                .map(|s| s.id)
                .filter(|id| id.tier() == tier)
                .collect()
        };
        assert_eq!(of(Tier::Land), fast);
        assert_eq!(of(Tier::Measure), MEASURE_TIER);
        assert_eq!(of(Tier::Full), FULL_ONLY);
        assert_eq!(full.last().map(|s| s.id), Some(StageId::CodexLiveUpgrade));
        let at = |id| full.iter().position(|s| s.id == id).expect("planned");
        let (sealed, builds, test) = (
            at(StageId::SealedLane),
            at(StageId::DriverBuilds),
            at(StageId::Test),
        );
        let barrier = full.iter().position(|s| s.exclusive).expect("a barrier");
        assert_eq!(full[sealed].lane, Lane::DriverTarget);
        assert!(builds < sealed && sealed < barrier, "{full:?}");
        assert!(crate::sched::awaited(&full, test).any(|j| j == sealed));
    }

    /// THE STAGE-SET PROOF across modes and scopes. Four rows follow their
    /// own crates: the sealed lane is aterm-link's, the deadline tests are
    /// aterm-update's, and the measuring tests and their release prime are
    /// aterm-conformance's. Every mode and scope plans EXACTLY the whole-tree
    /// ladder of its mode minus the rows its crates dropped — in the same
    /// order, nothing else lost. With the literal fast ladder above, the
    /// MEASURE tier and `--full`'s own stages, that pins every ladder the gate
    /// can print.
    #[test]
    fn a_scope_removes_exactly_the_lanes_whose_crates_it_dropped() {
        for mode in MODES {
            let whole = ids(&ctx(mode, Scope::workspace()));
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-grid"),
                Scope::crate_only("aterm-update"),
                Scope::crate_only("aterm-conformance"),
                Scope::changed("main", vec!["aterm-gui".into()], true),
                Scope::changed("main", vec![], true),
            ] {
                let c = ctx(mode, scope.clone());
                let expected: Vec<StageId> = whole
                    .iter()
                    .filter(|id| match id {
                        StageId::SealedLane => c.scope.includes_sealed_lane(),
                        StageId::DeadlineTests => holds_deadline_tests(&c.scope),
                        StageId::ConformanceRelease | StageId::MeasuringTests => {
                            measures_paint_and_spin(&c.scope)
                        }
                        _ => true,
                    })
                    .copied()
                    .collect();
                assert_eq!(
                    ids(&c),
                    expected,
                    "{mode:?} / {}: no other stage is dropped by a scope",
                    scope.label()
                );
            }
        }
        // A scope holding none of the crates drops every one of those rows.
        let scoped = ids(&ctx(Mode::Full, Scope::crate_only("aterm-grid")));
        for id in [
            StageId::SealedLane,
            StageId::DeadlineTests,
            StageId::ConformanceRelease,
            StageId::MeasuringTests,
        ] {
            assert!(!scoped.contains(&id), "{id:?}");
        }
        let link_only = ids(&ctx(Mode::Full, Scope::crate_only("aterm-link")));
        assert!(link_only.contains(&StageId::SealedLane));
        let update_only = ids(&ctx(Mode::Fast, Scope::crate_only("aterm-update")));
        assert!(update_only.contains(&StageId::DeadlineTests));
    }

    /// The deadline and measuring stages and the smokes own the machine because
    /// they judge deadlines and timings; the Codex live upgrade (`--full` only)
    /// because it waits on a real Codex's idle points under deadlines while
    /// `--full`'s MainTarget tiers would otherwise compile beside it. It is
    /// LAST, so nothing waits behind it.
    #[test]
    fn only_the_stages_that_judge_deadlines_and_timings_are_exclusive() {
        let exclusive = |mode| -> Vec<StageId> {
            plan(&ctx(mode, Scope::workspace()))
                .iter()
                .filter(|s| s.exclusive)
                .map(|s| s.id)
                .collect()
        };
        assert_eq!(
            exclusive(Mode::Full),
            [
                StageId::DeadlineTests,
                StageId::MeasuringTests,
                StageId::ControlSocketSmoke,
                StageId::GuiSmoke,
                StageId::CodexLiveUpgrade,
            ]
        );
        assert_eq!(
            plan(&ctx(Mode::Full, Scope::workspace()))
                .last()
                .map(|s| s.id),
            Some(StageId::CodexLiveUpgrade)
        );
        assert_eq!(
            exclusive(Mode::Fast),
            [StageId::DeadlineTests, StageId::ControlSocketSmoke]
        );
        assert_eq!(
            exclusive(Mode::Measure),
            [StageId::MeasuringTests, StageId::GuiSmoke]
        );
    }

    /// THE TESTS THE RUN SKIPS RUN ALONE, EACH IN THE TIER THAT OWNS IT
    /// (2026-09-26). The test run skips both lists, so a plan without their
    /// stage would drop them silently: every mode that runs the test run over
    /// aterm-update runs the deadline tests, as the LAND tier's first barrier,
    /// after it; every mode that runs the MEASURE tier over aterm-conformance
    /// runs the measuring tests, exclusive, after the conformance-release
    /// prime they use — and after the deadline tests when both run.
    #[test]
    fn the_skipped_tests_run_alone_in_the_tier_that_owns_them_in_every_scope() {
        for mode in MODES {
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-grid"),
                Scope::crate_only("aterm-update"),
                Scope::changed("main", vec!["aterm-conformance".into()], true),
                Scope::changed("main", vec![], true),
            ] {
                let p = plan(&ctx(mode, scope.clone()));
                let what = format!("{mode:?} / {}", scope.label());
                let at = |id| p.iter().position(|s| s.id == id);
                let barrier = p.iter().position(|s| s.exclusive).expect("exclusive");
                assert_eq!(at(StageId::Test).is_some(), mode.runs(Tier::Land), "{what}");
                let deadline = at(StageId::DeadlineTests);
                assert_eq!(
                    deadline.is_some(),
                    mode.runs(Tier::Land) && holds_deadline_tests(&scope),
                    "{what}"
                );
                if let Some(deadline) = deadline {
                    assert!(p[deadline].exclusive, "{what}");
                    assert_eq!(p[deadline].lane, Lane::MainTarget, "{what}");
                    assert!(at(StageId::Test).is_some_and(|t| t < deadline), "{what}");
                    assert_eq!(barrier, deadline, "{what}: the first barrier");
                }
                let measuring = at(StageId::MeasuringTests);
                assert_eq!(
                    measuring.is_some(),
                    mode.runs(Tier::Measure) && measures_paint_and_spin(&scope),
                    "{what}"
                );
                if let Some(measuring) = measuring {
                    assert!(p[measuring].exclusive, "{what}");
                    assert_eq!(p[measuring].lane, Lane::MainTarget, "{what}");
                    let prime = at(StageId::ConformanceRelease).expect("the prime");
                    assert!(prime < measuring, "{what}: the prime must finish first");
                    match deadline {
                        Some(deadline) => assert!(deadline < measuring, "{what}"),
                        None => assert_eq!(barrier, measuring, "{what}: the first barrier"),
                    }
                }
            }
        }
    }

    #[test]
    fn each_cargo_lane_names_its_own_target_dir() {
        let mut c = ctx(Mode::Fast, Scope::workspace());
        let dir = |c: &Ctx, l| lane_dir(c, l).map(|p| p.display().to_string());
        assert_eq!(dir(&c, Lane::Pure), None);
        assert_eq!(dir(&c, Lane::MainTarget).as_deref(), Some("/repo/target"));
        assert_eq!(
            dir(&c, Lane::TippyTarget).as_deref(),
            Some("/repo/target-tippy")
        );
        assert_eq!(
            dir(&c, Lane::XtaskTarget).as_deref(),
            Some("/repo/target-xtask")
        );
        assert_eq!(
            dir(&c, Lane::DriverTarget).as_deref(),
            Some("/repo/target-drivers")
        );
        // THE HELPER'S OWN PATH, not a path of this gate's choosing:
        // `release_bin` builds into `<root>/target/conformance-release`, and a
        // lane that named any other directory would prime an artifact nothing
        // reads while the suites still built their own.
        assert_eq!(
            dir(&c, Lane::ConformanceRelease).as_deref(),
            Some("/repo/target/conformance-release")
        );
        assert_eq!(
            dir(&c, Lane::FreezeGateTarget).as_deref(),
            Some("/repo/tools/freeze-safety-gate/target")
        );
        assert_eq!(
            dir(&c, Lane::LibcOracleTarget).as_deref(),
            Some("/repo/libc-oracle/target")
        );
        // The driven binaries never share the main lane's directory: the test
        // run would relink them under a smoke, a rung or a pack that is
        // driving them.
        assert_ne!(
            lane_dir(&c, Lane::DriverTarget),
            lane_dir(&c, Lane::MainTarget)
        );
        // A relative root still yields absolute side-lane dirs.
        c.root = PathBuf::from("relative-repo");
        let d = lane_dir(&c, Lane::XtaskTarget).expect("a cargo lane");
        assert!(
            d.is_absolute() && d.ends_with("relative-repo/target-xtask"),
            "{}",
            d.display()
        );
    }
}
