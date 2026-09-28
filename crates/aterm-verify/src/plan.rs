// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LADDER, as data: which stages run, in which order, contending for what.
//!
//! The order is the script's order and is part of the contract — reviewers and
//! agents read these runs top to bottom and know where to look. Concurrency
//! changes when a stage RUNS, never where it PRINTS.

use std::path::PathBuf;

use crate::Ctx;
use crate::cli::Mode;

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
    /// smokes' `aterm-gui`/`aterm-ctl`, the redraw harness, the eight objc
    /// drivers, the `driver builds` stage that pre-compiles all of them,
    /// (since 2026-09-14) the sealed fabric rung, whose suite boots real
    /// `aterm-gui`s, and (since 2026-09-16) the atpkg publish tooling, whose
    /// end-to-end pack suite drives a real `atpkg`. Only stages in this lane
    /// write its uplifted binaries, so no other stage can relink one under a
    /// smoke, under the rung or under the pack (in `target/` the test stage
    /// relinked `aterm-gui` with dev features).
    DriverTarget,
    /// `target/conformance-release/` — the RELEASE `aterm` the live-conformance
    /// suites JUDGE, and the one lane this gate did not invent: it is the exact
    /// directory `crates/aterm-conformance/tests/support/mod.rs`'s `release_bin`
    /// builds into, so [`lane_dir`] must spell it the way that helper spells it
    /// or the artifact gets built twice instead of primed once.
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
    /// The sealed cross-host rung of the fabric bridge, `--features sealed`. A
    /// DRIVER-lane stage: its suite boots real `aterm-gui`s (see `plan`).
    SealedLane,
    Tippy,
    Formatting,
    GrepGuards,
    /// The offline suites over the scripts that build, sign, install and
    /// publish the app (`stages::DELIVERY_SUITES`). "Release tooling" until
    /// 2026-09-26, when dev-app's signing identity and cargo pin joined it
    /// (and, 2026-09-27, the fabric demo's cleanup check).
    DeliveryTooling,
    AtpkgTooling,
    TrustContractProbe,
    /// `--full` only: the startup-comparison harness's own test.
    StartCompare,
    LibcOracle,
    FreezeGate,
    DriverBuilds,
    /// The RELEASE `aterm` the paint and spin suites judge, built in its own
    /// lane at t0 so the measuring stage finds it warm.
    ConformanceRelease,
    /// The tests the parallel run skips because they MEASURE the machine
    /// (`stages::MEASURING_TESTS`), run with nothing else in flight.
    MeasuringTests,
    ControlSocketSmoke,
    GuiSmoke,
    RedrawConformance,
    ObjcClassAudit,
    ObjcImeDrive,
    ObjcToolbarDrive,
    ObjcWindowDrive,
    ObjcEventDrive,
    ObjcAlertDrive,
    ObjcSwizzleDrive,
    ObjcBoundDrive,
    /// The aterm-gui unit tests that open a WindowServer connection, run here
    /// and in no parallel test run (`stages::window_server_tests_args`).
    WindowServerTests,
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

/// A stage's identity, its ladder header, and its scheduling constraints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageSpec {
    pub id: StageId,
    /// The `=== … ===` header, resolved against the scope where it varies.
    pub title: String,
    pub lane: Lane,
    /// Runs with nothing else in flight. Only the measuring tests and the two
    /// smokes, and only because they measure frame rates, latencies and
    /// deadlines: a stage whose verdict depends on how busy the machine is must
    /// own the machine.
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
        // Spelled as `release_bin` spells it — `root.join("target/conformance-release")`
        // in `crates/aterm-conformance/tests/support/mod.rs`. A second spelling
        // of the same artifact is a second BUILD, which is the one thing this
        // lane exists to prevent.
        Lane::ConformanceRelease => under_root("target/conformance-release"),
    }
}

