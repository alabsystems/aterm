// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LADDER, as data: which stages run, in which order, contending for what.
//!
//! The order is the script's order and is part of the contract — reviewers and
//! agents read these runs top to bottom and know where to look. Concurrency
//! changes when a stage RUNS, never where it PRINTS.

use std::path::{Path, PathBuf};

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
    /// `target/` — the workspace build, its tests and doctests, and the three
    /// `--full` tiers. (Until 2026-09-13 also the regex lane, the xtask verbs
    /// and every driven binary; those have their own lanes below.)
    MainTarget,
    /// `target-tippy/` — the lint keeps a SEPARATE target dir so the trust
    /// toolchain's artifacts stay off the stock build. That is also what makes
    /// it free to run beside the build.
    TippyTarget,
    /// `tools/freeze-safety-gate/` is its own workspace with its own target dir.
    FreezeGateTarget,
    /// `libc-oracle/{target,target-symgate}/` — the nested reference workspace
    /// and its emitted-symbol gate. The oracle owns both directories and may
    /// run beside the main workspace without contending for Cargo's lock.
    LibcOracleTarget,
    /// `target-regex/` — the regex search lane alone. Moved off `target/` on
    /// 2026-09-13: `-p aterm-search --features regex` resolves a feature set
    /// the workspace test never builds, so in `target/` it compiled its own
    /// variants serially, after the doctests. Same argv, own lock, t0.
    RegexTarget,
    /// `target-xtask/` — the xtask-verb stages (formatting, feature gates, the
    /// proof inventory). The verbs they run spawn no cargo (grep of
    /// `crates/xtask/src/gate.rs`, 2026-09-13), so the only lock they take is
    /// the one `targo run -p xtask` takes to build xtask.
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
    /// contends with nothing the build stage is doing.
    ConformanceRelease,
}

/// Every stage of the gate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StageId {
    Build,
    Test,
    Doctests,
    RegexLane,
    /// The sealed cross-host rung of the fabric bridge, `--features sealed`. A
    /// DRIVER-lane stage: its suite boots real `aterm-gui`s (see `plan`).
    SealedLane,
    Tippy,
    Formatting,
    GrepGuards,
    /// The hermetic suites over the release scripts (`stages::RELEASE_SUITES`).
    ReleaseTooling,
    AtpkgTooling,
    TrustGateVerdict,
    TrustContractProbe,
    StartCompare,
    LicenseHeaders,
    FeatureGates,
    LibcOracle,
    FreezeGate,
    ProofInventory,
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
    DifferentialOracle,
    KaniFloor,
    CrossCells,
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
    /// it: before 2026-09-13 the regex lane, the xtask verbs and the driver
    /// builds were serialised behind it in `target/`, so waiting for their new
    /// lanes keeps it at least as isolated as it was.
    pub after_lanes: Vec<Lane>,
}