/// Will this run's measuring stage run a suite that builds the conformance
/// RELEASE artifact for itself?
///
/// Two do — `aterm-conformance`'s `paint` and `spin` — and both reach it through
/// the same `release_bin` helper, so the question is exactly "does the selection
/// compile that crate" (atpkg's `untracked_stage`, the third, went with the
/// untracked lanes on 2026-09-24). Whole-tree always does. A narrowing that selects neither runs
/// no suite that would ever open the lane, and priming it would be a minutes-long
/// build for nobody — the same reason the sealed lane is REMOVED rather than
/// skipped under a scope that has nothing for it.
#[must_use]
fn primes_conformance_release(ctx: &Ctx) -> bool {
    ctx.scope.includes_crate("aterm-conformance")
}

/// Build the run's stage list.
///
/// Two things can remove a stage entirely (as opposed to skipping it):
///  * `--scope` narrowing away from `aterm-link` drops the sealed fabric lane,
///    and away from `aterm-conformance` the conformance-release prime
///    ([`primes_conformance_release`]), because there is nothing for either of
///    them to run;
///  * `--fast` drops the four `--full`-only stages.
///
/// Nothing else is conditional here. Absent TOOLS produce skips inside a stage,
/// never a missing stage, because a stage that vanishes is a stage nobody
/// notices was not run.
#[must_use]
pub fn plan(ctx: &Ctx) -> Vec<StageSpec> {
    let label = ctx.scope.label();
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
        // compile; the tests that measure the machine run in their own exclusive
        // stage (`StageId::MeasuringTests`), so the run no longer holds its
        // start for a compile.
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
    // DELIVERY TOOLING — "release tooling (installer update channel, export
    // policy, release preflight, after-cut; stubbed)" until 2026-09-26, when
    // five orphaned suites joined it and two of them made "release" false:
    // dev-app's signing identity and the cargo pin dev-app shares with the
    // installer (`stages.rs` section 3.5 has the roster and the reason).
    v.push(spec(
        StageId::DeliveryTooling,
        "delivery tooling (installer channel and guards, cargo pin, export policy, release preflight, shape and after-cut, site sync, dev signing identity, fabric demo cleanup; offline)",
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
    // DRIVER BUILDS (2026-09-13): the smoke, redraw and objc binaries — and,
    // since 2026-09-26, aterm-gui's library test binary for the window-server
    // unit tests — compiled at t0 in their own lane. Every driver stage below still runs its
    // own build argv — a fingerprint no-op after this — so nothing is proven
    // from this row's binaries that its own stage did not ask cargo for. What
    // it removes is those compiles running one after another inside the
    // exclusive tail. Declared here, before the smokes, because a stage after
    // an exclusive barrier could not start until the barrier finished.
    v.push(spec(
        StageId::DriverBuilds,
        "driver builds (smoke, redraw and objc binaries, the live lanes' aterm, and the window-server test binary)",
        Lane::DriverTarget,
    ));
    // THE CONFORMANCE RELEASE ARTIFACT (2026-09-22) — the driver-builds row's
    // twin, one lane over, and for the identical reason: a build the ladder
    // used to pay for INSIDE a stage that measures, while every other core
    // idled.
    //
    // `crates/aterm-conformance/tests/support/mod.rs`'s `release_bin` builds
    // `--locked --release -p aterm` into `target/conformance-release` and hands
    // the binary to the paint and spin suites — which judge the RELEASE
    // artifact and refuse to run without one. Cargo runs test binaries ONE AT A
    // TIME, so whichever of the two sorts first pays for
    // that whole release build inside the test stage's own time, with nothing
    // else in the gate running. MEASURED on a `--fast` run of 2026-09-22
    // (`--timings`): `test (--workspace)` was 33 of the run's 36
    // minutes, and its two slowest suites were `untracked_stage` (424 s, deleted
    // 2026-09-24) and `paint` (417 s) out of 581.
    //
    // This row runs THE SAME argv, in THE SAME directory, from THE SAME cwd —
    // `stages::conformance_release_cmd` is the one place that argv is written,
    // and `stages.rs`'s tests pin it against the helper's source. Measured
    // 2026-09-22 on the owner's Mac: the build is 325.9 s cold and 0.18 s warm
    // when both invocations carry the same environment. They do NOT today, and
    // `stages.rs`'s section header says exactly how much of it the suites still
    // redo and why (cargo's own per-package `CARGO_*` variables, forwarded by
    // the helper into its nested cargo, which two build scripts track).
    //
    // NOT in `after_lanes` of anything, and nothing waits on this lane: the
    // point is to overlap the test compile, and a waiter would put the cost
    // back on the critical path it was taken off. Nor is it a MEASUREMENT risk: the
    // two suites that use this artifact are the measuring stage's, which is
    // exclusive, so neither of them can start before this row has finished.
    if primes_conformance_release(ctx) {
        v.push(spec(
            StageId::ConformanceRelease,
            "conformance release artifact (paint/spin build it otherwise)",
            Lane::ConformanceRelease,
        ));
    }
    // THE SEALED FABRIC RUNG (2026-09-14), a DRIVER-lane stage declared behind
    // the driver builds. `tests/two_nodes_sealed.rs` is `#![cfg(feature =
    // "sealed")]`, so the workspace test run compiles the one test covering the
    // vendored astream-aead to nothing and this row is its only cadence. Its
    // suite boots real `aterm-gui --headless` processes, which aterm-link cannot
    // declare (`CARGO_BIN_EXE_` names its own package's binaries only), so the
    // harness FINDS one — first in the target dir the test binary was built
    // into — and refuses one older than any source cargo's depfile names.
    //
    // It first ran at t0 in a `target-sealed/` of its own, which never builds
    // `aterm-gui`, so the harness fell through to `target/`'s: still being
    // relinked by the build stage on a warm gate, absent on a cold one.
    // MEASURED on a warm gate after a commit that touched an aterm-gui
    // dependency: the rung ran 2.0–71.4 s, the build 2.0–116.9 s, and 5 of its
    // 9 tests were refused with "aterm-gui is STALE". The guard was right; the
    // order was wrong.
    //
    // So it runs where the gate's driven `aterm-gui` is built: the suite is
    // compiled into `target-drivers/`, which makes that binary the harness's
    // first search; the stage's first child is the smokes' own build argv (a
    // fingerprint no-op after the row above, a real build if anything moved)
    // and the suite starts only if it succeeded; and this lane's order keeps
    // the whole row behind the driver builds, with nothing else writing the
    // lane's binaries while it runs. Declared BEFORE the smokes' barrier, so
    // the test run still waits for it and never overlaps it; it waits on no
    // lanes itself, so `sched::check_after_lanes` still holds.
    if ctx.scope.includes_sealed_lane() {
        v.push(spec(
            StageId::SealedLane,
            "sealed fabric lane (aterm-link --features sealed)",
            Lane::DriverTarget,
        ));
    }
    // THE ATPKG PUBLISH TOOLING (2026-09-16) — the driver lane's other row, and
    // it moved here for the reason the rung above did. Its third suite
    // (`tools/test-atpkg-pack-one-compiler.sh`, section D) PACKS a sysroot
    // bundle with a real `atpkg`, and on a host that can run that pack (macOS
    // with cc, codesign and zstd) it treats a missing binary as a GAP in the
    // run and FAILS, rather than printing PASS over half its checks. It
    // resolved that binary as `$ATPKG`, else `<root>/target/debug/atpkg`, else
    // the release one — and a `Lane::Pure` stage starting at t0 can only read
    // those from a PREVIOUS run.
    //
    // MEASURED on 86381efbf (`--timings`): the row ran 2.685 s ->
    // 112.393 s and FAILED with "section D (the real pack end to end) cannot
    // run: no atpkg binary", while the workspace build that writes
    // `target/debug/atpkg` ran 2.685 s -> 805.370 s. Three earlier gates passed
    // the row only because a STALE `atpkg` from an earlier run sat in the warm
    // snapshot; on a cold one it could never pass.
    //
    // So it is a driver-lane stage: its first child is `targo build -q -p
    // atpkg` in `target-drivers/`, the pack suite runs only if that built and
    // is handed `$ATPKG` = the binary it just built (which puts the stale
    // `<root>/target` fallback out of reach), and this lane's order keeps the
    // whole row behind the driver builds, with nothing else writing the lane's
    // binaries while it runs. Declared BEFORE the smokes' barrier, so the test
    // run still waits for it and never overlaps it; it waits on no lanes
    // itself, so `sched::check_after_lanes` still holds.
    //
    // THE DRIVER LANE rather than one of its own, measured: of the 49 units
    // `-p atpkg` needs, 48 are units this lane already holds (unit graphs,
    // 2026-09-16) — 44 of them at an identical feature set, and 4 (`ring`'s
    // three units and `aterm-types`) at a different one, which cargo keys and
    // caches separately and therefore never relinks out from under the
    // `aterm-gui` the smokes drive. A lane of its own would compile all 49
    // from cold on every gate, which is the second `aterm-gui` mistake
    // `target-sealed/` made.
    v.push(spec(
        StageId::AtpkgTooling,
        "atpkg publish tooling (index/publish/pack rows; stubbed)",
        Lane::DriverTarget,
    ));
    // THE MEASURING TESTS (2026-09-23), the first exclusive stage: the paint
    // and spin matrices and aterm-update's launchd copy tests, which the test
    // run skips (`stages::MEASURING_TESTS`). They
    // judge frame timings and launchd deadlines, so like the smokes below they
    // run with nothing else in flight: every stage above has finished first,
    // the conformance-release prime they share included.
    v.push(exclusive(
        StageId::MeasuringTests,
        format!("measuring tests ({label}; run alone)"),
        Lane::MainTarget,
    ));
    v.push(exclusive(
        StageId::ControlSocketSmoke,
        "control-socket smoke",
        Lane::DriverTarget,
    ));
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
    // THE MODAL DRIVER (W13), unconditional like the five above it. W13 ported
    // `aterm-gui`'s last five `objc2` files, and four of them are one
    // subsystem — `alert_keys.rs`, `menu::confirm`, the paste sheet and
    // `app_introspect.rs` — that NONE of the drivers above touches: no
    // `NSAlert`, no sheet, no completion block, no local event monitor, no
    // `NSBitmapImageRep`. Its cheaper half, `gui_sent_prototypes.rs`, reads
    // the encoding of every selector those files send; a census cannot see a
    // correct send of the wrong selector, a block whose ABI is right and whose
    // ownership is wrong, or a capture that answers bytes which are not a PNG.
    // This one presents the real alert, reads the first button's key
    // equivalent off AppKit, attaches the sheet, drives a keyDown through the
    // swizzled `-sendEvent:` into the installed monitor, clicks, and reads the
    // response the copied completion block was handed.
    v.push(spec(
        StageId::ObjcAlertDrive,
        "objc alert drive (the real NSAlert: its buttons and key equivalent, the sheet and its copied completion block, the local key monitor, the menu bar walk and the chrome capture)",
        Lane::DriverTarget,
    ));
    // THE SWIZZLE DRIVER (W12), unconditional for the reason its module
    // states: NO ENCODING CHECK CAN SEE A SWIZZLE. `SwizzleSite` is what
    // installs the fork's `-[NSApplication sendEvent:]` override — the row
    // the containment's stop-outside/contain-inside order lives on — and
    // `tests/swizzle.rs` can only measure it on classes of its own making.
    // This one runs it against the live AppKit class: Apple's own encoding
    // for the row, the IMP's Mach-O image moving from AppKit into this
    // executable (part A2 of the live-class audit, drawn on the capability
    // itself), a real `NSEvent` through both halves of the chain, and the
    // prototype check refusing a wrong prototype.
    v.push(spec(
        StageId::ObjcSwizzleDrive,
        "objc swizzle drive (SwizzleSite against the live -[NSApplication sendEvent:]: Apple's encoding, the IMP's image, a real NSEvent through both halves of the chain, and the refusal)",
        Lane::DriverTarget,
    ));
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
    // THE WINDOW-SERVER UNIT TESTS (2026-09-26), unconditional like the drivers
    // above it and in their lane for their reason: these rows open a
    // WindowServer connection, which is the one thing AGENTS.md's "Concurrent
    // sessions" rule 5 keeps out of every parallel test run. On 2026-08-17 and
    // 2026-09-01 a unit test binary doing that from a bloated
    // `target/debug/deps` held WindowServer's main thread in a synchronous TCC
    // preflight past its 40 s watchdog, and the watchdog killed every window on
    // the machine. The rows are `#[ignore]`d in `mod window_server` blocks
    // (`tools/grep_guard.sh` B9e); `targo test` skips them everywhere and this
    // stage runs exactly them, from the driver lane's own target dir, after the
    // smokes' barrier. Never removed by a narrowing: the rows are aterm-gui's,
    // and a gate a narrowing can drop stops running when someone is in a hurry.
    v.push(spec(
        StageId::WindowServerTests,
        "window-server unit tests (aterm-gui's AppKit rows that open a WindowServer connection, ignored in every parallel test run)",
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
    if ctx.mode == Mode::Full {
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
    }
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
    use crate::scope::Scope;
    use std::path::PathBuf;

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
                StageId::ConformanceRelease,
                StageId::SealedLane,
                StageId::AtpkgTooling,
                StageId::MeasuringTests,
                StageId::ControlSocketSmoke,
                StageId::GuiSmoke,
                StageId::RedrawConformance,
                StageId::ObjcClassAudit,
                StageId::ObjcImeDrive,
                StageId::ObjcToolbarDrive,
                StageId::ObjcWindowDrive,
                StageId::ObjcEventDrive,
                StageId::ObjcAlertDrive,
                StageId::ObjcSwizzleDrive,
                StageId::ObjcBoundDrive,
                StageId::WindowServerTests,
                StageId::ForegroundHandback,
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

    /// THE SEALED RUNG IS ORDERED BEHIND THE aterm-gui IT DRIVES (2026-09-14).
    ///
    /// Its suite finds `aterm-gui` in the target dir it was built into and
    /// refuses a stale one. When it ran at t0 in a lane of its own, nothing
    /// ordered it behind any build of that binary, and a warm gate measured 5 of
    /// its 9 tests refused "aterm-gui is STALE" while the build stage was still
    /// linking. It must sit in the lane whose driver builds produce the binary,
    /// declared after them and before the smokes' barrier (so the test run
    /// still awaits it), and wait on no lanes itself. `stages.rs` pins that its
    /// commands build into that same lane's dir, and `sched.rs` model-checks
    /// that it never starts before the driver builds finish.
    #[test]
    fn the_sealed_rung_is_ordered_behind_the_driver_builds_of_the_aterm_gui_it_drives() {
        for mode in [Mode::Fast, Mode::Full] {
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-link"),
                Scope::changed("main", vec!["aterm-link".into()], true),
            ] {
                let p = plan(&ctx(mode, scope.clone()));
                let at = |id| p.iter().position(|s| s.id == id);
                let sealed = at(StageId::SealedLane).expect("the sealed rung is planned");
                let builds = at(StageId::DriverBuilds).expect("driver builds are planned");
                let test = at(StageId::Test).expect("test");
                let barrier = p
                    .iter()
                    .position(|s| s.exclusive)
                    .expect("an exclusive stage");
                let what = format!("{mode:?} / {}", scope.label());
                assert_eq!(p[sealed].lane, Lane::DriverTarget, "{what}");
                assert_eq!(p[builds].lane, p[sealed].lane, "{what}");
                assert!(
                    builds < sealed && sealed < barrier,
                    "{what}: the rung must follow the driver builds and precede the smokes"
                );
                assert!(p[sealed].after_lanes.is_empty(), "{what}");
                assert!(
                    crate::sched::awaited(&p, test).any(|j| j == sealed),
                    "{what}: the test run must still wait for the rung"
                );
            }
        }
    }

    /// THE ATPKG PUBLISH TOOLING IS ORDERED BEHIND THE atpkg IT DRIVES
    /// (2026-09-16).
    ///
    /// Its third suite packs a real sysroot bundle with a real `atpkg`, which
    /// it resolves as `$ATPKG`, else `<root>/target/debug/atpkg`, else the
    /// release one — and on a host that can run that pack it FAILS rather than
    /// skipping when there is none. At t0 in `Lane::Pure` it could only ever
    /// find a PREVIOUS run's binary: measured on 86381efbf, FAIL at 2.685 s ->
    /// 112.393 s while the build that writes that path ran to 805.370 s. So it
    /// must sit in the lane whose dir its own build child writes, declared
    /// after the driver builds and before the smokes' barrier (so the test run
    /// still awaits it), waiting on no lanes itself — and it must never again
    /// be `Lane::Pure`, which is what a t0 start means here. `stages.rs` pins
    /// the build child and the `$ATPKG` binding; `sched.rs` model-checks that
    /// it never starts before the driver builds finish.
    #[test]
    fn the_atpkg_tooling_is_ordered_behind_the_build_of_the_atpkg_it_drives() {
        for mode in [Mode::Fast, Mode::Full] {
            for scope in [
                Scope::workspace(),
                Scope::crate_only("atpkg"),
                Scope::crate_only("aterm-grid"),
                Scope::changed("main", vec!["aterm-gui".into()], true),
                Scope::changed("main", vec![], true),
            ] {
                let p = plan(&ctx(mode, scope.clone()));
                let at = |id| p.iter().position(|s| s.id == id);
                let what = format!("{mode:?} / {}", scope.label());
                // A narrowing may not remove it: the suites are whole-tree.
                let atpkg = at(StageId::AtpkgTooling)
                    .unwrap_or_else(|| panic!("{what}: the atpkg publish tooling is planned"));
                let builds = at(StageId::DriverBuilds).expect("driver builds are planned");
                let test = at(StageId::Test).expect("test");
                let barrier = p
                    .iter()
                    .position(|s| s.exclusive)
                    .expect("an exclusive stage");
                assert_eq!(p[atpkg].lane, Lane::DriverTarget, "{what}");
                assert_eq!(p[builds].lane, p[atpkg].lane, "{what}");
                assert!(
                    builds < atpkg && atpkg < barrier,
                    "{what}: the row must follow the driver builds and precede the smokes"
                );
                assert!(p[atpkg].after_lanes.is_empty(), "{what}");
                assert!(
                    crate::sched::awaited(&p, test).any(|j| j == atpkg),
                    "{what}: the test run must wait for it, as it does for every \
                     non-barrier row of that lane"
                );
                crate::sched::check_after_lanes(&p).expect("the plan cannot deadlock");
            }
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

    #[test]
    fn full_adds_its_opt_in_tiers_at_the_end_and_changes_nothing_else() {
        let fast = ids(&ctx(Mode::Fast, Scope::workspace()));
        let full = ids(&ctx(Mode::Full, Scope::workspace()));
        assert_eq!(full[..fast.len()], fast[..]);
        assert_eq!(
            full[fast.len()..],
            [
                StageId::KaniFloor,
                StageId::CrossCells,
                StageId::StartCompare,
                StageId::CodexLiveUpgrade,
            ]
        );
    }

    /// THE STAGE-SET PROOF across modes and scopes. Two rows follow their
    /// own crates: the sealed lane is aterm-link's, and the conformance-release
    /// prime belongs to the crate whose suites build that artifact
    /// (aterm-conformance).
    /// Every mode and scope plans EXACTLY the whole-tree ladder of its mode
    /// minus the lane rows its crates dropped — in the same order, nothing else
    /// lost. With the literal fast ladder above and `--full`'s four appended
    /// tiers, that pins every ladder the gate can print.
    #[test]
    fn a_scope_removes_exactly_the_lanes_whose_crates_it_dropped() {
        for mode in [Mode::Fast, Mode::Full] {
            let whole = ids(&ctx(mode, Scope::workspace()));
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-grid"),
                Scope::crate_only("aterm-search"),
                Scope::changed("main", vec!["aterm-gui".into()], true),
                Scope::changed("main", vec![], true),
            ] {
                let c = ctx(mode, scope.clone());
                let expected: Vec<StageId> = whole
                    .iter()
                    .filter(|id| match id {
                        StageId::SealedLane => c.scope.includes_sealed_lane(),
                        StageId::ConformanceRelease => primes_conformance_release(&c),
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
        // A scope holding neither crate drops both.
        let scoped = ids(&ctx(Mode::Fast, Scope::crate_only("aterm-grid")));
        assert!(!scoped.contains(&StageId::SealedLane));
        assert!(!scoped.contains(&StageId::ConformanceRelease));
        let link_only = ids(&ctx(Mode::Fast, Scope::crate_only("aterm-link")));
        assert!(link_only.contains(&StageId::SealedLane));
    }

    /// THE PRIME FOLLOWS THE SUITES THAT WOULD OTHERWISE BUILD IT.
    ///
    /// `release_bin` is reached from `aterm-conformance`'s paint and spin suites
    /// and from nowhere else (grep of the tree, 2026-09-24; atpkg's
    /// untracked-staging suite was the other caller until then). So the row is
    /// planned whole-tree, planned whenever a narrowing selects that crate, and
    /// REMOVED — not skipped — when it does not, because a minutes-long release build for a test
    /// stage that will never open the directory is the opposite of what this row
    /// is for.
    #[test]
    fn the_conformance_release_prime_follows_the_crates_whose_suites_build_it() {
        let has = |scope: Scope| {
            for mode in [Mode::Fast, Mode::Full] {
                let c = ctx(mode, scope.clone());
                assert_eq!(
                    ids(&c).contains(&StageId::ConformanceRelease),
                    primes_conformance_release(&c),
                    "{mode:?} / {}",
                    scope.label()
                );
            }
            ids(&ctx(Mode::Fast, scope)).contains(&StageId::ConformanceRelease)
        };
        assert!(has(Scope::workspace()), "--fast and --full both prime it");
        assert!(has(Scope::crate_only("aterm-conformance")));
        assert!(
            !has(Scope::crate_only("atpkg")),
            "no atpkg suite opens the lane since its untracked-staging suite went"
        );
        assert!(has(Scope::changed(
            "main",
            vec!["aterm-grid".into(), "aterm-conformance".into()],
            true
        )));
        assert!(!has(Scope::crate_only("aterm-grid")));
        assert!(!has(Scope::changed("main", vec!["aterm-gui".into()], true)));
        assert!(
            !has(Scope::changed("main", vec![], true)),
            "a selection that compiles nothing runs no suite either"
        );
    }

    /// THE PRIME IS SCHEDULED TO OVERLAP, WHICH IS ITS WHOLE POINT.
    ///
    /// It must start at t0 (declared before the first exclusive barrier, so no
    /// barrier can hold it back), own a lane nothing else is in (so no earlier
    /// stage in it can queue it), wait on no lanes, never be exclusive, and —
    /// above all — be awaited by NOBODY: a waiter would put the release build
    /// back on the critical path it was taken off. `target/conformance-release`
    /// is not `target/`, so the main lane's build never queues on it either.
    #[test]
    fn the_conformance_release_prime_overlaps_everything_and_blocks_nobody() {
        for mode in [Mode::Fast, Mode::Full] {
            for scope in [Scope::workspace(), Scope::crate_only("aterm-conformance")] {
                let p = plan(&ctx(mode, scope.clone()));
                let what = format!("{mode:?} / {}", scope.label());
                let prime = p
                    .iter()
                    .position(|s| s.id == StageId::ConformanceRelease)
                    .unwrap_or_else(|| panic!("{what}: the prime is planned"));
                assert_eq!(p[prime].lane, Lane::ConformanceRelease, "{what}");
                assert_ne!(p[prime].lane, Lane::MainTarget, "{what}");
                assert!(!p[prime].exclusive, "{what}");
                assert!(p[prime].after_lanes.is_empty(), "{what}");
                // Alone in its lane: nothing serialises in front of it.
                assert_eq!(
                    p.iter()
                        .filter(|s| s.lane == Lane::ConformanceRelease)
                        .count(),
                    1,
                    "{what}"
                );
                // Before the first barrier, so it is startable at t0.
                let barrier = p
                    .iter()
                    .position(|s| s.exclusive)
                    .expect("an exclusive stage");
                assert!(prime < barrier, "{what}: a barrier would delay it");
                let nothing_yet = vec![false; p.len()];
                assert!(
                    crate::sched::ready(&p, &nothing_yet, &nothing_yet, 0, prime),
                    "{what}: the prime must be startable with nothing else finished"
                );
                // And nobody waits for it.
                assert!(
                    p.iter()
                        .all(|s| !s.after_lanes.contains(&Lane::ConformanceRelease)),
                    "{what}: a waiter puts the build back on the critical path"
                );
                for i in 0..p.len() {
                    assert!(
                        !crate::sched::awaited(&p, i).any(|j| j == prime),
                        "{what}: {:?} awaits the prime",
                        p[i].id
                    );
                }
                crate::sched::check_after_lanes(&p).expect("the plan cannot deadlock");
            }
        }
    }

    /// The measuring stages own the machine because they judge timings; the
    /// Codex live upgrade (`--full` only) because it waits on a real Codex's
    /// idle points under deadlines while `--full`'s MainTarget tiers would
    /// otherwise compile beside it. It is LAST, so nothing waits behind it.
    #[test]
    fn only_the_measuring_stages_and_the_codex_live_upgrade_are_exclusive() {
        let p = plan(&ctx(Mode::Full, Scope::workspace()));
        let ex: Vec<StageId> = p.iter().filter(|s| s.exclusive).map(|s| s.id).collect();
        assert_eq!(
            ex,
            [
                StageId::MeasuringTests,
                StageId::ControlSocketSmoke,
                StageId::GuiSmoke,
                StageId::CodexLiveUpgrade,
            ]
        );
        assert_eq!(p.last().map(|s| s.id), Some(StageId::CodexLiveUpgrade));
        let fast = plan(&ctx(Mode::Fast, Scope::workspace()));
        let ex: Vec<StageId> = fast.iter().filter(|s| s.exclusive).map(|s| s.id).collect();
        assert_eq!(
            ex,
            [
                StageId::MeasuringTests,
                StageId::ControlSocketSmoke,
                StageId::GuiSmoke
            ]
        );
    }

    /// THE MEASURING TESTS RUN ALONE, IN EVERY TIER AND SCOPE (2026-09-23).
    /// The test run skips them, so a plan without this stage would drop them
    /// silently; and they judge timings, so the stage must be exclusive and
    /// start only after everything declared before it — the test run and the
    /// conformance-release prime they use included — has finished.
    #[test]
    fn the_measuring_tests_run_alone_after_the_test_run_in_every_tier_and_scope() {
        for mode in [Mode::Fast, Mode::Full] {
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-grid"),
                Scope::changed("main", vec!["aterm-conformance".into()], true),
                Scope::changed("main", vec![], true),
            ] {
                let p = plan(&ctx(mode, scope.clone()));
                let what = format!("{mode:?} / {}", scope.label());
                let at = |id| p.iter().position(|s| s.id == id);
                let measuring = at(StageId::MeasuringTests)
                    .unwrap_or_else(|| panic!("{what}: the measuring tests are planned"));
                assert!(p[measuring].exclusive, "{what}");
                assert_eq!(p[measuring].lane, Lane::MainTarget, "{what}");
                assert!(at(StageId::Test).is_some_and(|t| t < measuring), "{what}");
                if let Some(prime) = at(StageId::ConformanceRelease) {
                    assert!(prime < measuring, "{what}: the prime must finish first");
                }
                let barrier = p.iter().position(|s| s.exclusive).expect("exclusive");
                assert_eq!(barrier, measuring, "{what}: it is the first barrier");
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