/// The target directory a lane's cargo children use. `None` for [`Lane::Pure`],
/// which has none.
///
/// `MainTarget` keeps the rule it always had: the caller's `CARGO_TARGET_DIR`
/// (relative to the root, as cargo reads it) or `<root>/target`, and its
/// children inherit the variable rather than being handed one. The side lanes
/// are absolute paths under the root, because they ARE handed to children whose
/// cwd is the root and a relative one would be read twice.
#[must_use]
pub fn lane_dir(ctx: &Ctx, lane: Lane) -> Option<PathBuf> {
    let under_root = |rel: &str| {
        let p = ctx.root.join(rel);
        Some(std::path::absolute(&p).unwrap_or(p))
    };
    match lane {
        Lane::Pure => None,
        Lane::MainTarget => Some(ctx.env.cargo_target_dir.as_deref().map_or_else(
            || ctx.root.join("target"),
            |d| {
                let d = Path::new(d);
                if d.is_absolute() {
                    d.to_path_buf()
                } else {
                    ctx.root.join(d)
                }
            },
        )),
        // Spelled as `stages::tippy_cmd` spells it, which is unchanged.
        Lane::TippyTarget => Some(ctx.root.join("target-tippy")),
        Lane::FreezeGateTarget => under_root("tools/freeze-safety-gate/target"),
        Lane::LibcOracleTarget => under_root("libc-oracle/target"),
        Lane::RegexTarget => under_root("target-regex"),
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
/// build for nobody — the same reason the regex and sealed lanes are REMOVED
/// rather than skipped under a scope that has nothing for them.
#[must_use]
fn primes_conformance_release(ctx: &Ctx) -> bool {
    ctx.scope.includes_crate("aterm-conformance")
}

/// Build the run's stage list.
///
/// Two things can remove a stage entirely (as opposed to skipping it):
///  * `--scope` narrowing away from `aterm-search` drops the regex lane, away
///    from `aterm-link` the sealed fabric lane, and away from
///    `aterm-conformance` the conformance-release prime
///    ([`primes_conformance_release`]), because there is nothing for any of them
///    to run — the script never printed the regex lane's header either;
///  * `--fast` drops the two `--full`-only stages.
///
/// Nothing else is conditional here. Absent TOOLS produce skips inside a stage,
/// never a missing stage, because a stage that vanishes is a stage nobody
/// notices was not run.
#[must_use]
pub fn plan(ctx: &Ctx) -> Vec<StageSpec> {
    let label = ctx.scope.label();
    let mut v = vec![
        spec(StageId::Build, format!("build ({label})"), Lane::MainTarget),
        // THE TEST RUN WAITS FOR THE SIDE LANES (2026-09-13). They start at t0
        // beside the build; the test run still owns the machine's cargo work
        // the way it did when they queued behind it in `target/`. The sealed
        // fabric rung is one of the driver lane's stages, so it is waited for
        // through `DriverTarget`.
        //
        // NOT FOR TIPPY (2026-09-23). It waited for tippy's whole-workspace
        // compile from 2026-09-18, because a paint take inside the test run was
        // measured descheduled for 50 ms beside it. The tests that measure now
        // run in their own exclusive stage (`StageId::MeasuringTests`), so the
        // run no longer holds its start for the lint.
        StageSpec {
            after_lanes: vec![Lane::RegexTarget, Lane::XtaskTarget, Lane::DriverTarget],
            ..spec(StageId::Test, format!("test ({label})"), Lane::MainTarget)
        },
        spec(
            StageId::Doctests,
            format!("doctests ({label})"),
            Lane::MainTarget,
        ),
    ];
    if ctx.scope.includes_regex_lane() {
        v.push(spec(
            StageId::RegexLane,
            "regex search lane (aterm-search --features regex)",
            Lane::RegexTarget,
        ));
    }
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
    v.push(spec(StageId::GrepGuards, "grep guards", Lane::Pure));
    v.push(spec(
        StageId::ReleaseTooling,
        "release tooling (installer update channel, export policy, release preflight, after-cut; stubbed)",
        Lane::Pure,
    ));
    v.push(spec(
        StageId::TrustGateVerdict,
        "trust-gate verdict self-test",
        Lane::Pure,
    ));
    v.push(spec(
        StageId::TrustContractProbe,
        "trust contract probe (self-field ensures proves; off-switch no ICE)",
        Lane::Pure,
    ));
    v.push(spec(
        StageId::StartCompare,
        "startup comparison scheduler",
        Lane::Pure,
    ));
    v.push(spec(StageId::LicenseHeaders, "license headers", Lane::Pure));
    v.push(spec(
        StageId::FeatureGates,
        "feature gates (drift/dormant)",
        Lane::XtaskTarget,
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
    v.push(spec(
        StageId::ProofInventory,
        "computed proof inventory",
        Lane::XtaskTarget,
    ));
    // DRIVER BUILDS (2026-09-13): the smoke, redraw and objc binaries,
    // compiled at t0 in their own lane. Every driver stage below still runs its
    // own build argv — a fingerprint no-op after this — so nothing is proven
    // from this row's binaries that its own stage did not ask cargo for. What
    // it removes is those compiles running one after another inside the
    // exclusive tail. Declared here, before the smokes, because a stage after
    // an exclusive barrier could not start until the barrier finished.
    v.push(spec(
        StageId::DriverBuilds,
        "driver builds (smoke, redraw and objc binaries)",
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
    // (`ATERM_VERIFY_TIMINGS`): `test (--workspace)` was 33 of the run's 36
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
    // point is to overlap the build stage, and a waiter would put the cost back
    // on the critical path it was taken off. Nor is it a MEASUREMENT risk: the
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
    // MEASURED on 86381efbf (`ATERM_VERIFY_TIMINGS`): the row ran 2.685 s ->
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
    if ctx.mode == Mode::Full {
        v.push(spec(
            StageId::DifferentialOracle,
            "differential oracle (aterm vs alacritty)",
            Lane::MainTarget,
        ));
        v.push(spec(
            StageId::KaniFloor,
            "trust-mc / Kani BMC floor (config-free parser harnesses)",
            Lane::MainTarget,
        ));
        // THE OTHER FOUR CELLS, COMPILED. `--fast` type-checks exactly one of
        // the five targets aterm ships for; forge measures all five and
        // compiles none. `xtask gate cells` closes that with a real compiler
        // per triple, and it is `--full`-only for one measured reason: the
        // five-cell matrix costs ~19 s warm and ~106 s cold, against a `--fast`
        // budget that exists to be paid on every commit.
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
        // What `--fast` really does not see: `#[cfg(windows)]`,
        // `#[cfg(target_os = "linux")]`, the `unix` arms that exclude Apple,
        // `#[cfg(target_arch = "wasm32")]`, and every third-party crate that
        // only appears on a non-native cell. `--full` sees all four of those
        // now: measured over the 3,245 platform `cfg` attribute sites under
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
            "cross-cell type-check (forge's five cells, each for its own triple)",
            Lane::MainTarget,
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
            false,
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
                StageId::Build,
                StageId::Test,
                StageId::Doctests,
                StageId::RegexLane,
                StageId::Tippy,
                StageId::Formatting,
                StageId::GrepGuards,
                StageId::ReleaseTooling,
                StageId::TrustGateVerdict,
                StageId::TrustContractProbe,
                StageId::StartCompare,
                StageId::LicenseHeaders,
                StageId::FeatureGates,
                StageId::LibcOracle,
                StageId::FreezeGate,
                StageId::ProofInventory,
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
            ]
        );
    }

    /// Until 2026-09-13 the regex lane, the xtask verbs and every driver build
    /// queued behind the test run in `target/`. Now they have lanes of their
    /// own and start at t0, so the test run waits for all three, and for every
    /// stage of theirs that is not behind an exclusive barrier (those cannot
    /// overlap it by the barrier rule). It does NOT wait for tippy any more
    /// (2026-09-23): that wait existed for the paint takes, which now run in
    /// the exclusive measuring stage.
    #[test]
    fn the_test_run_waits_for_every_new_side_lane() {
        for mode in [Mode::Fast, Mode::Full] {
            let p = plan(&ctx(mode, Scope::workspace()));
            let test = p.iter().position(|s| s.id == StageId::Test).expect("test");
            assert_eq!(
                p[test].after_lanes,
                [Lane::RegexTarget, Lane::XtaskTarget, Lane::DriverTarget]
            );
            let awaited: Vec<StageId> = crate::sched::awaited(&p, test).map(|j| p[j].id).collect();
            assert!(!awaited.contains(&StageId::Tippy), "{awaited:?}");
            assert_eq!(
                awaited,
                [
                    StageId::RegexLane,
                    StageId::Formatting,
                    StageId::FeatureGates,
                    StageId::ProofInventory,
                    StageId::DriverBuilds,
                    StageId::SealedLane,
                    StageId::AtpkgTooling,
                ],
                "{mode:?}"
            );
            // Everything else in those lanes sits behind the smokes' barrier.
            let barrier = p
                .iter()
                .position(|s| s.exclusive)
                .expect("an exclusive stage");
            for (j, s) in p.iter().enumerate() {
                if p[test].after_lanes.contains(&s.lane) && !awaited.contains(&s.id) {
                    assert!(
                        j >= barrier,
                        "{:?} is neither awaited nor behind the barrier",
                        s.id
                    );
                }
            }
            // No other stage waits on lanes.
            assert!(
                p.iter()
                    .filter(|s| s.id != StageId::Test)
                    .all(|s| s.after_lanes.is_empty())
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

    /// A GATE NOBODY INVOKES IS NOT A GATE. The redraw harness is the only check
    /// in the tree that can see a control `select` actually repaint a window —
    /// every `#[test]` builds its host with `proxy: None` — and for a while it
    /// sat behind an off-by-default feature that no workflow, script or stage
    /// ever named. Dropping it from the plan is how that state returns.
    #[test]
    fn the_redraw_gate_runs_in_every_tier_and_every_scope() {
        for mode in [Mode::Fast, Mode::Full] {
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-grid"),
                Scope::changed("main", vec![], true),
            ] {
                assert!(
                    ids(&ctx(mode, scope.clone())).contains(&StageId::RedrawConformance),
                    "{mode:?} / {} lost the redraw gate",
                    scope.label()
                );
            }
        }
    }

    /// THE SAME OBLIGATION, for the live-class auditor. It is the only check in
    /// the tree that reads the class `vendor/winit`'s ported delegate actually
    /// registered — the seam census checks a mirror it declares itself, and two
    /// compile-verified plants passed that mirror while the build stayed green.
    /// A gate nobody invokes is not a gate.
    #[test]
    fn the_objc_class_audit_runs_in_every_tier_and_every_scope() {
        for mode in [Mode::Fast, Mode::Full] {
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-grid"),
                Scope::changed("main", vec![], true),
            ] {
                assert!(
                    ids(&ctx(mode, scope.clone())).contains(&StageId::ObjcClassAudit),
                    "{mode:?} / {} lost the objc live-class audit",
                    scope.label()
                );
                assert!(
                    ids(&ctx(mode, scope.clone())).contains(&StageId::ObjcImeDrive),
                    "{mode:?} / {} lost the objc IME drive",
                    scope.label()
                );
                assert!(
                    ids(&ctx(mode, scope.clone())).contains(&StageId::ObjcToolbarDrive),
                    "{mode:?} / {} lost the objc toolbar drive",
                    scope.label()
                );
                assert!(
                    ids(&ctx(mode, scope.clone())).contains(&StageId::ObjcEventDrive),
                    "{mode:?} / {} lost the objc event drive",
                    scope.label()
                );
                assert!(
                    ids(&ctx(mode, scope.clone())).contains(&StageId::ObjcAlertDrive),
                    "{mode:?} / {} lost the objc alert drive",
                    scope.label()
                );
                assert!(
                    ids(&ctx(mode, scope.clone())).contains(&StageId::ObjcSwizzleDrive),
                    "{mode:?} / {} lost the objc swizzle drive",
                    scope.label()
                );
                assert!(
                    ids(&ctx(mode, scope.clone())).contains(&StageId::ObjcBoundDrive),
                    "{mode:?} / {} lost the objc bound drive",
                    scope.label()
                );
            }
        }
    }

    /// This is the only route that resolves the pinned registry libc beside the
    /// first-party replacement and executes the non-const checks. In
    /// particular, a Linux gate host's native cell is what executes the Linux
    /// pointer-constant and C-macro runtime oracle; cross `cargo check` cannot.
    #[test]
    fn the_libc_oracle_runs_in_every_tier_and_every_scope() {
        for mode in [Mode::Fast, Mode::Full] {
            for scope in [
                Scope::workspace(),
                Scope::crate_only("aterm-grid"),
                Scope::changed("main", vec![], true),
            ] {
                assert!(
                    ids(&ctx(mode, scope.clone())).contains(&StageId::LibcOracle),
                    "{mode:?} / {} lost the libc oracle",
                    scope.label()
                );
            }
        }
    }

    #[test]
    fn full_adds_the_two_opt_in_tiers_at_the_end_and_changes_nothing_else() {
        let fast = ids(&ctx(Mode::Fast, Scope::workspace()));
        let full = ids(&ctx(Mode::Full, Scope::workspace()));
        assert_eq!(full[..fast.len()], fast[..]);
        assert_eq!(
            full[fast.len()..],
            [
                StageId::DifferentialOracle,
                StageId::KaniFloor,
                StageId::CrossCells
            ]
        );
    }

    #[test]
    fn scoping_puts_the_crate_in_every_header_that_names_a_scope() {
        let p = plan(&ctx(Mode::Fast, Scope::crate_only("aterm-grid")));
        let titles: Vec<&str> = p.iter().map(|s| s.title.as_str()).collect();
        assert!(titles.contains(&"build (-p aterm-grid)"));
        assert!(titles.contains(&"test (-p aterm-grid)"));
        assert!(titles.contains(&"doctests (-p aterm-grid)"));
        assert!(titles.contains(&"tippy lint (-p aterm-grid)"));
        // The whole-tree stages keep their whole-tree titles.
        assert!(titles.contains(&"grep guards"));
        assert!(titles.contains(&"license headers"));
    }

    /// THE STAGE-SET PROOF across modes and scopes. Three rows follow their
    /// own crates: the regex lane is aterm-search's, the sealed lane is
    /// aterm-link's, and the conformance-release prime belongs to the crate
    /// whose suites build that artifact (aterm-conformance).
    /// Every mode and scope plans EXACTLY the whole-tree ladder of its mode
    /// minus the lane rows its crates dropped — in the same order, nothing else
    /// lost. With the literal fast ladder above and `--full`'s three appended
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
                        StageId::RegexLane => c.scope.includes_regex_lane(),
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
        // A scope holding none of the three crates drops all three.
        let scoped = ids(&ctx(Mode::Fast, Scope::crate_only("aterm-grid")));
        assert!(!scoped.contains(&StageId::RegexLane));
        assert!(!scoped.contains(&StageId::SealedLane));
        assert!(!scoped.contains(&StageId::ConformanceRelease));
        assert!(
            ids(&ctx(Mode::Fast, Scope::crate_only("aterm-search"))).contains(&StageId::RegexLane)
        );
        let link_only = ids(&ctx(Mode::Fast, Scope::crate_only("aterm-link")));
        assert!(link_only.contains(&StageId::SealedLane));
        assert!(!link_only.contains(&StageId::RegexLane));
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

    #[test]
    fn only_the_measuring_stages_are_exclusive() {
        let p = plan(&ctx(Mode::Full, Scope::workspace()));
        let ex: Vec<StageId> = p.iter().filter(|s| s.exclusive).map(|s| s.id).collect();
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
    fn every_cargo_stage_declares_the_target_dir_it_contends_for() {
        // A stage that lied about its lane would let two cargo invocations queue
        // on a lock the scheduler thought was free — slower, and unexplainable.
        for s in plan(&ctx(Mode::Full, Scope::workspace())) {
            let want = match s.id {
                StageId::Tippy => Lane::TippyTarget,
                StageId::FreezeGate => Lane::FreezeGateTarget,
                StageId::LibcOracle => Lane::LibcOracleTarget,
                StageId::RegexLane => Lane::RegexTarget,
                StageId::ConformanceRelease => Lane::ConformanceRelease,
                StageId::Formatting | StageId::FeatureGates | StageId::ProofInventory => {
                    Lane::XtaskTarget
                }
                StageId::DriverBuilds
                | StageId::SealedLane
                | StageId::AtpkgTooling
                | StageId::ControlSocketSmoke
                | StageId::GuiSmoke
                | StageId::RedrawConformance
                | StageId::ObjcClassAudit
                | StageId::ObjcImeDrive
                | StageId::ObjcToolbarDrive
                | StageId::ObjcWindowDrive
                | StageId::ObjcEventDrive
                | StageId::ObjcAlertDrive
                | StageId::ObjcSwizzleDrive
                | StageId::ObjcBoundDrive => Lane::DriverTarget,
                StageId::GrepGuards
                | StageId::ReleaseTooling
                | StageId::TrustGateVerdict
                | StageId::TrustContractProbe
                | StageId::StartCompare
                | StageId::LicenseHeaders => Lane::Pure,
                _ => Lane::MainTarget,
            };
            assert_eq!(s.lane, want, "{:?} declares the wrong lane", s.id);
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
            dir(&c, Lane::RegexTarget).as_deref(),
            Some("/repo/target-regex")
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
        // The main lane alone follows the caller's redirect, as cargo does.
        c.env.cargo_target_dir = Some("rel".into());
        assert_eq!(dir(&c, Lane::MainTarget).as_deref(), Some("/repo/rel"));
        assert_eq!(
            dir(&c, Lane::DriverTarget).as_deref(),
            Some("/repo/target-drivers")
        );
        c.env.cargo_target_dir = Some("/abs".into());
        assert_eq!(dir(&c, Lane::MainTarget).as_deref(), Some("/abs"));
        assert_eq!(
            dir(&c, Lane::RegexTarget).as_deref(),
            Some("/repo/target-regex")
        );
        // …and the conformance lane least of all: the suites' helper computes
        // its directory from the workspace root and knows nothing about a
        // caller's redirect, so a lane that followed one would prime a
        // directory the suites never open.
        assert_eq!(
            dir(&c, Lane::ConformanceRelease).as_deref(),
            Some("/repo/target/conformance-release")
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

    #[test]
    fn a_missing_tool_never_removes_a_stage() {
        // Toolchain absence is invisible in the plan: it becomes a skip INSIDE
        // the stage, so the ladder still shows the row and the verdict still
        // names it. A stage that disappeared would be a stage nobody missed.
        let nothing_installed = ctx(Mode::Full, Scope::workspace());
        assert!(!nothing_installed.tools.have_targo());
        // 31 since 2026-09-08: the atpkg publish-tooling suites joined the ladder.
        // 32 since 2026-09-13: the driver builds row.
        // 33 since 2026-09-14: the sealed fabric lane — the one test covering the
        // vendored astream-aead is feature-gated and ran on no cadence before it.
        // 34 since 2026-09-22: the conformance-release prime.
        // 35 since 2026-09-23: the measuring tests, out of the test run.
        assert_eq!(plan(&nothing_installed).len(), 35);
    }
}
